//! The named-colour, profile-sequence, and dictionary tag-type readers,
//! transcribed from lcms2 `src/cmstypes.c`. These types all carry nested
//! structure (counted arrays, embedded MLUs, and positioned/offset tables), so
//! they live apart from the flat trivial/struct readers. Each reader takes the
//! positioned reader `r` (already past the 8-byte type base) and `size` =
//! `TagSize - 8` (the byte count lcms2's handler receives as `SizeOfTag`).
//!
//! - `Type_NamedColor_Read`            (`cmstypes.c:3369`) -> [`Tag::NamedColor2`]
//! - `Type_ProfileSequenceDesc_Read`   (`cmstypes.c:3541`) -> [`Tag::ProfileSequenceDesc`]
//! - `Type_ProfileSequenceId_Read`     (`cmstypes.c:3687`) -> [`Tag::ProfileSequenceId`]
//! - `Type_Dictionary_Read`            (`cmstypes.c:5436`) -> [`Tag::Dict`]

use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::profile::tag::{
    Dict, DictEntry, Mlu, NamedColor, NamedColorList, ProfileIdItem, ProfileSequenceItem, Tag,
};
use crate::profile::types::mlu::{read_mlu_value, read_text_description_value};
use crate::sig::Signature;

/// lcms2 `cmsMAXCHANNELS` (`include/lcms2.h`).
const MAX_CHANNELS: u32 = 16;
/// lcms2's `sizeof(_cmsTagBase)`: 4-byte type signature + 4 reserved bytes.
const TAG_BASE: u64 = 8;

// On-disk tag-TYPE signatures the embedded-text reader dispatches on (`ReadEmbeddedText`).
const T_TEXT: u32 = 0x7465_7874; // 'text'
const T_TEXT_DESCRIPTION: u32 = 0x6465_7363; // 'desc'
const T_MLU: u32 = 0x6D6C_7563; // 'mluc'

/// `Type_NamedColor_Read` (`cmstypes.c:3369`). Layout: `vendorFlag` (u32),
/// `count` (u32), `nDeviceCoords` (u32), `prefix` (32 ASCII), `suffix` (32
/// ASCII); then per colour a 32-byte `Root` name, a 3×u16 PCS, and
/// `nDeviceCoords` u16 device coordinates.
///
/// Bounds transcribed from the C:
/// - `cmsAllocNamedColorList` returns NULL (→ error) when `nDeviceCoords >
///   cmsMAXCHANNELS`; the reader also re-checks `nDeviceCoords > cmsMAXCHANNELS`.
///   Either way `nDeviceCoords > 16` is rejected.
/// - `prefix`/`suffix` are force-NUL-terminated at index 31, then `strncpy`'d
///   (so the stored value is the bytes up to the first NUL within the 32);
///   `read_ascii(32)` reproduces this (the forced NUL guarantees a stop ≤ 31).
/// - `Root` is force-terminated at index 32 (`Root[32]=0`), giving up to 32 name
///   bytes; `read_ascii(32)` matches (the on-disk field is 32 bytes).
///
/// lcms2 pre-allocates the whole list (`count` slots); a pathological `count`
/// fails that allocation. We instead push per colour, so a too-large `count`
/// surfaces as the natural truncation error on the first short read — same
/// accept/reject outcome without an attacker-controlled pre-allocation.
pub fn read_named_color2<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let vendor_flag = r.read_u32()?;
    let count = r.read_u32()?;
    let n_device_coords = r.read_u32()?;

    let prefix = r.read_ascii(32)?;
    let suffix = r.read_ascii(32)?;

    // cmsAllocNamedColorList rejects ColorantCount > cmsMAXCHANNELS (returns NULL).
    if n_device_coords > MAX_CHANNELS {
        return Err(Error::Corrupt("named color: nDeviceCoords > MAXCHANNELS"));
    }

    let mut colors: Vec<NamedColor> = Vec::new();
    for _ in 0..count {
        let name = r.read_ascii(32)?;
        let pcs = [r.read_u16()?, r.read_u16()?, r.read_u16()?];
        let mut device = Vec::with_capacity(n_device_coords as usize);
        for _ in 0..n_device_coords {
            device.push(r.read_u16()?);
        }
        colors.push(NamedColor { name, pcs, device });
    }

    Ok(Tag::NamedColor2(NamedColorList {
        vendor_flag,
        prefix,
        suffix,
        colors,
        colorant_count: n_device_coords as usize,
    }))
}

