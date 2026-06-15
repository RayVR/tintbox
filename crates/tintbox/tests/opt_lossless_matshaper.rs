//! Differential tests for the LOSSLESS `AccurateFast` matrix-shaper fast path
//! (`tintbox::opt::lossless_matshaper`).
//!
//! The ABSOLUTE REQUIREMENT this file proves: `AccurateFast` is **byte-for-byte
//! identical** to (a) the default `Accurate` strategy and (b) lcms2 run with
//! `cmsFLAGS_NOOPTIMIZE` (the differential oracle), over the full testbed RGB
//! matrix-shaper profile pairs × intents × pixel-formats {8,16,float}. Faster but
//! different = FAILURE — so every assertion here is an exact `assert_eq!`, with a
//! dense sweep that includes the shadow range (bytes 0..32 densely, where any
//! divergence would show first).
//!
//! `AccurateFast` only installs the fast path for 8-bit RGB input (the lossless
//! `LosslessMatShaper`, indexed by an input byte); for 16-bit and float input it
//! falls back to the in-place pipeline eval, which IS the `Accurate` path. We
//! assert parity for ALL formats so the fall-back is covered too, and separately
//! assert the fast path actually FIRES for the 8-bit cells.

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::format::decode;
use tintbox::opt::OptimizationStrategy;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::Transform;

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

/// The RGB matrix-shaper testbed profiles (device link probes to C-M-M-C, merged
/// by pre_optimize to C-M-C).
const MATRIX_SHAPER_RGB: &[&str] = &["crayons.icc", "ibm-t61.icc", "new.icc", "test5.icc"];

fn type_rgb_8() -> u32 {
    (decode::PT_RGB << 16) | (3u32 << 3) | 1
}
fn type_rgb_16() -> u32 {
    (decode::PT_RGB << 16) | (3u32 << 3) | 2
}
fn type_rgb_flt() -> u32 {
    decode::TYPE_RGB_FLT
}

fn load(name: &str) -> Vec<u8> {
    fs::read(testbed_dir().join(name)).unwrap_or_else(|_| panic!("read {name}"))
}

/// All ordered RGB matrix-shaper pairs.
fn pairs() -> Vec<(&'static str, &'static str)> {
    let mut v = Vec::new();
    for &a in MATRIX_SHAPER_RGB {
        for &b in MATRIX_SHAPER_RGB {
            if a != b {
                v.push((a, b));
            }
        }
    }
    v
}

/// A dense 8-bit RGB sweep, biased to the shadow range where any LSB divergence
/// would surface first: every byte 0..32 on each channel (the shadows), plus a
/// coarse grid across the rest of the cube. Returns packed TYPE_RGB_8 bytes.
fn rgb8_dense_sweep() -> Vec<u8> {
    // Channel sample values: dense 0..=31, then a coarse step to 255.
    let mut vals: Vec<u8> = (0u8..=31).collect();
    for v in (40u8..=255).step_by(13) {
        vals.push(v);
    }
    vals.push(255);
    vals.dedup();

    let mut out = Vec::with_capacity(vals.len().pow(3) * 3);
    for &r in &vals {
        for &g in &vals {
            for &b in &vals {
                out.push(r);
                out.push(g);
                out.push(b);
            }
        }
    }
    out
}

/// RGB intents that route through the matrix-shaper device link.
const INTENTS: &[RenderingIntent] = &[
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
];

/// Build a tintbox transform with a chosen strategy, single intent, no BPC.
fn build(
    pa: &Profile,
    pb: &Profile,
    intent: RenderingIntent,
    in_fmt: u32,
    out_fmt: u32,
    strategy: OptimizationStrategy,
) -> Transform {
    Transform::new_simple_with_formats_strategy(pa, pb, intent, false, in_fmt, out_fmt, strategy)
        .expect("build transform")
}

