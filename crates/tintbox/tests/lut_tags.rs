//! Differential tests for the LUT8 / LUT16 tag readers (`Type_LUT8_Read` /
//! `Type_LUT16_Read`), bit-for-bit against lcms2.
//!
//! For every `vendor/Little-CMS/testbed/*.icc` carrying an `mft1` or `mft2`
//! tag, read the tag via tintbox `read_tag` -> `Tag::Lut(Pipeline)`, build the SAME
//! pipeline via lcms2 `cmsReadTag`, and evaluate a grid of input colours through
//! BOTH stacks (`eval_float`/`eval_16` vs `cmsPipelineEvalFloat`/`Eval16`),
//! asserting bit-exact (f32::to_bits / u16) at every sample.

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::profile::{Profile, Tag};

const TY_LUT8: u32 = 0x6D66_7431; // 'mft1'
const TY_LUT16: u32 = 0x6D66_7432; // 'mft2'
const TY_LUT_A2B: u32 = 0x6D41_4220; // 'mAB '
const TY_LUT_B2A: u32 = 0x6D42_4120; // 'mBA '

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

/// A coarse per-channel input sweep with a bounded sample count.
///
/// For `n_in` input channels we pick `levels` grid points per channel from the
/// full 16-bit range (inclusive endpoints), and take the Cartesian product. The
/// number of grid points is reduced as `n_in` grows so the total stays bounded
/// (the CLUT/curve interpolation is the same routine at every node, so a coarse
/// sweep over the whole domain exercises the full code path).
fn input_grid_u16(n_in: usize) -> Vec<Vec<u16>> {
    let levels: usize = match n_in {
        1 => 257,
        2 => 17,
        3 => 9,
        4 => 6,
        _ => 3,
    };
    // The exact 16-bit value for grid index i of `levels` (0 -> 0, last -> 0xFFFF).
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

#[test]
fn lut_tag_pipelines_match_oracle_over_testbed() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut mft1_tags = 0usize;
    let mut mft2_tags = 0usize;
    let mut profiles_with_lut = 0usize;
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
            if ty != TY_LUT8 && ty != TY_LUT16 {
                continue;
            }

            // tintbox: read the tag into a Pipeline.
            let pipeline = match p.read_tag(sig).expect("rust lut") {
                Tag::Lut(pl) => pl,
                other => panic!("{name}:{raw:08x} expected Lut, got {other:?}"),
            };

            // lcms2: the pipeline channel counts must agree.
            let (c_in, c_out) =
                tintbox_oracle::lut_channels(&bytes, raw).expect("oracle lut channels");
            assert_eq!(
                pipeline.input_channels, c_in as usize,
                "{name}:{raw:08x} input channels"
            );
            assert_eq!(
                pipeline.output_channels, c_out as usize,
                "{name}:{raw:08x} output channels"
            );

            // Build a bounded input grid and flatten for the oracle call.
            let grid = input_grid_u16(pipeline.input_channels);
            let n_samples = grid.len();
            let n_in = pipeline.input_channels;
            let n_out = pipeline.output_channels;
            let flat_u16: Vec<u16> = grid.iter().flatten().copied().collect();

            // ---- 16-bit domain ----
            let oracle16 = tintbox_oracle::lut_eval16(&bytes, raw, &flat_u16, n_samples, n_out)
                .expect("oracle lut eval16");
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
            // From16ToFloat the same grid so both stacks see identical inputs.
            let flat_f32: Vec<f32> = flat_u16.iter().map(|&v| v as f32 / 65535.0_f32).collect();
            let oraclef = tintbox_oracle::lut_eval_float(&bytes, raw, &flat_f32, n_samples, n_out)
                .expect("oracle lut eval float");
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

            total_samples += n_samples;
            match ty {
                TY_LUT8 => mft1_tags += 1,
                TY_LUT16 => mft2_tags += 1,
                _ => unreachable!(),
            }
            hit_here = true;
        }
        if hit_here {
            profiles_with_lut += 1;
        }
    }

    println!(
        "testbed LUT diff: {mft1_tags} mft1 + {mft2_tags} mft2 tags over \
         {profiles_with_lut} profiles; {total_samples} input samples evaluated \
         (each in both 16-bit and float domains)"
    );
    assert!(
        mft1_tags + mft2_tags > 0,
        "expected at least one mft1/mft2 tag in the testbed"
    );
}

