//! Tag-descriptor table and link compatibility, transcribed from lcms2's
//! `SupportedTags` (`src/cmstypes.c:6059-6151`) and `_cmsGetTagDescriptor`
//! (`cmstypes.c:6239`).
//!
//! A `cmsTagDescriptor` in lcms2 carries `{ ElemCount, nSupportedTypes,
//! SupportedTypes[], Decider }`. The directory link gate (spec §7.2) only needs
//! the comparison performed by `CompatibleTypes` (`src/cmsio0.c:796`), which
//! compares `nSupportedTypes`, `ElemCount`, and the `SupportedTypes` set
//! element-by-element. The `Decider` function pointer plays no role in linking,
//! so we omit it here. We transcribe the *whole* table (every row) because the
//! type-dispatch in later tasks needs `allowed_types` for each tag.

use crate::sig::Signature;

/// `cmsTagDescriptor` reduced to the fields lcms2's `CompatibleTypes` and the
/// later tag-type dispatch actually read. `elem_count` is the C `ElemCount`,
/// `allowed_types` is the `SupportedTypes[0..nSupportedTypes]` slice in the
/// exact order lcms2 lists them.
#[derive(Clone, Copy, Debug)]
pub struct TagDescriptor {
    pub elem_count: u32,
    pub allowed_types: &'static [Signature],
}

// ---- ICC tag-type signatures (cmsTagTypeSignature, include/lcms2.h) ----
const SIG_CHROMATICITY_TYPE: Signature = Signature::from_raw(0x6368_726D); // 'chrm'
const SIG_CICP_TYPE: Signature = Signature::from_raw(0x6369_6370); // 'cicp'
const SIG_COLORANT_ORDER_TYPE: Signature = Signature::from_raw(0x636C_726F); // 'clro'
const SIG_COLORANT_TABLE_TYPE: Signature = Signature::from_raw(0x636C_7274); // 'clrt'
const SIG_CRD_INFO_TYPE: Signature = Signature::from_raw(0x6372_6469); // 'crdi'
const SIG_CURVE_TYPE: Signature = Signature::from_raw(0x6375_7276); // 'curv'
const SIG_DATA_TYPE: Signature = Signature::from_raw(0x6461_7461); // 'data'
const SIG_DICT_TYPE: Signature = Signature::from_raw(0x6469_6374); // 'dict'
const SIG_DATETIME_TYPE: Signature = Signature::from_raw(0x6474_696D); // 'dtim'
const SIG_LUT16_TYPE: Signature = Signature::from_raw(0x6d66_7432); // 'mft2'
const SIG_LUT8_TYPE: Signature = Signature::from_raw(0x6d66_7431); // 'mft1'
const SIG_LUT_ATOB_TYPE: Signature = Signature::from_raw(0x6d41_4220); // 'mAB '
const SIG_LUT_BTOA_TYPE: Signature = Signature::from_raw(0x6d42_4120); // 'mBA '
const SIG_MEASUREMENT_TYPE: Signature = Signature::from_raw(0x6D65_6173); // 'meas'
const SIG_MLU_TYPE: Signature = Signature::from_raw(0x6D6C_7563); // 'mluc'
const SIG_MPE_TYPE: Signature = Signature::from_raw(0x6D70_6574); // 'mpet'
const SIG_NAMED_COLOR2_TYPE: Signature = Signature::from_raw(0x6E63_6C32); // 'ncl2'
const SIG_PARAMETRIC_CURVE_TYPE: Signature = Signature::from_raw(0x7061_7261); // 'para'
const SIG_PROFILE_SEQUENCE_DESC_TYPE: Signature = Signature::from_raw(0x7073_6571); // 'pseq'
const SIG_PROFILE_SEQUENCE_ID_TYPE: Signature = Signature::from_raw(0x7073_6964); // 'psid'
const SIG_S15FIXED16_ARRAY_TYPE: Signature = Signature::from_raw(0x7366_3332); // 'sf32'
const SIG_SCREENING_TYPE: Signature = Signature::from_raw(0x7363_726E); // 'scrn'
const SIG_SIGNATURE_TYPE: Signature = Signature::from_raw(0x7369_6720); // 'sig '
const SIG_TEXT_TYPE: Signature = Signature::from_raw(0x7465_7874); // 'text'
const SIG_TEXT_DESCRIPTION_TYPE: Signature = Signature::from_raw(0x6465_7363); // 'desc'
const SIG_UCR_BG_TYPE: Signature = Signature::from_raw(0x6266_6420); // 'bfd '
const SIG_VCGT_TYPE: Signature = Signature::from_raw(0x7663_6774); // 'vcgt'
const SIG_VIEWING_CONDITIONS_TYPE: Signature = Signature::from_raw(0x7669_6577); // 'view'
const SIG_XYZ_TYPE: Signature = Signature::from_raw(0x5859_5A20); // 'XYZ '
const SIG_MHC2_TYPE: Signature = Signature::from_raw(0x4D48_4332); // 'MHC2'

