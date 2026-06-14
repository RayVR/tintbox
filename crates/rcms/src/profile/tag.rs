//! The decoded tag-value enum. Each variant is the cooked form lcms2's
//! per-type `ReadPtr` handler produces for a tag, modelled as a Rust value.
//!
//! This task (slice-2 task 3) lands only the *trivial* tag types — the ones
//! whose handler is a flat read with no nested structure. Struct-shaped types
//! (curves, LUTs, MLU, named colours, …) arrive in later tasks and are NOT
//! variants here: dispatching to them yields `Error::Unsupported` for now.

use crate::color::{CIExyYTriple, CIEXYZ};
use crate::fixed::{S15Fixed16, U16Fixed16};
use crate::profile::header::DateTime;
use crate::sig::Signature;

/// A decoded ICC tag value. One variant per supported on-disk tag *type*.
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    /// `XYZType` (`'XYZ '`): a single tristimulus value. Colorant/whitepoint/
    /// luminance tags. (`cmstypes.c:347`).
    Xyz(CIEXYZ),
    /// `S15Fixed16ArrayType` (`'sf32'`): an array of s15Fixed16, decoded to f64
    /// in lcms2 but kept as the raw fixed values here so the comparison is exact.
    /// (`cmstypes.c:752`).
    S15Fixed16Array(Vec<S15Fixed16>),
    /// `U16Fixed16ArrayType` (`'uf32'`): an array of u16Fixed16. (`cmstypes.c:812`).
    U16Fixed16Array(Vec<U16Fixed16>),
    /// `UInt8ArrayType`: a raw byte array. (`cmstypes.c:579`).
    U8Array(Vec<u8>),
    /// `UInt32ArrayType`: a raw u32 array. (`cmstypes.c:636`).
    U32Array(Vec<u32>),
    /// `SignatureType` (`'sig '`): a single 4-byte signature. (`cmstypes.c:879`).
    Signature(Signature),
    /// `DataType` (`'data'`): a flag word plus opaque bytes. (`cmstypes.c:1029`).
    Data { flag: u32, data: Vec<u8> },
    /// `DateTimeType` (`'dtim'`): a UTC timestamp. (`cmstypes.c:1556`).
    DateTime(DateTime),
    /// `ChromaticityType` (`'chrm'`): R/G/B phosphor chromaticities, Y forced to
    /// 1.0. (`cmstypes.c:407`).
    Chromaticity(CIExyYTriple),
    /// `TextType` (`'text'`): a 7-bit ASCII string (NUL-terminated on disk).
    /// (`cmstypes.c:925`).
    Text(String),
    /// `ColorantOrderType` (`'clro'`): the colorant laydown order, `Count` bytes.
    /// (`cmstypes.c:509`).
    ColorantOrder(Vec<u8>),
    /// `cmsSigMeasurementType` (`'meas'`): a `cmsICCMeasurementConditions` struct.
    /// (`cmstypes.c:1615`).
    Measurement(Measurement),
    /// `cmsSigViewingConditionsType` (`'view'`): a `cmsICCViewingConditions`
    /// struct. (`cmstypes.c:4135`).
    ViewingConditions(ViewingConditions),
    /// `cmsSigScreeningType` (`'scrn'`): a `cmsScreening` struct (flag, channels).
    /// (`cmstypes.c:4048`).
    Screening(Screening),
    /// `cmsSigCrdInfoType` (`'crdi'`): the PostScript CRD info — product name and
    /// four rendering-intent CRD names, each a counted ASCII string.
    /// (`cmstypes.c:3980`).
    CrdInfo(CrdInfo),
    /// `cmsSigcicpType` (`'cicp'`): a `cmsVideoSignalType` (ITU-T H.273 coding
    /// parameters). (`cmstypes.c:5614`).
    Cicp(Cicp),
    /// `cmsSigColorantTableType` (`'clrt'`): a count-prefixed list of named
    /// colorants, each a 32-byte name plus a 3×u16 PCS. (`cmstypes.c:3254`).
    ColorantTable(Vec<ColorantTableEntry>),
}

/// `cmsICCMeasurementConditions` (`include/lcms2.h:1051`). `flare` is the
/// s15Fixed16 `Flare` field decoded to f64 (lcms2 reads it via
/// `_cmsRead15Fixed16Number`); the three `u32` fields are the raw wire values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measurement {
    pub observer: u32,
    pub backing: CIEXYZ,
    pub geometry: u32,
    pub flare: f64,
    pub illuminant_type: u32,
}

/// `cmsICCViewingConditions` (`include/lcms2.h:1060`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewingConditions {
    pub illuminant_xyz: CIEXYZ,
    pub surround_xyz: CIEXYZ,
    pub illuminant_type: u32,
}

/// One `cmsScreeningChannel` (`include/lcms2.h:1429`). `frequency` and
/// `screen_angle` are s15Fixed16 decoded to f64 (lcms2 reads them via
/// `_cmsRead15Fixed16Number`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreeningChannel {
    pub frequency: f64,
    pub screen_angle: f64,
    pub spot_shape: u32,
}

/// `cmsScreening` (`include/lcms2.h:1434`). lcms2 caps the channel count at
/// `cmsMAXCHANNELS - 1` but keeps the original `n_channels`; we keep the cooked
/// channel vector (already capped) and the (possibly larger) declared count.
#[derive(Clone, Debug, PartialEq)]
pub struct Screening {
    pub flag: u32,
    pub n_channels: u32,
    pub channels: Vec<ScreeningChannel>,
}

/// The five counted ASCII strings of `cmsSigCrdInfoType`: the PostScript product
/// name and the rendering-intent 0..3 CRD names. lcms2 stores them in an MLU
/// under the `PS`/`nm`,`#0`..`#3` keys; we keep them as plain byte strings (the
/// counted bytes before the NUL the C appends), which is the comparable value.
#[derive(Clone, Debug, PartialEq)]
pub struct CrdInfo {
    pub product_name: Vec<u8>,
    pub crd_names: [Vec<u8>; 4],
}

/// `cmsVideoSignalType` (`include/lcms2.h:1067`): the four ITU-T H.273 bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cicp {
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub video_full_range_flag: u8,
}

/// One colorant of `cmsSigColorantTableType`: a 32-byte ASCII name (NUL-trimmed)
/// and the colorant's PCS as three u16. lcms2 stores these in a
/// `cmsNAMEDCOLORLIST`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorantTableEntry {
    pub name: String,
    pub pcs: [u16; 3],
}
