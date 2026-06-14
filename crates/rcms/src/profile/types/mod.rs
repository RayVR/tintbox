//! Tag-value dispatch: map an on-disk tag-*type* signature to the reader that
//! decodes it. This mirrors lcms2's `_cmsGetTagTypeHandler` lookup
//! (`cmstypes.c`), but only the trivial types (slice-2 task 3) are wired up;
//! every other (struct-shaped / deferred) type returns `Error::Unsupported`.

pub mod curve;
pub mod lut;
pub mod mlu;
pub mod mpe;
pub mod named;
pub mod structs;
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
const T_MEASUREMENT: u32 = 0x6D65_6173; // 'meas'
const T_VIEWING_CONDITIONS: u32 = 0x7669_6577; // 'view'
const T_SCREENING: u32 = 0x7363_726E; // 'scrn'
const T_CRD_INFO: u32 = 0x6372_6469; // 'crdi'
const T_CICP: u32 = 0x6369_6370; // 'cicp'
const T_COLORANT_TABLE: u32 = 0x636C_7274; // 'clrt'
const T_MLU: u32 = 0x6D6C_7563; // 'mluc'
const T_TEXT_DESCRIPTION: u32 = 0x6465_7363; // 'desc'
const T_NAMED_COLOR2: u32 = 0x6E63_6C32; // 'ncl2'
const T_PROFILE_SEQUENCE_DESC: u32 = 0x7073_6571; // 'pseq'
const T_PROFILE_SEQUENCE_ID: u32 = 0x7073_6964; // 'psid'
const T_DICT: u32 = 0x6469_6374; // 'dict'
const T_CURVE: u32 = 0x6375_7276; // 'curv'
const T_PARAMETRIC_CURVE: u32 = 0x7061_7261; // 'para'
const T_VCGT: u32 = 0x7663_6774; // 'vcgt'
const T_UCRBG: u32 = 0x6266_6420; // 'bfd '
const T_LUT8: u32 = 0x6D66_7431; // 'mft1'
const T_LUT16: u32 = 0x6D66_7432; // 'mft2'
const T_LUT_A2B: u32 = 0x6D41_4220; // 'mAB '
const T_LUT_B2A: u32 = 0x6D42_4120; // 'mBA '
const T_MPE: u32 = 0x6D70_6574; // 'mpet' MultiProcessElement

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
        T_MEASUREMENT => structs::read_measurement(r, size),
        T_VIEWING_CONDITIONS => structs::read_viewing_conditions(r, size),
        T_SCREENING => structs::read_screening(r, size),
        T_CRD_INFO => structs::read_crd_info(r, size),
        T_CICP => structs::read_cicp(r, size),
        T_COLORANT_TABLE => structs::read_colorant_table(r, size),
        T_MLU => mlu::read_mlu(r, size),
        T_TEXT_DESCRIPTION => mlu::read_text_description(r, size),
        T_NAMED_COLOR2 => named::read_named_color2(r, size),
        T_PROFILE_SEQUENCE_DESC => named::read_profile_sequence_desc(r, size),
        T_PROFILE_SEQUENCE_ID => named::read_profile_sequence_id(r, size),
        T_DICT => named::read_dictionary(r, size),
        T_CURVE => curve::read_curve(r, size),
        T_PARAMETRIC_CURVE => curve::read_parametric_curve(r, size),
        T_VCGT => curve::read_vcgt(r, size),
        T_UCRBG => curve::read_ucrbg(r, size),
        T_LUT8 => lut::read_lut8(r, size),
        T_LUT16 => lut::read_lut16(r, size),
        T_LUT_A2B => lut::read_lut_a2b(r, size),
        T_LUT_B2A => lut::read_lut_b2a(r, size),
        T_MPE => mpe::read_mpe(r, size),
        _ => Err(Error::Unsupported("tag type deferred to a later slice")),
    }
}
