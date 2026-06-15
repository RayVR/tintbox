//! Differential tests for the MPE (Multi-Process Element) tag reader
//! (`Type_MPE_Read`), bit-for-bit against lcms2.
//!
//! Two layers:
//!   1. Over every `vendor/Little-CMS/testbed/*.icc` carrying an `mpet` tag, read
//!      the tag via tintbox `read_tag` -> `Tag::Lut(Pipeline)`, build the SAME
//!      pipeline via lcms2 `cmsReadTag`, and evaluate a bounded input grid through
//!      BOTH stacks (`eval_float` / `eval_16` vs `cmsPipelineEvalFloat` / `Eval16`),
//!      asserting bit-exact (f32::to_bits / u16) at every sample.
//!   2. A hand-built synthetic profile whose single `D2B0` tag is an `mpet`
//!      exercising EACH element flavour (curve set, P×Q float matrix, float CLUT,
//!      plus `bACS`/`eACS` no-op markers), diffed against lcms2 the same way — so
//!      every MPE element type is covered even if the testbed's mpet does not hit
//!      all of them.

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::profile::{Profile, Tag};

const TY_MPE: u32 = 0x6D70_6574; // 'mpet'

// lcms2 segmented-curve / element signatures (include/lcms2.h).
const SIG_SEGMENTED_CURVE: u32 = 0x6375_7266; // 'curf'
const SIG_FORMULA_CURVE_SEG: u32 = 0x7061_7266; // 'parf'
const SIG_SAMPLED_CURVE_SEG: u32 = 0x7361_6D66; // 'samf'
const SIG_CURVE_SET_ELEM: u32 = 0x6376_7374; // 'cvst'
const SIG_MATRIX_ELEM: u32 = 0x6D61_7466; // 'matf'
const SIG_CLUT_ELEM: u32 = 0x636C_7574; // 'clut'
const SIG_BACS_ELEM: u32 = 0x6241_4353; // 'bACS'
const SIG_EACS_ELEM: u32 = 0x6541_4353; // 'eACS'
const SIG_MPE_TYPE: u32 = 0x6D70_6574; // 'mpet'
const TAG_D2B0: u32 = 0x4432_4230; // 'D2B0' (cmsSigDToB0Tag)

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

/// A coarse per-channel 16-bit input sweep with a bounded total sample count
/// (identical strategy to the LUT differential).
fn input_grid_u16(n_in: usize) -> Vec<Vec<u16>> {
    let levels: usize = match n_in {
        1 => 257,
        2 => 17,
        3 => 9,
        4 => 6,
        _ => 3,
    };
    let level_val = |i: usize| -> u16 {
        if levels == 1 {
            0
        } else {
            ((i as u64 * 0xFFFF) / (levels as u64 - 1)) as u16
        }
    };
    let total = levels.pow(n_in as u32);
    let mut out = Vec::with_capacity(total);
    for n in 0..total {
        let mut sample = Vec::with_capacity(n_in);
        let mut rem = n;
        for _ in 0..n_in {
            sample.push(level_val(rem % levels));
            rem /= levels;
        }
        out.push(sample);
    }
    out
}

