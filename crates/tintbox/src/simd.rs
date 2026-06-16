//! Optional SIMD kernels (feature `simd`, via the `wide` crate).
//!
//! Every kernel here is BIT-IDENTICAL to its scalar counterpart in
//! [`crate::pipeline::stage`] / [`crate::interp`] and therefore to lcms2
//! `cmsFLAGS_NOOPTIMIZE`. The vectorization axis is chosen so each lane computes
//! exactly the scalar result with the scalar ops, in the scalar order — there is
//! NO cross-lane reduction and NO fused-multiply-add (FMA changes rounding).
//!
//! The whole module is gated on `#[cfg(feature = "simd")]`; with the feature OFF
//! the crate is byte-for-byte as before. `wide`'s API is safe, so the crate's
//! `#![forbid(unsafe_code)]` still holds.
//!
//! Two kernels live here:
//!
//! 1. [`matrix3x3_across_pixels`] — a 3x3 `f64` matrix-multiply vectorized ACROSS
//!    PIXELS (`f64x4`). Each lane is one pixel; its row sum stays in-lane, plain
//!    `*`/`+` in scalar order → bit-identical to the [`Stage::Matrix`] f64 arm.
//! 2. [`tetrahedral_across_outputs`] — lcms2's SSE2-style 16-bit tetrahedral with
//!    the per-output-channel interpolation vectorized ACROSS OUTPUT CHANNELS
//!    (`i32x4`). The per-pixel grid indexing + 6-way leaf pick is scalar (once),
//!    then the EXACT integer ops run per output channel in lanes. Integer math is
//!    exact → bit-identical to [`crate::interp::tetrahedral_16`].
//!
//! [`Stage::Matrix`]: crate::pipeline::Stage::Matrix

use crate::interp::InterpParams;
use wide::{f64x4, i32x4};

