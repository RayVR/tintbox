//! Differential tests for 3D tetrahedral CLUT interpolation, bit-for-bit against
//! lcms2's `TetrahedralInterp16` / `TetrahedralInterpFloat` (cmsintrp.c).
//!
//! Coverage:
//! - `InterpParams::new` opta/domain for a hand-computed non-cubic grid.
//! - `tetrahedral_16` vs `rcms_oracle::tetra16`, bit-exact u16, over random
//!   3D grids (cubic and non-cubic, sizes 2..=17 per axis), nOut in {1,3,4},
//!   random u16 tables, and inputs that include 0x0000/0xFFFF on each axis,
//!   cell-boundary straddlers, and pure random values — millions of samples.
//! - `tetrahedral_float` vs `rcms_oracle::tetra_float`, bit-exact (f32::to_bits),
//!   same grid/output sweep with random f32 tables and inputs.

use rcms::interp::{
    bilinear_16, bilinear_float, eval_1_input, eval_1_input_float, eval_n_inputs,
    eval_n_inputs_float, interp_factory, tetrahedral_16, tetrahedral_float, trilinear_16,
    trilinear_float, Interp16, InterpFloat, InterpFn, InterpParams,
};
use rcms_oracle::{Rng, CMS_LERP_FLAGS_TRILINEAR};

/// Number of nodes in a 3D grid with the given per-axis sample counts.
fn n_nodes(grid: &[u32; 3]) -> usize {
    grid[0] as usize * grid[1] as usize * grid[2] as usize
}