/// Read an `mpet` tag from `bytes` (sig `raw`) through both stacks and diff a
/// bounded input grid in the 16-bit and float domains. Returns the sample count.
fn diff_mpet(bytes: &[u8], raw: u32, name: &str) -> usize {
    let p = Profile::open(bytes).expect("tintbox open");
    let sig = tintbox::sig::Signature::from_raw(raw);

    let pipeline = match p.read_tag(sig).expect("rust mpet") {
        Tag::Lut(pl) => pl,
        other => panic!("{name}:{raw:08x} expected Lut, got {other:?}"),
    };

    let (c_in, c_out) = tintbox_oracle::lut_channels(bytes, raw).expect("oracle lut channels");
    assert_eq!(
        pipeline.input_channels, c_in as usize,
        "{name}:{raw:08x} input channels"
    );
    assert_eq!(
        pipeline.output_channels, c_out as usize,
        "{name}:{raw:08x} output channels"
    );

    let grid = input_grid_u16(pipeline.input_channels);
    let n_samples = grid.len();
    let n_in = pipeline.input_channels;
    let n_out = pipeline.output_channels;
    let flat_u16: Vec<u16> = grid.iter().flatten().copied().collect();

    // ---- 16-bit domain ----
    let oracle16 =
        tintbox_oracle::lut_eval16(bytes, raw, &flat_u16, n_samples, n_out).expect("oracle eval16");
    for (s, sample) in grid.iter().enumerate() {
        let rust = pipeline.eval_16(sample);
        let c = &oracle16[s * n_out..(s + 1) * n_out];
        assert_eq!(
            rust.as_slice(),
            c,
            "{name}:{raw:08x} eval16 mismatch at sample {s} in={sample:?}"
        );
    }

    // ---- float domain ----
    let flat_f32: Vec<f32> = flat_u16.iter().map(|&v| v as f32 / 65535.0_f32).collect();
    let oraclef = tintbox_oracle::lut_eval_float(bytes, raw, &flat_f32, n_samples, n_out)
        .expect("oracle evalf");
    for s in 0..n_samples {
        let sample_f: Vec<f32> = flat_f32[s * n_in..(s + 1) * n_in].to_vec();
        let rust = pipeline.eval_float(&sample_f);
        let c = &oraclef[s * n_out..(s + 1) * n_out];
        for (j, (rv, cv)) in rust.iter().zip(c.iter()).enumerate() {
            assert_eq!(
                rv.to_bits(),
                cv.to_bits(),
                "{name}:{raw:08x} eval_float mismatch at sample {s} chan {j}: \
                 rust={rv} lcms2={cv} in={sample_f:?}"
            );
        }
    }

    n_samples
}

#[test]
fn mpet_tag_pipelines_match_oracle_over_testbed() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut mpet_tags = 0usize;
    let mut positive_tags = 0usize;
    let mut negative_tags = 0usize;
    let mut profiles_with_mpet = 0usize;
    let mut total_samples = 0usize;

    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();

        if !tintbox_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let mut hit_here = false;
        for sig in p.tags().collect::<Vec<_>>() {
            let raw = sig.to_raw();
            let ty = match tintbox_oracle::tag_true_type(&bytes, raw) {
                Some(t) => t,
                None => continue,
            };
            if ty != TY_MPE {
                continue;
            }
            mpet_tags += 1;
            hit_here = true;

            // lcms2's own `Type_MPE_Read` may reject a (deliberately malformed)
            // mpet tag — `cmsReadTag` then returns NULL and `lut_channels` is None.
            // In that case tintbox must ALSO reject the tag (with a non-Unsupported
            // error: the type IS dispatched, the data is just bad). When lcms2
            // does return a pipeline, the two must be bit-identical.
            let rust = p.read_tag(sig);
            match tintbox_oracle::lut_channels(&bytes, raw) {
                Some(_) => {
                    assert!(
                        matches!(rust, Ok(Tag::Lut(_))),
                        "{name}:{raw:08x} lcms2 read the mpet pipeline but tintbox did not: {rust:?}"
                    );
                    total_samples += diff_mpet(&bytes, raw, &name);
                    positive_tags += 1;
                }
                None => {
                    assert!(
                        rust.is_err(),
                        "{name}:{raw:08x} lcms2 rejected the mpet tag but tintbox accepted it: {rust:?}"
                    );
                    assert!(
                        !matches!(rust, Err(tintbox::error::Error::Unsupported(_))),
                        "{name}:{raw:08x} mpet must be dispatched (not Unsupported), got {rust:?}"
                    );
                    negative_tags += 1;
                }
            }
        }
        if hit_here {
            profiles_with_mpet += 1;
        }
    }

    println!(
        "testbed MPE diff: {mpet_tags} mpet tags ({positive_tags} readable, \
         {negative_tags} rejected by lcms2 too) over {profiles_with_mpet} profiles; \
         {total_samples} input samples evaluated (each in both 16-bit and float domains)"
    );
    assert!(
        mpet_tags > 0,
        "expected at least one mpet tag in the testbed"
    );
}

// ---- Synthetic mpet exercising every element flavour ----

fn be_u16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be_u32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn be_f32(v: f32) -> [u8; 4] {
    v.to_bits().to_be_bytes()
}

