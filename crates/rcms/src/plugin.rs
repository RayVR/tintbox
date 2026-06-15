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

#[cfg(test)]
mod tag_type_tests {
    //! Slice-8 T4: wiring of the TagType + Tag plugins (custom ICC tag types) into
    //! the read dispatch (`read_tag_value_in` → `Profile::read_tag_in`) and the
    //! serialize write dispatch (`save_to_mem_in`).
    use super::*;
    use crate::color::CIEXYZ;
    use crate::io::{ProfileReader, ProfileWriter};
    use crate::profile::header::{ColorSpace, DateTime, Header, ProfileClass, RenderingIntent};
    use crate::profile::tag::CustomTagData;
    use crate::profile::{save_to_mem, save_to_mem_in, Profile, Tag, WritableProfile};

    /// A minimal valid v4 header for the writer (size/illuminant are ignored).
    fn header() -> Header {
        Header {
            size: 0,
            cmm: Signature::from_raw(0),
            version: 0x0440_0000,
            device_class: ProfileClass::Display,
            color_space: ColorSpace::Rgb,
            pcs: ColorSpace::XYZ,
            date: DateTime {
                year: 2026,
                month: 6,
                day: 15,
                hours: 0,
                minutes: 0,
                seconds: 0,
            },
            platform: Signature::from_raw(0),
            flags: 0,
            manufacturer: Signature::from_raw(0),
            model: 0,
            attributes: 0,
            rendering_intent: RenderingIntent::Perceptual,
            illuminant: CIEXYZ {
                x: 0.9642,
                y: 1.0,
                z: 0.8249,
            },
            creator: Signature::from_raw(0),
            profile_id: [0u8; 16],
        }
    }

    // ---- Functional round-trip: a custom type carrying a single u32. ----

    /// The opaque cooked value the round-trip plugin reads/writes.
    #[derive(Debug, PartialEq)]
    struct MyVal(u32);

    const CUST_TYPE: Signature = Signature::from_bytes(*b"cust");
    const MTAG: Signature = Signature::from_bytes(*b"mTag");

    struct CustPlugin;
    impl TagTypePlugin for CustPlugin {
        fn type_sig(&self) -> Signature {
            CUST_TYPE
        }
        fn read(&self, r: &mut dyn ProfileReaderDyn, _size: u32) -> Result<Tag> {
            // Drive the generic reader helpers through the dyn shim.
            let mut r = r;
            let n = r.read_u32()?;
            Ok(Tag::Custom(CustomTagData {
                type_sig: CUST_TYPE,
                data: std::sync::Arc::new(MyVal(n)),
            }))
        }
        fn write(&self, w: &mut dyn ProfileWriterDyn, tag: &Tag) -> Result<()> {
            let Tag::Custom(c) = tag else {
                return Err(crate::error::Error::Unsupported("not a custom tag"));
            };
            let v = c.data.downcast_ref::<MyVal>().expect("MyVal payload");
            let mut w = w;
            w.write_u32(v.0)
        }
    }

