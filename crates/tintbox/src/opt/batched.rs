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
use crate::context::Context;
use crate::curve::ToneCurve;
use crate::fixed::from_8_to_16;
use crate::format::decode::PixelFormat;
#[cfg(not(feature = "simd"))]
use crate::interp::tetrahedral_16;
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
    /// A maximal run of consecutive Curve / `ClutTable::U16`-CLUT stages whose run
    /// boundaries are 16-bit quantization points, fused into one u16-domain kernel
    /// (the BIG lossless lever). Used ONLY by the 16-bit eval
    /// ([`BatchedPipeline::eval_16_buffer`]); the float eval treats the run's
    /// member stages generically (it walks `BatchedPipeline::stages_float`). The run
    /// reads f32 (recovering the boundary u16 via `quick_saturate_word(in*65535)`),
    /// processes wholly in u16 with no per-pixel f32↔int conversion, and writes f32
    /// (`u16/65535`). Both edge conversions reproduce the float path exactly because
    /// every run boundary is a quantization point (round-trip identity holds).
    U16Run(U16Run),
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
                // SIMD: the 3-input tetrahedral runs its per-output-channel interp
                // across i32x4 lanes (bit-identical integer math); other 16-bit
                // interpolators stay scalar.
                #[cfg(feature = "simd")]
                if let InterpFn::Lerp16(crate::interp::Interp16::Tetrahedral) = &self.interp {
                    crate::simd::tetrahedral_across_outputs(
                        &in16[..n_in],
                        &mut out16[..n_out],
                        table,
                        &self.params,
                    );
                } else {
                    lerp16.eval(&in16[..n_in], &mut out16[..n_out], table, &self.params);
                }
                #[cfg(not(feature = "simd"))]
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

/// A per-channel `u16 -> u16` curve table memoizing a [`Stage::ToneCurves`]
/// element under the 16-bit eval path. `table[ch][win]` is the EXACT u16 the
/// float path produces at the stage's *output* quantization point for input word
/// `win`: the entry/inter-stage `From16ToFloat` (`win as f32 / 65535.0`), the
/// curve (`eval_float`), and the consumer's `FromFloatTo16`
/// (`quick_saturate_word(.. as f64 * 65535.0)`) folded into one lookup.
///
/// This is valid ONLY when every value crossing the stage boundary is a 16-bit
/// quantization point — i.e. the curve's neighbours are themselves u16 boundaries
/// (the chain entry, the 16-bit exit, or a `ClutTable::U16` whose float in/out are
/// `quick_saturate_word(.. * 65535.0)` / `out16 as f32 / 65535.0`). [`U16Chain`]
/// only forms when the whole pipeline is such a Curve/U16-CLUT chain, so the fold
/// is a pure memoization of the float path and therefore byte-identical.
#[derive(Clone)]
struct U16Curves {
    /// One 65536-entry table per channel (`Box` to keep the stage cheap to move).
    tables: Vec<Box<[u16; 65536]>>,
}

impl U16Curves {
    /// Apply the per-channel tables to `src` (width = channel count), writing the
    /// looked-up output words into `dst`.
    #[inline]
    fn apply(&self, src: &[u16], dst: &mut [u16]) {
        for (ch, t) in self.tables.iter().enumerate() {
            dst[ch] = t[src[ch] as usize];
        }
    }
}

/// One stage of a pure u16-domain [`U16Chain`]: either a memoized per-channel
/// curve table or a `ClutTable::U16` evaluated directly in the u16 domain (the
/// tetrahedral / n-D interpolator runs on the u16 grid words with no f32
/// round-trip). Both reproduce the float path's bytes exactly (see [`U16Curves`]).
#[derive(Clone)]
enum U16Stage {
    Curves(U16Curves),
    Clut(ResolvedClut),
}

impl U16Stage {
    #[inline]
    fn out_width(&self) -> usize {
        match self {
            U16Stage::Curves(c) => c.tables.len(),
            U16Stage::Clut(c) => c.n_out,
        }
    }
}

/// A maximal run of consecutive Curve / `ClutTable::U16`-CLUT stages, evaluated
/// WHOLLY in the u16 domain: curve stages are per-channel table lookups and CLUT
/// stages run their u16 interpolator directly on the grid words. The run abuts the
/// rest of the (f32) batched pipeline through its `in_width`/`out_width` channels.
///
/// # Bit-identity
///
/// The run is formed (by [`split_u16_runs`]) only when BOTH its edges are 16-bit
/// quantization points: a U16-CLUT's input (it applies `FromFloatTo16` on read) /
/// output (`out16 as f32 / 65535.0`), the pipeline's 16-bit entry/exit. At such a
/// point the f32 value equals `w / 65535.0` for some u16 `w`, and the round-trip
/// `FromFloatTo16(w / 65535.0) == w` is the identity (proven by
/// [`entry_word_roundtrip_is_identity`]). So:
///
/// - **Entry** (`f32 -> u16`): `quick_saturate_word(in * 65535.0)` recovers exactly
///   the u16 the run's first stage consumes — matching the float path, where a
///   leading U16-CLUT does the same `FromFloatTo16` and a leading curve reads the
///   same `w / 65535.0`.
/// - **Internal boundaries**: every member's output IS a u16 (curve table value or
///   CLUT grid word); the next member consumes it directly, and the curve tables
///   fold the surrounding `From16ToFloat`/`eval_float`/`FromFloatTo16` into one
///   lookup — exactly the float-path bytes.
/// - **Exit** (`u16 -> f32`): widen `w / 65535.0`. The downstream consumer (a
///   U16-CLUT's `FromFloatTo16`, or the 16-bit exit's `FromFloatTo16`) re-quantizes
///   it to `w` — identical to the float path's value at that quantization point.
///
/// The result is byte-for-byte identical to the f32 stage-by-stage eval, with zero
/// per-pixel f32↔int conversions INSIDE the run.
#[derive(Clone)]
struct U16Run {
    stages: Vec<U16Stage>,
    in_width: usize,
    out_width: usize,
}