/// Vectorized 3x3 `f64` matrix-multiply (no offset) across `f64x4` pixel lanes,
/// bit-identical to the scalar [`Stage::Matrix`](crate::pipeline::Stage::Matrix)
/// f64 arm for `rows == cols == 3`, `offset == None`.
///
/// `inp` is `n` pixels of 3 interleaved f32 channels (`inp[p*3 + j]`); `out` is
/// `n` pixels of 3 f32 (`out[p*3 + i]`). `m` is the row-major 3x3 matrix
/// (`m[i*3 + j]`).
///
/// # Bit-identity
///
/// The scalar arm computes, per pixel, per output row `i`:
/// `tmp: f64 = 0.0; for j { tmp += in[j] as f64 * m[i*3+j]; } out[i] = tmp as f32`.
/// Here lane `l` of each vector holds pixel `p0+l`. We accumulate
/// `acc = acc + (in_j_vec * m_ij_splat)` with plain `*`/`+` (NOT `mul_add`), in
/// the SAME `j = 0,1,2` order, starting from `0.0`. Each lane therefore performs
/// the identical sequence of f64 rounding steps the scalar code does, so
/// `acc as f32` per lane equals the scalar `out[i]` bit-for-bit. There is no
/// cross-lane reduction: a pixel's three products+sums never leave its lane.
///
/// The tail (`n % 4`) runs the scalar code verbatim.
#[inline]
pub fn matrix3x3_across_pixels(inp: &[f32], out: &mut [f32], m: &[f64], n: usize) {
    debug_assert!(m.len() >= 9, "matrix3x3 needs a 3x3 matrix");
    debug_assert!(inp.len() >= n * 3 && out.len() >= n * 3);

    // Splat each matrix coefficient once.
    let m00 = f64x4::splat(m[0]);
    let m01 = f64x4::splat(m[1]);
    let m02 = f64x4::splat(m[2]);
    let m10 = f64x4::splat(m[3]);
    let m11 = f64x4::splat(m[4]);
    let m12 = f64x4::splat(m[5]);
    let m20 = f64x4::splat(m[6]);
    let m21 = f64x4::splat(m[7]);
    let m22 = f64x4::splat(m[8]);

    let lanes = 4;
    let mut p = 0;
    while p + lanes <= n {
        // Gather channel j across 4 consecutive pixels, widening f32 -> f64
        // exactly as the scalar `in[j] as f64`.
        let in0 = f64x4::new([
            inp[p * 3] as f64,
            inp[(p + 1) * 3] as f64,
            inp[(p + 2) * 3] as f64,
            inp[(p + 3) * 3] as f64,
        ]);
        let in1 = f64x4::new([
            inp[p * 3 + 1] as f64,
            inp[(p + 1) * 3 + 1] as f64,
            inp[(p + 2) * 3 + 1] as f64,
            inp[(p + 3) * 3 + 1] as f64,
        ]);
        let in2 = f64x4::new([
            inp[p * 3 + 2] as f64,
            inp[(p + 1) * 3 + 2] as f64,
            inp[(p + 2) * 3 + 2] as f64,
            inp[(p + 3) * 3 + 2] as f64,
        ]);

        // Row sums, accumulated from 0.0 in j-order with plain `*`/`+` — NO FMA.
        let o0 = f64x4::splat(0.0) + in0 * m00 + in1 * m01 + in2 * m02;
        let o1 = f64x4::splat(0.0) + in0 * m10 + in1 * m11 + in2 * m12;
        let o2 = f64x4::splat(0.0) + in0 * m20 + in1 * m21 + in2 * m22;

        let a0 = o0.to_array();
        let a1 = o1.to_array();
        let a2 = o2.to_array();
        for l in 0..lanes {
            out[(p + l) * 3] = a0[l] as f32;
            out[(p + l) * 3 + 1] = a1[l] as f32;
            out[(p + l) * 3 + 2] = a2[l] as f32;
        }
        p += lanes;
    }

    // Scalar tail — verbatim from the Stage::Matrix arm.
    while p < n {
        for i in 0..3 {
            let mut tmp: f64 = 0.0;
            for j in 0..3 {
                tmp += inp[p * 3 + j] as f64 * m[i * 3 + j];
            }
            out[p * 3 + i] = tmp as f32;
        }
        p += 1;
    }
}

/// `_cmsToFixedDomain`'s `FIXED_TO_INT(x)` (`x >> 16`, arithmetic).
#[inline]
fn fixed_to_int(x: i32) -> i32 {
    x >> 16
}

/// `FIXED_REST_TO_INT(x)` (`x & 0xFFFF`).
#[inline]
fn fixed_rest_to_int(x: i32) -> i32 {
    x & 0xFFFF
}

