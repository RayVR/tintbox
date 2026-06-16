//! Transform throughput benchmark: tintbox vs lcms2 on the hard color-transform
//! hot paths. Measures ONLY the per-pixel `do_transform` loop — every transform
//! is built once, outside the timed region — over a large (1024×1024) buffer so
//! the numbers reflect steady-state per-pixel throughput.
//!
//! Fairness invariants (see the perf-bench task spec):
//!   * Build the transform once, outside the timed loop; time only `do_transform`.
//!   * Identical input PROFILE BYTES feed both engines (tintbox `Profile::open`
//!     and lcms2 `cmsOpenProfileFromMem` get the same `&[u8]`).
//!   * Matched intent (Perceptual) / BPC (on) / pixel format within each scenario.
//!   * One deterministic pseudo-random input buffer per scenario, reused; one
//!     pre-allocated output buffer, reused. Buffers are NOT all-zero (zeros hit
//!     trivial CLUT corners).
//!
//! Engines compared per scenario:
//!   * tintbox-Accurate     — the lossless full-float pipeline eval (default).
//!   * tintbox-Lcms2Compat  — tintbox's lcms2-matching optimizer.
//!   * lcms2-NOOPTIMIZE — lcms2 with `NOOPTIMIZE | BPC` (lossless: the fair
//!     accuracy-matched fight against tintbox-Accurate).
//!   * lcms2-DEFAULT — lcms2 with `BPC` only, i.e. its DEFAULT optimizer (the
//!     fast-but-lossy baked CLUT/matshaper path).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use tintbox::format::decode::{
    TYPE_CMYK_16, TYPE_CMYK_8, TYPE_CMYK_FLT, TYPE_RGB_16, TYPE_RGB_8, TYPE_RGB_FLT,
};
use tintbox::opt::OptimizationStrategy;
use tintbox::profile::serialize::save_to_mem;
use tintbox::profile::virtuals::build_srgb_profile;
use tintbox::profile::Profile;
use tintbox::profile::RenderingIntent;
use tintbox::transform::{Flags, Transform};

use tintbox_oracle::{OracleTransform, CMS_FLAGS_BLACKPOINTCOMPENSATION, CMS_FLAGS_NOOPTIMIZE};

/// 1024×1024 — large enough that we measure steady-state throughput, not noise.
const N_PIXELS: usize = 1024 * 1024;

/// lcms2 `INTENT_PERCEPTUAL`.
const INTENT_PERCEPTUAL: u32 = 0;

/// Bytes per packed pixel for a tintbox/lcms2 format word: channels × depth,
/// where depth is bytes_sh-encoded (0 == float/4 here for our float formats —
/// but our float formats are 4-byte, so we special-case by table below).
fn pixel_bytes(fmt: u32) -> usize {
    match fmt {
        f if f == TYPE_RGB_8 => 3,
        f if f == TYPE_RGB_16 => 6,
        f if f == TYPE_RGB_FLT => 12,
        f if f == TYPE_CMYK_8 => 4,
        f if f == TYPE_CMYK_16 => 8,
        f if f == TYPE_CMYK_FLT => 16,
        _ => unreachable!("unexpected format in bench"),
    }
}

/// Deterministic pseudo-random byte fill (xorshift-derived), so both engines see
/// the SAME non-trivial input bytes every run. For float formats the bytes are
/// reinterpreted as f32 in [0,1) so we never feed NaN/inf or out-of-range values.
fn fill_input(buf: &mut [u8], fmt: u32) {
    let is_float = matches!(fmt, f if f == TYPE_RGB_FLT || f == TYPE_CMYK_FLT);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    if is_float {
        // Write f32 in [0,1) per 4 bytes.
        for chunk in buf.chunks_mut(4) {
            let r = (next() >> 40) as u32; // 24 bits
            let v = (r as f32) / (1u32 << 24) as f32;
            chunk.copy_from_slice(&v.to_le_bytes());
        }
    } else {
        for b in buf.iter_mut() {
            *b = (next() >> 33) as u8;
        }
    }
}

/// A scenario: a label, in/out profile bytes, and in/out format words.
struct Scenario {
    name: &'static str,
    in_bytes: Vec<u8>,
    out_bytes: Vec<u8>,
    in_fmt: u32,
    out_fmt: u32,
}

fn srgb_bytes() -> Vec<u8> {
    // Byte-identical to lcms2's built-in sRGB; the SAME bytes feed both engines.
    save_to_mem(&build_srgb_profile()).expect("save sRGB profile")
}

