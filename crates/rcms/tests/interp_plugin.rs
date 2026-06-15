//! Slice-8 T5: the Interpolation plugin (custom CLUT interpolators).
//!
//! Coverage:
//! - **Differential identity:** a custom interpolator that just forwards to the
//!   builtin `tetrahedral_16` is registered, a 3D 16-bit CLUT is built through
//!   `resolve_interp_in`, and its `eval` output is byte-for-byte identical to a
//!   builtin CLUT over a dense input sweep — proving the custom dispatch path
//!   does not perturb the result.
//! - **Functional:** a *distinguishable* custom interpolator (it writes a fixed
//!   sentinel) is selected and actually invoked, for both the 16-bit and float
//!   CLUT tables, proving the registry lookup + per-pixel dispatch are wired.
//! - **Fall-through:** a factory that declines (`None`) leaves the builtin path,
//!   and `interp_factory_in` with an empty context equals the builtin
//!   `interp_factory`.

use std::sync::Arc;

use rcms::context::Context;
use rcms::interp::{interp_factory, interp_factory_in, tetrahedral_16, InterpFn, InterpParams};
use rcms::pipeline::{Clut, ClutTable, ResolvedInterp};
use rcms::plugin::{CustomInterp, InterpolatorFactory};

/// A factory whose 16-bit interpolator forwards verbatim to the builtin
/// `tetrahedral_16`. Used by the differential-identity test: a CLUT built with
/// this factory MUST evaluate byte-identically to a plain builtin CLUT.
struct WrapTetra16;

impl InterpolatorFactory for WrapTetra16 {
    fn factory(
        &self,
        _n_in: usize,
        _n_out: usize,
        is_float: bool,
        _is_trilinear: bool,
    ) -> Option<CustomInterp> {
        if is_float {
            return None; // only the 16-bit path here
        }
        Some(CustomInterp::Lerp16(Arc::new(
            |input: &[u16], table: &[u16], p: &InterpParams, output: &mut [u16]| {
                tetrahedral_16(input, output, table, p);
            },
        )))
    }
}

/// A distinguishable custom interpolator: it ignores the table entirely and
/// writes a fixed sentinel into every output channel. Used by the functional
/// test to prove the custom routine is actually the one that runs.
const SENTINEL_U16: u16 = 0x1234;
const SENTINEL_F32: f32 = 0.5;

struct SentinelInterp;

impl InterpolatorFactory for SentinelInterp {
    fn factory(
        &self,
        _n_in: usize,
        _n_out: usize,
        is_float: bool,
        _is_trilinear: bool,
    ) -> Option<CustomInterp> {
        if is_float {
            Some(CustomInterp::LerpFloat(Arc::new(
                |_in: &[f32], _t: &[f32], _p: &InterpParams, out: &mut [f32]| {
                    out.iter_mut().for_each(|o| *o = SENTINEL_F32);
                },
            )))
        } else {
            Some(CustomInterp::Lerp16(Arc::new(
                |_in: &[u16], _t: &[u16], _p: &InterpParams, out: &mut [u16]| {
                    out.iter_mut().for_each(|o| *o = SENTINEL_U16);
                },
            )))
        }
    }
}

/// A factory that always declines — must fall through to the builtin.
struct DeclineAll;

impl InterpolatorFactory for DeclineAll {
    fn factory(&self, _: usize, _: usize, _: bool, _: bool) -> Option<CustomInterp> {
        None
    }
}

/// Tiny deterministic LCG so the differential sweep needs no oracle dependency.
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn u16(&mut self) -> u16 {
        (self.next() >> 32) as u16
    }
}

/// Build a 3-input 16-bit CLUT for a cubic `grid`³ with `n_out` outputs, filled
/// from `rng`. When `ctx` has the `WrapTetra16` factory registered the build
/// resolves a custom interpolator; otherwise it stays builtin.
fn build_clut_u16(ctx: &Context, grid: u32, n_out: usize, rng: &mut Lcg) -> Clut {
    let dims = [grid; 3];
    let params = InterpParams::new(&dims, 3, n_out);
    let len = (grid as usize).pow(3) * n_out;
    let table: Vec<u16> = (0..len).map(|_| rng.u16()).collect();
    Clut {
        table: ClutTable::U16(table),
        params,
        is_trilinear: false,
        implements_identity: false,
        resolved: ResolvedInterp::default(),
    }
    .resolve_interp_in(ctx)
}