/// 16-bit 3D tetrahedral interpolation with the per-output-channel interpolation
/// vectorized across output channels (`i32x4`), bit-identical to
/// [`crate::interp::tetrahedral_16`].
///
/// `input` is 3 u16; `output` receives `p.n_outputs` u16; `table` is the
/// flattened CLUT grid. The grid indexing (`x0/x1/y0/y1/z0/z1`, the rest values
/// `rx/ry/rz`) and the 6-way leaf selection are PER-PIXEL — computed once, in
/// scalar, exactly as the scalar kernel. The vector part is the per-output-channel
/// interpolation: for a group of up to 4 output channels we load `c0,c1,c2,c3`
/// for each channel into `i32x4` lanes (one lane per channel), apply the SAME
/// integer ops the scalar loop does, and store. Integer math is exact, the same
/// `to_fixed_domain` rest weights are reused, so each lane reproduces the scalar
/// `out[oc]` bit-for-bit.
///
/// This mirrors lcms2's `TetrahedralInterp16` SSE2 path: the leaf branch decides
/// which three offset triples `(o1,o2,o3)` (relative to the base `lut`) feed
/// `c1,c2,c3`, then every output channel interpolates identically.
pub fn tetrahedral_across_outputs(
    input: &[u16],
    output: &mut [u16],
    table: &[u16],
    p: &InterpParams,
) {
    let fx: i32 = crate::fixed::to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let fy: i32 = crate::fixed::to_fixed_domain(input[1] as i32 * p.domain[1] as i32);
    let fz: i32 = crate::fixed::to_fixed_domain(input[2] as i32 * p.domain[2] as i32);

    let x0 = fixed_to_int(fx);
    let y0 = fixed_to_int(fy);
    let z0 = fixed_to_int(fz);

    let rx: i32 = fixed_rest_to_int(fx);
    let ry: i32 = fixed_rest_to_int(fy);
    let rz: i32 = fixed_rest_to_int(fz);

    let x0_idx = p.opta[2] * x0 as u32;
    let mut x1: u32 = if input[0] == 0xFFFF { 0 } else { p.opta[2] };
    let y0_idx = p.opta[1] * y0 as u32;
    let mut y1: u32 = if input[1] == 0xFFFF { 0 } else { p.opta[1] };
    let z0_idx = p.opta[0] * z0 as u32;
    let mut z1: u32 = if input[2] == 0xFFFF { 0 } else { p.opta[0] };

    let lut: usize = (x0_idx + y0_idx + z0_idx) as usize;
    let total_out = p.n_outputs;

    // Select the leaf: compute the three corner offsets (relative to `lut`) for
    // c1, c2, c3 and the sign pattern. The scalar kernel forms each `c1/c2/c3` as a
    // difference of two looked-up corner densities; we encode that as a pair of
    // offsets per c-term and subtract. The corner offsets used by every branch are
    // among {0, x1, y1, z1} after the branch's `+=` fixups, plus `lut` base.
    //
    // We replicate the scalar branches exactly: each branch first does the same
    // `x1/y1/z1 +=` accumulation, then forms c1,c2,c3 as differences of the SAME
    // corners. To stay bit-identical we reproduce the corner choice per branch.
    //
    // For the i32x4 vectorization we need, per output channel, four corner reads
    // (c0 plus the three corners that feed the differences). Different branches mix
    // the corners differently (e.g. `c3 = c3 - c2`), so we capture, per branch, the
    // three "high" corner offsets (oh1, oh2, oh3) and three "low" corner offsets
    // (ol1, ol2, ol3) such that `ci = table[lut+ohi] - table[lut+oli]`, then the
    // vector interp is `out = c0 + ((c1*rx + c2*ry + c3*rz + 0x8001 + (...>>16))>>16)`.
    let (oh1, ol1, oh2, ol2, oh3, ol3);
    if rx >= ry {
        if ry >= rz {
            y1 += x1;
            z1 += y1;
            // c1=table[x1]-c0; c2=table[y1]-table[x1]; c3=table[z1]-table[y1]
            oh1 = x1;
            ol1 = 0;
            oh2 = y1;
            ol2 = x1;
            oh3 = z1;
            ol3 = y1;
        } else if rz >= rx {
            x1 += z1;
            y1 += x1;
            // c1=table[x1]-table[z1]; c2=table[y1]-table[x1]; c3=table[z1]-c0
            oh1 = x1;
            ol1 = z1;
            oh2 = y1;
            ol2 = x1;
            oh3 = z1;
            ol3 = 0;
        } else {
            z1 += x1;
            y1 += z1;
            // c1=table[x1]-c0; c2=table[y1]-table[z1]; c3=table[z1]-table[x1]
            oh1 = x1;
            ol1 = 0;
            oh2 = y1;
            ol2 = z1;
            oh3 = z1;
            ol3 = x1;
        }
    } else if rx >= rz {
        x1 += y1;
        z1 += x1;
        // c1=table[x1]-table[y1]; c2=table[y1]-c0; c3=table[z1]-table[x1]
        oh1 = x1;
        ol1 = y1;
        oh2 = y1;
        ol2 = 0;
        oh3 = z1;
        ol3 = x1;
    } else if ry >= rz {
        z1 += y1;
        x1 += z1;
        // c1=table[x1]-table[z1]; c2=table[y1]-c0; c3=table[z1]-table[y1]
        oh1 = x1;
        ol1 = z1;
        oh2 = y1;
        ol2 = 0;
        oh3 = z1;
        ol3 = y1;
    } else {
        y1 += z1;
        x1 += y1;
        // c1=table[x1]-table[y1]; c2=table[y1]-table[z1]; c3=table[z1]-c0
        oh1 = x1;
        ol1 = y1;
        oh2 = y1;
        ol2 = z1;
        oh3 = z1;
        ol3 = 0;
    }

    let rx_v = i32x4::splat(rx);
    let ry_v = i32x4::splat(ry);
    let rz_v = i32x4::splat(rz);
    let bias = i32x4::splat(0x8001);

    let oh1 = oh1 as usize;
    let ol1 = ol1 as usize;
    let oh2 = oh2 as usize;
    let ol2 = ol2 as usize;
    let oh3 = oh3 as usize;
    let ol3 = ol3 as usize;

    let mut oc = 0usize;
    while oc + 4 <= total_out {
        let base = lut + oc;
        // c0 and the three differences, per output channel into lanes.
        let c0 = i32x4::new([
            table[base] as i32,
            table[base + 1] as i32,
            table[base + 2] as i32,
            table[base + 3] as i32,
        ]);
        let load = |off: usize| -> i32x4 {
            i32x4::new([
                table[base + off] as i32,
                table[base + off + 1] as i32,
                table[base + off + 2] as i32,
                table[base + off + 3] as i32,
            ])
        };
        let c1 = load(oh1) - load(ol1);
        let c2 = load(oh2) - load(ol2);
        let c3 = load(oh3) - load(ol3);

        // rest = c1*rx + c2*ry + c3*rz + 0x8001 (wrapping i32, exact in lanes)
        let rest = c1 * rx_v + c2 * ry_v + c3 * rz_v + bias;
        // out = (c0 as u16) + (((rest + (rest>>16))>>16) as u16). The `>> 16` is an
        // ARITHMETIC shift on the signed i32 lanes (wide's `Shr<i32>` = wrapping_shr),
        // matching the scalar `rest >> 16` on `i32`.
        let shifted: i32x4 = (rest + (rest >> 16i32)) >> 16i32;
        let res = c0 + shifted;
        let a = res.to_array();
        // Truncate to u16 exactly as the scalar `as u16` casts do. The scalar code
        // does `(c0 as u16).wrapping_add((... ) as u16)`; `c0 + shifted` in i32 then
        // `as u16` is the same low-16-bit result (two's-complement add commutes with
        // truncation).
        output[oc] = a[0] as u16;
        output[oc + 1] = a[1] as u16;
        output[oc + 2] = a[2] as u16;
        output[oc + 3] = a[3] as u16;
        oc += 4;
    }

    // Scalar remainder (1..=3 output channels) — verbatim corner math.
    while oc < total_out {
        let base = lut + oc;
        let c0 = table[base] as i32;
        let c1 = table[base + oh1] as i32 - table[base + ol1] as i32;
        let c2 = table[base + oh2] as i32 - table[base + ol2] as i32;
        let c3 = table[base + oh3] as i32 - table[base + ol3] as i32;
        let rest = c1
            .wrapping_mul(rx)
            .wrapping_add(c2.wrapping_mul(ry))
            .wrapping_add(c3.wrapping_mul(rz))
            .wrapping_add(0x8001);
        output[oc] = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        oc += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{tetrahedral_16, InterpParams};
    use crate::pipeline::Stage;

    /// Scalar reference for the matrix kernel: the exact `Stage::Matrix` f64 arm.
    fn matrix_scalar(inp: &[f32], out: &mut [f32], m: &[f64], n: usize) {
        let stage = Stage::Matrix {
            rows: 3,
            cols: 3,
            m: m.to_vec(),
            offset: None,
        };
        for p in 0..n {
            let mut o = [0.0f32; 3];
            stage.eval(&inp[p * 3..p * 3 + 3], &mut o);
            out[p * 3..p * 3 + 3].copy_from_slice(&o);
        }
    }

    #[test]
    fn matrix3x3_bit_identical_to_scalar() {
        // A handful of representative matrices incl. negatives, tiny, large.
        let mats: [[f64; 9]; 4] = [
            [
                0.4124, 0.3576, 0.1805, 0.2126, 0.7152, 0.0722, 0.0193, 0.1192, 0.9505,
            ],
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            [-0.5, 1.3, 0.2, 0.9, -0.1, 0.2, 0.05, 0.15, 0.8],
            [
                3.2406, -1.5372, -0.4986, -0.9689, 1.8758, 0.0415, 0.0557, -0.2040, 1.0570,
            ],
        ];
        let mut s = 0x12345678u32;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 0.5 // range ~[-0.5, 1.5]
        };
        let n = 1000usize; // exercises the f64x4 body + a non-zero tail (1000 % 4 = 0; use 1003)
        let n = n + 3;
        for m in &mats {
            let inp: Vec<f32> = (0..n * 3).map(|_| rnd()).collect();
            let mut got = vec![0.0f32; n * 3];
            let mut want = vec![0.0f32; n * 3];
            matrix3x3_across_pixels(&inp, &mut got, m, n);
            matrix_scalar(&inp, &mut want, m, n);
            for i in 0..n * 3 {
                assert_eq!(
                    got[i].to_bits(),
                    want[i].to_bits(),
                    "matrix mismatch at {i} (m={m:?})"
                );
            }
        }
    }

    /// Build a test CLUT grid table (n_samples^3 nodes, n_out channels) with a
    /// deterministic non-trivial pattern.
    fn make_table(n_samples: u32, n_out: usize, seed: usize) -> Vec<u16> {
        let nodes = (n_samples as usize).pow(3);
        (0..nodes * n_out)
            .map(|i| ((i * 40503 + seed * 7919 + 137) & 0xffff) as u16)
            .collect()
    }

    #[test]
    fn tetrahedral_bit_identical_rgb_and_cmyk() {
        // 3-out (RGB, exercises the scalar remainder: 3 % 4 = 3) and 4-out (CMYK,
        // exercises one full i32x4 group with no remainder) and 6-out (one group +
        // 2 remainder).
        for &n_out in &[3usize, 4, 6, 8] {
            for &n_samples in &[2u32, 3, 9, 17] {
                let table = make_table(n_samples, n_out, n_out * 31 + n_samples as usize);
                let p = InterpParams::new(&[n_samples; 3], 3, n_out);
                let mut s = 0xCAFEBABEu32.wrapping_add((n_out as u32) << 8 | n_samples);
                let mut rnd16 = || {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    (s >> 16) as u16
                };
                // Many random pixels, plus the 0/0xFFFF poles on each axis.
                let mut pixels: Vec<[u16; 3]> =
                    (0..4000).map(|_| [rnd16(), rnd16(), rnd16()]).collect();
                for &a in &[0u16, 0xFFFF] {
                    for &b in &[0u16, 0xFFFF, 0x8000] {
                        for &c in &[0u16, 0xFFFF, 0x4000] {
                            pixels.push([a, b, c]);
                        }
                    }
                }
                for px in &pixels {
                    let mut got = vec![0u16; n_out];
                    let mut want = vec![0u16; n_out];
                    tetrahedral_across_outputs(px, &mut got, &table, &p);
                    tetrahedral_16(px, &mut want, &table, &p);
                    assert_eq!(
                        got, want,
                        "tetrahedral mismatch px={px:?} n_out={n_out} n_samples={n_samples}"
                    );
                }
            }
        }
    }
}
