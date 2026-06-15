//! Profile → pipeline LUT extraction, bit-identical to lcms2's
//! `_cmsReadInputLUT` / `_cmsReadOutputLUT` / `_cmsReadDevicelinkLUT`
//! (`src/cmsio1.c`).
//!
//! These functions turn a parsed [`Profile`] into a processing [`Pipeline`]:
//! either by cloning a LUT-based tag (A2Bx / B2Ax / DToBx / BToDx selected by
//! intent) with the version/Lab fixups lcms2 applies, or by synthesising a
//! matrix-shaper from the per-channel TRC curves and the RGB→XYZ colorant matrix.

use crate::color::CIEXYZ;
use crate::curve::{build_tabulated_16, reverse_tone_curve, ToneCurve};
use crate::error::{Error, Result};
use crate::math::matrix::Mat3;
use crate::math::whitepoint::D50;
use crate::pipeline::{Pipeline, Stage};
use crate::profile::{ColorSpace, Profile, ProfileClass, Tag};
use crate::sig::Signature;

// ---- tag signatures (cmsio1.c:32-50) ----------------------------------------

const TAG_A2B0: Signature = Signature::from_bytes(*b"A2B0");
const TAG_A2B1: Signature = Signature::from_bytes(*b"A2B1");
const TAG_A2B2: Signature = Signature::from_bytes(*b"A2B2");
const TAG_B2A0: Signature = Signature::from_bytes(*b"B2A0");
const TAG_B2A1: Signature = Signature::from_bytes(*b"B2A1");
const TAG_B2A2: Signature = Signature::from_bytes(*b"B2A2");
const TAG_D2B0: Signature = Signature::from_bytes(*b"D2B0");
const TAG_D2B1: Signature = Signature::from_bytes(*b"D2B1");
const TAG_D2B2: Signature = Signature::from_bytes(*b"D2B2");
const TAG_D2B3: Signature = Signature::from_bytes(*b"D2B3");
const TAG_B2D0: Signature = Signature::from_bytes(*b"B2D0");
const TAG_B2D1: Signature = Signature::from_bytes(*b"B2D1");
const TAG_B2D2: Signature = Signature::from_bytes(*b"B2D2");
const TAG_B2D3: Signature = Signature::from_bytes(*b"B2D3");

const TAG_RED_COLORANT: Signature = Signature::from_bytes(*b"rXYZ");
const TAG_GREEN_COLORANT: Signature = Signature::from_bytes(*b"gXYZ");
const TAG_BLUE_COLORANT: Signature = Signature::from_bytes(*b"bXYZ");
const TAG_RED_TRC: Signature = Signature::from_bytes(*b"rTRC");
const TAG_GREEN_TRC: Signature = Signature::from_bytes(*b"gTRC");
const TAG_BLUE_TRC: Signature = Signature::from_bytes(*b"bTRC");
const TAG_GRAY_TRC: Signature = Signature::from_bytes(*b"kTRC");

const TAG_NAMED_COLOR2: Signature = Signature::from_bytes(*b"ncl2");

/// `cmsSigLut16Type` (`'mft2'`): the on-disk type that gates the V2↔V4 Lab fixups.
const LUT16_TYPE: Signature = Signature::from_bytes(*b"mft2");

// `Device2PCS16` / `PCS2Device16` indexed by intent. Absolute (3) aliases
// relative (1): `{AToB0, AToB1, AToB2, AToB1}` / `{BToA0, BToA1, BToA2, BToA1}`
// (cmsio1.c:32-45).
const DEVICE2PCS16: [Signature; 4] = [TAG_A2B0, TAG_A2B1, TAG_A2B2, TAG_A2B1];
const DEVICE2PCS_FLOAT: [Signature; 4] = [TAG_D2B0, TAG_D2B1, TAG_D2B2, TAG_D2B3];
const PCS2DEVICE16: [Signature; 4] = [TAG_B2A0, TAG_B2A1, TAG_B2A2, TAG_B2A1];
const PCS2DEVICE_FLOAT: [Signature; 4] = [TAG_B2D0, TAG_B2D1, TAG_B2D2, TAG_B2D3];

