//! Differential test: `tintbox::curve::eval_parametric` vs lcms2's
//! `DefaultEvalParametricFn` (via `cmsBuildParametricToneCurve` +
//! `cmsEvalToneCurveFloat`), bit-for-bit (`f32::to_bits`).
//!
//! For every forward parametric type {1,2,3,4,5,6,7,8,108,109} AND every inverse
//! {-1..-8,-108,-109} we generate many random VALID parameter sets (correct
//! count, a != 0 where the formula needs it, domains lcms2 accepts), sweep
//! r ∈ [-0.1, 1.1] plus a handful of exact points (0, 0.5, 1, the sRGB
//! thresholds), and assert the f32 bit pattern matches the oracle.
//!
//! The oracle path: a one-segment curve spanning (MINUS_INF, PLUS_INF], so
//! `EvalSegmentedFn` dispatches straight to the evaluator for any finite r. Its
//! only extra processing before the f32 cast is the infinity clamp to ±1E22.
//! We mirror that clamp here so the comparison isolates the evaluator.

use tintbox::curve::eval_parametric;
use tintbox_oracle::Rng;

/// lcms2 `PLUS_INF`/`MINUS_INF` (cmsgamma.c:41-42): a *float* literal.
const PLUS_INF: f64 = 1E22_f32 as f64;
const MINUS_INF: f64 = -1E22_f32 as f64;

/// Number of parameter coefficients each ICC parametric type reads (lcms2
/// `DefaultCurves[].ParameterCount`). Forward and inverse share the count.
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

/// Apply lcms2's `EvalSegmentedFn` infinity clamp, then cast to f32 exactly as
/// `cmsEvalToneCurveFloat` does. This is the value the oracle returns.
///
/// `r` is the f32 input as the oracle sees it (`cmsEvalToneCurveFloat` takes a
/// `cmsFloat32Number`, which `EvalSegmentedFn` widens to f64). We must evaluate
/// at that exact widened value, not the original f64 sample, or the input itself
/// differs by up to a double-rounding ULP.
fn rust_eval_as_oracle(ty: i32, params: &[f64; 10], r_f32: f32) -> f32 {
    let r = r_f32 as f64;
    let out = eval_parametric(ty, params, r);
    let clamped = if out.is_infinite() {
        if out > 0.0 {
            PLUS_INF
        } else {
            MINUS_INF
        }
    } else {
        out
    };
    clamped as f32
}

/// A reasonable gamma: positive, away from 0, spanning sub- and super-unity.
fn gamma(rng: &mut Rng) -> f64 {
    0.2 + rng.next_f64_unit() * 2.8 // [0.2, 3.0)
}

/// A nonzero coefficient with sign, magnitude bounded away from the
/// MATRIX_DET_TOLERANCE (0.0001) so the formula takes its main branch.
fn nonzero(rng: &mut Rng) -> f64 {
    let mag = 0.3 + rng.next_f64_unit() * 1.7; // [0.3, 2.0)
    if rng.next_u64() & 1 == 0 {
        mag
    } else {
        -mag
    }
}

