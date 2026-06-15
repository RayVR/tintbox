//! PostScript Level 2 color resource generation (lcms2 `cmsps2.c`).
//!
//! Ports `cmsGetPostScriptCSA` (Color Space Array, device → XYZ/Lab) and
//! `cmsGetPostScriptCRD` (Color Rendering Dictionary, Lab → device). Both build an
//! internal device-link [`Transform`] against an Lab4 PCS, force it to a CLUT by
//! resampling, and **emit PostScript text** — the contract is byte-for-byte
//! identity with lcms2's emitters (number formatting, boilerplate, table layout).
//!
//! The emitters are transcribed verbatim from `cmsps2.c`. The CLUT sampling reuses
//! lcms2's `_cmsOptimizePipeline(cmsFLAGS_FORCE_CLUT)` machinery: sample the source
//! pipeline on the colorspace's reasonable grid ([`reasonable_gridpoints`]) via
//! `XFormSampler16`, then `FixWhiteMisalignment` patches the on-grid white node.
//! This is the same code the resampling optimizer ([`crate::opt::resampling`]) runs,
//! re-expressed here over the public pipeline/interp/curve APIs (and extended to the
//! Lab endpoints CSA/CRD need).
//!
//! ## Determinism
//! `GenerateCRD` (and only it) prepends an `EmitHeader` block carrying a wall-clock
//! `ctime()` timestamp, gated by `!cmsFLAGS_NODEFAULTRESOURCEDEF`. That line is
//! inherently non-deterministic, so tintbox emits the header only behind the same gate
//! and the byte-identity contract is exercised with
//! [`Flags::NODEFAULTRESOURCEDEF`] set (the header + resource-def trailer are then
//! skipped, exactly as in lcms2). CSA never emits a header.

use crate::color::CIEXYZ;
use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::curve::ToneCurve;
use crate::error::{Error, Result};
use crate::interp::{interp_factory, Interp16, InterpFn, InterpParams, MAX_STAGE_CHANNELS};
use crate::link::black_point::{detect_black_point, BlackPoint};
use crate::link::read_input_lut;
use crate::math::matrix::Mat3;
use crate::math::whitepoint::D50;
use crate::pipeline::{Pipeline, Stage};
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::transform::{Flags, Transform};

/// `MAXPSCOLS` (cmsps2.c:32): wrap the CLUT hex dump past 60 columns.
const MAXPSCOLS: i32 = 60;

/// `MAX_ENCODEABLE_XYZ` (lcms2_internal.h:71).
const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;

/// `INTENT_PERCEPTUAL` / `INTENT_ABSOLUTE_COLORIMETRIC` (lcms2.h).
const INTENT_PERCEPTUAL: u32 = 0;
const INTENT_RELATIVE_COLORIMETRIC: u32 = 1;
const INTENT_ABSOLUTE_COLORIMETRIC: u32 = 3;

/// `cmsFLAGS_NODEFAULTRESOURCEDEF` (lcms2.h:1779).
const FLAGS_NODEFAULTRESOURCEDEF: u32 = 0x0100_0000;
/// `cmsFLAGS_NOWHITEONWHITEFIXUP` (lcms2.h:1755).
const FLAGS_NOWHITEONWHITEFIXUP: u32 = 0x0004;
/// `cmsFLAGS_BLACKPOINTCOMPENSATION` (lcms2.h:1754).
const FLAGS_BLACKPOINTCOMPENSATION: u32 = 0x2000;

// ===========================================================================
//  PostScript output buffer + C printf-format helpers
// ===========================================================================

/// The growing PostScript byte buffer plus the `_cmsPSActualColumn` counter the
/// CLUT dumper uses to wrap hex bytes (`WriteByte`, cmsps2.c:300-310).
struct Ps {
    buf: Vec<u8>,
    col: i32,
}

impl Ps {
    fn new() -> Ps {
        Ps {
            buf: Vec::new(),
            col: 0,
        }
    }

    /// Append a literal string (the bulk of `_cmsIOPrintf` with no conversions).
    fn s(&mut self, txt: &str) {
        self.buf.extend_from_slice(txt.as_bytes());
    }

    /// `_cmsIOPrintf(m, "%f", v)` — C `printf %f`: fixed, exactly 6 decimals.
    /// Rust's `{:.6}` matches C byte-for-byte here (incl. `-0.000000`).
    fn f(&mut self, v: f64) {
        self.s(&fmt_f(v, 6));
    }

    /// `_cmsIOPrintf(m, "%.6f", v)`.
    fn f6(&mut self, v: f64) {
        self.s(&fmt_f(v, 6));
    }

    /// `_cmsIOPrintf(m, "%d", v)`.
    fn d(&mut self, v: i64) {
        self.s(&v.to_string());
    }

    /// `_cmsIOPrintf(m, "%g", v)` (C `printf %g`).
    fn g(&mut self, v: f64) {
        self.s(&fmt_g(v));
    }

    /// `WriteByte` (cmsps2.c:300): emit a lowercase 2-hex-digit byte and wrap the
    /// column past `MAXPSCOLS`.
    fn write_byte(&mut self, b: u8) {
        self.s(&format!("{b:02x}"));
        self.col += 2;
        if self.col > MAXPSCOLS {
            self.s("\n");
            self.col = 0;
        }
    }
}

/// C `printf("%.*f", prec, v)`. Rust's formatter rounds half-to-even while C
/// rounds half-away-from-zero, but every value emitted here (white/black XYZ in
/// [0,1], matrix entries) is far from a `.5`-at-`prec` tie, so the two agree. The
/// sign of negative zero is preserved by both.
fn fmt_f(v: f64, prec: usize) -> String {
    format!("{v:.prec$}")
}