/// lcms2 `ReadEmbeddedText` (`cmstypes.c:3503`): read a nested type base, then
/// dispatch to the `text`/`desc`/`mluc` reader, producing a [`Mlu`]. Any other
/// base type is rejected (the C `default: return FALSE`).
///
/// `size` is the `SizeOfTag` passed through; only `Type_Text_Description_Read`
/// actually consults it (the MLU/Text readers track their own bounds). Note the
/// embedded type base advances the cursor by 8 bytes, exactly as the C
/// `_cmsReadTypeBase` does, so the inner reader sees its own payload start.
fn read_embedded_mlu<R: ProfileReader>(r: &mut R, size: u32) -> Result<Mlu> {
    let base = r.read_type_base()?;
    // lcms2's `ReadEmbeddedText` passes the SAME `SizeOfTag` it received straight
    // to the inner handler — it does NOT subtract the 8-byte base it just read via
    // `_cmsReadTypeBase`. So `Type_Text_Read` reads `SizeOfTag` ASCII bytes (which,
    // for an embedded `text`, deliberately spans the rest of the element), and the
    // MLU/desc readers see `SizeOfTag` as their bound. We pass `size` unchanged.
    match base.to_raw() {
        T_TEXT => {
            // Type_Text_Read decodes one ASCII entry (cmsNoLanguage/cmsNoCountry),
            // reading exactly `SizeOfTag` bytes (lcms2 cmstypes.c:925).
            read_text_as_mlu(r, size)
        }
        T_TEXT_DESCRIPTION => read_text_description_value(r, size),
        T_MLU => read_mlu_value(r, size),
        _ => Err(Error::Corrupt("embedded text: unsupported base type")),
    }
}

/// `Type_Text_Read` (`cmstypes.c:925`) as it is used embedded: the whole payload
/// is one NUL-terminated ASCII string, stored by lcms2 in a `cmsMLU` under the
/// `cmsNoLanguage`/`cmsNoCountry` codes (`"\0\0"`/`"\0\0"`). We materialise that
/// single-entry MLU so an embedded `text` block compares equal to an embedded
/// `mluc`/`desc` block carrying the same string.
fn read_text_as_mlu<R: ProfileReader>(r: &mut R, size: u32) -> Result<Mlu> {
    let text = r.read_ascii(size as usize)?;
    Ok(Mlu {
        entries: vec![crate::profile::tag::MluEntry {
            language: [0, 0],
            country: [0, 0],
            text,
        }],
    })
}

/// `Type_ProfileSequenceDesc_Read` (`cmstypes.c:3541`). Layout: `Count` (u32),
/// then per element `{deviceMfg: sig, deviceModel: sig, attributes: u64,
/// technology: sig}` followed by two embedded-text MLUs (Manufacturer, Model).
///
/// The C decrements a running `SizeOfTag` after each fixed field and bails if it
/// would go negative; we mirror those guards exactly (the `size` we receive is the
/// handler's `SizeOfTag`, i.e. `TagSize - 8`).
pub fn read_profile_sequence_desc<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let count = r.read_u32()?;

    // if (SizeOfTag < sizeof(cmsUInt32Number)) return NULL; SizeOfTag -= 4;
    let mut remaining = size
        .checked_sub(4)
        .ok_or(Error::Corrupt("pseq: size < 4 for count"))?;

    let mut items = Vec::with_capacity(count.min(0x1_0000) as usize);
    for _ in 0..count {
        let device_mfg = Signature::from_raw(r.read_u32()?);
        remaining = remaining
            .checked_sub(4)
            .ok_or(Error::Corrupt("pseq: size underflow at deviceMfg"))?;

        let device_model = Signature::from_raw(r.read_u32()?);
        remaining = remaining
            .checked_sub(4)
            .ok_or(Error::Corrupt("pseq: size underflow at deviceModel"))?;

        let attributes = r.read_u64()?;
        remaining = remaining
            .checked_sub(8)
            .ok_or(Error::Corrupt("pseq: size underflow at attributes"))?;

        let technology = Signature::from_raw(r.read_u32()?);
        remaining = remaining
            .checked_sub(4)
            .ok_or(Error::Corrupt("pseq: size underflow at technology"))?;

        // Both embedded texts receive the current remaining SizeOfTag (the C passes
        // the same SizeOfTag to both ReadEmbeddedText calls without decrementing it
        // between them — only the fixed fields adjust the running size).
        let manufacturer = read_embedded_mlu(r, remaining)?;
        let model = read_embedded_mlu(r, remaining)?;

        items.push(ProfileSequenceItem {
            device_mfg,
            device_model,
            attributes,
            technology,
            manufacturer,
            model,
        });
    }

    Ok(Tag::ProfileSequenceDesc(items))
}

/// `Type_ProfileSequenceId_Read` (`cmstypes.c:3687`) + its `ReadPositionTable`
/// (`cmstypes.c:219`) / `ReadSeqID` callback. Layout: `Count` (u32), then a
/// position table of `Count` `{offset: u32, size: u32}` records (offsets relative
/// to `BaseOffset`), then per element a 16-byte profile ID and an embedded-MLU
/// description read at the element's positioned offset.
///
/// `BaseOffset = io->Tell - sizeof(_cmsTagBase)`: lcms2 takes the absolute tag
/// position (which already includes the 8-byte type base) and subtracts the base,
/// so element offsets are measured from the tag start. Our `r` enters this reader
/// already 8 bytes past the tag start (the base was consumed by `read_tag`), so
/// `BaseOffset = r.tell() - 8`.
pub fn read_profile_sequence_id<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    // BaseOffset = io->Tell(io) - sizeof(_cmsTagBase).
    let base_offset = r
        .tell()
        .checked_sub(TAG_BASE)
        .ok_or(Error::Corrupt("psid: cursor before tag base"))?;

    let count = r.read_u32()?;

    let table = read_position_table(r, count, base_offset, size)?;

    let mut items = Vec::with_capacity(count.min(0x1_0000) as usize);
    for &(off, sz) in &table {
        r.seek(off)?;
        let mut profile_id = [0u8; 16];
        r.read_exact(&mut profile_id)?;
        let description = read_embedded_mlu(r, sz)?;
        items.push(ProfileIdItem {
            profile_id,
            description,
        });
    }

    Ok(Tag::ProfileSequenceId(items))
}

