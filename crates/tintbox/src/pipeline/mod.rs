//! Multi-stage evaluation pipeline (lcms2 `cmsPipeline`).
//!
//! A pipeline is an ordered list of [`Stage`]s evaluated in the float domain via
//! a ping-pong double buffer, exactly as lcms2's `_LUTevalFloat` /  `_LUTeval16`
//! (cmslut.c:1329-1374). The 16-bit entry point converts in/out at the boundary
//! with `From16ToFloat` / `FromFloatTo16` (cmslut.c:83-101).

pub mod clut;
mod stage;

pub use clut::{Clut, ClutTable, ResolvedInterp};
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

    /// lcms2 `PreOptimize` (cmsopt.c:251-289), the structural simplification that
    /// `_cmsOptimizePipeline` (cmsopt.c:1952) runs **before** the
    /// `cmsFLAGS_NOOPTIMIZE` early-return (cmsopt.c:1961). Because it executes even
    /// under NOOPTIMIZE, an "accurate"/unoptimized tintbox device link must apply it
    /// too to stay bit-identical to lcms2: the merge of two adjacent matrix stages
    /// drops an intermediate `f32` rounding that lcms2's NOOPTIMIZE pipeline never
    /// performs, so leaving the matrices separate diverges by up to a few LSB after
    /// the following tone curve (the 8→16 matrix-shaper bug).
    ///
    /// Loops until no rule fires (`do { … } while (Opt)`), applying, in order:
    /// - remove `Identity` stages (`_Remove1Op(cmsSigIdentityElemType)`);
    /// - remove inverse-paired PCS conversions
    ///   (`_Remove2Op` for `Xyz2Lab`+`Lab2Xyz`, `Lab2Xyz`+`Xyz2Lab`,
    ///   `LabV4ToV2`+`LabV2ToV4`, `LabV2ToV4`+`LabV4ToV2`; the float-PCS pairs in
    ///   the C have no tintbox stage equivalent and are skipped);
    /// - merge two adjacent 3×3, offset-free `Matrix` stages into their product
    ///   `m2·m1` (`_MultiplyMatrix`), dropping the result entirely when it is
    ///   close-enough to identity (`isFloatMatrixIdentity`, tolerance `1e-5`).
    pub fn pre_optimize(&mut self) {
        loop {
            let mut any = false;
            any |= self.remove_1op_identity();
            any |= self.remove_2op(StageKind::Xyz2Lab, StageKind::Lab2Xyz);
            any |= self.remove_2op(StageKind::Lab2Xyz, StageKind::Xyz2Lab);
            any |= self.remove_2op(StageKind::LabV4ToV2, StageKind::LabV2ToV4);
            any |= self.remove_2op(StageKind::LabV2ToV4, StageKind::LabV4ToV2);
            any |= self.multiply_matrix();
            if !any {
                break;
            }
        }
    }

    /// `_Remove1Op(Lut, cmsSigIdentityElemType)` (cmsopt.c:117-135): drop every
    /// `Identity` stage. The chain stays consistent because an identity's input and
    /// output widths are equal.
    fn remove_1op_identity(&mut self) -> bool {
        let before = self.stages.len();
        self.stages.retain(|s| {
            // `cmsSigIdentityElemType`: identity curve stages (`Stage::Identity`)
            // and identity CLUTs built by `_cmsStageAllocIdentityCLut` (the
            // `implements_identity` marker — see [`crate::pipeline::clut::Clut`]).
            !matches!(s, Stage::Identity(_))
                && !matches!(
                    s,
                    Stage::Clut(c) if c.implements_identity
                )
        });
        self.stages.len() != before
    }

    /// `_Remove2Op(Lut, Op1, Op2)` (cmsopt.c:138-163): remove the first adjacent
    /// `Op1`-then-`Op2` pair, repeating from the start (the C re-scans on each
    /// removal because `pt1`/`pt2` shift). Returns whether anything was removed.
    fn remove_2op(&mut self, op1: StageKind, op2: StageKind) -> bool {
        let mut any = false;
        let mut i = 0;
        while i + 1 < self.stages.len() {
            if stage_kind(&self.stages[i]) == Some(op1)
                && stage_kind(&self.stages[i + 1]) == Some(op2)
            {
                // _RemoveElement(pt2); _RemoveElement(pt1): drop both, then the C
                // continues the while loop without advancing pt1 (it stays at the
                // same slot, now holding the following stage).
                self.stages.remove(i + 1);
                self.stages.remove(i);
                any = true;
                // Do not advance: re-test at the same index (matches the C, where
                // pt1 keeps pointing at Lut->Elements' new content at this spot).
            } else {
                i += 1;
            }
        }
        any
    }

    /// `_MultiplyMatrix(Lut)` (cmsopt.c:188-246): collapse adjacent 3×3 offset-free
    /// `Matrix` stages into their product. For a pair `(m1, m2)` evaluated `m1`
    /// then `m2`, the combined matrix is `m2·m1` (`_cmsMAT3per(&res, m2, m1)`); if
    /// it is close-enough to identity it is removed entirely.
    fn multiply_matrix(&mut self) -> bool {
        use crate::math::matrix::Mat3;

        let mut any = false;
        let mut i = 0;
        while i + 1 < self.stages.len() {
            // Mirror the C `Implements == cmsSigMatrixElemType` test on BOTH stages
            // (cmsopt.c:203). lcms2 models LabV2/V4 and the float-PCS normalizers as
            // matrix-type stages with an overridden Implements, so they are NOT
            // MatrixElemType and never merge — tintbox models them as distinct `Stage`
            // variants, so `Stage::Matrix` already excludes them.
            let both_matrix = matches!(self.stages[i], Stage::Matrix { .. })
                && matches!(self.stages[i + 1], Stage::Matrix { .. });
            if !both_matrix {
                i += 1;
                continue;
            }
            match matrix_pair_mergeable(&self.stages[i], &self.stages[i + 1]) {
                Some((m1, m2)) => {
                    // res = m2 * m1 (apply m1 first, then m2).
                    let res = Mat3(m2).per(&Mat3(m1));
                    // Remove both matrices (pt2 then pt1, as in the C).
                    self.stages.remove(i + 1);
                    self.stages.remove(i);
                    if !is_float_matrix_identity(&res.0) {
                        // Reinsert the combined matrix at the same position; the C
                        // splices Multmat back into the chain where pt1 was.
                        self.stages.insert(
                            i,
                            Stage::Matrix {
                                rows: 3,
                                cols: 3,
                                m: res.0.to_vec(),
                                offset: None,
                            },
                        );
                    }
                    any = true;
                    // The C advances pt1 only when the stages aren't both matrices,
                    // so after a merge it re-tests from the same slot (i unchanged).
                }
                None => {
                    // Both stages ARE matrices but the offset/3x3 guard fails:
                    // lcms2 `return FALSE` (cmsopt.c:213) — it BAILS the entire pass,
                    // it does NOT skip past to merge a later pair. Load-bearing: an
                    // `Matrix(offset), Matrix, Matrix` run must leave the trailing
                    // pair UN-merged to match lcms2. Returning false (not `any`)
                    // mirrors the C; the outer PreOptimize loop still converges.
                    return false;
                }
            }
        }
        any
    }

    /// Evaluate in the float domain (lcms2 `_LUTevalFloat`, cmslut.c:1355-1374).
    ///
    /// Copies `input_channels` floats into the first ping-pong buffer, walks the
    /// stages alternating buffers, and returns the `output_channels` floats from
    /// the final buffer. With no stages this is a straight `memmove` of the
    /// truncated input (matching the C, which copies `min` widths implicitly).
    pub fn eval_float(&self, input: &[f32]) -> Vec<f32> {
        self.eval_float_in(&crate::context::Context::new(), input)
    }

    /// Context-aware [`eval_float`](Self::eval_float): the hoisted `ctx` flows into
    /// each [`Stage::eval_in`], so a tone-curve stage carrying a custom parametric
    /// segment uses its registered plugin and — crucially for the hot path — never
    /// constructs+drops an empty [`Context`] per channel per pixel. With an empty
    /// `ctx` this is byte-for-byte the builtin [`eval_float`](Self::eval_float).
    pub fn eval_float_in(&self, ctx: &crate::context::Context, input: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.output_channels];
        self.eval_float_in_into(ctx, input, &mut out);
        out
    }

    /// Allocation-free [`eval_float_in`](Self::eval_float_in): writes the
    /// `output_channels` result floats into `out[..output_channels]` instead of
    /// returning a fresh `Vec`. Byte-for-byte identical to
    /// [`eval_float_in`](Self::eval_float_in) — the only difference is the
    /// destination. This is the per-pixel hot-path entry; reusing one caller-owned
    /// buffer across pixels removes the per-pixel heap `calloc`/`free`.
    pub fn eval_float_in_into(
        &self,
        ctx: &crate::context::Context,
        input: &[f32],
        out: &mut [f32],
    ) {
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
            stage.eval_in(ctx, &cur[..], &mut nxt[..]);
            phase = next;
        }

        out[..self.output_channels].copy_from_slice(&storage[phase][..self.output_channels]);
    }

    /// Evaluate on a 16-bit basis (lcms2 `_LUTeval16`, cmslut.c:1329-1349).
    ///
    /// `From16ToFloat` converts the inputs (`x as f32 / 65535.0_f32`), the stages
    /// run in the float domain, and `FromFloatTo16` converts the outputs
    /// (`quick_saturate_word(x as f64 * 65535.0)`).
    pub fn eval_16(&self, input: &[u16]) -> Vec<u16> {
        self.eval_16_in(&crate::context::Context::new(), input)
    }

    /// Context-aware [`eval_16`](Self::eval_16): the hoisted `ctx` flows into each
    /// [`Stage::eval_in`] (see [`eval_float_in`](Self::eval_float_in)). With an
    /// empty `ctx` this is byte-for-byte the builtin [`eval_16`](Self::eval_16);
    /// threading one `ctx` from the per-pixel loop removes the per-channel
    /// per-pixel [`Context`](crate::context::Context) construct/drop.
    pub fn eval_16_in(&self, ctx: &crate::context::Context, input: &[u16]) -> Vec<u16> {
        let mut out = vec![0u16; self.output_channels];
        self.eval_16_in_into(ctx, input, &mut out);
        out
    }

    /// Allocation-free [`eval_16_in`](Self::eval_16_in): writes the
    /// `output_channels` result words into `out[..output_channels]` instead of
    /// returning a fresh `Vec`. Byte-for-byte identical to
    /// [`eval_16_in`](Self::eval_16_in) — the only difference is the destination.
    /// This is the per-pixel hot-path entry; reusing one caller-owned buffer across
    /// pixels removes the per-pixel heap `calloc`/`free`.
    pub fn eval_16_in_into(&self, ctx: &crate::context::Context, input: &[u16], out: &mut [u16]) {
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
            stage.eval_in(ctx, &cur[..], &mut nxt[..]);
            phase = next;
        }

        // FromFloatTo16(&Storage[phase], Out, OutputChannels): In[i] is f32 and
        // 65535.0 is f64, so the multiply widens to f64 before saturation.
        for (i, o) in out[..self.output_channels].iter_mut().enumerate() {
            *o = Lcms2Floor::quick_saturate_word(storage[phase][i] as f64 * 65535.0);
        }
    }
}

