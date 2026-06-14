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

use rcms::interp::{tetrahedral_16, tetrahedral_float, InterpParams};
use rcms_oracle::Rng;

/// Number of nodes in a 3D grid with the given per-axis sample counts.
fn n_nodes(grid: &[u32; 3]) -> usize {
    grid[0] as usize * grid[1] as usize * grid[2] as usize
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
