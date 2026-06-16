//! Differential tests for the LOSSLESS BATCHED general/CLUT fast path
//! (`tintbox::opt::batched`), the `AccurateFast` speedup for the RGB→CMYK /
//! CMYK→CMYK device links that the matrix-shaper fast path does not cover.
//!
//! ABSOLUTE REQUIREMENT proven here: `AccurateFast` (batched) is **byte-for-byte
//! identical** to (a) the default `Accurate` strategy AND (b) lcms2 run with
//! `cmsFLAGS_NOOPTIMIZE` (the differential oracle), over the testbed CLUT/LUT/CMYK
//! profile pairs × intents × pixel-formats {8, 16, float}. Faster-but-different =
//! FAILURE — every assertion is an exact `assert_eq!`, with a dense sweep biased
//! to the shadow range where any LSB divergence surfaces first.

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::format::decode::{
    TYPE_CMYK_16, TYPE_CMYK_8, TYPE_CMYK_FLT, TYPE_RGB_16, TYPE_RGB_8, TYPE_RGB_FLT,
};
use tintbox::opt::OptimizationStrategy;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::Transform;

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

fn load(name: &str) -> Vec<u8> {
    fs::read(testbed_dir().join(name)).unwrap_or_else(|_| panic!("read {name}"))
}

/// Profile pairs that route through a CLUT/LUT device link (NOT the matrix-shaper
/// shape). `(in_name, in_is_rgb, out_name, out_is_rgb)`. The testbed CMYK
/// profiles are test1/test2 (prtr, CMYK→Lab); test3/test4 are spac RGB→Lab
/// (CLUT-based RGB→RGB device links).
fn pairs() -> Vec<(&'static str, bool, &'static str, bool)> {
    vec![
        // RGB -> CMYK (the real PDF print cases A/B).
        ("crayons.icc", true, "test1.icc", false),
        ("crayons.icc", true, "test2.icc", false),
        ("ibm-t61.icc", true, "test1.icc", false),
        ("new.icc", true, "test2.icc", false),
        // CMYK -> CMYK (scenario D).
        ("test1.icc", false, "test2.icc", false),
        ("test2.icc", false, "test1.icc", false),
        // CMYK -> RGB and RGB -> RGB through the CLUT (spac) profiles.
        ("test1.icc", false, "test3.icc", true),
        ("crayons.icc", true, "test4.icc", true),
    ]
}

const INTENTS: &[RenderingIntent] = &[
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
];

/// Dense 8-bit RGB sweep, shadow-biased (every byte 0..32, then coarse to 255).
fn rgb8_sweep() -> Vec<u8> {
    let mut vals: Vec<u8> = (0u8..=31).collect();
    for v in (40u8..=255).step_by(17) {
        vals.push(v);
    }
    vals.push(255);
    vals.dedup();
    let mut out = Vec::with_capacity(vals.len().pow(3) * 3);
    for &r in &vals {
        for &g in &vals {
            for &b in &vals {
                out.extend_from_slice(&[r, g, b]);
            }
        }
    }
    out
}

/// 8-bit CMYK sweep: a 4-D grid (coarser per axis since 4 channels), with a dense
/// shadow corner.
fn cmyk8_sweep() -> Vec<u8> {
    let coarse: Vec<u8> = vec![0, 1, 2, 5, 17, 64, 128, 200, 254, 255];
    let mut out = Vec::new();
    for &c in &coarse {
        for &m in &coarse {
            for &y in &coarse {
                for &k in &coarse {
                    out.extend_from_slice(&[c, m, y, k]);
                }
            }
        }
    }
    out
}

fn build(
    pa: &Profile,
    pb: &Profile,
    intent: RenderingIntent,
    in_fmt: u32,
    out_fmt: u32,
    strategy: OptimizationStrategy,
) -> Transform {
    Transform::new_simple_with_formats_strategy(pa, pb, intent, false, in_fmt, out_fmt, strategy)
        .expect("build transform")
}