/// The PCS-conversion stage kinds [`Pipeline::pre_optimize`] pairs up for the
/// `_Remove2Op` rules (a thin tag mirroring lcms2's `cmsStageSignature`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageKind {
    Xyz2Lab,
    Lab2Xyz,
    LabV2ToV4,
    LabV4ToV2,
}

/// The [`StageKind`] of a stage, or `None` for kinds the `_Remove2Op` rules never
/// reference.
fn stage_kind(s: &Stage) -> Option<StageKind> {
    match s {
        Stage::Xyz2Lab => Some(StageKind::Xyz2Lab),
        Stage::Lab2Xyz => Some(StageKind::Lab2Xyz),
        Stage::LabV2ToV4 => Some(StageKind::LabV2ToV4),
        Stage::LabV4ToV2 => Some(StageKind::LabV4ToV2),
        _ => None,
    }
}

/// If `a` then `b` are two 3×3 offset-free `Matrix` stages, return their row-major
/// coefficient arrays `(m1, m2)` ready for the `_MultiplyMatrix` merge. lcms2 bails
/// out of the optimization (`return FALSE`) if either matrix carries an offset or
/// is not 3×3; we mirror that by simply not treating such a pair as mergeable.
fn matrix_pair_mergeable(a: &Stage, b: &Stage) -> Option<([f64; 9], [f64; 9])> {
    let extract = |s: &Stage| -> Option<[f64; 9]> {
        if let Stage::Matrix {
            rows: 3,
            cols: 3,
            m,
            offset: None,
        } = s
        {
            let mut out = [0.0f64; 9];
            out.copy_from_slice(&m[..9]);
            Some(out)
        } else {
            None
        }
    };
    Some((extract(a)?, extract(b)?))
}

