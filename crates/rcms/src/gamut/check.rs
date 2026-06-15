//! Gamut-check LUT (lcms2 `_cmsCreateGamutCheckPipeline`, `cmsgmt.c:280-414`).
//!
//! Builds a 3→1 CLUT over the PCS whose single output channel encodes the dE that
//! results from a Lab → device → Lab round-trip on the gamut profile; values truly
//! out of gamut are nonzero. The transform machinery feeds this pipeline through
//! `cmsPipelineEvalFloat`/`Eval16Fn` and substitutes the alarm color where the
//! marker fires (see [`crate::transform`] proofing path).

use crate::color::CIELab;
use crate::format::decode::{
    PT_CMY, PT_CMYK, PT_GRAY, PT_HLS, PT_HSV, PT_LAB, PT_RGB, PT_XYZ, PT_YCBCR, PT_YUV, PT_YUVK,
    PT_YXY, TYPE_LAB_DBL,
};
use crate::interp::InterpParams;
use crate::pcs::delta_e;
use crate::pipeline::{Clut, ClutTable, Pipeline, Stage};
use crate::profile::virtuals::build_lab4_profile;
use crate::profile::{ColorSpace, Profile, RenderingIntent};
use crate::transform::{Flags, Transform};
use crate::Result;

/// `ERR_THRESHOLD` (`cmsgmt.c:210`).
const ERR_THRESHOLD: f64 = 5.0;

/// `_cmsLCMScolorSpace` (`cmspcs.c:810`).
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

/// `cmsChannelsOfColorSpace`.
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

/// `cmsIsMatrixShaper` (`cmsio1.c:806-827`).
fn is_matrix_shaper(profile: &Profile) -> bool {
    use crate::sig::Signature;
    let tag = |b: &[u8; 4]| profile.has_tag(Signature::from_bytes(*b));
    match profile.header().color_space {
        ColorSpace::Gray => tag(b"kTRC"),
        ColorSpace::Rgb => {
            tag(b"rXYZ")
                && tag(b"gXYZ")
                && tag(b"bXYZ")
                && tag(b"rTRC")
                && tag(b"gTRC")
                && tag(b"bTRC")
        }
        _ => false,
    }
}

/// `_cmsReasonableGridpointsByColorspace(.., cmsFLAGS_HIGHRESPRECALC)`
/// (`cmspcs.c:659-704`): the gamut-check pipeline always uses HIGHRES.
fn reasonable_gridpoints_highres(n_channels: usize) -> u32 {
    if n_channels > 4 {
        7
    } else if n_channels == 4 {
        23
    } else {
        49
    }
}

/// The round-trip transforms used by `GamutSampler` (`cmsgmt.c` `GAMUTCHAIN`).
struct GamutChain {
    h_input: Transform,
    h_forward: Transform,
    h_reverse: Transform,
    threshold: f64,
    /// Device channel count of the gamut profile (forward/reverse format AND the
    /// CLUT input dimensionality, per `cmsStageAllocCLut16bit(.., nChannels, 1)`).
    n_channels: usize,
    /// Input-profile channel count: `hInput` reads this many channels from the
    /// CLUT node (lcms2 `GamutSampler` passes the raw `In[]`; the formatter takes
    /// only `nInputChannels`).
    n_input_channels: usize,
}

impl GamutChain {
    /// `GamutSampler` (`cmsgmt.c:213-278`): for one CLUT node (`n_channels` 16-bit
    /// values), run the input→Lab, forward (Lab→device), reverse (device→Lab)
    /// chain twice and turn the two dE's into the out-of-gamut marker. `hInput`
    /// consumes only the first `n_input_channels` of the node.
    fn sample(&self, node: &[u16]) -> u16 {
        let lab_in1 = self.run_lab_dbl_from16(&self.h_input, node);

        // Forward: Lab → device (16-bit), then Reverse: device → Lab.
        let proof = self.run_device_from_lab(&self.h_forward, &lab_in1);
        let lab_out1 = self.run_lab_dbl_from_device(&self.h_reverse, &proof);

        let lab_in2 = lab_out1;

        // Again, taking the readback as input.
        let proof2 = self.run_device_from_lab(&self.h_forward, &lab_out1);
        let lab_out2 = self.run_lab_dbl_from_device(&self.h_reverse, &proof2);

        let de1 = delta_e(lab_in1, lab_out1);
        let de2 = delta_e(lab_in2, lab_out2);

        gamut_marker(de1, de2, self.threshold)
    }

