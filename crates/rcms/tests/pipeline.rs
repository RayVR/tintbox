//! Differential tests for `rcms::pipeline`, bit-for-bit against lcms2's
//! `cmsPipelineEvalFloat` / `cmsPipelineEval16` (cmslut.c `_LUTevalFloat` /
//! `_LUTeval16`).
//!
//! Coverage:
//! - Matrix stage alone (square 3x3 and non-square 3x4), random matrices and
//!   offsets, random inputs, diffed float (f32::to_bits) and 16-bit (u16).
//! - ToneCurves stage alone (random 16-bit tabulated curves), float + 16.
//! - Multi-stage `ToneCurves -> Matrix`, float + 16.
//! - From16ToFloat / FromFloatTo16 boundary behaviour at 0x0000 / 0xFFFF and the
//!   identity round-trip via an empty / identity pipeline.

use rcms::curve::{build_tabulated_16, ToneCurve};
use rcms::interp::InterpParams;
use rcms::pipeline::{Clut, ClutTable, Pipeline, Stage};
use rcms_oracle::Rng;

/// A random f64 in [-2, 2) — matrix entries span sign and a useful magnitude.
fn rand_m(rng: &mut Rng) -> f64 {
    rng.next_f64_unit() * 4.0 - 2.0
}

/// A random f64 offset in [-0.5, 0.5).
fn rand_off(rng: &mut Rng) -> f64 {
    rng.next_f64_unit() - 0.5
}

/// A random f32 input in [0, 1] (lcms2's float domain).
fn rand_in_f32(rng: &mut Rng) -> f32 {
    rng.next_f64_unit() as f32
}

fn assert_f32_bits(rust: f32, c: f32, ctx: &str) {
    assert_eq!(
        rust.to_bits(),
        c.to_bits(),
        "f32 bit mismatch {ctx}: rust={rust} c={c}"
    );
}

#[test]
fn matrix_stage_square_float_and_16() {
    let mut rng = Rng::new(0xA11CE);
    let (rows, cols) = (3usize, 3usize);

    for trial in 0..2000 {
        let m: Vec<f64> = (0..rows * cols).map(|_| rand_m(&mut rng)).collect();
        let offset: Option<Vec<f64>> = if trial % 3 == 0 {
            None
        } else {
            Some((0..rows).map(|_| rand_off(&mut rng)).collect())
        };

        let stage = Stage::Matrix {
            rows,
            cols,
            m: m.clone(),
            offset: offset.clone(),
        };
        let mut pl = Pipeline::new(cols, rows);
        pl.insert_stage_at_end(stage).unwrap();

        let inf: Vec<f32> = (0..cols).map(|_| rand_in_f32(&mut rng)).collect();
        let in16: Vec<u16> = (0..cols).map(|_| rng.next_u64() as u16).collect();

        // Float path.
        let got_f = pl.eval_float(&inf);
        let exp_f =
            rcms_oracle::pipeline_matrix_eval_float(rows, cols, &m, offset.as_deref(), &inf)
                .expect("oracle matrix float");
        for i in 0..rows {
            assert_f32_bits(
                got_f[i],
                exp_f[i],
                &format!("sq float trial={trial} out={i}"),
            );
        }

        // 16-bit path.
        let got_16 = pl.eval_16(&in16);
        let exp_16 = rcms_oracle::pipeline_matrix_eval16(rows, cols, &m, offset.as_deref(), &in16)
            .expect("oracle matrix 16");
        assert_eq!(got_16, exp_16, "sq 16 trial={trial}");
    }
}

