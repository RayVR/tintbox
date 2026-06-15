//! Bit-identical BATCHED (tiled) general-pipeline evaluator for
//! [`AccurateFast`](super::OptimizationStrategy::AccurateFast).
//!
//! This is the lossless speedup for the GENERAL (CLUT) path — the RGB→CMYK /
//! CMYK→CMYK device links that the matrix-shaper fast path
//! ([`LosslessMatShaper`](super::lossless_matshaper::LosslessMatShaper)) does not
//! cover. It produces **byte-for-byte** the same output as the per-pixel
//! [`Pipeline::eval_16`](crate::pipeline::Pipeline::eval_16) /
//! [`Pipeline::eval_float`](crate::pipeline::Pipeline::eval_float), but removes
//! three per-pixel costs:
//!
//! 1. **Per-pixel `Vec` allocation + dynamic stage dispatch.** The accurate path
//!    calls `eval_16`/`eval_float` once per pixel, each returning a freshly
//!    allocated `Vec`. This evaluator processes a CHUNK of pixels stage-by-stage
//!    (stage-outer / pixel-inner) into reusable scratch buffers — per-CHUNK
//!    ping-pong, not per-pixel — so the inner loops are tight fixed-stride and the
//!    intermediates stay cache-resident.
//! 2. **Per-pixel `interp_factory(..)`.** The CLUT interpolator
//!    ([`Interp16`]/[`InterpFloat`]) is resolved ONCE at build time from the grid
//!    geometry (a pure function of the geometry — so caching it is trivially
//!    bit-identical) instead of being re-derived on every pixel
//!    ([`clut.rs`](crate::pipeline::clut) `interp_factory` call).
//! 3. **Per-pixel input-curve eval for 8-bit input.** When the FIRST stage is a
//!    `ToneCurves` set and the packed input is 8-bit, each channel's curve is
//!    memoized into a 256-entry table indexed by the input byte — exactly as
//!    Task 1's [`LosslessMatShaper`](super::lossless_matshaper) does, with the
//!    identical input scaling (`from_8_to_16(byte) as f32 / 65535.0` then the
//!    curve). Pure memoization → bit-identical.
//!
//! # Bit-identity contract
//!
//! Each pixel is computed INDEPENDENTLY with the EXACT same operations, in the
//! same order, as the per-pixel eval — only the loop NESTING is reordered
//! (stage-outer/pixel-inner). There is no cross-pixel arithmetic and no reduction
//! across pixels. Every float↔int conversion at a stage boundary is reproduced
//! verbatim:
//!
//! - 16-bit input: `From16ToFloat` (`u16 as f32 / 65535.0`) at entry,
//!   `FromFloatTo16` (`quick_saturate_word(f32 as f64 * 65535.0)`) at exit, exactly
//!   as `eval_16`.
//! - Float input: straight `f32` copy at entry/exit, exactly as `eval_float`.
//! - Every stage is evaluated by the UNCHANGED [`Stage::eval`] (the CLUT included:
//!   its U16 path quantizes f32→u16 via `FromFloatTo16`, runs the resolved 16-bit
//!   interpolator, and widens back — that intentional quantization is preserved).
//!   The cached interpolator is plugged in by reconstructing the SAME `Clut`
//!   evaluation the per-pixel path runs; we never drop a conversion.
//!
//! The only thing that changes versus the accurate path is *where the bytes come
//! from* (cached LUT/interpolator) and *the loop order* — never the arithmetic.

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::fixed::from_8_to_16;
use crate::format::decode::PixelFormat;
use crate::interp::{interp_factory, InterpFn, InterpParams};
use crate::pipeline::{Clut, ClutTable, Pipeline, Stage, MAX_STAGE_CHANNELS};

/// How many pixels are processed per tile. Sized so the two ping-pong scratch
/// buffers (`CHUNK * MAX_STAGE_CHANNELS` f32 each = 8192 * 128 * 4 B = 4 MiB ...)
/// — actually the scratch is `CHUNK * width`, and the widths in play are small
/// (3/4 channels), so the working set is a few hundred KiB, staying L2-resident.
const CHUNK: usize = 8192;

