//! 1D interpolation (lcms2 `cmsintrp.c`). Transcribed verbatim so the integer
//! and floating-point cells match lcms2 bit-for-bit.

use crate::fixed::to_fixed_domain;

/// lcms2 `LinearInterp` (cmsintrp.c:184-189), the inline fixed-point cell:
///
/// ```c
/// cmsUInt32Number dif = (cmsUInt32Number) (h - l) * a + 0x8000;
/// dif = (dif >> 16) + l;
/// return (cmsUInt16Number) (dif);
/// ```
///
/// `a`, `l`, `h` are `cmsS15Fixed16Number` (`int`) at the call site; the callers
/// in `LinLerp1D` pass `rest` (a fixed-point remainder) and two `cmsUInt16Number`
/// table entries that promote to `int`. We therefore take `i32` args and match
/// the C integer pipeline exactly: `h - l` is computed in `i32`, cast to `u32`,
/// multiplied by `a as u32`, `+ 0x8000`, all with wrapping (C unsigned) semantics,
/// then `>> 16`, `+ l` (as `u32`), and truncated to `u16`.
#[inline]
fn linear_interp(a: i32, l: i32, h: i32) -> u16 {
    let dif = ((h - l) as u32).wrapping_mul(a as u32).wrapping_add(0x8000);
    let dif = (dif >> 16).wrapping_add(l as u32);
    dif as u16
}

/// lcms2 `LinLerp1D` (cmsintrp.c:193-221): 16-bit fixed-point 1D interpolation.
///
/// `domain` is the lcms2 `p->Domain[0]` (= `table.len() - 1`).
pub fn lin_lerp_16(value: u16, table: &[u16], domain: u32) -> u16 {
    // if last value or just one point
    if value == 0xffff || domain == 0 {
        table[domain as usize]
    } else {
        // val3 = p->Domain[0] * Value[0]; val3 = _cmsToFixedDomain(val3); (15.16)
        let val3 = to_fixed_domain((domain * value as u32) as i32);

        let cell0 = (val3 >> 16) as usize; // FIXED_TO_INT: 16 MSB bits
        let rest = val3 & 0xffff; // FIXED_REST_TO_INT: 16 LSB bits

        let y0 = table[cell0];
        let y1 = table[cell0 + 1];

        linear_interp(rest, y0 as i32, y1 as i32)
    }
}

/// lcms2 `fclamp` (cmsintrp.c:224-227): clamp to `[0, 1]`, mapping tiny/NaN to 0.
///
/// ```c
/// return ((v < 1.0e-9f) || isnan(v)) ? 0.0f : (v > 1.0f ? 1.0f : v);
/// ```
#[inline]
fn fclamp(v: f32) -> f32 {
    if v < 1.0e-9_f32 || v.is_nan() {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// lcms2 `LinLerp1Dfloat` (cmsintrp.c:230-261): floating-point 1D interpolation.
///
/// `domain` is the lcms2 `p->Domain[0]` (= `table.len() - 1`).
pub fn lin_lerp_1d_float(value: f32, table: &[f32], domain: u32) -> f32 {
    let val2 = fclamp(value);

    // if last value...
    if val2 == 1.0 || domain == 0 {
        table[domain as usize]
    } else {
        let val2 = val2 * domain as f32;

        // C: cell0 = (int)floor(val2); cell1 = (int)ceil(val2). These are the
        // libm floor/ceil (NOT the quick_floor magic hack). The float arg promotes
        // to double for the libm call, so floor/ceil run in f64 then truncate to int.
        let cell0 = (val2 as f64).floor() as i32 as usize;
        let cell1 = (val2 as f64).ceil() as i32 as usize;

        // Rest is 16 LSB bits
        let rest = val2 - cell0 as f32;

        let y0 = table[cell0];
        let y1 = table[cell1];

        y0 + (y1 - y0) * rest
    }
}
