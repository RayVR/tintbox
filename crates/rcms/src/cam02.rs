//! CIECAM02 color appearance model, transcribed verbatim from lcms2 `src/cmscam02.c`
//! and bit-identical to it.
//!
//! BIT-IDENTITY PRECONDITIONS: no `f64::mul_add`; oracle built `-ffp-contract=off`;
//! host FLT_EVAL_METHOD==0. All arithmetic is `f64` in the exact operand order of
//! the C. The transcendentals (`pow`, `exp`, `cos`, `sin`, `atan`, `fabs`) map to
//! the same system libm the oracle links, matching rcms's existing `pcs`/`adapt`
//! conventions (`.powf`, `.exp`, `.cos`, `.sin`, `.atan`, `.abs`).
//!
//! The C carries `cmsContext` only to drive `_cmsMallocZero`/`_cmsFree`; rcms owns
//! the model by value, so there is no allocator and no context.

// Bit-identity overrides clippy's stylistic lints here:
// - `neg_multiply`: lcms2 writes `-1.0 * FL * RGBp` / `-1.0 * 400.0 * temp`; the
//   `-1.0 *` factor is part of the verbatim operand order and must not be folded
//   into a unary negation.
// - `approx_constant`: lcms2 uses the literal `3.141592654` (a truncated pi), NOT
//   `std::f64::consts::PI`; substituting PI would change the bits.
#![allow(clippy::neg_multiply, clippy::approx_constant)]

use crate::color::{JCh, CIEXYZ};

/// Surround condition selector (lcms2 `AVG_SURROUND` … `CUTSHEET_SURROUND`).
pub const AVG_SURROUND: u32 = 1;
pub const DIM_SURROUND: u32 = 2;
pub const DARK_SURROUND: u32 = 3;
pub const CUTSHEET_SURROUND: u32 = 4;

/// Sentinel asking [`Cam02::new`] to compute the degree of adaptation `D`
/// (lcms2 `D_CALCULATE == -1`).
pub const D_CALCULATE: f64 = -1.0;

/// CAM02 viewing conditions (lcms2 `cmsViewingConditions`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewingConditions {
    pub white_point: CIEXYZ,
    pub yb: f64,
    pub la: f64,
    pub surround: u32,
    pub d_value: f64,
}

/// Per-color working set (lcms2 `CAM02COLOR`). Only the fields exercised by the
/// forward/reverse pipelines are tracked; the rest of the C struct's correlates
/// (`Q`, `s`, `M`, `H`, …) are computed locally where needed.
#[derive(Clone, Copy, Debug, Default)]
struct Cam02Color {
    xyz: [f64; 3],
    rgb: [f64; 3],
    rgbc: [f64; 3],
    rgbp: [f64; 3],
    rgbpa: [f64; 3],
    a: f64,
    b: f64,
    h: f64,
    big_a: f64,
    j: f64,
    c: f64,
}

/// Initialized CIECAM02 model (lcms2 `cmsCIECAM02`).
#[derive(Clone, Copy, Debug)]
pub struct Cam02 {
    adopted_white: Cam02Color,
    // `la`, `yb`, `f`, and `surround` are stored in the C `cmsCIECAM02` struct
    // but, after init derives the constants below, are never read back; kept for
    // 1:1 structural fidelity with the C model.
    #[allow(dead_code)]
    la: f64,
    #[allow(dead_code)]
    yb: f64,
    #[allow(dead_code)]
    f: f64,
    c: f64,
    nc: f64,
    #[allow(dead_code)]
    surround: u32,
    n: f64,
    nbb: f64,
    ncb: f64,
    z: f64,
    fl: f64,
    d: f64,
}

// ---------- field helpers (cmscam02.c:60-99) ------------------------------

fn compute_n(adopted_white_y: f64, yb: f64) -> f64 {
    yb / adopted_white_y
}

fn compute_z(n: f64) -> f64 {
    1.48 + n.powf(0.5)
}

fn compute_nbb(n: f64) -> f64 {
    0.725 * (1.0 / n).powf(0.2)
}

fn compute_fl(la: f64) -> f64 {
    let k = 1.0 / ((5.0 * la) + 1.0);
    0.2 * k.powf(4.0) * (5.0 * la)
        + 0.1 * ((1.0 - k.powf(4.0)).powf(2.0)) * ((5.0 * la).powf(1.0 / 3.0))
}

fn compute_d(f: f64, la: f64) -> f64 {
    let temp = 1.0 - ((1.0 / 3.6) * ((-la - 42.0) / 92.0).exp());
    f * temp
}

