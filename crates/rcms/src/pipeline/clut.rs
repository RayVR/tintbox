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
use crate::interp::{interp_factory, InterpFn, InterpParams};

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

    /// Evaluate the CLUT in the float domain, writing `output_channels()` floats
    /// into `output`.
    ///
    /// The 16-bit table takes lcms2's `EvaluateCLUTfloatIn16` path; the float
    /// table takes the `EvaluateCLUTfloat` path. The interpolation routine is
    /// resolved by [`interp_factory`] with the matching `is_float` flag and the
    /// `is_trilinear` hint. `cmsStageAllocCLut*` never sets the trilinear flag (so
    /// a freshly-read CLUT is tetrahedral for 3D), but
    /// `ChangeInterpolationToTrilinear` can flip it (see [`Clut::is_trilinear`]).
    pub fn eval(&self, input: &[f32], output: &mut [f32]) {
        let n_in = self.params.n_inputs;
        let n_out = self.params.n_outputs;

        match &self.table {
            ClutTable::U16(table) => {
                // EvaluateCLUTfloatIn16: FromFloatTo16 -> Lerp16 -> From16ToFloat.
                let fnsel = interp_factory(n_in, n_out, false, self.is_trilinear);
                let lerp16 = match fnsel {
                    InterpFn::Lerp16(l) => l,
                    InterpFn::LerpFloat(_) => unreachable!("16-bit CLUT selects a Lerp16 routine"),
                };

                // FromFloatTo16 (cmslut.c:83-90): In[i] (f32) * 65535.0 (f64),
                // widened to f64 before saturation.
                let mut in16 = [0u16; crate::interp::MAX_STAGE_CHANNELS];
                for i in 0..n_in {
                    in16[i] = Lcms2Floor::quick_saturate_word(input[i] as f64 * 65535.0);
                }

                let mut out16 = [0u16; crate::interp::MAX_STAGE_CHANNELS];
                lerp16.eval(&in16[..n_in], &mut out16[..n_out], table, &self.params);

                // From16ToFloat (cmslut.c:93-100): In[i] (u16) as f32 / 65535.0F.
                for i in 0..n_out {
                    output[i] = out16[i] as f32 / 65535.0_f32;
                }
            }
            ClutTable::F32(table) => {
                // EvaluateCLUTfloat: the float interpolator runs directly.
                let fnsel = interp_factory(n_in, n_out, true, self.is_trilinear);
                let lerpf = match fnsel {
                    InterpFn::LerpFloat(l) => l,
                    InterpFn::Lerp16(_) => unreachable!("float CLUT selects a LerpFloat routine"),
                };
                lerpf.eval(&input[..n_in], &mut output[..n_out], table, &self.params);
            }
        }
    }
}