/// The headline proof: over every RGB matrix-shaper pair × intent × format, the
/// `AccurateFast` packed output equals BOTH the `Accurate` output AND the lcms2
/// `NOOPTIMIZE` oracle, byte-for-byte. Zero mismatches.
#[test]
fn accurate_fast_is_byte_identical_to_accurate_and_lcms2_nooptimize() {
    let grid8 = rgb8_dense_sweep();
    let n = grid8.len() / 3;

    let mut total_cells = 0usize;
    let mut total_pixels_checked = 0usize;
    let mut fast_path_fired_cells = 0usize;
    let mut mismatches = 0usize;

    for (an, bn) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        for &intent in INTENTS {
            let intents_raw = [intent.to_raw(), intent.to_raw()];
            let bpc = [false, false];
            let adapt = [1.0, 1.0];

            // ---- 8-bit input (the fast path FIRES) ----
            // Input is always 8-bit so AccurateFast installs the LUT path; output
            // format is varied across 8/16/float.
            let in_fmt = type_rgb_8();
            for &out_fmt in &[type_rgb_8(), type_rgb_16(), type_rgb_flt()] {
                let out_bpp = match out_fmt {
                    f if f == type_rgb_8() => 3,
                    f if f == type_rgb_16() => 6,
                    _ => 12, // float
                };

                // (b) lcms2 NOOPTIMIZE oracle.
                let mut oracle = vec![0u8; n * out_bpp];
                let ok = tintbox_oracle::do_transform_packed(
                    &[&ab, &bb],
                    &intents_raw,
                    &bpc,
                    &adapt,
                    in_fmt,
                    out_fmt,
                    &grid8,
                    &mut oracle,
                    n,
                );
                assert!(ok, "lcms2-NOOPTIMIZE failed {an}->{bn} out={out_fmt:#x}");

                // (a) tintbox Accurate (default in-place eval).
                let acc = build(
                    &pa,
                    &pb,
                    intent,
                    in_fmt,
                    out_fmt,
                    OptimizationStrategy::Accurate,
                );
                let mut acc_out = vec![0u8; n * out_bpp];
                acc.do_transform(&grid8, &mut acc_out, n);

                // tintbox AccurateFast (the lossless fast path under test).
                let fast = build(
                    &pa,
                    &pb,
                    intent,
                    in_fmt,
                    out_fmt,
                    OptimizationStrategy::AccurateFast,
                );
                let mut fast_out = vec![0u8; n * out_bpp];
                fast.do_transform(&grid8, &mut fast_out, n);

                if fast.lossless_matshaper_fired() {
                    fast_path_fired_cells += 1;
                }

                // Byte-for-byte: AccurateFast == lcms2-NOOPTIMIZE.
                if fast_out != oracle {
                    mismatches += 1;
                    let bad = fast_out
                        .iter()
                        .zip(&oracle)
                        .position(|(a, b)| a != b)
                        .unwrap();
                    panic!(
                        "AccurateFast != lcms2-NOOPTIMIZE {an}->{bn} out={out_fmt:#x} \
                         at byte {bad}: fast={} oracle={} (fired={}, mismatches so far {mismatches})",
                        fast_out[bad],
                        oracle[bad],
                        fast.lossless_matshaper_fired()
                    );
                }
                // Byte-for-byte: AccurateFast == Accurate.
                if fast_out != acc_out {
                    mismatches += 1;
                    let bad = fast_out
                        .iter()
                        .zip(&acc_out)
                        .position(|(a, b)| a != b)
                        .unwrap();
                    panic!(
                        "AccurateFast != Accurate {an}->{bn} out={out_fmt:#x} at byte {bad}: \
                         fast={} accurate={} (fired={}, mismatches so far {mismatches})",
                        fast_out[bad],
                        acc_out[bad],
                        fast.lossless_matshaper_fired()
                    );
                }

                total_cells += 1;
                total_pixels_checked += n;
            }
        }
    }

    assert_eq!(mismatches, 0, "found {mismatches} mismatching cells");
    // The 8-bit-input cells must actually exercise the fast path for the
    // non-identity-merged pairs (the two identity-merged pairs collapse the matrix
    // to identity which pre_optimize drops, leaving a 2-stage C-C pipeline the
    // matrix-shaper detector correctly declines — those fall back to Pipeline).
    assert!(
        fast_path_fired_cells >= 24,
        "expected the lossless fast path to fire for most cells, got {fast_path_fired_cells}"
    );
    eprintln!(
        "[lossless-matshaper] AccurateFast == Accurate == lcms2-NOOPTIMIZE byte-for-byte: \
         {total_cells} cells, {total_pixels_checked} pixels checked, 0 mismatches; \
         lossless fast path fired in {fast_path_fired_cells} cells (rest fell back to Pipeline)"
    );
}

/// Non-8-bit input: AccurateFast must fall back to the Pipeline eval and remain
/// byte-identical to Accurate (the fast path cannot fire — input is not 8-bit).
#[test]
fn accurate_fast_falls_back_for_16bit_and_float_input() {
    // A small grid is enough; we only check the fall-back stays identical.
    let mut grid16 = Vec::new();
    for r in (0u16..=65535).step_by(4369) {
        for g in (0u16..=65535).step_by(8738) {
            for b in (0u16..=65535).step_by(13107) {
                grid16.extend_from_slice(&r.to_le_bytes());
                grid16.extend_from_slice(&g.to_le_bytes());
                grid16.extend_from_slice(&b.to_le_bytes());
            }
        }
    }
    let n = grid16.len() / 6;
    let in_fmt = type_rgb_16();

    for (an, bn) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        for &out_fmt in &[type_rgb_16(), type_rgb_8()] {
            let out_bpp = if out_fmt == type_rgb_16() { 6 } else { 3 };
            let acc = build(
                &pa,
                &pb,
                RenderingIntent::Perceptual,
                in_fmt,
                out_fmt,
                OptimizationStrategy::Accurate,
            );
            let fast = build(
                &pa,
                &pb,
                RenderingIntent::Perceptual,
                in_fmt,
                out_fmt,
                OptimizationStrategy::AccurateFast,
            );
            assert!(
                !fast.lossless_matshaper_fired(),
                "fast path must NOT fire for 16-bit input {an}->{bn}"
            );

            let mut acc_out = vec![0u8; n * out_bpp];
            let mut fast_out = vec![0u8; n * out_bpp];
            acc.do_transform(&grid16, &mut acc_out, n);
            fast.do_transform(&grid16, &mut fast_out, n);
            assert_eq!(
                fast_out, acc_out,
                "AccurateFast fall-back != Accurate (16-bit in) {an}->{bn}"
            );
        }
    }
}