#[test]
fn matrix_stage_non_square_float_and_16() {
    let mut rng = Rng::new(0xBEEF_1234);
    // 3 rows x 4 cols: maps 4 inputs to 3 outputs.
    let (rows, cols) = (3usize, 4usize);

    for trial in 0..2000 {
        let m: Vec<f64> = (0..rows * cols).map(|_| rand_m(&mut rng)).collect();
        let offset: Option<Vec<f64>> = if trial % 2 == 0 {
            Some((0..rows).map(|_| rand_off(&mut rng)).collect())
        } else {
            None
        };

        let stage = Stage::Matrix {
            rows,
            cols,
            m: m.clone(),
            offset: offset.clone(),
        };
        let mut pl = Pipeline::new(cols, rows);
        pl.insert_stage_at_end(stage).unwrap();

        let inf: Vec<f32> = (0..cols).map(|_| rand_in_f32(&mut rng)).collect();
        let in16: Vec<u16> = (0..cols).map(|_| rng.next_u64() as u16).collect();

        let got_f = pl.eval_float(&inf);
        let exp_f =
            rcms_oracle::pipeline_matrix_eval_float(rows, cols, &m, offset.as_deref(), &inf)
                .expect("oracle matrix float");
        for i in 0..rows {
            assert_f32_bits(
                got_f[i],
                exp_f[i],
                &format!("ns float trial={trial} out={i}"),
            );
        }

        let got_16 = pl.eval_16(&in16);
        let exp_16 = rcms_oracle::pipeline_matrix_eval16(rows, cols, &m, offset.as_deref(), &in16)
            .expect("oracle matrix 16");
        assert_eq!(got_16, exp_16, "ns 16 trial={trial}");
    }
}

/// Build `n_curves` random 16-bit tabulated curves of length `tbl_len`, returning
/// the rcms `ToneCurve`s and the contiguous table buffer for the oracle.
fn build_random_curves(
    rng: &mut Rng,
    n_curves: usize,
    tbl_len: usize,
) -> (Vec<ToneCurve>, Vec<u16>) {
    let mut tables = Vec::with_capacity(n_curves * tbl_len);
    let mut curves = Vec::with_capacity(n_curves);
    for _ in 0..n_curves {
        let tbl: Vec<u16> = (0..tbl_len).map(|_| rng.next_u64() as u16).collect();
        curves.push(build_tabulated_16(&tbl));
        tables.extend_from_slice(&tbl);
    }
    (curves, tables)
}

#[test]
fn tone_curves_stage_float_and_16() {
    let mut rng = Rng::new(0xC0FFEE);
    let n_curves = 3usize;

    for tbl_len in [2usize, 4, 17, 256] {
        for trial in 0..400 {
            let (curves, tables) = build_random_curves(&mut rng, n_curves, tbl_len);

            let mut pl = Pipeline::new(n_curves, n_curves);
            pl.insert_stage_at_end(Stage::ToneCurves(curves)).unwrap();

            let inf: Vec<f32> = (0..n_curves).map(|_| rand_in_f32(&mut rng)).collect();
            let in16: Vec<u16> = (0..n_curves).map(|_| rng.next_u64() as u16).collect();

            let got_f = pl.eval_float(&inf);
            let exp_f = rcms_oracle::pipeline_curves_eval_float(n_curves, tbl_len, &tables, &inf)
                .expect("oracle curves float");
            for i in 0..n_curves {
                assert_f32_bits(
                    got_f[i],
                    exp_f[i],
                    &format!("curves float len={tbl_len} trial={trial} out={i}"),
                );
            }

            let got_16 = pl.eval_16(&in16);
            let exp_16 = rcms_oracle::pipeline_curves_eval16(n_curves, tbl_len, &tables, &in16)
                .expect("oracle curves 16");
            assert_eq!(got_16, exp_16, "curves 16 len={tbl_len} trial={trial}");
        }
    }
}

