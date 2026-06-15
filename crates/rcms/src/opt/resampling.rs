//! lcms2's CLUT-bake optimizers: `OptimizeByComputingLinearization`
//! (`cmsopt.c:1045-1262`) and `OptimizeByResampling` (`cmsopt.c:646-814`).
//!
//! Both bake the (already pre-optimized) device-link pipeline into a single
//! resampled CLUT — a *lossy* simplification lcms2-default installs for LUT/CLUT
//! transforms. They share three machine parts transcribed verbatim here:
//!
//! - the grid sampler ([`sample_clut16`], lcms2 `cmsStageSampleCLut16bit` +
//!   `XFormSampler16`): sweep every grid node, decode it to a float input, eval
//!   the source pipeline in float, quantize the outputs to u16;
//! - the baked-pipeline 16-bit eval ([`Prelin16Eval`]/[`Prelin8Eval`], lcms2
//!   `PrelinEval16`/`PrelinEval8`): optional prelinearization curves (`Lerp16`),
//!   then the CLUT (tetrahedral for 3 inputs, n-D for ≥4), then optional
//!   postlinearization;
//! - the white-point fixup ([`fix_white_misalignment`], lcms2
//!   `FixWhiteMisalignment`): patch the on-grid white node to pure white.
//!
//! `OptimizeByComputingLinearization` additionally extracts per-channel
//! prelinearization curves by sampling the device link's gray-ramp response
//! (4096 points), slope-limiting, reversing, and pre-multiplying them into the
//! sampled space — the eval then runs `Trans` curves before the CLUT.
//!
//! Float formats decline both optimizers (the lossy guard), so this module is
//! only ever reached from the 16-bit eval path.

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::curve::{build_tabulated_16, reverse_tone_curve_ex, ToneCurve};
use crate::fixed::{from_8_to_16, to_fixed_domain};
use crate::format::decode::{PixelFormat, PT_CMYK, PT_GRAY, PT_RGB};
use crate::interp::{interp_factory, Interp16, InterpFn, InterpParams, MAX_STAGE_CHANNELS};
use crate::pipeline::{Pipeline, Stage};

/// lcms2 `PRELINEARIZATION_POINTS` (cmsopt.c:418).
const PRELINEARIZATION_POINTS: usize = 4096;

/// lcms2 `INTENT_ABSOLUTE_COLORIMETRIC` (lcms2.h).
const INTENT_ABSOLUTE_COLORIMETRIC: u32 = 3;

/// lcms2 `_cmsQuantizeVal` (cmslut.c:737-743): the `i`-th node of a `max_samples`
/// linear ramp, `(i * 65535) / (max_samples - 1)` saturated.
fn quantize_val(i: u32, max_samples: u32) -> u16 {
    let x = (i as f64 * 65535.0) / (max_samples - 1) as f64;
    Lcms2Floor::quick_saturate_word(x)
}

/// lcms2 `_cmsReasonableGridpointsByColorspace` (cmspcs.c:659-704), DEFAULT path
/// only (no HIGHRES/LOWRES/explicit grid flags — Lcms2Compat targets stock
/// `cmsCreateTransform(.., 0)`). Returns the per-axis grid node count.
fn reasonable_gridpoints(pt: u32) -> u32 {
    let n_channels = match pt {
        PT_GRAY => 1,
        PT_RGB => 3,
        PT_CMYK => 4,
        _ => 3,
    };
    // Default values (no flags).
    if n_channels > 4 {
        return 7; // 7 for Hifi
    }
    if n_channels == 4 {
        return 17; // 17 for CMYK
    }
    33 // 33 for RGB
}

// ---------------------------------------------------------------------------
// Grid sampler (cmsStageSampleCLut16bit + XFormSampler16)
// ---------------------------------------------------------------------------