#[test]
fn custom_wrapping_tetra16_is_byte_identical_to_builtin() {
    // ctx with the builtin-wrapping custom factory; the CLUT built through it
    // resolves InterpFn::Custom and dispatches to the stored closure.
    let mut custom_ctx = Context::new();
    custom_ctx.register_interpolator(Arc::new(WrapTetra16));

    let builtin_ctx = Context::new(); // empty -> builtin path

    let grids = [2u32, 3, 5, 9, 16, 17];
    let outs = [1usize, 3, 4];
    let mut compared: u64 = 0;

    for &grid in &grids {
        for &n_out in &outs {
            // Same RNG seed for both tables so the grids are identical.
            let mut rng_a = Lcg::new(0xC0FFEE ^ u64::from(grid) ^ ((n_out as u64) << 8));
            let mut rng_b = Lcg::new(0xC0FFEE ^ u64::from(grid) ^ ((n_out as u64) << 8));
            let custom = build_clut_u16(&custom_ctx, grid, n_out, &mut rng_a);
            let builtin = build_clut_u16(&builtin_ctx, grid, n_out, &mut rng_b);

            // The custom CLUT must carry a resolved custom interpolator; the
            // builtin one must not.
            assert!(
                matches!(custom.resolved.0, Some(InterpFn::Custom(_))),
                "custom CLUT should store an InterpFn::Custom (grid={grid}, n_out={n_out})"
            );
            assert!(
                builtin.resolved.0.is_none(),
                "builtin CLUT must keep the None slot (grid={grid}, n_out={n_out})"
            );

            let mut sweep = Lcg::new(0xABCDEF ^ u64::from(grid));
            for _ in 0..4000 {
                let input = [
                    sweep.u16() as f32 / 65535.0,
                    sweep.u16() as f32 / 65535.0,
                    sweep.u16() as f32 / 65535.0,
                ];
                let mut out_c = vec![0.0f32; n_out];
                let mut out_b = vec![0.0f32; n_out];
                custom.eval(&input, &mut out_c);
                builtin.eval(&input, &mut out_b);
                for i in 0..n_out {
                    assert_eq!(
                        out_c[i].to_bits(),
                        out_b[i].to_bits(),
                        "byte mismatch grid={grid} n_out={n_out} chan={i} input={input:?}"
                    );
                }
                compared += n_out as u64;
            }
        }
    }
    println!("custom-wraps-tetra16: {compared} output samples byte-identical to builtin");
}

#[test]
fn distinguishable_custom_interp_is_selected_and_used_u16() {
    let mut ctx = Context::new();
    ctx.register_interpolator(Arc::new(SentinelInterp));

    let mut rng = Lcg::new(7);
    let clut = build_clut_u16(&ctx, 5, 3, &mut rng);
    assert!(matches!(clut.resolved.0, Some(InterpFn::Custom(_))));

    let mut out = vec![0.0f32; 3];
    clut.eval(&[0.25, 0.5, 0.75], &mut out);
    // The sentinel u16 widened through From16ToFloat.
    let expected = SENTINEL_U16 as f32 / 65535.0;
    for (i, &o) in out.iter().enumerate() {
        assert_eq!(
            o.to_bits(),
            expected.to_bits(),
            "sentinel not used at chan {i}"
        );
    }
}

#[test]
fn distinguishable_custom_interp_is_selected_and_used_f32() {
    let mut ctx = Context::new();
    ctx.register_interpolator(Arc::new(SentinelInterp));

    let params = InterpParams::new(&[4, 4, 4], 3, 3);
    let table = vec![0.0f32; 4 * 4 * 4 * 3];
    let clut = Clut {
        table: ClutTable::F32(table),
        params,
        is_trilinear: false,
        implements_identity: false,
        resolved: ResolvedInterp::default(),
    }
    .resolve_interp_in(&ctx);
    assert!(matches!(clut.resolved.0, Some(InterpFn::Custom(_))));

    let mut out = vec![0.0f32; 3];
    clut.eval(&[0.1, 0.2, 0.3], &mut out);
    for (i, &o) in out.iter().enumerate() {
        assert_eq!(o, SENTINEL_F32, "float sentinel not used at chan {i}");
    }
}

#[test]
fn declining_factory_falls_through_to_builtin() {
    let mut ctx = Context::new();
    ctx.register_interpolator(Arc::new(DeclineAll));

    // A declining factory leaves the builtin path: no stored custom interp.
    let mut rng = Lcg::new(99);
    let clut = build_clut_u16(&ctx, 5, 3, &mut rng);
    assert!(
        clut.resolved.0.is_none(),
        "declined factory must keep builtin path"
    );

    // interp_factory_in with the declining ctx equals the builtin selection.
    let a = interp_factory_in(&ctx, 3, 3, false, false);
    let b = interp_factory(3, 3, false, false);
    assert!(matches!((&a, &b), (InterpFn::Lerp16(x), InterpFn::Lerp16(y)) if x == y));
}

#[test]
fn empty_context_interp_factory_in_equals_builtin() {
    let ctx = Context::new();
    for &n_in in &[1usize, 2, 3, 4, 8] {
        for &is_float in &[false, true] {
            for &is_tri in &[false, true] {
                let a = interp_factory_in(&ctx, n_in, 3, is_float, is_tri);
                let b = interp_factory(n_in, 3, is_float, is_tri);
                match (a, b) {
                    (InterpFn::Lerp16(x), InterpFn::Lerp16(y)) => assert_eq!(x, y),
                    (InterpFn::LerpFloat(x), InterpFn::LerpFloat(y)) => assert_eq!(x, y),
                    _ => panic!("empty-ctx interp_factory_in diverged from builtin"),
                }
            }
        }
    }
}