// ---------- forward pipeline stages (cmscam02.c:101-238) ------------------

fn xyz_to_cat02(mut clr: Cam02Color) -> Cam02Color {
    clr.rgb[0] = (clr.xyz[0] * 0.7328) + (clr.xyz[1] * 0.4296) + (clr.xyz[2] * -0.1624);
    clr.rgb[1] = (clr.xyz[0] * -0.7036) + (clr.xyz[1] * 1.6975) + (clr.xyz[2] * 0.0061);
    clr.rgb[2] = (clr.xyz[0] * 0.0030) + (clr.xyz[1] * 0.0136) + (clr.xyz[2] * 0.9834);
    clr
}

fn chromatic_adaptation(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    for i in 0..3 {
        clr.rgbc[i] = ((pmod.adopted_white.xyz[1] * (pmod.d / pmod.adopted_white.rgb[i]))
            + (1.0 - pmod.d))
            * clr.rgb[i];
    }
    clr
}

fn cat02_to_hpe(mut clr: Cam02Color) -> Cam02Color {
    let mut m = [0.0f64; 9];
    m[0] = (0.38971 * 1.096124) + (0.68898 * 0.454369) + (-0.07868 * -0.009628);
    m[1] = (0.38971 * -0.278869) + (0.68898 * 0.473533) + (-0.07868 * -0.005698);
    m[2] = (0.38971 * 0.182745) + (0.68898 * 0.072098) + (-0.07868 * 1.015326);
    m[3] = (-0.22981 * 1.096124) + (1.18340 * 0.454369) + (0.04641 * -0.009628);
    m[4] = (-0.22981 * -0.278869) + (1.18340 * 0.473533) + (0.04641 * -0.005698);
    m[5] = (-0.22981 * 0.182745) + (1.18340 * 0.072098) + (0.04641 * 1.015326);
    m[6] = -0.009628;
    m[7] = -0.005698;
    m[8] = 1.015326;

    clr.rgbp[0] = (clr.rgbc[0] * m[0]) + (clr.rgbc[1] * m[1]) + (clr.rgbc[2] * m[2]);
    clr.rgbp[1] = (clr.rgbc[0] * m[3]) + (clr.rgbc[1] * m[4]) + (clr.rgbc[2] * m[5]);
    clr.rgbp[2] = (clr.rgbc[0] * m[6]) + (clr.rgbc[1] * m[7]) + (clr.rgbc[2] * m[8]);
    clr
}

fn nonlinear_compression(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    for i in 0..3 {
        if clr.rgbp[i] < 0.0 {
            let temp = (-1.0 * pmod.fl * clr.rgbp[i] / 100.0).powf(0.42);
            clr.rgbpa[i] = (-1.0 * 400.0 * temp) / (temp + 27.13) + 0.1;
        } else {
            let temp = (pmod.fl * clr.rgbp[i] / 100.0).powf(0.42);
            clr.rgbpa[i] = (400.0 * temp) / (temp + 27.13) + 0.1;
        }
    }

    clr.big_a = (((2.0 * clr.rgbpa[0]) + clr.rgbpa[1] + (clr.rgbpa[2] / 20.0)) - 0.305) * pmod.nbb;

    clr
}

fn compute_correlates(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    let a = clr.rgbpa[0] - (12.0 * clr.rgbpa[1] / 11.0) + (clr.rgbpa[2] / 11.0);
    let b = (clr.rgbpa[0] + clr.rgbpa[1] - (2.0 * clr.rgbpa[2])) / 9.0;

    let r2d = 180.0 / 3.141592654;
    if a == 0.0 {
        if b == 0.0 {
            clr.h = 0.0;
        } else if b > 0.0 {
            clr.h = 90.0;
        } else {
            clr.h = 270.0;
        }
    } else if a > 0.0 {
        let temp = b / a;
        if b > 0.0 {
            clr.h = r2d * temp.atan();
        } else if b == 0.0 {
            clr.h = 0.0;
        } else {
            clr.h = (r2d * temp.atan()) + 360.0;
        }
    } else {
        let temp = b / a;
        clr.h = (r2d * temp.atan()) + 180.0;
    }

    let d2r = 3.141592654 / 180.0;
    let e = ((12500.0 / 13.0) * pmod.nc * pmod.ncb) * ((clr.h * d2r + 2.0).cos() + 3.8);

    // clr.H (Hue composition) is computed in the C but unused by Forward; the
    // branch is omitted because none of its inputs feed J/C/h.

    clr.j = 100.0 * (clr.big_a / pmod.adopted_white.big_a).powf(pmod.c * pmod.z);

    let t = (e * ((a * a) + (b * b)).powf(0.5))
        / (clr.rgbpa[0] + clr.rgbpa[1] + ((21.0 / 20.0) * clr.rgbpa[2]));

    clr.c = t.powf(0.9) * (clr.j / 100.0).powf(0.5) * (1.64 - 0.29f64.powf(pmod.n)).powf(0.73);

    clr
}