// Vendor-broken types lcms2 tolerates (cmstypes.c:40-41).
const CORBIS_BROKEN_XYZ_TYPE: Signature = Signature::from_raw(0x17A5_05B8);
const MONACO_BROKEN_CURVE_TYPE: Signature = Signature::from_raw(0x9478_ee00);

// ---- per-tag allowed-type sets (the SupportedTypes[] arrays) ----
const LUT_A2B: &[Signature] = &[SIG_LUT16_TYPE, SIG_LUT_ATOB_TYPE, SIG_LUT8_TYPE];
const LUT_B2A: &[Signature] = &[SIG_LUT16_TYPE, SIG_LUT_BTOA_TYPE, SIG_LUT8_TYPE];
const COLORANT_XYZ: &[Signature] = &[SIG_XYZ_TYPE, CORBIS_BROKEN_XYZ_TYPE];
const TRC: &[Signature] = &[
    SIG_CURVE_TYPE,
    SIG_PARAMETRIC_CURVE_TYPE,
    MONACO_BROKEN_CURVE_TYPE,
];
const CURVE_2: &[Signature] = &[SIG_CURVE_TYPE, SIG_PARAMETRIC_CURVE_TYPE];
const TEXT_TYPE_SET: &[Signature] = &[SIG_TEXT_TYPE, SIG_MLU_TYPE, SIG_TEXT_DESCRIPTION_TYPE];
const TEXT_DESC_SET: &[Signature] = &[SIG_TEXT_DESCRIPTION_TYPE, SIG_MLU_TYPE, SIG_TEXT_TYPE];

// One-element type sets, declared as named statics so the table rows stay terse.
const S_DATETIME: &[Signature] = &[SIG_DATETIME_TYPE];
const S_TEXT: &[Signature] = &[SIG_TEXT_TYPE];
const S_S15F16: &[Signature] = &[SIG_S15FIXED16_ARRAY_TYPE];
const S_CHROMATICITY: &[Signature] = &[SIG_CHROMATICITY_TYPE];
const S_COLORANT_ORDER: &[Signature] = &[SIG_COLORANT_ORDER_TYPE];
const S_COLORANT_TABLE: &[Signature] = &[SIG_COLORANT_TABLE_TYPE];
const S_XYZ: &[Signature] = &[SIG_XYZ_TYPE];
const S_NAMED_COLOR2: &[Signature] = &[SIG_NAMED_COLOR2_TYPE];
const S_PROFILE_SEQ_DESC: &[Signature] = &[SIG_PROFILE_SEQUENCE_DESC_TYPE];
const S_SIGNATURE: &[Signature] = &[SIG_SIGNATURE_TYPE];
const S_MEASUREMENT: &[Signature] = &[SIG_MEASUREMENT_TYPE];
const S_DATA: &[Signature] = &[SIG_DATA_TYPE];
const S_UCR_BG: &[Signature] = &[SIG_UCR_BG_TYPE];
const S_CRD_INFO: &[Signature] = &[SIG_CRD_INFO_TYPE];
const S_MPE: &[Signature] = &[SIG_MPE_TYPE];
const S_TEXT_DESCRIPTION: &[Signature] = &[SIG_TEXT_DESCRIPTION_TYPE];
const S_VIEWING_CONDITIONS: &[Signature] = &[SIG_VIEWING_CONDITIONS_TYPE];
const S_SCREENING: &[Signature] = &[SIG_SCREENING_TYPE];
const S_VCGT: &[Signature] = &[SIG_VCGT_TYPE];
const S_DICT: &[Signature] = &[SIG_DICT_TYPE];
const S_PROFILE_SEQ_ID: &[Signature] = &[SIG_PROFILE_SEQUENCE_ID_TYPE];
const S_MLU: &[Signature] = &[SIG_MLU_TYPE];
const S_CICP: &[Signature] = &[SIG_CICP_TYPE];
const S_MHC2: &[Signature] = &[SIG_MHC2_TYPE];