/// lcms2 `cmsStageSampleCLut16bit` (cmslut.c:748-811) driven by `XFormSampler16`
/// (cmsopt.c:422-447): sweep every grid node of an `n_in -> n_out` CLUT with
/// `params`, evaluate `src` in float at each node, and write the quantized u16
/// outputs into a freshly built row-major table.
///
/// The node decode is verbatim: flat index `i`, `t` from `n_in-1` down to 0,
/// `Colorant = rest % nSamples[t]`, `In[t] = _cmsQuantizeVal(...)`; then
/// `XFormSampler16` does `InFloat = In/65535.0`, `cmsPipelineEvalFloat`, and
/// `Out = _cmsQuickSaturateWord(OutFloat * 65535.0)`.
fn sample_clut16(params: &InterpParams, src: &Pipeline) -> Vec<u16> {
    let n_in = params.n_inputs;
    let n_out = params.n_outputs;
    let n_samples = &params.n_samples;

    // CubeSize: product of per-axis sample counts.
    let n_total: usize = n_samples[..n_in].iter().map(|&s| s as usize).product();

    let mut table = vec![0u16; n_total * n_out];

    let mut in16 = [0u16; MAX_STAGE_CHANNELS];
    let mut in_float = [0f32; MAX_STAGE_CHANNELS];

    let mut index = 0usize;
    for i in 0..n_total {
        let mut rest = i;
        // t from n_in-1 down to 0.
        for t in (0..n_in).rev() {
            let colorant = (rest % n_samples[t] as usize) as u32;
            rest /= n_samples[t] as usize;
            in16[t] = quantize_val(colorant, n_samples[t]);
        }

        // XFormSampler16: 16 -> float, eval source pipeline in float, float -> 16.
        for t in 0..n_in {
            in_float[t] = (in16[t] as f64 / 65535.0) as f32;
        }
        let out_float = src.eval_float(&in_float[..n_in]);
        for t in 0..n_out {
            table[index + t] = Lcms2Floor::quick_saturate_word(out_float[t] as f64 * 65535.0);
        }

        index += n_out;
    }

    table
}

// ---------------------------------------------------------------------------
// Baked-pipeline evals (PrelinEval16 / PrelinEval8)
// ---------------------------------------------------------------------------

/// The 16-bit baked-pipeline evaluator, lcms2 `PrelinEval16` (cmsopt.c:301-322):
/// per-input prelinearization curves (`Lerp16` over the curve's 16-bit table),
/// the CLUT (its `Lerp16` — tetrahedral for 3 inputs, n-D for ≥4), and per-output
/// postlinearization curves. `None` curve slots are `Eval16nop1D` passthroughs.
#[derive(Clone)]
pub struct Prelin16Eval {
    n_inputs: usize,
    n_outputs: usize,
    curve_in: Option<Vec<ToneCurve>>,
    clut_table: Vec<u16>,
    clut_params: InterpParams,
    clut_interp: Interp16,
    curve_out: Option<Vec<ToneCurve>>,
}

impl Prelin16Eval {
    /// lcms2 `PrelinEval16`.
    pub fn eval(&self, input: &[u16], output: &mut [u16]) {
        let mut abc = [0u16; MAX_STAGE_CHANNELS];
        for i in 0..self.n_inputs {
            abc[i] = match &self.curve_in {
                Some(c) => c[i].eval_16(input[i]),
                None => input[i], // Eval16nop1D
            };
        }

        let mut def = [0u16; MAX_STAGE_CHANNELS];
        self.clut_interp.eval(
            &abc[..self.n_inputs],
            &mut def[..self.n_outputs],
            &self.clut_table,
            &self.clut_params,
        );

        for i in 0..self.n_outputs {
            output[i] = match &self.curve_out {
                Some(c) => c[i].eval_16(def[i]),
                None => def[i], // Eval16nop1D
            };
        }
    }
}

