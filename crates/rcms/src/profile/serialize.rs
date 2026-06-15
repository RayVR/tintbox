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
use crate::curve::ToneCurve;
use crate::error::{Error, Result};
use crate::fixed::U8Fixed8;
use crate::io::{CountWriter, MemWriter, ProfileWriter};
use crate::pipeline::clut::ClutTable;
use crate::pipeline::{Pipeline, Stage};
use crate::profile::header::{DateTime, Header};
use crate::profile::tag::{
    Cicp, ColorantTableEntry, Measurement, Mlu, ProfileIdItem, ProfileSequenceItem, Tag,
    ViewingConditions,
};
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
        let ty = write_type_for(slot.sig, value, version)?;
        counter.write_type_base(ty)?;
        write_tag_body(&mut counter, value, ty, version)?;
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
        let ty = write_type_for(slot.sig, value, version)?;
        w.write_type_base(ty)?;
        write_tag_body(&mut w, value, ty, version)?;
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
const SIG_LUT8_TYPE: Signature = Signature::from_raw(0x6D667431); // 'mft1'
const SIG_LUT16_TYPE: Signature = Signature::from_raw(0x6D667432); // 'mft2'
const SIG_MPE_TYPE: Signature = Signature::from_raw(0x6D706574); // 'mpet'
const SIG_LUT_ATOB_TYPE: Signature = Signature::from_raw(0x6D414220); // 'mAB '
const SIG_LUT_BTOA_TYPE: Signature = Signature::from_raw(0x6D424120); // 'mBA '
const SIG_CURVE_TYPE: Signature = Signature::from_raw(0x6375_7276); // 'curv'
const SIG_PARAMETRIC_TYPE: Signature = Signature::from_raw(0x7061_7261); // 'para'
const SIG_PSEQ_TYPE: Signature = Signature::from_raw(0x7073_6571); // 'pseq'
const SIG_PSID_TYPE: Signature = Signature::from_raw(0x7073_6964); // 'psid'

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
// The DToB*/BToD* tags carry a multiProcessElements pipeline (descriptor
// `SupportedTypes[0] = cmsSigMultiProcessElementType`, no decider).
const TAG_DTOB0: Signature = Signature::from_bytes(*b"D2B0");
const TAG_DTOB1: Signature = Signature::from_bytes(*b"D2B1");
const TAG_DTOB2: Signature = Signature::from_bytes(*b"D2B2");
const TAG_DTOB3: Signature = Signature::from_bytes(*b"D2B3");
const TAG_BTOD0: Signature = Signature::from_bytes(*b"B2D0");
const TAG_BTOD1: Signature = Signature::from_bytes(*b"B2D1");
const TAG_BTOD2: Signature = Signature::from_bytes(*b"B2D2");
const TAG_BTOD3: Signature = Signature::from_bytes(*b"B2D3");

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
pub(crate) fn profile_version_float(version: u32) -> f64 {
    base_to_base(version >> 16, 16, 10) as f64 / 100.0
}

