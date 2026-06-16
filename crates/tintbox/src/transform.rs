//! Color transforms (`cmsCreateTransform`/`cmsDoTransform`, lcms2 `cmsxform.c`).
//!
//! A [`Transform`] owns a device-link [`Pipeline`] (built by
//! [`default_icc_intents`](crate::link::default_icc_intents)) plus the entry/exit
//! color spaces, media white points, and rendering intent that
//! `cmsCreateExtendedTransform` records. [`Transform::do_transform_float`] is the
//! `FloatXFORM` per-pixel evaluation; [`Transform::do_transform_16`] is the 16-bit
//! `PrecalculatedXFORM`/`CachedXFORM` path. Pixel-format packing/unpacking is
//! deferred to slice 6, so the buffers here are flat float/u16 arrays.
//!
//! # What slice 5 defers (the differential boundary)
//!
//! The transform/link path is bit-exact against lcms2 (NOOPTIMIZE) over the
//! testbed sweep, except for three explicitly-deferred areas. A future reader
//! should not mistake these for missing features; they are scoped out of slice 5:
//!
//! 1. **Black-point detection-by-sampling.** The BPC matrix math
//!    ([`compute_black_point_compensation`](crate::link::compute_black_point_compensation))
//!    and the V4-perceptual-black *constant* path are implemented and exact. Every
//!    black-point that lcms2 resolves by *sampling* the profile is deferred and
//!    surfaces as [`Error::Unsupported`]: V4 matrix-shaper under
//!    perceptual/saturation (`BlackPointAsDarkerColorant`), V2 BPC, ink-output
//!    relative-colorimetric (`BlackPointUsingPerceptualBlack`), the
//!    media-black-point-tag path, and the destination round-trip detection. These
//!    need slice-7 Lab virtual profiles + round-trip transforms.
//!    (See [`crate::link::black_point`].)
//! 2. **Fractional adaptation state `0 < s < 1`.** Only the fully-adapted
//!    (`s = 1`) and unadapted (`s = 0`) endpoints are handled in
//!    `ComputeAbsoluteIntent`. The interpolated state needs `cmsTempFromWhitePoint`
//!    (blackbody-temperature estimation), deferred.
//! 3. **Pixel-format packing + pipeline optimization.** The flat float/u16
//!    constructors keep the slice-5 behavior. The format-aware constructors add
//!    the `cmsFormatter` packing/unpacking layer (slice 6) and a swappable
//!    [`OptimizationStrategy`](crate::opt::OptimizationStrategy): the DEFAULT
//!    `Accurate` is the unchanged in-place eval (bit-identical to lcms2
//!    `NOOPTIMIZE`); opt-in `Lcms2Compat` applies lcms2's matrix-shaper optimizer
//!    (`MatShaperEval16`) to match lcms2-default. The lossy resampling bake is a
//!    later task.

use crate::color::CIEXYZ;
use crate::context::Context;
use crate::format::{
    self, formatter_is_float, get_input_formatter, get_input_formatter_float, get_output_formatter,
    get_output_formatter_float, AlphaCopyPlan, PackFloatFn, PackFn, UnpackFloatFn, UnpackFn,
    MAX_CHANNELS,
};
use crate::link::{link_bpc_mutation, link_icc_intents_in};
use crate::math::whitepoint::D50;
use crate::opt::{OptimizationStrategy, OptimizedEval};
use crate::pipeline::{Pipeline, MAX_STAGE_CHANNELS};
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::{Error, Result};

/// Transform creation flags (lcms2 `cmsFLAGS_*`). Slice 5 only needs
/// `NOOPTIMIZE` (the unoptimized device-link pipeline is the differential
/// reference); the default build is already unoptimized, so the flag is recorded
/// but does not change behavior here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    bits: u32,
}

impl Flags {
    /// `cmsFLAGS_NOOPTIMIZE` (`0x0100`). Skip pipeline optimization.
    pub const NOOPTIMIZE: Flags = Flags { bits: 0x0100 };

    /// `cmsFLAGS_GAMUTCHECK` (`0x1000`, lcms2.h:1751). Mark out-of-gamut colors in
    /// the proofing transform with the alarm color.
    pub const GAMUTCHECK: Flags = Flags { bits: 0x1000 };

    /// `cmsFLAGS_SOFTPROOFING` (`0x4000`, lcms2.h:1754). Use the proofing profile
    /// as the destination preview (the proofing 4-profile chain).
    pub const SOFTPROOFING: Flags = Flags { bits: 0x4000 };

    /// `cmsFLAGS_BLACKPOINTCOMPENSATION` (`0x2000`, lcms2.h:1752).
    pub const BLACKPOINTCOMPENSATION: Flags = Flags { bits: 0x2000 };

    /// `cmsFLAGS_COPY_ALPHA` (`0x0400_0000`, lcms2.h:1773). Copy the extra
    /// (alpha) channels straight from input to output on `cmsDoTransform`,
    /// depth-converting but NOT color-transforming them.
    pub const COPY_ALPHA: Flags = Flags { bits: 0x0400_0000 };

    /// An empty flag set.
    pub const fn empty() -> Flags {
        Flags { bits: 0 }
    }

    /// Construct from a raw `dwFlags` word.
    pub const fn from_bits(bits: u32) -> Flags {
        Flags { bits }
    }

    /// The raw `dwFlags` word.
    pub const fn bits(self) -> u32 {
        self.bits
    }

    /// Whether `other`'s bits are all set.
    pub const fn contains(self, other: Flags) -> bool {
        (self.bits & other.bits) == other.bits
    }

    /// Set union.
    pub const fn union(self, other: Flags) -> Flags {
        Flags {
            bits: self.bits | other.bits,
        }
    }
}

/// The packing/unpacking layer (lcms2 `FromInput*`/`ToOutput*` + the format
/// words). Selected once at construction from the input/output `PixelFormat`s,
/// then used by [`Transform::do_transform`] for the format-aware pixel loop.
///
/// lcms2 keeps separate 16-bit and float formatter tables and picks the path via
/// `_cmsFormatterIsFloat`: if *either* the input or output format carries the
/// `T_FLOAT` bit, the whole transform uses `FloatXFORM` (both ends are pulled
/// from the float tables); otherwise the 16-bit `PrecalculatedXFORM` path runs.
struct Formatters {
    in_fmt: u32,
    out_fmt: u32,
    is_float: bool,
    // 16-bit path (is_float == false).
    from_input16: Option<UnpackFn>,
    to_output16: Option<PackFn>,
    // Float path (is_float == true).
    from_input_float: Option<UnpackFloatFn>,
    to_output_float: Option<PackFloatFn>,
    // Extra-channel (alpha) copy plan, set when `cmsFLAGS_COPY_ALPHA` is on and
    // both formats carry extra channels (lcms2 `_cmsHandleExtraChannels`). `None`
    // means extra output bytes are left as the packer wrote them.
    alpha_copy: Option<AlphaCopyPlan>,
}

