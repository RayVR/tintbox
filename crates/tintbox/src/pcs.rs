//! PCS (Profile Connection Space) color conversions, transcribed verbatim from
//! lcms2 `src/cmspcs.c` and bit-identical to it.
//!
//! Covers XYZ <-> Lab, XYZ <-> xyY, Lab <-> LCh, and the ICC v2/v4 Lab and XYZ
//! 16-bit encodings. A `None` white point defaults to D50 exactly as lcms2 does
//! (`cmsXYZ2Lab`/`cmsLab2XYZ` substitute `cmsD50_XYZ()` for a NULL pointer).

use crate::color::{CIELCh, CIELab, CIExyY, CIEXYZ};
use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::math::whitepoint::D50;

// Encodeable-range constants (lcms2_internal.h:71-75).
const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;
const MIN_ENCODEABLE_AB2: f64 = -128.0;
const MAX_ENCODEABLE_AB2: f64 = (65535.0 / 256.0) - 128.0;
const MIN_ENCODEABLE_AB4: f64 = -128.0;
const MAX_ENCODEABLE_AB4: f64 = 127.0;

/// CIELab cube-root forward helper `f(t)` (cmspcs.c:118-127).
pub fn f(t: f64) -> f64 {
    let limit = (24.0 / 116.0) * (24.0 / 116.0) * (24.0 / 116.0);
    if t <= limit {
        (841.0 / 108.0) * t + (16.0 / 116.0)
    } else {
        t.powf(1.0 / 3.0)
    }
}

/// CIELab cube-root inverse helper `f_1(t)` (cmspcs.c:129-139).
pub fn f_1(t: f64) -> f64 {
    let limit = 24.0 / 116.0;
    if t <= limit {
        (108.0 / 841.0) * (t - (16.0 / 116.0))
    } else {
        t * t * t
    }
}

/// `cmsXYZ2Lab` (cmspcs.c:143-157). `wp == None` defaults to D50.
pub fn xyz_to_lab(wp: Option<CIEXYZ>, xyz: CIEXYZ) -> CIELab {
    let wp = wp.unwrap_or(D50);
    let fx = f(xyz.x / wp.x);
    let fy = f(xyz.y / wp.y);
    let fz = f(xyz.z / wp.z);
    CIELab {
        l: 116.0 * fy - 16.0,
        a: 500.0 * (fx - fy),
        b: 200.0 * (fy - fz),
    }
}

/// `cmsLab2XYZ` (cmspcs.c:161-176). `wp == None` defaults to D50.
pub fn lab_to_xyz(wp: Option<CIEXYZ>, lab: CIELab) -> CIEXYZ {
    let wp = wp.unwrap_or(D50);
    let y = (lab.l + 16.0) / 116.0;
    let x = y + 0.002 * lab.a;
    let z = y - 0.005 * lab.b;
    CIEXYZ {
        x: f_1(x) * wp.x,
        y: f_1(y) * wp.y,
        z: f_1(z) * wp.z,
    }
}

/// `cmsXYZ2xyY` (cmspcs.c:91-100).
pub fn xyz_to_xyy(xyz: CIEXYZ) -> CIExyY {
    let isum = 1.0 / (xyz.x + xyz.y + xyz.z);
    CIExyY {
        x: xyz.x * isum,
        y: xyz.y * isum,
        yy: xyz.y,
    }
}

/// `cmsxyY2XYZ` (cmspcs.c:102-107).
pub fn xyy_to_xyz(xyy: CIExyY) -> CIEXYZ {
    CIEXYZ {
        x: (xyy.x / xyy.y) * xyy.yy,
        y: xyy.yy,
        z: ((1.0 - xyy.x - xyy.y) / xyy.y) * xyy.yy,
    }
}

/// atan2 in degrees, returning 0 for (0,0), wrapped to [0,360) (cmspcs.c:320-339).
fn atan2deg(a: f64, b: f64) -> f64 {
    let mut h = if a == 0.0 && b == 0.0 {
        0.0
    } else {
        a.atan2(b)
    };
    h *= 180.0 / std::f64::consts::PI;
    while h > 360.0 {
        h -= 360.0;
    }
    while h < 0.0 {
        h += 360.0;
    }
    h
}