/// The tag-TYPE signature to write for a tag (lcms2 `SaveTags`, cmsio0.c:1458):
/// the `cmsTagDescriptor`'s `DecideType(Version, Data)` if present, else its
/// `SupportedTypes[0]`. The default `SupportedTypes[0]` is determined by the tag's
/// value shape (the rcms reader produces exactly one `Tag` variant per type), but
/// the version-dependent deciders (curv/para, text/mluc/desc, LUT selection) need
/// the tag SIGNATURE plus the profile version. We mirror the descriptor table.
pub(crate) fn write_type_for(sig: Signature, value: &Tag, version: f64) -> Result<Signature> {
    match value {
        // XYZType: rXYZ/gXYZ/bXYZ go through DecideXYZtype (always 'XYZ '); all
        // other XYZ-valued tags (wtpt/bkpt/lumi) default to 'XYZ '.
        Tag::Xyz(_) => Ok(SIG_XYZ_TYPE),

        // TextType vs mluc/desc is decided by the tag signature + version.
        Tag::Text(_) => Ok(decide_text(sig, version)),
        Tag::Mlu(_) => Ok(decide_text(sig, version)),

        // DecideCurveType (cmstypes.c:1436) inspects the curve's segments to pick
        // `curv` vs `para`; the body writers honour the choice.
        Tag::Curve(c) => Ok(decide_curve(c, version)),

        // ProfileSequenceDescType has a fixed type signature ('pseq'); the embedded
        // per-item descriptions select desc/mluc by version inside the writer.
        Tag::ProfileSequenceDesc(_) => Ok(SIG_PSEQ_TYPE),

        // ProfileSequenceIdType has a fixed type signature ('psid'); the embedded
        // per-item descriptions select desc/mluc by version inside the writer.
        Tag::ProfileSequenceId(_) => Ok(SIG_PSID_TYPE),

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

/// lcms2 `DecideCurveType` (cmstypes.c:1438). A curve writes `curv` unless the
/// profile is v4 AND the curve is a single non-inverted ICC parametric segment
/// (`nSegments == 1`, `0 <= Segments[0].Type <= 5`), in which case it writes
/// `para`. A pure tabulated curve (no segments) always writes `curv`.
fn decide_curve(curve: &ToneCurve, version: f64) -> Signature {
    if version < 4.0 {
        return SIG_CURVE_TYPE;
    }
    let segs = curve.segments();
    if segs.len() != 1 {
        return SIG_CURVE_TYPE; // Only 1-segment curves can be parametric.
    }
    // `DecideCurveType` rejects inverted (Type < 0) and non-ICC (Type > 5) curves;
    // only ICC parametric types 1..=5 (and the 0/sampled guard) select `para`.
    if !(0..=5).contains(&segs[0].seg_type) {
        return SIG_CURVE_TYPE;
    }
    SIG_PARAMETRIC_TYPE
}

/// lcms2 `DecideLUTtypeA2B` (cmstypes.c:1840) / `DecideLUTtypeB2A`
/// (cmstypes.c:1854). For v4 the A2B tags write `mAB ` and the B2A/gamut/preview
/// tags write `mBA `; for v2 they write `mft1`/`mft2` by the pipeline's
/// `SaveAs8Bits` flag (resolved in T3). The bodies are T3; this only fixes the
/// type signature so the dispatch table is complete.
fn decide_lut(sig: Signature, version: f64) -> Signature {
    // The DToB*/BToD* tags always serialize as multiProcessElements (no decider).
    if matches!(
        sig,
        TAG_DTOB0
            | TAG_DTOB1
            | TAG_DTOB2
            | TAG_DTOB3
            | TAG_BTOD0
            | TAG_BTOD1
            | TAG_BTOD2
            | TAG_BTOD3
    ) {
        return SIG_MPE_TYPE;
    }
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
fn write_tag_body<W: ProfileWriter>(
    w: &mut W,
    value: &Tag,
    ty: Signature,
    version: f64,
) -> Result<()> {
    match value {
        Tag::Xyz(xyz) => write_xyz(w, xyz),
        // The text-family tags (text/desc/mluc) all decode to one cooked value in
        // lcms2 (a `cmsMLU`); the BODY written is the one `DecideType` picked, so
        // we dispatch on the resolved type signature, building the missing
        // representation (a single ASCII string ↔ a one-entry MLU) as lcms2's
        // public API does.
        Tag::Text(s) => write_text_family(w, ty, &Mlu::from_ascii(s)),
        Tag::Mlu(m) => write_text_family(w, ty, m),
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
        Tag::Curve(c) => write_curve(w, ty, c),
        Tag::Vcgt(curves) => write_vcgt(w, curves),
        Tag::ProfileSequenceDesc(items) => write_pseq(w, items, version),
        Tag::ProfileSequenceId(items) => write_psid(w, items, version),
        Tag::Lut(lut) => write_lut(w, ty, lut),
        _ => Err(Error::Unsupported("tag type writer not yet implemented")),
    }
}

/// Dispatch a LUT/pipeline body by the type `DecideLUTtype{A2B,B2A}` selected:
/// `mft1` (LUT8), `mft2` (LUT16), `mAB ` (LutAtoB), `mBA ` (LutBtoA), or `mpet`
/// (multi-process elements). Each writer disassembles the parsed [`Pipeline`]
/// back into the on-disk structure exactly as its `Type_*_Write` does.
fn write_lut<W: ProfileWriter>(w: &mut W, ty: Signature, lut: &Pipeline) -> Result<()> {
    match ty {
        SIG_LUT8_TYPE => write_lut8(w, lut),
        SIG_LUT16_TYPE => write_lut16(w, lut),
        SIG_LUT_ATOB_TYPE => write_lut_a2b(w, lut),
        SIG_LUT_BTOA_TYPE => write_lut_b2a(w, lut),
        SIG_MPE_TYPE => write_mpe(w, lut),
        _ => Err(Error::Unsupported("unexpected LUT type signature")),
    }
}

/// Dispatch a text-family body by the type `DecideType` selected: `text` writes a
/// plain ASCII string, `desc` the legacy `textDescription` layout, `mluc` the
/// multi-localized pool. All three derive from the one cooked MLU value, matching
/// how lcms2 holds every text-family tag as a single `cmsMLU`.
fn write_text_family<W: ProfileWriter>(w: &mut W, ty: Signature, mlu: &Mlu) -> Result<()> {
    match ty {
        SIG_TEXT_TYPE => write_text(w, &mlu.preferred_ascii()),
        SIG_DESC_TYPE => write_text_description(w, mlu),
        SIG_MLUC_TYPE => write_mlu(w, mlu),
        _ => Err(Error::Unsupported("unexpected text-family type signature")),
    }
}

/// Dispatch a curve body by the type `DecideCurveType` selected: `curv` writes the
/// gamma special case or the tabulated table; `para` the parametric form.
fn write_curve<W: ProfileWriter>(w: &mut W, ty: Signature, curve: &ToneCurve) -> Result<()> {
    match ty {
        SIG_CURVE_TYPE => write_curve_curv(w, curve),
        SIG_PARAMETRIC_TYPE => write_curve_para(w, curve),
        _ => Err(Error::Unsupported("unexpected curve type signature")),
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

/// `Type_Curve_Write` (cmstypes.c:1387), the `curv` form. The GAMMA special case
/// fires when the curve is a single ICC type-1 parametric segment: write
/// `count = 1` then one u16 holding the gamma as a `cmsU8Fixed8Number`
/// (`_cmsDoubleTo8Fixed8`). Otherwise write `nEntries` (u32) followed by the
/// 16-bit approximation table — `Type_Curve_Write` always uses `Table16`, even
/// for a multi-segment curve (the table is the materialized approximation).
fn write_curve_curv<W: ProfileWriter>(w: &mut W, curve: &ToneCurve) -> Result<()> {
    let segs = curve.segments();
    if segs.len() == 1 && segs[0].seg_type == 1 {
        // Single gamma, preserve number (cmstypes.c:1389-1396).
        let gamma = U8Fixed8::from_f64(segs[0].params[0]);
        w.write_u32(1)?;
        w.write_u16(gamma.to_raw())?;
        return Ok(());
    }
    let table = curve.table16();
    w.write_u32(u32::try_from(table.len()).map_err(|_| Error::Range)?)?;
    for &v in table {
        w.write_u16(v)?;
    }
    Ok(())
}

/// lcms2 `ParamsByType` for `Type_ParametricCurve_Write` (cmstypes.c:1490),
/// indexed by the lcms2 segment type (ICC type + 1): type 1→1 param, 2→3, 3→4,
/// 4→5, 5→7. Index 0 is unused (a type-0/sampled segment never reaches here).
const PARAMETRIC_PARAMS_BY_TYPE: [usize; 6] = [0, 1, 3, 4, 5, 7];

/// `Type_ParametricCurve_Write` (cmstypes.c:1486), the `para` form. Write the ICC
/// parametric type (`Segments[0].Type - 1`) as a u16, a reserved u16 of 0, then
/// `ParamsByType[type]` parameters as s15Fixed16. Only single-segment,
/// non-inverted ICC types (1..=5) reach here (guaranteed by `DecideCurveType`).
fn write_curve_para<W: ProfileWriter>(w: &mut W, curve: &ToneCurve) -> Result<()> {
    let segs = curve.segments();
    if segs.len() != 1 {
        return Err(Error::Unsupported("multisegment parametric curve"));
    }
    let typen = segs[0].seg_type;
    if !(1..=5).contains(&typen) {
        return Err(Error::Unsupported("unsupported parametric curve type"));
    }
    let n_params = PARAMETRIC_PARAMS_BY_TYPE[typen as usize];
    w.write_u16(u16::try_from(typen - 1).map_err(|_| Error::Range)?)?;
    w.write_u16(0)?; // reserved
    for &p in &segs[0].params[..n_params] {
        w.write_s15fixed16(p)?;
    }
    Ok(())
}

/// lcms2 `cmsVideoCardGammaTableType` / `cmsVideoCardGammaFormulaType` selectors
/// (cmstypes.c:4929-4930), the leading u32 of a `vcgt` body.
const VCGT_TABLE_TYPE: u32 = 0;
const VCGT_FORMULA_TYPE: u32 = 1;

/// `Type_vcgt_Write` (cmstypes.c:5079). The three R/G/B tone curves write as the
/// FORMULA variant iff all three are lcms2 parametric type 5 (the form the
/// `Type_vcgt_Read` formula path builds): a `cmsVideoCardGammaFormulaType` tag of
/// three `(Gamma, Min, Max)` s15Fixed16 triples, with `Gamma = Params[0]`,
/// `Min = Params[5]`, `Max = pow(Params[1], Gamma) + Min`. Otherwise it writes the
/// TABLE variant: `cmsVideoCardGammaTableType`, `nChannels=3`, `nEntries=256`,
/// `nBytes=2`, then each channel's 256 words sampled via
/// `_cmsQuickSaturateWord(cmsEvalToneCurveFloat(c, j/255) * 65535)`.
fn write_vcgt<W: ProfileWriter>(w: &mut W, curves: &[ToneCurve]) -> Result<()> {
    if curves.len() != 3 {
        return Err(Error::Unsupported("vcgt must have exactly 3 curves"));
    }
    let all_type5 = curves.iter().all(|c| {
        let segs = c.segments();
        segs.len() == 1 && segs[0].seg_type == 5
    });

    if all_type5 {
        w.write_u32(VCGT_FORMULA_TYPE)?;
        for c in curves {
            let p = &c.segments()[0].params;
            let gamma = p[0];
            let min = p[5];
            let max = p[1].powf(gamma) + min;
            w.write_s15fixed16(gamma)?;
            w.write_s15fixed16(min)?;
            w.write_s15fixed16(max)?;
        }
        return Ok(());
    }

    // Always store as a table of 256 words per channel.
    use crate::compat::floor::{FloorStrategy, Lcms2Floor};
    w.write_u32(VCGT_TABLE_TYPE)?;
    w.write_u16(3)?; // nChannels
    w.write_u16(256)?; // nEntries
    w.write_u16(2)?; // nBytes
    for c in curves {
        for j in 0..256u32 {
            let v = c.eval_float(j as f32 / 255.0);
            let n = Lcms2Floor::quick_saturate_word(v as f64 * 65535.0);
            w.write_u16(n)?;
        }
    }
    Ok(())
}

/// `Type_MLU_Write` (cmstypes.c:1772): a u32 used-entry count, a u32 record size
/// (always 12), one `(lang, country, len, offset)` record per entry, then the
/// UTF-16BE string pool. lcms2 stores `Len`/`StrW` in host `wchar_t` units in
/// memory and converts back to u16 units on write — the width cancels, so we
/// build the pool directly from the entries' code-unit sequences. `len` is the
/// entry's byte length (`code_units * 2`); `offset` is `cumulative_units * 2 +
/// HeaderSize + 8` with `HeaderSize = 12 * UsedEntries + 8` (cmstypes.c:1788).
/// An empty entry stores a single `0x0000` unit (lcms2 forces one wide NUL).
fn write_mlu<W: ProfileWriter>(w: &mut W, mlu: &Mlu) -> Result<()> {
    let count = mlu.entries.len();
    w.write_u32(u32::try_from(count).map_err(|_| Error::Range)?)?;
    w.write_u32(12)?; // record size

    let header_size = 12u32 * u32::try_from(count).map_err(|_| Error::Range)? + 8;

    // Pre-encode each entry's UTF-16 code units; an empty string is a single 0.
    let units: Vec<Vec<u16>> = mlu
        .entries
        .iter()
        .map(|e| {
            let v: Vec<u16> = e.text.encode_utf16().collect();
            if v.is_empty() {
                vec![0]
            } else {
                v
            }
        })
        .collect();

    let mut cumulative: u32 = 0; // u16 units written into the pool so far.
    for (e, u) in mlu.entries.iter().zip(&units) {
        let len_bytes = u32::try_from(u.len() * 2).map_err(|_| Error::Range)?;
        let offset = cumulative * 2 + header_size + 8;
        w.write_u16(u16::from_be_bytes(e.language))?;
        w.write_u16(u16::from_be_bytes(e.country))?;
        w.write_u32(len_bytes)?;
        w.write_u32(offset)?;
        cumulative += u32::try_from(u.len()).map_err(|_| Error::Range)?;
    }

    // The UTF-16BE pool, entries back-to-back.
    for u in &units {
        for &unit in u {
            w.write_u16(unit)?;
        }
    }
    Ok(())
}

/// `Type_Text_Description_Write` (cmstypes.c:1198), the legacy v2 `desc` layout:
/// `len_text` u32 (ASCII length incl. NUL); the ASCII body; a u32 unicode-lang
/// (0); a u32 unicode-count (= `len_text`); the UTF-16BE unicode body of
/// `len_text` units; a u16 scriptcode (0); a u8 mac count (0); a 67-byte mac
/// buffer; then the writer's OWN alignment pad to `_cmsALIGNLONG` of the full tag
/// requirement (which includes the 8-byte type base). lcms2 derives the ASCII via
/// `cmsMLUgetASCII(cmsNoLanguage, cmsNoCountry)` and the wide via
/// `cmsMLUgetWide(cmsV2Unicode, cmsV2Unicode)`, both clipped to `len_text` units.
fn write_text_description<W: ProfileWriter>(w: &mut W, mlu: &Mlu) -> Result<()> {
    // ASCII representation (cmsNoLanguage/cmsNoCountry select).
    let ascii = mlu.preferred_ascii();
    // strlen(Text)+1: stop at the first embedded NUL, then add the terminator.
    let ascii_body: &str = ascii.split('\0').next().unwrap_or("");
    let len_text = u32::try_from(ascii_body.len() + 1).map_err(|_| Error::Range)?;

    // Wide representation (cmsV2Unicode select), clipped/zero-filled to len_text
    // code units (lcms2 calloc's a len_text buffer, copies the wide string, and
    // writes exactly len_text units — the tail is the NUL terminator + zeros).
    let wide_src: Vec<u16> = mlu
        .select([0xff, 0xff], [0xff, 0xff])
        .map(|e| e.text.encode_utf16().collect())
        .unwrap_or_default();
    let mut wide = vec![0u16; len_text as usize];
    let n = wide.len().saturating_sub(1).min(wide_src.len());
    wide[..n].copy_from_slice(&wide_src[..n]);

    // count + ascii body (with NUL terminator).
    w.write_u32(len_text)?;
    w.write_all(ascii_body.as_bytes())?;
    w.write_u8(0)?;

    // unicode language code (0) + count + body.
    w.write_u32(0)?;
    w.write_u32(len_text)?;
    for &unit in &wide {
        w.write_u16(unit)?;
    }

    // ScriptCode code (u16) + count (u8) + 67-byte buffer, all zero.
    w.write_u16(0)?;
    w.write_u8(0)?;
    w.write_all(&[0u8; 67])?;

    // lcms2's own end-of-tag pad: _cmsALIGNLONG(len_tag_requirement) where the
    // requirement INCLUDES the 8-byte type base (cmstypes.c:1249-1251). The outer
    // serializer also aligns, but this internal pad fires when the ASCII count is
    // not 4-aligned and must be reproduced for byte-identity.
    let len_tag_requirement = 8 + 4 + len_text + 4 + 4 + 2 * len_text + 2 + 1 + 67;
    let len_aligned = (len_tag_requirement + 3) & !3u32;
    for _ in 0..(len_aligned - len_tag_requirement) {
        w.write_u8(0)?;
    }
    Ok(())
}

/// `Type_ProfileSequenceDesc_Write` (cmstypes.c:3608): a u32 record count, then per
/// item deviceMfg (u32), deviceModel (u32), attributes (u64), technology (u32),
/// and the two embedded descriptions (manufacturer, model) via `SaveDescription`.
/// `SaveDescription` (cmstypes.c:3596) writes an 8-byte type base then the desc
/// (`Type_Text_Description_Write`) or mluc (`Type_MLU_Write`) body, chosen by the
/// profile version (v2 → `desc`, v4 → `mluc`).
fn write_pseq<W: ProfileWriter>(
    w: &mut W,
    items: &[ProfileSequenceItem],
    version: f64,
) -> Result<()> {
    w.write_u32(u32::try_from(items.len()).map_err(|_| Error::Range)?)?;
    for item in items {
        w.write_u32(item.device_mfg.to_raw())?;
        w.write_u32(item.device_model.to_raw())?;
        w.write_u64(item.attributes)?;
        w.write_u32(item.technology.to_raw())?;
        write_embedded_description(w, &item.manufacturer, version)?;
        write_embedded_description(w, &item.model, version)?;
    }
    Ok(())
}

/// lcms2 `SaveDescription` (cmstypes.c:3596): write the embedded text-description
/// type base (`desc` at v2, `mluc` at v4) followed by the matching body.
fn write_embedded_description<W: ProfileWriter>(w: &mut W, mlu: &Mlu, version: f64) -> Result<()> {
    if version < 4.0 {
        w.write_type_base(SIG_DESC_TYPE)?;
        write_text_description(w, mlu)
    } else {
        w.write_type_base(SIG_MLUC_TYPE)?;
        write_mlu(w, mlu)
    }
}

/// `Type_ProfileSequenceId_Write` (cmstypes.c:3739) via `WritePositionTable`
/// (cmstypes.c:273) + `WriteSeqID` (cmstypes.c:3719): a u32 record count, then a
/// per-item position directory of `(offset, size)` pairs (offsets are
/// type-base-relative), then each item — a 16-byte raw profile ID followed by the
/// item's `SaveDescription` (`desc` at v2, `mluc` at v4). No per-item alignment
/// padding is applied (matching `WriteSeqID`), so the directory back-patch uses
/// the exact element sizes.
fn write_psid<W: ProfileWriter>(w: &mut W, items: &[ProfileIdItem], version: f64) -> Result<()> {
    // BaseOffset = Tell - sizeof(_cmsTagBase): the 8-byte type base precedes us.
    let base_offset = w.position() - 8;
    let count = items.len();

    w.write_u32(u32::try_from(count).map_err(|_| Error::Range)?)?;

    // Placeholder position directory: (offset, size) per item.
    let directory_pos = w.position();
    for _ in 0..count {
        w.write_u32(0)?;
        w.write_u32(0)?;
    }

    let mut offsets = vec![0u32; count];
    let mut sizes = vec![0u32; count];
    for (i, item) in items.iter().enumerate() {
        offsets[i] = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        let before = w.position();
        w.write_all(&item.profile_id)?;
        write_embedded_description(w, &item.description, version)?;
        sizes[i] = u32::try_from(w.position() - before).map_err(|_| Error::Range)?;
    }

    for i in 0..count {
        w.patch_u32(directory_pos + i * 8, offsets[i])?;
        w.patch_u32(directory_pos + i * 8 + 4, sizes[i])?;
    }
    Ok(())
}

// ---- T3: LUT / MPE tag-body writers (cmstypes.c) ----

/// lcms2 `FROM_16_TO_8(rgb)` (lcms2_internal.h:126): the round-to-8-bit reduction
/// `((rgb * 65281 + 8388608) >> 24) & 0xFF`.
fn from_16_to_8(v: u16) -> u8 {
    (((v as u32 * 65281 + 8_388_608) >> 24) & 0xFF) as u8
}

/// lcms2 `Write8bitTables` (cmstypes.c:1936): for each of `n` channels, write a
/// 256-entry 8-bit table. The identity special case — a 2-entry table `{0,
/// 65535}` — is written as the linear ramp `0..256`; otherwise the curve MUST
/// have exactly 256 entries (lcms2 errors otherwise), each reduced via
/// `FROM_16_TO_8`. `curves` is `None` for a missing stage (the C `Tables ==
/// NULL`), in which case nothing is written.
fn write_8bit_tables<W: ProfileWriter>(
    w: &mut W,
    n: usize,
    curves: Option<&[ToneCurve]>,
) -> Result<()> {
    let Some(curves) = curves else {
        return Ok(());
    };
    for curve in curves.iter().take(n) {
        let table = curve.table16();
        if table.len() == 2 && table[0] == 0 && table[1] == 65535 {
            // Usual identity curve: the literal ramp j = 0..256.
            for j in 0..256u32 {
                w.write_u8(j as u8)?;
            }
        } else if table.len() != 256 {
            return Err(Error::Unsupported(
                "LUT8 needs 256 entries on linearization",
            ));
        } else {
            for &v in table {
                w.write_u8(from_16_to_8(v))?;
            }
        }
    }
    Ok(())
}

/// lcms2 `Write16bitTables` (cmstypes.c:2283): write every channel's full
/// `nEntries` u16 table verbatim, back-to-back.
fn write_16bit_tables<W: ProfileWriter>(w: &mut W, curves: &[ToneCurve]) -> Result<()> {
    for curve in curves {
        for &v in curve.table16() {
            w.write_u16(v)?;
        }
    }
    Ok(())
}

/// Borrow the curve set of a [`Stage::ToneCurves`], or `None` for any other stage.
fn as_tone_curves(stage: &Stage) -> Option<&[ToneCurve]> {
    match stage {
        Stage::ToneCurves(curves) => Some(curves),
        _ => None,
    }
}

/// A borrowed [`Stage::Matrix`] disassembled into `(rows, cols, m, offset)` — the
/// shape the LUT/MPE matrix-block writers consume.
type MatrixRef<'a> = (usize, usize, &'a [f64], Option<&'a [f64]>);

/// Borrow `(rows, cols, m, offset)` of a [`Stage::Matrix`], or `None` otherwise.
fn as_matrix(stage: &Stage) -> Option<MatrixRef<'_>> {
    match stage {
        Stage::Matrix {
            rows,
            cols,
            m,
            offset,
        } => Some((*rows, *cols, m, offset.as_deref())),
        _ => None,
    }
}

/// Borrow the CLUT of a [`Stage::Clut`], or `None` otherwise.
fn as_clut(stage: &Stage) -> Option<&crate::pipeline::Clut> {
    match stage {
        Stage::Clut(c) => Some(c),
        _ => None,
    }
}

/// lcms2 `Type_LUT8_Write` (cmstypes.c:2105). Disassemble the pipeline into the
/// fixed `[Matrix?] -> PreCurves? -> CLUT? -> PostCurves?` shape, then write the
/// LUT8 header (in/out/grid/pad), a 9-entry s15Fixed16 matrix (identity when
/// absent), the always-256-entry 8-bit input tables, the 8-bit CLUT, and the
/// 256-entry 8-bit output tables.
fn write_lut8<W: ProfileWriter>(w: &mut W, lut: &Pipeline) -> Result<()> {
    let (mat, pre, clut, post) = disassemble_lut(lut)?;

    let input_channels = lut.input_channels;
    let output_channels = lut.output_channels;

    // Lut8 requires uniform CLUT grid; clutPoints = nSamples[0] (0 when no CLUT).
    let clut_points = uniform_clut_points(clut, input_channels)?;

    w.write_u8(u8::try_from(input_channels).map_err(|_| Error::Range)?)?;
    w.write_u8(u8::try_from(output_channels).map_err(|_| Error::Range)?)?;
    w.write_u8(u8::try_from(clut_points).map_err(|_| Error::Range)?)?;
    w.write_u8(0)?; // padding

    write_lut_matrix(w, mat)?;

    write_8bit_tables(w, input_channels, pre)?;

    if let Some(clut) = clut {
        if clut_points > 0 {
            let table = clut_u16(clut)?;
            for &v in table {
                w.write_u8(from_16_to_8(v))?;
            }
        }
    }

    write_8bit_tables(w, output_channels, post)?;
    Ok(())
}

/// lcms2 `Type_LUT16_Write` (cmstypes.c:2399). Like [`write_lut8`] but writes the
/// input/output table lengths (the curves' `nEntries`, default 2 when a stage is
/// absent) and 16-bit tables/CLUT. A missing pre/post curve set is written as the
/// default identity `{0, 0xFFFF}` ramp per channel.
fn write_lut16<W: ProfileWriter>(w: &mut W, lut: &Pipeline) -> Result<()> {
    let (mat, pre, clut, post) = disassemble_lut(lut)?;

    let input_channels = lut.input_channels;
    let output_channels = lut.output_channels;
    let clut_points = uniform_clut_points(clut, input_channels)?;

    w.write_u8(u8::try_from(input_channels).map_err(|_| Error::Range)?)?;
    w.write_u8(u8::try_from(output_channels).map_err(|_| Error::Range)?)?;
    w.write_u8(u8::try_from(clut_points).map_err(|_| Error::Range)?)?;
    w.write_u8(0)?; // padding

    write_lut_matrix(w, mat)?;

    // Input/output table entry counts: the curve's nEntries, or 2 when absent.
    let in_entries = pre.map(|c| c[0].table16().len()).unwrap_or(2);
    let out_entries = post.map(|c| c[0].table16().len()).unwrap_or(2);
    w.write_u16(u16::try_from(in_entries).map_err(|_| Error::Range)?)?;
    w.write_u16(u16::try_from(out_entries).map_err(|_| Error::Range)?)?;

    // Prelinearization tables (default identity {0, 0xFFFF} per channel).
    match pre {
        Some(curves) => write_16bit_tables(w, curves)?,
        None => {
            for _ in 0..input_channels {
                w.write_u16(0)?;
                w.write_u16(0xFFFF)?;
            }
        }
    }

    if let Some(clut) = clut {
        if clut_points > 0 {
            let table = clut_u16(clut)?;
            for &v in table {
                w.write_u16(v)?;
            }
        }
    }

    match post {
        Some(curves) => write_16bit_tables(w, curves)?,
        None => {
            for _ in 0..output_channels {
                w.write_u16(0)?;
                w.write_u16(0xFFFF)?;
            }
        }
    }
    Ok(())
}

/// Disassemble a LUT8/LUT16 pipeline into `(matrix?, pre-curves?, clut?,
/// post-curves?)`, mirroring the C walk: an optional leading 3x3 `Matrix`, an
/// optional `ToneCurves`, an optional `Clut`, an optional `ToneCurves`. Any other
/// shape is rejected (lcms2 "LUT is not suitable to be saved as LUT8/16").
#[allow(clippy::type_complexity)]
fn disassemble_lut(
    lut: &Pipeline,
) -> Result<(
    Option<MatrixRef<'_>>,
    Option<&[ToneCurve]>,
    Option<&crate::pipeline::Clut>,
    Option<&[ToneCurve]>,
)> {
    let stages = lut.stages();
    let mut i = 0;
    let mut mat = None;
    let mut pre = None;
    let mut clut = None;
    let mut post = None;

    if i < stages.len() {
        if let Some((rows, cols, m, off)) = as_matrix(&stages[i]) {
            if rows != 3 || cols != 3 {
                return Err(Error::Unsupported("LUT matrix is not 3x3"));
            }
            mat = Some((rows, cols, m, off));
            i += 1;
        }
    }
    if i < stages.len() {
        if let Some(c) = as_tone_curves(&stages[i]) {
            pre = Some(c);
            i += 1;
        }
    }
    if i < stages.len() {
        if let Some(c) = as_clut(&stages[i]) {
            clut = Some(c);
            i += 1;
        }
    }
    if i < stages.len() {
        if let Some(c) = as_tone_curves(&stages[i]) {
            post = Some(c);
            i += 1;
        }
    }
    if i != stages.len() {
        return Err(Error::Unsupported(
            "LUT not suitable to be saved as mft1/mft2",
        ));
    }
    Ok((mat, pre, clut, post))
}

/// The uniform CLUT grid-point count for LUT8/LUT16 (`clut->Params->nSamples[0]`),
/// or 0 when there is no CLUT. lcms2 rejects a CLUT whose per-dimension sample
/// counts are not all equal (it cannot encode a non-uniform grid in mft1/mft2).
fn uniform_clut_points(clut: Option<&crate::pipeline::Clut>, input_channels: usize) -> Result<u32> {
    let Some(clut) = clut else {
        return Ok(0);
    };
    let n = &clut.params.n_samples;
    let points = n[0];
    if n.iter().take(input_channels).skip(1).any(|&s| s != points) {
        return Err(Error::Unsupported(
            "LUT with non-uniform CLUT samples cannot be saved as mft1/mft2",
        ));
    }
    Ok(points)
}

/// Borrow a CLUT's 16-bit table, rejecting a float CLUT (mft1/mft2/mAB/mBA store
/// 8- or 16-bit samples only — lcms2 `WriteCLUT` "Cannot save floating point").
fn clut_u16(clut: &crate::pipeline::Clut) -> Result<&[u16]> {
    match &clut.table {
        ClutTable::U16(t) => Ok(t),
        ClutTable::F32(_) => Err(Error::Unsupported(
            "cannot save a float CLUT as mft/mAB/mBA",
        )),
    }
}

/// Write the 9-entry s15Fixed16 LUT8/LUT16 matrix, or the identity matrix when no
/// matrix stage is present (cmstypes.c:2168-2189 / 2461-2482).
fn write_lut_matrix<W: ProfileWriter>(w: &mut W, mat: Option<MatrixRef<'_>>) -> Result<()> {
    match mat {
        Some((_, _, m, _)) => {
            for &v in &m[..9] {
                w.write_s15fixed16(v)?;
            }
        }
        None => {
            const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
            for v in IDENTITY {
                w.write_s15fixed16(v)?;
            }
        }
    }
    Ok(())
}

/// lcms2 `WriteMatrix` (cmstypes.c:2812): the `InputChannels * OutputChannels`
/// s15Fixed16 matrix entries, then `OutputChannels` offset entries (zeros when the
/// stage carries no offset).
fn write_matrix_block<W: ProfileWriter>(
    w: &mut W,
    rows: usize,
    cols: usize,
    m: &[f64],
    offset: Option<&[f64]>,
) -> Result<()> {
    for &v in &m[..rows * cols] {
        w.write_s15fixed16(v)?;
    }
    match offset {
        Some(off) => {
            for &v in &off[..rows] {
                w.write_s15fixed16(v)?;
            }
        }
        None => {
            for _ in 0..rows {
                w.write_s15fixed16(0.0)?;
            }
        }
    }
    Ok(())
}

/// lcms2 `WriteSetOfCurves` (cmstypes.c:2849): write each of the stage's curves as
/// `type-base + body + alignment`. The base type is `para`, downgraded to `curv`
/// for a tabulated curve (no segments), a float-tabulated curve (3 segments with a
/// sampled middle), or an inverted first segment (`Type < 0`).
fn write_set_of_curves<W: ProfileWriter>(w: &mut W, curves: &[ToneCurve]) -> Result<()> {
    for curve in curves {
        let segs = curve.segments();
        let use_curv = segs.is_empty()
            || (segs.len() == 3 && segs[1].seg_type == 0)
            || (!segs.is_empty() && segs[0].seg_type < 0);
        if use_curv {
            w.write_type_base(SIG_CURVE_TYPE)?;
            write_curve_curv(w, curve)?;
        } else {
            w.write_type_base(SIG_PARAMETRIC_TYPE)?;
            write_curve_para(w, curve)?;
        }
        w.write_alignment()?;
    }
    Ok(())
}

/// lcms2 `WriteCLUT` (cmstypes.c:2902): the 16-byte `gridPoints` array (per-input
/// sample count, zero-padded), a 1-byte precision (1 = 8-bit, 2 = 16-bit) + 3 pad,
/// the CLUT samples at that precision, then alignment. `precision` is the parsed
/// pipeline's `SaveAs8Bits` choice — rcms always stores a 16-bit table, so the
/// reader-produced pipelines round-trip at precision 2.
fn write_clut_block<W: ProfileWriter>(
    w: &mut W,
    clut: &crate::pipeline::Clut,
    precision: u8,
) -> Result<()> {
    let table = clut_u16(clut)?;

    let mut grid_points = [0u8; 16];
    for (slot, &n) in grid_points
        .iter_mut()
        .zip(clut.params.n_samples.iter())
        .take(clut.params.n_inputs)
    {
        *slot = u8::try_from(n).map_err(|_| Error::Range)?;
    }
    w.write_all(&grid_points)?;

    w.write_u8(precision)?;
    w.write_u8(0)?;
    w.write_u8(0)?;
    w.write_u8(0)?;

    match precision {
        1 => {
            for &v in table {
                w.write_u8(from_16_to_8(v))?;
            }
        }
        2 => {
            for &v in table {
                w.write_u16(v)?;
            }
        }
        _ => return Err(Error::Unsupported("unknown CLUT precision")),
    }

    w.write_alignment()
}

/// lcms2 `Type_LUTA2B_Write` (cmstypes.c:2953). Disassemble the pipeline into the
/// `{A, CLUT, M, Matrix, B}` roles (per the accepted A2B stage sequences), write
/// the in/out channel header + a u16 pad, a five-entry placeholder offset
/// directory, then the present blocks in `A, CLUT, M, Matrix, B` order recording
/// each block's type-base-relative offset, and finally back-patch the directory
/// (field order `B, Matrix, M, CLUT, A`).
fn write_lut_a2b<W: ProfileWriter>(w: &mut W, lut: &Pipeline) -> Result<()> {
    let roles = disassemble_a2b(lut)?;
    write_mab_mba(w, lut, &roles)
}

/// lcms2 `Type_LUTB2A_Write` (cmstypes.c:3141). The B2A mirror of [`write_lut_a2b`]
/// (the directory layout and block write order are identical; only the
/// stage→role mapping differs, handled by [`disassemble_b2a`]).
fn write_lut_b2a<W: ProfileWriter>(w: &mut W, lut: &Pipeline) -> Result<()> {
    let roles = disassemble_b2a(lut)?;
    write_mab_mba(w, lut, &roles)
}

/// The five optional blocks of an mAB/mBA tag, by role.
#[derive(Default)]
struct MabRoles<'a> {
    a: Option<&'a [ToneCurve]>,
    clut: Option<&'a crate::pipeline::Clut>,
    m: Option<&'a [ToneCurve]>,
    matrix: Option<MatrixRef<'a>>,
    b: Option<&'a [ToneCurve]>,
}

/// Disassemble an A2B pipeline into `{A, CLUT, M, Matrix, B}` per the accepted
/// stage sequences (cmstypes.c:2963-2972): `[B]`, `[M,Matrix,B]`, `[A,CLUT,B]`, or
/// `[A,CLUT,M,Matrix,B]`. Any other shape is "not suitable to be saved as
/// LutAToB".
fn disassemble_a2b(lut: &Pipeline) -> Result<MabRoles<'_>> {
    let s = lut.stages();
    let mut r = MabRoles::default();
    match s.len() {
        1 => {
            r.b = as_tone_curves(&s[0]);
            check(r.b.is_some())?;
        }
        3 => {
            // M, Matrix, B  OR  A, CLUT, B
            if let Some(matrix) = as_matrix(&s[1]) {
                r.m = as_tone_curves(&s[0]);
                r.matrix = Some(matrix);
                r.b = as_tone_curves(&s[2]);
                check(r.m.is_some() && r.b.is_some())?;
            } else {
                r.a = as_tone_curves(&s[0]);
                r.clut = as_clut(&s[1]);
                r.b = as_tone_curves(&s[2]);
                check(r.a.is_some() && r.clut.is_some() && r.b.is_some())?;
            }
        }
        5 => {
            r.a = as_tone_curves(&s[0]);
            r.clut = as_clut(&s[1]);
            r.m = as_tone_curves(&s[2]);
            r.matrix = as_matrix(&s[3]);
            r.b = as_tone_curves(&s[4]);
            check(
                r.a.is_some()
                    && r.clut.is_some()
                    && r.m.is_some()
                    && r.matrix.is_some()
                    && r.b.is_some(),
            )?;
        }
        _ => {
            return Err(Error::Unsupported(
                "LUT not suitable to be saved as LutAToB",
            ))
        }
    }
    Ok(r)
}

/// Disassemble a B2A pipeline into `{A, CLUT, M, Matrix, B}` per the accepted
/// stage sequences (cmstypes.c:3151-3160): `[B]`, `[B,Matrix,M]`, `[B,CLUT,A]`, or
/// `[B,Matrix,M,CLUT,A]`. Note the reversed roles (B leads).
fn disassemble_b2a(lut: &Pipeline) -> Result<MabRoles<'_>> {
    let s = lut.stages();
    let mut r = MabRoles::default();
    match s.len() {
        1 => {
            r.b = as_tone_curves(&s[0]);
            check(r.b.is_some())?;
        }
        3 => {
            // B, Matrix, M  OR  B, CLUT, A
            if let Some(matrix) = as_matrix(&s[1]) {
                r.b = as_tone_curves(&s[0]);
                r.matrix = Some(matrix);
                r.m = as_tone_curves(&s[2]);
                check(r.b.is_some() && r.m.is_some())?;
            } else {
                r.b = as_tone_curves(&s[0]);
                r.clut = as_clut(&s[1]);
                r.a = as_tone_curves(&s[2]);
                check(r.b.is_some() && r.clut.is_some() && r.a.is_some())?;
            }
        }
        5 => {
            r.b = as_tone_curves(&s[0]);
            r.matrix = as_matrix(&s[1]);
            r.m = as_tone_curves(&s[2]);
            r.clut = as_clut(&s[3]);
            r.a = as_tone_curves(&s[4]);
            check(
                r.b.is_some()
                    && r.matrix.is_some()
                    && r.m.is_some()
                    && r.clut.is_some()
                    && r.a.is_some(),
            )?;
        }
        _ => {
            return Err(Error::Unsupported(
                "LUT not suitable to be saved as LutBToA",
            ))
        }
    }
    Ok(r)
}

fn check(ok: bool) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Unsupported("LUT stage shape mismatch"))
    }
}