/// The 8-bit-input baked-pipeline evaluator, lcms2 `PrelinEval8` (cmsopt.c:929-1016)
/// with `PrelinOpt8alloc` (cmsopt.c:861-911). Precomputes, for each of 256 input
/// bytes, the prelin-curve-evaluated node base offset (`X0/Y0/Z0`) and fixed-point
/// remainder (`rx/ry/rz`), then runs the 6-leaf tetrahedral with `0x8001`
/// rounding. RGB only (3 inputs). Used when the device link's input format is
/// 8-bit.
#[derive(Clone)]
pub struct Prelin8Eval {
    rx: [u16; 256],
    ry: [u16; 256],
    rz: [u16; 256],
    x0: [u32; 256],
    y0: [u32; 256],
    z0: [u32; 256],
    opta: [u32; 3],
    n_outputs: usize,
    table: Vec<u16>,
}

impl Prelin8Eval {
    /// lcms2 `PrelinOpt8alloc` (cmsopt.c:861-911). `curves` are the 3 prelin
    /// curves (or `None` for the resampling case with no prelinearization).
    fn build(params: &InterpParams, table: Vec<u16>, curves: Option<&[ToneCurve]>) -> Prelin8Eval {
        let mut p8 = Prelin8Eval {
            rx: [0; 256],
            ry: [0; 256],
            rz: [0; 256],
            x0: [0; 256],
            y0: [0; 256],
            z0: [0; 256],
            opta: [params.opta[0], params.opta[1], params.opta[2]],
            n_outputs: params.n_outputs,
            table,
        };

        for i in 0..256usize {
            let input: [u16; 3] = match curves {
                Some(g) => [
                    g[0].eval_16(from_8_to_16(i as u8)),
                    g[1].eval_16(from_8_to_16(i as u8)),
                    g[2].eval_16(from_8_to_16(i as u8)),
                ],
                None => {
                    let v = from_8_to_16(i as u8);
                    [v, v, v]
                }
            };

            // Move to 0..1.0 in fixed domain.
            let v1 = to_fixed_domain(input[0] as i32 * params.domain[0] as i32);
            let v2 = to_fixed_domain(input[1] as i32 * params.domain[1] as i32);
            let v3 = to_fixed_domain(input[2] as i32 * params.domain[2] as i32);

            // Precalculated table of nodes (opta[2]*FIXED_TO_INT, etc.).
            p8.x0[i] = params.opta[2].wrapping_mul((v1 >> 16) as u32);
            p8.y0[i] = params.opta[1].wrapping_mul((v2 >> 16) as u32);
            p8.z0[i] = params.opta[0].wrapping_mul((v3 >> 16) as u32);

            // Precalculated table of offsets (FIXED_REST_TO_INT).
            p8.rx[i] = (v1 & 0xFFFF) as u16;
            p8.ry[i] = (v2 & 0xFFFF) as u16;
            p8.rz[i] = (v3 & 0xFFFF) as u16;
        }

        p8
    }

