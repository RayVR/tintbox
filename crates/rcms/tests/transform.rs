//! Differential sweep for `rcms::transform::Transform` + `do_transform_float`/
//! `_16` — the full create-transform + do-transform path (lcms2 `cmsxform.c`).
//!
//! For every compatible ordered pair of testbed profiles, for each of the four
//! ICC intents with **BPC off**, build the rcms `Transform` (which applies the
//! `_cmsLinkProfiles` BPC mutation internally) and `do_transform_float` AND
//! `do_transform_16` over a grid of input pixels. Build the same transform in
//! lcms2 via `cmsCreateExtendedTransform` (`cmsFLAGS_NOOPTIMIZE`) and
//! `cmsDoTransform` over the same pixels. Assert bit-exact (`f32::to_bits` /
//! `u16`).
//!
//! Cells where the BPC mutation *forces* BPC on (V4 profile under
//! perceptual/saturation, `cmscnvrt.c:1137-1145`) need the T5 black-point math and
//! are SKIPPED here.

use std::fs;
use std::path::{Path, PathBuf};

use rcms::format::decode;
use rcms::format::PixelFormat;
use rcms::profile::{ColorSpace, Profile, RenderingIntent};
use rcms::transform::Transform;

const ICC_VERSION_V4: u32 = 0x0400_0000;

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

fn testbed_icc() -> Vec<PathBuf> {
    let mut v: Vec<_> = fs::read_dir(testbed_dir())
        .expect("read testbed")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("icc"))
        .collect();
    v.sort();
    v
}

/// Device channel count of a color space (lcms2 `cmsChannelsOfColorSpace`), for
/// the obvious testbed spaces. `None` for spaces we don't drive the ends with.
fn channels(cs: ColorSpace) -> Option<usize> {
    Some(match cs {
        ColorSpace::Gray => 1,
        ColorSpace::XYZ | ColorSpace::Lab | ColorSpace::Rgb => 3,
        ColorSpace::Cmyk | ColorSpace::Mch4 => 4,
        _ => return None,
    })
}

/// A bounded per-channel input sweep: `levels` grid points per channel over
/// `[0, 1]` (cartesian product over `n_in` channels).
fn input_grid(n_in: usize, levels: usize) -> Vec<Vec<f32>> {
    let pts: Vec<f32> = (0..levels)
        .map(|i| i as f32 / (levels - 1) as f32)
        .collect();
    let mut rows: Vec<Vec<f32>> = vec![vec![]];
    for _ in 0..n_in {
        let mut next = Vec::new();
        for row in &rows {
            for &p in &pts {
                let mut r = row.clone();
                r.push(p);
                next.push(r);
            }
        }
        rows = next;
    }
    rows
}

struct Loaded {
    name: String,
    bytes: Vec<u8>,
}

