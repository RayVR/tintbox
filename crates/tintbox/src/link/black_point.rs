//! Black-point detection + black-point-compensation (BPC) math.
//!
//! Ports:
//! - `ComputeBlackPointCompensation` (`src/cmscnvrt.c:169-200`): the diagonal
//!   matrix + offset that linearly scales XYZ so the input black point maps to the
//!   output black point while D50 stays fixed. Always implementable.
//! - the **V4-perceptual-black-constant** short-circuit of `cmsDetectBlackPoint` /
//!   `cmsDetectDestinationBlackPoint` (`src/cmssamp.c:238-321` / `:399-456`): for a
//!   non-Link/Abstract/NamedColor profile under perceptual or saturation intent,
//!   when the profile is V4 (`cmsGetEncodedICCversion >= 0x4000000`) **and is not a
//!   matrix shaper**, the black point is the fixed constant `cmsPERCEPTUAL_BLACK_*`
//!   (`include/lcms2.h:297-299`). No transform is created.
//!
//! **Detection-by-sampling** (slice-7): the paths `cmsDetectBlackPoint` /
//! `...Destination...` resolve by *sampling* the profile —
//! `BlackPointAsDarkerColorant` (V2/rel-col, and V4 matrix shapers under
//! perc/sat), `BlackPointUsingPerceptualBlack` (ink output rel-col), and the
//! destination round-trip detection. These use the Lab2/Lab4 virtual profiles +
//! round-trip transforms ([`detect_black_point`] / [`detect_destination_black_point`]).

use crate::color::{CIELab, CIEXYZ};
use crate::error::Result;
use crate::format::decode::{PT_CMY, PT_CMYK, PT_GRAY, PT_LAB, PT_RGB, TYPE_LAB_DBL};
use crate::math::matrix::{Mat3, Vec3};
use crate::math::whitepoint::D50;
use crate::pcs::{lab_to_xyz, xyz_to_lab};
use crate::profile::virtuals::{build_lab2_profile, build_lab4_profile};
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::sig::Signature;
use crate::transform::{Flags, Transform};

/// `cmsGetEncodedICCversion >= 0x4000000` ⇒ V4 (`cmssamp.c:261`).
const ICC_VERSION_V4: u32 = 0x0400_0000;

/// `cmsPERCEPTUAL_BLACK_X` (`include/lcms2.h:297`).
const PERCEPTUAL_BLACK_X: f64 = 0.00336;
/// `cmsPERCEPTUAL_BLACK_Y` (`include/lcms2.h:298`).
const PERCEPTUAL_BLACK_Y: f64 = 0.0034731;
/// `cmsPERCEPTUAL_BLACK_Z` (`include/lcms2.h:299`).
const PERCEPTUAL_BLACK_Z: f64 = 0.00287;

// Matrix-shaper detection tags (`cmsIsMatrixShaper`, `cmsio1.c:806-827`).
const TAG_GRAY_TRC: Signature = Signature::from_bytes(*b"kTRC");
const TAG_RED_COLORANT: Signature = Signature::from_bytes(*b"rXYZ");
const TAG_GREEN_COLORANT: Signature = Signature::from_bytes(*b"gXYZ");
const TAG_BLUE_COLORANT: Signature = Signature::from_bytes(*b"bXYZ");
const TAG_RED_TRC: Signature = Signature::from_bytes(*b"rTRC");
const TAG_GREEN_TRC: Signature = Signature::from_bytes(*b"gTRC");
const TAG_BLUE_TRC: Signature = Signature::from_bytes(*b"bTRC");

/// Outcome of black-point detection.
///
/// The two detection entry points either resolve the black point to a concrete
/// XYZ (the constant or a sampled value) or report that the device class/intent
/// makes the black point `{0,0,0}` (lcms2's `return FALSE`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlackPoint {
    /// A resolved black point (the V4-perceptual constant or a sampled value).
    Resolved(CIEXYZ),
    /// lcms2 returns `FALSE` with `BlackPoint = {0,0,0}` (Link/Abstract/NamedColor
    /// class, or an intent that is not perceptual/relative/saturation). The caller
    /// treats this as a `{0,0,0}` black point, exactly as the C does when it
    /// ignores the boolean return and reads the (zeroed) out-parameter.
    Zero,
}

/// Intermediate result of the shared detection head ([`detect_common`]): either a
/// fully-resolved outcome, or "fall through to sampling" (the C continues past the
/// V4 short-circuit into `BlackPointAsDarkerColorant` / the round-trip).
enum CommonOutcome {
    Done(BlackPoint),
    Sample,
}

/// `cmsIsMatrixShaper` (`cmsio1.c:806-827`): a Gray profile with a `kTRC` tag, or
/// an RGB profile with all three colorant + all three TRC tags. Everything else is
/// not a matrix shaper.
fn is_matrix_shaper(profile: &Profile) -> bool {
    match profile.header().color_space {
        ColorSpace::Gray => profile.has_tag(TAG_GRAY_TRC),
        ColorSpace::Rgb => {
            profile.has_tag(TAG_RED_COLORANT)
                && profile.has_tag(TAG_GREEN_COLORANT)
                && profile.has_tag(TAG_BLUE_COLORANT)
                && profile.has_tag(TAG_RED_TRC)
                && profile.has_tag(TAG_GREEN_TRC)
                && profile.has_tag(TAG_BLUE_TRC)
        }
        _ => false,
    }
}

