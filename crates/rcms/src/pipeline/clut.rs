//! CLUT (color lookup table) pipeline stage, transcribed from lcms2's
//! `cmsSigCLutElemType` element (cmslut.c:428-697).
//!
//! lcms2 stores a CLUT either as 16-bit integers (`Tab.T`, allocated by
//! `cmsStageAllocCLut16bitGranular`) or as floats (`Tab.TFloat`,
//! `cmsStageAllocCLutFloatGranular`). The two allocators install *different*
//! float-domain evaluators:
//!
//! - A 16-bit CLUT uses `EvaluateCLUTfloatIn16` (cmslut.c:444-456): the float
//!   inputs are quantized to 16 bits (`FromFloatTo16`), the 16-bit interpolator
//!   (`Lerp16`, selected with `CMS_LERP_FLAGS_16BITS`) runs, and the 16-bit
//!   outputs are widened back to float (`From16ToFloat`).
//! - A float CLUT uses `EvaluateCLUTfloat` (cmslut.c:434-440): the float
//!   interpolator (`LerpFloat`, selected with `CMS_LERP_FLAGS_FLOAT`) runs
//!   directly on the float inputs/outputs.
//!
//! This mirrors that split exactly: [`ClutTable::U16`] takes the float-in-16
//! path and [`ClutTable::F32`] the direct path.

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::context::Context;
use crate::interp::{interp_factory, interp_factory_in, InterpFn, InterpParams};

/// The interpolator a [`Clut`] resolved at BUILD time, if a custom plugin factory
/// claimed it. `None` is the common builtin case: the per-pixel [`Clut::eval`]
/// hot path then re-derives the builtin routine from the grid geometry exactly as
/// before, so a plugin-free build is byte-for-byte unchanged.
///
/// Wrapped in a newtype so [`Clut`] can keep deriving `Clone`/`Debug`/`PartialEq`:
/// the resolved interpolator is a *cache* of the registry lookup, not part of a
/// CLUT's identity, so it is opaque to `Debug` and ignored by `PartialEq`. (It
/// also can't derive them — [`InterpFn::Custom`] carries an `Arc<dyn Fn>`.)
#[derive(Clone, Default)]
pub struct ResolvedInterp(pub Option<InterpFn>);

impl core::fmt::Debug for ResolvedInterp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.0 {
            None => f.write_str("ResolvedInterp(builtin)"),
            Some(InterpFn::Custom(_)) => f.write_str("ResolvedInterp(custom)"),
            Some(other) => write!(f, "ResolvedInterp({other:?})"),
        }
    }
}

