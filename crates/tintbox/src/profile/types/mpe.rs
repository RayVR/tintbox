//! Multi-Process Element tag reader (lcms2 `Type_MPE_Read`, cmstypes.c:4766).
//!
//! The `mpet` tag (`cmsSigMultiProcessElementType`) encodes a [`Pipeline`] as a
//! position-table of *elements*, each addressed by an offset/size pair relative
//! to the tag's on-disk start. Every element is read in the FLOAT domain (the MPE
//! curve/matrix/CLUT formats all carry IEEE float32 data), so the resulting
//! pipeline evaluates float-natively.
//!
//! Element dispatch (lcms2 `ReadMPEElem`, cmstypes.c:4716) recognises:
//!   - `cvst` (`cmsSigCurveSetElemType`) → [`read_mpe_curve`] (segmented curves),
//!   - `matf` (`cmsSigMatrixElemType`)   → [`read_mpe_matrix`] (P×Q float matrix),
//!   - `clut` (`cmsSigCLutElemType`)     → [`read_mpe_clut`] (float CLUT),
//!   - `bACS`/`eACS` (begin/end abstract colour space) → accepted, no stage,
//!   - anything else → an unknown-extension error.

use crate::curve::{build_mpe_segmented, CurveSegment, ToneCurve};
use crate::error::{Error, Result};
use crate::interp::InterpParams;
use crate::io::ProfileReader;
use crate::pipeline::clut::{Clut, ClutTable};
use crate::pipeline::{Pipeline, Stage};
use crate::profile::tag::Tag;
use crate::profile::types::lut::granular_clut_entries;

/// lcms2 `cmsMAXCHANNELS` (lcms2.h:113): the channel-count ceiling MPE readers
/// reject at or above (valid range 1..=15).
const CMS_MAXCHANNELS: u32 = 16;

/// lcms2 `MINUS_INF` / `PLUS_INF` (cmsgamma.c:41-42): the segmented-curve domain
/// bounds, the *float* literals `±1E22F`.
const MINUS_INF: f32 = -1E22_f32;
const PLUS_INF: f32 = 1E22_f32;

// ---- Curve-segment on-disk signatures (cmstypes.c) ----

/// `cmsSigSegmentedCurve` ('curf'): the wrapper of an embedded MPE curve.
const SIG_SEGMENTED_CURVE: u32 = 0x6375_7266; // 'curf'
/// `cmsSigFormulaCurveSeg` ('parf'): a parametric (formula) curve segment.
const SIG_FORMULA_CURVE_SEG: u32 = 0x7061_7266; // 'parf'
/// `cmsSigSampledCurveSeg` ('samf'): a sampled curve segment.
const SIG_SAMPLED_CURVE_SEG: u32 = 0x7361_6D66; // 'samf'

// ---- MPE element signatures (cmstypes.c `SupportedMPEtypes`) ----

/// `cmsSigCurveSetElemType` ('cvst').
const SIG_CURVE_SET_ELEM: u32 = 0x6376_7374; // 'cvst'
/// `cmsSigMatrixElemType` ('matf').
const SIG_MATRIX_ELEM: u32 = 0x6D61_7466; // 'matf'
/// `cmsSigCLutElemType` ('clut').
const SIG_CLUT_ELEM: u32 = 0x636C_7574; // 'clut'
/// `cmsSigBAcsElemType` ('bACS'): begin abstract colour space, no stage.
const SIG_BACS_ELEM: u32 = 0x6241_4353; // 'bACS'
/// `cmsSigEAcsElemType` ('eACS'): end abstract colour space, no stage.
const SIG_EACS_ELEM: u32 = 0x6541_4353; // 'eACS'