/// A single stage, pre-resolved for batched evaluation. Every arm reproduces the
/// EXACT per-pixel arithmetic of [`Stage::eval`]; the only difference is that
/// build-time-resolvable work (interpolator selection) is hoisted out of the
/// per-pixel loop.
#[derive(Clone)]
enum BatchedStage {
    /// A CLUT stage with the interpolator resolved once at build time. The
    /// per-pixel evaluation is reconstructed to match [`Clut::eval`] byte-for-byte
    /// (U16 path: `FromFloatTo16` → cached `Interp16` → `From16ToFloat`; F32 path:
    /// cached `InterpFloat` directly).
    Clut(ResolvedClut),
    /// Any other stage: evaluated by the UNCHANGED [`Stage::eval`] per pixel. We
    /// keep the original `Stage` so the arithmetic is literally the same code.
    Generic(Stage),
}

/// A CLUT stage with its interpolator resolved at build time. Holds an owned copy
/// of the grid table + [`InterpParams`] and the resolved builtin interpolator. A
/// CLUT carrying a custom plugin interpolator is NOT resolved here (it stays a
/// [`BatchedStage::Generic`] so [`Clut::eval`]'s custom-interp branch runs).
#[derive(Clone)]
struct ResolvedClut {
    table: ClutTable,
    params: InterpParams,
    n_in: usize,
    n_out: usize,
    /// The resolved builtin interpolator (`Lerp16` for a U16 table, `LerpFloat`
    /// for an F32 table). Resolved once from `interp_factory(n_in, n_out,
    /// is_float, is_trilinear)` — a pure function of the geometry.
    interp: InterpFn,
}

impl ResolvedClut {
    /// Evaluate one pixel exactly as [`Clut::eval`] does, using the cached
    /// interpolator. `input`/`output` are the float-domain channels.
    #[inline]
    fn eval(&self, input: &[f32], output: &mut [f32]) {
        let n_in = self.n_in;
        let n_out = self.n_out;
        match (&self.table, &self.interp) {
            (ClutTable::U16(table), InterpFn::Lerp16(lerp16)) => {
                // EvaluateCLUTfloatIn16: FromFloatTo16 -> Lerp16 -> From16ToFloat,
                // identical to the `Clut::eval` U16 arm.
                let mut in16 = [0u16; MAX_STAGE_CHANNELS];
                for i in 0..n_in {
                    in16[i] = Lcms2Floor::quick_saturate_word(input[i] as f64 * 65535.0);
                }
                let mut out16 = [0u16; MAX_STAGE_CHANNELS];
                lerp16.eval(&in16[..n_in], &mut out16[..n_out], table, &self.params);
                for i in 0..n_out {
                    output[i] = out16[i] as f32 / 65535.0_f32;
                }
            }
            (ClutTable::F32(table), InterpFn::LerpFloat(lerpf)) => {
                // EvaluateCLUTfloat: the float interpolator runs directly.
                lerpf.eval(&input[..n_in], &mut output[..n_out], table, &self.params);
            }
            // The interpolator domain always matches the table (resolved with the
            // table's `is_float`), so the other pairings never occur.
            _ => unreachable!("resolved interpolator domain matches the CLUT table"),
        }
    }
}

/// The batched general-pipeline evaluator: a list of pre-resolved stages plus the
/// pipeline's in/out widths. Built by [`try_optimize`]; evaluates a CHUNK of
/// pixels stage-by-stage. Byte-for-byte identical to the per-pixel
/// `eval_16`/`eval_float`.
#[derive(Clone)]
pub struct BatchedPipeline {
    stages: Vec<BatchedStage>,
    in_ch: usize,
    out_ch: usize,
    /// When the input is 8-bit AND the first stage is `ToneCurves`, the per-channel
    /// 256-entry LUTs memoizing that stage's output for each input byte:
    /// `in_lut[ch][byte] = curve[ch].eval_float(from_8_to_16(byte) as f32 /
    /// 65535.0)`. Used ONLY by [`Self::eval_16_buffer`] (where the 8-bit formatter
    /// produces `win = (byte<<8)|byte`, so `win & 0xff` recovers the index); it
    /// fuses the entry conversion + first stage. `eval_float_buffer` ignores it
    /// (the float 8-bit formatter produces a different float domain — it must run
    /// the first `ToneCurves` stage generically), so `stages[0]` is ALWAYS kept as
    /// the generic first stage too. `None` when the first stage is not an 8-bit
    /// tone-curve set.
    input_curve_luts: Option<Vec<[f32; 256]>>,
}

