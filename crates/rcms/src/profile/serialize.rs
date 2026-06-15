//! ICC profile serializer — the byte-exact inverse of the slice-2/3/4 readers.
//!
//! Mirrors lcms2's `cmsSaveProfileToIOhandler` (`src/cmsio0.c:1533`) over an
//! in-memory [`WritableProfile`] (the write-side analogue of the reader's
//! `Profile`; the reader borrows bytes and decodes lazily, so it is not a
//! mutable builder). A `WritableProfile` is lcms2's `_cmsICCPROFILE` reduced to
//! what the serializer reads: the header fields plus the ordered tag table
//! (`TagNames[]` / `TagPtrs[]` / `TagLinked[]` in insertion order).
//!
//! ## Two-pass (cmsio0.c:1553-1568)
//! Pass 1 writes the header + every tag body through a [`CountWriter`] (lcms2's
//! NULL handler) to learn each tag's offset/size and the total length. Pass 2
//! resolves links (lcms2 `SetLinks` — linked tags inherit the target's
//! offset/size), writes the header with the patched profile size, then writes
//! the directory and the tag bodies for real. Because both passes run the SAME
//! body-writing code, the computed sizes match the real write to the byte.
//!
//! ## Header (`_cmsWriteHeader`, cmsio0.c:988)
//! The fixed 128-byte `cmsICCHeader`, big-endian, in struct-field order. The
//! illuminant is ALWAYS written as `cmsD50_XYZ()` (cmsio0.c:1019-1022),
//! regardless of the parsed `illuminant` — so we hardcode D50 there. All
//! reserved bytes are zero; the profile-ID is 16 raw bytes.
//!
//! ## Directory (cmsio0.c:1034-1055)
//! A u32 count EXCLUDING null/placeholder slots, then one 12-byte
//! `(sig, offset, size)` triple per tag, big-endian, in insertion order.
//! `offset` is bytes from the profile start; `size` is type-base(8) + body,
//! BEFORE alignment padding.
//!
//! ## Tag bodies (`SaveTags`, cmsio0.c:1391)
//! For each non-linked tag: record the current offset, write the 8-byte
//! type-base (`_cmsWriteTypeBase`), write the type body, set `size = bytes
//! written` (pre-alignment), then `_cmsWriteAlignment` pads 0x00 to the next
//! 4-byte boundary. The NEXT tag's offset is thus post-alignment, while the
//! directory `size` is pre-alignment — the two differ by the padding.
//!
//! ## Linked tags (`SetLinks`, cmsio0.c:1506)
//! A slot may be a LINK to another tag (lcms2 `cmsLinkTag`). Its body is not
//! written; after pass 1 computes the target's offset/size, the link's directory
//! entry inherits them. Required for sRGB's three linked TRCs (T4).

use crate::color::CIEXYZ;
use crate::error::{Error, Result};
use crate::io::{CountWriter, MemWriter, ProfileWriter};
use crate::profile::header::Header;
use crate::profile::tag::Tag;
use crate::sig::Signature;

/// The content of one tag slot: either an owned tag value (a body to serialize)
/// or a link to another tag in the same profile (lcms2 `TagLinked[i]`).
#[derive(Clone, Debug)]
pub enum SlotContent {
    /// A real tag value; the serializer writes its type-base + body.
    Body(Tag),
    /// A link to the slot named by this signature (lcms2 `cmsLinkTag`): no body
    /// is written; the directory entry inherits the target's offset and size.
    Linked(Signature),
}

/// One entry of the write-side tag table — lcms2's `TagNames[i]` (`sig`) paired
/// with `TagPtrs[i]`/`TagLinked[i]` (`content`). Insertion order is preserved,
/// matching lcms2's directory order.
#[derive(Clone, Debug)]
pub struct TagSlot {
    pub sig: Signature,
    pub content: SlotContent,
}

/// A profile being assembled for serialization — lcms2's `_cmsICCPROFILE`
/// reduced to the header fields and the ordered tag table the serializer reads.
///
/// Build one with [`WritableProfile::new`] (or `Default`), set the header fields,
/// then add tags with [`add_tag`](Self::add_tag) / [`link_tag`](Self::link_tag)
/// in the order they should appear in the directory.
#[derive(Clone, Debug)]
pub struct WritableProfile {
    /// The header fields to write. `size` and `illuminant` are IGNORED at write
    /// time: `size` is patched to the computed total, and the illuminant is
    /// always D50 (cmsio0.c:1019). Every other field is written verbatim.
    pub header: Header,
    /// The tag slots in insertion (directory) order.
    pub tags: Vec<TagSlot>,
}

