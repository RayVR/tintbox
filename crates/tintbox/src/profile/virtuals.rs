//! Virtual / built-in profiles (lcms2 `cmsvirt.c`), the byte-exact constructors
//! behind `cmsCreate*Profile`.
//!
//! Each `build_*` assembles an in-memory [`WritableProfile`] — header fields plus
//! the ordered tag table — that [`crate::profile::serialize::save_to_mem`]
//! serializes byte-for-byte identically to lcms2's `cmsCreate*Profile` followed
//! by `cmsSaveProfileToMem`. The construction mirrors each `cmsCreate*` routine:
//! the same tags, in the same insertion (directory) order, built from the same
//! color math (`crate::adapt` for the RGB→XYZ colorant matrix + Bradford chad,
//! `crate::curve` for the tone curves, `crate::pipeline` for the LUTs).
//!
//! ## Determinism
//! lcms2's `cmsCreateProfilePlaceholder` seeds the header's CMM/creator/platform
//! with `'lcms'`/Mac-or-MS and the creation date from the wall clock. The
//! differential oracle overrides those four fields to fixed values after calling
//! the real constructor; these builders use the SAME fixed values
//! ([`virtual_header`]) so the byte streams match. Every other header field is
//! exactly what the C `cmsCreate*` sets (version/class/space/PCS/intent), and
//! `flags`/`manufacturer`/`model`/`attributes`/`profileID` are the placeholder's
//! zeroed defaults.

use crate::adapt::build_rgb2xyz_transfer_matrix;
use crate::color::{CIExyY, CIExyYTriple, CIEXYZ};
use crate::curve::{build_gamma, build_parametric, build_tabulated_16, ToneCurve};
use crate::interp::InterpParams;
use crate::math::matrix::Mat3;
use crate::math::whitepoint::D50;
use crate::pcs::xyz_to_xyy;
use crate::pipeline::clut::{Clut, ClutTable};
use crate::pipeline::{Pipeline, Stage};
use crate::profile::header::{ColorSpace, DateTime, Header, ProfileClass, RenderingIntent};
use crate::profile::serialize::WritableProfile;
use crate::profile::tag::{Mlu, ProfileIdItem, ProfileSequenceItem, Tag};
use crate::sig::Signature;

// ---- tag signatures (lcms2 `cmsTagSignature`) -----------------------------
const TAG_DESC: Signature = Signature::from_bytes(*b"desc");
const TAG_CPRT: Signature = Signature::from_bytes(*b"cprt");
const TAG_WTPT: Signature = Signature::from_bytes(*b"wtpt");
const TAG_CHAD: Signature = Signature::from_bytes(*b"chad");
const TAG_RXYZ: Signature = Signature::from_bytes(*b"rXYZ");
const TAG_GXYZ: Signature = Signature::from_bytes(*b"gXYZ");
const TAG_BXYZ: Signature = Signature::from_bytes(*b"bXYZ");
const TAG_RTRC: Signature = Signature::from_bytes(*b"rTRC");
const TAG_GTRC: Signature = Signature::from_bytes(*b"gTRC");
const TAG_BTRC: Signature = Signature::from_bytes(*b"bTRC");
const TAG_KTRC: Signature = Signature::from_bytes(*b"kTRC");
const TAG_CHRM: Signature = Signature::from_bytes(*b"chrm");
const TAG_A2B0: Signature = Signature::from_bytes(*b"A2B0");
const TAG_B2A0: Signature = Signature::from_bytes(*b"B2A0");
const TAG_PSEQ: Signature = Signature::from_bytes(*b"pseq");
const TAG_PSID: Signature = Signature::from_bytes(*b"psid");

/// `cmsD50_XYZ()` as a [`CIEXYZ`] (lcms2 `cmsD50{X,Y,Z}`).
fn d50_xyz() -> CIEXYZ {
    D50
}