/// C `printf("%g", v)` (default precision 6). Used only by `Emit1Gamma`'s
/// `{ %g exp }` path (no testbed profile reaches it, but kept faithful):
/// shortest of `%e`/`%f` with up to 6 significant digits, trailing zeros and a
/// trailing decimal point stripped.
fn fmt_g(v: f64) -> String {
    fmt_g_prec(v, 6)
}

fn fmt_g_prec(v: f64, precision: usize) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let p = if precision == 0 { 1 } else { precision };
    let exp = v.abs().log10().floor() as i32;
    // C: use %e if exp < -4 or exp >= precision, else %f.
    if exp < -4 || exp >= p as i32 {
        // %e with (p-1) digits after the point, then strip trailing zeros.
        let s = format!("{:.*e}", p - 1, v);
        strip_g_exp(&s)
    } else {
        let dec = (p as i32 - 1 - exp).max(0) as usize;
        let s = format!("{v:.dec$}");
        strip_g_fixed(&s)
    }
}

fn strip_g_fixed(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

fn strip_g_exp(s: &str) -> String {
    // Split mantissa and exponent (Rust emits e.g. "1.5e2"; C emits "1.5e+02").
    let (mant, exp) = match s.split_once('e') {
        Some((m, e)) => (m, e),
        None => return s.to_string(),
    };
    let mant = strip_g_fixed(mant);
    let exp_num: i32 = exp.parse().unwrap_or(0);
    let sign = if exp_num < 0 { '-' } else { '+' };
    format!("{mant}e{sign}{:02}", exp_num.abs())
}

// ===========================================================================
//  Colorspace helpers (PT codes + endpoints)
// ===========================================================================

/// Channel count for the `cmsFormatterForColorspaceOfProfile` output.
fn channels_of(cs: ColorSpace) -> Option<usize> {
    match cs {
        ColorSpace::Gray => Some(1),
        ColorSpace::Rgb | ColorSpace::Lab | ColorSpace::XYZ | ColorSpace::Cmy => Some(3),
        ColorSpace::Cmyk => Some(4),
        _ => None,
    }
}

/// lcms2 `_cmsEndPointsBySpace` (cmspcs.c:707): the white-point u16 vector, for
/// the spaces CSA/CRD touch. Returns `(white, n)`.
fn white_point_by_space(cs: ColorSpace) -> Option<(Vec<u16>, usize)> {
    Some(match cs {
        ColorSpace::Gray => (vec![0xffff], 1),
        ColorSpace::Rgb => (vec![0xffff, 0xffff, 0xffff], 3),
        ColorSpace::Lab => (vec![0xffff, 0x8080, 0x8080], 3),
        ColorSpace::Cmyk => (vec![0, 0, 0, 0], 4),
        ColorSpace::Cmy => (vec![0, 0, 0], 3),
        _ => return None,
    })
}

/// lcms2 `_cmsReasonableGridpointsByColorspace` (cmspcs.c:659), DEFAULT flags.
fn reasonable_gridpoints(cs: ColorSpace) -> u32 {
    let n = channels_of(cs).unwrap_or(3);
    if n > 4 {
        7
    } else if n == 4 {
        17
    } else {
        33
    }
}

// ===========================================================================
//  FORCE_CLUT resampling (cmsFLAGS_FORCE_CLUT, OptimizeByResampling)
// ===========================================================================

/// A resampled CLUT: the sampled 16-bit table + its [`InterpParams`].
struct ForcedClut {
    table: Vec<u16>,
    params: InterpParams,
}

impl ForcedClut {
    /// Evaluate the bare CLUT (no pre/post lin) in 16-bit, as `cmsPipelineEval16`
    /// over the single CLUT stage does inside `FixWhiteMisalignment`.
    fn eval_16(&self, input: &[u16], output: &mut [u16]) {
        let interp = clut_interp(&self.params);
        interp.eval(input, output, &self.table, &self.params);
    }
}

/// The 16-bit interpolator a sampled CLUT uses (default flags: tetrahedral for 3
/// inputs, n-D for ≥4, etc. — `interp_factory` with no trilinear hint).
fn clut_interp(params: &InterpParams) -> Interp16 {
    match interp_factory(params.n_inputs, params.n_outputs, false, false) {
        InterpFn::Lerp16(l) => l,
        InterpFn::LerpFloat(_) => unreachable!("16-bit CLUT selects Lerp16"),
        InterpFn::Custom(_) => unreachable!("builtin interp_factory never returns Custom"),
    }
}

/// lcms2 `XFormSampler16` + `cmsStageSampleCLut16bit` (cmsopt.c:422 / cmslut.c:748):
/// sweep every node of an `n_in -> n_out` grid of `params`, decode each node to a
/// float input, eval `src` in float, quantize the outputs to u16.
fn sample_clut16(params: &InterpParams, src: &Pipeline) -> Vec<u16> {
    let n_in = params.n_inputs;
    let n_out = params.n_outputs;
    let n_samples = &params.n_samples;

    let n_total: usize = n_samples[..n_in].iter().map(|&s| s as usize).product();
    let mut table = vec![0u16; n_total * n_out];

    let mut in16 = [0u16; MAX_STAGE_CHANNELS];
    let mut in_float = [0f32; MAX_STAGE_CHANNELS];

    let mut index = 0usize;
    for i in 0..n_total {
        let mut rest = i;
        for t in (0..n_in).rev() {
            let colorant = (rest % n_samples[t] as usize) as u32;
            rest /= n_samples[t] as usize;
            in16[t] = quantize_val(colorant, n_samples[t]);
        }
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

/// lcms2 `_cmsQuantizeVal` (cmslut.c:737): `round(i * 65535 / (max-1))` saturated.
fn quantize_val(i: u32, max_samples: u32) -> u16 {
    let x = (i as f64 * 65535.0) / (max_samples - 1) as f64;
    Lcms2Floor::quick_saturate_word(x)
}

/// lcms2 `WhitesAreEqual` (cmsopt.c:549).
fn whites_are_equal(w1: &[u16], w2: &[u16]) -> bool {
    for (a, b) in w1.iter().zip(w2.iter()) {
        if (*a as i32 - *b as i32).unsigned_abs() > 0xf000 {
            return true;
        }
        if a != b {
            return false;
        }
    }
    true
}

/// lcms2 `PatchLUT` (cmsopt.c:470), the 1/3/4-input branches: if `at` lands on an
/// exact grid node, overwrite that node's outputs with `value`.
fn patch_lut(clut: &mut ForcedClut, at: &[u16], value: &[u16]) {
    let p = &clut.params;
    let n_in = p.n_inputs;
    let n_out = p.n_outputs;
    if n_in != 1 && n_in != 3 && n_in != 4 {
        return;
    }

    let mut coords = [0i64; 4];
    for k in 0..n_in {
        let px = (at[k] as f64 * p.domain[k] as f64) / 65535.0;
        let x0 = px.floor();
        if (px - x0) != 0.0 {
            return;
        }
        coords[k] = x0 as i64;
    }

    // index = sum opta[n_in-1-k] * coord[k] (lcms2 indexes opta in reverse).
    let mut index: i64 = 0;
    for (k, &coord) in coords.iter().enumerate().take(n_in) {
        index += p.opta[n_in - 1 - k] as i64 * coord;
    }

    for (i, &v) in value.iter().enumerate().take(n_out) {
        let idx = index as usize + i;
        if idx < clut.table.len() {
            clut.table[idx] = v;
        }
    }
}

/// lcms2 `FixWhiteMisalignment` (cmsopt.c:565) for the single-CLUT FORCE_CLUT
/// result: probe the white point, and if it does not already map to white, patch
/// the on-grid white node. `entry`/`exit` are the device-link's color spaces.
fn fix_white_misalignment(clut: &mut ForcedClut, entry: ColorSpace, exit: ColorSpace) {
    let (white_in, n_ins) = match white_point_by_space(entry) {
        Some(w) => w,
        None => return,
    };
    let (white_out, n_outs) = match white_point_by_space(exit) {
        Some(w) => w,
        None => return,
    };
    if clut.params.n_inputs != n_ins || clut.params.n_outputs != n_outs {
        return;
    }

    let mut obtained = [0u16; MAX_STAGE_CHANNELS];
    clut.eval_16(&white_in, &mut obtained[..n_outs]);

    if whites_are_equal(&white_out, &obtained[..n_outs]) {
        return;
    }

    // No pre/post linearization in the FORCE_CLUT-only result.
    patch_lut(clut, &white_in[..n_ins], &white_out[..n_outs]);
}

/// lcms2 `_cmsOptimizePipeline(.., cmsFLAGS_FORCE_CLUT)` for the CSA/CRD case:
/// resample `src` into a single CLUT of the entry colorspace's reasonable grid,
/// then (unless absolute colorimetric or `NOWHITEONWHITEFIXUP`) fix the white
/// node. `entry`/`exit` are the device-link's input/output color spaces.
fn force_clut(
    src: &Pipeline,
    entry: ColorSpace,
    exit: ColorSpace,
    intent: u32,
    fix_white: bool,
) -> ForcedClut {
    let n_in = src.input_channels;
    let n_out = src.output_channels;

    let n_grid = if src.stages().is_empty() {
        2
    } else {
        reasonable_gridpoints(entry)
    };
    let n_samples: Vec<u32> = vec![n_grid; n_in];
    let params = InterpParams::new(&n_samples, n_in, n_out);
    let table = sample_clut16(&params, src);

    let mut clut = ForcedClut { table, params };

    // lcms2: NOWHITEONWHITEFIXUP forced on for absolute colorimetric.
    let do_fix = fix_white && intent != INTENT_ABSOLUTE_COLORIMETRIC;
    if do_fix {
        fix_white_misalignment(&mut clut, entry, exit);
    }
    clut
}

// ===========================================================================
//  Shared emitters (cmsps2.c)
// ===========================================================================

/// `EmitWhiteBlackD50` (cmsps2.c:391).
fn emit_white_black_d50(m: &mut Ps, black: &CIEXYZ) {
    m.s("/BlackPoint [");
    m.f(black.x);
    m.s(" ");
    m.f(black.y);
    m.s(" ");
    m.f(black.z);
    m.s("]\n");
    m.s("/WhitePoint [");
    m.f(D50.x);
    m.s(" ");
    m.f(D50.y);
    m.s(" ");
    m.f(D50.z);
    m.s("]\n");
}

/// `EmitRangeCheck` (cmsps2.c:405).
fn emit_range_check(m: &mut Ps) {
    m.s("dup 0.0 lt { pop 0.0 } if dup 1.0 gt { pop 1.0 } if ");
}

/// `EmitIntent` (cmsps2.c:415).
fn emit_intent(m: &mut Ps, intent: u32) {
    let s = match intent {
        0 => "Perceptual",
        1 => "RelativeColorimetric",
        3 => "AbsoluteColorimetric",
        2 => "Saturation",
        _ => "Undefined",
    };
    m.s("/RenderingIntent (");
    m.s(s);
    m.s(")\n");
}

/// `EmitLab2XYZ` (cmsps2.c:442).
fn emit_lab2xyz(m: &mut Ps) {
    m.s("/RangeABC [ 0 1 0 1 0 1]\n");
    m.s("/DecodeABC [\n");
    m.s("{100 mul  16 add 116 div } bind\n");
    m.s("{255 mul 128 sub 500 div } bind\n");
    m.s("{255 mul 128 sub 200 div } bind\n");
    m.s("]\n");
    m.s("/MatrixABC [ 1 1 1 1 0 0 0 0 -1]\n");
    m.s("/RangeLMN [ -0.236 1.254 0 1 -0.635 1.640 ]\n");
    m.s("/DecodeLMN [\n");
    m.s("{dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.964200 mul} bind\n");
    m.s("{dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse } bind\n");
    m.s("{dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.824900 mul} bind\n");
    m.s("]\n");
}

/// `cmsEstimateGamma(t, precision)` (cmsgamma.c:1465). Returns the mean exponent,
/// or `-1.0` when the curve is not a clean power function.
fn estimate_gamma(t: &ToneCurve, precision: f64) -> f64 {
    const MAX_NODES_IN_CURVE: u32 = 4097;
    let mut sum = 0.0f64;
    let mut sum2 = 0.0f64;
    let mut n = 0.0f64;

    for i in 1..(MAX_NODES_IN_CURVE - 1) {
        let x = i as f64 / (MAX_NODES_IN_CURVE - 1) as f64;
        let y = t.eval_float(x as f32) as f64;
        if y > 0.0 && y < 1.0 && x > 0.07 {
            let gamma = y.ln() / x.ln();
            sum += gamma;
            sum2 += gamma * gamma;
            n += 1.0;
        }
    }

    if n <= 1.0 {
        return -1.0;
    }
    let std = ((n * sum2 - sum * sum) / (n * (n - 1.0))).sqrt();
    if std > precision {
        return -1.0;
    }
    sum / n
}

/// `Emit1Gamma` (cmsps2.c:464): emit one tone curve as a PostScript transfer
/// function — a constant `1`, a `{ %g exp }` power, or a tabulated lerp.
fn emit_1_gamma(m: &mut Ps, table: Option<&ToneCurve>) {
    let t = match table {
        Some(t) if !t.table16().is_empty() && !t.is_linear() => t,
        _ => {
            m.s("{ 1 } bind ");
            return;
        }
    };

    // Check if it is really an exponential.
    let gamma = estimate_gamma(t, 0.001);
    if gamma > 0.0 {
        m.s("{ ");
        m.g(gamma);
        m.s(" exp } bind ");
        return;
    }

    m.s("{ ");
    emit_range_check(m);
    m.s(" [");
    let tab = t.table16();
    for (i, &v) in tab.iter().enumerate() {
        if i % 10 == 0 {
            m.s("\n  ");
        }
        m.d(v as i64);
        m.s(" ");
    }
    m.s("] ");
    m.s("dup ");
    m.s("length 1 sub ");
    m.s("3 -1 roll ");
    m.s("mul ");
    m.s("dup ");
    m.s("dup ");
    m.s("floor cvi ");
    m.s("exch ");
    m.s("ceiling cvi ");
    m.s("3 index ");
    m.s("exch ");
    m.s("get\n  ");
    m.s("4 -1 roll ");
    m.s("3 -1 roll ");
    m.s("get ");
    m.s("dup ");
    m.s("3 1 roll ");
    m.s("sub ");
    m.s("3 -1 roll ");
    m.s("dup ");
    m.s("floor cvi ");
    m.s("sub ");
    m.s("mul ");
    m.s("add ");
    m.s("65535 div\n");
    m.s(" } bind ");
}

/// `GammaTableEquals` (cmsps2.c:541).
fn gamma_table_equals(g1: &ToneCurve, g2: &ToneCurve) -> bool {
    g1.table16() == g2.table16()
}

/// `EmitNGamma` (cmsps2.c:551): a set of gamma curves, reusing `dup` when a curve
/// equals its predecessor's table.
fn emit_n_gamma(m: &mut Ps, curves: &[ToneCurve]) {
    for i in 0..curves.len() {
        if i > 0 && gamma_table_equals(&curves[i - 1], &curves[i]) {
            m.s("dup ");
        } else {
            emit_1_gamma(m, Some(&curves[i]));
        }
    }
}

// ===========================================================================
//  CLUT dump (WriteCLUT + OutputValueSampler)
// ===========================================================================

/// `Word2Byte` (cmsps2.c:292): `floor(w / 257 + 0.5)`.
fn word2byte(w: u16) -> u8 {
    (w as f64 / 257.0 + 0.5).floor() as u8
}

/// State for the row/column-aware CLUT byte dumper (`cmsPsSamplerCargo`).
struct ClutDump<'a> {
    pre_maj: &'a str,
    post_maj: &'a str,
    pre_min: &'a str,
    post_min: &'a str,
    fix_white: bool,
    color_space: ColorSpace,
    first_component: i64,
    second_component: i64,
}

/// `WriteCLUT` (cmsps2.c:667): emit the grid dims then the row-major hex CLUT,
/// driving `OutputValueSampler` (cmsps2.c:588) over every node.
#[allow(clippy::too_many_arguments)]
fn write_clut(
    m: &mut Ps,
    clut: &ForcedClut,
    pre_maj: &str,
    post_maj: &str,
    pre_min: &str,
    post_min: &str,
    fix_white: bool,
    color_space: ColorSpace,
) {
    let p = &clut.params;
    m.s("[");
    for i in 0..p.n_inputs {
        if i < MAX_STAGE_CHANNELS {
            m.s(" ");
            m.d(p.n_samples[i] as i64);
            m.s(" ");
        }
    }
    m.s(" [\n");

    let mut d = ClutDump {
        pre_maj,
        post_maj,
        pre_min,
        post_min,
        fix_white,
        color_space,
        first_component: -1,
        second_component: -1,
    };

    // SAMPLER_INSPECT sweep: same node order as cmsStageSampleCLut16bit.
    let n_in = p.n_inputs;
    let n_out = p.n_outputs;
    let n_total: usize = p.n_samples[..n_in].iter().map(|&s| s as usize).product();
    let mut in16 = [0u16; MAX_STAGE_CHANNELS];
    let mut out16 = [0u16; MAX_STAGE_CHANNELS];
    let mut idx = 0usize;
    for i in 0..n_total {
        let mut rest = i;
        for t in (0..n_in).rev() {
            let colorant = (rest % p.n_samples[t] as usize) as u32;
            rest /= p.n_samples[t] as usize;
            in16[t] = quantize_val(colorant, p.n_samples[t]);
        }
        out16[..n_out].copy_from_slice(&clut.table[idx..idx + n_out]);
        idx += n_out;
        output_value_sampler(m, &mut d, &in16[..n_in], &mut out16[..n_out]);
    }

    m.s(post_min);
    m.s(post_maj);
    m.s("] ");
}

/// `OutputValueSampler` (cmsps2.c:588): per node, optionally remap the L=100
/// near-neutral row to pure white, manage the per-row parentheses, then emit the
/// cooked output bytes.
fn output_value_sampler(m: &mut Ps, d: &mut ClutDump, input: &[u16], out: &mut [u16]) {
    if d.fix_white && input[0] == 0xFFFF {
        // Only in L* = 100, ab = [-8..8] (0x7800..0x8800).
        if (0x7800..=0x8800).contains(&input[1]) && (0x7800..=0x8800).contains(&input[2]) {
            if let Some((white, n_out)) = white_point_by_space(d.color_space) {
                for (i, slot) in out.iter_mut().enumerate().take(n_out) {
                    *slot = white[i];
                }
            }
        }
    }

    if input[0] as i64 != d.first_component {
        if d.first_component != -1 {
            m.s(d.post_min);
            d.second_component = -1;
            m.s(d.post_maj);
        }
        m.col = 0;
        m.s(d.pre_maj);
        d.first_component = input[0] as i64;
    }

    if input[1] as i64 != d.second_component {
        if d.second_component != -1 {
            m.s(d.post_min);
        }
        m.s(d.pre_min);
        d.second_component = input[1] as i64;
    }

    for &w in out.iter() {
        m.write_byte(word2byte(w));
    }
}

// ===========================================================================
//  CSA (Color Space Array)
// ===========================================================================

/// `EmitCIEBasedA` (cmsps2.c:713).
fn emit_cie_based_a(m: &mut Ps, curve: Option<&ToneCurve>, black: &CIEXYZ) {
    m.s("[ /CIEBasedA\n");
    m.s("  <<\n");
    m.s("/DecodeA ");
    emit_1_gamma(m, curve);
    m.s(" \n");
    m.s("/MatrixA [ 0.9642 1.0000 0.8249 ]\n");
    m.s("/RangeLMN [ 0.0 0.9642 0.0 1.0000 0.0 0.8249 ]\n");
    emit_white_black_d50(m, black);
    emit_intent(m, INTENT_PERCEPTUAL);
    m.s(">>\n");
    m.s("]\n");
}

/// `EmitCIEBasedABC` (cmsps2.c:741). `matrix` is row-major 3x3 (the lcms2 `MAT3`
/// memory layout is column-stored per `r.v[i].n[j]`; see [`write_input_matrix_shaper`]).
fn emit_cie_based_abc(m: &mut Ps, matrix: &[f64; 9], curves: &[ToneCurve], black: &CIEXYZ) {
    m.s("[ /CIEBasedABC\n");
    m.s("<<\n");
    m.s("/DecodeABC [ ");
    emit_n_gamma(m, curves);
    m.s("]\n");
    m.s("/MatrixABC [ ");
    for i in 0..3 {
        // lcms2 reads Matrix[i + 3*0], [i+3*1], [i+3*2] from the MAT3 doubles.
        m.f6(matrix[i]);
        m.s(" ");
        m.f6(matrix[i + 3]);
        m.s(" ");
        m.f6(matrix[i + 6]);
        m.s(" ");
    }
    m.s("]\n");
    m.s("/RangeLMN [ 0.0 0.9642 0.0 1.0000 0.0 0.8249 ]\n");
    emit_white_black_d50(m, black);
    emit_intent(m, INTENT_PERCEPTUAL);
    m.s(">>\n");
    m.s("]\n");
}

/// `EmitCIEBasedDEF` (cmsps2.c:779): the CLUT-based CSA for 3/4-channel inputs.
/// After FORCE_CLUT the pipeline is a single CLUT, so only the `/Table` branch fires.
fn emit_cie_based_def(
    m: &mut Ps,
    clut: &ForcedClut,
    n_in: usize,
    intent: u32,
    black: &CIEXYZ,
) -> bool {
    let (pre_maj, post_maj, pre_min, post_min) = match n_in {
        3 => {
            m.s("[ /CIEBasedDEF\n");
            ("<", ">\n", "", "")
        }
        4 => {
            m.s("[ /CIEBasedDEFG\n");
            ("[", "]\n", "<", ">\n")
        }
        _ => return false,
    };

    m.s("<<\n");
    m.s("/Table ");
    write_clut(
        m,
        clut,
        pre_maj,
        post_maj,
        pre_min,
        post_min,
        false,
        ColorSpace::XYZ, // dummy; FixWhite is false here
    );
    m.s("]\n");
    emit_lab2xyz(m);
    emit_white_black_d50(m, black);
    emit_intent(m, intent);
    m.s("   >>\n");
    m.s("]\n");
    true
}

/// `ExtractGray2Y` (cmsps2.c:840): a 256-entry tone curve of Y for gray input,
/// via a profile → XYZ(double) transform.
fn extract_gray2y(profile: &Profile, intent: u32) -> Result<ToneCurve> {
    use crate::curve::build_tabulated_16;
    use crate::format::decode::{TYPE_GRAY_8, TYPE_XYZ_DBL};

    let xyz = build_xyz_profile()?;
    let ri = RenderingIntent::from_raw(intent);
    let xform = Transform::new_with_formats(
        &[profile, &xyz],
        &[ri, ri],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        TYPE_GRAY_8,
        TYPE_XYZ_DBL,
    )?;

    let mut table = vec![0u16; 256];
    let mut input = [0u8; 1];
    let mut output = [0u8; 24]; // 3 doubles
    for (i, slot) in table.iter_mut().enumerate() {
        input[0] = i as u8;
        xform.do_transform(&input, &mut output, 1);
        // XYZ_DBL: X,Y,Z as f64 little-endian; take Y (bytes 8..16).
        let y = f64::from_le_bytes(output[8..16].try_into().unwrap());
        *slot = Lcms2Floor::quick_saturate_word(y * 65535.0);
    }
    Ok(build_tabulated_16(&table))
}

/// `WriteInputLUT` (cmsps2.c:870): the LUT-based CSA — gray → CIEBasedA, 3/4
/// channel → CIEBasedDEF(G) over the FORCE_CLUT'd device link.
fn write_input_lut(m: &mut Ps, profile: &Profile, intent: u32) -> Result<bool> {
    let cs = profile.header().color_space;
    let n_channels = channels_of(cs).ok_or(Error::Unsupported("CSA: unsupported colorspace"))?;

    let black = match detect_black_point(profile, RenderingIntent::from_raw(intent))? {
        BlackPoint::Resolved(xyz) => xyz,
        BlackPoint::Zero => CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    };

    match n_channels {
        1 => {
            let gray2y = extract_gray2y(profile, intent)?;
            emit_cie_based_a(m, Some(&gray2y), &black);
            Ok(true)
        }
        3 | 4 => {
            // Device link profile -> Lab4, then FORCE_CLUT.
            let lab4 = build_lab4_profile()?;
            let ri = RenderingIntent::from_raw(intent);
            let xform = Transform::new(
                &[profile, &lab4],
                &[ri, ri],
                &[false, false],
                &[1.0, 1.0],
                Flags::NOOPTIMIZE,
            )?;
            let link = xform.lut();
            // Entry = device colorspace, exit = Lab.
            let clut = force_clut(link, cs, ColorSpace::Lab, intent, /*fix_white*/ true);
            let ok = emit_cie_based_def(m, &clut, n_channels, intent, &black);
            Ok(ok)
        }
        _ => Err(Error::Unsupported("CSA: only 1/3/4 channels")),
    }
}

/// lcms2 `GetPtrToMatrix` reads the `_cmsStageMatrixData.Double` (row-major 3x3).
/// `WriteInputMatrixShaper` (cmsps2.c:962): matrix-shaper CSA (gray → CIEBasedA,
/// RGB → CIEBasedABC). Returns whether it emitted.
fn write_input_matrix_shaper(
    m: &mut Ps,
    profile: &Profile,
    matrix: &Mat3,
    shaper: &[ToneCurve],
) -> Result<bool> {
    let cs = profile.header().color_space;
    let black = match detect_black_point(
        profile,
        RenderingIntent::from_raw(INTENT_RELATIVE_COLORIMETRIC),
    )? {
        BlackPoint::Resolved(xyz) => xyz,
        BlackPoint::Zero => CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    };

    if cs == ColorSpace::Gray {
        emit_cie_based_a(m, shaper.first(), &black);
        Ok(true)
    } else if cs == ColorSpace::Rgb {
        // Mat *= MAX_ENCODEABLE_XYZ.
        let mut scaled = [0f64; 9];
        for (s, &v) in scaled.iter_mut().zip(matrix.0.iter()) {
            *s = v * MAX_ENCODEABLE_XYZ;
        }
        emit_cie_based_abc(m, &scaled, shaper, &black);
        Ok(true)
    } else {
        Err(Error::Unsupported("CSA: matrix shaper colorspace"))
    }
}

/// `GenerateCSA` (cmsps2.c:1062): pick matrix-shaper vs LUT path. Returns the
/// emitted bytes, or an error if lcms2 would have returned 0.
fn generate_csa(profile: &Profile, intent: u32) -> Result<Vec<u8>> {
    let mut m = Ps::new();

    if profile.header().device_class == ProfileClass::NamedColor {
        return Err(Error::Unsupported(
            "CSA: named color profiles not supported",
        ));
    }

    // Output (PCS) colorspace must be XYZ or Lab.
    let pcs = profile.header().pcs;
    if pcs != ColorSpace::XYZ && pcs != ColorSpace::Lab {
        return Err(Error::Unsupported("CSA: invalid output color space"));
    }

    // Read the input LUT.
    let lut = read_input_lut(profile, intent)?;

    // Tone curves + matrix can be implemented without any LUT: detect the exact
    // [ToneCurves, Matrix] shape (cmsPipelineCheckAndRetreiveStages).
    if let Some((shaper, matrix)) = check_curveset_matrix(&lut) {
        if !write_input_matrix_shaper(&mut m, profile, &matrix, &shaper)? {
            return Err(Error::Unsupported("CSA: matrix shaper failed"));
        }
    } else if !write_input_lut(&mut m, profile, intent)? {
        return Err(Error::Unsupported("CSA: LUT path failed"));
    }

    Ok(m.buf)
}

/// lcms2 `cmsPipelineCheckAndRetreiveStages(lut, 2, CurveSet, Matrix, &Shaper,
/// &Matrix)`: the pipeline is EXACTLY `[ToneCurves, Matrix]`. Returns the curve set
/// and the matrix (as a row-major 3x3) on a match.
fn check_curveset_matrix(lut: &Pipeline) -> Option<(Vec<ToneCurve>, Mat3)> {
    let stages = lut.stages();
    if stages.len() != 2 {
        return None;
    }
    let shaper = match &stages[0] {
        Stage::ToneCurves(c) => c.clone(),
        _ => return None,
    };
    let matrix = match &stages[1] {
        Stage::Matrix {
            rows: 3,
            cols: 3,
            m,
            offset: None,
        } => {
            let mut a = [0f64; 9];
            a.copy_from_slice(&m[..9]);
            Mat3(a)
        }
        _ => return None,
    };
    Some((shaper, matrix))
}

// ===========================================================================
//  CRD (Color Rendering Dictionary)
// ===========================================================================

/// `EmitPQRStage` (cmsps2.c:1192): the chromatic-adaptation PQR transform block.
fn emit_pqr_stage(m: &mut Ps, profile: &Profile, do_bpc: bool, is_absolute: bool) {
    if is_absolute {
        let white = read_media_white_point(profile);
        m.s("/MatrixPQR [1 0 0 0 1 0 0 0 1 ]\n");
        m.s("/RangePQR [ -0.5 2 -0.5 2 -0.5 2 ]\n");
        m.s("% Absolute colorimetric -- encode to relative to maximize LUT usage\n");
        m.s("/TransformPQR [\n");
        m.s("{0.9642 mul ");
        m.g(white.x);
        m.s(" div exch pop exch pop exch pop exch pop} bind\n");
        m.s("{1.0000 mul ");
        m.g(white.y);
        m.s(" div exch pop exch pop exch pop exch pop} bind\n");
        m.s("{0.8249 mul ");
        m.g(white.z);
        m.s(" div exch pop exch pop exch pop exch pop} bind\n]\n");
        return;
    }

    m.s("% Bradford Cone Space\n");
    m.s("/MatrixPQR [0.8951 -0.7502 0.0389 0.2664 1.7135 -0.0685 -0.1614 0.0367 1.0296 ] \n");
    m.s("/RangePQR [ -0.5 2 -0.5 2 -0.5 2 ]\n");

    if !do_bpc {
        m.s("% VonKries-like transform in Bradford Cone Space\n");
        m.s("/TransformPQR [\n");
        m.s("{exch pop exch 3 get mul exch pop exch 3 get div} bind\n");
        m.s("{exch pop exch 4 get mul exch pop exch 4 get div} bind\n");
        m.s("{exch pop exch 5 get mul exch pop exch 5 get div} bind\n]\n");
    } else {
        m.s("% VonKries-like transform in Bradford Cone Space plus BPC\n");
        m.s("/TransformPQR [\n");
        for g in [3, 4, 5] {
            m.s(&format!(
                "{{4 index {g} get div 2 index {g} get mul \
                 2 index {g} get 2 index {g} get sub mul \
                 2 index {g} get 4 index {g} get 3 index {g} get sub mul sub \
                 3 index {g} get 3 index {g} get exch sub div \
                 exch pop exch pop exch pop exch pop }} bind\n"
            ));
        }
        m.s("]\n");
    }
}

/// `EmitXYZ2Lab` (cmsps2.c:1265).
fn emit_xyz2lab(m: &mut Ps) {
    m.s("/RangeLMN [ -0.635 2.0 0 2 -0.635 2.0 ]\n");
    m.s("/EncodeLMN [\n");
    m.s("{ 0.964200  div dup 0.008856 le {7.787 mul 16 116 div add}{1 3 div exp} ifelse } bind\n");
    m.s("{ 1.000000  div dup 0.008856 le {7.787 mul 16 116 div add}{1 3 div exp} ifelse } bind\n");
    m.s("{ 0.824900  div dup 0.008856 le {7.787 mul 16 116 div add}{1 3 div exp} ifelse } bind\n");
    m.s("]\n");
    m.s("/MatrixABC [ 0 1 0 1 -1 1 0 0 -1 ]\n");
    m.s("/EncodeABC [\n");
    m.s("{ 116 mul  16 sub 100 div  } bind\n");
    m.s("{ 500 mul 128 add 256 div  } bind\n");
    m.s("{ 200 mul 128 add 256 div  } bind\n");
    m.s("]\n");
}

/// `WriteOutputLUT` (cmsps2.c:1294): the always-CLUT CRD body.
fn write_output_lut(m: &mut Ps, profile: &Profile, intent: u32, flags: u32) -> Result<bool> {
    let cs = profile.header().color_space;
    let n_channels = channels_of(cs).ok_or(Error::Unsupported("CRD: unsupported colorspace"))?;
    let l_do_bpc = (flags & FLAGS_BLACKPOINTCOMPENSATION) != 0;
    let mut l_fix_white = (flags & FLAGS_NOWHITEONWHITEFIXUP) == 0;

    // For absolute colorimetric, the LUT is encoded as relative.
    let relative_encoding_intent = if intent == INTENT_ABSOLUTE_COLORIMETRIC {
        INTENT_RELATIVE_COLORIMETRIC
    } else {
        intent
    };

    // Lab4 -> profile device link.
    let lab4 = build_lab4_profile()?;
    let ri = RenderingIntent::from_raw(relative_encoding_intent);
    let xform = Transform::new(
        &[&lab4, profile],
        &[ri, ri],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
    )?;
    let link = xform.lut();

    // FORCE_CLUT: sample Lab(grid) -> device. Entry = Lab, exit = device space.
    // lcms2's optimizer FixWhiteMisalignment runs here (skipped on absolute).
    let clut = force_clut(link, ColorSpace::Lab, cs, relative_encoding_intent, true);

    m.s("<<\n");
    m.s("/ColorRenderingType 1\n");

    let black = match detect_black_point(profile, RenderingIntent::from_raw(intent))? {
        BlackPoint::Resolved(xyz) => xyz,
        BlackPoint::Zero => CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    };

    emit_white_black_d50(m, &black);
    emit_pqr_stage(m, profile, l_do_bpc, intent == INTENT_ABSOLUTE_COLORIMETRIC);
    emit_xyz2lab(m);

    // FIXUP: don't fix white on absolute.
    if intent == INTENT_ABSOLUTE_COLORIMETRIC {
        l_fix_white = false;
    }

    m.s("/RenderTable ");
    // The forced CLUT is a single stage -> emit it.
    write_clut(m, &clut, "<", ">\n", "", "", l_fix_white, cs);

    m.s(" ");
    m.d(n_channels as i64);
    m.s(" {} bind ");
    for _ in 1..n_channels {
        m.s("dup ");
    }
    m.s("]\n");

    emit_intent(m, intent);
    m.s(">>\n");

    if (flags & FLAGS_NODEFAULTRESOURCEDEF) == 0 {
        m.s("/Current exch /ColorRendering defineresource pop\n");
    }

    Ok(true)
}

/// `GenerateCRD` (cmsps2.c:1509). The header (with the non-deterministic timestamp)
/// is emitted only when `NODEFAULTRESOURCEDEF` is clear — see the module note.
fn generate_crd(profile: &Profile, intent: u32, flags: u32) -> Result<Vec<u8>> {
    let mut m = Ps::new();

    if (flags & FLAGS_NODEFAULTRESOURCEDEF) == 0 {
        emit_header(&mut m, "Color Rendering Dictionary (CRD)", profile);
    }

    if profile.header().device_class == ProfileClass::NamedColor {
        return Err(Error::Unsupported(
            "CRD: named color profiles not supported",
        ));
    }

    if !write_output_lut(&mut m, profile, intent, flags)? {
        return Err(Error::Unsupported("CRD: output LUT failed"));
    }

    if (flags & FLAGS_NODEFAULTRESOURCEDEF) == 0 {
        m.s("%%EndResource\n");
        m.s("\n% CRD End\n");
    }

    Ok(m.buf)
}

/// `EmitHeader` (cmsps2.c:358). NOTE: lcms2 emits a wall-clock `ctime()` line, so
/// this path is NOT byte-deterministic (and is only reached without
/// `NODEFAULTRESOURCEDEF`). tintbox emits a placeholder timestamp; callers needing
/// byte-identity must set `NODEFAULTRESOURCEDEF`.
fn emit_header(m: &mut Ps, title: &str, profile: &Profile) {
    let desc = read_ascii_tag(profile, *b"desc");
    let cprt = read_ascii_tag(profile, *b"cprt");
    m.s("%!PS-Adobe-3.0\n");
    m.s("%\n");
    m.s(&format!("% {title}\n"));
    m.s(&format!("% Source: {}\n", remove_cr(&desc)));
    m.s(&format!("%         {}\n", remove_cr(&cprt)));
    // ctime() — non-deterministic; placeholder.
    m.s("% Created: (timestamp)\n");
    m.s("%\n");
    m.s("%%BeginResource\n");
}

/// `RemoveCR` (cmsps2.c:317): replace CR/LF with spaces, truncated at 2047 bytes.
fn remove_cr(txt: &str) -> String {
    let mut s: String = txt.chars().take(2047).collect();
    s = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    s
}

/// `cmsMLUgetASCII(cmsReadTag(sig), cmsNoLanguage, cmsNoCountry, .., 255)`.
fn read_ascii_tag(profile: &Profile, sig: [u8; 4]) -> String {
    use crate::profile::Tag;
    match profile.read_tag(crate::sig::Signature::from_bytes(sig)) {
        Ok(Tag::Mlu(mlu)) => mlu.preferred_ascii(),
        _ => String::new(),
    }
}

/// `_cmsReadMediaWhitePoint` (cmsio1.c:64): the `wtpt` tag, with two D50
/// overrides — no tag at all, or a V2 Display-class profile (whose stored white is
/// conventionally the adapted D50).
fn read_media_white_point(profile: &Profile) -> CIEXYZ {
    use crate::profile::Tag;
    let tag = match profile.read_tag(crate::sig::Signature::from_bytes(*b"wtpt")) {
        Ok(Tag::Xyz(xyz)) => xyz,
        _ => return D50, // No wp -> D50.
    };

    // V2 display profiles should give D50.
    let header = profile.header();
    if header.version < 0x0400_0000 && header.device_class == ProfileClass::Display {
        return D50;
    }
    tag
}

// ---- virtual profile helpers ------------------------------------------------

fn build_lab4_profile() -> Result<Profile<'static>> {
    Profile::from_writable(&crate::profile::virtuals::build_lab4_profile())
}

fn build_xyz_profile() -> Result<Profile<'static>> {
    Profile::from_writable(&crate::profile::virtuals::build_xyz_profile())
}

// ===========================================================================
//  Public API
// ===========================================================================

/// lcms2 `cmsGetPostScriptCSA` (Color Space Array): emit the PostScript CSA for
/// `profile` at `intent` (a raw lcms2 intent 0..3) with `flags`. Returns the
/// emitted bytes, or an error where lcms2 returns a zero byte count.
pub fn get_post_script_csa(profile: &Profile, intent: u32, _flags: u32) -> Result<Vec<u8>> {
    generate_csa(profile, intent)
}

/// lcms2 `cmsGetPostScriptCRD` (Color Rendering Dictionary): emit the PostScript
/// CRD for `profile` at `intent` with `flags`. Set
/// [`Flags::NODEFAULTRESOURCEDEF`](Flags) (raw `0x0100_0000`) for deterministic
/// (header-free) output.
pub fn get_post_script_crd(profile: &Profile, intent: u32, flags: u32) -> Result<Vec<u8>> {
    generate_crd(profile, intent, flags)
}
