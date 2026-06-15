//! The tone-curve tag-type readers, transcribed from lcms2 `src/cmstypes.c`.
//! Both `cmsSigCurveType` (`'curv'`) and `cmsSigParametricCurveType` (`'para'`)
//! decode to a `cmsToneCurve`, modelled here as [`crate::curve::ToneCurve`] and
//! wrapped in [`Tag::Curve`]. Each reader takes the positioned reader `r`
//! (already past the 8-byte type base) and `size` = `TagSize - 8` (the byte
//! count the C handler receives as `SizeOfTag`, unused by these readers).

use crate::curve::{build_gamma, build_parametric, build_tabulated_16};
use crate::error::{Error, Result};
use crate::fixed::U8Fixed8;
use crate::io::ProfileReader;
use crate::profile::tag::Tag;

/// `Type_Curve_Read` (`cmstypes.c:1333`). Read `Count` (u32):
/// - `Count == 0`: a linear curve — lcms2 `cmsBuildParametricToneCurve(1, {1.0})`,
///   i.e. [`build_gamma`] with gamma 1.0.
/// - `Count == 1`: a single gamma exponent stored as a `cmsU8Fixed8Number` (u16)
///   decoded via `_cms8Fixed8toDouble`, then [`build_gamma`].
/// - `Count > 1`: a 16-bit tabulated curve of `Count` entries (lcms2 caps at
///   `0x7FFF` to reject hostile sizes), read as a big-endian u16 array.
pub fn read_curve<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let count = r.read_u32()?;
    let curve = match count {
        0 => build_gamma(1.0),
        1 => {
            let fixed = U8Fixed8::from_raw(r.read_u16()?);
            build_gamma(fixed.to_f64())
        }
        _ => {
            // lcms2: "This is to prevent bad guys for doing bad things".
            if count > 0x7FFF {
                return Err(Error::Corrupt("curve entry count exceeds 0x7FFF"));
            }
            let table = r.read_u16_array(count as usize)?;
            build_tabulated_16(&table)
        }
    };
    Ok(Tag::Curve(curve))
}

/// lcms2 `ParamsByType` (`cmstypes.c:1453`): the coefficient count for each ICC
/// parametric curve type 0..=4 (one segment each).
const PARAMS_BY_TYPE: [usize; 5] = [1, 3, 4, 5, 7];

/// `Type_ParametricCurve_Read` (`cmstypes.c:1451`). Read the ICC parametric curve
/// `Type` (u16), skip a reserved u16, then read `PARAMS_BY_TYPE[Type]` parameters
/// each as a 15.16 fixed (`_cmsRead15Fixed16Number` → f64). lcms2 rejects
/// `Type > 4`. The lcms2 curve type is the ICC type plus one
/// (`cmsBuildParametricToneCurve(Type + 1, Params)`).
pub fn read_parametric_curve<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let icc_type = r.read_u16()?;
    let _reserved = r.read_u16()?;

    if icc_type > 4 {
        return Err(Error::Corrupt("unknown parametric curve type"));
    }

    let n = PARAMS_BY_TYPE[icc_type as usize];
    let mut params = [0.0f64; 10];
    for slot in params.iter_mut().take(n) {
        *slot = r.read_s15f16()?.to_f64();
    }

    // lcms2 curve type = ICC type + 1; build_parametric reads params[0..n].
    let curve = build_parametric(icc_type as i32 + 1, &params)
        .ok_or(Error::Corrupt("parametric curve build failed"))?;
    Ok(Tag::Curve(curve))
}

/// lcms2 `cmsVideoCardGammaTableType` — gamma stored as an on-disk table.
const VCGT_TABLE_TYPE: u32 = 0;
/// lcms2 `cmsVideoCardGammaFormulaType` — gamma stored as a gamma/min/max formula.
const VCGT_FORMULA_TYPE: u32 = 1;

/// lcms2 `FROM_8_TO_16(rgb)` (lcms2_internal.h:125): replicate the 8-bit value
/// into both bytes of a 16-bit word (`(v << 8) | v`), so `0xFF → 0xFFFF`.
fn from_8_to_16(v: u8) -> u16 {
    ((v as u16) << 8) | v as u16
}