/// The condition ladder shared by `cmsDetectBlackPoint` (`cmssamp.c:238-274`) and
/// the head of `cmsDetectDestinationBlackPoint` (`:413-445`), up to and including
/// the V4-perceptual-black short-circuit. Matrix shapers under V4 perc/sat use
/// `BlackPointAsDarkerColorant(REL_COL)` (cmssamp.c:268-269).
///
/// Returns [`CommonOutcome::Done`] for the resolved/zero cases, or
/// [`CommonOutcome::Sample`] when the C falls through past the short-circuit into
/// the sampling tail.
fn detect_common(profile: &Profile, intent: RenderingIntent) -> CommonOutcome {
    let dev_class = profile.header().device_class;

    // Make sure the device class is adequate (cmssamp.c:243-249).
    if dev_class == ProfileClass::Link
        || dev_class == ProfileClass::Abstract
        || dev_class == ProfileClass::NamedColor
    {
        return CommonOutcome::Done(BlackPoint::Zero);
    }

    // Make sure intent is adequate (cmssamp.c:252-257).
    if intent != RenderingIntent::Perceptual
        && intent != RenderingIntent::RelativeColorimetric
        && intent != RenderingIntent::Saturation
    {
        return CommonOutcome::Done(BlackPoint::Zero);
    }

    // v4 + perceptual & saturation intents have their own black point
    // (cmssamp.c:261-274).
    if profile.header().version >= ICC_VERSION_V4
        && (intent == RenderingIntent::Perceptual || intent == RenderingIntent::Saturation)
    {
        // Matrix shapers share MRC & perceptual intents → BlackPointAsDarkerColorant
        // with INTENT_RELATIVE_COLORIMETRIC (cmssamp.c:268-269).
        if is_matrix_shaper(profile) {
            return CommonOutcome::Sample;
        }

        // Fixed perceptual black for V4 profiles under perceptual & saturation.
        return CommonOutcome::Done(BlackPoint::Resolved(CIEXYZ {
            x: PERCEPTUAL_BLACK_X,
            y: PERCEPTUAL_BLACK_Y,
            z: PERCEPTUAL_BLACK_Z,
        }));
    }

    // Everything below this in cmsDetectBlackPoint is sampling-based.
    CommonOutcome::Sample
}

/// `cmsDetectBlackPoint` (`cmssamp.c:241-324`).
///
/// Resolves the device-class/intent guards + the V4-perceptual short-circuit via
/// [`detect_common`]; the sampling tail (`cmssamp.c:314-323`) is the ink-output
/// `BlackPointUsingPerceptualBlack` branch (rel-col + Output class + ink space)
/// else `BlackPointAsDarkerColorant`. The matrix-shaper V4 perc/sat case samples
/// with `INTENT_RELATIVE_COLORIMETRIC` (cmssamp.c:269).
pub fn detect_black_point(profile: &Profile, intent: RenderingIntent) -> Result<BlackPoint> {
    match detect_common(profile, intent) {
        CommonOutcome::Done(bp) => Ok(bp),
        CommonOutcome::Sample => {
            // Matrix-shaper V4 perc/sat → BlackPointAsDarkerColorant(REL_COL).
            let sampling_intent = if profile.header().version >= ICC_VERSION_V4
                && (intent == RenderingIntent::Perceptual || intent == RenderingIntent::Saturation)
                && is_matrix_shaper(profile)
            {
                RenderingIntent::RelativeColorimetric
            } else {
                intent
            };

            // If output profile, discount ink-limiting (cmssamp.c:316-320).
            if intent == RenderingIntent::RelativeColorimetric
                && profile.header().device_class == ProfileClass::Output
                && is_ink_colorspace(profile.header().color_space)
            {
                return black_point_using_perceptual_black(profile);
            }

            // Nope, compute BP using current intent (cmssamp.c:323).
            black_point_as_darker_colorant(profile, sampling_intent)
        }
    }
}

/// `cmsDetectDestinationBlackPoint` (`cmssamp.c:402-602`).
///
/// The head (`:416-448`) reuses [`detect_common`]. Then (`:451-460`) if the
/// profile is not a CLUT in the output direction, or its colorspace is not
/// gray/rgb/ink, it is handled as the input case via [`detect_black_point`].
/// Otherwise it runs the Adobe 256-sample round-trip over the Lab4 virtual.
pub fn detect_destination_black_point(
    profile: &Profile,
    intent: RenderingIntent,
) -> Result<BlackPoint> {
    match detect_common(profile, intent) {
        CommonOutcome::Done(bp) => return Ok(bp),
        CommonOutcome::Sample => {}
    }

    // Check if the profile is lut based and gray, rgb or cmyk (cmssamp.c:452-460).
    let color_space = profile.header().color_space;
    if !is_clut(profile, intent, Direction::Output)
        || (color_space != ColorSpace::Gray
            && color_space != ColorSpace::Rgb
            && !is_ink_colorspace(color_space))
    {
        // In this case, handle as input case.
        return detect_black_point(profile, intent);
    }

    destination_black_point_roundtrip(profile, intent)
}

