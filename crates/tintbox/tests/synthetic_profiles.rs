//! Differential tests for the tag types with NO testbed coverage: NamedColor2
//! (`ncl2`), ProfileSequenceDesc (`pseq`), and ProfileSequenceId (`psid`). The
//! Little-CMS `testbed` carries none of these, so we hand-build a minimal, valid
//! ICC profile carrying exactly one such tag and assert lcms2 (`cmsOpenProfileFromMem`
//! + the oracle extractors) and tintbox `Profile::read_tag` decode it identically.
//!
//! The profile is the smallest thing both stacks accept: a 128-byte header with
//! the ICC magic, version 4.4, device class 0 (lcms2 explicitly allows zero), and
//! a one-entry tag directory. Tag data is 4-byte aligned, exactly as lcms2 writes.

use tintbox::profile::tag::Tag;
use tintbox::profile::Profile;
use tintbox::sig::Signature;

/// Build a minimal valid ICC profile carrying a single tag `(sig, payload)`. The
/// payload INCLUDES the 8-byte type base (type sig + reserved). Returns the full
/// profile bytes.
fn build_profile(tag_sig: u32, payload: &[u8]) -> Vec<u8> {
    let header_len = 128u32;
    let dir_len = 4 + 12; // count + one 12-byte entry
    let data_off = header_len + dir_len;
    // 4-byte align the tag data offset (lcms2 aligns tag data).
    let data_off = (data_off + 3) & !3;
    let total = data_off + payload.len() as u32;

    let mut b = vec![0u8; total as usize];
    // Header.
    b[0..4].copy_from_slice(&total.to_be_bytes()); // size
                                                   // version 4.4.0.0 at offset 8 (BCD-ish: 0x04400000).
    b[8..12].copy_from_slice(&0x0440_0000u32.to_be_bytes());
    // deviceClass @12 = 0 (allowed). colorSpace/pcs left 0.
    // magic 'acsp' @36.
    b[36..40].copy_from_slice(b"acsp");

    // Tag directory @128.
    b[128..132].copy_from_slice(&1u32.to_be_bytes()); // count
    b[132..136].copy_from_slice(&tag_sig.to_be_bytes());
    b[136..140].copy_from_slice(&data_off.to_be_bytes());
    b[140..144].copy_from_slice(&(payload.len() as u32).to_be_bytes());

    // Tag data.
    b[data_off as usize..].copy_from_slice(payload);
    b
}

fn type_base(sig: &[u8; 4]) -> Vec<u8> {
    let mut v = sig.to_vec();
    v.extend_from_slice(&[0, 0, 0, 0]);
    v
}

fn name32(s: &str) -> Vec<u8> {
    let mut b = [0u8; 32];
    b[..s.len()].copy_from_slice(s.as_bytes());
    b.to_vec()
}

/// A standalone `mluc` payload (no type base) with one BMP translation. Offset
/// bakes in the writer's `+8` (SizeOfHeader = 12 + 8 = 20, pool at 20 + 8 = 28).
fn mluc_payload(lang: &[u8; 2], country: &[u8; 2], text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut b = Vec::new();
    b.extend_from_slice(&1u32.to_be_bytes());
    b.extend_from_slice(&12u32.to_be_bytes());
    b.extend_from_slice(lang);
    b.extend_from_slice(country);
    b.extend_from_slice(&((units.len() * 2) as u32).to_be_bytes());
    b.extend_from_slice(&28u32.to_be_bytes());
    for u in units {
        b.extend_from_slice(&u.to_be_bytes());
    }
    b
}