/// A color transform: a device-link pipeline plus the recorded entry/exit color
/// spaces, media white points, and rendering intent (lcms2 `_cmsTRANSFORM`).
pub struct Transform {
    lut: Pipeline,
    entry_color_space: ColorSpace,
    exit_color_space: ColorSpace,
    entry_white_point: CIEXYZ,
    exit_white_point: CIEXYZ,
    rendering_intent: RenderingIntent,
    // Gamut-check pipeline (lcms2 `GamutCheck`). Built by the proofing constructor
    // ([`Transform::new_proofing`]); `None` otherwise.
    gamut_check: Option<Pipeline>,
    // Alarm colors substituted for out-of-gamut pixels (lcms2 per-context
    // `AlarmCodes`, default `{0x7F00, 0x7F00, 0x7F00, 0, ...}`).
    alarm_codes: [u16; MAX_CHANNELS],
    // Packing layer (None for the flat-buffer constructors that bypass formatters).
    formatters: Option<Formatters>,
    // The pipeline-optimization strategy (swappable; default Accurate).
    strategy: OptimizationStrategy,
    // The optimized eval the do_transform loop calls, built from `strategy` once
    // the formats are known. `Pipeline` (the accurate in-place eval) until a
    // format-aware constructor with a firing optimizer rebuilds it.
    opt_eval: OptimizedEval,
}

/// lcms2 `NormalizeXYZ` (`cmsxform.c:1090-1101`): some profiles store the media
/// white × 100; divide by 10 until all components fall below 2.
fn normalize_xyz(mut wp: CIEXYZ) -> CIEXYZ {
    while wp.x > 2.0 && wp.y > 2.0 && wp.z > 2.0 {
        wp.x /= 10.0;
        wp.y /= 10.0;
        wp.z /= 10.0;
    }
    wp
}

const SIG_MEDIA_WHITE_POINT: crate::sig::Signature = crate::sig::Signature::from_raw(0x7774_7074);

/// `DEFAULT_ALARM_CODES_VALUE` (`cmsxform.c:87`): the alarm color used for
/// out-of-gamut pixels by the gamut-check path.
const DEFAULT_ALARM_CODES: [u16; MAX_CHANNELS] = {
    let mut a = [0u16; MAX_CHANNELS];
    a[0] = 0x7F00;
    a[1] = 0x7F00;
    a[2] = 0x7F00;
    a
};

/// lcms2 `SetWhitePoint` (`cmsxform.c:1103-1119`) applied to
/// `cmsReadTag(MediaWhitePoint)`. Note this is NOT `_cmsReadMediaWhitePoint`: it
/// reads the raw `wtpt` tag (no V2-display→D50 fallback) and, if present,
/// `NormalizeXYZ`-clamps it; if absent it yields D50.
fn transform_white_point(profile: &Profile) -> CIEXYZ {
    match profile.read_tag(SIG_MEDIA_WHITE_POINT) {
        Ok(crate::profile::Tag::Xyz(xyz)) => normalize_xyz(xyz),
        _ => D50,
    }
}

/// lcms2 `GetXFormColorSpaces` (`cmsxform.c:1016-1065`): the chain's entry color
/// space (first profile's input direction) and exit color space (last profile's
/// output direction), walking the same input/output direction logic the link
/// chain uses.
fn xform_color_spaces(profiles: &[&Profile]) -> Result<(ColorSpace, ColorSpace)> {
    if profiles.is_empty() {
        return Err(Error::Range);
    }

    let mut input = profiles[0].header().color_space;
    let mut post_color_space = profiles[0].header().color_space;

    for (i, profile) in profiles.iter().enumerate() {
        let l_is_input = post_color_space != ColorSpace::XYZ && post_color_space != ColorSpace::Lab;

        let cls = profile.header().device_class;

        let (cs_in, cs_out) = if cls == ProfileClass::NamedColor {
            let out = if profiles.len() > 1 {
                profile.header().pcs
            } else {
                profile.header().color_space
            };
            // cmsSig1colorData = '1CLR'.
            (ColorSpace::Color1, out)
        } else if l_is_input || cls == ProfileClass::Link {
            (profile.header().color_space, profile.header().pcs)
        } else {
            (profile.header().pcs, profile.header().color_space)
        };

        if i == 0 {
            input = cs_in;
        }
        post_color_space = cs_out;
    }

    Ok((input, post_color_space))
}

/// Select the input/output formatters for `in_fmt`/`out_fmt`, choosing the float
/// vs 16-bit path per lcms2 `_cmsFormatterIsFloat` (float if either end is float).
///
/// When `flags` carries `cmsFLAGS_COPY_ALPHA` and both formats have extra
/// channels, an [`AlphaCopyPlan`] is built so `do_transform` copies the extra
/// channels across (lcms2 `_cmsHandleExtraChannels`); otherwise no alpha copy is
/// performed.
fn select_formatters(in_fmt: u32, out_fmt: u32, flags: Flags) -> Result<Formatters> {
    let is_float = formatter_is_float(in_fmt) || formatter_is_float(out_fmt);

    // lcms2 `_cmsHandleExtraChannels` runs only with cmsFLAGS_COPY_ALPHA set;
    // `AlphaCopyPlan::build` itself returns None unless both ends have extras.
    let alpha_copy = if flags.contains(Flags::COPY_ALPHA) {
        AlphaCopyPlan::build(in_fmt, out_fmt)
    } else {
        None
    };

    if is_float {
        let from_input_float = get_input_formatter_float(in_fmt)
            .ok_or(Error::Unsupported("input pixel format not supported"))?;
        let to_output_float = get_output_formatter_float(out_fmt)
            .ok_or(Error::Unsupported("output pixel format not supported"))?;
        Ok(Formatters {
            in_fmt,
            out_fmt,
            is_float,
            from_input16: None,
            to_output16: None,
            from_input_float: Some(from_input_float),
            to_output_float: Some(to_output_float),
            alpha_copy,
        })
    } else {
        let from_input16 = get_input_formatter(in_fmt)
            .ok_or(Error::Unsupported("input pixel format not supported"))?;
        let to_output16 = get_output_formatter(out_fmt)
            .ok_or(Error::Unsupported("output pixel format not supported"))?;
        Ok(Formatters {
            in_fmt,
            out_fmt,
            is_float,
            from_input16: Some(from_input16),
            to_output16: Some(to_output16),
            from_input_float: None,
            to_output_float: None,
            alpha_copy,
        })
    }
}

/// Bytes one packed pixel of `fmt` occupies: `(channels + extra) * bytes`, where
/// `bytes` is `T_BYTES` (1/2/4) or 8 for double (`T_BYTES == 0`), matching lcms2
/// `PixelSize` × the per-pixel sample count.
fn pixel_bytes(fmt: u32) -> usize {
    let f = format::PixelFormat(fmt);
    let sample = match f.bytes() {
        0 => 8, // double
        b => b as usize,
    };
    (f.channels() + f.extra()) as usize * sample
}

