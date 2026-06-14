//! Pipeline stages (lcms2 `cmsStage` / Multi-Processing Elements).
//!
//! Each stage maps `input_channels` floats to `output_channels` floats, always
//! evaluated in the float domain. This mirrors lcms2's per-element `EvalPtr`
//! callbacks (cmslut.c): `EvaluateCurves` (cmsSigCurveSetElemType) and
//! `EvaluateMatrix` (cmsSigMatrixElemType). The CLUT and Lab/XYZ elements arrive
//! in a later task; the variant set is intentionally left open.

use crate::curve::ToneCurve;

/// A single processing element in a [`Pipeline`](super::Pipeline).
#[derive(Clone, Debug)]
pub enum Stage {
    /// `cmsSigCurveSetElemType`: one tone curve per channel. Input and output
    /// width both equal the curve count (cmslut.c `cmsStageAllocToneCurves`,
    /// allocated `nChannels -> nChannels`).
    ToneCurves(Vec<ToneCurve>),

    /// `cmsSigMatrixElemType`: an affine map `out = M * in + offset`. The matrix
    /// is row-major with `rows * cols` entries (lcms2 stores `Double` row-major,
    /// indexed `[i*InputChannels + j]`); `offset`, when present, has `rows`
    /// entries. Input width is `cols`, output width is `rows` (cmslut.c
    /// `cmsStageAllocMatrix` allocates `Cols -> Rows`).
    Matrix {
        rows: usize,
        cols: usize,
        m: Vec<f64>,
        offset: Option<Vec<f64>>,
    },

    /// Pass-through of `n` channels (lcms2 `_cmsStageAllocIdentityCurves` is an
    /// identity tone-curve set; this is the cheaper structural equivalent).
    Identity(usize),
}

impl Stage {
    /// Number of input channels this stage consumes.
    pub fn input_channels(&self) -> usize {
        match self {
            Stage::ToneCurves(curves) => curves.len(),
            Stage::Matrix { cols, .. } => *cols,
            Stage::Identity(n) => *n,
        }
    }

    /// Number of output channels this stage produces.
    pub fn output_channels(&self) -> usize {
        match self {
            Stage::ToneCurves(curves) => curves.len(),
            Stage::Matrix { rows, .. } => *rows,
            Stage::Identity(n) => *n,
        }
    }

    /// Evaluate the stage, writing `output_channels()` floats into `output`.
    ///
    /// `output` must have at least `output_channels()` slots; `input` at least
    /// `input_channels()`. The float domain is lcms2's 0..1.0 notation, but no
    /// clamping is performed here (matching the C callbacks, which clamp only at
    /// the 16-bit boundary).
    pub fn eval(&self, input: &[f32], output: &mut [f32]) {
        match self {
            // lcms2 EvaluateCurves (cmslut.c:167-184).
            Stage::ToneCurves(curves) => {
                for (i, curve) in curves.iter().enumerate() {
                    output[i] = curve.eval_float(input[i]);
                }
            }

            // lcms2 EvaluateMatrix (cmslut.c:312-336). Accumulate each output in
            // an f64 temporary and cast to f32 only on store — verbatim from the
            // C, which uses a `cmsFloat64Number Tmp` precisely to avoid precision
            // loss. No fused multiply-add: the C is a plain `Tmp += In[j] * M`.
            Stage::Matrix {
                rows,
                cols,
                m,
                offset,
            } => {
                for i in 0..*rows {
                    let mut tmp: f64 = 0.0;
                    for j in 0..*cols {
                        tmp += input[j] as f64 * m[i * *cols + j];
                    }
                    if let Some(off) = offset {
                        tmp += off[i];
                    }
                    output[i] = tmp as f32;
                }
            }

            Stage::Identity(n) => {
                output[..*n].copy_from_slice(&input[..*n]);
            }
        }
    }
}