/// Build an embedded segmented-curve blob: a 3-segment curve with a parametric
/// edge segment, a sampled middle segment, and a parametric tail — so the sampled
/// implicit-point fix-up is exercised.
fn segmented_curve_blob() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&be_u32(SIG_SEGMENTED_CURVE));
    b.extend_from_slice(&be_u32(0)); // reserved
    b.extend_from_slice(&be_u16(3)); // nSegments
    b.extend_from_slice(&be_u16(0)); // reserved
                                     // 2 break-points (nSegments - 1).
    b.extend_from_slice(&be_f32(0.0));
    b.extend_from_slice(&be_f32(1.0));

    // Seg 0: formula type 0 (params {4}): gamma form a*x^? ... ICC type 0 reads 4
    // params. Use a simple linear-ish formula.
    b.extend_from_slice(&be_u32(SIG_FORMULA_CURVE_SEG));
    b.extend_from_slice(&be_u32(0));
    b.extend_from_slice(&be_u16(0)); // type 0 -> parametric 6
    b.extend_from_slice(&be_u16(0));
    for v in [1.0f32, 1.0, 0.0, 0.0] {
        b.extend_from_slice(&be_f32(v));
    }

    // Seg 1: sampled (5 stored points; the 6th implicit first point is filled).
    b.extend_from_slice(&be_u32(SIG_SAMPLED_CURVE_SEG));
    b.extend_from_slice(&be_u32(0));
    b.extend_from_slice(&be_u32(5)); // count (stored points)
    for v in [0.1f32, 0.3, 0.5, 0.7, 0.95] {
        b.extend_from_slice(&be_f32(v));
    }

    // Seg 2: formula type 0 again.
    b.extend_from_slice(&be_u32(SIG_FORMULA_CURVE_SEG));
    b.extend_from_slice(&be_u32(0));
    b.extend_from_slice(&be_u16(0));
    b.extend_from_slice(&be_u16(0));
    for v in [1.0f32, 1.0, 0.0, 0.0] {
        b.extend_from_slice(&be_f32(v));
    }

    b
}

/// Build a curve-set element (`cvst`) of `n` identical segmented curves, returned
/// WITHOUT the 8-byte element base (the caller's position table addresses it).
fn curve_set_element(n: u16) -> Vec<u8> {
    let curve = segmented_curve_blob();
    let mut body = Vec::new();
    body.extend_from_slice(&be_u16(n)); // InputChans
    body.extend_from_slice(&be_u16(n)); // OutputChans

    // Position table: n (offset, size) pairs relative to BaseOffset (the element
    // base, i.e. 8 bytes before `body`). Directory starts after the 4-byte header.
    let header = 8u32 /* element base */ + 4 /* in/out chans */;
    let dir_len = (n as u32) * 8;
    let mut data_off = header + dir_len;
    let mut dir = Vec::new();
    let mut data = Vec::new();
    for _ in 0..n {
        dir.extend_from_slice(&be_u32(data_off));
        dir.extend_from_slice(&be_u32(curve.len() as u32));
        data.extend_from_slice(&curve);
        data_off += curve.len() as u32;
    }
    body.extend_from_slice(&dir);
    body.extend_from_slice(&data);
    body
}

/// Build a P×Q float-matrix element (`matf`) body (no element base).
fn matrix_element(input: u16, output: u16, m: &[f32], offsets: &[f32]) -> Vec<u8> {
    assert_eq!(m.len(), (input as usize) * (output as usize));
    assert_eq!(offsets.len(), output as usize);
    let mut b = Vec::new();
    b.extend_from_slice(&be_u16(input));
    b.extend_from_slice(&be_u16(output));
    for &v in m {
        b.extend_from_slice(&be_f32(v));
    }
    for &v in offsets {
        b.extend_from_slice(&be_f32(v));
    }
    b
}

/// Build a float-CLUT element (`clut`) body (no element base).
fn clut_element(input: u16, output: u16, grid: &[u8], table: &[f32]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&be_u16(input));
    b.extend_from_slice(&be_u16(output));
    let mut dims = [0u8; 16];
    dims[..grid.len()].copy_from_slice(grid);
    b.extend_from_slice(&dims);
    for &v in table {
        b.extend_from_slice(&be_f32(v));
    }
    b
}