    /// lcms2 `PrelinEval8` (cmsopt.c:929-1016). `input` carries 3 u16 channels
    /// from an 8-bit format (`>> 8` recovers the byte).
    pub fn eval(&self, input: &[u16], output: &mut [u16]) {
        let r = (input[0] >> 8) as usize;
        let g = (input[1] >> 8) as usize;
        let b = (input[2] >> 8) as usize;

        let x0 = self.x0[r] as i32;
        let y0 = self.y0[g] as i32;
        let z0 = self.z0[b] as i32;

        let rx = self.rx[r] as i32;
        let ry = self.ry[g] as i32;
        let rz = self.rz[b] as i32;

        let x1 = x0 + if rx == 0 { 0 } else { self.opta[2] as i32 };
        let y1 = y0 + if ry == 0 { 0 } else { self.opta[1] as i32 };
        let z1 = z0 + if rz == 0 { 0 } else { self.opta[0] as i32 };

        let total_out = self.n_outputs;
        let t = &self.table;
        // DENS(i,j,k) = LutTable[i + j + k + OutChan]
        let dens = |i: i32, j: i32, k: i32, out_chan: i32| -> i32 {
            t[(i + j + k + out_chan) as usize] as i32
        };

        for out_chan in 0..total_out as i32 {
            let c0 = dens(x0, y0, z0, out_chan);
            let (c1, c2, c3);

            if rx >= ry && ry >= rz {
                c1 = dens(x1, y0, z0, out_chan) - c0;
                c2 = dens(x1, y1, z0, out_chan) - dens(x1, y0, z0, out_chan);
                c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0, out_chan);
            } else if rx >= rz && rz >= ry {
                c1 = dens(x1, y0, z0, out_chan) - c0;
                c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0, z1, out_chan);
                c3 = dens(x1, y0, z1, out_chan) - dens(x1, y0, z0, out_chan);
            } else if rz >= rx && rx >= ry {
                c1 = dens(x1, y0, z1, out_chan) - dens(x0, y0, z1, out_chan);
                c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0, z1, out_chan);
                c3 = dens(x0, y0, z1, out_chan) - c0;
            } else if ry >= rx && rx >= rz {
                c1 = dens(x1, y1, z0, out_chan) - dens(x0, y1, z0, out_chan);
                c2 = dens(x0, y1, z0, out_chan) - c0;
                c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0, out_chan);
            } else if ry >= rz && rz >= rx {
                c1 = dens(x1, y1, z1, out_chan) - dens(x0, y1, z1, out_chan);
                c2 = dens(x0, y1, z0, out_chan) - c0;
                c3 = dens(x0, y1, z1, out_chan) - dens(x0, y1, z0, out_chan);
            } else if rz >= ry && ry >= rx {
                c1 = dens(x1, y1, z1, out_chan) - dens(x0, y1, z1, out_chan);
                c2 = dens(x0, y1, z1, out_chan) - dens(x0, y0, z1, out_chan);
                c3 = dens(x0, y0, z1, out_chan) - c0;
            } else {
                c1 = 0;
                c2 = 0;
                c3 = 0;
            }

            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            output[out_chan as usize] =
                (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    }
}

/// The installed baked eval (an [`super::OptimizedEval`] payload). Either the
/// 16-bit `PrelinEval16` path or the 8-bit `PrelinEval8` path.
#[derive(Clone)]
pub enum BakedEval {
    /// lcms2 `PrelinEval16` (or the plain `Lerp16` resampling eval, expressed as
    /// a `PrelinEval16` with no pre/post curves).
    Prelin16(Box<Prelin16Eval>),
    /// lcms2 `PrelinEval8` (RGB, 8-bit input).
    Prelin8(Box<Prelin8Eval>),
}

impl BakedEval {
    pub fn eval(&self, input: &[u16], output: &mut [u16]) {
        match self {
            BakedEval::Prelin16(p) => p.eval(input, output),
            BakedEval::Prelin8(p) => p.eval(input, output),
        }
    }
}

// ---------------------------------------------------------------------------
// FixWhiteMisalignment (cmsopt.c:565-635)
// ---------------------------------------------------------------------------

/// lcms2 `_cmsEndPointsBySpace` (cmspcs.c:707-...), white only, for the spaces the
/// testbed device links use. Returns the white-point u16 vector for `pt`, or
/// `None` for spaces lcms2 doesn't fix up.
fn white_point_by_space(pt: u32) -> Option<Vec<u16>> {
    match pt {
        PT_GRAY => Some(vec![0xffff]),
        PT_RGB => Some(vec![0xffff, 0xffff, 0xffff]),
        PT_CMYK => Some(vec![0, 0, 0, 0]),
        // Lab/XYZ handled elsewhere in lcms2; device links here are device
        // colorspaces on both ends, so those branches never fire for our chain.
        _ => None,
    }
}

/// lcms2 `WhitesAreEqual` (cmsopt.c:549-560).
fn whites_are_equal(white1: &[u16], white2: &[u16]) -> bool {
    for (a, b) in white1.iter().zip(white2.iter()) {
        if (*a as i32 - *b as i32).unsigned_abs() > 0xf000 {
            return true; // so extremely different that the fixup should be avoided
        }
        if a != b {
            return false;
        }
    }
    true
}

