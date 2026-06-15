//! ICC profile parsing. The 128-byte header (`header`), the tag directory
//! (`directory`), and the tag-descriptor table (`descriptor`) compose into
//! `Profile`. Tag *values* (decoded per-type) arrive in later slice-2 tasks.

pub mod descriptor;
pub mod directory;
pub mod header;
pub mod serialize;
pub mod tag;
pub mod types;

pub use directory::TagEntry;
pub use header::{ColorSpace, DateTime, Header, ProfileClass, RenderingIntent};
pub use serialize::{save_to_mem, SlotContent, TagSlot, WritableProfile};
pub use tag::Tag;

use crate::error::{Error, Result};
use crate::io::{MemReader, ProfileReader};
use crate::sig::Signature;
use core::cell::RefCell;
use std::collections::BTreeMap;

/// A parsed ICC profile: the validated header plus the accepted tag directory.
/// Borrows the source bytes so positioned tag reads (Task 3+) can decode values
/// lazily without copying. `cache` is a placeholder for those decoded values.
pub struct Profile<'a> {
    bytes: &'a [u8],
    header: Header,
    dir: Vec<TagEntry>,
    /// Lazy per-tag decoded-value cache, keyed by the *resolved* (link-chased)
    /// tag signature. `read_tag` populates it on first read (lcms2 `TagPtrs`).
    cache: RefCell<BTreeMap<u32, Tag>>,
}

impl<'a> Profile<'a> {
    /// Open a profile from its raw bytes: parse the header (lcms2 `_cmsReadHeader`
    /// header half) then the tag directory (its directory loop + dup check). The
    /// reader is positioned at byte 128 after the header parse, exactly where the
    /// directory begins. Errors propagate from either stage (bad magic/version,
    /// truncation, out-of-range tag count, or a duplicate tag signature).
    pub fn open(bytes: &'a [u8]) -> Result<Profile<'a>> {
        let mut r = MemReader::new(bytes);
        let header = Header::parse(&mut r)?;
        let dir = directory::parse_directory(&mut r, header.size, bytes.len())?;
        Ok(Profile {
            bytes,
            header,
            dir,
            cache: RefCell::new(BTreeMap::new()),
        })
    }

    /// The profile's raw bytes (the slice `open` borrowed).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The validated 128-byte header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The accepted tag signatures, in directory order.
    pub fn tags(&self) -> impl Iterator<Item = Signature> + '_ {
        self.dir.iter().map(|e| e.sig)
    }

    /// Whether the profile carries a tag with the given signature.
    pub fn has_tag(&self, sig: Signature) -> bool {
        self.dir.iter().any(|e| e.sig == sig)
    }

    /// The directory entry for `sig`, if present.
    pub fn tag_entry(&self, sig: Signature) -> Option<&TagEntry> {
        self.dir.iter().find(|e| e.sig == sig)
    }

    /// lcms2 `_cmsSearchTag(Icc, sig, TRUE)` (`cmsio0.c:688-715`): locate the
    /// directory entry for `sig`, following tag links TRANSITIVELY (a link points
    /// at an earlier accepted entry; that entry may itself be a link). Returns the
    /// resolved entry — the one whose offset/size hold the actual data. `None`
    /// when `sig` is absent. We bound the walk by the directory length so a
    /// pathological self-referential chain cannot loop forever (lcms2 relies on
    /// links only ever pointing *backwards*, which the directory builder enforces).
    fn search_tag(&self, sig: Signature) -> Option<&TagEntry> {
        let mut cur = self.tag_entry(sig)?;
        for _ in 0..self.dir.len() {
            match cur.linked {
                Some(linked) => cur = self.tag_entry(linked)?,
                None => return Some(cur),
            }
        }
        Some(cur)
    }

    /// The uncooked tag payload bytes: the `size` bytes at the tag's on-disk
    /// `offset`, links chased to the resolved entry. Includes the 8-byte type
    /// base — this is the raw on-disk tag exactly as lcms2 would read it raw.
    /// Errors if the tag is absent or the byte range falls outside the profile.
    pub fn read_tag_raw(&self, sig: Signature) -> Result<&'a [u8]> {
        let entry = self.search_tag(sig).ok_or(Error::Range)?;
        let off = entry.offset as usize;
        let end = off.checked_add(entry.size as usize).ok_or(Error::Range)?;
        self.bytes.get(off..end).ok_or(Error::Range)
    }

    /// lcms2 `_cmsGetTagTrueType` (`cmsio0.c:1889`): the on-disk tag-TYPE
    /// signature for `sig` — the 8-byte type base lcms2 records while reading the
    /// tag. Links are chased to the resolved entry (matching `read_tag`). Returns
    /// `None` if the tag is absent or its byte range falls outside the profile.
    /// Needed by the V2↔V4 LUT gates, which key on `OriginalType == Lut16Type`.
    pub fn tag_true_type(&self, sig: Signature) -> Option<Signature> {
        let entry = self.search_tag(sig)?;
        let mut r = MemReader::new(self.bytes);
        r.seek(entry.offset as u64).ok()?;
        r.read_type_base().ok()
    }

    /// lcms2 `cmsReadTag` (`cmsio0.c:1722-1876`), §7.5 flow, returning the decoded
    /// [`Tag`] (a clone of the cached value).
    ///
    /// 1. `_cmsSearchTag(sig, TRUE)` — chase links transitively to the resolved
    ///    entry. Absent → `Error::Range`.
    /// 2. On-disk `TagSize < 8` → corruption error.
    /// 3. Read the 8-byte type base; confirm the type sig is one of the original
    ///    `sig`'s descriptor `allowed_types` (else `Error::BadType`, matching
    ///    lcms2's `IsTypeSupported`).
    /// 4. The handler receives `TagSize - 8` bytes; dispatch on the type.
    /// 5. Validate the decoded element count `>= descriptor.elem_count`
    ///    (`cmsio0.c:1852`); else corruption.
    /// 6. Cache and return.
    pub fn read_tag(&self, sig: Signature) -> Result<Tag> {
        // 1. Locate + chase links. The cache is keyed by the resolved signature so
        //    two links to the same data share one entry (lcms2 caches per slot n).
        let entry = *self.search_tag(sig).ok_or(Error::Range)?;
        let resolved = entry.sig;

        if let Some(cached) = self.cache.borrow().get(&resolved.to_raw()) {
            return Ok(cached.clone());
        }

        // The descriptor for the ORIGINAL sig drives the type/elem-count check
        // (lcms2 looks up `_cmsGetTagDescriptor(Icc, sig)`). Unknown tag → error.
        let desc = descriptor::descriptor(sig).ok_or(Error::BadType(sig))?;

        // 2. TagSize < 8 is corruption (cmsio0.c:1772).
        if entry.size < 8 {
            return Err(Error::Corrupt("tag size < 8"));
        }

        // 3. Seek to offset, read the type base, validate against allowed_types.
        let mut r = MemReader::new(self.bytes);
        r.seek(entry.offset as u64)?;
        let type_sig = r.read_type_base()?;
        if !desc.allowed_types.contains(&type_sig) {
            return Err(Error::BadType(type_sig));
        }

        // 4. Dispatch; the handler gets TagSize - 8 payload bytes.
        let payload = entry.size - 8;
        let value = types::read_tag_value(type_sig, &mut r, payload)?;

        // 5. Element-count check (cmsio0.c:1852): the decoded count must be at
        //    least the descriptor's required ElemCount.
        if elem_count(&value) < desc.elem_count {
            return Err(Error::Corrupt("inconsistent element count"));
        }

        // 6. Cache + return.
        self.cache
            .borrow_mut()
            .insert(resolved.to_raw(), value.clone());
        Ok(value)
    }
}