/// Test-local mirror of `interp::fclamp` (cmsintrp.c `fclamp`): clamp to `[0,1]`,
/// mapping NaN and `< 1e-9` to 0. Used only to identify the OOB band the float
/// interpolators must skip.
fn fclamp_ref(v: f32) -> f32 {
    if v.is_nan() || v < 1e-9 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// Test-local mirror of `interp::quick_floor` (lcms2 `_cmsQuickFloor`): the
/// magic-number truncating floor. Used only to detect the near-1.0 boundary band.
fn quick_floor_ref(val: f32) -> i32 {
    const MAGIC: f64 = 68_719_476_736.0 * 1.5; // 2^36 * 1.5
    let temp = val as f64 + MAGIC;
    (temp.to_bits() as u32 as i32) >> 16
}

/// Per-axis sample counts to sweep. Includes the smallest interpolating grid
/// (2), powers of two, odd sizes, and the spec's non-cubic example [9,17,5].
const GRIDS: &[[u32; 3]] = &[
    [2, 2, 2],
    [3, 3, 3],
    [4, 4, 4],
    [17, 17, 17],
    [9, 17, 5],
    [5, 9, 17],
    [2, 5, 11],
    [16, 2, 8],
    [3, 7, 13],
    [10, 10, 2],
];

const N_OUTS: &[usize] = &[1, 3, 4];

#[test]
fn interp_params_opta_non_cubic() {
    // Hand-computed for nSamples=[9,17,5], nInputs=3, nOutputs=4.
    // domain[i] = nSamples[i]-1 => [8,16,4].
    // opta[0] = nOutputs = 4.
    // opta[1] = opta[0] * nSamples[nInputs-1] = 4 * nSamples[2] = 4 * 5 = 20.
    // opta[2] = opta[1] * nSamples[nInputs-2] = 20 * nSamples[1] = 20 * 17 = 340.
    let p = InterpParams::new(&[9, 17, 5], 3, 4);
    assert_eq!(p.n_inputs, 3);
    assert_eq!(p.n_outputs, 4);
    assert_eq!(p.n_samples, vec![9, 17, 5]);
    assert_eq!(p.domain, vec![8, 16, 4]);
    assert_eq!(p.opta, vec![4, 20, 340]);
}

#[test]
fn interp_params_opta_cubic() {
    // nSamples=[17,17,17], nOutputs=3.
    // opta[0]=3, opta[1]=3*17=51, opta[2]=51*17=867.
    let p = InterpParams::new(&[17, 17, 17], 3, 3);
    assert_eq!(p.domain, vec![16, 16, 16]);
    assert_eq!(p.opta, vec![3, 51, 867]);
}

/// Build a per-axis-aware sweep of u16 inputs for a grid: the corner cases
/// (0x0000 / 0xFFFF on each axis), values that land exactly on a node and just
/// off it (straddling cell boundaries), plus pure-random fill.
fn u16_inputs(grid: &[u32; 3], rng: &mut Rng) -> Vec<[u16; 3]> {
    let mut inputs: Vec<[u16; 3]> = Vec::new();
    let edges = [0u16, 1, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF];

    // All-corner combinations on the three axes.
    for &a in &edges {
        for &b in &edges {
            for &c in &edges {
                inputs.push([a, b, c]);
            }
        }
    }

    // Node-aligned and boundary-straddling values per axis. A node k on an axis
    // with `domain` cells sits at round(k * 65535 / domain).
    let axis_vals = |dom: u32| -> Vec<u16> {
        let mut v = Vec::new();
        if dom == 0 {
            v.push(0);
        } else {
            for k in 0..=dom {
                let exact = ((u64::from(k) * 65535) / u64::from(dom)) as i64;
                for d in [-1i64, 0, 1] {
                    let val = (exact + d).clamp(0, 65535) as u16;
                    v.push(val);
                }
            }
        }
        v
    };
    let xs = axis_vals(grid[0] - 1);
    let ys = axis_vals(grid[1] - 1);
    let zs = axis_vals(grid[2] - 1);
    // Cross a bounded subset to keep the count reasonable but boundary-dense.
    for &x in xs.iter() {
        for &y in ys.iter() {
            for &z in zs.iter() {
                inputs.push([x, y, z]);
            }
        }
    }

    // Pure random fill.
    for _ in 0..2000 {
        let r = rng.next_u64();
        inputs.push([r as u16, (r >> 16) as u16, (r >> 32) as u16]);
    }

    inputs
}

#[test]
fn tetrahedral_16_matches_oracle() {
    let mut rng = Rng::new(0x7E72_A4ED_A100);
    let mut total_samples: u64 = 0;

    for &grid in GRIDS {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 3, n_out);
            let table_len = n_nodes(&grid) * n_out;

            // A few random tables per (grid, nOut).
            for _ in 0..4 {
                let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();

                for input in u16_inputs(&grid, &mut rng) {
                    let mut out = vec![0u16; n_out];
                    tetrahedral_16(&input, &mut out, &table, &p);

                    let oracle = rcms_oracle::tetra16(&grid, n_out, &table, &input)
                        .expect("oracle tetra16 alloc");

                    assert_eq!(
                        out, oracle,
                        "tetra16 mismatch: grid={grid:?} n_out={n_out} input={input:?}"
                    );
                    total_samples += n_out as u64;
                }
            }
        }
    }

    println!("tetrahedral_16: {total_samples} output samples compared bit-exact");
    assert!(total_samples > 1_000_000, "expected millions of samples");
}

/// Random f32 inputs in roughly [0,1] plus the boundary cases lcms2's `fclamp`
/// keys on: values <1e-9 (mapped to 0), exactly/just-below/just-above 1.0, and
/// out-of-range values (clamped). NaN is excluded — lcms2 maps it to 0 but the
/// oracle path is exercised separately and we keep the table values finite.
fn f32_inputs(rng: &mut Rng) -> Vec<[f32; 3]> {
    let mut inputs = Vec::new();
    let edges: [f32; 8] = [
        0.0,
        1e-10,
        1e-9,
        0.5,
        1.0 - f32::EPSILON,
        1.0,
        1.0 + f32::EPSILON,
        2.0,
    ];
    for &a in &edges {
        for &b in &edges {
            for &c in &edges {
                inputs.push([a, b, c]);
            }
        }
    }
    for _ in 0..3000 {
        let x = rng.next_f64_unit() as f32;
        let y = rng.next_f64_unit() as f32;
        let z = rng.next_f64_unit() as f32;
        inputs.push([x, y, z]);
    }
    inputs
}