// ---- ICC tag signatures (cmsTagSignature, include/lcms2.h) ----
const T_A2B0: u32 = 0x4132_4230;
const T_A2B1: u32 = 0x4132_4231;
const T_A2B2: u32 = 0x4132_4232;
const T_B2A0: u32 = 0x4232_4130;
const T_B2A1: u32 = 0x4232_4131;
const T_B2A2: u32 = 0x4232_4132;
const T_RED_COLORANT: u32 = 0x7258_595A; // 'rXYZ'
const T_GREEN_COLORANT: u32 = 0x6758_595A; // 'gXYZ'
const T_BLUE_COLORANT: u32 = 0x6258_595A; // 'bXYZ'
const T_RED_TRC: u32 = 0x7254_5243; // 'rTRC'
const T_GREEN_TRC: u32 = 0x6754_5243; // 'gTRC'
const T_BLUE_TRC: u32 = 0x6254_5243; // 'bTRC'
const T_CALIBRATION_DATETIME: u32 = 0x6361_6C74; // 'calt'
const T_CHAR_TARGET: u32 = 0x7461_7267; // 'targ'
const T_CHROMATIC_ADAPTATION: u32 = 0x6368_6164; // 'chad'
const T_CHROMATICITY: u32 = 0x6368_726D; // 'chrm'
const T_COLORANT_ORDER: u32 = 0x636C_726F; // 'clro'
const T_COLORANT_TABLE: u32 = 0x636C_7274; // 'clrt'
const T_COLORANT_TABLE_OUT: u32 = 0x636C_6F74; // 'clot'
const T_COPYRIGHT: u32 = 0x6370_7274; // 'cprt'
const T_DATETIME: u32 = 0x6474_696D; // 'dtim'
const T_DEVICE_MFG_DESC: u32 = 0x646D_6E64; // 'dmnd'
const T_DEVICE_MODEL_DESC: u32 = 0x646D_6464; // 'dmdd'
const T_GAMUT: u32 = 0x6761_6D74; // 'gamt'
const T_GRAY_TRC: u32 = 0x6b54_5243; // 'kTRC'
const T_LUMINANCE: u32 = 0x6C75_6D69; // 'lumi'
const T_MEDIA_BLACK_POINT: u32 = 0x626B_7074; // 'bkpt'
const T_MEDIA_WHITE_POINT: u32 = 0x7774_7074; // 'wtpt'
const T_NAMED_COLOR2: u32 = 0x6E63_6C32; // 'ncl2'
const T_PREVIEW0: u32 = 0x7072_6530; // 'pre0'
const T_PREVIEW1: u32 = 0x7072_6531; // 'pre1'
const T_PREVIEW2: u32 = 0x7072_6532; // 'pre2'
const T_PROFILE_DESCRIPTION: u32 = 0x6465_7363; // 'desc'
const T_PROFILE_SEQUENCE_DESC: u32 = 0x7073_6571; // 'pseq'
const T_TECHNOLOGY: u32 = 0x7465_6368; // 'tech'
const T_COLORIMETRIC_INTENT_IMAGE_STATE: u32 = 0x6369_6973; // 'ciis'
const T_PERCEPTUAL_RENDERING_INTENT_GAMUT: u32 = 0x7269_6730; // 'rig0'
const T_SATURATION_RENDERING_INTENT_GAMUT: u32 = 0x7269_6732; // 'rig2'
const T_MEASUREMENT: u32 = 0x6D65_6173; // 'meas'
const T_PS2_CRD0: u32 = 0x7073_6430; // 'psd0'
const T_PS2_CRD1: u32 = 0x7073_6431; // 'psd1'
const T_PS2_CRD2: u32 = 0x7073_6432; // 'psd2'
const T_PS2_CRD3: u32 = 0x7073_6433; // 'psd3'
const T_PS2_CSA: u32 = 0x7073_3273; // 'ps2s'
const T_PS2_RENDERING_INTENT: u32 = 0x7073_3269; // 'ps2i'
const T_VIEWING_COND_DESC: u32 = 0x7675_6564; // 'vued'
const T_UCR_BG: u32 = 0x6266_6420; // 'bfd '
const T_CRD_INFO: u32 = 0x6372_6469; // 'crdi'
const T_DTOB0: u32 = 0x4432_4230; // 'D2B0'
const T_DTOB1: u32 = 0x4432_4231; // 'D2B1'
const T_DTOB2: u32 = 0x4432_4232; // 'D2B2'
const T_DTOB3: u32 = 0x4432_4233; // 'D2B3'
const T_BTOD0: u32 = 0x4232_4430; // 'B2D0'
const T_BTOD1: u32 = 0x4232_4431; // 'B2D1'
const T_BTOD2: u32 = 0x4232_4432; // 'B2D2'
const T_BTOD3: u32 = 0x4232_4433; // 'B2D3'
const T_SCREENING_DESC: u32 = 0x7363_7264; // 'scrd'
const T_VIEWING_CONDITIONS: u32 = 0x7669_6577; // 'view'
const T_SCREENING: u32 = 0x7363_726E; // 'scrn'
const T_VCGT: u32 = 0x7663_6774; // 'vcgt'
const T_META: u32 = 0x6D65_7461; // 'meta'
const T_PROFILE_SEQUENCE_ID: u32 = 0x7073_6964; // 'psid'
const T_PROFILE_DESCRIPTION_ML: u32 = 0x6473_636d; // 'dscm'
const T_CICP: u32 = 0x6369_6370; // 'cicp'
const T_ARGYLL_ARTS: u32 = 0x6172_7473; // 'arts'
const T_MHC2: u32 = 0x4D48_4332; // 'MHC2'