/// `cmsD50_xyY()` (`cmswtpnt.c:38`): `cmsXYZ2xyY(cmsD50_XYZ())`.
fn d50_xyy() -> CIExyY {
    xyz_to_xyy(D50)
}

/// `cmsxyY2XYZ` (`cmspcs.c:102`): convert an xyY chromaticity to XYZ.
fn xyy_to_xyz(s: CIExyY) -> CIEXYZ {
    CIEXYZ {
        x: (s.x / s.y) * s.yy,
        y: s.yy,
        z: ((1.0 - s.x - s.y) / s.y) * s.yy,
    }
}

/// The deterministic header shared by every virtual profile: the four
/// nondeterministic placeholder fields (CMM/creator/platform/date) pinned to the
/// same fixed values the oracle uses, the rest as the constructor sets them. The
/// caller overrides `version`/`device_class`/`color_space`/`pcs`/`rendering_intent`.
fn virtual_header() -> Header {
    Header {
        size: 0, // patched by the serializer.
        cmm: Signature::from_raw(0),
        version: 0x0440_0000, // overridden per constructor.
        device_class: ProfileClass::Display,
        color_space: ColorSpace::Rgb,
        pcs: ColorSpace::XYZ,
        date: DateTime {
            year: 2026,
            month: 6,
            day: 15,
            hours: 12,
            minutes: 34,
            seconds: 56,
        },
        platform: Signature::from_raw(0),
        flags: 0,
        manufacturer: Signature::from_raw(0),
        model: 0,
        attributes: 0,
        rendering_intent: RenderingIntent::Perceptual,
        // The serializer always writes D50 at the illuminant field; the value
        // here is unused but kept consistent.
        illuminant: D50,
        creator: Signature::from_raw(0),
        profile_id: [0u8; 16],
    }
}

/// lcms2 `SetTextTags(hProfile, Description)` (`cmsvirt.c:33`): write the profile
/// description and a fixed copyright, both as one-entry `en`/`US` MLUs. At v4 these
/// serialize as `mluc`; at v2 as `desc` (description) / `text` (copyright) via the
/// DecideType deciders. Appends in `desc`, `cprt` order; re-issuing replaces the
/// existing slots in place (we model lcms2's slot reuse by writing them once with
/// the FINAL description string).
fn set_text_tags(p: &mut WritableProfile, description: &str) {
    let desc = Mlu::from_wide(*b"en", *b"US", description);
    let cprt = Mlu::from_wide(*b"en", *b"US", "No copyright, use freely");
    p.add_tag(TAG_DESC, Tag::Mlu(desc));
    p.add_tag(TAG_CPRT, Tag::Mlu(cprt));
}

/// The Bradford D50→D50 (or WhitePoint→D50) chromatic-adaptation tag value
/// (`cmsSigChromaticAdaptationTag`, an `sf32` of 9 row-major s15Fixed16). lcms2
/// computes `_cmsAdaptationMatrix(NULL, WhitePointXYZ, D50)` and stores the MAT3
/// in memory (row-major) order.
fn chad_tag(white_point_xyz: CIEXYZ) -> Tag {
    let m = crate::adapt::adaptation_matrix(None, white_point_xyz, d50_xyz())
        .expect("Bradford adaptation to D50 is always invertible for valid white points");
    mat3_to_sf32(&m)
}

/// A [`Mat3`] (row-major) as an `S15Fixed16Array` tag of 9 entries — the on-disk
/// `chad` layout (lcms2 stores the `cmsMAT3` straight through `Type_S15Fixed16_Write`).
fn mat3_to_sf32(m: &Mat3) -> Tag {
    Tag::S15Fixed16Array(
        m.0.iter()
            .map(|&v| crate::fixed::S15Fixed16::from_f64(v))
            .collect(),
    )
}

/// An XYZ-valued tag from raw components.
fn xyz_tag(x: f64, y: f64, z: f64) -> Tag {
    Tag::Xyz(CIEXYZ { x, y, z })
}

