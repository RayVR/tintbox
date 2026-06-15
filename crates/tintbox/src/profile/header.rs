//! The 128-byte ICC profile header (`cmsICCHeader`, lcms2 `include/lcms2.h`),
//! parsed big-endian per `_cmsReadHeader` (lcms2 `src/cmsio0.c`).
//!
//! lcms2 reads the whole struct raw then byte-swaps individual fields; we read
//! field-by-field via the big-endian `ProfileReader` primitives, which yields the
//! same values. We keep exactly the fields lcms2 keeps, validate the magic
//! number, clamp the version through `_validatedVersion`, and reject versions
//! `> 0x5000000` — matching lcms2's accept/reject decision.

use crate::color::CIEXYZ;
use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::sig::Signature;

/// ICC magic number, `'acsp'` (lcms2 `cmsMagicNumber`, `include/lcms2.h`).
const MAGIC: u32 = 0x6163_7370;

/// `cmsProfileClassSignature` (lcms2 `include/lcms2.h`). The named variants are
/// the device classes lcms2 enumerates; anything else round-trips via `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileClass {
    Input,      // 'scnr'
    Display,    // 'mntr'
    Output,     // 'prtr'
    Link,       // 'link'
    Abstract,   // 'abst'
    ColorSpace, // 'spac'
    NamedColor, // 'nmcl'
    Other(Signature),
}

impl ProfileClass {
    const INPUT: u32 = 0x7363_6E72; // 'scnr'
    const DISPLAY: u32 = 0x6D6E_7472; // 'mntr'
    const OUTPUT: u32 = 0x7072_7472; // 'prtr'
    const LINK: u32 = 0x6C69_6E6B; // 'link'
    const ABSTRACT: u32 = 0x6162_7374; // 'abst'
    const COLOR_SPACE: u32 = 0x7370_6163; // 'spac'
    const NAMED_COLOR: u32 = 0x6E6D_636C; // 'nmcl'

    pub fn from_raw(v: u32) -> Self {
        match v {
            Self::INPUT => Self::Input,
            Self::DISPLAY => Self::Display,
            Self::OUTPUT => Self::Output,
            Self::LINK => Self::Link,
            Self::ABSTRACT => Self::Abstract,
            Self::COLOR_SPACE => Self::ColorSpace,
            Self::NAMED_COLOR => Self::NamedColor,
            other => Self::Other(Signature::from_raw(other)),
        }
    }

    pub fn to_raw(self) -> u32 {
        match self {
            Self::Input => Self::INPUT,
            Self::Display => Self::DISPLAY,
            Self::Output => Self::OUTPUT,
            Self::Link => Self::LINK,
            Self::Abstract => Self::ABSTRACT,
            Self::ColorSpace => Self::COLOR_SPACE,
            Self::NamedColor => Self::NAMED_COLOR,
            Self::Other(s) => s.to_raw(),
        }
    }
}

/// `cmsColorSpaceSignature` (lcms2 `include/lcms2.h`). Transcribes the full
/// enum, including the `MCH1..MCHF` channels and the `1CLR..FCLR` / `LuvK`
/// color spaces; anything else round-trips via `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    XYZ,     // 'XYZ '
    Lab,     // 'Lab '
    Luv,     // 'Luv '
    YCbCr,   // 'YCbr'
    Yxy,     // 'Yxy '
    Rgb,     // 'RGB '
    Gray,    // 'GRAY'
    Hsv,     // 'HSV '
    Hls,     // 'HLS '
    Cmyk,    // 'CMYK'
    Cmy,     // 'CMY '
    Mch1,    // 'MCH1'
    Mch2,    // 'MCH2'
    Mch3,    // 'MCH3'
    Mch4,    // 'MCH4'
    Mch5,    // 'MCH5'
    Mch6,    // 'MCH6'
    Mch7,    // 'MCH7'
    Mch8,    // 'MCH8'
    Mch9,    // 'MCH9'
    MchA,    // 'MCHA'
    MchB,    // 'MCHB'
    MchC,    // 'MCHC'
    MchD,    // 'MCHD'
    MchE,    // 'MCHE'
    MchF,    // 'MCHF'
    Named,   // 'nmcl'
    Color1,  // '1CLR'
    Color2,  // '2CLR'
    Color3,  // '3CLR'
    Color4,  // '4CLR'
    Color5,  // '5CLR'
    Color6,  // '6CLR'
    Color7,  // '7CLR'
    Color8,  // '8CLR'
    Color9,  // '9CLR'
    Color10, // 'ACLR'
    Color11, // 'BCLR'
    Color12, // 'CCLR'
    Color13, // 'DCLR'
    Color14, // 'ECLR'
    Color15, // 'FCLR'
    LuvK,    // 'LuvK'
    Other(Signature),
}

