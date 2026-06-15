//! n-D interpolation parameters and 3D tetrahedral interpolators, transcribed
//! bit-for-bit from lcms2's `cmsintrp.c`.
//!
//! `InterpParams` mirrors `_cmsComputeInterpParamsEx` (the opta/domain setup);
//! [`tetrahedral_16`] mirrors `TetrahedralInterp16` (the 2-level nested-branch
//! 6-leaf Sakamoto routine) and [`tetrahedral_float`] mirrors the separate flat
//! 6-case `TetrahedralInterpFloat`.

use crate::fixed::to_fixed_domain;

/// `_cmsToFixedDomain`'s `FIXED_TO_INT(x)` macro (`x >> 16`). Rust `i32 >> 16`
/// is an arithmetic shift, matching C's signed `cmsS15Fixed16Number` shift.
#[inline]
const fn fixed_to_int(x: i32) -> i32 {
    x >> 16
}

/// `FIXED_REST_TO_INT(x)` macro (`x & 0xFFFF`).
#[inline]
const fn fixed_rest_to_int(x: i32) -> i32 {
    x & 0xFFFF
}

/// `fclamp` (cmsintrp.c:224): clamp to `[0, 1]`, mapping NaN and sub-1e-9 to 0.
#[inline]
fn fclamp(v: f32) -> f32 {
    if v < 1.0e-9f32 || v.is_nan() {
        0.0f32
    } else if v > 1.0f32 {
        1.0f32
    } else {
        v
    }
}

/// Precomputed parameters for n-D grid interpolation, mirroring lcms2's
/// `cmsInterpParams` fields populated by `_cmsComputeInterpParamsEx`.
///
/// The CLUT table itself is passed to the interpolation functions separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpParams {
    /// Number of input channels (grid dimensions).
    pub n_inputs: usize,
    /// Number of output channels per grid node.
    pub n_outputs: usize,
    /// Number of samples (nodes) along each input dimension.
    pub n_samples: Vec<u32>,
    /// `n_samples[i] - 1` per input dimension.
    pub domain: Vec<u32>,
    /// Strides used to index the flattened grid array (`opta`).
    pub opta: Vec<u32>,
}

impl InterpParams {
    /// Mirror of `_cmsComputeInterpParamsEx`'s domain/opta setup
    /// (cmsintrp.c:136-146).
    ///
    /// `domain[i] = n_samples[i] - 1`; `opta[0] = n_outputs`; and for
    /// `i in 1..n_inputs`, `opta[i] = opta[i-1] * n_samples[n_inputs - i]`
    /// (the reversed `n_samples` index is transcribed exactly from the C).
    #[must_use]
    pub fn new(n_samples: &[u32], n_inputs: usize, n_outputs: usize) -> Self {
        let mut domain = vec![0u32; n_inputs];
        for i in 0..n_inputs {
            domain[i] = n_samples[i] - 1;
        }

        let mut opta = vec![0u32; n_inputs];
        opta[0] = n_outputs as u32;
        for i in 1..n_inputs {
            opta[i] = opta[i - 1] * n_samples[n_inputs - i];
        }

        Self {
            n_inputs,
            n_outputs,
            n_samples: n_samples.to_vec(),
            domain,
            opta,
        }
    }
}

/// The 16-bit interpolation routine selected by [`interp_factory`], mirroring the
/// `Lerp16` member of lcms2's `cmsInterpFunction` union. Each variant carries the
/// same `(input, output, table, params)` calling convention as the standalone
/// functions; [`Interp16::eval`] dispatches with a zero-cost `match`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp16 {
    /// 1D linear, single output (`LinLerp1D`).
    Linear,
    /// 1D linear, multi output (`Eval1Input`).
    Eval1,
    /// 2D bilinear (`BilinearInterp16`).
    Bilinear,
    /// 3D trilinear (`TrilinearInterp16`).
    Trilinear,
    /// 3D tetrahedral (`TetrahedralInterp16`).
    Tetrahedral,
    /// n-D (4..=15 inputs) (`Eval4Inputs`..`Eval15Inputs`).
    EvalN,
}

impl Interp16 {
    /// Evaluate the selected 16-bit interpolator.
    #[inline]
    pub fn eval(self, input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
        match self {
            Interp16::Linear => lin_lerp_1d(input, output, table, p),
            Interp16::Eval1 => eval_1_input(input, output, table, p),
            Interp16::Bilinear => bilinear_16(input, output, table, p),
            Interp16::Trilinear => trilinear_16(input, output, table, p),
            Interp16::Tetrahedral => tetrahedral_16(input, output, table, p),
            Interp16::EvalN => eval_n_inputs(input, output, table, p),
        }
    }
}

/// The float interpolation routine selected by [`interp_factory`], mirroring the
/// `LerpFloat` member of lcms2's `cmsInterpFunction` union.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpFloat {
    /// 1D linear, single output (`LinLerp1Dfloat`).
    Linear,
    /// 1D linear, multi output (`Eval1InputFloat`).
    Eval1,
    /// 2D bilinear (`BilinearInterpFloat`).
    Bilinear,
    /// 3D trilinear (`TrilinearInterpFloat`).
    Trilinear,
    /// 3D tetrahedral (`TetrahedralInterpFloat`).
    Tetrahedral,
    /// n-D (4..=15 inputs) (`Eval4InputsFloat`..`Eval15InputsFloat`).
    EvalN,
}

