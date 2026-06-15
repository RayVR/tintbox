//! Pipeline optimization — a **swappable strategy** the [`Transform`] holds, not
//! inlined into eval (mirrors the [`compat::floor`](crate::compat::floor) seam).
//!
//! lcms2 runs `_cmsOptimizePipeline` (`cmsopt.c:1919`) after building the
//! device-link pipeline. Note that even with `cmsFLAGS_NOOPTIMIZE` it first runs
//! `PreOptimize` (`cmsopt.c:1952`) — identity/inverse removal AND the
//! `_MultiplyMatrix` adjacent-matrix merge — and only THEN returns at the
//! NOOPTIMIZE gate (`cmsopt.c:1961`) to evaluate the pipeline in place. rcms
//! replicates `PreOptimize` at link time (see
//! [`Pipeline::pre_optimize`](crate::pipeline::Pipeline::pre_optimize), called
//! from [`default_icc_intents`](crate::link::default_icc_intents)), so the device
//! link the strategies receive is already pre-optimized. rcms exposes the
//! remaining optimizer choice as [`OptimizationStrategy`]:
//!
//! - [`Accurate`](OptimizationStrategy::Accurate) (DEFAULT) — full in-place
//!   pipeline eval, exactly what slice-5 `do_transform` does. Bit-identical to
//!   lcms2 `-NOOPTIMIZE` (because the link already carries lcms2's unconditional
//!   `PreOptimize` matrix merge), and MORE accurate than lcms2-DEFAULT. Never
//!   produces an optimized eval; the structural simplifications lcms2 applies
//!   unconditionally are baked into the linked pipeline, not the strategy.
//! - [`Lcms2Compat`](OptimizationStrategy::Lcms2Compat) (opt-in) — replicate
//!   lcms2's DEFAULT optimizer so the output matches stock lcms2-default. This
//!   module implements its **matrix-shaper** optimizer
//!   ([`matshaper`]); the lossy resampling bake is a later task. When the
//!   matrix-shaper pattern does not match, `Lcms2Compat` falls back to the
//!   accurate pipeline eval (still correct, just not bit-identical to
//!   lcms2-default for that pipeline).
//!
//! The strategy is selected at [`Transform`](crate::transform::Transform)
//! construction and produces an [`OptimizedEval`] the per-pixel `do_transform`
//! loop calls — swapping strategies never touches the formatter or the loop.

pub mod matshaper;

use crate::pipeline::Pipeline;
use matshaper::MatShaper8Data;

/// Which pipeline-optimization posture a [`Transform`](crate::transform::Transform)
/// uses. Default is [`Accurate`](Self::Accurate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptimizationStrategy {
    /// Full in-place pipeline eval (lcms2 `-NOOPTIMIZE`). Most accurate; the
    /// rcms default.
    #[default]
    Accurate,
    /// Replicate lcms2's DEFAULT optimizer (currently: the matrix-shaper
    /// `MatShaperEval16` 1.14-fixed evaluator). Opt-in; for drop-in bit-identity
    /// with stock lcms2-default or as a speed knob.
    Lcms2Compat,
}

/// The eval a [`Transform`](crate::transform::Transform) calls per pixel, built
/// by [`OptimizationStrategy::build`]. Either the original pipeline (no optimizer
/// fired — evaluate in place) or a specialized fast eval.
#[derive(Clone)]
pub enum OptimizedEval {
    /// No optimizer fired; evaluate the pipeline in place (the accurate path).
    Pipeline,
    /// The matrix-shaper `MatShaperEval16` evaluator fired (8/16-bit path only;
    /// `eval_16`-style input). Boxed: the precomputed tables are ~50 KB, so
    /// inlining them would bloat every `Pipeline`-variant value.
    MatShaper(Box<MatShaper8Data>),
}

impl OptimizationStrategy {
    /// Build the [`OptimizedEval`] for `lut` under the given in/out format words
    /// (lcms2 `_cmsOptimizePipeline`). `Accurate` always yields
    /// [`OptimizedEval::Pipeline`]; `Lcms2Compat` tries the matrix-shaper
    /// optimizer and falls back to `Pipeline` if it does not fire.
    pub fn build(self, lut: &Pipeline, in_fmt: u32, out_fmt: u32) -> OptimizedEval {
        match self {
            OptimizationStrategy::Accurate => OptimizedEval::Pipeline,
            OptimizationStrategy::Lcms2Compat => {
                match matshaper::try_optimize(lut, in_fmt, out_fmt) {
                    Some(data) => OptimizedEval::MatShaper(Box::new(data)),
                    None => OptimizedEval::Pipeline,
                }
            }
        }
    }
}