impl Transform {
    /// lcms2 `cmsCreateExtendedTransform` (the device-link build), builtin path.
    /// Delegates to [`Transform::new_in`] with an empty [`Context`], so existing
    /// callers (and every differential test) hit the builtin
    /// [`default_icc_intents`](crate::link::default_icc_intents) link verbatim —
    /// no plugin dispatch.
    pub fn new(
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: Flags,
    ) -> Result<Transform> {
        Transform::new_in(&Context::new(), profiles, intents, bpc, adaptation, flags)
    }

    /// lcms2 `cmsCreateExtendedTransform` (the device-link build), dispatching the
    /// link through `ctx`'s [`RenderingIntentPlugin`](crate::plugin::RenderingIntentPlugin)
    /// registry. Applies the `_cmsLinkProfiles` BPC-array mutation (a copy — the
    /// caller's `bpc` is not touched), builds the link via
    /// [`link_icc_intents_in`](crate::link::link_icc_intents_in) (which runs the
    /// builtin [`default_icc_intents`](crate::link::default_icc_intents) unless a
    /// registered custom intent in the chain claims it), and records the entry/exit
    /// color spaces, media white points, and rendering intent.
    pub fn new_in(
        ctx: &Context,
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: Flags,
    ) -> Result<Transform> {
        let n = profiles.len();
        // cmsxform.c:1140: 1..255 profiles.
        if n == 0 || n > 255 {
            return Err(Error::Range);
        }
        assert_eq!(intents.len(), n, "intents length must match profiles");
        assert_eq!(bpc.len(), n, "bpc length must match profiles");
        assert_eq!(adaptation.len(), n, "adaptation length must match profiles");

        let last_intent = intents[n - 1];

        // Entry/exit spaces (cmsxform.c:1168, GetXFormColorSpaces).
        let (entry_color_space, exit_color_space) = xform_color_spaces(profiles)?;

        // Apply the _cmsLinkProfiles BPC mutation on a private copy (cmscnvrt.c:
        // 1137-1145, invoked inside _cmsLinkProfiles before the chain is built).
        let mut bpc_mut = bpc.to_vec();
        link_bpc_mutation(intents, profiles, &mut bpc_mut);

        // Build the device-link pipeline (cmsxform.c:1194, _cmsLinkProfiles),
        // dispatching to a registered custom intent plugin if the chain requests
        // one; otherwise the builtin `default_icc_intents`.
        let lut = link_icc_intents_in(ctx, profiles, intents, &bpc_mut, adaptation, flags.bits())?;

        // White points (cmsxform.c:1221-1222, SetWhitePoint over cmsReadTag).
        let entry_white_point = transform_white_point(profiles[0]);
        let exit_white_point = transform_white_point(profiles[n - 1]);

        Ok(Transform {
            lut,
            entry_color_space,
            exit_color_space,
            entry_white_point,
            exit_white_point,
            // cmsxform.c:1218: RenderingIntent = Intents[nProfiles-1].
            rendering_intent: last_intent,
            gamut_check: None,
            alarm_codes: DEFAULT_ALARM_CODES,
            formatters: None,
            strategy: OptimizationStrategy::Accurate,
            opt_eval: OptimizedEval::Pipeline,
        })
    }