impl U16Run {
    /// Run this fused u16 sub-chain over `m` pixels, reading f32 from `cur`
    /// (pixel-major, `in_width` channels) and writing f32 to `nxt` (`out_width`).
    fn eval(&self, cur: &[f32], nxt: &mut [f32], m: usize, scratch: &mut U16RunScratch) {
        let in_w = self.in_width;
        let out_w = self.out_width;

        // Entry: recover the boundary u16 for each input channel.
        for p in 0..m {
            let src = &cur[p * in_w..p * in_w + in_w];
            let dst = &mut scratch.a[p * in_w..p * in_w + in_w];
            for ch in 0..in_w {
                dst[ch] = Lcms2Floor::quick_saturate_word(src[ch] as f64 * 65535.0);
            }
        }

        let mut width = in_w;
        let mut from_a = true;
        for stage in &self.stages {
            let stage_out = stage.out_width();
            if from_a {
                let (a, b) = (&scratch.a, &mut scratch.b);
                run_u16_stage(stage, a, b, m, width, stage_out);
            } else {
                let (b, a) = (&scratch.b, &mut scratch.a);
                run_u16_stage(stage, b, a, m, width, stage_out);
            }
            from_a = !from_a;
            width = stage_out;
        }

        // Exit: widen the final u16 words to f32 (`w / 65535.0`).
        let final_buf = if from_a { &scratch.a } else { &scratch.b };
        for p in 0..m {
            let src = &final_buf[p * out_w..p * out_w + out_w];
            let dst = &mut nxt[p * out_w..p * out_w + out_w];
            for ch in 0..out_w {
                dst[ch] = src[ch] as f32 / 65535.0_f32;
            }
        }
    }
}

/// Reusable u16 ping-pong scratch for [`U16Run::eval`] (allocated once per call to
/// the 16-bit batched eval, reused across runs and chunks).
struct U16RunScratch {
    a: Vec<u16>,
    b: Vec<u16>,
}

impl U16RunScratch {
    fn with_capacity(cap_pixels: usize) -> Self {
        U16RunScratch {
            a: vec![0u16; cap_pixels * MAX_STAGE_CHANNELS],
            b: vec![0u16; cap_pixels * MAX_STAGE_CHANNELS],
        }
    }
}

/// Reusable scratch for a whole batched eval: the f32 ping-pong pair plus the
/// u16-run scratch. Allocated ONCE per `do_transform` call (in
/// [`Transform::do_transform_batched`](crate::transform)) and reused across every
/// tile — the per-tile `vec![0; CHUNK*MAX_STAGE_CHANNELS]` allocation+zeroing
/// (`__bzero`) is the hot waste this removes. Every slot a stage reads is written
/// by the prior stage (or the entry conversion) before being read, so the buffers
/// carrying stale data across tiles is harmless: the eval never reads an
/// un-rewritten slot.
pub struct BatchedScratch {
    buf_a: Vec<f32>,
    buf_b: Vec<f32>,
    u16: U16RunScratch,
    /// The eval's internal tile width in PIXELS: `min(CHUNK, cap_pixels)`. The eval
    /// must never process more than `cap_pixels` pixels per inner tile, since the
    /// scratch buffers only hold `cap_pixels * MAX_STAGE_CHANNELS` slots.
    cap_pixels: usize,
}

impl BatchedScratch {
    /// Allocate the ping-pong + u16-run scratch once, sized for one tile of
    /// `cap_pixels` pixels at the maximum stage width. `cap_pixels` is clamped to
    /// `CHUNK` (the eval's tile cap) and to at least 1. A SMALL `do_transform` call
    /// (e.g. one scanline) sizes the scratch to its own pixel count, so it never
    /// allocates+zeros a full `CHUNK`-wide (megabyte) buffer — the per-call overhead
    /// that made the batched path catastrophic for small calls. Reuse the same value
    /// across all of a transform's tiles to avoid re-zeroing per tile.
    #[must_use]
    pub fn with_capacity(cap_pixels: usize) -> Self {
        let cap = cap_pixels.clamp(1, CHUNK);
        BatchedScratch {
            buf_a: vec![0.0f32; cap * MAX_STAGE_CHANNELS],
            buf_b: vec![0.0f32; cap * MAX_STAGE_CHANNELS],
            u16: U16RunScratch::with_capacity(cap),
            cap_pixels: cap,
        }
    }