/// The full `SupportedTags` table (`cmstypes.c:6059-6151`), as
/// `(tag_sig, ElemCount, SupportedTypes[])`. Order matches the C array.
const SUPPORTED_TAGS: &[(u32, u32, &[Signature])] = &[
    (T_A2B0, 1, LUT_A2B),
    (T_A2B1, 1, LUT_A2B),
    (T_A2B2, 1, LUT_A2B),
    (T_B2A0, 1, LUT_B2A),
    (T_B2A1, 1, LUT_B2A),
    (T_B2A2, 1, LUT_B2A),
    (T_RED_COLORANT, 1, COLORANT_XYZ),
    (T_GREEN_COLORANT, 1, COLORANT_XYZ),
    (T_BLUE_COLORANT, 1, COLORANT_XYZ),
    (T_RED_TRC, 1, TRC),
    (T_GREEN_TRC, 1, TRC),
    (T_BLUE_TRC, 1, TRC),
    (T_CALIBRATION_DATETIME, 1, S_DATETIME),
    (T_CHAR_TARGET, 1, S_TEXT),
    (T_CHROMATIC_ADAPTATION, 9, S_S15F16),
    (T_CHROMATICITY, 1, S_CHROMATICITY),
    (T_COLORANT_ORDER, 1, S_COLORANT_ORDER),
    (T_COLORANT_TABLE, 1, S_COLORANT_TABLE),
    (T_COLORANT_TABLE_OUT, 1, S_COLORANT_TABLE),
    (T_COPYRIGHT, 1, TEXT_TYPE_SET),
    (T_DATETIME, 1, S_DATETIME),
    (T_DEVICE_MFG_DESC, 1, TEXT_DESC_SET),
    (T_DEVICE_MODEL_DESC, 1, TEXT_DESC_SET),
    (T_GAMUT, 1, LUT_B2A),
    (T_GRAY_TRC, 1, CURVE_2),
    (T_LUMINANCE, 1, S_XYZ),
    (T_MEDIA_BLACK_POINT, 1, COLORANT_XYZ),
    (T_MEDIA_WHITE_POINT, 1, COLORANT_XYZ),
    (T_NAMED_COLOR2, 1, S_NAMED_COLOR2),
    (T_PREVIEW0, 1, LUT_B2A),
    (T_PREVIEW1, 1, LUT_B2A),
    (T_PREVIEW2, 1, LUT_B2A),
    (T_PROFILE_DESCRIPTION, 1, TEXT_DESC_SET),
    (T_PROFILE_SEQUENCE_DESC, 1, S_PROFILE_SEQ_DESC),
    (T_TECHNOLOGY, 1, S_SIGNATURE),
    (T_COLORIMETRIC_INTENT_IMAGE_STATE, 1, S_SIGNATURE),
    (T_PERCEPTUAL_RENDERING_INTENT_GAMUT, 1, S_SIGNATURE),
    (T_SATURATION_RENDERING_INTENT_GAMUT, 1, S_SIGNATURE),
    (T_MEASUREMENT, 1, S_MEASUREMENT),
    (T_PS2_CRD0, 1, S_DATA),
    (T_PS2_CRD1, 1, S_DATA),
    (T_PS2_CRD2, 1, S_DATA),
    (T_PS2_CRD3, 1, S_DATA),
    (T_PS2_CSA, 1, S_DATA),
    (T_PS2_RENDERING_INTENT, 1, S_DATA),
    (T_VIEWING_COND_DESC, 1, TEXT_DESC_SET),
    (T_UCR_BG, 1, S_UCR_BG),
    (T_CRD_INFO, 1, S_CRD_INFO),
    (T_DTOB0, 1, S_MPE),
    (T_DTOB1, 1, S_MPE),
    (T_DTOB2, 1, S_MPE),
    (T_DTOB3, 1, S_MPE),
    (T_BTOD0, 1, S_MPE),
    (T_BTOD1, 1, S_MPE),
    (T_BTOD2, 1, S_MPE),
    (T_BTOD3, 1, S_MPE),
    (T_SCREENING_DESC, 1, S_TEXT_DESCRIPTION),
    (T_VIEWING_CONDITIONS, 1, S_VIEWING_CONDITIONS),
    (T_SCREENING, 1, S_SCREENING),
    (T_VCGT, 1, S_VCGT),
    (T_META, 1, S_DICT),
    (T_PROFILE_SEQUENCE_ID, 1, S_PROFILE_SEQ_ID),
    (T_PROFILE_DESCRIPTION_ML, 1, S_MLU),
    (T_CICP, 1, S_CICP),
    (T_ARGYLL_ARTS, 9, S_S15F16),
    (T_MHC2, 1, S_MHC2),
];