impl PartialEq for ResolvedInterp {
    /// A resolved interpolator is a build-time cache, not identity: two CLUTs with
    /// the same grid compare equal regardless of which interpolator was resolved.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// The CLUT grid storage. `U16` is the 16-bit integer table installed by
/// `cmsStageAllocCLut16bitGranular`; `F32` is the float table installed by
/// `cmsStageAllocCLutFloatGranular`. The layout in both cases is row-major with
/// `params.n_outputs` values per grid node (lcms2's flattened CLUT layout).
#[derive(Clone, Debug, PartialEq)]
pub enum ClutTable {
    /// 16-bit grid (`Tab.T`): evaluated via the float-in-16 path.
    U16(Vec<u16>),
    /// Float grid (`Tab.TFloat`): evaluated directly in the float domain.
    F32(Vec<f32>),
}

/// A CLUT element: the grid table plus the precomputed [`InterpParams`] that
/// describe its sample geometry. Mirrors lcms2's `_cmsStageCLutData`
/// (the `Tab`/`Params` pair).
#[derive(Clone, Debug, PartialEq)]
pub struct Clut {
    /// The grid storage (16-bit or float).
    pub table: ClutTable,
    /// Interpolation parameters (sample counts, domain, opta strides).
    pub params: InterpParams,
    /// Whether the `CMS_LERP_FLAGS_TRILINEAR` hint is set (lcms2
    /// `_cmsStageCLutData::Params->dwFlags`). The `cmsStageAllocCLut*` allocators
    /// never set it (so a freshly-read CLUT is `false` = tetrahedral for 3D), but
    /// `ChangeInterpolationToTrilinear` (cmsio1.c:516-534) ORs it in on every CLUT
    /// stage of an output/devicelink LUT whose PCS is Lab — flipping a 3-input
    /// CLUT from tetrahedral to trilinear, a *different* numeric result.
    pub is_trilinear: bool,
    /// `cmsStage::Implements == cmsSigIdentityElemType` (`_cmsStageAllocIdentityCLut`,
    /// cmslut.c:730 sets it). lcms2's `PreOptimize` runs `_Remove1Op(IdentityElem)`
    /// BEFORE the `cmsFLAGS_NOOPTIMIZE` gate (cmsopt.c:261 vs :1961), so an
    /// identity-marked CLUT is dropped even under NOOPTIMIZE. Only the in-memory
    /// virtual (built via `_cmsStageAllocIdentityCLut`) carries this marker; a CLUT
    /// READ from disk (`mft2`/`mAB`) never does, so a serialize→reparse round-trip
    /// loses it — exactly the divergence the black-point Lab2/Lab4 virtuals exploit.
    pub implements_identity: bool,
    /// The interpolator resolved from a [`Context`]'s registered
    /// [`InterpolatorFactory`](crate::plugin::InterpolatorFactory) plugins at
    /// BUILD time (slice-8 T5). [`ResolvedInterp::default()`] (`None`) means "use
    /// the builtin factory" — the per-pixel hot path then selects the builtin
    /// routine inline, byte-identically to before. A custom interpolator is only
    /// stored when a CLUT is built through [`Clut::resolve_interp_in`] with a
    /// matching factory registered.
    pub resolved: ResolvedInterp,
}

impl Clut {
    /// Number of input channels (grid dimensions).
    #[must_use]
    pub fn input_channels(&self) -> usize {
        self.params.n_inputs
    }

    /// Number of output channels per grid node.
    #[must_use]
    pub fn output_channels(&self) -> usize {
        self.params.n_outputs
    }

    /// Resolve and store the interpolator from `ctx`'s registered
    /// [`InterpolatorFactory`](crate::plugin::InterpolatorFactory) plugins at BUILD
    /// time (slice-8 T5). Consults the registry once via [`interp_factory_in`]; if
    /// no factory claims the CLUT's geometry the stored slot stays `None` and
    /// [`Clut::eval`] keeps the builtin hot path. The 16-bit and float tables
    /// resolve their respective routines so a stored `Custom` interpolator matches
    /// the table's domain.
    ///
    /// Returns `self` for chaining at a construction site
    /// (`Stage::Clut(Clut { … }.resolve_interp_in(ctx))`).
    #[must_use]
    pub fn resolve_interp_in(mut self, ctx: &Context) -> Self {
        // Nothing to do if no factory is registered: keep the builtin path.
        if ctx.plugins().interpolators.is_empty() {
            return self;
        }
        let n_in = self.params.n_inputs;
        let n_out = self.params.n_outputs;
        let is_float = matches!(self.table, ClutTable::F32(_));
        let fnsel = interp_factory_in(ctx, n_in, n_out, is_float, self.is_trilinear);
        // Only store a genuinely custom interpolator; a builtin selection leaves
        // the slot `None` so the hot path is byte-for-byte unchanged.
        self.resolved = match fnsel {
            InterpFn::Custom(_) => ResolvedInterp(Some(fnsel)),
            InterpFn::Lerp16(_) | InterpFn::LerpFloat(_) => ResolvedInterp(None),
        };
        self
    }