#[test]
fn tetrahedral_float_matches_oracle() {
    let mut rng = Rng::new(0xF10A_7E72_A400);
    let mut total_samples: u64 = 0;

    for &grid in GRIDS {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 3, n_out);
            let table_len = n_nodes(&grid) * n_out;

            for _ in 0..4 {
                // Random f32 table values in [0,1].
                let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();

                for input in f32_inputs(&mut rng) {
                    let mut out = vec![0f32; n_out];
                    tetrahedral_float(&input, &mut out, &table, &p);

                    let oracle = rcms_oracle::tetra_float(&grid, n_out, &table, &input)
                        .expect("oracle tetra_float alloc");

                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "tetra_float mismatch: grid={grid:?} n_out={n_out} \
                             input={input:?} chan={k} rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total_samples += n_out as u64;
                }
            }
        }
    }

    println!("tetrahedral_float: {total_samples} output samples compared bit-exact");
    assert!(
        total_samples > 100_000,
        "expected hundreds of thousands of samples"
    );
}

// ---------------------------------------------------------------------------
// n-D helpers: build per-axis grid sweeps and random inputs for N dimensions.
// ---------------------------------------------------------------------------

/// Node count of an n-D grid.
fn n_nodes_nd(grid: &[u32]) -> usize {
    grid.iter().map(|&g| g as usize).product()
}

/// A u16 input vector sweep for an n-D grid: all-corner combinations on each axis
/// (cheap because we use a small corner set raised to a bounded count) plus
/// random fill. Keeps the per-axis 0x0000/0xFFFF corner cases (the `X1 == X0`
/// branch) dense without exploding for high dimensionality.
fn u16_inputs_nd(n: usize, rng: &mut Rng, n_random: usize) -> Vec<Vec<u16>> {
    let mut inputs: Vec<Vec<u16>> = Vec::new();
    // A compact corner set; full cartesian over n axes is bounded by using only
    // 0x0000 / 0xFFFF for the corner sweep (2^n), which stays small for n<=6.
    let corners = [0u16, 0xFFFF];
    let total = 1usize << n;
    for mask in 0..total {
        let mut v = Vec::with_capacity(n);
        for axis in 0..n {
            v.push(corners[(mask >> axis) & 1]);
        }
        inputs.push(v);
    }
    // A few mid-range / boundary edge values mixed per axis.
    let edges = [0u16, 1, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF];
    for _ in 0..n_random {
        let r = rng.next_u64();
        let mut v = Vec::with_capacity(n);
        for axis in 0..n {
            // Mix: ~25% snap to an edge value, else random across the u16 range.
            let pick = (r >> (axis * 8)) & 0xFF;
            if pick < 64 {
                v.push(edges[(pick as usize) % edges.len()]);
            } else {
                v.push((rng.next_u64() as u16) ^ (pick as u16));
            }
        }
        inputs.push(v);
    }
    inputs
}

/// A f32 input vector sweep for an n-D grid for the `_cmsQuickFloor`-based float
/// paths (`BilinearInterpFloat`, `Eval*InputsFloat`): corner combinations plus
/// random fill.
///
/// IMPORTANT: lcms2's `_cmsQuickFloor` is a *truncating* magic-number floor, but
/// the f32 value `1.0 - f32::EPSILON` (= 0.99999994) is so close to 1.0 that
/// `quick_floor(that * domain)` truncates to `domain` itself while `fclamp(input)
/// < 1.0` — so lcms2 computes `K1 = K0 + opta`, reading one node *past* the grid
/// (latent OOB in lcms2's float n-D path; the input is expected to be clamped to
/// `[0,1]` before reaching the interpolator). We therefore avoid the
/// `(1 - tiny, 1.0)` band: `1.0` itself is safe (`fclamp >= 1.0` zeroes the K1
/// step), and so is anything `>= 1.0` (clamped) or `<= ~0.9998`. Random fill is
/// drawn from `[0, 0.9995]` to stay well clear of the boundary at every domain.
fn f32_inputs_nd(n: usize, rng: &mut Rng, n_random: usize) -> Vec<Vec<f32>> {
    let mut inputs: Vec<Vec<f32>> = Vec::new();
    let corners = [0.0f32, 1.0];
    let total = 1usize << n;
    for mask in 0..total {
        let mut v = Vec::with_capacity(n);
        for axis in 0..n {
            v.push(corners[(mask >> axis) & 1]);
        }
        inputs.push(v);
    }
    // Safe explicit edges: 0.0 / tiny (fclamp -> 0), 0.5, exactly 1.0 and above
    // (fclamp -> 1.0, K1 == K0). No `1 - epsilon` band (see doc comment).
    let edges = [0.0f32, 1e-10, 1e-9, 0.5, 1.0, 1.0 + f32::EPSILON, 2.0];
    for _ in 0..n_random {
        let mut v = Vec::with_capacity(n);
        for _axis in 0..n {
            if rng.next_u64() & 7 == 0 {
                v.push(edges[(rng.next_u64() as usize) % edges.len()]);
            } else {
                // Draw from [0, 0.9995] to stay clear of the quick_floor boundary.
                v.push((rng.next_f64_unit() * 0.9995) as f32);
            }
        }
        inputs.push(v);
    }
    inputs
}