impl WritableProfile {
    /// A new profile carrying `header` and no tags.
    pub fn new(header: Header) -> Self {
        WritableProfile {
            header,
            tags: Vec::new(),
        }
    }

    /// Append a tag with an owned body, in directory order. lcms2 reuses the slot
    /// for a repeated signature; we keep it simple and append (the caller controls
    /// uniqueness, matching how the virtual-profile builders emit each tag once).
    pub fn add_tag(&mut self, sig: Signature, value: Tag) -> &mut Self {
        self.tags.push(TagSlot {
            sig,
            content: SlotContent::Body(value),
        });
        self
    }

    /// Append a tag that LINKS to `target` (lcms2 `cmsLinkTag(sig, target)`): the
    /// link's body is not written; its directory entry inherits the target's
    /// offset/size during serialization.
    pub fn link_tag(&mut self, sig: Signature, target: Signature) -> &mut Self {
        self.tags.push(TagSlot {
            sig,
            content: SlotContent::Linked(target),
        });
        self
    }
}

/// Per-tag offset/size computed in pass 1 (lcms2 `TagOffsets[i]`/`TagSizes[i]`).
/// `offset` is bytes from the profile start; `size` is type-base + body, BEFORE
/// alignment (the value that lands in the directory).
#[derive(Clone, Copy, Debug)]
struct TagLayout {
    offset: u32,
    size: u32,
}

/// The byte length of the profile header plus the tag directory: the 128-byte
/// header, a u32 tag count, and 12 bytes per non-null tag. Tag bodies begin here.
/// lcms2 computes this implicitly by writing the header+directory first; we
/// compute it up front so a single forward pass can assign offsets.
fn header_and_directory_len(tag_count: usize) -> usize {
    128 + 4 + tag_count * 12
}

/// Serialize `profile` to ICC bytes, byte-identical to lcms2
/// `cmsSaveProfileToMem` over the same constructed profile.
///
/// Runs the two-pass: pass 1 lays out every non-linked tag body through a
/// [`CountWriter`] to learn its offset/size and the total length; then links are
/// resolved and pass 2 writes the header (with the patched profile size), the
/// directory, and the tag bodies for real.
pub fn save_to_mem(profile: &WritableProfile) -> Result<Vec<u8>> {
    let tag_count = profile.tags.len();

    // ---- Pass 1: lay out tag bodies, compute offset/size + total. ----
    // Bodies begin right after the header + directory. We walk the tags in order,
    // writing each non-linked body through a counting writer; the running
    // position (header+dir + bytes so far) is the tag's offset, and the
    // pre-alignment delta is its size. Linked slots are skipped here (resolved
    // below). This mirrors `SaveTags` running through lcms2's NULL handler.
    let body_start = header_and_directory_len(tag_count);
    let mut layouts: Vec<Option<TagLayout>> = vec![None; tag_count];
    let mut counter = CountWriter::new();

    for (i, slot) in profile.tags.iter().enumerate() {
        let SlotContent::Body(ref value) = slot.content else {
            continue; // Linked tags are not written (cmsio0.c:1410).
        };
        let begin = body_start + counter.len(); // io->UsedSpace at body start.
        counter.write_type_base(write_type_for(slot.sig, value)?)?;
        write_tag_body(&mut counter, value)?;
        let size = (body_start + counter.len()) - begin; // pre-alignment size.
        counter.write_alignment()?;
        layouts[i] = Some(TagLayout {
            offset: u32::try_from(begin).map_err(|_| Error::Range)?,
            size: u32::try_from(size).map_err(|_| Error::Range)?,
        });
    }

    let total = body_start + counter.len();
    let used_space = u32::try_from(total).map_err(|_| Error::Range)?;

    // ---- SetLinks: linked slots inherit the target's offset/size. ----
    // (cmsio0.c:1506.) A link points at an earlier-or-later body slot by sig; we
    // resolve to that slot's already-computed layout. Resolution is single-level
    // (lcms2 `cmsLinkTag` targets a real tag, not another link).
    for (i, slot) in profile.tags.iter().enumerate() {
        if let SlotContent::Linked(target) = slot.content {
            let j = profile
                .tags
                .iter()
                .position(|s| s.sig == target && matches!(s.content, SlotContent::Body(_)))
                .ok_or(Error::Corrupt("linked tag target missing"))?;
            layouts[i] = layouts[j];
        }
    }

    // ---- Pass 2: write header + directory + bodies for real. ----
    let mut w = MemWriter::new();
    write_header(&mut w, &profile.header, used_space)?;
    write_directory(&mut w, profile, &layouts)?;

    for slot in &profile.tags {
        let SlotContent::Body(ref value) = slot.content else {
            continue;
        };
        w.write_type_base(write_type_for(slot.sig, value)?)?;
        write_tag_body(&mut w, value)?;
        w.write_alignment()?;
    }

    let bytes = w.into_inner();
    debug_assert_eq!(
        bytes.len(),
        total,
        "size pass ({total}) and write pass ({}) disagree",
        bytes.len()
    );
    Ok(bytes)
}