#[test]
fn named_color2_matches_oracle() {
    // ncl2 payload: type base + vendorFlag + count + nDeviceCoords + prefix +
    // suffix + per-colour {name32, pcs[3], device[2]}.
    let mut payload = type_base(b"ncl2");
    payload.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // vendorFlag
    payload.extend_from_slice(&2u32.to_be_bytes()); // count
    payload.extend_from_slice(&2u32.to_be_bytes()); // nDeviceCoords
    payload.extend_from_slice(&name32("PRE")); // prefix
    payload.extend_from_slice(&name32("SUF")); // suffix
    for (nm, pcs, dev) in [
        ("Cyan", [0x0101u16, 0x0202, 0x0303], [0x1111u16, 0x2222]),
        ("Mag", [0x0404u16, 0x0505, 0x0606], [0x3333u16, 0x4444]),
    ] {
        payload.extend_from_slice(&name32(nm));
        for v in pcs {
            payload.extend_from_slice(&v.to_be_bytes());
        }
        for v in dev {
            payload.extend_from_slice(&v.to_be_bytes());
        }
    }

    let bytes = build_profile(0x6E63_6C32, &payload); // 'ncl2'
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic ncl2 profile"
    );

    let oracle = tintbox_oracle::read_tag_named_color2(&bytes, 0x6E63_6C32).expect("oracle ncl2");
    let p = Profile::open(&bytes).expect("tintbox open");
    let list = match p
        .read_tag(Signature::from_raw(0x6E63_6C32))
        .expect("tintbox ncl2")
    {
        Tag::NamedColor2(l) => l,
        other => panic!("expected NamedColor2, got {other:?}"),
    };

    // vendorFlag is discarded by lcms2; verify tintbox kept the raw on-disk value.
    assert_eq!(list.vendor_flag, 0x1234_5678);
    assert_eq!(list.prefix, oracle.prefix);
    assert_eq!(list.suffix, oracle.suffix);
    assert_eq!(list.colors.len(), oracle.colors.len());
    for (r, o) in list.colors.iter().zip(oracle.colors.iter()) {
        assert_eq!(r.name, o.name, "color name");
        assert_eq!(r.pcs, o.pcs, "color pcs");
        assert_eq!(r.device, o.device, "color device");
    }
}

#[test]
fn profile_sequence_desc_matches_oracle() {
    // pseq payload: type base + Count + one element {mfg, model, attr, tech,
    // mluc(Manufacturer), mluc(Model)}.
    let manu = mluc_payload(b"en", b"US", "MakerCo");
    let model = mluc_payload(b"de", b"DE", "Geraet");

    let mut payload = type_base(b"pseq");
    payload.extend_from_slice(&1u32.to_be_bytes()); // Count
    payload.extend_from_slice(&0x6D6E_6672u32.to_be_bytes()); // deviceMfg 'mnfr'
    payload.extend_from_slice(&0x6D6F_646Cu32.to_be_bytes()); // deviceModel 'modl'
    payload.extend_from_slice(&0x00AA_BB00_1122_3344u64.to_be_bytes()); // attributes
    payload.extend_from_slice(&0x6373_636Eu32.to_be_bytes()); // technology 'cscn'
    payload.extend_from_slice(&type_base(b"mluc"));
    payload.extend_from_slice(&manu);
    payload.extend_from_slice(&type_base(b"mluc"));
    payload.extend_from_slice(&model);

    let bytes = build_profile(0x7073_6571, &payload); // 'pseq'
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic pseq profile"
    );

    let oracle = tintbox_oracle::read_tag_seq_desc(&bytes, 0x7073_6571).expect("oracle pseq");
    let p = Profile::open(&bytes).expect("tintbox open");
    let items = match p
        .read_tag(Signature::from_raw(0x7073_6571))
        .expect("tintbox pseq")
    {
        Tag::ProfileSequenceDesc(v) => v,
        other => panic!("expected ProfileSequenceDesc, got {other:?}"),
    };

    assert_eq!(items.len(), oracle.len());
    for (r, o) in items.iter().zip(oracle.iter()) {
        assert_eq!(r.device_mfg.to_raw(), o.device_mfg, "deviceMfg");
        assert_eq!(r.device_model.to_raw(), o.device_model, "deviceModel");
        assert_eq!(r.attributes, o.attributes, "attributes");
        assert_eq!(r.technology.to_raw(), o.technology, "technology");
        assert_mlu_eq(&r.manufacturer, &o.manufacturer, "manufacturer");
        assert_mlu_eq(&r.model, &o.model, "model");
    }
}

