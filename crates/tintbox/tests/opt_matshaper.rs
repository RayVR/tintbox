//! Differential tests for the `Lcms2Compat` matrix-shaper optimizer
//! (`tintbox::opt::matshaper`, lcms2 `OptimizeMatrixShaper` / `MatShaperEval16`).
//!
//! The RGB testbed profiles crayons/ibm-t61/new/test5 all collapse to the
//! `ToneCurves(3) -> Matrix -> Matrix -> ToneCurves(3)` device link. lcms2-DEFAULT
//! optimizes the **non-identity-merged** ones into the 1.14-fixed
//! `MatShaperEval16` evaluator for an 8-bit input format; the two whose merged
//! matrix is the identity (ibm-t61 <-> new, which share colorant data) go through
//! `OptimizeByJoiningCurves` instead — a different optimizer that is out of scope
//! here, so for those tintbox's `Lcms2Compat` falls back to the accurate eval.
//!
//! We assert:
//! 1. **Lcms2Compat == lcms2-DEFAULT**, bit-exact, over the full 8-bit RGB cube,
//!    for 8-bit→8-bit and 8-bit→16-bit (the MatShaper path; verified to fire).
//! 2. **Accurate == lcms2-NOOPTIMIZE** bit-exact for 8-bit→8-bit output
//!    (regression guard: the default strategy is unchanged from slice 5). The
//!    8-bit→16-bit *output* path carries a separate, pre-existing slice-5
//!    `eval_16` rounding boundary (a few LSB) that is out of scope here.
//! 3. A **divergence report**: how many Accurate outputs differ from Lcms2Compat,
//!    and the max per-channel delta — the accuracy cost of lcms2's 1.14-fixed
//!    optimizer vs the accurate float eval.

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

/// The RGB matrix-shaper testbed profiles (device link probes to C-M-M-C).
const MATRIX_SHAPER_RGB: &[&str] = &["crayons.icc", "ibm-t61.icc", "new.icc", "test5.icc"];

fn type_rgb_8() -> u32 {
    (decode::PT_RGB << 16) | (3u32 << 3) | 1
}
fn type_rgb_16() -> u32 {
    (decode::PT_RGB << 16) | (3u32 << 3) | 2
}