/// `INTENT_ABSOLUTE_COLORIMETRIC` (lcms2.h): the highest intent the `<= ` guards
/// in `_cmsReadInputLUT`/`Output`/`Devicelink` accept.
const INTENT_ABSOLUTE_COLORIMETRIC: u32 = 3;

/// `MAX_ENCODEABLE_XYZ` (lcms2_internal.h:71): `1.0 + 32767.0/32768.0`.
const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;

/// `InpAdj = 1.0/MAX_ENCODEABLE_XYZ` (cmsio1.c:54).
const INP_ADJ: f64 = 1.0 / MAX_ENCODEABLE_XYZ;

/// `OutpAdj = MAX_ENCODEABLE_XYZ` (cmsio1.c:55).
const OUTP_ADJ: f64 = MAX_ENCODEABLE_XYZ;

/// Read a `Tag::Xyz` colorant value, mapping anything else / absence to an error
/// (lcms2's `ReadICCMatrixRGB2XYZ` returns FALSE when any colorant is missing).
fn read_colorant_xyz(profile: &Profile, sig: Signature) -> Result<CIEXYZ> {
    match profile.read_tag(sig)? {
        Tag::Xyz(v) => Ok(v),
        _ => Err(Error::Corrupt("colorant tag is not XYZ")),
    }
}

/// Read a `Tag::Curve` TRC, mapping anything else / absence to an error.
fn read_trc(profile: &Profile, sig: Signature) -> Result<ToneCurve> {
    match profile.read_tag(sig)? {
        Tag::Curve(c) => Ok(c),
        _ => Err(Error::Corrupt("TRC tag is not a curve")),
    }
}

/// lcms2 `ReadICCMatrixRGB2XYZ` (cmsio1.c:132-151): build the RGB→XYZ colorant
/// matrix `M` (row-major, `_cmsVEC3init` per row) whose columns are the
/// red/green/blue colorant XYZ tristimulus: `M.row0 = {R.X, G.X, B.X}`,
/// `row1 = the Y's`, `row2 = the Z's`.
fn read_icc_matrix_rgb2xyz(profile: &Profile) -> Result<Mat3> {
    let r = read_colorant_xyz(profile, TAG_RED_COLORANT)?;
    let g = read_colorant_xyz(profile, TAG_GREEN_COLORANT)?;
    let b = read_colorant_xyz(profile, TAG_BLUE_COLORANT)?;
    Ok(Mat3([
        r.x, g.x, b.x, // row 0 (X's)
        r.y, g.y, b.y, // row 1 (Y's)
        r.z, g.z, b.z, // row 2 (Z's)
    ]))
}

// ---- float-normalize stages (cmslut.c:1063-1140) ----------------------------
// `_cmsStageNormalizeToLabFloat` / `FromLabFloat` / `ToXyzFloat` / `FromXyzFloat`
// are diagonal Matrix stages (Lab carries an offset). They re-encode the
// floating-point LUT's space-native units into/out of lcms2's 0..1.0 notation.

fn normalize_to_lab_float() -> Stage {
    Stage::Matrix {
        rows: 3,
        cols: 3,
        m: vec![100.0, 0.0, 0.0, 0.0, 255.0, 0.0, 0.0, 0.0, 255.0],
        offset: Some(vec![0.0, -128.0, -128.0]),
    }
}

fn normalize_from_lab_float() -> Stage {
    Stage::Matrix {
        rows: 3,
        cols: 3,
        m: vec![
            1.0 / 100.0,
            0.0,
            0.0,
            0.0,
            1.0 / 255.0,
            0.0,
            0.0,
            0.0,
            1.0 / 255.0,
        ],
        offset: Some(vec![0.0, 128.0 / 255.0, 128.0 / 255.0]),
    }
}