// ---------------------------------------------------------------------------
// Bilinear (2D).
// ---------------------------------------------------------------------------

const GRIDS_2D: &[[u32; 2]] = &[
    [2, 2],
    [3, 3],
    [4, 4],
    [17, 17],
    [9, 5],
    [2, 11],
    [16, 8],
    [3, 13],
];

#[test]
fn bilinear_16_matches_oracle() {
    let mut rng = Rng::new(0xB111_0007_1600);
    let mut total: u64 = 0;
    for &grid in GRIDS_2D {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 2, n_out);
            let table_len = grid[0] as usize * grid[1] as usize * n_out;
            for _ in 0..4 {
                let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();
                for input in u16_inputs_nd(2, &mut rng, 4000) {
                    let mut out = vec![0u16; n_out];
                    bilinear_16(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp16(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp16 bilinear");
                    assert_eq!(
                        out, oracle,
                        "bilinear16 grid={grid:?} n_out={n_out} in={input:?}"
                    );
                    total += n_out as u64;
                }
            }
        }
    }
    println!("bilinear_16: {total} output samples compared bit-exact");
    assert!(total > 500_000, "expected many samples");
}

#[test]
fn bilinear_float_matches_oracle() {
    let mut rng = Rng::new(0xB111_F10A_1600);
    let mut total: u64 = 0;
    for &grid in GRIDS_2D {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 2, n_out);
            let table_len = grid[0] as usize * grid[1] as usize * n_out;
            for _ in 0..4 {
                let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();
                for input in f32_inputs_nd(2, &mut rng, 1500) {
                    let mut out = vec![0f32; n_out];
                    bilinear_float(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp_float(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp_float bilinear");
                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "bilinear_float grid={grid:?} n_out={n_out} in={input:?} ch={k} \
                             rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total += n_out as u64;
                }
            }
        }
    }
    println!("bilinear_float: {total} output samples compared bit-exact");
    assert!(total > 100_000, "expected many samples");
}

/// Full-domain `bilinear_float` parity: now that `bilinear_float` floors with
/// `quick_floor` (matching lcms2's `_cmsQuickFloor`, NOT libm floor), it is
/// bit-exact against the oracle across the ENTIRE valid `[0, 1)` input domain,
/// including the `1 - f32::EPSILON` value the original test had to avoid.
///
/// The only band lcms2 itself mishandles is where `fclamp(in) < 1.0` yet
/// `quick_floor(fclamp(in) * domain) == domain` — there the C reads one node past
/// the grid (latent OOB; inputs are expected pre-clamped). rcms clamps the index
/// to stay memory-safe, so at that exact band rcms and the (OOB-reading) oracle
/// may differ; calling the OOB oracle is UB, so we do NOT probe it. Every other
/// input up to and including `1 - f32::EPSILON` is asserted bit-exact.
#[test]
fn bilinear_float_full_domain_matches_oracle() {
    let mut rng = Rng::new(0xB111_FD00_1600);
    let mut total: u64 = 0;
    let mut boundary_hits: u64 = 0;

    // `fclamp` maps `< 1e-9` to 0 and clamps `>= 1.0` to 1.0; the interesting
    // domain is `[1e-9, 1.0)`. Sweep a dense set incl. the largest sub-1.0 f32.
    let one_minus_eps = 1.0f32 - f32::EPSILON; // 0.99999994

    for &grid in GRIDS_2D {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 2, n_out);
            let table_len = grid[0] as usize * grid[1] as usize * n_out;
            let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();

            // Per-axis fine sweep covering the whole `[0,1)` plus the top edge.
            let steps = 400usize;
            for ix in 0..=steps {
                for iy in 0..=steps {
                    let mk = |i: usize| -> f32 {
                        if i == steps {
                            one_minus_eps
                        } else {
                            (i as f64 / steps as f64) as f32
                        }
                    };
                    let input = [mk(ix), mk(iy)];

                    // Skip ONLY the genuine OOB band: where lcms2 would read past
                    // the grid (quick_floor rounds to `domain` while fclamp < 1).
                    let oob = |v: f32, dom: u32| -> bool {
                        let fc = fclamp_ref(v);
                        fc < 1.0 && quick_floor_ref(fc * dom as f32) >= dom as i32
                    };
                    if oob(input[0], grid[0]) || oob(input[1], grid[1]) {
                        boundary_hits += 1;
                        continue;
                    }

                    let mut out = vec![0f32; n_out];
                    bilinear_float(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp_float(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp_float bilinear");
                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "bilinear_float full-domain grid={grid:?} n_out={n_out} \
                             in={input:?} ch={k} rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total += n_out as u64;
                }
            }
        }
    }
    println!(
        "bilinear_float full-domain: {total} output samples bit-exact \
         ({boundary_hits} OOB-band inputs skipped)"
    );
    assert!(total > 1_000_000, "expected a dense full-domain sweep");
}

