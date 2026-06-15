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

use crate::color::{CIExyYTriple, CIEXYZ};
use crate::error::{Error, Result};
use crate::io::{CountWriter, MemWriter, ProfileWriter};
use crate::profile::header::{DateTime, Header};
use crate::profile::tag::{Cicp, ColorantTableEntry, Measurement, Tag, ViewingConditions};
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
    // lcms2 `Version = cmsGetProfileVersion(Icc)` (cmsio0.c:1402): the BCD header
    // version decoded to a float, used by the DecideType deciders.
    let version = profile_version_float(profile.header.version);

    for (i, slot) in profile.tags.iter().enumerate() {
        let SlotContent::Body(ref value) = slot.content else {
            continue; // Linked tags are not written (cmsio0.c:1410).
        };
        let begin = body_start + counter.len(); // io->UsedSpace at body start.
        counter.write_type_base(write_type_for(slot.sig, value, version)?)?;
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
        w.write_type_base(write_type_for(slot.sig, value, version)?)?;
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

// ---- Tag-type dispatch: the DecideType descriptor table (cmsio0.c:1458) ----

// Tag-TYPE signatures (`cmsTagTypeSignature`, include/lcms2.h).
const SIG_XYZ_TYPE: Signature = Signature::from_raw(0x5859_5A20); // 'XYZ '
const SIG_TEXT_TYPE: Signature = Signature::from_raw(0x7465_7874); // 'text'
const SIG_DESC_TYPE: Signature = Signature::from_raw(0x6465_7363); // 'desc'
const SIG_MLUC_TYPE: Signature = Signature::from_raw(0x6D6C_7563); // 'mluc'
const SIG_SIGNATURE_TYPE: Signature = Signature::from_raw(0x7369_6720); // 'sig '
const SIG_DATA_TYPE: Signature = Signature::from_raw(0x6461_7461); // 'data'
const SIG_DATETIME_TYPE: Signature = Signature::from_raw(0x6474_696D); // 'dtim'
const SIG_CHROMATICITY_TYPE: Signature = Signature::from_raw(0x6368_726D); // 'chrm'
const SIG_COLORANT_ORDER_TYPE: Signature = Signature::from_raw(0x636C_726F); // 'clro'
const SIG_S15F16_TYPE: Signature = Signature::from_raw(0x7366_3332); // 'sf32'
const SIG_U16F16_TYPE: Signature = Signature::from_raw(0x7566_3332); // 'uf32'
const SIG_UINT32_TYPE: Signature = Signature::from_raw(0x7569_3332); // 'ui32'
const SIG_MEASUREMENT_TYPE: Signature = Signature::from_raw(0x6D65_6173); // 'meas'
const SIG_VIEWING_COND_TYPE: Signature = Signature::from_raw(0x76696577); // 'view'
const SIG_COLORANT_TABLE_TYPE: Signature = Signature::from_raw(0x636C_7274); // 'clrt'
const SIG_CICP_TYPE: Signature = Signature::from_raw(0x6369_6370); // 'cicp'
const SIG_VCGT_TYPE: Signature = Signature::from_raw(0x7663_6774); // 'vcgt'
const SIG_LUT16_TYPE: Signature = Signature::from_raw(0x6D667432); // 'mft2'
const SIG_LUT_ATOB_TYPE: Signature = Signature::from_raw(0x6D414220); // 'mAB '
const SIG_LUT_BTOA_TYPE: Signature = Signature::from_raw(0x6D424120); // 'mBA '

// Tag signatures (`cmsTagSignature`, include/lcms2.h) needed by the descriptor
// table — only those whose default type or decider is non-obvious from the value.
const TAG_COPYRIGHT: Signature = Signature::from_bytes(*b"cprt");
const TAG_DEVICE_MFG_DESC: Signature = Signature::from_bytes(*b"dmnd");
const TAG_DEVICE_MODEL_DESC: Signature = Signature::from_bytes(*b"dmdd");
const TAG_PROFILE_DESCRIPTION: Signature = Signature::from_bytes(*b"desc");
const TAG_VIEWING_COND_DESC: Signature = Signature::from_bytes(*b"vued");
const TAG_SCREENING_DESC: Signature = Signature::from_bytes(*b"scrd");
const TAG_ATOB0: Signature = Signature::from_bytes(*b"A2B0");
const TAG_ATOB1: Signature = Signature::from_bytes(*b"A2B1");
const TAG_ATOB2: Signature = Signature::from_bytes(*b"A2B2");
const TAG_BTOA0: Signature = Signature::from_bytes(*b"B2A0");
const TAG_BTOA1: Signature = Signature::from_bytes(*b"B2A1");
const TAG_BTOA2: Signature = Signature::from_bytes(*b"B2A2");
const TAG_GAMUT: Signature = Signature::from_bytes(*b"gamt");
const TAG_PREVIEW0: Signature = Signature::from_bytes(*b"pre0");
const TAG_PREVIEW1: Signature = Signature::from_bytes(*b"pre1");
const TAG_PREVIEW2: Signature = Signature::from_bytes(*b"pre2");

/// lcms2 `BaseToBase(in, BaseIn, BaseOut)` (cmsio0.c:1209): reinterpret `in`'s
/// `BaseIn` digits as `BaseOut` digits. `cmsGetProfileVersion` uses
/// `BaseToBase(Version>>16, 16, 10) / 100.0`.
fn base_to_base(mut input: u32, base_in: u32, base_out: u32) -> u32 {
    let mut digits = [0u8; 100];
    let mut len = 0usize;
    while input > 0 && len < 100 {
        digits[len] = (input % base_in) as u8;
        input /= base_in;
        len += 1;
    }
    let mut out = 0u32;
    for i in (0..len).rev() {
        out = out * base_out + digits[i] as u32;
    }
    out
}

/// lcms2 `cmsGetProfileVersion` (cmsio0.c:1237): the BCD header version decoded to
/// a float (e.g. `0x04400000` → 4.4). The DecideType deciders compare this to 4.0.
fn profile_version_float(version: u32) -> f64 {
    base_to_base(version >> 16, 16, 10) as f64 / 100.0
}

/// The tag-TYPE signature to write for a tag (lcms2 `SaveTags`, cmsio0.c:1458):
/// the `cmsTagDescriptor`'s `DecideType(Version, Data)` if present, else its
/// `SupportedTypes[0]`. The default `SupportedTypes[0]` is determined by the tag's
/// value shape (the rcms reader produces exactly one `Tag` variant per type), but
/// the version-dependent deciders (curv/para, text/mluc/desc, LUT selection) need
/// the tag SIGNATURE plus the profile version. We mirror the descriptor table.
fn write_type_for(sig: Signature, value: &Tag, version: f64) -> Result<Signature> {
    match value {
        // XYZType: rXYZ/gXYZ/bXYZ go through DecideXYZtype (always 'XYZ '); all
        // other XYZ-valued tags (wtpt/bkpt/lumi) default to 'XYZ '.
        Tag::Xyz(_) => Ok(SIG_XYZ_TYPE),

        // TextType vs mluc/desc is decided by the tag signature + version.
        Tag::Text(_) => Ok(decide_text(sig, version)),
        Tag::Mlu(_) => Ok(decide_text(sig, version)),

        // DecideCurveType (cmstypes.c:1436) inspects the curve's segments, and the
        // curv/para body writers land in T2; stub with a clear Unsupported.
        Tag::Curve(_) => Err(Error::Unsupported("curv/para writer is T2")),

        // LUT type selection is DecideLUTtypeA2B/B2A (version + pipeline shape, T3).
        Tag::Lut(_) => Ok(decide_lut(sig, version)),

        // No-decider trivial types: SupportedTypes[0] is fixed by the value shape.
        Tag::S15Fixed16Array(_) => Ok(SIG_S15F16_TYPE),
        Tag::U16Fixed16Array(_) => Ok(SIG_U16F16_TYPE),
        Tag::U32Array(_) => Ok(SIG_UINT32_TYPE),
        Tag::Signature(_) => Ok(SIG_SIGNATURE_TYPE),
        Tag::Data { .. } => Ok(SIG_DATA_TYPE),
        Tag::DateTime(_) => Ok(SIG_DATETIME_TYPE),
        Tag::Chromaticity(_) => Ok(SIG_CHROMATICITY_TYPE),
        Tag::ColorantOrder(_) => Ok(SIG_COLORANT_ORDER_TYPE),
        Tag::Measurement(_) => Ok(SIG_MEASUREMENT_TYPE),
        Tag::ViewingConditions(_) => Ok(SIG_VIEWING_COND_TYPE),
        Tag::ColorantTable(_) => Ok(SIG_COLORANT_TABLE_TYPE),
        Tag::Cicp(_) => Ok(SIG_CICP_TYPE),
        Tag::Vcgt(_) => Ok(SIG_VCGT_TYPE),

        _ => Err(Error::Unsupported("tag type writer not yet implemented")),
    }
}

/// lcms2 `DecideTextType` (cmstypes.c:1012) and `DecideTextDescType`
/// (cmstypes.c:1315). The copyright tag uses `DecideTextType` (v2 → `text`,
/// v4 → `mluc`); the description tags use `DecideTextDescType` (v2 → `desc`,
/// v4 → `mluc`). Tags whose descriptor lists `cmsSigTextType` with NO decider
/// (e.g. `charTarget`) always write `text`.
fn decide_text(sig: Signature, version: f64) -> Signature {
    match sig {
        TAG_COPYRIGHT => {
            // DecideTextType: v4 → mluc, else text.
            if version >= 4.0 {
                SIG_MLUC_TYPE
            } else {
                SIG_TEXT_TYPE
            }
        }
        TAG_DEVICE_MFG_DESC
        | TAG_DEVICE_MODEL_DESC
        | TAG_PROFILE_DESCRIPTION
        | TAG_VIEWING_COND_DESC => {
            // DecideTextDescType: v4 → mluc, else desc.
            if version >= 4.0 {
                SIG_MLUC_TYPE
            } else {
                SIG_DESC_TYPE
            }
        }
        // profileDescriptionML (no decider) is always mluc; screeningDesc (no
        // decider) is always desc.
        TAG_SCREENING_DESC => SIG_DESC_TYPE,
        // charTarget and any other no-decider TextType tag → text.
        _ => SIG_TEXT_TYPE,
    }
}

/// lcms2 `DecideLUTtypeA2B` (cmstypes.c:1840) / `DecideLUTtypeB2A`
/// (cmstypes.c:1854). For v4 the A2B tags write `mAB ` and the B2A/gamut/preview
/// tags write `mBA `; for v2 they write `mft1`/`mft2` by the pipeline's
/// `SaveAs8Bits` flag (resolved in T3). The bodies are T3; this only fixes the
/// type signature so the dispatch table is complete.
fn decide_lut(sig: Signature, version: f64) -> Signature {
    let is_a2b = matches!(sig, TAG_ATOB0 | TAG_ATOB1 | TAG_ATOB2);
    let is_b2a = matches!(
        sig,
        TAG_BTOA0 | TAG_BTOA1 | TAG_BTOA2 | TAG_GAMUT | TAG_PREVIEW0 | TAG_PREVIEW1 | TAG_PREVIEW2
    );
    if version >= 4.0 {
        if is_a2b {
            SIG_LUT_ATOB_TYPE
        } else if is_b2a {
            SIG_LUT_BTOA_TYPE
        } else {
            // No A2B/B2A tag carried this LUT; default to the v4 A2B form.
            SIG_LUT_ATOB_TYPE
        }
    } else {
        // v2: mft1/mft2 by SaveAs8Bits — T3 resolves which; default to LUT16.
        SIG_LUT16_TYPE
    }
}

/// Write a tag's body (NOT the type-base — the caller writes that). Dispatches on
/// the cooked [`Tag`] value, mirroring each `Type_*_Write` in cmstypes.c. The
/// curve/MLU/LUT/MPE bodies arrive in T2/T3 and return `Unsupported` here.
fn write_tag_body<W: ProfileWriter>(w: &mut W, value: &Tag) -> Result<()> {
    match value {
        Tag::Xyz(xyz) => write_xyz(w, xyz),
        Tag::Text(s) => write_text(w, s),
        Tag::Signature(s) => write_signature(w, *s),
        Tag::Data { flag, data } => write_data(w, *flag, data),
        Tag::DateTime(d) => write_datetime(w, d),
        Tag::Chromaticity(t) => write_chromaticity(w, t),
        Tag::ColorantOrder(order) => write_colorant_order(w, order),
        Tag::S15Fixed16Array(v) => write_s15f16_array(w, v),
        Tag::U16Fixed16Array(v) => write_u16f16_array(w, v),
        Tag::U32Array(v) => write_u32_array(w, v),
        Tag::Measurement(m) => write_measurement(w, m),
        Tag::ViewingConditions(v) => write_viewing_conditions(w, v),
        Tag::ColorantTable(entries) => write_colorant_table(w, entries),
        Tag::Cicp(c) => write_cicp(w, c),
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

/// `Type_Signature_Write` (cmstypes.c:893): one big-endian u32.
fn write_signature<W: ProfileWriter>(w: &mut W, sig: Signature) -> Result<()> {
    w.write_u32(sig.to_raw())
}

/// `Type_Data_Write` (cmstypes.c:1063): a u32 flag word, then the opaque bytes.
fn write_data<W: ProfileWriter>(w: &mut W, flag: u32, data: &[u8]) -> Result<()> {
    w.write_u32(flag)?;
    w.write_all(data)
}

/// `Type_DateTime_Write` (cmstypes.c:1575) → `_cmsEncodeDateTimeNumber`
/// (cmsplugin.c:407): a `cmsDateTimeNumber` of six big-endian u16 in struct-field
/// order (year, month, day, hours, minutes, seconds). rcms keeps the wire values,
/// so they serialize directly.
fn write_datetime<W: ProfileWriter>(w: &mut W, d: &DateTime) -> Result<()> {
    w.write_u16(d.year)?;
    w.write_u16(d.month)?;
    w.write_u16(d.day)?;
    w.write_u16(d.hours)?;
    w.write_u16(d.minutes)?;
    w.write_u16(d.seconds)?;
    Ok(())
}

/// `Type_Chromaticity_Write` (cmstypes.c:464): u16 nChannels=3, u16 Table=0, then
/// three `SaveOneChromaticity` (cmstypes.c:455) records — each two s15Fixed16
/// (x, y). The luminance Y is not stored. The reader keeps x/y as f64 (`yy=1.0`),
/// so they re-encode via `_cmsDoubleTo15Fixed16`.
fn write_chromaticity<W: ProfileWriter>(w: &mut W, t: &CIExyYTriple) -> Result<()> {
    w.write_u16(3)?; // nChannels
    w.write_u16(0)?; // Table
    for c in [&t.red, &t.green, &t.blue] {
        w.write_s15fixed16(c.x)?;
        w.write_s15fixed16(c.y)?;
    }
    Ok(())
}

/// `Type_ColorantOrderType_Write` (cmstypes.c:537): a u32 count of the ordering
/// bytes, then the bytes. lcms2 counts the non-`0xFF` entries of a 16-byte array;
/// the rcms reader already trimmed to exactly those `Count` bytes.
fn write_colorant_order<W: ProfileWriter>(w: &mut W, order: &[u8]) -> Result<()> {
    w.write_u32(u32::try_from(order.len()).map_err(|_| Error::Range)?)?;
    w.write_all(order)
}

/// `Type_S15Fixed16_Write` (cmstypes.c:776): each value as `_cmsWrite15Fixed16Number`.
/// The reader keeps the raw fixed bits, so we emit them directly (a re-encode of
/// the f64 would round-trip to the same bits).
fn write_s15f16_array<W: ProfileWriter>(w: &mut W, v: &[crate::fixed::S15Fixed16]) -> Result<()> {
    for &x in v {
        w.write_s15fixed16_raw(x)?;
    }
    Ok(())
}

/// `Type_U16Fixed16_Write` (cmstypes.c:839): each value as
/// `floor(value*65536 + 0.5)` big-endian. The reader keeps the raw u32, so we
/// emit it directly.
fn write_u16f16_array<W: ProfileWriter>(w: &mut W, v: &[crate::fixed::U16Fixed16]) -> Result<()> {
    for &x in v {
        w.write_u16fixed16_raw(x)?;
    }
    Ok(())
}

/// `Type_UInt32_Write` (cmstypes.c:660): each value as a big-endian u32.
fn write_u32_array<W: ProfileWriter>(w: &mut W, v: &[u32]) -> Result<()> {
    for &x in v {
        w.write_u32(x)?;
    }
    Ok(())
}

/// `Type_Measurement_Write` (cmstypes.c:1637): Observer (u32), Backing (XYZ),
/// Geometry (u32), Flare (s15Fixed16), IlluminantType (u32).
fn write_measurement<W: ProfileWriter>(w: &mut W, m: &Measurement) -> Result<()> {
    w.write_u32(m.observer)?;
    write_xyz(w, &m.backing)?;
    w.write_u32(m.geometry)?;
    w.write_s15fixed16(m.flare)?;
    w.write_u32(m.illuminant_type)?;
    Ok(())
}

/// `Type_ViewingConditions_Write` (cmstypes.c:4162): IlluminantXYZ, SurroundXYZ,
/// IlluminantType (u32).
fn write_viewing_conditions<W: ProfileWriter>(w: &mut W, v: &ViewingConditions) -> Result<()> {
    write_xyz(w, &v.illuminant_xyz)?;
    write_xyz(w, &v.surround_xyz)?;
    w.write_u32(v.illuminant_type)?;
    Ok(())
}

/// `Type_ColorantTable_Write` (cmstypes.c:3300): a u32 colorant count, then per
/// colorant a fixed 32-byte ASCII name (zero-padded, force-NUL at index 32) and
/// three big-endian u16 PCS. The reader stores the NUL-trimmed name, so we pad it
/// back to 32 bytes (Latin-1 1:1, matching lcms2's `Root`).
fn write_colorant_table<W: ProfileWriter>(w: &mut W, entries: &[ColorantTableEntry]) -> Result<()> {
    w.write_u32(u32::try_from(entries.len()).map_err(|_| Error::Range)?)?;
    for e in entries {
        let mut name = [0u8; 32];
        for (slot, ch) in name.iter_mut().zip(e.name.chars()) {
            // lcms2 stores `Root` as the raw byte string; names are 7-bit ASCII /
            // Latin-1 1:1. A char beyond 0xFF cannot have come from the reader.
            *slot = u8::try_from(ch as u32).map_err(|_| Error::Range)?;
        }
        w.write_all(&name)?;
        w.write_u16(e.pcs[0])?;
        w.write_u16(e.pcs[1])?;
        w.write_u16(e.pcs[2])?;
    }
    Ok(())
}

/// `Type_VideoSignal_Write` (cmstypes.c:5640): four u8 — ColourPrimaries,
/// TransferCharacteristics, MatrixCoefficients, VideoFullRangeFlag.
fn write_cicp<W: ProfileWriter>(w: &mut W, c: &Cicp) -> Result<()> {
    w.write_u8(c.colour_primaries)?;
    w.write_u8(c.transfer_characteristics)?;
    w.write_u8(c.matrix_coefficients)?;
    w.write_u8(c.video_full_range_flag)?;
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

    // ---- T1: trivial tag writers + DecideType, diff-tested vs lcms2 ----

    use crate::color::{CIExyY, CIExyYTriple};
    use crate::fixed::{S15Fixed16, U16Fixed16};
    use crate::profile::tag::{Cicp, ColorantTableEntry, Measurement, ViewingConditions};

    // `which` selectors mirroring shim.c's `RCMS_T1_*` enum.
    const T1_SIG: i32 = 0;
    const T1_DATA: i32 = 1;
    const T1_DATETIME: i32 = 2;
    const T1_CHROMATICITY: i32 = 3;
    const T1_COLORANT_ORDER: i32 = 4;
    const T1_SF32: i32 = 5;
    const T1_MEASUREMENT: i32 = 6;
    const T1_VIEWING: i32 = 7;
    const T1_COLORANT_TABLE: i32 = 8;
    const T1_CICP: i32 = 9;
    const T1_XYZ_LUMI: i32 = 10;

    // Tag signatures the single-tag profiles use (lcms2 `cmsTagSignature`).
    const TECHNOLOGY: Signature = Signature::from_bytes(*b"tech");
    const PS2CRD0: Signature = Signature::from_bytes(*b"psd0");
    const DATETIME_TAG: Signature = Signature::from_bytes(*b"dtim");
    const CHROMATICITY_TAG: Signature = Signature::from_bytes(*b"chrm");
    const COLORANT_ORDER_TAG: Signature = Signature::from_bytes(*b"clro");
    const CHAD_TAG: Signature = Signature::from_bytes(*b"chad");
    const MEASUREMENT_TAG: Signature = Signature::from_bytes(*b"meas");
    const VIEWING_TAG: Signature = Signature::from_bytes(*b"view");
    const COLORANT_TABLE_TAG: Signature = Signature::from_bytes(*b"clrt");
    const CICP_TAG: Signature = Signature::from_bytes(*b"cicp");
    const LUMINANCE_TAG: Signature = Signature::from_bytes(*b"lumi");

    /// Build the rcms single-tag profile for selector `which`, with the exact
    /// values shim.c uses, so the two serializations must be byte-identical.
    fn build_single(which: i32) -> WritableProfile {
        let mut p = WritableProfile::new(basic_header());
        match which {
            T1_SIG => {
                p.add_tag(TECHNOLOGY, Tag::Signature(Signature::from_raw(0x6D6E_7472)));
            }
            T1_DATA => {
                p.add_tag(
                    PS2CRD0,
                    Tag::Data {
                        flag: 1,
                        data: vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42],
                    },
                );
            }
            T1_DATETIME => {
                p.add_tag(
                    DATETIME_TAG,
                    Tag::DateTime(DateTime {
                        year: 2030,
                        month: 11,
                        day: 23,
                        hours: 7,
                        minutes: 8,
                        seconds: 9,
                    }),
                );
            }
            T1_CHROMATICITY => {
                let xy = |x, y| CIExyY { x, y, yy: 1.0 };
                p.add_tag(
                    CHROMATICITY_TAG,
                    Tag::Chromaticity(CIExyYTriple {
                        red: xy(0.640, 0.330),
                        green: xy(0.300, 0.600),
                        blue: xy(0.150, 0.060),
                    }),
                );
            }
            T1_COLORANT_ORDER => {
                p.add_tag(COLORANT_ORDER_TAG, Tag::ColorantOrder(vec![3, 0, 1, 2]));
            }
            T1_SF32 => {
                let vals = [
                    1.0478, 0.0229, -0.0501, 0.0296, 0.9905, -0.0171, -0.0092, 0.0151, 0.7517,
                ];
                p.add_tag(
                    CHAD_TAG,
                    Tag::S15Fixed16Array(vals.iter().map(|&v| S15Fixed16::from_f64(v)).collect()),
                );
            }
            T1_MEASUREMENT => {
                p.add_tag(
                    MEASUREMENT_TAG,
                    Tag::Measurement(Measurement {
                        observer: 1,
                        backing: CIEXYZ {
                            x: 0.0,
                            y: 0.0,
                            z: 0.0,
                        },
                        geometry: 1,
                        flare: 0.01,
                        illuminant_type: 3,
                    }),
                );
            }
            T1_VIEWING => {
                p.add_tag(
                    VIEWING_TAG,
                    Tag::ViewingConditions(ViewingConditions {
                        illuminant_xyz: CIEXYZ {
                            x: 0.9642,
                            y: 1.0,
                            z: 0.8249,
                        },
                        surround_xyz: CIEXYZ {
                            x: 0.5,
                            y: 0.6,
                            z: 0.7,
                        },
                        illuminant_type: 1,
                    }),
                );
            }
            T1_COLORANT_TABLE => {
                p.add_tag(
                    COLORANT_TABLE_TAG,
                    Tag::ColorantTable(vec![
                        ColorantTableEntry {
                            name: "Cyan".to_string(),
                            pcs: [0x1111, 0x2222, 0x3333],
                        },
                        ColorantTableEntry {
                            name: "Magenta".to_string(),
                            pcs: [0x4444, 0x5555, 0x6666],
                        },
                        ColorantTableEntry {
                            name: "Yellow".to_string(),
                            pcs: [0x7777, 0x8888, 0x9999],
                        },
                    ]),
                );
            }
            T1_CICP => {
                p.add_tag(
                    CICP_TAG,
                    Tag::Cicp(Cicp {
                        colour_primaries: 9,
                        transfer_characteristics: 16,
                        matrix_coefficients: 9,
                        video_full_range_flag: 1,
                    }),
                );
            }
            T1_XYZ_LUMI => {
                p.add_tag(LUMINANCE_TAG, xyz(80.0, 100.0, 90.0));
            }
            other => panic!("unknown single-tag selector {other}"),
        }
        p
    }

    /// Serialize the rcms single-tag profile and assert it is byte-identical to
    /// lcms2's `cmsSaveProfileToMem` over the same constructed profile.
    fn assert_single_tag_identical(which: i32, label: &str) {
        let rust = save_to_mem(&build_single(which)).expect("rcms serialize");
        let c = rcms_oracle::save_single_tag(which).expect("lcms2 serialize");
        assert_eq!(
            rust.len(),
            c.len(),
            "{label}: length mismatch rcms={} lcms2={}",
            rust.len(),
            c.len()
        );
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            panic!(
                "{label}: byte mismatch at {first:?}\n rcms={:02x?}\n lcms={:02x?}",
                rust, c
            );
        }
    }

    #[test]
    fn single_tag_signature_byte_identical() {
        assert_single_tag_identical(T1_SIG, "sig");
    }
    #[test]
    fn single_tag_data_byte_identical() {
        assert_single_tag_identical(T1_DATA, "data");
    }
    #[test]
    fn single_tag_datetime_byte_identical() {
        assert_single_tag_identical(T1_DATETIME, "dtim");
    }
    #[test]
    fn single_tag_chromaticity_byte_identical() {
        assert_single_tag_identical(T1_CHROMATICITY, "chrm");
    }
    #[test]
    fn single_tag_colorant_order_byte_identical() {
        assert_single_tag_identical(T1_COLORANT_ORDER, "clro");
    }
    #[test]
    fn single_tag_s15fixed16_array_byte_identical() {
        assert_single_tag_identical(T1_SF32, "sf32");
    }
    #[test]
    fn single_tag_measurement_byte_identical() {
        assert_single_tag_identical(T1_MEASUREMENT, "meas");
    }
    #[test]
    fn single_tag_viewing_conditions_byte_identical() {
        assert_single_tag_identical(T1_VIEWING, "view");
    }
    #[test]
    fn single_tag_colorant_table_byte_identical() {
        assert_single_tag_identical(T1_COLORANT_TABLE, "clrt");
    }
    #[test]
    fn single_tag_cicp_byte_identical() {
        assert_single_tag_identical(T1_CICP, "cicp");
    }

    /// DecideXYZtype: an XYZ-valued tag (luminance) writes the `XYZ ` type and a
    /// 3×s15Fixed16 body, byte-identical to lcms2.
    #[test]
    fn single_tag_xyz_decidexyztype_byte_identical() {
        assert_single_tag_identical(T1_XYZ_LUMI, "lumi/XYZ");
    }

    /// `ui32`/`uf32` have no built-in tag whose default type selects them, so they
    /// can't be exercised through `cmsWriteTag`. Their body layout is unambiguous
    /// (a flat big-endian array), so assert the writer's bytes directly. The
    /// `uf32` raw values are kept exactly by the reader, so the bytes are the raw
    /// fixed words back-to-back.
    #[test]
    fn ui32_and_uf32_body_layout() {
        let mut w = MemWriter::new();
        write_tag_body(&mut w, &Tag::U32Array(vec![1, 0x1020_3040, 0xFFFF_FFFF, 7])).unwrap();
        assert_eq!(
            w.as_bytes(),
            &[
                0, 0, 0, 1, //
                0x10, 0x20, 0x30, 0x40, //
                0xFF, 0xFF, 0xFF, 0xFF, //
                0, 0, 0, 7,
            ]
        );

        let mut w = MemWriter::new();
        // u16Fixed16 raw words = floor(v*65536 + 0.5), kept verbatim by the reader.
        let raws = [0x0001_8000u32, 0x0002_4000, 0x0000_0000]; // 1.5, 2.25, 0.0
        let vals: Vec<U16Fixed16> = raws.iter().map(|&r| U16Fixed16::from_raw(r)).collect();
        write_tag_body(&mut w, &Tag::U16Fixed16Array(vals)).unwrap();
        let mut expect = Vec::new();
        for r in raws {
            expect.extend_from_slice(&r.to_be_bytes());
        }
        assert_eq!(w.as_bytes(), expect.as_slice());
    }

    /// DecideTextType / DecideTextDescType: the copyright tag picks `mluc` at v4
    /// and `text` at v2; the description tags pick `mluc` at v4 and `desc` at v2;
    /// the charTarget tag always picks `text`. (Bodies for mluc/desc are T2; this
    /// asserts the type SELECTION the table makes.)
    #[test]
    fn decide_text_type_selection() {
        let cprt = Signature::from_bytes(*b"cprt");
        let desc = Signature::from_bytes(*b"desc");
        let targ = Signature::from_bytes(*b"targ");
        // v4.4 → mluc for cprt/desc; v2.1 → text/desc.
        assert_eq!(decide_text(cprt, 4.4), SIG_MLUC_TYPE);
        assert_eq!(decide_text(cprt, 2.1), SIG_TEXT_TYPE);
        assert_eq!(decide_text(desc, 4.4), SIG_MLUC_TYPE);
        assert_eq!(decide_text(desc, 2.1), SIG_DESC_TYPE);
        // charTarget is always plain text.
        assert_eq!(decide_text(targ, 4.4), SIG_TEXT_TYPE);
        assert_eq!(decide_text(targ, 2.1), SIG_TEXT_TYPE);
    }

    /// DecideLUTtypeA2B/B2A: v4 picks `mAB `/`mBA ` by direction; v2 picks `mft2`.
    #[test]
    fn decide_lut_type_selection() {
        assert_eq!(decide_lut(TAG_ATOB0, 4.4), SIG_LUT_ATOB_TYPE);
        assert_eq!(decide_lut(TAG_BTOA0, 4.4), SIG_LUT_BTOA_TYPE);
        assert_eq!(decide_lut(TAG_GAMUT, 4.4), SIG_LUT_BTOA_TYPE);
        assert_eq!(decide_lut(TAG_ATOB0, 2.1), SIG_LUT16_TYPE);
    }

    /// `profile_version_float` mirrors `cmsGetProfileVersion`: the BCD header
    /// version decodes to a float (0x04400000 → 4.4, 0x02100000 → 2.1).
    #[test]
    fn profile_version_float_decodes_bcd() {
        assert!((profile_version_float(0x0440_0000) - 4.4).abs() < 1e-9);
        assert!((profile_version_float(0x0210_0000) - 2.1).abs() < 1e-9);
        assert!((profile_version_float(0x0400_0000) - 4.0).abs() < 1e-9);
    }
}