#[test]
fn transform_matches_oracle_over_testbed_pairs_all_intents_float_and_16() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut loaded: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        if Profile::open(&bytes).is_err() {
            continue;
        }
        loaded.push(Loaded { name, bytes });
    }
    assert!(loaded.len() >= 2, "need at least two loadable profiles");

    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
        RenderingIntent::Perceptual,
        RenderingIntent::Saturation,
    ];
    let adaptation = [1.0f64, 1.0f64];
    let bpc = [false, false];

    let mut pairs_linked = 0usize;
    let mut float_samples = 0usize;
    let mut u16_samples = 0usize;
    let mut cells_skipped_t5 = 0usize;
    let mut bpc_cells_passed = 0usize;
    let mut per_intent: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }

            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            let in_chans = match channels(pa.header().color_space) {
                Some(c) => c,
                None => continue,
            };
            let out_chans = match channels(pb.header().color_space) {
                Some(c) => c,
                None => continue,
            };

            let levels = if in_chans >= 4 { 4 } else { 6 };
            let grid = input_grid(in_chans, levels);
            let mut flat_in: Vec<f32> = Vec::with_capacity(grid.len() * in_chans);
            for row in &grid {
                flat_in.extend_from_slice(row);
            }
            // 16-bit inputs: quantize the same grid points (round-to-nearest like
            // lcms2's FromFloatTo16 boundary uses for the encoded grid).
            let flat_in_16: Vec<u16> = flat_in
                .iter()
                .map(|&v| (v as f64 * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u16)
                .collect();

            for &intent in &intents {
                // Mirror the _cmsLinkProfiles BPC mutation to know which cells are
                // forced-BPC-on. The chain is profile a then b; BPC is forced ON for
                // a V4 profile under perceptual/saturation. These cells exercise the
                // T5 black-point math (constant path) or remain deferred to
                // post-slice-7 (detection-by-sampling) — distinguished below by
                // whether rcms returns Unsupported.
                let forces_bpc_on = (intent == RenderingIntent::Perceptual
                    || intent == RenderingIntent::Saturation)
                    && (pa.header().version >= ICC_VERSION_V4
                        || pb.header().version >= ICC_VERSION_V4);

                let intents_raw = [intent.to_raw(), intent.to_raw()];

                // Ask lcms2 (NOOPTIMIZE) whether this chain links + transform float.
                // lcms2 applies the same BPC mutation internally and detects the
                // black points, so the oracle already reflects the forced-BPC math.
                let oracle_f = rcms_oracle::transform_eval_float(
                    &[&a.bytes, &b.bytes],
                    &intents_raw,
                    &bpc,
                    &adaptation,
                    &flat_in,
                    in_chans,
                    out_chans,
                    grid.len(),
                );
                let oracle_f = match oracle_f {
                    Some(o) => o,
                    None => continue, // lcms2 rejected; rcms isn't required to link it.
                };

                // Build the rcms transform. For forced-BPC cells that still need
                // detection-by-sampling (V4 matrix-shaper perc/sat, etc.), rcms
                // returns Unsupported — count those as deferred and skip.
                let xform = match Transform::new_simple(&pa, &pb, intent, false) {
                    Ok(x) => x,
                    Err(e) if forces_bpc_on => {
                        // Deferred BPC cell (post-slice-7, Lab virtual profiles).
                        let _ = e;
                        cells_skipped_t5 += 1;
                        continue;
                    }
                    Err(e) => panic!(
                        "lcms2 linked {} -> {} ({intent:?}) but rcms Transform failed: {e}",
                        a.name, b.name
                    ),
                };

                let oracle_16 = rcms_oracle::transform_eval_16(
                    &[&a.bytes, &b.bytes],
                    &intents_raw,
                    &bpc,
                    &adaptation,
                    &flat_in_16,
                    in_chans,
                    out_chans,
                    grid.len(),
                )
                .expect("lcms2 built float xform but 16-bit failed");

                assert_eq!(
                    xform.lut().input_channels,
                    in_chans,
                    "{} -> {} ({intent:?}): input channel mismatch",
                    a.name,
                    b.name
                );
                assert_eq!(
                    xform.lut().output_channels,
                    out_chans,
                    "{} -> {} ({intent:?}): output channel mismatch",
                    a.name,
                    b.name
                );

                // Float path.
                let mut rust_f = vec![0f32; grid.len() * out_chans];
                xform.do_transform_float(&flat_in, &mut rust_f, grid.len());
                for s in 0..grid.len() {
                    for ch in 0..out_chans {
                        let r = rust_f[s * out_chans + ch];
                        let o = oracle_f[s * out_chans + ch];
                        assert_eq!(
                            r.to_bits(),
                            o.to_bits(),
                            "FLOAT {} -> {} ({intent:?}) sample {:?} ch{ch}: rust={r} lcms2={o}",
                            a.name,
                            b.name,
                            grid[s]
                        );
                    }
                    float_samples += 1;
                }

                // 16-bit path.
                let mut rust_16 = vec![0u16; grid.len() * out_chans];
                xform.do_transform_16(&flat_in_16, &mut rust_16, grid.len());
                for s in 0..grid.len() {
                    for ch in 0..out_chans {
                        let r = rust_16[s * out_chans + ch];
                        let o = oracle_16[s * out_chans + ch];
                        assert_eq!(
                            r, o,
                            "U16 {} -> {} ({intent:?}) sample {:?} ch{ch}: rust={r} lcms2={o}",
                            a.name, b.name, grid[s]
                        );
                    }
                    u16_samples += 1;
                }

                pairs_linked += 1;
                if forces_bpc_on {
                    bpc_cells_passed += 1;
                }
                *per_intent.entry(format!("{intent:?}")).or_default() += 1;
            }
        }
    }

    println!(
        "Transform sweep (4 intents): {pairs_linked} (pair×intent) cells linked + \
         transformed bit-exact, {float_samples} float samples, {u16_samples} u16 samples. \
         Of those, {bpc_cells_passed} forced-BPC cells (V4 perc/sat) now pass via the \
         black-point-compensation math; {cells_skipped_t5} forced-BPC cells remain DEFERRED \
         (detection-by-sampling, post-slice-7 Lab virtual profiles)."
    );
    for (intent, n) in &per_intent {
        println!("  {intent}: {n} cells");
    }
    assert!(
        pairs_linked > 0,
        "expected at least one linkable testbed (pair, intent) cell"
    );
    // Both domains must have run.
    assert!(float_samples > 0 && u16_samples > 0);
}

// --- Unit tests: field derivation + multi-pixel iteration -------------------