// ---- Sampling-based detection (cmssamp.c) -----------------------------------

/// `isInkColorspace` (`cmssamp.c:192-233`): CMYK/CMY and the multichannel/N-color
/// "ink" spaces.
fn is_ink_colorspace(c: ColorSpace) -> bool {
    matches!(
        c,
        ColorSpace::Cmyk
            | ColorSpace::Cmy
            | ColorSpace::Mch1
            | ColorSpace::Mch2
            | ColorSpace::Mch3
            | ColorSpace::Mch4
            | ColorSpace::Mch5
            | ColorSpace::Mch6
            | ColorSpace::Mch7
            | ColorSpace::Mch8
            | ColorSpace::Mch9
            | ColorSpace::MchA
            | ColorSpace::MchB
            | ColorSpace::MchC
            | ColorSpace::MchD
            | ColorSpace::MchE
            | ColorSpace::MchF
            | ColorSpace::Color1
            | ColorSpace::Color2
            | ColorSpace::Color3
            | ColorSpace::Color4
            | ColorSpace::Color5
            | ColorSpace::Color6
            | ColorSpace::Color7
            | ColorSpace::Color8
            | ColorSpace::Color9
            | ColorSpace::Color10
            | ColorSpace::Color11
            | ColorSpace::Color12
            | ColorSpace::Color13
            | ColorSpace::Color14
            | ColorSpace::Color15
    )
}

/// Direction argument for [`is_clut`] / [`is_intent_supported`]
/// (`LCMS_USED_AS_INPUT` / `LCMS_USED_AS_OUTPUT`).
#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Input,
    Output,
}

// Device↔PCS LUT tags by intent (cmsio1.c:32-45).
const TAG_A2B0: Signature = Signature::from_bytes(*b"A2B0"); // Perceptual
const TAG_A2B1: Signature = Signature::from_bytes(*b"A2B1"); // Relative / Absolute
const TAG_A2B2: Signature = Signature::from_bytes(*b"A2B2"); // Saturation
const TAG_B2A0: Signature = Signature::from_bytes(*b"B2A0");
const TAG_B2A1: Signature = Signature::from_bytes(*b"B2A1");
const TAG_B2A2: Signature = Signature::from_bytes(*b"B2A2");

/// `cmsIsCLUT` (`cmsio1.c:830-857`). For devicelinks, the supported intent is the
/// one in the header. Otherwise the per-intent Device2PCS16/PCS2Device16 tag is
/// looked up. (`LCMS_USED_AS_PROOF` is not used by the black-point paths.)
fn is_clut(profile: &Profile, intent: RenderingIntent, dir: Direction) -> bool {
    if profile.header().device_class == ProfileClass::Link {
        return profile.header().rendering_intent == intent;
    }

    let table = match dir {
        Direction::Input => [TAG_A2B0, TAG_A2B1, TAG_A2B2, TAG_A2B1],
        Direction::Output => [TAG_B2A0, TAG_B2A1, TAG_B2A2, TAG_B2A1],
    };

    // Extended intents are not strictly CLUT-based (cmsio1.c:853).
    let idx = match intent {
        RenderingIntent::Perceptual => 0,
        RenderingIntent::RelativeColorimetric => 1,
        RenderingIntent::Saturation => 2,
        RenderingIntent::AbsoluteColorimetric => 3,
        RenderingIntent::Other(_) => return false,
    };
    profile.has_tag(table[idx])
}

/// `cmsIsIntentSupported` (`cmsio1.c:864-876`): CLUT-supported in the direction, or
/// a matrix shaper.
fn is_intent_supported(profile: &Profile, intent: RenderingIntent, dir: Direction) -> bool {
    if is_clut(profile, intent, dir) {
        return true;
    }
    is_matrix_shaper(profile)
}

/// `cmsFormatterForColorspaceOfProfile(hProfile, 2, FALSE)` (`cmspack.c:4025`) for
/// the `_cmsEndPointsBySpace` spaces (the only ones reachable in
/// `BlackPointAsDarkerColorant`): `FLOAT_SH(0) | COLORSPACE_SH(pt) | BYTES_SH(2) |
/// CHANNELS_SH(nchan)`.
fn darker_colorant_device_format(space: ColorSpace) -> Option<(u32, [u16; 4], usize)> {
    // (PT, darker-colorant 16-bit values, channel count). The values are
    // `_cmsEndPointsBySpace`'s `*Black` arrays (cmspcs.c:707-756).
    let (pt, black, nchan): (u32, [u16; 4], usize) = match space {
        ColorSpace::Gray => (PT_GRAY, [0, 0, 0, 0], 1),
        ColorSpace::Rgb => (PT_RGB, [0, 0, 0, 0], 3),
        ColorSpace::Lab => (PT_LAB, [0, 0x8080, 0x8080, 0], 3),
        ColorSpace::Cmyk => (PT_CMYK, [0xffff, 0xffff, 0xffff, 0xffff], 4),
        ColorSpace::Cmy => (PT_CMY, [0xffff, 0xffff, 0xffff, 0], 3),
        _ => return None,
    };
    // FLOAT_SH(0) | COLORSPACE_SH(pt) | BYTES_SH(2) | CHANNELS_SH(nchan).
    let fmt = (pt << 16) | ((nchan as u32) << 3) | 2;
    Some((fmt, black, nchan))
}