/// lcms2 `isFloatMatrixIdentity` (cmsopt.c:172-185): every entry within `1e-5`
/// (`CloseEnoughFloat`, the `0.00001f` literal) of the identity.
fn is_float_matrix_identity(m: &[f64; 9]) -> bool {
    const IDENT: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    // CloseEnoughFloat compares against the *float* literal 0.00001f.
    let tol = 0.00001f32 as f64;
    m.iter()
        .zip(IDENT.iter())
        .all(|(&a, &b)| (b - a).abs() < tol)
}

#[cfg(test)]
mod pre_optimize_tests {
    use super::*;

    fn matrix(m: [f64; 9]) -> Stage {
        Stage::Matrix {
            rows: 3,
            cols: 3,
            m: m.to_vec(),
            offset: None,
        }
    }

    fn curves3() -> Stage {
        use crate::curve::build_gamma;
        Stage::ToneCurves(vec![build_gamma(2.2), build_gamma(2.2), build_gamma(2.2)])
    }

    /// Two adjacent 3×3 offset-free matrices collapse into their product `m2·m1`,
    /// dropping the intermediate that a sequential eval would otherwise round.
    #[test]
    fn merges_adjacent_matrices_into_product() {
        let m1 = [2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 4.0];
        let m2 = [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5];
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(curves3()).unwrap();
        p.insert_stage_at_end(matrix(m1)).unwrap();
        p.insert_stage_at_end(matrix(m2)).unwrap();
        p.insert_stage_at_end(curves3()).unwrap();
        assert_eq!(p.stages().len(), 4);

        p.pre_optimize();

        // curves, ONE merged matrix, curves.
        assert_eq!(p.stages().len(), 3, "two matrices must merge into one");
        assert!(matches!(p.stages()[0], Stage::ToneCurves(_)));
        assert!(matches!(p.stages()[2], Stage::ToneCurves(_)));
        // Merged matrix = m2 * m1 = diag(1.0, 1.5, 2.0).
        if let Stage::Matrix { m, .. } = &p.stages()[1] {
            assert_eq!(m[0], 1.0);
            assert_eq!(m[4], 1.5);
            assert_eq!(m[8], 2.0);
        } else {
            panic!("stage 1 must be the merged matrix");
        }
    }