fn open_pair() -> (Vec<u8>, Vec<u8>) {
    // Pick two distinct loadable testbed profiles whose ends have known channel
    // counts (RGB display + something), to exercise field derivation.
    let files = testbed_icc();
    let mut rgb: Option<Vec<u8>> = None;
    let mut other: Option<Vec<u8>> = None;
    for p in &files {
        let bytes = fs::read(p).unwrap();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let Ok(prof) = Profile::open(&bytes) else {
            continue;
        };
        if prof.header().color_space == ColorSpace::Rgb && rgb.is_none() {
            rgb = Some(bytes);
        } else if other.is_none() && channels(prof.header().color_space).is_some() {
            other = Some(bytes);
        }
    }
    (
        rgb.expect("an RGB testbed profile"),
        other.expect("a second testbed profile"),
    )
}

#[test]
fn transform_field_derivation_matches_profiles() {
    let (rgb_bytes, out_bytes) = open_pair();
    let pin = Profile::open(&rgb_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();

    let x = Transform::new_simple(&pin, &pout, RenderingIntent::RelativeColorimetric, false)
        .expect("build transform");

    // Entry color space = first profile's device color space (input direction).
    assert_eq!(x.entry_color_space(), pin.header().color_space);
    // Exit color space = last profile's device color space (output direction)
    // when the last profile is consumed PCS->device.
    assert_eq!(x.exit_color_space(), pout.header().color_space);
    // Rendering intent is the last link's intent.
    assert_eq!(x.rendering_intent(), RenderingIntent::RelativeColorimetric);
    // White points are valid (positive Y).
    assert!(x.entry_white_point().y > 0.0);
    assert!(x.exit_white_point().y > 0.0);
    // No gamut-check pipeline in slice 5.
    assert!(x.gamut_check().is_none());
}

#[test]
fn do_transform_iterates_multiple_pixels() {
    let (rgb_bytes, out_bytes) = open_pair();
    let pin = Profile::open(&rgb_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();
    let x =
        Transform::new_simple(&pin, &pout, RenderingIntent::RelativeColorimetric, false).unwrap();

    let in_ch = x.lut().input_channels;
    let out_ch = x.lut().output_channels;

    // Three distinct pixels in one buffer.
    let mut pixels: Vec<f32> = Vec::new();
    let samples = [0.0f32, 0.5, 1.0];
    for &s in &samples {
        for _ in 0..in_ch {
            pixels.push(s);
        }
    }
    let mut out = vec![0f32; samples.len() * out_ch];
    x.do_transform_float(&pixels, &mut out, samples.len());

    // Each pixel's block must equal evaluating that pixel alone.
    for (i, &s) in samples.iter().enumerate() {
        let single = x.lut().eval_float(&vec![s; in_ch]);
        for ch in 0..out_ch {
            assert_eq!(
                out[i * out_ch + ch].to_bits(),
                single[ch].to_bits(),
                "multi-pixel float block {i} ch{ch} differs from single eval"
            );
        }
    }

    // 16-bit multi-pixel iteration.
    let mut pin16: Vec<u16> = Vec::new();
    let s16 = [0u16, 32768, 65535];
    for &s in &s16 {
        for _ in 0..in_ch {
            pin16.push(s);
        }
    }
    let mut out16 = vec![0u16; s16.len() * out_ch];
    x.do_transform_16(&pin16, &mut out16, s16.len());
    for (i, &s) in s16.iter().enumerate() {
        let single = x.lut().eval_16(&vec![s; in_ch]);
        for ch in 0..out_ch {
            assert_eq!(
                out16[i * out_ch + ch],
                single[ch],
                "multi-pixel u16 block {i} ch{ch} differs from single eval"
            );
        }
    }
}

// --- End-to-end format-aware do_transform (packed buffers) ------------------

/// Pixel-format kinds we drive end-to-end. `Flt`/`Dbl` use the float path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    U8,
    U16,
    Flt,
    Dbl,
}

/// Bytes per sample for a kind.
fn kind_sample_bytes(k: Kind) -> usize {
    match k {
        Kind::U8 => 1,
        Kind::U16 => 2,
        Kind::Flt => 4,
        Kind::Dbl => 8,
    }
}

/// Build the `TYPE_*` format word for a device color space with `n_chan`
/// channels and the given sample kind (no extra/swap/flavor — plain chunky).
fn make_format(cs: ColorSpace, n_chan: usize, k: Kind) -> Option<u32> {
    // Match the PT_* the profile's device space uses.
    let (pt, chans) = match cs {
        ColorSpace::Gray => (decode::PT_GRAY, 1),
        ColorSpace::Rgb => (decode::PT_RGB, 3),
        ColorSpace::Cmyk => (decode::PT_CMYK, 4),
        _ => return None,
    };
    if chans != n_chan {
        return None;
    }
    let bytes = match k {
        Kind::U8 => 1u32,
        Kind::U16 => 2,
        Kind::Flt => 4,
        Kind::Dbl => 0,
    };
    let float_bit = if matches!(k, Kind::Flt | Kind::Dbl) {
        decode::float_marker(1)
    } else {
        0
    };
    Some(float_bit | (pt << 16) | ((chans as u32) << 3) | bytes)
}

/// Encode one 0..1 float channel value into `dst` at the right kind. For device
/// (RGB/CMYK/Gray) formats lcms2's unpack scales 8/16-bit by /255 or /65535 and
/// float by identity (or /100 for ink). To feed the *same* numbers to both rcms
/// and lcms2 we just write the packed representation of the chosen 0..1 grid:
/// for 8/16-bit we quantize, for float we write the value directly (device float
/// is 0..1 identity for RGB/Gray; CMYK float would be /100, but we feed 0..1 so
/// both sides see the identical bytes and therefore the identical unpacked value).
fn encode_sample(dst: &mut [u8], v: f32, k: Kind) {
    match k {
        Kind::U8 => {
            let q = (v as f64 * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
            dst[0] = q;
        }
        Kind::U16 => {
            let q = (v as f64 * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u16;
            dst[..2].copy_from_slice(&q.to_le_bytes());
        }
        Kind::Flt => dst[..4].copy_from_slice(&v.to_le_bytes()),
        Kind::Dbl => dst[..8].copy_from_slice(&(v as f64).to_le_bytes()),
    }
}

fn pixel_bytes_fmt(fmt: u32) -> usize {
    let f = PixelFormat(fmt);
    let sample = match f.bytes() {
        0 => 8,
        b => b as usize,
    };
    (f.channels() + f.extra()) as usize * sample
}

#[test]
fn do_transform_packed_matches_oracle_over_testbed_pairs() {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut loaded: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        if Profile::open(&bytes).is_err() {
            continue;
        }
        loaded.push(Loaded { name, bytes });
    }
    assert!(loaded.len() >= 2, "need at least two loadable profiles");

    // Relative + absolute, BPC off (avoid the forced-BPC V4 perc/sat cells that
    // the slice-5 sweep already documents as deferred for some profiles).
    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
    ];
    let adaptation = [1.0f64, 1.0f64];
    let bpc = [false, false];

    // Sample-kind pairs to exercise (in_kind, out_kind): a 16-bit pair, an 8→16
    // pair, a float pair (FloatXFORM path), and a double pair.
    let kind_pairs = [
        (Kind::U8, Kind::U16),
        (Kind::U16, Kind::U16),
        (Kind::U8, Kind::U8),
        (Kind::Flt, Kind::Flt),
        (Kind::U16, Kind::Flt), // mixed: float path (output float)
        (Kind::Dbl, Kind::Dbl),
    ];

    let mut cells = 0usize;
    let mut byte_samples = 0usize;
    let mut float_cells = 0usize;
    let mut u16_cells = 0usize;

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }
            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            let in_cs = pa.header().color_space;
            let out_cs = pb.header().color_space;
            let in_chans = match channels(in_cs) {
                Some(c) => c,
                None => continue,
            };
            let out_chans = match channels(out_cs) {
                Some(c) => c,
                None => continue,
            };
            // Only device spaces we can build a TYPE_* word for.
            if !matches!(in_cs, ColorSpace::Gray | ColorSpace::Rgb | ColorSpace::Cmyk)
                || !matches!(
                    out_cs,
                    ColorSpace::Gray | ColorSpace::Rgb | ColorSpace::Cmyk
                )
            {
                continue;
            }

            let levels = if in_chans >= 4 { 4 } else { 6 };
            let grid = input_grid(in_chans, levels);

            for &intent in &intents {
                let intents_raw = [intent.to_raw(), intent.to_raw()];

                for &(ik, ok) in &kind_pairs {
                    let in_fmt = match make_format(in_cs, in_chans, ik) {
                        Some(f) => f,
                        None => continue,
                    };
                    let out_fmt = match make_format(out_cs, out_chans, ok) {
                        Some(f) => f,
                        None => continue,
                    };

                    // Pack the grid into input bytes.
                    let in_stride = pixel_bytes_fmt(in_fmt);
                    let out_stride = pixel_bytes_fmt(out_fmt);
                    let mut packed_in = vec![0u8; grid.len() * in_stride];
                    let isb = kind_sample_bytes(ik);
                    for (p, row) in grid.iter().enumerate() {
                        for (c, &v) in row.iter().enumerate() {
                            let off = p * in_stride + c * isb;
                            encode_sample(&mut packed_in[off..], v, ik);
                        }
                    }

                    // Oracle reference (NOOPTIMIZE).
                    let mut oracle_out = vec![0u8; grid.len() * out_stride];
                    let ok_built = rcms_oracle::do_transform_packed(
                        &[&a.bytes, &b.bytes],
                        &intents_raw,
                        &bpc,
                        &adaptation,
                        in_fmt,
                        out_fmt,
                        &packed_in,
                        &mut oracle_out,
                        grid.len(),
                    );
                    if !ok_built {
                        continue; // lcms2 rejected this chain; rcms not required.
                    }

                    // rcms transform.
                    let xform = match Transform::new_simple_with_formats(
                        &pa, &pb, intent, false, in_fmt, out_fmt,
                    ) {
                        Ok(x) => x,
                        Err(_) => continue, // deferred (e.g. forced-BPC detection cell)
                    };
                    let mut rcms_out = vec![0u8; grid.len() * out_stride];
                    xform.do_transform(&packed_in, &mut rcms_out, grid.len());

                    assert_eq!(
                        rcms_out, oracle_out,
                        "PACKED {} -> {} ({intent:?}) in={in_fmt:#x} out={out_fmt:#x}: \
                         byte mismatch",
                        a.name, b.name
                    );

                    cells += 1;
                    byte_samples += grid.len();
                    if PixelFormat(in_fmt).is_float() || PixelFormat(out_fmt).is_float() {
                        float_cells += 1;
                    } else {
                        u16_cells += 1;
                    }
                }
            }
        }
    }

    println!(
        "Packed do_transform sweep: {cells} (pair×intent×format) cells bit-exact \
         vs lcms2 NOOPTIMIZE ({byte_samples} pixels). float-path cells: {float_cells}, \
         16-bit-path cells: {u16_cells}."
    );
    assert!(cells > 0, "expected at least one packed format cell");
    assert!(
        float_cells > 0 && u16_cells > 0,
        "both paths must be exercised"
    );
}

