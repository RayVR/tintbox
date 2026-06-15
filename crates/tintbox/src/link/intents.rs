//! The intent-driven profile-link chain (lcms2 `DefaultICCintents`,
//! `src/cmscnvrt.c:511-654`) and its helpers `ComputeConversion` (:353),
//! `AddConversion` (:421), `IsEmptyLayer` (:329), plus the `_cmsLinkProfiles`
//! BPC-array mutation (:1119-1135).
//!
//! This is the first end-to-end transform: given a list of profiles + per-link
//! intents/BPC/adaptation, it builds the un-optimized device-link
//! [`Pipeline`](crate::pipeline::Pipeline) by reading each profile's LUT
//! (`read_input_lut` / `read_output_lut` / `read_devicelink_lut`), inserting the
//! PCS-adaptation stages between profiles, and concatenating.
//!
//! `compute_conversion` covers the relative (identity) path, the
//! absolute-colorimetric branch (`ComputeAbsoluteIntent`), and the
//! black-point-compensation branch (`ComputeBlackPointCompensation` +
//! `cmsDetectBlackPoint`/`...Destination...`). It still divides the offset by
//! `MAX_ENCODEABLE_XYZ` to match the C code path. Black-point detection — the
//! V4-perceptual-black constant, a `{0,0,0}` black point, AND the
//! detection-by-sampling cases (V2, V4 matrix-shaper perc/sat, rel-col, ink
//! output, destination round-trip) — is fully implemented in
//! [`crate::link::black_point`] (Lab2/Lab4 virtual profiles + round-trip
//! transforms).

use crate::color::CIEXYZ;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::link::black_point::{
    compute_black_point_compensation, detect_black_point, detect_destination_black_point,
    BlackPoint,
};
use crate::link::profile_lut::{read_devicelink_lut, read_input_lut, read_output_lut};
use crate::math::matrix::{Mat3, Vec3};
use crate::math::whitepoint::D50;
use crate::pipeline::{Pipeline, Stage};
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::sig::Signature;

/// `MAX_ENCODEABLE_XYZ` (lcms2_internal.h:71): `1.0 + 32767.0/32768.0`. The
/// `ComputeConversion` offset divisor (cmscnvrt.c:412).
const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;

/// `cmsGetEncodedICCversion >= 0x4000000` ⇒ V4 (cmscnvrt.c:1132). `Header.version`
/// already holds the validated/clamped encoded value (`cmsGetEncodedICCversion`).
const ICC_VERSION_V4: u32 = 0x0400_0000;

/// `cmsSigMediaWhitePointTag` (`'wtpt'`, include/lcms2.h:394).
const SIG_MEDIA_WHITE_POINT: Signature = Signature::from_raw(0x7774_7074);
/// `cmsSigChromaticAdaptationTag` (`'chad'`, include/lcms2.h:365).
const SIG_CHROMATIC_ADAPTATION: Signature = Signature::from_raw(0x6368_6164);