    /// Allocate scratch sized for a full `CHUNK` tile (the steady-state large-buffer
    /// case). Equivalent to `with_capacity(CHUNK)`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(CHUNK)
    }
}

impl Default for BatchedScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one u16-domain stage over `m` pixels (pixel-major layout).
#[inline]
fn run_u16_stage(
    stage: &U16Stage,
    cur: &[u16],
    nxt: &mut [u16],
    m: usize,
    in_width: usize,
    out_width: usize,
) {
    match stage {
        U16Stage::Curves(c) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                c.apply(src, dst);
            }
        }
        U16Stage::Clut(c) => match (&c.table, &c.interp) {
            // 3-input tetrahedral is the dominant print shape; call it directly so
            // the inner loop has fixed small bounds (no 128-wide overhead).
            (ClutTable::U16(table), InterpFn::Lerp16(crate::interp::Interp16::Tetrahedral)) => {
                for p in 0..m {
                    let src = &cur[p * in_width..p * in_width + in_width];
                    let dst = &mut nxt[p * out_width..p * out_width + out_width];
                    // SIMD: vectorize the per-output-channel interp across i32x4
                    // lanes — bit-identical (integer math is exact). Scalar
                    // otherwise.
                    #[cfg(feature = "simd")]
                    crate::simd::tetrahedral_across_outputs(
                        &src[..3],
                        &mut dst[..out_width],
                        table,
                        &c.params,
                    );
                    #[cfg(not(feature = "simd"))]
                    tetrahedral_16(&src[..3], &mut dst[..out_width], table, &c.params);
                }
            }
            (ClutTable::U16(table), InterpFn::Lerp16(lerp16)) => {
                for p in 0..m {
                    let src = &cur[p * in_width..p * in_width + in_width];
                    let dst = &mut nxt[p * out_width..p * out_width + out_width];
                    lerp16.eval(&src[..in_width], &mut dst[..out_width], table, &c.params);
                }
            }
            // A U16Run only ever holds u16 tables with u16 interpolators.
            _ => unreachable!("U16Run holds only ClutTable::U16 stages"),
        },
    }
}

/// Build the per-channel u16 curve table for a [`Stage::ToneCurves`] element at a
/// u16 quantization boundary: `table[ch][win] = FromFloatTo16(curve.eval_float(
/// From16ToFloat(win)))`, the EXACT fold of the surrounding float conversions.
fn build_u16_curve_tables(curves: &[ToneCurve]) -> U16Curves {
    let mut tables: Vec<Box<[u16; 65536]>> = Vec::with_capacity(curves.len());
    for curve in curves {
        let mut t = Box::new([0u16; 65536]);
        for (win, slot) in t.iter_mut().enumerate() {
            let r = win as f32 / 65535.0_f32;
            let out = curve.eval_float(r);
            *slot = Lcms2Floor::quick_saturate_word(out as f64 * 65535.0);
        }
        tables.push(t);
    }
    U16Curves { tables }
}

/// The batched general-pipeline evaluator: a list of pre-resolved stages plus the
/// pipeline's in/out widths. Built by [`try_optimize`]; evaluates a CHUNK of
/// pixels stage-by-stage. Byte-for-byte identical to the per-pixel
/// `eval_16`/`eval_float`.
#[derive(Clone)]
pub struct BatchedPipeline {
    /// The stage list for the 16-bit eval ([`Self::eval_16_buffer`]): maximal
    /// Curve/U16-CLUT runs are fused into [`BatchedStage::U16Run`] (the big lossless
    /// lever), the rest stay `Clut`/`Generic`.
    stages_16: Vec<BatchedStage>,
    /// The stage list for the FLOAT eval ([`Self::eval_float_buffer`]): NO u16 runs
    /// (float input is a different domain with no quantization points), so every
    /// stage is `Clut`/`Generic` — the unchanged f32 path.
    stages_float: Vec<BatchedStage>,
    in_ch: usize,
    out_ch: usize,
    /// When the input is 8-bit AND the first stage is `ToneCurves`, the per-channel
    /// 256-entry LUTs memoizing that stage's output for each input byte:
    /// `in_lut[ch][byte] = curve[ch].eval_float(from_8_to_16(byte) as f32 /
    /// 65535.0)`. Used ONLY by [`Self::eval_16_buffer`] when the 16-bit eval does
    /// NOT begin with a u16 run that already covers the first curve (it fuses the
    /// entry conversion + first stage). `eval_float_buffer` ignores it. `None` when
    /// the first stage is not an 8-bit tone-curve set, or when a leading u16 run
    /// already subsumes it.
    input_curve_luts: Option<Vec<[f32; 256]>>,
    /// Whether the 16-bit eval contains at least one fused [`BatchedStage::U16Run`]
    /// (the big lossless lever is live for this transform).
    has_u16_run: bool,
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