/// Shared mAB/mBA body writer (the two `Type_LUT{A2B,B2A}_Write` bodies are
/// byte-identical once stages are mapped to roles). Writes in/out channels + a
/// u16 pad, a five-u32 placeholder directory, the present blocks in
/// `A, CLUT, M, Matrix, B` order (recording each type-base-relative offset), then
/// back-patches the directory in `B, Matrix, M, CLUT, A` order.
fn write_mab_mba<W: ProfileWriter>(w: &mut W, lut: &Pipeline, roles: &MabRoles) -> Result<()> {
    // BaseOffset = Tell - sizeof(_cmsTagBase): the type base was written just
    // before this body, so the current position is BaseOffset + 8.
    let base_offset = w.position() - 8;

    let input_channels = lut.input_channels;
    let output_channels = lut.output_channels;

    w.write_u8(u8::try_from(input_channels).map_err(|_| Error::Range)?)?;
    w.write_u8(u8::try_from(output_channels).map_err(|_| Error::Range)?)?;
    w.write_u16(0)?; // pad

    // Placeholder directory (offsetB, offsetMat, offsetM, offsetC, offsetA).
    let directory_pos = w.position();
    for _ in 0..5 {
        w.write_u32(0)?;
    }

    let mut offset_a = 0u32;
    let mut offset_c = 0u32;
    let mut offset_m = 0u32;
    let mut offset_mat = 0u32;
    let mut offset_b = 0u32;

    if let Some(a) = roles.a {
        offset_a = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        write_set_of_curves(w, a)?;
    }
    if let Some(clut) = roles.clut {
        offset_c = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        // rcms stores a 16-bit table; reader-produced pipelines re-save at
        // precision 2 (lcms2's SaveAs8Bits is never set when reading mAB/mBA).
        write_clut_block(w, clut, 2)?;
    }
    if let Some(m) = roles.m {
        offset_m = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        write_set_of_curves(w, m)?;
    }
    if let Some((rows, cols, m, off)) = roles.matrix {
        offset_mat = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        write_matrix_block(w, rows, cols, m, off)?;
    }
    if let Some(b) = roles.b {
        offset_b = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        write_set_of_curves(w, b)?;
    }

    // Back-patch the directory (field order B, Matrix, M, CLUT, A).
    w.patch_u32(directory_pos, offset_b)?;
    w.patch_u32(directory_pos + 4, offset_mat)?;
    w.patch_u32(directory_pos + 8, offset_m)?;
    w.patch_u32(directory_pos + 12, offset_c)?;
    w.patch_u32(directory_pos + 16, offset_a)?;
    Ok(())
}