/// lcms2 `FixWhiteMisalignment` (cmsopt.c:565-635), specialized to a Prelin/CLUT
/// baked pipeline whose CLUT is `clut_table`. Patches the on-grid white node of
/// the CLUT to map white-in to white-out, mutating `clut_table` in place. Returns
/// without effect (matching the C's "we don't care if it fails" path) when the
/// white node is not exactly on a grid node.
///
/// `prelin`/`postlin` are the installed pre/post curve sets (the resampling
/// default-flags path has none; the linearization path has only `prelin`).
#[allow(clippy::too_many_arguments)]
fn fix_white_misalignment(
    clut_table: &mut [u16],
    params: &InterpParams,
    entry_pt: u32,
    exit_pt: u32,
    prelin: Option<&[ToneCurve]>,
    postlin: Option<&[ToneCurve]>,
    eval_baked: &dyn Fn(&[u16], &mut [u16]),
) {
    let white_in = match white_point_by_space(entry_pt) {
        Some(w) => w,
        None => return,
    };
    let white_out = match white_point_by_space(exit_pt) {
        Some(w) => w,
        None => return,
    };
    let n_ins = white_in.len();
    let n_outs = white_out.len();

    if params.n_inputs != n_ins || params.n_outputs != n_outs {
        return;
    }

    // cmsPipelineEval16(WhitePointIn, ObtainedOut, Lut) on the BAKED pipeline.
    let mut obtained = [0u16; MAX_STAGE_CHANNELS];
    eval_baked(&white_in, &mut obtained[..n_outs]);

    if whites_are_equal(&white_out, &obtained[..n_outs]) {
        return; // whites already match
    }

    // Interpolate white points through pre/post curves.
    let mut node_in = [0u16; MAX_STAGE_CHANNELS];
    if let Some(curves) = prelin {
        for i in 0..n_ins {
            node_in[i] = curves[i].eval_16(white_in[i]);
        }
    } else {
        node_in[..n_ins].copy_from_slice(&white_in);
    }

    let mut node_out = [0u16; MAX_STAGE_CHANNELS];
    if let Some(curves) = postlin {
        for i in 0..n_outs {
            let inverse = reverse_tone_curve_ex(4096, &curves[i]);
            node_out[i] = inverse.eval_16(white_out[i]);
        }
    } else {
        node_out[..n_outs].copy_from_slice(&white_out);
    }

    // PatchLUT: locate the exact node and overwrite, else do nothing.
    patch_lut(clut_table, params, &node_in[..n_ins], &node_out[..n_outs]);
}

/// lcms2 `PatchLUT` (cmsopt.c:470-546): if `at` lands exactly on a grid node, set
/// that node's outputs to `value`. Supports 1/3/4 input channels (the device
/// spaces in our chain). No-op otherwise.
fn patch_lut(table: &mut [u16], params: &InterpParams, at: &[u16], value: &[u16]) {
    let n_in = params.n_inputs;
    let n_out = params.n_outputs;

    // Per-axis: px = At[k] * Domain[k] / 65535; must be integral.
    let mut coords = [0i64; 4];
    if n_in > 4 {
        return;
    }
    for k in 0..n_in {
        let px = (at[k] as f64 * params.domain[k] as f64) / 65535.0;
        let x0 = px.floor();
        if (px - x0) != 0.0 {
            return; // Not on exact node
        }
        coords[k] = x0 as i64;
    }

    // index = sum opta[n_in-1-k] * coord[k]  (lcms2 indexes opta in reverse).
    let mut index: i64 = 0;
    for (k, &coord) in coords.iter().enumerate().take(n_in) {
        index += params.opta[n_in - 1 - k] as i64 * coord;
    }

    for (i, &v) in value.iter().enumerate().take(n_out) {
        let idx = index as usize + i;
        if idx < table.len() {
            table[idx] = v;
        }
    }
}

