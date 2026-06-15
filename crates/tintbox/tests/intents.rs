//! Differential test for the intent-driven profile-link chain
//! (`tintbox::link::default_icc_intents`) — the FIRST END-TO-END TRANSFORM.
//!
//! For every compatible ordered pair of testbed profiles, under
//! **RelativeColorimetric with BPC off**, build the tintbox device-link pipeline via
//! `default_icc_intents([&in, &out], [Relative; 2], [false; 2], [1.0; 2],
//! NOOPTIMIZE)` and evaluate a grid of input colors through it via
//! `Pipeline::eval_float`. Build the same chain in lcms2 via
//! `cmsCreateExtendedTransform` with `cmsFLAGS_NOOPTIMIZE` (explicit
//! intents/bpc/adaptation) and `cmsDoTransform` over the same float inputs. Assert
//! bit-exact (`f32::to_bits`).
//!
//! Pairs exercise matrix-shaper→LUT (RGB display → CMYK/Lab output) and LUT→LUT.
//! lcms2 is the arbiter of which pairs link: if the oracle builds a transform, so
//! must tintbox, and the pixels must match to the bit.

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::link::default_icc_intents;
use tintbox::profile::{ColorSpace, Profile, RenderingIntent};

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

/// Device channel count of a color space (lcms2 `cmsChannelsOfColorSpace`), for
/// the obvious testbed spaces. Returns `None` for spaces we don't expect to drive
/// the chain ends.
fn channels(cs: ColorSpace) -> Option<usize> {
    Some(match cs {
        ColorSpace::Gray => 1,
        ColorSpace::XYZ | ColorSpace::Lab | ColorSpace::Rgb => 3,
        ColorSpace::Cmyk | ColorSpace::Mch4 => 4,
        _ => return None,
    })
}

/// A bounded per-channel input sweep: `levels` grid points per channel over
/// `[0, 1]` (cartesian product over `n_in` channels).
fn input_grid(n_in: usize, levels: usize) -> Vec<Vec<f32>> {
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

struct Loaded {
    name: String,
    bytes: Vec<u8>,
}

#[test]
fn default_icc_intents_relative_matches_oracle_over_testbed_pairs() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    // Load every profile lcms2 accepts AND tintbox parses.
    let mut loaded: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !tintbox_oracle::open_succeeds(&bytes) {
            continue;
        }
        if Profile::open(&bytes).is_err() {
            continue;
        }
        loaded.push(Loaded { name, bytes });
    }
    assert!(loaded.len() >= 2, "need at least two loadable profiles");

    let intent = RenderingIntent::RelativeColorimetric;
    let intents_raw = [intent.to_raw(), intent.to_raw()];
    let bpc = [false, false];
    let adaptation = [1.0f64, 1.0f64];

    let mut pairs_linked = 0usize;
    let mut total_samples = 0usize;
    let mut path_kinds: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for a in &loaded {
        for b in &loaded {
            // Skip identical-handle pairs (self→self is degenerate but allowed;
            // keep it only when distinct to keep the matrix focused).
            if a.name == b.name {
                continue;
            }

            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            // The chain's input/output device channel counts: first profile's
            // color space drives the input; last profile's color space the output.
            let in_chans = match channels(pa.header().color_space) {
                Some(c) => c,
                None => continue,
            };
            let out_chans = match channels(pb.header().color_space) {
                Some(c) => c,
                None => continue,
            };

            // Build the grid once (coarser for 4-channel inputs to bound the
            // cartesian product).
            let levels = if in_chans >= 4 { 4 } else { 6 };
            let grid = input_grid(in_chans, levels);
            let mut flat_in: Vec<f32> = Vec::with_capacity(grid.len() * in_chans);
            for row in &grid {
                flat_in.extend_from_slice(row);
            }

            // Ask lcms2 (NOOPTIMIZE) whether this chain links + transform pixels.
            let oracle_out = tintbox_oracle::transform_eval_float(
                &[&a.bytes, &b.bytes],
                &intents_raw,
                &bpc,
                &adaptation,
                &flat_in,
                in_chans,
                out_chans,
                grid.len(),
            );
            let oracle_out = match oracle_out {
                Some(o) => o,
                None => continue, // lcms2 rejected this chain; tintbox isn't required to link it.
            };

            // lcms2 linked it: tintbox MUST link it too.
            let lut = default_icc_intents(&[&pa, &pb], &[intent, intent], &bpc, &adaptation, 0)
                .unwrap_or_else(|e| {
                    panic!(
                        "lcms2 linked {} -> {} but tintbox failed: {e}",
                        a.name, b.name
                    )
                });

            assert_eq!(
                lut.input_channels, in_chans,
                "{} -> {}: input channel mismatch",
                a.name, b.name
            );
            assert_eq!(
                lut.output_channels, out_chans,
                "{} -> {}: output channel mismatch",
                a.name, b.name
            );

            for (s, row) in grid.iter().enumerate() {
                let rust_out = lut.eval_float(row);
                let oref = &oracle_out[s * out_chans..(s + 1) * out_chans];
                for ch in 0..out_chans {
                    assert_eq!(
                        rust_out[ch].to_bits(),
                        oref[ch].to_bits(),
                        "{} -> {} sample {row:?} ch{ch}: rust={} lcms2={}",
                        a.name,
                        b.name,
                        rust_out[ch],
                        oref[ch]
                    );
                }
                total_samples += 1;
            }

            pairs_linked += 1;
            path_kinds.insert(format!(
                "{}({:?}/{:?})->{}({:?}/{:?})",
                a.name,
                pa.header().color_space,
                pa.header().pcs,
                b.name,
                pb.header().color_space,
                pb.header().pcs,
            ));
        }
    }

    println!(
        "default_icc_intents (relative, BPC off): {pairs_linked} profile pairs linked + \
         transformed bit-exact, {total_samples} samples"
    );
    for k in &path_kinds {
        println!("  pair: {k}");
    }
    assert!(
        pairs_linked > 0,
        "expected at least one linkable testbed profile pair"
    );
}
