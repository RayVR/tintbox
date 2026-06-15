//! Pipeline optimization — a **swappable strategy** the [`Transform`] holds, not
//! inlined into eval (mirrors the [`compat::floor`](crate::compat::floor) seam).
//!
//! lcms2 runs `_cmsOptimizePipeline` (`cmsopt.c:1919`) after building the
//! device-link pipeline. Note that even with `cmsFLAGS_NOOPTIMIZE` it first runs
//! `PreOptimize` (`cmsopt.c:1952`) — identity/inverse removal AND the
//! `_MultiplyMatrix` adjacent-matrix merge — and only THEN returns at the
//! NOOPTIMIZE gate (`cmsopt.c:1961`) to evaluate the pipeline in place. tintbox
//! replicates `PreOptimize` at link time (see
//! [`Pipeline::pre_optimize`](crate::pipeline::Pipeline::pre_optimize), called
//! from [`default_icc_intents`](crate::link::default_icc_intents)), so the device
//! link the strategies receive is already pre-optimized. tintbox exposes the
//! remaining optimizer choice as [`OptimizationStrategy`]:
//!
//! - [`Accurate`](OptimizationStrategy::Accurate) (DEFAULT) — full in-place
//!   pipeline eval, exactly what slice-5 `do_transform` does. Bit-identical to
//!   lcms2 `-NOOPTIMIZE` (because the link already carries lcms2's unconditional
//!   `PreOptimize` matrix merge), and MORE accurate than lcms2-DEFAULT. Never
//!   produces an optimized eval; the structural simplifications lcms2 applies
//!   unconditionally are baked into the linked pipeline, not the strategy.
//! - [`Lcms2Compat`](OptimizationStrategy::Lcms2Compat) (opt-in) — replicate
//!   lcms2's FULL DEFAULT optimizer chain so the output is byte-identical to
//!   stock lcms2-default. [`build`](OptimizationStrategy::build) runs lcms2's
//!   `DefaultOptimization[]` list (cmsopt.c:1822-1828) in source order,
//!   first-success-wins: [`joincurves`] (`OptimizeByJoiningCurves`),
//!   [`matshaper`] (`OptimizeMatrixShaper`), then [`resampling`]
//!   (`OptimizeByComputingLinearization` and `OptimizeByResampling` — the lossy
//!   devicelink CLUT bake). If every optimizer declines (e.g. float formats, where
//!   lcms2 also declines), `Lcms2Compat` evaluates the pipeline in place — which
//!   then equals lcms2-default, since the only thing lcms2 keeps in that case is
//!   `PreOptimize`'s structural merge, already baked into the linked pipeline.
//!
//! The strategy is selected at [`Transform`](crate::transform::Transform)
//! construction and produces an [`OptimizedEval`] the per-pixel `do_transform`
//! loop calls — swapping strategies never touches the formatter or the loop.

pub mod batched;
pub mod joincurves;
pub mod lossless_matshaper;
pub mod matshaper;
pub mod resampling;

use crate::pipeline::Pipeline;
use batched::BatchedPipeline;
use joincurves::CurvesEval;
use lossless_matshaper::LosslessMatShaper;
use matshaper::MatShaper8Data;
use resampling::BakedEval;

