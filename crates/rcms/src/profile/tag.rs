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
}
