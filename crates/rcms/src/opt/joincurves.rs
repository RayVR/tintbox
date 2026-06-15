//! lcms2's curve-collapse optimizer `OptimizeByJoiningCurves` (cmsopt.c:1398-1519).
//!
//! When the (pre-optimized) device link consists ONLY of curve-set stages, lcms2
//! collapses the whole chain into one per-channel tabulated curve by sampling its
//! gray-ramp response at 4096 points, then installs a fast per-channel table
//! lookup: `FastEvaluateCurves8` (8-bit input: `Out = Curve[In >> 8]`) or
//! `FastEvaluateCurves16` (16-bit input: `Out = Curve[In]`). If the collapsed
//! curves are all linear it installs the identity (`FastIdentity16`).
//!
//! The collapsed-curve tables are evaluated EXACTLY as lcms2 does: `CurvesAlloc`
//! (cmsopt.c:1300-1348) precomputes 256 entries (8-bit, indexed by `FROM_8_TO_16`)
//! or 65536 entries (16-bit) of `cmsEvalToneCurve16` over the collapsed curve.

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::curve::{build_tabulated_16, ToneCurve};
use crate::fixed::from_8_to_16;
use crate::format::decode::PixelFormat;
use crate::interp::MAX_STAGE_CHANNELS;
use crate::pipeline::{Pipeline, Stage};

const PRELINEARIZATION_POINTS: usize = 4096;

/// The installed curve-collapse eval (an [`super::OptimizedEval`] payload).
/// `tables[i]` is the precomputed lookup for channel `i`: 256 entries (8-bit) or
/// 65536 entries (16-bit). `is_8bit` selects the `>> 8` index. Identity (all
/// linear) installs an empty `tables` and evaluates as a passthrough.
#[derive(Clone)]
pub struct CurvesEval {
    n_curves: usize,
    is_8bit: bool,
    /// `None` => identity passthrough (lcms2 `FastIdentity16`).
    tables: Option<Vec<Vec<u16>>>,
}

impl CurvesEval {
    /// lcms2 `FastEvaluateCurves8`/`FastEvaluateCurves16`/`FastIdentity16`.
    pub fn eval(&self, input: &[u16], output: &mut [u16]) {
        match &self.tables {
            None => {
                // FastIdentity16: copy InputChannels through.
                output[..self.n_curves].copy_from_slice(&input[..self.n_curves]);
            }
            Some(tables) => {
                for i in 0..self.n_curves {
                    let idx = if self.is_8bit {
                        (input[i] >> 8) as usize
                    } else {
                        input[i] as usize
                    };
                    output[i] = tables[i][idx];
                }
            }
        }
    }
}

/// lcms2 `cmsIsToneCurveLinear`-based `AllCurvesAreLinear` over a single curve set
/// (cmsopt.c:451-466) applied to the collapsed curves.
fn all_curves_linear(curves: &[ToneCurve]) -> bool {
    curves.iter().all(|c| c.is_linear())
}

/// lcms2 `CurvesAlloc` (cmsopt.c:1300-1348): precompute the per-channel lookup
/// tables. `n_elements` is 256 (8-bit) or 65536 (16-bit).
fn curves_alloc(curves: &[ToneCurve], n_elements: usize) -> Vec<Vec<u16>> {
    curves
        .iter()
        .map(|c| {
            let mut tab = vec![0u16; n_elements];
            if n_elements == 256 {
                for (j, slot) in tab.iter_mut().enumerate() {
                    *slot = c.eval_16(from_8_to_16(j as u8));
                }
            } else {
                for (j, slot) in tab.iter_mut().enumerate() {
                    *slot = c.eval_16(j as u16);
                }
            }
            tab
        })
        .collect()
}

/// lcms2 `OptimizeByJoiningCurves` (cmsopt.c:1398-1519), DEFAULT flags. Returns
/// `None` (declines) for float formats or when the pipeline is not all curve-set
/// stages.
pub fn optimize_by_joining_curves(
    lut: &Pipeline,
    in_fmt: u32,
    out_fmt: u32,
) -> Option<super::OptimizedEval> {
    let inf = PixelFormat(in_fmt);
    let outf = PixelFormat(out_fmt);

    // Lossy: never for float.
    if inf.is_float() || outf.is_float() {
        return None;
    }

    // Only curve-set stages in this LUT.
    if lut.stages().is_empty() {
        return None;
    }
    for stage in lut.stages() {
        if !matches!(stage, Stage::ToneCurves(_)) {
            return None;
        }
    }

    let n_in = lut.input_channels;

    // Compute the collapsed 16-bit curves by sampling in float (gray ramp).
    let mut tables: Vec<Vec<u16>> = (0..n_in)
        .map(|_| vec![0u16; PRELINEARIZATION_POINTS])
        .collect();
    let mut in_buf = [0f32; MAX_STAGE_CHANNELS];
    for i in 0..PRELINEARIZATION_POINTS {
        let v = (i as f64 / (PRELINEARIZATION_POINTS - 1) as f64) as f32;
        for x in in_buf.iter_mut().take(n_in) {
            *x = v;
        }
        let out = lut.eval_float(&in_buf[..n_in]);
        for (j, table) in tables.iter_mut().enumerate() {
            table[i] = Lcms2Floor::quick_saturate_word(out[j] as f64 * 65535.0);
        }
    }

    let obtained: Vec<ToneCurve> = tables.iter().map(|t| build_tabulated_16(t)).collect();
    let is_8bit = inf.bytes() == 1;

    // Maybe the curves are linear at the end -> identity.
    if all_curves_linear(&obtained) {
        return Some(super::OptimizedEval::Curves(Box::new(CurvesEval {
            n_curves: n_in,
            is_8bit,
            tables: None,
        })));
    }

    let n_elements = if is_8bit { 256 } else { 65536 };
    let lookup = curves_alloc(&obtained, n_elements);

    Some(super::OptimizedEval::Curves(Box::new(CurvesEval {
        n_curves: n_in,
        is_8bit,
        tables: Some(lookup),
    })))
}
