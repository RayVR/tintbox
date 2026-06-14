//! Pipeline stages (lcms2 `cmsStage` / Multi-Processing Elements).
//!
//! Each stage maps `input_channels` floats to `output_channels` floats, always
//! evaluated in the float domain. This mirrors lcms2's per-element `EvalPtr`
//! callbacks (cmslut.c): `EvaluateCurves` (cmsSigCurveSetElemType) and
//! `EvaluateMatrix` (cmsSigMatrixElemType). The CLUT and Lab/XYZ elements arrive
//! in a later task; the variant set is intentionally left open.

use crate::color::{CIELab, CIEXYZ};
use crate::curve::ToneCurve;
use crate::pcs::{lab_to_xyz, xyz_to_lab};

use super::clut::Clut;

/// `MAX_ENCODEABLE_XYZ` (lcms2_internal.h:71): `1.0 + 32767.0/32768.0`, the
/// `XYZadj` divisor/multiplier the Lab2XYZ / XYZ2Lab stage evals use to map the
/// 0..1.0 normalized PCS domain to/from raw XYZ.
const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;

/// `_cmsStageAllocLabV2ToV4`'s diagonal scale `65535.0/65280.0` (cmslut.c:1029).
const V2_TO_V4: f64 = 65535.0 / 65280.0;

/// `_cmsStageAllocLabV4ToV2`'s diagonal scale `65280.0/65535.0` (cmslut.c:1045).
const V4_TO_V2: f64 = 65280.0 / 65535.0;

/// A single processing element in a [`Pipeline`](super::Pipeline).
#[derive(Clone, Debug, PartialEq)]
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

    /// `cmsSigCLutElemType`: an n-D color lookup table (cmslut.c:428-697). Input
    /// width is `params.n_inputs`, output width `params.n_outputs`.
    Clut(Clut),

    /// `cmsSigLab2XYZElemType` (cmslut.c:947-979). 3 -> 3. Decodes a normalized
    /// V4 Lab triple to raw Lab (`L*100`, `a*255-128`, `b*255-128`), runs
    /// `cmsLab2XYZ(NULL,..)`, and normalizes XYZ by `MAX_ENCODEABLE_XYZ`.
    Lab2Xyz,

    /// `cmsSigXYZ2LabElemType` (cmslut.c:1161-1190). 3 -> 3. Scales normalized
    /// XYZ by `MAX_ENCODEABLE_XYZ`, runs `cmsXYZ2Lab(NULL,..)`, and normalizes
    /// V4 Lab to 0..1.0 (`L/100`, `(a+128)/255`, `(b+128)/255`).
    Xyz2Lab,

    /// `cmsSigLabV2toV4` matrix form (`_cmsStageAllocLabV2ToV4`, cmslut.c:1027).
    /// 3 -> 3 diagonal scale by `65535/65280`.
    LabV2ToV4,

    /// `cmsSigLabV4toV2` matrix form (`_cmsStageAllocLabV4ToV2`, cmslut.c:1043).
    /// 3 -> 3 diagonal scale by `65280/65535`.
    LabV4ToV2,
}

impl Stage {
    /// Number of input channels this stage consumes.
    pub fn input_channels(&self) -> usize {
        match self {
            Stage::ToneCurves(curves) => curves.len(),
            Stage::Matrix { cols, .. } => *cols,
            Stage::Identity(n) => *n,
            Stage::Clut(c) => c.input_channels(),
            Stage::Lab2Xyz | Stage::Xyz2Lab | Stage::LabV2ToV4 | Stage::LabV4ToV2 => 3,
        }
    }

    /// Number of output channels this stage produces.
    pub fn output_channels(&self) -> usize {
        match self {
            Stage::ToneCurves(curves) => curves.len(),
            Stage::Matrix { rows, .. } => *rows,
            Stage::Identity(n) => *n,
            Stage::Clut(c) => c.output_channels(),
            Stage::Lab2Xyz | Stage::Xyz2Lab | Stage::LabV2ToV4 | Stage::LabV4ToV2 => 3,
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

            // lcms2 EvaluateCLUTfloat / EvaluateCLUTfloatIn16 (cmslut.c:434-456).
            Stage::Clut(c) => c.eval(input, output),

            // lcms2 EvaluateLab2XYZ (cmslut.c:947-972). `In[i]` is f32; each
            // scale constant is f64, so the products widen to f64 exactly as the
            // C `Lab.L = In[0] * 100.0` does. `cmsLab2XYZ(NULL,..)` defaults to
            // D50 (our `lab_to_xyz(None,..)`).
            Stage::Lab2Xyz => {
                let lab = CIELab {
                    l: input[0] as f64 * 100.0,
                    a: input[1] as f64 * 255.0 - 128.0,
                    b: input[2] as f64 * 255.0 - 128.0,
                };
                let xyz = lab_to_xyz(None, lab);
                output[0] = (xyz.x / MAX_ENCODEABLE_XYZ) as f32;
                output[1] = (xyz.y / MAX_ENCODEABLE_XYZ) as f32;
                output[2] = (xyz.z / MAX_ENCODEABLE_XYZ) as f32;
            }

            // lcms2 EvaluateXYZ2Lab (cmslut.c:1161-1184). `cmsXYZ2Lab(NULL,..)`
            // defaults to D50 (our `xyz_to_lab(None,..)`).
            Stage::Xyz2Lab => {
                let xyz = CIEXYZ {
                    x: input[0] as f64 * MAX_ENCODEABLE_XYZ,
                    y: input[1] as f64 * MAX_ENCODEABLE_XYZ,
                    z: input[2] as f64 * MAX_ENCODEABLE_XYZ,
                };
                let lab = xyz_to_lab(None, xyz);
                output[0] = (lab.l / 100.0) as f32;
                output[1] = ((lab.a + 128.0) / 255.0) as f32;
                output[2] = ((lab.b + 128.0) / 255.0) as f32;
            }

            // `_cmsStageAllocLabV2ToV4` (cmslut.c:1027): a 3x3 diagonal matrix
            // with no offset. EvaluateMatrix accumulates each output in an f64
            // temporary (`Tmp += In[j] * M[..]`) and stores as f32. For the pure
            // diagonal that reduces to `Tmp = In[i] * V2_TO_V4`.
            Stage::LabV2ToV4 => {
                for i in 0..3 {
                    let tmp: f64 = input[i] as f64 * V2_TO_V4;
                    output[i] = tmp as f32;
                }
            }

            // `_cmsStageAllocLabV4ToV2` (cmslut.c:1043): diagonal `V4_TO_V2`.
            Stage::LabV4ToV2 => {
                for i in 0..3 {
                    let tmp: f64 = input[i] as f64 * V4_TO_V2;
                    output[i] = tmp as f32;
                }
            }
        }
    }
}