    fn cust_ctx() -> Context<'static> {
        let mut ctx = Context::new();
        ctx.register_tag_type(std::sync::Arc::new(CustPlugin));
        ctx.register_tag(
            MTAG,
            std::sync::Arc::new(TagDescriptor {
                supported_types: vec![CUST_TYPE],
                decide_type: |_v, _t| CUST_TYPE,
            }),
        );
        ctx
    }

    #[test]
    fn custom_tag_round_trips_through_serialize_and_read() {
        let ctx = cust_ctx();
        let mut p = WritableProfile::new(header());
        p.add_tag(
            MTAG,
            Tag::Custom(CustomTagData {
                type_sig: CUST_TYPE,
                data: std::sync::Arc::new(MyVal(0xDEAD_BEEF)),
            }),
        );

        let bytes = save_to_mem_in(&ctx, &p).expect("serialize with plugin");
        let prof = Profile::open(&bytes).expect("reopen");
        let tag = prof.read_tag_in(&ctx, MTAG).expect("read custom tag");

        match tag {
            Tag::Custom(c) => {
                assert_eq!(c.type_sig, CUST_TYPE);
                let v = c.data.downcast_ref::<MyVal>().expect("MyVal");
                assert_eq!(*v, MyVal(0xDEAD_BEEF));
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn read_tag_in_without_registration_rejects_custom_tag() {
        // Same bytes, but read with an EMPTY context: the tag sig has no builtin
        // descriptor and no registered descriptor, so it is rejected (BadType).
        let ctx = cust_ctx();
        let mut p = WritableProfile::new(header());
        p.add_tag(
            MTAG,
            Tag::Custom(CustomTagData {
                type_sig: CUST_TYPE,
                data: std::sync::Arc::new(MyVal(1)),
            }),
        );
        let bytes = save_to_mem_in(&ctx, &p).expect("serialize");
        let prof = Profile::open(&bytes).expect("reopen");
        assert!(
            prof.read_tag(MTAG).is_err(),
            "legacy read must not decode it"
        );
    }

    // ---- Differential bonus: a custom type reusing the builtin XYZ encoding. ----

    const XYZ2_TYPE: Signature = Signature::from_bytes(*b"xyz2");
    const XTAG: Signature = Signature::from_bytes(*b"xTg ");
    // A real builtin XYZ-valued tag, to obtain the reference 'XYZ ' bytes.
    const WTPT: Signature = Signature::from_bytes(*b"wtpt");

    struct Xyz2Plugin;
    impl TagTypePlugin for Xyz2Plugin {
        fn type_sig(&self) -> Signature {
            XYZ2_TYPE
        }
        fn read(&self, r: &mut dyn ProfileReaderDyn, _size: u32) -> Result<Tag> {
            let mut r = r;
            let xyz = r.read_xyz()?; // reuse the builtin s15Fixed16×3 decode.
            Ok(Tag::Custom(CustomTagData {
                type_sig: XYZ2_TYPE,
                data: std::sync::Arc::new(xyz),
            }))
        }
        fn write(&self, w: &mut dyn ProfileWriterDyn, tag: &Tag) -> Result<()> {
            let Tag::Custom(c) = tag else {
                return Err(crate::error::Error::Unsupported("not a custom tag"));
            };
            let xyz = c.data.downcast_ref::<CIEXYZ>().expect("CIEXYZ payload");
            let mut w = w;
            // Mirror Type_XYZ_Write exactly: three s15Fixed16.
            w.write_s15fixed16(xyz.x)?;
            w.write_s15fixed16(xyz.y)?;
            w.write_s15fixed16(xyz.z)
        }
    }

    #[test]
    fn custom_xyz_body_bytes_equal_builtin_xyz_body() {
        let xyz = CIEXYZ {
            x: 0.5,
            y: 0.25,
            z: 0.125,
        };

        // Plugin profile: custom 'xyz2' type under a custom tag.
        let mut ctx = Context::new();
        ctx.register_tag_type(std::sync::Arc::new(Xyz2Plugin));
        ctx.register_tag(
            XTAG,
            std::sync::Arc::new(TagDescriptor {
                supported_types: vec![XYZ2_TYPE],
                decide_type: |_v, _t| XYZ2_TYPE,
            }),
        );
        let mut pc = WritableProfile::new(header());
        pc.add_tag(
            XTAG,
            Tag::Custom(CustomTagData {
                type_sig: XYZ2_TYPE,
                data: std::sync::Arc::new(xyz),
            }),
        );
        let custom_bytes = save_to_mem_in(&ctx, &pc).expect("serialize custom");

        // Builtin profile: a real XYZ tag (wtpt) with the same value.
        let mut pb = WritableProfile::new(header());
        pb.add_tag(WTPT, Tag::Xyz(xyz));
        let builtin_bytes = save_to_mem(&pb).expect("serialize builtin");

        // Both single-tag profiles have identical header+directory layout, so the
        // tag bodies sit at the same offset. Extract each body (8-byte type base +
        // payload) and assert the PAYLOADS are byte-identical (only the 4-byte type
        // signature in the base differs: 'xyz2' vs 'XYZ ').
        let cust_entry = Profile::open(&custom_bytes)
            .unwrap()
            .tag_entry(XTAG)
            .copied()
            .expect("xTg entry");
        let bi_entry = Profile::open(&builtin_bytes)
            .unwrap()
            .tag_entry(WTPT)
            .copied()
            .expect("wtpt entry");
        assert_eq!(cust_entry.size, bi_entry.size, "tag sizes differ");

        let cust_body =
            &custom_bytes[cust_entry.offset as usize + 8..][..cust_entry.size as usize - 8];
        let bi_body = &builtin_bytes[bi_entry.offset as usize + 8..][..bi_entry.size as usize - 8];
        assert_eq!(cust_body, bi_body, "custom XYZ body != builtin XYZ body");

        // And the custom type base carries 'xyz2', confirming the plugin path ran.
        let base = &custom_bytes[cust_entry.offset as usize..][..4];
        assert_eq!(base, b"xyz2");
    }

    #[test]
    fn builtin_type_sig_cannot_be_shadowed() {
        // Register a plugin claiming the builtin 'XYZ ' type sig. A real XYZ tag
        // must STILL decode via the builtin (Tag::Xyz), never the plugin.
        struct Shadow;
        impl TagTypePlugin for Shadow {
            fn type_sig(&self) -> Signature {
                Signature::from_bytes(*b"XYZ ")
            }
            fn read(&self, _r: &mut dyn ProfileReaderDyn, _size: u32) -> Result<Tag> {
                panic!("builtin XYZ must not reach the plugin");
            }
            fn write(&self, _w: &mut dyn ProfileWriterDyn, _t: &Tag) -> Result<()> {
                panic!("builtin XYZ must not reach the plugin");
            }
        }
        let mut ctx = Context::new();
        ctx.register_tag_type(std::sync::Arc::new(Shadow));

        let mut p = WritableProfile::new(header());
        p.add_tag(
            WTPT,
            Tag::Xyz(CIEXYZ {
                x: 0.9642,
                y: 1.0,
                z: 0.8249,
            }),
        );
        // Write through the plugin-aware path: must use the builtin XYZ writer.
        let bytes = save_to_mem_in(&ctx, &p).expect("serialize");
        let prof = Profile::open(&bytes).expect("reopen");
        // The builtin XYZ reader (not the panicking plugin) decoded the body; the
        // s15Fixed16 round-trip quantizes 0.9642, so compare within tolerance.
        match prof.read_tag_in(&ctx, WTPT).expect("read wtpt") {
            Tag::Xyz(v) => assert!((v.x - 0.9642).abs() < 1e-4, "x={}", v.x),
            other => panic!("expected builtin Xyz, got {other:?}"),
        }
    }
}