    fn run_lab_dbl_from16(&self, xform: &Transform, node: &[u16]) -> CIELab {
        // hInput's input format is nInputChannels-wide 16-bit; feed exactly that.
        let mut in_buf = vec![0u8; self.n_input_channels * 2];
        for k in 0..self.n_input_channels {
            in_buf[k * 2..k * 2 + 2].copy_from_slice(&node[k].to_ne_bytes());
        }
        let mut out_buf = [0u8; 24];
        xform.do_transform(&in_buf, &mut out_buf, 1);
        lab_from_buf(&out_buf)
    }

    fn run_device_from_lab(&self, xform: &Transform, lab: &CIELab) -> Vec<u16> {
        let mut in_buf = [0u8; 24];
        in_buf[0..8].copy_from_slice(&lab.l.to_ne_bytes());
        in_buf[8..16].copy_from_slice(&lab.a.to_ne_bytes());
        in_buf[16..24].copy_from_slice(&lab.b.to_ne_bytes());
        let mut out_buf = vec![0u8; self.n_channels * 2];
        xform.do_transform(&in_buf, &mut out_buf, 1);
        (0..self.n_channels)
            .map(|k| u16::from_ne_bytes(out_buf[k * 2..k * 2 + 2].try_into().unwrap()))
            .collect()
    }

    fn run_lab_dbl_from_device(&self, xform: &Transform, dev: &[u16]) -> CIELab {
        let mut in_buf = vec![0u8; self.n_channels * 2];
        for k in 0..self.n_channels {
            in_buf[k * 2..k * 2 + 2].copy_from_slice(&dev[k].to_ne_bytes());
        }
        let mut out_buf = [0u8; 24];
        xform.do_transform(&in_buf, &mut out_buf, 1);
        lab_from_buf(&out_buf)
    }
}

fn lab_from_buf(buf: &[u8]) -> CIELab {
    CIELab {
        l: f64::from_ne_bytes(buf[0..8].try_into().unwrap()),
        a: f64::from_ne_bytes(buf[8..16].try_into().unwrap()),
        b: f64::from_ne_bytes(buf[16..24].try_into().unwrap()),
    }
}

/// The marker logic of `GamutSampler` (`cmsgmt.c:248-274`).
fn gamut_marker(de1: f64, de2: f64, threshold: f64) -> u16 {
    use crate::compat::floor::{FloorStrategy, Lcms2Floor};

    if de1 < threshold && de2 < threshold {
        0
    } else if de1 < threshold && de2 > threshold {
        // Undefined, assume in gamut.
        0
    } else if de1 > threshold && de2 < threshold {
        // Clearly out of gamut.
        Lcms2Floor::quick_floor((de1 - threshold) + 0.5) as u16
    } else {
        // Both big: take error ratio.
        let error_ratio = if de2 == 0.0 { de1 } else { de1 / de2 };
        if error_ratio > threshold {
            Lcms2Floor::quick_floor((error_ratio - threshold) + 0.5) as u16
        } else {
            0
        }
    }
}