/// Build a 2-point-per-axis identity CLUT pipeline (3→3), exactly as lcms2's
/// `_cmsStageAllocIdentityCLut(3)` + `IdentitySampler`: an `nChan`-input grid with
/// 2 samples per axis whose every node's output equals its (quantized) input
/// coordinate. The node iteration order matches `cmsStageSampleCLut16bit` (last
/// input dimension varies fastest), and each coordinate is
/// `_cmsQuantizeVal(c, 2)` = `c == 0 ? 0 : 65535`.
fn identity_clut_pipeline(n_chan: usize) -> Pipeline {
    let grid: Vec<u32> = vec![2u32; n_chan];
    let params = InterpParams::new(&grid, n_chan, n_chan);

    let n_total: usize = 1usize << n_chan; // 2^n_chan nodes.
    let mut table = Vec::with_capacity(n_total * n_chan);
    for i in 0..n_total {
        // Decompose i into per-axis colorants, last axis fastest (matches the C).
        let mut coords = vec![0u16; n_chan];
        let mut rest = i;
        for t in (0..n_chan).rev() {
            let colorant = rest % 2;
            rest /= 2;
            coords[t] = quantize_val(colorant as u32, 2);
        }
        // IdentitySampler copies In -> Out.
        table.extend_from_slice(&coords);
    }

    let mut lut = Pipeline::new(n_chan, n_chan);
    lut.insert_stage_at_end(Stage::Clut(Clut {
        table: ClutTable::U16(table),
        params,
        is_trilinear: false,
        // `_cmsStageAllocIdentityCLut` sets Implements = cmsSigIdentityElemType
        // (cmslut.c:730), so PreOptimize drops this stage even under NOOPTIMIZE.
        implements_identity: true,
        resolved: Default::default(),
    }))
    .expect("identity CLUT stage shape is valid");
    lut
}

/// lcms2 `_cmsQuantizeVal(i, MaxSamples)` (`cmslut.c:737`): `round(i * 65535 /
/// (MaxSamples - 1))` saturated to u16.
fn quantize_val(i: u32, max_samples: u32) -> u16 {
    use crate::compat::floor::{FloorStrategy, Lcms2Floor};
    let x = (i as f64 * 65535.0) / (max_samples - 1) as f64;
    Lcms2Floor::quick_saturate_word(x)
}

/// `_cmsStageAllocIdentityCurves(n)` (`cmslut.c:300`): a tone-curve stage of `n`
/// gamma-1.0 (`cmsBuildGamma(1.0)`) identity curves.
///
/// NOTE: lcms2 marks this stage `Implements == cmsSigIdentityElemType`, so its
/// `_Remove1Op` drops it during `PreOptimize`. tintbox does NOT set the identity
/// flag here, so `pre_optimize` keeps it. This is currently harmless: the only
/// consumers (Lab4/XYZ `A2B0`) are always serialized→reparsed before linking, at
/// which point lcms2 ALSO sees an unmarked curve stage (a disk curve defaults to
/// its own type). It would become a real divergence only if a future caller links
/// one of these curve virtuals NATIVE (un-reparsed) — set the identity flag then.
fn identity_curves_stage(n: usize) -> Stage {
    Stage::ToneCurves((0..n).map(|_| build_gamma(1.0)).collect())
}

// ---- constructors ----------------------------------------------------------