/// lcms2 `ReadPositionTable` (`cmstypes.c:219`): read `count` `{offset, size}`
/// records, add `base_offset` to each offset, and return the (absolute offset,
/// size) pairs. The caller then seeks to each and invokes its element reader.
///
/// The C guards `((ReportedSize - currentPosition) / 8) < Count` up front
/// (enough room for the `count` 8-byte records). `ReportedSize` is the IOHANDLER's
/// reported stream length; for a tag read that is the whole-profile byte length.
/// We approximate the same bound using the position table's own extent: there must
/// be room for `count` 8-byte records after the current cursor within the tag's
/// `size` window. This rejects the same overflowing tables the C rejects without
/// needing the global stream length.
fn read_position_table<R: ProfileReader>(
    r: &mut R,
    count: u32,
    base_offset: u64,
    size: u32,
) -> Result<Vec<(u64, u32)>> {
    // Room check: the directory of `count` 8-byte records must fit. The directory
    // begins right after the 4-byte Count we just read (which sits at tag offset
    // 8); `size` is TagSize - 8, so the bytes available for the directory are
    // `size - 4`. `count * 8` must not exceed that.
    let dir_avail = (size as u64).saturating_sub(4);
    if (count as u64).saturating_mul(8) > dir_avail {
        return Err(Error::Corrupt(
            "position table: not enough room for offsets",
        ));
    }

    let mut table = Vec::with_capacity(count.min(0x1_0000) as usize);
    for _ in 0..count {
        let off = r.read_u32()?;
        let sz = r.read_u32()?;
        // ElementOffsets[i] += BaseOffset.
        let abs = (off as u64)
            .checked_add(base_offset)
            .ok_or(Error::Corrupt("position table: offset overflow"))?;
        table.push((abs, sz));
    }
    Ok(table)
}

