//! lcms2 `DefaultEvalParametricFn` (cmsgamma.c:344-720), transcribed VERBATIM.
//!
//! This is the single most bit-sensitive function in the tone-curve slice: every
//! `pow`/`log`/`log10` call, operand order, domain comparison (`>=` vs `>`) and
//! negative-input clip is copied directly from the C so the f64 arithmetic
//! matches lcms2 bit-for-bit. `f64::powf`/`ln`/`log10` are verified bit-identical
//! to the oracle's libm, so they stand in for C `pow`/`log`/`log10`. No
//! `mul_add` anywhere (it would fuse multiply-add and diverge from the C).

/// lcms2 `MATRIX_DET_TOLERANCE` (lcms2_internal.h:142).
const MATRIX_DET_TOLERANCE: f64 = 0.0001;
/// lcms2 `PLUS_INF` (cmsgamma.c:42), a *float* literal widened to f64.
const PLUS_INF: f64 = 1E22_f32 as f64;

// Generates a sigmoidal function with desired steepness (cmsgamma.c:317).
#[inline]
fn sigmoid_base(k: f64, t: f64) -> f64 {
    (1.0 / (1.0 + (-k * t).exp())) - 0.5
}

#[inline]
fn inverted_sigmoid_base(k: f64, t: f64) -> f64 {
    -((1.0 / (t + 0.5)) - 1.0).ln() / k
}

#[inline]
fn sigmoid_factory(k: f64, t: f64) -> f64 {
    let correction = 0.5 / sigmoid_base(k, 1.0);
    correction * sigmoid_base(k, 2.0 * t - 1.0) + 0.5
}

#[inline]
fn inverse_sigmoid_factory(k: f64, t: f64) -> f64 {
    let correction = 0.5 / sigmoid_base(k, 1.0);
    (inverted_sigmoid_base(k, (t - 0.5) / correction) + 1.0) / 2.0
}