#[test]
fn multi_stage_curves_then_matrix_float_and_16() {
    let mut rng = Rng::new(0xD15EA5E);
    let n_curves = 3usize; // == matrix cols
    let tbl_len = 33usize;

    for &(rows, cols) in &[(3usize, 3usize), (4usize, 3usize)] {
        assert_eq!(cols, n_curves);
        for trial in 0..600 {
            let (curves, tables) = build_random_curves(&mut rng, n_curves, tbl_len);
            let m: Vec<f64> = (0..rows * cols).map(|_| rand_m(&mut rng)).collect();
            let offset: Option<Vec<f64>> = if trial % 4 == 0 {
                None
            } else {
                Some((0..rows).map(|_| rand_off(&mut rng)).collect())
            };

            let mut pl = Pipeline::new(n_curves, rows);
            pl.insert_stage_at_end(Stage::ToneCurves(curves)).unwrap();
            pl.insert_stage_at_end(Stage::Matrix {
                rows,
                cols,
                m: m.clone(),
                offset: offset.clone(),
            })
            .unwrap();

            let inf: Vec<f32> = (0..n_curves).map(|_| rand_in_f32(&mut rng)).collect();
            let in16: Vec<u16> = (0..n_curves).map(|_| rng.next_u64() as u16).collect();

            let got_f = pl.eval_float(&inf);
            let exp_f = rcms_oracle::pipeline_curves_matrix_eval_float(
                n_curves,
                tbl_len,
                &tables,
                rows,
                cols,
                &m,
                offset.as_deref(),
                &inf,
            )
            .expect("oracle multi float");
            for i in 0..rows {
                assert_f32_bits(
                    got_f[i],
                    exp_f[i],
                    &format!("multi float {rows}x{cols} trial={trial} out={i}"),
                );
            }

            let got_16 = pl.eval_16(&in16);
            let exp_16 = rcms_oracle::pipeline_curves_matrix_eval16(
                n_curves,
                tbl_len,
                &tables,
                rows,
                cols,
                &m,
                offset.as_deref(),
                &in16,
            )
            .expect("oracle multi 16");
            assert_eq!(got_16, exp_16, "multi 16 {rows}x{cols} trial={trial}");
        }
    }
}

/// The 16-bit boundary conversion must match lcms2 exactly at 0x0000 / 0xFFFF and
/// across the full u16 range, using an identity-matrix pipeline (so the only
/// transform is From16ToFloat then FromFloatTo16).
#[test]
fn boundary_from16tofloat_fromfloatto16() {
    let rows = 3usize;
    let cols = 3usize;
    // Identity matrix, no offset: out == in in the float domain.
    let m = vec![
        1.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, //
        0.0, 0.0, 1.0,
    ];
    let mut pl = Pipeline::new(cols, rows);
    pl.insert_stage_at_end(Stage::Matrix {
        rows,
        cols,
        m: m.clone(),
        offset: None,
    })
    .unwrap();

    let mut rng = Rng::new(0xF00D);
    // Include the exact boundaries plus a dense random sweep.
    let mut samples: Vec<[u16; 3]> = vec![
        [0x0000, 0x0000, 0x0000],
        [0xFFFF, 0xFFFF, 0xFFFF],
        [0x0000, 0xFFFF, 0x8000],
        [0x7FFF, 0x8000, 0x0001],
    ];
    for _ in 0..50_000 {
        samples.push([
            rng.next_u64() as u16,
            rng.next_u64() as u16,
            rng.next_u64() as u16,
        ]);
    }

    for s in &samples {
        let got = pl.eval_16(s);
        let exp = rcms_oracle::pipeline_matrix_eval16(rows, cols, &m, None, s)
            .expect("oracle identity 16");
        assert_eq!(got, exp, "boundary 16 input={s:?}");

        // Confirm From16ToFloat narrows exactly: each output equals the input
        // round-tripped through lcms2's conversion (identity transform).
        let exp_f = rcms_oracle::pipeline_matrix_eval_float(
            rows,
            cols,
            &m,
            None,
            &[
                s[0] as f32 / 65535.0,
                s[1] as f32 / 65535.0,
                s[2] as f32 / 65535.0,
            ],
        )
        .expect("oracle identity float");
        let got_f = pl.eval_float(&[
            s[0] as f32 / 65535.0,
            s[1] as f32 / 65535.0,
            s[2] as f32 / 65535.0,
        ]);
        for i in 0..3 {
            assert_f32_bits(
                got_f[i],
                exp_f[i],
                &format!("boundary float input={s:?} i={i}"),
            );
        }
    }
}