/// lcms2 `ReadPositionTable` (cmstypes.c:219): read `count` offset/size pairs
/// (each two big-endian u32, offsets relative to `base_offset`), then seek to
/// each element in turn and invoke `element_fn(reader, index)`. The per-element
/// size is read but, as in the C, not forwarded to the (sizeless) MPE readers.
fn read_position_table<R: ProfileReader>(
    r: &mut R,
    count: u32,
    base_offset: u64,
    mut element_fn: impl FnMut(&mut R, u32) -> Result<()>,
) -> Result<()> {
    // Read every (offset, size) pair first, then process — the C buffers the
    // whole table before seeking (the reads here advance past the directory).
    // Cap the capacity hint: `count` is an attacker-controlled u32 ElementCount;
    // an unbounded hint reserves gigabytes and aborts before the first read. The
    // loop is bounded by `count` reads that fail on truncation. (Same pattern as
    // the MLU/named readers.)
    let mut offsets = Vec::with_capacity((count as usize).min(0x1_0000));
    for _ in 0..count {
        let off = r.read_u32()? as u64;
        let _size = r.read_u32()?;
        offsets.push(base_offset + off);
    }

    for (i, &off) in offsets.iter().enumerate() {
        r.seek(off)?;
        element_fn(r, i as u32)?;
    }
    Ok(())
}

/// lcms2 `ReadSegmentedCurve` (cmstypes.c:4224): read one embedded segmented
/// tone curve. The wrapper is `cmsSigSegmentedCurve` + reserved; then `nSegments`
/// (u16) + reserved; then `nSegments - 1` float32 break-points; then each segment
/// (`cmsSigFormulaCurveSeg` → type + params, or `cmsSigSampledCurveSeg` → count +
/// samples). The first segment starts at `MINUS_INF`, the last ends at `PLUS_INF`.
fn read_segmented_curve<R: ProfileReader>(r: &mut R) -> Result<ToneCurve> {
    let element_sig = r.read_u32()?;
    if element_sig != SIG_SEGMENTED_CURVE {
        return Err(Error::Corrupt(
            "MPE curve: expected segmented curve signature",
        ));
    }
    let _reserved = r.read_u32()?;
    let n_segments = r.read_u16()?;
    let _reserved2 = r.read_u16()?;

    if n_segments < 1 {
        return Err(Error::Corrupt("MPE curve: nSegments < 1"));
    }
    let n_segments = n_segments as usize;

    let mut segments: Vec<CurveSegment> = Vec::with_capacity(n_segments);
    for _ in 0..n_segments {
        segments.push(CurveSegment {
            x0: 0.0,
            x1: 0.0,
            seg_type: 0,
            params: [0.0; 10],
            sampled: Vec::new(),
        });
    }

    // Read breakpoints: Segments[i].x0 = PrevBreak, x1 = next break (float32).
    let mut prev_break = MINUS_INF;
    for seg in segments.iter_mut().take(n_segments - 1) {
        seg.x0 = prev_break;
        seg.x1 = r.read_f32()?;
        prev_break = seg.x1;
    }
    segments[n_segments - 1].x0 = prev_break;
    segments[n_segments - 1].x1 = PLUS_INF;

    // Read each segment body.
    for seg in segments.iter_mut() {
        let seg_sig = r.read_u32()?;
        let _reserved = r.read_u32()?;

        match seg_sig {
            SIG_FORMULA_CURVE_SEG => {
                // ParamsByType = {4, 5, 5} for on-disk types {0, 1, 2}.
                const PARAMS_BY_TYPE: [usize; 3] = [4, 5, 5];
                let ty = r.read_u16()?;
                let _reserved = r.read_u16()?;
                if ty > 2 {
                    return Err(Error::Corrupt("MPE curve: formula type > 2"));
                }
                // lcms2 stores Segments[i].Type = Type + 6 (parametric 6/7/8).
                seg.seg_type = ty as i32 + 6;
                for j in 0..PARAMS_BY_TYPE[ty as usize] {
                    seg.params[j] = r.read_f32()? as f64;
                }
            }
            SIG_SAMPLED_CURVE_SEG => {
                let count = r.read_u32()?;
                // The first point is implicit (slot 0 = 0.0, filled by
                // build_mpe_segmented's fix-up). Build incrementally rather than
                // pre-allocating `count+1` from the untrusted u32 (which would
                // abort on a malformed huge count); the reads fail fast on
                // truncation.
                let total = count as usize + 1;
                let mut sampled = Vec::with_capacity(total.min(0x1_0000));
                sampled.push(0.0f32);
                for _ in 0..count {
                    sampled.push(r.read_f32()?);
                }
                seg.seg_type = 0;
                seg.sampled = sampled;
            }
            _ => return Err(Error::Corrupt("MPE curve: unknown curve element type")),
        }
    }

    Ok(build_mpe_segmented(segments))
}