impl InterpFloat {
    /// Evaluate the selected float interpolator.
    #[inline]
    pub fn eval(self, input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
        match self {
            InterpFloat::Linear => lin_lerp_1d_float(input, output, table, p),
            InterpFloat::Eval1 => eval_1_input_float(input, output, table, p),
            InterpFloat::Bilinear => bilinear_float(input, output, table, p),
            InterpFloat::Trilinear => trilinear_float(input, output, table, p),
            InterpFloat::Tetrahedral => tetrahedral_float(input, output, table, p),
            InterpFloat::EvalN => eval_n_inputs_float(input, output, table, p),
        }
    }
}

/// The interpolator [`interp_factory`] resolves for a given channel count and
/// flags — either the 16-bit or the float routine, per `is_float`. Mirrors the
/// `cmsInterpFunction` union that `DefaultInterpolatorsFactory` fills.
///
/// The builtin factory only ever produces [`InterpFn::Lerp16`] /
/// [`InterpFn::LerpFloat`]. The [`InterpFn::Custom`] arm is produced exclusively
/// by [`interp_factory_in`] when a registered [`InterpolatorFactory`](crate::plugin::InterpolatorFactory)
/// claims the combination; it carries the plugin's resolved
/// [`CustomInterp`](crate::plugin::CustomInterp) so the per-pixel loop can call it
/// without revisiting the registry.
///
/// Holding a [`CustomInterp`](crate::plugin::CustomInterp) (an `Arc<dyn Fn>`)
/// means `InterpFn` is neither `Copy` nor `Eq`; the two builtin arms are still
/// cheap value types.
#[derive(Clone)]
pub enum InterpFn {
    /// 16-bit interpolation routine.
    Lerp16(Interp16),
    /// Float interpolation routine.
    LerpFloat(InterpFloat),
    /// A custom interpolator resolved by a registered plugin factory at
    /// CLUT-build time. Never produced by the builtin [`interp_factory`]. Boxed to
    /// break the [`InterpFn`] ↔ [`CustomInterp`](crate::plugin::CustomInterp)
    /// recursive-type cycle (`CustomInterp::Builtin` holds an `InterpFn`).
    Custom(Box<crate::plugin::CustomInterp>),
}