fn normalize_to_xyz_float() -> Stage {
    let n = 65535.0 / 32768.0;
    Stage::Matrix {
        rows: 3,
        cols: 3,
        m: vec![n, 0.0, 0.0, 0.0, n, 0.0, 0.0, 0.0, n],
        offset: None,
    }
}

fn normalize_from_xyz_float() -> Stage {
    let n = 32768.0 / 65535.0;
    Stage::Matrix {
        rows: 3,
        cols: 3,
        m: vec![n, 0.0, 0.0, 0.0, n, 0.0, 0.0, 0.0, n],
        offset: None,
    }
}

/// Clone the LUT pipeline a tag decodes to, or error if it is not a `Tag::Lut`.
fn read_lut_pipeline(profile: &Profile, sig: Signature) -> Result<Pipeline> {
    match profile.read_tag(sig)? {
        Tag::Lut(p) => Ok(p),
        _ => Err(Error::Corrupt("LUT tag is not a pipeline")),
    }
}

// ============================================================================
//  Input LUT (`_cmsReadInputLUT`, cmsio1.c:309-401)
// ============================================================================

/// lcms2 `_cmsReadInputLUT` (cmsio1.c:309): build the device→PCS pipeline for
/// `intent`. Float tag precedence → 16-bit tag (perceptual fallback) → matrix
/// shaper (gray or RGB). Named-color profiles are routed but not yet supported.
pub fn read_input_lut(profile: &Profile, intent: u32) -> Result<Pipeline> {
    let header = profile.header();

    // Named color: route without panicking (full named transforms deferred).
    if header.device_class == ProfileClass::NamedColor {
        if !profile.has_tag(TAG_NAMED_COLOR2) {
            return Err(Error::Corrupt("named-color profile lacks ncl2"));
        }
        return Err(Error::Unsupported("named-color input LUT"));
    }

    if intent <= INTENT_ABSOLUTE_COLORIMETRIC {
        let idx = intent as usize;
        let tag_float = DEVICE2PCS_FLOAT[idx];

        // Float tag takes precedence (cmsio1.c:343).
        if profile.has_tag(tag_float) {
            return read_float_input_tag(profile, tag_float);
        }

        // Revert to perceptual if the intent's 16-bit tag is absent.
        let mut tag16 = DEVICE2PCS16[idx];
        if !profile.has_tag(tag16) {
            tag16 = DEVICE2PCS16[0];
        }

        if profile.has_tag(tag16) {
            let original_type = profile.tag_true_type(tag16);
            let mut lut = read_lut_pipeline(profile, tag16)?;

            // Adjust only for Lab16 on output (cmsio1.c:370).
            if original_type != Some(LUT16_TYPE) || header.pcs != ColorSpace::Lab {
                return Ok(lut);
            }

            // If the input is Lab, add a V4→V2 conversion at the begin.
            if header.color_space == ColorSpace::Lab {
                lut.prepend_stage(Stage::LabV4ToV2)?;
            }
            // V2→V4 Lab PCS matrix at the end (always).
            lut.insert_stage_at_end(Stage::LabV2ToV4)?;
            return Ok(lut);
        }
    }

    // No LUT: matrix shaper. Gray vs RGB by color space.
    if header.color_space == ColorSpace::Gray {
        build_gray_input_matrix_pipeline(profile)
    } else {
        build_rgb_input_matrix_shaper(profile)
    }
}

/// lcms2 `_cmsReadFloatInputTag` (cmsio1.c:264-303).
fn read_float_input_tag(profile: &Profile, tag_float: Signature) -> Result<Pipeline> {
    let header = profile.header();
    let mut lut = read_lut_pipeline(profile, tag_float)?;

    match header.color_space {
        ColorSpace::Lab => lut.prepend_stage(normalize_to_lab_float())?,
        ColorSpace::XYZ => lut.prepend_stage(normalize_to_xyz_float())?,
        _ => {}
    }
    match header.pcs {
        ColorSpace::Lab => lut.insert_stage_at_end(normalize_from_lab_float())?,
        ColorSpace::XYZ => lut.insert_stage_at_end(normalize_from_xyz_float())?,
        _ => {}
    }
    Ok(lut)
}