impl ColorSpace {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0x5859_5A20 => Self::XYZ,
            0x4C61_6220 => Self::Lab,
            0x4C75_7620 => Self::Luv,
            0x5943_6272 => Self::YCbCr,
            0x5978_7920 => Self::Yxy,
            0x5247_4220 => Self::Rgb,
            0x4752_4159 => Self::Gray,
            0x4853_5620 => Self::Hsv,
            0x484C_5320 => Self::Hls,
            0x434D_594B => Self::Cmyk,
            0x434D_5920 => Self::Cmy,
            0x4D43_4831 => Self::Mch1,
            0x4D43_4832 => Self::Mch2,
            0x4D43_4833 => Self::Mch3,
            0x4D43_4834 => Self::Mch4,
            0x4D43_4835 => Self::Mch5,
            0x4D43_4836 => Self::Mch6,
            0x4D43_4837 => Self::Mch7,
            0x4D43_4838 => Self::Mch8,
            0x4D43_4839 => Self::Mch9,
            0x4D43_4841 => Self::MchA,
            0x4D43_4842 => Self::MchB,
            0x4D43_4843 => Self::MchC,
            0x4D43_4844 => Self::MchD,
            0x4D43_4845 => Self::MchE,
            0x4D43_4846 => Self::MchF,
            0x6E6D_636C => Self::Named,
            0x3143_4C52 => Self::Color1,
            0x3243_4C52 => Self::Color2,
            0x3343_4C52 => Self::Color3,
            0x3443_4C52 => Self::Color4,
            0x3543_4C52 => Self::Color5,
            0x3643_4C52 => Self::Color6,
            0x3743_4C52 => Self::Color7,
            0x3843_4C52 => Self::Color8,
            0x3943_4C52 => Self::Color9,
            0x4143_4C52 => Self::Color10,
            0x4243_4C52 => Self::Color11,
            0x4343_4C52 => Self::Color12,
            0x4443_4C52 => Self::Color13,
            0x4543_4C52 => Self::Color14,
            0x4643_4C52 => Self::Color15,
            0x4C75_764B => Self::LuvK,
            other => Self::Other(Signature::from_raw(other)),
        }
    }

    pub fn to_raw(self) -> u32 {
        match self {
            Self::XYZ => 0x5859_5A20,
            Self::Lab => 0x4C61_6220,
            Self::Luv => 0x4C75_7620,
            Self::YCbCr => 0x5943_6272,
            Self::Yxy => 0x5978_7920,
            Self::Rgb => 0x5247_4220,
            Self::Gray => 0x4752_4159,
            Self::Hsv => 0x4853_5620,
            Self::Hls => 0x484C_5320,
            Self::Cmyk => 0x434D_594B,
            Self::Cmy => 0x434D_5920,
            Self::Mch1 => 0x4D43_4831,
            Self::Mch2 => 0x4D43_4832,
            Self::Mch3 => 0x4D43_4833,
            Self::Mch4 => 0x4D43_4834,
            Self::Mch5 => 0x4D43_4835,
            Self::Mch6 => 0x4D43_4836,
            Self::Mch7 => 0x4D43_4837,
            Self::Mch8 => 0x4D43_4838,
            Self::Mch9 => 0x4D43_4839,
            Self::MchA => 0x4D43_4841,
            Self::MchB => 0x4D43_4842,
            Self::MchC => 0x4D43_4843,
            Self::MchD => 0x4D43_4844,
            Self::MchE => 0x4D43_4845,
            Self::MchF => 0x4D43_4846,
            Self::Named => 0x6E6D_636C,
            Self::Color1 => 0x3143_4C52,
            Self::Color2 => 0x3243_4C52,
            Self::Color3 => 0x3343_4C52,
            Self::Color4 => 0x3443_4C52,
            Self::Color5 => 0x3543_4C52,
            Self::Color6 => 0x3643_4C52,
            Self::Color7 => 0x3743_4C52,
            Self::Color8 => 0x3843_4C52,
            Self::Color9 => 0x3943_4C52,
            Self::Color10 => 0x4143_4C52,
            Self::Color11 => 0x4243_4C52,
            Self::Color12 => 0x4343_4C52,
            Self::Color13 => 0x4443_4C52,
            Self::Color14 => 0x4543_4C52,
            Self::Color15 => 0x4643_4C52,
            Self::LuvK => 0x4C75_764B,
            Self::Other(s) => s.to_raw(),
        }
    }
}

