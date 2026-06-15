//! Bit-identical (LOSSLESS) matrix-shaper fast path for
//! [`AccurateFast`](super::OptimizationStrategy::AccurateFast).
//!
//! This is the lossless analogue of lcms2's LOSSY `MatShaperEval16`
//! ([`matshaper`](super::matshaper)). Both detect the same RGB matrix-shaper
//! device-link shape
//!
//! ```text
//!   ToneCurves(3) -> Matrix(3x3) -> ToneCurves(3)
//! ```
//!
//! but where `MatShaperEval16` quantizes to 1.14 signed fixed-point and indexes
//! the output curves by an 8-bit value (introducing error), this evaluator
//! reproduces the **exact** arithmetic of the in-place pipeline eval
//! ([`Pipeline::eval_16`](crate::pipeline::Pipeline::eval_16)) — only memoizing
//! the per-pixel input-curve evaluation into a table indexed by the 8-bit input
//! byte:
//!
//! 1. **Input-curve LUT.** For each of the 3 channels, a 256-entry table holds
//!    `curve.eval_float(from_8_to_16(byte) as f32 / 65535.0_f32)`. This is the
//!    EXACT value the `ToneCurves` stage produces for that input byte in the
//!    Pipeline path: the 8-bit unpack yields `win = (byte<<8)|byte`, and
//!    [`eval_16`](crate::pipeline::Pipeline::eval_16) converts it with the f32
//!    division `win as f32 / 65535.0_f32`. The LUT entry is a straight memoization
//!    of `Stage::eval`'s tone-curve arm — byte-for-byte identical, no
//!    `i/255`-style requantization (that is the lossy trap `fill_first_shaper`
//!    falls into).
//! 2. **Matrix.** The merged 3x3 matrix is evaluated by the SAME `Stage::eval`
//!    Matrix arm (f64 accumulation, `tmp += in[j] as f64 * m[..]`, stored as f32,
//!    no FMA). We re-use the actual [`Stage`] so the arithmetic is literally the
//!    same code.
//! 3. **Output curves.** Evaluated by the EXACT `curve.eval_float` (full
//!    resolution), NOT an 8-bit-indexed table — then packed with the SAME
//!    `quick_saturate_word(out as f64 * 65535.0)` the Pipeline output uses.
//!
//! The net effect: identical numeric output to
//! [`OptimizedEval::Pipeline`](super::OptimizedEval::Pipeline) for every 8-bit
//! input pixel, but the per-pixel input-curve `powf`/parametric eval is replaced
//! by a table lookup.

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::curve::ToneCurve;
use crate::fixed::from_8_to_16;
use crate::format::PixelFormat;
use crate::pipeline::{Pipeline, Stage};

/// The precomputed lossless matrix-shaper evaluator. Holds the exact input-curve
/// LUTs (per channel, indexed by the 8-bit input byte), the merged matrix stage
/// (evaluated by the unchanged [`Stage::eval`]), and the output tone curves
/// (evaluated at full resolution by [`ToneCurve::eval_float`]).
#[derive(Clone)]
pub struct LosslessMatShaper {
    /// `in_lut[ch][byte]` = `curve_in[ch].eval_float(from_8_to_16(byte) as f32 /
    /// 65535.0_f32)`. The EXACT output of the first `ToneCurves` stage for that
    /// input byte under the Pipeline path.
    in_lut: [[f32; 256]; 3],
    /// The merged 3x3 matrix stage, evaluated by the unchanged `Stage::eval`
    /// (f64 accumulation, store-as-f32). Kept as a `Stage` so the arithmetic is
    /// literally the same code the accurate path runs.
    matrix: Stage,
    /// The output tone curves, evaluated at full resolution by `eval_float`.
    out_curves: [ToneCurve; 3],
}

