//! Idiomatic extension traits — rcms's equivalent of lcms2's plugin system, with
//! NO C ABI. Each trait exposes one of rcms's existing internal seams as a public
//! Rust extension point; a [`Plugins`] registry (owned by [`Context`]) holds the
//! registered implementations, and [`Context`] gains `register_*` ergonomic
//! methods.
//!
//! This module (slice-8 task T0) lands only the SHARED TYPES and trait
//! SIGNATURES — the registry, the traits, and the `Context` registrars. It wires
//! in NO dispatch: [`Plugins::default()`] is empty, every registry is consulted
//! by the later tasks (T1–T5), and no existing entry point changes behaviour.
//!
//! ## Invariants the later tasks MUST honour
//! - **Register-order = priority.** Each registry is a `Vec`; the dispatcher
//!   walks it front-to-back and the FIRST match wins. `register_*` pushes, so the
//!   earliest-registered plugin has the highest priority.
//! - **Builtins win.** Every dispatcher matches the builtin (enum) arms FIRST; a
//!   plugin can only occupy a previously-`Unsupported`/unknown id. A plugin can
//!   never shadow `'XYZ '`, sRGB parametric type 4, etc.
//! - **Lookup happens at construction/link/read time**, never per-pixel. A lookup
//!   resolves to a concrete value (`OptimizedEval`/`InterpFn`/`Tag`/`Pipeline`/…)
//!   BEFORE any hot loop, so the per-pixel path keeps matching builtin enum arms
//!   and never touches `Context`/`Arc`.
//! - **`*_in(ctx, …)` convention.** Every dispatching entry point added by a later
//!   task takes the [`Context`] as its FIRST parameter and is named
//!   `<legacy_name>_in`. The legacy no-`ctx` function is kept as a delegating
//!   wrapper that calls `<name>_in(&Context::new(), …)`, so existing callers (and
//!   every differential test) hit the builtin path verbatim.

use std::sync::Arc;

use crate::context::Context;
use crate::error::Result;
use crate::io::{ProfileReader, ProfileWriter};
use crate::pipeline::Pipeline;
use crate::profile::{Profile, RenderingIntent, Tag};
use crate::sig::Signature;

// Re-export the optimizer seam, which lives in `crate::opt` (next to
// `OptimizedEval`/`OptimizationStrategy`) to avoid a `plugin` → `opt` → `plugin`
// dependency cycle. Consumers register it via `Context::set_optimizer`.
pub use crate::opt::{OptimizedEval, Optimizer};

// ---------------------------------------------------------------------------
// Object-safe profile I/O shims
// ---------------------------------------------------------------------------

/// Object-safe (`dyn`-compatible) view of [`ProfileReader`], so a plugin trait
/// object (`&mut dyn TagTypePlugin`) can read a tag body without being generic
/// over the concrete reader. The generic [`ProfileReader`] has `Self: Sized`
/// default-method machinery and so is not itself object-safe; this trait exposes
/// the minimal primitive set, and the blanket [`ProfileReader`] impl below
/// re-derives every convenience helper for `&mut dyn ProfileReaderDyn`.
pub trait ProfileReaderDyn {
    /// See [`ProfileReader::read_exact`].
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()>;
    /// See [`ProfileReader::seek`].
    fn seek(&mut self, pos: u64) -> Result<()>;
    /// See [`ProfileReader::tell`].
    fn tell(&self) -> u64;
    /// See [`ProfileReader::read_at`].
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()>;
}

impl<R: ProfileReader + ?Sized> ProfileReaderDyn for R {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        ProfileReader::read_exact(self, buf)
    }
    fn seek(&mut self, pos: u64) -> Result<()> {
        ProfileReader::seek(self, pos)
    }
    fn tell(&self) -> u64 {
        ProfileReader::tell(self)
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        ProfileReader::read_at(self, off, buf)
    }
}

/// Bridge a `&mut dyn ProfileReaderDyn` back into the generic [`ProfileReader`]
/// API, so a plugin can reuse every `read_u16`/`read_f32`/… helper on the
/// trait-object reader it is handed.
impl ProfileReader for &mut dyn ProfileReaderDyn {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        (**self).read_exact(buf)
    }
    fn seek(&mut self, pos: u64) -> Result<()> {
        (**self).seek(pos)
    }
    fn tell(&self) -> u64 {
        (**self).tell()
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_at(off, buf)
    }
}