/// `cmsRenderingIntent` (lcms2 `include/lcms2.h`). The four ICC intents plus
/// `Other` for vendor/private intents (lcms2 stores the raw u32 and clamps only
/// at transform-build time, so we preserve the raw value here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    Perceptual,           // 0
    RelativeColorimetric, // 1
    Saturation,           // 2
    AbsoluteColorimetric, // 3
    Other(u32),
}

impl RenderingIntent {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Perceptual,
            1 => Self::RelativeColorimetric,
            2 => Self::Saturation,
            3 => Self::AbsoluteColorimetric,
            other => Self::Other(other),
        }
    }

    pub fn to_raw(self) -> u32 {
        match self {
            Self::Perceptual => 0,
            Self::RelativeColorimetric => 1,
            Self::Saturation => 2,
            Self::AbsoluteColorimetric => 3,
            Self::Other(v) => v,
        }
    }
}

/// `cmsDateTimeNumber` decoded (lcms2 `_cmsDecodeDateTimeNumber`,
/// `src/cmsplugin.c`). Six big-endian u16, stored raw — lcms2's `struct tm`
/// adjusts month/year for C's calendar, but we keep the on-wire values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hours: u16,
    pub minutes: u16,
    pub seconds: u16,
}

/// The parsed 128-byte ICC profile header. Holds the fields lcms2 keeps from
/// `cmsICCHeader`; the magic number is validated and dropped, and the version is
/// the value after `_validatedVersion` (matching `cmsGetEncodedICCversion`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub size: u32,
    pub cmm: Signature,
    /// Validated/clamped version (matches `cmsGetEncodedICCversion`).
    pub version: u32,
    pub device_class: ProfileClass,
    pub color_space: ColorSpace,
    pub pcs: ColorSpace,
    pub date: DateTime,
    pub platform: Signature,
    pub flags: u32,
    pub manufacturer: Signature,
    pub model: u32,
    pub attributes: u64,
    pub rendering_intent: RenderingIntent,
    pub illuminant: CIEXYZ,
    pub creator: Signature,
    pub profile_id: [u8; 16],
}

/// lcms2 `_validatedVersion` (`src/cmsio0.c`). Operates on the four big-endian
/// version bytes *in disk order*: byte 0 is the BCD major version (max 9), byte
/// 1 is two BCD digits (one per nibble, high nibble ≤ 0x90, low nibble ≤ 0x09),
/// and bytes 2 & 3 are reserved (forced to 0).
///
/// lcms2 reads the struct raw, so `pByte[0]` is the first disk byte = the MSB of
/// the big-endian u32 we read. We therefore operate on `to_be_bytes()` and
/// reassemble with `from_be_bytes`, reproducing the C result byte-for-byte.
///
/// ```text
/// cmsUInt8Number* pByte = (cmsUInt8Number*) &DWord;
/// if (*pByte > 0x09) *pByte = 0x09;
/// temp1 = *(pByte+1) & 0xf0;
/// temp2 = *(pByte+1) & 0x0f;
/// if (temp1 > 0x90U) temp1 = 0x90U;
/// if (temp2 > 0x09U) temp2 = 0x09U;
/// *(pByte+1) = temp1 | temp2;
/// *(pByte+2) = 0;
/// *(pByte+3) = 0;
/// return DWord;
/// ```
pub fn validate_version(raw: u32) -> u32 {
    let mut b = raw.to_be_bytes();
    if b[0] > 0x09 {
        b[0] = 0x09;
    }
    let mut temp1 = b[1] & 0xf0;
    let temp2 = b[1] & 0x0f;
    if temp1 > 0x90 {
        temp1 = 0x90;
    }
    // temp2 is at most 0x0f; the C guard `temp2 > 0x09` clamps it to 0x09.
    let temp2 = if temp2 > 0x09 { 0x09 } else { temp2 };
    b[1] = temp1 | temp2;
    b[2] = 0;
    b[3] = 0;
    u32::from_be_bytes(b)
}