// ---------------------------------------------------------------------------
// OptimizeByResampling (cmsopt.c:646-814), DEFAULT flags
// ---------------------------------------------------------------------------

/// Map a `PixelFormat`'s colorspace to its grid input-channel count for grid
/// sizing, returning `None` for spaces we cannot drive (matches lcms2's
/// `_cmsICCcolorSpace == 0` guard).
fn pt_of(fmt: PixelFormat) -> Option<u32> {
    match fmt.colorspace() {
        PT_GRAY => Some(PT_GRAY),
        PT_RGB => Some(PT_RGB),
        PT_CMYK => Some(PT_CMYK),
        _ => None,
    }
}

/// lcms2 `OptimizeByResampling` (cmsopt.c:646-814) for DEFAULT flags (no
/// `CLUT_PRE/POST_LINEARIZATION`, no `FORCE_CLUT`). Samples `lut` into a single
/// CLUT of the colorspace's reasonable grid resolution, installs the plain
/// `Lerp16` eval, and applies the white-point fixup (skipped on absolute
/// colorimetric). Returns `None` if the colorspace is unspecified.
pub fn optimize_by_resampling(
    lut: &Pipeline,
    in_fmt: u32,
    out_fmt: u32,
    intent: u32,
) -> Option<super::OptimizedEval> {
    let inf = PixelFormat(in_fmt);
    let outf = PixelFormat(out_fmt);

    // Lossy: never for float.
    if inf.is_float() || outf.is_float() {
        return None;
    }

    let entry_pt = pt_of(inf)?;
    let exit_pt = pt_of(outf)?;

    let n_in = lut.input_channels;
    let n_out = lut.output_channels;

    // For empty LUTs, 2 points are enough.
    let n_grid = if lut.stages().is_empty() {
        2
    } else {
        // Lab16 input cannot be optimized by a CLUT — not reachable for our device
        // spaces (no Lab device ends), so we don't special-case it here.
        reasonable_gridpoints(entry_pt)
    };

    let n_samples: Vec<u32> = vec![n_grid; n_in];
    let params = InterpParams::new(&n_samples, n_in, n_out);

    // Sample the source pipeline (no pre/post linearization in default flags).
    let table = sample_clut16(&params, lut);

    let mut baked = build_baked_eval(table, params.clone(), None, in_fmt);

    // White-point fixup (skip on absolute colorimetric).
    if intent != INTENT_ABSOLUTE_COLORIMETRIC {
        apply_white_fixup(&mut baked, &params, entry_pt, exit_pt, None);
    }

    Some(super::OptimizedEval::Baked(Box::new(baked)))
}

// ---------------------------------------------------------------------------
// OptimizeByComputingLinearization (cmsopt.c:1045-1262), DEFAULT flags
// ---------------------------------------------------------------------------

/// lcms2 `SlopeLimiting` (cmsopt.c:826-857): normalize the curve endpoints by
/// slope-limiting the lowest/highest 2% to assure exact endpoints. Mutates the
/// 16-bit table in place; handles descending curves.
fn slope_limiting(table: &mut [u16], descending: bool) {
    let n_entries = table.len();
    let at_begin = (n_entries as f64 * 0.02 + 0.5).floor() as i32; // Cutoff at 2%
    let at_end = n_entries as i32 - at_begin - 1; // And 98%

    if at_begin <= 0 {
        return;
    }

    let (begin_val, end_val): (i32, i32) = if descending { (0xffff, 0) } else { (0, 0xffff) };

    // Begin of curve.
    let val = table[at_begin as usize] as f64;
    let slope = (val - begin_val as f64) / at_begin as f64;
    let beta = val - slope * at_begin as f64;
    for (i, slot) in table.iter_mut().enumerate().take(at_begin as usize) {
        *slot = Lcms2Floor::quick_saturate_word(i as f64 * slope + beta);
    }

    // End of curve (AtBegin holds the X interval).
    let val = table[at_end as usize] as f64;
    let slope = (end_val as f64 - val) / at_begin as f64;
    let beta = val - slope * at_end as f64;
    for (i, slot) in table.iter_mut().enumerate().skip(at_end as usize) {
        *slot = Lcms2Floor::quick_saturate_word(i as f64 * slope + beta);
    }
}

