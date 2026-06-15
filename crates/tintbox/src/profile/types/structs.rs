//! The struct-shaped tag-type readers, each transcribed from lcms2
//! `src/cmstypes.c`. "Struct-shaped" = a fixed record (or a counted array of
//! records) with no MLU/curve nesting. Every reader takes the positioned reader
//! `r` (already past the 8-byte type base) and `size` = `TagSize - 8` (the byte
//! count the C handler receives as `SizeOfTag`), and returns the cooked [`Tag`].

use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::profile::tag::{
    Cicp, ColorantTableEntry, CrdInfo, Measurement, Screening, ScreeningChannel, Tag,
    ViewingConditions,
};

/// lcms2 `cmsMAXCHANNELS` (`include/lcms2.h`).
const MAX_CHANNELS: u32 = 16;

/// `Type_Measurement_Read` (`cmstypes.c:1615`). Reads a
/// `cmsICCMeasurementConditions`:
///
/// ```c
/// if (!_cmsReadUInt32Number(io, &mc.Observer)) return NULL;
/// if (!_cmsReadXYZNumber(io,    &mc.Backing)) return NULL;
/// if (!_cmsReadUInt32Number(io, &mc.Geometry)) return NULL;
/// if (!_cmsRead15Fixed16Number(io, &mc.Flare)) return NULL;   // s15Fixed16 -> f64
/// if (!_cmsReadUInt32Number(io, &mc.IlluminantType)) return NULL;
/// ```
///
/// Note `Flare` is `_cmsRead15Fixed16Number` (s15Fixed16 decoded to f64), NOT a
/// u16Fixed16. `SizeOfTag` is unused by the C.
pub fn read_measurement<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let observer = r.read_u32()?;
    let backing = r.read_xyz()?;
    let geometry = r.read_u32()?;
    let flare = r.read_s15f16()?.to_f64();
    let illuminant_type = r.read_u32()?;
    Ok(Tag::Measurement(Measurement {
        observer,
        backing,
        geometry,
        flare,
        illuminant_type,
    }))
}

/// `Type_ViewingConditions_Read` (`cmstypes.c:4135`). Reads a
/// `cmsICCViewingConditions`:
///
/// ```c
/// if (!_cmsReadXYZNumber(io, &vc->IlluminantXYZ)) goto Error;
/// if (!_cmsReadXYZNumber(io, &vc->SurroundXYZ)) goto Error;
/// if (!_cmsReadUInt32Number(io, &vc->IlluminantType)) goto Error;
/// ```
pub fn read_viewing_conditions<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let illuminant_xyz = r.read_xyz()?;
    let surround_xyz = r.read_xyz()?;
    let illuminant_type = r.read_u32()?;
    Ok(Tag::ViewingConditions(ViewingConditions {
        illuminant_xyz,
        surround_xyz,
        illuminant_type,
    }))
}

/// `Type_Screening_Read` (`cmstypes.c:4048`):
///
/// ```c
/// if (!_cmsReadUInt32Number(io, &sc->Flag)) goto Error;
/// if (!_cmsReadUInt32Number(io, &sc->nChannels)) goto Error;
/// if (sc->nChannels > cmsMAXCHANNELS - 1)
///     sc->nChannels = cmsMAXCHANNELS - 1;          // cap; original count is lost
/// for (i=0; i < sc->nChannels; i++) {
///     if (!_cmsRead15Fixed16Number(io, &sc->Channels[i].Frequency)) goto Error;
///     if (!_cmsRead15Fixed16Number(io, &sc->Channels[i].ScreenAngle)) goto Error;
///     if (!_cmsReadUInt32Number(io, &sc->Channels[i].SpotShape)) goto Error;
/// }
/// ```
///
/// lcms2 overwrites `sc->nChannels` with the cap, so a profile declaring more
/// than `cmsMAXCHANNELS - 1` channels reads only the first `cmsMAXCHANNELS - 1`
/// and reports that capped count. We replicate that exactly: `n_channels` is the
/// capped count, and only that many channels are read.
pub fn read_screening<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let flag = r.read_u32()?;
    let mut n_channels = r.read_u32()?;
    if n_channels > MAX_CHANNELS - 1 {
        n_channels = MAX_CHANNELS - 1;
    }
    let mut channels = Vec::with_capacity(n_channels as usize);
    for _ in 0..n_channels {
        let frequency = r.read_s15f16()?.to_f64();
        let screen_angle = r.read_s15f16()?.to_f64();
        let spot_shape = r.read_u32()?;
        channels.push(ScreeningChannel {
            frequency,
            screen_angle,
            spot_shape,
        });
    }
    Ok(Tag::Screening(Screening {
        flag,
        n_channels,
        channels,
    }))
}