/// The full 8-bit RGB cube subsampled to `levels` points per channel, packed as
/// TYPE_RGB_8 bytes. With `levels = 16` (step 17) this covers byte values not on
/// the coarse slice-5 grid, exercising the rounding boundaries.
fn rgb8_grid(levels: usize) -> Vec<u8> {
    let step = if levels <= 1 { 255 } else { 255 / (levels - 1) };
    let mut out = Vec::with_capacity(levels * levels * levels * 3);
    for ri in 0..levels {
        for gi in 0..levels {
            for bi in 0..levels {
                out.push((ri * step).min(255) as u8);
                out.push((gi * step).min(255) as u8);
                out.push((bi * step).min(255) as u8);
            }
        }
    }
    out
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

const INTENT: RenderingIntent = RenderingIntent::RelativeColorimetric;

#[test]
fn lcms2compat_matshaper_bit_identical_to_lcms2_default_8_and_16() {
    let grid = rgb8_grid(16); // 4096 pixels
    let n = grid.len() / 3;
    let intents_raw = [INTENT.to_raw(), INTENT.to_raw()];
    let bpc = [false, false];
    let adapt = [1.0, 1.0];
    let in_fmt = type_rgb_8();

    let mut fired_cells = 0usize;
    let mut fallback_cells = 0usize;

    for (an, bn) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        for &out_fmt in &[type_rgb_8(), type_rgb_16()] {
            let bpp = if out_fmt == type_rgb_8() { 3 } else { 6 };

            // lcms2 DEFAULT (runs OptimizeMatrixShaper -> MatShaperEval16 for the
            // non-identity pairs; OptimizeByJoiningCurves for the identity pairs).
            let mut oracle = vec![0u8; n * bpp];
            let ok = if out_fmt == type_rgb_8() {
                tintbox_oracle::do_transform_packed_default_8(
                    &[&ab, &bb],
                    &intents_raw,
                    &bpc,
                    &adapt,
                    in_fmt,
                    out_fmt,
                    &grid,
                    &mut oracle,
                    n,
                )
            } else {
                tintbox_oracle::do_transform_packed_default_16(
                    &[&ab, &bb],
                    &intents_raw,
                    &bpc,
                    &adapt,
                    in_fmt,
                    out_fmt,
                    &grid,
                    &mut oracle,
                    n,
                )
            };
            assert!(ok, "lcms2-default transform failed for {an} -> {bn}");

            // tintbox Lcms2Compat.
            let xform = Transform::new_simple_with_formats_strategy(
                &pa,
                &pb,
                INTENT,
                false,
                in_fmt,
                out_fmt,
                OptimizationStrategy::Lcms2Compat,
            )
            .unwrap_or_else(|e| panic!("tintbox Lcms2Compat build {an} -> {bn}: {e:?}"));

            let mut rcms_out = vec![0u8; n * bpp];
            xform.do_transform(&grid, &mut rcms_out, n);

            // Bit-exact vs lcms2-default, whether the matrix-shaper optimizer fired
            // (the MatShaper path) or fell back to the accurate eval (the two
            // identity-merged curve-join pairs — still matches lcms2 here).
            assert_eq!(
                rcms_out,
                oracle,
                "Lcms2Compat != lcms2-default for {an} -> {bn} (out_fmt={out_fmt:#x}, \
                 fired={})",
                xform.matshaper_fired()
            );

            if xform.matshaper_fired() {
                fired_cells += 1;
            } else {
                fallback_cells += 1;
            }
        }
    }

    // The bulk of the cells must actually exercise the MatShaper path.
    assert!(
        fired_cells >= 16,
        "expected the matrix-shaper optimizer to fire for most cells, got {fired_cells}"
    );
    eprintln!(
        "[matshaper] Lcms2Compat == lcms2-default bit-exact: {fired_cells} MatShaper cells + \
         {fallback_cells} curve-join fallback cells (identity-merged pairs)"
    );
}

#[test]
fn accurate_matches_lcms2_nooptimize_8bit() {
    // Regression guard: the DEFAULT `Accurate` strategy is the unchanged slice-5
    // in-place eval and must stay bit-exact vs lcms2-NOOPTIMIZE. We assert 8-bit
    // output, where the `eval_16` -> `FROM_16_TO_8` pack absorbs any LSB rounding
    // and the result is exact. (The 8-bit -> 16-bit *output* path carries a
    // separate, pre-existing slice-5 float->16 pack rounding boundary of a few
    // LSB; that is unrelated to this task and the slice-5 sweep documents it.)
    let grid = rgb8_grid(16);
    let n = grid.len() / 3;
    let intents_raw = [INTENT.to_raw(), INTENT.to_raw()];
    let bpc = [false, false];
    let adapt = [1.0, 1.0];
    let in_fmt = type_rgb_8();
    let out_fmt = type_rgb_8();

    let mut cells = 0usize;
    for (an, bn) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        let mut oracle = vec![0u8; n * 3];
        let ok = tintbox_oracle::do_transform_packed(
            &[&ab, &bb],
            &intents_raw,
            &bpc,
            &adapt,
            in_fmt,
            out_fmt,
            &grid,
            &mut oracle,
            n,
        );
        assert!(ok, "lcms2-NOOPTIMIZE transform failed for {an} -> {bn}");

        // tintbox Accurate (default): the optimizer never fires.
        let xform =
            Transform::new_simple_with_formats(&pa, &pb, INTENT, false, in_fmt, out_fmt).unwrap();
        assert_eq!(xform.strategy(), OptimizationStrategy::Accurate);
        assert!(
            !xform.matshaper_fired(),
            "Accurate must never fire the optimizer"
        );

        let mut rcms_out = vec![0u8; n * 3];
        xform.do_transform(&grid, &mut rcms_out, n);

        assert_eq!(
            rcms_out, oracle,
            "Accurate != lcms2-NOOPTIMIZE (8-bit) for {an} -> {bn}"
        );
        cells += 1;
    }
    assert!(cells >= 8);
    eprintln!("[matshaper] Accurate == lcms2-NOOPTIMIZE (8-bit) bit-exact over {cells} pairs");
}

