//! Total Area Coverage estimation (lcms2 `cmsDetectTAC`, `cmsgmt.c:416-510`).
//!
//! Builds a Lab16 → output-profile round-trip on the perceptual intent
//! (`NOOPTIMIZE|NOCACHE`), sweeps a `6 × 74 × 74` Lab grid via
//! [`slice_space_16`], sums the ink per node, and returns the maximum total ink
//! (in %). Output (non-output-class) profiles return `0`.

use crate::format::decode::{
    PT_CMY, PT_CMYK, PT_GRAY, PT_HLS, PT_HSV, PT_LAB, PT_RGB, PT_XYZ, PT_YCBCR, PT_YUV, PT_YUVK,
    PT_YXY, TYPE_LAB_16,
};
use crate::profile::virtuals::build_lab4_profile;
use crate::profile::{ColorSpace, Profile, ProfileClass, RenderingIntent};
use crate::transform::{Flags, Transform};

/// `cmsMAXCHANNELS`.
const MAX_CHANNELS: usize = 16;
/// `MAX_INPUT_DIMENSIONS` cap echoed by `cmsSliceSpace16`'s `In[cmsMAXCHANNELS]`.
const MAX_GRID_INPUTS: usize = 16;

/// `_cmsQuantizeVal` (`cmslut.c:737`): `round((i * 65535) / (max-1))`.
fn quantize_val(i: u32, max_samples: u32) -> u16 {
    use crate::compat::floor::{FloorStrategy, Lcms2Floor};
    let x = (i as f64 * 65535.0) / (max_samples - 1) as f64;
    Lcms2Floor::quick_saturate_word(x)
}

/// `cmsSliceSpace16` (`cmslut.c:887-913`): sweep every node of an `n_inputs`-D
/// grid with per-axis counts `clut_points`, calling `sampler` with the quantized
/// 16-bit input for each node. Returns `false` for the degenerate cases (matches
/// lcms2: too many inputs, or an empty cube).
///
/// The node decode (`rest`/`Colorant`, `t` from `n_inputs-1` down to 0) is
/// transcribed verbatim so the iteration order — and thus any order-dependent
/// reduction in the sampler — matches lcms2.
pub fn slice_space_16<F: FnMut(&[u16]) -> bool>(clut_points: &[u32], mut sampler: F) -> bool {
    let n_inputs = clut_points.len();
    if n_inputs >= MAX_GRID_INPUTS {
        return false;
    }

    // CubeSize: product of per-axis counts.
    let n_total: usize = clut_points.iter().map(|&p| p as usize).product();
    if n_total == 0 {
        return false;
    }

    let mut in16 = [0u16; MAX_CHANNELS];
    for i in 0..n_total {
        let mut rest = i;
        for t in (0..n_inputs).rev() {
            let colorant = (rest % clut_points[t] as usize) as u32;
            rest /= clut_points[t] as usize;
            in16[t] = quantize_val(colorant, clut_points[t]);
        }
        if !sampler(&in16[..n_inputs]) {
            return false;
        }
    }
    true
}

/// `_cmsLCMScolorSpace` (`cmspcs.c:810`): the `PT_*` bits for a profile color
/// space. `None` for spaces lcms2 maps to 0.
fn lcms_color_space(cs: ColorSpace) -> Option<u32> {
    use ColorSpace::*;
    Some(match cs {
        Gray => PT_GRAY,
        Rgb => PT_RGB,
        Cmy => PT_CMY,
        Cmyk => PT_CMYK,
        YCbCr => PT_YCBCR,
        Luv => PT_YUV,
        XYZ => PT_XYZ,
        Lab => PT_LAB,
        LuvK => PT_YUVK,
        Hsv => PT_HSV,
        Hls => PT_HLS,
        Yxy => PT_YXY,
        // 1..15-color / MCHn map to PT_MCHn = n (cmspcs.c).
        Color1 | Mch1 => 1,
        Color2 | Mch2 => 2,
        Color3 | Mch3 => 3,
        Color4 | Mch4 => 4,
        Color5 | Mch5 => 5,
        Color6 | Mch6 => 6,
        Color7 | Mch7 => 7,
        Color8 | Mch8 => 8,
        Color9 | Mch9 => 9,
        Color10 | MchA => 10,
        Color11 | MchB => 11,
        Color12 | MchC => 12,
        Color13 | MchD => 13,
        Color14 | MchE => 14,
        Color15 | MchF => 15,
        _ => return None,
    })
}