/// Write the 128-byte ICC header (`_cmsWriteHeader`, cmsio0.c:988). Field order
/// is the `cmsICCHeader` struct order; `used_space` is patched into `size`. The
/// illuminant is ALWAYS D50 (cmsio0.c:1019), not `header.illuminant`.
fn write_header<W: ProfileWriter>(w: &mut W, h: &Header, used_space: u32) -> Result<()> {
    w.write_u32(used_space)?; // size (patched to total)
    w.write_u32(h.cmm.to_raw())?; // cmmId
    w.write_u32(h.version)?; // version (already BCD-encoded)
    w.write_u32(h.device_class.to_raw())?; // deviceClass
    w.write_u32(h.color_space.to_raw())?; // colorSpace
    w.write_u32(h.pcs.to_raw())?; // pcs
                                  // date: six big-endian u16 (year..seconds), wire order.
    w.write_u16(h.date.year)?;
    w.write_u16(h.date.month)?;
    w.write_u16(h.date.day)?;
    w.write_u16(h.date.hours)?;
    w.write_u16(h.date.minutes)?;
    w.write_u16(h.date.seconds)?;
    w.write_u32(MAGIC)?; // 'acsp'
    w.write_u32(h.platform.to_raw())?; // platform
    w.write_u32(h.flags)?; // flags
    w.write_u32(h.manufacturer.to_raw())?; // manufacturer
    w.write_u32(h.model)?; // model
    w.write_u64(h.attributes)?; // attributes (u64)
    w.write_u32(h.rendering_intent.to_raw())?; // renderingIntent
                                               // illuminant: ALWAYS D50 (cmsio0.c:1019-1022),
                                               // i.e. `cmsD50_XYZ()` = {0.9642, 1.0, 0.8249}
                                               // (cmswhitepoint.c `cmsD50{X,Y,Z}`), NOT
                                               // `header.illuminant`.
    w.write_s15fixed16(D50_X)?;
    w.write_s15fixed16(D50_Y)?;
    w.write_s15fixed16(D50_Z)?;
    w.write_u32(h.creator.to_raw())?; // creator
    w.write_all(&h.profile_id)?; // profileID (16 raw bytes)
    w.write_all(&[0u8; 28])?; // reserved[28], zeroed
    Ok(())
}

/// Write the tag directory (cmsio0.c:1034-1055): u32 count of NON-null tags, then
/// a 12-byte `(sig, offset, size)` triple per tag in insertion order. Every slot
/// here is a real tag (we have no placeholder/null slots), so the count is the
/// table length.
fn write_directory<W: ProfileWriter>(
    w: &mut W,
    profile: &WritableProfile,
    layouts: &[Option<TagLayout>],
) -> Result<()> {
    w.write_u32(u32::try_from(profile.tags.len()).map_err(|_| Error::Range)?)?;
    for (i, slot) in profile.tags.iter().enumerate() {
        let layout = layouts[i].ok_or(Error::Corrupt("tag layout unresolved"))?;
        w.write_u32(slot.sig.to_raw())?;
        w.write_u32(layout.offset)?;
        w.write_u32(layout.size)?;
    }
    Ok(())
}

/// The ICC magic number `'acsp'` (lcms2 `cmsMagicNumber`).
const MAGIC: u32 = 0x6163_7370;

/// `cmsD50_XYZ()` (lcms2 `cmsD50{X,Y,Z}`): the D50 illuminant the header always
/// stores at the illuminant field (cmsio0.c:1019-1022).
const D50_X: f64 = 0.9642;
const D50_Y: f64 = 1.0;
const D50_Z: f64 = 0.8249;

// ---- Tag-type dispatch (T0: XYZ + text only) ----