/// `cmsCreateRGBProfileTHR` (`cmsvirt.c:101`): a v4.4 Display RGB/XYZ profile from
/// a white point, RGB primaries, and three transfer curves. Tags in constructor
/// order: `desc`, `cprt`, `wtpt` (literal D50), `chad` (WhitePoint→D50 Bradford),
/// `rXYZ`, `bXYZ`, `gXYZ` (note R, B, G order — `cmsvirt.c:174-176`), `rTRC`,
/// `gTRC`, `bTRC` (the latter two linked to `rTRC` when their curve equals the
/// red one), `chrm`. The colorant XYZ come from
/// [`build_rgb2xyz_transfer_matrix`]. `description` is the `SetTextTags` string.
pub fn build_rgb_profile(
    white_point: CIExyY,
    primaries: CIExyYTriple,
    transfer: &ToneCurve,
    description: &str,
) -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Display,
        color_space: ColorSpace::Rgb,
        pcs: ColorSpace::XYZ,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    set_text_tags(&mut p, description);

    // WhitePoint present: wtpt = literal D50, chad = Bradford(WhitePoint -> D50).
    p.add_tag(TAG_WTPT, Tag::Xyz(d50_xyz()));
    let wp_xyz = xyy_to_xyz(white_point);
    p.add_tag(TAG_CHAD, chad_tag(wp_xyz));

    // Colorants from the RGB->XYZ transfer matrix (columns = R, G, B XYZ).
    let m = build_rgb2xyz_transfer_matrix(white_point, primaries)
        .expect("RGB primaries matrix is invertible for valid primaries");
    // r.v[row].n[col]: red = column 0, green = column 1, blue = column 2.
    let red = xyz_tag(m.0[0], m.0[3], m.0[6]);
    let green = xyz_tag(m.0[1], m.0[4], m.0[7]);
    let blue = xyz_tag(m.0[2], m.0[5], m.0[8]);
    p.add_tag(TAG_RXYZ, red);
    p.add_tag(TAG_BXYZ, blue); // R, B, G order (cmsvirt.c:174-176).
    p.add_tag(TAG_GXYZ, green);

    // TRCs: red carries the body; green/blue link to red (all three are the same
    // curve in sRGB / the gamma-only RGB builder).
    p.add_tag(TAG_RTRC, Tag::Curve(transfer.clone()));
    p.link_tag(TAG_GTRC, TAG_RTRC);
    p.link_tag(TAG_BTRC, TAG_RTRC);

    // chrm: the primaries chromaticities.
    p.add_tag(TAG_CHRM, Tag::Chromaticity(primaries));

    p
}

/// `Build_sRGBGamma` (`cmsvirt.c:639`): the sRGB transfer curve, parametric type 4
/// with `{g=2.4, a=1/1.055, b=0.055/1.055, c=1/12.92, d=0.04045}`.
fn build_srgb_gamma() -> ToneCurve {
    build_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])
        .expect("ICC parametric type 4 with 5 params is always valid")
}

/// `cmsCreate_sRGBProfileTHR` (`cmsvirt.c:653`): the sRGB virtual profile.
/// Rec709 primaries + D65 white point + the sRGB gamma curve through
/// [`build_rgb_profile`], with the description finally set to `"sRGB built-in"`
/// (lcms2 re-runs `SetTextTags` after the RGB builder, replacing desc/cprt in
/// place).
pub fn build_srgb_profile() -> WritableProfile {
    let d65 = CIExyY {
        x: 0.3127,
        y: 0.3290,
        yy: 1.0,
    };
    let rec709 = CIExyYTriple {
        red: CIExyY {
            x: 0.6400,
            y: 0.3300,
            yy: 1.0,
        },
        green: CIExyY {
            x: 0.3000,
            y: 0.6000,
            yy: 1.0,
        },
        blue: CIExyY {
            x: 0.1500,
            y: 0.0600,
            yy: 1.0,
        },
    };
    // lcms2 sets "RGB built-in" in the RGB builder, then overwrites desc/cprt with
    // "sRGB built-in"; the final stored description is "sRGB built-in".
    build_rgb_profile(d65, rec709, &build_srgb_gamma(), "sRGB built-in")
}

/// `cmsCreateGrayProfileTHR` (`cmsvirt.c:227`): a v4.4 Display Gray/XYZ profile.
/// Tags: `desc`, `cprt`, `wtpt` (= `cmsxyY2XYZ(WhitePoint)`, NOT literal D50),
/// `kTRC` (the gray transfer curve).
pub fn build_gray_profile(white_point: CIExyY, transfer: &ToneCurve) -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Display,
        color_space: ColorSpace::Gray,
        pcs: ColorSpace::XYZ,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    set_text_tags(&mut p, "gray built-in");
    p.add_tag(TAG_WTPT, Tag::Xyz(xyy_to_xyz(white_point)));
    p.add_tag(TAG_KTRC, Tag::Curve(transfer.clone()));
    p
}

