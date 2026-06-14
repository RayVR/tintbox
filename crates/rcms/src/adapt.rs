//! White point from temperature and Bradford chromatic adaptation.
//! Bit-identical transcription of `cmswtpnt.c`.
//!
//! BIT-IDENTITY PRECONDITIONS: no `f64::mul_add`; oracle built `-ffp-contract=off`;
//! host FLT_EVAL_METHOD==0. Polynomial and matrix-multiply operand order are
//! transcribed verbatim from the C.

use crate::color::{CIExyY, CIEXYZ};
use crate::math::matrix::{Mat3, Vec3};

/// Bradford cone-response matrix (`LamRigg`, `cmswtpnt.c:238-242`), row-major.
/// This is the default cone matrix for `_cmsAdaptationMatrix`.
pub const BRADFORD: Mat3 = Mat3([
    0.8951, 0.2664, -0.1614, //
    -0.7502, 1.7135, 0.0367, //
    0.0389, -0.0685, 1.0296,
]);

/// Obtains a white point from a correlated color temperature.
/// Transcribes `cmsWhitePointFromTemp` (`cmswtpnt.c:48-91`).
///
/// Returns `None` for temperatures outside `[4000, 25000]` K (lcms2 returns
/// FALSE there). The `x` polynomial uses different coefficients for the
/// `[4000, 7000]` and `(7000, 25000]` ranges; `y` is derived from `x`.
pub fn white_point_from_temp(temp_k: f64) -> Option<CIExyY> {
    let t = temp_k;
    let t2 = t * t; // Square
    let t3 = t2 * t; // Cube

    let x;
    // For correlated color temperature (T) between 4000K and 7000K:
    // (clippy: `>= && <=` is an inclusive-range contains; the branch is a pure
    // boolean test, so the form is bit-irrelevant.)
    if (4000. ..=7000.).contains(&t) {
        x = -4.6070 * (1E9 / t3) + 2.9678 * (1E6 / t2) + 0.09911 * (1E3 / t) + 0.244063;
    }
    // or for correlated color temperature (T) between 7000K and 25000K:
    else if t > 7000.0 && t <= 25000.0 {
        x = -2.0064 * (1E9 / t3) + 1.9018 * (1E6 / t2) + 0.24748 * (1E3 / t) + 0.237040;
    } else {
        return None;
    }

    // Obtain y(x)
    let y = -3.000 * (x * x) + 2.870 * x - 0.275;

    Some(CIExyY { x, y, yy: 1.0 })
}

/// Compute the chromatic adaptation matrix from a cone matrix and source/dest
/// white points. Transcribes `ComputeChromaticAdaptation` (`cmswtpnt.c:190-232`).
///
/// `result = inverse(cone) * (diag * cone)`, where `diag` scales each cone-domain
/// channel by `dest/source`. Returns `None` if `cone` is singular or any source
/// cone-domain channel is below the matrix determinant tolerance.
fn compute_chromatic_adaptation(
    source_white_point: CIEXYZ,
    dest_white_point: CIEXYZ,
    chad: &Mat3,
) -> Option<Mat3> {
    // Tmp = *Chad; if (!_cmsMAT3inverse(&Tmp, &Chad_Inv)) return FALSE;
    let chad_inv = chad.inverse()?;

    let cone_source_xyz = Vec3([
        source_white_point.x,
        source_white_point.y,
        source_white_point.z,
    ]);
    let cone_dest_xyz = Vec3([dest_white_point.x, dest_white_point.y, dest_white_point.z]);

    let cone_source_rgb = chad.eval(cone_source_xyz);
    let cone_dest_rgb = chad.eval(cone_dest_xyz);

    if cone_source_rgb.0[0].abs() < crate::math::matrix::MATRIX_DET_TOLERANCE
        || cone_source_rgb.0[1].abs() < crate::math::matrix::MATRIX_DET_TOLERANCE
        || cone_source_rgb.0[2].abs() < crate::math::matrix::MATRIX_DET_TOLERANCE
    {
        return None;
    }

    // Build matrix (diagonal dest/source scaling).
    let cone = Mat3([
        cone_dest_rgb.0[0] / cone_source_rgb.0[0],
        0.0,
        0.0,
        0.0,
        cone_dest_rgb.0[1] / cone_source_rgb.0[1],
        0.0,
        0.0,
        0.0,
        cone_dest_rgb.0[2] / cone_source_rgb.0[2],
    ]);

    // Normalize: Tmp = Cone * Chad; Conversion = Chad_Inv * Tmp.
    let tmp = cone.per(chad);
    Some(chad_inv.per(&tmp))
}