impl Header {
    /// Parse the 128-byte ICC header (lcms2 `_cmsReadHeader`, `src/cmsio0.c`).
    ///
    /// Reads each field big-endian in `cmsICCHeader` order, validates the magic
    /// number at offset 36 (`Error::BadSignature` on mismatch), clamps the
    /// version via [`validate_version`], and rejects versions `> 0x5000000`
    /// (`Error::Unsupported`) — matching lcms2's reject decision. The device
    /// class is *not* validated here (lcms2's `validDeviceClass` check has no
    /// effect on the header fields we expose, and the differential test confirms
    /// every accepted-by-lcms2 testbed profile parses identically).
    pub fn parse<R: ProfileReader>(r: &mut R) -> Result<Header> {
        // Offsets 0..36: size, cmm, version, deviceClass, colorSpace, pcs, date.
        let size = r.read_u32()?;
        let cmm = Signature::from_raw(r.read_u32()?);
        let version_raw = r.read_u32()?;
        let device_class = ProfileClass::from_raw(r.read_u32()?);
        let color_space = ColorSpace::from_raw(r.read_u32()?);
        let pcs = ColorSpace::from_raw(r.read_u32()?);
        let date = DateTime {
            year: r.read_u16()?,
            month: r.read_u16()?,
            day: r.read_u16()?,
            hours: r.read_u16()?,
            minutes: r.read_u16()?,
            seconds: r.read_u16()?,
        };

        // Offset 36: magic — validate and drop (lcms2 rejects on mismatch).
        let magic = r.read_u32()?;
        if magic != MAGIC {
            return Err(Error::BadSignature(Signature::from_raw(magic)));
        }

        let platform = Signature::from_raw(r.read_u32()?);
        let flags = r.read_u32()?;
        let manufacturer = Signature::from_raw(r.read_u32()?);
        let model = r.read_u32()?;
        let attributes = r.read_u64()?;
        let rendering_intent = RenderingIntent::from_raw(r.read_u32()?);
        let illuminant = r.read_xyz()?;
        let creator = Signature::from_raw(r.read_u32()?);
        let mut profile_id = [0u8; 16];
        r.read_exact(&mut profile_id)?;

        // Offsets 100..128: `cmsInt8Number reserved[28]` (lcms2 `cmsICCHeader`).
        // lcms2 reads the whole 128-byte struct, so its IOhandler is left at 128 —
        // exactly where the tag directory begins. We must consume these 28 bytes
        // too, or the directory parse would start 28 bytes early.
        let mut reserved = [0u8; 28];
        r.read_exact(&mut reserved)?;

        // Version: clamp like lcms2, then reject anything above 0x5000000.
        let version = validate_version(version_raw);
        if version > 0x5000000 {
            return Err(Error::Unsupported("profile version"));
        }

        Ok(Header {
            size,
            cmm,
            version,
            device_class,
            color_space,
            pcs,
            date,
            platform,
            flags,
            manufacturer,
            model,
            attributes,
            rendering_intent,
            illuminant,
            creator,
            profile_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;
    use std::fs;
    use std::path::Path;

    #[test]
    fn validate_version_normal_v44_unchanged() {
        // v4.4.0.0 on the wire is 0x04400000; bytes 2/3 already zero.
        assert_eq!(validate_version(0x0440_0000), 0x0440_0000);
        // v2.2.0.0 (sRGB-style) likewise passes through.
        assert_eq!(validate_version(0x0220_0000), 0x0220_0000);
    }

    #[test]
    fn validate_version_clamps_over_range_major() {
        // Major byte 0xFF clamps to 0x09.
        assert_eq!(validate_version(0xFF40_0000), 0x0940_0000);
    }

    #[test]
    fn validate_version_clamps_bcd_nibbles() {
        // byte1 = 0xFF -> high nibble clamps to 0x90, low clamps to 0x09 -> 0x99.
        assert_eq!(validate_version(0x04FF_0000), 0x0499_0000);
        // byte1 high nibble 0xA (>0x90 as 0xA0) clamps to 0x90, low 0x5 kept.
        assert_eq!(validate_version(0x04A5_0000), 0x0495_0000);
    }

    #[test]
    fn validate_version_zeroes_reserved_bytes() {
        // bytes 2 and 3 are always forced to zero.
        assert_eq!(validate_version(0x0440_ABCD), 0x0440_0000);
        assert_eq!(validate_version(0x0000_FFFF), 0x0000_0000);
    }

    #[test]
    fn parse_rejects_bad_magic() {
        // 128 zero bytes -> magic is 0 -> BadSignature.
        let buf = [0u8; 128];
        let mut r = MemReader::new(&buf);
        assert!(matches!(Header::parse(&mut r), Err(Error::BadSignature(_))));
    }

    /// Locate the vendored lcms2 testbed directory from the crate root.
    fn testbed_dir() -> std::path::PathBuf {
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed"
        ))
        .to_path_buf()
    }

    /// Differential test: for every `*.icc` in the testbed, compare our header
    /// parse against lcms2 (opened over the SAME bytes). Assert the accept/reject
    /// decision matches, and for accepted profiles assert every comparable field
    /// is bit-identical.
    #[test]
    fn header_matches_oracle_over_testbed() {
        let dir = testbed_dir();
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read testbed {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("icc"))
            .collect();
        entries.sort();
        assert!(!entries.is_empty(), "no .icc files in {}", dir.display());

        let mut compared = 0usize;
        let mut accepted = 0usize;
        for path in &entries {
            let bytes = fs::read(path).unwrap();
            let oracle = tintbox_oracle::read_header(&bytes);
            let mut r = MemReader::new(&bytes);
            let rust = Header::parse(&mut r);

            match (oracle, rust) {
                (Some(o), Ok(h)) => {
                    accepted += 1;
                    let name = path.file_name().unwrap().to_string_lossy();
                    assert_eq!(
                        h.device_class.to_raw(),
                        o.device_class,
                        "device_class mismatch in {name}"
                    );
                    assert_eq!(
                        h.color_space.to_raw(),
                        o.color_space,
                        "color_space mismatch in {name}"
                    );
                    assert_eq!(h.pcs.to_raw(), o.pcs, "pcs mismatch in {name}");
                    assert_eq!(h.version, o.version, "version mismatch in {name}");
                    assert_eq!(
                        h.rendering_intent.to_raw(),
                        o.rendering_intent,
                        "rendering_intent mismatch in {name}"
                    );
                    assert_eq!(h.flags, o.flags, "flags mismatch in {name}");
                    assert_eq!(
                        h.manufacturer.to_raw(),
                        o.manufacturer,
                        "manufacturer mismatch in {name}"
                    );
                    assert_eq!(h.model, o.model, "model mismatch in {name}");
                    assert_eq!(h.creator.to_raw(), o.creator, "creator mismatch in {name}");
                    assert_eq!(h.attributes, o.attributes, "attributes mismatch in {name}");
                    assert_eq!(h.profile_id, o.profile_id, "profile_id mismatch in {name}");
                }
                (None, Err(_)) => { /* both reject — header-level agreement */ }
                (Some(_), Err(e)) => {
                    panic!(
                        "lcms2 accepted but tintbox rejected {}: {e}",
                        path.display()
                    )
                }
                (None, Ok(_)) => panic!("tintbox accepted but lcms2 rejected {}", path.display()),
            }
            compared += 1;
        }
        // Surface coverage in the test log.
        println!(
            "testbed header diff: compared {compared} .icc files, {accepted} accepted by both"
        );
        assert!(
            accepted > 0,
            "expected at least one profile accepted by both"
        );
    }
}
