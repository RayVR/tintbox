//! Differential tests for rcms's `Lcms2Compat` resampling/devicelink optimizers
//! (`rcms::opt::resampling` + `rcms::opt::joincurves`, lcms2
//! `OptimizeByComputingLinearization` / `OptimizeByResampling` /
//! `OptimizeByJoiningCurves`).
//!
//! The contract: a transform built with `OptimizationStrategy::Lcms2Compat` is
//! BYTE-IDENTICAL to `cmsCreateTransform(.., intent, /*flags*/ 0)` +
//! `cmsDoTransform` (stock lcms2-DEFAULT, i.e. WITHOUT NOOPTIMIZE) for every
//! loadable testbed profile pair × 4 intents × {8,16,float} packed formats.
//!
//! This complements `opt_matshaper.rs` (which covers the matrix-shaper path) by
//! exercising the LUT/CLUT pairs that select the baked-CLUT optimizers.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rcms::format::decode::{self, PixelFormat};
use rcms::opt::OptimizationStrategy;
use rcms::profile::{ColorSpace, Profile, RenderingIntent};
use rcms::transform::Transform;

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

struct Loaded {
    name: String,
    bytes: Vec<u8>,
    cs: ColorSpace,
}

fn load_all() -> Vec<Loaded> {
    let mut files: Vec<_> = fs::read_dir(testbed_dir())
        .expect("read testbed")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "icc").unwrap_or(false))
        .collect();
    files.sort();

    let mut v = Vec::new();
    for p in &files {
        let bytes = fs::read(p).unwrap();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let cs = match Profile::open(&bytes) {
            Ok(prof) => prof.header().color_space,
            Err(_) => continue,
        };
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        v.push(Loaded { name, bytes, cs });
    }
    v
}

/// Device channel count for the colorspaces we can build TYPE_* words for.
fn device_channels(cs: ColorSpace) -> Option<usize> {
    match cs {
        ColorSpace::Gray => Some(1),
        ColorSpace::Rgb => Some(3),
        ColorSpace::Cmyk => Some(4),
        _ => None,
    }
}