/// `Type_vcgt_Read` (`cmstypes.c:4943`). The video card gamma tag has two
/// flavors, distinguished by a leading `TagType` u32:
///
/// - **Table** (`TagType == 0`): read `nChannels` (must be 3), `nElems`, `nBytes`.
///   lcms2's Adobe quirk fixup forces `nBytes = 2` when `nElems == 256 &&
///   nBytes == 1 && SizeOfTag == 1576`. Then read 3 channels × `nElems` samples,
///   each either an 8-bit value scaled via [`from_8_to_16`] or a 16-bit value read
///   verbatim, into a [`build_tabulated_16`] curve per channel.
/// - **Formula** (`TagType == 1`): read per channel (3) `gamma`/`min`/`max` each as
///   a 15.16 fixed. lcms2 maps the vcgt formula `Y = (max - min)·X^gamma + min`
///   onto an ICC type-5 parametric curve `Y = (aX + b)^g + e` with
///   `params = [gamma, (max-min)^(1/gamma), 0, 0, 0, min, 0]`, built via
///   [`build_parametric`]`(5, …)`.
///
/// `_size` is lcms2's `SizeOfTag`; the table-variant Adobe fixup needs it.
pub fn read_vcgt<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let tag_type = r.read_u32()?;
    match tag_type {
        VCGT_TABLE_TYPE => {
            let n_channels = r.read_u16()?;
            if n_channels != 3 {
                return Err(Error::Corrupt("unsupported number of channels for VCGT"));
            }
            let n_elems = r.read_u16()? as usize;
            let mut n_bytes = r.read_u16()?;

            // Adobe's quirk fixup. Fixing broken profiles...
            if n_elems == 256 && n_bytes == 1 && size == 1576 {
                n_bytes = 2;
            }

            let mut curves = Vec::with_capacity(3);
            for _ in 0..3 {
                let table = match n_bytes {
                    1 => {
                        let mut t = Vec::with_capacity(n_elems);
                        for _ in 0..n_elems {
                            t.push(from_8_to_16(r.read_u8()?));
                        }
                        t
                    }
                    2 => r.read_u16_array(n_elems)?,
                    _ => return Err(Error::Corrupt("unsupported bit depth for VCGT")),
                };
                curves.push(build_tabulated_16(&table));
            }
            Ok(Tag::Vcgt(curves))
        }
        VCGT_FORMULA_TYPE => {
            let mut curves = Vec::with_capacity(3);
            for _ in 0..3 {
                let gamma = r.read_s15f16()?.to_f64();
                let min = r.read_s15f16()?.to_f64();
                let max = r.read_s15f16()?.to_f64();

                // Parametric curve type 5: Y = (aX + b)^Gamma + e  (X >= d)
                //                          Y = cX + f               (X < d)
                // vcgt formula:            Y = (Max - Min)·X^Gamma + Min
                // => a = (Max - Min)^(1/Gamma), e = Min, b = c = d = f = 0.
                let params = [
                    gamma,
                    (max - min).powf(1.0 / gamma),
                    0.0,
                    0.0,
                    0.0,
                    min,
                    0.0,
                ];
                let curve = build_parametric(5, &params)
                    .ok_or(Error::Corrupt("vcgt formula curve build failed"))?;
                curves.push(curve);
            }
            Ok(Tag::Vcgt(curves))
        }
        _ => Err(Error::Corrupt("unsupported tag type for VCGT")),
    }
}