    /// Like [`Transform::new`], but also selects and stores the input/output
    /// pixel formatters from the in/out `PixelFormat` words so the resulting
    /// transform can run [`Transform::do_transform`] over packed byte buffers.
    ///
    /// The path is chosen exactly as lcms2 (`_cmsFormatterIsFloat`): if either
    /// `in_fmt` or `out_fmt` is a float format, the float (`FloatXFORM`) path is
    /// selected and both formatters are pulled from the float tables; otherwise
    /// the 16-bit path is used. Returns [`Error::Unsupported`] if a formatter for
    /// either format is not available (e.g. planar/premul/half — later tasks).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_formats(
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: Flags,
        in_fmt: u32,
        out_fmt: u32,
    ) -> Result<Transform> {
        Transform::new_with_formats_strategy(
            profiles,
            intents,
            bpc,
            adaptation,
            flags,
            in_fmt,
            out_fmt,
            OptimizationStrategy::Accurate,
        )
    }

    /// Like [`Transform::new_with_formats`] but choosing the pipeline
    /// [`OptimizationStrategy`] explicitly. With
    /// [`OptimizationStrategy::Lcms2Compat`] the matrix-shaper optimizer is built
    /// against the in/out formats (firing only for the RGB 8-bit-input
    /// matrix-shaper pattern; otherwise it falls back to the accurate in-place
    /// eval). [`Accurate`](OptimizationStrategy::Accurate) always evaluates the
    /// pipeline in place.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_formats_strategy(
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: Flags,
        in_fmt: u32,
        out_fmt: u32,
        strategy: OptimizationStrategy,
    ) -> Result<Transform> {
        // No-`ctx` wrapper (the plugin `*_in` convention): the empty default
        // context carries no custom optimizer, so this hits the builtin strategy
        // path verbatim — every existing caller and differential test is unchanged.
        Transform::new_with_formats_strategy_in(
            &crate::context::Context::new(),
            profiles,
            intents,
            bpc,
            adaptation,
            flags,
            in_fmt,
            out_fmt,
            strategy,
        )
    }

    /// Convenience 2-profile format-aware constructor (lcms2 `cmsCreateTransform`
    /// with explicit format words). Forces `NOOPTIMIZE`, default adaptation 1.0.
    pub fn new_simple_with_formats(
        input: &Profile,
        output: &Profile,
        intent: RenderingIntent,
        bpc: bool,
        in_fmt: u32,
        out_fmt: u32,
    ) -> Result<Transform> {
        Transform::new_with_formats(
            &[input, output],
            &[intent, intent],
            &[bpc, bpc],
            &[1.0, 1.0],
            Flags::NOOPTIMIZE,
            in_fmt,
            out_fmt,
        )
    }

    /// Convenience 2-profile format-aware constructor passing explicit `flags`
    /// (lcms2 `cmsCreateTransform`). Default adaptation 1.0, same `intent`/`bpc`
    /// on both links. Use this to set `cmsFLAGS_COPY_ALPHA` (the extra-channel
    /// copy) — e.g. `Flags::NOOPTIMIZE.union(Flags::COPY_ALPHA)`.
    pub fn new_simple_with_formats_flags(
        input: &Profile,
        output: &Profile,
        intent: RenderingIntent,
        bpc: bool,
        in_fmt: u32,
        out_fmt: u32,
        flags: Flags,
    ) -> Result<Transform> {
        Transform::new_with_formats(
            &[input, output],
            &[intent, intent],
            &[bpc, bpc],
            &[1.0, 1.0],
            flags,
            in_fmt,
            out_fmt,
        )
    }

    /// Convenience 2-profile format-aware constructor choosing the
    /// [`OptimizationStrategy`] (lcms2 `cmsCreateTransform` with explicit format
    /// words and a chosen optimizer). Default adaptation 1.0, BPC `bpc`.
    ///
    /// Optimization is driven SOLELY by the [`OptimizationStrategy`], never by a
    /// build flag: `default_icc_intents` always runs the lcms2 `PreOptimize`
    /// matrix-merge at link time, and `cmsFLAGS_NOOPTIMIZE` is inert in tintbox (the
    /// linker ignores it). `Accurate` evaluates the linked pipeline in place;
    /// `Lcms2Compat` applies the matrix-shaper / resampling optimizers on top.
    pub fn new_simple_with_formats_strategy(
        input: &Profile,
        output: &Profile,
        intent: RenderingIntent,
        bpc: bool,
        in_fmt: u32,
        out_fmt: u32,
        strategy: OptimizationStrategy,
    ) -> Result<Transform> {
        Transform::new_with_formats_strategy(
            &[input, output],
            &[intent, intent],
            &[bpc, bpc],
            &[1.0, 1.0],
            Flags::NOOPTIMIZE,
            in_fmt,
            out_fmt,
            strategy,
        )
    }

    /// The pipeline-optimization strategy this transform was built with.
    pub fn strategy(&self) -> OptimizationStrategy {
        self.strategy
    }

    /// Whether the matrix-shaper optimizer actually fired for this transform
    /// (true only under [`OptimizationStrategy::Lcms2Compat`] when the RGB 8-bit
    /// matrix-shaper pattern matched).
    pub fn matshaper_fired(&self) -> bool {
        matches!(self.opt_eval, OptimizedEval::MatShaper(_))
    }

    /// A label naming which lcms2 optimizer produced the installed 16-bit eval —
    /// for diagnostics in the differential tests. One of `"pipeline"`,
    /// `"matshaper"`, `"curves"` (`OptimizeByJoiningCurves`), or `"baked"`
    /// (`OptimizeByComputingLinearization` / `OptimizeByResampling`).
    pub fn opt_path_label(&self) -> &'static str {
        match &self.opt_eval {
            OptimizedEval::Pipeline => "pipeline",
            OptimizedEval::MatShaper(_) => "matshaper",
            OptimizedEval::Curves(_) => "curves",
            OptimizedEval::Baked(_) => "baked",
            OptimizedEval::LosslessMatShaper(_) => "lossless-matshaper",
            OptimizedEval::Batched(_) => "batched",
        }
    }

    /// Whether the LOSSLESS BATCHED general/CLUT fast path fired for this transform
    /// (true only under [`OptimizationStrategy::AccurateFast`] for a general/CLUT
    /// pipeline the matrix-shaper fast path does not cover).
    pub fn batched_fired(&self) -> bool {
        matches!(self.opt_eval, OptimizedEval::Batched(_))
    }

    /// Whether the batched fast path built the pure u16-domain chain (the big
    /// lossless lever): the 16-bit eval runs wholly in u16, no f32 round-trips.
    pub fn batched_uses_u16_chain(&self) -> bool {
        matches!(&self.opt_eval, OptimizedEval::Batched(b) if b.uses_u16_chain())
    }

    /// Whether the LOSSLESS matrix-shaper fast path fired for this transform
    /// (true only under [`OptimizationStrategy::AccurateFast`] when the RGB 8-bit
    /// matrix-shaper shape matched).
    pub fn lossless_matshaper_fired(&self) -> bool {
        matches!(self.opt_eval, OptimizedEval::LosslessMatShaper(_))
    }

    /// Convenience 2-profile constructor (lcms2 `cmsCreateTransform`, which routes
    /// through `cmsCreateMultiprofileTransform`: each link gets `intent`, `bpc`,
    /// and the default adaptation state of `1.0`). Forces `NOOPTIMIZE`.
    pub fn new_simple(
        input: &Profile,
        output: &Profile,
        intent: RenderingIntent,
        bpc: bool,
    ) -> Result<Transform> {
        Transform::new(
            &[input, output],
            &[intent, intent],
            &[bpc, bpc],
            &[1.0, 1.0],
            Flags::NOOPTIMIZE,
        )
    }

    /// The device-link pipeline.
    pub fn lut(&self) -> &Pipeline {
        &self.lut
    }

    /// The chain's entry color space (first profile, input direction).
    pub fn entry_color_space(&self) -> ColorSpace {
        self.entry_color_space
    }

    /// The chain's exit color space (last profile, output direction).
    pub fn exit_color_space(&self) -> ColorSpace {
        self.exit_color_space
    }

    /// The first profile's media white point (`SetWhitePoint`-normalized).
    pub fn entry_white_point(&self) -> CIEXYZ {
        self.entry_white_point
    }

    /// The last profile's media white point (`SetWhitePoint`-normalized).
    pub fn exit_white_point(&self) -> CIEXYZ {
        self.exit_white_point
    }

    /// The transform's rendering intent (last link's intent).
    pub fn rendering_intent(&self) -> RenderingIntent {
        self.rendering_intent
    }

    /// The gamut-check pipeline, if any (always `None` in slice 5).
    pub fn gamut_check(&self) -> Option<&Pipeline> {
        self.gamut_check.as_ref()
    }

    /// lcms2 `cmsGetNamedColorList` (cmsnamed.c:975): the named-color list of a
    /// named-color transform. lcms2 returns it only when the *first* pipeline
    /// stage is a `cmsSigNamedColorElemType`; we mirror that, returning `None`
    /// for any non-named transform. (Slice9-named: the named-color transform
    /// path.)
    pub fn named_color_list(&self) -> Option<&crate::named::NamedColorList> {
        match self.lut.stages().first() {
            Some(crate::pipeline::Stage::NamedColor { list, .. }) => Some(list),
            _ => None,
        }
    }

    /// lcms2 `FloatXFORM` (`cmsxform.c:258-322`): for each of `n_pixels`, read
    /// `in_channels` floats, `eval_float`, write `out_channels` floats. No
    /// pixel-format packing (slice 6). `input` is `n_pixels * in_channels` f32;
    /// `output` is `n_pixels * out_channels` f32.
    pub fn do_transform_float(&self, input: &[f32], output: &mut [f32], n_pixels: usize) {
        let in_ch = self.lut.input_channels;
        let out_ch = self.lut.output_channels;
        assert_eq!(input.len(), n_pixels * in_ch, "input buffer size mismatch");
        assert_eq!(
            output.len(),
            n_pixels * out_ch,
            "output buffer size mismatch"
        );

        // One empty `Context` hoisted above the per-pixel loop (see `do_transform`).
        let ctx = Context::new();
        // Reusable per-pixel eval output buffer + gamut-marker buffer, allocated
        // ONCE here and overwritten per pixel — the `_into` evals write straight
        // into these, so the per-pixel `eval_*` no longer heap-allocates a `Vec`.
        let mut res = [0f32; MAX_STAGE_CHANNELS];
        let mut gout = [0f32; MAX_STAGE_CHANNELS];
        for i in 0..n_pixels {
            let pix = &input[i * in_ch..i * in_ch + in_ch];
            // Gamut check (lcms2 `FloatXFORM`, cmsxform.c:288-312): all channels get
            // the float-scaled alarm color when the marker is `> 0.0`.
            if let Some(gamut) = &self.gamut_check {
                let g_in = gamut.input_channels;
                let mut padded = [0f32; MAX_CHANNELS];
                padded[..in_ch].copy_from_slice(pix);
                gamut.eval_float_in_into(&ctx, &padded[..g_in], &mut gout);
                if gout[0] > 0.0 {
                    for c in 0..out_ch {
                        output[i * out_ch + c] = self.alarm_codes[c] as f32 / 65535.0_f32;
                    }
                    continue;
                }
            }
            self.lut.eval_float_in_into(&ctx, pix, &mut res);
            output[i * out_ch..i * out_ch + out_ch].copy_from_slice(&res[..out_ch]);
        }
    }

    /// lcms2 16-bit transform (`PrecalculatedXFORM`/`CachedXFORM`,
    /// `cmsxform.c:403-555`): per pixel `eval_16`. The 1-pixel cache is
    /// value-neutral and omitted (§8.8). `input` is `n_pixels * in_channels` u16;
    /// `output` is `n_pixels * out_channels` u16.
    pub fn do_transform_16(&self, input: &[u16], output: &mut [u16], n_pixels: usize) {
        let in_ch = self.lut.input_channels;
        let out_ch = self.lut.output_channels;
        assert_eq!(input.len(), n_pixels * in_ch, "input buffer size mismatch");
        assert_eq!(
            output.len(),
            n_pixels * out_ch,
            "output buffer size mismatch"
        );

        // One empty `Context` hoisted above the per-pixel loop (see `do_transform`).
        let ctx = Context::new();
        // Reusable per-pixel eval output buffer + gamut-marker buffer, allocated
        // ONCE here and overwritten per pixel — the `_into` evals write straight
        // into these, so the per-pixel `eval_*` no longer heap-allocates a `Vec`.
        let mut res = [0u16; MAX_STAGE_CHANNELS];
        let mut gout = [0u16; MAX_STAGE_CHANNELS];
        for i in 0..n_pixels {
            let pix = &input[i * in_ch..i * in_ch + in_ch];
            // Gamut check (lcms2 `TransformOnePixelWithGamutCheck`,
            // cmsxform.c:443-462): marker `>= 1` ⇒ emit the alarm color.
            if let Some(gamut) = &self.gamut_check {
                let g_in = gamut.input_channels;
                let mut padded = [0u16; MAX_CHANNELS];
                padded[..in_ch].copy_from_slice(pix);
                gamut.eval_16_in_into(&ctx, &padded[..g_in], &mut gout);
                if gout[0] >= 1 {
                    output[i * out_ch..i * out_ch + out_ch]
                        .copy_from_slice(&self.alarm_codes[..out_ch]);
                    continue;
                }
            }
            self.lut.eval_16_in_into(&ctx, pix, &mut res);
            output[i * out_ch..i * out_ch + out_ch].copy_from_slice(&res[..out_ch]);
        }
    }

    /// The LOSSLESS BATCHED general/CLUT eval over packed byte buffers
    /// ([`OptimizedEval::Batched`], `AccurateFast`). Unpacks a CHUNK of pixels into
    /// a contiguous channel buffer, runs the batched stage-by-stage eval (CLUT
    /// interpolator resolved once + memoized 8-bit input curves), and packs the
    /// chunk back out. Byte-for-byte identical to the per-pixel
    /// `eval_16`/`eval_float` Pipeline path — only the loop nesting + cached
    /// build-time work differ. Extra (alpha) channels are copied exactly as the
    /// per-pixel path (`_cmsHandleExtraChannels`).
    fn do_transform_batched(
        &self,
        fmts: &Formatters,
        batched: &crate::opt::batched::BatchedPipeline,
        input: &[u8],
        output: &mut [u8],
        n_pixels: usize,
    ) {
        // Pixels per unpack/eval/pack tile. Matches the eval's internal CHUNK so
        // the whole pipeline stays in cache; the eval itself re-tiles internally.
        const TILE: usize = 8192;

        let in_ch = self.lut.input_channels;
        let out_ch = self.lut.output_channels;
        let in_stride = pixel_bytes(fmts.in_fmt);
        let out_stride = pixel_bytes(fmts.out_fmt);

        // Size the per-call tile to the ACTUAL work, not the full TILE. A small call
        // (the batched path only fires at all for n_pixels >= BATCHED_THRESHOLD, but
        // that threshold is well below TILE) must not allocate+zero a full
        // TILE-wide channel buffer + scratch — that per-call overhead is exactly
        // what made AccurateFast catastrophic for small calls. `tile` pixels of
        // scratch are allocated; the eval re-tiles internally at `min(CHUNK, tile)`.
        let tile = n_pixels.min(TILE);

        // The batched eval's ping-pong scratch, allocated ONCE here (right-sized to
        // `tile`) and reused across every tile. This removes the per-tile
        // `vec![0; CHUNK*MAX_STAGE_CHANNELS]` zeroing that otherwise dominates the
        // profile as `__bzero`.
        let mut eval_scratch = crate::opt::batched::BatchedScratch::with_capacity(tile);

        if fmts.is_float {
            let from_input = fmts.from_input_float.as_ref().unwrap();
            let to_output = fmts.to_output_float.as_ref().unwrap();
            // Contiguous channel scratch for one tile (in/out widths).
            let mut chan_in = vec![0f32; tile * in_ch];
            let mut chan_out = vec![0f32; tile * out_ch];
            // Per-pixel unpack target (the formatter writes MAX_CHANNELS slots).
            let mut fin = [0f32; MAX_CHANNELS];

            let mut base = 0usize;
            while base < n_pixels {
                let m = (n_pixels - base).min(tile);
                // Unpack the tile into the contiguous channel buffer.
                for p in 0..m {
                    let in_pixel =
                        &input[(base + p) * in_stride..(base + p) * in_stride + in_stride];
                    from_input(in_pixel, &mut fin);
                    chan_in[p * in_ch..p * in_ch + in_ch].copy_from_slice(&fin[..in_ch]);
                }
                // Batched eval (identical to per-pixel eval_float), reusing scratch.
                batched.eval_float_buffer_with(
                    &chan_in[..m * in_ch],
                    &mut chan_out[..m * out_ch],
                    m,
                    &mut eval_scratch,
                );
                // Pack the tile back out (padding the packer's MAX_CHANNELS input).
                let mut fout = [0f32; MAX_CHANNELS];
                for p in 0..m {
                    fout[..out_ch].copy_from_slice(&chan_out[p * out_ch..p * out_ch + out_ch]);
                    let out_pixel =
                        &mut output[(base + p) * out_stride..(base + p) * out_stride + out_stride];
                    to_output(&fout, out_pixel);
                    if let Some(plan) = &fmts.alpha_copy {
                        let in_pixel =
                            &input[(base + p) * in_stride..(base + p) * in_stride + in_stride];
                        let out_pixel = &mut output
                            [(base + p) * out_stride..(base + p) * out_stride + out_stride];
                        plan.copy_pixel(in_pixel, out_pixel);
                    }
                }
                base += m;
            }
        } else {
            let from_input = fmts.from_input16.as_ref().unwrap();
            let to_output = fmts.to_output16.as_ref().unwrap();
            let mut chan_in = vec![0u16; tile * in_ch];
            let mut chan_out = vec![0u16; tile * out_ch];
            let mut win = [0u16; MAX_CHANNELS];

            let mut base = 0usize;
            while base < n_pixels {
                let m = (n_pixels - base).min(tile);
                for p in 0..m {
                    let in_pixel =
                        &input[(base + p) * in_stride..(base + p) * in_stride + in_stride];
                    from_input(in_pixel, &mut win);
                    chan_in[p * in_ch..p * in_ch + in_ch].copy_from_slice(&win[..in_ch]);
                }
                batched.eval_16_buffer_with(
                    &chan_in[..m * in_ch],
                    &mut chan_out[..m * out_ch],
                    m,
                    &mut eval_scratch,
                );
                let mut wout = [0u16; MAX_CHANNELS];
                for p in 0..m {
                    wout[..out_ch].copy_from_slice(&chan_out[p * out_ch..p * out_ch + out_ch]);
                    let out_pixel =
                        &mut output[(base + p) * out_stride..(base + p) * out_stride + out_stride];
                    to_output(&wout, out_pixel);
                    if let Some(plan) = &fmts.alpha_copy {
                        let in_pixel =
                            &input[(base + p) * in_stride..(base + p) * in_stride + in_stride];
                        let out_pixel = &mut output
                            [(base + p) * out_stride..(base + p) * out_stride + out_stride];
                        plan.copy_pixel(in_pixel, out_pixel);
                    }
                }
                base += m;
            }
        }
    }

    /// Format-aware transform over packed byte buffers (lcms2 `cmsDoTransform`):
    /// for each of `n_pixels`, unpack one packed pixel via the stored input
    /// formatter → evaluate the pipeline → pack one packed pixel via the output
    /// formatter. Requires the transform to have been built with
    /// [`Transform::new_with_formats`] / [`Transform::new_simple_with_formats`].
    ///
    /// The float-vs-16-bit path is the one selected at construction
    /// (`_cmsFormatterIsFloat`): the float path mirrors `FloatXFORM`
    /// (unpack→`eval_float`→pack), the 16-bit path mirrors `PrecalculatedXFORM`
    /// (unpack→`eval_16`→pack). Contiguous (chunky) buffers only; stride/planar
    /// is deferred. The eval call stays abstract (the existing pipeline eval) — no
    /// optimization is inlined here (that is a later swappable strategy).
    ///
    /// # Panics
    /// Panics if the transform was not built with formatters, or if `input` /
    /// `output` are too small for `n_pixels` packed pixels of the in/out formats.
    /// Minimum `n_pixels` for a `do_transform` call to use the batched general/CLUT
    /// fast path ([`Transform::do_transform_batched`]).
    ///
    /// Below this width a call routes to the per-pixel `eval_16`/`eval_float` (the
    /// bit-identical Accurate path), so a small call (a renderer's scanline/tile/
    /// single pixel) never pays the batched per-call setup (right-sized scratch
    /// alloc + unpack/pack bookkeeping). MEASURED via `examples/profile_transform`'s
    /// chunk-size sweep: at and above this width AccurateFast (batched) is `>=`
    /// Accurate at every chunk size while still winning big on large buffers; below
    /// it the per-pixel fallback is at parity with Accurate (it IS the Accurate
    /// path). 256 is the smallest power-of-two tile that clears Accurate's
    /// throughput in the sweep with margin.
    const BATCHED_THRESHOLD: usize = 256;

    pub fn do_transform(&self, input: &[u8], output: &mut [u8], n_pixels: usize) {
        let fmts = self
            .formatters
            .as_ref()
            .expect("do_transform requires a transform built with formats (new_with_formats)");

        let in_ch = self.lut.input_channels;
        let out_ch = self.lut.output_channels;
        let in_stride = pixel_bytes(fmts.in_fmt);
        let out_stride = pixel_bytes(fmts.out_fmt);
        assert!(
            input.len() >= n_pixels * in_stride,
            "input buffer too small"
        );
        assert!(
            output.len() >= n_pixels * out_stride,
            "output buffer too small"
        );

        // LOSSLESS BATCHED general/CLUT fast path (AccurateFast). Byte-for-byte
        // identical to the per-pixel Pipeline eval, but unpacks/evaluates/packs in
        // CHUNKS so the CLUT interpolator + input curves are resolved once and the
        // intermediate buffers stay cache-resident. Only taken when there is no
        // gamut-check pipeline (batched never fires for proofing transforms).
        // The batched machinery (right-sized scratch alloc + unpack-into-contiguous-
        // buffer + stage-by-stage eval + pack) only pays off once a call processes
        // enough pixels to amortize its per-call setup. Below BATCHED_THRESHOLD a
        // call routes to the per-pixel `eval_16`/`eval_float` (the bit-identical
        // Accurate path, reached via the `Batched(_)` arms in the loops below), so a
        // small call (a scanline/tile/single pixel from a renderer) never pays the
        // batched setup. Measured so AccurateFast >= Accurate at every chunk size
        // (see crates/tintbox/benches/transform.rs and examples/profile_transform).
        if self.gamut_check.is_none() && n_pixels >= Self::BATCHED_THRESHOLD {
            if let OptimizedEval::Batched(batched) = &self.opt_eval {
                self.do_transform_batched(fmts, batched, input, output, n_pixels);
                return;
            }
        }

        // One empty `Context` hoisted ABOVE the per-pixel loop. The default
        // `Accurate` path evaluates the linked pipeline per pixel via `eval_*_in`;
        // threading this single context stops `Stage::eval`'s tone-curve arm from
        // constructing+dropping an empty `Context` per channel per pixel (measured
        // ~7-17% of a curve-heavy CMYK transform). An empty context routes every
        // curve through the builtin path, so output is byte-for-byte unchanged.
        let ctx = Context::new();

        // Reusable per-pixel eval output + gamut-marker buffers, allocated ONCE here
        // and overwritten per pixel — the `_into` evals write straight into these,
        // so the per-pixel `eval_*` no longer heap-allocates a `Vec`.
        let mut gout_f = [0f32; MAX_STAGE_CHANNELS];
        let mut gout_w = [0u16; MAX_STAGE_CHANNELS];

        if fmts.is_float {
            let from_input = fmts.from_input_float.as_ref().unwrap();
            let to_output = fmts.to_output_float.as_ref().unwrap();
            let mut fin = [0f32; MAX_CHANNELS];
            for i in 0..n_pixels {
                let in_pixel = &input[i * in_stride..i * in_stride + in_stride];
                let acc = in_pixel;
                from_input(acc, &mut fin);
                let mut fout = [0f32; MAX_CHANNELS];
                // lcms2 `FloatXFORM` gamut check (cmsxform.c:288-312): evaluate the
                // gamut marker; if `> 0.0`, fill ALL channels with the alarm color
                // scaled to float, else run the LUT normally.
                if let Some(gamut) = &self.gamut_check {
                    let g_in = gamut.input_channels;
                    gamut.eval_float_in_into(&ctx, &fin[..g_in], &mut gout_f);
                    if gout_f[0] > 0.0 {
                        for (slot, &code) in fout.iter_mut().zip(self.alarm_codes.iter()) {
                            *slot = code as f32 / 65535.0_f32;
                        }
                    } else {
                        self.lut.eval_float_in_into(&ctx, &fin[..in_ch], &mut fout);
                    }
                } else {
                    // Abstract eval (no inlined optimization — see module docs).
                    self.lut.eval_float_in_into(&ctx, &fin[..in_ch], &mut fout);
                }
                {
                    let out = &mut output[i * out_stride..i * out_stride + out_stride];
                    to_output(&fout, out);
                }
                // lcms2 `_cmsHandleExtraChannels`: copy the extra channels from the
                // ORIGINAL input pixel to the output pixel, depth-converting only.
                if let Some(plan) = &fmts.alpha_copy {
                    let in_pixel = &input[i * in_stride..i * in_stride + in_stride];
                    let out_pixel = &mut output[i * out_stride..i * out_stride + out_stride];
                    plan.copy_pixel(in_pixel, out_pixel);
                }
            }
        } else {
            let from_input = fmts.from_input16.as_ref().unwrap();
            let to_output = fmts.to_output16.as_ref().unwrap();
            let mut win = [0u16; MAX_CHANNELS];
            for i in 0..n_pixels {
                let in_pixel = &input[i * in_stride..i * in_stride + in_stride];
                from_input(in_pixel, &mut win);
                let mut wout = [0u16; MAX_CHANNELS];
                // lcms2 `TransformOnePixelWithGamutCheck` (cmsxform.c:443-462): if a
                // gamut-check pipeline is present, evaluate it on the (zero-extended)
                // input pixel; if the marker is `>= 1`, emit the alarm color instead
                // of running the LUT.
                if let Some(gamut) = &self.gamut_check {
                    let g_in = gamut.input_channels;
                    gamut.eval_16_in_into(&ctx, &win[..g_in], &mut gout_w);
                    if gout_w[0] >= 1 {
                        wout[..out_ch].copy_from_slice(&self.alarm_codes[..out_ch]);
                    } else {
                        self.lut.eval_16_in_into(&ctx, &win[..in_ch], &mut wout);
                    }
                } else {
                    match &self.opt_eval {
                        // lcms2 `MatShaperEval16`: the 1.14-fixed RGB matrix-shaper
                        // evaluator (Lcms2Compat). Replaces the float pipeline eval.
                        OptimizedEval::MatShaper(data) => {
                            let res = data.eval(&[win[0], win[1], win[2]]);
                            wout[..3].copy_from_slice(&res);
                        }
                        // lcms2 `FastEvaluateCurves8/16` / `FastIdentity16`
                        // (OptimizeByJoiningCurves).
                        OptimizedEval::Curves(c) => {
                            c.eval(&win[..in_ch], &mut wout[..out_ch]);
                        }
                        // lcms2 `PrelinEval16` / `PrelinEval8` (the baked CLUT from
                        // OptimizeByComputingLinearization / OptimizeByResampling).
                        OptimizedEval::Baked(b) => {
                            b.eval(&win[..in_ch], &mut wout[..out_ch]);
                        }
                        // The LOSSLESS matrix-shaper fast path (AccurateFast):
                        // byte-for-byte identical to the Pipeline arm, but the
                        // per-pixel input-curve eval is a table lookup.
                        OptimizedEval::LosslessMatShaper(data) => {
                            let res = data.eval(&[win[0], win[1], win[2]]);
                            wout[..3].copy_from_slice(&res);
                        }
                        // The accurate in-place pipeline eval. The Batched general
                        // fast path is handled by the early-return chunked loop in
                        // `do_transform`, so reaching it here (only possible with a
                        // gamut-check pipeline, which batched declines) falls back
                        // to the bit-identical in-place eval.
                        OptimizedEval::Pipeline | OptimizedEval::Batched(_) => {
                            self.lut.eval_16_in_into(&ctx, &win[..in_ch], &mut wout);
                        }
                    }
                }
                {
                    let out = &mut output[i * out_stride..i * out_stride + out_stride];
                    to_output(&wout, out);
                }
                // lcms2 `_cmsHandleExtraChannels`: copy the extra channels from the
                // ORIGINAL input pixel to the output pixel, depth-converting only.
                if let Some(plan) = &fmts.alpha_copy {
                    let in_pixel = &input[i * in_stride..i * in_stride + in_stride];
                    let out_pixel = &mut output[i * out_stride..i * out_stride + out_stride];
                    plan.copy_pixel(in_pixel, out_pixel);
                }
            }
        }
    }
}