    /// Whether the 16-bit eval contains at least one fused u16-domain run (the big
    /// lossless lever): those stages run wholly in u16 with no f32↔int conversion.
    #[must_use]
    pub fn uses_u16_chain(&self) -> bool {
        self.has_u16_run
    }

    /// Evaluate `n` pixels of 16-bit (`win`-domain) input, writing `n` pixels of
    /// 16-bit output. Byte-for-byte identical to calling
    /// [`Pipeline::eval_16`](crate::pipeline::Pipeline::eval_16) per pixel.
    ///
    /// `input` is `n * in_ch` u16 (each channel as produced by the input
    /// formatter: an 8-bit byte arrives as `win = (byte<<8)|byte`, so the low byte
    /// indexes an `InputCurves8` LUT). `output` is `n * out_ch` u16.
    pub fn eval_16_buffer(&self, input: &[u16], output: &mut [u16], n: usize) {
        let mut scratch = BatchedScratch::new();
        self.eval_16_buffer_with(input, output, n, &mut scratch, &Context::new());
    }

    /// [`Self::eval_16_buffer`] reusing caller-owned [`BatchedScratch`] (allocated
    /// ONCE per `do_transform` and shared across tiles), so no per-tile ping-pong
    /// allocation/zeroing. Bit-identical to `eval_16_buffer`.
    pub fn eval_16_buffer_with(
        &self,
        input: &[u16],
        output: &mut [u16],
        n: usize,
        scratch: &mut BatchedScratch,
        ctx: &Context,
    ) {
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;

        let BatchedScratch {
            buf_a,
            buf_b,
            u16: u16_scratch,
            cap_pixels,
        } = scratch;
        // Inner tile width: never exceed the scratch capacity (which may be < CHUNK
        // for a small `do_transform` call).
        let tile = (*cap_pixels).min(CHUNK);
        // `ctx` is created ONCE per `do_transform` (threaded in) — NOT per tile, so
        // a `Generic` tone-curve stage's `eval_float_in` never allocs/drops a
        // `Context`'s plugin registries in the hot loop.

        let mut base = 0usize;
        while base < n {
            let m = (n - base).min(tile);

            // ---- Entry: convert this chunk's input into buf_a (pixel-major). ----
            // When the first stage is an 8-bit input-curve LUT, the entry + first
            // stage are fused: index the LUT by the input byte directly. Otherwise
            // From16ToFloat (u16 as f32 / 65535.0), exactly as eval_16.
            let mut cur: &mut Vec<f32> = &mut *buf_a;
            let mut nxt: &mut Vec<f32> = &mut *buf_b;
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
                start_stage = 1; // stages_16[0] (the generic ToneCurves) is fused in.
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

            // ---- Stages: stage-outer, pixel-inner. Fused U16Run stages process
            // their whole sub-chain in the u16 domain (no f32↔int round-trips). ----
            for stage in &self.stages_16[start_stage..] {
                let stage_out = stage.output_width(width);
                match stage {
                    BatchedStage::U16Run(run) => run.eval(cur, nxt, m, u16_scratch),
                    _ => run_stage(stage, cur, nxt, m, width, stage_out, ctx),
                }
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
        let mut scratch = BatchedScratch::new();
        self.eval_float_buffer_with(input, output, n, &mut scratch, &Context::new());
    }

    /// [`Self::eval_float_buffer`] reusing caller-owned [`BatchedScratch`] across
    /// tiles (no per-tile ping-pong allocation). Bit-identical to
    /// `eval_float_buffer`.
    pub fn eval_float_buffer_with(
        &self,
        input: &[f32],
        output: &mut [f32],
        n: usize,
        scratch: &mut BatchedScratch,
        ctx: &Context,
    ) {
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;

        let BatchedScratch {
            buf_a,
            buf_b,
            cap_pixels,
            ..
        } = scratch;
        // Inner tile width: never exceed the scratch capacity (see `eval_16_buffer`).
        let tile = (*cap_pixels).min(CHUNK);
        // `ctx` threaded in (created once per `do_transform`, not per tile).

        let mut base = 0usize;
        while base < n {
            let m = (n - base).min(tile);

            let mut cur: &mut Vec<f32> = &mut *buf_a;
            let mut nxt: &mut Vec<f32> = &mut *buf_b;
            let mut width = in_ch;

            // Entry: straight f32 copy (eval_float's memmove of input_channels).
            for p in 0..m {
                let inp = &input[(base + p) * in_ch..(base + p) * in_ch + in_ch];
                cur[p * in_ch..p * in_ch + in_ch].copy_from_slice(inp);
            }

            for stage in &self.stages_float {
                let stage_out = stage.output_width(width);
                run_stage(stage, cur, nxt, m, width, stage_out, ctx);
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
            BatchedStage::U16Run(r) => r.out_width,
            BatchedStage::Generic(s) => s.output_channels(),
        }
    }
}

/// Run one stage over `m` pixels: read `in_width` channels per pixel from `cur`,
/// write `out_width` channels per pixel into `nxt` (pixel-major layout). `ctx` is a
/// single [`Context`] hoisted for the WHOLE eval so a `Generic` tone-curve stage
/// never allocates one per channel-per-pixel (see [`run_curve_set`]).
#[inline]
fn run_stage(
    stage: &BatchedStage,
    cur: &[f32],
    nxt: &mut [f32],
    m: usize,
    in_width: usize,
    out_width: usize,
    ctx: &Context,
) {
    match stage {
        BatchedStage::Clut(c) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                c.eval(src, dst);
            }
        }
        // A `Generic` tone-curve set: evaluate each channel via `eval_float_in` with
        // the HOISTED `ctx`, NOT `eval_float` (which calls `Context::new()` — an
        // alloc+drop — per channel per pixel). With an empty context `eval_float_in`
        // is byte-for-byte the builtin `eval_float`, so this is bit-identical; it
        // only removes the per-call context churn. Width is preserved by a curve
        // set, so `in_width == out_width`.
        BatchedStage::Generic(Stage::ToneCurves(curves)) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                for (i, curve) in curves.iter().enumerate() {
                    dst[i] = curve.eval_float_in(ctx, src[i]);
                }
            }
        }
        // SIMD: a 3x3 no-offset matrix is vectorized ACROSS PIXELS (f64x4) — each
        // lane is one pixel, plain `*`/`+` in scalar order, no FMA → bit-identical
        // to the Stage::Matrix f64 arm. Only this exact shape is specialized;
        // everything else (offsets, other widths) falls through to Stage::eval.
        #[cfg(feature = "simd")]
        BatchedStage::Generic(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: mat,
            offset: None,
        }) => {
            crate::simd::matrix3x3_across_pixels(cur, nxt, mat, m);
            let _ = (in_width, out_width);
        }
        BatchedStage::Generic(s) => {
            for p in 0..m {
                let src = &cur[p * in_width..p * in_width + in_width];
                let dst = &mut nxt[p * out_width..p * out_width + out_width];
                s.eval(src, dst);
            }
        }
        // U16Run stages only appear in the 16-bit eval's stage list, which
        // dispatches them via `U16Run::eval`; the float path never holds one.
        BatchedStage::U16Run(_) => unreachable!("U16Run is only used by the 16-bit eval"),
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

    // THE BIG LOSSLESS LEVER: fuse every maximal run of Curve / `ClutTable::U16`-CLUT
    // stages bounded by 16-bit quantization points into one u16-domain kernel (no
    // per-pixel f32↔int conversion inside the run). `stages_float` keeps the
    // unfused f32 stages (the float eval has no quantization points). If a leading
    // run begins at the 8-bit input curve, the `input_curve_luts` fast path would
    // double-cover it — so suppress those LUTs when a u16 run owns stage 0.
    let (stages_16, has_u16_run, leading_run_owns_first) = split_u16_runs(stages, &batched);
    if leading_run_owns_first {
        input_curve_luts = None;
    }

    Some(BatchedPipeline {
        stages_16,
        stages_float: batched,
        in_ch: lut.input_channels,
        out_ch: lut.output_channels,
        input_curve_luts,
        has_u16_run,
    })
}

