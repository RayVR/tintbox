//! Gamut boundary descriptor (lcms2 `cmssm.c`), Jan Morovic's segment-maxima
//! method. A [`GamutBoundaryDescriptor`] quantizes specified Lab points into a
//! `SECTORS × SECTORS` spherical grid (radius = C\*, alpha = Hab, theta = L\*),
//! keeping the maximum radius per sector ([`GamutBoundaryDescriptor::add_point`]),
//! interpolates missing sectors ([`GamutBoundaryDescriptor::compute`]), and tests
//! membership ([`GamutBoundaryDescriptor::check_point`]).
//!
//! The math (angle quantization, the segment grid, the spiral neighbor search, and
//! the closest-line-to-line solve) is transcribed verbatim from `cmssm.c` so the
//! in/out verdict matches lcms2's `cmsGDBCheckPoint`.

use crate::color::CIELab;
use crate::math::matrix::Vec3;

/// `MATRIX_DET_TOLERANCE` (`cmsmtrx.c`): the near-parallel / near-singular gate
/// used by [`closest_line_to_line`]. lcms2 reuses the matrix determinant
/// tolerance here.
const MATRIX_DET_TOLERANCE: f64 = 0.0001;

/// `SECTORS` (`cmssm.c:39`): divisions in both alpha and theta.
const SECTORS: usize = 16;

/// `ToSpherical`/`ToCartesian`'s spherical coordinate (`cmssm.c:42-48`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Spherical {
    r: f64,
    alpha: f64,
    theta: f64,
}

/// `GDBPointType` (`cmssm.c:50-55`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointType {
    Empty,
    Specified,
    Modeled,
}

/// `cmsGDBPoint` (`cmssm.c:58-63`).
#[derive(Clone, Copy, Debug)]
struct GdbPoint {
    ty: PointType,
    p: Spherical,
}

impl Default for GdbPoint {
    fn default() -> Self {
        GdbPoint {
            ty: PointType::Empty,
            p: Spherical::default(),
        }
    }
}

/// `cmsLine`: parametric line `P = a + t·u` (`cmssm.c:76-81`).
struct Line {
    a: Vec3,
    u: Vec3,
}

/// `_cmsAtan2` (`cmssm.c:100-115`): degrees in `[0, 360)`, `0` for the undefined
/// `(0,0)` case.
fn atan2_deg(y: f64, x: f64) -> f64 {
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let mut a = (y.atan2(x) * 180.0) / std::f64::consts::PI;
    while a < 0.0 {
        a += 360.0;
    }
    a
}

/// `ToSpherical` (`cmssm.c:118-137`). Note lcms2's axis convention: `L = n[VX]`,
/// `a = n[VY]`, `b = n[VZ]`, `alpha = atan2(a, b)`, `theta = atan2(sqrt(a²+b²), L)`.
fn to_spherical(v: &Vec3) -> Spherical {
    let l = v.0[0];
    let a = v.0[1];
    let b = v.0[2];

    let r = (l * l + a * a + b * b).sqrt();
    if r == 0.0 {
        return Spherical {
            r,
            alpha: 0.0,
            theta: 0.0,
        };
    }
    Spherical {
        r,
        alpha: atan2_deg(a, b),
        theta: atan2_deg((a * a + b * b).sqrt(), l),
    }
}

/// `ToCartesian` (`cmssm.c:141-162`).
fn to_cartesian(sp: &Spherical) -> Vec3 {
    let sin_alpha = ((std::f64::consts::PI * sp.alpha) / 180.0).sin();
    let cos_alpha = ((std::f64::consts::PI * sp.alpha) / 180.0).cos();
    let sin_theta = ((std::f64::consts::PI * sp.theta) / 180.0).sin();
    let cos_theta = ((std::f64::consts::PI * sp.theta) / 180.0).cos();

    let a = sp.r * sin_theta * sin_alpha;
    let b = sp.r * sin_theta * cos_alpha;
    let l = sp.r * cos_theta;

    Vec3([l, a, b])
}

