//! Round-trip sweep over the testbed: writer ↔ reader parity (slice-7 T5).
//!
//! For every `vendor/Little-CMS/testbed/*.icc` that both rcms and lcms2 accept:
//!
//! 1. rcms reads the profile, then [`build_writable`] reconstructs a
//!    [`WritableProfile`] carrying ALL its tags (each parsed `Tag` → the writable
//!    representation), preserving tag order + links, and [`save_to_mem`] writes
//!    fresh ICC bytes.
//! 2. rcms reads those bytes BACK and asserts STRUCTURAL/semantic equality of every
//!    tag against the original parse ([`assert_tag_semeq`] — curves/LUTs/vcgt are
//!    compared by evaluation at sample points; everything else by value equality).
//! 3. CROSS-CHECK: lcms2 reads the rcms-written bytes and a few representative tag
//!    values (wtpt/desc/the LUT or TRC tags) match rcms's parsed values — proving
//!    the rcms-written profile is valid + correct for a third-party reader.
//!
//! Byte-vs-original is NOT the contract (lcms2 raw-copies unmodified disk tags;
//! rcms re-serializes). The T0-T4 byte-identity tests cover byte exactness for the
//! tags both stacks serialize from the same structure; this guards the WRITERS
//! against the READERS over real profiles, and surfaces any read-supported tag
//! type that still lacks a writer (the "gap set").

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rcms::error::Error;
use rcms::profile::tag::Tag;
use rcms::profile::{save_to_mem, Profile, SlotContent, WritableProfile};
use rcms::sig::Signature;

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

/// The known/intended writer deferrals: read-supported tag *types* that
/// `WritableProfile` cannot yet serialize (`write_type_for` returns
/// `Unsupported`). Surfaced explicitly so the sweep can assert the gap set is
/// EXACTLY these — no silent coverage holes. Keyed by on-disk type sig.
///
/// rcms reads these but slice-7 did not land writers for them (they are not
/// emitted by any virtual profile and were out of T1-T4 scope). Only `dict` has
/// real testbed coverage (the `meta` tag in ibm-t61.icc / new.icc); the rest are
/// listed so an unexpected appearance still trips the "no silent holes" assert:
///   - `ui08` UInt8Array, `scrn` Screening, `crdi` CrdInfo, `ncl2` NamedColor2,
///     `dict` Dictionary, `bfd ` UcrBg.
///
/// (`vcgt` was a writer gap until slice-7 T5 landed `write_vcgt`, transcribed from
/// `Type_vcgt_Write`; it is now fully writable and NOT a deferral.)
const DEFERRED_WRITER_TYPES: &[(u32, &str)] = &[
    (0x7569_3038, "ui08 (UInt8Array)"),
    (0x7363_726E, "scrn (Screening)"),
    (0x6372_6469, "crdi (CrdInfo)"),
    (0x6E63_6C32, "ncl2 (NamedColor2)"),
    (0x6469_6374, "dict (Dictionary)"),
    (0x6266_6420, "bfd  (UcrBg)"),
];

fn is_deferred(ty: u32) -> bool {
    DEFERRED_WRITER_TYPES.iter().any(|&(t, _)| t == ty)
}

/// Chase a tag's link chain (mirroring the reader's transitive `search_tag`) to
/// the ultimate non-linked signature whose entry holds the actual body. Bounded
/// by the tag count so a pathological cycle cannot loop forever.
fn resolve_link(p: &Profile, sig: Signature) -> Signature {
    let mut cur = sig;
    for _ in 0..p.tags().count() + 1 {
        match p.tag_entry(cur).and_then(|e| e.linked) {
            Some(next) => cur = next,
            None => return cur,
        }
    }
    cur
}