// ---------------------------------------------------------------------------
// Trilinear (3D). Forced through TrilinearInterp16/Float via the trilinear flag.
// ---------------------------------------------------------------------------

#[test]
fn trilinear_16_matches_oracle() {
    let mut rng = Rng::new(0x7211_0007_3000);
    let mut total: u64 = 0;
    for &grid in GRIDS {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 3, n_out);
            let table_len = n_nodes(&grid) * n_out;
            for _ in 0..4 {
                let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();
                for input in u16_inputs(&grid, &mut rng) {
                    let mut out = vec![0u16; n_out];
                    trilinear_16(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp16(
                        &grid,
                        n_out,
                        &table,
                        CMS_LERP_FLAGS_TRILINEAR,
                        &input,
                    )
                    .expect("oracle interp16 trilinear");
                    assert_eq!(
                        out, oracle,
                        "trilinear16 grid={grid:?} n_out={n_out} in={input:?}"
                    );
                    total += n_out as u64;
                }
            }
        }
    }
    println!("trilinear_16: {total} output samples compared bit-exact (forced TRILINEAR flag)");
    assert!(total > 1_000_000, "expected millions of samples");
}

#[test]
fn trilinear_float_matches_oracle() {
    let mut rng = Rng::new(0x7211_F10A_3000);
    let mut total: u64 = 0;
    for &grid in GRIDS {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, 3, n_out);
            let table_len = n_nodes(&grid) * n_out;
            for _ in 0..4 {
                let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();
                for input in f32_inputs(&mut rng) {
                    let mut out = vec![0f32; n_out];
                    trilinear_float(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp_float(
                        &grid,
                        n_out,
                        &table,
                        CMS_LERP_FLAGS_TRILINEAR,
                        &input,
                    )
                    .expect("oracle interp_float trilinear");
                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "trilinear_float grid={grid:?} n_out={n_out} in={input:?} ch={k} \
                             rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total += n_out as u64;
                }
            }
        }
    }
    println!("trilinear_float: {total} output samples compared bit-exact (forced TRILINEAR flag)");
    assert!(total > 100_000, "expected hundreds of thousands of samples");
}

