//! Differential tests for 1D interpolation + tabulated/segmented/parametric
//! tone-curve construction and evaluation, bit-for-bit against lcms2.
//!
//! Coverage:
//! - `eval_16` of a tabulated-16 curve vs `cmsEvalToneCurve16` (the `LinLerp1D`
//!   fixed-point path), bit-exact u16 over random tables of various sizes.
//! - `eval_float` of tabulated-16, tabulated-float, and parametric curves vs
//!   `cmsEvalToneCurveFloat`, bit-exact f32 over a sweep of x.
//! - The materialised 16-bit table of a parametric/tabulated-float curve vs
//!   `cmsGetToneCurveEstimatedTable`, element-wise (validates nEntries, sampling,
//!   and quantization).
//! - `is_linear` / `is_monotonic` on known curves.

use rcms::curve::{build_parametric, build_tabulated_16, build_tabulated_float};
use rcms_oracle::Rng;

/// Coefficient count each ICC parametric type reads (lcms2 `ParameterCount`).
fn n_params(ty: i32) -> usize {
    match ty.abs() {
        1 => 1,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 7,
        6 => 4,
        7 => 5,
        8 => 5,
        108 => 1,
        109 => 1,
        _ => unreachable!(),
    }
}

/// A spread of table sizes covering the `domain == 0` single-point case, the
/// smallest interpolating tables, powers of two, and odd sizes.
const TABLE_SIZES: &[usize] = &[1, 2, 3, 4, 5, 7, 16, 17, 33, 256, 257, 1024, 4096];

#[test]
fn eval16_tabulated16_matches_oracle() {
    let mut rng = Rng::new(0x070E_C0FF_EE00);
    for &n in TABLE_SIZES {
        // A few random tables per size.
        for _ in 0..8 {
            let table: Vec<u16> = (0..n).map(|_| (rng.next_u64() & 0xffff) as u16).collect();
            let curve = build_tabulated_16(&table);

            // Sweep the full 16-bit input domain at a fine stride plus exact edges.
            let mut v: u32 = 0;
            while v <= 0xffff {
                let got = curve.eval_16(v as u16);
                let want = rcms_oracle::tabulated16_eval16(&table, v as u16);
                assert_eq!(got, want, "n={n} v={v:#06x} table={table:?}");
                v += 251; // coprime-ish stride to hit varied cells
            }
            for &v in &[0u16, 1, 0x7fff, 0x8000, 0xfffe, 0xffff] {
                let got = curve.eval_16(v);
                let want = rcms_oracle::tabulated16_eval16(&table, v);
                assert_eq!(got, want, "edge n={n} v={v:#06x}");
            }
        }
    }
}

/// x sweep for the float-eval tests: includes out-of-range, the exact 0/1 edges,
/// and a fine interior stride.
fn x_sweep() -> Vec<f32> {
    let mut xs = vec![-0.1f32, -1e-12, 0.0, 1e-12, 1.0, 1.0 + 1e-6, 1.1];
    let mut x = 0.0f64;
    while x <= 1.1 {
        xs.push(x as f32);
        x += 0.0007;
    }
    xs
}

#[test]
fn eval_float_tabulated16_matches_oracle() {
    let mut rng = Rng::new(0x000F_10A7_AB1E);
    let xs = x_sweep();
    for &n in TABLE_SIZES {
        for _ in 0..4 {
            let table: Vec<u16> = (0..n).map(|_| (rng.next_u64() & 0xffff) as u16).collect();
            let curve = build_tabulated_16(&table);
            for &x in &xs {
                let got = curve.eval_float(x);
                let want = rcms_oracle::tabulated16_eval_float(&table, x);
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "n={n} x={x} got={got} want={want}"
                );
            }
        }
    }
}

#[test]
fn eval_float_tabulated_float_matches_oracle() {
    let mut rng = Rng::new(0x0007_ABF1_0A70);
    let xs = x_sweep();
    // tabulated-float needs at least 1 entry; sweep sizes >= 1.
    for &n in TABLE_SIZES {
        for _ in 0..4 {
            let table: Vec<f32> = (0..n).map(|_| rng.next_f64_unit() as f32).collect();
            let curve = build_tabulated_float(&table).expect("non-empty table");
            for &x in &xs {
                let got = curve.eval_float(x);
                let want = rcms_oracle::tabulated_float_eval_float(&table, x)
                    .expect("non-empty table accepted by lcms2");
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "n={n} x={x} got={got} want={want} table={table:?}"
                );
            }
        }
    }
}

/// Forward + inverse parametric types lcms2 recognises.
const PARAM_TYPES: &[i32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 108, 109, -1, -2, -3, -4, -5, -6, -7, -8, -108, -109,
];

