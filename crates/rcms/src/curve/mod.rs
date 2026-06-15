//! Tone curves (lcms2 `cmsToneCurve`, cmsgamma.c).
//!
//! A tone curve is described by zero or more *curve segments* plus a
//! limited-precision 16-bit approximation table. A segment is either *sampled*
//! (`seg_type == 0`, evaluated by table interpolation over `sampled`) or
//! *parametric* (`seg_type != 0`, evaluated by [`eval_parametric`] from the ICC
//! parametric function family).
//!
//! This module lands the data model, the parametric evaluator, 1D interpolation
//! ([`interp`]), the tabulated/segmented/parametric/gamma constructors, and full
//! evaluation ([`ToneCurve::eval_16`], [`ToneCurve::eval_float`]).

mod interp;
mod parametric;

pub use interp::{lin_lerp_16, lin_lerp_1d_float};
pub use parametric::eval_parametric;

use crate::compat::floor::{FloorStrategy, Lcms2Floor};

/// lcms2 `MINUS_INF` (cmsgamma.c:41): the *float* literal `-1E22F`.
const MINUS_INF: f32 = -1E22_f32;
/// lcms2 `PLUS_INF` (cmsgamma.c:42): the *float* literal `+1E22F`.
const PLUS_INF: f32 = 1E22_f32;

/// lcms2 `DefaultCurves.ParameterCount` (cmsgamma.c:64): coefficient count per
/// ICC parametric type. Forward and inverse share the count. Returns `None` for
/// any type lcms2's `GetParametricCurveByType` does not recognise.
fn parametric_param_count(ty: i32) -> Option<usize> {
    Some(match ty.abs() {
        1 => 1,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 7,
        6 => 4,
        7 => 5,
        8 => 5,
        108 => 1,
        109 => 1,
        _ => return None,
    })
}

/// One segment of a segmented tone curve (lcms2 `cmsCurveSegment`).
///
/// `seg_type == 0` marks a *sampled* segment: it carries `sampled` points
/// interpolated over `[x0, x1]`. A nonzero `seg_type` is an ICC parametric
/// function (positive forward types 1..=8/108/109 and their negative inverses);
/// `params` holds its coefficients (each type reads `params[0..n]`).
#[derive(Clone, Debug, PartialEq)]
pub struct CurveSegment {
    /// Lower bound of the segment's domain (exclusive in lcms2's `EvalSegmentedFn`).
    pub x0: f32,
    /// Upper bound of the segment's domain (inclusive in lcms2's `EvalSegmentedFn`).
    pub x1: f32,
    /// ICC parametric function type, or `0` for a sampled segment.
    pub seg_type: i32,
    /// Parametric coefficients (only `params[0..n]` are meaningful per type).
    pub params: [f64; 10],
    /// Sampled points (used only when `seg_type == 0`).
    pub sampled: Vec<f32>,
}

/// A tone curve (lcms2 `cmsToneCurve`).
///
/// `segments` is the floating-point description (empty for a pure tabulated
/// curve); `table16` is the 16-bit limited-precision approximation used by the
/// integer fast paths. Constructors that populate these land in a later task.
#[derive(Clone, Debug, PartialEq)]
pub struct ToneCurve {
    pub(crate) segments: Vec<CurveSegment>,
    pub(crate) table16: Vec<u16>,
}

/// lcms2 `EntriesByGamma` (cmsgamma.c:788-793): a gamma-1.0 identity curve only
/// needs 2 grid points; everything else gets 4096.
fn entries_by_gamma(gamma: f64) -> u32 {
    if (gamma - 1.0).abs() < 0.001 {
        2
    } else {
        4096
    }
}

/// Build a 16-bit limited-precision tabulated curve.
///
/// lcms2 `cmsBuildTabulatedToneCurve16` (cmsgamma.c:783): zero segments, the
/// supplied table copied verbatim as the 16-bit approximation.
pub fn build_tabulated_16(table: &[u16]) -> ToneCurve {
    ToneCurve {
        segments: Vec::new(),
        table16: table.to_vec(),
    }
}