// ============================================================================
// Proofing transform (lcms2 `cmsCreateProofingTransform`) + gamut-check wiring.
// Appended block (slice9-gamut): self-contained new constructors so the proofing
// path merges cleanly with the parallel named-color work on this file.
// ============================================================================

impl Transform {
    /// The transform's alarm colors (lcms2 `cmsGetAlarmCodes`).
    pub fn alarm_codes(&self) -> &[u16; MAX_CHANNELS] {
        &self.alarm_codes
    }

    /// Set the alarm colors substituted for out-of-gamut pixels (lcms2
    /// `cmsSetAlarmCodes`). Only meaningful for a proofing transform built with
    /// `cmsFLAGS_GAMUTCHECK`.
    pub fn set_alarm_codes(&mut self, codes: [u16; MAX_CHANNELS]) {
        self.alarm_codes = codes;
    }

    /// lcms2 `cmsCreateExtendedTransform` extended with a gamut-check profile +
    /// position (the general entry point the proofing constructor routes through).
    /// Builds the device-link as [`Transform::new_with_formats`] does, then — when
    /// `flags` carries `cmsFLAGS_GAMUTCHECK` and `gamut_profile` is `Some` — builds
    /// the gamut-check pipeline ([`crate::gamut::create_gamut_check_pipeline`]) from
    /// the ORIGINAL (un-BPC-mutated) `bpc`/`intents`/`adaptation` arrays, exactly as
    /// `cmsCreateExtendedTransform` passes them to `_cmsCreateGamutCheckPipeline`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_extended_with_formats(
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        gamut_profile: Option<&Profile>,
        n_gamut_pcs_position: usize,
        flags: Flags,
        in_fmt: u32,
        out_fmt: u32,
    ) -> Result<Transform> {
        // If gamut check is requested but no gamut profile, drop the flag
        // (cmsxform.c:1154-1156).
        let do_gamut = flags.contains(Flags::GAMUTCHECK) && gamut_profile.is_some();

        // Validate the gamut PCS position (cmsxform.c:1158-1161).
        let n = profiles.len();
        if do_gamut && (n_gamut_pcs_position == 0 || n_gamut_pcs_position >= n - 1) {
            return Err(Error::Range);
        }

        let mut xform = Transform::new_with_formats(
            profiles, intents, bpc, adaptation, flags, in_fmt, out_fmt,
        )?;

        if do_gamut {
            let gamut = gamut_profile.unwrap();
            xform.gamut_check = crate::gamut::create_gamut_check_pipeline(
                profiles,
                bpc,
                intents,
                adaptation,
                n_gamut_pcs_position,
                gamut,
            )?;
        }

        Ok(xform)
    }

    /// lcms2 `cmsCreateProofingTransform` (`cmsxform.c:1365-1394`). Builds the
    /// device → proof → proof → device chain with intents
    /// `[nIntent, nIntent, REL_COL, proofing_intent]`, BPC `[doBPC, doBPC, 0, 0]`
    /// (doBPC from `cmsFLAGS_BLACKPOINTCOMPENSATION`), adaptation all −1 (default
    /// state), and the proofing profile as the gamut profile at position 1.
    ///
    /// When neither `cmsFLAGS_SOFTPROOFING` nor `cmsFLAGS_GAMUTCHECK` is set, this
    /// degrades to a plain `input → output` transform (cmsxform.c:1388-1389).
    #[allow(clippy::too_many_arguments)]
    pub fn new_proofing(
        input: &Profile,
        in_fmt: u32,
        output: &Profile,
        out_fmt: u32,
        proofing: &Profile,
        n_intent: RenderingIntent,
        proofing_intent: RenderingIntent,
        flags: Flags,
    ) -> Result<Transform> {
        let do_bpc = flags.contains(Flags::BLACKPOINTCOMPENSATION);

        // Not soft-proofing nor gamut-check: plain input→output (cmsxform.c:1388).
        if !(flags.contains(Flags::SOFTPROOFING) || flags.contains(Flags::GAMUTCHECK)) {
            return Transform::new_with_formats(
                &[input, output],
                &[n_intent, n_intent],
                &[do_bpc, do_bpc],
                // cmsCreateMultiprofileTransform default adaptation 1.0.
                &[1.0, 1.0],
                flags,
                in_fmt,
                out_fmt,
            );
        }

        // The proofing chain (cmsxform.c:1382-1386).
        let profiles = [input, proofing, proofing, output];
        let intents = [
            n_intent,
            n_intent,
            RenderingIntent::RelativeColorimetric,
            proofing_intent,
        ];
        let bpc = [do_bpc, do_bpc, false, false];
        // cmsSetAdaptationStateTHR(ContextID, -1) returns the global default
        // adaptation state, which is 1.0 in stock lcms2 (matches the rest of tintbox).
        let adaptation = [1.0, 1.0, 1.0, 1.0];

        Transform::new_extended_with_formats(
            &profiles,
            &intents,
            &bpc,
            &adaptation,
            Some(proofing),
            1,
            flags,
            in_fmt,
            out_fmt,
        )
    }
}