/// `cmsDeltaE` (cmspcs.c): Euclidean ΔE76 between two Lab colors. Transcribed
/// verbatim as `pow(dL² + da² + db², 0.5)` (NOT `sqrt`): lcms2 uses `pow(.,0.5)`
/// over the absolute-valued component differences, and the `pow`/`sqrt` results
/// can differ in the last bit, so the gamut sampler's verdict depends on keeping
/// the `pow` form.
pub fn delta_e(lab1: CIELab, lab2: CIELab) -> f64 {
    let dl = (lab1.l - lab2.l).abs();
    let da = (lab1.a - lab2.a).abs();
    let db = (lab1.b - lab2.b).abs();
    (dl * dl + da * da + db * db).powf(0.5)
}

/// `cmsLab2LCh` (cmspcs.c:349-354).
pub fn lab_to_lch(lab: CIELab) -> CIELCh {
    CIELCh {
        l: lab.l,
        c: (lab.a * lab.a + lab.b * lab.b).powf(0.5),
        h: atan2deg(lab.b, lab.a),
    }
}

/// `cmsLCh2Lab` (cmspcs.c:358-365).
pub fn lch_to_lab(lch: CIELCh) -> CIELab {
    let h = (lch.h * std::f64::consts::PI) / 180.0;
    CIELab {
        l: lch.l,
        a: lch.c * h.cos(),
        b: lch.c * h.sin(),
    }
}

// ---- Lab v2 encoding (cmspcs.c:178-265) ------------------------------------

fn l2float2(v: u16) -> f64 {
    v as f64 / 652.800
}
fn ab2float2(v: u16) -> f64 {
    (v as f64 / 256.0) - 128.0
}
fn l2fix2(l: f64) -> u16 {
    Lcms2Floor::quick_saturate_word(l * 652.8)
}
fn ab2fix2(ab: f64) -> u16 {
    Lcms2Floor::quick_saturate_word((ab + 128.0) * 256.0)
}
fn clamp_l_v2(l: f64) -> f64 {
    let l_max = (0xFFFF as f64 * 100.0) / 0xFF00 as f64;
    l.clamp(0.0, l_max)
}
fn clamp_ab_v2(ab: f64) -> f64 {
    ab.clamp(MIN_ENCODEABLE_AB2, MAX_ENCODEABLE_AB2)
}

/// `cmsLabEncoded2FloatV2` (cmspcs.c:218-223).
pub fn lab_v2_encoded_to_float(w: &[u16; 3]) -> CIELab {
    CIELab {
        l: l2float2(w[0]),
        a: ab2float2(w[1]),
        b: ab2float2(w[2]),
    }
}

/// `cmsFloat2LabEncodedV2` (cmspcs.c:254-265).
pub fn float_to_lab_v2_encoded(lab: CIELab) -> [u16; 3] {
    let l = clamp_l_v2(lab.l);
    let a = clamp_ab_v2(lab.a);
    let b = clamp_ab_v2(lab.b);
    [l2fix2(l), ab2fix2(a), ab2fix2(b)]
}

// ---- Lab v4 encoding (cmspcs.c:204-309) ------------------------------------

fn l2float4(v: u16) -> f64 {
    v as f64 / 655.35
}
fn ab2float4(v: u16) -> f64 {
    (v as f64 / 257.0) - 128.0
}
fn l2fix4(l: f64) -> u16 {
    Lcms2Floor::quick_saturate_word(l * 655.35)
}
fn ab2fix4(ab: f64) -> u16 {
    Lcms2Floor::quick_saturate_word((ab + 128.0) * 257.0)
}
fn clamp_l_v4(l: f64) -> f64 {
    l.clamp(0.0, 100.0)
}
fn clamp_ab_v4(ab: f64) -> f64 {
    ab.clamp(MIN_ENCODEABLE_AB4, MAX_ENCODEABLE_AB4)
}

/// `cmsLabEncoded2Float` (v4, cmspcs.c:226-231).
pub fn lab_v4_encoded_to_float(w: &[u16; 3]) -> CIELab {
    CIELab {
        l: l2float4(w[0]),
        a: ab2float4(w[1]),
        b: ab2float4(w[2]),
    }
}

/// `cmsFloat2LabEncoded` (v4, cmspcs.c:298-309).
pub fn float_to_lab_v4_encoded(lab: CIELab) -> [u16; 3] {
    let l = clamp_l_v4(lab.l);
    let a = clamp_ab_v4(lab.a);
    let b = clamp_ab_v4(lab.b);
    [l2fix4(l), ab2fix4(a), ab2fix4(b)]
}

