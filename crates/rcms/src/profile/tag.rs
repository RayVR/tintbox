//! The decoded tag-value enum. Each variant is the cooked form lcms2's
//! per-type `ReadPtr` handler produces for a tag, modelled as a Rust value.
//!
//! This task (slice-2 task 3) lands only the *trivial* tag types — the ones
//! whose handler is a flat read with no nested structure. Struct-shaped types
//! (curves, LUTs, MLU, named colours, …) arrive in later tasks and are NOT
//! variants here: dispatching to them yields `Error::Unsupported` for now.

use crate::color::{CIExyYTriple, CIEXYZ};
use crate::curve::ToneCurve;
use crate::fixed::{S15Fixed16, U16Fixed16};
use crate::pipeline::Pipeline;
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
    /// `cmsSigMultiLocalizedUnicodeType` (`'mluc'`, `Type_MLU_Read`,
    /// `cmstypes.c:1677`) AND `cmsSigTextDescriptionType` (`'desc'`,
    /// `Type_Text_Description_Read`, `cmstypes.c:1096`). Both decode in lcms2 to a
    /// `cmsMLU`, so they share one Rust value. (`textDescription` is the ICC v2
    /// form that an ICC v4 profile would carry as `mluc`.)
    Mlu(Mlu),
    /// `cmsSigNamedColor2Type` (`'ncl2'`, `Type_NamedColor_Read`,
    /// `cmstypes.c:3369`): a `cmsNAMEDCOLORLIST` — a vendor flag, shared
    /// prefix/suffix, and a list of named colours each with a PCS and per-channel
    /// device coordinates.
    NamedColor2(NamedColorList),
    /// `cmsSigProfileSequenceDescType` (`'pseq'`, `Type_ProfileSequenceDesc_Read`,
    /// `cmstypes.c:3541`): the source→destination profile-sequence description,
    /// one record per combined profile.
    ProfileSequenceDesc(Vec<ProfileSequenceItem>),
    /// `cmsSigProfileSequenceIdType` (`'psid'`, `Type_ProfileSequenceId_Read`,
    /// `cmstypes.c:3687`): a positioned array of {16-byte profile ID, MLU
    /// description}, used to identify the profiles in a device-link sequence.
    ProfileSequenceId(Vec<ProfileIdItem>),
    /// `cmsSigDictType` (`'dict'`, `Type_Dictionary_Read`, `cmstypes.c:5436`): a
    /// metadata dictionary — name/value UTF-16 strings with optional localized
    /// display-name/value MLUs. Carried by the `meta` (and `dict`) tags.
    Dict(Dict),
    /// `cmsSigCurveType` (`'curv'`, `Type_Curve_Read`, `cmstypes.c:1333`) AND
    /// `cmsSigParametricCurveType` (`'para'`, `Type_ParametricCurve_Read`,
    /// `cmstypes.c:1451`). Both decode in lcms2 to a `cmsToneCurve`, so they share
    /// one Rust value. Carried by the per-channel TRC tags (red/green/blue/grayTRC).
    Curve(ToneCurve),
    /// `cmsSigVcgtType` (`'vcgt'`, `Type_vcgt_Read`, `cmstypes.c:4943`): the video
    /// card gamma table — three `cmsToneCurve` (R/G/B), built from either an on-disk
    /// 8/16-bit table (the table variant) or a per-channel gamma/min/max formula
    /// (the formula variant, built as an ICC type-5 parametric curve). The `Vec`
    /// always has length 3 (lcms2 rejects any other channel count).
    Vcgt(Vec<ToneCurve>),
    /// `cmsSigUcrBgType` (`'bfd '`, `Type_UcrBg_Read`, `cmstypes.c:3789`): the
    /// under-color-removal / black-generation curves plus a free-text description.
    /// `ucr` and `bg` are 16-bit tabulated curves; `desc` is the trailing ASCII
    /// string (lcms2 stores it in a `cmsMLU`, but it is set via `cmsMLUsetASCII`
    /// from the raw bytes, so the comparable value is the plain string).
    UcrBg {
        ucr: ToneCurve,
        bg: ToneCurve,
        desc: String,
    },
    /// `cmsSigLut8Type` (`'mft1'`, `Type_LUT8_Read`, `cmstypes.c:2002`) AND
    /// `cmsSigLut16Type` (`'mft2'`, `Type_LUT16_Read`, `cmstypes.c:2307`). Both
    /// decode in lcms2 to a `cmsPipeline`, so they share one Rust value. Carried
    /// by the A2Bx / B2Ax / gamut / preview LUT tags.
    Lut(Pipeline),
}