/// `BlackPointAsDarkerColorant` (`cmssamp.c:63-145`, lcms2 2.19.1). Builds a
/// one-way transform profile→Lab2 (lab2 avoids recursion), pushes the
/// darker-colorant 16-bit device value through, clips the read-back L*, and
/// converts to XYZ. (Unlike older trees, 2.19.1 does not force a=b=0.) Returns
/// [`BlackPoint::Zero`] for the `return FALSE` (zeroed out-param) cases.
// The L* clip is the `>95 → 0` / `<0 → 0` / `>50 → 50` ladder transcribed
// verbatim from cmssamp.c:126-131. It is NOT a clamp (the `>95 → 0` branch maps
// the high end to zero, not to the upper bound), so `manual_clamp` does not apply.
#[allow(clippy::manual_clamp)]
fn black_point_as_darker_colorant(
    profile: &Profile,
    intent: RenderingIntent,
) -> Result<BlackPoint> {
    // If the profile does not support input direction, assume Black point 0.
    if !is_intent_supported(profile, intent, Direction::Input) {
        return Ok(BlackPoint::Zero);
    }

    // Darker colorant + the 16-bit device format word for this color space.
    let space = profile.header().color_space;
    let Some((dev_format, black, nchan)) = darker_colorant_device_format(space) else {
        // _cmsEndPointsBySpace returned FALSE → Black point 0.
        return Ok(BlackPoint::Zero);
    };

    // Lab2 will be used as the output space (lab2 avoids recursion). Build it as
    // an IN-MEMORY profile (not serialize→reparse) so its identity CLUT links
    // bit-identically to lcms2's freshly-built cmsCreateLab2Profile. lcms2
    // (cmssamp.c:43-45) maps a NULL Lab2 profile to a {0,0,0} black point.
    let lab2 = match Profile::from_writable(&build_lab2_profile()) {
        Ok(p) => p,
        Err(_) => return Ok(BlackPoint::Zero),
    };

    // device(16-bit) → Lab2 (TYPE_Lab_DBL out), rel/intent, NOOPTIMIZE|NOCACHE.
    // lcms2 (cmssamp.c:53-57) maps an un-buildable transform to {0,0,0}, not a
    // hard failure. The up-front `is_intent_supported(.., Input)` guard already
    // ensures the forward direction exists, so this rarely fires — but mirroring
    // lcms2's NULL→zero keeps the error path bit-identical.
    let xform = match Transform::new_with_formats(
        &[profile, &lab2],
        &[intent, intent],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        dev_format,
        TYPE_LAB_DBL,
    ) {
        Ok(x) => x,
        Err(_) => return Ok(BlackPoint::Zero),
    };

    // Pack the darker colorant into a 16-bit native buffer and push it through.
    let mut in_buf = vec![0u8; nchan * 2];
    for (k, &v) in black.iter().take(nchan).enumerate() {
        in_buf[k * 2..k * 2 + 2].copy_from_slice(&v.to_ne_bytes());
    }
    let mut out_buf = vec![0u8; 3 * 8]; // TYPE_Lab_DBL: 3 doubles
    xform.do_transform(&in_buf, &mut out_buf, 1);

    let mut lab = CIELab {
        l: f64::from_ne_bytes(out_buf[0..8].try_into().unwrap()),
        a: f64::from_ne_bytes(out_buf[8..16].try_into().unwrap()),
        b: f64::from_ne_bytes(out_buf[16..24].try_into().unwrap()),
    };

    // Clip L* (cmssamp.c:126-131). NOTE: lcms2 2.19.1 does NOT zero a/b here (the
    // `Lab.a = Lab.b = 0` of older trees is gone), so the residual chroma carries
    // through to the XYZ — matched bit-for-bit.
    if lab.l > 95.0 {
        lab.l = 0.0; // for synthetical negative profiles
    } else if lab.l < 0.0 {
        lab.l = 0.0;
    } else if lab.l > 50.0 {
        lab.l = 50.0;
    }

    Ok(BlackPoint::Resolved(lab_to_xyz(None, lab)))
}

/// `BlackPointUsingPerceptualBlack` (`cmssamp.c:153-189`): Lab(0,0,0) →
/// [Perceptual] profile → CMYK → [RelCol] profile → Lab round trip, then clip.
fn black_point_using_perceptual_black(profile: &Profile) -> Result<BlackPoint> {
    // Is the perceptual intent supported in input direction?
    if !is_intent_supported(profile, RenderingIntent::Perceptual, Direction::Input) {
        return Ok(BlackPoint::Zero);
    }

    // lcms2 `BlackPointUsingPerceptualBlack` (cmssamp.c:165-168): if the
    // round-trip transform can't be built (e.g. an Output-class profile with an
    // `A2B0` but no `B2A0`, so the Lab→device leg has no tag), lcms2 sets the
    // black point to {0,0,0} and returns — it does NOT fail the parent transform.
    // tintbox previously propagated this error, aborting a build lcms2 completes;
    // map the un-buildable round-trip to a zero black point to stay bit-identical.
    let xform = match create_roundtrip_xform(profile, RenderingIntent::Perceptual) {
        Ok(x) => x,
        Err(_) => return Ok(BlackPoint::Zero),
    };

    // LabIn = (0,0,0).
    let lab_out = roundtrip_lab(
        &xform,
        CIELab {
            l: 0.0,
            a: 0.0,
            b: 0.0,
        },
    );

    // Clip Lab to reasonable limits (cmssamp.c:176-178).
    let l = if lab_out.l > 50.0 { 50.0 } else { lab_out.l };
    let lab = CIELab { l, a: 0.0, b: 0.0 };

    Ok(BlackPoint::Resolved(lab_to_xyz(None, lab)))
}

