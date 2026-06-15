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
use crate::format::{
    self, formatter_is_float, get_input_formatter, get_input_formatter_float, get_output_formatter,
    get_output_formatter_float, AlphaCopyPlan, PackFloatFn, PackFn, UnpackFloatFn, UnpackFn,
    MAX_CHANNELS,
};
use crate::link::{default_icc_intents, link_bpc_mutation};
use crate::math::whitepoint::D50;
use crate::opt::{OptimizationStrategy, OptimizedEval};
use crate::pipeline::Pipeline;
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
    // Gamut-check pipeline (lcms2 `GamutCheck`). Hook only for now — slice 5 does
    // not build it, so it stays `None`.
    gamut_check: Option<Pipeline>,
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
    /// lcms2 `cmsCreateExtendedTransform` (the device-link build). Applies the
    /// `_cmsLinkProfiles` BPC-array mutation (a copy — the caller's `bpc` is not
    /// touched), builds the link via [`default_icc_intents`], and records the
    /// entry/exit color spaces, media white points, and rendering intent.
    pub fn new(
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

        // Build the device-link pipeline (cmsxform.c:1194, _cmsLinkProfiles).
        let lut = default_icc_intents(profiles, intents, &bpc_mut, adaptation, flags.bits())?;

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
        let mut xform = Transform::new(profiles, intents, bpc, adaptation, flags)?;
        xform.formatters = Some(select_formatters(in_fmt, out_fmt, flags)?);
        xform.strategy = strategy;
        // lcms2 passes the LAST intent to `_cmsOptimizePipeline`
        // (cmsxform.c:1145,1210 `LastIntent`); rcms stores it as
        // `rendering_intent`.
        let intent = xform.rendering_intent.to_raw();
        xform.opt_eval = strategy.build(&xform.lut, in_fmt, out_fmt, intent);
        Ok(xform)
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
    /// matrix-merge at link time, and `cmsFLAGS_NOOPTIMIZE` is inert in rcms (the
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
        }
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

        for i in 0..n_pixels {
            let pix = &input[i * in_ch..i * in_ch + in_ch];
            let res = self.lut.eval_float(pix);
            output[i * out_ch..i * out_ch + out_ch].copy_from_slice(&res);
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

        for i in 0..n_pixels {
            let pix = &input[i * in_ch..i * in_ch + in_ch];
            let res = self.lut.eval_16(pix);
            output[i * out_ch..i * out_ch + out_ch].copy_from_slice(&res);
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

        if fmts.is_float {
            let from_input = fmts.from_input_float.as_ref().unwrap();
            let to_output = fmts.to_output_float.as_ref().unwrap();
            let mut fin = [0f32; MAX_CHANNELS];
            for i in 0..n_pixels {
                let in_pixel = &input[i * in_stride..i * in_stride + in_stride];
                let acc = in_pixel;
                from_input(acc, &mut fin);
                // Abstract eval (no inlined optimization — see module docs).
                let res = self.lut.eval_float(&fin[..in_ch]);
                let mut fout = [0f32; MAX_CHANNELS];
                fout[..out_ch].copy_from_slice(&res);
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
                    // The accurate in-place pipeline eval.
                    OptimizedEval::Pipeline => {
                        let res = self.lut.eval_16(&win[..in_ch]);
                        wout[..out_ch].copy_from_slice(&res);
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