/// `cmsCreateLab2ProfileTHR` (`cmsvirt.c:473`): a v2.1 Abstract Lab/Lab identity.
/// Built atop the RGB builder with `WhitePoint = D50_xyY`, no primaries/TRC (so
/// only `desc`, `cprt`, `wtpt`, `chad` carry over), then re-classed to v2.1
/// Abstract Lab/Lab with desc/cprt replaced and an `A2B0` identity 3D CLUT
/// appended. Final order: `desc`, `cprt`, `wtpt`, `chad`, `A2B0`.
pub fn build_lab2_profile() -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0210_0000, // v2.1
        device_class: ProfileClass::Abstract,
        color_space: ColorSpace::Lab,
        pcs: ColorSpace::Lab,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    // From the RGB builder (WhitePoint = D50, no primaries/TRC): desc, cprt, wtpt,
    // chad. The description is finally "Lab identity built-in".
    set_text_tags(&mut p, "Lab identity built-in");
    p.add_tag(TAG_WTPT, Tag::Xyz(d50_xyz()));
    p.add_tag(TAG_CHAD, chad_tag(xyy_to_xyz(d50_xyy())));

    // A2B0 = identity 3D CLUT (2 points/axis).
    p.add_tag(TAG_A2B0, Tag::Lut(identity_clut_pipeline(3)));
    p
}

/// `cmsCreateLab4ProfileTHR` (`cmsvirt.c:524`): a v4.4 Abstract Lab/Lab identity.
/// Built atop the RGB builder with NO white point (so only `desc`, `cprt` carry
/// over), then re-classed to v4.4 Abstract Lab/Lab with `wtpt` (D50) added,
/// desc/cprt replaced, and an `A2B0` identity-curves LUT appended. Final order:
/// `desc`, `cprt`, `wtpt`, `A2B0`.
pub fn build_lab4_profile() -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Abstract,
        color_space: ColorSpace::Lab,
        pcs: ColorSpace::Lab,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    // RGB builder with NULL white point: only desc, cprt. Then wtpt(D50) is added
    // by the Lab4 path, and desc/cprt are replaced (already present at 0,1).
    set_text_tags(&mut p, "Lab identity built-in");
    p.add_tag(TAG_WTPT, Tag::Xyz(d50_xyz()));

    // A2B0 = identity curves (3 gamma-1.0 curves).
    let mut lut = Pipeline::new(3, 3);
    lut.insert_stage_at_end(identity_curves_stage(3))
        .expect("identity curves stage is valid");
    p.add_tag(TAG_A2B0, Tag::Lut(lut));
    p
}

/// `cmsCreateXYZProfileTHR` (`cmsvirt.c:577`): a v4.4 Abstract XYZ/XYZ identity.
/// Like Lab2's carry-over (RGB builder with `WhitePoint = D50_xyY` → desc, cprt,
/// wtpt, chad), re-classed to v4.4 Abstract XYZ/XYZ with desc/cprt replaced and an
/// `A2B0` identity-curves LUT appended. Final order: `desc`, `cprt`, `wtpt`,
/// `chad`, `A2B0`.
pub fn build_xyz_profile() -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Abstract,
        color_space: ColorSpace::XYZ,
        pcs: ColorSpace::XYZ,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    set_text_tags(&mut p, "XYZ identity built-in");
    p.add_tag(TAG_WTPT, Tag::Xyz(d50_xyz()));
    p.add_tag(TAG_CHAD, chad_tag(xyy_to_xyz(d50_xyy())));

    let mut lut = Pipeline::new(3, 3);
    lut.insert_stage_at_end(identity_curves_stage(3))
        .expect("identity curves stage is valid");
    p.add_tag(TAG_A2B0, Tag::Lut(lut));
    p
}