/// `Type_Dictionary_Read` (`cmstypes.c:5436`). Layout: `Count` (u32), `Length`
/// (u32, ∈ {16, 24, 32}); then a column/offset directory of `Count` rows, each
/// row holding `{offset, size}` pairs for Name, Value, and — when `Length` > 16 —
/// DisplayName, and — when `Length` > 24 — DisplayValue; then the positioned
/// string/MLU data. Offsets are relative to `BaseOffset` (an offset of 0 means
/// "absent", per the ICC dictionary spec).
///
/// `BaseOffset = io->Tell - sizeof(_cmsTagBase)`; as in `psid`, our `r` is 8 bytes
/// into the tag, so `BaseOffset = r.tell() - 8`.
///
/// The C runs a signed `SignedSizeOfTag` and rejects underflow at each stage:
/// 4 bytes for Count, 4 for Length, then per row `4 * 4` bytes (Name+Value, two
/// `{offset,size}` pairs) and, conditionally, `2 * 4` for each of DisplayName and
/// DisplayValue. We mirror that signed accounting exactly with `i64`.
///
/// Name/Value are required: a row whose Name or Value offset is 0 (NULL wide
/// string) is `cmsERROR_CORRUPTION_DETECTED` in the C (`rc = FALSE` → `goto
/// Error`). DisplayName/DisplayValue are optional MLUs (absent when their offset
/// or size is 0).
pub fn read_dictionary<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    let base_offset = r
        .tell()
        .checked_sub(TAG_BASE)
        .ok_or(Error::Corrupt("dict: cursor before tag base"))?;

    let mut signed_size = size as i64;

    // SignedSizeOfTag -= 4; if < 0 Error; read Count.
    signed_size -= 4;
    if signed_size < 0 {
        return Err(Error::Corrupt("dict: size < 4 for count"));
    }
    let count = r.read_u32()?;

    // SignedSizeOfTag -= 4; if < 0 Error; read Length.
    signed_size -= 4;
    if signed_size < 0 {
        return Err(Error::Corrupt("dict: size < 8 for length"));
    }
    let length = r.read_u32()?;

    if length != 16 && length != 24 && length != 32 {
        return Err(Error::Corrupt("dict: unknown record length"));
    }

    // ---- ReadOffsetArray (cmstypes.c:5249): the column/offset directory. ----
    // Each cell: (offset, size). offset 0 stays 0 (special); otherwise += BaseOffset.
    let read_cell = |r: &mut R| -> Result<DictCell> {
        let off = r.read_u32()?;
        let sz = r.read_u32()?;
        let offset = if off > 0 {
            (off as u64)
                .checked_add(base_offset)
                .ok_or(Error::Corrupt("dict: offset overflow"))?
        } else {
            0
        };
        Ok(DictCell { offset, size: sz })
    };

    let mut names = Vec::with_capacity(count.min(0x1_0000) as usize);
    let mut values = Vec::with_capacity(count.min(0x1_0000) as usize);
    let mut display_names = Vec::with_capacity(count.min(0x1_0000) as usize);
    let mut display_values = Vec::with_capacity(count.min(0x1_0000) as usize);

    for _ in 0..count {
        // if (SignedSizeOfTag < 4*4) return FALSE; SignedSizeOfTag -= 16;
        signed_size -= 16;
        if signed_size < 0 {
            return Err(Error::Corrupt("dict: size underflow at name/value offsets"));
        }
        names.push(read_cell(r)?);
        values.push(read_cell(r)?);

        if length > 16 {
            signed_size -= 8;
            if signed_size < 0 {
                return Err(Error::Corrupt(
                    "dict: size underflow at display-name offset",
                ));
            }
            display_names.push(Some(read_cell(r)?));
        } else {
            display_names.push(None);
        }

        if length > 24 {
            signed_size -= 8;
            if signed_size < 0 {
                return Err(Error::Corrupt(
                    "dict: size underflow at display-value offset",
                ));
            }
            display_values.push(Some(read_cell(r)?));
        } else {
            display_values.push(None);
        }
    }

    // ---- Seek to each element and read it (cmstypes.c:5481). ----
    let mut entries = Vec::with_capacity(count.min(0x1_0000) as usize);
    for i in 0..count as usize {
        let name = read_one_wchar(r, &names[i])?;
        let value = read_one_wchar(r, &values[i])?;

        let display_name = match &display_names[i] {
            Some(c) => read_one_mluc(r, c)?,
            None => None,
        };
        let display_value = match &display_values[i] {
            Some(c) => read_one_mluc(r, c)?,
            None => None,
        };

        // Name == NULL || Value == NULL -> cmsERROR_CORRUPTION_DETECTED.
        match (name, value) {
            (Some(name), Some(value)) => entries.push(DictEntry {
                name,
                value,
                display_name,
                display_value,
            }),
            _ => return Err(Error::Corrupt("dict: bad (null) name/value")),
        }
    }

    Ok(Tag::Dict(Dict { entries }))
}

/// lcms2 `ReadOneWChar` (`cmstypes.c:5323`): read a positioned UTF-16 string into
/// a wide string. Returns `None` for an offset-0 cell (the ICC "undefined string"
/// sentinel). `nChars = size / 2`; `nChars > 0x7ffff` is rejected. The decode
/// mirrors `_cmsReadWCharArray` on a 64-bit `wchar_t` platform, i.e.
/// `convert_utf16_to_utf32` (`cmstypes.c:147`): UTF-16 surrogate pairs combine to
/// a single scalar; a lone/mismatched surrogate is a hard error ("Corrupted
/// string"). This is STRICTER than the MLU pool decode (which keeps raw units),
/// because the dictionary Name/Value path goes through `_cmsReadWCharArray`.
struct DictCell {
    offset: u64,
    size: u32,
}
fn read_one_wchar<R: ProfileReader>(r: &mut R, cell: &DictCell) -> Result<Option<String>> {
    if cell.offset == 0 {
        return Ok(None);
    }
    r.seek(cell.offset)?;
    let n_chars = cell.size / 2;
    if n_chars > 0x7ffff {
        return Err(Error::Corrupt("dict: wide string too long"));
    }
    let mut buf = vec![0u8; (n_chars as usize) * 2];
    r.read_exact(&mut buf)?;
    Ok(Some(decode_wchar_array(&buf)?))
}

/// lcms2 `ReadOneMLUC` (`cmstypes.c:5388`): a positioned embedded MLU. Returns
/// `None` when offset == 0 OR size == 0 (the C's "null MLUC" guard); otherwise
/// seeks to the offset and runs `Type_MLU_Read` with the cell size as `SizeOfTag`.
/// Note: unlike `ReadEmbeddedText`, this does NOT read a type base — the cell
/// points straight at the MLU payload (Count/RecLen/...).
fn read_one_mluc<R: ProfileReader>(r: &mut R, cell: &DictCell) -> Result<Option<Mlu>> {
    if cell.offset == 0 || cell.size == 0 {
        return Ok(None);
    }
    r.seek(cell.offset)?;
    Ok(Some(read_mlu_value(r, cell.size)?))
}

