//! 3x3 matrix / 3-vector ops, bit-identical to cmsmtrx.c.
//! BIT-IDENTITY PRECONDITIONS: no `f64::mul_add`; oracle built `-ffp-contract=off`;
//! host FLT_EVAL_METHOD==0. Operation order transcribed exactly from the C.

/// lcms2 `MATRIX_DET_TOLERANCE` (lcms2_internal.h:142). Shared so slices 3-4 agree.
pub(crate) const MATRIX_DET_TOLERANCE: f64 = 0.0001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec3(pub [f64; 3]);
/// Row-major 3x3 (m[row*3 + col]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat3(pub [f64; 9]);

impl Mat3 {
    /// Evaluate a vector across the matrix: `r = self * v`. Transcribes `_cmsMAT3eval`.
    pub fn eval(&self, v: Vec3) -> Vec3 {
        let a = &self.0;
        let v = &v.0;
        // a->v[i].n[j] == a[i*3 + j]; VX=0, VY=1, VZ=2.
        Vec3([
            a[0] * v[0] + a[1] * v[1] + a[2] * v[2],
            a[3] * v[0] + a[4] * v[1] + a[5] * v[2],
            a[6] * v[0] + a[7] * v[1] + a[8] * v[2],
        ])
    }

    /// Multiply two matrices: `r = self * b`. Transcribes `_cmsMAT3per` (ROWCOL macro).
    pub fn per(&self, b: &Mat3) -> Mat3 {
        let a = &self.0;
        let b = &b.0;
        // ROWCOL(i,j) = a->v[i].n[0]*b->v[0].n[j]
        //             + a->v[i].n[1]*b->v[1].n[j]
        //             + a->v[i].n[2]*b->v[2].n[j]
        // a->v[i].n[k] == a[i*3+k]; b->v[k].n[j] == b[k*3+j].
        let rowcol = |i: usize, j: usize| -> f64 {
            a[i * 3] * b[j] + a[i * 3 + 1] * b[3 + j] + a[i * 3 + 2] * b[6 + j]
        };
        Mat3([
            rowcol(0, 0),
            rowcol(0, 1),
            rowcol(0, 2),
            rowcol(1, 0),
            rowcol(1, 1),
            rowcol(1, 2),
            rowcol(2, 0),
            rowcol(2, 1),
            rowcol(2, 2),
        ])
    }

    /// Inverse `b = self^(-1)`. Transcribes `_cmsMAT3inverse` verbatim, including the
    /// unary-negate-then-add form of `c1` and the `MATRIX_DET_TOLERANCE` singularity gate.
    pub fn inverse(&self) -> Option<Mat3> {
        let a = &self.0;
        // a->v[i].n[j] == a[i*3 + j].
        let c0 = a[4] * a[8] - a[5] * a[7];
        let c1 = -(a[3] * a[8]) + a[5] * a[6];
        let c2 = a[3] * a[7] - a[4] * a[6];

        let det = a[0] * c0 + a[1] * c1 + a[2] * c2;

        if det.abs() < MATRIX_DET_TOLERANCE {
            return None; // singular matrix; can't invert
        }

        Some(Mat3([
            c0 / det,
            (a[2] * a[7] - a[1] * a[8]) / det,
            (a[1] * a[5] - a[2] * a[4]) / det,
            c1 / det,
            (a[0] * a[8] - a[2] * a[6]) / det,
            (a[2] * a[3] - a[0] * a[5]) / det,
            c2 / det,
            (a[1] * a[6] - a[0] * a[7]) / det,
            (a[0] * a[4] - a[1] * a[3]) / det,
        ]))
    }

    /// Solve `self * x = b`. Transcribes `_cmsMAT3solve`: invert, then eval `b` through
    /// the inverse (returns `None` on singular matrix).
    pub fn solve(&self, b: Vec3) -> Option<Vec3> {
        let a_1 = self.inverse()?;
        Some(a_1.eval(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rand_mat(rng: &mut tintbox_oracle::Rng) -> [f64; 9] {
        let mut m = [0.0; 9];
        for x in &mut m {
            *x = (rng.next_f64_unit() - 0.5) * 4.0;
        }
        m
    }
    #[test]
    fn eval_matches_oracle_bitwise() {
        let mut rng = tintbox_oracle::Rng::new(11);
        for _ in 0..500_000 {
            let m = rand_mat(&mut rng);
            let v = [
                rng.next_f64_unit(),
                rng.next_f64_unit(),
                rng.next_f64_unit(),
            ];
            let r = Mat3(m).eval(Vec3(v));
            let c = tintbox_oracle::mat3_eval(&m, &v);
            for (i, (&rv, &cv)) in r.0.iter().zip(c.iter()).enumerate() {
                tintbox_oracle::assert_f64_bits_eq(rv, cv, (i, m, v));
            }
        }
    }
    #[test]
    fn per_matches_oracle_bitwise() {
        let mut rng = tintbox_oracle::Rng::new(22);
        for _ in 0..500_000 {
            let (a, b) = (rand_mat(&mut rng), rand_mat(&mut rng));
            let r = Mat3(a).per(&Mat3(b));
            let c = tintbox_oracle::mat3_per(&a, &b);
            for (i, (&rv, &cv)) in r.0.iter().zip(c.iter()).enumerate() {
                tintbox_oracle::assert_f64_bits_eq(rv, cv, (i, a, b));
            }
        }
    }
    #[test]
    fn inverse_matches_oracle_bitwise() {
        let mut rng = tintbox_oracle::Rng::new(33);
        let mut tested = 0;
        for _ in 0..500_000 {
            let a = rand_mat(&mut rng);
            match (Mat3(a).inverse(), tintbox_oracle::mat3_inverse(&a)) {
                (Some(r), Some(c)) => {
                    for (i, (&rv, &cv)) in r.0.iter().zip(c.iter()).enumerate() {
                        tintbox_oracle::assert_f64_bits_eq(rv, cv, (i, a));
                    }
                    tested += 1;
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "singularity disagreement for {a:?}: rust={} c={}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        assert!(tested > 1000, "too few invertible samples");
    }
    #[test]
    fn solve_matches_oracle_bitwise() {
        let mut rng = tintbox_oracle::Rng::new(44);
        let mut tested = 0;
        for _ in 0..500_000 {
            let a = rand_mat(&mut rng);
            let b = [
                rng.next_f64_unit(),
                rng.next_f64_unit(),
                rng.next_f64_unit(),
            ];
            match (Mat3(a).solve(Vec3(b)), tintbox_oracle::mat3_solve(&a, &b)) {
                (Some(r), Some(c)) => {
                    for (i, (&rv, &cv)) in r.0.iter().zip(c.iter()).enumerate() {
                        tintbox_oracle::assert_f64_bits_eq(rv, cv, (i, a, b));
                    }
                    tested += 1;
                }
                (None, None) => {}
                (rs, cs) => panic!(
                    "solve singularity disagreement for {a:?}: rust={} c={}",
                    rs.is_some(),
                    cs.is_some()
                ),
            }
        }
        assert!(tested > 1000, "too few solvable samples");
    }
}
