//! Tests for the Optimization plugin wiring (lcms2 `cmsPluginOptimization`),
//! slice8-opt task S8-T2.
//!
//! A custom [`Optimizer`] registered on a [`Context`] is consulted at transform
//! construction BEFORE the builtin strategy chain. We assert:
//!
//! 1. **Functional — custom eval is used.** An optimizer returning a known
//!    [`OptimizedEval`] (a `MatShaper` built from the builtin matrix-shaper
//!    helper) fires even under the DEFAULT `Accurate` strategy, which would
//!    otherwise install the in-place pipeline eval. We observe this via
//!    `opt_path_label()` flipping from `"pipeline"` to `"matshaper"`.
//! 2. **Functional — decline falls back to Accurate.** An optimizer returning
//!    `None` declines, so the transform is byte-identical to the no-optimizer
//!    Accurate transform over the full 8-bit RGB cube.
//! 3. **Differential bonus.** A custom optimizer returning the SAME matrix-shaper
//!    data the builtin `Lcms2Compat` chain would → output byte-identical to the
//!    builtin `Lcms2Compat` transform.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tintbox::context::Context;
use tintbox::format::decode;
use tintbox::opt::matshaper;
use tintbox::opt::{OptimizationStrategy, OptimizedEval, Optimizer};
use tintbox::pipeline::Pipeline;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::Transform;

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

fn load(name: &str) -> Vec<u8> {
    std::fs::read(testbed_dir().join(name)).unwrap_or_else(|_| panic!("read {name}"))
}

fn type_rgb_8() -> u32 {
    (decode::PT_RGB << 16) | (3u32 << 3) | 1
}

const INTENT: RenderingIntent = RenderingIntent::RelativeColorimetric;

/// A full 8-bit RGB cube subsampled to `levels` points per channel, packed as
/// TYPE_RGB_8 bytes.
fn rgb8_grid(levels: usize) -> Vec<u8> {
    let step = if levels <= 1 { 255 } else { 255 / (levels - 1) };
    let mut out = Vec::with_capacity(levels * levels * levels * 3);
    for ri in 0..levels {
        for gi in 0..levels {
            for bi in 0..levels {
                out.push((ri * step).min(255) as u8);
                out.push((gi * step).min(255) as u8);
                out.push((bi * step).min(255) as u8);
            }
        }
    }
    out
}

/// A custom optimizer that returns whatever the builtin matrix-shaper helper
/// produces (lcms2 `OptimizeMatrixShaper`), i.e. it re-uses the builtin
/// `Lcms2Compat` matrix-shaper data — declining (`None`) when that helper does.
struct MatShaperOptimizer;
impl Optimizer for MatShaperOptimizer {
    fn optimize(
        &self,
        lut: &Pipeline,
        in_fmt: u32,
        out_fmt: u32,
        _intent: u32,
    ) -> Option<OptimizedEval> {
        matshaper::try_optimize(lut, in_fmt, out_fmt).map(|d| OptimizedEval::MatShaper(Box::new(d)))
    }
}

/// A custom optimizer that always declines (`None`).
struct DecliningOptimizer;
impl Optimizer for DecliningOptimizer {
    fn optimize(&self, _: &Pipeline, _: u32, _: u32, _: u32) -> Option<OptimizedEval> {
        None
    }
}

#[test]
fn custom_optimizer_eval_is_used_under_accurate_default() {
    // crayons -> test5 is a non-identity-merged RGB matrix-shaper pair, so the
    // builtin matrix-shaper helper fires. With the DEFAULT `Accurate` strategy the
    // builtin path would install `OptimizedEval::Pipeline` ("pipeline"); the custom
    // optimizer must override that to the matrix-shaper eval ("matshaper").
    let ab = load("crayons.icc");
    let bb = load("test5.icc");
    let pa = Profile::open(&ab).unwrap();
    let pb = Profile::open(&bb).unwrap();
    let fmt = type_rgb_8();

    // Baseline: no optimizer, Accurate -> in-place pipeline eval.
    let baseline = Transform::new_simple_with_formats(&pa, &pb, INTENT, false, fmt, fmt).unwrap();
    assert_eq!(baseline.opt_path_label(), "pipeline");
    assert!(!baseline.matshaper_fired());

    // With the custom optimizer registered, it fires FIRST, even under Accurate.
    let mut ctx = Context::new();
    ctx.set_optimizer(Arc::new(MatShaperOptimizer));
    let xform = Transform::new_simple_with_formats_strategy_in(
        &ctx,
        &pa,
        &pb,
        INTENT,
        false,
        fmt,
        fmt,
        OptimizationStrategy::Accurate,
    )
    .unwrap();
    assert_eq!(
        xform.opt_path_label(),
        "matshaper",
        "custom optimizer's eval must be installed, overriding Accurate"
    );
    assert!(xform.matshaper_fired());
}

