//! The trivial tag-type readers, each transcribed from lcms2 `src/cmstypes.c`.
//! "Trivial" = a flat read with no nested structure. Every reader takes the
//! positioned reader `r` (already past the 8-byte type base) and `size` =
//! `TagSize - 8` (the byte count the C handler receives as `SizeOfTag`), and
//! returns the cooked [`Tag`].

use crate::color::{CIExyY, CIExyYTriple};
use crate::error::{Error, Result};
use crate::fixed::U16Fixed16;
use crate::io::ProfileReader;
use crate::profile::header::DateTime;
use crate::profile::tag::Tag;
use crate::sig::Signature;

/// lcms2 `cmsMAXCHANNELS` (`include/lcms2.h:690`).
const MAX_CHANNELS: u32 = 16;

/// `Type_XYZ_Read` (`cmstypes.c:347`): one `_cmsReadXYZNumber`. `SizeOfTag` unused.
pub fn read_xyz<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    Ok(Tag::Xyz(r.read_xyz()?))
}

/// `Type_S15Fixed16_Read` (`cmstypes.c:752`): `n = SizeOfTag / 4` s15Fixed16
/// values. lcms2 stores them as f64 (`raw / 65536`); we keep the raw fixed
/// values, which is a lossless superset (the f64 is `S15Fixed16::to_f64`).
pub fn read_s15fixed16_array<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let n = size / 4;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(r.read_s15f16()?);
    }
    Ok(Tag::S15Fixed16Array(v))
}

/// `Type_U16Fixed16_Read` (`cmstypes.c:812`): `n = SizeOfTag / 4` u16Fixed16
/// values (read as raw u32, lcms2 then divides by 65536 into f64). We keep the
/// raw u32.
pub fn read_u16fixed16_array<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let n = size / 4;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(U16Fixed16::from_raw(r.read_u32()?));
    }
    Ok(Tag::U16Fixed16Array(v))
}

/// `Type_UInt8_Read` (`cmstypes.c:579`): `n = SizeOfTag` raw bytes.
pub fn read_uint8_array<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let mut v = vec![0u8; size as usize];
    r.read_exact(&mut v)?;
    Ok(Tag::U8Array(v))
}

/// `Type_UInt32_Read` (`cmstypes.c:636`): `n = SizeOfTag / 4` big-endian u32.
pub fn read_uint32_array<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let n = size / 4;
    Ok(Tag::U32Array(r.read_u32_array(n as usize)?))
}

/// `Type_Signature_Read` (`cmstypes.c:879`): one `_cmsReadUInt32Number`.
pub fn read_signature<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    Ok(Tag::Signature(Signature::from_raw(r.read_u32()?)))
}

/// `Type_Data_Read` (`cmstypes.c:1029`): require `SizeOfTag >= 4`; the flag is a
/// u32, the remaining `SizeOfTag - 4` bytes are opaque data.
pub fn read_data<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    if size < 4 {
        return Err(Error::Corrupt("data tag too small"));
    }
    let flag = r.read_u32()?;
    let len = size - 4;
    let mut data = vec![0u8; len as usize];
    r.read_exact(&mut data)?;
    Ok(Tag::Data { flag, data })
}

/// `Type_DateTime_Read` (`cmstypes.c:1556`): read a `cmsDateTimeNumber` (six
/// big-endian u16: year, month, day, hours, minutes, seconds) and decode via
/// `_cmsDecodeDateTimeNumber`. We keep the wire values (matching the `DateTime`
/// type), not the C `struct tm`'s month-1 / year-1900 adjustment.
pub fn read_datetime<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let year = r.read_u16()?;
    let month = r.read_u16()?;
    let day = r.read_u16()?;
    let hours = r.read_u16()?;
    let minutes = r.read_u16()?;
    let seconds = r.read_u16()?;
    Ok(Tag::DateTime(DateTime {
        year,
        month,
        day,
        hours,
        minutes,
        seconds,
    }))
}

/// `Type_Chromaticity_Read` (`cmstypes.c:407`), including the §7.8 lcms1-bug
/// recovery. The exact C:
///
/// ```c
/// if (!_cmsReadUInt16Number(io, &nChans)) goto Error;
/// // Let's recover from a bug introduced in early versions of lcms1
/// if (nChans == 0 && SizeOfTag == 32) {
///     if (!_cmsReadUInt16Number(io, NULL)) goto Error;   // skip one u16
///     if (!_cmsReadUInt16Number(io, &nChans)) goto Error;
/// }
/// if (nChans != 3) goto Error;
/// if (!_cmsReadUInt16Number(io, &Table)) goto Error;     // Table, discarded
/// if (!_cmsRead15Fixed16Number(io, &chrm->Red.x)) goto Error;
/// if (!_cmsRead15Fixed16Number(io, &chrm->Red.y)) goto Error;
/// chrm->Red.Y = 1.0;
/// ... Green, Blue identically, each .Y = 1.0 ...
/// ```
///
/// Note the chromaticity coordinates are `_cmsRead15Fixed16Number` (s15Fixed16
/// decoded to f64), NOT plain u16.
pub fn read_chromaticity<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let mut n_chans = r.read_u16()?;

    // lcms1-bug recovery: a 32-byte tag that starts with a 0 channel count has a
    // spurious leading u16; skip it and re-read the real count.
    if n_chans == 0 && size == 32 {
        let _skipped = r.read_u16()?;
        n_chans = r.read_u16()?;
    }

    if n_chans != 3 {
        return Err(Error::Corrupt("chromaticity channels != 3"));
    }

    let _table = r.read_u16()?; // encoding type, discarded by lcms2

    let red = read_xy(r)?;
    let green = read_xy(r)?;
    let blue = read_xy(r)?;

    Ok(Tag::Chromaticity(CIExyYTriple { red, green, blue }))
}

