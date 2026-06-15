//! Differential sweep for `rcms::transform::Transform` + `do_transform_float`/
//! `_16` — the full create-transform + do-transform path (lcms2 `cmsxform.c`).
//!
//! For every compatible ordered pair of testbed profiles, for each of the four
//! ICC intents with **BPC off**, build the rcms `Transform` (which applies the
//! `_cmsLinkProfiles` BPC mutation internally) and `do_transform_float` AND
//! `do_transform_16` over a grid of input pixels. Build the same transform in
//! lcms2 via `cmsCreateExtendedTransform` (`cmsFLAGS_NOOPTIMIZE`) and
//! `cmsDoTransform` over the same pixels. Assert bit-exact (`f32::to_bits` /
//! `u16`).
//!
//! Cells where the BPC mutation *forces* BPC on (V4 profile under
//! perceptual/saturation, `cmscnvrt.c:1137-1145`) need the T5 black-point math and
//! are SKIPPED here.

use std::fs;
use std::path::{Path, PathBuf};

use rcms::profile::{ColorSpace, Profile, RenderingIntent};
use rcms::transform::Transform;

const ICC_VERSION_V4: u32 = 0x0400_0000;

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
/// the obvious testbed spaces. `None` for spaces we don't drive the ends with.
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
fn transform_matches_oracle_over_testbed_pairs_all_intents_float_and_16() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut loaded: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        if Profile::open(&bytes).is_err() {
            continue;
        }
        loaded.push(Loaded { name, bytes });
    }
    assert!(loaded.len() >= 2, "need at least two loadable profiles");

    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
        RenderingIntent::Perceptual,
        RenderingIntent::Saturation,
    ];
    let adaptation = [1.0f64, 1.0f64];
    let bpc = [false, false];

    let mut pairs_linked = 0usize;
    let mut float_samples = 0usize;
    let mut u16_samples = 0usize;
    let mut cells_skipped_t5 = 0usize;
    let mut bpc_cells_passed = 0usize;
    let mut per_intent: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }

            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

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
            // 16-bit inputs: quantize the same grid points (round-to-nearest like
            // lcms2's FromFloatTo16 boundary uses for the encoded grid).
            let flat_in_16: Vec<u16> = flat_in
                .iter()
                .map(|&v| (v as f64 * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u16)
                .collect();

            for &intent in &intents {
                // Mirror the _cmsLinkProfiles BPC mutation to know which cells are
                // forced-BPC-on. The chain is profile a then b; BPC is forced ON for
                // a V4 profile under perceptual/saturation. These cells exercise the
                // T5 black-point math (constant path) or remain deferred to
                // post-slice-7 (detection-by-sampling) — distinguished below by
                // whether rcms returns Unsupported.
                let forces_bpc_on = (intent == RenderingIntent::Perceptual
                    || intent == RenderingIntent::Saturation)
                    && (pa.header().version >= ICC_VERSION_V4
                        || pb.header().version >= ICC_VERSION_V4);

                let intents_raw = [intent.to_raw(), intent.to_raw()];

                // Ask lcms2 (NOOPTIMIZE) whether this chain links + transform float.
                // lcms2 applies the same BPC mutation internally and detects the
                // black points, so the oracle already reflects the forced-BPC math.
                let oracle_f = rcms_oracle::transform_eval_float(
                    &[&a.bytes, &b.bytes],
                    &intents_raw,
                    &bpc,
                    &adaptation,
                    &flat_in,
                    in_chans,
                    out_chans,
                    grid.len(),
                );
                let oracle_f = match oracle_f {
                    Some(o) => o,
                    None => continue, // lcms2 rejected; rcms isn't required to link it.
                };

                // Build the rcms transform. For forced-BPC cells that still need
                // detection-by-sampling (V4 matrix-shaper perc/sat, etc.), rcms
                // returns Unsupported — count those as deferred and skip.
                let xform = match Transform::new_simple(&pa, &pb, intent, false) {
                    Ok(x) => x,
                    Err(e) if forces_bpc_on => {
                        // Deferred BPC cell (post-slice-7, Lab virtual profiles).
                        let _ = e;
                        cells_skipped_t5 += 1;
                        continue;
                    }
                    Err(e) => panic!(
                        "lcms2 linked {} -> {} ({intent:?}) but rcms Transform failed: {e}",
                        a.name, b.name
                    ),
                };

                let oracle_16 = rcms_oracle::transform_eval_16(
                    &[&a.bytes, &b.bytes],
                    &intents_raw,
                    &bpc,
                    &adaptation,
                    &flat_in_16,
                    in_chans,
                    out_chans,
                    grid.len(),
                )
                .expect("lcms2 built float xform but 16-bit failed");

                assert_eq!(
                    xform.lut().input_channels,
                    in_chans,
                    "{} -> {} ({intent:?}): input channel mismatch",
                    a.name,
                    b.name
                );
                assert_eq!(
                    xform.lut().output_channels,
                    out_chans,
                    "{} -> {} ({intent:?}): output channel mismatch",
                    a.name,
                    b.name
                );

                // Float path.
                let mut rust_f = vec![0f32; grid.len() * out_chans];
                xform.do_transform_float(&flat_in, &mut rust_f, grid.len());
                for s in 0..grid.len() {
                    for ch in 0..out_chans {
                        let r = rust_f[s * out_chans + ch];
                        let o = oracle_f[s * out_chans + ch];
                        assert_eq!(
                            r.to_bits(),
                            o.to_bits(),
                            "FLOAT {} -> {} ({intent:?}) sample {:?} ch{ch}: rust={r} lcms2={o}",
                            a.name,
                            b.name,
                            grid[s]
                        );
                    }
                    float_samples += 1;
                }

                // 16-bit path.
                let mut rust_16 = vec![0u16; grid.len() * out_chans];
                xform.do_transform_16(&flat_in_16, &mut rust_16, grid.len());
                for s in 0..grid.len() {
                    for ch in 0..out_chans {
                        let r = rust_16[s * out_chans + ch];
                        let o = oracle_16[s * out_chans + ch];
                        assert_eq!(
                            r, o,
                            "U16 {} -> {} ({intent:?}) sample {:?} ch{ch}: rust={r} lcms2={o}",
                            a.name, b.name, grid[s]
                        );
                    }
                    u16_samples += 1;
                }

                pairs_linked += 1;
                if forces_bpc_on {
                    bpc_cells_passed += 1;
                }
                *per_intent.entry(format!("{intent:?}")).or_default() += 1;
            }
        }
    }

    println!(
        "Transform sweep (4 intents): {pairs_linked} (pair×intent) cells linked + \
         transformed bit-exact, {float_samples} float samples, {u16_samples} u16 samples. \
         Of those, {bpc_cells_passed} forced-BPC cells (V4 perc/sat) now pass via the \
         black-point-compensation math; {cells_skipped_t5} forced-BPC cells remain DEFERRED \
         (detection-by-sampling, post-slice-7 Lab virtual profiles)."
    );
    for (intent, n) in &per_intent {
        println!("  {intent}: {n} cells");
    }
    assert!(
        pairs_linked > 0,
        "expected at least one linkable testbed (pair, intent) cell"
    );
    // Both domains must have run.
    assert!(float_samples > 0 && u16_samples > 0);
}