/// `cmsCreateNULLProfileTHR` (`cmsvirt.c:960`): a v4.4 Output Gray/Lab profile that
/// always returns black. Tags in order: `desc`, `cprt`, `B2A0`, `wtpt` (D50). The
/// `B2A0` pipeline is `ToneCurves[zero;3] -> Matrix(1x3 pick-L) -> ToneCurves[zero;1]`,
/// each curve a 2-entry `{0,0}` table.
pub fn build_null_profile() -> WritableProfile {
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Output,
        color_space: ColorSpace::Gray,
        pcs: ColorSpace::Lab,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    set_text_tags(&mut p, "NULL profile built-in");

    // B2A0 pipeline: zero curves (3) -> pick-L matrix (1x3 = {1,0,0}) -> zero curve (1).
    let zero_curve = || build_tabulated_16(&[0u16, 0u16]);
    let mut lut = Pipeline::new(3, 1);
    lut.insert_stage_at_end(Stage::ToneCurves(vec![
        zero_curve(),
        zero_curve(),
        zero_curve(),
    ]))
    .expect("zero post-curves stage is valid");
    lut.insert_stage_at_end(Stage::Matrix {
        rows: 1,
        cols: 3,
        m: vec![1.0, 0.0, 0.0],
        offset: None,
    })
    .expect("pick-L matrix stage is valid");
    lut.insert_stage_at_end(Stage::ToneCurves(vec![zero_curve()]))
        .expect("zero out-curve stage is valid");
    p.add_tag(TAG_B2A0, Tag::Lut(lut));

    p.add_tag(TAG_WTPT, Tag::Xyz(d50_xyz()));
    p
}