/// Object-safe (`dyn`-compatible) view of [`ProfileWriter`]; mirror of
/// [`ProfileReaderDyn`] for the write side.
pub trait ProfileWriterDyn {
    /// See [`ProfileWriter::write_all`].
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
    /// See [`ProfileWriter::position`].
    fn position(&self) -> usize;
    /// See [`ProfileWriter::patch_u32`].
    fn patch_u32(&mut self, pos: usize, v: u32) -> Result<()>;
}

impl<W: ProfileWriter + ?Sized> ProfileWriterDyn for W {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        ProfileWriter::write_all(self, bytes)
    }
    fn position(&self) -> usize {
        ProfileWriter::position(self)
    }
    fn patch_u32(&mut self, pos: usize, v: u32) -> Result<()> {
        ProfileWriter::patch_u32(self, pos, v)
    }
}

/// Bridge a `&mut dyn ProfileWriterDyn` back into the generic [`ProfileWriter`]
/// API; mirror of the [`ProfileReader`] bridge above.
impl ProfileWriter for &mut dyn ProfileWriterDyn {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        (**self).write_all(bytes)
    }
    fn position(&self) -> usize {
        (**self).position()
    }
    fn patch_u32(&mut self, pos: usize, v: u32) -> Result<()> {
        (**self).patch_u32(pos, v)
    }
}

// ---------------------------------------------------------------------------
// Parametric curves
// ---------------------------------------------------------------------------

/// A custom parametric tone-curve function (lcms2 `cmsPluginParametricCurves`).
/// Registered via [`Context::register_parametric_curve`]; consulted by
/// `eval_parametric_in` / `parametric_param_count_in` (T1) AFTER the builtin
/// types, so a plugin can only add NEW function-type ids.
pub trait ParametricCurvePlugin: Send + Sync {
    /// The function-type ids this plugin services (lcms2's `Curves[]` /
    /// `nFunctions`). Negative ids denote the inverse of the positive id.
    fn function_types(&self) -> &[i32];
    /// The number of `params` slots the given type consumes (lcms2's
    /// `ParameterCount[]`).
    fn parameter_count(&self, ty: i32) -> usize;
    /// Evaluate the curve at `r` for the given type and parameters. `params` is
    /// the fixed 10-slot array lcms2's `cmsToneCurve` carries.
    fn eval(&self, ty: i32, params: &[f64; 10], r: f64) -> f64;
}

// ---------------------------------------------------------------------------
// Tag types & tags
// ---------------------------------------------------------------------------

/// A custom on-disk tag *type* handler (lcms2 `cmsPluginTagType`). Registered via
/// [`Context::register_tag_type`]; consulted by `read_tag_value_in` /
/// the serialize write path (T4) AFTER every builtin type, so a plugin can only
/// add a NEW type signature.
pub trait TagTypePlugin: Send + Sync {
    /// The on-disk type signature this plugin reads/writes.
    fn type_sig(&self) -> Signature;
    /// Decode a tag body of `size` bytes from `r`, producing a [`Tag`] (typically
    /// a [`Tag::Custom`] carrying the plugin's own value).
    fn read(&self, r: &mut dyn ProfileReaderDyn, size: u32) -> Result<Tag>;
    /// Serialize `tag` to `w`. Mirror of the builtin `Type_*_Write` handlers.
    fn write(&self, w: &mut dyn ProfileWriterDyn, tag: &Tag) -> Result<()>;
}

/// A custom logical *tag* (lcms2 `cmsPluginTag`): which on-disk types it may use,
/// and (mirroring lcms2's `DecideType` callback) how to choose one when writing.
/// Registered via [`Context::register_tag`].
pub struct TagDescriptor {
    /// The on-disk type signatures this tag may serialize as (lcms2's
    /// `SupportedTypes[]`). `SupportedTypes[0]` is the default.
    pub supported_types: Vec<Signature>,
    /// lcms2's `DecideType(version, data)`: pick the on-disk type for a value at a
    /// given ICC version. The default writer uses `supported_types[0]` when this
    /// is not consulted.
    pub decide_type: fn(f64, &Tag) -> Signature,
}

// ---------------------------------------------------------------------------
// Rendering intents
// ---------------------------------------------------------------------------