impl core::fmt::Debug for InterpFn {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `CustomInterp` wraps an `Arc<dyn Fn>` with no `Debug`, so render the
        // custom arm opaquely; the builtin arms forward to their derived `Debug`.
        match self {
            InterpFn::Lerp16(l) => f.debug_tuple("Lerp16").field(l).finish(),
            InterpFn::LerpFloat(l) => f.debug_tuple("LerpFloat").field(l).finish(),
            InterpFn::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

impl PartialEq for InterpFn {
    /// The two builtin arms compare by value; a `Custom` arm compares by `Arc`
    /// pointer identity of the underlying closure (two clones of the same
    /// resolved interpolator are equal, distinct closures are not). `CustomInterp`
    /// itself carries an `Arc<dyn Fn>` and so cannot be value-compared; this is the
    /// only meaningful equality, and it keeps the builtin-only `assert_eq!`s in the
    /// existing interp tests working unchanged.
    fn eq(&self, other: &Self) -> bool {
        use crate::plugin::CustomInterp;
        match (self, other) {
            (InterpFn::Lerp16(a), InterpFn::Lerp16(b)) => a == b,
            (InterpFn::LerpFloat(a), InterpFn::LerpFloat(b)) => a == b,
            (InterpFn::Custom(a), InterpFn::Custom(b)) => match (a.as_ref(), b.as_ref()) {
                (CustomInterp::Builtin(x), CustomInterp::Builtin(y)) => x == y,
                (CustomInterp::Lerp16(x), CustomInterp::Lerp16(y)) => std::sync::Arc::ptr_eq(x, y),
                (CustomInterp::LerpFloat(x), CustomInterp::LerpFloat(y)) => {
                    std::sync::Arc::ptr_eq(x, y)
                }
                _ => false,
            },
            _ => false,
        }
    }
}

/// Select the interpolation routine for `(n_inputs, is_float, is_trilinear)`,
/// bit-for-bit matching lcms2's `DefaultInterpolatorsFactory` (cmsintrp.c:1178).
///
/// - 1 input  -> `LinLerp1D`/`LinLerp1Dfloat` when `n_outputs == 1`, else
///   `Eval1Input`/`Eval1InputFloat` (multi-output 1D LUTs), matching lcms2's
///   `nOutputChannels == 1` branch exactly.
/// - 2 inputs -> bilinear.
/// - 3 inputs -> trilinear if `is_trilinear` (the `CMS_LERP_FLAGS_TRILINEAR`
///   hint), else tetrahedral.
/// - 4..=15   -> the n-D `EvalN` routine.
///
/// `n_outputs` is taken for the lcms2 safety check (`>= 4` inputs with
/// `>= MAX_STAGE_CHANNELS` outputs is rejected) and to mirror the C signature.
///
/// # Panics
/// Panics if `n_inputs` is 0 or `> 15`, or if the lcms2 safety check rejects the
/// combination (`n_inputs >= 4 && n_outputs >= MAX_STAGE_CHANNELS`) — lcms2
/// returns a zeroed (null) interpolator there, which has no valid routine.
#[must_use]
pub fn interp_factory(
    n_inputs: usize,
    n_outputs: usize,
    is_float: bool,
    is_trilinear: bool,
) -> InterpFn {
    assert!(
        !(n_inputs >= 4 && n_outputs >= MAX_STAGE_CHANNELS),
        "lcms2 safety check rejects >= 4 inputs with >= MAX_STAGE_CHANNELS outputs"
    );

    match n_inputs {
        1 => {
            // lcms2 `DefaultInterpolatorsFactory` case 1: single-output 1D LUTs
            // use `LinLerp1D`/`LinLerp1Dfloat`; multi-output 1D LUTs use
            // `Eval1Input`/`Eval1InputFloat` (the same math looped over outputs).
            match (n_outputs == 1, is_float) {
                (true, true) => InterpFn::LerpFloat(InterpFloat::Linear),
                (true, false) => InterpFn::Lerp16(Interp16::Linear),
                (false, true) => InterpFn::LerpFloat(InterpFloat::Eval1),
                (false, false) => InterpFn::Lerp16(Interp16::Eval1),
            }
        }
        2 => {
            if is_float {
                InterpFn::LerpFloat(InterpFloat::Bilinear)
            } else {
                InterpFn::Lerp16(Interp16::Bilinear)
            }
        }
        3 => {
            if is_trilinear {
                if is_float {
                    InterpFn::LerpFloat(InterpFloat::Trilinear)
                } else {
                    InterpFn::Lerp16(Interp16::Trilinear)
                }
            } else if is_float {
                InterpFn::LerpFloat(InterpFloat::Tetrahedral)
            } else {
                InterpFn::Lerp16(Interp16::Tetrahedral)
            }
        }
        4..=15 => {
            if is_float {
                InterpFn::LerpFloat(InterpFloat::EvalN)
            } else {
                InterpFn::Lerp16(Interp16::EvalN)
            }
        }
        _ => panic!("interp_factory: n_inputs must be in 1..=15, got {n_inputs}"),
    }
}

/// Context-aware interpolator selection (slice-8 task T5). Consults the
/// registered [`InterpolatorFactory`](crate::plugin::InterpolatorFactory) plugins
/// FIRST, in register-order (first `Some` wins); if every factory declines (or
/// none is registered), falls through to the builtin [`interp_factory`].
///
/// A factory that returns [`CustomInterp::Builtin`](crate::plugin::CustomInterp::Builtin)
/// resolves to that builtin [`InterpFn`] directly; a custom
/// [`Lerp16`](crate::plugin::CustomInterp::Lerp16) /
/// [`LerpFloat`](crate::plugin::CustomInterp::LerpFloat) resolves to
/// [`InterpFn::Custom`]. The resolution happens at CLUT-build time, so the
/// per-pixel loop never touches the [`Context`](crate::context::Context).
///
/// # Panics
/// Same as [`interp_factory`] when it is reached (no factory claims the combo).
#[must_use]
pub fn interp_factory_in(
    ctx: &crate::context::Context,
    n_inputs: usize,
    n_outputs: usize,
    is_float: bool,
    is_trilinear: bool,
) -> InterpFn {
    for factory in &ctx.plugins().interpolators {
        if let Some(custom) = factory.factory(n_inputs, n_outputs, is_float, is_trilinear) {
            return match custom {
                crate::plugin::CustomInterp::Builtin(f) => f,
                other => InterpFn::Custom(Box::new(other)),
            };
        }
    }
    interp_factory(n_inputs, n_outputs, is_float, is_trilinear)
}

/// 16-bit 3D tetrahedral interpolation, bit-identical to
/// `TetrahedralInterp16` (cmsintrp.c:720-850).
///
/// `input` is 3 u16 channels, `output` receives `p.n_outputs` u16 channels, and
/// `table` is the flattened CLUT grid (`n_outputs` u16 per node, lcms2 layout).
///
/// # Panics
/// Panics (via slice indexing) if `table`, `input`, or `output` are sized
/// inconsistently with `p`.
pub fn tetrahedral_16(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    // const cmsUInt16Number* LutTable = (cmsUInt16Number*) p -> Table;
    // We track a base index `lut` into `table` instead of advancing a pointer.

    let fx: i32 = to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let fy: i32 = to_fixed_domain(input[1] as i32 * p.domain[1] as i32);
    let fz: i32 = to_fixed_domain(input[2] as i32 * p.domain[2] as i32);

    let x0 = fixed_to_int(fx);
    let y0 = fixed_to_int(fy);
    let z0 = fixed_to_int(fz);

    let rx: i32 = fixed_rest_to_int(fx);
    let ry: i32 = fixed_rest_to_int(fy);
    let rz: i32 = fixed_rest_to_int(fz);

    let x0_idx = p.opta[2] * x0 as u32;
    let mut x1: u32 = if input[0] == 0xFFFF { 0 } else { p.opta[2] };

    let y0_idx = p.opta[1] * y0 as u32;
    let mut y1: u32 = if input[1] == 0xFFFF { 0 } else { p.opta[1] };

    let z0_idx = p.opta[0] * z0 as u32;
    let mut z1: u32 = if input[2] == 0xFFFF { 0 } else { p.opta[0] };

    // LutTable += X0+Y0+Z0;
    let mut lut: usize = (x0_idx + y0_idx + z0_idx) as usize;

    let total_out = p.n_outputs;

    // Output should be computed as x = ROUND_FIXED_TO_INT(_cmsToFixedDomain(Rest))
    // ... t = Rest+0x8001, x = (t + (t>>16))>>16.
    if rx >= ry {
        if ry >= rz {
            y1 += x1;
            z1 += y1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c3 = c3 - c2;
                let c2 = c2 - c1;
                let c1 = c1 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        } else if rz >= rx {
            x1 += z1;
            y1 += x1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c2 = c2 - c1;
                let c1 = c1 - c3;
                let c3 = c3 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        } else {
            z1 += x1;
            y1 += z1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c2 = c2 - c3;
                let c3 = c3 - c1;
                let c1 = c1 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        }
    } else if rx >= rz {
        x1 += y1;
        z1 += x1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c3 = c3 - c1;
            let c1 = c1 - c2;
            let c2 = c2 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    } else if ry >= rz {
        z1 += y1;
        x1 += z1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c1 = c1 - c3;
            let c3 = c3 - c2;
            let c2 = c2 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    } else {
        y1 += z1;
        x1 += y1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c1 = c1 - c2;
            let c2 = c2 - c3;
            let c3 = c3 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    }
}

/// `LinearInterp` (cmsintrp.c:184): fixed-point lerp used by the 16-bit n-D
/// `EvalN` routines. `dif = (u32)(h - l) * a + 0x8000; dif = (dif >> 16) + l`.
///
/// `h - l` is computed as `cmsS15Fixed16Number` (i32) and then cast to `u32`,
/// so the subtraction and the multiply both wrap (`CMS_NO_SANITIZE`).
#[inline]
fn linear_interp(a: i32, l: i32, h: i32) -> u16 {
    let dif = (h.wrapping_sub(l) as u32)
        .wrapping_mul(a as u32)
        .wrapping_add(0x8000);
    let dif = (dif >> 16).wrapping_add(l as u32);
    dif as u16
}

/// `LERP(a,l,h)` for the 16-bit bilinear/trilinear routines (cmsintrp.c:417):
/// `(cmsUInt16Number)(l + ROUND_FIXED_TO_INT((h - l) * a))` where
/// `ROUND_FIXED_TO_INT(x) = (x + 0x8000) >> 16`. All arithmetic is `int` (i32)
/// and wraps; the macro casts the result to `cmsUInt16Number`, so each nested
/// LERP's intermediate (`dx00`, `dxy0`, ...) is truncated to 16 bits *before*
/// the next LERP consumes it — the returned `i32` is therefore in `0..=0xFFFF`.
#[inline]
fn lerp16(a: i32, l: i32, h: i32) -> i32 {
    let v = l.wrapping_add((h.wrapping_sub(l)).wrapping_mul(a).wrapping_add(0x8000) >> 16);
    v as u16 as i32
}

/// 16-bit 1D linear interpolation, bit-identical to `LinLerp1D`
/// (cmsintrp.c:189-220).
///
/// `input` is 1 u16 channel; `output` receives 1 u16 channel. Single-output by
/// construction (lcms2 only routes 1-input/1-output LUTs here).
pub fn lin_lerp_1d(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    if input[0] == 0xffff || p.domain[0] == 0 {
        output[0] = table[p.domain[0] as usize];
        return;
    }
    let val3 = p.domain[0] as i32 * input[0] as i32;
    let val3 = to_fixed_domain(val3);
    let cell0 = fixed_to_int(val3);
    let rest = fixed_rest_to_int(val3);
    let y0 = table[cell0 as usize] as i32;
    let y1 = table[cell0 as usize + 1] as i32;
    output[0] = linear_interp(rest, y0, y1);
}

/// Float 1D linear interpolation, bit-identical to `LinLerp1Dfloat`
/// (cmsintrp.c:229-261).
pub fn lin_lerp_1d_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    let val2 = fclamp(input[0]);
    if val2 == 1.0 || p.domain[0] == 0 {
        output[0] = table[p.domain[0] as usize];
        return;
    }
    let val2 = val2 * p.domain[0] as f32;
    let cell0 = val2.floor() as i32;
    let cell1 = val2.ceil() as i32;
    let rest = val2 - cell0 as f32;
    let y0 = table[cell0 as usize];
    let y1 = table[cell1 as usize];
    output[0] = y0 + (y1 - y0) * rest;
}

/// 16-bit 1D linear interpolation for MULTI-output 1D LUTs, bit-identical to
/// `Eval1Input` (cmsintrp.c:191-228). The single-output case uses [`lin_lerp_1d`]
/// (`LinLerp1D`); this is the same math looped over `p.n_outputs`, indexing each
/// node's output channels at `K0 + OutChan` / `K1 + OutChan`.
pub fn eval_1_input(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    let total_out = p.n_outputs;
    if input[0] == 0xffff || p.domain[0] == 0 {
        let y0 = (p.domain[0] * p.opta[0]) as usize;
        for (oc, out) in output.iter_mut().enumerate().take(total_out) {
            *out = table[y0 + oc];
        }
        return;
    }

    let v = input[0] as i32 * p.domain[0] as i32;
    let fk = to_fixed_domain(v);
    let k0 = fixed_to_int(fk);
    let rk = fixed_rest_to_int(fk);
    // k1 = k0 + (Input != 0xFFFF ? 1 : 0); the 0xFFFF branch is handled above.
    let k1 = k0 + 1;
    let kk0 = (p.opta[0] as i32 * k0) as usize;
    let kk1 = (p.opta[0] as i32 * k1) as usize;

    for (oc, out) in output.iter_mut().enumerate().take(total_out) {
        *out = linear_interp(rk, table[kk0 + oc] as i32, table[kk1 + oc] as i32);
    }
}

/// Float 1D linear interpolation for MULTI-output 1D LUTs, bit-identical to
/// `Eval1InputFloat` (cmsintrp.c:264-303). The single-output case uses
/// [`lin_lerp_1d_float`] (`LinLerp1Dfloat`); this loops over `p.n_outputs`.
pub fn eval_1_input_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    let total_out = p.n_outputs;
    let val2 = fclamp(input[0]);
    if val2 == 1.0 || p.domain[0] == 0 {
        let start = (p.domain[0] * p.opta[0]) as usize;
        for (oc, out) in output.iter_mut().enumerate().take(total_out) {
            *out = table[start + oc];
        }
        return;
    }