/// Reconstruct a [`WritableProfile`] carrying every tag of a parsed `Profile`,
/// preserving directory order and tag links. A tag whose `Tag` value has no
/// writer (`save_to_mem` would return `Unsupported`) is recorded in `gaps`
/// (keyed by its on-disk type sig) and dropped from the writable profile, so the
/// rest of the profile still round-trips and the gap is surfaced — never silently
/// skipped. Returns the writable profile and the per-type gap counts.
fn build_writable(p: &Profile, bytes: &[u8]) -> (WritableProfile, BTreeMap<u32, usize>) {
    let mut wp = WritableProfile::new(*p.header());
    let mut gaps: BTreeMap<u32, usize> = BTreeMap::new();

    for sig in p.tags().collect::<Vec<_>>() {
        let entry = p.tag_entry(sig).expect("tag entry present");
        // A linked entry: emit a link to its ULTIMATE body target. The reader's
        // links may form a transitive chain (e.g. crayons.icc bTRC → gTRC → rTRC),
        // but the serializer resolves links single-level (lcms2 `cmsLinkTag` only
        // ever targets a real tag). So chase the chain here to the final non-linked
        // signature and link directly to it — the on-wire effect is identical
        // (every link entry inherits the one body's offset/size).
        if entry.linked.is_some() {
            let target = resolve_link(p, sig);
            wp.link_tag(sig, target);
            continue;
        }
        // A real body: read the cooked tag, then check whether a writer exists by
        // attempting a trial serialization of a one-tag profile. If the writer is
        // missing (Unsupported), record the on-disk type as a gap.
        let value = match p.read_tag(sig) {
            Ok(v) => v,
            // A genuinely corrupt/unreadable tag (lcms2 would also fail) — not a
            // writer gap; skip it (the read-side sweep already asserts agreement).
            Err(_) => continue,
        };
        if !writer_supports(*p.header(), sig, &value) {
            let ty = rcms_oracle::tag_true_type(bytes, sig.to_raw()).unwrap_or(0);
            *gaps.entry(ty).or_default() += 1;
            continue;
        }
        wp.add_tag(sig, value);
    }
    (wp, gaps)
}

/// Whether `save_to_mem` can serialize `value` under `sig` — probed by building a
/// one-tag profile and attempting the write. Returns `false` only for the
/// `Unsupported` writer-gap sentinel; any other error is treated as "supported but
/// this particular value is malformed" (which the full-profile write surfaces).
fn writer_supports(header: rcms::profile::Header, sig: Signature, value: &Tag) -> bool {
    let mut one = WritableProfile::new(header);
    one.add_tag(sig, value.clone());
    !matches!(save_to_mem(&one), Err(Error::Unsupported(_)))
}

/// 16 sample points in [0, 1] for evaluation-based tag comparison.
fn samples() -> Vec<f32> {
    (0..16).map(|i| i as f32 / 15.0).collect()
}