impl BatchedPipeline {
    /// Number of input channels (matches the pipeline).
    #[must_use]
    pub fn input_channels(&self) -> usize {
        self.in_ch
    }

    /// Number of output channels (matches the pipeline).
    #[must_use]
    pub fn output_channels(&self) -> usize {
        self.out_ch
    }

    /// Evaluate `n` pixels of 16-bit (`win`-domain) input, writing `n` pixels of
    /// 16-bit output. Byte-for-byte identical to calling
    /// [`Pipeline::eval_16`](crate::pipeline::Pipeline::eval_16) per pixel.
    ///
    /// `input` is `n * in_ch` u16 (each channel as produced by the input
    /// formatter: an 8-bit byte arrives as `win = (byte<<8)|byte`, so the low byte
    /// indexes an `InputCurves8` LUT). `output` is `n * out_ch` u16.
    pub fn eval_16_buffer(&self, input: &[u16], output: &mut [u16], n: usize) {
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;

        // Two ping-pong scratch buffers, each holding CHUNK pixels at the working
        // width. Allocated once per call (not per pixel).
        let mut buf_a = vec![0.0f32; CHUNK * MAX_STAGE_CHANNELS];
        let mut buf_b = vec![0.0f32; CHUNK * MAX_STAGE_CHANNELS];

        let mut base = 0usize;
        while base < n {
            let m = (n - base).min(CHUNK);

            // ---- Entry: convert this chunk's input into buf_a (pixel-major). ----
            // When the first stage is an 8-bit input-curve LUT, the entry + first
            // stage are fused: index the LUT by the input byte directly. Otherwise
            // From16ToFloat (u16 as f32 / 65535.0), exactly as eval_16.
            let mut cur = &mut buf_a;
            let mut nxt = &mut buf_b;
            let mut width = in_ch;
            let mut start_stage = 0usize;

            if let Some(luts) = &self.input_curve_luts {
                // Fuse the entry conversion + first ToneCurves stage: index each
                // channel's LUT by the input byte (win = (byte<<8)|byte, low byte
                // is the index). Bit-identical to From16ToFloat then the first
                // ToneCurves stage. ToneCurves preserves channel count.
                for p in 0..m {
                    let inp = &input[(base + p) * in_ch..(base + p) * in_ch + in_ch];
                    let out = &mut cur[p * in_ch..p * in_ch + in_ch];
                    for ch in 0..in_ch {
                        out[ch] = luts[ch][(inp[ch] & 0xff) as usize];
                    }
                }
                start_stage = 1; // stages[0] (the generic ToneCurves) is fused in.
            } else {
                // From16ToFloat (u16 as f32 / 65535.0), exactly as eval_16.
                for p in 0..m {
                    let inp = &input[(base + p) * in_ch..(base + p) * in_ch + in_ch];
                    let out = &mut cur[p * in_ch..p * in_ch + in_ch];
                    for ch in 0..in_ch {
                        out[ch] = inp[ch] as f32 / 65535.0_f32;
                    }
                }
            }

            // ---- Stages: stage-outer, pixel-inner. ----
            for stage in &self.stages[start_stage..] {
                let stage_out = stage.output_width(width);
                run_stage(stage, cur, nxt, m, width, stage_out);
                core::mem::swap(&mut cur, &mut nxt);
                width = stage_out;
            }

            // ---- Exit: FromFloatTo16 (f32 as f64 * 65535.0 saturated). ----
            for p in 0..m {
                let src = &cur[p * out_ch..p * out_ch + out_ch];
                let dst = &mut output[(base + p) * out_ch..(base + p) * out_ch + out_ch];
                for ch in 0..out_ch {
                    dst[ch] = Lcms2Floor::quick_saturate_word(src[ch] as f64 * 65535.0);
                }
            }

            base += m;
        }
    }