/// One named colour of a `cmsNAMEDCOLORLIST` (`cmstypes.c:3369`). `name` is the
/// fixed 32-byte root name (NUL-trimmed, Latin-1 1:1 like lcms2's `Root`), `pcs`
/// is the 3×u16 PCS, and `device` holds `nDeviceCoords` u16 device coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedColor {
    pub name: String,
    pub pcs: [u16; 3],
    pub device: Vec<u16>,
}

/// A `cmsNAMEDCOLORLIST` (`Type_NamedColor_Read`, `cmstypes.c:3369`). `vendor_flag`
/// is the leading u32; `prefix`/`suffix` are the fixed 32-byte ASCII name affixes
/// (NUL-trimmed, force-terminated at index 31 like lcms2); `colors` is the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedColorList {
    pub vendor_flag: u32,
    pub prefix: String,
    pub suffix: String,
    pub colors: Vec<NamedColor>,
    /// lcms2 `cmsNAMEDCOLORLIST::ColorantCount` (the ncl2 `nDeviceCoords`). All
    /// colors share this device-channel width. Kept explicitly because a list may
    /// legitimately carry zero colors yet still declare a colorant count, and the
    /// named-color transform's device output width is driven by it.
    pub colorant_count: usize,
}

impl NamedColorList {
    /// `cmsNamedColorCount` (cmsnamed.c:856): the number of spot colors.
    pub fn count(&self) -> usize {
        self.colors.len()
    }

    /// The device-colorant channel width (`cmsNAMEDCOLORLIST::ColorantCount`,
    /// the ncl2 `nDeviceCoords`).
    pub fn colorant_count(&self) -> usize {
        self.colorant_count
    }

    /// `cmsNamedColorIndex` (cmsnamed.c:890): the index of the color whose root
    /// name matches `name` case-insensitively (lcms2 `cmsstrcasecmp`, ASCII),
    /// or `None` if absent (the C returns `-1`). The prefix/suffix are not part
    /// of the comparison.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.colors
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }
}

/// One `cmsPSEQDESC` record of `Type_ProfileSequenceDesc_Read` (`cmstypes.c:3541`):
/// the four header-derived fields plus the two embedded-text MLUs (manufacturer
/// and model descriptions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSequenceItem {
    pub device_mfg: Signature,
    pub device_model: Signature,
    pub attributes: u64,
    pub technology: Signature,
    pub manufacturer: Mlu,
    pub model: Mlu,
}

/// One element of `Type_ProfileSequenceId_Read` (`cmstypes.c:3687`): a 16-byte
/// profile ID and the positioned embedded-MLU description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileIdItem {
    pub profile_id: [u8; 16],
    pub description: Mlu,
}

/// One entry of `Type_Dictionary_Read` (`cmstypes.c:5436`). `name`/`value` are the
/// required UTF-16 strings (decoded to `String`); `display_name`/`display_value`
/// are the optional localized MLUs (present only when the record length is 24/32
/// and the entry's offset/size are non-zero).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictEntry {
    pub name: String,
    pub value: String,
    pub display_name: Option<Mlu>,
    pub display_value: Option<Mlu>,
}

/// A `cmsHANDLE` dictionary (`Type_Dictionary_Read`, `cmstypes.c:5436`).
/// `entries` is in ON-DISK record order. Note lcms2's `cmsDictAddEntry` PREPENDS
/// to the list head, so `cmsDictGetEntryList` enumerates entries in the REVERSE
/// of this order — the differential test accounts for that by reversing the
/// oracle's enumeration before comparing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dict {
    pub entries: Vec<DictEntry>,
}