/// A custom rendering intent (lcms2 `cmsPluginRenderingIntent`). Registered via
/// [`Context::register_intent`]; consulted by the intent dispatcher (T3) AFTER
/// the builtin intents, so a plugin can only add a NEW intent number.
pub trait RenderingIntentPlugin: Send + Sync {
    /// The intent number this plugin services (lcms2's `Intent`).
    fn intent(&self) -> u32;
    /// A human-readable description (lcms2's `Description`).
    fn description(&self) -> &str;
    /// Build the device-link pipeline for the chained profiles — the plugin's
    /// equivalent of [`crate::link::default_icc_intents`]. Mirrors lcms2's
    /// `cmsIntentFn` `Link` callback signature.
    fn link(
        &self,
        ctx: &Context,
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: u32,
    ) -> Result<Pipeline>;
}

// ---------------------------------------------------------------------------
// Interpolation (tier-2)
// ---------------------------------------------------------------------------

/// A custom 16-bit interpolation routine: `(input, table, params, output)`.
/// Mirrors lcms2's `cmsInterpFn16` slot of the `cmsInterpFunction` union.
pub type CustomLerp16 =
    Arc<dyn Fn(&[u16], &[u16], &crate::interp::InterpParams, &mut [u16]) + Send + Sync>;

/// A custom float interpolation routine: `(input, table, params, output)`.
/// Mirrors lcms2's `cmsInterpFnFloat` slot of the `cmsInterpFunction` union.
pub type CustomLerpFloat =
    Arc<dyn Fn(&[f32], &[f32], &crate::interp::InterpParams, &mut [f32]) + Send + Sync>;

/// The concrete interpolator a [`InterpolatorFactory`] resolves — either a
/// builtin [`InterpFn`](crate::interp::InterpFn) the plugin selected, or a fully
/// custom evaluator. The custom evaluators are resolved at CLUT-build time and
/// stored in the pipeline, so the per-pixel loop never touches the factory.
#[derive(Clone)]
pub enum CustomInterp {
    /// Reuse one of rcms's builtin interpolation routines.
    Builtin(crate::interp::InterpFn),
    /// A custom 16-bit interpolator.
    Lerp16(CustomLerp16),
    /// A custom float interpolator.
    LerpFloat(CustomLerpFloat),
}

/// A custom interpolator factory (lcms2 `cmsPluginInterpolation`). Registered via
/// [`Context::register_interpolator`]; consulted by `interp_factory_in` (T5)
/// BEFORE the builtin factory in lcms2's model — but rcms keeps the
/// builtin-wins invariant by having the factory return `None` to decline and fall
/// through to the builtin.
pub trait InterpolatorFactory: Send + Sync {
    /// Resolve the interpolator for `(n_in, n_out, is_float, is_trilinear)`, or
    /// `None` to decline (fall through to the builtin factory).
    fn factory(
        &self,
        n_in: usize,
        n_out: usize,
        is_float: bool,
        is_trilinear: bool,
    ) -> Option<CustomInterp>;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The plugin registry [`Context`] owns. Default-empty; cheap to clone (every
/// field is a `Vec`/`Option` of `Arc` handles). Each `Vec` is consulted in
/// register-order (first match wins) by the later tasks' dispatchers.
#[derive(Default, Clone)]
pub struct Plugins {
    /// Registered parametric-curve plugins (T1).
    pub parametric_curves: Vec<Arc<dyn ParametricCurvePlugin>>,
    /// Registered tag-type handlers (T4).
    pub tag_types: Vec<Arc<dyn TagTypePlugin>>,
    /// Registered logical tags and their type descriptors (T4).
    pub tags: Vec<(Signature, Arc<TagDescriptor>)>,
    /// Registered rendering-intent plugins (T3).
    pub intents: Vec<Arc<dyn RenderingIntentPlugin>>,
    /// The custom pipeline optimizer, if set (T2). At most one (lcms2 keeps a
    /// list, but the rcms strategy seam selects a single optimizer).
    pub optimizer: Option<Arc<dyn Optimizer>>,
    /// Registered custom interpolator factories (tier-2, T5).
    pub interpolators: Vec<Arc<dyn InterpolatorFactory>>,
}

// `Context` lives in `crate::context`; its `Plugins` field and `register_*`
// methods are defined there (it already owns `logger`). Keeping the methods on
// `Context` next to the field avoids a split-impl across modules.