/// lcms2 `DefaultEvalParametricFn(Type, Params, R)` (cmsgamma.c:344-720).
///
/// Evaluates the ICC parametric function `curve_type` at `r`, reading the
/// coefficients it needs from `params`. Forward types are `1..=8`, `108`, `109`;
/// their inverses are the negatives. The `default` arm (any other type) returns
/// `0.0`, exactly as the C does for an "unsupported parametric curve".
///
/// Transcribed verbatim — including `MATRIX_DET_TOLERANCE` guards, the
/// `disc = -params[2]/params[1]` discriminants (types 2-5), the type-7 `log10`
/// argument guard, the type-5 inner clip, the type-8 `(a,b,c,d,e)` indexing, and
/// the near-gamma-1.0 special cases in the inverses.
//
// `if_same_then_else` fires on the inverse cases (-5, -8) where two *distinct*
// guard conditions in the C both fall through to `Val = 0` (e.g. a == 0 vs
// b == 0). Collapsing them would obscure which lcms2 guard is being mirrored and
// invite drift if a future lcms2 version makes the arms differ; we keep the
// branch structure verbatim and silence the lint.
#[allow(clippy::if_same_then_else)]
pub fn eval_parametric(curve_type: i32, params: &[f64; 10], r: f64) -> f64 {
    let val: f64;

    match curve_type {
        // X = Y ^ Gamma
        1 => {
            if r < 0.0 {
                if (params[0] - 1.0).abs() < MATRIX_DET_TOLERANCE {
                    val = r;
                } else {
                    val = 0.0;
                }
            } else {
                val = r.powf(params[0]);
            }
        }

        // Type 1 Reversed: X = Y ^1/gamma
        -1 => {
            if r < 0.0 {
                if (params[0] - 1.0).abs() < MATRIX_DET_TOLERANCE {
                    val = r;
                } else {
                    val = 0.0;
                }
            } else if params[0].abs() < MATRIX_DET_TOLERANCE {
                val = PLUS_INF;
            } else {
                val = r.powf(1.0 / params[0]);
            }
        }

        // CIE 122-1966
        // Y = (aX + b)^Gamma  | X >= -b/a
        // Y = 0               | else
        2 => {
            if params[1].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                let disc = -params[2] / params[1];
                if r >= disc {
                    let e = params[1] * r + params[2];
                    if e > 0.0 {
                        val = e.powf(params[0]);
                    } else {
                        val = 0.0;
                    }
                } else {
                    val = 0.0;
                }
            }
        }

        // Type 2 Reversed
        // X = (Y ^1/g  - b) / a
        -2 => {
            if params[0].abs() < MATRIX_DET_TOLERANCE || params[1].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                let mut v = if r < 0.0 {
                    0.0
                } else {
                    (r.powf(1.0 / params[0]) - params[2]) / params[1]
                };
                if v < 0.0 {
                    v = 0.0;
                }
                val = v;
            }
        }

        // IEC 61966-3
        // Y = (aX + b)^Gamma + c | X <= -b/a
        // Y = c                  | else
        3 => {
            if params[1].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                let mut disc = -params[2] / params[1];
                if disc < 0.0 {
                    disc = 0.0;
                }
                if r >= disc {
                    let e = params[1] * r + params[2];
                    if e > 0.0 {
                        val = e.powf(params[0]) + params[3];
                    } else {
                        val = 0.0;
                    }
                } else {
                    val = params[3];
                }
            }
        }

        // Type 3 reversed
        // X=((Y-c)^1/g - b)/a      | (Y>=c)
        // X=-b/a                   | (Y<c)
        -3 => {
            if params[0].abs() < MATRIX_DET_TOLERANCE || params[1].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else if r >= params[3] {
                let e = r - params[3];
                if e > 0.0 {
                    val = (e.powf(1.0 / params[0]) - params[2]) / params[1];
                } else {
                    val = 0.0;
                }
            } else {
                val = -params[2] / params[1];
            }
        }

        // IEC 61966-2.1 (sRGB)
        // Y = (aX + b)^Gamma | X >= d
        // Y = cX             | X < d
        4 => {
            if r >= params[4] {
                let e = params[1] * r + params[2];
                if e > 0.0 {
                    val = e.powf(params[0]);
                } else {
                    val = 0.0;
                }
            } else {
                val = r * params[3];
            }
        }

        // Type 4 reversed
        // X=((Y^1/g-b)/a)    | Y >= (ad+b)^g
        // X=Y/c              | Y< (ad+b)^g
        -4 => {
            let e = params[1] * params[4] + params[2];
            let disc = if e < 0.0 { 0.0 } else { e.powf(params[0]) };
            if r >= disc {
                if params[0].abs() < MATRIX_DET_TOLERANCE || params[1].abs() < MATRIX_DET_TOLERANCE
                {
                    val = 0.0;
                } else {
                    val = (r.powf(1.0 / params[0]) - params[2]) / params[1];
                }
            } else if params[3].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                val = r / params[3];
            }
        }

        // Y = (aX + b)^Gamma + e | X >= d
        // Y = cX + f             | X < d
        5 => {
            if r >= params[4] {
                let e = params[1] * r + params[2];
                if e > 0.0 {
                    val = e.powf(params[0]) + params[5];
                } else {
                    val = params[5];
                }
            } else {
                val = r * params[3] + params[6];
            }
        }

        // Reversed type 5
        // X=((Y-e)1/g-b)/a   | Y >=(ad+b)^g+e), cd+f
        // X=(Y-f)/c          | else
        -5 => {
            let disc = params[3] * params[4] + params[6];
            if r >= disc {
                let e = r - params[5];
                if e < 0.0 {
                    val = 0.0;
                } else if params[0].abs() < MATRIX_DET_TOLERANCE
                    || params[1].abs() < MATRIX_DET_TOLERANCE
                {
                    val = 0.0;
                } else {
                    val = (e.powf(1.0 / params[0]) - params[2]) / params[1];
                }
            } else if params[3].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                val = (r - params[6]) / params[3];
            }
        }

        // Types 6,7,8 comes from segmented curves as described in ICCSpecRevision_02_11_06_Float.pdf
        // Type 6 is basically identical to type 5 without d

        // Y = (a * X + b) ^ Gamma + c
        6 => {
            let e = params[1] * r + params[2];
            // On gamma 1.0, don't clamp
            if params[0] == 1.0 {
                val = e + params[3];
            } else if e < 0.0 {
                val = params[3];
            } else {
                val = e.powf(params[0]) + params[3];
            }
        }

        // ((Y - c) ^1/Gamma - b) / a
        -6 => {
            if params[0].abs() < MATRIX_DET_TOLERANCE || params[1].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                let e = r - params[3];
                if e < 0.0 {
                    val = 0.0;
                } else {
                    val = (e.powf(1.0 / params[0]) - params[2]) / params[1];
                }
            }
        }

        // Y = a * log (b * X^Gamma + c) + d
        7 => {
            let e = params[2] * r.powf(params[0]) + params[3];
            if e <= 0.0 {
                val = params[4];
            } else {
                val = params[1] * e.log10() + params[4];
            }
        }

        // (Y - d) / a = log(b * X ^Gamma + c)
        // pow(10, (Y-d) / a) = b * X ^Gamma + c
        // pow((pow(10, (Y-d) / a) - c) / b, 1/g) = X
        -7 => {
            if params[0].abs() < MATRIX_DET_TOLERANCE
                || params[1].abs() < MATRIX_DET_TOLERANCE
                || params[2].abs() < MATRIX_DET_TOLERANCE
            {
                val = 0.0;
            } else {
                val = ((10.0_f64.powf((r - params[4]) / params[1]) - params[3]) / params[2])
                    .powf(1.0 / params[0]);
            }
        }

        // Y = a * b^(c*X+d) + e
        8 => {
            val = params[0] * params[1].powf(params[2] * r + params[3]) + params[4];
        }

        // Y = (log((y-e) / a) / log(b) - d ) / c
        // a=0, b=1, c=2, d=3, e=4,
        -8 => {
            let disc = r - params[4];
            if disc < 0.0 {
                val = 0.0;
            } else if params[0].abs() < MATRIX_DET_TOLERANCE
                || params[2].abs() < MATRIX_DET_TOLERANCE
            {
                val = 0.0;
            } else {
                val = ((disc / params[0]).ln() / params[1].ln() - params[3]) / params[2];
            }
        }

        // S-Shaped: (1 - (1-x)^1/g)^1/g
        108 => {
            if params[0].abs() < MATRIX_DET_TOLERANCE {
                val = 0.0;
            } else {
                val = (1.0 - (1.0 - r).powf(1.0 / params[0])).powf(1.0 / params[0]);
            }
        }

        // y = (1 - (1-x)^1/g)^1/g
        // y^g = (1 - (1-x)^1/g)
        // 1 - y^g = (1-x)^1/g
        // (1 - y^g)^g = 1 - x
        // 1 - (1 - y^g)^g
        -108 => {
            val = 1.0 - (1.0 - r.powf(params[0])).powf(params[0]);
        }

        // Sigmoidals
        109 => {
            val = sigmoid_factory(params[0], r);
        }

        -109 => {
            val = inverse_sigmoid_factory(params[0], r);
        }

        // Unsupported parametric curve. Should never reach here.
        _ => return 0.0,
    }

    val
}
