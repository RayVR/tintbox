//! lcms2's matrix-shaper optimizer (`cmsopt.c:1532-1805`).
//!
//! When a device-link pipeline collapses to the RGB matrix-shaper pattern
//!
//! ```text
//!   ToneCurves(3) -> Matrix(3x3) [-> Matrix(3x3)] -> ToneCurves(3)
//! ```
//!
//! and the input pixel format is 8-bit, lcms2 replaces the float pipeline eval
//! with [`MatShaper8Data::eval`] — `MatShaperEval16` (`cmsopt.c:1542-1576`): a
//! 1.14 signed-fixed-point evaluator that precomputes the input curves into
//! 256-entry shaper tables, the (possibly merged) matrix into 1.14 fixed, and
//! the output curves into 16385-entry shaper tables. This is FAST but NOT
//! bit-identical to the full float pipeline eval (the 1.14 fixed quantization and
//! the 8-bit input indexing introduce error); it lives in the opt-in
//! [`Lcms2Compat`](super::OptimizationStrategy::Lcms2Compat) strategy purely for
//! drop-in bit-identity with stock lcms2-default.
//!
//! Everything here is transcribed VERBATIM from `cmsopt.c`; see the inline
//! references for each constant and rounding step.

use crate::curve::ToneCurve;
use crate::format::PixelFormat;
use crate::pipeline::{Pipeline, Stage};

/// `cmsS1Fixed14Number` (`cmsopt.c:71`): a signed 1.14 fixed-point value. Note
/// the comment "this may hold more than 16 bits" — the matrix entries and
/// accumulator overflow 16 bits, so this is a 32-bit signed integer.
type S1Fixed14 = i32;

/// A row-major 3x3 matrix in `f64` (lcms2 `cmsMAT3`, `v[i].n[j]` = row i, col j).
type Mat3 = [[f64; 3]; 3];

/// `DOUBLE_TO_1FIXED14(x)` (`cmsopt.c:73`): `(cmsS1Fixed14Number) floor(x*16384.0 + 0.5)`.
#[inline]
fn double_to_1fixed14(x: f64) -> S1Fixed14 {
    (x * 16384.0 + 0.5).floor() as S1Fixed14
}

/// lcms2 `_cmsQuickSaturateWord` (lcms2_internal.h:188): `floor(d + 0.5)`
/// saturated into `[0, 0xffff]`. The matrix-shaper output quantization uses this
/// (not the fast-floor magic-number path, which `FillSecondShaper` does not
/// invoke — it calls the plain inline that does an ordinary floor).
#[inline]
fn quick_saturate_word(d: f64) -> u16 {
    let d = d + 0.5;
    if d <= 0.0 {
        return 0;
    }
    if d >= 65535.0 {
        return 0xffff;
    }
    d.floor() as u16
}

/// lcms2 `FROM_16_TO_8` (lcms2_internal.h:126).
#[inline]
fn from_16_to_8(rgb: u16) -> u8 {
    (((rgb as u32 * 65281 + 8_388_608) >> 24) & 0xff) as u8
}

/// lcms2 `FROM_8_TO_16` (lcms2_internal.h:125).
#[inline]
fn from_8_to_16(rgb: u8) -> u16 {
    ((rgb as u16) << 8) | rgb as u16
}

/// The precomputed matrix-shaper evaluation tables (lcms2 `MatShaper8Data`,
/// `cmsopt.c:77-90`). About 50 KB; built once per transform.
#[derive(Clone)]
pub struct MatShaper8Data {
    /// `Shaper1{R,G,B}[256]`: input curves evaluated at `i/255`, in 1.14 fixed.
    shaper1: [[S1Fixed14; 256]; 3],
    /// `Mat[3][3]`: the (possibly merged) matrix in 1.14 fixed (`n.14`).
    mat: [[S1Fixed14; 3]; 3],
    /// `Off[3]`: the offset in 1.14 fixed (zero when the matrix has no offset).
    off: [S1Fixed14; 3],
    /// `Shaper2{R,G,B}[16385]`: output curves evaluated at `j/16384`, quantized to
    /// u16 (8-bit-rounded when the output format is 8-bit).
    shaper2: [Vec<u16>; 3],
}