/// lcms2 `BuildRGBInputMatrixShaper` (cmsio1.c:210-259): `ToneCurves([R,G,B])`
/// then `Matrix(3,3, M*InpAdj, None)`; iff PCS==Lab append `Xyz2Lab`.
fn build_rgb_input_matrix_shaper(profile: &Profile) -> Result<Pipeline> {
    let mut mat = read_icc_matrix_rgb2xyz(profile)?;

    // Every matrix entry × InpAdj (cmsio1.c:224-226).
    for cell in &mut mat.0 {
        *cell *= INP_ADJ;
    }

    let red = read_trc(profile, TAG_RED_TRC)?;
    let green = read_trc(profile, TAG_GREEN_TRC)?;
    let blue = read_trc(profile, TAG_BLUE_TRC)?;

    let mut lut = Pipeline::new(3, 3);
    lut.insert_stage_at_end(Stage::ToneCurves(vec![red, green, blue]))?;
    lut.insert_stage_at_end(Stage::Matrix {
        rows: 3,
        cols: 3,
        m: mat.0.to_vec(),
        offset: None,
    })?;

    if profile.header().pcs == ColorSpace::Lab {
        lut.insert_stage_at_end(Stage::Xyz2Lab)?;
    }
    Ok(lut)
}

/// lcms2 `BuildGrayInputMatrixPipeline` (cmsio1.c:156-206).
fn build_gray_input_matrix_pipeline(profile: &Profile) -> Result<Pipeline> {
    let gray = read_trc(profile, TAG_GRAY_TRC)?;
    let mut lut = Pipeline::new(1, 3);

    if profile.header().pcs == ColorSpace::Lab {
        // Identity matrix [3:1] of ones, then 3 tone curves {gray, empty, empty}.
        let empty = build_tabulated_16(&[0x8080, 0x8080]);
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 1,
            m: vec![1.0, 1.0, 1.0],
            offset: None,
        })?;
        lut.insert_stage_at_end(Stage::ToneCurves(vec![gray, empty.clone(), empty]))?;
    } else {
        // XYZ PCS: tone curve then GrayInputMatrix = InpAdj * D50 (cmsio1.c:58).
        lut.insert_stage_at_end(Stage::ToneCurves(vec![gray]))?;
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 1,
            m: vec![INP_ADJ * D50.x, INP_ADJ * D50.y, INP_ADJ * D50.z],
            offset: None,
        })?;
    }
    Ok(lut)
}

// ============================================================================
//  Output LUT (`_cmsReadOutputLUT`, cmsio1.c:582-658)
// ============================================================================

/// lcms2 `_cmsReadOutputLUT` (cmsio1.c:582): build the PCS→device pipeline for
/// `intent`. Float tag → 16-bit tag (perceptual fallback, trilinear-flip for Lab,
/// V4↔V2 gate) → inverse matrix shaper (gray or RGB).
pub fn read_output_lut(profile: &Profile, intent: u32) -> Result<Pipeline> {
    let header = profile.header();

    if intent <= INTENT_ABSOLUTE_COLORIMETRIC {
        let idx = intent as usize;
        let tag_float = PCS2DEVICE_FLOAT[idx];

        if profile.has_tag(tag_float) {
            return read_float_output_tag(profile, tag_float);
        }

        let mut tag16 = PCS2DEVICE16[idx];
        if !profile.has_tag(tag16) {
            tag16 = PCS2DEVICE16[0];
        }

        if profile.has_tag(tag16) {
            let original_type = profile.tag_true_type(tag16);
            let mut lut = read_lut_pipeline(profile, tag16)?;

            // Lab indexer space → trilinear interpolation (cmsio1.c:623).
            if header.pcs == ColorSpace::Lab {
                lut.change_interpolation_to_trilinear();
            }

            // Adjust only for Lab16 type (cmsio1.c:627).
            if original_type != Some(LUT16_TYPE) || header.pcs != ColorSpace::Lab {
                return Ok(lut);
            }

            // V4→V2 Lab PCS matrix at the begin.
            lut.prepend_stage(Stage::LabV4ToV2)?;
            // If the output is Lab, V2→V4 at the end.
            if header.color_space == ColorSpace::Lab {
                lut.insert_stage_at_end(Stage::LabV2ToV4)?;
            }
            return Ok(lut);
        }
    }

    if header.color_space == ColorSpace::Gray {
        build_gray_output_pipeline(profile)
    } else {
        build_rgb_output_matrix_shaper(profile)
    }
}