/// Reachability: mft1/mft2 on-disk types no longer return `Error::Unsupported`
/// from the type dispatcher (this slice implemented the LUT readers).
#[test]
fn lut_types_no_longer_unsupported() {
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
            let ty = match tintbox_oracle::tag_true_type(&bytes, raw) {
                Some(t) => t,
                None => continue,
            };
            if ty != TY_LUT8 && ty != TY_LUT16 {
                continue;
            }
            let res = p.read_tag(sig);
            assert!(
                matches!(res, Ok(Tag::Lut(_))),
                "{name}:{raw:08x} LUT tag should decode to Tag::Lut, got {res:?}"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "expected at least one mft1/mft2 tag in testbed"
    );
}

/// Differential for the V4 `mAB `/`mBA ` (LutAtoB / LutBtoA) tag readers,
/// bit-for-bit against lcms2. For every testbed profile carrying such a tag,
/// read it via tintbox `read_tag` -> `Tag::Lut(Pipeline)`, build the SAME pipeline
/// via lcms2 `cmsReadTag`, and evaluate a bounded input grid through BOTH stacks
/// in the 16-bit and float domains, asserting bit-exact at every sample.
#[test]
fn mab_mba_tag_pipelines_match_oracle_over_testbed() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut mab_tags = 0usize;
    let mut mba_tags = 0usize;
    let mut profiles_with_lut = 0usize;
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
            if ty != TY_LUT_A2B && ty != TY_LUT_B2A {
                continue;
            }

            // tintbox: read the tag into a Pipeline.
            let pipeline = match p.read_tag(sig).expect("rust mab/mba") {
                Tag::Lut(pl) => pl,
                other => panic!("{name}:{raw:08x} expected Lut, got {other:?}"),
            };

            // lcms2: the pipeline channel counts must agree.
            let (c_in, c_out) =
                tintbox_oracle::lut_channels(&bytes, raw).expect("oracle lut channels");
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
            let oracle16 = tintbox_oracle::lut_eval16(&bytes, raw, &flat_u16, n_samples, n_out)
                .expect("oracle lut eval16");
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
            let oraclef = tintbox_oracle::lut_eval_float(&bytes, raw, &flat_f32, n_samples, n_out)
                .expect("oracle lut eval float");
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

            total_samples += n_samples;
            match ty {
                TY_LUT_A2B => mab_tags += 1,
                TY_LUT_B2A => mba_tags += 1,
                _ => unreachable!(),
            }
            hit_here = true;
        }
        if hit_here {
            profiles_with_lut += 1;
        }
    }

    println!(
        "testbed mAB/mBA diff: {mab_tags} mAB + {mba_tags} mBA tags over \
         {profiles_with_lut} profiles; {total_samples} input samples evaluated \
         (each in both 16-bit and float domains)"
    );
    assert!(
        mab_tags + mba_tags > 0,
        "expected at least one mAB/mBA tag in the testbed"
    );
}

/// Reachability: `mAB `/`mBA ` on-disk types no longer return
/// `Error::Unsupported` from the type dispatcher (this slice implemented the V4
/// LUT readers).
#[test]
fn mab_mba_types_no_longer_unsupported() {
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
            let ty = match tintbox_oracle::tag_true_type(&bytes, raw) {
                Some(t) => t,
                None => continue,
            };
            if ty != TY_LUT_A2B && ty != TY_LUT_B2A {
                continue;
            }
            let res = p.read_tag(sig);
            assert!(
                matches!(res, Ok(Tag::Lut(_))),
                "{name}:{raw:08x} mAB/mBA tag should decode to Tag::Lut, got {res:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "expected at least one mAB/mBA tag in testbed");
}