/// lcms2 `FillFirstShaper` (`cmsopt.c:1580-1595`): evaluate `Curve` at `i/255.0`
/// for `i = 0..256`, store as 1.14 fixed (`DOUBLE_TO_1FIXED14`). Values `>=
/// 131072.0` saturate to `0x7fffffff` (matches the C guard exactly).
fn fill_first_shaper(curve: &ToneCurve) -> [S1Fixed14; 256] {
    let mut table = [0i32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let r = (i as f64 / 255.0) as f32;
        let y = curve.eval_float(r);
        if (y as f64) < 131072.0 {
            *slot = double_to_1fixed14(y as f64);
        } else {
            *slot = 0x7fff_ffff;
        }
    }
    table
}

/// lcms2 `FillSecondShaper` (`cmsopt.c:1599-1628`): evaluate `Curve` at
/// `j/16384.0` for `j = 0..16385`, clamp to `[0, 1]`, then quantize.
///
/// When `is_8bit_output` the value is rounded to a byte and re-expanded
/// (`FROM_8_TO_16(FROM_16_TO_8(w))`) so the later pack's `>> 8` is exact;
/// otherwise it is the plain `_cmsQuickSaturateWord(Val * 65535.0)`.
fn fill_second_shaper(curve: &ToneCurve, is_8bit_output: bool) -> Vec<u16> {
    let mut table = vec![0u16; 16385];
    for (i, slot) in table.iter_mut().enumerate() {
        let r = (i as f64 / 16384.0) as f32;
        let mut val = curve.eval_float(r) as f64;
        // Transcribed from FillSecondShaper: the `< 0` then `> 1.0` order is the
        // C's, not `f64::clamp` (which differs on NaN and reorders the compares).
        #[allow(clippy::manual_clamp)]
        if val < 0.0 {
            val = 0.0;
        }
        if val > 1.0 {
            val = 1.0;
        }
        if is_8bit_output {
            let w = quick_saturate_word(val * 65535.0);
            let b = from_16_to_8(w);
            *slot = from_8_to_16(b);
        } else {
            *slot = quick_saturate_word(val * 65535.0);
        }
    }
    table
}

impl MatShaper8Data {
    /// lcms2 `SetMatShaper` (`cmsopt.c:1632-1677`): precompute the shaper and
    /// matrix tables. `curve1`/`curve2` are the input/output tone-curve triples,
    /// `mat` the merged 3x3 (row-major `mat[i][j]`), `off` the optional offset,
    /// and `is_8bit_output` is `_cmsFormatterIs8bit(OutputFormat)`.
    fn build(
        curve1: &[ToneCurve],
        mat: &Mat3,
        off: Option<&[f64; 3]>,
        curve2: &[ToneCurve],
        is_8bit_output: bool,
    ) -> MatShaper8Data {
        let shaper1 = [
            fill_first_shaper(&curve1[0]),
            fill_first_shaper(&curve1[1]),
            fill_first_shaper(&curve1[2]),
        ];
        let shaper2 = [
            fill_second_shaper(&curve2[0], is_8bit_output),
            fill_second_shaper(&curve2[1], is_8bit_output),
            fill_second_shaper(&curve2[2], is_8bit_output),
        ];

        let mut m = [[0i32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = double_to_1fixed14(mat[i][j]);
            }
        }

        let mut o = [0i32; 3];
        for i in 0..3 {
            o[i] = match off {
                None => 0,
                Some(v) => double_to_1fixed14(v[i]),
            };
        }

        MatShaper8Data {
            shaper1,
            mat: m,
            off: o,
            shaper2,
        }
    }

    /// lcms2 `MatShaperEval16` (`cmsopt.c:1542-1576`). `input` carries 3 u16
    /// channels derived from an 8-bit format (`In[i] = a<<8 | a`), so the low
    /// byte indexes the first shaper. Transcribed verbatim: the `+0x2000 >> 14`
    /// rounding, the `[0, 16384]` clip, the i32 accumulator.
    #[inline]
    pub fn eval(&self, input: &[u16; 3]) -> [u16; 3] {
        // In[] is assured to come from an 8-bit number (a << 8 | a), so & 0xFF.
        let ri = (input[0] & 0xff) as usize;
        let gi = (input[1] & 0xff) as usize;
        let bi = (input[2] & 0xff) as usize;

        // First shaper, also converts to 1.14 fixed.
        let r = self.shaper1[0][ri];
        let g = self.shaper1[1][gi];
        let b = self.shaper1[2][bi];

        // Evaluate the matrix in 1.14 fixed point.
        let l1 =
            (self.mat[0][0] * r + self.mat[0][1] * g + self.mat[0][2] * b + self.off[0] + 0x2000)
                >> 14;
        let l2 =
            (self.mat[1][0] * r + self.mat[1][1] * g + self.mat[1][2] * b + self.off[1] + 0x2000)
                >> 14;
        let l3 =
            (self.mat[2][0] * r + self.mat[2][1] * g + self.mat[2][2] * b + self.off[2] + 0x2000)
                >> 14;

        // Clip to 0..1.0 range (0..16384 in 1.14).
        let ro = clip_1fixed14(l1);
        let go = clip_1fixed14(l2);
        let bo = clip_1fixed14(l3);

        // Second shaper.
        [
            self.shaper2[0][ro],
            self.shaper2[1][go],
            self.shaper2[2][bo],
        ]
    }
}