    let val2 = val2 * p.domain[0] as f32;
    let cell0 = val2.floor() as i32;
    let cell1 = val2.ceil() as i32;
    let rest = val2 - cell0 as f32;
    let cell0 = (cell0 * p.opta[0] as i32) as usize;
    let cell1 = (cell1 * p.opta[0] as i32) as usize;

    for (oc, out) in output.iter_mut().enumerate().take(total_out) {
        let y0 = table[cell0 + oc];
        let y1 = table[cell1 + oc];
        *out = y0 + (y1 - y0) * rest;
    }
}

/// 16-bit 2D bilinear interpolation, bit-identical to `BilinearInterp16`
/// (cmsintrp.c:409-465).
///
/// `input` is 2 u16 channels; `output` receives `p.n_outputs` u16 channels.
pub fn bilinear_16(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    let total_out = p.n_outputs;

    let fx = to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let x0 = fixed_to_int(fx);
    let rx = fixed_rest_to_int(fx);

    let fy = to_fixed_domain(input[1] as i32 * p.domain[1] as i32);
    let y0 = fixed_to_int(fy);
    let ry = fixed_rest_to_int(fy);

    let xx0 = p.opta[1] as i32 * x0;
    let xx1 = xx0
        + if input[0] == 0xFFFF {
            0
        } else {
            p.opta[1] as i32
        };

    let yy0 = p.opta[0] as i32 * y0;
    let yy1 = yy0
        + if input[1] == 0xFFFF {
            0
        } else {
            p.opta[0] as i32
        };

    for (out_chan, out) in output.iter_mut().enumerate().take(total_out) {
        let oc = out_chan as i32;
        let dens = |i: i32, j: i32| -> i32 { table[(i + j + oc) as usize] as i32 };

        let d00 = dens(xx0, yy0);
        let d01 = dens(xx0, yy1);
        let d10 = dens(xx1, yy0);
        let d11 = dens(xx1, yy1);

        let dx0 = lerp16(rx, d00, d10);
        let dx1 = lerp16(rx, d01, d11);

        let dxy = lerp16(ry, dx0, dx1);

        *out = dxy as u16;
    }
}