/// Channels per color space (`cmsChannelsOfColorSpace`).
fn channels_of(cs: ColorSpace) -> Option<usize> {
    use ColorSpace::*;
    Some(match cs {
        Mch1 | Color1 | Gray => 1,
        Mch2 | Color2 => 2,
        XYZ | Lab | Luv | YCbCr | Yxy | Rgb | Hsv | Hls | Cmy | Mch3 | Color3 => 3,
        LuvK | Cmyk | Mch4 | Color4 => 4,
        Mch5 | Color5 => 5,
        Mch6 | Color6 => 6,
        Mch7 | Color7 => 7,
        Mch8 | Color8 => 8,
        Mch9 | Color9 => 9,
        MchA | Color10 => 10,
        MchB | Color11 => 11,
        MchC | Color12 => 12,
        MchD | Color13 => 13,
        MchE | Color14 => 14,
        MchF | Color15 => 15,
        _ => return None,
    })
}

/// `cmsFormatterForColorspaceOfProfile(hProfile, 4, TRUE)` (`cmspack.c:4025`):
/// `FLOAT_SH(1) | COLORSPACE_SH(pt) | BYTES_SH(4) | CHANNELS_SH(nchan)`.
fn float_formatter_for_profile(profile: &Profile) -> Option<u32> {
    let cs = profile.header().color_space;
    let pt = lcms_color_space(cs)?;
    let nchan = channels_of(cs)? as u32;
    // FLOAT_SH(1) | COLORSPACE_SH(pt) | BYTES_SH(4&7=4) | CHANNELS_SH(nchan).
    Some((1 << 22) | (pt << 16) | (nchan << 3) | 4)
}

/// `cmsDetectTAC` (`cmsgmt.c:462-510`). Total area coverage of an output profile,
/// in %. Non-output-class or unsupported-color-space profiles return `0`.
pub fn detect_tac(profile: &Profile) -> f64 {
    // TAC only works on output profiles (cmsgmt.c:471).
    if profile.header().device_class != ProfileClass::Output {
        return 0.0;
    }

    // Fake float formatter for the result (cmsgmt.c:476).
    let Some(dw_formatter) = float_formatter_for_profile(profile) else {
        return 0.0;
    };
    let n_output_chans = ((dw_formatter >> 3) & 15) as usize; // T_CHANNELS

    // For safety (cmsgmt.c:485).
    if n_output_chans >= MAX_CHANNELS {
        return 0.0;
    }

    // Lab4 → output round-trip on the perceptual intent, NOOPTIMIZE|NOCACHE
    // (cmsgmt.c:487-491). The in-memory Lab4 virtual matches lcms2's freshly-built
    // cmsCreateLab4Profile bit-for-bit (see `Profile::from_writable`).
    let Ok(lab4) = Profile::from_writable(&build_lab4_profile()) else {
        return 0.0;
    };
    let intent = RenderingIntent::Perceptual;
    let Ok(xform) = Transform::new_with_formats(
        &[&lab4, profile],
        &[intent, intent],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        TYPE_LAB_16,
        dw_formatter,
    ) else {
        return 0.0;
    };

    // For L* only black & white; for C* many points (cmsgmt.c:497-499).
    let grid_points = [6u32, 74, 74];

    let mut max_tac: f32 = 0.0;
    let in_stride = 3 * 2; // Lab16: 3 channels × 2 bytes
    let out_stride = n_output_chans * 4; // float output

    let ok = slice_space_16(&grid_points, |in16| {
        // Pack Lab16 → run the float round-trip → sum the ink (EstimateTAC,
        // cmsgmt.c:429-458).
        let mut in_buf = [0u8; 3 * 2];
        for k in 0..3 {
            in_buf[k * 2..k * 2 + 2].copy_from_slice(&in16[k].to_ne_bytes());
        }
        let mut out_buf = vec![0u8; out_stride];
        xform.do_transform(&in_buf[..in_stride], &mut out_buf, 1);

        let mut sum: f32 = 0.0;
        for k in 0..n_output_chans {
            let v = f32::from_ne_bytes(out_buf[k * 4..k * 4 + 4].try_into().unwrap());
            sum += v;
        }
        if sum > max_tac {
            max_tac = sum;
        }
        true
    });

    if !ok {
        return 0.0;
    }

    max_tac as f64
}
