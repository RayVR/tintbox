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
use crate::context::Context;

thread_local! {
    /// A reusable empty [`Context`] for the no-context convenience evaluators
    /// ([`ToneCurve::eval_float`]/[`ToneCurve::eval_segmented`]). Holding one per
    /// thread avoids the per-call `Context::new()` construct+drop these wrappers
    /// otherwise pay; an empty context routes every parametric segment through the
    /// builtin path, so the result is byte-for-byte unchanged. (A plain `static`
    /// would need `Context<'static>: Sync`, which the `&dyn Logger` field denies;
    /// a `thread_local!` sidesteps the bound with no signature change.)
    static EMPTY_CONTEXT: Context<'static> = Context::new();
}

/// lcms2 `MINUS_INF` (cmsgamma.c:41): the *float* literal `-1E22F`.
const MINUS_INF: f32 = -1E22_f32;
/// lcms2 `PLUS_INF` (cmsgamma.c:42): the *float* literal `+1E22F`.
const PLUS_INF: f32 = 1E22_f32;

/// lcms2 `DefaultCurves.ParameterCount` (cmsgamma.c:64): coefficient count per
/// ICC *builtin* parametric type. Forward and inverse share the count. Returns
/// `None` for any type lcms2's builtin `GetParametricCurveByType` does not
/// recognise (a custom plugin may still service it — see
/// [`parametric_param_count_in`]).
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

/// Find a registered [`ParametricCurvePlugin`](crate::plugin::ParametricCurvePlugin)
/// servicing `ty` (or its inverse `-ty`), consulting the registry in
/// register-order (first match wins). Returns `None` when `ty` is a builtin id
/// (builtins are matched FIRST — a plugin can never shadow a builtin type) or
/// when no plugin services it.
///
/// Mirrors lcms2's `GetParametricCurveByType` plugin walk, but with the
/// builtin-wins guard tintbox enforces.
fn find_parametric_plugin<'a>(
    ctx: &'a Context,
    ty: i32,
) -> Option<&'a dyn crate::plugin::ParametricCurvePlugin> {
    // Builtins win: a plugin can never occupy a builtin id (forward or inverse).
    if parametric_param_count(ty).is_some() {
        return None;
    }
    for plugin in &ctx.plugins().parametric_curves {
        // lcms2 stores forward and inverse types as separate (signed) entries in
        // `Curves[]`; a plugin services an id iff it lists that exact id. So the
        // inverse `-ty` is only resolvable when the plugin explicitly registers
        // `-ty` (callers reverse-dispatch with the negated id).
        if plugin.function_types().contains(&ty) {
            return Some(plugin.as_ref());
        }
    }
    None
}

/// Context-aware coefficient count: a builtin type resolves via
/// [`parametric_param_count`]; otherwise a registered plugin that lists exactly
/// `ty` supplies the count. Returns `None` when neither knows `ty`.
pub fn parametric_param_count_in(ctx: &Context, ty: i32) -> Option<usize> {
    if let Some(n) = parametric_param_count(ty) {
        return Some(n);
    }
    find_parametric_plugin(ctx, ty).map(|p| p.parameter_count(ty))
}

/// Context-aware parametric evaluation. Builtins are tried FIRST (so a plugin can
/// never shadow ICC types `1..=8`/`108`/`109` or their inverses); only an
/// otherwise-unsupported `ty` consults the registered
/// [`ParametricCurvePlugin`](crate::plugin::ParametricCurvePlugin)s in
/// register-order. An unknown `ty` with no plugin returns `0.0`, exactly as the
/// builtin [`eval_parametric`] does.
pub fn eval_parametric_in(ctx: &Context, curve_type: i32, params: &[f64; 10], r: f64) -> f64 {
    // Builtins win.
    if parametric_param_count(curve_type).is_some() {
        return eval_parametric(curve_type, params, r);
    }
    if let Some(plugin) = find_parametric_plugin(ctx, curve_type) {
        return plugin.eval(curve_type, params, r);
    }
    // Unknown type, no plugin: mirror the builtin `_ => 0.0` arm.
    0.0
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
    build_parametric_in(&Context::new(), curve_type, params)
}