/// Float 2D bilinear interpolation, bit-identical to `BilinearInterpFloat`
/// (cmsintrp.c:356-406).
pub fn bilinear_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    let total_out = p.n_outputs;

    let px = fclamp(input[0]) * p.domain[0] as f32;
    let py = fclamp(input[1]) * p.domain[1] as f32;

    // lcms2 uses `_cmsQuickFloor` here (NOT libm floor), so match it bit-for-bit
    // in the valid `[0,1)` domain — same helper `eval_n_float` uses. `quick_floor`
    // and `.floor()` agree for in-range `px`; they can disagree only at the
    // near-1.0 OOB band where lcms2 itself reads one node past the grid.
    let x0 = quick_floor(px);
    let fx = px - x0 as f32;
    let y0 = quick_floor(py);
    let fy = py - y0 as f32;

    let xx0 = p.opta[1] as i32 * x0;
    let xx1 = xx0
        + if fclamp(input[0]) >= 1.0 {
            0
        } else {
            p.opta[1] as i32
        };

    let yy0 = p.opta[0] as i32 * y0;
    let yy1 = yy0
        + if fclamp(input[1]) >= 1.0 {
            0
        } else {
            p.opta[0] as i32
        };

    // Memory-safety clamp: at the degenerate near-1.0 boundary `quick_floor(px)`
    // can round up to `domain` while `fclamp(input) < 1.0` leaves the `+ opta`
    // step in, so the highest index `xx1 + yy1 + (n_out-1)` would point one node
    // past `table` — lcms2 reads that OOB (a latent bug; inputs are expected to
    // be pre-clamped). rcms must stay in bounds, so cap the per-axis base indices
    // at the last node. In the valid domain this clamp never fires, so it does
    // not perturb the bit-exact result there.
    let max_x = (p.n_samples[0].saturating_sub(1)) as i32 * p.opta[1] as i32;
    let max_y = (p.n_samples[1].saturating_sub(1)) as i32 * p.opta[0] as i32;
    let xx0 = xx0.min(max_x);
    let xx1 = xx1.min(max_x);
    let yy0 = yy0.min(max_y);
    let yy1 = yy1.min(max_y);

    for (out_chan, out) in output.iter_mut().enumerate().take(total_out) {
        let oc = out_chan as i32;
        let dens = |i: i32, j: i32| -> f32 { table[(i + j + oc) as usize] };
        let lerp = |a: f32, l: f32, h: f32| -> f32 { l + (h - l) * a };

        let d00 = dens(xx0, yy0);
        let d01 = dens(xx0, yy1);
        let d10 = dens(xx1, yy0);
        let d11 = dens(xx1, yy1);

        let dx0 = lerp(fx, d00, d10);
        let dx1 = lerp(fx, d01, d11);

        *out = lerp(fy, dx0, dx1);
    }
}