/// `_cmsCreateGamutCheckPipeline` (`cmsgmt.c:280-414`). Builds the 3→1 gamut-check
/// LUT. `profiles`/`bpc`/`intents`/`adaptation` are the proofing chain;
/// `n_gamut_pcs_position` is the chain index whose PCS the gamut is measured from
/// (1-based count of leading profiles before the Lab identity); `gamut` is the
/// proofing profile. Returns `None` if the position is out of range or a
/// sub-transform fails to build.
#[allow(clippy::too_many_arguments)]
pub fn create_gamut_check_pipeline(
    profiles: &[&Profile],
    bpc: &[bool],
    intents: &[RenderingIntent],
    adaptation: &[f64],
    n_gamut_pcs_position: usize,
    gamut: &Profile,
) -> Result<Option<Pipeline>> {
    if n_gamut_pcs_position == 0 || n_gamut_pcs_position > 255 {
        return Ok(None);
    }

    let lab4_input = Profile::from_writable(&build_lab4_profile())?;
    let lab4_fwd = Profile::from_writable(&build_lab4_profile())?;
    let lab4_rev = Profile::from_writable(&build_lab4_profile())?;

    // Threshold: matrix shapers are near-exact (cmsgmt.c:326-332).
    let threshold = if is_matrix_shaper(gamut) {
        1.0
    } else {
        ERR_THRESHOLD
    };

    let color_space = gamut.header().color_space;
    let Some(n_channels) = channels_of(color_space) else {
        return Ok(None);
    };
    let n_gridpoints = reasonable_gridpoints_highres(n_channels);

    let input_color_space = profiles[0].header().color_space;
    let Some(n_input_channels) = channels_of(input_color_space) else {
        return Ok(None);
    };
    let Some(input_pt) = lcms_color_space(input_color_space) else {
        return Ok(None);
    };

    // Input format: CHANNELS_SH(nInputChannels) | BYTES_SH(2) (cmsgmt.c:355).
    let in_dw_format = (input_pt << 16) | ((n_input_channels as u32) << 3) | 2;

    // hInput: chain[0..position] + Lab identity → Lab DBL, NOCACHE
    // (cmsgmt.c:336-366). Build the profile list = leading profiles + Lab4.
    let mut input_profiles: Vec<&Profile> = profiles[..n_gamut_pcs_position].to_vec();
    input_profiles.push(&lab4_input);
    let mut input_intents: Vec<RenderingIntent> = intents[..n_gamut_pcs_position].to_vec();
    input_intents.push(RenderingIntent::RelativeColorimetric);
    let mut input_bpc: Vec<bool> = bpc[..n_gamut_pcs_position].to_vec();
    input_bpc.push(false);
    let mut input_adapt: Vec<f64> = adaptation[..n_gamut_pcs_position].to_vec();
    input_adapt.push(1.0);

    let h_input = Transform::new_with_formats(
        &input_profiles,
        &input_intents,
        &input_bpc,
        &input_adapt,
        Flags::NOOPTIMIZE,
        in_dw_format,
        TYPE_LAB_DBL,
    )?;

    // Forward: Lab DBL → gamut device (16-bit), rel-col, NOCACHE (cmsgmt.c:370-375).
    let dev_dw_format =
        (lcms_color_space(color_space).unwrap() << 16) | ((n_channels as u32) << 3) | 2;
    let h_forward = Transform::new_with_formats(
        &[&lab4_fwd, gamut],
        &[
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::RelativeColorimetric,
        ],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        TYPE_LAB_DBL,
        dev_dw_format,
    )?;

    // Reverse: gamut device (16-bit) → Lab DBL, rel-col, NOCACHE (cmsgmt.c:378-381).
    let h_reverse = Transform::new_with_formats(
        &[gamut, &lab4_rev],
        &[
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::RelativeColorimetric,
        ],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        dev_dw_format,
        TYPE_LAB_DBL,
    )?;

    let chain = GamutChain {
        h_input,
        h_forward,
        h_reverse,
        threshold,
        n_channels,
        n_input_channels,
    };

    // Build the CLUT and sample it (cmsgmt.c:390-401). lcms2 allocates the CLUT
    // with `nChannels` (the GAMUT device channel count) input dimensions —
    // `cmsStageAllocCLut16bit(nGridpoints, nChannels, 1, NULL)` — even though the
    // owning pipeline is declared `cmsPipelineAlloc(3, 1)`. The stage's own input
    // width (`nChannels`) governs evaluation, so the rcms pipeline declares
    // `n_channels` inputs to read the same slice of the proofing transform's input
    // pixel that lcms2 reads.
    let n_samples: Vec<u32> = vec![n_gridpoints; n_channels];
    let params = InterpParams::new(&n_samples, n_channels, 1);
    let table = sample_gamut_clut(&params, &chain);

    let mut gamut_pipe = Pipeline::new(n_channels, 1);
    let clut = Clut {
        table: ClutTable::U16(table),
        params,
        is_trilinear: false,
        implements_identity: false,
    };
    gamut_pipe.insert_stage_at_end(Stage::Clut(clut))?;

    Ok(Some(gamut_pipe))
}

/// Sweep the `n_channels`-D grid (`cmsStageSampleCLut16bit`, node decode identical
/// to [`crate::gamut::tac::slice_space_16`]) running `GamutSampler` at each node.
fn sample_gamut_clut(params: &InterpParams, chain: &GamutChain) -> Vec<u16> {
    use crate::compat::floor::{FloorStrategy, Lcms2Floor};

    let n_in = params.n_inputs;
    let n_samples = &params.n_samples;
    let n_total: usize = n_samples[..n_in].iter().map(|&s| s as usize).product();
    let mut table = vec![0u16; n_total];

    let mut node = vec![0u16; n_in];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut rest = i;
        for t in (0..n_in).rev() {
            let colorant = (rest % n_samples[t] as usize) as u32;
            rest /= n_samples[t] as usize;
            let x = (colorant as f64 * 65535.0) / (n_samples[t] - 1) as f64;
            node[t] = Lcms2Floor::quick_saturate_word(x);
        }
        *slot = chain.sample(&node);
    }
    table
}