    /// The build-time custom interpolator, if one was resolved
    /// ([`Clut::resolve_interp_in`]). `None` in the common builtin case, so
    /// [`Clut::eval`] takes the unchanged builtin hot path.
    #[inline]
    fn custom_interp(&self) -> Option<&crate::plugin::CustomInterp> {
        match &self.resolved.0 {
            Some(InterpFn::Custom(boxed)) => Some(boxed.as_ref()),
            _ => None,
        }
    }

    /// Evaluate the CLUT in the float domain, writing `output_channels()` floats
    /// into `output`.
    ///
    /// The 16-bit table takes lcms2's `EvaluateCLUTfloatIn16` path; the float
    /// table takes the `EvaluateCLUTfloat` path. The interpolation routine is
    /// resolved by [`interp_factory`] with the matching `is_float` flag and the
    /// `is_trilinear` hint. `cmsStageAllocCLut*` never sets the trilinear flag (so
    /// a freshly-read CLUT is tetrahedral for 3D), but
    /// `ChangeInterpolationToTrilinear` can flip it (see [`Clut::is_trilinear`]).
    ///
    /// When a custom interpolator was resolved at build time
    /// ([`Clut::resolve_interp_in`]), it is invoked instead of the builtin
    /// routine; otherwise the builtin hot path below runs unchanged.
    pub fn eval(&self, input: &[f32], output: &mut [f32]) {
        let n_in = self.params.n_inputs;
        let n_out = self.params.n_outputs;

        match &self.table {
            ClutTable::U16(table) => {
                // FromFloatTo16 (cmslut.c:83-90): In[i] (f32) * 65535.0 (f64),
                // widened to f64 before saturation.
                let mut in16 = [0u16; crate::interp::MAX_STAGE_CHANNELS];
                for i in 0..n_in {
                    in16[i] = Lcms2Floor::quick_saturate_word(input[i] as f64 * 65535.0);
                }

                let mut out16 = [0u16; crate::interp::MAX_STAGE_CHANNELS];
                match self.custom_interp() {
                    // A build-time custom 16-bit interpolator: call its closure.
                    Some(crate::plugin::CustomInterp::Lerp16(f)) => {
                        f(&in16[..n_in], table, &self.params, &mut out16[..n_out]);
                    }
                    // EvaluateCLUTfloatIn16: FromFloatTo16 -> Lerp16 -> From16ToFloat.
                    _ => {
                        let fnsel = interp_factory(n_in, n_out, false, self.is_trilinear);
                        let lerp16 = match fnsel {
                            InterpFn::Lerp16(l) => l,
                            InterpFn::LerpFloat(_) => {
                                unreachable!("16-bit CLUT selects a Lerp16 routine")
                            }
                            InterpFn::Custom(_) => {
                                unreachable!("builtin interp_factory never returns Custom")
                            }
                        };
                        lerp16.eval(&in16[..n_in], &mut out16[..n_out], table, &self.params);
                    }
                }

                // From16ToFloat (cmslut.c:93-100): In[i] (u16) as f32 / 65535.0F.
                for i in 0..n_out {
                    output[i] = out16[i] as f32 / 65535.0_f32;
                }
            }
            ClutTable::F32(table) => {
                match self.custom_interp() {
                    // A build-time custom float interpolator: call its closure.
                    Some(crate::plugin::CustomInterp::LerpFloat(f)) => {
                        f(&input[..n_in], table, &self.params, &mut output[..n_out]);
                    }
                    // EvaluateCLUTfloat: the float interpolator runs directly.
                    _ => {
                        let fnsel = interp_factory(n_in, n_out, true, self.is_trilinear);
                        let lerpf = match fnsel {
                            InterpFn::LerpFloat(l) => l,
                            InterpFn::Lerp16(_) => {
                                unreachable!("float CLUT selects a LerpFloat routine")
                            }
                            InterpFn::Custom(_) => {
                                unreachable!("builtin interp_factory never returns Custom")
                            }
                        };
                        lerpf.eval(&input[..n_in], &mut output[..n_out], table, &self.params);
                    }
                }
            }
        }
    }
}