/// Decode a UTF-16BE byte slice the way `_cmsReadWCharArray` →
/// `convert_utf16_to_utf32` does on a 64-bit-`wchar_t` platform: combine
/// surrogate pairs, and treat a lone/mismatched surrogate as corruption.
fn decode_wchar_array(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    let mut it = bytes.chunks_exact(2);
    while let Some(c) = it.next() {
        let uc = u16::from_be_bytes([c[0], c[1]]);
        // is_surrogate(uc): (uc - 0xd800) < 2048.
        if (uc.wrapping_sub(0xd800)) < 2048 {
            // High surrogate must pair with an immediately-following low surrogate.
            let low = it
                .next()
                .ok_or(Error::Corrupt("dict: truncated surrogate pair"))?;
            let low = u16::from_be_bytes([low[0], low[1]]);
            let is_high = (uc & 0xfc00) == 0xd800;
            let is_low = (low & 0xfc00) == 0xdc00;
            if is_high && is_low {
                let scalar = ((uc as u32) << 10)
                    .wrapping_add(low as u32)
                    .wrapping_sub(0x035f_dc00);
                match char::from_u32(scalar) {
                    Some(ch) => out.push(ch),
                    None => return Err(Error::Corrupt("dict: invalid surrogate scalar")),
                }
            } else {
                return Err(Error::Corrupt("dict: mismatched surrogate"));
            }
        } else {
            // BMP scalar: a non-surrogate u16 is always a valid char.
            out.push(char::from_u32(uc as u32).expect("non-surrogate u16 is a valid scalar"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::cursor::MemReader;
    use crate::profile::tag::MluEntry;

    // ---- shared byte builders ------------------------------------------------

    /// 8-byte type base (`sig` + reserved zeros).
    fn type_base(sig: &[u8; 4]) -> Vec<u8> {
        let mut b = sig.to_vec();
        b.extend_from_slice(&[0, 0, 0, 0]);
        b
    }

    /// A 32-byte fixed name field (NUL-padded).
    fn name32(s: &str) -> Vec<u8> {
        let mut b = [0u8; 32];
        let bytes = s.as_bytes();
        b[..bytes.len()].copy_from_slice(bytes);
        b.to_vec()
    }

    /// A standalone `mluc` payload (NO type base) holding one BMP-string
    /// translation. `Offset` is measured from the tag start and bakes in the
    /// +8 the writer always adds (see `Type_MLU_Write`): SizeOfHeader = 12*1 + 8 =
    /// 20, pool begins at tag-offset 20 + 8 = 28, so the single record's Offset is
    /// 28. Returns the payload bytes (Count, RecLen, one record, pool).
    fn mluc_payload(lang: &[u8; 2], country: &[u8; 2], text: &str) -> Vec<u8> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let len_bytes = (units.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes()); // Count = 1
        b.extend_from_slice(&12u32.to_be_bytes()); // RecLen = 12
        b.extend_from_slice(lang);
        b.extend_from_slice(country);
        b.extend_from_slice(&len_bytes.to_be_bytes());
        b.extend_from_slice(&28u32.to_be_bytes()); // Offset = SizeOfHeader(20) + 8
        for u in units {
            b.extend_from_slice(&u.to_be_bytes());
        }
        b
    }

    // ---- NamedColor2 ---------------------------------------------------------

    /// Synthetic NamedColor2: vendorFlag, count=2, nDeviceCoords=3, prefix/suffix,
    /// then two colours. Hand-computed expected values.
    #[test]
    fn named_color2_synthetic() {
        let mut b = Vec::new();
        b.extend_from_slice(&0xABCD_1234u32.to_be_bytes()); // vendorFlag
        b.extend_from_slice(&2u32.to_be_bytes()); // count
        b.extend_from_slice(&3u32.to_be_bytes()); // nDeviceCoords
        b.extend_from_slice(&name32("pre")); // prefix
        b.extend_from_slice(&name32("suf")); // suffix
                                             // colour 0
        b.extend_from_slice(&name32("Red"));
        for v in [0x1111u16, 0x2222, 0x3333] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        for v in [0x0001u16, 0x0002, 0x0003] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        // colour 1
        b.extend_from_slice(&name32("Green"));
        for v in [0xAAAAu16, 0xBBBB, 0xCCCC] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        for v in [0x0010u16, 0x0020, 0x0030] {
            b.extend_from_slice(&v.to_be_bytes());
        }

        let mut r = MemReader::new(&b);
        match read_named_color2(&mut r, b.len() as u32).unwrap() {
            Tag::NamedColor2(list) => {
                assert_eq!(list.vendor_flag, 0xABCD_1234);
                assert_eq!(list.prefix, "pre");
                assert_eq!(list.suffix, "suf");
                assert_eq!(list.colors.len(), 2);
                assert_eq!(list.colors[0].name, "Red");
                assert_eq!(list.colors[0].pcs, [0x1111, 0x2222, 0x3333]);
                assert_eq!(list.colors[0].device, vec![1, 2, 3]);
                assert_eq!(list.colors[1].name, "Green");
                assert_eq!(list.colors[1].pcs, [0xAAAA, 0xBBBB, 0xCCCC]);
                assert_eq!(list.colors[1].device, vec![0x10, 0x20, 0x30]);
            }
            other => panic!("expected NamedColor2, got {other:?}"),
        }
    }

    /// NamedColor2 with nDeviceCoords > cmsMAXCHANNELS is rejected (lcms2's
    /// cmsAllocNamedColorList returns NULL).
    #[test]
    fn named_color2_too_many_coords_rejected() {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes()); // vendorFlag
        b.extend_from_slice(&0u32.to_be_bytes()); // count
        b.extend_from_slice(&17u32.to_be_bytes()); // nDeviceCoords > 16
        b.extend_from_slice(&name32("")); // prefix
        b.extend_from_slice(&name32("")); // suffix
        let mut r = MemReader::new(&b);
        assert!(matches!(
            read_named_color2(&mut r, b.len() as u32),
            Err(Error::Corrupt(_))
        ));
    }

    // ---- ProfileSequenceDesc -------------------------------------------------

    /// Synthetic ProfileSequenceDesc: Count=1, one element with two embedded MLUs.
    /// Embedded text blocks each carry their own 8-byte type base (`mluc`).
    #[test]
    fn profile_sequence_desc_synthetic() {
        let manu = mluc_payload(b"en", b"US", "Acme");
        let model = mluc_payload(b"de", b"DE", "Model5");

        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // Count = 1
        body.extend_from_slice(&0x6D6E_6672u32.to_be_bytes()); // deviceMfg 'mnfr'
        body.extend_from_slice(&0x6D6F_646Cu32.to_be_bytes()); // deviceModel 'modl'
        body.extend_from_slice(&0x0011_2233_4455_6677u64.to_be_bytes()); // attributes
        body.extend_from_slice(&0x7463_686Eu32.to_be_bytes()); // technology 'tchn'
        body.extend_from_slice(&type_base(b"mluc"));
        body.extend_from_slice(&manu);
        body.extend_from_slice(&type_base(b"mluc"));
        body.extend_from_slice(&model);

        let mut r = MemReader::new(&body);
        match read_profile_sequence_desc(&mut r, body.len() as u32).unwrap() {
            Tag::ProfileSequenceDesc(items) => {
                assert_eq!(items.len(), 1);
                let it = &items[0];
                assert_eq!(it.device_mfg.to_raw(), 0x6D6E_6672);
                assert_eq!(it.device_model.to_raw(), 0x6D6F_646C);
                assert_eq!(it.attributes, 0x0011_2233_4455_6677);
                assert_eq!(it.technology.to_raw(), 0x7463_686E);
                assert_eq!(
                    it.manufacturer.entries,
                    vec![MluEntry {
                        language: *b"en",
                        country: *b"US",
                        text: "Acme".into(),
                    }]
                );
                assert_eq!(
                    it.model.entries,
                    vec![MluEntry {
                        language: *b"de",
                        country: *b"DE",
                        text: "Model5".into(),
                    }]
                );
            }
            other => panic!("expected ProfileSequenceDesc, got {other:?}"),
        }
    }

    /// The embedded-text helper dispatches on the nested type base. For a `text`
    /// base it produces one cmsNoLanguage/cmsNoCountry MLU entry, reading exactly
    /// `SizeOfTag` ASCII bytes (lcms2 `Type_Text_Read`) and truncating the string
    /// at the first NUL (matching `cmsMLUsetASCII`). `SizeOfTag` here is the size
    /// of just this `text` block's content (the realistic LAST-block case, where
    /// the remaining tag size equals the block content).
    #[test]
    fn embedded_text_base_decodes_to_single_entry() {
        let mut block = type_base(b"text");
        block.extend_from_slice(b"Hello\0");
        // SizeOfTag passed to ReadEmbeddedText is the content size (after the base
        // the helper itself consumes): 6 bytes ("Hello\0").
        let content_size = 6u32;
        let mut r = MemReader::new(&block);
        let mlu = read_embedded_mlu(&mut r, content_size).unwrap();
        assert_eq!(mlu.entries.len(), 1);
        assert_eq!(mlu.entries[0].language, [0, 0]);
        assert_eq!(mlu.entries[0].country, [0, 0]);
        assert_eq!(mlu.entries[0].text, "Hello");
    }

    /// The embedded-text helper rejects an unknown nested type base (lcms2
    /// `ReadEmbeddedText` `default: return FALSE`).
    #[test]
    fn embedded_text_unknown_base_rejected() {
        let block = type_base(b"junk");
        let mut r = MemReader::new(&block);
        assert!(matches!(
            read_embedded_mlu(&mut r, 0),
            Err(Error::Corrupt(_))
        ));
    }

    // ---- ProfileSequenceId (positioned) -------------------------------------

    /// Synthetic ProfileSequenceId driven through the real type-base path so the
    /// `BaseOffset = Tell - 8` positioning is exercised end-to-end. Count=2; each
    /// element = 16-byte ID + embedded `mluc`. Offsets are relative to the tag
    /// start (offset 0 = the type base).
    #[test]
    fn profile_sequence_id_synthetic() {
        // Element payloads (16-byte ID + type base + mluc).
        let id0 = [0x11u8; 16];
        let id1 = [0x22u8; 16];
        let mlu0 = mluc_payload(b"en", b"US", "First");
        let mlu1 = mluc_payload(b"fr", b"FR", "Second");

        let mut elem0 = id0.to_vec();
        elem0.extend_from_slice(&type_base(b"mluc"));
        elem0.extend_from_slice(&mlu0);
        let mut elem1 = id1.to_vec();
        elem1.extend_from_slice(&type_base(b"mluc"));
        elem1.extend_from_slice(&mlu1);

        // Layout: [8-byte base][Count u32][2 × {offset u32, size u32}][elem0][elem1].
        // Directory begins at tag-offset 12; each record is 8 bytes → elem0 starts
        // at tag-offset 12 + 16 = 28; elem1 at 28 + elem0.len().
        let off0 = 28u32;
        let off1 = off0 + elem0.len() as u32;

        let mut full = type_base(b"psid");
        let body_start = full.len(); // 8
        full.extend_from_slice(&2u32.to_be_bytes()); // Count
        full.extend_from_slice(&off0.to_be_bytes());
        full.extend_from_slice(&(elem0.len() as u32).to_be_bytes());
        full.extend_from_slice(&off1.to_be_bytes());
        full.extend_from_slice(&(elem1.len() as u32).to_be_bytes());
        full.extend_from_slice(&elem0);
        full.extend_from_slice(&elem1);
        let size = (full.len() - body_start) as u32; // SizeOfTag = TagSize - 8

        let mut r = MemReader::new(&full);
        let _ = r.read_type_base().unwrap(); // cursor -> 8, BaseOffset -> 0
        match read_profile_sequence_id(&mut r, size).unwrap() {
            Tag::ProfileSequenceId(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].profile_id, id0);
                assert_eq!(items[0].description.entries[0].text, "First");
                assert_eq!(items[0].description.entries[0].language, *b"en");
                assert_eq!(items[1].profile_id, id1);
                assert_eq!(items[1].description.entries[0].text, "Second");
                assert_eq!(items[1].description.entries[0].country, *b"FR");
            }
            other => panic!("expected ProfileSequenceId, got {other:?}"),
        }
    }

    // ---- Dictionary (positioned) --------------------------------------------

    /// UTF-16BE bytes for a string (no NUL).
    fn utf16be(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    /// Build a `dict` tag (with type base) of record length 16 (Name/Value only).
    /// Two entries. Offsets are tag-relative (offset 0 = the type base start).
    #[test]
    fn dictionary_reclen16_synthetic() {
        // Strings.
        let n0 = utf16be("k1");
        let v0 = utf16be("val1");
        let n1 = utf16be("key2");
        let v1 = utf16be("v2");

        // Header: [base 8][Count u32][Length u32]. Directory: Count rows of
        // 2 × {offset,size} = 16 bytes each. Data follows.
        let header_len = 8 + 4 + 4; // 16
        let dir_len = 2 * 16; // two rows × 16 bytes
        let data_start = (header_len + dir_len) as u32; // tag-relative

        let o_n0 = data_start;
        let o_v0 = o_n0 + n0.len() as u32;
        let o_n1 = o_v0 + v0.len() as u32;
        let o_v1 = o_n1 + n1.len() as u32;

        let mut full = type_base(b"dict");
        full.extend_from_slice(&2u32.to_be_bytes()); // Count
        full.extend_from_slice(&16u32.to_be_bytes()); // Length
                                                      // row 0
        full.extend_from_slice(&o_n0.to_be_bytes());
        full.extend_from_slice(&(n0.len() as u32).to_be_bytes());
        full.extend_from_slice(&o_v0.to_be_bytes());
        full.extend_from_slice(&(v0.len() as u32).to_be_bytes());
        // row 1
        full.extend_from_slice(&o_n1.to_be_bytes());
        full.extend_from_slice(&(n1.len() as u32).to_be_bytes());
        full.extend_from_slice(&o_v1.to_be_bytes());
        full.extend_from_slice(&(v1.len() as u32).to_be_bytes());
        // data
        full.extend_from_slice(&n0);
        full.extend_from_slice(&v0);
        full.extend_from_slice(&n1);
        full.extend_from_slice(&v1);

        let size = (full.len() - 8) as u32;
        let mut r = MemReader::new(&full);
        let _ = r.read_type_base().unwrap();
        match read_dictionary(&mut r, size).unwrap() {
            Tag::Dict(d) => {
                assert_eq!(d.entries.len(), 2);
                assert_eq!(d.entries[0].name, "k1");
                assert_eq!(d.entries[0].value, "val1");
                assert!(d.entries[0].display_name.is_none());
                assert!(d.entries[0].display_value.is_none());
                assert_eq!(d.entries[1].name, "key2");
                assert_eq!(d.entries[1].value, "v2");
            }
            other => panic!("expected Dict, got {other:?}"),
        }
    }

    /// Record length 32 (Name, Value, DisplayName, DisplayValue). One entry whose
    /// DisplayName MLU is present and DisplayValue is absent (offset 0).
    #[test]
    fn dictionary_reclen32_with_display_mlu() {
        let name = utf16be("color");
        let value = utf16be("red");
        // DisplayName MLU payload (standalone, no type base).
        let disp = mluc_payload(b"en", b"US", "Red");

        // Header [base 8][Count 4][Length 4] = 16. Directory: 1 row × 4 cells ×
        // 8 bytes = 32. Data starts at tag-offset 48.
        let data_start = (16 + 32) as u32;
        let o_name = data_start;
        let o_value = o_name + name.len() as u32;
        let o_disp = o_value + value.len() as u32;

        let mut full = type_base(b"dict");
        full.extend_from_slice(&1u32.to_be_bytes()); // Count
        full.extend_from_slice(&32u32.to_be_bytes()); // Length
                                                      // row 0: Name, Value, DisplayName, DisplayValue cells
        full.extend_from_slice(&o_name.to_be_bytes());
        full.extend_from_slice(&(name.len() as u32).to_be_bytes());
        full.extend_from_slice(&o_value.to_be_bytes());
        full.extend_from_slice(&(value.len() as u32).to_be_bytes());
        full.extend_from_slice(&o_disp.to_be_bytes());
        full.extend_from_slice(&(disp.len() as u32).to_be_bytes());
        full.extend_from_slice(&0u32.to_be_bytes()); // DisplayValue offset = 0 (absent)
        full.extend_from_slice(&0u32.to_be_bytes()); // DisplayValue size = 0
                                                     // data
        full.extend_from_slice(&name);
        full.extend_from_slice(&value);
        full.extend_from_slice(&disp);

        let size = (full.len() - 8) as u32;
        let mut r = MemReader::new(&full);
        let _ = r.read_type_base().unwrap();
        match read_dictionary(&mut r, size).unwrap() {
            Tag::Dict(d) => {
                assert_eq!(d.entries.len(), 1);
                assert_eq!(d.entries[0].name, "color");
                assert_eq!(d.entries[0].value, "red");
                let dn = d.entries[0].display_name.as_ref().expect("display name");
                assert_eq!(
                    dn.entries,
                    vec![MluEntry {
                        language: *b"en",
                        country: *b"US",
                        text: "Red".into(),
                    }]
                );
                assert!(d.entries[0].display_value.is_none());
            }
            other => panic!("expected Dict, got {other:?}"),
        }
    }

    /// A row whose Name offset is 0 (NULL wide string) is rejected exactly as the
    /// C does (`cmsERROR_CORRUPTION_DETECTED`).
    #[test]
    fn dictionary_null_name_rejected() {
        let value = utf16be("v");
        let data_start = (16 + 16) as u32; // header + one 16-byte row
        let o_value = data_start;

        let mut full = type_base(b"dict");
        full.extend_from_slice(&1u32.to_be_bytes()); // Count
        full.extend_from_slice(&16u32.to_be_bytes()); // Length
        full.extend_from_slice(&0u32.to_be_bytes()); // Name offset = 0 (NULL)
        full.extend_from_slice(&0u32.to_be_bytes()); // Name size = 0
        full.extend_from_slice(&o_value.to_be_bytes());
        full.extend_from_slice(&(value.len() as u32).to_be_bytes());
        full.extend_from_slice(&value);

        let size = (full.len() - 8) as u32;
        let mut r = MemReader::new(&full);
        let _ = r.read_type_base().unwrap();
        assert!(matches!(
            read_dictionary(&mut r, size),
            Err(Error::Corrupt(_))
        ));
    }

    /// An unknown record length (e.g. 20) is rejected (lcms2
    /// `cmsERROR_UNKNOWN_EXTENSION`).
    #[test]
    fn dictionary_bad_reclen_rejected() {
        let mut full = type_base(b"dict");
        full.extend_from_slice(&0u32.to_be_bytes()); // Count
        full.extend_from_slice(&20u32.to_be_bytes()); // Length (invalid)
        let size = (full.len() - 8) as u32;
        let mut r = MemReader::new(&full);
        let _ = r.read_type_base().unwrap();
        assert!(matches!(
            read_dictionary(&mut r, size),
            Err(Error::Corrupt(_))
        ));
    }

    /// The dict wide-string decoder pairs surrogates (astral plane) exactly like
    /// lcms2's `convert_utf16_to_utf32`, and rejects a lone surrogate.
    #[test]
    fn dict_wchar_surrogate_pairing() {
        // U+1F600 GRINNING FACE = surrogate pair D83D DE00.
        let bytes = [0xD8, 0x3D, 0xDE, 0x00];
        assert_eq!(decode_wchar_array(&bytes).unwrap(), "\u{1F600}");
        // Lone high surrogate is corruption.
        let lone = [0xD8, 0x3D, 0x00, 0x41];
        assert!(matches!(decode_wchar_array(&lone), Err(Error::Corrupt(_))));
    }
}