/// `QuantizeToSector` (`cmssm.c:167-177`). Saturate 360/180 to the last sector.
fn quantize_to_sector(sp: &Spherical) -> (i32, i32) {
    let mut alpha = ((sp.alpha * SECTORS as f64) / 360.0).floor() as i32;
    let mut theta = ((sp.theta * SECTORS as f64) / 180.0).floor() as i32;
    if alpha >= SECTORS as i32 {
        alpha = SECTORS as i32 - 1;
    }
    if theta >= SECTORS as i32 {
        theta = SECTORS as i32 - 1;
    }
    (alpha, theta)
}

/// `LineOf2Points` (`cmssm.c:181-189`).
fn line_of_2_points(a: &Vec3, b: &Vec3) -> Line {
    Line {
        a: Vec3([a.0[0], a.0[1], a.0[2]]),
        u: Vec3([b.0[0] - a.0[0], b.0[1] - a.0[1], b.0[2] - a.0[2]]),
    }
}

/// `GetPointOfLine` (`cmssm.c:193-199`).
fn get_point_of_line(line: &Line, t: f64) -> Vec3 {
    Vec3([
        line.a.0[0] + t * line.u.0[0],
        line.a.0[1] + t * line.u.0[1],
        line.a.0[2] + t * line.u.0[2],
    ])
}

fn dot(a: &Vec3, b: &Vec3) -> f64 {
    a.0[0] * b.0[0] + a.0[1] * b.0[1] + a.0[2] * b.0[2]
}

fn minus(a: &Vec3, b: &Vec3) -> Vec3 {
    Vec3([a.0[0] - b.0[0], a.0[1] - b.0[1], a.0[2] - b.0[2]])
}

/// `ClosestLineToLine` (`cmssm.c:216-294`): closest point on `line1` (segment
/// `0 ≤ t ≤ 1`) to `line2`. Transcribed verbatim including the parallel-lines and
/// edge-visibility branches.
fn closest_line_to_line(line1: &Line, line2: &Line) -> Vec3 {
    let w0 = minus(&line1.a, &line2.a);

    let a = dot(&line1.u, &line1.u);
    let b = dot(&line1.u, &line2.u);
    let c = dot(&line2.u, &line2.u);
    let d = dot(&line1.u, &w0);
    let e = dot(&line2.u, &w0);

    let big_d = a * c - b * b; // Denominator

    let mut s_d = big_d;
    let mut t_d = big_d;
    let mut s_n;
    let mut t_n;

    if big_d < MATRIX_DET_TOLERANCE {
        // Lines almost parallel.
        s_n = 0.0;
        s_d = 1.0;
        t_n = e;
        t_d = c;
    } else {
        s_n = b * e - c * d;
        t_n = a * e - b * d;

        if s_n < 0.0 {
            s_n = 0.0;
            t_n = e;
            t_d = c;
        } else if s_n > s_d {
            s_n = s_d;
            t_n = e + b;
            t_d = c;
        }
    }

    if t_n < 0.0 {
        t_n = 0.0;
        if -d < 0.0 {
            s_n = 0.0;
        } else if -d > a {
            s_n = s_d;
        } else {
            s_n = -d;
            s_d = a;
        }
    } else if t_n > t_d {
        t_n = t_d;
        if (-d + b) < 0.0 {
            s_n = 0.0;
        } else if (-d + b) > a {
            s_n = s_d;
        } else {
            s_n = -d + b;
            s_d = a;
        }
    }

    let sc = if s_n.abs() < MATRIX_DET_TOLERANCE {
        0.0
    } else {
        s_n / s_d
    };
    // `tc` is left for future use in lcms2; not needed here.
    let _ = t_n;
    let _ = t_d;

    get_point_of_line(line1, sc)
}

/// `Spiral[]` (`cmssm.c:427-435`): the relative neighbor offsets for
/// `FindNearSectors`, in lcms2's exact order.
const SPIRAL: [(i32, i32); 24] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (-1, -2),
    (0, -2),
    (1, -2),
    (2, -2),
    (2, -1),
    (2, 0),
    (2, 1),
    (2, 2),
    (1, 2),
    (0, 2),
    (-1, 2),
    (-2, 2),
    (-2, 1),
    (-2, 0),
    (-2, -1),
    (-2, -2),
];