impl LosslessMatShaper {
    /// Evaluate one pixel. `input` is 3 u16 channels as produced by the 8-bit
    /// input formatter (`win = (byte<<8)|byte`), so `& 0xff` recovers the byte
    /// that indexes the input LUT. The result is 3 u16 channels exactly as the
    /// Pipeline path's `eval_16` would produce (output quantized by
    /// `quick_saturate_word(x as f64 * 65535.0)`).
    #[inline]
    pub fn eval(&self, input: &[u16; 3]) -> [u16; 3] {
        // Input curves via the exact-memoized LUT. `input[ch]` came from an 8-bit
        // byte as `(byte<<8)|byte`, so the low byte is the index.
        let stage_in = [
            self.in_lut[0][(input[0] & 0xff) as usize],
            self.in_lut[1][(input[1] & 0xff) as usize],
            self.in_lut[2][(input[2] & 0xff) as usize],
        ];

        // Matrix: the unchanged Stage::eval (f64 accumulation → f32 store).
        let mut stage_mat = [0.0f32; 3];
        self.matrix.eval(&stage_in, &mut stage_mat);

        // Output curves at full resolution, then the Pipeline's float→16 pack.
        let mut out = [0u16; 3];
        for ch in 0..3 {
            let y = self.out_curves[ch].eval_float(stage_mat[ch]);
            out[ch] = Lcms2Floor::quick_saturate_word(y as f64 * 65535.0);
        }
        out
    }

    /// Build the input-curve LUTs from the input tone curves. Each entry is the
    /// EXACT value the first `ToneCurves` stage produces for the corresponding
    /// input byte in the Pipeline path:
    /// `curve.eval_float(from_8_to_16(byte) as f32 / 65535.0_f32)`.
    fn build_in_lut(curve_in: &[ToneCurve]) -> [[f32; 256]; 3] {
        let mut lut = [[0.0f32; 256]; 3];
        for (ch, table) in lut.iter_mut().enumerate() {
            for (byte, slot) in table.iter_mut().enumerate() {
                // Mirror the Pipeline's 8-bit input scaling EXACTLY: the unpack
                // produces win = (byte<<8)|byte, and eval_16 converts it via the
                // f32 division `win as f32 / 65535.0_f32`.
                let win = from_8_to_16(byte as u8);
                let r = win as f32 / 65535.0_f32;
                *slot = curve_in[ch].eval_float(r);
            }
        }
        lut
    }
}