/// `CreateRoundtripXForm` (`cmssamp.c:41-59`): the 4-profile PCS→PCS round trip
/// Lab4 → profile → profile → Lab4, intents
/// `[REL, nIntent, REL, REL]`, BPC all false, adaptation all 1.0, TYPE_Lab_DBL
/// both ends, NOCACHE|NOOPTIMIZE.
fn create_roundtrip_xform(profile: &Profile, intent: RenderingIntent) -> Result<Transform> {
    // In-memory Lab4 virtuals (see `Profile::from_writable`): match lcms2's
    // freshly-built cmsCreateLab4Profile bit-for-bit.
    let lab4_a = Profile::from_writable(&build_lab4_profile())?;
    let lab4_b = Profile::from_writable(&build_lab4_profile())?;

    let rel = RenderingIntent::RelativeColorimetric;
    Transform::new_with_formats(
        &[&lab4_a, profile, profile, &lab4_b],
        &[rel, intent, rel, rel],
        &[false, false, false, false],
        &[1.0, 1.0, 1.0, 1.0],
        Flags::NOOPTIMIZE,
        TYPE_LAB_DBL,
        TYPE_LAB_DBL,
    )
}

/// Push one Lab through a TYPE_Lab_DBL→TYPE_Lab_DBL round-trip transform.
fn roundtrip_lab(xform: &Transform, lab_in: CIELab) -> CIELab {
    let mut in_buf = [0u8; 24];
    in_buf[0..8].copy_from_slice(&lab_in.l.to_ne_bytes());
    in_buf[8..16].copy_from_slice(&lab_in.a.to_ne_bytes());
    in_buf[16..24].copy_from_slice(&lab_in.b.to_ne_bytes());
    let mut out_buf = [0u8; 24];
    xform.do_transform(&in_buf, &mut out_buf, 1);
    CIELab {
        l: f64::from_ne_bytes(out_buf[0..8].try_into().unwrap()),
        a: f64::from_ne_bytes(out_buf[8..16].try_into().unwrap()),
        b: f64::from_ne_bytes(out_buf[16..24].try_into().unwrap()),
    }
}

/// `RootOfLeastSquaresFitQuadraticCurve` (`cmssamp.c:333-396`): least-squares fit
/// of a quadratic to `(x, y)` then return the clipped root/vertex.
fn root_of_least_squares_fit_quadratic_curve(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len();
    if n < 4 {
        return 0.0;
    }

    let (mut sum_x, mut sum_x2, mut sum_x3, mut sum_x4) = (0.0, 0.0, 0.0, 0.0);
    let (mut sum_y, mut sum_yx, mut sum_yx2) = (0.0, 0.0, 0.0);

    for i in 0..n {
        let xn = x[i];
        let yn = y[i];
        sum_x += xn;
        sum_x2 += xn * xn;
        sum_x3 += xn * xn * xn;
        sum_x4 += xn * xn * xn * xn;
        sum_y += yn;
        sum_yx += yn * xn;
        sum_yx2 += yn * xn * xn;
    }

    let m = Mat3([
        n as f64, sum_x, sum_x2, sum_x, sum_x2, sum_x3, sum_x2, sum_x3, sum_x4,
    ]);
    let v = Vec3([sum_y, sum_yx, sum_yx2]);

    let res = match m.solve(v) {
        Some(r) => r,
        None => return 0.0,
    };

    let a = res.0[2];
    let b = res.0[1];
    let c = res.0[0];

    if a.abs() < 1.0e-10 {
        if b.abs() < 1.0e-10 {
            return 0.0;
        }
        return (0.0f64).max((50.0f64).min(-c / b));
    }

    let d = b * b - 4.0 * a * c;
    if d <= 0.0 {
        return 0.0;
    }
    // (fabs(a) < 1E-10) re-check is dead here (a already passed the test above),
    // transcribed for fidelity but cannot fire.
    let rt = (-b + d.sqrt()) / (2.0 * a);
    (0.0f64).max((50.0f64).min(rt))
}

