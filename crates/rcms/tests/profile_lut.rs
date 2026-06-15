//! Differential tests for profile → pipeline LUT extraction
//! (`rcms::link::read_input_lut` / `read_output_lut` / `read_devicelink_lut`),
//! bit-for-bit against lcms2's `_cmsReadInputLUT` / `_cmsReadOutputLUT` /
//! `_cmsReadDevicelinkLUT` (`src/cmsio1.c`).
//!
//! For every `vendor/Little-CMS/testbed/*.icc` and each intent 0..=3, if lcms2
//! builds a LUT, build the rcms pipeline too and evaluate BOTH at a grid of
//! inputs (input-channel count from the profile color space), asserting bit-exact
//! `f32::to_bits` equality. The testbed has matrix-shaper RGB profiles (crayons,
//! ibm-t61, new, test5) AND A2B/B2A LUT profiles (test1..4) — both exercised.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rcms::link::{read_devicelink_lut, read_input_lut, read_output_lut};
use rcms::profile::Profile;
use rcms_oracle::ReadLut;

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

/// A bounded per-channel input sweep: `levels` grid points per channel over
/// `[0, 1]`, capped to keep the cartesian product reasonable for 3/4 channels.
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

/// Classify the pipeline path the profile takes for the input LUT (for the
/// coverage report). Returns "lut", "matrix-shaper", or "gray".
fn classify_input(p: &Profile) -> &'static str {
    use rcms::profile::ColorSpace;
    let h = p.header();
    let a2b = [b"A2B0", b"A2B1", b"A2B2"];
    let has_lut = a2b
        .iter()
        .any(|s| p.has_tag(rcms::sig::Signature::from_bytes(**s)))
        || p.has_tag(rcms::sig::Signature::from_bytes(*b"D2B0"));
    if has_lut {
        "lut"
    } else if h.color_space == ColorSpace::Gray {
        "gray"
    } else {
        "matrix-shaper"
    }
}

/// The shared differential sweep over one `ReadLut` direction. Returns
/// (profiles_with_lut, intent_cells, total_samples) and asserts bit-exactness.
fn sweep(
    which: ReadLut,
    rust_build: impl Fn(&Profile, u32) -> rcms::Result<rcms::pipeline::Pipeline>,
    path_kinds: &mut BTreeMap<String, usize>,
) -> (usize, usize, usize) {
    let files = testbed_icc();
    assert!(!files.is_empty(), "no .icc in testbed");

    let mut profiles_hit = 0usize;
    let mut cells = 0usize;
    let mut total_samples = 0usize;

    for path in &files {
        let bytes = fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();

        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let profile = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let mut profile_hit = false;
        for intent in 0u32..=3 {
            // Ask lcms2 whether it builds a LUT and its channel counts.
            let (n_in, n_out) = match rcms_oracle::read_lut_channels(&bytes, which, intent) {
                Some(c) => (c.0 as usize, c.1 as usize),
                None => {
                    // lcms2 returns NULL: rcms must also fail to build (or build
                    // something we never compare). Assert agreement on Unsupported
                    // for the obviously-routed cases is out of scope; we only
                    // require that the cells lcms2 DOES build match.
                    continue;
                }
            };

            let rust_lut = match rust_build(&profile, intent) {
                Ok(l) => l,
                Err(e) => panic!(
                    "{name} {which:?} intent {intent}: lcms2 built a {n_in}->{n_out} LUT but \
                     rcms failed: {e}"
                ),
            };
            assert_eq!(
                rust_lut.input_channels, n_in,
                "{name} {which:?} intent {intent}: input channel mismatch"
            );
            assert_eq!(
                rust_lut.output_channels, n_out,
                "{name} {which:?} intent {intent}: output channel mismatch"
            );

            // Build a grid (coarser for 4 inputs to bound the product).
            let levels = if n_in >= 4 { 5 } else { 9 };
            let grid = input_grid(n_in, levels);

            // Flatten inputs for the oracle batch call.
            let mut flat_in: Vec<f32> = Vec::with_capacity(grid.len() * n_in);
            for row in &grid {
                flat_in.extend_from_slice(row);
            }
            let oracle_out = rcms_oracle::read_lut_eval_float(
                &bytes,
                which,
                intent,
                &flat_in,
                grid.len(),
                n_out,
            )
            .expect("oracle eval");

            for (s, row) in grid.iter().enumerate() {
                let rust_out = rust_lut.eval_float(row);
                let oref = &oracle_out[s * n_out..(s + 1) * n_out];
                for ch in 0..n_out {
                    assert_eq!(
                        rust_out[ch].to_bits(),
                        oref[ch].to_bits(),
                        "{name} {which:?} intent {intent} sample {row:?} ch{ch}: \
                         rust={} lcms2={}",
                        rust_out[ch],
                        oref[ch]
                    );
                }
                total_samples += 1;
            }

            cells += 1;
            profile_hit = true;
        }
        if profile_hit {
            *path_kinds
                .entry(format!("{}:{}", classify_input(&profile), name))
                .or_default() += 1;
            profiles_hit += 1;
        }
    }

    (profiles_hit, cells, total_samples)
}