/// `Type_UcrBg_Read` (`cmstypes.c:3789`). Read the under-color-removal curve
/// (`CountUcr` u32 + `CountUcr` × u16 → [`build_tabulated_16`]), then the
/// black-generation curve (`CountBg` u32 + `CountBg` × u16), then the remaining
/// `SizeOfTag` bytes as an ASCII description string (NUL-terminated by lcms2 and
/// copied up to the first NUL via `cmsMLUsetASCII`). `size` is lcms2's `SizeOfTag`
/// (`TagSize - 8`), the byte budget the description length is computed from.
pub fn read_ucrbg<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let mut remaining = size as i64;

    // First curve is under-color removal.
    if remaining < 4 {
        return Err(Error::Corrupt("UcrBg tag too small for UCR count"));
    }
    let count_ucr = r.read_u32()? as usize;
    remaining -= 4;
    if remaining < (count_ucr * 2) as i64 {
        return Err(Error::Corrupt("UcrBg tag too small for UCR table"));
    }
    let ucr_table = r.read_u16_array(count_ucr)?;
    remaining -= (count_ucr * 2) as i64;

    // Second curve is black generation.
    if remaining < 4 {
        return Err(Error::Corrupt("UcrBg tag too small for BG count"));
    }
    let count_bg = r.read_u32()? as usize;
    remaining -= 4;
    if remaining < (count_bg * 2) as i64 {
        return Err(Error::Corrupt("UcrBg tag too small for BG table"));
    }
    let bg_table = r.read_u16_array(count_bg)?;
    remaining -= (count_bg * 2) as i64;

    // lcms2 rejects a negative or absurdly large trailing text length.
    if !(0..=32000).contains(&remaining) {
        return Err(Error::Corrupt("UcrBg description length out of range"));
    }

    // The remaining bytes are the ASCII description (lcms2 force-terminates and
    // copies up to the first NUL via cmsMLUsetASCII).
    let desc = r.read_ascii(remaining as usize)?;

    Ok(Tag::UcrBg {
        ucr: build_tabulated_16(&ucr_table),
        bg: build_tabulated_16(&bg_table),
        desc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// `Count == 0` → an identity (gamma-1.0) linear curve, exactly
    /// `cmsBuildParametricToneCurve(1, {1.0})` == [`build_gamma(1.0)`].
    #[test]
    fn curve_count0_is_identity() {
        let body = 0u32.to_be_bytes(); // Count = 0
        let mut r = MemReader::new(&body);
        match read_curve(&mut r, body.len() as u32).unwrap() {
            Tag::Curve(c) => assert_eq!(c, build_gamma(1.0)),
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// `Count == 1` → a single gamma exponent stored as a `cmsU8Fixed8Number`.
    /// `0x0240` = 2.25 (576 / 256), so the curve is `build_gamma(2.25)`.
    #[test]
    fn curve_count1_single_gamma() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // Count = 1
        body.extend_from_slice(&0x0240u16.to_be_bytes()); // 8.8 fixed = 2.25
        let mut r = MemReader::new(&body);
        match read_curve(&mut r, body.len() as u32).unwrap() {
            Tag::Curve(c) => {
                assert_eq!(c, build_gamma(2.25));
                // Sanity: the stored exponent decodes via _cms8Fixed8toDouble.
                assert_eq!(U8Fixed8::from_raw(0x0240).to_f64(), 2.25);
            }
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// `Count > 1` → a 16-bit tabulated curve copying the on-disk samples verbatim.
    #[test]
    fn curve_count_n_tabulated() {
        let table: [u16; 4] = [0, 0x5555, 0xAAAA, 0xFFFF];
        let mut body = Vec::new();
        body.extend_from_slice(&(table.len() as u32).to_be_bytes());
        for v in table {
            body.extend_from_slice(&v.to_be_bytes());
        }
        let mut r = MemReader::new(&body);
        match read_curve(&mut r, body.len() as u32).unwrap() {
            Tag::Curve(c) => assert_eq!(c, build_tabulated_16(&table)),
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// `Count > 0x7FFF` is rejected (lcms2's hostile-size guard).
    #[test]
    fn curve_count_too_large_rejected() {
        let body = 0x8000u32.to_be_bytes();
        let mut r = MemReader::new(&body);
        assert!(matches!(
            read_curve(&mut r, body.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }

    /// `para` type 0 (ICC) → lcms2 type 1, one s15Fixed16 param. `0x0002_0000` =
    /// 2.0, so this is `build_parametric(1, {2.0})`.
    #[test]
    fn parametric_type0_gamma() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u16.to_be_bytes()); // ICC Type 0
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // gamma = 2.0
        let mut r = MemReader::new(&body);
        match read_parametric_curve(&mut r, body.len() as u32).unwrap() {
            Tag::Curve(c) => assert_eq!(c, build_parametric(1, &[2.0]).unwrap()),
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// `para` type 3 (ICC) → lcms2 type 4, five s15Fixed16 params (a sRGB-like set).
    #[test]
    fn parametric_type3_params() {
        // ParamsByType[3] = 5 params; values picked to be exact in 15.16.
        let raws: [u32; 5] = [
            0x0002_0000, // 2.0
            0x0000_8000, // 0.5
            0x0000_4000, // 0.25
            0x0000_2000, // 0.125
            0x0000_1000, // 0.0625
        ];
        let mut body = Vec::new();
        body.extend_from_slice(&3u16.to_be_bytes()); // ICC Type 3
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        for raw in raws {
            body.extend_from_slice(&raw.to_be_bytes());
        }
        let mut r = MemReader::new(&body);
        let expected = build_parametric(4, &[2.0, 0.5, 0.25, 0.125, 0.0625]).unwrap();
        match read_parametric_curve(&mut r, body.len() as u32).unwrap() {
            Tag::Curve(c) => assert_eq!(c, expected),
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// ICC parametric type > 4 is rejected (lcms2's unknown-extension guard).
    #[test]
    fn parametric_unknown_type_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&5u16.to_be_bytes()); // ICC Type 5 (> 4)
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        let mut r = MemReader::new(&body);
        assert!(matches!(
            read_parametric_curve(&mut r, body.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }

    /// vcgt table variant with 1-byte entries: each value is scaled via
    /// `FROM_8_TO_16` (`(v << 8) | v`) into the curve's 16-bit table.
    #[test]
    fn vcgt_table_1byte_scales_via_from_8_to_16() {
        let n_elems = 4u16;
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // table type
        body.extend_from_slice(&3u16.to_be_bytes()); // nChannels
        body.extend_from_slice(&n_elems.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // 1 byte/entry
        let samples: [u8; 4] = [0x00, 0x40, 0x80, 0xFF];
        for _ in 0..3 {
            body.extend_from_slice(&samples);
        }
        let mut r = MemReader::new(&body);
        let expected: Vec<u16> = samples.iter().map(|&v| from_8_to_16(v)).collect();
        match read_vcgt(&mut r, body.len() as u32).unwrap() {
            Tag::Vcgt(curves) => {
                assert_eq!(curves.len(), 3);
                for c in &curves {
                    assert_eq!(*c, build_tabulated_16(&expected));
                }
            }
            other => panic!("expected Vcgt, got {other:?}"),
        }
        // 0xFF must replicate to 0xFFFF, 0x00 to 0x0000.
        assert_eq!(from_8_to_16(0xFF), 0xFFFF);
        assert_eq!(from_8_to_16(0x00), 0x0000);
    }

    /// vcgt formula variant: the ICC type-5 params are
    /// `[gamma, (max-min)^(1/gamma), 0, 0, 0, min, 0]`.
    #[test]
    fn vcgt_formula_builds_type5_params() {
        let s15f16 = |v: f64| ((v * 65536.0).round() as i32) as u32;
        // Decode back through s15Fixed16 so `expected` uses the exact values the
        // reader sees (gamma 2.2 is not exactly representable in 15.16).
        let unfix = |raw: u32| (raw as i32) as f64 / 65536.0;
        let (g, mn, mx) = (2.2f64, 0.0625f64, 0.9375f64);
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // formula type
        for _ in 0..3 {
            body.extend_from_slice(&s15f16(g).to_be_bytes());
            body.extend_from_slice(&s15f16(mn).to_be_bytes());
            body.extend_from_slice(&s15f16(mx).to_be_bytes());
        }
        let mut r = MemReader::new(&body);
        let (g, mn, mx) = (unfix(s15f16(g)), unfix(s15f16(mn)), unfix(s15f16(mx)));
        let params = [g, (mx - mn).powf(1.0 / g), 0.0, 0.0, 0.0, mn, 0.0];
        let expected = build_parametric(5, &params).unwrap();
        match read_vcgt(&mut r, body.len() as u32).unwrap() {
            Tag::Vcgt(curves) => {
                for c in &curves {
                    assert_eq!(*c, expected);
                }
            }
            other => panic!("expected Vcgt, got {other:?}"),
        }
    }

    /// vcgt with `nChannels != 3` is rejected (lcms2's unknown-extension guard).
    #[test]
    fn vcgt_non3_channels_rejected() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // nChannels = 1
        body.extend_from_slice(&4u16.to_be_bytes());
        body.extend_from_slice(&2u16.to_be_bytes());
        let mut r = MemReader::new(&body);
        assert!(matches!(
            read_vcgt(&mut r, body.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }

    /// UcrBg: UCR table, BG table, trailing ASCII description.
    #[test]
    fn ucrbg_reads_curves_and_desc() {
        let ucr: [u16; 3] = [0, 0x8000, 0xFFFF];
        let bg: [u16; 2] = [0xFFFF, 0];
        let desc = b"hello";
        let mut body = Vec::new();
        body.extend_from_slice(&(ucr.len() as u32).to_be_bytes());
        for v in ucr {
            body.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(&(bg.len() as u32).to_be_bytes());
        for v in bg {
            body.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(desc);
        let mut r = MemReader::new(&body);
        match read_ucrbg(&mut r, body.len() as u32).unwrap() {
            Tag::UcrBg {
                ucr: u,
                bg: b,
                desc: d,
            } => {
                assert_eq!(u, build_tabulated_16(&ucr));
                assert_eq!(b, build_tabulated_16(&bg));
                assert_eq!(d, "hello");
            }
            other => panic!("expected UcrBg, got {other:?}"),
        }
    }
}
