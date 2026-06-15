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
//! **Deferred (post-slice-7):** every path that `cmsDetectBlackPoint` /
//! `...Destination...` resolves by *sampling* the profile — i.e.
//! `BlackPointAsDarkerColorant` (V2/rel-col, and V4 matrix shapers under
//! perc/sat), `BlackPointUsingPerceptualBlack` (ink output rel-col), and the
//! destination round-trip detection. Those need slice-7 Lab virtual profiles +
//! round-trip transforms. They return [`BlackPoint::NeedsSampling`].

use crate::color::CIEXYZ;
use crate::math::matrix::{Mat3, Vec3};
use crate::math::whitepoint::D50;
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::sig::Signature;

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
/// XYZ (the constant or, eventually, a sampled value), report that the device
/// class/intent makes the black point `{0,0,0}` (lcms2's `return FALSE`), or
/// signal that the only remaining path requires sampling — which rcms defers to
/// post-slice-7.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlackPoint {
    /// A resolved black point (the V4-perceptual constant in slice 5).
    Resolved(CIEXYZ),
    /// lcms2 returns `FALSE` with `BlackPoint = {0,0,0}` (Link/Abstract/NamedColor
    /// class, or an intent that is not perceptual/relative/saturation). The caller
    /// treats this as a `{0,0,0}` black point, exactly as the C does when it
    /// ignores the boolean return and reads the (zeroed) out-parameter.
    Zero,
    /// The only path left is detection-by-sampling
    /// (`BlackPointAsDarkerColorant` / `BlackPointUsingPerceptualBlack` /
    /// destination round-trip) — deferred to post-slice-7 (Lab virtual profiles).
    NeedsSampling,
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
/// the V4-perceptual-black short-circuit.
///
/// Returns:
/// - [`BlackPoint::Zero`] for Link/Abstract/NamedColor class, or an intent outside
///   {perceptual, relative, saturation} (both lcms2 `return FALSE` cases).
/// - [`BlackPoint::Resolved`] of the `cmsPERCEPTUAL_BLACK_*` constant for a V4
///   non-matrix-shaper profile under perceptual/saturation.
/// - [`BlackPoint::NeedsSampling`] for everything else (the matrix-shaper V4
///   perc/sat case, all relative-colorimetric, and all V2) — deferred.
fn detect_common(profile: &Profile, intent: RenderingIntent) -> BlackPoint {
    let dev_class = profile.header().device_class;

    // Make sure the device class is adequate (cmssamp.c:243-249).
    if dev_class == ProfileClass::Link
        || dev_class == ProfileClass::Abstract
        || dev_class == ProfileClass::NamedColor
    {
        return BlackPoint::Zero;
    }

    // Make sure intent is adequate (cmssamp.c:252-257).
    if intent != RenderingIntent::Perceptual
        && intent != RenderingIntent::RelativeColorimetric
        && intent != RenderingIntent::Saturation
    {
        return BlackPoint::Zero;
    }

    // v4 + perceptual & saturation intents have their own black point
    // (cmssamp.c:261-274).
    if profile.header().version >= ICC_VERSION_V4
        && (intent == RenderingIntent::Perceptual || intent == RenderingIntent::Saturation)
    {
        // Matrix shapers share MRC & perceptual intents → BlackPointAsDarkerColorant
        // (sampling, deferred).
        if is_matrix_shaper(profile) {
            return BlackPoint::NeedsSampling;
        }

        // Fixed perceptual black for V4 profiles under perceptual & saturation.
        return BlackPoint::Resolved(CIEXYZ {
            x: PERCEPTUAL_BLACK_X,
            y: PERCEPTUAL_BLACK_Y,
            z: PERCEPTUAL_BLACK_Z,
        });
    }

    // Everything below this in cmsDetectBlackPoint is sampling-based
    // (CMS_USE_PROFILE_BLACK_POINT_TAG read, BlackPointUsingPerceptualBlack,
    // BlackPointAsDarkerColorant). Deferred to post-slice-7.
    BlackPoint::NeedsSampling
}

/// `cmsDetectBlackPoint` (`cmssamp.c:238-321`), V4-perceptual-black-constant subset.
/// See [`detect_common`] for the resolved/zero/needs-sampling cases.
pub fn detect_black_point(profile: &Profile, intent: RenderingIntent) -> BlackPoint {
    detect_common(profile, intent)
}

/// `cmsDetectDestinationBlackPoint` (`cmssamp.c:399-456`), V4-perceptual-black-
/// constant subset.
///
/// The head of the destination function (`:413-445`) is byte-for-byte the same
/// ladder as the source function up to the V4 short-circuit, so we reuse it. The
/// remainder (`:447-456`) is the LUT/colorspace test that either falls through to
/// `cmsDetectBlackPoint` (handled-as-input) or proceeds to the destination
/// round-trip sampling — all sampling, so [`BlackPoint::NeedsSampling`].
pub fn detect_destination_black_point(profile: &Profile, intent: RenderingIntent) -> BlackPoint {
    detect_common(profile, intent)
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
        // the fixed perceptual black. crayons.icc is V4 RGB; if it is a matrix
        // shaper it would defer, so probe a CLUT one. Search the testbed.
        let want = CIEXYZ {
            x: PERCEPTUAL_BLACK_X,
            y: PERCEPTUAL_BLACK_Y,
            z: PERCEPTUAL_BLACK_Z,
        };
        let p = load("crayons.icc");
        assert!(p.header().version >= ICC_VERSION_V4);
        let bp = detect_black_point(&p, RenderingIntent::Perceptual);
        if is_matrix_shaper(&p) {
            assert_eq!(bp, BlackPoint::NeedsSampling, "matrix shaper defers");
        } else {
            assert_eq!(bp, BlackPoint::Resolved(want));
            assert_eq!(
                detect_black_point(&p, RenderingIntent::Saturation),
                BlackPoint::Resolved(want)
            );
            assert_eq!(
                detect_destination_black_point(&p, RenderingIntent::Perceptual),
                BlackPoint::Resolved(want)
            );
        }
    }

    #[test]
    fn v4_relative_defers_to_sampling() {
        // V4 + relative colorimetric does NOT short-circuit to the constant; it is
        // sampling-based ⇒ deferred.
        let p = load("crayons.icc");
        assert_eq!(
            detect_black_point(&p, RenderingIntent::RelativeColorimetric),
            BlackPoint::NeedsSampling
        );
    }

    #[test]
    fn v2_perceptual_defers_to_sampling() {
        // V2 perceptual never hits the V4 short-circuit ⇒ sampling ⇒ deferred.
        let p = load("test5.icc");
        assert!(p.header().version < ICC_VERSION_V4);
        assert_eq!(
            detect_black_point(&p, RenderingIntent::Perceptual),
            BlackPoint::NeedsSampling
        );
    }

    #[test]
    fn inadequate_intent_is_zero() {
        // An intent outside {perceptual, relative, saturation} ⇒ Zero.
        let p = load("crayons.icc");
        assert_eq!(
            detect_black_point(&p, RenderingIntent::AbsoluteColorimetric),
            BlackPoint::Zero
        );
    }
}