/// The `(l < 0) ? 0 : ((l > 16384) ? 16384 : l)` clip from `MatShaperEval16`,
/// returning a `usize` index into the second shaper.
#[inline]
fn clip_1fixed14(l: S1Fixed14) -> usize {
    if l < 0 {
        0
    } else if l > 16384 {
        16384
    } else {
        l as usize
    }
}

/// lcms2 `CloseEnough` (cmsmtrx.c:92): `|b - a| < 1/65535`.
fn close_enough(a: f64, b: f64) -> bool {
    (b - a).abs() < (1.0 / 65535.0)
}

/// lcms2 `_cmsMAT3isIdentity` (cmsmtrx.c:98): all entries `CloseEnough` the 3x3
/// identity.
fn mat_is_identity(m: &Mat3) -> bool {
    let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for i in 0..3 {
        for j in 0..3 {
            if !close_enough(m[i][j], id[i][j]) {
                return false;
            }
        }
    }
    true
}

/// lcms2 `_cmsMAT3per(r, a, b)` (cmsmtrx.c:114): `r = a * b` for row-major 3x3.
fn mat3_per(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

/// Read a `Stage::Matrix` that is exactly 3x3 into a row-major array plus its
/// optional offset. Returns `None` if the stage is not a 3x3 matrix.
fn matrix_3x3(stage: &Stage) -> Option<(Mat3, Option<[f64; 3]>)> {
    if let Stage::Matrix {
        rows: 3,
        cols: 3,
        m,
        offset,
    } = stage
    {
        let mut mat = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                mat[i][j] = m[i * 3 + j];
            }
        }
        let off = offset.as_ref().map(|o| [o[0], o[1], o[2]]);
        Some((mat, off))
    } else {
        None
    }
}

fn tone_curves_3(stage: &Stage) -> Option<&[ToneCurve]> {
    match stage {
        Stage::ToneCurves(c) if c.len() == 3 => Some(c),
        _ => None,
    }
}