/// One localized record of a `cmsMLU` (`_cmsMLUentry`, `cmsnamed.c`): a 2-byte
/// ISO-639 language code, a 2-byte ISO-3166 country code (both raw, byte-for-byte
/// as on disk — lcms2 keeps them as the big-endian u16 wire value and `strFrom16`
/// splits that back into two bytes), and the decoded text.
///
/// `text` is the entry's UTF-16BE string pool slice decoded with
/// [`char::decode_utf16`] (U+FFFD on lone surrogates) — the exact code-unit
/// sequence lcms2 keeps in its wide `MemPool`. lcms2 reads each UTF-16BE unit
/// straight into a `wchar_t` with NO surrogate pairing (`_cmsReadWCharArray`);
/// decoding the identical unit sequence is the faithful comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MluEntry {
    pub language: [u8; 2],
    pub country: [u8; 2],
    pub text: String,
}

/// A `cmsMLU` (multi-localized unicode): an ordered set of localized strings.
/// `entries` preserves the on-disk record order (lcms2 keeps `Entries[0..Count]`
/// in directory order), which `cmsMLUtranslationsCodes` enumerates by index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mlu {
    pub entries: Vec<MluEntry>,
}

impl Mlu {
    /// A one-entry MLU carrying `text` under the `cmsNoLanguage`/`cmsNoCountry`
    /// codes (`"\0\0"`), mirroring `cmsMLUsetASCII(mlu, cmsNoLanguage,
    /// cmsNoCountry, text)`. This is how lcms2 holds a plain `TextType` value
    /// before `DecideType` chooses the on-disk type, so the serializer can build
    /// any text-family body from a bare string.
    pub fn from_ascii(text: &str) -> Mlu {
        Mlu {
            entries: vec![MluEntry {
                language: [0, 0],
                country: [0, 0],
                text: text.to_string(),
            }],
        }
    }

    /// A one-entry MLU carrying `text` under the given 2-byte language/country
    /// codes, mirroring `cmsMLUsetWide(mlu, lang, country, text)` (the call lcms2's
    /// virtual-profile `SetTextTags` uses with `"en"`/`"US"`).
    pub fn from_wide(language: [u8; 2], country: [u8; 2], text: &str) -> Mlu {
        Mlu {
            entries: vec![MluEntry {
                language,
                country,
                text: text.to_string(),
            }],
        }
    }

    /// The entry lcms2 `_cmsMLUgetWide` selects for `(lang, country)`: an exact
    /// `language == lang` match (preferring an exact country too), else the first
    /// entry. `lang`/`country` are the raw 2-byte codes (`cmsNoLanguage` is
    /// `[0,0]`, `cmsV2Unicode` is `[0xFF,0xFF]`). Returns `None` only for an empty
    /// MLU.
    pub(crate) fn select(&self, lang: [u8; 2], country: [u8; 2]) -> Option<&MluEntry> {
        let mut best: Option<&MluEntry> = None;
        for e in &self.entries {
            if e.language == lang {
                if best.is_none() {
                    best = Some(e);
                }
                if e.country == country {
                    return Some(e); // exact language+country match wins.
                }
            }
        }
        best.or_else(|| self.entries.first())
    }

    /// The ASCII representation lcms2's writers obtain via `cmsMLUgetASCII(mlu,
    /// cmsNoLanguage, cmsNoCountry, ...)`: select the `[0,0]`-or-first entry and
    /// downcast each code unit (`< 0xFF` → that byte, else `'?'`).
    pub(crate) fn preferred_ascii(&self) -> String {
        let Some(e) = self.select([0, 0], [0, 0]) else {
            return String::new();
        };
        e.text
            .encode_utf16()
            .map(|u| if u < 0xff { u as u8 as char } else { '?' })
            .collect()
    }
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