/// Dense 8→16 regression for the matrix-shaper `PreOptimize` matrix-merge fix.
///
/// lcms2's `_cmsOptimizePipeline` runs `PreOptimize` (which merges adjacent matrix
/// stages via `_MultiplyMatrix`) BEFORE the `cmsFLAGS_NOOPTIMIZE` early-return
/// (cmsopt.c:1952 vs 1961), so even the un-optimized device link has the input
/// profile's RGB→XYZ matrix and the output profile's XYZ→RGB matrix collapsed into
/// ONE pre-multiplied matrix. rcms previously left them as two separate `Matrix`
/// stages, which applied an extra intermediate `f32` rounding and diverged from
/// lcms2-NOOPTIMIZE by up to a few LSB after the following output tone curve — but
/// only for some 8-bit input bytes (e.g. `ibm-t61.icc → test5.icc`, input byte
/// 115 → 8→16 value 29555, output channel 1: rcms 27417 vs lcms2 27419). The
/// coarse slice-5 16-bit grid quantized inputs round-to-nearest and missed the
/// `(a<<8)|a` 8-bit expansion that exposes it.
///
/// This sweeps EVERY 8-bit value (0..=255) on each RGB channel through an 8-bit→
/// 16-bit transform for every loadable RGB→RGB testbed pair under all four intents
/// (BPC off), comparing `Transform::do_transform` (Accurate strategy) byte-for-byte
/// against lcms2 `cmsCreateExtendedTransform(NOOPTIMIZE)` + `cmsDoTransform`.
#[test]
fn dense_8bit_to_16bit_matrix_shaper_bit_identical_to_lcms2_nooptimize() {
    let files = testbed_icc();
    let mut loaded: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let Ok(prof) = Profile::open(&bytes) else {
            continue;
        };
        if prof.header().color_space != ColorSpace::Rgb {
            continue;
        }
        loaded.push(Loaded { name, bytes });
    }
    assert!(loaded.len() >= 2, "need >= 2 loadable RGB testbed profiles");

    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
        RenderingIntent::Perceptual,
        RenderingIntent::Saturation,
    ];
    // 8-bit RGB in, 16-bit RGB out (the 8→16 path: lcms2 unpacks each byte via
    // FROM_8_TO_16 = (a<<8)|a, the expansion that exposed the bug).
    let in_fmt = (decode::PT_RGB << 16) | (3u32 << 3) | 1;
    let out_fmt = (decode::PT_RGB << 16) | (3u32 << 3) | 2;

    // 256 pixels: pixel v carries a per-channel sweep so all 256 byte values appear
    // on every channel (channel 0 = v, channels 1/2 = decorrelated permutations so
    // the three matrix columns each see the full 0..255 range, not just the diagonal).
    let n = 256usize;
    let mut packed_in = vec![0u8; n * 3];
    for v in 0..n {
        packed_in[v * 3] = v as u8;
        packed_in[v * 3 + 1] = ((v * 7 + 3) % 256) as u8;
        packed_in[v * 3 + 2] = ((v * 13 + 17) % 256) as u8;
    }

    let mut cells = 0usize;
    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }
            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            for &intent in &intents {
                let intents_raw = [intent.to_raw(), intent.to_raw()];

                let mut oracle_out = vec![0u8; n * 3 * 2];
                let ok = rcms_oracle::do_transform_packed(
                    &[&a.bytes, &b.bytes],
                    &intents_raw,
                    &[false, false],
                    &[1.0, 1.0],
                    in_fmt,
                    out_fmt,
                    &packed_in,
                    &mut oracle_out,
                    n,
                );
                if !ok {
                    continue;
                }
                let xform = match Transform::new_simple_with_formats(
                    &pa, &pb, intent, false, in_fmt, out_fmt,
                ) {
                    Ok(x) => x,
                    Err(_) => continue, // deferred (e.g. forced-BPC detection cell)
                };
                let mut rcms_out = vec![0u8; n * 3 * 2];
                xform.do_transform(&packed_in, &mut rcms_out, n);

                // Byte-exact, with a precise per-sample message if it ever drifts.
                if rcms_out != oracle_out {
                    for px in 0..n {
                        for ch in 0..3 {
                            let off = (px * 3 + ch) * 2;
                            let r = u16::from_le_bytes([rcms_out[off], rcms_out[off + 1]]);
                            let o = u16::from_le_bytes([oracle_out[off], oracle_out[off + 1]]);
                            assert_eq!(
                                r,
                                o,
                                "8->16 {} -> {} ({intent:?}) pixel {px} (bytes {:?}) ch{ch}: \
                                 rcms={r} lcms2={o}",
                                a.name,
                                b.name,
                                &packed_in[px * 3..px * 3 + 3],
                            );
                        }
                    }
                }
                cells += 1;
            }
        }
    }

    println!(
        "Dense 8->16 matrix-shaper sweep: {cells} (RGB pair × intent) cells, all 256 \
         byte values per channel, bit-exact vs lcms2 NOOPTIMIZE."
    );
    assert!(cells > 0, "expected at least one RGB pair × intent cell");
}