/// lcms2 `_cmsReadFloatOutputTag` (cmsio1.c:538-579).
fn read_float_output_tag(profile: &Profile, tag_float: Signature) -> Result<Pipeline> {
    let header = profile.header();
    let mut lut = read_lut_pipeline(profile, tag_float)?;

    // PCS is the input space here.
    match header.pcs {
        ColorSpace::Lab => lut.prepend_stage(normalize_to_lab_float())?,
        ColorSpace::XYZ => lut.prepend_stage(normalize_to_xyz_float())?,
        _ => {}
    }
    // dataSpace (color_space) is the output space.
    match header.color_space {
        ColorSpace::Lab => lut.insert_stage_at_end(normalize_from_lab_float())?,
        ColorSpace::XYZ => lut.insert_stage_at_end(normalize_from_xyz_float())?,
        _ => {}
    }
    Ok(lut)
}

/// lcms2 `BuildRGBOutputMatrixShaper` (cmsio1.c:452-513): iff PCS==Lab prepend
/// `Lab2XYZ`, then `Matrix(3,3, Inv*OutpAdj)` then `ToneCurves([R⁻¹,G⁻¹,B⁻¹])`.
fn build_rgb_output_matrix_shaper(profile: &Profile) -> Result<Pipeline> {
    let mat = read_icc_matrix_rgb2xyz(profile)?;
    let mut inv = mat
        .inverse()
        .ok_or(Error::Corrupt("singular colorant matrix"))?;

    // Every entry × OutpAdj (cmsio1.c:471-473).
    for cell in &mut inv.0 {
        *cell *= OUTP_ADJ;
    }

    let red = read_trc(profile, TAG_RED_TRC)?;
    let green = read_trc(profile, TAG_GREEN_TRC)?;
    let blue = read_trc(profile, TAG_BLUE_TRC)?;
    let inv_red = reverse_tone_curve(&red);
    let inv_green = reverse_tone_curve(&green);
    let inv_blue = reverse_tone_curve(&blue);

    let mut lut = Pipeline::new(3, 3);

    if profile.header().pcs == ColorSpace::Lab {
        lut.insert_stage_at_end(Stage::Lab2Xyz)?;
    }
    lut.insert_stage_at_end(Stage::Matrix {
        rows: 3,
        cols: 3,
        m: inv.0.to_vec(),
        offset: None,
    })?;
    lut.insert_stage_at_end(Stage::ToneCurves(vec![inv_red, inv_green, inv_blue]))?;
    Ok(lut)
}

/// lcms2 `BuildGrayOutputPipeline` (cmsio1.c:411-449): `Matrix(1,3, PickLstar|
/// PickY)` then the reversed gray TRC.
fn build_gray_output_pipeline(profile: &Profile) -> Result<Pipeline> {
    let gray = read_trc(profile, TAG_GRAY_TRC)?;
    let rev = reverse_tone_curve(&gray);

    let mut lut = Pipeline::new(3, 1);

    if profile.header().pcs == ColorSpace::Lab {
        // PickLstarMatrix = {1, 0, 0} (cmsio1.c:61).
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 1,
            cols: 3,
            m: vec![1.0, 0.0, 0.0],
            offset: None,
        })?;
    } else {
        // PickYMatrix = {0, OutpAdj*D50Y, 0} (cmsio1.c:60).
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 1,
            cols: 3,
            m: vec![0.0, OUTP_ADJ * D50.y, 0.0],
            offset: None,
        })?;
    }
    lut.insert_stage_at_end(Stage::ToneCurves(vec![rev]))?;
    Ok(lut)
}

