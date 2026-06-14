//! Tag-value dispatch: map an on-disk tag-*type* signature to the reader that
//! decodes it. This mirrors lcms2's `_cmsGetTagTypeHandler` lookup
//! (`cmstypes.c`), but only the trivial types (slice-2 task 3) are wired up;
//! every other (struct-shaped / deferred) type returns `Error::Unsupported`.

pub mod trivial;

use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::profile::tag::Tag;
use crate::sig::Signature;

// On-disk tag-TYPE signatures (cmsTagTypeSignature, include/lcms2.h). Only the
// trivial-type sigs we dispatch on are named here; deferred types fall through.
const T_XYZ: u32 = 0x5859_5A20; // 'XYZ '
const T_CORBIS_BROKEN_XYZ: u32 = 0x17A5_05B8; // vendor-broken XYZ lcms2 maps to XYZ
const T_S15FIXED16_ARRAY: u32 = 0x7366_3332; // 'sf32'
const T_U16FIXED16_ARRAY: u32 = 0x7566_3332; // 'uf32'
const T_UINT8_ARRAY: u32 = 0x7569_3038; // 'ui08'
const T_UINT32_ARRAY: u32 = 0x7569_3332; // 'ui32'
const T_SIGNATURE: u32 = 0x7369_6720; // 'sig '
const T_DATA: u32 = 0x6461_7461; // 'data'
const T_DATETIME: u32 = 0x6474_696D; // 'dtim'
const T_CHROMATICITY: u32 = 0x6368_726D; // 'chrm'
const T_TEXT: u32 = 0x7465_7874; // 'text'
const T_COLORANT_ORDER: u32 = 0x636C_726F; // 'clro'

/// Decode the tag value for the on-disk `type_sig`. `r` is positioned at the
/// start of the type payload (already past the 8-byte type base); `size` is the
/// payload byte count (`TagSize - 8`, lcms2's `SizeOfTag`). Unknown or
/// not-yet-implemented (deferred) types return `Error::Unsupported`, never panic.
pub fn read_tag_value<R: ProfileReader>(type_sig: Signature, r: &mut R, size: u32) -> Result<Tag> {
    match type_sig.to_raw() {
        T_XYZ | T_CORBIS_BROKEN_XYZ => trivial::read_xyz(r, size),
        T_S15FIXED16_ARRAY => trivial::read_s15fixed16_array(r, size),
        T_U16FIXED16_ARRAY => trivial::read_u16fixed16_array(r, size),
        T_UINT8_ARRAY => trivial::read_uint8_array(r, size),
        T_UINT32_ARRAY => trivial::read_uint32_array(r, size),
        T_SIGNATURE => trivial::read_signature(r, size),
        T_DATA => trivial::read_data(r, size),
        T_DATETIME => trivial::read_datetime(r, size),
        T_CHROMATICITY => trivial::read_chromaticity(r, size),
        T_TEXT => trivial::read_text(r, size),
        T_COLORANT_ORDER => trivial::read_colorant_order(r, size),
        _ => Err(Error::Unsupported("tag type deferred to a later slice")),
    }
}