/// A hand-computed trilinear reference (independent of the C optimized fixed-point
/// path), used to cross-check `trilinear_float` for a few cases. This is the plain
/// f64 8-corner trilinear blend, evaluated at a cell-interior point where the
/// fixed-point rounding of the C path does not diverge from the exact blend.
#[test]
fn trilinear_float_against_hand_reference() {
    // 2x2x2 grid, single output. Corner values c[x][y][z].
    let grid = [2u32, 2, 2];
    let p = InterpParams::new(&grid, 3, 1);
    let c = [
        [[0.10f32, 0.20], [0.30, 0.40]],
        [[0.50, 0.60], [0.70, 0.80]],
    ];
    // Table layout (lcms2): node index = x*opta[2] + y*opta[1] + z*opta[0], with
    // opta=[1, 2, 4] for nOut=1. So flat order is z fastest, then y, then x.
    let mut table = vec![0f32; 8];
    for x in 0..2 {
        for y in 0..2 {
            for z in 0..2 {
                table[x * 4 + y * 2 + z] = c[x][y][z];
            }
        }
    }
    for &(fx, fy, fz) in &[(0.25f32, 0.5, 0.75), (0.1, 0.9, 0.3), (0.5, 0.5, 0.5)] {
        let mut out = [0f32];
        trilinear_float(&[fx, fy, fz], &mut out, &table, &p);
        // Exact trilinear blend in f64.
        let lerp = |a: f64, l: f64, h: f64| l + (h - l) * a;
        let (fxd, fyd, fzd) = (fx as f64, fy as f64, fz as f64);
        let d00 = lerp(fxd, c[0][0][0] as f64, c[1][0][0] as f64);
        let d01 = lerp(fxd, c[0][0][1] as f64, c[1][0][1] as f64);
        let d10 = lerp(fxd, c[0][1][0] as f64, c[1][1][0] as f64);
        let d11 = lerp(fxd, c[0][1][1] as f64, c[1][1][1] as f64);
        let dy0 = lerp(fyd, d00, d10);
        let dy1 = lerp(fyd, d01, d11);
        let want = lerp(fzd, dy0, dy1);
        assert!(
            (out[0] as f64 - want).abs() < 1e-6,
            "trilinear hand-ref mismatch at ({fx},{fy},{fz}): got {} want {want}",
            out[0]
        );
    }
}

// ---------------------------------------------------------------------------
// n-D EvalN (4D, 5D, 6D).
// ---------------------------------------------------------------------------

/// Per-dimensionality grid sweeps for the EvalN tests. Mixed cubic/non-cubic,
/// small grids to keep table sizes (and oracle alloc cost) reasonable.
fn grids_nd(n: usize) -> Vec<Vec<u32>> {
    match n {
        4 => vec![
            vec![2, 2, 2, 2],
            vec![3, 3, 3, 3],
            vec![5, 4, 3, 2],
            vec![2, 5, 3, 4],
            vec![6, 2, 4, 3],
        ],
        5 => vec![
            vec![2, 2, 2, 2, 2],
            vec![3, 3, 3, 3, 3],
            vec![4, 3, 2, 3, 2],
            vec![2, 4, 3, 2, 3],
        ],
        6 => vec![
            vec![2, 2, 2, 2, 2, 2],
            vec![3, 2, 3, 2, 3, 2],
            vec![2, 3, 2, 3, 2, 3],
        ],
        _ => unreachable!(),
    }
}

fn eval_n_16_check(n: usize, seed: u64) -> u64 {
    let mut rng = Rng::new(seed);
    let mut total: u64 = 0;
    for grid in grids_nd(n) {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, n, n_out);
            let table_len = n_nodes_nd(&grid) * n_out;
            for _ in 0..3 {
                let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();
                for input in u16_inputs_nd(n, &mut rng, 5000) {
                    let mut out = vec![0u16; n_out];
                    eval_n_inputs(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp16(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp16 EvalN");
                    assert_eq!(
                        out, oracle,
                        "eval{n} grid={grid:?} n_out={n_out} in={input:?}"
                    );
                    total += n_out as u64;
                }
            }
        }
    }
    total
}