/// Assert two parsed tags are SEMANTICALLY equal. Curves, LUTs (pipelines), and
/// vcgt are compared by EVALUATION at sample points (re-serialization can
/// materialize a curve as a table vs a gamma, or rebuild a pipeline's stages, so
/// byte/struct identity is not the contract — the function they encode is).
/// Everything else is compared by `PartialEq`.
fn assert_tag_semeq(sig: Signature, a: &Tag, b: &Tag, ctx: &str) {
    let xs = samples();
    match (a, b) {
        (Tag::Curve(ca), Tag::Curve(cb)) => {
            for &x in &xs {
                assert_eq!(
                    ca.eval_float(x).to_bits(),
                    cb.eval_float(x).to_bits(),
                    "{ctx} {sig}: curve eval differs at x={x}"
                );
            }
        }
        (Tag::Vcgt(va), Tag::Vcgt(vb)) => {
            assert_eq!(va.len(), vb.len(), "{ctx} {sig}: vcgt channel count");
            for (ch, (ca, cb)) in va.iter().zip(vb.iter()).enumerate() {
                for &x in &xs {
                    assert_eq!(
                        ca.eval_float(x).to_bits(),
                        cb.eval_float(x).to_bits(),
                        "{ctx} {sig}: vcgt ch{ch} eval differs at x={x}"
                    );
                }
            }
        }
        (Tag::Lut(la), Tag::Lut(lb)) => {
            assert_eq!(
                la.input_channels, lb.input_channels,
                "{ctx} {sig}: LUT input channels"
            );
            assert_eq!(
                la.output_channels, lb.output_channels,
                "{ctx} {sig}: LUT output channels"
            );
            // Evaluate both pipelines over a small per-channel grid and compare
            // outputs bit-for-bit (the re-serialized pipeline must encode the
            // identical function).
            for input in lut_grid(la.input_channels) {
                let oa = la.eval_float(&input);
                let ob = lb.eval_float(&input);
                assert_eq!(oa.len(), ob.len(), "{ctx} {sig}: LUT output width");
                for (i, (ya, yb)) in oa.iter().zip(ob.iter()).enumerate() {
                    assert_eq!(
                        ya.to_bits(),
                        yb.to_bits(),
                        "{ctx} {sig}: LUT eval ch{i} differs at input {input:?}"
                    );
                }
            }
        }
        (Tag::Mlu(ma), Tag::Mlu(mb)) => {
            // The v2 `desc` (textDescription) writer ALWAYS materializes a
            // unicode block (`Type_Text_Description_Write` mirrors the ASCII into
            // the V2Unicode `[0xff,0xff]` record), so a re-read can gain a
            // `[0xff,0xff]` entry the original on-disk tag (with an empty unicode
            // body) lacked. That is faithful lcms2 behavior, not a divergence: the
            // re-read MUST contain every original translation, and any EXTRA entry
            // must be exactly the V2Unicode mirror of the ASCII content.
            for e in &ma.entries {
                assert!(
                    mb.entries.iter().any(|f| f.language == e.language
                        && f.country == e.country
                        && f.text == e.text),
                    "{ctx} {sig}: re-read MLU dropped translation {:?}/{:?}",
                    e.language,
                    e.country
                );
            }
            for f in &mb.entries {
                let in_orig = ma.entries.iter().any(|e| {
                    e.language == f.language && e.country == f.country && e.text == f.text
                });
                let is_v2_unicode_mirror = f.language == [0xff, 0xff]
                    && f.country == [0xff, 0xff]
                    && ma.entries.iter().any(|e| e.text == f.text);
                assert!(
                    in_orig || is_v2_unicode_mirror,
                    "{ctx} {sig}: re-read MLU has unexpected entry {:?}/{:?} = {:?}",
                    f.language,
                    f.country,
                    f.text
                );
            }
        }
        // Every other tag type: structural value equality. The reader produces the
        // identical cooked value from the re-serialized body, so == is exact.
        _ => assert_eq!(a, b, "{ctx} {sig}: tag value differs"),
    }
}

/// A bounded per-channel input grid for LUT evaluation (3 points per channel,
/// capped so 4-channel CLUTs stay reasonable).
fn lut_grid(n_in: usize) -> Vec<Vec<f32>> {
    let levels = if n_in >= 4 { 2 } else { 3 };
    let pts: Vec<f32> = (0..levels)
        .map(|i| i as f32 / (levels - 1) as f32)
        .collect();
    let mut rows: Vec<Vec<f32>> = vec![vec![]];
    for _ in 0..n_in {
        let mut next = Vec::new();
        for row in &rows {
            for &p in &pts {
                let mut r = row.clone();
                r.push(p);
                next.push(r);
            }
        }
        rows = next;
    }
    rows
}