/// `cmsCreateLinearizationDeviceLinkTHR` (`cmsvirt.c:288`): a Link-class profile in
/// the given color space carrying one prelinearization tone-curve stage. Tags in
/// order: `desc`, `cprt`, `A2B0` (the curve-set pipeline), `pseq` (the
/// `SetSeqDescTag` profile-sequence description). Only the RGB form (3 transfer
/// curves) is exercised here.
pub fn build_linearization_devicelink(
    color_space: ColorSpace,
    transfer: &[ToneCurve],
) -> WritableProfile {
    let n = transfer.len();
    let mut p = WritableProfile::new(Header {
        version: 0x0440_0000,
        device_class: ProfileClass::Link,
        color_space,
        pcs: color_space,
        rendering_intent: RenderingIntent::Perceptual,
        ..virtual_header()
    });

    set_text_tags(&mut p, "Linearization built-in");

    let mut lut = Pipeline::new(n, n);
    lut.insert_stage_at_end(Stage::ToneCurves(transfer.to_vec()))
        .expect("prelinearization curves stage is valid");
    p.add_tag(TAG_A2B0, Tag::Lut(lut));

    // SetSeqDescTag(hICC, "Linearization built-in"): `cmsAllocProfileSequenceDescription`
    // leaves the per-item Manufacturer/Model MLU handles NULL, and `cmsMLUsetASCII`
    // on a NULL handle is a no-op — so the descriptions stay NULL. `Type_MLU_Write`
    // emits the EMPTY placeholder (count 0) for a NULL MLU, which our empty-`Mlu`
    // serializes identically. mfg/model/attributes/technology are all zero.
    let item = ProfileSequenceItem {
        device_mfg: Signature::from_raw(0),
        device_model: Signature::from_raw(0),
        attributes: 0,
        technology: Signature::from_raw(0),
        manufacturer: Mlu {
            entries: Vec::new(),
        },
        model: Mlu {
            entries: Vec::new(),
        },
    };
    // `_cmsWriteProfileSequence` (cmsio1.c:918) writes the pseq tag, then for v4
    // ALSO the psid (ProfileSequenceId) tag carrying the same sequence — here a
    // single all-zero profile ID with a NULL (empty) description.
    p.add_tag(TAG_PSEQ, Tag::ProfileSequenceDesc(vec![item]));
    p.add_tag(
        TAG_PSID,
        Tag::ProfileSequenceId(vec![ProfileIdItem {
            profile_id: [0u8; 16],
            description: Mlu {
                entries: Vec::new(),
            },
        }]),
    );

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::serialize::save_to_mem;

    // `which` selectors mirroring shim.c's `RCMS_T4_*` enum.
    const T4_SRGB: i32 = 0;
    const T4_GRAY: i32 = 1;
    const T4_LAB2: i32 = 2;
    const T4_LAB4: i32 = 3;
    const T4_XYZ: i32 = 4;
    const T4_NULL: i32 = 5;
    const T4_RGB: i32 = 6;
    const T4_LIN: i32 = 7;

    /// Serialize `built` and assert it is byte-identical to lcms2's
    /// `cmsCreate*Profile` + `cmsSaveProfileToMem` for the same `which`. On a
    /// mismatch, report the first differing byte to localize the divergence
    /// (header at 0..128, directory after, then each tag body at its offset).
    fn assert_virtual_identical(which: i32, built: &WritableProfile, label: &str) {
        let rust = save_to_mem(built).expect("tintbox serialize");
        let c = tintbox_oracle::save_virtual_profile(which).expect("lcms2 serialize");
        assert_eq!(
            rust.len(),
            c.len(),
            "{label}: length mismatch tintbox={} lcms2={}",
            rust.len(),
            c.len()
        );
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            let at = first.unwrap_or(0);
            let lo = at.saturating_sub(4);
            let hi = (at + 16).min(rust.len());
            panic!(
                "{label}: byte mismatch at {first:?}\n tintbox[{lo}..{hi}]={:02x?}\n lcms[{lo}..{hi}]={:02x?}",
                &rust[lo..hi],
                &c[lo..hi],
            );
        }
    }

    #[test]
    fn srgb_byte_identical() {
        assert_virtual_identical(T4_SRGB, &build_srgb_profile(), "sRGB");
    }

    #[test]
    fn gray_byte_identical() {
        let p = build_gray_profile(d50_xyy(), &build_gamma(2.2));
        assert_virtual_identical(T4_GRAY, &p, "gray");
    }

    #[test]
    fn lab2_byte_identical() {
        assert_virtual_identical(T4_LAB2, &build_lab2_profile(), "Lab2");
    }

    #[test]
    fn lab4_byte_identical() {
        assert_virtual_identical(T4_LAB4, &build_lab4_profile(), "Lab4");
    }

    #[test]
    fn xyz_byte_identical() {
        assert_virtual_identical(T4_XYZ, &build_xyz_profile(), "XYZ");
    }

    #[test]
    fn null_byte_identical() {
        assert_virtual_identical(T4_NULL, &build_null_profile(), "NULL");
    }

    #[test]
    fn rgb_byte_identical() {
        let d65 = CIExyY {
            x: 0.3127,
            y: 0.3290,
            yy: 1.0,
        };
        let rec709 = CIExyYTriple {
            red: CIExyY {
                x: 0.6400,
                y: 0.3300,
                yy: 1.0,
            },
            green: CIExyY {
                x: 0.3000,
                y: 0.6000,
                yy: 1.0,
            },
            blue: CIExyY {
                x: 0.1500,
                y: 0.0600,
                yy: 1.0,
            },
        };
        let p = build_rgb_profile(d65, rec709, &build_gamma(2.2), "RGB built-in");
        assert_virtual_identical(T4_RGB, &p, "RGB");
    }

    #[test]
    fn linearization_byte_identical() {
        let curves: Vec<ToneCurve> = (0..3).map(|_| build_gamma(2.2)).collect();
        let p = build_linearization_devicelink(ColorSpace::Rgb, &curves);
        assert_virtual_identical(T4_LIN, &p, "linearization");
    }
}