fn eval_n_float_check(n: usize, seed: u64) -> u64 {
    let mut rng = Rng::new(seed);
    let mut total: u64 = 0;
    for grid in grids_nd(n) {
        for &n_out in N_OUTS {
            let p = InterpParams::new(&grid, n, n_out);
            let table_len = n_nodes_nd(&grid) * n_out;
            for _ in 0..3 {
                let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();
                for input in f32_inputs_nd(n, &mut rng, 4000) {
                    let mut out = vec![0f32; n_out];
                    eval_n_inputs_float(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp_float(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp_float EvalN");
                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "eval{n}_float grid={grid:?} n_out={n_out} in={input:?} ch={k} \
                             rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total += n_out as u64;
                }
            }
        }
    }
    total
}

#[test]
fn eval4_inputs_16_matches_oracle() {
    let total = eval_n_16_check(4, 0xE4_0007_4000);
    println!("eval4_inputs_16: {total} output samples compared bit-exact");
    assert!(total > 500_000, "expected many samples");
}

#[test]
fn eval4_inputs_float_matches_oracle() {
    let total = eval_n_float_check(4, 0xE4_F10A_4000);
    println!("eval4_inputs_float: {total} output samples compared bit-exact");
    assert!(total > 100_000, "expected many samples");
}

#[test]
fn eval5_inputs_16_matches_oracle() {
    let total = eval_n_16_check(5, 0xE5_0007_5000);
    println!("eval5_inputs_16: {total} output samples compared bit-exact");
    assert!(total > 200_000, "expected many samples");
}

#[test]
fn eval5_inputs_float_matches_oracle() {
    let total = eval_n_float_check(5, 0xE5_F10A_5000);
    println!("eval5_inputs_float: {total} output samples compared bit-exact");
    assert!(total > 50_000, "expected many samples");
}

#[test]
fn eval6_inputs_16_matches_oracle() {
    let total = eval_n_16_check(6, 0xE6_0007_6000);
    println!("eval6_inputs_16: {total} output samples compared bit-exact");
    assert!(total > 100_000, "expected many samples");
}

#[test]
fn eval6_inputs_float_matches_oracle() {
    let total = eval_n_float_check(6, 0xE6_F10A_6000);
    println!("eval6_inputs_float: {total} output samples compared bit-exact");
    assert!(total > 30_000, "expected many samples");
}

// ---------------------------------------------------------------------------
// 1-input, multi-output (Eval1Input / Eval1InputFloat).
// ---------------------------------------------------------------------------

const GRIDS_1D: &[u32] = &[2, 3, 4, 8, 17, 256, 4096];

#[test]
fn eval1_input_16_matches_oracle() {
    let mut rng = Rng::new(0xE1_0007_1000);
    let mut total: u64 = 0;
    for &n in GRIDS_1D {
        let grid = [n];
        for &n_out in N_OUTS {
            // Skip the single-output case here: that routes to LinLerp1D, covered
            // separately. Eval1Input is the multi-output path.
            if n_out == 1 {
                continue;
            }
            let p = InterpParams::new(&grid, 1, n_out);
            let table_len = n as usize * n_out;
            for _ in 0..3 {
                let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();
                for input in u16_inputs_nd(1, &mut rng, 3000) {
                    let mut out = vec![0u16; n_out];
                    eval_1_input(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp16(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp16 Eval1Input");
                    assert_eq!(
                        out, oracle,
                        "eval1_input16 grid={grid:?} n_out={n_out} in={input:?}"
                    );
                    total += n_out as u64;
                }
            }
        }
    }
    println!("eval1_input_16: {total} output samples compared bit-exact");
    assert!(total > 200_000, "expected many samples");
}

#[test]
fn eval1_input_float_matches_oracle() {
    let mut rng = Rng::new(0xE1_F10A_1000);
    let mut total: u64 = 0;
    for &n in GRIDS_1D {
        let grid = [n];
        for &n_out in N_OUTS {
            if n_out == 1 {
                continue;
            }
            let p = InterpParams::new(&grid, 1, n_out);
            let table_len = n as usize * n_out;
            for _ in 0..3 {
                let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();
                for input in f32_inputs_nd(1, &mut rng, 1500) {
                    let mut out = vec![0f32; n_out];
                    eval_1_input_float(&input, &mut out, &table, &p);
                    let oracle = rcms_oracle::interp_float(&grid, n_out, &table, 0, &input)
                        .expect("oracle interp_float Eval1InputFloat");
                    for k in 0..n_out {
                        assert_eq!(
                            out[k].to_bits(),
                            oracle[k].to_bits(),
                            "eval1_input_float grid={grid:?} n_out={n_out} in={input:?} ch={k} \
                             rust={} c={}",
                            out[k],
                            oracle[k]
                        );
                    }
                    total += n_out as u64;
                }
            }
        }
    }
    println!("eval1_input_float: {total} output samples compared bit-exact");
    assert!(total > 100_000, "expected many samples");
}

// ---------------------------------------------------------------------------
// Factory selection.
// ---------------------------------------------------------------------------

#[test]
fn factory_selects_expected_routine() {
    // 1 input, 1 output -> LinLerp1D / LinLerp1Dfloat.
    assert_eq!(
        interp_factory(1, 1, false, false),
        InterpFn::Lerp16(Interp16::Linear)
    );
    assert_eq!(
        interp_factory(1, 1, true, false),
        InterpFn::LerpFloat(InterpFloat::Linear)
    );
    // 1 input, multi output -> Eval1Input / Eval1InputFloat.
    assert_eq!(
        interp_factory(1, 3, false, false),
        InterpFn::Lerp16(Interp16::Eval1)
    );
    assert_eq!(
        interp_factory(1, 4, true, false),
        InterpFn::LerpFloat(InterpFloat::Eval1)
    );
    // 2 inputs -> bilinear (trilinear flag irrelevant).
    assert_eq!(
        interp_factory(2, 3, false, false),
        InterpFn::Lerp16(Interp16::Bilinear)
    );
    assert_eq!(
        interp_factory(2, 3, false, true),
        InterpFn::Lerp16(Interp16::Bilinear)
    );
    assert_eq!(
        interp_factory(2, 3, true, false),
        InterpFn::LerpFloat(InterpFloat::Bilinear)
    );
    // 3 inputs -> tetrahedral by default, trilinear with the flag.
    assert_eq!(
        interp_factory(3, 3, false, false),
        InterpFn::Lerp16(Interp16::Tetrahedral)
    );
    assert_eq!(
        interp_factory(3, 3, false, true),
        InterpFn::Lerp16(Interp16::Trilinear)
    );
    assert_eq!(
        interp_factory(3, 3, true, false),
        InterpFn::LerpFloat(InterpFloat::Tetrahedral)
    );
    assert_eq!(
        interp_factory(3, 3, true, true),
        InterpFn::LerpFloat(InterpFloat::Trilinear)
    );
    // 4..=15 -> EvalN (trilinear flag irrelevant).
    for n in 4..=15 {
        assert_eq!(
            interp_factory(n, 4, false, false),
            InterpFn::Lerp16(Interp16::EvalN),
            "n_inputs={n}"
        );
        assert_eq!(
            interp_factory(n, 4, true, true),
            InterpFn::LerpFloat(InterpFloat::EvalN),
            "n_inputs={n}"
        );
    }
}

#[test]
fn factory_dispatch_matches_direct_call() {
    // The factory-selected routine produces the same result as calling the
    // underlying interpolator directly, for one case per dimensionality.
    let mut rng = Rng::new(0xFAC0_0042);

    // 2D bilinear.
    {
        let grid = [4u32, 5];
        let p = InterpParams::new(&grid, 2, 3);
        let table: Vec<u16> = (0..(4 * 5 * 3)).map(|_| rng.next_u64() as u16).collect();
        let input = [0x4000u16, 0xC000];
        let mut a = vec![0u16; 3];
        let mut b = vec![0u16; 3];
        bilinear_16(&input, &mut a, &table, &p);
        match interp_factory(2, 3, false, false) {
            InterpFn::Lerp16(f) => f.eval(&input, &mut b, &table, &p),
            _ => panic!("expected Lerp16"),
        }
        assert_eq!(a, b);
    }
    // 3D trilinear via flag.
    {
        let grid = [3u32, 3, 3];
        let p = InterpParams::new(&grid, 3, 2);
        let table: Vec<u16> = (0..(27 * 2)).map(|_| rng.next_u64() as u16).collect();
        let input = [0x2000u16, 0x9000, 0xF000];
        let mut a = vec![0u16; 2];
        let mut b = vec![0u16; 2];
        trilinear_16(&input, &mut a, &table, &p);
        match interp_factory(3, 2, false, true) {
            InterpFn::Lerp16(f) => f.eval(&input, &mut b, &table, &p),
            _ => panic!("expected Lerp16"),
        }
        assert_eq!(a, b);
    }
    // 4D EvalN float.
    {
        let grid = [3u32, 2, 4, 3];
        let p = InterpParams::new(&grid, 4, 3);
        let table_len = (3 * 2 * 4 * 3) * 3;
        let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();
        let input = [0.2f32, 0.7, 0.4, 0.9];
        let mut a = vec![0f32; 3];
        let mut b = vec![0f32; 3];
        eval_n_inputs_float(&input, &mut a, &table, &p);
        match interp_factory(4, 3, true, false) {
            InterpFn::LerpFloat(f) => f.eval(&input, &mut b, &table, &p),
            _ => panic!("expected LerpFloat"),
        }
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}