/// lcms2 `Type_MPE_Write` (cmstypes.c:4811). Writes in/out channels + a u32 element
/// count, a per-element placeholder position table, each element (8-byte element
/// base + body + alignment) recording its type-base-relative offset and size, then
/// back-patches the position table.
fn write_mpe<W: ProfileWriter>(w: &mut W, lut: &Pipeline) -> Result<()> {
    let base_offset = w.position() - 8;
    let stages = lut.stages();
    let elem_count = stages.len();

    w.write_u16(u16::try_from(lut.input_channels).map_err(|_| Error::Range)?)?;
    w.write_u16(u16::try_from(lut.output_channels).map_err(|_| Error::Range)?)?;
    w.write_u32(u32::try_from(elem_count).map_err(|_| Error::Range)?)?;

    // Placeholder directory: (offset, size) per element.
    let directory_pos = w.position();
    for _ in 0..elem_count {
        w.write_u32(0)?; // offset
        w.write_u32(0)?; // size
    }

    let mut offsets = vec![0u32; elem_count];
    let mut sizes = vec![0u32; elem_count];

    for (i, stage) in stages.iter().enumerate() {
        offsets[i] = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        let before = w.position();
        let sig = mpe_element_sig(stage)?;
        w.write_u32(sig.to_raw())?;
        w.write_u32(0)?; // reserved
        write_mpe_element(w, stage)?;
        w.write_alignment()?;
        sizes[i] = u32::try_from(w.position() - before).map_err(|_| Error::Range)?;
    }

    for i in 0..elem_count {
        w.patch_u32(directory_pos + i * 8, offsets[i])?;
        w.patch_u32(directory_pos + i * 8 + 4, sizes[i])?;
    }
    Ok(())
}