/// Whether a stage can be a u16-domain member (a tone-curve set or a builtin-interp
/// `ClutTable::U16` CLUT). The paired `BatchedStage` confirms a CLUT resolved to a
/// builtin interpolator (a custom-interp CLUT stays `Generic`, so it is excluded).
fn is_u16_member(stage: &Stage, bs: &BatchedStage) -> bool {
    match (stage, bs) {
        (Stage::ToneCurves(_), _) => true,
        (Stage::Clut(_), BatchedStage::Clut(rc)) => matches!(rc.table, ClutTable::U16(_)),
        _ => false,
    }
}

/// Whether the boundary at `pos` (0 = pipeline entry, `n` = exit, otherwise between
/// `members[pos-1]` and `members[pos]`) is a 16-bit QUANTIZATION POINT: the f32
/// value there is `w / 65535.0` for some u16 `w`. True at entry/exit and at any
/// boundary adjacent to a U16-CLUT (it applies `FromFloatTo16` on read and widens
/// `out16 / 65535.0` on write). A boundary flanked only by curves / matrix / Lab is
/// NOT quantized (those carry free f32), so a u16 run may not straddle it.
fn is_quant_boundary(members: &[Option<U16Kind>], pos: usize) -> bool {
    let n = members.len();
    if pos == 0 || pos == n {
        return true;
    }
    matches!(members[pos - 1], Some(U16Kind::Clut)) || matches!(members[pos], Some(U16Kind::Clut))
}

/// Light tag for the boundary analysis: a stage is either a u16-able curve, a
/// u16-able CLUT, or neither (`None`).
#[derive(Clone, Copy, PartialEq)]
enum U16Kind {
    Curves,
    Clut,
}