    /// When the product is close-enough to identity, the merged matrix is dropped
    /// entirely (`isFloatMatrixIdentity`).
    #[test]
    fn drops_matrix_pair_that_multiplies_to_identity() {
        let m = [2.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 5.0];
        let inv = [0.5, 0.0, 0.0, 0.0, 0.25, 0.0, 0.0, 0.0, 0.2];
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(matrix(m)).unwrap();
        p.insert_stage_at_end(matrix(inv)).unwrap();
        p.pre_optimize();
        assert!(
            p.stages().is_empty(),
            "m * m^-1 ≈ identity ⇒ both matrices removed"
        );
    }

    /// `Identity` stages are removed (`_Remove1Op`).
    #[test]
    fn removes_identity_stages() {
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(curves3()).unwrap();
        p.insert_stage_at_end(Stage::Identity(3)).unwrap();
        p.insert_stage_at_end(curves3()).unwrap();
        p.pre_optimize();
        assert_eq!(p.stages().len(), 2);
        assert!(p.stages().iter().all(|s| !matches!(s, Stage::Identity(_))));
    }

    /// An inverse-paired PCS conversion (`Xyz2Lab` then `Lab2Xyz`) is removed
    /// (`_Remove2Op`); a non-paired ordering is left intact.
    #[test]
    fn removes_inverse_paired_pcs_conversions() {
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(Stage::Xyz2Lab).unwrap();
        p.insert_stage_at_end(Stage::Lab2Xyz).unwrap();
        p.pre_optimize();
        assert!(p.stages().is_empty(), "Xyz2Lab+Lab2Xyz cancel");

        // Reverse order does NOT match the Xyz2Lab/Lab2Xyz rule the same way, but
        // Lab2Xyz+Xyz2Lab is its own rule and also cancels.
        let mut p2 = Pipeline::new(3, 3);
        p2.insert_stage_at_end(Stage::Lab2Xyz).unwrap();
        p2.insert_stage_at_end(Stage::Xyz2Lab).unwrap();
        p2.pre_optimize();
        assert!(p2.stages().is_empty(), "Lab2Xyz+Xyz2Lab cancel");
    }