/// lcms2 `Type_MPEcurve_Read` (cmstypes.c:4363): a curve-set element. Header is
/// InputChans (u16) == OutputChans (u16); then a position table of `InputChans`
/// embedded segmented curves; one [`Stage::ToneCurves`].
fn read_mpe_curve<R: ProfileReader>(r: &mut R, lut: &mut Pipeline) -> Result<()> {
    // BaseOffset = Tell - sizeof(_cmsTagBase). The reader is positioned past the
    // element's signature+reserved (the 8-byte element base ReadMPEElem read).
    let base_offset = r.tell() - 8;

    let input_chans = r.read_u16()?;
    let output_chans = r.read_u16()?;
    if input_chans != output_chans {
        return Err(Error::Corrupt("MPE curve set: input != output channels"));
    }

    let n = input_chans as u32;
    let mut curves: Vec<ToneCurve> = Vec::with_capacity(n as usize);
    read_position_table(r, n, base_offset, |r, _i| {
        curves.push(read_segmented_curve(r)?);
        Ok(())
    })?;

    lut.insert_stage_at_end(Stage::ToneCurves(curves))
}

/// lcms2 `Type_MPEmatrix_Read` (cmstypes.c:4515): InputChans (u16), OutputChans
/// (u16); then `InputChans * OutputChans` float32 matrix entries (row-major,
/// `[e11..e1P, e21..e2P, ..]`), then `OutputChans` float32 offsets. Builds a
/// `cmsStageAllocMatrix(OutputChans, InputChans, ..)` — a `Matrix` stage with
/// `rows = OutputChans`, `cols = InputChans`, always carrying the offset vector.
fn read_mpe_matrix<R: ProfileReader>(r: &mut R, lut: &mut Pipeline) -> Result<()> {
    let input_chans = r.read_u16()? as u32;
    let output_chans = r.read_u16()? as u32;

    if input_chans >= CMS_MAXCHANNELS || output_chans >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("MPE matrix: channel count out of range"));
    }

    let n_elems = (input_chans * output_chans) as usize;
    let mut m = Vec::with_capacity(n_elems);
    for _ in 0..n_elems {
        m.push(r.read_f32()? as f64);
    }
    let mut offset = Vec::with_capacity(output_chans as usize);
    for _ in 0..output_chans {
        offset.push(r.read_f32()? as f64);
    }

    lut.insert_stage_at_end(Stage::Matrix {
        rows: output_chans as usize,
        cols: input_chans as usize,
        m,
        offset: Some(offset),
    })
}