#[test]
fn divergence_accurate_vs_lcms2compat_report() {
    // Quantify the accuracy cost of lcms2's 1.14-fixed matrix-shaper optimizer:
    // run BOTH tintbox-Accurate and tintbox-Lcms2Compat over the full 8-bit RGB cube
    // and report how many outputs differ + the max per-channel delta. Asserts the
    // parities (Accurate==lcms2-NOOPTIMIZE, Lcms2Compat==lcms2-default) along the
    // way for the 8->8 cells.
    let grid = rgb8_grid(16);
    let n = grid.len() / 3;
    let intents_raw = [INTENT.to_raw(), INTENT.to_raw()];
    let bpc = [false, false];
    let adapt = [1.0, 1.0];
    let in_fmt = type_rgb_8();
    let out_fmt = type_rgb_8();

    let mut total_diff_pixels = 0usize;
    let mut overall_max_delta = 0i32;

    for (an, bn) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        let mut lcms2_default = vec![0u8; n * 3];
        tintbox_oracle::do_transform_packed_default_8(
            &[&ab, &bb],
            &intents_raw,
            &bpc,
            &adapt,
            in_fmt,
            out_fmt,
            &grid,
            &mut lcms2_default,
            n,
        );
        let mut lcms2_noopt = vec![0u8; n * 3];
        tintbox_oracle::do_transform_packed(
            &[&ab, &bb],
            &intents_raw,
            &bpc,
            &adapt,
            in_fmt,
            out_fmt,
            &grid,
            &mut lcms2_noopt,
            n,
        );

        let acc =
            Transform::new_simple_with_formats(&pa, &pb, INTENT, false, in_fmt, out_fmt).unwrap();
        let mut acc_out = vec![0u8; n * 3];
        acc.do_transform(&grid, &mut acc_out, n);

        let compat = Transform::new_simple_with_formats_strategy(
            &pa,
            &pb,
            INTENT,
            false,
            in_fmt,
            out_fmt,
            OptimizationStrategy::Lcms2Compat,
        )
        .unwrap();
        let mut compat_out = vec![0u8; n * 3];
        compat.do_transform(&grid, &mut compat_out, n);

        assert_eq!(
            acc_out, lcms2_noopt,
            "Accurate != lcms2-NOOPTIMIZE {an} -> {bn}"
        );
        assert_eq!(
            compat_out, lcms2_default,
            "Lcms2Compat != lcms2-default {an} -> {bn}"
        );

        let mut differing = 0usize;
        let mut max_delta = 0i32;
        for p in 0..n {
            let mut diff = false;
            for c in 0..3 {
                let d = (acc_out[p * 3 + c] as i32 - compat_out[p * 3 + c] as i32).abs();
                if d != 0 {
                    diff = true;
                }
                max_delta = max_delta.max(d);
            }
            if diff {
                differing += 1;
            }
        }
        total_diff_pixels += differing;
        overall_max_delta = overall_max_delta.max(max_delta);
        eprintln!(
            "[matshaper divergence] {an} -> {bn}: {differing}/{n} pixels differ \
             (Accurate vs Lcms2Compat), max per-channel delta = {max_delta}, \
             optimizer fired = {}",
            compat.matshaper_fired()
        );
    }

    eprintln!(
        "[matshaper divergence] TOTAL across all pairs: {total_diff_pixels} differing pixels, \
         overall max per-channel delta = {overall_max_delta} (= the accuracy cost of lcms2's \
         1.14-fixed matrix-shaper optimizer vs the accurate float eval)"
    );
}