/// Generate a parameter set lcms2 accepts and that exercises the formula: gamma
/// in a sane range, `a` away from zero, thresholds in [0,1].
fn gen_params(ty: i32, rng: &mut Rng) -> Vec<f64> {
    let n = n_params(ty);
    let mut p = vec![0.0f64; n];
    // Gamma (param 0): keep in [0.2, 4] and away from 0.
    p[0] = 0.2 + rng.next_f64_unit() * 3.8;
    for (i, slot) in p.iter_mut().enumerate().skip(1) {
        // `a`-like params away from zero; others spread over [-1, 1] / [0, 1].
        let v = rng.next_f64_unit();
        *slot = if i == 1 {
            0.5 + v // keep slope positive and away from 0
        } else {
            v // [0, 1)
        };
    }
    p
}

#[test]
fn eval_float_parametric_matches_oracle() {
    let mut rng = Rng::new(0x009A_7A1C);
    let xs = x_sweep();
    for &ty in PARAM_TYPES {
        for _ in 0..16 {
            let params = gen_params(ty, &mut rng);
            // Only proceed when both sides accept the params (they share the same
            // type/count validation, so this is just the build feasibility gate).
            let curve = match build_parametric(ty, &params) {
                Some(c) => c,
                None => continue,
            };
            for &x in &xs {
                let got = curve.eval_float(x);
                let want = match rcms_oracle::parametric_eval_float(ty, &params, x) {
                    Some(w) => w,
                    None => continue,
                };
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "ty={ty} x={x} params={params:?} got={got} want={want}"
                );
            }
        }
    }
}

#[test]
fn parametric_table16_materialization_matches_oracle() {
    let mut rng = Rng::new(0x0007_AB1E_1600);
    for &ty in PARAM_TYPES {
        for _ in 0..16 {
            let params = gen_params(ty, &mut rng);
            let curve = match build_parametric(ty, &params) {
                Some(c) => c,
                None => continue,
            };
            let want = match rcms_oracle::parametric_table16(ty, &params) {
                Some(w) => w,
                None => continue,
            };
            assert_eq!(
                curve.table16(),
                want.as_slice(),
                "ty={ty} params={params:?} (len got={} want={})",
                curve.table16().len(),
                want.len()
            );
        }
    }
}

#[test]
fn tabulated_float_table16_materialization_matches_oracle() {
    let mut rng = Rng::new(0x000F_10A7_AB16);
    for &n in TABLE_SIZES {
        for _ in 0..4 {
            let table: Vec<f32> = (0..n).map(|_| rng.next_f64_unit() as f32).collect();
            let curve = build_tabulated_float(&table).expect("non-empty table");
            let want = rcms_oracle::tabulated_float_table16(&table).expect("accepted");
            assert_eq!(curve.table16(), want.as_slice(), "n={n} table={table:?}");
        }
    }
}

#[test]
fn gamma_table_node_count() {
    // gamma 1.0 collapses to a 2-node identity (EntriesByGamma); everything else
    // uses the full 4096-node grid.
    let g1 = rcms::curve::build_gamma(1.0);
    assert_eq!(g1.table16().len(), 2, "gamma=1.0 should use 2 grid points");
    let g22 = rcms::curve::build_gamma(2.2);
    assert_eq!(
        g22.table16().len(),
        4096,
        "gamma=2.2 should use 4096 points"
    );

    // Confirm the 2-node identity matches lcms2's materialization exactly.
    let want = rcms_oracle::parametric_table16(1, &[1.0]).expect("accepted");
    assert_eq!(g1.table16(), want.as_slice());
}

#[test]
fn is_linear_on_known_curves() {
    // A true linear ramp is linear; gamma 2.2 is not.
    let linear: Vec<u16> = (0..256)
        .map(|i| ((i as u32 * 65535) / 255) as u16)
        .collect();
    assert!(build_tabulated_16(&linear).is_linear());

    assert!(rcms::curve::build_gamma(1.0).is_linear());
    assert!(!rcms::curve::build_gamma(2.2).is_linear());
}

#[test]
fn is_monotonic_on_known_curves() {
    let ascending: Vec<u16> = (0..256)
        .map(|i| ((i as u32 * 65535) / 255) as u16)
        .collect();
    assert!(build_tabulated_16(&ascending).is_monotonic());

    let descending: Vec<u16> = ascending.iter().rev().copied().collect();
    assert!(build_tabulated_16(&descending).is_monotonic());

    // A clear non-monotone zig-zag.
    let zigzag: Vec<u16> = vec![0, 60000, 1000, 65000, 2000, 64000];
    assert!(!build_tabulated_16(&zigzag).is_monotonic());

    assert!(rcms::curve::build_gamma(2.2).is_monotonic());
}