fn cmyk_bytes(name: &str) -> Vec<u8> {
    // A real CMYK profile from the lcms2 testbed.
    let path = format!(
        "{}/../../vendor/Little-CMS/testbed/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn scenarios() -> Vec<Scenario> {
    let srgb = srgb_bytes();
    let cmyk1 = cmyk_bytes("test1.icc");
    let cmyk2 = cmyk_bytes("test2.icc");
    vec![
        Scenario {
            name: "A_rgb8_to_cmyk8",
            in_bytes: srgb.clone(),
            out_bytes: cmyk1.clone(),
            in_fmt: TYPE_RGB_8,
            out_fmt: TYPE_CMYK_8,
        },
        Scenario {
            name: "B_rgb16_to_cmyk16",
            in_bytes: srgb.clone(),
            out_bytes: cmyk1.clone(),
            in_fmt: TYPE_RGB_16,
            out_fmt: TYPE_CMYK_16,
        },
        Scenario {
            name: "C_rgbflt_to_cmykflt",
            in_bytes: srgb.clone(),
            out_bytes: cmyk1.clone(),
            in_fmt: TYPE_RGB_FLT,
            out_fmt: TYPE_CMYK_FLT,
        },
        Scenario {
            name: "D_cmyk8_to_cmyk8",
            in_bytes: cmyk1.clone(),
            out_bytes: cmyk2.clone(),
            in_fmt: TYPE_CMYK_8,
            out_fmt: TYPE_CMYK_8,
        },
        Scenario {
            name: "E_rgb8_to_rgb8_matrix_shaper",
            in_bytes: srgb.clone(),
            out_bytes: srgb.clone(),
            in_fmt: TYPE_RGB_8,
            out_fmt: TYPE_RGB_8,
        },
        // F: a GENUINE non-identity RGB matrix-shaper pair (crayons -> test5), so
        // the merged matrix is NOT identity and the matrix-shaper fast paths
        // actually fire. Scenario E above degenerates to a curves-only pipeline
        // (sRGB -> sRGB merges to the identity matrix, dropped by pre_optimize),
        // which exercises curve-join, not the matrix shaper. F is the worst-gap
        // case the lossless AccurateFast targets.
        Scenario {
            name: "F_rgb8_to_rgb8_matrix_shaper_real",
            in_bytes: cmyk_bytes("crayons.icc"),
            out_bytes: cmyk_bytes("test5.icc"),
            in_fmt: TYPE_RGB_8,
            out_fmt: TYPE_RGB_8,
        },
    ]
}

/// Build a tintbox transform with the given optimization strategy, intent
/// Perceptual, BPC on, over the scenario's exact profile bytes and formats.
fn build_tintbox<'a>(
    in_prof: &'a Profile<'a>,
    out_prof: &'a Profile<'a>,
    in_fmt: u32,
    out_fmt: u32,
    strategy: OptimizationStrategy,
) -> Transform {
    Transform::new_with_formats_strategy(
        &[in_prof, out_prof],
        &[RenderingIntent::Perceptual, RenderingIntent::Perceptual],
        &[true, true],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE.union(Flags::BLACKPOINTCOMPENSATION),
        in_fmt,
        out_fmt,
        strategy,
    )
    .expect("build tintbox transform")
}

fn bench(c: &mut Criterion) {
    let scenarios = scenarios();

    for sc in &scenarios {
        let in_stride = pixel_bytes(sc.in_fmt);
        let out_stride = pixel_bytes(sc.out_fmt);

        // One deterministic input buffer + one output buffer, reused by every
        // engine in this scenario (identical input bytes to all four engines).
        let mut input = vec![0u8; N_PIXELS * in_stride];
        fill_input(&mut input, sc.in_fmt);
        let mut output = vec![0u8; N_PIXELS * out_stride];

        // tintbox profiles opened from the SAME bytes lcms2 will receive.
        let in_prof = Profile::open(&sc.in_bytes).expect("open tintbox input profile");
        let out_prof = Profile::open(&sc.out_bytes).expect("open tintbox output profile");

        // ---- Build all four transforms ONCE, outside the timed region. ----
        let tb_accurate = build_tintbox(
            &in_prof,
            &out_prof,
            sc.in_fmt,
            sc.out_fmt,
            OptimizationStrategy::Accurate,
        );
        let tb_compat = build_tintbox(
            &in_prof,
            &out_prof,
            sc.in_fmt,
            sc.out_fmt,
            OptimizationStrategy::Lcms2Compat,
        );
        // The LOSSLESS AccurateFast path: byte-identical to tintbox-Accurate, but
        // installs the exact-LUT matrix-shaper fast path for the RGB 8-bit-input
        // matrix-shaper shape (scenario E) and the exact input-curve LUT elsewhere
        // (falls back to Pipeline where the shape does not match).
        let tb_accurate_fast = build_tintbox(
            &in_prof,
            &out_prof,
            sc.in_fmt,
            sc.out_fmt,
            OptimizationStrategy::AccurateFast,
        );
        let lcms_noopt = OracleTransform::create(
            &sc.in_bytes,
            sc.in_fmt,
            &sc.out_bytes,
            sc.out_fmt,
            INTENT_PERCEPTUAL,
            CMS_FLAGS_NOOPTIMIZE | CMS_FLAGS_BLACKPOINTCOMPENSATION,
        )
        .expect("create lcms2 NOOPTIMIZE transform");
        let lcms_default = OracleTransform::create(
            &sc.in_bytes,
            sc.in_fmt,
            &sc.out_bytes,
            sc.out_fmt,
            INTENT_PERCEPTUAL,
            CMS_FLAGS_BLACKPOINTCOMPENSATION,
        )
        .expect("create lcms2 DEFAULT transform");

        let mut group = c.benchmark_group(sc.name);
        group.throughput(Throughput::Elements(N_PIXELS as u64));
        // Statistical rigor: 100 samples + a long measurement/warm-up so criterion
        // reports tight confidence intervals (the 20-sample config could not
        // distinguish a real gain from run-to-run noise). Run only on a QUIET
        // machine — background CPU load dominates these medians otherwise.
        group.sample_size(100);
        group.measurement_time(std::time::Duration::from_secs(12));
        group.warm_up_time(std::time::Duration::from_secs(3));

        group.bench_function(BenchmarkId::new(sc.name, "tintbox-Accurate"), |b| {
            b.iter(|| {
                tb_accurate.do_transform(black_box(&input), black_box(&mut output), N_PIXELS);
            })
        });
        group.bench_function(BenchmarkId::new(sc.name, "tintbox-Lcms2Compat"), |b| {
            b.iter(|| {
                tb_compat.do_transform(black_box(&input), black_box(&mut output), N_PIXELS);
            })
        });
        group.bench_function(BenchmarkId::new(sc.name, "tintbox-AccurateFast"), |b| {
            b.iter(|| {
                tb_accurate_fast.do_transform(black_box(&input), black_box(&mut output), N_PIXELS);
            })
        });
        group.bench_function(BenchmarkId::new(sc.name, "lcms2-NOOPTIMIZE"), |b| {
            b.iter(|| {
                lcms_noopt.do_transform(black_box(&input), black_box(&mut output), N_PIXELS);
            })
        });
        group.bench_function(BenchmarkId::new(sc.name, "lcms2-DEFAULT"), |b| {
            b.iter(|| {
                lcms_default.do_transform(black_box(&input), black_box(&mut output), N_PIXELS);
            })
        });

        group.finish();
    }
}