/// The tag-TYPE signature to write for a given tag signature + value. lcms2 picks
/// this via `_cmsGetTagDescriptor(sig)->DecideType` or `SupportedTypes[0]`
/// (cmsio0.c:1461). T0 covers only the two trivial type writers this task lands;
/// the full DecideType table (curv/para, mluc/desc, LUT selection) is T1.
fn write_type_for(_sig: Signature, value: &Tag) -> Result<Signature> {
    match value {
        Tag::Xyz(_) => Ok(SIG_XYZ_TYPE),
        Tag::Text(_) => Ok(SIG_TEXT_TYPE),
        _ => Err(Error::Unsupported("tag type writer not yet implemented")),
    }
}

const SIG_XYZ_TYPE: Signature = Signature::from_raw(0x5859_5A20); // 'XYZ '
const SIG_TEXT_TYPE: Signature = Signature::from_raw(0x7465_7874); // 'text'

/// Write a tag's body (NOT the type-base — the caller writes that). Dispatches on
/// the cooked [`Tag`] value. T0: XYZ and text.
fn write_tag_body<W: ProfileWriter>(w: &mut W, value: &Tag) -> Result<()> {
    match value {
        Tag::Xyz(xyz) => write_xyz(w, xyz),
        Tag::Text(s) => write_text(w, s),
        _ => Err(Error::Unsupported("tag type writer not yet implemented")),
    }
}

/// `Type_XYZ_Write` (cmstypes.c:367) → `_cmsWriteXYZNumber` (cmsplugin.c:350):
/// three s15Fixed16 (X, Y, Z), big-endian.
fn write_xyz<W: ProfileWriter>(w: &mut W, xyz: &CIEXYZ) -> Result<()> {
    w.write_s15fixed16(xyz.x)?;
    w.write_s15fixed16(xyz.y)?;
    w.write_s15fixed16(xyz.z)?;
    Ok(())
}