/// 16-bit 3D trilinear interpolation, bit-identical to `TrilinearInterp16`
/// (cmsintrp.c:540-606).
///
/// `input` is 3 u16 channels; `output` receives `p.n_outputs` u16 channels.
pub fn trilinear_16(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    let total_out = p.n_outputs;

    let fx = to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let x0 = fixed_to_int(fx);
    let rx = fixed_rest_to_int(fx);

    let fy = to_fixed_domain(input[1] as i32 * p.domain[1] as i32);
    let y0 = fixed_to_int(fy);
    let ry = fixed_rest_to_int(fy);

    let fz = to_fixed_domain(input[2] as i32 * p.domain[2] as i32);
    let z0 = fixed_to_int(fz);
    let rz = fixed_rest_to_int(fz);

    let xx0 = p.opta[2] as i32 * x0;
    let xx1 = xx0
        + if input[0] == 0xFFFF {
            0
        } else {
            p.opta[2] as i32
        };

    let yy0 = p.opta[1] as i32 * y0;
    let yy1 = yy0
        + if input[1] == 0xFFFF {
            0
        } else {
            p.opta[1] as i32
        };

    let zz0 = p.opta[0] as i32 * z0;
    let zz1 = zz0
        + if input[2] == 0xFFFF {
            0
        } else {
            p.opta[0] as i32
        };

    for (out_chan, out) in output.iter_mut().enumerate().take(total_out) {
        let oc = out_chan as i32;
        let dens = |i: i32, j: i32, k: i32| -> i32 { table[(i + j + k + oc) as usize] as i32 };

        let d000 = dens(xx0, yy0, zz0);
        let d001 = dens(xx0, yy0, zz1);
        let d010 = dens(xx0, yy1, zz0);
        let d011 = dens(xx0, yy1, zz1);

        let d100 = dens(xx1, yy0, zz0);
        let d101 = dens(xx1, yy0, zz1);
        let d110 = dens(xx1, yy1, zz0);
        let d111 = dens(xx1, yy1, zz1);

        let dx00 = lerp16(rx, d000, d100);
        let dx01 = lerp16(rx, d001, d101);
        let dx10 = lerp16(rx, d010, d110);
        let dx11 = lerp16(rx, d011, d111);

        let dxy0 = lerp16(ry, dx00, dx10);
        let dxy1 = lerp16(ry, dx01, dx11);

        let dxyz = lerp16(rz, dxy0, dxy1);

        *out = dxyz as u16;
    }
}

/// Float 3D trilinear interpolation, bit-identical to `TrilinearInterpFloat`
/// (cmsintrp.c:467-535).
pub fn trilinear_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    let total_out = p.n_outputs;

    let px = fclamp(input[0]) * p.domain[0] as f32;
    let py = fclamp(input[1]) * p.domain[1] as f32;
    let pz = fclamp(input[2]) * p.domain[2] as f32;

    let x0 = px.floor() as i32;
    let fx = px - x0 as f32;
    let y0 = py.floor() as i32;
    let fy = py - y0 as f32;
    let z0 = pz.floor() as i32;
    let fz = pz - z0 as f32;

    let xx0 = p.opta[2] as i32 * x0;
    let xx1 = xx0
        + if fclamp(input[0]) >= 1.0 {
            0
        } else {
            p.opta[2] as i32
        };

    let yy0 = p.opta[1] as i32 * y0;
    let yy1 = yy0
        + if fclamp(input[1]) >= 1.0 {
            0
        } else {
            p.opta[1] as i32
        };

    let zz0 = p.opta[0] as i32 * z0;
    let zz1 = zz0
        + if fclamp(input[2]) >= 1.0 {
            0
        } else {
            p.opta[0] as i32
        };

    for (out_chan, out) in output.iter_mut().enumerate().take(total_out) {
        let oc = out_chan as i32;
        let dens = |i: i32, j: i32, k: i32| -> f32 { table[(i + j + k + oc) as usize] };
        let lerp = |a: f32, l: f32, h: f32| -> f32 { l + (h - l) * a };

        let d000 = dens(xx0, yy0, zz0);
        let d001 = dens(xx0, yy0, zz1);
        let d010 = dens(xx0, yy1, zz0);
        let d011 = dens(xx0, yy1, zz1);

        let d100 = dens(xx1, yy0, zz0);
        let d101 = dens(xx1, yy0, zz1);
        let d110 = dens(xx1, yy1, zz0);
        let d111 = dens(xx1, yy1, zz1);

        let dx00 = lerp(fx, d000, d100);
        let dx01 = lerp(fx, d001, d101);
        let dx10 = lerp(fx, d010, d110);
        let dx11 = lerp(fx, d011, d111);

        let dxy0 = lerp(fy, dx00, dx10);
        let dxy1 = lerp(fy, dx01, dx11);

        *out = lerp(fz, dxy0, dxy1);
    }
}