/// Build a floating-point tabulated curve.
///
/// lcms2 `cmsBuildTabulatedToneCurveFloat` (cmsgamma.c:832-873): wraps the
/// samples in a three-segment curve — constant `values[0]` below 0, a sampled
/// segment over `[0, 1]`, constant `values[last]` above 1 — then materialises the
/// 16-bit table via [`build_segmented`]. Returns `None` for an empty table
/// (lcms2's `nEntries == 0` guard).
pub fn build_tabulated_float(table: &[f32]) -> Option<ToneCurve> {
    if table.is_empty() {
        return None;
    }
    let first = table[0] as f64;
    let last = table[table.len() - 1] as f64;

    // Seg[0]: constant = samples[0] for x in (-inf, 0], type 6, params {1,0,0,first,0}.
    let seg0 = CurveSegment {
        x0: MINUS_INF,
        x1: 0.0,
        seg_type: 6,
        params: [1.0, 0.0, 0.0, first, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        sampled: Vec::new(),
    };
    // Seg[1]: sampled over [0, 1].
    let seg1 = CurveSegment {
        x0: 0.0,
        x1: 1.0,
        seg_type: 0,
        params: [0.0; 10],
        sampled: table.to_vec(),
    };
    // Seg[2]: constant = samples[last] for x in (1, +inf], type 6.
    let seg2 = CurveSegment {
        x0: 1.0,
        x1: PLUS_INF,
        seg_type: 6,
        params: [1.0, 0.0, 0.0, last, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        sampled: Vec::new(),
    };

    Some(build_segmented(vec![seg0, seg1, seg2]))
}

/// Build a parametric curve of the given ICC type.
///
/// lcms2 `cmsBuildParametricToneCurve` (cmsgamma.c:880-904): one function segment
/// spanning `(MINUS_INF, PLUS_INF]`, then materialised via [`build_segmented`].
/// Returns `None` when lcms2 would reject the type (unknown) or `params` is too
/// short for the type's coefficient count.
pub fn build_parametric(curve_type: i32, params: &[f64]) -> Option<ToneCurve> {
    let n = parametric_param_count(curve_type)?;
    if params.len() < n {
        return None;
    }
    // memset(&Seg0, 0, ...); memmove(Seg0.Params, Params, count*sizeof(double)).
    let mut p = [0.0f64; 10];
    p[..n].copy_from_slice(&params[..n]);

    let seg = CurveSegment {
        x0: MINUS_INF,
        x1: PLUS_INF,
        seg_type: curve_type,
        params: p,
        sampled: Vec::new(),
    };
    Some(build_segmented(vec![seg]))
}

/// Build a gamma curve.
///
/// lcms2 `cmsBuildGamma` (cmsgamma.c:909-912): `cmsBuildParametricToneCurve(1, {gamma})`.
pub fn build_gamma(gamma: f64) -> ToneCurve {
    // The parametric type 1 with a single in-range param is always accepted.
    build_parametric(1, &[gamma]).expect("type-1 parametric curve is always valid")
}

/// Build a segmented curve and materialise its 16-bit approximation table.
///
/// lcms2 `cmsBuildSegmentedToneCurve` (cmsgamma.c:797-829): the table has 4096
/// grid points, reduced to [`entries_by_gamma`] only for the single-segment
/// gamma-1.0 identity. Each entry `i` is `EvalSegmentedFn(i/(n-1))` quantised by
/// `_cmsQuickSaturateWord(Val * 65535.0)`.
pub fn build_segmented(segments: Vec<CurveSegment>) -> ToneCurve {
    let mut n_grid_points: u32 = 4096;

    // Optimization for identity curves.
    if segments.len() == 1 && segments[0].seg_type == 1 {
        n_grid_points = entries_by_gamma(segments[0].params[0]);
    }

    let mut curve = ToneCurve {
        segments,
        table16: vec![0u16; n_grid_points as usize],
    };

    for i in 0..n_grid_points {
        let r = i as f64 / (n_grid_points - 1) as f64;
        let val = curve.eval_segmented(r);
        // Round and saturate: _cmsQuickSaturateWord(Val * 65535.0).
        curve.table16[i as usize] = Lcms2Floor::quick_saturate_word(val * 65535.0);
    }

    curve
}

/// Build a segmented curve from MPE-decoded segments, applying lcms2's
/// `ReadSegmentedCurve` post-build fix-up (cmstypes.c:4317-4332).
///
/// lcms2 builds the curve via `cmsBuildSegmentedToneCurve` (which materialises
/// the 16-bit table) and THEN, for every *sampled* segment (`seg_type == 0`),
/// overwrites the implicit first sample point with
/// `cmsEvalToneCurveFloat(Curve, x0)`. The first point is implicit in the ICC
/// wire format (lcms2 allocates an extra slot, leaves it `0`, then fills it
/// here). The table is built BEFORE the fix-up, so the fix-up affects only later
/// float-domain evaluation of those sampled segments — match that ordering.
pub fn build_mpe_segmented(segments: Vec<CurveSegment>) -> ToneCurve {
    let mut curve = build_segmented(segments);

    // Fix-up implicit points: SampledPoints[0] = EvalSegmentedFn(x0) for each
    // sampled segment, exactly as the C does after building the table.
    let n = curve.segments.len();
    for i in 0..n {
        if curve.segments[i].seg_type == 0 {
            let x0 = curve.segments[i].x0 as f64;
            let v = curve.eval_segmented(x0);
            if !curve.segments[i].sampled.is_empty() {
                curve.segments[i].sampled[0] = v as f32;
            }
        }
    }

    curve
}

impl ToneCurve {
    /// lcms2 `EvalSegmentedFn` (cmsgamma.c:722-765): evaluate the floating-point
    /// segmented description at `r`. Segments are scanned BACKWARDS; the first
    /// whose `(x0, x1]` contains `r` is evaluated (sampled → float interpolation,
    /// otherwise the parametric evaluator). An infinite result clamps to `±1E22`.
    /// Returns `MINUS_INF` when no segment matches.
    pub fn eval_segmented(&self, r: f64) -> f64 {
        for seg in self.segments.iter().rev() {
            // Check for domain: (R > x0) && (R <= x1).
            if r > seg.x0 as f64 && r <= seg.x1 as f64 {
                let out = if seg.seg_type == 0 {
                    // Sampled segment: rescale R into [0,1] over [x0, x1], then
                    // float-interp the sampled points. R1 is computed in f32.
                    let r1 = (r - seg.x0 as f64) as f32 / (seg.x1 - seg.x0);
                    let domain = (seg.sampled.len() - 1) as u32;
                    lin_lerp_1d_float(r1, &seg.sampled, domain) as f64
                } else {
                    eval_parametric(seg.seg_type, &seg.params, r)
                };

                // cmsgamma.c:752-758: `if (isinf(Out)) return PLUS_INF;` — C's
                // isinf() is true for BOTH +inf and -inf, so both clamp to
                // PLUS_INF (the subsequent `isinf(-Out) → MINUS_INF` is dead code
                // in the C, only reachable when Out is finite). Match exactly.
                if out.is_infinite() {
                    return PLUS_INF as f64;
                }
                return out;
            }
        }
        MINUS_INF as f64
    }

    /// lcms2 `cmsEvalToneCurveFloat` (cmsgamma.c:1418-1434).
    ///
    /// A pure tabulated curve (`nSegments == 0`) round-trips through the 16-bit
    /// table; otherwise the segmented description is evaluated directly (the f32
    /// input widens to f64 for [`eval_segmented`], then the result narrows to f32).
    pub fn eval_float(&self, v: f32) -> f32 {
        if self.segments.is_empty() {
            let in_ = Lcms2Floor::quick_saturate_word(v as f64 * 65535.0);
            let out = self.eval_16(in_);
            (out as f64 / 65535.0) as f32
        } else {
            self.eval_segmented(v as f64) as f32
        }
    }

    /// lcms2 `cmsEvalToneCurve16` (cmsgamma.c:1437-1445): always interpolate over
    /// the 16-bit table (`domain == nEntries - 1`).
    pub fn eval_16(&self, v: u16) -> u16 {
        let domain = (self.table16.len() - 1) as u32;
        lin_lerp_16(v, &self.table16, domain)
    }

    /// lcms2 `cmsIsToneCurveLinear` (cmsgamma.c:1329-1344): every table entry is
    /// within `0x0f` of the ideal linear ramp `_cmsQuantizeVal(i, nEntries)`.
    pub fn is_linear(&self) -> bool {
        let n = self.table16.len();
        for (i, &v) in self.table16.iter().enumerate() {
            let diff = (v as i32 - quantize_val(i as f64, n as u32) as i32).abs();
            if diff > 0x0f {
                return false;
            }
        }
        true
    }

    /// lcms2 `cmsIsToneCurveDescending` (cmsgamma.c:1393-1398).
    fn is_descending(&self) -> bool {
        self.table16[0] > self.table16[self.table16.len() - 1]
    }

    /// lcms2 `cmsIsToneCurveMonotonic` (cmsgamma.c:1347-1390): monotone in the
    /// curve's direction, allowing a 2-count ripple. Degenerate (`< 2` entries)
    /// curves are treated as monotonic.
    pub fn is_monotonic(&self) -> bool {
        let n = self.table16.len();
        if n < 2 {
            return true;
        }

        if self.is_descending() {
            let mut last = self.table16[0] as i32;
            for i in 1..n {
                if self.table16[i] as i32 - last > 2 {
                    return false;
                }
                last = self.table16[i] as i32;
            }
        } else {
            let mut last = self.table16[n - 1] as i32;
            for i in (0..n - 1).rev() {
                if self.table16[i] as i32 - last > 2 {
                    return false;
                }
                last = self.table16[i] as i32;
            }
        }
        true
    }

    /// lcms2 `cmsIsToneCurveMultisegment` (cmsgamma.c:1402-1407).
    pub fn is_multisegment(&self) -> bool {
        self.segments.len() > 1
    }

    /// The 16-bit limited-precision approximation table (lcms2
    /// `cmsGetToneCurveEstimatedTable`).
    pub fn table16(&self) -> &[u16] {
        &self.table16
    }
}

/// lcms2 `GetInterval` (cmsgamma.c:1015-1067): find the table interval whose
/// `[y0, y1]` (or `[y1, y0]` when locally decreasing) brackets `inp`. `lut_table`
/// is the source curve's 16-bit table; `domain` is `nEntries - 1`. Returns the
/// interval index `j` (so `[lut_table[j], lut_table[j+1]]` brackets `inp`), or
/// `-1` if none. The overall-ascending case scans high→low; the
/// overall-descending case scans low→high — transcribed verbatim.
fn get_interval(inp: f64, lut_table: &[u16], domain: usize) -> i32 {
    // A 1 point table is not allowed.
    if domain < 1 {
        return -1;
    }

    if lut_table[0] < lut_table[domain] {
        // Table is overall ascending: scan from Domain-1 down to 0.
        let mut i = domain as i32 - 1;
        while i >= 0 {
            let y0 = lut_table[i as usize] as i32;
            let y1 = lut_table[i as usize + 1] as i32;
            let ii = inp as i32;
            if y0 <= y1 {
                if ii >= y0 && ii <= y1 {
                    return i;
                }
            } else if y1 < y0 && ii >= y1 && ii <= y0 {
                return i;
            }
            i -= 1;
        }
    } else {
        // Table is overall descending: scan from 0 up to Domain-1.
        for i in 0..domain {
            let y0 = lut_table[i] as i32;
            let y1 = lut_table[i + 1] as i32;
            let ii = inp as i32;
            if y0 <= y1 {
                if ii >= y0 && ii <= y1 {
                    return i as i32;
                }
            } else if y1 < y0 && ii >= y1 && ii <= y0 {
                return i as i32;
            }
        }
    }

    -1
}

/// lcms2 `cmsReverseToneCurve` (cmsgamma.c:1137-1141): reverse with 4096 result
/// samples.
pub fn reverse_tone_curve(curve: &ToneCurve) -> ToneCurve {
    reverse_tone_curve_ex(4096, curve)
}

/// lcms2 `cmsReverseToneCurveEx` (cmsgamma.c:1070-1134).
///
/// If the curve is a single parametric segment of a recognised type, the inverse
/// is built analytically as `build_parametric(-type, params)` (the negative type
/// the parametric evaluator already handles). Otherwise the 16-bit table is
/// reversed numerically: for each result sample `i`, find the interval bracketing
/// `y = i*65535/(n-1)` via [`get_interval`] and linearly interpolate the inverse.
///
/// # Panics
/// Panics if `n_result_samples < 2` (lcms2 divides by `n - 1`).
pub fn reverse_tone_curve_ex(n_result_samples: u32, curve: &ToneCurve) -> ToneCurve {
    // Analytic reversal whenever possible: one parametric segment of a known type.
    if curve.segments.len() == 1
        && curve.segments[0].seg_type > 0
        && parametric_param_count(curve.segments[0].seg_type).is_some()
    {
        let seg = &curve.segments[0];
        if let Some(c) = build_parametric(-seg.seg_type, &seg.params) {
            return c;
        }
    }

    // Numeric reversal of the table.
    let n = n_result_samples as usize;
    let mut out = vec![0u16; n];

    let src = &curve.table16;
    let n_entries = src.len();
    // Domain[0] of the SOURCE curve = nEntries - 1.
    let domain = n_entries.saturating_sub(1);

    // Ascending = !cmsIsToneCurveDescending; cmsIsToneCurveDescending is
    // `Table16[0] > Table16[nEntries-1]` (cmsgamma.c:1393), so Ascending is the
    // negation: `Table16[0] <= Table16[nEntries-1]`.
    let descending = src[0] > src[n_entries - 1];
    let ascending = !descending;

    let mut a = 0.0f64;
    let mut b = 0.0f64;

    for (i, slot) in out.iter_mut().enumerate() {
        let y = i as f64 * 65535.0 / (n_result_samples - 1) as f64;

        let j = get_interval(y, src, domain);
        if j >= 0 {
            let j = j as usize;
            let x1 = src[j] as f64;
            let x2 = src[j + 1] as f64;

            let y1 = (j as f64 * 65535.0) / (n_entries - 1) as f64;
            let y2 = ((j + 1) as f64 * 65535.0) / (n_entries - 1) as f64;

            if x1 == x2 {
                // Collapsed interval: use either endpoint per direction.
                *slot = Lcms2Floor::quick_saturate_word(if ascending { y2 } else { y1 });
                continue;
            } else {
                a = (y2 - y1) / (x2 - x1);
                b = y2 - a * x2;
            }
        }

        *slot = Lcms2Floor::quick_saturate_word(a * y + b);
    }

    build_tabulated_16(&out)
}

/// lcms2 `_cmsQuantizeVal` (cmslut.c:737-743): the ideal `i`-th node of an
/// `MaxSamples`-entry linear ramp, `(i * 65535) / (MaxSamples - 1)` saturated.
fn quantize_val(i: f64, max_samples: u32) -> u16 {
    let x = (i * 65535.0) / (max_samples - 1) as f64;
    Lcms2Floor::quick_saturate_word(x)
}