#[test]
fn read_input_lut_matches_oracle_over_testbed() {
    let mut kinds = BTreeMap::new();
    let (profiles, cells, samples) = sweep(ReadLut::Input, read_input_lut, &mut kinds);
    println!("read_input_lut: {profiles} profiles, {cells} intent cells, {samples} samples");
    for k in kinds.keys() {
        println!("  {k}");
    }
    assert!(profiles > 0, "expected at least one input-LUT profile");
}

#[test]
fn read_output_lut_matches_oracle_over_testbed() {
    let mut kinds = BTreeMap::new();
    let (profiles, cells, samples) = sweep(ReadLut::Output, read_output_lut, &mut kinds);
    println!("read_output_lut: {profiles} profiles, {cells} intent cells, {samples} samples");
    for k in kinds.keys() {
        println!("  {k}");
    }
    assert!(profiles > 0, "expected at least one output-LUT profile");
}

#[test]
fn read_devicelink_lut_matches_oracle_over_testbed() {
    let mut kinds = BTreeMap::new();
    let (profiles, cells, samples) = sweep(ReadLut::Devicelink, read_devicelink_lut, &mut kinds);
    println!("read_devicelink_lut: {profiles} profiles, {cells} intent cells, {samples} samples");
    for k in kinds.keys() {
        println!("  {k}");
    }
    // Only link/abstract/LUT profiles (test1..4) build a devicelink LUT.
    assert!(profiles > 0, "expected at least one devicelink-LUT profile");
}

/// Differential: `rcms::curve::reverse_tone_curve` of a tabulated curve must
/// match lcms2 `cmsReverseToneCurve` evaluated at a grid of points. Exercises the
/// numeric table-reversal path (`GetInterval` + interpolation) used by the output
/// matrix-shaper. Uses synthetic monotone tables plus the testbed RGB TRC tables.
#[test]
fn reverse_tone_curve_matches_oracle() {
    use rcms::curve::{build_tabulated_16, reverse_tone_curve};
    use rcms::profile::Tag;

    let xs: Vec<f32> = (0..256).map(|i| i as f32 / 255.0).collect();

    // (a) synthetic tables: ascending gamma-like, descending, and a collapsed run.
    let mut tables: Vec<Vec<u16>> = Vec::new();
    tables.push(
        (0..32)
            .map(|i| ((i as f64 / 31.0).powf(2.2) * 65535.0) as u16)
            .collect(),
    );
    tables.push(
        (0..32)
            .map(|i| (65535 - (i as u32 * 65535 / 31)) as u16)
            .collect(),
    );
    {
        // ascending with a flat (collapsed) middle plateau.
        let mut t: Vec<u16> = (0..16).map(|i| (i as u32 * 65535 / 31) as u16).collect();
        t.extend(std::iter::repeat_n(t[15], 8));
        t.extend((24..32).map(|i| (i as u32 * 65535 / 31) as u16));
        tables.push(t);
    }

    // (b) testbed RGB TRC tables.
    for path in testbed_icc() {
        let bytes = fs::read(&path).unwrap();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for s in [b"rTRC", b"gTRC", b"bTRC", b"kTRC"] {
            let sig = rcms::sig::Signature::from_bytes(*s);
            if !p.has_tag(sig) {
                continue;
            }
            if let Ok(Tag::Curve(c)) = p.read_tag(sig) {
                tables.push(c.table16().to_vec());
            }
        }
    }

    let mut checked = 0usize;
    for table in &tables {
        let rev = reverse_tone_curve(&build_tabulated_16(table));
        let oracle = rcms_oracle::reverse_tabulated16_eval_float(table, &xs).expect("oracle rev");
        for (i, (&x, &cy)) in xs.iter().zip(oracle.iter()).enumerate() {
            let ry = rev.eval_float(x);
            assert_eq!(
                ry.to_bits(),
                cy.to_bits(),
                "reverse curve (len {}) sample[{i}] x={x}: rust={ry} lcms2={cy}",
                table.len()
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected several reverse-curve tables checked"
    );
    println!(
        "reverse_tone_curve diff: {checked} tables x {} points",
        xs.len()
    );
}

/// Coverage assertion: confirm BOTH the matrix-shaper RGB path and the A2B/B2A
/// LUT path are exercised by the input sweep (the testbed has both).
#[test]
fn input_sweep_covers_matrix_shaper_and_lut() {
    let mut saw_lut = false;
    let mut saw_matrix = false;
    for path in testbed_icc() {
        let bytes = fs::read(&path).unwrap();
        if !rcms_oracle::open_succeeds(&bytes) {
            continue;
        }
        let p = match Profile::open(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if rcms_oracle::read_lut_channels(&bytes, ReadLut::Input, 0).is_none() {
            continue;
        }
        match classify_input(&p) {
            "lut" => saw_lut = true,
            "matrix-shaper" => saw_matrix = true,
            _ => {}
        }
    }
    assert!(
        saw_lut,
        "expected at least one A2B/LUT input profile (test1..4)"
    );
    assert!(
        saw_matrix,
        "expected at least one matrix-shaper RGB input profile (crayons/ibm-t61/new/test5)"
    );
}