/// The MPE element type signature for a stage (lcms2 `Elem->Type`):
/// `ToneCurves` → `cvst`, `Matrix` → `matf`, `Clut` → `clut`.
fn mpe_element_sig(stage: &Stage) -> Result<Signature> {
    match stage {
        Stage::ToneCurves(_) => Ok(SIG_MPE_CURVE_ELEM),
        Stage::Matrix { .. } => Ok(SIG_MPE_MATRIX_ELEM),
        Stage::Clut(_) => Ok(SIG_MPE_CLUT_ELEM),
        _ => Err(Error::Unsupported(
            "stage cannot be saved as an MPE element",
        )),
    }
}

/// Write one MPE element body (after its 8-byte element base): a curve-set
/// (`Type_MPEcurve_Write`), a matrix (`Type_MPEmatrix_Write`), or a float CLUT
/// (`Type_MPEclut_Write`).
fn write_mpe_element<W: ProfileWriter>(w: &mut W, stage: &Stage) -> Result<()> {
    match stage {
        Stage::ToneCurves(curves) => write_mpe_curve(w, curves),
        Stage::Matrix {
            rows,
            cols,
            m,
            offset,
        } => write_mpe_matrix(w, *rows, *cols, m, offset.as_deref()),
        Stage::Clut(clut) => write_mpe_clut(w, clut),
        _ => Err(Error::Unsupported(
            "stage cannot be saved as an MPE element",
        )),
    }
}