// ---------- reverse pipeline stages (cmscam02.c:242-360) ------------------

fn inverse_correlates(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    let d2r = 3.141592654 / 180.0;

    let t = (clr.c / ((clr.j / 100.0).powf(0.5) * (1.64 - 0.29f64.powf(pmod.n)).powf(0.73)))
        .powf(1.0 / 0.9);
    let e = ((12500.0 / 13.0) * pmod.nc * pmod.ncb) * ((clr.h * d2r + 2.0).cos() + 3.8);

    clr.big_a = pmod.adopted_white.big_a * (clr.j / 100.0).powf(1.0 / (pmod.c * pmod.z));

    let p2 = (clr.big_a / pmod.nbb) + 0.305;

    if t <= 0.0 {
        // special case from spec notes, avoid divide by zero
        clr.a = 0.0;
        clr.b = 0.0;
    } else {
        let hr = clr.h * d2r;
        let p1 = e / t;
        let p3 = 21.0 / 20.0;

        if hr.sin().abs() >= hr.cos().abs() {
            let p4 = p1 / hr.sin();
            clr.b = (p2 * (2.0 + p3) * (460.0 / 1403.0))
                / (p4 + (2.0 + p3) * (220.0 / 1403.0) * (hr.cos() / hr.sin()) - (27.0 / 1403.0)
                    + p3 * (6300.0 / 1403.0));
            clr.a = clr.b * (hr.cos() / hr.sin());
        } else {
            let p5 = p1 / hr.cos();
            clr.a = (p2 * (2.0 + p3) * (460.0 / 1403.0))
                / (p5 + (2.0 + p3) * (220.0 / 1403.0)
                    - ((27.0 / 1403.0) - p3 * (6300.0 / 1403.0)) * (hr.sin() / hr.cos()));
            clr.b = clr.a * (hr.sin() / hr.cos());
        }
    }

    clr.rgbpa[0] =
        ((460.0 / 1403.0) * p2) + ((451.0 / 1403.0) * clr.a) + ((288.0 / 1403.0) * clr.b);
    clr.rgbpa[1] =
        ((460.0 / 1403.0) * p2) - ((891.0 / 1403.0) * clr.a) - ((261.0 / 1403.0) * clr.b);
    clr.rgbpa[2] =
        ((460.0 / 1403.0) * p2) - ((220.0 / 1403.0) * clr.a) - ((6300.0 / 1403.0) * clr.b);

    clr
}

fn inverse_nonlinearity(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    for i in 0..3 {
        let c1 = if (clr.rgbpa[i] - 0.1) < 0.0 {
            -1.0
        } else {
            1.0
        };
        clr.rgbp[i] = c1
            * (100.0 / pmod.fl)
            * ((27.13 * (clr.rgbpa[i] - 0.1).abs()) / (400.0 - (clr.rgbpa[i] - 0.1).abs()))
                .powf(1.0 / 0.42);
    }
    clr
}

fn hpe_to_cat02(mut clr: Cam02Color) -> Cam02Color {
    let mut m = [0.0f64; 9];
    m[0] = (0.7328 * 1.910197) + (0.4296 * 0.370950);
    m[1] = (0.7328 * -1.112124) + (0.4296 * 0.629054);
    m[2] = (0.7328 * 0.201908) + (0.4296 * 0.000008) - 0.1624;
    m[3] = (-0.7036 * 1.910197) + (1.6975 * 0.370950);
    m[4] = (-0.7036 * -1.112124) + (1.6975 * 0.629054);
    m[5] = (-0.7036 * 0.201908) + (1.6975 * 0.000008) + 0.0061;
    m[6] = (0.0030 * 1.910197) + (0.0136 * 0.370950);
    m[7] = (0.0030 * -1.112124) + (0.0136 * 0.629054);
    m[8] = (0.0030 * 0.201908) + (0.0136 * 0.000008) + 0.9834;

    clr.rgbc[0] = (clr.rgbp[0] * m[0]) + (clr.rgbp[1] * m[1]) + (clr.rgbp[2] * m[2]);
    clr.rgbc[1] = (clr.rgbp[0] * m[3]) + (clr.rgbp[1] * m[4]) + (clr.rgbp[2] * m[5]);
    clr.rgbc[2] = (clr.rgbp[0] * m[6]) + (clr.rgbp[1] * m[7]) + (clr.rgbp[2] * m[8]);
    clr
}

