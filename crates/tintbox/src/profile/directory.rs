//! ICC tag directory parse, transcribed from lcms2 `_cmsReadHeader`'s tag-table
//! loop (`src/cmsio0.c:929-988`).
//!
//! The directory follows the 128-byte header: a u32 tag count, then `count`
//! 12-byte entries `{ sig: u32, offset: u32, size: u32 }`. We replicate lcms2's
//! three behaviours exactly:
//!
//! * §7.3 — sanity skips (cmsio0.c:947-950): drop an entry when `size==0 ||
//!   offset==0`, or `offset+size > HeaderSize` (strict), or `offset+size <
//!   offset` (u32 overflow). The profile still opens.
//! * §7.2 — link detection (cmsio0.c:956-970): an accepted entry links to an
//!   EARLIER accepted entry with the same offset AND size, provided
//!   `CompatibleTypes` holds for the two tag descriptors. lcms2 keeps the LAST
//!   such match (the loop overwrites `TagLinked` without breaking).
//! * §7.4 — duplicate signatures (cmsio0.c:976-986): after the loop, if any two
//!   accepted entries share a signature, the whole profile is rejected.
//!
//! `HeaderSize` is `min(header.size, reported_size)` where `reported_size` is the
//! actual byte length of the profile slice (cmsio0.c:915-919: the header size is
//! clamped down to the IOhandler's `ReportedSize`).

use crate::error::{Error, Result};
use crate::io::ProfileReader;
use crate::profile::descriptor::compatible_types;
use crate::sig::Signature;

/// lcms2 `MAX_TABLE_TAG` (`src/lcms2_internal.h`).
const MAX_TABLE_TAG: u32 = 100;

/// One accepted tag-directory entry. `linked` is `Some(sig)` when this entry is
/// a link to an earlier entry's data (lcms2 `TagLinked`), else `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagEntry {
    pub sig: Signature,
    pub offset: u32,
    pub size: u32,
    pub linked: Option<Signature>,
}

/// Parse the tag directory. `r` must be positioned at the start of the directory
/// (byte offset 128, immediately after the header). `header_size` is the raw
/// `header.size` field; `slice_len` is the profile's actual byte length (the
/// IOhandler `ReportedSize`). Returns the accepted entries, or an error when the
/// tag count is out of range, a read truncates, or a duplicate signature is
/// found (§7.4).
pub fn parse_directory<R: ProfileReader>(
    r: &mut R,
    header_size: u32,
    slice_len: usize,
) -> Result<Vec<TagEntry>> {
    // HeaderSize = min(header.size, reported_size). lcms2 clamps the reported
    // header size down to the actual profile length (cmsio0.c:915-919).
    let reported_size = u32::try_from(slice_len).unwrap_or(u32::MAX);
    let header_clamped = header_size.min(reported_size);

    let tag_count = r.read_u32()?;
    if tag_count > MAX_TABLE_TAG {
        return Err(Error::Range);
    }

    let mut entries: Vec<TagEntry> = Vec::with_capacity(tag_count as usize);
    for _ in 0..tag_count {
        let sig = Signature::from_raw(r.read_u32()?);
        let offset = r.read_u32()?;
        let size = r.read_u32()?;

        // §7.3 sanity: offset + size must fall inside the (clamped) profile.
        if size == 0 || offset == 0 {
            continue;
        }
        // `offset + size > HeaderSize || offset + size < offset` (overflow),
        // computed with u32 wrapping to match the C exactly.
        let end = offset.wrapping_add(size);
        if end > header_clamped || end < offset {
            continue;
        }

        // §7.2 links: scan earlier accepted entries for a matching (offset,size)
        // whose descriptors are CompatibleTypes. lcms2 keeps the LAST match.
        let mut linked = None;
        for prev in &entries {
            if prev.offset == offset && prev.size == size && compatible_types(prev.sig, sig) {
                linked = Some(prev.sig);
            }
        }

        entries.push(TagEntry {
            sig,
            offset,
            size,
            linked,
        });
    }

    // §7.4 duplicate signatures reject the whole profile.
    for i in 0..entries.len() {
        for j in 0..entries.len() {
            if i != j && entries[i].sig == entries[j].sig {
                return Err(Error::Corrupt("duplicate tag"));
            }
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// Build a directory byte block: u32 count then 12-byte entries.
    fn dir_bytes(entries: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (sig, off, size) in entries {
            v.extend_from_slice(&sig.to_be_bytes());
            v.extend_from_slice(&off.to_be_bytes());
            v.extend_from_slice(&size.to_be_bytes());
        }
        v
    }

    #[test]
    fn skips_zero_and_out_of_range() {
        // entry1: size 0 (skip); entry2: offset 0 (skip); entry3: end > headersize (skip);
        // entry4: valid.
        let bytes = dir_bytes(&[
            (0x41324230, 200, 0),   // A2B0 size 0 -> skip
            (0x42324130, 0, 10),    // B2A0 offset 0 -> skip
            (0x6b545243, 900, 200), // kTRC end=1100 > 1000 -> skip
            (0x77747074, 200, 20),  // wtpt valid
        ]);
        let mut r = MemReader::new(&bytes);
        let e = parse_directory(&mut r, 1000, 1000).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].sig.to_raw(), 0x77747074);
        assert!(e[0].linked.is_none());
    }

    #[test]
    fn clamps_headersize_to_slice_len() {
        // header.size says 100000 but the slice is only 1000 bytes; an entry that
        // would fit in 100000 but not 1000 is skipped.
        let bytes = dir_bytes(&[(0x77747074, 500, 600)]); // end=1100
        let mut r = MemReader::new(&bytes);
        let e = parse_directory(&mut r, 100_000, 1000).unwrap();
        assert!(
            e.is_empty(),
            "clamped to slice_len=1000 -> end 1100 dropped"
        );
    }

    #[test]
    fn detects_link_for_compatible_shared_offset() {
        // DeviceMfgDesc and ProfileDescription share offset/size and are
        // CompatibleTypes -> the second links to the first.
        let bytes = dir_bytes(&[
            (0x646D_6E64, 200, 40), // dmnd
            (0x6465_7363, 200, 40), // desc -> links to dmnd
        ]);
        let mut r = MemReader::new(&bytes);
        let e = parse_directory(&mut r, 1000, 1000).unwrap();
        assert_eq!(e.len(), 2);
        assert!(e[0].linked.is_none());
        assert_eq!(e[1].linked, Some(Signature::from_raw(0x646D_6E64)));
    }

    #[test]
    fn no_link_when_incompatible_even_if_offset_matches() {
        // A2B0 (LUT types) and wtpt (XYZ types) share offset/size but are NOT
        // CompatibleTypes -> no link.
        let bytes = dir_bytes(&[
            (0x4132_4230, 200, 40), // A2B0
            (0x7774_7074, 200, 40), // wtpt, same offset/size but incompatible
        ]);
        let mut r = MemReader::new(&bytes);
        let e = parse_directory(&mut r, 1000, 1000).unwrap();
        assert_eq!(e.len(), 2);
        assert!(e[1].linked.is_none());
    }

    #[test]
    fn duplicate_signature_rejects() {
        let bytes = dir_bytes(&[(0x77747074, 200, 20), (0x77747074, 300, 20)]);
        let mut r = MemReader::new(&bytes);
        assert!(matches!(
            parse_directory(&mut r, 1000, 1000),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn too_many_tags_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&101u32.to_be_bytes());
        let mut r = MemReader::new(&bytes);
        assert!(matches!(
            parse_directory(&mut r, 1000, 1000),
            Err(Error::Range)
        ));
    }
}