/// Which pipeline-optimization posture a [`Transform`](crate::transform::Transform)
/// uses. Default is [`Accurate`](Self::Accurate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptimizationStrategy {
    /// Full in-place pipeline eval (lcms2 `-NOOPTIMIZE`). Most accurate; the
    /// tintbox default.
    #[default]
    Accurate,
    /// Replicate lcms2's full DEFAULT optimizer chain (curve-join, matrix-shaper,
    /// computing-linearization, resampling — the lossy devicelink CLUT bake).
    /// Opt-in; for drop-in byte-identity with stock lcms2-default or as a speed
    /// knob.
    Lcms2Compat,
    /// LOSSLESS speedups that keep byte-for-byte parity with
    /// [`Accurate`](Self::Accurate) (and thus lcms2 `-NOOPTIMIZE`), opt-in for
    /// now. Detects the RGB 8-bit-input matrix-shaper shape and installs the
    /// [`LosslessMatShaper`](lossless_matshaper::LosslessMatShaper) fast path —
    /// exact input-curve LUTs + the unchanged f64-accumulated matrix + exact
    /// output-curve eval — which removes the per-pixel `powf`/parametric input
    /// eval without changing a single output bit. Any pipeline that does not match
    /// the shape falls back to the in-place pipeline eval
    /// ([`Pipeline`](OptimizedEval::Pipeline)).
    AccurateFast,
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
    /// `OptimizeByJoiningCurves` fired: a collapsed per-channel curve lookup
    /// (lcms2 `FastEvaluateCurves8/16` / `FastIdentity16`).
    Curves(Box<CurvesEval>),
    /// `OptimizeByComputingLinearization` or `OptimizeByResampling` fired: a baked
    /// CLUT (with optional prelinearization), evaluated by lcms2
    /// `PrelinEval16`/`PrelinEval8`.
    Baked(Box<BakedEval>),
    /// The LOSSLESS matrix-shaper fast path fired
    /// ([`AccurateFast`](OptimizationStrategy::AccurateFast)): exact input-curve
    /// LUTs + the unchanged f64 matrix + exact output-curve eval. Byte-for-byte
    /// identical to [`Pipeline`](Self::Pipeline); boxed (the LUTs + curves are a
    /// few KB).
    LosslessMatShaper(Box<LosslessMatShaper>),
    /// The LOSSLESS BATCHED general/CLUT fast path fired
    /// ([`AccurateFast`](OptimizationStrategy::AccurateFast)): the pipeline's
    /// stages with the CLUT interpolator resolved once at build and (for 8-bit
    /// input) the first tone-curve stage memoized into byte LUTs, evaluated a
    /// CHUNK of pixels at a time (stage-outer/pixel-inner). Byte-for-byte identical
    /// to [`Pipeline`](Self::Pipeline); boxed (it owns the CLUT grid copies).
    Batched(Box<BatchedPipeline>),
}

/// A custom pipeline optimizer (lcms2 `cmsPluginOptimization`). Registered via
/// [`Context::set_optimizer`](crate::context::Context::set_optimizer) and
/// re-exported from [`crate::plugin`]. The optimizer is consulted at
/// [`Transform`](crate::transform::Transform) construction (T2), BEFORE the
/// builtin strategy chain; returning `None` declines and falls through to the
/// builtin path, preserving the builtin-wins invariant. The lookup resolves to a
/// concrete [`OptimizedEval`] before any hot loop, so the per-pixel path never
/// touches `Context`/`Arc`.
pub trait Optimizer: Send + Sync {
    /// Resolve a specialized evaluator for `lut` under the given in/out format
    /// words and rendering `intent`, or `None` to decline (fall through to the
    /// builtin [`OptimizationStrategy`]). Mirrors lcms2's
    /// `_cmsOptimizationPluginChunk` `OptimizePtr` callback.
    fn optimize(
        &self,
        lut: &Pipeline,
        in_fmt: u32,
        out_fmt: u32,
        intent: u32,
    ) -> Option<OptimizedEval>;
}

impl OptimizationStrategy {
    /// Build the [`OptimizedEval`] for `lut` under the given in/out format words
    /// and rendering `intent` (lcms2 `_cmsOptimizePipeline`). `Accurate` always
    /// yields [`OptimizedEval::Pipeline`]; `Lcms2Compat` runs lcms2's DEFAULT
    /// optimizer chain in source order, first-success-wins, and falls back to
    /// `Pipeline` if every optimizer declines (which itself matches lcms2's
    /// `AnySuccess` fallback once `PreOptimize` has already been baked into the
    /// link).
    ///
    /// The chain order is lcms2's `DefaultOptimization[]` (cmsopt.c:1822-1828):
    /// `OptimizeByJoiningCurves` → `OptimizeMatrixShaper` →
    /// `OptimizeByComputingLinearization` → `OptimizeByResampling`.
    pub fn build(self, lut: &Pipeline, in_fmt: u32, out_fmt: u32, intent: u32) -> OptimizedEval {
        match self {
            OptimizationStrategy::Accurate => OptimizedEval::Pipeline,
            OptimizationStrategy::Lcms2Compat => Self::lcms2_compat(lut, in_fmt, out_fmt, intent),
            OptimizationStrategy::AccurateFast => Self::accurate_fast(lut, in_fmt, out_fmt),
        }
    }

