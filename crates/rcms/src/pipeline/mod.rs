//! Multi-stage evaluation pipeline (lcms2 `cmsPipeline`).
//!
//! A pipeline is an ordered list of [`Stage`]s evaluated in the float domain via
//! a ping-pong double buffer, exactly as lcms2's `_LUTevalFloat` /  `_LUTeval16`
//! (cmslut.c:1329-1374). The 16-bit entry point converts in/out at the boundary
//! with `From16ToFloat` / `FromFloatTo16` (cmslut.c:83-101).

mod stage;

pub use stage::Stage;

use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::error::{Error, Result};

/// lcms2 `MAX_STAGE_CHANNELS` (lcms2_internal.h:78). Width of each ping-pong
/// storage buffer; a stage may not exceed this on input or output.
pub const MAX_STAGE_CHANNELS: usize = 128;

/// An ordered chain of [`Stage`]s mapping `input_channels` to `output_channels`.
#[derive(Clone, Debug)]
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