    /// Evaluate `n` pixels of float input, writing `n` pixels of float output.
    /// Byte-for-byte identical to calling
    /// [`Pipeline::eval_float`](crate::pipeline::Pipeline::eval_float) per pixel.
    ///
    /// `input` is `n * in_ch` f32; `output` is `n * out_ch` f32.
    pub fn eval_float_buffer(&self, input: &[f32], output: &mut [f32], n: usize) {
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;

        let mut buf_a = vec![0.0f32; CHUNK * MAX_STAGE_CHANNELS];
        let mut buf_b = vec![0.0f32; CHUNK * MAX_STAGE_CHANNELS];

        let mut base = 0usize;
        while base < n {
            let m = (n - base).min(CHUNK);

            let mut cur = &mut buf_a;
            let mut nxt = &mut buf_b;
            let mut width = in_ch;

            // Entry: straight f32 copy (eval_float's memmove of input_channels).
            for p in 0..m {
                let inp = &input[(base + p) * in_ch..(base + p) * in_ch + in_ch];
                cur[p * in_ch..p * in_ch + in_ch].copy_from_slice(inp);
            }

            for stage in &self.stages {
                let stage_out = stage.output_width(width);
                run_stage(stage, cur, nxt, m, width, stage_out);
                core::mem::swap(&mut cur, &mut nxt);
                width = stage_out;
            }

            // Exit: straight f32 copy.
            for p in 0..m {
                let src = &cur[p * out_ch..p * out_ch + out_ch];
                output[(base + p) * out_ch..(base + p) * out_ch + out_ch].copy_from_slice(src);
            }

            base += m;
        }
    }
}

impl BatchedStage {
    /// The stage's output channel width given its input width.
    #[inline]
    fn output_width(&self, _in_width: usize) -> usize {
        match self {
            BatchedStage::Clut(c) => c.n_out,
            BatchedStage::Generic(s) => s.output_channels(),
        }
    }
}

/// Run one stage over `m` pixels: read `in_width` channels per pixel from `cur`,
/// write `out_width` channels per pixel into `nxt` (pixel-major layout).
#[inline]
fn run_stage(
    stage: &BatchedStage,
    cur: &[f32],
    nxt: &mut [f32],
    m: usize,
    in_width: usize,
    out_width: usize,
) {
    match stage {
        BatchedStage::Clut(c) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                c.eval(src, dst);
            }
        }
        BatchedStage::Generic(s) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                s.eval(src, dst);
            }
        }
    }
}

/// Build the input-curve LUTs for an 8-bit-input `ToneCurves` first stage. Each
/// entry is `curve[ch].eval_float(from_8_to_16(byte) as f32 / 65535.0)` — the
/// EXACT value the stage produces for that input byte under the pipeline path
/// (mirrors `LosslessMatShaper::build_in_lut`).
fn build_input_curve_luts(curves: &[crate::curve::ToneCurve]) -> Vec<[f32; 256]> {
    let mut luts = vec![[0.0f32; 256]; curves.len()];
    for (ch, curve) in curves.iter().enumerate() {
        for byte in 0u16..=255 {
            let win = from_8_to_16(byte as u8);
            let r = win as f32 / 65535.0_f32;
            luts[ch][byte as usize] = curve.eval_float(r);
        }
    }
    luts
}