/// lcms2 `ReadCountAndString` (`cmstypes.c:3932`): one counted ASCII string.
///
/// ```c
/// if (*SizeOfTag < sizeof(cmsUInt32Number)) return FALSE;
/// if (!_cmsReadUInt32Number(io, &Count)) return FALSE;
/// if (Count > UINT_MAX - sizeof(cmsUInt32Number)) return FALSE;
/// if (*SizeOfTag < Count + sizeof(cmsUInt32Number)) return FALSE;
/// Text = malloc(Count+1); read Count bytes; Text[Count] = 0;
/// cmsMLUsetASCII(mlu, "PS", Section, Text);
/// *SizeOfTag -= (Count + sizeof(cmsUInt32Number));
/// ```
///
/// `cmsMLUsetASCII` stores the bytes up to the first NUL, so the comparable
/// value is the read `Count` bytes truncated at the first embedded NUL (if any).
/// `size` is the remaining `SizeOfTag`, decremented in place by the consumed
/// `Count + 4`.
fn read_count_and_string<R: ProfileReader>(r: &mut R, size: &mut u32) -> Result<Vec<u8>> {
    if *size < 4 {
        return Err(Error::Corrupt("crdinfo string: size < 4"));
    }
    let count = r.read_u32()?;
    // C guards `Count > UINT_MAX - 4`; in u32 arithmetic that is the overflow
    // case for `Count + 4`, which `checked_add` catches.
    let count_plus = count
        .checked_add(4)
        .ok_or(Error::Corrupt("crdinfo overflow"))?;
    if *size < count_plus {
        return Err(Error::Corrupt("crdinfo string: size < count + 4"));
    }
    let mut text = vec![0u8; count as usize];
    r.read_exact(&mut text)?;
    // cmsMLUsetASCII copies up to the first NUL (it strcpy/strlen-style trims).
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    text.truncate(end);
    *size -= count_plus;
    Ok(text)
}

/// `Type_CrdInfo_Read` (`cmstypes.c:3980`): five counted ASCII strings, the
/// PostScript product name (`nm`) then the four rendering-intent CRD names
/// (`#0`..`#3`):
///
/// ```c
/// if (!ReadCountAndString(self, io, mlu, &SizeOfTag, "nm")) goto Error;
/// if (!ReadCountAndString(self, io, mlu, &SizeOfTag, "#0")) goto Error;
/// if (!ReadCountAndString(self, io, mlu, &SizeOfTag, "#1")) goto Error;
/// if (!ReadCountAndString(self, io, mlu, &SizeOfTag, "#2")) goto Error;
/// if (!ReadCountAndString(self, io, mlu, &SizeOfTag, "#3")) goto Error;
/// ```
pub fn read_crd_info<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let mut remaining = size;
    let product_name = read_count_and_string(r, &mut remaining)?;
    let crd_names = [
        read_count_and_string(r, &mut remaining)?,
        read_count_and_string(r, &mut remaining)?,
        read_count_and_string(r, &mut remaining)?,
        read_count_and_string(r, &mut remaining)?,
    ];
    Ok(Tag::CrdInfo(CrdInfo {
        product_name,
        crd_names,
    }))
}

/// `Type_VideoSignal_Read` (`cmstypes.c:5614`):
///
/// ```c
/// if (SizeOfTag != 4) return NULL;
/// if (!_cmsReadUInt8Number(io, &cicp->ColourPrimaries)) goto Error;
/// if (!_cmsReadUInt8Number(io, &cicp->TransferCharacteristics)) goto Error;
/// if (!_cmsReadUInt8Number(io, &cicp->MatrixCoefficients)) goto Error;
/// if (!_cmsReadUInt8Number(io, &cicp->VideoFullRangeFlag)) goto Error;
/// ```
///
/// The `SizeOfTag != 4` guard is exact: lcms2 rejects any payload that is not
/// exactly four bytes (the four H.273 coding-parameter bytes; the type base's
/// reserved word already consumed the nominal "reserved" field).
pub fn read_cicp<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    if size != 4 {
        return Err(Error::Corrupt("cicp tag size != 4"));
    }
    let colour_primaries = r.read_u8()?;
    let transfer_characteristics = r.read_u8()?;
    let matrix_coefficients = r.read_u8()?;
    let video_full_range_flag = r.read_u8()?;
    Ok(Tag::Cicp(Cicp {
        colour_primaries,
        transfer_characteristics,
        matrix_coefficients,
        video_full_range_flag,
    }))
}