/// Assemble a full `mpet` tag payload (INCLUDING the 8-byte type base) from a list
/// of `(element_sig, element_body)` pairs. The element body excludes the 8-byte
/// element base (sig + reserved), which this writer prepends.
fn build_mpet_payload(input: u16, output: u16, elements: &[(u32, Vec<u8>)]) -> Vec<u8> {
    // The tag payload = type base (8) + header (in/out/count = 8) + position table.
    let n = elements.len() as u32;
    let header = 8u32 + 8;
    let dir_len = n * 8;
    let mut data_off = header + dir_len; // relative to BaseOffset (the type base start)

    let mut full_elements = Vec::new();
    let mut dir = Vec::new();
    for (sig, body) in elements {
        let mut elem = Vec::new();
        elem.extend_from_slice(&be_u32(*sig));
        elem.extend_from_slice(&be_u32(0)); // reserved
        elem.extend_from_slice(body);
        dir.extend_from_slice(&be_u32(data_off));
        dir.extend_from_slice(&be_u32(elem.len() as u32));
        data_off += elem.len() as u32;
        full_elements.extend_from_slice(&elem);
    }

    let mut b = Vec::new();
    b.extend_from_slice(&be_u32(SIG_MPE_TYPE)); // type base sig
    b.extend_from_slice(&be_u32(0)); // reserved
    b.extend_from_slice(&be_u16(input));
    b.extend_from_slice(&be_u16(output));
    b.extend_from_slice(&be_u32(n));
    b.extend_from_slice(&dir);
    b.extend_from_slice(&full_elements);
    b
}