/// MPE element type signatures (`cmsSig{CurveSet,Matrix,CLut}ElemType`).
const SIG_MPE_CURVE_ELEM: Signature = Signature::from_raw(0x6376_7374); // 'cvst'
const SIG_MPE_MATRIX_ELEM: Signature = Signature::from_raw(0x6D61_7466); // 'matf'
const SIG_MPE_CLUT_ELEM: Signature = Signature::from_raw(0x636C_7574); // 'clut'

/// MPE segmented-curve on-disk signatures.
const SIG_MPE_SEGMENTED_CURVE: u32 = 0x6375_7266; // 'curf'
const SIG_MPE_FORMULA_SEG: u32 = 0x7061_7266; // 'parf'
const SIG_MPE_SAMPLED_SEG: u32 = 0x7361_6D66; // 'samf'

/// lcms2 `Type_MPEcurve_Write` (cmstypes.c:4486): InputChans (== OutputChans for a
/// curve set), then a position table of one segmented curve per channel.
fn write_mpe_curve<W: ProfileWriter>(w: &mut W, curves: &[ToneCurve]) -> Result<()> {
    let base_offset = w.position() - 8;
    let n = curves.len();

    w.write_u16(u16::try_from(n).map_err(|_| Error::Range)?)?;
    w.write_u16(u16::try_from(n).map_err(|_| Error::Range)?)?;

    let directory_pos = w.position();
    for _ in 0..n {
        w.write_u32(0)?; // offset
        w.write_u32(0)?; // size
    }

    let mut offsets = vec![0u32; n];
    let mut sizes = vec![0u32; n];
    for (i, curve) in curves.iter().enumerate() {
        offsets[i] = u32::try_from(w.position() - base_offset).map_err(|_| Error::Range)?;
        let before = w.position();
        write_segmented_curve(w, curve)?;
        sizes[i] = u32::try_from(w.position() - before).map_err(|_| Error::Range)?;
    }

    for i in 0..n {
        w.patch_u32(directory_pos + i * 8, offsets[i])?;
        w.patch_u32(directory_pos + i * 8 + 4, sizes[i])?;
    }
    Ok(())
}

/// lcms2 `WriteSegmentedCurve` (cmstypes.c:4405): the `cmsSigSegmentedCurve`
/// wrapper, a reserved word, nSegments (u16), a reserved word, the `nSegments - 1`
/// float32 break-points (each segment's `x1`), then each segment. A sampled
/// segment is `samf` plus a count plus the `nGridPoints - 1` points after the
/// implicit first; a formula segment is `parf` plus a type plus the type's
/// float32 params. lcms2 stores `Segments[i].Type` as `ICCtype + 6` for formula
/// segments, written back here as `Type - 6`.
fn write_segmented_curve<W: ProfileWriter>(w: &mut W, curve: &ToneCurve) -> Result<()> {
    let segs = curve.segments();
    let n = segs.len();
    if n == 0 {
        return Err(Error::Unsupported(
            "MPE segmented curve must have >= 1 segment",
        ));
    }

    w.write_u32(SIG_MPE_SEGMENTED_CURVE)?;
    w.write_u32(0)?; // reserved
    w.write_u16(u16::try_from(n).map_err(|_| Error::Range)?)?;
    w.write_u16(0)?; // reserved

    // Break-points: Segments[i].x1 for i in 0..n-1.
    for seg in &segs[..n - 1] {
        w.write_f32(seg.x1)?;
    }

    const PARAMS_BY_TYPE: [usize; 3] = [4, 5, 5];
    for seg in segs {
        if seg.seg_type == 0 {
            // Sampled segment: first point implicit, write count-1 then the rest.
            let n_grid = seg.sampled.len();
            w.write_u32(SIG_MPE_SAMPLED_SEG)?;
            w.write_u32(0)?; // reserved
            w.write_u32(
                u32::try_from(n_grid)
                    .map_err(|_| Error::Range)?
                    .saturating_sub(1),
            )?;
            for &p in seg.sampled.iter().skip(1) {
                w.write_f32(p)?;
            }
        } else {
            // Formula segment: only ICC types 0..2 (stored type 6..8) are allowed.
            let ty = seg.seg_type - 6;
            if !(0..=2).contains(&ty) {
                return Err(Error::Unsupported("MPE formula segment type out of range"));
            }
            w.write_u32(SIG_MPE_FORMULA_SEG)?;
            w.write_u32(0)?; // reserved
            w.write_u16(u16::try_from(ty).map_err(|_| Error::Range)?)?;
            w.write_u16(0)?; // reserved
            for &p in &seg.params[..PARAMS_BY_TYPE[ty as usize]] {
                w.write_f32(p as f32)?;
            }
        }
    }
    Ok(())
}

/// lcms2 `Type_MPEmatrix_Write` (cmstypes.c:4580): InputChans (u16), OutputChans
/// (u16), the `InputChans * OutputChans` float32 matrix entries, then `OutputChans`
/// float32 offsets (zeros when absent). The rcms `Matrix` stage is `cols -> rows`,
/// so InputChans = cols and OutputChans = rows.
fn write_mpe_matrix<W: ProfileWriter>(
    w: &mut W,
    rows: usize,
    cols: usize,
    m: &[f64],
    offset: Option<&[f64]>,
) -> Result<()> {
    w.write_u16(u16::try_from(cols).map_err(|_| Error::Range)?)?; // InputChannels
    w.write_u16(u16::try_from(rows).map_err(|_| Error::Range)?)?; // OutputChannels

    for &v in &m[..rows * cols] {
        w.write_f32(v as f32)?;
    }
    for i in 0..rows {
        match offset {
            Some(off) => w.write_f32(off[i] as f32)?,
            None => w.write_f32(0.0)?,
        }
    }
    Ok(())
}