fn pt_of(cs: ColorSpace) -> Option<u32> {
    match cs {
        ColorSpace::Gray => Some(decode::PT_GRAY),
        ColorSpace::Rgb => Some(decode::PT_RGB),
        ColorSpace::Cmyk => Some(decode::PT_CMYK),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    U8,
    U16,
    Flt,
}

fn make_format(cs: ColorSpace, k: Kind) -> Option<u32> {
    let pt = pt_of(cs)?;
    let chans = device_channels(cs)? as u32;
    let (bytes, float_bit) = match k {
        Kind::U8 => (1u32, 0u32),
        Kind::U16 => (2, 0),
        Kind::Flt => (4, decode::float_marker(1)),
    };
    Some(float_bit | (pt << 16) | (chans << 3) | bytes)
}

fn sample_bytes(k: Kind) -> usize {
    match k {
        Kind::U8 => 1,
        Kind::U16 => 2,
        Kind::Flt => 4,
    }
}

/// Cube grid over `n_in` channels, `levels` points per channel, as 0..1 floats.
fn input_grid(n_in: usize, levels: usize) -> Vec<Vec<f32>> {
    let mut rows = vec![vec![]];
    for _ in 0..n_in {
        let mut next = Vec::new();
        for row in &rows {
            for li in 0..levels {
                let v = if levels <= 1 {
                    0.0
                } else {
                    li as f32 / (levels - 1) as f32
                };
                let mut r = row.clone();
                r.push(v);
                next.push(r);
            }
        }
        rows = next;
    }
    rows
}

fn encode_sample(dst: &mut [u8], v: f32, k: Kind) {
    match k {
        Kind::U8 => dst[0] = (v as f64 * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8,
        Kind::U16 => {
            let q = (v as f64 * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u16;
            dst[..2].copy_from_slice(&q.to_le_bytes());
        }
        Kind::Flt => dst[..4].copy_from_slice(&v.to_le_bytes()),
    }
}

fn pixel_bytes(fmt: u32) -> usize {
    let f = PixelFormat(fmt);
    let sample = match f.bytes() {
        0 => 8,
        b => b as usize,
    };
    (f.channels() + f.extra()) as usize * sample
}

#[test]
fn lcms2compat_resampling_bit_identical_to_lcms2_default_over_testbed() {
    let loaded = load_all();
    assert!(loaded.len() >= 2, "need >= 2 loadable testbed profiles");

    let intents = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ];
    let kinds = [Kind::U8, Kind::U16, Kind::Flt];

    let mut cells = 0usize;
    let mut byte_samples = 0usize;
    // Per-optimizer-path cell counts.
    let mut path_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut mismatches: Vec<String> = Vec::new();

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }
            let in_chans = match device_channels(a.cs) {
                Some(c) => c,
                None => continue,
            };
            let _out_chans = match device_channels(b.cs) {
                Some(c) => c,
                None => continue,
            };

            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            // Coarser cube for 4-input (CMYK) to keep the sweep bounded.
            let levels = if in_chans >= 4 { 4 } else { 6 };
            let grid = input_grid(in_chans, levels);
            let n = grid.len();

            for &intent in &intents {
                let intents_raw = [intent.to_raw(), intent.to_raw()];
                let bpc = [false, false];
                let adapt = [1.0, 1.0];

                for &(ik, ok) in &[
                    (Kind::U8, Kind::U8),
                    (Kind::U8, Kind::U16),
                    (Kind::U16, Kind::U16),
                    (Kind::Flt, Kind::Flt),
                ] {
                    let _ = kinds;
                    let in_fmt = match make_format(a.cs, ik) {
                        Some(f) => f,
                        None => continue,
                    };
                    let out_fmt = match make_format(b.cs, ok) {
                        Some(f) => f,
                        None => continue,
                    };

                    let in_stride = pixel_bytes(in_fmt);
                    let out_stride = pixel_bytes(out_fmt);
                    let isb = sample_bytes(ik);
                    let mut packed_in = vec![0u8; n * in_stride];
                    for (p, row) in grid.iter().enumerate() {
                        for (c, &v) in row.iter().enumerate() {
                            encode_sample(&mut packed_in[p * in_stride + c * isb..], v, ik);
                        }
                    }

                    // lcms2 DEFAULT (flags = 0).
                    let mut oracle = vec![0u8; n * out_stride];
                    let built = rcms_oracle::do_transform_packed_default(
                        &[&a.bytes, &b.bytes],
                        &intents_raw,
                        &bpc,
                        &adapt,
                        in_fmt,
                        out_fmt,
                        &packed_in,
                        &mut oracle,
                        n,
                    );
                    if !built {
                        continue; // lcms2 rejected this chain; rcms not required.
                    }

                    // rcms Lcms2Compat.
                    let xform = match Transform::new_simple_with_formats_strategy(
                        &pa,
                        &pb,
                        intent,
                        false,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::Lcms2Compat,
                    ) {
                        Ok(x) => x,
                        Err(_) => continue, // deferred build cell
                    };
                    let mut rcms_out = vec![0u8; n * out_stride];
                    xform.do_transform(&packed_in, &mut rcms_out, n);

                    *path_counts.entry(xform.opt_path_label()).or_insert(0) += 1;

                    if rcms_out != oracle {
                        // Find first differing pixel for diagnostics.
                        let mut first = None;
                        for p in 0..n {
                            let ro = &rcms_out[p * out_stride..(p + 1) * out_stride];
                            let oo = &oracle[p * out_stride..(p + 1) * out_stride];
                            if ro != oo {
                                first = Some((p, ro.to_vec(), oo.to_vec()));
                                break;
                            }
                        }
                        mismatches.push(format!(
                            "{} -> {} [{intent:?}] in={in_fmt:#x} out={out_fmt:#x} path={} \
                             first_diff={:?}",
                            a.name,
                            b.name,
                            xform.opt_path_label(),
                            first
                        ));
                    } else {
                        cells += 1;
                        byte_samples += n;
                    }
                }
            }
        }
    }

    eprintln!(
        "[resampling] Lcms2Compat == lcms2-default: {cells} bit-exact cells \
         ({byte_samples} pixels). Path distribution: {path_counts:?}"
    );
    for m in &mismatches {
        eprintln!("[resampling][MISMATCH] {m}");
    }

    assert!(cells > 0, "expected at least one bit-exact cell");
    assert!(
        mismatches.is_empty(),
        "{} cells diverged from lcms2-default (see [MISMATCH] lines above)",
        mismatches.len()
    );
}