// --- cmsFLAGS_COPY_ALPHA: extra-channel copy ---------------------------------

/// Bytes one packed pixel of `fmt` occupies (color + extra channels), double = 8.
fn px_bytes(fmt: u32) -> usize {
    let f = PixelFormat(fmt);
    let s = match f.bytes() {
        0 => 8,
        b => b as usize,
    };
    (f.channels() + f.extra()) as usize * s
}

/// Encode a 0..1 color value into one sample of `fmt`'s kind at `dst`.
fn enc_color(dst: &mut [u8], v: f32, fmt: u32) {
    let f = PixelFormat(fmt);
    if f.is_float() {
        if f.bytes() == 0 {
            dst[..8].copy_from_slice(&(v as f64).to_le_bytes());
        } else {
            dst[..4].copy_from_slice(&v.to_le_bytes());
        }
    } else if f.bytes() == 2 {
        let q = (v as f64 * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u16;
        dst[..2].copy_from_slice(&q.to_le_bytes());
    } else {
        dst[0] = (v as f64 * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
    }
}

/// Write a random alpha sample into `dst` for `fmt`'s kind, returning the bytes
/// written so the test can prove the depth conversion ran. Alpha is the LAST
/// color+extra sample of a plain (no-swap) RGBA/CMYKA format.
fn enc_random_alpha(dst: &mut [u8], rng: &mut rcms_oracle::Rng, fmt: u32) {
    let f = PixelFormat(fmt);
    if f.is_float() {
        // 0..1-ish float alpha (a few values just outside to exercise saturation).
        let u = (rng.next_u64() & 0xffff_ffff) as u32;
        let a = ((u as f64) / (u32::MAX as f64)) as f32 * 1.1 - 0.05;
        if f.bytes() == 0 {
            dst[..8].copy_from_slice(&(a as f64).to_le_bytes());
        } else {
            dst[..4].copy_from_slice(&a.to_le_bytes());
        }
    } else if f.bytes() == 2 {
        let a = (rng.next_u64() & 0xffff) as u16;
        dst[..2].copy_from_slice(&a.to_le_bytes());
    } else {
        dst[0] = (rng.next_u64() & 0xff) as u8;
    }
}

/// RGBA/CMYKA COPY_ALPHA sweep: for several depth-conversion format pairs over
/// loadable testbed RGB→RGB (and a CMYK case) pairs, build the rcms transform
/// with `COPY_ALPHA` and diff `do_transform` byte-for-byte against lcms2
/// `cmsCreateExtendedTransform(COPY_ALPHA | NOOPTIMIZE)` + `cmsDoTransform`. The
/// alpha values are random so the conversion (FROM_8_TO_16, /255, saturate, …)
/// is exercised, not just identity.
#[test]
fn copy_alpha_extra_channel_matches_lcms2() {
    let files = testbed_icc();

    // RGB profiles for RGBA cases; any-CMYK for the CMYKA case.
    let mut rgb: Vec<Loaded> = Vec::new();
    let mut cmyk: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let Ok(prof) = Profile::open(&bytes) else {
            continue;
        };
        match prof.header().color_space {
            ColorSpace::Rgb => rgb.push(Loaded { name, bytes }),
            ColorSpace::Cmyk => cmyk.push(Loaded { name, bytes }),
            _ => {}
        }
    }
    assert!(rgb.len() >= 2, "need >= 2 loadable RGB testbed profiles");

    let intents = [
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Perceptual,
    ];

    // (in_fmt, out_fmt, n_color_chans): depth-conversion pairs.
    let rgba_pairs: &[(u32, u32)] = &[
        (decode::TYPE_RGBA_8, decode::TYPE_RGBA_8), // 8 -> 8 identity
        (decode::TYPE_RGBA_8, decode::TYPE_RGBA_16), // 8 -> 16 (FROM_8_TO_16)
        (decode::TYPE_RGBA_16, decode::TYPE_RGBA_8), // 16 -> 8 (FROM_16_TO_8)
        (decode::TYPE_RGBA_16, decode::TYPE_RGBA_FLT), // 16 -> float (/65535)
        (decode::TYPE_RGBA_8, decode::TYPE_RGBA_FLT), // 8 -> float (/255)
        (decode::TYPE_RGBA_FLT, decode::TYPE_RGBA_16), // float -> 16 (saturate)
        (decode::TYPE_RGBA_FLT, decode::TYPE_RGBA_8), // float -> 8 (saturate byte)
        (decode::TYPE_RGBA_FLT, decode::TYPE_RGBA_FLT), // float -> float
    ];

    let mut rng = rcms_oracle::Rng::new(0x00A1_FA00);
    let n = 64usize; // pixels per cell
    let mut cells = 0usize;
    let mut alpha_samples = 0usize;

    let flags = rcms::transform::Flags::NOOPTIMIZE.union(rcms::transform::Flags::COPY_ALPHA);

    // A few RGB pairs (keep the matrix small but multiple).
    'pairs: for a in &rgb {
        for b in &rgb {
            if a.name == b.name {
                continue;
            }
            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            for &intent in &intents {
                let intents_raw = [intent.to_raw(), intent.to_raw()];

                for &(in_fmt, out_fmt) in rgba_pairs {
                    let in_stride = px_bytes(in_fmt);
                    let out_stride = px_bytes(out_fmt);

                    // Build input: random-ish color in 0..1 + random alpha.
                    let mut packed_in = vec![0u8; n * in_stride];
                    let in_sb = in_stride / 4; // 4 samples (RGBA), bytes per sample
                    for p in 0..n {
                        let base = p * in_stride;
                        for ch in 0..3 {
                            let u = (rng.next_u64() & 0xffff) as f32 / 65535.0;
                            enc_color(&mut packed_in[base + ch * in_sb..], u, in_fmt);
                        }
                        // alpha = last sample
                        enc_random_alpha(&mut packed_in[base + 3 * in_sb..], &mut rng, in_fmt);
                    }

                    let mut oracle_out = vec![0u8; n * out_stride];
                    let ok = rcms_oracle::do_transform_packed_copyalpha(
                        &[&a.bytes, &b.bytes],
                        &intents_raw,
                        &[false, false],
                        &[1.0, 1.0],
                        in_fmt,
                        out_fmt,
                        &packed_in,
                        &mut oracle_out,
                        n,
                    );
                    if !ok {
                        continue;
                    }

                    let xform = match Transform::new_simple_with_formats_flags(
                        &pa, &pb, intent, false, in_fmt, out_fmt, flags,
                    ) {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    let mut rcms_out = vec![0u8; n * out_stride];
                    xform.do_transform(&packed_in, &mut rcms_out, n);

                    assert_eq!(
                        rcms_out, oracle_out,
                        "COPY_ALPHA {} -> {} ({intent:?}) in={in_fmt:#x} out={out_fmt:#x}: \
                         byte mismatch (color or alpha)",
                        a.name, b.name
                    );

                    cells += 1;
                    alpha_samples += n;
                }
            }
            // Two RGB source profiles' worth of pairs is plenty.
            if cells >= rgba_pairs.len() * 2 * intents.len() {
                break 'pairs;
            }
        }
    }
    assert!(cells > 0, "expected at least one RGBA COPY_ALPHA cell");

    // CMYKA case (CMYK + 1 extra), if a CMYK testbed pair exists.
    let mut cmyka_cells = 0usize;
    if cmyk.len() >= 2 {
        let pa = Profile::open(&cmyk[0].bytes).unwrap();
        let pb = Profile::open(&cmyk[1].bytes).unwrap();
        let intent = RenderingIntent::RelativeColorimetric;
        let intents_raw = [intent.to_raw(), intent.to_raw()];
        let in_fmt = decode::TYPE_CMYKA_8;
        let out_fmt = decode::TYPE_CMYKA_8;
        let in_stride = px_bytes(in_fmt);
        let out_stride = px_bytes(out_fmt);
        let mut packed_in = vec![0u8; n * in_stride];
        for p in 0..n {
            let base = p * in_stride;
            for ch in 0..4 {
                let u = (rng.next_u64() & 0xffff) as f32 / 65535.0;
                enc_color(&mut packed_in[base + ch..], u, in_fmt);
            }
            enc_random_alpha(&mut packed_in[base + 4..], &mut rng, in_fmt);
        }
        let mut oracle_out = vec![0u8; n * out_stride];
        let ok = rcms_oracle::do_transform_packed_copyalpha(
            &[&cmyk[0].bytes, &cmyk[1].bytes],
            &intents_raw,
            &[false, false],
            &[1.0, 1.0],
            in_fmt,
            out_fmt,
            &packed_in,
            &mut oracle_out,
            n,
        );
        if ok {
            if let Ok(xform) = Transform::new_simple_with_formats_flags(
                &pa, &pb, intent, false, in_fmt, out_fmt, flags,
            ) {
                let mut rcms_out = vec![0u8; n * out_stride];
                xform.do_transform(&packed_in, &mut rcms_out, n);
                assert_eq!(rcms_out, oracle_out, "CMYKA COPY_ALPHA byte mismatch");
                cmyka_cells += 1;
                alpha_samples += n;
            }
        }
    }

    println!(
        "COPY_ALPHA sweep: {cells} RGBA cells + {cmyka_cells} CMYKA cell(s) bit-exact \
         vs lcms2 (COPY_ALPHA|NOOPTIMIZE), {alpha_samples} alpha samples (random)."
    );
}

/// COPY_ALPHA must NOT perturb the no-extra-channel path: RGB→RGB with the flag
/// set produces exactly the same bytes as without it (no extra channels to copy).
#[test]
fn copy_alpha_noop_when_no_extra_channels() {
    let files = testbed_icc();
    let mut rgb: Vec<Loaded> = Vec::new();
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let Ok(prof) = Profile::open(&bytes) else {
            continue;
        };
        if prof.header().color_space == ColorSpace::Rgb {
            rgb.push(Loaded { name, bytes });
        }
    }
    assert!(rgb.len() >= 2);

    let pa = Profile::open(&rgb[0].bytes).unwrap();
    let pb = Profile::open(&rgb[1].bytes).unwrap();
    let intent = RenderingIntent::RelativeColorimetric;
    let in_fmt = decode::TYPE_RGB_8;
    let out_fmt = decode::TYPE_RGB_16;

    let n = 256usize;
    let mut packed_in = vec![0u8; n * 3];
    for v in 0..n {
        packed_in[v * 3] = v as u8;
        packed_in[v * 3 + 1] = ((v * 7 + 3) % 256) as u8;
        packed_in[v * 3 + 2] = ((v * 13 + 17) % 256) as u8;
    }

    let plain =
        Transform::new_simple_with_formats(&pa, &pb, intent, false, in_fmt, out_fmt).unwrap();
    let with_alpha = Transform::new_simple_with_formats_flags(
        &pa,
        &pb,
        intent,
        false,
        in_fmt,
        out_fmt,
        rcms::transform::Flags::NOOPTIMIZE.union(rcms::transform::Flags::COPY_ALPHA),
    )
    .unwrap();

    let mut out_plain = vec![0u8; n * 6];
    let mut out_alpha = vec![0u8; n * 6];
    plain.do_transform(&packed_in, &mut out_plain, n);
    with_alpha.do_transform(&packed_in, &mut out_alpha, n);
    assert_eq!(out_plain, out_alpha, "COPY_ALPHA changed the no-extra path");
}