/// Read a `Stage::Matrix` that is exactly 3x3, returning a clone of the stage.
fn matrix_3x3(stage: &Stage) -> Option<Stage> {
    if let Stage::Matrix {
        rows: 3, cols: 3, ..
    } = stage
    {
        Some(stage.clone())
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

/// Detect the RGB matrix-shaper shape and, if it matches, build a
/// [`LosslessMatShaper`]. Returns `None` (so the caller falls back to the full
/// pipeline eval) when:
/// - the input or output format is not 3-channel,
/// - the input format is not 8-bit,
/// - either format is float,
/// - the stage sequence is not `Curves, Matrix, Curves` (3x3 matrix).
///
/// The pipeline is the **post-`pre_optimize`** device link, so two adjacent
/// matrices have already been merged (`_MultiplyMatrix`) into a single matrix —
/// we therefore only need to handle the 3-stage `Curves, Matrix, Curves` shape.
/// (A 4-stage `Curves, Matrix, Matrix, Curves` only survives pre_optimize when a
/// matrix carries an offset, which the lossy detector also rejects; we decline it
/// here rather than re-merge, since re-merging would drop the intermediate f32
/// round the accurate path performs and break bit-identity.)
pub fn try_optimize(lut: &Pipeline, in_fmt: u32, out_fmt: u32) -> Option<LosslessMatShaper> {
    let inf = PixelFormat(in_fmt);
    let outf = PixelFormat(out_fmt);

    // RGB to RGB (3 channels in + out).
    if inf.channels() != 3 || outf.channels() != 3 {
        return None;
    }
    // 8-bit input only (the LUT is indexed by an input byte).
    if inf.bytes() != 1 {
        return None;
    }
    // Float never takes this path.
    if inf.is_float() || outf.is_float() {
        return None;
    }

    let stages = lut.stages();
    if stages.len() != 3 {
        return None;
    }

    let curve_in = tone_curves_3(&stages[0])?;
    let matrix = matrix_3x3(&stages[1])?;
    let curve_out = tone_curves_3(&stages[2])?;

    let in_lut = LosslessMatShaper::build_in_lut(curve_in);
    let out_curves = [
        curve_out[0].clone(),
        curve_out[1].clone(),
        curve_out[2].clone(),
    ];

    Some(LosslessMatShaper {
        in_lut,
        matrix,
        out_curves,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::build_gamma;

    /// The LUT entry must equal the scalar Pipeline path bit-for-bit: for every
    /// input byte, `in_lut[ch][byte]` == `curve.eval_float((byte<<8|byte) as f32
    /// / 65535)`.
    #[test]
    fn in_lut_is_exact_memoization() {
        let curve = build_gamma(2.4);
        let curves = [curve.clone(), curve.clone(), curve.clone()];
        let lut = LosslessMatShaper::build_in_lut(&curves);
        for byte in 0u16..=255 {
            let win = from_8_to_16(byte as u8);
            let r = win as f32 / 65535.0_f32;
            let expected = curve.eval_float(r);
            assert_eq!(
                lut[0][byte as usize].to_bits(),
                expected.to_bits(),
                "byte {byte}: LUT diverges from scalar eval_float"
            );
        }
    }

    /// A full `Curves -> Matrix -> Curves` pipeline evaluated by the lossless fast
    /// path must equal the in-place `eval_16` bit-for-bit, for every 8-bit input.
    #[test]
    fn fast_path_equals_pipeline_eval_16() {
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(2.2),
            build_gamma(2.2),
            build_gamma(2.2),
        ]))
        .unwrap();
        p.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            // A non-identity, non-diagonal matrix (channel mixing).
            m: vec![0.9, 0.05, 0.05, 0.1, 0.8, 0.1, 0.02, 0.13, 0.85],
            offset: None,
        })
        .unwrap();
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(1.0 / 2.2),
            build_gamma(1.0 / 2.2),
            build_gamma(1.0 / 2.2),
        ]))
        .unwrap();

        let in_fmt = (crate::format::decode::PT_RGB << 16) | (3u32 << 3) | 1;
        let out_fmt = in_fmt;
        let fast = try_optimize(&p, in_fmt, out_fmt).expect("matrix-shaper detected");

        for r in 0u16..=255 {
            for &g in &[0u16, 1, 64, 127, 128, 200, 254, 255] {
                for &b in &[0u16, 17, 99, 255] {
                    let win = [
                        from_8_to_16(r as u8),
                        from_8_to_16(g as u8),
                        from_8_to_16(b as u8),
                    ];
                    let fast_out = fast.eval(&win);
                    let slow_out = p.eval_16(&win);
                    assert_eq!(
                        fast_out,
                        [slow_out[0], slow_out[1], slow_out[2]],
                        "mismatch at rgb byte ({r},{g},{b})"
                    );
                }
            }
        }
    }

    /// Detection declines a 4-stage (un-merged double-matrix) pipeline rather than
    /// re-merging (which would break bit-identity).
    #[test]
    fn declines_four_stage_pipeline() {
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(2.2),
            build_gamma(2.2),
            build_gamma(2.2),
        ]))
        .unwrap();
        p.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            offset: None,
        })
        .unwrap();
        p.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            offset: None,
        })
        .unwrap();
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(1.0 / 2.2),
            build_gamma(1.0 / 2.2),
            build_gamma(1.0 / 2.2),
        ]))
        .unwrap();
        let in_fmt = (crate::format::decode::PT_RGB << 16) | (3u32 << 3) | 1;
        assert!(try_optimize(&p, in_fmt, in_fmt).is_none());
    }
}