#[test]
fn profile_sequence_id_matches_oracle() {
    // psid payload: type base + Count + position table + per-element {16-byte ID,
    // mluc description}. Offsets are tag-relative (offset 0 = the type base).
    let id0 = [0xAAu8; 16];
    let id1 = [0xBBu8; 16];
    let mlu0 = mluc_payload(b"en", b"US", "Alpha");
    let mlu1 = mluc_payload(b"fr", b"FR", "Beta");

    let mut elem0 = id0.to_vec();
    elem0.extend_from_slice(&type_base(b"mluc"));
    elem0.extend_from_slice(&mlu0);
    let mut elem1 = id1.to_vec();
    elem1.extend_from_slice(&type_base(b"mluc"));
    elem1.extend_from_slice(&mlu1);

    // Tag layout: [base 8][Count 4][2×{off,size}=16][elem0][elem1].
    let off0 = 8 + 4 + 16; // 28
    let off1 = off0 + elem0.len();

    let mut payload = type_base(b"psid");
    payload.extend_from_slice(&2u32.to_be_bytes()); // Count
    payload.extend_from_slice(&(off0 as u32).to_be_bytes());
    payload.extend_from_slice(&(elem0.len() as u32).to_be_bytes());
    payload.extend_from_slice(&(off1 as u32).to_be_bytes());
    payload.extend_from_slice(&(elem1.len() as u32).to_be_bytes());
    payload.extend_from_slice(&elem0);
    payload.extend_from_slice(&elem1);

    let bytes = build_profile(0x7073_6964, &payload); // 'psid'
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic psid profile"
    );

    let oracle = tintbox_oracle::read_tag_seq_id(&bytes, 0x7073_6964).expect("oracle psid");
    let p = Profile::open(&bytes).expect("tintbox open");
    let items = match p
        .read_tag(Signature::from_raw(0x7073_6964))
        .expect("tintbox psid")
    {
        Tag::ProfileSequenceId(v) => v,
        other => panic!("expected ProfileSequenceId, got {other:?}"),
    };

    assert_eq!(items.len(), oracle.len());
    for (r, o) in items.iter().zip(oracle.iter()) {
        assert_eq!(r.profile_id, o.profile_id, "profile id");
        assert_mlu_eq(&r.description, &o.description, "description");
    }
}

/// UTF-16BE bytes (no NUL).
fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

#[test]
fn dictionary_reclen32_matches_oracle() {
    // Testbed dicts are all record-length 16; this exercises the length-32 path
    // (DisplayName + DisplayValue MLUs) against lcms2. Two entries: entry 0 has
    // both display MLUs, entry 1 has neither (offset 0).
    let n0 = utf16be("k0");
    let v0 = utf16be("val0");
    let dn0 = mluc_payload(b"en", b"US", "Name0");
    let dv0 = mluc_payload(b"en", b"US", "Value0");
    let n1 = utf16be("k1");
    let v1 = utf16be("val1");

    // Layout: [base 8][Count 4][Length 4] = 16; directory: 2 rows × 4 cells × 8 =
    // 64; data starts at tag-offset 80.
    let data_start = 16 + 64;
    let o_n0 = data_start as u32;
    let o_v0 = o_n0 + n0.len() as u32;
    let o_dn0 = o_v0 + v0.len() as u32;
    let o_dv0 = o_dn0 + dn0.len() as u32;
    let o_n1 = o_dv0 + dv0.len() as u32;
    let o_v1 = o_n1 + n1.len() as u32;

    let cell = |o: u32, s: usize| {
        let mut b = Vec::new();
        b.extend_from_slice(&o.to_be_bytes());
        b.extend_from_slice(&(s as u32).to_be_bytes());
        b
    };

    let mut payload = type_base(b"dict");
    payload.extend_from_slice(&2u32.to_be_bytes()); // Count
    payload.extend_from_slice(&32u32.to_be_bytes()); // Length
                                                     // row 0
    payload.extend_from_slice(&cell(o_n0, n0.len()));
    payload.extend_from_slice(&cell(o_v0, v0.len()));
    payload.extend_from_slice(&cell(o_dn0, dn0.len()));
    payload.extend_from_slice(&cell(o_dv0, dv0.len()));
    // row 1 (no display MLUs → offset 0, size 0)
    payload.extend_from_slice(&cell(o_n1, n1.len()));
    payload.extend_from_slice(&cell(o_v1, v1.len()));
    payload.extend_from_slice(&cell(0, 0));
    payload.extend_from_slice(&cell(0, 0));
    // data
    payload.extend_from_slice(&n0);
    payload.extend_from_slice(&v0);
    payload.extend_from_slice(&dn0);
    payload.extend_from_slice(&dv0);
    payload.extend_from_slice(&n1);
    payload.extend_from_slice(&v1);

    // lcms2 (and tintbox) carry the dict TYPE under the `meta` TAG; there is no
    // standalone 'dict' tag in the supported-tags table.
    let meta_tag = 0x6D65_7461u32; // 'meta'
    let bytes = build_profile(meta_tag, &payload);
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic meta/dict profile"
    );

    let mut oracle = tintbox_oracle::read_tag_dict(&bytes, meta_tag).expect("oracle dict");
    oracle.entries.reverse(); // lcms2 enumerates reverse of disk order.

    let p = Profile::open(&bytes).expect("tintbox open");
    let dict = match p
        .read_tag(Signature::from_raw(meta_tag))
        .expect("tintbox dict")
    {
        Tag::Dict(d) => d,
        other => panic!("expected Dict, got {other:?}"),
    };

    assert_eq!(dict.entries.len(), oracle.entries.len());
    for (r, o) in dict.entries.iter().zip(oracle.entries.iter()) {
        assert_eq!(r.name, o.name, "name");
        assert_eq!(r.value, o.value, "value");
        match (&r.display_name, &o.display_name) {
            (None, None) => {}
            (Some(a), Some(b)) => assert_mlu_eq(a, b, "display_name"),
            _ => panic!("display_name presence mismatch"),
        }
        match (&r.display_value, &o.display_value) {
            (None, None) => {}
            (Some(a), Some(b)) => assert_mlu_eq(a, b, "display_value"),
            _ => panic!("display_value presence mismatch"),
        }
    }
}