/// Final chromatic adaptation matrix from illuminant `from` to illuminant `to`.
/// Transcribes `_cmsAdaptationMatrix` (`cmswtpnt.c:236-248`): when `cone` is
/// `None`, Bradford is assumed.
pub fn adaptation_matrix(cone: Option<&Mat3>, from: CIEXYZ, to: CIEXYZ) -> Option<Mat3> {
    let cone = cone.unwrap_or(&BRADFORD);
    compute_chromatic_adaptation(from, to, cone)
}

/// Adapts a color to a given illuminant. The original color is expected to have
/// a `source_wp` white point. Transcribes `cmsAdaptToIlluminant`
/// (`cmswtpnt.c:328-351`).
///
/// Returns `None` if the Bradford adaptation matrix cannot be built.
pub fn adapt_to_illuminant(source_wp: CIEXYZ, illuminant: CIEXYZ, value: CIEXYZ) -> Option<CIEXYZ> {
    let bradford = adaptation_matrix(None, source_wp, illuminant)?;
    let out = bradford.eval(Vec3([value.x, value.y, value.z]));
    Some(CIEXYZ {
        x: out.0[0],
        y: out.0[1],
        z: out.0[2],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard illuminants used by lcms2's profile builders (computed, not
    /// constants) plus D50, as XYZ. Mix of typical white points.
    fn standard_wps() -> Vec<CIEXYZ> {
        vec![
            crate::math::whitepoint::D50,
            CIEXYZ {
                x: 0.9504,
                y: 1.0,
                z: 1.0888,
            }, // ~D65
            CIEXYZ {
                x: 1.0985,
                y: 1.0,
                z: 0.3558,
            }, // ~A
            CIEXYZ {
                x: 0.9807,
                y: 1.0,
                z: 1.1822,
            }, // ~D55-ish
        ]
    }

    #[test]
    fn white_point_from_temp_matches_oracle_bitwise() {
        // Sweep the whole valid range plus a margin of out-of-range temps.
        let mut rng = rcms_oracle::Rng::new(0xC0FFEE);
        let mut tested = 0;
        for _ in 0..200_000 {
            // Bias toward [4000,25000] but include out-of-range to hit both None.
            let temp = 1000.0 + rng.next_f64_unit() * 30000.0;
            match (
                white_point_from_temp(temp),
                rcms_oracle::white_point_from_temp(temp),
            ) {
                (Some(wp), Some(c)) => {
                    rcms_oracle::assert_f64_bits_eq(wp.x, c[0], ("x", temp));
                    rcms_oracle::assert_f64_bits_eq(wp.y, c[1], ("y", temp));
                    rcms_oracle::assert_f64_bits_eq(wp.yy, c[2], ("Y", temp));
                    tested += 1;
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "range disagreement at temp={temp}: rust={} c={}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        assert!(tested > 1000, "too few in-range samples");
    }

    #[test]
    fn white_point_from_temp_boundaries() {
        // Both branches and the 7000K seam: exact bit match across the boundary.
        for &t in &[4000.0_f64, 4000.0001, 6999.999, 7000.0, 7000.0001, 25000.0] {
            let r = white_point_from_temp(t);
            let c = rcms_oracle::white_point_from_temp(t);
            match (r, c) {
                (Some(wp), Some(c)) => {
                    rcms_oracle::assert_f64_bits_eq(wp.x, c[0], ("x", t));
                    rcms_oracle::assert_f64_bits_eq(wp.y, c[1], ("y", t));
                    rcms_oracle::assert_f64_bits_eq(wp.yy, c[2], ("Y", t));
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "boundary disagreement at {t}: {} vs {}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        // Strictly out of range -> both None.
        for &t in &[3999.9999_f64, 25000.0001, 0.0, -100.0, 1e9] {
            assert!(white_point_from_temp(t).is_none());
            assert!(rcms_oracle::white_point_from_temp(t).is_none());
        }
    }

    #[test]
    fn adaptation_matrix_matches_oracle_bitwise() {
        let mut rng = rcms_oracle::Rng::new(0xBADC0DE);
        let std = standard_wps();
        let mut tested = 0;
        for _ in 0..200_000 {
            // Mix random and standard white points for source/dest.
            let pick = |rng: &mut rcms_oracle::Rng| -> CIEXYZ {
                if rng.next_u64() & 1 == 0 {
                    std[(rng.next_u64() as usize) % std.len()]
                } else {
                    CIEXYZ {
                        x: rng.next_f64_unit() * 2.0,
                        y: rng.next_f64_unit() * 2.0,
                        z: rng.next_f64_unit() * 2.0,
                    }
                }
            };
            let from = pick(&mut rng);
            let to = pick(&mut rng);
            match (
                adaptation_matrix(None, from, to),
                rcms_oracle::adaptation_matrix(&[from.x, from.y, from.z], &[to.x, to.y, to.z]),
            ) {
                (Some(m), Some(c)) => {
                    for (i, (&rv, &cv)) in m.0.iter().zip(c.iter()).enumerate() {
                        rcms_oracle::assert_f64_bits_eq(rv, cv, (i, from, to));
                    }
                    tested += 1;
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "adaptation_matrix singularity disagreement from={from:?} to={to:?}: rust={} c={}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        assert!(tested > 1000, "too few non-singular samples");
    }

    #[test]
    fn adapt_to_illuminant_matches_oracle_bitwise() {
        let mut rng = rcms_oracle::Rng::new(0xFEEDFACE);
        let std = standard_wps();
        let mut tested = 0;
        for _ in 0..200_000 {
            let pick = |rng: &mut rcms_oracle::Rng| -> CIEXYZ {
                if rng.next_u64() & 1 == 0 {
                    std[(rng.next_u64() as usize) % std.len()]
                } else {
                    CIEXYZ {
                        x: rng.next_f64_unit() * 2.0,
                        y: rng.next_f64_unit() * 2.0,
                        z: rng.next_f64_unit() * 2.0,
                    }
                }
            };
            let src = pick(&mut rng);
            let ill = pick(&mut rng);
            let value = CIEXYZ {
                x: rng.next_f64_unit(),
                y: rng.next_f64_unit(),
                z: rng.next_f64_unit(),
            };
            match (
                adapt_to_illuminant(src, ill, value),
                rcms_oracle::adapt_to_illuminant(
                    &[src.x, src.y, src.z],
                    &[ill.x, ill.y, ill.z],
                    &[value.x, value.y, value.z],
                ),
            ) {
                (Some(r), Some(c)) => {
                    rcms_oracle::assert_f64_bits_eq(r.x, c[0], ("X", src, ill, value));
                    rcms_oracle::assert_f64_bits_eq(r.y, c[1], ("Y", src, ill, value));
                    rcms_oracle::assert_f64_bits_eq(r.z, c[2], ("Z", src, ill, value));
                    tested += 1;
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "adapt singularity disagreement src={src:?} ill={ill:?}: rust={} c={}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        assert!(tested > 1000, "too few non-singular samples");
    }

    #[test]
    fn degenerate_white_point_is_none() {
        // A source white point with a zero cone-domain channel -> singular -> None
        // for both rust and oracle. The all-zero XYZ drives cone_source_rgb to 0.
        let zero = CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let d50 = crate::math::whitepoint::D50;
        assert!(adaptation_matrix(None, zero, d50).is_none());
        assert!(rcms_oracle::adaptation_matrix(&[0.0, 0.0, 0.0], &[d50.x, d50.y, d50.z]).is_none());
        // adapt_to_illuminant propagates the None.
        let value = CIEXYZ {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        };
        assert!(adapt_to_illuminant(zero, d50, value).is_none());
        assert!(rcms_oracle::adapt_to_illuminant(
            &[0.0, 0.0, 0.0],
            &[d50.x, d50.y, d50.z],
            &[value.x, value.y, value.z]
        )
        .is_none());
    }
}
