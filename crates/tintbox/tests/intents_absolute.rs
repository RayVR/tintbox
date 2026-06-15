//! Differential test for the absolute-colorimetric chromatic adaptation and the
//! perceptual / saturation intent routing in `tintbox::link::default_icc_intents`
//! (slice-5 T3).
//!
//! For every compatible ordered pair of testbed profiles, build the tintbox
//! device-link pipeline under each of:
//! - **AbsoluteColorimetric** (adaptation state 1.0 — the default/common case;
//!   `ComputeAbsoluteIntent` reads media white points + CHADs and builds the
//!   diagonal WPin/WPout adaptation matrix),
//! - **Perceptual**, **Saturation** (which route through the same
//!   `DefaultICCintents` path as relative; the difference is only the A2B/B2A LUT
//!   tag selected — already handled in T1 — plus the `_cmsLinkProfiles` BPC
//!   mutation),
//!
//! and evaluate a grid of input colors through it via `Pipeline::eval_float`.
//! Build the same chain in lcms2 via `cmsCreateExtendedTransform` with
//! `cmsFLAGS_NOOPTIMIZE` (explicit intents/bpc/adaptation) and `cmsDoTransform`
//! over the same float inputs. Assert bit-exact (`f32::to_bits`).
//!
//! **BPC scoping (spec §8.6):** the `_cmsLinkProfiles` BPC mutation forces BPC ON
//! for V4 profiles under perceptual/saturation, and OFF for absolute. BPC math is
//! T5 (deferred). So this test applies the SAME mutation tintbox's `Transform` will
//! (via `link_bpc_mutation`) and SKIPS any pair whose mutated BPC array is not all
//! `false` — i.e. V4 perceptual/saturation cells are deferred to T5. Those skips
//! are counted and reported. lcms2 is the arbiter of which pairs link.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use tintbox::link::{default_icc_intents, link_bpc_mutation};
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

fn channels(cs: ColorSpace) -> Option<usize> {
    Some(match cs {
        ColorSpace::Gray => 1,
        ColorSpace::XYZ | ColorSpace::Lab | ColorSpace::Rgb => 3,
        ColorSpace::Cmyk | ColorSpace::Mch4 => 4,
        _ => return None,
    })
}

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

fn load_all() -> Vec<Loaded> {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");
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
    loaded
}

/// Sweep every compatible ordered testbed pair under `intent`, comparing tintbox's
/// `default_icc_intents` pipeline to lcms2 (`NOOPTIMIZE`) bit-for-bit. The
/// `_cmsLinkProfiles` BPC mutation is applied to `[bpc; 2]` first; cells that end
/// up with any BPC forced on are skipped (deferred to T5) and counted.
fn sweep_intent(intent: RenderingIntent, requested_bpc: bool) -> (usize, usize, usize) {
    let loaded = load_all();
    let intents_raw = [intent.to_raw(), intent.to_raw()];
    let adaptation = [1.0f64, 1.0f64];

    let mut pairs_linked = 0usize;
    let mut total_samples = 0usize;
    let mut bpc_deferred = 0usize;
    let mut path_kinds: BTreeSet<String> = BTreeSet::new();

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }

            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();
            let profiles = [&pa, &pb];

            // Apply the _cmsLinkProfiles BPC mutation, exactly as Transform::new
            // will (spec §8.6). If it forces any BPC on, the BPC math (T5) is not
            // implemented yet → defer this cell.
            let mut bpc = [requested_bpc, requested_bpc];
            link_bpc_mutation(&[intent, intent], &profiles, &mut bpc);
            if bpc.iter().any(|&x| x) {
                bpc_deferred += 1;
                continue;
            }

            let in_chans = match channels(pa.header().color_space) {
                Some(c) => c,
                None => continue,
            };
            let out_chans = match channels(pb.header().color_space) {
                Some(c) => c,
                None => continue,
            };

            let levels = if in_chans >= 4 { 4 } else { 6 };
            let grid = input_grid(in_chans, levels);
            let mut flat_in: Vec<f32> = Vec::with_capacity(grid.len() * in_chans);
            for row in &grid {
                flat_in.extend_from_slice(row);
            }

            // lcms2 is the arbiter — same (mutated) bpc, same adaptation.
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
                None => continue, // lcms2 rejected this chain.
            };

            let lut = default_icc_intents(&profiles, &[intent, intent], &bpc, &adaptation, 0)
                .unwrap_or_else(|e| {
                    panic!(
                        "lcms2 linked {} -> {} ({intent:?}) but tintbox failed: {e}",
                        a.name, b.name
                    )
                });

            assert_eq!(lut.input_channels, in_chans, "{} -> {}", a.name, b.name);
            assert_eq!(lut.output_channels, out_chans, "{} -> {}", a.name, b.name);

            for (s, row) in grid.iter().enumerate() {
                let rust_out = lut.eval_float(row);
                let oref = &oracle_out[s * out_chans..(s + 1) * out_chans];
                for ch in 0..out_chans {
                    assert_eq!(
                        rust_out[ch].to_bits(),
                        oref[ch].to_bits(),
                        "{} -> {} ({intent:?}) sample {row:?} ch{ch}: rust={} lcms2={}",
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
        "{intent:?} (requested bpc={requested_bpc}): {pairs_linked} pairs bit-exact, \
         {total_samples} samples, {bpc_deferred} cells deferred to T5 (BPC forced on)"
    );
    for k in &path_kinds {
        println!("  pair: {k}");
    }
    (pairs_linked, total_samples, bpc_deferred)
}

#[test]
fn absolute_colorimetric_state_1_matches_oracle_over_testbed_pairs() {
    // Absolute always forces BPC off (mutation), so no cells are deferred here.
    let (pairs, samples, deferred) = sweep_intent(RenderingIntent::AbsoluteColorimetric, false);
    assert_eq!(deferred, 0, "absolute never forces BPC on");
    assert!(pairs > 0, "expected at least one linkable absolute pair");
    assert!(samples > 0);
}

#[test]
fn perceptual_matches_oracle_over_testbed_pairs() {
    // BPC requested off; V4 perc cells get BPC forced on → deferred to T5.
    let (pairs, samples, _deferred) = sweep_intent(RenderingIntent::Perceptual, false);
    assert!(pairs > 0, "expected at least one linkable perceptual pair");
    assert!(samples > 0);
}

#[test]
fn saturation_matches_oracle_over_testbed_pairs() {
    let (pairs, samples, _deferred) = sweep_intent(RenderingIntent::Saturation, false);
    assert!(pairs > 0, "expected at least one linkable saturation pair");
    assert!(samples > 0);
}