// ============================================================================
//  Device-link LUT (`_cmsReadDevicelinkLUT`, cmsio1.c:705-801)
// ============================================================================

/// lcms2 `_cmsReadDevicelinkLUT` (cmsio1.c:705): build the LUT for a device-link
/// or abstract profile. Float-tag precedence (intent then perceptual), then the
/// 16-bit tag with trilinear-flip and a `Lut16Type`-only V2↔V4 gate.
pub fn read_devicelink_lut(profile: &Profile, intent: u32) -> Result<Pipeline> {
    let header = profile.header();

    if intent > INTENT_ABSOLUTE_COLORIMETRIC {
        return Err(Error::Unsupported("devicelink intent out of range"));
    }
    let idx = intent as usize;
    let mut tag16 = DEVICE2PCS16[idx];
    let tag_float = DEVICE2PCS_FLOAT[idx];

    // Named color (cmsio1.c:721).
    if header.device_class == ProfileClass::NamedColor {
        if !profile.has_tag(TAG_NAMED_COLOR2) {
            return Err(Error::Corrupt("named-color profile lacks ncl2"));
        }
        return Err(Error::Unsupported("named-color devicelink LUT"));
    }

    // Float tag for the intent takes precedence (cmsio1.c:745).
    if profile.has_tag(tag_float) {
        return read_float_devicelink_tag(profile, tag_float);
    }

    // Perceptual float fallback: cloned verbatim, NO normalize stages (cmsio1.c:751).
    let tag_float0 = DEVICE2PCS_FLOAT[0];
    if profile.has_tag(tag_float0) {
        return read_lut_pipeline(profile, tag_float0);
    }

    // 16-bit tag, perceptual fallback (cmsio1.c:757).
    if !profile.has_tag(tag16) {
        tag16 = DEVICE2PCS16[0];
        if !profile.has_tag(tag16) {
            return Err(Error::Corrupt("devicelink profile lacks an A2B/LUT tag"));
        }
    }

    let mut lut = read_lut_pipeline(profile, tag16)?;

    // Lab indexer → trilinear (cmsio1.c:775).
    if header.pcs == ColorSpace::Lab {
        lut.change_interpolation_to_trilinear();
    }

    let original_type = profile.tag_true_type(tag16);

    // Adjust for Lab16 on output only (cmsio1.c:782): gate is Lut16Type ALONE.
    if original_type != Some(LUT16_TYPE) {
        return Ok(lut);
    }

    // Lab on both sides is possible here.
    if header.color_space == ColorSpace::Lab {
        lut.prepend_stage(Stage::LabV4ToV2)?;
    }
    if header.pcs == ColorSpace::Lab {
        lut.insert_stage_at_end(Stage::LabV2ToV4)?;
    }
    Ok(lut)
}

/// lcms2 `_cmsReadFloatDevicelinkTag` (cmsio1.c:663-701). Same normalize-stage
/// insertion as the input float tag.
fn read_float_devicelink_tag(profile: &Profile, tag_float: Signature) -> Result<Pipeline> {
    let header = profile.header();
    let mut lut = read_lut_pipeline(profile, tag_float)?;

    match header.color_space {
        ColorSpace::Lab => lut.prepend_stage(normalize_to_lab_float())?,
        ColorSpace::XYZ => lut.prepend_stage(normalize_to_xyz_float())?,
        _ => {}
    }
    match header.pcs {
        ColorSpace::Lab => lut.insert_stage_at_end(normalize_from_lab_float())?,
        ColorSpace::XYZ => lut.insert_stage_at_end(normalize_from_xyz_float())?,
        _ => {}
    }
    Ok(lut)
}