    /// A lone matrix (no adjacent matrix) and a curves/matrix/curves chain that is
    /// already minimal are left unchanged.
    #[test]
    fn leaves_already_minimal_pipeline_unchanged() {
        let m = [2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 4.0];
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(curves3()).unwrap();
        p.insert_stage_at_end(matrix(m)).unwrap();
        p.insert_stage_at_end(curves3()).unwrap();
        let before = p.clone();
        p.pre_optimize();
        assert_eq!(p, before, "no adjacent matrices ⇒ no change");
    }

    /// The allocation-free `_into` evals must produce BYTE-FOR-BYTE the same result
    /// as the `Vec`-returning `eval_*_in` over a non-trivial curves/matrix/curves
    /// pipeline and a sweep of inputs. The `_into` form is the per-pixel hot path,
    /// so any divergence here would be a transform-wide divergence from lcms2.
    #[test]
    fn eval_into_matches_vec_returning_bit_for_bit() {
        let m = [0.9, 0.1, 0.0, 0.05, 0.85, 0.1, 0.0, 0.2, 0.8];
        let mut p = Pipeline::new(3, 3);
        p.insert_stage_at_end(curves3()).unwrap();
        p.insert_stage_at_end(matrix(m)).unwrap();
        p.insert_stage_at_end(curves3()).unwrap();

        let ctx = crate::context::Context::new();
        for &t in &[0u16, 1, 257, 12345, 32768, 50000, 65534, 65535] {
            let win = [t, 65535 - t, t / 2 + 1];

            let want16 = p.eval_16_in(&ctx, &win);
            let mut got16 = [0u16; MAX_STAGE_CHANNELS];
            p.eval_16_in_into(&ctx, &win, &mut got16);
            assert_eq!(
                &got16[..p.output_channels],
                &want16[..],
                "eval_16_in_into diverged at {t}"
            );

            let fin = [
                t as f32 / 65535.0,
                (65535 - t) as f32 / 65535.0,
                (t / 2 + 1) as f32 / 65535.0,
            ];
            let wantf = p.eval_float_in(&ctx, &fin);
            let mut gotf = [0f32; MAX_STAGE_CHANNELS];
            p.eval_float_in_into(&ctx, &fin, &mut gotf);
            assert_eq!(
                gotf[..p.output_channels]
                    .iter()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>(),
                wantf.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                "eval_float_in_into diverged at {t}"
            );
        }
    }
}