#[test]
fn declining_optimizer_falls_back_to_accurate_byte_identical() {
    // An optimizer that returns None must leave the Accurate path untouched: the
    // output is byte-identical to the no-optimizer Accurate transform.
    let grid = rgb8_grid(16);
    let n = grid.len() / 3;
    let ab = load("crayons.icc");
    let bb = load("test5.icc");
    let pa = Profile::open(&ab).unwrap();
    let pb = Profile::open(&bb).unwrap();
    let fmt = type_rgb_8();

    let baseline = Transform::new_simple_with_formats(&pa, &pb, INTENT, false, fmt, fmt).unwrap();
    let mut base_out = vec![0u8; n * 3];
    baseline.do_transform(&grid, &mut base_out, n);

    let mut ctx = Context::new();
    ctx.set_optimizer(Arc::new(DecliningOptimizer));
    let xform = Transform::new_simple_with_formats_strategy_in(
        &ctx,
        &pa,
        &pb,
        INTENT,
        false,
        fmt,
        fmt,
        OptimizationStrategy::Accurate,
    )
    .unwrap();
    assert_eq!(xform.opt_path_label(), "pipeline");
    assert!(!xform.matshaper_fired());
    let mut out = vec![0u8; n * 3];
    xform.do_transform(&grid, &mut out, n);

    assert_eq!(
        out, base_out,
        "declining optimizer must fall back to Accurate byte-identically"
    );
}

#[test]
fn custom_optimizer_matches_builtin_lcms2compat() {
    // Differential bonus: a custom optimizer returning the SAME matrix-shaper data
    // the builtin `Lcms2Compat` chain produces must yield output byte-identical to
    // the builtin `Lcms2Compat` transform over the full 8-bit RGB cube.
    let grid = rgb8_grid(16);
    let n = grid.len() / 3;
    let ab = load("crayons.icc");
    let bb = load("test5.icc");
    let pa = Profile::open(&ab).unwrap();
    let pb = Profile::open(&bb).unwrap();
    let fmt = type_rgb_8();

    // Builtin Lcms2Compat (no custom optimizer).
    let builtin = Transform::new_simple_with_formats_strategy(
        &pa,
        &pb,
        INTENT,
        false,
        fmt,
        fmt,
        OptimizationStrategy::Lcms2Compat,
    )
    .unwrap();
    assert!(
        builtin.matshaper_fired(),
        "expected builtin matshaper to fire"
    );
    let mut builtin_out = vec![0u8; n * 3];
    builtin.do_transform(&grid, &mut builtin_out, n);

    // Custom optimizer reproducing the same matshaper data, even under Accurate.
    let mut ctx = Context::new();
    ctx.set_optimizer(Arc::new(MatShaperOptimizer));
    let custom = Transform::new_simple_with_formats_strategy_in(
        &ctx,
        &pa,
        &pb,
        INTENT,
        false,
        fmt,
        fmt,
        OptimizationStrategy::Accurate,
    )
    .unwrap();
    assert!(custom.matshaper_fired());
    let mut custom_out = vec![0u8; n * 3];
    custom.do_transform(&grid, &mut custom_out, n);

    assert_eq!(
        custom_out, builtin_out,
        "custom optimizer reproducing Lcms2Compat matshaper data must match builtin Lcms2Compat"
    );
}
