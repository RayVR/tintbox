//! MLU (`cmsSigMultiLocalizedUnicodeType`, `'mluc'`) and TextDescription
//! (`cmsSigTextDescriptionType`, `'desc'`) tag-type readers, transcribed from
//! lcms2 `src/cmstypes.c`. Both decode to a [`Mlu`] in lcms2, so they share the
//! Rust value type. Every reader takes the positioned reader `r` (already past
//! the 8-byte type base) and `size` = `TagSize - 8` (the byte count lcms2's
//! handler receives as `SizeOfTag`).

// Untrusted-input parser: forbid the constructs that panic on malformed bytes
// (a panic here is a DoS). Arithmetic that mirrors lcms2's C wrapping uses
// `wrapping_*` explicitly.
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::profile::tag::{Mlu, MluEntry, Tag};

/// lcms2's `sizeof(_cmsTagBase)`: the 4-byte type signature plus 4 reserved
/// bytes that precede every tag's payload on disk.
const TAG_BASE: u32 = 8;

/// Decode a slice of UTF-16BE bytes the way lcms2 keeps it in a `cmsMLU` wide
/// pool. lcms2's `_cmsReadWCharArray` reads each big-endian u16 straight into a
/// `wchar_t` with NO surrogate pairing; `cmsMLUgetWide` then hands those raw
/// units back. We decode the identical unit sequence with [`char::decode_utf16`]
/// (lone surrogates → U+FFFD) so the comparison is over the same code units.
///
/// `byte_len` is the entry's `Len` in BYTES; the unit count is `byte_len / 2`
/// (lcms2: `Entries[i].Len` is `Len` bytes, and `cmsMLUgetASCII`/`getWide`
/// iterate `Len / sizeof(wchar_t)` units — a trailing odd byte is dropped).
fn decode_utf16be(bytes: &[u8]) -> String {
    // `chunks_exact(2)` always yields 2-byte slices, so the slice pattern is
    // exhaustive in practice; the `_` arm is unreachable (panic-free decode).
    let units = bytes.chunks_exact(2).map(|c| match c {
        [hi, lo] => u16::from_be_bytes([*hi, *lo]),
        _ => 0,
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// `Type_MLU_Read` (`cmstypes.c:1677`). Layout: `Count` (u32, number of records)
/// and `RecLen` (u32, must be 12), then `Count` directory records of
/// `{Language: u16, Country: u16, Len: u32, Offset: u32}`, then the UTF-16BE
/// string pool. `Offset`/`Len` are byte counts; `Offset` is measured from the
/// TAG start (i.e. it includes the 8-byte type base and the directory).
///
/// Faithful transcription of the C bounds checks:
/// ```c
/// SizeOfHeader = 12 * Count + sizeof(_cmsTagBase);   // 12*Count + 8
/// if (Offset & 1) goto Error;                          // must be even
/// if (Offset < (SizeOfHeader + 8)) goto Error;
/// if (((Offset + Len) < Len) || ((Offset + Len) > SizeOfTag + 8)) goto Error;
/// BeginOfThisString = Offset - SizeOfHeader - 8;       // relative to pool start
/// ```
///
/// `BeginOfThisString` is relative to the end of the directory; since `r` sits
/// exactly there once the directory is read, the absolute string position is
/// `pool_start + BeginOfThisString`. lcms2 reads the whole pool up to the
/// largest `BeginOfThisString + Len` into one wide block and slices per entry;
/// we read each entry's slice directly — identical bytes either way.
pub fn read_mlu<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    Ok(Tag::Mlu(read_mlu_value(r, size)?))
}

/// `Type_MLU_Read` returning the bare [`Mlu`] (not wrapped in [`Tag`]). Used both
/// by [`read_mlu`] and by the embedded-MLU readers (profile-sequence, dictionary)
/// that need the `cmsMLU*` directly, mirroring lcms2's `Type_MLU_Read` cast.
pub fn read_mlu_value<R: ProfileReader>(r: &mut R, size: u32) -> Result<Mlu> {
    let count = r.read_u32()?;
    let rec_len = r.read_u32()?;

    // multiLocalizedUnicodeType of len != 12 is not supported (cmsERROR_UNKNOWN_EXTENSION).
    if rec_len != 12 {
        return Err(Error::Corrupt("MLU record length != 12"));
    }

    // SizeOfHeader = 12 * Count + sizeof(_cmsTagBase). The C uses cmsUInt32Number
    // arithmetic; mirror the wrapping so overflow paths agree.
    let size_of_header = 12u32.wrapping_mul(count).wrapping_add(TAG_BASE);

    // Directory sits immediately after Count+RecLen; the pool follows the
    // directory, which is where the cursor lands after the loop.
    // Cap the capacity hint: `count` is attacker-controlled; an unbounded hint
    // OOM-aborts on wasm (32-bit linear memory). The loop is bounded by `count`
    // reads that fail on truncation, so a small hint is safe. (Matches the cap
    // used by the directory / pseq / psid / dict readers.)
    let mut dir = Vec::with_capacity((count as usize).min(0x1_0000));
    for _ in 0..count {
        let language = r.read_u16()?.to_be_bytes();
        let country = r.read_u16()?.to_be_bytes();
        let len = r.read_u32()?;
        let offset = r.read_u32()?;

        // Offset MUST be even (it indexes a block of utf16 chars).
        if offset & 1 != 0 {
            return Err(Error::Corrupt("MLU entry offset is odd"));
        }
        // Check for overflow / underflow against the directory + tag bounds.
        if offset < size_of_header.wrapping_add(8) {
            return Err(Error::Corrupt("MLU entry offset inside header"));
        }
        let end = offset.wrapping_add(len);
        if end < len || end > size.wrapping_add(8) {
            return Err(Error::Corrupt("MLU entry runs past tag"));
        }

        // True begin of the string, relative to the start of the pool.
        let begin = offset - size_of_header - 8;
        dir.push((language, country, len, begin));
    }

    // The cursor now sits at the start of the string pool.
    let pool_start = r.tell();

    // lcms2 reads the pool as one block of `LargestPosition` bytes (the max
    // `BeginOfThisString + Len` over all entries) starting at the pool, leaving the
    // IOHANDLER cursor at `pool_start + LargestPosition`. We read per-entry via
    // `read_at`, so we must restore the cursor to that same end position — this
    // matters for the SEQUENTIAL embedded-MLU reads in pseq/psid, where the next
    // `_cmsReadTypeBase` continues from exactly there.
    let mut largest_position: u64 = 0;
    let mut entries = Vec::with_capacity((count as usize).min(0x1_0000));
    for (language, country, len, begin) in dir {
        // Read this entry's UTF-16BE slice straight from the pool. lcms2's
        // Entries[i].Len is `len` BYTES; the unit count is len/2 (an odd
        // trailing byte is never consumed by cmsMLUget*).
        let mut buf = vec![0u8; len as usize];
        r.read_at(pool_start + begin as u64, &mut buf)?;
        let text = decode_utf16be(&buf);
        largest_position = largest_position.max(begin as u64 + len as u64);
        entries.push(MluEntry {
            language,
            country,
            text,
        });
    }

    // Leave the cursor where lcms2's single pool read leaves it.
    r.seek(pool_start + largest_position)?;

    Ok(Mlu { entries })
}

/// `Type_Text_Description_Read` (`cmstypes.c:1096`): the ICC v2 `textDescription`
/// type. Layout: an ASCII block (`AsciiCount: u32` + `AsciiCount` ASCII bytes),
/// then an optional Unicode block (`ucLangCode: u32` + `ucCount: u32` + UTF-16BE),
/// then an optional Macintosh ScriptCode block (67 fixed bytes).
///
/// lcms2 builds a `cmsMLU` with two slots: the ASCII under `cmsNoLanguage`/
/// `cmsNoCountry` (the `"\0\0"`/`"\0\0"` codes), and — when the Unicode block is
/// present and valid — the wide string under `cmsV2Unicode`/`cmsV2Unicode` (the
/// `"\xff\xff"`/`"\xff\xff"` codes). We replicate exactly which slots lcms2 sets,
/// in the order it sets them.
///
/// Important tolerances transcribed from the C:
/// - `AsciiCount > 0x7ffff` → error; `SizeOfTag < AsciiCount` → error.
/// - The Unicode block is skipped (no second entry) unless ALL hold:
///   remaining size ≥ 8 (the two u32s), then `ucCount != 0`,
///   `ucCount <= 0x7ffff`, and remaining size ≥ `ucCount * 2`. lcms2 `goto Done`s
///   (keeping just the ASCII entry) the moment any check fails.
/// - The ASCII is read as Latin-1/NUL-terminated (matching `cmsMLUsetASCII`,
///   which stores wide chars 1:1 from the bytes up to the first NUL).
pub fn read_text_description<R: ProfileReader>(r: &mut R, size: u32) -> Result<Tag> {
    Ok(Tag::Mlu(read_text_description_value(r, size)?))
}

/// `Type_Text_Description_Read` returning the bare [`Mlu`]. Used by the embedded
/// text reader (profile-sequence) when the embedded type base is `'desc'`.
pub fn read_text_description_value<R: ProfileReader>(r: &mut R, size: u32) -> Result<Mlu> {
    // One dword should be there.
    if size < 4 {
        return Err(Error::Corrupt("textDescription too small for ASCII count"));
    }

    let ascii_count = r.read_u32()?;
    if ascii_count > 0x7ffff {
        return Err(Error::Corrupt("textDescription ASCII count too large"));
    }
    let mut remaining = size - 4;
    if remaining < ascii_count {
        return Err(Error::Corrupt("textDescription ASCII runs past tag"));
    }

    // Read the ASCII (Latin-1, truncated at the first NUL — cmsMLUsetASCII copies
    // up to the NUL the C force-appends). This is the cmsNoLanguage/cmsNoCountry
    // entry, whose codes are "\0\0"/"\0\0".
    let ascii = r.read_ascii(ascii_count as usize)?;
    remaining -= ascii_count;

    let mut entries = vec![MluEntry {
        language: [0, 0],
        country: [0, 0],
        text: ascii,
    }];

    // Skip Unicode code. From here the C is tolerant: any shortfall `goto Done`s
    // with just the ASCII entry kept.
    if remaining >= 8 {
        let _unicode_code = r.read_u32()?;
        let unicode_count = r.read_u32()?;
        remaining -= 8;

        if unicode_count != 0
            && unicode_count <= 0x7ffff
            && remaining >= unicode_count.wrapping_mul(2)
        {
            let mut buf = vec![0u8; (unicode_count as usize) * 2];
            r.read_exact(&mut buf)?;
            // Unlike Type_MLU_Read (which keeps the raw Len/2 units), the
            // textDescription Unicode block goes through cmsMLUsetWide, which
            // measures the length with `mywcslen` and so STOPS at the first NUL
            // code unit. Truncate to the first U+0000 to match.
            let nul = buf
                .chunks_exact(2)
                .position(|c| c == [0, 0])
                .map(|i| i * 2)
                .unwrap_or(buf.len());
            // cmsV2Unicode is "\xff\xff" for both language and country.
            entries.push(MluEntry {
                language: [0xff, 0xff],
                country: [0xff, 0xff],
                text: decode_utf16be(buf.get(..nul).unwrap_or_default()),
            });
            // The remaining ScriptCode block is skipped by the C; we never store
            // it, so there is no need to consume it here.
        }
    }

    Ok(Mlu { entries })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::io::cursor::MemReader;

    /// Hand-built two-record `mluc` payload exercising the offset/pool path
    /// deterministically. Mirrors the real `read_tag` path: an 8-byte type base
    /// (`'mluc'` + reserved) precedes the payload, the reader is seeked past the
    /// base via `read_type_base`, and `Offset` is measured from the tag start
    /// (so it includes the 8-byte base). Two BMP strings "Hi" and "Yo" live in
    /// the pool back-to-back.
    #[test]
    fn synthetic_two_record_mlu_decodes() {
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&2u32.to_be_bytes()); // Count
        payload.extend_from_slice(&12u32.to_be_bytes()); // RecLen

        // SizeOfHeader = 12*2 + 8 = 32; pool starts at tag-offset 32 + 8 = 40.
        // Record 0: en/US, "Hi" (4 bytes) at offset 40.
        payload.extend_from_slice(b"en");
        payload.extend_from_slice(b"US");
        payload.extend_from_slice(&4u32.to_be_bytes()); // Len (bytes)
        payload.extend_from_slice(&40u32.to_be_bytes()); // Offset (from tag start)
                                                         // Record 1: de/DE, "Yo" (4 bytes) at offset 44.
        payload.extend_from_slice(b"de");
        payload.extend_from_slice(b"DE");
        payload.extend_from_slice(&4u32.to_be_bytes());
        payload.extend_from_slice(&44u32.to_be_bytes());
        // Pool: "Hi" then "Yo" as UTF-16BE.
        payload.extend_from_slice(&[0x00, b'H', 0x00, b'i']);
        payload.extend_from_slice(&[0x00, b'Y', 0x00, b'o']);

        let size = payload.len() as u32;

        // Prepend the 8-byte type base and drive the reader exactly as read_tag does.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"mluc");
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&payload);

        let mut r = MemReader::new(&buf);
        let _sig = r.read_type_base().unwrap();
        let tag = read_mlu(&mut r, size).unwrap();

        match tag {
            Tag::Mlu(mlu) => {
                assert_eq!(mlu.entries.len(), 2);
                assert_eq!(mlu.entries[0].language, *b"en");
                assert_eq!(mlu.entries[0].country, *b"US");
                assert_eq!(mlu.entries[0].text, "Hi");
                assert_eq!(mlu.entries[1].language, *b"de");
                assert_eq!(mlu.entries[1].country, *b"DE");
                assert_eq!(mlu.entries[1].text, "Yo");
            }
            other => panic!("expected Mlu, got {other:?}"),
        }
    }

    /// RecLen != 12 is rejected (cmsERROR_UNKNOWN_EXTENSION in lcms2).
    #[test]
    fn rec_len_not_12_is_rejected() {
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&16u32.to_be_bytes()); // bad RecLen
        let size = payload.len() as u32;
        let mut r = MemReader::new(&payload);
        assert!(read_mlu(&mut r, size).is_err());
    }
}