/// lcms2 `OptimizeByComputingLinearization` (cmsopt.c:1045-1262), DEFAULT flags.
/// Chunky RGB → RGB only. Builds gray-ramp prelin curves, slope-limits, reverses
/// them, prepends the reverse curves to the source, samples a CLUT, and installs
/// the prelin-curve + CLUT eval (`PrelinEval8`/`PrelinEval16`). Returns `None`
/// when the optimizer declines (non-RGB, planar, float, degenerate/non-monotone
/// linearization).
pub fn optimize_by_computing_linearization(
    lut: &Pipeline,
    in_fmt: u32,
    out_fmt: u32,
    intent: u32,
) -> Option<super::OptimizedEval> {
    let inf = PixelFormat(in_fmt);
    let outf = PixelFormat(out_fmt);

    // Lossy: never for float.
    if inf.is_float() || outf.is_float() {
        return None;
    }
    // Only on chunky RGB -> RGB.
    if inf.colorspace() != PT_RGB || inf.planar() {
        return None;
    }
    if outf.colorspace() != PT_RGB || outf.planar() {
        return None;
    }
    // On 16 bits the feature requires CLUT_PRE_LINEARIZATION (not set in default
    // flags), EXCEPT this optimizer still runs for 8-bit input. lcms2 returns
    // FALSE for 16-bit input without the flag.
    let is_8bit_input = inf.bytes() == 1;
    if !is_8bit_input {
        // 16-bit input: lcms2 declines unless CLUT_PRE_LINEARIZATION is set.
        return None;
    }

    let n_in = lut.input_channels; // 3
    let n_out = lut.output_channels; // 3
    let n_grid = reasonable_gridpoints(PT_RGB); // 33

    // If the last stage is a degenerate curve set, we cannot optimize.
    if let Some(Stage::ToneCurves(curves)) = lut.stages().last() {
        for c in curves {
            if c.is_degenerated() {
                return None;
            }
        }
    }

    // Build prelin curves Trans[t] by sampling the gray-ramp response.
    let mut trans_tables: Vec<Vec<u16>> = Vec::with_capacity(n_in);
    for _ in 0..n_in {
        trans_tables.push(vec![0u16; PRELINEARIZATION_POINTS]);
    }
    for i in 0..PRELINEARIZATION_POINTS {
        let v = (i as f64 / (PRELINEARIZATION_POINTS - 1) as f64) as f32;
        let in_vec = vec![v; n_in];
        let out = lut.eval_float(&in_vec);
        for (t, table) in trans_tables.iter_mut().enumerate() {
            table[i] = Lcms2Floor::quick_saturate_word(out[t] as f64 * 65535.0);
        }
    }

    // Slope-limit and validate.
    for table in trans_tables.iter_mut() {
        let descending = table[0] > table[table.len() - 1];
        slope_limiting(table, descending);
    }

    let trans: Vec<ToneCurve> = trans_tables.iter().map(|t| build_tabulated_16(t)).collect();

    // Check for validity (monotonic, not degenerate).
    for c in &trans {
        if !c.is_monotonic() {
            return None;
        }
        if c.is_degenerated() {
            return None;
        }
    }

    // Invert curves.
    let trans_reverse: Vec<ToneCurve> = trans
        .iter()
        .map(|c| reverse_tone_curve_ex(PRELINEARIZATION_POINTS as u32, c))
        .collect();

    // LutPlusCurves = TransReverse prepended to the original LUT.
    let mut lut_plus_curves = lut.clone();
    lut_plus_curves
        .prepend_stage(Stage::ToneCurves(trans_reverse))
        .ok()?;

    // Sample the CLUT.
    let n_samples: Vec<u32> = vec![n_grid; n_in];
    let params = InterpParams::new(&n_samples, n_in, n_out);
    let table = sample_clut16(&params, &lut_plus_curves);

    let mut baked = build_baked_eval(table, params.clone(), Some(&trans), in_fmt);

    // White-point fixup (skip on absolute colorimetric).
    if intent != INTENT_ABSOLUTE_COLORIMETRIC {
        apply_white_fixup(&mut baked, &params, PT_RGB, PT_RGB, Some(&trans));
    }

    Some(super::OptimizedEval::Baked(Box::new(baked)))
}