/// One chromaticity coordinate pair: two `_cmsRead15Fixed16Number`, with the
/// luminance `Y` hardcoded to 1.0 (lcms2 `chrm->*.Y = 1.0`).
fn read_xy<R: ProfileReader>(r: &mut R) -> Result<CIExyY> {
    let x = r.read_s15f16()?.to_f64();
    let y = r.read_s15f16()?.to_f64();
    Ok(CIExyY { x, y, yy: 1.0 })
}

/// `Type_Text_Read` (`cmstypes.c:925`): read `SizeOfTag` bytes, force-terminate
/// with a NUL, store the text up to the first NUL as ASCII. We reuse the
/// reader's `read_ascii`, which already implements that convention.
pub fn read_text<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    Ok(Tag::Text(r.read_ascii(size as usize)?))
}

/// `Type_ColorantOrderType_Read` (`cmstypes.c:509`): read a u32 `Count`
/// (rejected if `> cmsMAXCHANNELS`), then `Count` bytes — lcms2 pads a 16-byte
/// array with 0xFF and overwrites the first `Count`. The cooked value is just
/// those `Count` ordering bytes.
pub fn read_colorant_order<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let count = r.read_u32()?;
    if count > MAX_CHANNELS {
        return Err(Error::Corrupt("colorant order count > MAXCHANNELS"));
    }
    let mut order = vec![0u8; count as usize];
    r.read_exact(&mut order)?;
    Ok(Tag::ColorantOrder(order))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// Synthesize the lcms1-bug Chromaticity case: a 32-byte payload whose first
    /// u16 channel count is 0, followed by a spurious u16, then the *real* count
    /// (3), then Table + 3×(x,y) as s15Fixed16. The reader must skip the spurious
    /// u16 and recover, yielding the three chromaticities.
    #[test]
    fn chromaticity_lcms1_bug_recovery() {
        // Build a 32-byte payload (SizeOfTag == 32 triggers the recovery branch).
        let mut b = Vec::new();
        b.extend_from_slice(&0u16.to_be_bytes()); // nChans == 0 (the bug)
        b.extend_from_slice(&0xABCDu16.to_be_bytes()); // spurious u16, skipped
        b.extend_from_slice(&3u16.to_be_bytes()); // real nChans == 3
        b.extend_from_slice(&0u16.to_be_bytes()); // Table, discarded
                                                  // 3 channels × (x, y) as s15Fixed16. Use 0.5 == 0x00008000.
        let half = 0x0000_8000u32; // 0.5 in s15Fixed16
        for _ in 0..6 {
            b.extend_from_slice(&half.to_be_bytes());
        }
        assert_eq!(b.len(), 32, "payload must be exactly 32 bytes");

        let mut r = MemReader::new(&b);
        let tag = read_chromaticity(&mut r, 32).expect("recovered chromaticity");
        match tag {
            Tag::Chromaticity(t) => {
                for ch in [t.red, t.green, t.blue] {
                    assert_eq!(ch.x, 0.5);
                    assert_eq!(ch.y, 0.5);
                    assert_eq!(ch.yy, 1.0);
                }
            }
            other => panic!("expected Chromaticity, got {other:?}"),
        }
    }

    /// The non-bug path: a well-formed payload (nChans == 3 immediately) reads
    /// without skipping anything.
    #[test]
    fn chromaticity_well_formed() {
        let mut b = Vec::new();
        b.extend_from_slice(&3u16.to_be_bytes()); // nChans
        b.extend_from_slice(&0u16.to_be_bytes()); // Table
        let one = 0x0001_0000u32; // 1.0 in s15Fixed16
        for _ in 0..6 {
            b.extend_from_slice(&one.to_be_bytes());
        }
        let mut r = MemReader::new(&b);
        // SizeOfTag here is 28, not 32, so the recovery branch must NOT trigger.
        let tag = read_chromaticity(&mut r, b.len() as u32).expect("chromaticity");
        match tag {
            Tag::Chromaticity(t) => {
                assert_eq!(t.red.x, 1.0);
                assert_eq!(t.blue.y, 1.0);
            }
            other => panic!("expected Chromaticity, got {other:?}"),
        }
    }

    /// A 0-channel-count tag that is NOT 32 bytes does not get the recovery and
    /// fails the `nChans == 3` check.
    #[test]
    fn chromaticity_zero_chans_non32_rejects() {
        let mut b = Vec::new();
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        let mut r = MemReader::new(&b);
        assert!(matches!(
            read_chromaticity(&mut r, b.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }
}
