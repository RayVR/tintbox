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
//! 3. **Pixel-format packing + pipeline optimization.** Buffers are flat
//!    float/u16 arrays; the `cmsFormatter` packing/unpacking layer and the
//!    optimizer (`cmsFLAGS_NOOPTIMIZE` is forced on) are slice 6.

use crate::color::CIEXYZ;
use crate::link::{default_icc_intents, link_bpc_mutation};
use crate::math::whitepoint::D50;
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
        })
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
}