/// `Type_Text_Write` (cmstypes.c:965): the ASCII bytes INCLUDING the trailing NUL
/// separator. lcms2 gets the string length via `cmsMLUgetASCII(..., NULL, 0)`,
/// which counts the implicit `\0`, then writes exactly that many bytes — i.e. the
/// text followed by one NUL. No 4-byte alignment is applied by the writer itself
/// (the caller's `_cmsWriteAlignment` handles padding).
fn write_text<W: ProfileWriter>(w: &mut W, s: &str) -> Result<()> {
    w.write_all(s.as_bytes())?;
    w.write_u8(0)?; // trailing NUL separator (the "extra \0" lcms2 counts).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::header::{ColorSpace, DateTime, Header, ProfileClass, RenderingIntent};

    // Tag signatures used by the oracle's `save_basic_profile` (lcms2 sigs).
    const WTPT: Signature = Signature::from_raw(0x7774_7074); // 'wtpt'
    const RXYZ: Signature = Signature::from_raw(0x7258_595A); // 'rXYZ'
    const GXYZ: Signature = Signature::from_raw(0x6758_595A); // 'gXYZ'
    const BXYZ: Signature = Signature::from_raw(0x6258_595A); // 'bXYZ'
    const TARG: Signature = Signature::from_raw(0x7461_7267); // 'targ'

    /// Build the header that the oracle's `rcms_oracle_save_basic_profile` sets:
    /// a v4.4 Display RGB/XYZ profile with every field fixed (so the byte stream
    /// is deterministic). `illuminant`/`size` are ignored by the serializer.
    fn basic_header() -> Header {
        Header {
            size: 0, // patched by the serializer.
            cmm: Signature::from_raw(0),
            version: 0x0440_0000, // v4.4 BCD (cmsSetProfileVersion(4.4)).
            device_class: ProfileClass::Display,
            color_space: ColorSpace::Rgb,
            pcs: ColorSpace::XYZ,
            date: DateTime {
                year: 2026,
                month: 6,
                day: 15,
                hours: 12,
                minutes: 34,
                seconds: 56,
            },
            platform: Signature::from_raw(0),
            flags: 0,
            manufacturer: Signature::from_raw(0x6E6F_6E65), // 'none'
            model: 0x6D6F_6431,                             // 'mod1'
            attributes: 0,
            rendering_intent: RenderingIntent::RelativeColorimetric,
            illuminant: CIEXYZ {
                x: D50_X,
                y: D50_Y,
                z: D50_Z,
            },
            creator: Signature::from_raw(0),
            profile_id: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        }
    }

    fn xyz(x: f64, y: f64, z: f64) -> Tag {
        Tag::Xyz(CIEXYZ { x, y, z })
    }

    /// Build the rcms equivalent of the oracle's basic profile. `link` mirrors the
    /// oracle's `link` flag (green/blue link to red instead of carrying bodies).
    fn build_basic(link: bool) -> WritableProfile {
        let mut p = WritableProfile::new(basic_header());
        p.add_tag(WTPT, xyz(D50_X, D50_Y, D50_Z));
        p.add_tag(RXYZ, xyz(0.5, 0.25, 0.125));
        if link {
            p.link_tag(GXYZ, RXYZ);
            p.link_tag(BXYZ, RXYZ);
        } else {
            p.add_tag(GXYZ, xyz(0.25, 0.5, 0.0625));
            p.add_tag(BXYZ, xyz(0.125, 0.0625, 0.75));
        }
        p.add_tag(TARG, Tag::Text("rcms serializer test".to_string()));
        p
    }

    /// THE primary T0 contract: a built RGB/XYZ display profile (header + wtpt +
    /// rXYZ/gXYZ/bXYZ colorants + a text tag) serializes BYTE-IDENTICALLY to
    /// lcms2's `cmsSaveProfileToMem` over the same constructed profile. This
    /// proves the header layout, tag directory, alignment, offset computation, and
    /// the XYZ/text writers all match.
    #[test]
    fn save_to_mem_byte_identical_to_lcms2_unlinked() {
        let rust = save_to_mem(&build_basic(false)).expect("rcms serialize");
        let c = rcms_oracle::save_basic_profile(false).expect("lcms2 serialize");
        assert_eq!(
            rust.len(),
            c.len(),
            "length mismatch: rcms={} lcms2={}",
            rust.len(),
            c.len()
        );
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            panic!(
                "byte mismatch at index {:?}\n rcms[..48]={:02x?}\n lcms[..48]={:02x?}",
                first,
                &rust[..rust.len().min(48)],
                &c[..c.len().min(48)],
            );
        }
    }

    /// The linked-tag path: gXYZ/bXYZ are `cmsLinkTag`'d to rXYZ. Their directory
    /// entries must share rXYZ's offset/size, and only one XYZ body is written.
    /// Byte-identical to lcms2 linking the same three tags.
    #[test]
    fn save_to_mem_byte_identical_to_lcms2_linked() {
        let rust = save_to_mem(&build_basic(true)).expect("rcms serialize");
        let c = rcms_oracle::save_basic_profile(true).expect("lcms2 serialize");
        assert_eq!(rust.len(), c.len(), "linked length mismatch");
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            panic!("linked byte mismatch at index {first:?}");
        }
    }

    /// Re-open the serialized linked profile with the READER and confirm the link
    /// machinery is on-wire: gXYZ and bXYZ resolve to rXYZ's body (the three
    /// colorant entries share one offset/size), and the resolved XYZ value is what
    /// we wrote for red. This guards the offset-reuse without going through lcms2.
    #[test]
    fn linked_entries_share_red_offset_and_size() {
        let bytes = save_to_mem(&build_basic(true)).expect("rcms serialize");
        let prof = crate::profile::Profile::open(&bytes).expect("reopen");

        let r = prof.tag_entry(RXYZ).expect("rXYZ entry");
        let g = prof.tag_entry(GXYZ).expect("gXYZ entry");
        let b = prof.tag_entry(BXYZ).expect("bXYZ entry");
        assert_eq!(g.offset, r.offset, "gXYZ offset != rXYZ offset");
        assert_eq!(g.size, r.size, "gXYZ size != rXYZ size");
        assert_eq!(b.offset, r.offset, "bXYZ offset != rXYZ offset");
        assert_eq!(b.size, r.size, "bXYZ size != rXYZ size");

        // The shared body decodes to red's value.
        match prof.read_tag(GXYZ).expect("read gXYZ") {
            Tag::Xyz(v) => {
                assert_eq!(v.x, 0.5);
                assert_eq!(v.y, 0.25);
                assert_eq!(v.z, 0.125);
            }
            other => panic!("expected Xyz, got {other:?}"),
        }
    }

    /// The size pass and write pass agree: the serialized length equals the
    /// header's patched `size` field, which is the pass-1 total.
    #[test]
    fn size_pass_equals_buffer_len() {
        let bytes = save_to_mem(&build_basic(false)).expect("serialize");
        // The patched profile size lives at header offset 0 (big-endian u32).
        let header_size = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(header_size as usize, bytes.len());
    }
}