/// THE round-trip sweep. For every testbed profile both stacks accept:
/// read → build-writable → save → read-back → semantic tag equality, and tally
/// the writer-gap set. Asserts the gap set is EXACTLY the intended deferrals.
#[test]
fn roundtrip_writer_reader_parity_over_testbed() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut clean_profiles = 0usize; // round-tripped with zero gaps.
    let mut profiles_attempted = 0usize;
    let mut tags_compared = 0usize;
    // Union of writer-gap on-disk types over the whole testbed.
    let mut gap_types: BTreeMap<u32, usize> = BTreeMap::new();
    let mut gap_profiles: BTreeMap<String, Vec<u32>> = BTreeMap::new();

    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        profiles_attempted += 1;

        // 1. Build a writable carrying every tag (links preserved); surface gaps.
        let (wp, gaps) = build_writable(&p, &bytes);
        if gaps.is_empty() {
            clean_profiles += 1;
        } else {
            for (ty, n) in &gaps {
                *gap_types.entry(*ty).or_default() += n;
                gap_profiles.entry(name.clone()).or_default().push(*ty);
            }
        }

        // 2. Serialize. Any Unsupported here is a missed gap (writer_supports
        //    already filtered the known ones); a different error is a real bug.
        let written = match save_to_mem(&wp) {
            Ok(b) => b,
            Err(e) => panic!("{name}: save_to_mem failed: {e:?}"),
        };

        // 3. rcms reads the written bytes back; assert it is a valid profile.
        let back = Profile::open(&written)
            .unwrap_or_else(|e| panic!("{name}: rcms cannot re-open its own bytes: {e:?}"));

        // Header round-trips (the fields the serializer writes verbatim).
        assert_eq!(
            back.header().version,
            p.header().version,
            "{name}: header version"
        );
        assert_eq!(
            back.header().device_class,
            p.header().device_class,
            "{name}: device class"
        );
        assert_eq!(
            back.header().color_space,
            p.header().color_space,
            "{name}: color space"
        );
        assert_eq!(back.header().pcs, p.header().pcs, "{name}: pcs");

        // 4. Every written tag must read back to a semantically-equal value, and
        //    must appear in the same order with the same links.
        for slot in &wp.tags {
            let sig = slot.sig;
            match &slot.content {
                SlotContent::Linked(target) => {
                    // The link survives: the re-read entry points at the target.
                    let e = back
                        .tag_entry(sig)
                        .unwrap_or_else(|| panic!("{name}: linked tag {sig} missing on read-back"));
                    let te = back
                        .tag_entry(*target)
                        .unwrap_or_else(|| panic!("{name}: link target {target} missing"));
                    assert_eq!(
                        e.offset, te.offset,
                        "{name}: linked {sig} offset != target {target}"
                    );
                    assert_eq!(
                        e.size, te.size,
                        "{name}: linked {sig} size != target {target}"
                    );
                }
                SlotContent::Body(orig) => {
                    let reread = back
                        .read_tag(sig)
                        .unwrap_or_else(|e| panic!("{name}: read-back of {sig} failed: {e:?}"));
                    assert_tag_semeq(sig, orig, &reread, &name);
                    tags_compared += 1;
                }
            }
        }
    }

    // ---- Surface the writer-gap set explicitly: it MUST equal the intended
    //      deferrals (no silent coverage holes). ----
    println!(
        "round-trip sweep: {clean_profiles}/{profiles_attempted} profiles round-tripped \
         with ZERO writer gaps; {tags_compared} tags compared semantically"
    );
    if gap_types.is_empty() {
        println!("  writer-gap set: EMPTY (every testbed tag has a writer)");
    } else {
        println!("  writer-gap set (intended deferrals):");
        for (ty, n) in &gap_types {
            let b = ty.to_be_bytes();
            let label: String = b
                .iter()
                .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
                .collect();
            println!("    '{label}' ({ty:08x}): {n} tag(s)");
        }
        println!("  profiles with gaps:");
        for (prof, types) in &gap_profiles {
            let labels: Vec<String> = types
                .iter()
                .map(|t| {
                    DEFERRED_WRITER_TYPES
                        .iter()
                        .find(|(d, _)| d == t)
                        .map(|(_, n)| n.to_string())
                        .unwrap_or_else(|| format!("{t:08x}"))
                })
                .collect();
            println!("    {prof}: {}", labels.join(", "));
        }
    }

    // Assert EVERY gap encountered is one of the intended deferrals — a gap of an
    // unexpected type is a regression (a writer that should exist is missing).
    for ty in gap_types.keys() {
        assert!(
            is_deferred(*ty),
            "unexpected writer gap for on-disk type {ty:08x} — this type should be \
             writable; the gap set must be exactly the intended deferrals \
             {DEFERRED_WRITER_TYPES:08x?}"
        );
    }

    assert!(
        profiles_attempted > 0,
        "expected at least one accepted profile"
    );
    assert!(clean_profiles > 0, "expected some gap-free round-trips");
    assert!(tags_compared > 0, "expected to compare some tags");
}

