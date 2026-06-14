//! n-D interpolation parameters and 3D tetrahedral interpolators, transcribed
//! bit-for-bit from lcms2's `cmsintrp.c`.
//!
//! `InterpParams` mirrors `_cmsComputeInterpParamsEx` (the opta/domain setup);
//! [`tetrahedral_16`] mirrors `TetrahedralInterp16` (the 2-level nested-branch
//! 6-leaf Sakamoto routine) and [`tetrahedral_float`] mirrors the separate flat
//! 6-case `TetrahedralInterpFloat`.

use crate::fixed::to_fixed_domain;

/// `_cmsToFixedDomain`'s `FIXED_TO_INT(x)` macro (`x >> 16`). Rust `i32 >> 16`
/// is an arithmetic shift, matching C's signed `cmsS15Fixed16Number` shift.
#[inline]
const fn fixed_to_int(x: i32) -> i32 {
    x >> 16
}

/// `FIXED_REST_TO_INT(x)` macro (`x & 0xFFFF`).
#[inline]
const fn fixed_rest_to_int(x: i32) -> i32 {
    x & 0xFFFF
}

/// `fclamp` (cmsintrp.c:224): clamp to `[0, 1]`, mapping NaN and sub-1e-9 to 0.
#[inline]
fn fclamp(v: f32) -> f32 {
    if v < 1.0e-9f32 || v.is_nan() {
        0.0f32
    } else if v > 1.0f32 {
        1.0f32
    } else {
        v
    }
}

/// Precomputed parameters for n-D grid interpolation, mirroring lcms2's
/// `cmsInterpParams` fields populated by `_cmsComputeInterpParamsEx`.
///
/// The CLUT table itself is passed to the interpolation functions separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpParams {
    /// Number of input channels (grid dimensions).
    pub n_inputs: usize,
    /// Number of output channels per grid node.
    pub n_outputs: usize,
    /// Number of samples (nodes) along each input dimension.
    pub n_samples: Vec<u32>,
    /// `n_samples[i] - 1` per input dimension.
    pub domain: Vec<u32>,
    /// Strides used to index the flattened grid array (`opta`).
    pub opta: Vec<u32>,
}

impl InterpParams {
    /// Mirror of `_cmsComputeInterpParamsEx`'s domain/opta setup
    /// (cmsintrp.c:136-146).
    ///
    /// `domain[i] = n_samples[i] - 1`; `opta[0] = n_outputs`; and for
    /// `i in 1..n_inputs`, `opta[i] = opta[i-1] * n_samples[n_inputs - i]`
    /// (the reversed `n_samples` index is transcribed exactly from the C).
    #[must_use]
    pub fn new(n_samples: &[u32], n_inputs: usize, n_outputs: usize) -> Self {
        let mut domain = vec![0u32; n_inputs];
        for i in 0..n_inputs {
            domain[i] = n_samples[i] - 1;
        }

        let mut opta = vec![0u32; n_inputs];
        opta[0] = n_outputs as u32;
        for i in 1..n_inputs {
            opta[i] = opta[i - 1] * n_samples[n_inputs - i];
        }

        Self {
            n_inputs,
            n_outputs,
            n_samples: n_samples.to_vec(),
            domain,
            opta,
        }
    }
}

/// 16-bit 3D tetrahedral interpolation, bit-identical to
/// `TetrahedralInterp16` (cmsintrp.c:720-850).
///
/// `input` is 3 u16 channels, `output` receives `p.n_outputs` u16 channels, and
/// `table` is the flattened CLUT grid (`n_outputs` u16 per node, lcms2 layout).
///
/// # Panics
/// Panics (via slice indexing) if `table`, `input`, or `output` are sized
/// inconsistently with `p`.
pub fn tetrahedral_16(input: &[u16], output: &mut [u16], table: &[u16], p: &InterpParams) {
    // const cmsUInt16Number* LutTable = (cmsUInt16Number*) p -> Table;
    // We track a base index `lut` into `table` instead of advancing a pointer.

    let fx: i32 = to_fixed_domain(input[0] as i32 * p.domain[0] as i32);
    let fy: i32 = to_fixed_domain(input[1] as i32 * p.domain[1] as i32);
    let fz: i32 = to_fixed_domain(input[2] as i32 * p.domain[2] as i32);

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

    // LutTable += X0+Y0+Z0;
    let mut lut: usize = (x0_idx + y0_idx + z0_idx) as usize;

    let total_out = p.n_outputs;

    // Output should be computed as x = ROUND_FIXED_TO_INT(_cmsToFixedDomain(Rest))
    // ... t = Rest+0x8001, x = (t + (t>>16))>>16.
    if rx >= ry {
        if ry >= rz {
            y1 += x1;
            z1 += y1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c3 = c3 - c2;
                let c2 = c2 - c1;
                let c1 = c1 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        } else if rz >= rx {
            x1 += z1;
            y1 += x1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c2 = c2 - c1;
                let c1 = c1 - c3;
                let c3 = c3 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        } else {
            z1 += x1;
            y1 += z1;
            for out in output.iter_mut().take(total_out) {
                let c1 = table[lut + x1 as usize] as i32;
                let c2 = table[lut + y1 as usize] as i32;
                let c3 = table[lut + z1 as usize] as i32;
                let c0 = table[lut] as i32;
                lut += 1;
                let c2 = c2 - c3;
                let c3 = c3 - c1;
                let c1 = c1 - c0;
                let rest = c1
                    .wrapping_mul(rx)
                    .wrapping_add(c2.wrapping_mul(ry))
                    .wrapping_add(c3.wrapping_mul(rz))
                    .wrapping_add(0x8001);
                *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
            }
        }
    } else if rx >= rz {
        x1 += y1;
        z1 += x1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c3 = c3 - c1;
            let c1 = c1 - c2;
            let c2 = c2 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    } else if ry >= rz {
        z1 += y1;
        x1 += z1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c1 = c1 - c3;
            let c3 = c3 - c2;
            let c2 = c2 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    } else {
        y1 += z1;
        x1 += y1;
        for out in output.iter_mut().take(total_out) {
            let c1 = table[lut + x1 as usize] as i32;
            let c2 = table[lut + y1 as usize] as i32;
            let c3 = table[lut + z1 as usize] as i32;
            let c0 = table[lut] as i32;
            lut += 1;
            let c1 = c1 - c2;
            let c2 = c2 - c3;
            let c3 = c3 - c0;
            let rest = c1
                .wrapping_mul(rx)
                .wrapping_add(c2.wrapping_mul(ry))
                .wrapping_add(c3.wrapping_mul(rz))
                .wrapping_add(0x8001);
            *out = (c0 as u16).wrapping_add(((rest.wrapping_add(rest >> 16)) >> 16) as u16);
        }
    }
}