/// `Type_ColorantTable_Read` (`cmstypes.c:3254`):
///
/// ```c
/// if (!_cmsReadUInt32Number(io, &Count)) return NULL;
/// if (Count > cmsMAXCHANNELS) { signal RANGE; return NULL; }
/// List = cmsAllocNamedColorList(... Count ...);
/// for (i=0; i < Count; i++) {
///     if (io->Read(io, Name, 32, 1) != 1) goto Error;
///     Name[32] = 0;
///     if (!_cmsReadUInt16Array(io, 3, PCS)) goto Error;
///     cmsAppendNamedColor(List, Name, PCS, NULL);
/// }
/// ```
///
/// The name is a fixed 32-byte field, force-NUL-terminated at index 32, so the
/// stored name is the bytes up to the first NUL within those 32. The PCS is
/// three big-endian u16.
pub fn read_colorant_table<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let count = r.read_u32()?;
    if count > MAX_CHANNELS {
        return Err(Error::Corrupt("colorant table count > MAXCHANNELS"));
    }
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut name_buf = [0u8; 32];
        r.read_exact(&mut name_buf)?;
        let end = name_buf.iter().position(|&b| b == 0).unwrap_or(32);
        let name: String = name_buf[..end].iter().map(|&b| b as char).collect();
        let pcs = [r.read_u16()?, r.read_u16()?, r.read_u16()?];
        entries.push(ColorantTableEntry { name, pcs });
    }
    Ok(Tag::ColorantTable(entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// Synthetic `cmsICCViewingConditions`: IlluminantXYZ, SurroundXYZ (each three
    /// s15Fixed16), then IlluminantType u32. Hand-computed expected values.
    #[test]
    fn viewing_conditions_synthetic() {
        let mut b = Vec::new();
        // IlluminantXYZ = (0.5, 1.0, 0.25)
        for raw in [0x0000_8000u32, 0x0001_0000, 0x0000_4000] {
            b.extend_from_slice(&raw.to_be_bytes());
        }
        // SurroundXYZ = (1.5, 2.0, 0.0)
        for raw in [0x0001_8000u32, 0x0002_0000, 0x0000_0000] {
            b.extend_from_slice(&raw.to_be_bytes());
        }
        b.extend_from_slice(&5u32.to_be_bytes()); // IlluminantType = D55
        let mut r = MemReader::new(&b);
        match read_viewing_conditions(&mut r, b.len() as u32).unwrap() {
            Tag::ViewingConditions(v) => {
                assert_eq!(v.illuminant_xyz.x, 0.5);
                assert_eq!(v.illuminant_xyz.y, 1.0);
                assert_eq!(v.illuminant_xyz.z, 0.25);
                assert_eq!(v.surround_xyz.x, 1.5);
                assert_eq!(v.surround_xyz.y, 2.0);
                assert_eq!(v.surround_xyz.z, 0.0);
                assert_eq!(v.illuminant_type, 5);
            }
            other => panic!("expected ViewingConditions, got {other:?}"),
        }
    }

    /// Synthetic Screening: Flag, nChannels=2, then 2 channels of
    /// {Frequency s15f16, ScreenAngle s15f16, SpotShape u32}.
    #[test]
    fn screening_synthetic() {
        let mut b = Vec::new();
        b.extend_from_slice(&7u32.to_be_bytes()); // Flag
        b.extend_from_slice(&2u32.to_be_bytes()); // nChannels
                                                  // ch0: freq 1.0, angle 0.5, spot 3
        b.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        b.extend_from_slice(&0x0000_8000u32.to_be_bytes());
        b.extend_from_slice(&3u32.to_be_bytes());
        // ch1: freq 2.0, angle 0.25, spot 6
        b.extend_from_slice(&0x0002_0000u32.to_be_bytes());
        b.extend_from_slice(&0x0000_4000u32.to_be_bytes());
        b.extend_from_slice(&6u32.to_be_bytes());
        let mut r = MemReader::new(&b);
        match read_screening(&mut r, b.len() as u32).unwrap() {
            Tag::Screening(s) => {
                assert_eq!(s.flag, 7);
                assert_eq!(s.n_channels, 2);
                assert_eq!(s.channels.len(), 2);
                assert_eq!(s.channels[0].frequency, 1.0);
                assert_eq!(s.channels[0].screen_angle, 0.5);
                assert_eq!(s.channels[0].spot_shape, 3);
                assert_eq!(s.channels[1].frequency, 2.0);
                assert_eq!(s.channels[1].screen_angle, 0.25);
                assert_eq!(s.channels[1].spot_shape, 6);
            }
            other => panic!("expected Screening, got {other:?}"),
        }
    }

    /// Screening with nChannels beyond the cap is clamped to cmsMAXCHANNELS - 1
    /// (15), and only that many channels are read, exactly as lcms2 does.
    #[test]
    fn screening_channel_cap() {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes()); // Flag
        b.extend_from_slice(&100u32.to_be_bytes()); // nChannels well over the cap
                                                    // Provide 15 channels' worth of data (the cap).
        for i in 0..15u32 {
            b.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            b.extend_from_slice(&0x0000_0000u32.to_be_bytes());
            b.extend_from_slice(&i.to_be_bytes());
        }
        let mut r = MemReader::new(&b);
        match read_screening(&mut r, b.len() as u32).unwrap() {
            Tag::Screening(s) => {
                assert_eq!(s.n_channels, 15, "count capped to MAXCHANNELS - 1");
                assert_eq!(s.channels.len(), 15);
                assert_eq!(s.channels[14].spot_shape, 14);
            }
            other => panic!("expected Screening, got {other:?}"),
        }
    }

    /// Synthetic CrdInfo: five counted ASCII strings. The C appends a NUL and
    /// MLUsetASCII trims at the first NUL; our value is the counted bytes.
    #[test]
    fn crd_info_synthetic() {
        fn counted(s: &str) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&(s.len() as u32).to_be_bytes());
            v.extend_from_slice(s.as_bytes());
            v
        }
        let mut b = Vec::new();
        b.extend_from_slice(&counted("Product"));
        b.extend_from_slice(&counted("CRD0"));
        b.extend_from_slice(&counted("CRD1"));
        b.extend_from_slice(&counted("CRD2"));
        b.extend_from_slice(&counted("CRD3"));
        let mut r = MemReader::new(&b);
        match read_crd_info(&mut r, b.len() as u32).unwrap() {
            Tag::CrdInfo(c) => {
                assert_eq!(c.product_name, b"Product");
                assert_eq!(c.crd_names[0], b"CRD0");
                assert_eq!(c.crd_names[1], b"CRD1");
                assert_eq!(c.crd_names[2], b"CRD2");
                assert_eq!(c.crd_names[3], b"CRD3");
            }
            other => panic!("expected CrdInfo, got {other:?}"),
        }
    }

    /// A CrdInfo string whose Count overruns the remaining size must be rejected,
    /// matching lcms2's `*SizeOfTag < Count + 4` guard.
    #[test]
    fn crd_info_overrun_rejected() {
        let mut b = Vec::new();
        b.extend_from_slice(&100u32.to_be_bytes()); // claims 100 bytes
        b.extend_from_slice(b"short"); // only 5 present
        let mut r = MemReader::new(&b);
        assert!(matches!(
            read_crd_info(&mut r, b.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }

    /// Synthetic cicp: exactly four bytes.
    #[test]
    fn cicp_synthetic() {
        let b = [9u8, 16, 0, 1];
        let mut r = MemReader::new(&b);
        match read_cicp(&mut r, 4).unwrap() {
            Tag::Cicp(c) => {
                assert_eq!(c.colour_primaries, 9);
                assert_eq!(c.transfer_characteristics, 16);
                assert_eq!(c.matrix_coefficients, 0);
                assert_eq!(c.video_full_range_flag, 1);
            }
            other => panic!("expected Cicp, got {other:?}"),
        }
    }

    /// cicp rejects any payload that is not exactly 4 bytes (lcms2 `SizeOfTag != 4`).
    #[test]
    fn cicp_wrong_size_rejected() {
        let b = [1u8, 2, 3, 4, 5];
        let mut r = MemReader::new(&b);
        assert!(matches!(read_cicp(&mut r, 5), Err(Error::Corrupt(_))));
    }

    /// Synthetic ColorantTable: count=2, each {32-byte name, 3×u16 PCS}.
    #[test]
    fn colorant_table_synthetic() {
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_be_bytes());
        let mut name0 = [0u8; 32];
        name0[..3].copy_from_slice(b"Red");
        b.extend_from_slice(&name0);
        for v in [0x1111u16, 0x2222, 0x3333] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        let mut name1 = [0u8; 32];
        name1[..4].copy_from_slice(b"Cyan");
        b.extend_from_slice(&name1);
        for v in [0xAAAAu16, 0xBBBB, 0xCCCC] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        let mut r = MemReader::new(&b);
        match read_colorant_table(&mut r, b.len() as u32).unwrap() {
            Tag::ColorantTable(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].name, "Red");
                assert_eq!(v[0].pcs, [0x1111, 0x2222, 0x3333]);
                assert_eq!(v[1].name, "Cyan");
                assert_eq!(v[1].pcs, [0xAAAA, 0xBBBB, 0xCCCC]);
            }
            other => panic!("expected ColorantTable, got {other:?}"),
        }
    }

    /// ColorantTable rejects a count above cmsMAXCHANNELS (lcms2 RANGE error).
    #[test]
    fn colorant_table_count_too_large() {
        let mut b = Vec::new();
        b.extend_from_slice(&17u32.to_be_bytes()); // > cmsMAXCHANNELS (16)
        let mut r = MemReader::new(&b);
        assert!(matches!(
            read_colorant_table(&mut r, b.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }
}