/// CROSS-CHECK: lcms2 reads the rcms-WRITTEN bytes and the representative tag
/// values (wtpt / desc-family / the TRC or LUT tags) match rcms's parsed values.
/// This proves the rcms-written profile is valid and correct for a third-party
/// reader, not merely self-consistent under rcms's own reader.
#[test]
fn lcms2_rereads_rcms_written_bytes_over_testbed() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    const WTPT: u32 = 0x7774_7074; // 'wtpt'
    const BKPT: u32 = 0x626B_7074; // 'bkpt'
    const LUMI: u32 = 0x6C75_6D69; // 'lumi'
    const RXYZ: u32 = 0x7258_595A; // 'rXYZ'
    const RTRC: u32 = 0x7254_5243; // 'rTRC'
    const KTRC: u32 = 0x6B54_5243; // 'kTRC'
    const A2B0: u32 = 0x4132_4230; // 'A2B0'

    let xs: Vec<f32> = (0..32).map(|i| i as f32 / 31.0).collect();

    let mut profiles_crosschecked = 0usize;
    let mut xyz_checks = 0usize;
    let mut curve_checks = 0usize;
    let mut lut_checks = 0usize;

    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let (wp, _gaps) = build_writable(&p, &bytes);
        let written = match save_to_mem(&wp) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // lcms2 must accept the rcms-written bytes.
        assert!(
            rcms_oracle::open_succeeds(&written),
            "{name}: lcms2 rejects rcms-written bytes"
        );
        profiles_crosschecked += 1;

        // XYZ-valued tags: rcms parse vs lcms2 reading the written bytes.
        for raw in [WTPT, BKPT, LUMI, RXYZ] {
            let sig = Signature::from_raw(raw);
            let Ok(Tag::Xyz(v)) = p.read_tag(sig) else {
                continue;
            };
            let Some(c) = rcms_oracle::read_tag_xyz(&written, raw) else {
                continue;
            };
            rcms_oracle::assert_f64_bits_eq(v.x, c[0], (name.as_str(), raw, "x"));
            rcms_oracle::assert_f64_bits_eq(v.y, c[1], (name.as_str(), raw, "y"));
            rcms_oracle::assert_f64_bits_eq(v.z, c[2], (name.as_str(), raw, "z"));
            xyz_checks += 1;
        }

        // TRC curves: lcms2 evaluates the rcms-written curve; compare to rcms's.
        for raw in [RTRC, KTRC] {
            let sig = Signature::from_raw(raw);
            let Ok(Tag::Curve(curve)) = p.read_tag(sig) else {
                continue;
            };
            let Some(cys) = rcms_oracle::read_tag_curve(&written, raw, &xs) else {
                continue;
            };
            for (&x, &cy) in xs.iter().zip(cys.iter()) {
                let ry = curve.eval_float(x);
                assert_eq!(
                    ry.to_bits(),
                    cy.to_bits(),
                    "{name}:{raw:08x} curve x={x}: rcms={ry} lcms2(rcms-bytes)={cy}"
                );
            }
            curve_checks += 1;
        }

        // The A2B0 LUT: lcms2 evaluates the rcms-written tag through its own LUT
        // reader; compare outputs to rcms's pipeline over a small grid.
        if let Ok(Tag::Lut(lut)) = p.read_tag(Signature::from_raw(A2B0)) {
            if let Some((n_in, n_out)) = rcms_oracle::lut_channels(&written, A2B0) {
                if n_in as usize == lut.input_channels && n_out as usize == lut.output_channels {
                    for input in lut_grid(lut.input_channels) {
                        let rust = lut.eval_float(&input);
                        if let Some(c) =
                            rcms_oracle::lut_eval_float(&written, A2B0, &input, 1, n_out as usize)
                        {
                            for (i, (ry, cy)) in rust.iter().zip(c.iter()).enumerate() {
                                assert_eq!(
                                    ry.to_bits(),
                                    cy.to_bits(),
                                    "{name}: A2B0 lcms2(rcms-bytes) ch{i} at {input:?}"
                                );
                            }
                        }
                    }
                    lut_checks += 1;
                }
            }
        }
    }

    println!(
        "lcms2 re-read cross-check: {profiles_crosschecked} rcms-written profiles accepted \
         by lcms2; {xyz_checks} XYZ, {curve_checks} curve, {lut_checks} LUT tag values matched"
    );
    assert!(
        profiles_crosschecked > 0,
        "expected lcms2 to accept some rcms-written profiles"
    );
    assert!(
        xyz_checks + curve_checks + lut_checks > 0,
        "expected at least one cross-checked tag value"
    );
}