/// 256 sample points x in [0, 1] (inclusive endpoints) for tone-curve diffs.
fn sample_xs() -> Vec<f32> {
    (0..256).map(|i| i as f32 / 255.0).collect()
}

/// A `vcgt` formula-variant payload (no type base): TagType=1, then per channel
/// (3) gamma/min/max each as s15Fixed16. lcms2 builds an ICC type-5 parametric
/// curve `Y = (max-min)·X^gamma + min` per channel.
#[test]
fn vcgt_formula_matches_oracle() {
    let s15f16 = |v: f64| ((v * 65536.0).round() as i32) as u32;
    // Per channel: (gamma, min, max). Distinct, in-range values that are exact in
    // 15.16 so the on-disk encoding has no rounding the oracle wouldn't see too.
    let chans = [(2.2, 0.0, 1.0), (1.8, 0.0625, 0.9375), (2.4, 0.125, 0.875)];

    let mut payload = type_base(b"vcgt");
    payload.extend_from_slice(&1u32.to_be_bytes()); // GammaType = formula
    for (g, mn, mx) in chans {
        payload.extend_from_slice(&s15f16(g).to_be_bytes());
        payload.extend_from_slice(&s15f16(mn).to_be_bytes());
        payload.extend_from_slice(&s15f16(mx).to_be_bytes());
    }

    let vcgt_tag = 0x7663_6774u32; // 'vcgt'
    let bytes = build_profile(vcgt_tag, &payload);
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic formula-vcgt profile"
    );

    let xs = sample_xs();
    let oracle = tintbox_oracle::read_tag_vcgt(&bytes, vcgt_tag, &xs).expect("oracle vcgt formula");

    let p = Profile::open(&bytes).expect("tintbox open");
    let curves = match p
        .read_tag(Signature::from_raw(vcgt_tag))
        .expect("tintbox vcgt")
    {
        Tag::Vcgt(c) => c,
        other => panic!("expected Vcgt, got {other:?}"),
    };
    assert_eq!(curves.len(), 3);
    for (ch, oys) in oracle.iter().enumerate() {
        for (i, (&x, &cy)) in xs.iter().zip(oys.iter()).enumerate() {
            let ry = curves[ch].eval_float(x);
            assert_eq!(
                ry.to_bits(),
                cy.to_bits(),
                "vcgt formula ch{ch} sample[{i}] x={x}: rust={ry} lcms2={cy}"
            );
        }
    }
}