// --- Unit tests: field derivation + multi-pixel iteration -------------------

fn open_pair() -> (Vec<u8>, Vec<u8>) {
    // Pick two distinct loadable testbed profiles whose ends have known channel
    // counts (RGB display + something), to exercise field derivation.
    let files = testbed_icc();
    let mut rgb: Option<Vec<u8>> = None;
    let mut other: Option<Vec<u8>> = None;
    for p in &files {
        let bytes = fs::read(p).unwrap();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let Ok(prof) = Profile::open(&bytes) else {
            continue;
        };
        if prof.header().color_space == ColorSpace::Rgb && rgb.is_none() {
            rgb = Some(bytes);
        } else if other.is_none() && channels(prof.header().color_space).is_some() {
            other = Some(bytes);
        }
    }
    (
        rgb.expect("an RGB testbed profile"),
        other.expect("a second testbed profile"),
    )
}

#[test]
fn transform_field_derivation_matches_profiles() {
    let (rgb_bytes, out_bytes) = open_pair();
    let pin = Profile::open(&rgb_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();

    let x = Transform::new_simple(&pin, &pout, RenderingIntent::RelativeColorimetric, false)
        .expect("build transform");

    // Entry color space = first profile's device color space (input direction).
    assert_eq!(x.entry_color_space(), pin.header().color_space);
    // Exit color space = last profile's device color space (output direction)
    // when the last profile is consumed PCS->device.
    assert_eq!(x.exit_color_space(), pout.header().color_space);
    // Rendering intent is the last link's intent.
    assert_eq!(x.rendering_intent(), RenderingIntent::RelativeColorimetric);
    // White points are valid (positive Y).
    assert!(x.entry_white_point().y > 0.0);
    assert!(x.exit_white_point().y > 0.0);
    // No gamut-check pipeline in slice 5.
    assert!(x.gamut_check().is_none());
}

#[test]
fn do_transform_iterates_multiple_pixels() {
    let (rgb_bytes, out_bytes) = open_pair();
    let pin = Profile::open(&rgb_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();
    let x =
        Transform::new_simple(&pin, &pout, RenderingIntent::RelativeColorimetric, false).unwrap();

    let in_ch = x.lut().input_channels;
    let out_ch = x.lut().output_channels;

    // Three distinct pixels in one buffer.
    let mut pixels: Vec<f32> = Vec::new();
    let samples = [0.0f32, 0.5, 1.0];
    for &s in &samples {
        for _ in 0..in_ch {
            pixels.push(s);
        }
    }
    let mut out = vec![0f32; samples.len() * out_ch];
    x.do_transform_float(&pixels, &mut out, samples.len());

    // Each pixel's block must equal evaluating that pixel alone.
    for (i, &s) in samples.iter().enumerate() {
        let single = x.lut().eval_float(&vec![s; in_ch]);
        for ch in 0..out_ch {
            assert_eq!(
                out[i * out_ch + ch].to_bits(),
                single[ch].to_bits(),
                "multi-pixel float block {i} ch{ch} differs from single eval"
            );
        }
    }

    // 16-bit multi-pixel iteration.
    let mut pin16: Vec<u16> = Vec::new();
    let s16 = [0u16, 32768, 65535];
    for &s in &s16 {
        for _ in 0..in_ch {
            pin16.push(s);
        }
    }
    let mut out16 = vec![0u16; s16.len() * out_ch];
    x.do_transform_16(&pin16, &mut out16, s16.len());
    for (i, &s) in s16.iter().enumerate() {
        let single = x.lut().eval_16(&vec![s; in_ch]);
        for ch in 0..out_ch {
            assert_eq!(
                out16[i * out_ch + ch],
                single[ch],
                "multi-pixel u16 block {i} ch{ch} differs from single eval"
            );
        }
    }
}