// ============================================================================
// Optimization plugin wiring (lcms2 `cmsPluginOptimization`), slice8-opt (S8-T2).
// Appended block: a single context-carrying ctor so the custom-optimizer path
// merges cleanly alongside the parallel work on this file.
// ============================================================================

impl Transform {
    /// Like [`Transform::new_with_formats_strategy`], but takes the plugin
    /// [`Context`](crate::context::Context) as its first parameter (the slice-8
    /// `*_in` convention) so a custom pipeline optimizer registered via
    /// [`Context::set_optimizer`](crate::context::Context::set_optimizer) is
    /// consulted.
    ///
    /// The optimizer is queried FIRST (lcms2 `_cmsOptimizePipeline` walks the
    /// registered optimizer list before the builtin `DefaultOptimization[]`
    /// chain): if it returns `Some(eval)` that eval is installed (lcms2
    /// `return TRUE`); if it returns `None` it declined and the chosen builtin
    /// `strategy` posture runs (`Accurate` → in-place pipeline eval,
    /// `Lcms2Compat` → the builtin optimizer chain). With no optimizer registered
    /// (the default context) this is byte-identical to
    /// [`new_with_formats_strategy`](Transform::new_with_formats_strategy).
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_formats_strategy_in(
        ctx: &crate::context::Context,
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: Flags,
        in_fmt: u32,
        out_fmt: u32,
        strategy: OptimizationStrategy,
    ) -> Result<Transform> {
        // Route the link through `new_in(ctx, …)` so a registered custom INTENT
        // composes with the custom OPTIMIZER below (both plugins honored on the
        // same transform). With an empty `ctx` this is identical to `new`.
        let mut xform = Transform::new_in(ctx, profiles, intents, bpc, adaptation, flags)?;
        xform.formatters = Some(select_formatters(in_fmt, out_fmt, flags)?);
        xform.strategy = strategy;
        // lcms2 passes the LAST intent to `_cmsOptimizePipeline`
        // (cmsxform.c:1145,1210 `LastIntent`); tintbox stores it as
        // `rendering_intent`.
        let intent = xform.rendering_intent.to_raw();
        // Consult the context's custom optimizer first, then fall back to the
        // builtin strategy posture (builtin-wins on decline).
        xform.opt_eval = strategy.build_with_optimizer(
            ctx.plugins().optimizer.as_ref(),
            &xform.lut,
            in_fmt,
            out_fmt,
            intent,
        );
        Ok(xform)
    }

    /// Convenience 2-profile context-carrying constructor (lcms2
    /// `cmsCreateTransformTHR` with explicit format words + a chosen optimizer
    /// posture). Default adaptation 1.0, same `intent`/`bpc` on both links,
    /// `NOOPTIMIZE` flag (inert in tintbox — optimization is driven by the
    /// `strategy` + the context's optimizer, never the flag). Use this to drive a
    /// custom [`Optimizer`](crate::opt::Optimizer) registered on `ctx`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_simple_with_formats_strategy_in(
        ctx: &crate::context::Context,
        input: &Profile,
        output: &Profile,
        intent: RenderingIntent,
        bpc: bool,
        in_fmt: u32,
        out_fmt: u32,
        strategy: OptimizationStrategy,
    ) -> Result<Transform> {
        Transform::new_with_formats_strategy_in(
            ctx,
            &[input, output],
            &[intent, intent],
            &[bpc, bpc],
            &[1.0, 1.0],
            Flags::NOOPTIMIZE,
            in_fmt,
            out_fmt,
            strategy,
        )
    }
}
