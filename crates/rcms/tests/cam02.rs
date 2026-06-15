//! Differential test for the CIECAM02 appearance model (`rcms::cam02`) against
//! lcms2 `cmsCIECAM02Forward` / `cmsCIECAM02Reverse`, bit-for-bit (`f64::to_bits`).
//!
//! Sweeps many XYZ inputs across several viewing conditions (varying whitepoint,
//! La, Yb, surround) and asserts:
//! - Forward: rcms JCh == lcms2 JCh (each of J, C, h bit-exact),
//! - Reverse: rcms XYZ == lcms2 XYZ over the JCh produced by the forward pass.
//!
//! lcms2 uses `f64` throughout and rcms transcribes the same operation order, so
//! the contract is exact bit-equality, not tolerance.

use rcms::cam02::{
    Cam02, ViewingConditions, AVG_SURROUND, CUTSHEET_SURROUND, DARK_SURROUND, DIM_SURROUND,
    D_CALCULATE,
};
use rcms::color::{JCh, CIEXYZ};
use rcms_oracle::OracleCam02;

/// (whitepoint XYZ, Yb, La, surround, D_value) for each viewing condition cell.
fn viewing_conditions() -> Vec<([f64; 3], f64, f64, u32, f64)> {
    // D50 and D65 white points (scaled to Y=100, the CAM02 convention).
    let d50 = [96.422, 100.0, 82.521];
    let d65 = [95.047, 100.0, 108.883];
    let mut out = Vec::new();
    for wp in [d50, d65] {
        for la in [4.0, 20.0, 100.0, 318.31] {
            for yb in [16.0, 20.0, 25.0] {
                for surround in [AVG_SURROUND, DIM_SURROUND, DARK_SURROUND, CUTSHEET_SURROUND] {
                    // Exercise both an explicit D and the D_CALCULATE path.
                    out.push((wp, yb, la, surround, D_CALCULATE));
                    out.push((wp, yb, la, surround, 1.0));
                }
            }
        }
    }
    out
}

/// A spread of XYZ samples (Y up to ~100, the CAM02 absolute scale), including
/// neutrals, primaries-ish corners, and the white point itself.
fn xyz_samples() -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    for &x in &[0.5, 5.0, 19.01, 50.0, 95.05] {
        for &y in &[0.5, 5.0, 20.0, 50.0, 100.0] {
            for &z in &[0.5, 5.0, 21.78, 60.0, 108.88] {
                out.push([x, y, z]);
            }
        }
    }
    // A few neutrals along the achromatic axis.
    for &g in &[1.0, 10.0, 40.0, 80.0, 100.0] {
        out.push([0.9505 * g, g, 1.0888 * g]);
    }
    out
}

fn bits_eq(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

#[test]
fn cam02_forward_and_reverse_bit_exact_vs_lcms2() {
    let samples = xyz_samples();
    let mut fwd_cells = 0u64;
    let mut rev_cells = 0u64;

    for (wp, yb, la, surround, d_value) in viewing_conditions() {
        let model = Cam02::new(&ViewingConditions {
            white_point: CIEXYZ {
                x: wp[0],
                y: wp[1],
                z: wp[2],
            },
            yb,
            la,
            surround,
            d_value,
        });
        let oracle = OracleCam02::new(wp, yb, la, surround, d_value);

        for xyz in &samples {
            // ---- Forward ----
            let r_jch = model.forward(&CIEXYZ {
                x: xyz[0],
                y: xyz[1],
                z: xyz[2],
            });
            let o_jch = oracle.forward(*xyz);

            assert!(
                bits_eq(r_jch.j, o_jch[0])
                    && bits_eq(r_jch.c, o_jch[1])
                    && bits_eq(r_jch.h, o_jch[2]),
                "Forward mismatch wp={wp:?} Yb={yb} La={la} surround={surround} D={d_value} \
                 XYZ={xyz:?}: rcms JCh=({},{},{}) [{:#018x},{:#018x},{:#018x}] vs \
                 lcms2=({},{},{}) [{:#018x},{:#018x},{:#018x}]",
                r_jch.j,
                r_jch.c,
                r_jch.h,
                r_jch.j.to_bits(),
                r_jch.c.to_bits(),
                r_jch.h.to_bits(),
                o_jch[0],
                o_jch[1],
                o_jch[2],
                o_jch[0].to_bits(),
                o_jch[1].to_bits(),
                o_jch[2].to_bits(),
            );
            fwd_cells += 1;

            // ---- Reverse (round-trip the forward JCh through both inverses) ----
            let r_xyz = model.reverse(&JCh {
                j: r_jch.j,
                c: r_jch.c,
                h: r_jch.h,
            });
            let o_xyz = oracle.reverse(o_jch);

            assert!(
                bits_eq(r_xyz.x, o_xyz[0])
                    && bits_eq(r_xyz.y, o_xyz[1])
                    && bits_eq(r_xyz.z, o_xyz[2]),
                "Reverse mismatch wp={wp:?} Yb={yb} La={la} surround={surround} D={d_value} \
                 JCh=({},{},{}): rcms XYZ=({},{},{}) [{:#018x},{:#018x},{:#018x}] vs \
                 lcms2=({},{},{}) [{:#018x},{:#018x},{:#018x}]",
                r_jch.j,
                r_jch.c,
                r_jch.h,
                r_xyz.x,
                r_xyz.y,
                r_xyz.z,
                r_xyz.x.to_bits(),
                r_xyz.y.to_bits(),
                r_xyz.z.to_bits(),
                o_xyz[0],
                o_xyz[1],
                o_xyz[2],
                o_xyz[0].to_bits(),
                o_xyz[1].to_bits(),
                o_xyz[2].to_bits(),
            );
            rev_cells += 1;
        }
    }

    println!(
        "cam02 bit-exact: forward {fwd_cells} cells, reverse {rev_cells} cells \
         ({} viewing conditions x {} XYZ samples)",
        viewing_conditions().len(),
        samples.len(),
    );
    assert!(fwd_cells > 0 && rev_cells > 0);
}