/// The 3x3 identity matrix (`_cmsMAT3identity`, cmsmtrx.c).
fn mat3_identity() -> Mat3 {
    Mat3([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
}

/// lcms2 `_cmsReadMediaWhitePoint` (`cmsio1.c:64-90`): read the `mediaWhitePoint`
/// (`wtpt`) tag. Fallbacks, verbatim:
/// - if the tag is absent → D50;
/// - else if the profile is **V2** (`encoded version < 0x4000000`) AND a
///   **display** class profile → D50 (a V2 display profile's wtpt is, per spec,
///   always D50 in the PCS);
/// - else → the tag value as-is.
pub fn read_media_white_point(profile: &Profile) -> Result<CIEXYZ> {
    let tag = match profile.read_tag(SIG_MEDIA_WHITE_POINT) {
        Ok(crate::profile::Tag::Xyz(xyz)) => Some(xyz),
        // Tag absent (or present but not an XYZ value, which lcms2's typed read
        // would also reject → treated as "no wp").
        _ => None,
    };

    // If no wp, take D50.
    let tag = match tag {
        Some(t) => t,
        None => return Ok(D50),
    };

    // V2 display profiles should give D50.
    if profile.header().version < ICC_VERSION_V4
        && profile.header().device_class == ProfileClass::Display
    {
        return Ok(D50);
    }

    Ok(tag)
}

/// lcms2 `_cmsReadCHAD` (`cmsio1.c:94-128`): read the `chromaticAdaptation`
/// (`chad`) tag as a row-major [`Mat3`] (9 s15Fixed16 → f64). Fallbacks, verbatim:
/// - if the tag is present → that matrix;
/// - else default to identity, then: if the profile is **V2** AND a **display**
///   class profile, replace identity with the Bradford adaptation matrix from the
///   media white point to D50 (or keep identity if there is no wtpt tag).
pub fn read_chad(profile: &Profile) -> Result<Mat3> {
    if let Ok(crate::profile::Tag::S15Fixed16Array(v)) = profile.read_tag(SIG_CHROMATIC_ADAPTATION)
    {
        if v.len() == 9 {
            let mut m = [0.0f64; 9];
            for (i, s) in v.iter().enumerate() {
                m[i] = s.to_f64();
            }
            return Ok(Mat3(m));
        }
        // A chad tag with the wrong arity is malformed; fall through to the
        // identity/display default (lcms2 only ever stores 9-element chads).
    }

    // No CHAD available, default it to identity.
    let mut dest = mat3_identity();

    // V2 display profiles should give D50.
    if profile.header().version < ICC_VERSION_V4
        && profile.header().device_class == ProfileClass::Display
    {
        let white = match profile.read_tag(SIG_MEDIA_WHITE_POINT) {
            Ok(crate::profile::Tag::Xyz(xyz)) => Some(xyz),
            _ => None,
        };
        match white {
            // No wtpt → stay identity.
            None => return Ok(dest),
            // Bradford adaptation from the media white point to D50.
            Some(w) => {
                dest = crate::adapt::adaptation_matrix(None, w, D50)
                    .ok_or(Error::Corrupt("singular CHAD adaptation matrix"))?;
            }
        }
    }

    Ok(dest)
}

/// lcms2 `ComputeAbsoluteIntent` (`cmscnvrt.c:250-325`): join the relative→absolute
/// and absolute→relative scalings into a single 3x3 chromatic-adaptation matrix
/// between the input and output media white points.
///
/// **AdaptationState semantics (transcribed verbatim):**
/// - `== 1.0` (the default / fully-adapted observer, standard V4 behaviour): a
///   pure **diagonal** matrix of `WPin/WPout` per-axis ratios. The CHADs are *not*
///   used in this branch.
/// - `== 0.0` (observer not adapted, undo the chromatic adaptation): uses only the
///   CHAD matrices + the diagonal scale; no temperature involved.
/// - `0.0 < state < 1.0` (incomplete adaptation): needs `CHAD2Temp`/`Temp2CHAD`
///   (correlated-colour-temperature round-trip), which depend on
///   `cmsTempFromWhitePoint` — **not yet ported** to tintbox. This sub-case returns
///   [`Error::Unsupported`]. The two endpoint states (1.0 and 0.0) are fully
///   implemented and tested; the default state is 1.0.
pub fn compute_absolute_intent(
    adaptation_state: f64,
    white_point_in: &CIEXYZ,
    chad_in: &Mat3,
    white_point_out: &CIEXYZ,
    chad_out: &Mat3,
) -> Result<Mat3> {
    // Adaptation state.
    if adaptation_state == 1.0 {
        // Observer is fully adapted. Keep chromatic adaptation.
        // That is the standard V4 behaviour.
        return Ok(Mat3([
            white_point_in.x / white_point_out.x,
            0.0,
            0.0,
            0.0,
            white_point_in.y / white_point_out.y,
            0.0,
            0.0,
            0.0,
            white_point_in.z / white_point_out.z,
        ]));
    }

    // Incomplete adaptation. This is an advanced feature.
    let scale = Mat3([
        white_point_in.x / white_point_out.x,
        0.0,
        0.0,
        0.0,
        white_point_in.y / white_point_out.y,
        0.0,
        0.0,
        0.0,
        white_point_in.z / white_point_out.z,
    ]);

    if adaptation_state == 0.0 {
        // m1 = *ChromaticAdaptationMatrixOut;
        // _cmsMAT3per(&m2, &m1, &Scale);
        // m2 holds CHAD from output white to D50 times abs. col. scaling.
        let m2 = chad_out.per(&scale);

        // Observer is not adapted, undo the chromatic adaptation.
        // _cmsMAT3per(m, &m2, ChromaticAdaptationMatrixOut);
        // NOTE: the C overwrites `m` here, then immediately overwrites it again
        // below with `_cmsMAT3per(m, &m2, &m4)`; only the second assignment
        // survives, so the first is dead. We transcribe only the surviving op.
        //
        // m3 = *ChromaticAdaptationMatrixIn;
        // if (!_cmsMAT3inverse(&m3, &m4)) return FALSE;
        // _cmsMAT3per(m, &m2, &m4);
        let m4 = chad_in
            .inverse()
            .ok_or(Error::Corrupt("singular input CHAD"))?;
        Ok(m2.per(&m4))
    } else {
        // 0 < AdaptationState < 1: the CHAD2Temp/Temp2CHAD mixed-temperature path
        // (cmscnvrt.c:293-320) needs cmsTempFromWhitePoint, which is not yet
        // ported. The default/tested adaptation state is 1.0; 0.0 is also
        // supported above. Fractional adaptation is deferred.
        Err(Error::Unsupported(
            "fractional absolute-colorimetric adaptation (CHAD2Temp/Temp2CHAD) not implemented",
        ))
    }
}

/// lcms2 `_cmsLinkProfiles` BPC-array mutation (`cmscnvrt.c:1119-1135`), which
/// runs BEFORE the chain is built. Following Adobe's document: BPC does not apply
/// to absolute colorimetric (forced off), and is forced ON for V4 profiles in
/// perceptual and saturation.
///
/// `bpc` is mutated in place. `intents` and `profiles` must be the same length as
/// `bpc`; for each link `i`:
/// - if `intents[i] == AbsoluteColorimetric` → `bpc[i] = false`;
/// - if `intents[i] ∈ {Perceptual, Saturation}` and `profiles[i]` is V4
///   (encoded version `>= 0x4000000`) → `bpc[i] = true`.
pub fn link_bpc_mutation(intents: &[RenderingIntent], profiles: &[&Profile], bpc: &mut [bool]) {
    for i in 0..profiles.len() {
        if intents[i] == RenderingIntent::AbsoluteColorimetric {
            bpc[i] = false;
        }
        if intents[i] == RenderingIntent::Perceptual || intents[i] == RenderingIntent::Saturation {
            // Force BPC for V4 profiles in perceptual and saturation.
            if profiles[i].header().version >= ICC_VERSION_V4 {
                bpc[i] = true;
            }
        }
    }
}

/// lcms2 `IsEmptyLayer` (`cmscnvrt.c:329-348`): is the matrix/offset close enough
/// to identity that the conversion stage can be dropped? Returns `true` when
/// `Σ|m − I| + Σ|off| < 0.002`. (lcms2 also treats a NULL matrix as empty; here
/// the matrix is always present, so we only implement the numeric test.)
pub fn is_empty_layer(m: &Mat3, off: &Vec3) -> bool {
    let ident = mat3_identity();
    let mut diff = 0.0f64;

    // for (i=0; i < 3*3; i++) diff += fabs(m[i] - Ident[i]);
    for i in 0..9 {
        diff += (m.0[i] - ident.0[i]).abs();
    }
    // for (i=0; i < 3; i++) diff += fabs(off[i]);
    for i in 0..3 {
        diff += off.0[i].abs();
    }

    diff < 0.002
}

/// Map a [`BlackPoint`] detection outcome to the concrete XYZ that
/// `ComputeConversion` (cmscnvrt.c:389-392) would use, mirroring how the C reads
/// the `{0,0,0}`-initialized out-parameter regardless of the boolean return:
/// - [`BlackPoint::Resolved`] → that XYZ (the V4 perceptual constant or a sampled
///   value);
/// - [`BlackPoint::Zero`] → `{0,0,0}` (the C `return FALSE` with the zeroed
///   out-param — e.g. inadequate class/intent).
fn resolve_black_point(bp: BlackPoint) -> CIEXYZ {
    match bp {
        BlackPoint::Resolved(xyz) => xyz,
        BlackPoint::Zero => CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    }
}

/// lcms2 `ComputeConversion` (`cmscnvrt.c:353-416`): compute the PCS-adaptation
/// matrix `m` and offset `off` between profile `i-1` (the current PCS) and
/// profile `i`.
///
/// Implemented branches: the relative (identity) path, the absolute-colorimetric
/// branch (`ComputeAbsoluteIntent`), and the black-point-compensation branch
/// (`ComputeBlackPointCompensation`). Both black points are resolved via
/// [`detect_black_point`]/[`detect_destination_black_point`] — the
/// V4-perceptual-black constant, the `{0,0,0}` class/intent cases, AND
/// detection-by-sampling (`BlackPointAsDarkerColorant` / the destination
/// round-trip over the Lab2/Lab4 virtuals). Regardless of the branch, the C
/// unconditionally divides every offset component by `MAX_ENCODEABLE_XYZ` at the
/// end; we replicate that.
pub fn compute_conversion(
    i: usize,
    profiles: &[&Profile],
    intent: RenderingIntent,
    bpc: bool,
    adaptation_state: f64,
) -> Result<(Mat3, Vec3)> {
    // m and off are set to identity and this is detected later on (cmscnvrt.c:364).
    let mut m = mat3_identity();
    let mut off = Vec3([0.0, 0.0, 0.0]);

    if intent == RenderingIntent::AbsoluteColorimetric {
        // Absolute colorimetric (cmscnvrt.c:368-383): read media white points +
        // CHADs of profiles[i-1] (current PCS) and profiles[i], then build the
        // chromatic-adaptation matrix. The offset stays zero.
        let white_point_in = read_media_white_point(profiles[i - 1])?;
        let chad_in = read_chad(profiles[i - 1])?;

        let white_point_out = read_media_white_point(profiles[i])?;
        let chad_out = read_chad(profiles[i])?;

        m = compute_absolute_intent(
            adaptation_state,
            &white_point_in,
            &chad_in,
            &white_point_out,
            &chad_out,
        )?;
    } else if bpc {
        // Black-point compensation (cmscnvrt.c:387-399). Detect both black points;
        // if they differ, build the diagonal scaling + offset.
        //
        // cmsDetectBlackPoint / ...Destination... init their out-param to {0,0,0}
        // and `return FALSE` (leaving it zero) for inadequate class/intent. We map
        // that FALSE-with-zero to BlackPoint::Zero ⇒ CIEXYZ{0,0,0}. The
        // detection-by-sampling paths resolve to a sampled XYZ (BlackPoint::Resolved).
        let bp_in = resolve_black_point(detect_black_point(profiles[i - 1], intent)?);
        let bp_out = resolve_black_point(detect_destination_black_point(profiles[i], intent)?);

        // If black points are equal, then do nothing (cmscnvrt.c:394-398).
        if bp_in.x != bp_out.x || bp_in.y != bp_out.y || bp_in.z != bp_out.z {
            let (bpc_m, bpc_off) = compute_black_point_compensation(&bp_in, &bp_out);
            m = bpc_m;
            off = bpc_off;
        }
    }

    // Offset should be adjusted because of the encoding (cmscnvrt.c:402-413).
    // for (k=0; k < 3; k++) off[k] /= MAX_ENCODEABLE_XYZ;
    for k in 0..3 {
        off.0[k] /= MAX_ENCODEABLE_XYZ;
    }

    Ok((m, off))
}

/// lcms2 `AddConversion` (`cmscnvrt.c:421-487`): append the PCS-adaptation stage
/// for the `in_pcs` → `out_pcs` transition. The matrix `m`/offset `off` operate in
/// XYZ space; [`is_empty_layer`] decides whether the `Stage::Matrix` is dropped.
///
/// The four PCS cases:
/// - **XYZ → XYZ:** Matrix (iff not empty).
/// - **XYZ → Lab:** Matrix (iff not empty), then `Xyz2Lab`.
/// - **Lab → XYZ:** `Lab2Xyz`, then Matrix (iff not empty).
/// - **Lab → Lab:** iff not empty → `Lab2Xyz` + Matrix + `Xyz2Lab` (all three or
///   none).
/// - **default:** require `in_pcs == out_pcs`, else a colorspace-mismatch error.
pub fn add_conversion(
    result: &mut Pipeline,
    in_pcs: ColorSpace,
    out_pcs: ColorSpace,
    m: &Mat3,
    off: &Vec3,
) -> Result<()> {
    // The Matrix stage as cmsStageAllocMatrix(3, 3, m, off) builds it: 3 rows,
    // 3 cols, row-major matrix, 3-element offset.
    let matrix_stage = || Stage::Matrix {
        rows: 3,
        cols: 3,
        m: m.0.to_vec(),
        offset: Some(off.0.to_vec()),
    };

    match in_pcs {
        // Input profile operates in XYZ (cmscnvrt.c:429).
        ColorSpace::XYZ => match out_pcs {
            ColorSpace::XYZ => {
                // XYZ -> XYZ
                if !is_empty_layer(m, off) {
                    result.insert_stage_at_end(matrix_stage())?;
                }
            }
            ColorSpace::Lab => {
                // XYZ -> Lab
                if !is_empty_layer(m, off) {
                    result.insert_stage_at_end(matrix_stage())?;
                }
                result.insert_stage_at_end(Stage::Xyz2Lab)?;
            }
            _ => return Err(Error::Corrupt("ColorSpace mismatch")),
        },

        // Input profile operates in Lab (cmscnvrt.c:452).
        ColorSpace::Lab => match out_pcs {
            ColorSpace::XYZ => {
                // Lab -> XYZ
                result.insert_stage_at_end(Stage::Lab2Xyz)?;
                if !is_empty_layer(m, off) {
                    result.insert_stage_at_end(matrix_stage())?;
                }
            }
            ColorSpace::Lab => {
                // Lab -> Lab
                if !is_empty_layer(m, off) {
                    result.insert_stage_at_end(Stage::Lab2Xyz)?;
                    result.insert_stage_at_end(matrix_stage())?;
                    result.insert_stage_at_end(Stage::Xyz2Lab)?;
                }
            }
            _ => return Err(Error::Corrupt("ColorSpace mismatch")),
        },

        // On colorspaces other than PCS, check for same space (cmscnvrt.c:481).
        _ => {
            if in_pcs != out_pcs {
                return Err(Error::Corrupt("ColorSpace mismatch"));
            }
        }
    }

    Ok(())
}

/// lcms2 `ColorSpaceIsCompatible` (`cmscnvrt.c:491-506`): are `a` and `b`
/// interchangeable for the chain-junction check? Same space, MCH4↔CMYK, or
/// XYZ↔Lab.
fn color_space_is_compatible(a: ColorSpace, b: ColorSpace) -> bool {
    if a == b {
        return true;
    }
    // MCH4 substitution of CMYK.
    if a == ColorSpace::Mch4 && b == ColorSpace::Cmyk {
        return true;
    }
    if a == ColorSpace::Cmyk && b == ColorSpace::Mch4 {
        return true;
    }
    // XYZ/Lab are interchangeable (one computable from the other).
    if a == ColorSpace::XYZ && b == ColorSpace::Lab {
        return true;
    }
    if a == ColorSpace::Lab && b == ColorSpace::XYZ {
        return true;
    }
    false
}

/// lcms2 `cmsChannelsOfColorSpace` (`cmspcs.c:877-940`): the device channel count
/// of a color space, or `None` for an unrecognized space (the C `-1`).
fn channels_of_color_space(cs: ColorSpace) -> Option<usize> {
    use ColorSpace::*;
    Some(match cs {
        Mch1 | Color1 | Gray => 1,
        Mch2 | Color2 => 2,
        XYZ | Lab | Luv | YCbCr | Yxy | Rgb | Hsv | Hls | Cmy | Mch3 | Color3 => 3,
        LuvK | Cmyk | Mch4 | Color4 => 4,
        Mch5 | Color5 => 5,
        Mch6 | Color6 => 6,
        Mch7 | Color7 => 7,
        Mch8 | Color8 => 8,
        Mch9 | Color9 => 9,
        MchA | Color10 => 10,
        MchB | Color11 => 11,
        MchC | Color12 => 12,
        MchD | Color13 => 13,
        MchE | Color14 => 14,
        MchF | Color15 => 15,
        _ => return None,
    })
}

/// lcms2 `DefaultICCintents` (`cmscnvrt.c:511-651`): build the un-optimized
/// device-link [`Pipeline`] for the chain of `profiles` under the per-link
/// `intents`, `bpc`, and `adaptation` states.
///
/// The three-way conversion branch (spec §8.2), per profile `i`:
/// - **input leg** (`l_is_input`, a non-PCS connection): `read_input_lut` →
///   concat only (no conversion).
/// - **output leg** (a PCS connection, intent applies): `read_output_lut` →
///   `compute_conversion` → `add_conversion(Result, CurrentColorSpace,
///   ColorSpaceIn, m, off)` → concat.
/// - **devicelink / abstract leg** (link or abstract class): `read_devicelink_lut`;
///   conversion only if `Abstract && i > 0` (else identity m/off) →
///   `add_conversion` → concat.
///
/// After each profile, `CurrentColorSpace` advances to that profile's output
/// space. `flags` is currently unused (NOOPTIMIZE is the only slice-5 reference,
/// and NONEGATIVES clipping is out of T2 scope).
///
/// NOTE: the caller is expected to have already applied [`link_bpc_mutation`] to
/// `bpc` (lcms2 does this in `_cmsLinkProfiles` before invoking the handler).
pub fn default_icc_intents(
    profiles: &[&Profile],
    intents: &[RenderingIntent],
    bpc: &[bool],
    adaptation: &[f64],
    _flags: u32,
) -> Result<Pipeline> {
    let n_profiles = profiles.len();
    // For safety (cmscnvrt.c:529).
    if n_profiles == 0 {
        return Err(Error::Range);
    }
    assert_eq!(intents.len(), n_profiles);
    assert_eq!(bpc.len(), n_profiles);
    assert_eq!(adaptation.len(), n_profiles);

    // Allocate an empty LUT for holding the result. 0 channels means 'undefined'.
    let mut result = Pipeline::new(0, 0);

    // CurrentColorSpace = cmsGetColorSpace(hProfiles[0]) (cmscnvrt.c:535).
    let mut current_color_space = profiles[0].header().color_space;
    let mut color_space_out = ColorSpace::Lab; // initialized as in the C (:524).
                                               // Whether the final leg is a single named-color profile producing DEVICE
                                               // colorants. lcms2's bookkeeping `ColorSpaceOut` for that leg is the PCS
                                               // (cmscnvrt.c:560), but `_cmsReadDevicelinkLUT` builds a UsePCS=FALSE named
                                               // stage whose output is the device space — so the pipeline's true output
                                               // width is the colorant count, not the PCS channel count. We skip the final
                                               // width guard in that case (lcms2 has no such explicit guard).
    let mut final_is_named_device = false;

    for i in 0..n_profiles {
        let profile = profiles[i];
        let class_sig = profile.header().device_class;
        let l_is_device_link =
            class_sig == ProfileClass::Link || class_sig == ProfileClass::Abstract;

        // First profile is used as input unless devicelink or abstract
        // (cmscnvrt.c:546-553).
        let l_is_input = if i == 0 && !l_is_device_link {
            true
        } else {
            // Else use the profile in the input direction if current space is not PCS.
            current_color_space != ColorSpace::XYZ && current_color_space != ColorSpace::Lab
        };

        let intent = intents[i];

        let (color_space_in, cs_out) = if l_is_input || l_is_device_link {
            (profile.header().color_space, profile.header().pcs)
        } else {
            (profile.header().pcs, profile.header().color_space)
        };
        color_space_out = cs_out;

        if !color_space_is_compatible(color_space_in, current_color_space) {
            return Err(Error::Corrupt("ColorSpace mismatch"));
        }

        // If devicelink is found, then no custom intent is allowed and we can read
        // the LUT to be applied. Settings don't apply here (cmscnvrt.c:576). We
        // also route a single named-color profile through here (nProfiles == 1).
        let single_named = class_sig == ProfileClass::NamedColor && n_profiles == 1;
        final_is_named_device = single_named;

        let lut = if l_is_device_link || single_named {
            let lut = read_devicelink_lut(profile, intent.to_raw())?;

            // What about abstract profiles? (cmscnvrt.c:583-589.)
            let (m, off) = if class_sig == ProfileClass::Abstract && i > 0 {
                compute_conversion(i, profiles, intent, bpc[i], adaptation[i])?
            } else {
                (mat3_identity(), Vec3([0.0, 0.0, 0.0]))
            };

            add_conversion(&mut result, current_color_space, color_space_in, &m, &off)?;
            lut
        } else if l_is_input {
            // Input direction means non-pcs connection, so proceed like devicelinks
            // (cmscnvrt.c:597-600). No conversion.
            read_input_lut(profile, intent.to_raw())?
        } else {
            // Output direction means PCS connection. Intent may apply here
            // (cmscnvrt.c:602-611).
            let lut = read_output_lut(profile, intent.to_raw())?;
            let (m, off) = compute_conversion(i, profiles, intent, bpc[i], adaptation[i])?;
            add_conversion(&mut result, current_color_space, color_space_in, &m, &off)?;
            lut
        };

        // Concatenate to the output LUT (cmscnvrt.c:616).
        result.concat(&lut)?;

        // Update current space (cmscnvrt.c:623).
        current_color_space = color_space_out;
    }

    // lcms2 `PreOptimize` (cmsopt.c:251-289) runs inside `_cmsOptimizePipeline`
    // (cmsopt.c:1952) BEFORE the `cmsFLAGS_NOOPTIMIZE` early-return (cmsopt.c:1961),
    // so it applies even to the "unoptimized" device link. We must replicate it for
    // bit-identity: its `_MultiplyMatrix` step merges adjacent matrix stages (e.g.
    // an input matrix-shaper's RGB→XYZ matrix and an output matrix-shaper's XYZ→RGB
    // matrix) into a single pre-multiplied matrix, removing an intermediate f32
    // rounding. Leaving them separate diverges from lcms2-NOOPTIMIZE by a few LSB
    // after the following tone curve (the 8→16 matrix-shaper divergence). This is a
    // value-preserving structural simplification only for identity/inverse removal;
    // the matrix merge is a genuine numeric change that lcms2 performs unconditionally.
    result.pre_optimize();

    // Final channel sanity guard: the chain's output width must match the device
    // channel count of the last profile's output space. lcms2 enforces this
    // implicitly through cmsPipelineCat/BlessLUT as stages are appended; we assert
    // it explicitly to catch a mis-built chain early. (NONEGATIVES clipping,
    // cmscnvrt.c:626-640, is out of T2 scope.)
    if !final_is_named_device {
        if let Some(n) = channels_of_color_space(color_space_out) {
            if result.output_channels != n {
                return Err(Error::Corrupt(
                    "final pipeline output width does not match output color space channels",
                ));
            }
        }
    }

    Ok(result)
}

/// Intent-dispatching entry point (lcms2 `_cmsLinkProfiles` →
/// `cmsGetIntentsFromProfile`/`SearchIntent`): scan `intents`; if any equals a
/// registered [`RenderingIntentPlugin`]'s `intent()`, the FIRST such plugin's
/// `link` runs instead of the builtin. lcms2's `_cmsLinkProfiles` walks the chain
/// looking for the first intent that has a registered handler and dispatches to
/// it; with no custom intent in the chain (or an empty registry) this is
/// [`default_icc_intents`] verbatim — the BUILTIN path.
///
/// A custom plugin's `link` may itself recurse into [`default_icc_intents`] for
/// the non-custom legs, exactly as lcms2's custom intent functions do.
///
/// The `*_in(ctx, …)` convention: the no-`ctx` builtin builder remains
/// [`default_icc_intents`]; callers that want plugin dispatch route through here.
pub fn link_icc_intents_in(
    ctx: &Context,
    profiles: &[&Profile],
    intents: &[RenderingIntent],
    bpc: &[bool],
    adaptation: &[f64],
    flags: u32,
) -> Result<Pipeline> {
    let registry = &ctx.plugins().intents;
    if !registry.is_empty() {
        // First intent in the chain that a registered plugin services wins
        // (lcms2 dispatches to the first matching handler found while walking the
        // chain). Builtins are matched implicitly by `default_icc_intents`, so a
        // plugin can only claim an intent number it registered.
        for &intent in intents {
            let raw = intent.to_raw();
            if let Some(plugin) = registry.iter().find(|p| p.intent() == raw) {
                return plugin.link(ctx, profiles, intents, bpc, adaptation, flags);
            }
        }
    }
    default_icc_intents(profiles, intents, bpc, adaptation, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_empty_layer (boundary 0.002) ------------------------------------

    #[test]
    fn is_empty_layer_identity_is_empty() {
        let m = mat3_identity();
        let off = Vec3([0.0, 0.0, 0.0]);
        assert!(is_empty_layer(&m, &off));
    }

    #[test]
    fn is_empty_layer_boundary() {
        // Sum of |m-I|+|off| just under 0.002 → empty; at/over → not empty.
        // Perturb a single matrix entry by exactly 0.0019 (< 0.002) → empty.
        let mut m = mat3_identity();
        m.0[0] = 1.0 + 0.0019;
        let off = Vec3([0.0, 0.0, 0.0]);
        assert!(is_empty_layer(&m, &off), "0.0019 < 0.002 ⇒ empty");

        // Exactly 0.002 → NOT < 0.002 ⇒ not empty.
        let mut m2 = mat3_identity();
        m2.0[0] = 1.0 + 0.002;
        assert!(
            !is_empty_layer(&m2, &off),
            "0.002 is not < 0.002 ⇒ not empty"
        );

        // Just over via offset: |off| = 0.0025 ⇒ not empty.
        let off2 = Vec3([0.0025, 0.0, 0.0]);
        assert!(!is_empty_layer(&mat3_identity(), &off2));
    }

    #[test]
    fn is_empty_layer_accumulates_across_entries() {
        // Several tiny perturbations summing to >= 0.002 ⇒ not empty.
        let mut m = mat3_identity();
        m.0[1] = 0.001;
        m.0[2] = 0.001;
        let off = Vec3([0.0005, 0.0, 0.0]);
        // 0.001 + 0.001 + 0.0005 = 0.0025 >= 0.002.
        assert!(!is_empty_layer(&m, &off));
    }

    // ---- add_conversion (each of the 4 PCS cases) ---------------------------

    fn ident_m_off() -> (Mat3, Vec3) {
        (mat3_identity(), Vec3([0.0, 0.0, 0.0]))
    }

    #[test]
    fn add_conversion_xyz_to_xyz_empty_inserts_nothing() {
        let (m, off) = ident_m_off();
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::XYZ, ColorSpace::XYZ, &m, &off).unwrap();
        assert!(p.stages().is_empty(), "identity XYZ->XYZ adds no stage");
    }

    #[test]
    fn add_conversion_xyz_to_xyz_non_empty_inserts_matrix() {
        let mut m = mat3_identity();
        m.0[0] = 2.0; // clearly not empty
        let off = Vec3([0.0, 0.0, 0.0]);
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::XYZ, ColorSpace::XYZ, &m, &off).unwrap();
        assert_eq!(p.stages().len(), 1);
        assert!(matches!(p.stages()[0], Stage::Matrix { .. }));
    }

    #[test]
    fn add_conversion_xyz_to_lab() {
        let (m, off) = ident_m_off();
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::XYZ, ColorSpace::Lab, &m, &off).unwrap();
        // Identity matrix dropped; only Xyz2Lab remains.
        assert_eq!(p.stages().len(), 1);
        assert!(matches!(p.stages()[0], Stage::Xyz2Lab));

        // Non-empty: Matrix then Xyz2Lab.
        let mut m2 = mat3_identity();
        m2.0[4] = 0.5;
        let mut p2 = Pipeline::new(3, 3);
        add_conversion(&mut p2, ColorSpace::XYZ, ColorSpace::Lab, &m2, &off).unwrap();
        assert_eq!(p2.stages().len(), 2);
        assert!(matches!(p2.stages()[0], Stage::Matrix { .. }));
        assert!(matches!(p2.stages()[1], Stage::Xyz2Lab));
    }

    #[test]
    fn add_conversion_lab_to_xyz() {
        let (m, off) = ident_m_off();
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::Lab, ColorSpace::XYZ, &m, &off).unwrap();
        // Identity matrix dropped; only Lab2Xyz remains.
        assert_eq!(p.stages().len(), 1);
        assert!(matches!(p.stages()[0], Stage::Lab2Xyz));

        // Non-empty: Lab2Xyz then Matrix.
        let mut m2 = mat3_identity();
        m2.0[8] = 0.5;
        let mut p2 = Pipeline::new(3, 3);
        add_conversion(&mut p2, ColorSpace::Lab, ColorSpace::XYZ, &m2, &off).unwrap();
        assert_eq!(p2.stages().len(), 2);
        assert!(matches!(p2.stages()[0], Stage::Lab2Xyz));
        assert!(matches!(p2.stages()[1], Stage::Matrix { .. }));
    }

    #[test]
    fn add_conversion_lab_to_lab() {
        let (m, off) = ident_m_off();
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::Lab, ColorSpace::Lab, &m, &off).unwrap();
        // Identity ⇒ all three dropped.
        assert!(p.stages().is_empty());

        // Non-empty ⇒ Lab2Xyz + Matrix + Xyz2Lab.
        let mut m2 = mat3_identity();
        m2.0[0] = 1.5;
        let mut p2 = Pipeline::new(3, 3);
        add_conversion(&mut p2, ColorSpace::Lab, ColorSpace::Lab, &m2, &off).unwrap();
        assert_eq!(p2.stages().len(), 3);
        assert!(matches!(p2.stages()[0], Stage::Lab2Xyz));
        assert!(matches!(p2.stages()[1], Stage::Matrix { .. }));
        assert!(matches!(p2.stages()[2], Stage::Xyz2Lab));
    }

    #[test]
    fn add_conversion_default_same_space_ok_mismatch_err() {
        let (m, off) = ident_m_off();
        // Non-PCS same space → no stage, no error.
        let mut p = Pipeline::new(3, 3);
        add_conversion(&mut p, ColorSpace::Rgb, ColorSpace::Rgb, &m, &off).unwrap();
        assert!(p.stages().is_empty());

        // Non-PCS mismatch → error.
        let mut p2 = Pipeline::new(3, 3);
        let err = add_conversion(&mut p2, ColorSpace::Rgb, ColorSpace::Cmyk, &m, &off);
        assert!(err.is_err());
    }

    // ---- link_bpc_mutation --------------------------------------------------

    #[test]
    fn link_bpc_mutation_absolute_forces_off() {
        // Build a minimal V4 RGB profile from the testbed (crayons is V4).
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed/crayons.icc"
        ))
        .unwrap();
        let p = Profile::open(&bytes).unwrap();
        assert!(p.header().version >= ICC_VERSION_V4, "crayons is V4");
        let profiles = [&p, &p];

        // Absolute → forced off even when requested on.
        let mut bpc = [true, true];
        link_bpc_mutation(
            &[
                RenderingIntent::AbsoluteColorimetric,
                RenderingIntent::AbsoluteColorimetric,
            ],
            &profiles,
            &mut bpc,
        );
        assert_eq!(bpc, [false, false]);
    }

    #[test]
    fn link_bpc_mutation_v4_perceptual_forces_on() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed/crayons.icc"
        ))
        .unwrap();
        let p = Profile::open(&bytes).unwrap();
        let profiles = [&p, &p];

        // V4 perceptual/saturation → forced on even when requested off.
        let mut bpc = [false, false];
        link_bpc_mutation(
            &[RenderingIntent::Perceptual, RenderingIntent::Saturation],
            &profiles,
            &mut bpc,
        );
        assert_eq!(bpc, [true, true]);
    }

    #[test]
    fn link_bpc_mutation_v2_perceptual_unchanged() {
        // test5 is a V2 RGB display profile (ver 0x02100000 < 0x04000000).
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed/test5.icc"
        ))
        .unwrap();
        let p = Profile::open(&bytes).unwrap();
        assert!(p.header().version < ICC_VERSION_V4, "test5 is V2");
        let profiles = [&p, &p];

        // V2 perceptual → NOT forced (left as caller requested).
        let mut bpc = [false, true];
        link_bpc_mutation(
            &[RenderingIntent::Perceptual, RenderingIntent::Perceptual],
            &profiles,
            &mut bpc,
        );
        assert_eq!(bpc, [false, true]);

        // RelativeColorimetric never touches the flag.
        let mut bpc2 = [true, false];
        link_bpc_mutation(
            &[
                RenderingIntent::RelativeColorimetric,
                RenderingIntent::RelativeColorimetric,
            ],
            &profiles,
            &mut bpc2,
        );
        assert_eq!(bpc2, [true, false]);
    }

    // ---- compute_conversion (relative path) ---------------------------------

    // ---- read_media_white_point / read_chad ---------------------------------

    fn load(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../vendor/Little-CMS/testbed/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    #[test]
    fn read_media_white_point_v2_display_is_d50() {
        // test5 is a V2 RGB display profile → media white point forced to D50.
        let bytes = load("test5.icc");
        let p = Profile::open(&bytes).unwrap();
        assert!(p.header().version < ICC_VERSION_V4);
        assert_eq!(p.header().device_class, ProfileClass::Display);
        let wp = read_media_white_point(&p).unwrap();
        assert_eq!(wp, D50);
    }

    #[test]
    fn read_chad_falls_back_when_absent() {
        // Whatever the profile, read_chad must succeed (identity, Bradford, or a
        // real chad tag) and never error for a valid testbed profile.
        for name in ["test5.icc", "crayons.icc"] {
            let bytes = load(name);
            let p = Profile::open(&bytes).unwrap();
            let chad = read_chad(&p).unwrap();
            // The matrix must be non-singular (invertible) — both identity and a
            // real adaptation matrix are.
            assert!(chad.inverse().is_some(), "{name}: chad singular");
        }
    }

    // ---- compute_absolute_intent --------------------------------------------

    #[test]
    fn compute_absolute_intent_state_1_is_diagonal_ratios() {
        // AdaptationState == 1.0 → pure diagonal of WPin/WPout ratios; CHADs unused.
        let wp_in = CIEXYZ {
            x: 0.95,
            y: 1.0,
            z: 1.08,
        };
        let wp_out = CIEXYZ {
            x: 0.9642,
            y: 1.0,
            z: 0.8249,
        };
        // Use non-identity CHADs to prove they are ignored at state 1.0.
        let chad_in = Mat3([1.1, 0.0, 0.0, 0.0, 1.2, 0.0, 0.0, 0.0, 1.3]);
        let chad_out = Mat3([0.9, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.7]);
        let m = compute_absolute_intent(1.0, &wp_in, &chad_in, &wp_out, &chad_out).unwrap();
        let expected = Mat3([
            wp_in.x / wp_out.x,
            0.0,
            0.0,
            0.0,
            wp_in.y / wp_out.y,
            0.0,
            0.0,
            0.0,
            wp_in.z / wp_out.z,
        ]);
        for k in 0..9 {
            assert_eq!(m.0[k].to_bits(), expected.0[k].to_bits(), "entry {k}");
        }
    }

    #[test]
    fn compute_absolute_intent_state_1_same_wp_is_identity() {
        // Identical white points at state 1.0 → exact identity ratios (1.0).
        let wp = CIEXYZ {
            x: 0.9642,
            y: 1.0,
            z: 0.8249,
        };
        let id = mat3_identity();
        let m = compute_absolute_intent(1.0, &wp, &id, &wp, &id).unwrap();
        assert!(is_empty_layer(&m, &Vec3([0.0, 0.0, 0.0])));
    }

    #[test]
    fn compute_absolute_intent_state_0_uses_chads() {
        // AdaptationState == 0.0 → m = (chad_out * scale) * chad_in^-1.
        let wp_in = CIEXYZ {
            x: 0.95,
            y: 1.0,
            z: 1.08,
        };
        let wp_out = CIEXYZ {
            x: 0.9642,
            y: 1.0,
            z: 0.8249,
        };
        let chad_in = Mat3([1.1, 0.01, 0.0, 0.0, 1.2, 0.02, 0.0, 0.0, 1.3]);
        let chad_out = Mat3([0.9, 0.0, 0.03, 0.0, 0.8, 0.0, 0.01, 0.0, 0.7]);
        let scale = Mat3([
            wp_in.x / wp_out.x,
            0.0,
            0.0,
            0.0,
            wp_in.y / wp_out.y,
            0.0,
            0.0,
            0.0,
            wp_in.z / wp_out.z,
        ]);
        let m2 = chad_out.per(&scale);
        let expected = m2.per(&chad_in.inverse().unwrap());
        let m = compute_absolute_intent(0.0, &wp_in, &chad_in, &wp_out, &chad_out).unwrap();
        for k in 0..9 {
            assert_eq!(m.0[k].to_bits(), expected.0[k].to_bits(), "entry {k}");
        }
    }

    #[test]
    fn compute_absolute_intent_fractional_unsupported() {
        let wp = CIEXYZ {
            x: 0.9642,
            y: 1.0,
            z: 0.8249,
        };
        let id = mat3_identity();
        let err = compute_absolute_intent(0.5, &wp, &id, &wp, &id);
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn compute_conversion_absolute_state_1_matches_diagonal() {
        // End-to-end through compute_conversion: absolute intent at state 1.0 over
        // two real profiles yields the WPin/WPout diagonal, offset zero.
        let a = load("test5.icc");
        let b = load("crayons.icc");
        let pa = Profile::open(&a).unwrap();
        let pb = Profile::open(&b).unwrap();
        let profiles = [&pa, &pb];
        let (m, off) = compute_conversion(
            1,
            &profiles,
            RenderingIntent::AbsoluteColorimetric,
            false,
            1.0,
        )
        .unwrap();
        let wp_in = read_media_white_point(&pa).unwrap();
        let wp_out = read_media_white_point(&pb).unwrap();
        let expected = Mat3([
            wp_in.x / wp_out.x,
            0.0,
            0.0,
            0.0,
            wp_in.y / wp_out.y,
            0.0,
            0.0,
            0.0,
            wp_in.z / wp_out.z,
        ]);
        for k in 0..9 {
            assert_eq!(m.0[k].to_bits(), expected.0[k].to_bits(), "entry {k}");
        }
        assert_eq!(off, Vec3([0.0, 0.0, 0.0]));
    }

    #[test]
    fn compute_conversion_relative_is_identity() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed/crayons.icc"
        ))
        .unwrap();
        let p = Profile::open(&bytes).unwrap();
        let profiles = [&p, &p];
        let (m, off) = compute_conversion(
            1,
            &profiles,
            RenderingIntent::RelativeColorimetric,
            false,
            1.0,
        )
        .unwrap();
        assert_eq!(m, mat3_identity());
        // Zero offset divided by MAX_ENCODEABLE_XYZ is still zero.
        assert_eq!(off, Vec3([0.0, 0.0, 0.0]));
        // And the resulting layer is empty.
        assert!(is_empty_layer(&m, &off));
    }
}