/// Float 3D tetrahedral interpolation, bit-identical to
/// `TetrahedralInterpFloat` (cmsintrp.c:622-716).
///
/// `input` is 3 f32 channels, `output` receives `p.n_outputs` f32 channels, and
/// `table` is the flattened CLUT grid (`n_outputs` f32 per node).
pub fn tetrahedral_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    // #define DENS(i,j,k) (LutTable[(i)+(j)+(k)+OutChan])
    let dens =
        |x: i32, y: i32, z: i32, out_chan: i32| -> f32 { table[(x + y + z + out_chan) as usize] };

    let total_out = p.n_outputs as i32;

    // We need some clipping here
    let px = fclamp(input[0]) * p.domain[0] as f32;
    let py = fclamp(input[1]) * p.domain[1] as f32;
    let pz = fclamp(input[2]) * p.domain[2] as f32;

    let x0 = px.floor() as i32;
    let rx = px - x0 as f32;
    let y0 = py.floor() as i32;
    let ry = py - y0 as f32;
    let z0 = pz.floor() as i32;
    let rz = pz - z0 as f32;

    let x0i = p.opta[2] as i32 * x0;
    let x1 = x0i
        + if fclamp(input[0]) >= 1.0 {
            0
        } else {
            p.opta[2] as i32
        };

    let y0i = p.opta[1] as i32 * y0;
    let y1 = y0i
        + if fclamp(input[1]) >= 1.0 {
            0
        } else {
            p.opta[1] as i32
        };

    let z0i = p.opta[0] as i32 * z0;
    let z1 = z0i
        + if fclamp(input[2]) >= 1.0 {
            0
        } else {
            p.opta[0] as i32
        };

    let mut out_chan = 0i32;
    while out_chan < total_out {
        // These are the 6 Tetrahedral
        let c0 = dens(x0i, y0i, z0i, out_chan);
        let c1;
        let c2;
        let c3;

        if rx >= ry && ry >= rz {
            c1 = dens(x1, y0i, z0i, out_chan) - c0;
            c2 = dens(x1, y1, z0i, out_chan) - dens(x1, y0i, z0i, out_chan);
            c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0i, out_chan);
        } else if rx >= rz && rz >= ry {
            c1 = dens(x1, y0i, z0i, out_chan) - c0;
            c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0i, z1, out_chan);
            c3 = dens(x1, y0i, z1, out_chan) - dens(x1, y0i, z0i, out_chan);
        } else if rz >= rx && rx >= ry {
            c1 = dens(x1, y0i, z1, out_chan) - dens(x0i, y0i, z1, out_chan);
            c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0i, z1, out_chan);
            c3 = dens(x0i, y0i, z1, out_chan) - c0;
        } else if ry >= rx && rx >= rz {
            c1 = dens(x1, y1, z0i, out_chan) - dens(x0i, y1, z0i, out_chan);
            c2 = dens(x0i, y1, z0i, out_chan) - c0;
            c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0i, out_chan);
        } else if ry >= rz && rz >= rx {
            c1 = dens(x1, y1, z1, out_chan) - dens(x0i, y1, z1, out_chan);
            c2 = dens(x0i, y1, z0i, out_chan) - c0;
            c3 = dens(x0i, y1, z1, out_chan) - dens(x0i, y1, z0i, out_chan);
        } else if rz >= ry && ry >= rx {
            c1 = dens(x1, y1, z1, out_chan) - dens(x0i, y1, z1, out_chan);
            c2 = dens(x0i, y1, z1, out_chan) - dens(x0i, y0i, z1, out_chan);
            c3 = dens(x0i, y0i, z1, out_chan) - c0;
        } else {
            c1 = 0.0;
            c2 = 0.0;
            c3 = 0.0;
        }

        output[out_chan as usize] = c0 + c1 * rx + c2 * ry + c3 * rz;
        out_chan += 1;
    }
}

/// Maximum output channels lcms2 stacks on the C call stack in the n-D `EvalN`
/// scratch buffers (`MAX_STAGE_CHANNELS`, lcms2_internal.h). The `EvalN` factory
/// rejects `>= 4` inputs with `>= MAX_STAGE_CHANNELS` outputs, so this bounds the
/// temporaries used by [`eval_n_inputs`].
pub const MAX_STAGE_CHANNELS: usize = 128;

/// 16-bit n-D interpolation for 4..=15 inputs, bit-identical to lcms2's
/// `Eval4Inputs`..`Eval15Inputs` (cmsintrp.c, the `EVAL_FNS` macro chain).
///
/// lcms2 generates `EvalN` from a template that fixes the *first* input on
/// `opta[N-1]` / `Domain[0]`, evaluates the `(N-1)`-D interpolation on the two
/// sub-cubes flanking it (table offsets `K0` and `K1`), and linearly
/// interpolates between them along the fixed dimension with [`linear_interp`].
/// The recursion bottoms out at 3 inputs, which lcms2's `Eval4Inputs` evaluates
/// with the 3D tetrahedral kernel — so this routine recurses down to
/// [`tetrahedral_16`].
///
/// `p` describes the full n-D grid. The recursion never rebuilds `opta`/`n_samples`
/// (matching the C, which copies `*p16` and only rewrites `Domain` and `Table`),
/// so the inner calls read `p.opta[0..n-2]` exactly as the C does.
///
/// # Panics
/// Panics if `p.n_inputs < 4` (use [`tetrahedral_16`] for 3 inputs).
pub fn eval_n_inputs(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    assert!(p.n_inputs >= 4, "eval_n_inputs requires >= 4 inputs");
    eval_n_inputs_rec(input, output, table, p, p.n_inputs);
}