/// The Adobe 256-sample round-trip tail of `cmsDetectDestinationBlackPoint`
/// (`cmssamp.c:462-601`).
fn destination_black_point_roundtrip(
    profile: &Profile,
    intent: RenderingIntent,
) -> Result<BlackPoint> {
    // Set a first guess (cmssamp.c:466-484).
    let initial_lab = if intent == RenderingIntent::RelativeColorimetric {
        // calculate initial Lab as source black point.
        let ini = match detect_black_point(profile, intent)? {
            BlackPoint::Resolved(xyz) => xyz,
            // cmsDetectBlackPoint returned FALSE → the C returns FALSE here too,
            // leaving the out-param {0,0,0}. The caller reads zero.
            BlackPoint::Zero => return Ok(BlackPoint::Zero),
        };
        xyz_to_lab(None, ini)
    } else {
        CIELab {
            l: 0.0,
            a: 0.0,
            b: 0.0,
        }
    };

    // Step 2: create a roundtrip (cmssamp.c:491).
    let xform = create_roundtrip_xform(profile, intent)?;

    // Compute ramps (cmssamp.c:496-506).
    let mut in_ramp = [0.0f64; 256];
    let mut out_ramp = [0.0f64; 256];
    let a_clip = (-50.0f64).max(initial_lab.a).min(50.0);
    let b_clip = (-50.0f64).max(initial_lab.b).min(50.0);
    for l in 0..256 {
        let lab = CIELab {
            l: (l as f64 * 100.0) / 255.0,
            a: a_clip,
            b: b_clip,
        };
        let dest = roundtrip_lab(&xform, lab);
        in_ramp[l] = lab.l;
        out_ramp[l] = dest.l;
    }

    // Make monotonic (cmssamp.c:509-511).
    for l in (1..255).rev() {
        out_ramp[l] = out_ramp[l].min(out_ramp[l + 1]);
    }

    // Check (cmssamp.c:514-519). Transcribed verbatim as `!(a < b)` — NOT `a >= b`:
    // the C uses `!(outRamp[0] < outRamp[255])`, and with a NaN ramp value the two
    // forms differ, so we keep the negated `<` for bit-exact parity.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let descending_or_flat = !(out_ramp[0] < out_ramp[255]);
    if descending_or_flat {
        return Ok(BlackPoint::Zero);
    }

    // Test for mid range straight (cmssamp.c:523-544).
    let min_l = out_ramp[0];
    let max_l = out_ramp[255];
    if intent == RenderingIntent::RelativeColorimetric {
        let mut nearly_straight_midrange = true;
        for l in 0..256 {
            if !((in_ramp[l] <= min_l + 0.2 * (max_l - min_l))
                || ((in_ramp[l] - out_ramp[l]).abs() < 4.0))
            {
                nearly_straight_midrange = false;
            }
        }
        if nearly_straight_midrange {
            return Ok(BlackPoint::Resolved(lab_to_xyz(None, initial_lab)));
        }
    }

    // Curve fitting (cmssamp.c:549-552).
    let mut y_ramp = [0.0f64; 256];
    for l in 0..256 {
        y_ramp[l] = (out_ramp[l] - min_l) / (max_l - min_l);
    }

    // (cmssamp.c:555-564).
    let (lo, hi) = if intent == RenderingIntent::RelativeColorimetric {
        (0.1, 0.5)
    } else {
        (0.03, 0.25)
    };

    // Capture shadow points (cmssamp.c:567-577).
    let mut x = Vec::with_capacity(256);
    let mut y = Vec::with_capacity(256);
    for l in 0..256 {
        let ff = y_ramp[l];
        if ff >= lo && ff < hi {
            x.push(in_ramp[l]);
            y.push(y_ramp[l]);
        }
    }

    // No suitable points (cmssamp.c:580-585).
    if x.len() < 3 {
        return Ok(BlackPoint::Zero);
    }

    // Fit and get the vertex (cmssamp.c:589-598).
    let mut l = root_of_least_squares_fit_quadratic_curve(&x, &y);
    if l < 0.0 {
        l = 0.0;
    }
    let lab = CIELab {
        l,
        a: initial_lab.a,
        b: initial_lab.b,
    };
    Ok(BlackPoint::Resolved(lab_to_xyz(None, lab)))
}