/// Whether a `ToneCurves` stage is wholly TABULATED (every channel curve has no
/// float segment description, `nSegments == 0`). For such a stage the per-pixel
/// `ToneCurve::eval_float` is `quick_saturate_word(in*65535) -> eval_16 -> /65535`
/// — it QUANTIZES its f32 input to u16 as its first act, exactly the entry
/// conversion a [`U16Run`] applies. So a tabulated curve may START a u16 run even
/// from a free-f32 left boundary (e.g. the sRGB-input curve sitting AFTER a
/// matrix/Lab stage in an RGB->CMYK link): the fold is a pure memoization of the
/// float path. A SEGMENTED curve sees CONTINUOUS input (no leading quantization),
/// so it must NOT start a run from a non-quant boundary — quantizing first would
/// diverge.
fn is_tabulated_curve_set(stage: &Stage) -> bool {
    matches!(stage, Stage::ToneCurves(curves) if curves.iter().all(|c| c.segments().is_empty()))
}

/// Fuse maximal runs of u16-able stages bounded by 16-bit quantization points into
/// [`BatchedStage::U16Run`] stages, returning the 16-bit stage list, whether any
/// run was formed, and whether a run owns stage 0 (so the 8-bit input-curve LUT
/// fast path must stand down). Stages outside a run keep their `Clut`/`Generic`
/// form. See [`U16Run`] for the bit-identity argument.
fn split_u16_runs(stages: &[Stage], batched: &[BatchedStage]) -> (Vec<BatchedStage>, bool, bool) {
    let n = stages.len();
    // Per-stage u16 kind (None = not u16-able).
    let kinds: Vec<Option<U16Kind>> = stages
        .iter()
        .zip(batched.iter())
        .map(|(s, bs)| {
            if !is_u16_member(s, bs) {
                None
            } else if matches!(s, Stage::Clut(_)) {
                Some(U16Kind::Clut)
            } else {
                Some(U16Kind::Curves)
            }
        })
        .collect();

    // A leading run starting at a U16-CLUT relies on `FromFloatTo16(From16ToFloat(
    // win)) == win`. Verify that round-trip identity once; if it ever fails (it does
    // not, but PROVE rather than assume), decline u16 runs entirely.
    let roundtrip_ok = entry_word_roundtrip_is_identity();

    let mut out: Vec<BatchedStage> = Vec::with_capacity(n);
    let mut has_run = false;
    let mut leading_owns_first = false;
    let mut i = 0usize;
    while i < n {
        // A run can begin at i if i is a u16 member AND its left edge is a valid
        // run start: either boundary i is a 16-bit quantization point, OR stage i is
        // a TABULATED tone-curve set (whose `eval_float` quantizes its input itself,
        // so the run's entry conversion reproduces the float path exactly — see
        // `is_tabulated_curve_set`). The latter lets a tabulated input curve that is
        // separated from the CLUT by a matrix/Lab stage join the CLUT's run.
        let can_start = is_quant_boundary(&kinds, i)
            || (matches!(kinds[i], Some(U16Kind::Curves)) && is_tabulated_curve_set(&stages[i]));
        if kinds[i].is_some() && can_start && roundtrip_ok {
            // Extend while the NEXT stage is a u16 member and the boundary BETWEEN
            // them is a quant point (so the run never straddles a free-f32 value).
            let mut j = i;
            while j + 1 < n && kinds[j + 1].is_some() && is_quant_boundary(&kinds, j + 1) {
                j += 1;
            }
            // The run's right edge (boundary j+1) must also be a quant point.
            if is_quant_boundary(&kinds, j + 1) {
                // Build the fused u16 run over stages[i..=j].
                let mut u16_stages = Vec::with_capacity(j - i + 1);
                for k in i..=j {
                    match (&stages[k], &batched[k]) {
                        (Stage::ToneCurves(curves), _) => {
                            u16_stages.push(U16Stage::Curves(build_u16_curve_tables(curves)));
                        }
                        (Stage::Clut(_), BatchedStage::Clut(rc)) => {
                            u16_stages.push(U16Stage::Clut(rc.clone()));
                        }
                        _ => unreachable!("run members are u16-able by construction"),
                    }
                }
                let run_in = stages[i].input_channels();
                let run_out = stages[j].output_channels();
                out.push(BatchedStage::U16Run(U16Run {
                    stages: u16_stages,
                    in_width: run_in,
                    out_width: run_out,
                }));
                has_run = true;
                if i == 0 {
                    leading_owns_first = true;
                }
                i = j + 1;
                continue;
            }
        }
        // Not the start of a run: keep the stage's f32 form.
        out.push(batched[i].clone());
        i += 1;
    }

    (out, has_run, leading_owns_first)
}