/// Resolve a CLUT into a [`ResolvedClut`] with its builtin interpolator cached, or
/// `None` if the CLUT carries a custom plugin interpolator (which must keep going
/// through [`Clut::eval`] so the plugin's closure runs). The interpolator is
/// resolved by the SAME `interp_factory(n_in, n_out, is_float, is_trilinear)` the
/// per-pixel path uses — a pure function of the geometry.
fn resolve_clut(clut: &Clut) -> Option<ResolvedClut> {
    // A CLUT with a resolved CUSTOM interpolator is not specialized here; its
    // `resolved` slot carries an `InterpFn::Custom`, which only `Clut::eval` knows
    // how to dispatch. Leave it Generic so the per-pixel `Clut::eval` runs it.
    if clut.has_custom_interp() {
        return None;
    }
    let n_in = clut.params.n_inputs;
    let n_out = clut.params.n_outputs;
    let is_float = matches!(clut.table, ClutTable::F32(_));
    let interp = interp_factory(n_in, n_out, is_float, clut.is_trilinear);
    Some(ResolvedClut {
        table: clut.table.clone(),
        params: clut.params.clone(),
        n_in,
        n_out,
        interp,
    })
}

/// Detect a general/CLUT pipeline that the batched evaluator can accelerate and
/// build a [`BatchedPipeline`]. Returns `None` (so the caller falls back to the
/// in-place pipeline eval) for the matrix-shaper shape Task 1 already handles, or
/// when the pipeline is empty / a width exceeds [`MAX_STAGE_CHANNELS`].
///
/// `in_fmt` is consulted only to decide whether the FIRST `ToneCurves` stage can
/// be memoized into an 8-bit input-curve LUT (input is 8-bit, not float). Every
/// other stage is evaluated by the unchanged [`Stage::eval`] / cached
/// interpolator, so the result is byte-identical to the accurate path regardless
/// of `in_fmt`.
pub fn try_optimize(lut: &Pipeline, in_fmt: u32, _out_fmt: u32) -> Option<BatchedPipeline> {
    let stages = lut.stages();
    if stages.is_empty() {
        // An empty pipeline is a trivial copy; let the in-place eval handle it.
        return None;
    }

    let inf = PixelFormat(in_fmt);
    // 8-bit (1 byte, non-float) input lets us memoize the first ToneCurves stage.
    let input_8bit = inf.bytes() == 1 && !inf.is_float();

    let mut batched = Vec::with_capacity(stages.len());
    let mut input_curve_luts = None;

    for (idx, stage) in stages.iter().enumerate() {
        // Reject any stage that would overflow the scratch width.
        if stage.input_channels() > MAX_STAGE_CHANNELS
            || stage.output_channels() > MAX_STAGE_CHANNELS
        {
            return None;
        }

        // First stage + 8-bit input: memoize the tone curves into per-channel byte
        // LUTs for the 16-bit eval path. The stage is ALSO kept generic (below) so
        // the float eval path — where the 8-bit float formatter produces a
        // different float domain — runs it the normal way.
        if let Stage::ToneCurves(curves) = stage {
            if idx == 0 && input_8bit {
                input_curve_luts = Some(build_input_curve_luts(curves));
            }
        }

        let bs = match stage {
            Stage::Clut(clut) => match resolve_clut(clut) {
                Some(rc) => BatchedStage::Clut(rc),
                // Custom plugin interpolator: keep the generic Stage::eval path.
                None => BatchedStage::Generic(stage.clone()),
            },
            _ => BatchedStage::Generic(stage.clone()),
        };
        batched.push(bs);
    }

    Some(BatchedPipeline {
        stages: batched,
        in_ch: lut.input_channels,
        out_ch: lut.output_channels,
        input_curve_luts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::build_gamma;
    use crate::format::decode::{TYPE_CMYK_16, TYPE_CMYK_FLT, TYPE_RGB_8};
    use crate::interp::InterpParams;
    use crate::pipeline::{Clut, ClutTable, ResolvedInterp};

    /// A tiny 2x2x2x2 RGB->CMYK-ish CLUT pipeline (Curves -> CLUT) for testing.
    fn rgb_clut_pipeline() -> Pipeline {
        let mut p = Pipeline::new(3, 4);
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(2.2),
            build_gamma(2.2),
            build_gamma(2.2),
        ]))
        .unwrap();
        // 3-input, 4-output CLUT with 2 samples per axis (8 nodes * 4 out = 32).
        let n_samples = [2u32, 2, 2];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0u16; 8 * 4];
        // Fill with a deterministic non-trivial pattern.
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 4099 + 137) & 0xffff) as u16;
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();
        p
    }

    /// The batched 16-bit eval must equal the per-pixel `eval_16` bit-for-bit.
    #[test]
    fn batched_16_equals_eval_16() {
        let p = rgb_clut_pipeline();
        let batched = try_optimize(&p, TYPE_RGB_8, TYPE_CMYK_16).expect("batched built");

        // Build an 8-bit-style input buffer (win = (byte<<8)|byte for each chan).
        let mut input = Vec::new();
        let vals: Vec<u8> = (0u8..=255).step_by(7).collect();
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    input.push(from_8_to_16(r));
                    input.push(from_8_to_16(g));
                    input.push(from_8_to_16(b));
                }
            }
        }
        let n = input.len() / 3;
        let mut batched_out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut batched_out, n);

        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            let expect = p.eval_16(win);
            assert_eq!(
                &batched_out[i * 4..i * 4 + 4],
                &expect[..],
                "pixel {i} win={win:?} batched != eval_16"
            );
        }
    }

    /// The batched 16-bit eval with 16-bit (non-8-bit) input — the input-curve LUT
    /// is NOT used (the first stage runs generically) — still equals `eval_16`.
    #[test]
    fn batched_16_equals_eval_16_non_8bit_input() {
        let p = rgb_clut_pipeline();
        let batched = try_optimize(&p, TYPE_CMYK_16, TYPE_CMYK_16).expect("batched built");
        assert!(
            batched.input_curve_luts.is_none(),
            "16-bit input must not specialize the first curve stage"
        );

        let mut input = Vec::new();
        for r in (0u16..=65535).step_by(4369) {
            for g in (0u16..=65535).step_by(8738) {
                for b in (0u16..=65535).step_by(13107) {
                    input.push(r);
                    input.push(g);
                    input.push(b);
                }
            }
        }
        let n = input.len() / 3;
        let mut batched_out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut batched_out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            let expect = p.eval_16(win);
            assert_eq!(&batched_out[i * 4..i * 4 + 4], &expect[..], "pixel {i}");
        }
    }

    /// The batched float eval must equal the per-pixel `eval_float` bit-for-bit.
    #[test]
    fn batched_float_equals_eval_float() {
        // Float CLUT pipeline.
        let mut p = Pipeline::new(3, 4);
        let n_samples = [2u32, 2, 2];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0.0f32; 8 * 4];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i as f32) * 0.013 + 0.01).fract();
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::F32(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();

        let batched = try_optimize(&p, TYPE_CMYK_FLT, TYPE_CMYK_FLT).expect("batched built");

        let mut input = Vec::new();
        let vals = [0.0f32, 0.1, 0.25, 0.5, 0.7, 0.9, 1.0];
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    input.push(r);
                    input.push(g);
                    input.push(b);
                }
            }
        }
        let n = input.len() / 3;
        let mut batched_out = vec![0.0f32; n * 4];
        batched.eval_float_buffer(&input, &mut batched_out, n);
        for i in 0..n {
            let pix = &input[i * 3..i * 3 + 3];
            let expect = p.eval_float(pix);
            for c in 0..4 {
                assert_eq!(
                    batched_out[i * 4 + c].to_bits(),
                    expect[c].to_bits(),
                    "pixel {i} chan {c} batched != eval_float"
                );
            }
        }
    }

    /// A non-chunk-aligned pixel count (forces a partial final tile).
    #[test]
    fn batched_handles_partial_final_chunk() {
        let p = rgb_clut_pipeline();
        let batched = try_optimize(&p, TYPE_RGB_8, TYPE_CMYK_16).expect("batched built");
        let n = CHUNK + 37; // one full chunk + a partial.
        let mut input = vec![0u16; n * 3];
        for (i, slot) in input.iter_mut().enumerate() {
            let byte = ((i * 31 + 7) & 0xff) as u8;
            *slot = from_8_to_16(byte);
        }
        let mut batched_out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut batched_out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            let expect = p.eval_16(win);
            assert_eq!(&batched_out[i * 4..i * 4 + 4], &expect[..], "pixel {i}");
        }
    }
}