/// Recursive worker for [`eval_n_inputs`]. `n` is the number of *remaining*
/// inputs at this level (counts down to 3, where the tetrahedral kernel runs).
fn eval_n_inputs_rec(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams, n: usize) {
    if n == 3 {
        // lcms2 inlines TetrahedralInterp16 here; tetrahedral_16 is bit-identical.
        tetrahedral_16(input, output, table, p);
        return;
    }

    // NM = N - 1 in the C macro; the first input is fixed on opta[NM]/Domain[0].
    let nm = n - 1;

    let fk = to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let k0 = fixed_to_int(fk);
    let rk = fixed_rest_to_int(fk);

    // K0 = opta[NM] * k0; K1 = opta[NM] * (k0 + (Input[0] != 0xFFFF ? 1 : 0)).
    let opta_nm = p.opta[nm] as i32;
    let k0_idx = opta_nm * k0;
    let k1_idx = opta_nm * (k0 + if input[0] != 0xFFFF { 1 } else { 0 });

    // p1 is *p with Domain shifted left by one (Domain[0..NM] <- Domain[1..N]).
    // opta/n_samples are untouched by the C memmove, so the inner level reads the
    // same opta entries.
    let p1 = shift_domain(p, nm);

    let n_out = p.n_outputs;
    let mut tmp1 = [0u16; MAX_STAGE_CHANNELS];
    let mut tmp2 = [0u16; MAX_STAGE_CHANNELS];

    eval_n_inputs_rec(
        &input[1..],
        &mut tmp1[..n_out],
        &table[k0_idx as usize..],
        &p1,
        nm,
    );
    eval_n_inputs_rec(
        &input[1..],
        &mut tmp2[..n_out],
        &table[k1_idx as usize..],
        &p1,
        nm,
    );

    for i in 0..n_out {
        output[i] = linear_interp(rk, tmp1[i] as i32, tmp2[i] as i32);
    }
}

/// Float n-D interpolation for 4..=15 inputs, bit-identical to lcms2's
/// `Eval4InputsFloat`..`Eval15InputsFloat`. Same decomposition as
/// [`eval_n_inputs`] but in floating point, bottoming out at
/// [`tetrahedral_float`].
///
/// # Panics
/// Panics if `p.n_inputs < 4`.
pub fn eval_n_inputs_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    assert!(p.n_inputs >= 4, "eval_n_inputs_float requires >= 4 inputs");
    eval_n_inputs_float_rec(input, output, table, p, p.n_inputs);
}

fn eval_n_inputs_float_rec(
    input: &[f32],
    output: &mut [f32],
    table: &[f32],
    p: &InterpParams,
    n: usize,
) {
    if n == 3 {
        tetrahedral_float(input, output, table, p);
        return;
    }

    let nm = n - 1;

    let pk = fclamp(input[0]) * p.domain[0] as f32;
    let k0 = quick_floor(pk);
    let rest = pk - k0 as f32;

    let opta_nm = p.opta[nm] as i32;
    let k0_idx = opta_nm * k0;
    let k1_idx = k0_idx + if fclamp(input[0]) >= 1.0 { 0 } else { opta_nm };

    let p1 = shift_domain(p, nm);

    let n_out = p.n_outputs;
    let mut tmp1 = [0f32; MAX_STAGE_CHANNELS];
    let mut tmp2 = [0f32; MAX_STAGE_CHANNELS];

    eval_n_inputs_float_rec(
        &input[1..],
        &mut tmp1[..n_out],
        &table[k0_idx as usize..],
        &p1,
        nm,
    );
    eval_n_inputs_float_rec(
        &input[1..],
        &mut tmp2[..n_out],
        &table[k1_idx as usize..],
        &p1,
        nm,
    );

    for i in 0..n_out {
        let y0 = tmp1[i];
        let y1 = tmp2[i];
        output[i] = y0 + (y1 - y0) * rest;
    }
}

/// `_cmsQuickFloor` (lcms2_internal.h, the fast-floor path; `CMS_DONT_USE_FAST_FLOOR`
/// is *not* defined in the pinned build): add the magic `2^36 * 1.5` to `val`,
/// then take the low 32 bits of the IEEE-754 double (`halves[0]` on a
/// little-endian host) arithmetically shifted right by 16. The `+0.5` in
/// `Eval4InputsFloat`/`EvalNFloat` is *not* applied here — they call
/// `_cmsQuickFloor(pk)` directly — so this is a bit-exact transcription of the C
/// macro the oracle exposes as `rcms_oracle_quick_floor`.
#[inline]
fn quick_floor(val: f32) -> i32 {
    const MAGIC: f64 = 68_719_476_736.0 * 1.5; // 2^36 * 1.5
    let temp = val as f64 + MAGIC;
    // halves[0] = low 32 bits of the f64 on little-endian; `>> 16` is arithmetic
    // (signed int shift in C).
    (temp.to_bits() as u32 as i32) >> 16
}

/// Build the inner-level [`InterpParams`] for the n-D recursion: a copy of `p`
/// with `domain` shifted left by one (`domain[0..keep] = p.domain[1..=keep]`) and
/// `n_inputs` reduced. lcms2 only `memmove`s `Domain` (and offsets `Table`); it
/// leaves `opta`/`nSamples`/`nOutputs` untouched, and so does this.
fn shift_domain(p: &InterpParams, keep: usize) -> InterpParams {
    let mut domain = p.domain.clone();
    domain[..keep].copy_from_slice(&p.domain[1..=keep]);
    InterpParams {
        n_inputs: keep,
        n_outputs: p.n_outputs,
        n_samples: p.n_samples.clone(),
        domain,
        opta: p.opta.clone(),
    }
}