fn inverse_chromatic_adaptation(mut clr: Cam02Color, pmod: &Cam02) -> Cam02Color {
    for i in 0..3 {
        clr.rgb[i] = clr.rgbc[i]
            / ((pmod.adopted_white.xyz[1] * pmod.d / pmod.adopted_white.rgb[i]) + 1.0 - pmod.d);
    }
    clr
}

fn cat02_to_xyz(mut clr: Cam02Color) -> Cam02Color {
    clr.xyz[0] = (clr.rgb[0] * 1.096124) + (clr.rgb[1] * -0.278869) + (clr.rgb[2] * 0.182745);
    clr.xyz[1] = (clr.rgb[0] * 0.454369) + (clr.rgb[1] * 0.473533) + (clr.rgb[2] * 0.072098);
    clr.xyz[2] = (clr.rgb[0] * -0.009628) + (clr.rgb[1] * -0.005698) + (clr.rgb[2] * 1.015326);
    clr
}

impl Cam02 {
    /// Initialize a CIECAM02 model from viewing conditions
    /// (lcms2 `cmsCIECAM02Init`, cmscam02.c:363-430).
    pub fn new(vc: &ViewingConditions) -> Cam02 {
        let adopted_white = Cam02Color {
            xyz: [vc.white_point.x, vc.white_point.y, vc.white_point.z],
            ..Cam02Color::default()
        };

        let la = vc.la;
        let yb = vc.yb;
        let mut d = vc.d_value;
        let surround = vc.surround;

        let (f, c, nc) = match surround {
            CUTSHEET_SURROUND => (0.8, 0.41, 0.8),
            DARK_SURROUND => (0.8, 0.525, 0.8),
            DIM_SURROUND => (0.9, 0.59, 0.95),
            // Average surround
            _ => (1.0, 0.69, 1.0),
        };

        let n = compute_n(adopted_white.xyz[1], yb);
        let z = compute_z(n);
        let nbb = compute_nbb(n);
        let fl = compute_fl(la);

        if d == D_CALCULATE {
            d = compute_d(f, la);
        }

        let ncb = nbb;

        let mut pmod = Cam02 {
            adopted_white,
            la,
            yb,
            f,
            c,
            nc,
            surround,
            n,
            nbb,
            ncb,
            z,
            fl,
            d,
        };

        // The C reassigns `lpMod->adoptedWhite` after each stage, and the next
        // stage reads back the freshly-updated value (e.g. `ChromaticAdaptation`
        // divides by `adoptedWhite.RGB[i]`, which `XYZtoCAT02` just produced).
        // Keep `pmod.adopted_white` in lockstep so each stage sees the same input.
        pmod.adopted_white = xyz_to_cat02(pmod.adopted_white);
        pmod.adopted_white = chromatic_adaptation(pmod.adopted_white, &pmod);
        pmod.adopted_white = cat02_to_hpe(pmod.adopted_white);
        pmod.adopted_white = nonlinear_compression(pmod.adopted_white, &pmod);

        pmod
    }

    /// Forward transform XYZ → JCh (lcms2 `cmsCIECAM02Forward`, cmscam02.c:440-464).
    pub fn forward(&self, xyz: &CIEXYZ) -> JCh {
        let mut clr = Cam02Color {
            xyz: [xyz.x, xyz.y, xyz.z],
            ..Cam02Color::default()
        };

        clr = xyz_to_cat02(clr);
        clr = chromatic_adaptation(clr, self);
        clr = cat02_to_hpe(clr);
        clr = nonlinear_compression(clr, self);
        clr = compute_correlates(clr, self);

        JCh {
            j: clr.j,
            c: clr.c,
            h: clr.h,
        }
    }

    /// Reverse transform JCh → XYZ (lcms2 `cmsCIECAM02Reverse`, cmscam02.c:466-490).
    pub fn reverse(&self, jch: &JCh) -> CIEXYZ {
        let mut clr = Cam02Color {
            j: jch.j,
            c: jch.c,
            h: jch.h,
            ..Cam02Color::default()
        };

        clr = inverse_correlates(clr, self);
        clr = inverse_nonlinearity(clr, self);
        clr = hpe_to_cat02(clr);
        clr = inverse_chromatic_adaptation(clr, self);
        clr = cat02_to_xyz(clr);

        CIEXYZ {
            x: clr.xyz[0],
            y: clr.xyz[1],
            z: clr.xyz[2],
        }
    }
}