/// Context-aware [`build_parametric`]: the coefficient count and the materialised
/// 16-bit table both resolve through the [`Context`]'s parametric-curve registry
/// (builtins first), so a custom function type produces its plugin-defined table.
pub fn build_parametric_in(ctx: &Context, curve_type: i32, params: &[f64]) -> Option<ToneCurve> {
    let n = parametric_param_count_in(ctx, curve_type)?;
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
    Some(build_segmented_in(ctx, vec![seg]))
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
    build_segmented_in(&Context::new(), segments)
}

/// Context-aware [`build_segmented`]: the 16-bit table is materialised through
/// the [`Context`]-aware segmented evaluator, so any custom parametric segment
/// uses its registered plugin (builtins still win).
pub fn build_segmented_in(ctx: &Context, segments: Vec<CurveSegment>) -> ToneCurve {
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
        let val = curve.eval_segmented_in(ctx, r);
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
        EMPTY_CONTEXT.with(|ctx| self.eval_segmented_in(ctx, r))
    }

    /// Context-aware [`eval_segmented`](Self::eval_segmented): parametric segments
    /// route through [`eval_parametric_in`], so custom function types use their
    /// registered plugin (builtins still win). With an empty [`Context`] this is
    /// byte-for-byte the builtin path.
    pub fn eval_segmented_in(&self, ctx: &Context, r: f64) -> f64 {
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
                    eval_parametric_in(ctx, seg.seg_type, &seg.params, r)
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
        EMPTY_CONTEXT.with(|ctx| self.eval_float_in(ctx, v))
    }

    /// Context-aware [`eval_float`](Self::eval_float): a segmented curve carrying
    /// a custom parametric type is evaluated through [`eval_segmented_in`], so the
    /// plugin's formula is used. A pure tabulated curve is context-independent
    /// (it round-trips the already-materialised table). With an empty [`Context`]
    /// this is byte-for-byte the builtin path.
    pub fn eval_float_in(&self, ctx: &Context, v: f32) -> f32 {
        if self.segments.is_empty() {
            let in_ = Lcms2Floor::quick_saturate_word(v as f64 * 65535.0);
            let out = self.eval_16(in_);
            (out as f64 / 65535.0) as f32
        } else {
            self.eval_segmented_in(ctx, v as f64) as f32
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
    pub fn is_descending(&self) -> bool {
        self.table16[0] > self.table16[self.table16.len() - 1]
    }

    /// lcms2 `IsDegenerated` (cmsopt.c:1022-1039): curves with wide empty areas
    /// (too many `0x0000` or `0xffff` entries) are not optimizeable. A perfectly
    /// linear table (exactly one zero and one pole) is NOT degenerated.
    pub fn is_degenerated(&self) -> bool {
        let n_entries = self.table16.len();
        let mut zeros = 0usize;
        let mut poles = 0usize;
        for &v in &self.table16 {
            if v == 0x0000 {
                zeros += 1;
            }
            if v == 0xffff {
                poles += 1;
            }
        }
        if zeros == 1 && poles == 1 {
            return false; // For linear tables
        }
        if zeros > (n_entries / 20) {
            return true; // Degenerated, many zeros
        }
        if poles > (n_entries / 20) {
            return true; // Degenerated, many poles
        }
        false
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

    /// The floating-point segment description (lcms2 `cmsToneCurve.Segments`).
    /// Empty for a pure tabulated curve. The serializer inspects
    /// `nSegments`/`Segments[0].Type`/`Params` to mirror `Type_Curve_Write`'s
    /// gamma special case and `DecideCurveType`.
    pub fn segments(&self) -> &[CurveSegment] {
        &self.segments
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

/// Context-aware [`reverse_tone_curve`].
pub fn reverse_tone_curve_in(ctx: &Context, curve: &ToneCurve) -> ToneCurve {
    reverse_tone_curve_ex_in(ctx, 4096, curve)
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
    reverse_tone_curve_ex_in(&Context::new(), n_result_samples, curve)
}

/// Context-aware [`reverse_tone_curve_ex`].
///
/// Analytic reversal is attempted for a single parametric segment whose inverse
/// is known — a builtin type, OR a custom forward type whose plugin ALSO
/// registers the inverse `-type`. A custom forward type with NO registered
/// inverse falls through to NUMERIC table reversal (never producing the `0.0`
/// the bare `eval_parametric_in(-type)` would yield).
pub fn reverse_tone_curve_ex_in(
    ctx: &Context,
    n_result_samples: u32,
    curve: &ToneCurve,
) -> ToneCurve {
    // Analytic reversal whenever possible: one parametric segment whose inverse
    // is resolvable. For builtins that is `parametric_param_count(type)`; for a
    // custom forward type the inverse must be a SEPARATELY registered `-type`
    // plugin — otherwise we must fall back to numeric reversal below.
    if curve.segments.len() == 1 && curve.segments[0].seg_type > 0 {
        let seg = &curve.segments[0];
        let inv_ty = -seg.seg_type;
        let inverse_known = parametric_param_count(seg.seg_type).is_some()
            || find_parametric_plugin(ctx, inv_ty).is_some();
        if inverse_known {
            if let Some(c) = build_parametric_in(ctx, inv_ty, &seg.params) {
                return c;
            }
        }
        // else: custom forward type without a registered inverse — fall through
        // to numeric table reversal rather than emitting a 0.0 curve.
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

#[cfg(test)]
mod plugin_dispatch_tests {
    use super::*;
    use crate::plugin::ParametricCurvePlugin;
    use std::sync::Arc;

    /// A plugin servicing a NEW forward type id (200): `Y = X^gamma`, i.e. a
    /// re-implementation of the builtin type-1 formula under an unused id. No
    /// inverse registered.
    struct Pow200;
    impl ParametricCurvePlugin for Pow200 {
        fn function_types(&self) -> &[i32] {
            &[200]
        }
        fn parameter_count(&self, _ty: i32) -> usize {
            1
        }
        fn eval(&self, _ty: i32, params: &[f64; 10], r: f64) -> f64 {
            // Mirror builtin type 1 exactly (cmsgamma.c type 1).
            if r < 0.0 {
                if (params[0] - 1.0).abs() < MATRIX_DET_TOLERANCE_TEST {
                    r
                } else {
                    0.0
                }
            } else {
                r.powf(params[0])
            }
        }
    }
    const MATRIX_DET_TOLERANCE_TEST: f64 = 0.0001;

    fn ctx_with_pow200() -> Context<'static> {
        let mut ctx = Context::new();
        ctx.register_parametric_curve(Arc::new(Pow200));
        ctx
    }

    #[test]
    fn custom_type_param_count_resolves_via_plugin() {
        let ctx = ctx_with_pow200();
        // Builtin path does not know 200.
        assert_eq!(parametric_param_count(200), None);
        // Context-aware path resolves the count from the plugin.
        assert_eq!(parametric_param_count_in(&ctx, 200), Some(1));
        // An empty context still does not know 200.
        assert_eq!(parametric_param_count_in(&Context::new(), 200), None);
    }

    #[test]
    fn custom_type_eval_follows_plugin_formula() {
        let ctx = ctx_with_pow200();
        let mut p = [0.0f64; 10];
        p[0] = 2.2;
        // Custom formula: r^2.2.
        for &r in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let got = eval_parametric_in(&ctx, 200, &p, r);
            assert_eq!(got, r.powf(2.2));
        }
        // Without the plugin: unknown type → 0.0 (mirrors builtin `_` arm).
        assert_eq!(eval_parametric_in(&Context::new(), 200, &p, 0.5), 0.0);
    }

    #[test]
    fn build_parametric_in_materializes_custom_curve() {
        let ctx = ctx_with_pow200();
        let curve = build_parametric_in(&ctx, 200, &[2.2]).expect("custom type builds");
        // eval_float over the materialised table should follow r^2.2 (within the
        // 16-bit table's resolution).
        for &r in &[0.1f32, 0.5, 0.9] {
            let expected = (r as f64).powf(2.2) as f32;
            // eval_float_in routes the custom segment through the plugin.
            let got = curve.eval_float_in(&ctx, r);
            assert!(
                (got - expected).abs() < 1.0 / 256.0,
                "r={r}: got {got}, expected {expected}"
            );
            // The materialised table16 (built with ctx) also follows the formula.
            let got16 = curve.eval_16(Lcms2Floor::quick_saturate_word(r as f64 * 65535.0));
            assert!(
                (got16 as f64 / 65535.0 - expected as f64).abs() < 1.0 / 256.0,
                "table16 r={r}: got {got16}"
            );
        }
        // Legacy no-ctx build cannot service type 200.
        assert!(build_parametric(200, &[2.2]).is_none());
    }

    /// Differential bonus: a plugin re-implementing the builtin type-1 formula
    /// under a NEW id (200) must produce a BYTE-IDENTICAL `table16` to the
    /// builtin type-1 curve — proving the custom evaluator path matches the
    /// known-good builtin path bit-for-bit.
    #[test]
    fn custom_reimpl_of_type1_is_byte_identical() {
        let ctx = ctx_with_pow200();
        let builtin = build_parametric(1, &[2.4]).expect("builtin type 1");
        let custom = build_parametric_in(&ctx, 200, &[2.4]).expect("custom type 200");
        // The builtin type-1 single-segment curve uses the gamma!=1.0 grid of
        // 4096 points; the custom curve (seg_type 200) also uses 4096 (it is not
        // the seg_type==1 identity special case). Tables must match byte-for-byte.
        assert_eq!(custom.table16(), builtin.table16());
    }

    #[test]
    fn builtins_cannot_be_shadowed() {
        // A plugin claiming builtin type 1 must NEVER be consulted; the builtin
        // wins. Register a plugin that would return a bogus value for type 1.
        struct Bogus1;
        impl ParametricCurvePlugin for Bogus1 {
            fn function_types(&self) -> &[i32] {
                &[1]
            }
            fn parameter_count(&self, _ty: i32) -> usize {
                9
            }
            fn eval(&self, _ty: i32, _params: &[f64; 10], _r: f64) -> f64 {
                -999.0
            }
        }
        let mut ctx = Context::new();
        ctx.register_parametric_curve(Arc::new(Bogus1));
        // Param count: builtin (1), not the plugin's bogus 9.
        assert_eq!(parametric_param_count_in(&ctx, 1), Some(1));
        // Eval: builtin r^gamma, not -999.0.
        let mut p = [0.0f64; 10];
        p[0] = 2.0;
        assert_eq!(eval_parametric_in(&ctx, 1, &p, 0.5), 0.5f64.powf(2.0));
        // find_parametric_plugin refuses to surface a plugin for a builtin id.
        assert!(find_parametric_plugin(&ctx, 1).is_none());
    }

    #[test]
    fn custom_forward_without_inverse_falls_back_to_numeric() {
        // Pow200 registers only the forward type 200 (no -200). Reversing such a
        // curve must NOT analytically build `-200` (which would eval to 0.0);
        // it must fall back to NUMERIC table reversal.
        let ctx = ctx_with_pow200();
        let curve = build_parametric_in(&ctx, 200, &[2.2]).expect("custom builds");
        let rev = reverse_tone_curve_ex_in(&ctx, 4096, &curve);

        // A numeric reversal of a monotone increasing r^2.2 is a sane increasing
        // curve from ~0 to ~65535 — definitely NOT the all-zero table the bare
        // `eval_parametric_in(-200)` (returns 0.0) would have produced.
        assert!(rev.table16().iter().any(|&v| v > 0));
        assert!(*rev.table16().last().unwrap() > 60000);
        // Monotone non-decreasing.
        assert!(rev.table16().windows(2).all(|w| w[0] <= w[1]));

        // PROOF the fallback used the NUMERIC path: custom-200(g=2.2).table16 is
        // byte-identical to builtin-1(g=2.2).table16 (the byte-identical test
        // proves this for g=2.4; same construction for g=2.2), so reversing the
        // custom curve numerically must equal reversing a TABULATED copy of the
        // builtin curve (which forces the numeric path too, since it has no
        // parametric segment).
        let builtin = build_parametric(1, &[2.2]).unwrap();
        assert_eq!(curve.table16(), builtin.table16());
        let tabulated_builtin = build_tabulated_16(builtin.table16());
        let rev_numeric = reverse_tone_curve_ex(4096, &tabulated_builtin);
        assert_eq!(rev.table16(), rev_numeric.table16());

        // Reversing the builtin type-1 curve directly stays ANALYTIC (single
        // parametric segment of a known type) and is unaffected by the registry.
        let rev_builtin = reverse_tone_curve_ex_in(&ctx, 4096, &builtin);
        assert_eq!(rev_builtin, reverse_tone_curve_ex(4096, &builtin));
        // The analytic and numeric reversals differ (proving 200 took numeric).
        assert_ne!(rev_builtin.table16(), rev.table16());
    }
}
