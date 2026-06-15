//! Multi-stage evaluation pipeline (lcms2 `cmsPipeline`).
//!
//! A pipeline is an ordered list of [`Stage`]s evaluated in the float domain via
//! a ping-pong double buffer, exactly as lcms2's `_LUTevalFloat` /  `_LUTeval16`
//! (cmslut.c:1329-1374). The 16-bit entry point converts in/out at the boundary
//! with `From16ToFloat` / `FromFloatTo16` (cmslut.c:83-101).

pub mod clut;
mod stage;

pub use clut::{Clut, ClutTable};
pub use stage::Stage;

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::error::{Error, Result};

/// lcms2 `MAX_STAGE_CHANNELS` (lcms2_internal.h:78). Width of each ping-pong
/// storage buffer; a stage may not exceed this on input or output.
pub const MAX_STAGE_CHANNELS: usize = 128;

/// An ordered chain of [`Stage`]s mapping `input_channels` to `output_channels`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline {
    pub input_channels: usize,
    pub output_channels: usize,
    stages: Vec<Stage>,
}

impl Pipeline {
    /// Allocate an empty pipeline (lcms2 `cmsPipelineAlloc`). An empty pipeline
    /// is the identity over `min(input_channels, output_channels)` channels.
    pub fn new(input_channels: usize, output_channels: usize) -> Self {
        Pipeline {
            input_channels,
            output_channels,
            stages: Vec::new(),
        }
    }

    /// The stages, in evaluation order.
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    /// Append a stage at the end (lcms2 `cmsPipelineInsertStage` with
    /// `cmsAT_END`). Rejects stages whose width exceeds [`MAX_STAGE_CHANNELS`]
    /// or that do not chain with the preceding stage's output width.
    pub fn insert_stage_at_end(&mut self, s: Stage) -> Result<()> {
        if s.input_channels() > MAX_STAGE_CHANNELS || s.output_channels() > MAX_STAGE_CHANNELS {
            return Err(Error::Unsupported("stage exceeds MAX_STAGE_CHANNELS"));
        }
        let expected_in = match self.stages.last() {
            Some(last) => last.output_channels(),
            None => self.input_channels,
        };
        if s.input_channels() != expected_in {
            return Err(Error::Unsupported(
                "stage input width does not chain with the previous stage",
            ));
        }
        self.stages.push(s);
        Ok(())
    }

    /// Prepend a stage at the beginning (lcms2 `cmsPipelineInsertStage` with
    /// `cmsAT_BEGIN`, cmslut.c:1529-1532, followed by `BlessLUT`). After
    /// prepending, `BlessLUT` (cmslut.c:1306-1307) sets `input_channels` to the
    /// new first stage's input width and validates the chain: the new stage's
    /// **output** width must equal the old first stage's input width. With no
    /// existing stages the new stage simply becomes both first and last, so
    /// `BlessLUT` sets `input_channels`/`output_channels` from it (no junction to
    /// check). lcms2 returns FALSE on a chain mismatch; we return
    /// [`Error::Corrupt`].
    pub fn prepend_stage(&mut self, s: Stage) -> Result<()> {
        if s.input_channels() > MAX_STAGE_CHANNELS || s.output_channels() > MAX_STAGE_CHANNELS {
            return Err(Error::Unsupported("stage exceeds MAX_STAGE_CHANNELS"));
        }
        // BlessLUT's chain check at the junction with the (old) first stage:
        // next->InputChannels (old first) must equal prev->OutputChannels (new).
        if let Some(first) = self.stages.first() {
            if first.input_channels() != s.output_channels() {
                return Err(Error::Corrupt(
                    "prepended stage output width does not chain with the first stage",
                ));
            }
        }
        // BlessLUT sets InputChannels = First->InputChannels (the new stage);
        // OutputChannels = Last->OutputChannels (unchanged: still the old last
        // stage, or the new stage itself when the pipeline was empty).
        self.input_channels = s.input_channels();
        if self.stages.is_empty() {
            self.output_channels = s.output_channels();
        }
        self.stages.insert(0, s);
        Ok(())
    }

