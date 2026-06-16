//! Dedicated hot-path profiling target — pure tintbox, no oracle/criterion noise.
//!
//! Builds one transform and hammers `do_transform` over a 1024×1024 buffer so a
//! sampling profiler (samply/Instruments) attributes cycles to the real per-pixel
//! work. Pick the scenario + strategy via argv so we can profile each hot case:
//!
//!   cargo build --release --example profile_transform
//!   samply record ./target/release/examples/profile_transform rgb8_cmyk8 fast
//!
//! scenario: rgb8_cmyk8 | rgb16_cmyk16 | cmyk8_cmyk8 | matshaper
//! strategy: fast (AccurateFast) | accurate | compat

use std::time::Instant;

use tintbox::format::decode::{TYPE_CMYK_16, TYPE_CMYK_8, TYPE_LAB_16, TYPE_RGB_16, TYPE_RGB_8};
use tintbox::opt::OptimizationStrategy;
use tintbox::prelude::*;
use tintbox::profile::{save_to_mem, virtuals::build_srgb_profile, RenderingIntent};

const N: usize = 1024 * 1024;

fn testbed(name: &str) -> Vec<u8> {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed/"
    );
    std::fs::read(format!("{dir}{name}")).expect("read testbed profile")
}

fn strategy(s: &str) -> OptimizationStrategy {
    match s {
        "fast" => OptimizationStrategy::AccurateFast,
        "accurate" => OptimizationStrategy::Accurate,
        "compat" => OptimizationStrategy::Lcms2Compat,
        other => panic!("unknown strategy {other}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let scenario = args.next().unwrap_or_else(|| "rgb8_cmyk8".into());
    let strat = strategy(&args.next().unwrap_or_else(|| "fast".into()));

    let srgb = save_to_mem(&build_srgb_profile()).expect("srgb bytes");
    let lab4 = save_to_mem(&tintbox::profile::virtuals::build_lab4_profile()).expect("lab4 bytes");
    let test1 = testbed("test1.icc");
    let test2 = testbed("test2.icc");

    // (input profile bytes, output profile bytes, in_fmt, out_fmt, in_bpp, out_bpp)
    let (ina, outa, inf, outf, inbpp, outbpp) = match scenario.as_str() {
        "rgb8_cmyk8" => (&srgb, &test1, TYPE_RGB_8, TYPE_CMYK_8, 3, 4),
        "rgb16_cmyk16" => (&srgb, &test1, TYPE_RGB_16, TYPE_CMYK_16, 6, 8),
        "cmyk8_cmyk8" => (&test1, &test2, TYPE_CMYK_8, TYPE_CMYK_8, 4, 4),
        "cmyk16_cmyk16" => (&test1, &test2, TYPE_CMYK_16, TYPE_CMYK_16, 8, 8),
        // Lab paths: sRGB -> Lab (XYZ->Lab cube-root on output) and Lab -> CMYK
        // (the real Lab-to-ink separation; Lab->XYZ inverse cube-root on input).
        "rgb8_lab16" => (&srgb, &lab4, TYPE_RGB_8, TYPE_LAB_16, 3, 6),
        "lab16_cmyk8" => (&lab4, &test1, TYPE_LAB_16, TYPE_CMYK_8, 6, 4),
        other => panic!("unknown scenario {other}"),
    };

    let pin = Profile::open(ina).expect("open in");
    let pout = Profile::open(outa).expect("open out");
    let xform = Transform::new_simple_with_formats_strategy(
        &pin,
        &pout,
        RenderingIntent::Perceptual,
        true,
        inf,
        outf,
        strat,
    )
    .expect("build transform");

    // Deterministic non-zero input (avoid trivial CLUT corners).
    let mut input = vec![0u8; N * inbpp];
    let mut s = 0x2545F491u32;
    for b in input.iter_mut() {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        *b = (s >> 24) as u8;
    }
    let mut output = vec![0u8; N * outbpp];

    // Hammer the hot path. ~6s of work is plenty for a sampling profile.
    eprintln!("profiling {scenario} / {strat:?} — warming up");
    for _ in 0..5 {
        xform.do_transform(&input, &mut output, N);
    }
    let secs_target: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);
    // Process the N-pixel buffer in `chunk`-sized do_transform calls (default: one
    // call). Small chunks model a renderer calling per scanline/tile/pixel.
    let chunk: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(N);
    let start = Instant::now();
    let mut iters = 0u64;
    while start.elapsed().as_secs_f64() < secs_target {
        let mut off = 0;
        while off < N {
            let k = chunk.min(N - off);
            xform.do_transform(
                &input[off * inbpp..(off + k) * inbpp],
                &mut output[off * outbpp..(off + k) * outbpp],
                k,
            );
            off += k;
        }
        iters += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    let mpx = (iters as f64 * N as f64) / secs / 1e6;
    // Touch the output so the loop can't be optimized away.
    let sink: u64 = output.iter().map(|&b| b as u64).sum();
    eprintln!("{scenario}/{strat:?}: {iters} iters in {secs:.2}s = {mpx:.2} Mpx/s (sink={sink})");
    let _ = (inbpp, outbpp);
}