/// Channel-chaining validation in `insert_stage_at_end`.
#[test]
fn insert_stage_validates_chaining() {
    let mut pl = Pipeline::new(3, 3);
    // First stage must accept the pipeline input width (3).
    assert!(pl
        .insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 4,
            m: vec![0.0; 12],
            offset: None,
        })
        .is_err());

    // 3->2 matrix is fine as the first stage.
    pl.insert_stage_at_end(Stage::Matrix {
        rows: 2,
        cols: 3,
        m: vec![0.0; 6],
        offset: None,
    })
    .unwrap();
    // Next stage must take 2 inputs; a 3-curve stage does not chain.
    assert!(pl
        .insert_stage_at_end(Stage::ToneCurves(vec![
            build_tabulated_16(&[0, 0xFFFF]),
            build_tabulated_16(&[0, 0xFFFF]),
            build_tabulated_16(&[0, 0xFFFF]),
        ]))
        .is_err());
    // A 2-channel identity does chain.
    pl.insert_stage_at_end(Stage::Identity(2)).unwrap();
    assert_eq!(pl.stages().len(), 2);
}

/// Total node count of a CLUT grid (product of per-axis sample counts).
fn cube_size(grid: &[u32]) -> usize {
    grid.iter().map(|&n| n as usize).product()
}

/// Build a `Clut` stage wrapped in a 1-stage pipeline, diff its float-domain
/// eval against the matching lcms2 CLUT-stage pipeline (via `cmsPipelineEvalFloat`).
#[test]
fn clut_stage_u16_3d_and_4d() {
    let mut rng = Rng::new(0xC10D_3D4D);

    // 3D (tetrahedral) and 4D (EvalN) grids, varied per-axis sample counts.
    let cases: &[(&[u32], usize)] = &[
        (&[2, 2, 2], 3),
        (&[3, 4, 5], 3),
        (&[9, 9, 9], 4),
        (&[2, 3, 4, 5], 3),
        (&[3, 3, 3, 3], 4),
    ];

    for &(grid, n_out) in cases {
        let n_in = grid.len();
        let table_len = cube_size(grid) * n_out;

        for trial in 0..200 {
            let table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();

            let clut = Clut {
                table: ClutTable::U16(table.clone()),
                params: InterpParams::new(grid, n_in, n_out),
            };
            let mut pl = Pipeline::new(n_in, n_out);
            pl.insert_stage_at_end(Stage::Clut(clut)).unwrap();

            let input: Vec<f32> = (0..n_in).map(|_| rand_in_f32(&mut rng)).collect();

            let got = pl.eval_float(&input);
            let exp = rcms_oracle::clut_stage_eval16(grid, n_out, &table, &input)
                .expect("oracle clut16 stage");
            for i in 0..n_out {
                assert_f32_bits(
                    got[i],
                    exp[i],
                    &format!("clut u16 grid={grid:?} nOut={n_out} trial={trial} out={i}"),
                );
            }
        }
    }
}

#[test]
fn clut_stage_f32_3d() {
    let mut rng = Rng::new(0xC10F3D);

    let cases: &[(&[u32], usize)] = &[(&[2, 2, 2], 3), (&[4, 5, 6], 3), (&[9, 9, 9], 4)];

    for &(grid, n_out) in cases {
        let n_in = grid.len();
        let table_len = cube_size(grid) * n_out;

        for trial in 0..200 {
            // Float CLUT values in [0,1) (lcms2's float CLUT domain).
            let table: Vec<f32> = (0..table_len).map(|_| rng.next_f64_unit() as f32).collect();

            let clut = Clut {
                table: ClutTable::F32(table.clone()),
                params: InterpParams::new(grid, n_in, n_out),
            };
            let mut pl = Pipeline::new(n_in, n_out);
            pl.insert_stage_at_end(Stage::Clut(clut)).unwrap();

            let input: Vec<f32> = (0..n_in).map(|_| rand_in_f32(&mut rng)).collect();

            let got = pl.eval_float(&input);
            let exp = rcms_oracle::clut_stage_eval_float(grid, n_out, &table, &input)
                .expect("oracle clut float stage");
            for i in 0..n_out {
                assert_f32_bits(
                    got[i],
                    exp[i],
                    &format!("clut f32 grid={grid:?} nOut={n_out} trial={trial} out={i}"),
                );
            }
        }
    }
}