/// Generate a VALID parameter set for `ty`. "Valid" = correct count and avoids
/// the degenerate sub-tolerance coefficients that send the evaluator down its
/// trivial 0-returning guards (those are exercised separately by edge params).
fn gen_params(ty: i32, rng: &mut Rng) -> [f64; 10] {
    let mut p = [0.0_f64; 10];
    match ty.abs() {
        1 => {
            p[0] = gamma(rng);
        }
        2 => {
            // (aX+b)^g ; a != 0
            p[0] = gamma(rng);
            p[1] = 0.5 + rng.next_f64_unit(); // a in [0.5,1.5)
            p[2] = rng.next_f64_unit() * 0.4 - 0.1; // b in [-0.1,0.3)
        }
        3 => {
            p[0] = gamma(rng);
            p[1] = 0.5 + rng.next_f64_unit();
            p[2] = rng.next_f64_unit() * 0.4 - 0.1;
            p[3] = rng.next_f64_unit() * 0.3; // c in [0,0.3)
        }
        4 => {
            // sRGB-like: a,c != 0
            p[0] = gamma(rng);
            p[1] = 0.5 + rng.next_f64_unit();
            p[2] = rng.next_f64_unit() * 0.4 - 0.1;
            p[3] = 0.3 + rng.next_f64_unit(); // c (linear slope)
            p[4] = rng.next_f64_unit() * 0.3; // d (threshold) in [0,0.3)
        }
        5 => {
            p[0] = gamma(rng);
            p[1] = 0.5 + rng.next_f64_unit();
            p[2] = rng.next_f64_unit() * 0.4 - 0.1;
            p[3] = 0.3 + rng.next_f64_unit();
            p[4] = rng.next_f64_unit() * 0.3;
            p[5] = rng.next_f64_unit() * 0.2 - 0.1; // e
            p[6] = rng.next_f64_unit() * 0.2 - 0.1; // f
        }
        6 => {
            p[0] = gamma(rng);
            p[1] = 0.5 + rng.next_f64_unit();
            p[2] = rng.next_f64_unit() * 0.4 - 0.1;
            p[3] = rng.next_f64_unit() * 0.3; // c
        }
        7 => {
            // a*log10(b*X^g + c) + d ; b != 0
            p[0] = gamma(rng);
            p[1] = 0.3 + rng.next_f64_unit(); // a
            p[2] = 0.5 + rng.next_f64_unit(); // b
            p[3] = 0.1 + rng.next_f64_unit(); // c (keep argument positive)
            p[4] = rng.next_f64_unit() * 0.3; // d
        }
        8 => {
            // a*b^(c*X+d) + e ; b > 0 for pow
            p[0] = 0.3 + rng.next_f64_unit(); // a
            p[1] = 1.2 + rng.next_f64_unit(); // b (>1)
            p[2] = nonzero(rng); // c
            p[3] = rng.next_f64_unit() * 0.4 - 0.2; // d
            p[4] = rng.next_f64_unit() * 0.3; // e
        }
        108 => {
            p[0] = gamma(rng);
        }
        109 => {
            // sigmoid steepness; nonzero
            p[0] = 1.0 + rng.next_f64_unit() * 9.0; // k in [1,10)
        }
        _ => unreachable!(),
    }
    p
}

/// The r-values to sweep for every param set: a fine span over [-0.1, 1.1] plus
/// exact corner points and the sRGB thresholds.
fn sweep_points() -> Vec<f64> {
    let mut v = vec![
        0.0_f64,
        0.5,
        1.0,
        0.040_45,    // sRGB encoded→linear threshold
        0.003_130_8, // sRGB linear→encoded threshold
        -0.1,
        1.1,
    ];
    // 25 evenly spaced points across [-0.1, 1.1].
    for i in 0..=24 {
        v.push(-0.1 + 1.2 * (i as f64) / 24.0);
    }
    v
}

const ALL_TYPES: [i32; 20] = [
    1, 2, 3, 4, 5, 6, 7, 8, 108, 109, -1, -2, -3, -4, -5, -6, -7, -8, -108, -109,
];

/// Minimum number of bit-exact comparisons each type must contribute (samples
/// where the oracle returned NaN are skipped, so we assert real coverage).
const MIN_SAMPLES_PER_TYPE: usize = 200;