/// SMALL-CHUNK regression guard: a renderer drives a CMM per scanline/tile, not in
/// one giant call. The batched `AccurateFast` path used to allocate+zero a full
/// TILE-wide (~hundreds of KB) scratch on EVERY `do_transform` call regardless of
/// `n_pixels`, so small calls were catastrophically slow (up to ~400x slower than
/// Accurate). This bench drives RGB8->CMYK8 in 64-pixel chunks for AccurateFast vs
/// Accurate so that regression class can't silently return: AccurateFast must stay
/// >= Accurate here. (The 1M single-call regime is covered by `bench` above.)
fn bench_small_chunks(c: &mut Criterion) {
    const CHUNK: usize = 64;
    let srgb = srgb_bytes();
    let cmyk1 = cmyk_bytes("test1.icc");
    let in_fmt = TYPE_RGB_8;
    let out_fmt = TYPE_CMYK_8;
    let in_stride = pixel_bytes(in_fmt);
    let out_stride = pixel_bytes(out_fmt);

    let mut input = vec![0u8; N_PIXELS * in_stride];
    fill_input(&mut input, in_fmt);
    let mut output = vec![0u8; N_PIXELS * out_stride];

    let in_prof = Profile::open(&srgb).expect("open input profile");
    let out_prof = Profile::open(&cmyk1).expect("open output profile");

    let tb_accurate = build_tintbox(
        &in_prof,
        &out_prof,
        in_fmt,
        out_fmt,
        OptimizationStrategy::Accurate,
    );
    let tb_accurate_fast = build_tintbox(
        &in_prof,
        &out_prof,
        in_fmt,
        out_fmt,
        OptimizationStrategy::AccurateFast,
    );

    // Drive the full N-pixel buffer in CHUNK-sized do_transform calls.
    let run = |xform: &Transform, input: &[u8], output: &mut [u8]| {
        let mut off = 0;
        while off < N_PIXELS {
            let k = CHUNK.min(N_PIXELS - off);
            xform.do_transform(
                &input[off * in_stride..(off + k) * in_stride],
                &mut output[off * out_stride..(off + k) * out_stride],
                k,
            );
            off += k;
        }
    };

    let mut group = c.benchmark_group("A_rgb8_to_cmyk8_chunk64");
    group.throughput(Throughput::Elements(N_PIXELS as u64));
    group.sample_size(30);
    group.measurement_time(std::time::Duration::from_secs(8));
    group.warm_up_time(std::time::Duration::from_secs(2));

    group.bench_function(BenchmarkId::new("chunk64", "tintbox-Accurate"), |b| {
        b.iter(|| run(&tb_accurate, black_box(&input), black_box(&mut output)));
    });
    group.bench_function(BenchmarkId::new("chunk64", "tintbox-AccurateFast"), |b| {
        b.iter(|| run(&tb_accurate_fast, black_box(&input), black_box(&mut output)));
    });

    group.finish();
}

criterion_group!(benches, bench, bench_small_chunks);
criterion_main!(benches);