/// `cmsGDB`: the segment-maxima descriptor (`cmssm.c:66-71`), indexed
/// `Gamut[theta][alpha]`.
#[derive(Clone)]
pub struct GamutBoundaryDescriptor {
    gamut: [[GdbPoint; SECTORS]; SECTORS],
}

impl Default for GamutBoundaryDescriptor {
    fn default() -> Self {
        Self::new()
    }
}

impl GamutBoundaryDescriptor {
    /// `cmsGBDAlloc` (`cmssm.c:302-310`): a zeroed (all-`Empty`) descriptor.
    #[must_use]
    pub fn new() -> Self {
        GamutBoundaryDescriptor {
            gamut: [[GdbPoint::default(); SECTORS]; SECTORS],
        }
    }

    /// `GetPoint` (`cmssm.c:322-354`): center L\* on 50, convert to spherical,
    /// quantize to a sector. Returns the `(theta, alpha)` sector index and the
    /// spherical coordinate, or `None` for the out-of-range error cases.
    fn get_point(lab: &CIELab) -> Option<((usize, usize), Spherical)> {
        let v = Vec3([lab.l - 50.0, lab.a, lab.b]);
        let sp = to_spherical(&v);

        if sp.r < 0.0 || sp.alpha < 0.0 || sp.theta < 0.0 {
            return None;
        }

        let (alpha, theta) = quantize_to_sector(&sp);
        if alpha < 0 || theta < 0 || alpha >= SECTORS as i32 || theta >= SECTORS as i32 {
            return None;
        }
        Some(((theta as usize, alpha as usize), sp))
    }

    /// `cmsGDBAddPoint` (`cmssm.c:358-387`): record `lab` in its sector, keeping
    /// the maximum radius. Returns `false` on the range error (matches lcms2).
    pub fn add_point(&mut self, lab: &CIELab) -> bool {
        let Some(((theta, alpha), sp)) = Self::get_point(lab) else {
            return false;
        };
        let ptr = &mut self.gamut[theta][alpha];
        // lcms2 (cmssm.c:370-384): if the sector is empty, record; otherwise
        // substitute only when the new radius is greater. The two arms set the same
        // fields, so they collapse to one condition.
        if ptr.ty == PointType::Empty || sp.r > ptr.p.r {
            ptr.ty = PointType::Specified;
            ptr.p = sp;
        }
        true
    }

    /// `cmsGDBCheckPoint` (`cmssm.c:390-406`): `true` iff `lab`'s radius is within
    /// (≤) the sector's stored maximum. An empty sector returns `false`.
    pub fn check_point(&self, lab: &CIELab) -> bool {
        let Some(((theta, alpha), sp)) = Self::get_point(lab) else {
            return false;
        };
        let ptr = &self.gamut[theta][alpha];
        if ptr.ty == PointType::Empty {
            return false;
        }
        sp.r <= ptr.p.r
    }

    /// `FindNearSectors` (`cmssm.c:439-469`): collect the non-empty neighbor
    /// sectors around `(alpha, theta)` along the spiral, wrapping at the edges.
    fn find_near_sectors(&self, alpha: i32, theta: i32) -> Vec<Spherical> {
        let mut close = Vec::with_capacity(SPIRAL.len());
        for (adv_x, adv_y) in SPIRAL {
            let mut a = (alpha + adv_x) % SECTORS as i32;
            let mut t = (theta + adv_y) % SECTORS as i32;
            if a < 0 {
                a += SECTORS as i32;
            }
            if t < 0 {
                t += SECTORS as i32;
            }
            let pt = &self.gamut[t as usize][a as usize];
            if pt.ty != PointType::Empty {
                close.push(pt.p);
            }
        }
        close
    }