/// Widen an 8-bit packed buffer to a 16-bit packed buffer (each byte -> u16 via
/// `(b<<8)|b`, the lcms2 8->16 unpack) and to a float buffer (`b/255`-equivalent
/// via the 16-bit value `/65535`). Returns LE bytes for the 16-bit / float input.
fn widen_to_16(bytes8: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes8.len() * 2);
    for &b in bytes8 {
        let w = ((b as u16) << 8) | b as u16;
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

fn widen_to_float(bytes8: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes8.len() * 4);
    for &b in bytes8 {
        let w = ((b as u16) << 8) | b as u16;
        let f = w as f32 / 65535.0_f32;
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// The headline proof. Over every CLUT/CMYK pair × intent × {8,16,float}, the
/// batched `AccurateFast` output equals BOTH `Accurate` AND lcms2-NOOPTIMIZE,
/// byte-for-byte.
#[test]
fn batched_is_byte_identical_to_accurate_and_lcms2_nooptimize() {
    let mut total_cells = 0usize;
    let mut total_pixels = 0usize;
    let mut batched_fired_cells = 0usize;
    let mut u16_chain_cells = 0usize;
    let mut nonfloat_in_cells = 0usize;
    let mut mismatches = 0usize;

    for (an, in_rgb, bn, out_rgb) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        // Input channel count: RGB (3) or CMYK (4).
        let grid8 = if in_rgb { rgb8_sweep() } else { cmyk8_sweep() };
        let in_ch = if in_rgb { 3 } else { 4 };
        let n = grid8.len() / in_ch;
        let out_ch = if out_rgb { 3 } else { 4 };

        let (in8, in16, inflt) = if in_rgb {
            (TYPE_RGB_8, TYPE_RGB_16, TYPE_RGB_FLT)
        } else {
            (TYPE_CMYK_8, TYPE_CMYK_16, TYPE_CMYK_FLT)
        };
        let (out8, out16, outflt) = if out_rgb {
            (TYPE_RGB_8, TYPE_RGB_16, TYPE_RGB_FLT)
        } else {
            (TYPE_CMYK_8, TYPE_CMYK_16, TYPE_CMYK_FLT)
        };
        let buf16 = widen_to_16(&grid8);
        let bufflt = widen_to_float(&grid8);

        for &intent in INTENTS {
            let intents_raw = [intent.to_raw(), intent.to_raw()];
            let bpc = [false, false];
            let adapt = [1.0, 1.0];

            // Each input depth, sweeping the output format across {8,16,float}.
            for &(in_fmt, in_buf) in &[(in8, &grid8), (in16, &buf16), (inflt, &bufflt)] {
                for &out_fmt in &[out8, out16, outflt] {
                    let out_bpp = if out_fmt == out8 {
                        out_ch
                    } else if out_fmt == out16 {
                        out_ch * 2
                    } else {
                        out_ch * 4
                    };

                    // (b) lcms2-NOOPTIMIZE oracle.
                    let mut oracle = vec![0u8; n * out_bpp];
                    let ok = tintbox_oracle::do_transform_packed(
                        &[&ab, &bb],
                        &intents_raw,
                        &bpc,
                        &adapt,
                        in_fmt,
                        out_fmt,
                        in_buf,
                        &mut oracle,
                        n,
                    );
                    assert!(ok, "lcms2-NOOPTIMIZE failed {an}->{bn}");

                    // (a) tintbox Accurate.
                    let acc = build(
                        &pa,
                        &pb,
                        intent,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::Accurate,
                    );
                    let mut acc_out = vec![0u8; n * out_bpp];
                    acc.do_transform(in_buf, &mut acc_out, n);

                    // tintbox AccurateFast (batched).
                    let fast = build(
                        &pa,
                        &pb,
                        intent,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::AccurateFast,
                    );
                    let mut fast_out = vec![0u8; n * out_bpp];
                    fast.do_transform(in_buf, &mut fast_out, n);

                    if fast.batched_fired() {
                        batched_fired_cells += 1;
                    }
                    // The pure u16-domain chain (the big lossless lever) must fire
                    // for every non-float-INPUT cell of a Curve/U16-CLUT pipeline
                    // (these testbed device links are exactly that shape).
                    let in_is_float = in_fmt == inflt;
                    if !in_is_float {
                        nonfloat_in_cells += 1;
                        if fast.batched_uses_u16_chain() {
                            u16_chain_cells += 1;
                        }
                    }

                    if fast_out != oracle {
                        mismatches += 1;
                        let bad = fast_out
                            .iter()
                            .zip(&oracle)
                            .position(|(a, b)| a != b)
                            .unwrap();
                        panic!(
                            "AccurateFast(batched) != lcms2-NOOPTIMIZE {an}->{bn} \
                             in={in_fmt:#x} out={out_fmt:#x} at byte {bad}: \
                             fast={} oracle={} (batched_fired={}, mismatches {mismatches})",
                            fast_out[bad],
                            oracle[bad],
                            fast.batched_fired()
                        );
                    }
                    if fast_out != acc_out {
                        mismatches += 1;
                        let bad = fast_out
                            .iter()
                            .zip(&acc_out)
                            .position(|(a, b)| a != b)
                            .unwrap();
                        panic!(
                            "AccurateFast(batched) != Accurate {an}->{bn} \
                             in={in_fmt:#x} out={out_fmt:#x} at byte {bad}: \
                             fast={} accurate={} (batched_fired={}, mismatches {mismatches})",
                            fast_out[bad],
                            acc_out[bad],
                            fast.batched_fired()
                        );
                    }

                    total_cells += 1;
                    total_pixels += n;
                }
            }
        }
    }

    assert_eq!(mismatches, 0, "found {mismatches} mismatching cells");
    // The batched path must actually fire for the overwhelming majority of cells
    // (every CLUT pipeline qualifies). Allow a margin in case some pair degenerates.
    assert!(
        batched_fired_cells >= total_cells * 9 / 10,
        "expected the batched fast path to fire for nearly all cells, got \
         {batched_fired_cells}/{total_cells}"
    );
    // The fused u16-domain run (the big lossless lever) must fire for EVERY
    // non-float-input cell: every testbed device link has at least one Curve/
    // U16-CLUT run bounded by 16-bit quantization points (the CLUT->output-curves
    // tail at minimum). Proves the lever is live across the whole sweep.
    assert_eq!(
        u16_chain_cells, nonfloat_in_cells,
        "the fused u16-domain run must fire for every non-float-input cell"
    );
    eprintln!(
        "[batched] AccurateFast == Accurate == lcms2-NOOPTIMIZE byte-for-byte: \
         {total_cells} cells, {total_pixels} pixels checked, 0 mismatches; \
         batched fired in {batched_fired_cells}/{total_cells} cells; \
         u16-domain chain fired in {u16_chain_cells}/{nonfloat_in_cells} non-float-input cells"
    );
}

/// Run a transform over `n` pixels in `chunk`-sized `do_transform` calls (the way a
/// real renderer drives a CMM: per scanline/tile/pixel). `in_bpp`/`out_bpp` are the
/// packed bytes-per-pixel of the in/out formats.
fn transform_chunked(
    xform: &Transform,
    input: &[u8],
    output: &mut [u8],
    n: usize,
    in_bpp: usize,
    out_bpp: usize,
    chunk: usize,
) {
    let mut off = 0;
    while off < n {
        let k = chunk.min(n - off);
        xform.do_transform(
            &input[off * in_bpp..(off + k) * in_bpp],
            &mut output[off * out_bpp..(off + k) * out_bpp],
            k,
        );
        off += k;
    }
}

/// The CHUNKED-CALL byte-identity proof: the threshold routing + right-sized scratch
/// must not change output. Drives `AccurateFast` in SMALL chunks (1 and 64 pixels,
/// straddling `BATCHED_THRESHOLD == 256`, so both the per-pixel fallback AND the
/// batched path with a sub-CHUNK tile are exercised) and asserts the result is still
/// byte-for-byte identical to Accurate (single-call) AND lcms2-NOOPTIMIZE. This is
/// what catches any divergence introduced by the chunked/threshold path that the
/// single-large-call sweep above cannot see.
#[test]
fn batched_chunked_calls_are_byte_identical_to_accurate_and_lcms2_nooptimize() {
    // Small chunks straddling the batched threshold: 1 (per-pixel fallback),
    // 64 (still below threshold), and 300 (above threshold => batched with a
    // sub-CHUNK tile of 300).
    const CHUNKS: &[usize] = &[1, 64, 300];

    let mut total_cells = 0usize;
    let mut mismatches = 0usize;

    for (an, in_rgb, bn, out_rgb) in pairs() {
        let ab = load(an);
        let bb = load(bn);
        let pa = Profile::open(&ab).unwrap();
        let pb = Profile::open(&bb).unwrap();

        let grid8 = if in_rgb { rgb8_sweep() } else { cmyk8_sweep() };
        let in_ch = if in_rgb { 3 } else { 4 };
        let n = grid8.len() / in_ch;
        let out_ch = if out_rgb { 3 } else { 4 };

        let (in8, in16, inflt) = if in_rgb {
            (TYPE_RGB_8, TYPE_RGB_16, TYPE_RGB_FLT)
        } else {
            (TYPE_CMYK_8, TYPE_CMYK_16, TYPE_CMYK_FLT)
        };
        let (out8, out16, outflt) = if out_rgb {
            (TYPE_RGB_8, TYPE_RGB_16, TYPE_RGB_FLT)
        } else {
            (TYPE_CMYK_8, TYPE_CMYK_16, TYPE_CMYK_FLT)
        };
        let buf16 = widen_to_16(&grid8);
        let bufflt = widen_to_float(&grid8);

        for &intent in INTENTS {
            let intents_raw = [intent.to_raw(), intent.to_raw()];
            let bpc = [false, false];
            let adapt = [1.0, 1.0];

            for &(in_fmt, in_buf, in_bpp) in &[
                (in8, &grid8, in_ch),
                (in16, &buf16, in_ch * 2),
                (inflt, &bufflt, in_ch * 4),
            ] {
                for &out_fmt in &[out8, out16, outflt] {
                    let out_bpp = if out_fmt == out8 {
                        out_ch
                    } else if out_fmt == out16 {
                        out_ch * 2
                    } else {
                        out_ch * 4
                    };

                    let mut oracle = vec![0u8; n * out_bpp];
                    let ok = tintbox_oracle::do_transform_packed(
                        &[&ab, &bb],
                        &intents_raw,
                        &bpc,
                        &adapt,
                        in_fmt,
                        out_fmt,
                        in_buf,
                        &mut oracle,
                        n,
                    );
                    assert!(ok, "lcms2-NOOPTIMIZE failed {an}->{bn}");

                    let acc = build(
                        &pa,
                        &pb,
                        intent,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::Accurate,
                    );
                    let mut acc_out = vec![0u8; n * out_bpp];
                    acc.do_transform(in_buf, &mut acc_out, n);

                    let fast = build(
                        &pa,
                        &pb,
                        intent,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::AccurateFast,
                    );

                    for &chunk in CHUNKS {
                        let mut fast_out = vec![0u8; n * out_bpp];
                        transform_chunked(&fast, in_buf, &mut fast_out, n, in_bpp, out_bpp, chunk);

                        if fast_out != oracle {
                            mismatches += 1;
                            let bad = fast_out
                                .iter()
                                .zip(&oracle)
                                .position(|(a, b)| a != b)
                                .unwrap();
                            panic!(
                                "AccurateFast(chunk={chunk}) != lcms2-NOOPTIMIZE {an}->{bn} \
                                 in={in_fmt:#x} out={out_fmt:#x} at byte {bad}: \
                                 fast={} oracle={} (mismatches {mismatches})",
                                fast_out[bad], oracle[bad],
                            );
                        }
                        if fast_out != acc_out {
                            mismatches += 1;
                            let bad = fast_out
                                .iter()
                                .zip(&acc_out)
                                .position(|(a, b)| a != b)
                                .unwrap();
                            panic!(
                                "AccurateFast(chunk={chunk}) != Accurate {an}->{bn} \
                                 in={in_fmt:#x} out={out_fmt:#x} at byte {bad}: \
                                 fast={} accurate={} (mismatches {mismatches})",
                                fast_out[bad], acc_out[bad],
                            );
                        }
                        total_cells += 1;
                    }
                }
            }
        }
    }

    assert_eq!(
        mismatches, 0,
        "found {mismatches} chunked mismatching cells"
    );
    eprintln!(
        "[batched-chunked] AccurateFast (chunks {CHUNKS:?}) == Accurate == \
         lcms2-NOOPTIMIZE byte-for-byte across {total_cells} chunked cells, 0 mismatches"
    );
}