/// Diff each Lab/XYZ conversion stage against the matching lcms2 stage eval over
/// random inputs in the valid normalized (0..1.0) domain.
#[test]
fn lab_xyz_stages_match_oracle() {
    let mut rng = Rng::new(0x1AB0072);

    // (which, Stage). which: 0=Lab2XYZ, 1=XYZ2Lab, 2=LabV2ToV4, 3=LabV4ToV2.
    let cases = [
        (0u32, Stage::Lab2Xyz),
        (1u32, Stage::Xyz2Lab),
        (2u32, Stage::LabV2ToV4),
        (3u32, Stage::LabV4ToV2),
    ];

    for (which, stage) in cases {
        let mut pl = Pipeline::new(3, 3);
        pl.insert_stage_at_end(stage).unwrap();

        for trial in 0..200_000 {
            let input = [
                rand_in_f32(&mut rng),
                rand_in_f32(&mut rng),
                rand_in_f32(&mut rng),
            ];
            let got = pl.eval_float(&input);
            let exp = rcms_oracle::labxyz_stage_eval(which, &input).expect("oracle labxyz stage");
            for i in 0..3 {
                assert_f32_bits(
                    got[i],
                    exp[i],
                    &format!("labxyz which={which} trial={trial} in={input:?} out={i}"),
                );
            }
        }
    }
}

/// A pipeline combining a CLUT stage + curves + matrix, diffed vs
/// `cmsPipelineEvalFloat`.
#[test]
fn pipeline_clut_curves_matrix_float() {
    let mut rng = Rng::new(0xC1C23A);

    let grid: &[u32] = &[5, 6, 7];
    let n_in = 3usize;
    let n_out = 4usize; // CLUT outputs == curve count == matrix cols
    let tbl_len = 33usize;
    let rows = 3usize;

    for trial in 0..500 {
        let table_len = cube_size(grid) * n_out;
        let clut_table: Vec<u16> = (0..table_len).map(|_| rng.next_u64() as u16).collect();
        let (curves, curve_tables) = build_random_curves(&mut rng, n_out, tbl_len);
        let m: Vec<f64> = (0..rows * n_out).map(|_| rand_m(&mut rng)).collect();
        let offset: Option<Vec<f64>> = if trial % 3 == 0 {
            None
        } else {
            Some((0..rows).map(|_| rand_off(&mut rng)).collect())
        };

        let mut pl = Pipeline::new(n_in, rows);
        pl.insert_stage_at_end(Stage::Clut(Clut {
            table: ClutTable::U16(clut_table.clone()),
            params: InterpParams::new(grid, n_in, n_out),
        }))
        .unwrap();
        pl.insert_stage_at_end(Stage::ToneCurves(curves)).unwrap();
        pl.insert_stage_at_end(Stage::Matrix {
            rows,
            cols: n_out,
            m: m.clone(),
            offset: offset.clone(),
        })
        .unwrap();

        let input: Vec<f32> = (0..n_in).map(|_| rand_in_f32(&mut rng)).collect();

        let got = pl.eval_float(&input);
        let exp = rcms_oracle::pipeline_clut_curves_matrix_eval_float(
            grid,
            n_out,
            &clut_table,
            tbl_len,
            &curve_tables,
            rows,
            &m,
            offset.as_deref(),
            &input,
        )
        .expect("oracle clut+curves+matrix");
        for i in 0..rows {
            assert_f32_bits(
                got[i],
                exp[i],
                &format!("clut+curves+matrix trial={trial} out={i}"),
            );
        }
    }
}