/// `ComputeBlackPointCompensation` (`cmscnvrt.c:169-200`). Black-point compensation
/// as a linear scaling in XYZ: a diagonal matrix `m` plus offset `off` such that
/// `m·bp_in + off = bp_out` and `m·D50 + off = D50`.
///
/// Per-axis, with `t = bp_in − D50`:
/// - `a = (bp_out − D50) / t`
/// - `b = − D50 · (bp_out − bp_in) / t`
///
/// Black points come relative to the white point (D50 in the PCS). `m` is diagonal
/// `(ax, ay, az)`; `off` is `(bx, by, bz)`.
pub fn compute_black_point_compensation(bp_in: &CIEXYZ, bp_out: &CIEXYZ) -> (Mat3, Vec3) {
    let tx = bp_in.x - D50.x;
    let ty = bp_in.y - D50.y;
    let tz = bp_in.z - D50.z;

    let ax = (bp_out.x - D50.x) / tx;
    let ay = (bp_out.y - D50.y) / ty;
    let az = (bp_out.z - D50.z) / tz;

    let bx = -D50.x * (bp_out.x - bp_in.x) / tx;
    let by = -D50.y * (bp_out.y - bp_in.y) / ty;
    let bz = -D50.z * (bp_out.z - bp_in.z) / tz;

    let m = Mat3([ax, 0.0, 0.0, 0.0, ay, 0.0, 0.0, 0.0, az]);
    let off = Vec3([bx, by, bz]);
    (m, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str) -> Profile<'static> {
        let bytes: &'static [u8] = Box::leak(
            std::fs::read(format!(
                "{}/../../vendor/Little-CMS/testbed/{}",
                env!("CARGO_MANIFEST_DIR"),
                name
            ))
            .unwrap()
            .into_boxed_slice(),
        );
        Profile::open(bytes).unwrap()
    }

    // ---- compute_black_point_compensation -----------------------------------

    #[test]
    fn bpc_identity_when_black_points_equal() {
        // bp_in == bp_out ⇒ a = 1 on every axis, b = 0. (The caller skips BPC in
        // this case, but the math must still be the identity.)
        let bp = CIEXYZ {
            x: 0.01,
            y: 0.012,
            z: 0.009,
        };
        let (m, off) = compute_black_point_compensation(&bp, &bp);
        assert_eq!(m, Mat3([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
        assert_eq!(off, Vec3([0.0, 0.0, 0.0]));
    }

    #[test]
    fn bpc_matches_hand_computed() {
        // Hand-compute a=(bpout-D50)/(bpin-D50), b=-D50*(bpout-bpin)/(bpin-D50).
        let bp_in = CIEXYZ {
            x: 0.002,
            y: 0.0025,
            z: 0.0015,
        };
        let bp_out = CIEXYZ {
            x: 0.00336,
            y: 0.0034731,
            z: 0.00287,
        };
        let (m, off) = compute_black_point_compensation(&bp_in, &bp_out);

        let tx = bp_in.x - D50.x;
        let ty = bp_in.y - D50.y;
        let tz = bp_in.z - D50.z;
        let ax = (bp_out.x - D50.x) / tx;
        let ay = (bp_out.y - D50.y) / ty;
        let az = (bp_out.z - D50.z) / tz;
        let bx = -D50.x * (bp_out.x - bp_in.x) / tx;
        let by = -D50.y * (bp_out.y - bp_in.y) / ty;
        let bz = -D50.z * (bp_out.z - bp_in.z) / tz;

        let expected_m = Mat3([ax, 0.0, 0.0, 0.0, ay, 0.0, 0.0, 0.0, az]);
        let expected_off = Vec3([bx, by, bz]);
        for k in 0..9 {
            assert_eq!(m.0[k].to_bits(), expected_m.0[k].to_bits(), "m[{k}]");
        }
        for k in 0..3 {
            assert_eq!(off.0[k].to_bits(), expected_off.0[k].to_bits(), "off[{k}]");
        }

        // Invariant: m·D50 + off == D50 (the white point is fixed).
        let wx = ax * D50.x + bx;
        let wy = ay * D50.y + by;
        let wz = az * D50.z + bz;
        assert!((wx - D50.x).abs() < 1e-12);
        assert!((wy - D50.y).abs() < 1e-12);
        assert!((wz - D50.z).abs() < 1e-12);

        // Invariant: m·bp_in + off == bp_out (the black point maps through).
        let ox = ax * bp_in.x + bx;
        let oy = ay * bp_in.y + by;
        let oz = az * bp_in.z + bz;
        assert!((ox - bp_out.x).abs() < 1e-12);
        assert!((oy - bp_out.y).abs() < 1e-12);
        assert!((oz - bp_out.z).abs() < 1e-12);
    }

    // ---- detection: V4-perceptual-black-constant ----------------------------

    #[test]
    fn v4_perceptual_returns_constant_for_clut_profile() {
        // A V4 CLUT (non-matrix-shaper) profile under perceptual/saturation ⇒
        // the fixed perceptual black. test4.icc is V4 RGB/Lab abstract (CLUT).
        let want = CIEXYZ {
            x: PERCEPTUAL_BLACK_X,
            y: PERCEPTUAL_BLACK_Y,
            z: PERCEPTUAL_BLACK_Z,
        };
        let p = load("test4.icc");
        assert!(p.header().version >= ICC_VERSION_V4);
        assert!(!is_matrix_shaper(&p));
        assert_eq!(
            detect_black_point(&p, RenderingIntent::Perceptual).unwrap(),
            BlackPoint::Resolved(want)
        );
        assert_eq!(
            detect_black_point(&p, RenderingIntent::Saturation).unwrap(),
            BlackPoint::Resolved(want)
        );
    }

    #[test]
    fn inadequate_intent_is_zero() {
        // An intent outside {perceptual, relative, saturation} ⇒ Zero.
        let p = load("crayons.icc");
        assert_eq!(
            detect_black_point(&p, RenderingIntent::AbsoluteColorimetric).unwrap(),
            BlackPoint::Zero
        );
    }

    // ---- detection-by-sampling: bit-exact vs lcms2 --------------------------

    /// All four ICC intents in raw order.
    const ALL_INTENTS: [RenderingIntent; 4] = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ];

    /// Testbed profiles that load cleanly and exercise the detection paths
    /// (matrix shapers, CMYK ink output, RGB/Lab CLUTs, V2 + V4). `toosmall.icc` is
    /// a deliberately truncated profile that does not parse, so it is excluded.
    const DETECT_PROFILES: [&str; 7] = [
        "crayons.icc", // V4 RGB matrix shaper
        "ibm-t61.icc", // V2 RGB matrix shaper
        "new.icc",     // V2 RGB matrix shaper
        "test1.icc",   // V2 CMYK printer (ink output)
        "test2.icc",   // V2 CMYK printer (ink output)
        "test3.icc",   // V2 RGB/Lab CLUT
        "test4.icc",   // V4 RGB/Lab CLUT
    ];

    fn read_bytes(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../vendor/Little-CMS/testbed/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .unwrap()
    }

    fn bp_xyz(bp: BlackPoint) -> [f64; 3] {
        match bp {
            BlackPoint::Resolved(xyz) => [xyz.x, xyz.y, xyz.z],
            BlackPoint::Zero => [0.0, 0.0, 0.0],
        }
    }

    #[test]
    fn detect_black_point_bit_exact_vs_lcms2() {
        let mut cells = 0;
        for name in DETECT_PROFILES {
            let bytes = read_bytes(name);
            let p = Profile::open(&bytes).unwrap();
            for intent in ALL_INTENTS {
                let mine = bp_xyz(detect_black_point(&p, intent).unwrap());
                let (_, theirs) = tintbox_oracle::detect_black_point(&bytes, intent.to_raw(), 0);
                for k in 0..3 {
                    assert_eq!(
                        mine[k].to_bits(),
                        theirs[k].to_bits(),
                        "detect_black_point mismatch: {name} intent={} axis={k} (mine={} theirs={})",
                        intent.to_raw(),
                        mine[k],
                        theirs[k]
                    );
                }
                cells += 1;
            }
        }
        assert_eq!(cells, DETECT_PROFILES.len() * 4);
    }

    #[test]
    fn detect_destination_black_point_bit_exact_vs_lcms2() {
        let mut cells = 0;
        for name in DETECT_PROFILES {
            let bytes = read_bytes(name);
            let p = Profile::open(&bytes).unwrap();
            for intent in ALL_INTENTS {
                let mine = bp_xyz(detect_destination_black_point(&p, intent).unwrap());
                let (_, theirs) =
                    tintbox_oracle::detect_destination_black_point(&bytes, intent.to_raw(), 0);
                for k in 0..3 {
                    assert_eq!(
                        mine[k].to_bits(),
                        theirs[k].to_bits(),
                        "detect_destination_black_point mismatch: {name} intent={} axis={k} (mine={} theirs={})",
                        intent.to_raw(),
                        mine[k],
                        theirs[k]
                    );
                }
                cells += 1;
            }
        }
        assert_eq!(cells, DETECT_PROFILES.len() * 4);
    }

    // ---- BPC transform that previously hit Unsupported, now bit-identical -----

    #[test]
    fn bpc_transform_via_sampling_matches_lcms2() {
        // Slice 5 left a BPC link Unsupported whenever a black point needed
        // detection-by-sampling. Build such a transform (RGB matrix-shaper input →
        // CMYK ink-output printer, relative colorimetric, BPC ON, NOOPTIMIZE) and
        // diff the transformed pixels vs lcms2. Source black point is sampled via
        // BlackPointAsDarkerColorant; the CMYK destination via the round-trip.
        use crate::format::decode::{TYPE_CMYK_16, TYPE_RGB_16};
        use crate::transform::{Flags, Transform};

        let in_bytes = read_bytes("test5.icc"); // V2 RGB monitor (matrix shaper)
        let out_bytes = read_bytes("test1.icc"); // V2 CMYK printer (ink output, CLUT)
        let pin = Profile::open(&in_bytes).unwrap();
        let pout = Profile::open(&out_bytes).unwrap();

        let intent = RenderingIntent::RelativeColorimetric;
        let xform = Transform::new_with_formats(
            &[&pin, &pout],
            &[intent, intent],
            &[true, true], // BPC ON
            &[1.0, 1.0],
            Flags::NOOPTIMIZE,
            TYPE_RGB_16,
            TYPE_CMYK_16,
        )
        .expect("BPC link via detection-by-sampling must succeed (slice-5 deferral closed)");

        // A spread of RGB16 inputs.
        let inputs: [[u16; 3]; 6] = [
            [0, 0, 0],
            [0xffff, 0xffff, 0xffff],
            [0x8000, 0x4000, 0xc000],
            [0x1234, 0x5678, 0x9abc],
            [0xfedc, 0xba98, 0x7654],
            [0x0101, 0x8080, 0xfefe],
        ];

        let mut in_buf = Vec::with_capacity(inputs.len() * 6);
        for px in &inputs {
            for &c in px {
                in_buf.extend_from_slice(&c.to_ne_bytes());
            }
        }
        let n = inputs.len();
        let mut mine = vec![0u8; n * 8]; // CMYK16 = 8 bytes/pixel
        xform.do_transform(&in_buf, &mut mine, n);

        let mut theirs = vec![0u8; n * 8];
        let ok = tintbox_oracle::do_transform_packed(
            &[&in_bytes, &out_bytes],
            &[intent.to_raw(), intent.to_raw()],
            &[true, true],
            &[1.0, 1.0],
            TYPE_RGB_16,
            TYPE_CMYK_16,
            &in_buf,
            &mut theirs,
            n,
        );
        assert!(ok, "lcms2 BPC transform failed to build");
        assert_eq!(
            mine, theirs,
            "BPC-via-sampling transformed pixels differ from lcms2"
        );
    }
}