#[test]
fn parametric_eval_bit_identical_all_types() {
    let mut rng = Rng::new(0x00C0_FFEE_1234_5678);
    let points = sweep_points();

    for &ty in &ALL_TYPES {
        let np = n_params(ty);
        let mut compared = 0usize;

        // Many param sets per type so the sweep covers each formula branch.
        for _ in 0..64 {
            let params = gen_params(ty, &mut rng);

            for &r in &points {
                let r_f32 = r as f32;
                let oracle = match tintbox_oracle::eval_parametric(ty, &params[..np], r_f32) {
                    Some(y) => y,
                    None => continue, // lcms2 rejected this param set; skip.
                };
                // NaN on either side is not a bit-comparable outcome (lcms2 can
                // produce NaN from e.g. log of a negative in -7/-8); the spec
                // says skip those. We still demand MIN_SAMPLES of real overlap.
                if oracle.is_nan() {
                    continue;
                }

                let rust = rust_eval_as_oracle(ty, &params, r_f32);
                if rust.is_nan() {
                    // If rust is NaN but oracle is not, that's a real divergence.
                    panic!(
                        "type {ty} r={r}: rust NaN but oracle={oracle} (params={:?})",
                        &params[..np]
                    );
                }

                assert_eq!(
                    rust.to_bits(),
                    oracle.to_bits(),
                    "type {ty} r={r}: rust={rust} (bits {:#010x}) oracle={oracle} (bits {:#010x}) params={:?}",
                    rust.to_bits(),
                    oracle.to_bits(),
                    &params[..np],
                );
                compared += 1;
            }
        }

        assert!(
            compared >= MIN_SAMPLES_PER_TYPE,
            "type {ty}: only {compared} bit-exact comparisons (< {MIN_SAMPLES_PER_TYPE}); \
             param generator is producing too many oracle-NaN/rejected cases",
        );
        println!("type {ty:>4}: {compared} bit-exact comparisons");
    }
}

/// Exercise the degenerate / guard branches explicitly: a == 0 (sub-tolerance),
/// gamma == 1 special cases, and the type-7 non-positive log argument. These
/// must still match the oracle bit-for-bit.
#[test]
fn parametric_eval_guard_branches() {
    let points = sweep_points();

    // (type, params) pairs that drive specific guards.
    let cases: &[(i32, [f64; 10])] = &[
        // type 1 / -1 with gamma exactly 1.0 (negative-input identity branch).
        (1, [1.0, 0., 0., 0., 0., 0., 0., 0., 0., 0.]),
        (-1, [1.0, 0., 0., 0., 0., 0., 0., 0., 0., 0.]),
        // -1 with gamma 0 -> PLUS_INF, clamped to 1E22 then cast.
        (-1, [0.0, 0., 0., 0., 0., 0., 0., 0., 0., 0.]),
        // type 2/3 with a == 0 (sub-tolerance) -> 0.
        (2, [2.4, 0.0, 0.05, 0., 0., 0., 0., 0., 0., 0.]),
        (3, [2.4, 0.0, 0.05, 0.1, 0., 0., 0., 0., 0., 0.]),
        // type 6 with gamma exactly 1.0 (no-clamp branch).
        (6, [1.0, 1.2, -0.1, 0.2, 0., 0., 0., 0., 0., 0.]),
        // type 7 with c negative so b*X^g+c can go non-positive (log guard).
        (7, [2.4, 1.0, 1.0, -0.5, 0.1, 0., 0., 0., 0., 0.]),
        // -7 with a==0 guard.
        (-7, [2.4, 0.0, 1.0, 0.1, 0.1, 0., 0., 0., 0., 0.]),
        // -8 with disc<0 region and valid coefficients.
        (-8, [1.5, 2.0, 1.2, 0.3, 0.1, 0., 0., 0., 0., 0.]),
        // -108 / 108 identity-ish gamma.
        (108, [1.0, 0., 0., 0., 0., 0., 0., 0., 0., 0.]),
        (-108, [1.0, 0., 0., 0., 0., 0., 0., 0., 0., 0.]),
    ];

    for &(ty, params) in cases {
        let np = n_params(ty);
        for &r in &points {
            let r_f32 = r as f32;
            let oracle = match tintbox_oracle::eval_parametric(ty, &params[..np], r_f32) {
                Some(y) => y,
                None => continue,
            };
            if oracle.is_nan() {
                continue;
            }
            let rust = rust_eval_as_oracle(ty, &params, r_f32);
            assert_eq!(
                rust.to_bits(),
                oracle.to_bits(),
                "guard type {ty} r={r}: rust={rust} oracle={oracle} params={:?}",
                &params[..np],
            );
        }
    }
}