/// The decoded element count lcms2's handler reports via `*nItems`, used for the
/// §7.5 element-count validation (`cmsio0.c:1852`). Mirrors each trivial
/// handler's `*nItems`: array types report their length, scalar/struct types 1.
fn elem_count(tag: &Tag) -> u32 {
    match tag {
        // Array handlers set *nItems = n (the element count).
        Tag::S15Fixed16Array(v) => v.len() as u32,
        Tag::U16Fixed16Array(v) => v.len() as u32,
        Tag::U8Array(v) => v.len() as u32,
        Tag::U32Array(v) => v.len() as u32,
        // Scalar / single-struct handlers set *nItems = 1.
        Tag::Xyz(_)
        | Tag::Signature(_)
        | Tag::Data { .. }
        | Tag::DateTime(_)
        | Tag::Chromaticity(_)
        | Tag::Text(_)
        | Tag::ColorantOrder(_)
        | Tag::Measurement(_)
        | Tag::ViewingConditions(_)
        | Tag::Screening(_)
        | Tag::CrdInfo(_)
        | Tag::Cicp(_)
        | Tag::ColorantTable(_)
        // Both Type_MLU_Read and Type_Text_Description_Read set *nItems = 1.
        | Tag::Mlu(_)
        // NamedColor2 / ProfileSequence{Desc,Id} / Dictionary all set *nItems = 1.
        | Tag::NamedColor2(_)
        | Tag::ProfileSequenceDesc(_)
        | Tag::ProfileSequenceId(_)
        | Tag::Dict(_)
        | Tag::Curve(_)
        // Type_vcgt_Read and Type_UcrBg_Read both set *nItems = 1.
        | Tag::Vcgt(_)
        | Tag::UcrBg { .. }
        // Type_LUT8_Read and Type_LUT16_Read both set *nItems = 1.
        | Tag::Lut(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn testbed_dir() -> PathBuf {
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/Little-CMS/testbed"
        ))
        .to_path_buf()
    }

    fn testbed_icc() -> Vec<PathBuf> {
        let mut v: Vec<_> = fs::read_dir(testbed_dir())
            .expect("read testbed")
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("icc"))
            .collect();
        v.sort();
        v
    }

    /// Differential: `Profile::open` accept/reject decision must agree with full
    /// lcms2 `cmsOpenProfileFromMem` (header + directory + dup check) on every
    /// testbed file, and accepted files must carry the same accepted tag SET.
    #[test]
    fn open_and_tags_match_oracle_over_testbed() {
        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        let mut compared = 0usize;
        let mut both_accept = 0usize;
        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();

            let oracle_ok = rcms_oracle::open_succeeds(&bytes);
            let rust = Profile::open(&bytes);

            assert_eq!(
                rust.is_ok(),
                oracle_ok,
                "open accept/reject disagree on {name}: rust={:?} lcms2={oracle_ok}",
                rust.as_ref().err()
            );

            if oracle_ok {
                both_accept += 1;
                let p = rust.unwrap();
                let rust_set: BTreeSet<u32> = p.tags().map(|s| s.to_raw()).collect();
                let oracle_set: BTreeSet<u32> = rcms_oracle::tag_signatures(&bytes)
                    .expect("oracle tag sigs")
                    .into_iter()
                    .collect();
                assert_eq!(
                    rust_set, oracle_set,
                    "accepted tag set mismatch on {name}\n rust={rust_set:x?}\n lcms2={oracle_set:x?}"
                );
            }
            compared += 1;
        }
        println!(
            "testbed open diff: compared {compared} .icc files, {both_accept} accepted by both"
        );
        assert!(both_accept > 0, "expected at least one accepted profile");
    }

    /// The named malformed files: assert open agreement explicitly (toosmall.icc
    /// is rejected at directory validation by lcms2, and now by rcms too).
    #[test]
    fn malformed_files_agree_with_oracle() {
        for name in ["bad.icc", "bad_mpe.icc", "toosmall.icc"] {
            let path = testbed_dir().join(name);
            if !path.exists() {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            let oracle_ok = rcms_oracle::open_succeeds(&bytes);
            let rust_ok = Profile::open(&bytes).is_ok();
            assert_eq!(rust_ok, oracle_ok, "open disagree on {name}");
        }
    }

    // On-disk tag-TYPE signatures of this task's trivial readers.
    const TY_XYZ: u32 = 0x5859_5A20; // 'XYZ '
    const TY_CORBIS_XYZ: u32 = 0x17A5_05B8;
    const TY_S15F16: u32 = 0x7366_3332; // 'sf32'
    const TY_U16F16: u32 = 0x7566_3332; // 'uf32'
    const TY_UI08: u32 = 0x7569_3038; // 'ui08'
    const TY_UI32: u32 = 0x7569_3332; // 'ui32'
    const TY_SIG: u32 = 0x7369_6720; // 'sig '
    const TY_DATA: u32 = 0x6461_7461; // 'data'
    const TY_DTIM: u32 = 0x6474_696D; // 'dtim'
    const TY_CHRM: u32 = 0x6368_726D; // 'chrm'
    const TY_TEXT: u32 = 0x7465_7874; // 'text'
    const TY_CLRO: u32 = 0x636C_726F; // 'clro'

    /// Differential: for every testbed profile both rcms and lcms2 accept, every
    /// tag whose on-disk TYPE is one of this task's trivial readers must decode to
    /// the SAME value via rcms `read_tag` as via lcms2 `cmsReadTag`. Tallies which
    /// trivial types were exercised over how many profiles.
    #[test]
    fn trivial_tag_values_match_oracle_over_testbed() {
        use std::collections::BTreeMap;

        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        // type-sig -> count of tags compared with that on-disk type.
        let mut exercised: BTreeMap<u32, usize> = BTreeMap::new();
        let mut profiles_with_trivial = 0usize;

        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();

            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let mut hit_here = false;
            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                let ty = match rcms_oracle::tag_true_type(&bytes, raw) {
                    Some(t) => t,
                    None => continue,
                };
                let rust = p.read_tag(sig);

                match ty {
                    TY_XYZ | TY_CORBIS_XYZ => {
                        let c = rcms_oracle::read_tag_xyz(&bytes, raw).expect("oracle xyz");
                        match rust.expect("rust xyz") {
                            Tag::Xyz(v) => {
                                rcms_oracle::assert_f64_bits_eq(
                                    v.x,
                                    c[0],
                                    (name.as_ref(), raw, "x"),
                                );
                                rcms_oracle::assert_f64_bits_eq(
                                    v.y,
                                    c[1],
                                    (name.as_ref(), raw, "y"),
                                );
                                rcms_oracle::assert_f64_bits_eq(
                                    v.z,
                                    c[2],
                                    (name.as_ref(), raw, "z"),
                                );
                            }
                            other => panic!("{name}:{raw:08x} expected Xyz, got {other:?}"),
                        }
                    }
                    TY_S15F16 => {
                        // Count derives from on-disk payload size / 4.
                        let entry = p.tag_entry(sig).unwrap();
                        let n = ((entry.size - 8) / 4) as usize;
                        let c =
                            rcms_oracle::read_tag_s15f16(&bytes, raw, n).expect("oracle s15f16");
                        match rust.expect("rust s15f16") {
                            Tag::S15Fixed16Array(v) => {
                                assert_eq!(v.len(), n, "{name}:{raw:08x} s15f16 len");
                                for (i, (rv, cv)) in v.iter().zip(c.iter()).enumerate() {
                                    rcms_oracle::assert_f64_bits_eq(
                                        rv.to_f64(),
                                        *cv,
                                        (name.as_ref(), raw, i),
                                    );
                                }
                            }
                            other => {
                                panic!("{name}:{raw:08x} expected S15Fixed16Array, got {other:?}")
                            }
                        }
                    }
                    TY_U16F16 => {
                        // No oracle extractor for u16f16 specifically; compare the
                        // raw u32 cells against the raw tag bytes (payload after the
                        // 8-byte base) — bit-exact by construction.
                        match rust.expect("rust u16f16") {
                            Tag::U16Fixed16Array(v) => {
                                let raw_tag = p.read_tag_raw(sig).unwrap();
                                let payload = &raw_tag[8..];
                                assert_eq!(
                                    v.len(),
                                    payload.len() / 4,
                                    "{name}:{raw:08x} u16f16 len"
                                );
                                for (i, cell) in payload.chunks_exact(4).enumerate() {
                                    let want = u32::from_be_bytes(cell.try_into().unwrap());
                                    assert_eq!(v[i].to_raw(), want, "{name}:{raw:08x} u16f16[{i}]");
                                }
                            }
                            other => {
                                panic!("{name}:{raw:08x} expected U16Fixed16Array, got {other:?}")
                            }
                        }
                    }
                    TY_UI08 => match rust.expect("rust ui08") {
                        Tag::U8Array(v) => {
                            let raw_tag = p.read_tag_raw(sig).unwrap();
                            assert_eq!(v, &raw_tag[8..], "{name}:{raw:08x} ui08 bytes");
                        }
                        other => panic!("{name}:{raw:08x} expected U8Array, got {other:?}"),
                    },
                    TY_UI32 => match rust.expect("rust ui32") {
                        Tag::U32Array(v) => {
                            let raw_tag = p.read_tag_raw(sig).unwrap();
                            let payload = &raw_tag[8..];
                            assert_eq!(v.len(), payload.len() / 4, "{name}:{raw:08x} ui32 len");
                            for (i, cell) in payload.chunks_exact(4).enumerate() {
                                let want = u32::from_be_bytes(cell.try_into().unwrap());
                                assert_eq!(v[i], want, "{name}:{raw:08x} ui32[{i}]");
                            }
                        }
                        other => panic!("{name}:{raw:08x} expected U32Array, got {other:?}"),
                    },
                    TY_SIG => {
                        let c = rcms_oracle::read_tag_signature(&bytes, raw).expect("oracle sig");
                        match rust.expect("rust sig") {
                            Tag::Signature(s) => assert_eq!(s.to_raw(), c, "{name}:{raw:08x} sig"),
                            other => panic!("{name}:{raw:08x} expected Signature, got {other:?}"),
                        }
                    }
                    TY_DATA => {
                        let (cflag, cdata) =
                            rcms_oracle::read_tag_data(&bytes, raw).expect("oracle data");
                        match rust.expect("rust data") {
                            Tag::Data { flag, data } => {
                                assert_eq!(flag, cflag, "{name}:{raw:08x} data flag");
                                assert_eq!(data, cdata, "{name}:{raw:08x} data bytes");
                            }
                            other => panic!("{name}:{raw:08x} expected Data, got {other:?}"),
                        }
                    }
                    TY_DTIM => {
                        let c =
                            rcms_oracle::read_tag_datetime(&bytes, raw).expect("oracle datetime");
                        match rust.expect("rust datetime") {
                            Tag::DateTime(d) => {
                                assert_eq!(
                                    [d.year, d.month, d.day, d.hours, d.minutes, d.seconds],
                                    c,
                                    "{name}:{raw:08x} datetime"
                                );
                            }
                            other => panic!("{name}:{raw:08x} expected DateTime, got {other:?}"),
                        }
                    }
                    TY_CHRM => {
                        let c =
                            rcms_oracle::read_tag_chromaticity(&bytes, raw).expect("oracle chrm");
                        match rust.expect("rust chrm") {
                            Tag::Chromaticity(t) => {
                                let got =
                                    [t.red.x, t.red.y, t.green.x, t.green.y, t.blue.x, t.blue.y];
                                for (i, (g, cv)) in got.iter().zip(c.iter()).enumerate() {
                                    rcms_oracle::assert_f64_bits_eq(
                                        *g,
                                        *cv,
                                        (name.as_ref(), raw, i),
                                    );
                                }
                            }
                            other => {
                                panic!("{name}:{raw:08x} expected Chromaticity, got {other:?}")
                            }
                        }
                    }
                    TY_TEXT => {
                        let c = rcms_oracle::read_tag_text(&bytes, raw).expect("oracle text");
                        match rust.expect("rust text") {
                            Tag::Text(s) => {
                                assert_eq!(s.as_bytes(), c.as_slice(), "{name}:{raw:08x} text");
                            }
                            other => panic!("{name}:{raw:08x} expected Text, got {other:?}"),
                        }
                    }
                    TY_CLRO => {
                        // The oracle returns the 16-byte 0xFF-padded array; rcms
                        // returns just the Count leading bytes. Compare rcms's bytes
                        // against the oracle's leading bytes.
                        let c =
                            rcms_oracle::read_tag_colorant_order(&bytes, raw).expect("oracle clro");
                        match rust.expect("rust clro") {
                            Tag::ColorantOrder(order) => {
                                assert!(order.len() <= c.len(), "{name}:{raw:08x} clro len");
                                assert_eq!(
                                    order.as_slice(),
                                    &c[..order.len()],
                                    "{name}:{raw:08x} clro bytes"
                                );
                            }
                            other => {
                                panic!("{name}:{raw:08x} expected ColorantOrder, got {other:?}")
                            }
                        }
                    }
                    // Not a trivial type this task handles; skip (e.g. curv, mluc).
                    _ => continue,
                }

                *exercised.entry(ty).or_default() += 1;
                hit_here = true;
            }
            if hit_here {
                profiles_with_trivial += 1;
            }
        }

        println!(
            "trivial tag diff: {profiles_with_trivial} profiles carried trivial tags; \
             per-type comparison counts (type_sig -> n):"
        );
        for (ty, n) in &exercised {
            let b = ty.to_be_bytes();
            let s: String = b.iter().map(|&c| c as char).collect();
            println!("  '{s}' ({ty:08x}): {n}");
        }

        // XYZ colorant tags appear in test1..5 / sRGB-style profiles; require we
        // exercised at least the XYZ reader.
        assert!(
            exercised.contains_key(&TY_XYZ),
            "expected at least one XYZ-typed tag exercised over the testbed"
        );
    }

    /// Differential: every testbed profile carrying a `'meas'` tag (the only
    /// struct-shaped type in this task that has real testbed coverage —
    /// test5.icc) must decode to the same `cmsICCMeasurementConditions` via rcms
    /// `read_tag` as via lcms2 `cmsReadTag` (bit-exact for the f64 fields).
    #[test]
    fn measurement_tag_values_match_oracle_over_testbed() {
        const TY_MEAS: u32 = 0x6D65_6173; // 'meas'
        let mut checked = 0usize;
        for path in testbed_icc() {
            let bytes = fs::read(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                if rcms_oracle::tag_true_type(&bytes, raw) != Some(TY_MEAS) {
                    continue;
                }
                let (cu, cf) =
                    rcms_oracle::read_tag_measurement(&bytes, raw).expect("oracle measurement");
                match p.read_tag(sig).expect("rust measurement") {
                    Tag::Measurement(m) => {
                        assert_eq!(m.observer, cu[0], "{name}:{raw:08x} observer");
                        assert_eq!(m.geometry, cu[1], "{name}:{raw:08x} geometry");
                        assert_eq!(m.illuminant_type, cu[2], "{name}:{raw:08x} illuminant");
                        rcms_oracle::assert_f64_bits_eq(m.backing.x, cf[0], (name.as_ref(), "bx"));
                        rcms_oracle::assert_f64_bits_eq(m.backing.y, cf[1], (name.as_ref(), "by"));
                        rcms_oracle::assert_f64_bits_eq(m.backing.z, cf[2], (name.as_ref(), "bz"));
                        rcms_oracle::assert_f64_bits_eq(m.flare, cf[3], (name.as_ref(), "flare"));
                    }
                    other => panic!("{name}:{raw:08x} expected Measurement, got {other:?}"),
                }
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "expected at least one 'meas' tag in the testbed"
        );
    }

    /// Differential: every testbed profile carrying an `'mluc'`- or `'desc'`-typed
    /// tag must decode to the same set of translations via rcms `read_tag` as via
    /// lcms2's MLU API. We compare (a) the set of (language, country) codes against
    /// `cmsMLUtranslationsCount`/`Codes`, and (b) for each code the decoded `text`
    /// against `cmsMLUgetWide` (both decoded from the identical UTF-16 units, so
    /// the comparison is over the same normalization).
    #[test]
    fn mlu_tag_values_match_oracle_over_testbed() {
        use std::collections::BTreeMap;

        const TY_MLUC: u32 = 0x6D6C_7563; // 'mluc'
        const TY_DESC: u32 = 0x6465_7363; // 'desc'

        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        let mut exercised: BTreeMap<u32, usize> = BTreeMap::new();
        let mut translations_compared = 0usize;
        let mut profiles_with_mlu = 0usize;

        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let mut hit_here = false;
            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                let ty = match rcms_oracle::tag_true_type(&bytes, raw) {
                    Some(t) => t,
                    None => continue,
                };
                if ty != TY_MLUC && ty != TY_DESC {
                    continue;
                }

                let oracle = rcms_oracle::mlu_entries(&bytes, raw).expect("oracle mlu");
                let mlu = match p.read_tag(sig).expect("rust mlu") {
                    Tag::Mlu(m) => m,
                    other => panic!("{name}:{raw:08x} expected Mlu, got {other:?}"),
                };

                // Same number of translations.
                assert_eq!(
                    mlu.entries.len(),
                    oracle.len(),
                    "{name}:{raw:08x} translation count"
                );

                // Same (language, country) codes AND decoded text, in the same
                // index order (lcms2 enumerates Entries[0..Count] in disk order;
                // rcms preserves that order too).
                for (i, (r, o)) in mlu.entries.iter().zip(oracle.iter()).enumerate() {
                    assert_eq!(
                        r.language, o.language,
                        "{name}:{raw:08x}[{i}] language code"
                    );
                    assert_eq!(r.country, o.country, "{name}:{raw:08x}[{i}] country code");
                    assert_eq!(r.text, o.text, "{name}:{raw:08x}[{i}] decoded text");
                    translations_compared += 1;
                }

                *exercised.entry(ty).or_default() += 1;
                hit_here = true;
            }
            if hit_here {
                profiles_with_mlu += 1;
            }
        }

        println!(
            "mlu tag diff: {profiles_with_mlu} profiles carried mluc/desc tags; \
             {translations_compared} translations compared; per-type tag counts:"
        );
        for (ty, n) in &exercised {
            let b = ty.to_be_bytes();
            let s: String = b.iter().map(|&c| c as char).collect();
            println!("  '{s}' ({ty:08x}): {n}");
        }

        // The v4 testbed profiles carry mluc tags (profileDescription, copyright,
        // …); require we exercised at least one.
        assert!(
            !exercised.is_empty(),
            "expected at least one mluc/desc tag over the testbed"
        );
    }

    /// Reachability: each struct-shaped type's on-disk signature now dispatches to
    /// a reader (no longer `Error::Unsupported`). We feed a minimal valid payload
    /// straight through `read_tag_value` and assert it does NOT return the
    /// deferred-`Unsupported` sentinel. (Per-field correctness is covered by the
    /// synthetic unit tests in `types::structs` and the `meas` differential above.)
    #[test]
    fn struct_types_now_dispatch() {
        use crate::io::MemReader;
        use crate::profile::types::read_tag_value;

        // (type_sig, minimal valid payload bytes, expected SizeOfTag)
        let meas = {
            let mut b = Vec::new();
            b.extend_from_slice(&1u32.to_be_bytes()); // Observer
            b.extend_from_slice(&[0u8; 12]); // Backing XYZ
            b.extend_from_slice(&0u32.to_be_bytes()); // Geometry
            b.extend_from_slice(&0u32.to_be_bytes()); // Flare
            b.extend_from_slice(&0u32.to_be_bytes()); // IlluminantType
            b
        };
        let view = vec![0u8; 28]; // 2 XYZ + u32
        let scrn = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes()); // Flag
            b.extend_from_slice(&0u32.to_be_bytes()); // nChannels = 0
            b
        };
        let crdi = {
            let mut b = Vec::new();
            for _ in 0..5 {
                b.extend_from_slice(&0u32.to_be_bytes()); // five zero-length strings
            }
            b
        };
        let cicp = vec![1u8, 13, 0, 1];
        let clrt = 0u32.to_be_bytes().to_vec(); // count = 0
        let mluc = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes()); // Count = 0
            b.extend_from_slice(&12u32.to_be_bytes()); // RecLen = 12
            b
        };
        let desc = 0u32.to_be_bytes().to_vec(); // AsciiCount = 0

        for (ty, body) in [
            (0x6D65_6173u32, meas), // 'meas'
            (0x7669_6577, view),    // 'view'
            (0x7363_726E, scrn),    // 'scrn'
            (0x6372_6469, crdi),    // 'crdi'
            (0x6369_6370, cicp),    // 'cicp'
            (0x636C_7274, clrt),    // 'clrt'
            (0x6D6C_7563, mluc),    // 'mluc'
            (0x6465_7363, desc),    // 'desc'
        ] {
            let mut r = MemReader::new(&body);
            let res = read_tag_value(Signature::from_raw(ty), &mut r, body.len() as u32);
            assert!(
                !matches!(res, Err(Error::Unsupported(_))),
                "type {ty:08x} should dispatch, got {res:?}"
            );
            assert!(res.is_ok(), "type {ty:08x} should parse, got {res:?}");
        }
    }

    /// A TRC curve tag (on-disk type `'curv'`/`'para'`) now decodes to a
    /// `Tag::Curve` — it must NO LONGER return `Error::Unsupported` (this slice
    /// implemented the curve readers that the slice-2 dispatcher deferred).
    #[test]
    fn trc_curve_tag_no_longer_unsupported() {
        let red_trc = Signature::from_raw(0x7254_5243); // 'rTRC'
        let gray_trc = Signature::from_raw(0x6b54_5243); // 'kTRC'

        let mut checked = 0usize;
        for path in testbed_icc() {
            let bytes = fs::read(&path).unwrap();
            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for sig in [red_trc, gray_trc] {
                if !p.has_tag(sig) {
                    continue;
                }
                // Only assert for curve/parametric on-disk types.
                let ty = match rcms_oracle::tag_true_type(&bytes, sig.to_raw()) {
                    Some(t) => t,
                    None => continue,
                };
                const TY_CURV: u32 = 0x6375_7276; // 'curv'
                const TY_PARA: u32 = 0x7061_7261; // 'para'
                if ty != TY_CURV && ty != TY_PARA {
                    continue;
                }
                let res = p.read_tag(sig);
                assert!(
                    !matches!(res, Err(Error::Unsupported(_))),
                    "curve tag {sig} in {:?} should no longer be Unsupported, got {res:?}",
                    path.file_name().unwrap()
                );
                assert!(
                    matches!(res, Ok(Tag::Curve(_))),
                    "curve tag {sig} in {:?} should decode to Tag::Curve, got {res:?}",
                    path.file_name().unwrap()
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "expected at least one TRC curve tag in the testbed"
        );
    }

    /// Differential: every testbed profile carrying a `'dict'`-typed tag (the
    /// `meta` tag in ibm-t61.icc / new.icc) must decode to the same dictionary via
    /// rcms `read_tag` as via lcms2's dict API. lcms2 enumerates entries in the
    /// REVERSE of the on-disk record order (`cmsDictAddEntry` prepends); rcms keeps
    /// on-disk order, so we reverse the oracle list before comparing.
    #[test]
    fn dict_tag_values_match_oracle_over_testbed() {
        const TY_DICT: u32 = 0x6469_6374; // 'dict'
        let mut checked = 0usize;
        for path in testbed_icc() {
            let bytes = fs::read(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };
            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                if rcms_oracle::tag_true_type(&bytes, raw) != Some(TY_DICT) {
                    continue;
                }
                let mut oracle = rcms_oracle::read_tag_dict(&bytes, raw).expect("oracle dict");
                oracle.entries.reverse(); // disk order to match rcms.

                let dict = match p.read_tag(sig).expect("rust dict") {
                    Tag::Dict(d) => d,
                    other => panic!("{name}:{raw:08x} expected Dict, got {other:?}"),
                };

                assert_eq!(
                    dict.entries.len(),
                    oracle.entries.len(),
                    "{name}:{raw:08x} dict entry count"
                );
                for (i, (r, o)) in dict.entries.iter().zip(oracle.entries.iter()).enumerate() {
                    assert_eq!(r.name, o.name, "{name}:{raw:08x}[{i}] name");
                    assert_eq!(r.value, o.value, "{name}:{raw:08x}[{i}] value");
                    assert_opt_mlu_eq(
                        r.display_name.as_ref(),
                        o.display_name.as_ref(),
                        &format!("{name}:{raw:08x}[{i}] display_name"),
                    );
                    assert_opt_mlu_eq(
                        r.display_value.as_ref(),
                        o.display_value.as_ref(),
                        &format!("{name}:{raw:08x}[{i}] display_value"),
                    );
                }
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "expected at least one 'dict'/'meta' tag in the testbed"
        );
    }

    /// Compare an optional rcms [`Mlu`] against an optional oracle MLU
    /// (translation-by-translation: language, country, decoded text). Both must be
    /// `Some`/`None` together; an absent display MLU is `None` on both sides.
    fn assert_opt_mlu_eq(
        rust: Option<&crate::profile::tag::Mlu>,
        oracle: Option<&rcms_oracle::OracleMlu>,
        ctx: &str,
    ) {
        match (rust, oracle) {
            (None, None) => {}
            (Some(m), Some(o)) => {
                assert_eq!(m.entries.len(), o.entries.len(), "{ctx} translation count");
                for (i, (r, c)) in m.entries.iter().zip(o.entries.iter()).enumerate() {
                    assert_eq!(r.language, c.language, "{ctx}[{i}] language");
                    assert_eq!(r.country, c.country, "{ctx}[{i}] country");
                    assert_eq!(r.text, c.text, "{ctx}[{i}] text");
                }
            }
            (r, o) => panic!(
                "{ctx} presence mismatch: rust={:?} oracle={:?}",
                r.is_some(),
                o.is_some()
            ),
        }
    }

    // ---- Part B: comprehensive done-criteria testbed sweep ----

    /// On-disk tag-TYPE signatures that are implemented (in-scope): the full set
    /// dispatched in `crate::profile::types::read_tag_value`. Any tag whose on-disk
    /// type is in this set MUST succeed (`Ok`) from `read_tag`.
    const INSCOPE_TYPES: &[u32] = &[
        0x5859_5A20, // 'XYZ '
        0x17A5_05B8, // Corbis broken XYZ (mapped to XYZ)
        0x7366_3332, // 'sf32' S15Fixed16Array
        0x7566_3332, // 'uf32' U16Fixed16Array
        0x7569_3038, // 'ui08' UInt8Array
        0x7569_3332, // 'ui32' UInt32Array
        0x7369_6720, // 'sig ' Signature
        0x6461_7461, // 'data' Data
        0x6474_696D, // 'dtim' DateTime
        0x6368_726D, // 'chrm' Chromaticity
        0x7465_7874, // 'text' Text
        0x636C_726F, // 'clro' ColorantOrder
        0x6D65_6173, // 'meas' Measurement
        0x7669_6577, // 'view' ViewingConditions
        0x7363_726E, // 'scrn' Screening
        0x6372_6469, // 'crdi' CrdInfo
        0x6369_6370, // 'cicp' Cicp
        0x636C_7274, // 'clrt' ColorantTable
        0x6D6C_7563, // 'mluc' Mlu
        0x6465_7363, // 'desc' TextDescription (decoded as Mlu)
        0x6E63_6C32, // 'ncl2' NamedColor2
        0x7073_6571, // 'pseq' ProfileSequenceDesc
        0x7073_6964, // 'psid' ProfileSequenceId
        0x6469_6374, // 'dict' Dictionary
        0x6375_7276, // 'curv' Curve
        0x7061_7261, // 'para' ParametricCurve
        0x7663_6774, // 'vcgt' Vcgt
        0x6266_6420, // 'bfd ' UcrBg
        0x6D66_7431, // 'mft1' Lut8
        0x6D66_7432, // 'mft2' Lut16
        0x6D41_4220, // 'mAB ' LutAtoB
        0x6D42_4120, // 'mBA ' LutBtoA
        0x6D70_6574, // 'mpet' MultiProcessElement
    ];

    /// Comprehensive testbed sweep: for every `vendor/Little-CMS/testbed/*.icc`:
    ///
    /// 1. Accept/reject agrees with lcms2 `cmsOpenProfileFromMem`.
    /// 2. For each tag in accepted profiles: `read_tag` never panics, returns either
    ///    `Ok(tag)` OR `Err(Unsupported)`.
    /// 3. `Err(Unsupported)` occurs ONLY when the on-disk type is NOT in the
    ///    in-scope set (i.e. is genuinely a deferred type like `curv`/`para`/LUTs).
    ///    An in-scope type that returns `Unsupported` is a BUG and fails the test.
    /// 4. Prints a summary: profiles, tags total, in-scope (Ok), deferred
    ///    (Unsupported), and the distribution of deferred on-disk types.
    #[test]
    fn comprehensive_testbed_sweep() {
        use std::collections::BTreeMap;

        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        let mut n_profiles = 0usize;
        let mut n_tags_total = 0usize;
        let mut n_inscope_ok = 0usize;
        let mut n_deferred = 0usize;
        // Map from on-disk type sig → count of tags that returned Unsupported.
        let mut deferred_by_type: BTreeMap<u32, usize> = BTreeMap::new();

        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();

            let oracle_ok = rcms_oracle::open_succeeds(&bytes);
            let rust = Profile::open(&bytes);

            // 1. Accept/reject must agree with lcms2.
            assert_eq!(
                rust.is_ok(),
                oracle_ok,
                "[sweep] open accept/reject disagree on {name}: rust={:?} lcms2={oracle_ok}",
                rust.as_ref().err()
            );

            if !oracle_ok {
                continue; // Both reject — nothing more to test.
            }

            n_profiles += 1;
            let p = rust.unwrap();

            for sig in p.tags().collect::<Vec<_>>() {
                n_tags_total += 1;
                let raw_sig = sig.to_raw();

                // Determine the on-disk type so we know whether Unsupported is legitimate.
                let on_disk_type = rcms_oracle::tag_true_type(&bytes, raw_sig);

                // 2. read_tag must not panic and must return Ok or Err(Unsupported).
                let result = p.read_tag(sig);
                match &result {
                    Ok(_) => {
                        n_inscope_ok += 1;
                        // 3a. If it's Ok, the on-disk type must be in-scope. If the
                        //     oracle can't even tell us the type it's fine (linked/unknown);
                        //     but if it IS known it should be in the in-scope set.
                        if let Some(ty) = on_disk_type {
                            assert!(
                                INSCOPE_TYPES.contains(&ty),
                                "[sweep] {name}:{raw_sig:08x} read_tag returned Ok but on-disk type \
                                 {ty:08x} is not in the in-scope set — unexpected success",
                            );
                        }
                    }
                    Err(Error::Unsupported(_)) => {
                        // 3b. Unsupported is only legitimate for deferred on-disk types.
                        if let Some(ty) = on_disk_type {
                            assert!(
                                !INSCOPE_TYPES.contains(&ty),
                                "[sweep] {name}:{raw_sig:08x} read_tag returned Unsupported but \
                                 on-disk type {ty:08x} is in-scope — this is a BUG (tag sig \
                                 {raw_sig:08x}, type {ty:08x})",
                            );
                            *deferred_by_type.entry(ty).or_default() += 1;
                        }
                        n_deferred += 1;
                    }
                    Err(other) => {
                        // Any error other than Unsupported for a tag whose on-disk type IS
                        // in-scope is a bug. BadType, Corrupt, or Range are expected when:
                        //   - The tag has an unknown descriptor (rcms returns BadType).
                        //   - The tag has a known descriptor but a mismatched on-disk type
                        //     (e.g., an in-scope tag carrying an unrecognized type sig).
                        //   - Data is genuinely corrupt.
                        // These are NOT bugs — they represent lcms2's own BadType / corrupt
                        // path, not an rcms deficiency.
                        let ok =
                            matches!(other, Error::BadType(_) | Error::Range | Error::Corrupt(_));
                        assert!(
                            ok,
                            "[sweep] {name}:{raw_sig:08x} read_tag returned unexpected \
                             error {other:?} — should be Ok, Unsupported, BadType, or Corrupt"
                        );
                        // Verify: if the on-disk type IS in-scope, this error is a
                        // bug ONLY when lcms2 itself successfully read the tag. Some
                        // testbed tags are deliberately malformed (e.g. bad_mpe.icc's
                        // mpet), so lcms2's own `cmsReadTag` returns NULL — rcms is
                        // then RIGHT to fail (Corrupt/BadType), matching lcms2.
                        if let Some(ty) = on_disk_type {
                            if INSCOPE_TYPES.contains(&ty) {
                                assert!(
                                    !rcms_oracle::tag_read_succeeds(&bytes, raw_sig),
                                    "[sweep] {name}:{raw_sig:08x} INSCOPE type {ty:08x} returned \
                                     non-Unsupported error {other:?} but lcms2 read it fine — \
                                     this is a rcms bug",
                                );
                            }
                        }
                        // Count these as "other" — not in-scope-Ok, not deferred-Unsupported.
                    }
                }
            }
        }

        // Print the summary (visible with --nocapture).
        eprintln!(
            "\n=== Comprehensive testbed sweep ===\n\
             Profiles accepted: {n_profiles}/{total_files}\n\
             Tags total:        {n_tags_total}\n\
             In-scope (Ok):     {n_inscope_ok}\n\
             Deferred (Unsupported): {n_deferred}",
            total_files = files.len()
        );
        if deferred_by_type.is_empty() {
            eprintln!("  (no deferred-type tags encountered)");
        } else {
            eprintln!("  Deferred on-disk type distribution:");
            for (ty, count) in &deferred_by_type {
                let b = ty.to_be_bytes();
                let label: String = b
                    .iter()
                    .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
                    .collect();
                eprintln!("    '{label}' ({ty:08x}): {count} tag(s)");
            }
        }
        eprintln!("===================================\n");

        // Slice 4 done-criteria: every LUT/MPE on-disk type (mft1/mft2/mAB/mBA/
        // mpet) is now in-scope, joining the tone-curve and struct types. With the
        // MPE reader landed, ZERO deferred tag types remain — no testbed tag may
        // return `Unsupported` for a known on-disk type.
        assert!(
            deferred_by_type.is_empty(),
            "[sweep] expected 0 deferred on-disk types, but these still defer: {:08x?}",
            deferred_by_type.keys().collect::<Vec<_>>()
        );

        assert!(n_profiles > 0, "expected at least one accepted profile");
        assert!(
            n_tags_total > 0,
            "expected at least one tag over all profiles"
        );
        assert!(n_inscope_ok > 0, "expected at least one in-scope Ok tag");
    }

    // ---- End Part B ----

    /// Reachability: NamedColor2 / ProfileSequence{Desc,Id} / Dictionary now
    /// dispatch (no longer `Error::Unsupported`). Minimal valid payloads are fed
    /// straight through `read_tag_value`; per-field correctness lives in the
    /// `types::named` synthetic unit tests and the dict testbed differential.
    #[test]
    fn named_seq_dict_types_now_dispatch() {
        use crate::io::MemReader;
        use crate::profile::types::read_tag_value;

        // ncl2: vendorFlag, count=0, nDeviceCoords=0, prefix[32], suffix[32].
        let ncl2 = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes()); // vendorFlag
            b.extend_from_slice(&0u32.to_be_bytes()); // count = 0
            b.extend_from_slice(&0u32.to_be_bytes()); // nDeviceCoords = 0
            b.extend_from_slice(&[0u8; 32]); // prefix
            b.extend_from_slice(&[0u8; 32]); // suffix
            b
        };
        // pseq: Count = 0.
        let pseq = 0u32.to_be_bytes().to_vec();
        // psid: Count = 0 (empty position table).
        let psid = 0u32.to_be_bytes().to_vec();
        // dict: Count = 0, Length = 16.
        let dict = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes()); // Count = 0
            b.extend_from_slice(&16u32.to_be_bytes()); // Length = 16
            b
        };

        // psid/dict use Tell-based BaseOffset, so drive them through the real
        // type-base-prefixed path (8-byte base consumed by read_type_base).
        for (ty, body, needs_base) in [
            (0x6E63_6C32u32, ncl2, false), // 'ncl2'
            (0x7073_6571, pseq, false),    // 'pseq'
            (0x7073_6964, psid, true),     // 'psid'
            (0x6469_6374, dict, true),     // 'dict'
        ] {
            let mut full = Vec::new();
            full.extend_from_slice(&ty.to_be_bytes());
            full.extend_from_slice(&[0, 0, 0, 0]); // reserved
            full.extend_from_slice(&body);
            let mut r = MemReader::new(&full);
            let _ = r.read_type_base().unwrap();
            let res = read_tag_value(Signature::from_raw(ty), &mut r, body.len() as u32);
            let _ = needs_base;
            assert!(
                !matches!(res, Err(Error::Unsupported(_))),
                "type {ty:08x} should dispatch, got {res:?}"
            );
            assert!(res.is_ok(), "type {ty:08x} should parse, got {res:?}");
        }
    }

    /// Differential: for every testbed profile both rcms and lcms2 accept, every
    /// tag whose on-disk TYPE is `curv` or `para` must decode to a `Tag::Curve`
    /// whose `eval_float` is bit-identical (f32::to_bits) to lcms2's
    /// `cmsEvalToneCurveFloat` at 256 sample points in [0, 1]. Tallies how many
    /// curve tags were exercised over how many profiles.
    #[test]
    fn curve_tag_values_match_oracle_over_testbed() {
        const TY_CURV: u32 = 0x6375_7276; // 'curv'
        const TY_PARA: u32 = 0x7061_7261; // 'para'

        // 256 sample points x in [0, 1] (inclusive endpoints).
        let xs: Vec<f32> = (0..256).map(|i| i as f32 / 255.0).collect();

        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        let mut curv_tags = 0usize;
        let mut para_tags = 0usize;
        let mut profiles_with_curve = 0usize;

        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();

            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let mut hit_here = false;
            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                let ty = match rcms_oracle::tag_true_type(&bytes, raw) {
                    Some(t) => t,
                    None => continue,
                };
                if ty != TY_CURV && ty != TY_PARA {
                    continue;
                }

                // Must no longer be Unsupported, and must be a Tag::Curve.
                let curve = match p.read_tag(sig).expect("rust curve") {
                    Tag::Curve(c) => c,
                    other => panic!("{name}:{raw:08x} expected Curve, got {other:?}"),
                };

                let c = rcms_oracle::read_tag_curve(&bytes, raw, &xs).expect("oracle curve");
                assert_eq!(c.len(), xs.len(), "{name}:{raw:08x} sample count");
                for (i, (&x, &cy)) in xs.iter().zip(c.iter()).enumerate() {
                    let ry = curve.eval_float(x);
                    assert_eq!(
                        ry.to_bits(),
                        cy.to_bits(),
                        "{name}:{raw:08x} curve sample[{i}] x={x}: rust={ry} lcms2={cy}"
                    );
                }

                match ty {
                    TY_CURV => curv_tags += 1,
                    TY_PARA => para_tags += 1,
                    _ => unreachable!(),
                }
                hit_here = true;
            }
            if hit_here {
                profiles_with_curve += 1;
            }
        }

        println!(
            "testbed curve diff: {curv_tags} curv + {para_tags} para tags over \
             {profiles_with_curve} profiles"
        );
        assert!(
            curv_tags + para_tags > 0,
            "expected at least one curv/para tag in the testbed"
        );
    }

    /// `curv`/`para` tags no longer return `Error::Unsupported` from the type
    /// dispatcher (the slice-2 fallthrough is gone for these two type sigs).
    #[test]
    fn curve_types_now_dispatch() {
        use crate::io::MemReader;
        use crate::profile::types::read_tag_value;

        // curv: Count = 0 (linear).
        let curv = 0u32.to_be_bytes().to_vec();
        // para: ICC Type 0, reserved, one s15Fixed16 gamma = 1.0.
        let para = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u16.to_be_bytes()); // Type 0
            b.extend_from_slice(&0u16.to_be_bytes()); // reserved
            b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // 1.0
            b
        };

        for (ty, body) in [
            (0x6375_7276u32, curv), // 'curv'
            (0x7061_7261, para),    // 'para'
        ] {
            let mut full = Vec::new();
            full.extend_from_slice(&ty.to_be_bytes());
            full.extend_from_slice(&[0, 0, 0, 0]); // reserved
            full.extend_from_slice(&body);
            let mut r = MemReader::new(&full);
            let _ = r.read_type_base().unwrap();
            let res = read_tag_value(Signature::from_raw(ty), &mut r, body.len() as u32);
            assert!(
                !matches!(res, Err(Error::Unsupported(_))),
                "type {ty:08x} should dispatch, got {res:?}"
            );
            assert!(
                matches!(res, Ok(Tag::Curve(_))),
                "type {ty:08x} should parse to Curve, got {res:?}"
            );
        }
    }

    /// Differential: for every testbed profile both stacks accept, every tag whose
    /// on-disk TYPE is `vcgt` must decode to a `Tag::Vcgt` of 3 curves whose
    /// `eval_float` is bit-identical (f32::to_bits) to lcms2's
    /// `cmsEvalToneCurveFloat` at 256 points in [0, 1] for each channel. `new.icc`
    /// and `ibm-t61.icc` carry table-variant vcgt tags, so this is REAL coverage.
    #[test]
    fn vcgt_tag_values_match_oracle_over_testbed() {
        const TY_VCGT: u32 = 0x7663_6774; // 'vcgt'

        let xs: Vec<f32> = (0..256).map(|i| i as f32 / 255.0).collect();
        let files = testbed_icc();
        assert!(!files.is_empty(), "no .icc in testbed");

        let mut vcgt_tags = 0usize;
        for path in &files {
            let bytes = fs::read(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();

            if !rcms_oracle::open_succeeds(&bytes) {
                continue;
            }
            let p = match Profile::open(&bytes) {
                Ok(p) => p,
                Err(_) => continue,
            };

            for sig in p.tags().collect::<Vec<_>>() {
                let raw = sig.to_raw();
                if rcms_oracle::tag_true_type(&bytes, raw) != Some(TY_VCGT) {
                    continue;
                }

                let curves = match p.read_tag(sig).expect("rust vcgt") {
                    Tag::Vcgt(c) => c,
                    other => panic!("{name}:{raw:08x} expected Vcgt, got {other:?}"),
                };
                assert_eq!(curves.len(), 3, "{name}:{raw:08x} channel count");

                let oracle = rcms_oracle::read_tag_vcgt(&bytes, raw, &xs).expect("oracle vcgt");
                for (ch, oys) in oracle.iter().enumerate() {
                    for (i, (&x, &cy)) in xs.iter().zip(oys.iter()).enumerate() {
                        let ry = curves[ch].eval_float(x);
                        assert_eq!(
                            ry.to_bits(),
                            cy.to_bits(),
                            "{name}:{raw:08x} ch{ch} sample[{i}] x={x}: rust={ry} lcms2={cy}"
                        );
                    }
                }
                vcgt_tags += 1;
            }
        }

        println!("testbed vcgt diff: {vcgt_tags} vcgt tags");
        assert!(
            vcgt_tags >= 2,
            "expected vcgt tags in new.icc and ibm-t61.icc, found {vcgt_tags}"
        );
    }
}