/// Build a minimal valid ICC profile carrying a single tag `(sig, payload)`. The
/// payload INCLUDES the 8-byte type base.
fn build_profile(tag_sig: u32, payload: &[u8]) -> Vec<u8> {
    let header_len = 128u32;
    let dir_len = 4 + 12;
    let data_off = header_len + dir_len;
    let data_off = (data_off + 3) & !3;
    let total = data_off + payload.len() as u32;

    let mut b = vec![0u8; total as usize];
    b[0..4].copy_from_slice(&total.to_be_bytes());
    b[8..12].copy_from_slice(&0x0440_0000u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[128..132].copy_from_slice(&1u32.to_be_bytes());
    b[132..136].copy_from_slice(&tag_sig.to_be_bytes());
    b[136..140].copy_from_slice(&data_off.to_be_bytes());
    b[140..144].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    b[data_off as usize..].copy_from_slice(payload);
    b
}

#[test]
fn synthetic_mpet_all_elements_match_oracle() {
    // A 3->3 pipeline: bACS, curve-set(3), 3x3 matrix+offset, 2-grid float CLUT,
    // eACS. Every element flavour is present.
    let curves = curve_set_element(3);
    let matrix = matrix_element(
        3,
        3,
        &[0.9, 0.05, 0.05, 0.05, 0.9, 0.05, 0.05, 0.05, 0.9],
        &[0.01, 0.02, 0.03],
    );
    // 2x2x2 grid float CLUT, 3 outputs per node (24 floats).
    let mut table = Vec::with_capacity(24);
    for i in 0..8 {
        table.push(((i & 1) as f32) * 0.8 + 0.1);
        table.push((((i >> 1) & 1) as f32) * 0.8 + 0.1);
        table.push((((i >> 2) & 1) as f32) * 0.8 + 0.1);
    }
    let clut = clut_element(3, 3, &[2, 2, 2], &table);

    let elements = vec![
        (SIG_BACS_ELEM, Vec::new()),
        (SIG_CURVE_SET_ELEM, curves),
        (SIG_MATRIX_ELEM, matrix),
        (SIG_CLUT_ELEM, clut),
        (SIG_EACS_ELEM, Vec::new()),
    ];

    let payload = build_mpet_payload(3, 3, &elements);
    let profile = build_profile(TAG_D2B0, &payload);

    assert!(
        tintbox_oracle::open_succeeds(&profile),
        "lcms2 must accept the synthetic mpet profile"
    );
    // The on-disk type must be mpet.
    assert_eq!(
        tintbox_oracle::tag_true_type(&profile, TAG_D2B0),
        Some(TY_MPE),
        "synthetic D2B0 tag must be on-disk type mpet"
    );

    let samples = diff_mpet(&profile, TAG_D2B0, "synthetic");
    println!("synthetic mpet (curve+matrix+clut+bACS/eACS): {samples} samples diffed bit-exact");
    assert!(samples > 0);
}

#[test]
fn synthetic_mpet_nonsquare_matrix_matches_oracle() {
    // A 2->4 pipeline: a 2-channel curve set, then a 4x2 float matrix (arbitrary
    // P=2, Q=4) with a 4-element offset. Exercises the non-square matrix path the
    // MPE spec emphasises (P x Q + Q float32 elements).
    let curves = curve_set_element(2);
    #[rustfmt::skip]
    let m = [
        0.7, 0.3,
        0.1, 0.9,
        0.5, 0.5,
        0.25, 0.8,
    ];
    let matrix = matrix_element(2, 4, &m, &[0.0, 0.05, -0.02, 0.1]);

    let elements = vec![(SIG_CURVE_SET_ELEM, curves), (SIG_MATRIX_ELEM, matrix)];

    let payload = build_mpet_payload(2, 4, &elements);
    let profile = build_profile(TAG_D2B0, &payload);

    assert!(
        tintbox_oracle::open_succeeds(&profile),
        "lcms2 must accept the non-square synthetic mpet profile"
    );
    assert_eq!(
        tintbox_oracle::tag_true_type(&profile, TAG_D2B0),
        Some(TY_MPE)
    );
    assert_eq!(
        tintbox_oracle::lut_channels(&profile, TAG_D2B0),
        Some((2, 4)),
        "non-square mpet must read as 2->4"
    );

    let samples = diff_mpet(&profile, TAG_D2B0, "synthetic-nonsquare");
    println!("synthetic mpet (2->4 non-square matrix): {samples} samples diffed bit-exact");
    assert!(samples > 0);
}

/// Adversarial CLUT grid: an `mpet` with a 4-input float CLUT whose grid dims
/// are `[255,255,255,255]`. The node count `255^4 = 4_228_250_625` FITS in `u32`
/// (so a naive `checked_mul` chain would accept it and try to allocate ~16 GiB),
/// but it exceeds lcms2's `CubeSize` final `UINT_MAX/15` ceiling, so lcms2 itself
/// rejects the tag (`cmsReadTag` -> NULL). tintbox must match: reject WITHOUT
/// allocating, via the same `CubeSize` parity guard. We cross-check against the
/// oracle: tintbox rejects exactly when lcms2 does.
#[test]
fn mpet_adversarial_clut_grid_rejected_like_oracle() {
    // 4->1 float CLUT, grid [255,255,255,255]. The table bytes are NOT supplied
    // in full (both stacks reject on the entry-count guard before reading them).
    let clut = clut_element(4, 1, &[255, 255, 255, 255], &[]);
    let elements = vec![(SIG_CLUT_ELEM, clut)];
    let payload = build_mpet_payload(4, 1, &elements);
    let profile = build_profile(TAG_D2B0, &payload);

    // The profile must open; the on-disk type must be mpet.
    assert!(
        tintbox_oracle::open_succeeds(&profile),
        "lcms2 must open the profile (the tag is read lazily)"
    );
    assert_eq!(
        tintbox_oracle::tag_true_type(&profile, TAG_D2B0),
        Some(TY_MPE)
    );

    // Oracle: lcms2's CubeSize rejects (255^4 > UINT_MAX/15), so cmsReadTag -> NULL.
    let oracle_reads = tintbox_oracle::tag_read_succeeds(&profile, TAG_D2B0);
    assert!(
        !oracle_reads,
        "expected lcms2 to REJECT the adversarial [255;4] grid (CubeSize guard)"
    );

    // tintbox must also reject (and must not be Unsupported: the type IS dispatched).
    let p = Profile::open(&profile).expect("tintbox open");
    let sig = tintbox::sig::Signature::from_raw(TAG_D2B0);
    let res = p.read_tag(sig);
    assert!(
        res.is_err(),
        "tintbox accepted the adversarial [255;4] CLUT grid but lcms2 rejected it: {res:?}"
    );
    assert!(
        !matches!(res, Err(tintbox::error::Error::Unsupported(_))),
        "adversarial mpet must be dispatched (not Unsupported), got {res:?}"
    );
    println!("adversarial mpet [255;4] CLUT grid rejected by BOTH tintbox and lcms2");
}

/// Reachability: the `mpet` on-disk type no longer returns `Error::Unsupported`.
#[test]
fn mpet_type_no_longer_unsupported() {
    // Testbed coverage.
    let mut checked = 0usize;
    for path in testbed_icc() {
        let bytes = fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        if !tintbox_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for sig in p.tags().collect::<Vec<_>>() {
            let raw = sig.to_raw();
            if tintbox_oracle::tag_true_type(&bytes, raw) != Some(TY_MPE) {
                continue;
            }
            // The mpet on-disk type must DISPATCH (never `Unsupported`). It may
            // still legitimately fail on a malformed tag (lcms2 itself does), so we
            // assert "not Unsupported" rather than "Ok".
            let res = p.read_tag(sig);
            assert!(
                !matches!(res, Err(tintbox::error::Error::Unsupported(_))),
                "{name}:{raw:08x} mpet tag must be dispatched, got {res:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "expected at least one mpet tag in testbed");
}