    /// `InterpolateMissingSector` (`cmssm.c:473-545`): fill the `(alpha, theta)`
    /// sector by intersecting a center ray with every edge between near sectors,
    /// keeping the farthest intersection that still lands in this sector.
    fn interpolate_missing_sector(&mut self, alpha: i32, theta: i32) {
        if self.gamut[theta as usize][alpha as usize].ty != PointType::Empty {
            return;
        }

        let close = self.find_near_sectors(alpha, theta);

        // Central point on this sector.
        let sp = Spherical {
            alpha: ((alpha as f64 + 0.5) * 360.0) / SECTORS as f64,
            theta: ((theta as f64 + 0.5) * 180.0) / SECTORS as f64,
            r: 50.0,
        };
        let lab = to_cartesian(&sp);

        // Ray from the centre to this point.
        let centre = Vec3([50.0, 0.0, 0.0]);
        let ray = line_of_2_points(&lab, &centre);

        let mut closel = Spherical {
            r: 0.0,
            alpha: 0.0,
            theta: 0.0,
        };

        let n = close.len();
        for k in 0..n {
            for m in (k + 1)..n {
                let a1 = to_cartesian(&close[k]);
                let a2 = to_cartesian(&close[m]);
                let edge = line_of_2_points(&a1, &a2);

                let temp = closest_line_to_line(&ray, &edge);
                let templ = to_spherical(&temp);

                if templ.r > closel.r
                    && templ.theta >= (theta as f64 * 180.0 / SECTORS as f64)
                    && templ.theta <= ((theta as f64 + 1.0) * 180.0 / SECTORS as f64)
                    && templ.alpha >= (alpha as f64 * 360.0 / SECTORS as f64)
                    && templ.alpha <= ((alpha as f64 + 1.0) * 360.0 / SECTORS as f64)
                {
                    closel = templ;
                }
            }
        }

        self.gamut[theta as usize][alpha as usize].p = closel;
        self.gamut[theta as usize][alpha as usize].ty = PointType::Modeled;
    }

    /// `cmsGDBCompute` (`cmssm.c:550-582`): interpolate the missing sectors —
    /// black slice (theta=0), white slice (theta=SECTORS-1), then the mid range.
    pub fn compute(&mut self) {
        for alpha in 0..SECTORS as i32 {
            self.interpolate_missing_sector(alpha, 0);
        }
        for alpha in 0..SECTORS as i32 {
            self.interpolate_missing_sector(alpha, SECTORS as i32 - 1);
        }
        for theta in 1..SECTORS as i32 {
            for alpha in 0..SECTORS as i32 {
                self.interpolate_missing_sector(alpha, theta);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_cmsAtan2` returns degrees in `[0, 360)` and 0 for the undefined origin.
    #[test]
    fn atan2_deg_range() {
        assert_eq!(atan2_deg(0.0, 0.0), 0.0);
        assert!((atan2_deg(0.0, 1.0) - 0.0).abs() < 1e-9);
        assert!((atan2_deg(1.0, 0.0) - 90.0).abs() < 1e-9);
        // Negative y wraps to [0,360).
        let a = atan2_deg(-1.0, 0.0);
        assert!((a - 270.0).abs() < 1e-9, "got {a}");
    }

    /// A specified point falls in its own sector (radius equal ⇒ `<=` is in).
    #[test]
    fn added_point_is_in_gamut() {
        let mut gbd = GamutBoundaryDescriptor::new();
        let p = CIELab {
            l: 50.0,
            a: 30.0,
            b: 20.0,
        };
        assert!(gbd.add_point(&p));
        // Before compute, the exact added point is on the boundary ⇒ in gamut.
        assert!(gbd.check_point(&p));
    }

    /// After `compute`, the center of a roughly spherical hull is inside and a far
    /// point is outside.
    #[test]
    fn compute_fills_and_classifies() {
        let mut gbd = GamutBoundaryDescriptor::new();
        for li in 0..11 {
            let l = li as f64 * 10.0;
            for ai in -5..=5 {
                for bi in -5..=5 {
                    let a = ai as f64 * 8.0;
                    let b = bi as f64 * 8.0;
                    if ((l - 50.0).powi(2) + a * a + b * b).sqrt() <= 45.0 {
                        gbd.add_point(&CIELab { l, a, b });
                    }
                }
            }
        }
        gbd.compute();
        assert!(gbd.check_point(&CIELab {
            l: 50.0,
            a: 0.0,
            b: 0.0
        }));
        assert!(!gbd.check_point(&CIELab {
            l: 50.0,
            a: 120.0,
            b: 0.0
        }));
    }
}