/// lcms2 `_cmsGetTagDescriptor` (`cmstypes.c:6239`): linear search of
/// `SupportedTags` for `sig`. Returns `None` for unknown/unsupported tags
/// (matching the C, which returns `NULL`). We ignore plug-in tags (rcms has no
/// plug-in registry).
pub fn descriptor(sig: Signature) -> Option<TagDescriptor> {
    let raw = sig.to_raw();
    SUPPORTED_TAGS
        .iter()
        .find(|(t, _, _)| *t == raw)
        .map(|(_, elem_count, allowed_types)| TagDescriptor {
            elem_count: *elem_count,
            allowed_types,
        })
}

/// lcms2 `CompatibleTypes` (`src/cmsio0.c:796`), operating on tag signatures.
///
/// ```c
/// cmsBool CompatibleTypes(const cmsTagDescriptor* desc1, const cmsTagDescriptor* desc2)
/// {
///     cmsUInt32Number i;
///     if (desc1 == NULL || desc2 == NULL) return FALSE;
///     if (desc1->nSupportedTypes != desc2->nSupportedTypes) return FALSE;
///     if (desc1->ElemCount != desc2->ElemCount) return FALSE;
///     for (i = 0; i < desc1->nSupportedTypes; i++)
///     {
///         if (desc1->SupportedTypes[i] != desc2->SupportedTypes[i]) return FALSE;
///     }
///     return TRUE;
/// }
/// ```
///
/// lcms2 looks each tag's descriptor up via `_cmsGetTagDescriptor`; an unknown
/// tag yields `NULL`, so `CompatibleTypes(NULL, ...)` is `FALSE`. We mirror that:
/// either descriptor missing → not compatible.
pub fn compatible_types(a: Signature, b: Signature) -> bool {
    let (da, db) = match (descriptor(a), descriptor(b)) {
        (Some(da), Some(db)) => (da, db),
        _ => return false,
    };
    if da.allowed_types.len() != db.allowed_types.len() {
        return false;
    }
    if da.elem_count != db.elem_count {
        return false;
    }
    da.allowed_types
        .iter()
        .zip(db.allowed_types.iter())
        .all(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_known_and_unknown() {
        // A2B0 is supported with the LUT_A2B type set.
        let d = descriptor(Signature::from_raw(T_A2B0)).unwrap();
        assert_eq!(d.elem_count, 1);
        assert_eq!(d.allowed_types, LUT_A2B);
        // An invented signature is not in the table.
        assert!(descriptor(Signature::from_raw(0xDEAD_BEEF)).is_none());
    }

    #[test]
    fn compatible_pair_shares_type_set() {
        // DeviceMfgDesc and ProfileDescription both use the TEXT_DESC set with
        // ElemCount 1 -> compatible. (Same SupportedTypes array, same order.)
        let a = Signature::from_raw(T_DEVICE_MFG_DESC);
        let b = Signature::from_raw(T_PROFILE_DESCRIPTION);
        assert!(compatible_types(a, b));
        // Copyright uses TEXT_TYPE_SET (text, mluc, desc) which differs in ORDER
        // from the TEXT_DESC set (desc, mluc, text) -> NOT compatible.
        let c = Signature::from_raw(T_COPYRIGHT);
        assert!(!compatible_types(a, c));
    }

    #[test]
    fn incompatible_pair_distinct_types() {
        // A2B0 (LUT types) vs RedColorant (XYZ types): different sets -> false.
        let a = Signature::from_raw(T_A2B0);
        let b = Signature::from_raw(T_RED_COLORANT);
        assert!(!compatible_types(a, b));
        // Unknown tag is never compatible (descriptor == None, like C's NULL).
        assert!(!compatible_types(a, Signature::from_raw(0xDEAD_BEEF)));
    }

    #[test]
    fn chad_and_arts_differ_in_elem_count_vs_others() {
        // ChromaticAdaptation has ElemCount 9; a 1-elem s15f16 tag would differ,
        // but there is none with the same single-type set AND elem 1, so compare
        // chad to itself (reflexive true) and to luminance (XYZ, false).
        let chad = Signature::from_raw(T_CHROMATIC_ADAPTATION);
        assert!(compatible_types(chad, chad));
        let lumi = Signature::from_raw(T_LUMINANCE);
        assert!(!compatible_types(chad, lumi));
    }
}