/// lcms2 `OptimizeMatrixShaper` (`cmsopt.c:1681-1805`), the detection + build.
///
/// Detects the RGB matrix-shaper pattern in `lut` and, if it fires, returns the
/// precomputed [`MatShaper8Data`]. Returns `None` (so the caller falls back to
/// the full pipeline eval) when:
/// - the input or output format is not 3-channel,
/// - the input format is not 8-bit (`_cmsFormatterIs8bit`),
/// - the stage sequence is not `Curves, Matrix, Matrix, Curves` or
///   `Curves, Matrix, Curves` (each matrix 3x3),
/// - the first matrix carries an offset (the `Data1->Offset != NULL` guard),
/// - the merged matrix is the identity with no offset (lcms2 routes that to
///   `OptimizeByJoiningCurves`, a different optimizer — out of scope here).
pub fn try_optimize(lut: &Pipeline, in_fmt: u32, out_fmt: u32) -> Option<MatShaper8Data> {
    let inf = PixelFormat(in_fmt);
    let outf = PixelFormat(out_fmt);

    // Only works on RGB to RGB (3 channels in + out).
    if inf.channels() != 3 || outf.channels() != 3 {
        return None;
    }
    // Only works on 8-bit input.
    if inf.bytes() != 1 {
        return None;
    }
    // Float never takes this path.
    if inf.is_float() || outf.is_float() {
        return None;
    }

    let stages = lut.stages();
    let is_8bit_output = outf.bytes() == 1;

    // shaper-matrix-matrix-shaper or shaper-matrix-shaper.
    let (curve1, mat, off, curve2) = if stages.len() == 4 {
        let curve1 = tone_curves_3(&stages[0])?;
        let (m1, off1) = matrix_3x3(&stages[1])?;
        let (m2, off2) = matrix_3x3(&stages[2])?;
        let curve2 = tone_curves_3(&stages[3])?;

        // Input offset should be zero (Data1->Offset != NULL).
        if off1.is_some() {
            return None;
        }

        // Multiply both matrices: res = Mat2 * Mat1 (_cmsMAT3per(res, Data2, Data1)).
        let res = mat3_per(&m2, &m1);
        // Only 2nd matrix has offset.
        (curve1, res, off2, curve2)
    } else if stages.len() == 3 {
        let curve1 = tone_curves_3(&stages[0])?;
        let (m1, off1) = matrix_3x3(&stages[1])?;
        let curve2 = tone_curves_3(&stages[2])?;
        (curve1, m1, off1, curve2)
    } else {
        return None;
    };

    // If the merged matrix is identity with no offset, lcms2 defers to
    // OptimizeByJoiningCurves — not the matrix-shaper path. Fall back.
    if mat_is_identity(&mat) && off.is_none() {
        return None;
    }

    Some(MatShaper8Data::build(
        curve1,
        &mat,
        off.as_ref(),
        curve2,
        is_8bit_output,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::ToneCurve;

    /// Build a linear (identity) gamma-1.0 tone curve.
    fn linear_curve() -> ToneCurve {
        crate::curve::build_gamma(1.0)
    }

    #[test]
    fn double_to_1fixed14_rounds_half_up() {
        // 0.5 * 16384 = 8192 exactly.
        assert_eq!(double_to_1fixed14(0.5), 8192);
        // 1.0 -> 16384.
        assert_eq!(double_to_1fixed14(1.0), 16384);
        // floor(x*16384 + 0.5).
        assert_eq!(double_to_1fixed14(0.0), 0);
        // A value needing the +0.5 round.
        let x = 1.0 / 16384.0 / 2.0; // 0.5 in fixed -> floor(0.5 + 0.5) = 1.
        assert_eq!(double_to_1fixed14(x), 1);
    }

    #[test]
    fn eval_identity_matshaper_is_passthrough_8bit() {
        // Identity curves + identity matrix => out byte == in byte.
        let c1 = [linear_curve(), linear_curve(), linear_curve()];
        let c2 = [linear_curve(), linear_curve(), linear_curve()];
        // NOT mat_is_identity-rejected here: we call build directly.
        let mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let p = MatShaper8Data::build(&c1, &mat, None, &c2, true);

        for v in [0u8, 1, 64, 127, 128, 200, 254, 255] {
            let w = from_8_to_16(v);
            let out = p.eval(&[w, w, w]);
            // 8-bit output is stored as FROM_8_TO_16(byte); recover the byte.
            assert_eq!(from_16_to_8(out[0]), v, "channel R at in byte {v}");
            assert_eq!(from_16_to_8(out[1]), v);
            assert_eq!(from_16_to_8(out[2]), v);
        }
    }

    #[test]
    fn eval_known_scaling_matrix() {
        // Matrix scales R by 0.5; identity curves. For in byte 255 -> r=1.0 in
        // 1.14 (16384), l1 = (8192*16384 ... ) — compute via the same fixed math.
        let c1 = [linear_curve(), linear_curve(), linear_curve()];
        let c2 = [linear_curve(), linear_curve(), linear_curve()];
        let mat = [[0.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let p = MatShaper8Data::build(&c1, &mat, None, &c2, false);

        let w = from_8_to_16(255);
        let out = p.eval(&[w, w, w]);
        // R halved: ~0.5 in 0..1 -> ~32768 (the shaper2 quantization of 0.5).
        // Compute the exact expected via the same shaper path.
        // Shaper1[255] = DOUBLE_TO_1FIXED14(1.0) = 16384.
        let r = p.shaper1[0][255];
        let l1 = (p.mat[0][0] * r + 0x2000) >> 14;
        let idx = clip_1fixed14(l1);
        assert_eq!(out[0], p.shaper2[0][idx]);
        // Sanity: it's near half of full-scale.
        assert!(out[0] > 30000 && out[0] < 35000, "R={}", out[0]);
        // G and B unchanged -> full scale.
        assert_eq!(out[1], p.shaper2[1][16384]);
    }
}