/// Whether `FromFloatTo16(From16ToFloat(win)) == win` for EVERY `win` in
/// `0..=0xFFFF`, i.e. quantizing each word to float and back is the identity. The
/// pure u16 chain feeds raw input words to a leading CLUT, which the f32 path only
/// reaches after this exact round-trip; the chain is byte-identical only if the
/// round-trip is lossless. Evaluated once at build time.
fn entry_word_roundtrip_is_identity() -> bool {
    (0u32..=0xFFFF).all(|win| {
        let r = win as f32 / 65535.0_f32;
        let back = Lcms2Floor::quick_saturate_word(r as f64 * 65535.0);
        back as u32 == win
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

    /// A curves -> CLUT -> curves pipeline (the dominant 8/16-bit print shape):
    /// builds the pure u16 chain and must equal `eval_16` bit-for-bit.
    fn rgb_curve_clut_curve_pipeline() -> Pipeline {
        let mut p = Pipeline::new(3, 4);
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(2.2),
            build_gamma(1.8),
            build_gamma(2.4),
        ]))
        .unwrap();
        let n_samples = [3u32, 3, 3];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0u16; 27 * 4];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 2417 + 991) & 0xffff) as u16;
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();
        // Output curves (4 channels).
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(0.9),
            build_gamma(1.1),
            build_gamma(1.0),
            build_gamma(2.0),
        ]))
        .unwrap();
        p
    }

    /// The pure u16 chain forms for a Curve/U16-CLUT pipeline and is byte-identical
    /// to `eval_16` across a dense 16-bit sweep (incl. shadows + 0xFFFF poles).
    #[test]
    fn u16_chain_equals_eval_16() {
        let p = rgb_curve_clut_curve_pipeline();
        let batched = try_optimize(&p, TYPE_CMYK_16, TYPE_CMYK_16).expect("batched built");
        assert!(
            batched.uses_u16_chain(),
            "Curve/U16-CLUT pipeline must build the pure u16 chain"
        );

        let mut input = Vec::new();
        // Shadow-dense plus a coarse high sweep and the 0xFFFF pole.
        let mut vals: Vec<u16> = (0u16..=32).collect();
        for v in (100u16..=65535).step_by(4099) {
            vals.push(v);
        }
        vals.push(0xFFFF);
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
        let mut out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            let expect = p.eval_16(win);
            assert_eq!(&out[i * 4..i * 4 + 4], &expect[..], "pixel {i} win={win:?}");
        }
    }

    /// A pipeline LEADING with a U16 CLUT (no input curve) still forms the u16
    /// chain (entry round-trip is the identity) and equals `eval_16`.
    #[test]
    fn u16_chain_leading_clut_equals_eval_16() {
        let mut p = Pipeline::new(3, 4);
        let n_samples = [3u32, 3, 3];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0u16; 27 * 4];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 5077 + 13) & 0xffff) as u16;
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();
        let batched = try_optimize(&p, TYPE_CMYK_16, TYPE_CMYK_16).expect("batched built");
        assert!(batched.uses_u16_chain(), "leading CLUT must form the chain");

        let mut input = Vec::new();
        let vals: Vec<u16> = vec![0, 1, 7, 255, 4369, 30000, 60000, 0xFFFF];
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    input.extend_from_slice(&[r, g, b]);
                }
            }
        }
        let n = input.len() / 3;
        let mut out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            assert_eq!(&out[i * 4..i * 4 + 4], &p.eval_16(win)[..], "pixel {i}");
        }
    }

    /// A TABULATED curve separated from the CLUT by a matrix/Lab stage (a free-f32
    /// upstream boundary) must STILL be absorbed into the u16 run that owns the
    /// CLUT: a tabulated curve's `eval_float` quantizes its input via
    /// `quick_saturate_word(in*65535)` — exactly the run's entry conversion — so the
    /// fold is a pure memoization. This is the RGB8->CMYK8 sRGB-input shape
    /// (Curves -> Matrix -> Xyz2Lab -> Curves(tabulated) -> CLUT -> Curves), where
    /// the post-Lab tabulated curve used to run per-pixel via `eval_float`.
    #[test]
    fn tabulated_curve_after_matrix_absorbed_into_u16_run() {
        use crate::curve::build_tabulated_16;

        let mut p = Pipeline::new(3, 4);
        // A matrix with a non-trivial mix so the post-matrix domain is genuinely
        // continuous (no per-channel structure to exploit).
        p.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: vec![0.9, 0.05, 0.05, 0.1, 0.8, 0.1, 0.05, 0.15, 0.8],
            offset: None,
        })
        .unwrap();
        // A TABULATED curve (segments empty) right before the CLUT. Its input
        // boundary (matrix output) is free f32 — NOT a quant point — yet it is
        // u16-foldable because it is tabulated.
        let mk_tab = |bias: u32| {
            let t: Vec<u16> = (0u32..256)
                .map(|i| (((i * 257 + bias) * 251) & 0xffff) as u16)
                .collect();
            build_tabulated_16(&t)
        };
        p.insert_stage_at_end(Stage::ToneCurves(vec![mk_tab(0), mk_tab(7), mk_tab(13)]))
            .unwrap();
        let n_samples = [3u32, 3, 3];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0u16; 27 * 4];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 2417 + 991) & 0xffff) as u16;
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();

        let batched = try_optimize(&p, TYPE_RGB_8, TYPE_CMYK_16).expect("batched built");
        assert!(
            batched.uses_u16_chain(),
            "tabulated curve before CLUT must form a u16 run"
        );
        // The tabulated curve + CLUT must be in ONE u16 run (the curve ABSORBED),
        // so the 16-bit stage list is exactly [Matrix(Generic), U16Run{Curves,Clut}]
        // — the curve does NOT survive as its own Generic stage.
        assert_eq!(
            batched.stages_16.len(),
            2,
            "expected [Matrix, U16Run]; the curve must be folded INTO the run"
        );
        match (&batched.stages_16[0], &batched.stages_16[1]) {
            (BatchedStage::Generic(Stage::Matrix { .. }), BatchedStage::U16Run(run)) => {
                assert_eq!(run.stages.len(), 2, "run must hold the curve + the CLUT");
                assert!(
                    matches!(run.stages[0], U16Stage::Curves(_)),
                    "the absorbed tabulated curve must be the run's first u16 stage"
                );
                assert!(
                    matches!(run.stages[1], U16Stage::Clut(_)),
                    "the CLUT must follow the absorbed curve in the run"
                );
            }
            _ => panic!("expected [Matrix(Generic), U16Run] with the curve absorbed"),
        }

        // Bit-identity vs eval_16 over a 16-bit sweep.
        let mut input = Vec::new();
        let mut vals: Vec<u16> = (0u16..=20).collect();
        for v in (100u16..=65535).step_by(3037) {
            vals.push(v);
        }
        vals.push(0xFFFF);
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    input.extend_from_slice(&[r, g, b]);
                }
            }
        }
        let n = input.len() / 3;
        let mut out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            assert_eq!(&out[i * 4..i * 4 + 4], &p.eval_16(win)[..], "pixel {i}");
        }
    }

    /// A SEGMENTED curve separated from the CLUT by a matrix (free-f32 boundary)
    /// must NOT be absorbed into the u16 run: a segmented curve sees CONTINUOUS
    /// input, so quantizing it at the run entry would diverge from the float path.
    /// It stays `Generic`; the CLUT (+ any tail) still forms its own run.
    #[test]
    fn segmented_curve_after_matrix_not_absorbed() {
        use crate::curve::build_gamma;

        let mut p = Pipeline::new(3, 4);
        p.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: vec![0.9, 0.05, 0.05, 0.1, 0.8, 0.1, 0.05, 0.15, 0.8],
            offset: None,
        })
        .unwrap();
        // Segmented (parametric) curves — continuous input domain.
        p.insert_stage_at_end(Stage::ToneCurves(vec![
            build_gamma(2.2),
            build_gamma(1.8),
            build_gamma(2.4),
        ]))
        .unwrap();
        let n_samples = [3u32, 3, 3];
        let params = InterpParams::new(&n_samples, 3, 4);
        let mut table = vec![0u16; 27 * 4];
        for (i, v) in table.iter_mut().enumerate() {
            *v = ((i * 2417 + 991) & 0xffff) as u16;
        }
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();

        let batched = try_optimize(&p, TYPE_CMYK_16, TYPE_CMYK_16).expect("built");
        // The segmented curve must remain Generic (not folded into a run). The CLUT
        // alone forms a run. Stage list: [Matrix(Generic), Curves(Generic), U16Run].
        match &batched.stages_16[1] {
            BatchedStage::Generic(Stage::ToneCurves(_)) => {}
            BatchedStage::U16Run(_) => panic!("segmented curve must not be folded into a u16 run"),
            _ => panic!("segmented curve must stay a Generic ToneCurves stage"),
        }

        // Bit-identity vs eval_16 still holds (the segmented curve runs per-pixel).
        let mut input = Vec::new();
        let vals: Vec<u16> = vec![0, 1, 255, 4369, 30000, 60000, 0xFFFF];
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    input.extend_from_slice(&[r, g, b]);
                }
            }
        }
        let n = input.len() / 3;
        let mut out = vec![0u16; n * 4];
        batched.eval_16_buffer(&input, &mut out, n);
        for i in 0..n {
            let win = &input[i * 3..i * 3 + 3];
            assert_eq!(&out[i * 4..i * 4 + 4], &p.eval_16(win)[..], "pixel {i}");
        }
    }

    /// A FLOAT CLUT pipeline must NOT form the u16 chain (its boundary is f32).
    #[test]
    fn float_clut_does_not_form_u16_chain() {
        let mut p = Pipeline::new(3, 4);
        let n_samples = [2u32, 2, 2];
        let params = InterpParams::new(&n_samples, 3, 4);
        let table = vec![0.5f32; 8 * 4];
        p.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::F32(table),
            params,
            is_trilinear: false,
            implements_identity: false,
            resolved: ResolvedInterp::default(),
        }))
        .unwrap();
        let batched = try_optimize(&p, TYPE_CMYK_16, TYPE_CMYK_16).expect("built");
        assert!(
            !batched.uses_u16_chain(),
            "float CLUT must not be promoted to the u16 chain"
        );
    }

    /// The entry word round-trip `FromFloatTo16(From16ToFloat(win)) == win` is the
    /// identity for every u16 — the property the leading-CLUT u16 chain relies on.
    #[test]
    fn entry_word_roundtrip_identity_holds() {
        assert!(entry_word_roundtrip_is_identity());
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