/// A `vcgt` table-variant payload with 1-byte entries (the FROM_8_TO_16 scaling
/// path the testbed's 2-byte profiles never exercise). 3 channels × 16 entries.
#[test]
fn vcgt_table_1byte_matches_oracle() {
    let n_elems = 16u16;
    let mut payload = type_base(b"vcgt");
    payload.extend_from_slice(&0u32.to_be_bytes()); // GammaType = table
    payload.extend_from_slice(&3u16.to_be_bytes()); // nChannels
    payload.extend_from_slice(&n_elems.to_be_bytes()); // nElems
    payload.extend_from_slice(&1u16.to_be_bytes()); // nBytes = 1
    for ch in 0..3u32 {
        for i in 0..n_elems as u32 {
            // A distinct ramp per channel, full 0..255 range.
            let v = ((i * 255 / (n_elems as u32 - 1)) + ch * 3).min(255) as u8;
            payload.push(v);
        }
    }

    let vcgt_tag = 0x7663_6774u32;
    let bytes = build_profile(vcgt_tag, &payload);
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic 1-byte-table vcgt profile"
    );

    let xs = sample_xs();
    let oracle = tintbox_oracle::read_tag_vcgt(&bytes, vcgt_tag, &xs).expect("oracle vcgt table");

    let p = Profile::open(&bytes).expect("tintbox open");
    let curves = match p
        .read_tag(Signature::from_raw(vcgt_tag))
        .expect("tintbox vcgt")
    {
        Tag::Vcgt(c) => c,
        other => panic!("expected Vcgt, got {other:?}"),
    };
    for (ch, oys) in oracle.iter().enumerate() {
        for (i, (&x, &cy)) in xs.iter().zip(oys.iter()).enumerate() {
            let ry = curves[ch].eval_float(x);
            assert_eq!(
                ry.to_bits(),
                cy.to_bits(),
                "vcgt 1-byte table ch{ch} sample[{i}] x={x}: rust={ry} lcms2={cy}"
            );
        }
    }
}

/// A `bfd ` (UcrBg) payload: CountUcr u32 + UCR u16 table, CountBg u32 + BG u16
/// table, then a trailing ASCII description (no NUL on disk — the tag size bounds
/// it). lcms2 builds 16-bit tabulated Ucr/Bg curves and an MLU-backed Desc.
#[test]
fn ucrbg_matches_oracle() {
    let ucr: [u16; 5] = [0, 0x4000, 0x8000, 0xC000, 0xFFFF];
    let bg: [u16; 3] = [0xFFFF, 0x8000, 0x0000];
    let desc = "Synthetic UCR/BG method";

    let mut payload = type_base(b"bfd ");
    payload.extend_from_slice(&(ucr.len() as u32).to_be_bytes());
    for v in ucr {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(&(bg.len() as u32).to_be_bytes());
    for v in bg {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(desc.as_bytes());

    let bfd_tag = 0x6266_6420u32; // 'bfd '
    let bytes = build_profile(bfd_tag, &payload);
    assert!(
        tintbox_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic bfd/UcrBg profile"
    );

    let xs = sample_xs();
    let oracle = tintbox_oracle::read_tag_ucrbg(&bytes, bfd_tag, &xs).expect("oracle ucrbg");

    let p = Profile::open(&bytes).expect("tintbox open");
    let (r_ucr, r_bg, r_desc) = match p
        .read_tag(Signature::from_raw(bfd_tag))
        .expect("tintbox ucrbg")
    {
        Tag::UcrBg { ucr, bg, desc } => (ucr, bg, desc),
        other => panic!("expected UcrBg, got {other:?}"),
    };

    assert_eq!(r_desc, oracle.desc, "desc");
    for (i, (&x, &cy)) in xs.iter().zip(oracle.ucr.iter()).enumerate() {
        let ry = r_ucr.eval_float(x);
        assert_eq!(ry.to_bits(), cy.to_bits(), "ucr sample[{i}] x={x}");
    }
    for (i, (&x, &cy)) in xs.iter().zip(oracle.bg.iter()).enumerate() {
        let ry = r_bg.eval_float(x);
        assert_eq!(ry.to_bits(), cy.to_bits(), "bg sample[{i}] x={x}");
    }
}

/// Compare an tintbox MLU against an oracle MLU translation-by-translation.
fn assert_mlu_eq(rust: &tintbox::profile::tag::Mlu, oracle: &tintbox_oracle::OracleMlu, ctx: &str) {
    assert_eq!(
        rust.entries.len(),
        oracle.entries.len(),
        "{ctx} translation count"
    );
    for (i, (r, o)) in rust.entries.iter().zip(oracle.entries.iter()).enumerate() {
        assert_eq!(r.language, o.language, "{ctx}[{i}] language");
        assert_eq!(r.country, o.country, "{ctx}[{i}] country");
        assert_eq!(r.text, o.text, "{ctx}[{i}] text");
    }
}