// ---------------------------------------------------------------------------
// Shared install + fixup helpers
// ---------------------------------------------------------------------------

/// Build the installed baked eval. 8-bit RGB input with 3 inputs -> `PrelinEval8`;
/// otherwise `PrelinEval16`. `prelin` are the optional prelinearization curves.
fn build_baked_eval(
    table: Vec<u16>,
    params: InterpParams,
    prelin: Option<&[ToneCurve]>,
    in_fmt: u32,
) -> BakedEval {
    let inf = PixelFormat(in_fmt);
    let is_8bit = inf.bytes() == 1;

    // lcms2 PrelinEval8 only fires from OptimizeByComputingLinearization (RGB,
    // 3 inputs). OptimizeByResampling always installs the 16-bit eval.
    if is_8bit && params.n_inputs == 3 && prelin.is_some() {
        BakedEval::Prelin8(Box::new(Prelin8Eval::build(&params, table, prelin)))
    } else {
        let interp = match interp_factory(params.n_inputs, params.n_outputs, false, false) {
            InterpFn::Lerp16(l) => l,
            InterpFn::LerpFloat(_) => unreachable!("16-bit CLUT selects Lerp16"),
        };
        BakedEval::Prelin16(Box::new(Prelin16Eval {
            n_inputs: params.n_inputs,
            n_outputs: params.n_outputs,
            curve_in: prelin.map(|c| c.to_vec()),
            clut_table: table,
            clut_params: params,
            clut_interp: interp,
            curve_out: None,
        }))
    }
}

/// Apply [`fix_white_misalignment`] to a baked eval, mutating its CLUT table.
///
/// lcms2's `FixWhiteMisalignment` probes the white point with
/// `cmsPipelineEval16`, which walks the INSTALLED stages (the prelinearization
/// curve set's `Lerp16` then the CLUT16 stage's own `Lerp16` — tetrahedral for 3
/// inputs), NOT the precomputed `PrelinEval8` fast path. So the white probe here
/// always uses a `Prelin16`-style stage walk over the same table/params/curves,
/// even when the installed fast eval is `PrelinEval8`.
fn apply_white_fixup(
    baked: &mut BakedEval,
    params: &InterpParams,
    entry_pt: u32,
    exit_pt: u32,
    prelin: Option<&[ToneCurve]>,
) {
    // Grab a mutable handle to the CLUT table and a snapshot of it for the probe.
    let table_ref: &mut Vec<u16> = match baked {
        BakedEval::Prelin16(p) => &mut p.clut_table,
        BakedEval::Prelin8(p) => &mut p.table,
    };

    // Build the stage-walk probe (cmsPipelineEval16 over the installed stages).
    let interp = match interp_factory(params.n_inputs, params.n_outputs, false, false) {
        InterpFn::Lerp16(l) => l,
        InterpFn::LerpFloat(_) => unreachable!("16-bit CLUT selects Lerp16"),
    };
    let probe = Prelin16Eval {
        n_inputs: params.n_inputs,
        n_outputs: params.n_outputs,
        curve_in: prelin.map(|c| c.to_vec()),
        clut_table: table_ref.clone(),
        clut_params: params.clone(),
        clut_interp: interp,
        curve_out: None,
    };
    let eval_fn = move |inp: &[u16], outp: &mut [u16]| probe.eval(inp, outp);

    fix_white_misalignment(table_ref, params, entry_pt, exit_pt, prelin, None, &eval_fn);
}