/// lcms2 `Type_MPEclut_Read` (cmstypes.c:4618): InputChans (u16), OutputChans
/// (u16); 16 dimension bytes (per-input-channel grid-point counts, any `== 1`
/// rejected); then `nEntries` float32 (`nEntries = OutputChans * Π grid`). Builds
/// a `cmsStageAllocCLutFloatGranular` — a float CLUT ([`ClutTable::F32`]).
fn read_mpe_clut<R: ProfileReader>(r: &mut R, lut: &mut Pipeline) -> Result<()> {
    let input_chans = r.read_u16()? as u32;
    let output_chans = r.read_u16()? as u32;

    if input_chans == 0 || input_chans >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("MPE CLUT: input channels out of range"));
    }
    if output_chans == 0 || output_chans >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("MPE CLUT: output channels out of range"));
    }

    // 16 dimension bytes; only the first InputChans are grid points (InputChans
    // <= 15 = MAX_INPUT_DIMENSIONS < cmsMAXCHANNELS, so all are used). The rest
    // are read and discarded.
    let mut grid: Vec<u32> = Vec::with_capacity(input_chans as usize);
    for i in 0..16u32 {
        let d = r.read_u8()?;
        if i < input_chans {
            if d == 1 {
                return Err(Error::Corrupt("MPE CLUT: grid point == 1"));
            }
            grid.push(d as u32);
        }
    }

    // nEntries = OutputChans * CubeSize(grid) (cmsStageAllocCLutFloatGranular).
    // `CubeSize` applies lcms2's dim<=1 / per-step overflow / UINT_MAX/15 guards;
    // an `n == 0` result is the C `nEntries == 0 -> return NULL`, i.e. reject.
    let n_entries = granular_clut_entries(output_chans, &grid)
        .ok_or(Error::Corrupt("MPE CLUT: table size invalid"))?;

    let mut table = vec![0.0f32; n_entries as usize];
    for slot in table.iter_mut() {
        *slot = r.read_f32()?;
    }

    let params = InterpParams::new(&grid, input_chans as usize, output_chans as usize);
    lut.insert_stage_at_end(Stage::Clut(Clut {
        table: ClutTable::F32(table),
        params,
        is_trilinear: false,
        implements_identity: false,
        resolved: Default::default(),
    }))
}

/// lcms2 `ReadMPEElem` (cmstypes.c:4716): read the element signature (u32) +
/// reserved (u32), then dispatch. `bACS`/`eACS` are accepted with no stage; the
/// real elements append a stage; an unknown signature is an error.
fn read_mpe_elem<R: ProfileReader>(r: &mut R, lut: &mut Pipeline) -> Result<()> {
    let element_sig = r.read_u32()?;
    let _reserved = r.read_u32()?;

    match element_sig {
        SIG_CURVE_SET_ELEM => read_mpe_curve(r, lut),
        SIG_MATRIX_ELEM => read_mpe_matrix(r, lut),
        SIG_CLUT_ELEM => read_mpe_clut(r, lut),
        // Begin/end abstract colour space: accepted, no stage inserted.
        SIG_BACS_ELEM | SIG_EACS_ELEM => Ok(()),
        _ => Err(Error::Corrupt("MPE: unknown element type")),
    }
}

/// lcms2 `Type_MPE_Read` (cmstypes.c:4766). Reads InputChans (u16), OutputChans
/// (u16), an ElementCount (u32), then a position table of that many elements,
/// each appended to the pipeline. Finally asserts the declared channel counts
/// match the assembled pipeline's input/output widths.
pub fn read_mpe<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    // BaseOffset = Tell - sizeof(_cmsTagBase): the reader sits just past the
    // 8-byte type base, so its position is BaseOffset + 8.
    let base_offset = r.tell() - 8;

    let input_chans = r.read_u16()? as u32;
    let output_chans = r.read_u16()? as u32;

    if input_chans == 0 || input_chans >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("MPE: input channels out of range"));
    }
    if output_chans == 0 || output_chans >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("MPE: output channels out of range"));
    }

    let mut lut = Pipeline::new(input_chans as usize, output_chans as usize);

    let element_count = r.read_u32()?;
    read_position_table(r, element_count, base_offset, |r, _i| {
        read_mpe_elem(r, &mut lut)
    })?;

    // Channel-count check: declared header widths must match the pipeline.
    if input_chans as usize != lut.input_channels || output_chans as usize != lut.output_channels {
        return Err(Error::Corrupt("MPE: channel count mismatch"));
    }

    Ok(Tag::Lut(lut))
}