/// lcms2 `Type_MPEclut_Write` (cmstypes.c:4665): InputChans (u16), OutputChans
/// (u16), a 16-byte dimensions array (per-input sample count, zero-padded), then
/// the float32 CLUT samples. MPE CLUTs are float-only (rcms `ClutTable::F32`).
fn write_mpe_clut<W: ProfileWriter>(w: &mut W, clut: &crate::pipeline::Clut) -> Result<()> {
    let ClutTable::F32(table) = &clut.table else {
        return Err(Error::Unsupported("MPE CLUT must be float"));
    };

    w.write_u16(u16::try_from(clut.params.n_inputs).map_err(|_| Error::Range)?)?;
    w.write_u16(u16::try_from(clut.params.n_outputs).map_err(|_| Error::Range)?)?;

    let mut dims = [0u8; 16];
    for (slot, &n) in dims
        .iter_mut()
        .zip(clut.params.n_samples.iter())
        .take(clut.params.n_inputs)
    {
        *slot = u8::try_from(n).map_err(|_| Error::Range)?;
    }
    w.write_all(&dims)?;

    for &v in table {
        w.write_f32(v)?;
    }
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
        write_tag_body(
            &mut w,
            &Tag::U32Array(vec![1, 0x1020_3040, 0xFFFF_FFFF, 7]),
            SIG_UINT32_TYPE,
            4.4,
        )
        .unwrap();
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
        write_tag_body(&mut w, &Tag::U16Fixed16Array(vals), SIG_U16F16_TYPE, 4.4).unwrap();
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

    // ---- T2: curv/para + mluc/desc (+pseq) writers, diff-tested vs lcms2 ----

    use crate::curve::{build_gamma, build_parametric, build_tabulated_16};
    use crate::profile::tag::{Mlu, MluEntry, ProfileSequenceItem};

    // `which` selectors mirroring shim.c's `RCMS_T2_*` enum.
    const T2_CURV_GAMMA_V2: i32 = 0;
    const T2_CURV_TABLE_V2: i32 = 1;
    const T2_CURV_TABLE_V4: i32 = 2;
    const T2_PARA_GAMMA_V4: i32 = 3;
    const T2_PARA_TYPE1_V4: i32 = 4;
    const T2_PARA_TYPE2_V4: i32 = 5;
    const T2_PARA_TYPE3_V4: i32 = 6;
    const T2_PARA_TYPE4_V4: i32 = 7;
    const T2_MLUC_V4: i32 = 8;
    const T2_DESC_V2: i32 = 9;
    const T2_PSEQ_V4: i32 = 10;
    const T2_PSEQ_V2: i32 = 11;

    const RED_TRC: Signature = Signature::from_bytes(*b"rTRC");
    const CPRT: Signature = Signature::from_bytes(*b"cprt");
    const DESC_TAG: Signature = Signature::from_bytes(*b"desc");
    const PSEQ_TAG: Signature = Signature::from_bytes(*b"pseq");

    /// `basic_header` re-versioned (the serializer ignores `size`/`illuminant`;
    /// only `version` matters for the DecideType deciders).
    fn header_versioned(version: u32) -> Header {
        Header {
            version,
            ..basic_header()
        }
    }

    fn mlu_entry(lang: &[u8; 2], country: &[u8; 2], text: &str) -> MluEntry {
        MluEntry {
            language: *lang,
            country: *country,
            text: text.to_string(),
        }
    }

    /// Build the rcms single-tag profile for T2 selector `which`, matching the
    /// structure shim.c constructs (same curve/MLU/pseq values + profile version).
    fn build_t2(which: i32) -> WritableProfile {
        match which {
            T2_CURV_GAMMA_V2 => {
                let mut p = WritableProfile::new(header_versioned(0x0210_0000));
                p.add_tag(RED_TRC, Tag::Curve(build_gamma(2.4)));
                p
            }
            T2_CURV_TABLE_V2 | T2_CURV_TABLE_V4 => {
                let version = if which == T2_CURV_TABLE_V2 {
                    0x0210_0000
                } else {
                    0x0440_0000
                };
                let mut p = WritableProfile::new(header_versioned(version));
                let tbl = [0u16, 0x3000, 0x7000, 0xB000, 0xFFFF];
                p.add_tag(RED_TRC, Tag::Curve(build_tabulated_16(&tbl)));
                p
            }
            T2_PARA_GAMMA_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                p.add_tag(RED_TRC, Tag::Curve(build_gamma(2.4)));
                p
            }
            T2_PARA_TYPE1_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                let c = build_parametric(2, &[2.4, 0.9, 0.1]).unwrap();
                p.add_tag(RED_TRC, Tag::Curve(c));
                p
            }
            T2_PARA_TYPE2_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                let c = build_parametric(3, &[2.4, 0.9, 0.1, 0.05]).unwrap();
                p.add_tag(RED_TRC, Tag::Curve(c));
                p
            }
            T2_PARA_TYPE3_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                let c =
                    build_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])
                        .unwrap();
                p.add_tag(RED_TRC, Tag::Curve(c));
                p
            }
            T2_PARA_TYPE4_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                let c = build_parametric(
                    5,
                    &[
                        2.4,
                        1.0 / 1.055,
                        0.055 / 1.055,
                        1.0 / 12.92,
                        0.04045,
                        0.1,
                        0.2,
                    ],
                )
                .unwrap();
                p.add_tag(RED_TRC, Tag::Curve(c));
                p
            }
            T2_MLUC_V4 => {
                let mut p = WritableProfile::new(header_versioned(0x0440_0000));
                let mlu = Mlu {
                    entries: vec![
                        mlu_entry(b"en", b"US", "Hello"),
                        mlu_entry(b"de", b"DE", "Gr\u{00fc}\u{00df}e"),
                        mlu_entry(b"ja", b"JP", "\u{65e5}\u{672c}\u{8a9e}"),
                    ],
                };
                p.add_tag(CPRT, Tag::Mlu(mlu));
                p
            }
            T2_DESC_V2 => {
                let mut p = WritableProfile::new(header_versioned(0x0210_0000));
                // lcms2 holds a single cmsNoLanguage/cmsNoCountry ASCII entry.
                let mlu = Mlu {
                    entries: vec![mlu_entry(&[0, 0], &[0, 0], "rcms desc test")],
                };
                p.add_tag(DESC_TAG, Tag::Mlu(mlu));
                p
            }
            T2_PSEQ_V4 | T2_PSEQ_V2 => {
                let version = if which == T2_PSEQ_V4 {
                    0x0440_0000
                } else {
                    0x0210_0000
                };
                let mut p = WritableProfile::new(header_versioned(version));
                let mk = |i: u32, mfg: &str, model: &str| ProfileSequenceItem {
                    device_mfg: Signature::from_raw(0x4D46_4731 + i),
                    device_model: Signature::from_raw(0x4D4F_4431 + i),
                    attributes: u64::from(i + 1),
                    technology: Signature::from_raw(0x6D6E_7472),
                    manufacturer: Mlu {
                        entries: vec![mlu_entry(&[0, 0], &[0, 0], mfg)],
                    },
                    model: Mlu {
                        entries: vec![mlu_entry(&[0, 0], &[0, 0], model)],
                    },
                };
                p.add_tag(
                    PSEQ_TAG,
                    Tag::ProfileSequenceDesc(vec![
                        mk(0, "MakerOne", "ModelOne"),
                        mk(1, "MakerTwo", "ModelTwo"),
                    ]),
                );
                p
            }
            other => panic!("unknown T2 selector {other}"),
        }
    }

    fn assert_t2_identical(which: i32, label: &str) {
        let rust = save_to_mem(&build_t2(which)).expect("rcms serialize");
        let c = rcms_oracle::save_curve_mlu_tag(which).expect("lcms2 serialize");
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
    fn curve_gamma_v2_curv_byte_identical() {
        assert_t2_identical(T2_CURV_GAMMA_V2, "curv/gamma v2");
    }
    #[test]
    fn curve_table_v2_curv_byte_identical() {
        assert_t2_identical(T2_CURV_TABLE_V2, "curv/table v2");
    }
    #[test]
    fn curve_table_v4_curv_byte_identical() {
        assert_t2_identical(T2_CURV_TABLE_V4, "curv/table v4");
    }
    #[test]
    fn curve_gamma_v4_para_byte_identical() {
        assert_t2_identical(T2_PARA_GAMMA_V4, "para/gamma v4");
    }
    #[test]
    fn para_type1_byte_identical() {
        assert_t2_identical(T2_PARA_TYPE1_V4, "para type1");
    }
    #[test]
    fn para_type2_byte_identical() {
        assert_t2_identical(T2_PARA_TYPE2_V4, "para type2");
    }
    #[test]
    fn para_type3_byte_identical() {
        assert_t2_identical(T2_PARA_TYPE3_V4, "para type3");
    }
    #[test]
    fn para_type4_byte_identical() {
        assert_t2_identical(T2_PARA_TYPE4_V4, "para type4");
    }
    #[test]
    fn mluc_multilang_byte_identical() {
        assert_t2_identical(T2_MLUC_V4, "mluc multilang");
    }
    #[test]
    fn desc_v2_byte_identical() {
        assert_t2_identical(T2_DESC_V2, "desc v2");
    }
    #[test]
    fn pseq_v4_byte_identical() {
        assert_t2_identical(T2_PSEQ_V4, "pseq v4 (mluc embeds)");
    }
    #[test]
    fn pseq_v2_byte_identical() {
        assert_t2_identical(T2_PSEQ_V2, "pseq v2 (desc embeds)");
    }

    /// DecideCurveType: v2 always `curv`; v4 single non-inverted ICC parametric
    /// (type 1..5) → `para`, multi-segment / tabulated → `curv`.
    #[test]
    fn decide_curve_type_selection() {
        let gamma = build_gamma(2.2);
        let table = build_tabulated_16(&[0, 0x8000, 0xFFFF]);
        assert_eq!(decide_curve(&gamma, 2.1), SIG_CURVE_TYPE);
        assert_eq!(decide_curve(&gamma, 4.4), SIG_PARAMETRIC_TYPE);
        assert_eq!(decide_curve(&table, 4.4), SIG_CURVE_TYPE);
        let para5 = build_parametric(5, &[2.4, 0.9, 0.1, 0.05, 0.1, 0.2, 0.3]).unwrap();
        assert_eq!(decide_curve(&para5, 4.4), SIG_PARAMETRIC_TYPE);
    }

    // ---- T3: mft1/mft2/mAB/mBA/mpet writers, diff-tested vs lcms2 ----

    use crate::profile::Profile;

    /// Load a testbed profile's bytes, read its `src_sig` pipeline tag, build a
    /// `WritableProfile` (deterministic header at `version`) carrying that parsed
    /// pipeline under `dst_sig`, serialize it, and assert byte-equality with lcms2
    /// reading the SAME tag and re-serializing the same parsed pipeline. This
    /// isolates the LUT body writers: both sides serialize the parsed structure
    /// (lcms2 via `cmsWriteTag`, NOT a raw disk copy).
    fn assert_lut_resave_identical(
        testbed: &str,
        src_sig: Signature,
        dst_sig: Signature,
        version: u32,
        save_as_8bit: bool,
        label: &str,
    ) {
        let path = format!(
            "{}/../../vendor/Little-CMS/testbed/{testbed}.icc",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{label}: read {path}: {e}"));

        let prof = Profile::open(&bytes).expect("open testbed");
        let tag = prof.read_tag(src_sig).expect("read LUT tag");
        let lut = match tag {
            Tag::Lut(p) => p,
            other => panic!("{label}: expected Lut tag, got {other:?}"),
        };

        let mut p = WritableProfile::new(header_versioned(version));
        p.add_tag(dst_sig, Tag::Lut(lut));
        let rust = save_to_mem(&p).expect("rcms serialize");

        let version_f = profile_version_float(version);
        let c = rcms_oracle::resave_lut_tag(
            &bytes,
            src_sig.to_raw(),
            dst_sig.to_raw(),
            version_f,
            save_as_8bit,
        )
        .expect("lcms2 resave");

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
                "{label}: byte mismatch at {first:?}\n rcms[..first+16]={:02x?}\n lcms[..first+16]={:02x?}",
                &rust[..rust.len().min(first.unwrap_or(0) + 16)],
                &c[..c.len().min(first.unwrap_or(0) + 16)],
            );
        }
    }

    const A2B0: Signature = Signature::from_bytes(*b"A2B0");
    const B2A0: Signature = Signature::from_bytes(*b"B2A0");
    const V2: u32 = 0x0230_0000;
    const V4: u32 = 0x0440_0000;

    /// `mft2` (LUT16): the test1 A2B0 16-bit LUT (matrix + curves + CLUT), parsed
    /// and re-serialized at v2 (so DecideLUTtype picks mft2).
    #[test]
    fn lut16_mft2_byte_identical() {
        assert_lut_resave_identical("test1", A2B0, A2B0, V2, false, "mft2 test1/A2B0");
    }

    /// `mft2` again over a different profile, to exercise larger CLUT tables.
    #[test]
    fn lut16_mft2_test2_byte_identical() {
        assert_lut_resave_identical("test2", A2B0, A2B0, V2, false, "mft2 test2/A2B0");
    }

    /// Slice the body of the single tag from a serialized profile (after its
    /// 8-byte type base, length = directory `size` - 8). The profile carries
    /// exactly one tag (the deterministic header + one LUT tag), so the directory
    /// has one entry at offset 132.
    fn slice_only_tag_body(profile: &[u8]) -> &[u8] {
        let count = u32::from_be_bytes(profile[128..132].try_into().unwrap());
        assert_eq!(count, 1, "expected a single-tag profile");
        let offset = u32::from_be_bytes(profile[132 + 4..132 + 8].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(profile[132 + 8..132 + 12].try_into().unwrap()) as usize;
        // Body = the tag bytes minus the 8-byte type base.
        &profile[offset + 8..offset + size]
    }

    /// `mft1` (LUT8) BODY: the test3 A2B0 LUT8, written by the rcms LUT8 body
    /// writer, compared to the LUT8 body lcms2 emits when re-serializing the same
    /// parsed pipeline with `SaveAs8Bits` forced on. rcms's `Tag::Lut` carries no
    /// `SaveAs8Bits`, so the whole-profile dispatch never selects mft1 (it would
    /// pick mft2 at v2); we drive the body writer directly and diff the body
    /// bytes against lcms2's forced-8-bit serialization.
    fn assert_lut8_body_identical(testbed: &str, src_sig: Signature, label: &str) {
        let path = format!(
            "{}/../../vendor/Little-CMS/testbed/{testbed}.icc",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{label}: read {path}: {e}"));
        let prof = Profile::open(&bytes).expect("open testbed");
        let lut = match prof.read_tag(src_sig).expect("read LUT tag") {
            Tag::Lut(p) => p,
            other => panic!("{label}: expected Lut, got {other:?}"),
        };

        let mut w = MemWriter::new();
        write_tag_body(&mut w, &Tag::Lut(lut), SIG_LUT8_TYPE, 2.3).expect("rcms LUT8 body");
        let rust_body = w.as_bytes();

        let c = rcms_oracle::resave_lut_tag(&bytes, src_sig.to_raw(), src_sig.to_raw(), 2.3, true)
            .expect("lcms2 resave (mft1)");
        let c_body = slice_only_tag_body(&c);

        assert_eq!(
            rust_body.len(),
            c_body.len(),
            "{label}: LUT8 body length mismatch rcms={} lcms2={}",
            rust_body.len(),
            c_body.len()
        );
        if rust_body != c_body {
            let first = rust_body.iter().zip(c_body).position(|(a, b)| a != b);
            panic!("{label}: LUT8 body byte mismatch at {first:?}");
        }
    }

    /// `mft1` body over an A2B LUT8 (test3 A2B0).
    #[test]
    fn lut8_mft1_body_byte_identical() {
        assert_lut8_body_identical("test3", A2B0, "mft1 test3/A2B0");
    }

    /// `mft1` body over a B2A LUT8 (test1 B2A0).
    #[test]
    fn lut8_mft1_b2a_body_byte_identical() {
        assert_lut8_body_identical("test1", B2A0, "mft1 test1/B2A0");
    }

    /// `mAB ` (LutAtoB): the test4 A2B0 (curves + matrix + CLUT), re-serialized at
    /// v4 (so DecideLUTtypeA2B picks mAB). Exercises the offset directory +
    /// back-patch, WriteSetOfCurves, WriteMatrix, and WriteCLUT.
    #[test]
    fn lut_a2b_mab_byte_identical() {
        assert_lut_resave_identical("test4", A2B0, A2B0, V4, false, "mAB test4/A2B0");
    }

    /// `mBA ` (LutBtoA): the test4 B2A0, re-serialized at v4 (DecideLUTtypeB2A →
    /// mBA). The B2A role mapping mirrors mAB with reversed stage order.
    #[test]
    fn lut_b2a_mba_byte_identical() {
        assert_lut_resave_identical("test4", B2A0, B2A0, V4, false, "mBA test4/B2A0");
    }

    /// `mpet` (multiProcessElements): a synthetic curve-set → matrix → float-CLUT
    /// pipeline built identically in rcms and lcms2, serialized under DToB0 at v4.
    /// Exercises the position-table back-patch and the cvst/matf/clut element
    /// writers.
    #[test]
    fn mpe_mpet_byte_identical() {
        use crate::curve::{build_mpe_segmented, CurveSegment};
        use crate::interp::InterpParams;
        use crate::pipeline::clut::{Clut, ClutTable};

        const DTOB0: Signature = Signature::from_bytes(*b"D2B0");

        // Curve-set: three identical single-segment formula curves (stored Type 6
        // = on-disk formula type 0: Y = (a*X + b)^g + c), matching the oracle.
        let make_curve = || {
            let mut params = [0.0f64; 10];
            params[0] = 1.0; // g
            params[1] = 1.0; // a
            params[2] = 0.0; // b
            params[3] = 0.0; // c
            build_mpe_segmented(vec![CurveSegment {
                x0: -1e22,
                x1: 1e22,
                seg_type: 6,
                params,
                sampled: Vec::new(),
            }])
        };

        let mut lut = Pipeline::new(3, 3);
        lut.insert_stage_at_end(Stage::ToneCurves(vec![
            make_curve(),
            make_curve(),
            make_curve(),
        ]))
        .unwrap();

        // Matrix 3x3 with offset (same values as the oracle).
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: vec![1.1, 0.0, 0.0, 0.0, 0.9, 0.0, 0.0, 0.0, 1.05],
            offset: Some(vec![0.01, -0.02, 0.03]),
        })
        .unwrap();

        // Float CLUT, 2 points per dimension, 3->3, zero-initialized (NULL data).
        let grid = [2u32, 2, 2];
        let params = InterpParams::new(&grid, 3, 3);
        let n_entries = 3 * 2 * 2 * 2;
        lut.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::F32(vec![0.0f32; n_entries]),
            params,
            is_trilinear: false,
            implements_identity: false,
        }))
        .unwrap();

        let mut p = WritableProfile::new(header_versioned(V4));
        p.add_tag(DTOB0, Tag::Lut(lut));
        let rust = save_to_mem(&p).expect("rcms serialize");

        let c = rcms_oracle::save_mpe_tag().expect("lcms2 mpe serialize");
        assert_eq!(
            rust.len(),
            c.len(),
            "mpet: length mismatch rcms={} lcms2={}",
            rust.len(),
            c.len()
        );
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            panic!("mpet: byte mismatch at {first:?}");
        }
    }
}