    /// The LOSSLESS [`AccurateFast`](Self::AccurateFast) build: detect the RGB
    /// 8-bit-input matrix-shaper shape and install the byte-identical
    /// [`LosslessMatShaper`](lossless_matshaper::LosslessMatShaper); otherwise
    /// fall back to the in-place pipeline eval (the accurate path). Never changes
    /// an output bit versus [`Accurate`](Self::Accurate).
    fn accurate_fast(lut: &Pipeline, in_fmt: u32, out_fmt: u32) -> OptimizedEval {
        // 1. The matrix-shaper shape (Task 1) — RGB 8-bit-input C-M-C.
        if let Some(eval) = lossless_matshaper::try_optimize(lut, in_fmt, out_fmt) {
            return OptimizedEval::LosslessMatShaper(Box::new(eval));
        }
        // 2. The general/CLUT path: batched stage-by-stage eval with the CLUT
        //    interpolator resolved once + (for 8-bit input) memoized input curves.
        //    Byte-for-byte identical to the in-place Pipeline eval.
        if let Some(eval) = batched::try_optimize(lut, in_fmt, out_fmt) {
            return OptimizedEval::Batched(Box::new(eval));
        }
        OptimizedEval::Pipeline
    }

    /// Build the [`OptimizedEval`], consulting an optional custom [`Optimizer`]
    /// (lcms2 `cmsPluginOptimization`) FIRST. This mirrors lcms2's
    /// `_cmsOptimizePipeline`, which walks the registered optimizer list before
    /// the builtin `DefaultOptimization[]` chain and takes the first that returns
    /// `TRUE`. If `optimizer` is `Some` and its
    /// [`optimize`](Optimizer::optimize) returns `Some(eval)`, that eval is used
    /// (lcms2 `return TRUE`); otherwise the optimizer declined (`None`) and we
    /// fall through to the chosen builtin posture via [`build`](Self::build),
    /// preserving the builtin-wins invariant. With no optimizer this is exactly
    /// [`build`](Self::build).
    pub fn build_with_optimizer(
        self,
        optimizer: Option<&std::sync::Arc<dyn Optimizer>>,
        lut: &Pipeline,
        in_fmt: u32,
        out_fmt: u32,
        intent: u32,
    ) -> OptimizedEval {
        if let Some(opt) = optimizer {
            if let Some(eval) = opt.optimize(lut, in_fmt, out_fmt, intent) {
                return eval;
            }
        }
        self.build(lut, in_fmt, out_fmt, intent)
    }

    /// lcms2's DEFAULT optimizer chain, first-success-wins (cmsopt.c:1977-1985).
    fn lcms2_compat(lut: &Pipeline, in_fmt: u32, out_fmt: u32, intent: u32) -> OptimizedEval {
        // 1. OptimizeByJoiningCurves.
        if let Some(e) = joincurves::optimize_by_joining_curves(lut, in_fmt, out_fmt) {
            return e;
        }
        // 2. OptimizeMatrixShaper.
        if let Some(data) = matshaper::try_optimize(lut, in_fmt, out_fmt) {
            return OptimizedEval::MatShaper(Box::new(data));
        }
        // 3. OptimizeByComputingLinearization.
        if let Some(e) =
            resampling::optimize_by_computing_linearization(lut, in_fmt, out_fmt, intent)
        {
            return e;
        }
        // 4. OptimizeByResampling.
        if let Some(e) = resampling::optimize_by_resampling(lut, in_fmt, out_fmt, intent) {
            return e;
        }
        // Every optimizer declined: evaluate in place (lcms2 returns AnySuccess,
        // i.e. only PreOptimize's structural merge — already baked into `lut`).
        OptimizedEval::Pipeline
    }
}