    /// Concatenate `other` onto `self` (lcms2 `cmsPipelineCat`, cmslut.c:1613).
    ///
    /// Duplicates each of `other`'s stages and appends them at the end (each
    /// append goes through `cmsPipelineInsertStage(.., cmsAT_END, ..)` +
    /// `BlessLUT`, so the chain is validated stage by stage). Channel-count
    /// rules transcribed from the C:
    /// - If **both** pipelines are empty, `self` inherits `other`'s
    ///   `input_channels` and `output_channels` (cmslut.c:1619-1622).
    /// - Otherwise each appended stage runs `BlessLUT`, which sets
    ///   `self.input_channels` to the first stage's input (only changes anything
    ///   when `self` was empty: it then adopts `other`'s input width) and
    ///   `self.output_channels` to the last stage's output (i.e. `other`'s
    ///   output width once all of `other`'s stages are appended).
    /// - The junction stage (`other`'s first stage) must chain: its input width
    ///   must equal `self`'s current output width. On mismatch lcms2 returns
    ///   FALSE; we return [`Error::Corrupt`].
    pub fn concat(&mut self, other: &Pipeline) -> Result<()> {
        // Both empty: inherit channel counts (cmslut.c:1619-1622).
        if self.stages.is_empty() && other.stages.is_empty() {
            self.input_channels = other.input_channels;
            self.output_channels = other.output_channels;
            return Ok(());
        }

        // When self has no stages, C's first AT_END insert does not check the
        // stage against l1.InputChannels (BlessLUT only *sets* it from the new
        // first stage). Adopt other's input width up front so the existing
        // append's "first stage must match input_channels" check passes for the
        // junction stage, mirroring BlessLUT's InputChannels = First->Input.
        if self.stages.is_empty() {
            self.input_channels = other.input_channels;
        }

        for stage in &other.stages {
            // cmsStageDup + cmsPipelineInsertStage(AT_END): the existing
            // append validates the junction (stage.input == current last
            // output). We translate insert's Unsupported chain error into
            // Corrupt to mirror lcms2's BlessLUT returning FALSE for an
            // inconsistent cat.
            self.insert_stage_at_end(stage.clone())
                .map_err(|e| match e {
                    Error::Unsupported(
                        "stage input width does not chain with the previous stage",
                    ) => Error::Corrupt(
                        "pipeline output width does not chain with concatenated pipeline",
                    ),
                    other => other,
                })?;
        }

        // After appending all of other's stages, BlessLUT has set
        // output_channels = last stage's output = other.output_channels. When
        // self was empty, insert_stage_at_end leaves output_channels at its
        // original value, so set it explicitly to match BlessLUT.
        self.output_channels = other.output_channels;
        Ok(())
    }

    /// lcms2 `ChangeInterpolationToTrilinear` (cmsio1.c:516-534): set the
    /// `CMS_LERP_FLAGS_TRILINEAR` hint on every CLUT stage. For a 3-input CLUT this
    /// flips the interpolation from tetrahedral to trilinear — a different numeric
    /// result. `_cmsReadOutputLUT`/`_cmsReadDevicelinkLUT` call this when the PCS is
    /// Lab.
    pub fn change_interpolation_to_trilinear(&mut self) {
        for stage in &mut self.stages {
            if let Stage::Clut(clut) = stage {
                clut.is_trilinear = true;
            }
        }
    }

    /// Evaluate in the float domain (lcms2 `_LUTevalFloat`, cmslut.c:1355-1374).
    ///
    /// Copies `input_channels` floats into the first ping-pong buffer, walks the
    /// stages alternating buffers, and returns the `output_channels` floats from
    /// the final buffer. With no stages this is a straight `memmove` of the
    /// truncated input (matching the C, which copies `min` widths implicitly).
    pub fn eval_float(&self, input: &[f32]) -> Vec<f32> {
        let mut storage = [[0.0f32; MAX_STAGE_CHANNELS]; 2];
        let mut phase = 0usize;

        // memmove(&Storage[0], In, InputChannels * sizeof(f32)).
        storage[phase][..self.input_channels].copy_from_slice(&input[..self.input_channels]);

        for stage in &self.stages {
            let next = phase ^ 1;
            let (cur, nxt) = if phase == 0 {
                let (a, b) = storage.split_at_mut(1);
                (&a[0], &mut b[0])
            } else {
                let (a, b) = storage.split_at_mut(1);
                (&b[0], &mut a[0])
            };
            stage.eval(&cur[..], &mut nxt[..]);
            phase = next;
        }

        storage[phase][..self.output_channels].to_vec()
    }

    /// Evaluate on a 16-bit basis (lcms2 `_LUTeval16`, cmslut.c:1329-1349).
    ///
    /// `From16ToFloat` converts the inputs (`x as f32 / 65535.0_f32`), the stages
    /// run in the float domain, and `FromFloatTo16` converts the outputs
    /// (`quick_saturate_word(x as f64 * 65535.0)`).
    pub fn eval_16(&self, input: &[u16]) -> Vec<u16> {
        let mut storage = [[0.0f32; MAX_STAGE_CHANNELS]; 2];
        let mut phase = 0usize;

        // From16ToFloat(In, &Storage[0], InputChannels): f32 division by 65535.0F.
        for i in 0..self.input_channels {
            storage[phase][i] = input[i] as f32 / 65535.0_f32;
        }

        for stage in &self.stages {
            let next = phase ^ 1;
            let (cur, nxt) = if phase == 0 {
                let (a, b) = storage.split_at_mut(1);
                (&a[0], &mut b[0])
            } else {
                let (a, b) = storage.split_at_mut(1);
                (&b[0], &mut a[0])
            };
            stage.eval(&cur[..], &mut nxt[..]);
            phase = next;
        }

        // FromFloatTo16(&Storage[phase], Out, OutputChannels): In[i] is f32 and
        // 65535.0 is f64, so the multiply widens to f64 before saturation.
        let mut out = vec![0u16; self.output_channels];
        for (i, o) in out.iter_mut().enumerate() {
            *o = Lcms2Floor::quick_saturate_word(storage[phase][i] as f64 * 65535.0);
        }
        out
    }
}