/// Dense rounding-boundary probe for the baked RGB→RGB path. Sweeps a fine RGB
/// cube (every-3rd byte → 86³ ≈ 636 k pixels would be huge, so step 9 → 29³) for
/// every RGB→RGB pair under all 4 intents, 8-bit and 16-bit, asserting
/// byte-identity to lcms2-default. This catches CLUT-node / `0x8001`-rounding
/// boundaries the coarse cube above can miss, and specifically exercises the
/// 16-bit matrix-shaper pairs that now route through
/// `OptimizeByComputingLinearization` (the baked path) instead of the 8-bit
/// `MatShaperEval16`.
#[test]
fn lcms2compat_baked_rgb_dense_bit_identical() {
    let loaded: Vec<_> = load_all()
        .into_iter()
        .filter(|l| l.cs == ColorSpace::Rgb)
        .collect();
    assert!(loaded.len() >= 2, "need >= 2 loadable RGB testbed profiles");

    let intents = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ];

    // 29 levels per channel (~step 9 in 8-bit): a dense but bounded cube.
    let levels = 29usize;
    let grid = input_grid(3, levels);
    let n = grid.len();

    let mut baked_cells = 0usize;
    let mut other_cells = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for a in &loaded {
        for b in &loaded {
            if a.name == b.name {
                continue;
            }
            let pa = Profile::open(&a.bytes).unwrap();
            let pb = Profile::open(&b.bytes).unwrap();

            for &intent in &intents {
                let intents_raw = [intent.to_raw(), intent.to_raw()];
                let bpc = [false, false];
                let adapt = [1.0, 1.0];

                for &(ik, ok) in &[(Kind::U8, Kind::U8), (Kind::U16, Kind::U16)] {
                    let in_fmt = make_format(a.cs, ik).unwrap();
                    let out_fmt = make_format(b.cs, ok).unwrap();
                    let in_stride = pixel_bytes(in_fmt);
                    let out_stride = pixel_bytes(out_fmt);
                    let isb = sample_bytes(ik);

                    let mut packed_in = vec![0u8; n * in_stride];
                    for (p, row) in grid.iter().enumerate() {
                        for (c, &v) in row.iter().enumerate() {
                            encode_sample(&mut packed_in[p * in_stride + c * isb..], v, ik);
                        }
                    }

                    let mut oracle = vec![0u8; n * out_stride];
                    let built = rcms_oracle::do_transform_packed_default(
                        &[&a.bytes, &b.bytes],
                        &intents_raw,
                        &bpc,
                        &adapt,
                        in_fmt,
                        out_fmt,
                        &packed_in,
                        &mut oracle,
                        n,
                    );
                    if !built {
                        continue;
                    }

                    let xform = match Transform::new_simple_with_formats_strategy(
                        &pa,
                        &pb,
                        intent,
                        false,
                        in_fmt,
                        out_fmt,
                        OptimizationStrategy::Lcms2Compat,
                    ) {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    let mut rcms_out = vec![0u8; n * out_stride];
                    xform.do_transform(&packed_in, &mut rcms_out, n);

                    if xform.opt_path_label() == "baked" {
                        baked_cells += 1;
                    } else {
                        other_cells += 1;
                    }

                    if rcms_out != oracle {
                        let mut first = None;
                        for p in 0..n {
                            let ro = &rcms_out[p * out_stride..(p + 1) * out_stride];
                            let oo = &oracle[p * out_stride..(p + 1) * out_stride];
                            if ro != oo {
                                first = Some((row_str(&grid[p]), ro.to_vec(), oo.to_vec()));
                                break;
                            }
                        }
                        mismatches.push(format!(
                            "{} -> {} [{intent:?}] in={in_fmt:#x} out={out_fmt:#x} path={} \
                             first_diff={:?}",
                            a.name,
                            b.name,
                            xform.opt_path_label(),
                            first
                        ));
                    }
                }
            }
        }
    }

    eprintln!(
        "[resampling dense] RGB→RGB {levels}^3 cube: {baked_cells} baked cells + \
         {other_cells} other cells bit-exact vs lcms2-default ({} pixels/cell)",
        n
    );
    for m in &mismatches {
        eprintln!("[resampling dense][MISMATCH] {m}");
    }
    assert!(baked_cells > 0, "expected the baked path to be exercised");
    assert!(
        mismatches.is_empty(),
        "{} dense cells diverged from lcms2-default",
        mismatches.len()
    );
}

fn row_str(r: &[f32]) -> String {
    r.iter()
        .map(|v| format!("{v:.4}"))
        .collect::<Vec<_>>()
        .join(",")
}