/// Float 3D tetrahedral interpolation, bit-identical to
/// `TetrahedralInterpFloat` (cmsintrp.c:622-716).
///
/// `input` is 3 f32 channels, `output` receives `p.n_outputs` f32 channels, and
/// `table` is the flattened CLUT grid (`n_outputs` f32 per node).
pub fn tetrahedral_float(input: &[f32], output: &mut [f32], table: &[f32], p: &InterpParams) {
    // #define DENS(i,j,k) (LutTable[(i)+(j)+(k)+OutChan])
    let dens =
        |x: i32, y: i32, z: i32, out_chan: i32| -> f32 { table[(x + y + z + out_chan) as usize] };

    let total_out = p.n_outputs as i32;

    // We need some clipping here
    let px = fclamp(input[0]) * p.domain[0] as f32;
    let py = fclamp(input[1]) * p.domain[1] as f32;
    let pz = fclamp(input[2]) * p.domain[2] as f32;

    let x0 = px.floor() as i32;
    let rx = px - x0 as f32;
    let y0 = py.floor() as i32;
    let ry = py - y0 as f32;
    let z0 = pz.floor() as i32;
    let rz = pz - z0 as f32;

    let x0i = p.opta[2] as i32 * x0;
    let x1 = x0i
        + if fclamp(input[0]) >= 1.0 {
            0
        } else {
            p.opta[2] as i32
        };

    let y0i = p.opta[1] as i32 * y0;
    let y1 = y0i
        + if fclamp(input[1]) >= 1.0 {
            0
        } else {
            p.opta[1] as i32
        };

    let z0i = p.opta[0] as i32 * z0;
    let z1 = z0i
        + if fclamp(input[2]) >= 1.0 {
            0
        } else {
            p.opta[0] as i32
        };

    let mut out_chan = 0i32;
    while out_chan < total_out {
        // These are the 6 Tetrahedral
        let c0 = dens(x0i, y0i, z0i, out_chan);
        let c1;
        let c2;
        let c3;

        if rx >= ry && ry >= rz {
            c1 = dens(x1, y0i, z0i, out_chan) - c0;
            c2 = dens(x1, y1, z0i, out_chan) - dens(x1, y0i, z0i, out_chan);
            c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0i, out_chan);
        } else if rx >= rz && rz >= ry {
            c1 = dens(x1, y0i, z0i, out_chan) - c0;
            c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0i, z1, out_chan);
            c3 = dens(x1, y0i, z1, out_chan) - dens(x1, y0i, z0i, out_chan);
        } else if rz >= rx && rx >= ry {
            c1 = dens(x1, y0i, z1, out_chan) - dens(x0i, y0i, z1, out_chan);
            c2 = dens(x1, y1, z1, out_chan) - dens(x1, y0i, z1, out_chan);
            c3 = dens(x0i, y0i, z1, out_chan) - c0;
        } else if ry >= rx && rx >= rz {
            c1 = dens(x1, y1, z0i, out_chan) - dens(x0i, y1, z0i, out_chan);
            c2 = dens(x0i, y1, z0i, out_chan) - c0;
            c3 = dens(x1, y1, z1, out_chan) - dens(x1, y1, z0i, out_chan);
        } else if ry >= rz && rz >= rx {
            c1 = dens(x1, y1, z1, out_chan) - dens(x0i, y1, z1, out_chan);
            c2 = dens(x0i, y1, z0i, out_chan) - c0;
            c3 = dens(x0i, y1, z1, out_chan) - dens(x0i, y1, z0i, out_chan);
        } else if rz >= ry && ry >= rx {
            c1 = dens(x1, y1, z1, out_chan) - dens(x0i, y1, z1, out_chan);
            c2 = dens(x0i, y1, z1, out_chan) - dens(x0i, y0i, z1, out_chan);
            c3 = dens(x0i, y0i, z1, out_chan) - c0;
        } else {
            c1 = 0.0;
            c2 = 0.0;
            c3 = 0.0;
        }

        output[out_chan as usize] = c0 + c1 * rx + c2 * ry + c3 * rz;
        out_chan += 1;
    }
}