// ---- XYZ encoding (1.15 fixed point, cmspcs.c:367-434) ---------------------

fn xyz2fix(d: f64) -> u16 {
    Lcms2Floor::quick_saturate_word(d * 32768.0)
}
/// `XYZ2float` (cmspcs.c:416-426): widen 1.15 -> 15.16 via `v << 1`, then /65536.
fn xyz2float(v: u16) -> f64 {
    // lcms2: fix32 = v << 1 (a cmsS15Fixed16Number), then _cms15Fixed16toDouble.
    let fix32 = (v as i32) << 1;
    fix32 as f64 / 65536.0
}

/// `cmsXYZEncoded2Float` (cmspcs.c:429-434).
pub fn xyz_encoded_to_float(w: &[u16; 3]) -> CIEXYZ {
    CIEXYZ {
        x: xyz2float(w[0]),
        y: xyz2float(w[1]),
        z: xyz2float(w[2]),
    }
}

/// `cmsFloat2XYZEncoded` (cmspcs.c:374-412). Clamps Y<=0 to all-zero, then each
/// channel into `[0, MAX_ENCODEABLE_XYZ]` before the 1.15 fixed conversion.
// The explicit `> MAX` / `< 0` branch chain is transcribed verbatim from lcms2
// and intentionally NOT collapsed into `f64::clamp`: clamp panics when min/max
// are mis-ordered and has its own NaN contract, whereas this branch form (each
// `if` independently false for NaN) reproduces the C output bit-for-bit on every
// input, including NaN. Keeping the C shape is what guarantees parity.
#[allow(clippy::manual_clamp)]
pub fn float_to_xyz_encoded(xyz: CIEXYZ) -> [u16; 3] {
    let mut x = xyz.x;
    let mut y = xyz.y;
    let mut z = xyz.z;

    if y <= 0.0 {
        x = 0.0;
        y = 0.0;
        z = 0.0;
    }

    if x > MAX_ENCODEABLE_XYZ {
        x = MAX_ENCODEABLE_XYZ;
    }
    if x < 0.0 {
        x = 0.0;
    }
    if y > MAX_ENCODEABLE_XYZ {
        y = MAX_ENCODEABLE_XYZ;
    }
    if y < 0.0 {
        y = 0.0;
    }
    if z > MAX_ENCODEABLE_XYZ {
        z = MAX_ENCODEABLE_XYZ;
    }
    if z < 0.0 {
        z = 0.0;
    }

    [xyz2fix(x), xyz2fix(y), xyz2fix(z)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tintbox_oracle::{assert_f64_bits_eq, Rng};

    // D65 white point (one of the standard alternates) for sweeping the WP arg.
    const D65: CIEXYZ = CIEXYZ {
        x: 0.9504,
        y: 1.0,
        z: 1.0888,
    };

    fn wps() -> [Option<CIEXYZ>; 3] {
        [None, Some(D50), Some(D65)]
    }

    fn wp_arr(wp: Option<CIEXYZ>) -> Option<[f64; 3]> {
        wp.map(|w| [w.x, w.y, w.z])
    }

    // Map a unit f64 into [lo, hi].
    fn scale(u: f64, lo: f64, hi: f64) -> f64 {
        lo + u * (hi - lo)
    }

    #[test]
    fn f_matches_oracle_via_xyz_to_lab() {
        // f/f_1 are static in C; exercise them through xyz_to_lab/lab_to_xyz,
        // including the cube-root threshold exactly.
        let mut rng = Rng::new(101);
        for i in 0..200_000u32 {
            let xyz = CIEXYZ {
                x: scale(rng.next_f64_unit(), 0.0, 2.0),
                y: scale(rng.next_f64_unit(), 0.0, 2.0),
                z: scale(rng.next_f64_unit(), 0.0, 2.0),
            };
            for wp in wps() {
                let r = xyz_to_lab(wp, xyz);
                let c = tintbox_oracle::xyz2lab(wp_arr(wp), &[xyz.x, xyz.y, xyz.z]);
                assert_f64_bits_eq(r.l, c[0], (i, "L", wp, xyz));
                assert_f64_bits_eq(r.a, c[1], (i, "a", wp, xyz));
                assert_f64_bits_eq(r.b, c[2], (i, "b", wp, xyz));
            }
        }
    }

    #[test]
    fn xyz_to_lab_threshold_edges() {
        // t exactly at / around the f() break point (24/116)^3 for X/Xn etc.
        let limit = (24.0 / 116.0f64) * (24.0 / 116.0) * (24.0 / 116.0);
        let xn = D50.x;
        let cases = [
            limit,
            limit * xn,
            (limit - f64::EPSILON) * xn,
            (limit + f64::EPSILON) * xn,
            0.0,
            xn,
        ];
        for &x in &cases {
            let xyz = CIEXYZ { x, y: x, z: x };
            for wp in wps() {
                let r = xyz_to_lab(wp, xyz);
                let c = tintbox_oracle::xyz2lab(wp_arr(wp), &[xyz.x, xyz.y, xyz.z]);
                assert_f64_bits_eq(r.l, c[0], ("edge L", x, wp));
                assert_f64_bits_eq(r.a, c[1], ("edge a", x, wp));
                assert_f64_bits_eq(r.b, c[2], ("edge b", x, wp));
            }
        }
    }

    #[test]
    fn lab_to_xyz_matches_oracle() {
        let mut rng = Rng::new(202);
        for i in 0..200_000u32 {
            let lab = CIELab {
                l: scale(rng.next_f64_unit(), 0.0, 100.0),
                a: scale(rng.next_f64_unit(), -128.0, 127.0),
                b: scale(rng.next_f64_unit(), -128.0, 127.0),
            };
            for wp in wps() {
                let r = lab_to_xyz(wp, lab);
                let c = tintbox_oracle::lab2xyz(wp_arr(wp), &[lab.l, lab.a, lab.b]);
                assert_f64_bits_eq(r.x, c[0], (i, "X", wp, lab));
                assert_f64_bits_eq(r.y, c[1], (i, "Y", wp, lab));
                assert_f64_bits_eq(r.z, c[2], (i, "Z", wp, lab));
            }
        }
        // L=0 edge.
        for wp in wps() {
            let lab = CIELab {
                l: 0.0,
                a: 0.0,
                b: 0.0,
            };
            let r = lab_to_xyz(wp, lab);
            let c = tintbox_oracle::lab2xyz(wp_arr(wp), &[lab.l, lab.a, lab.b]);
            assert_f64_bits_eq(r.x, c[0], ("L0 X", wp));
            assert_f64_bits_eq(r.y, c[1], ("L0 Y", wp));
            assert_f64_bits_eq(r.z, c[2], ("L0 Z", wp));
        }
    }

    #[test]
    fn xyz_to_xyy_matches_oracle() {
        let mut rng = Rng::new(303);
        for i in 0..200_000u32 {
            let xyz = CIEXYZ {
                x: scale(rng.next_f64_unit(), 0.0, 2.0),
                y: scale(rng.next_f64_unit(), 0.0, 2.0),
                z: scale(rng.next_f64_unit(), 0.0, 2.0),
            };
            let r = xyz_to_xyy(xyz);
            let c = tintbox_oracle::xyz2xyy(&[xyz.x, xyz.y, xyz.z]);
            assert_f64_bits_eq(r.x, c[0], (i, "x", xyz));
            assert_f64_bits_eq(r.y, c[1], (i, "y", xyz));
            assert_f64_bits_eq(r.yy, c[2], (i, "Y", xyz));
        }
        // All-zero -> ISum = 1/0 = +inf; x=y=0*inf=NaN. Match lcms2's NaN exactly.
        let z = CIEXYZ {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let r = xyz_to_xyy(z);
        let c = tintbox_oracle::xyz2xyy(&[0.0, 0.0, 0.0]);
        assert_f64_bits_eq(r.x, c[0], "zero x");
        assert_f64_bits_eq(r.y, c[1], "zero y");
        assert_f64_bits_eq(r.yy, c[2], "zero Y");
    }

    #[test]
    fn xyy_to_xyz_matches_oracle() {
        let mut rng = Rng::new(404);
        for i in 0..200_000u32 {
            let xyy = CIExyY {
                x: scale(rng.next_f64_unit(), 0.0, 1.0),
                y: scale(rng.next_f64_unit(), 0.0, 1.0),
                yy: scale(rng.next_f64_unit(), 0.0, 2.0),
            };
            let r = xyy_to_xyz(xyy);
            let c = tintbox_oracle::xyy2xyz(&[xyy.x, xyy.y, xyy.yy]);
            assert_f64_bits_eq(r.x, c[0], (i, "X", xyy));
            assert_f64_bits_eq(r.y, c[1], (i, "Y", xyy));
            assert_f64_bits_eq(r.z, c[2], (i, "Z", xyy));
        }
        // y near/at zero -> division by zero; match lcms2 (inf/NaN) exactly.
        for &yv in &[0.0f64, 1e-300] {
            let xyy = CIExyY {
                x: 0.3,
                y: yv,
                yy: 1.0,
            };
            let r = xyy_to_xyz(xyy);
            let c = tintbox_oracle::xyy2xyz(&[xyy.x, xyy.y, xyy.yy]);
            assert_f64_bits_eq(r.x, c[0], ("y0 X", yv));
            assert_f64_bits_eq(r.y, c[1], ("y0 Y", yv));
            assert_f64_bits_eq(r.z, c[2], ("y0 Z", yv));
        }
    }

    #[test]
    fn lab_to_lch_matches_oracle() {
        let mut rng = Rng::new(505);
        for i in 0..200_000u32 {
            let lab = CIELab {
                l: scale(rng.next_f64_unit(), 0.0, 100.0),
                a: scale(rng.next_f64_unit(), -128.0, 127.0),
                b: scale(rng.next_f64_unit(), -128.0, 127.0),
            };
            let r = lab_to_lch(lab);
            let c = tintbox_oracle::lab2lch(&[lab.l, lab.a, lab.b]);
            assert_f64_bits_eq(r.l, c[0], (i, "L", lab));
            assert_f64_bits_eq(r.c, c[1], (i, "C", lab));
            assert_f64_bits_eq(r.h, c[2], (i, "h", lab));
        }
        // a==b==0 -> atan2deg returns exactly 0.
        let lab = CIELab {
            l: 50.0,
            a: 0.0,
            b: 0.0,
        };
        let r = lab_to_lch(lab);
        let c = tintbox_oracle::lab2lch(&[lab.l, lab.a, lab.b]);
        assert_f64_bits_eq(r.h, c[2], "zero h");
    }

    #[test]
    fn lch_to_lab_matches_oracle() {
        let mut rng = Rng::new(606);
        for i in 0..200_000u32 {
            let lch = CIELCh {
                l: scale(rng.next_f64_unit(), 0.0, 100.0),
                c: scale(rng.next_f64_unit(), 0.0, 181.0),
                h: scale(rng.next_f64_unit(), 0.0, 360.0),
            };
            let r = lch_to_lab(lch);
            let c = tintbox_oracle::lch2lab(&[lch.l, lch.c, lch.h]);
            assert_f64_bits_eq(r.l, c[0], (i, "L", lch));
            assert_f64_bits_eq(r.a, c[1], (i, "a", lch));
            assert_f64_bits_eq(r.b, c[2], (i, "b", lch));
        }
    }

    #[test]
    fn lab_v2_encode_decode_matches_oracle() {
        let mut rng = Rng::new(707);
        // Decode: sweep representative u16 triples (including extremes).
        for i in 0..300_000u32 {
            let w = [
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
            ];
            let r = lab_v2_encoded_to_float(&w);
            let c = tintbox_oracle::lab_enc2float_v2(&w);
            assert_f64_bits_eq(r.l, c[0], (i, "L", w));
            assert_f64_bits_eq(r.a, c[1], (i, "a", w));
            assert_f64_bits_eq(r.b, c[2], (i, "b", w));
        }
        for &w in &[
            [0u16, 0, 0],
            [0xFFFF, 0xFFFF, 0xFFFF],
            [0xFF00, 0x8000, 0x8000],
        ] {
            let r = lab_v2_encoded_to_float(&w);
            let c = tintbox_oracle::lab_enc2float_v2(&w);
            assert_f64_bits_eq(r.l, c[0], ("edge L", w));
            assert_f64_bits_eq(r.a, c[1], ("edge a", w));
            assert_f64_bits_eq(r.b, c[2], ("edge b", w));
        }
        // Encode: sweep Lab including out-of-range to exercise clamps.
        for i in 0..300_000u32 {
            let lab = CIELab {
                l: scale(rng.next_f64_unit(), -10.0, 110.0),
                a: scale(rng.next_f64_unit(), -140.0, 140.0),
                b: scale(rng.next_f64_unit(), -140.0, 140.0),
            };
            let r = float_to_lab_v2_encoded(lab);
            let c = tintbox_oracle::float2lab_enc_v2(&[lab.l, lab.a, lab.b]);
            assert_eq!(r, c, "v2 enc mismatch {i} {lab:?}");
        }
    }

    #[test]
    fn lab_v4_encode_decode_matches_oracle() {
        let mut rng = Rng::new(808);
        for i in 0..300_000u32 {
            let w = [
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
            ];
            let r = lab_v4_encoded_to_float(&w);
            let c = tintbox_oracle::lab_enc2float_v4(&w);
            assert_f64_bits_eq(r.l, c[0], (i, "L", w));
            assert_f64_bits_eq(r.a, c[1], (i, "a", w));
            assert_f64_bits_eq(r.b, c[2], (i, "b", w));
        }
        for &w in &[
            [0u16, 0, 0],
            [0xFFFF, 0xFFFF, 0xFFFF],
            [0xFFFF, 0x8080, 0x8080],
        ] {
            let r = lab_v4_encoded_to_float(&w);
            let c = tintbox_oracle::lab_enc2float_v4(&w);
            assert_f64_bits_eq(r.l, c[0], ("edge L", w));
            assert_f64_bits_eq(r.a, c[1], ("edge a", w));
            assert_f64_bits_eq(r.b, c[2], ("edge b", w));
        }
        for i in 0..300_000u32 {
            let lab = CIELab {
                l: scale(rng.next_f64_unit(), -10.0, 110.0),
                a: scale(rng.next_f64_unit(), -140.0, 140.0),
                b: scale(rng.next_f64_unit(), -140.0, 140.0),
            };
            let r = float_to_lab_v4_encoded(lab);
            let c = tintbox_oracle::float2lab_enc_v4(&[lab.l, lab.a, lab.b]);
            assert_eq!(r, c, "v4 enc mismatch {i} {lab:?}");
        }
    }

    #[test]
    fn xyz_encode_decode_matches_oracle() {
        let mut rng = Rng::new(909);
        for i in 0..300_000u32 {
            let w = [
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
                (rng.next_u64() & 0xFFFF) as u16,
            ];
            let r = xyz_encoded_to_float(&w);
            let c = tintbox_oracle::xyz_enc2float(&w);
            assert_f64_bits_eq(r.x, c[0], (i, "X", w));
            assert_f64_bits_eq(r.y, c[1], (i, "Y", w));
            assert_f64_bits_eq(r.z, c[2], (i, "Z", w));
        }
        // Encode: sweep XYZ including out-of-range and Y<=0 special-case.
        for i in 0..300_000u32 {
            let xyz = CIEXYZ {
                x: scale(rng.next_f64_unit(), -0.5, 2.5),
                y: scale(rng.next_f64_unit(), -0.5, 2.5),
                z: scale(rng.next_f64_unit(), -0.5, 2.5),
            };
            let r = float_to_xyz_encoded(xyz);
            let c = tintbox_oracle::float2xyz_enc(&[xyz.x, xyz.y, xyz.z]);
            assert_eq!(r, c, "xyz enc mismatch {i} {xyz:?}");
        }
        // Y<=0 zeroes everything; negative/overflow clamps.
        let edges = [
            CIEXYZ {
                x: 1.0,
                y: 0.0,
                z: 1.0,
            },
            CIEXYZ {
                x: 1.0,
                y: -0.1,
                z: 1.0,
            },
            CIEXYZ {
                x: 3.0,
                y: 1.0,
                z: -1.0,
            },
            CIEXYZ {
                x: MAX_ENCODEABLE_XYZ,
                y: 1.0,
                z: MAX_ENCODEABLE_XYZ,
            },
        ];
        for xyz in edges {
            let r = float_to_xyz_encoded(xyz);
            let c = tintbox_oracle::float2xyz_enc(&[xyz.x, xyz.y, xyz.z]);
            assert_eq!(r, c, "xyz enc edge {xyz:?}");
        }
    }
}
