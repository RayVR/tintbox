//! LUT8 / LUT16 tag readers (lcms2 `Type_LUT8_Read` / `Type_LUT16_Read`,
//! cmstypes.c:2002-2100 / 2307-2395). Both build a `cmsPipeline` from a fixed
//! structure: an optional 3x3 matrix, an input tone-curve set, a CLUT, and an
//! output tone-curve set.
//!
//! The two differ only in the on-disk sample width: LUT8 reads 256-entry 8-bit
//! curve tables and an 8-bit CLUT (each value widened with `FROM_8_TO_16`), and
//! its header has no entry counts; LUT16 reads `InputEntries`/`OutputEntries`
//! u16 counts after `CLUTpoints` and stores 16-bit tables/CLUT verbatim.

// Untrusted-input parser: forbid the constructs that panic on malformed bytes
// (a panic here is a DoS). Size arithmetic that mirrors lcms2's C wrapping uses
// `wrapping_*`/`checked_*` explicitly.
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use crate::curve::{build_tabulated_16, ToneCurve};
use crate::error::{Error, Result};
use crate::interp::InterpParams;
use crate::io::{ProfileReader, READ_RESERVE_CAP};
use crate::pipeline::clut::{Clut, ClutTable};
use crate::pipeline::{Pipeline, Stage};
use crate::profile::tag::Tag;
use crate::profile::types::curve::{read_curve, read_parametric_curve};

/// lcms2 `cmsMAXCHANNELS` (lcms2.h:113): the channel-count ceiling both LUT
/// readers reject above.
const CMS_MAXCHANNELS: u32 = 16;

/// lcms2 `MAX_INPUT_DIMENSIONS` (lcms2_internal.h): the most input channels a
/// CLUT interpolator handles. `_cmsComputeInterpParamsEx` (cmsintrp.c:120)
/// rejects more with `cmsERROR_RANGE`.
const MAX_INPUT_DIMENSIONS: u32 = 15;

/// lcms2 `FROM_8_TO_16(rgb)` (lcms2_internal.h:125): replicate the 8-bit value
/// into both bytes of a 16-bit word (`(v << 8) | v`), so `0xFF -> 0xFFFF`.
fn from_8_to_16(v: u8) -> u16 {
    ((v as u16) << 8) | v as u16
}

/// lcms2 `_cmsMAT3isIdentity` (cmsmtrx.c:98): every entry of the 3x3 matrix is
/// `CloseEnough` to the identity matrix. `CloseEnough` (cmsmtrx.c:88) is
/// `fabs(b - a) < (1.0 / 65535.0)`.
fn mat3_is_identity(m: &[f64; 9]) -> bool {
    const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    m.iter()
        .zip(IDENTITY.iter())
        .all(|(a, b)| (b - a).abs() < (1.0 / 65535.0))
}

/// lcms2 `uipow` (cmstypes.c:1974): `n * a^b` with overflow check. Returns
/// `None` on overflow (the C `(cmsUInt32Number) -1` sentinel) so callers reject
/// the tag. `a == 0` or `n == 0` yields `0`.
///
/// Used by the LUT8/LUT16 CLUT path ONLY, which in the pinned lcms2 calls
/// `uipow` directly (cmstypes.c:2051 / 2378) rather than `CubeSize`. The granular
/// (per-dimension) CLUT readers use [`cube_size`] instead — see its note.
fn uipow(n: u32, a: u32, b: u32) -> Option<u32> {
    if a == 0 || n == 0 {
        return Some(0);
    }
    let mut rv: u32 = 1;
    for _ in 0..b {
        rv = rv.checked_mul(a)?;
    }
    rv.checked_mul(n)
}

/// lcms2 `CubeSize` (cmslut.c:461): the hypercube node count for a per-dimension
/// grid `dims`, with lcms2's exact overflow/validity guards. Returns `0` (the C
/// sentinel that makes `cmsStageAllocCLut*Granular` reject the stage) when:
///   - any `dim <= 1` (an impossible grid size: 0 = no CLUT, else >= 2),
///   - the running product would exceed `u32::MAX / dim` (per-step overflow),
///   - or the final product exceeds `u32::MAX / 15` (so a later `outputChan * rv`
///     with `outputChan <= 15` cannot overflow `u32`).
///
/// This is STRICTER than a plain `checked_mul` chain: the `<= 1` rejection and
/// the `/15` ceiling both reject grids that `checked_mul` would accept, matching
/// lcms2 accept/reject parity for the granular CLUT readers (A2B/B2A and MPE).
/// The intermediate product is accumulated in `u64` exactly as the C uses a
/// `cmsUInt64Number rv`, so the `> u32::MAX/dim` test sees the true value.
fn cube_size(dims: &[u32]) -> u32 {
    let mut rv: u64 = 1;
    // Iterate in reverse to mirror the C `for (; b > 0; b--)` over `Dims[b-1]`;
    // the order is immaterial to the product but keeps the transcription literal.
    for &dim in dims.iter().rev() {
        let dim = dim as u64;
        if dim <= 1 {
            return 0;
        }
        if rv > u64::from(u32::MAX) / dim {
            return 0;
        }
        rv *= dim;
    }
    if rv > u64::from(u32::MAX) / 15 {
        return 0;
    }
    rv as u32
}

/// `n_entries = output_channels * CubeSize(grid)` for a granular CLUT, returning
/// `None` (→ reject the tag) when lcms2's `cmsStageAllocCLut*Granular` would see
/// `n == 0` and bail (`CubeSize` hit a guard, or `output_channels == 0`). The
/// `output_channels * cube` product cannot overflow `u32` because `cube_size`
/// caps `cube` at `u32::MAX / 15` and `output_channels <= 15`.
pub(crate) fn granular_clut_entries(output_channels: u32, grid: &[u32]) -> Option<u32> {
    let cube = cube_size(grid);
    let n = output_channels.checked_mul(cube)?;
    if n == 0 {
        return None;
    }
    Some(n)
}

/// lcms2 `Read8bitTables` (cmstypes.c:1885-1932): read `n_channels` 256-entry
/// 8-bit curve tables, each widened with `FROM_8_TO_16`, and append a
/// `ToneCurves` stage. Rejects `n_channels == 0` or `> cmsMAXCHANNELS`.
fn read_8bit_tables<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    n_channels: u32,
) -> Result<()> {
    if n_channels > CMS_MAXCHANNELS || n_channels == 0 {
        return Err(Error::Corrupt("LUT8 channel count out of range"));
    }

    let mut curves = Vec::with_capacity(n_channels as usize);
    for _ in 0..n_channels {
        let mut table = vec![0u16; 256];
        for entry in table.iter_mut() {
            *entry = from_8_to_16(r.read_u8()?);
        }
        curves.push(build_tabulated_16(&table));
    }

    lut.insert_stage_at_end(Stage::ToneCurves(curves))
}

/// lcms2 `Read16bitTables` (cmstypes.c:2238-2280): read `n_channels` tables of
/// `n_entries` u16 each (verbatim) and append a `ToneCurves` stage. An empty
/// table (`n_entries == 0`) is a no-op (lcms2 extension); `n_entries == 1` is
/// rejected as malicious; `n_channels > cmsMAXCHANNELS` is rejected.
fn read_16bit_tables<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    n_channels: u32,
    n_entries: u32,
) -> Result<()> {
    if n_entries == 0 {
        return Ok(());
    }
    if n_entries < 2 {
        return Err(Error::Corrupt("LUT16 table needs >= 2 entries"));
    }
    if n_channels > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("LUT16 channel count out of range"));
    }

    let mut curves = Vec::with_capacity(n_channels as usize);
    for _ in 0..n_channels {
        let table = r.read_u16_array(n_entries as usize)?;
        curves.push(build_tabulated_16(&table));
    }

    lut.insert_stage_at_end(Stage::ToneCurves(curves))
}

/// Read the 3x3 matrix (9 s15Fixed16 decoded to f64, exactly lcms2's
/// `_cmsRead15Fixed16Number` loop), and append a `Matrix` stage (3->3, no
/// offset) when `input_channels == 3` and the matrix is NOT identity. The C
/// inserts this BEGIN for LUT8 and END for LUT16, but in both cases the pipeline
/// is empty at this point, so the stage lands first either way.
fn read_matrix<R: ProfileReader>(r: &mut R, lut: &mut Pipeline, input_channels: u32) -> Result<()> {
    let mut m = [0.0f64; 9];
    for slot in m.iter_mut() {
        *slot = r.read_s15f16()?.to_f64();
    }

    if input_channels == 3 && !mat3_is_identity(&m) {
        lut.insert_stage_at_end(Stage::Matrix {
            rows: 3,
            cols: 3,
            m: m.to_vec(),
            offset: None,
        })?;
    }
    Ok(())
}

/// Read the CLUT and append a `Clut` stage. `read_value` reads one grid sample
/// (8-bit widened or 16-bit verbatim). The table holds
/// `CLUTpoints^InputChannels * OutputChannels` u16 in lcms2's row-major layout
/// (output channels innermost). A zero-size table (`CLUTpoints == 0`) is a
/// no-op (the C `nTabSize > 0` guard).
fn read_clut<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    clut_points: u32,
    input_channels: u32,
    output_channels: u32,
    read_value: impl Fn(&mut R) -> Result<u16>,
) -> Result<()> {
    // The mft1/mft2 channel check admits `input_channels == 16` (it bounds by
    // cmsMAXCHANNELS), but a CLUT with > MAX_INPUT_DIMENSIONS (15) inputs is
    // rejected by lcms2's `_cmsComputeInterpParamsEx` (cmsintrp.c:120). Without
    // this guard such a profile would reach `interp_factory`'s 1..=15 selector
    // and panic at eval. Reject here to keep accept/reject parity with lcms2 and
    // make that panic unreachable.
    if input_channels > MAX_INPUT_DIMENSIONS {
        return Err(Error::Range);
    }
    let n_tab_size = uipow(output_channels, clut_points, input_channels)
        .ok_or(Error::Corrupt("LUT CLUT table size overflow"))?;
    if n_tab_size == 0 {
        return Ok(());
    }

    // Bounded reservation hint (n_tab_size is attacker-controlled via the grid);
    // the push loop grows to the true size, so a malformed huge table can't force
    // a giant up-front allocation. Byte-identical on valid input.
    let mut table = Vec::with_capacity((n_tab_size as usize).min(READ_RESERVE_CAP));
    for _ in 0..n_tab_size {
        table.push(read_value(r)?);
    }

    // grid = CLUTpoints per input dimension (cmsStageAllocCLut16bit uniforms).
    let grid = vec![clut_points; input_channels as usize];
    let params = InterpParams::new(&grid, input_channels as usize, output_channels as usize);
    lut.insert_stage_at_end(Stage::Clut(Clut {
        table: ClutTable::U16(table),
        params,
        is_trilinear: false,
        implements_identity: false,
        resolved: Default::default(),
    }))
}

/// lcms2 `Type_LUT8_Read` (cmstypes.c:2002-2100). Builds a pipeline:
/// `[Matrix?] -> InputCurves -> CLUT? -> OutputCurves`.
pub fn read_lut8<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let input_channels = r.read_u8()? as u32;
    let output_channels = r.read_u8()? as u32;
    let clut_points = r.read_u8()? as u32;
    let _pad = r.read_u8()?;

    // CLUTpoints == 1 is impossible (0 = no CLUT, else >= 2).
    if clut_points == 1 {
        return Err(Error::Corrupt("LUT8 CLUTpoints == 1"));
    }
    if input_channels == 0 || input_channels > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("LUT8 input channels out of range"));
    }
    if output_channels == 0 || output_channels > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("LUT8 output channels out of range"));
    }

    let mut lut = Pipeline::new(input_channels as usize, output_channels as usize);

    read_matrix(r, &mut lut, input_channels)?;
    read_8bit_tables(r, &mut lut, input_channels)?;
    read_clut(
        r,
        &mut lut,
        clut_points,
        input_channels,
        output_channels,
        |r| Ok(from_8_to_16(r.read_u8()?)),
    )?;
    read_8bit_tables(r, &mut lut, output_channels)?;

    Ok(Tag::Lut(lut))
}

/// lcms2 `Type_LUT16_Read` (cmstypes.c:2307-2395). Like [`read_lut8`] but reads
/// `InputEntries`/`OutputEntries` u16 counts (after the matrix) and 16-bit
/// tables/CLUT.
pub fn read_lut16<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let input_channels = r.read_u8()? as u32;
    let output_channels = r.read_u8()? as u32;
    let clut_points = r.read_u8()? as u32;
    let _pad = r.read_u8()?;

    if input_channels == 0 || input_channels > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("LUT16 input channels out of range"));
    }
    if output_channels == 0 || output_channels > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("LUT16 output channels out of range"));
    }

    let mut lut = Pipeline::new(input_channels as usize, output_channels as usize);

    read_matrix(r, &mut lut, input_channels)?;

    let input_entries = r.read_u16()? as u32;
    let output_entries = r.read_u16()? as u32;

    if input_entries > 0x7FFF || output_entries > 0x7FFF {
        return Err(Error::Corrupt("LUT16 entry count > 0x7FFF"));
    }
    if clut_points == 1 {
        return Err(Error::Corrupt("LUT16 CLUTpoints == 1"));
    }

    read_16bit_tables(r, &mut lut, input_channels, input_entries)?;
    read_clut(
        r,
        &mut lut,
        clut_points,
        input_channels,
        output_channels,
        |r| r.read_u16(),
    )?;
    read_16bit_tables(r, &mut lut, output_channels, output_entries)?;

    Ok(Tag::Lut(lut))
}

// ---- V4 LutAtoB / LutBtoA readers (offset-based pipelines) ----

/// On-disk tag-type signatures of the two embedded curve flavors accepted by
/// `ReadEmbeddedCurve` (cmstypes.c:2664): `cmsSigCurveType` and
/// `cmsSigParametricCurveType`.
const T_CURVE: u32 = 0x6375_7276; // 'curv'
const T_PARAMETRIC_CURVE: u32 = 0x7061_7261; // 'para'

/// lcms2 `ReadEmbeddedCurve` (cmstypes.c:2664): read the 8-byte type base, then
/// dispatch on the type sig — `curv` → `Type_Curve_Read`, `para` →
/// `Type_ParametricCurve_Read`; any other type is rejected as an unknown
/// extension. Returns the decoded [`ToneCurve`].
fn read_embedded_curve<R: ProfileReader>(r: &mut R) -> Result<ToneCurve> {
    let base_type = r.read_type_base()?;
    let tag = match base_type.to_raw() {
        T_CURVE => read_curve(r, 0)?,
        T_PARAMETRIC_CURVE => read_parametric_curve(r, 0)?,
        _ => return Err(Error::Corrupt("unknown embedded curve type")),
    };
    match tag {
        Tag::Curve(c) => Ok(c),
        // read_curve / read_parametric_curve always return Tag::Curve.
        _ => unreachable!("embedded curve reader returned a non-Curve tag"),
    }
}

/// lcms2 `ReadSetOfCurves` (cmstypes.c:2692): seek to `offset`, read `n_curves`
/// embedded curves (each `ReadEmbeddedCurve` followed by `_cmsReadAlignment`),
/// and append a `ToneCurves` stage. Rejects `n_curves > cmsMAXCHANNELS`.
fn read_set_of_curves<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    offset: u64,
    n_curves: u32,
) -> Result<()> {
    if n_curves > CMS_MAXCHANNELS {
        return Err(Error::Corrupt("AtoB/BtoA curve count out of range"));
    }
    r.seek(offset)?;

    let mut curves = Vec::with_capacity(n_curves as usize);
    for _ in 0..n_curves {
        curves.push(read_embedded_curve(r)?);
        r.read_alignment()?;
    }

    lut.insert_stage_at_end(Stage::ToneCurves(curves))
}

/// lcms2 `ReadMatrix` (cmstypes.c:2566): seek to `offset`, read a 3x3 matrix
/// (9 s15Fixed16) followed by a 3-element offset vector (3 s15Fixed16), and
/// append a 3→3 `Matrix` stage WITH the offset (unlike LUT8/16, the V4 matrix
/// always carries offset terms).
fn read_matrix_with_offset<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    offset: u64,
) -> Result<()> {
    r.seek(offset)?;

    let mut m = [0.0f64; 9];
    for slot in m.iter_mut() {
        *slot = r.read_s15f16()?.to_f64();
    }
    let mut off = [0.0f64; 3];
    for slot in off.iter_mut() {
        *slot = r.read_s15f16()?.to_f64();
    }

    lut.insert_stage_at_end(Stage::Matrix {
        rows: 3,
        cols: 3,
        m: m.to_vec(),
        offset: Some(off.to_vec()),
    })
}

/// lcms2 `ReadCLUT` (cmstypes.c:2601): seek to `offset`, read the 16-byte
/// `gridPoints8` array (a grid-point count per channel; any `== 1` is rejected
/// as impossible), read a 1-byte `Precision` plus 3 pad bytes, and build a CLUT
/// from `cmsMAXCHANNELS` grid dims sliced to `input_channels`. Precision 1 reads
/// 8-bit samples widened via `FROM_8_TO_16`; Precision 2 reads 16-bit verbatim;
/// any other precision is rejected. The table holds
/// `(Π grid[0..input_channels]) * output_channels` u16 in row-major layout.
fn read_clut_at<R: ProfileReader>(
    r: &mut R,
    lut: &mut Pipeline,
    offset: u64,
    input_channels: u32,
    output_channels: u32,
) -> Result<()> {
    r.seek(offset)?;

    let mut grid_points8 = [0u8; CMS_MAXCHANNELS as usize];
    for slot in grid_points8.iter_mut() {
        *slot = r.read_u8()?;
        // 0 means "no CLUT" (handled by the caller's offset gate); 1 is an
        // impossible value lcms2 rejects outright.
        if *slot == 1 {
            return Err(Error::Corrupt("AtoB/BtoA CLUT grid point == 1"));
        }
    }

    let precision = r.read_u8()?;
    let _pad0 = r.read_u8()?;
    let _pad1 = r.read_u8()?;
    let _pad2 = r.read_u8()?;

    // The grid uses the first `input_channels` of the 16 declared dimensions.
    // `take` (vs slicing) is panic-free; `input_channels <= 15 < 16` on valid
    // input, so the collected grid is identical.
    let grid: Vec<u32> = grid_points8
        .iter()
        .take(input_channels as usize)
        .map(|&g| g as u32)
        .collect();

    // nEntries = output_channels × CubeSize(grid) (cmsStageAllocCLut16bitGranular).
    // `CubeSize` applies lcms2's dim<=1 / overflow / UINT_MAX/15 guards; an
    // `n == 0` result is the C `nEntries == 0 -> return NULL`, i.e. reject.
    let n_entries = granular_clut_entries(output_channels, &grid)
        .ok_or(Error::Corrupt("AtoB/BtoA CLUT table size invalid"))?;

    // Bounded reservation hint (n_entries is attacker-controlled via the grid);
    // each branch pushes the true count, so a malformed huge table can't force a
    // giant up-front allocation. Byte-identical on valid input.
    let mut table = Vec::with_capacity((n_entries as usize).min(READ_RESERVE_CAP));
    match precision {
        1 => {
            for _ in 0..n_entries {
                table.push(from_8_to_16(r.read_u8()?));
            }
        }
        2 => {
            for _ in 0..n_entries {
                table.push(r.read_u16()?);
            }
        }
        _ => return Err(Error::Corrupt("AtoB/BtoA CLUT unknown precision")),
    }

    let params = InterpParams::new(&grid, input_channels as usize, output_channels as usize);
    lut.insert_stage_at_end(Stage::Clut(Clut {
        table: ClutTable::U16(table),
        params,
        is_trilinear: false,
        implements_identity: false,
        resolved: Default::default(),
    }))
}

/// Read the shared V4 LUT header: `inputChan` (u8), `outputChan` (u8), a u16 pad,
/// then the five u32 offsets `(offsetB, offsetMat, offsetM, offsetC, offsetA)`.
/// Returns `(input_channels, output_channels, [offsetB, offsetMat, offsetM,
/// offsetC, offsetA])`. Rejects `chan == 0` or `chan >= cmsMAXCHANNELS` (the C
/// `>=` bound, valid range 1..=15).
fn read_a2b_b2a_header<R: ProfileReader>(r: &mut R) -> Result<(u32, u32, [u32; 5])> {
    let input_channels = r.read_u8()? as u32;
    let output_channels = r.read_u8()? as u32;
    let _pad = r.read_u16()?;

    let offset_b = r.read_u32()?;
    let offset_mat = r.read_u32()?;
    let offset_m = r.read_u32()?;
    let offset_c = r.read_u32()?;
    let offset_a = r.read_u32()?;

    if input_channels == 0 || input_channels >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("AtoB/BtoA input channels out of range"));
    }
    if output_channels == 0 || output_channels >= CMS_MAXCHANNELS {
        return Err(Error::Corrupt("AtoB/BtoA output channels out of range"));
    }

    Ok((
        input_channels,
        output_channels,
        [offset_b, offset_mat, offset_m, offset_c, offset_a],
    ))
}

/// lcms2 `Type_LUTA2B_Read` (cmstypes.c:2745). The mAB tag encodes a pipeline as
/// five offset-addressed parts read in a FIXED stage order:
///
/// `A-curves → CLUT → M-curves → Matrix → B-curves`
///
/// Each part is gated on a non-zero offset, all appended `cmsAT_END`. Channel
/// widths: A-curves = inputChan, CLUT = inputChan→outputChan, M-curves =
/// outputChan, Matrix = 3→3, B-curves = outputChan. `BaseOffset` is the tag's
/// on-disk start (`tell - sizeof(_cmsTagBase)`); each offset is relative to it.
pub fn read_lut_a2b<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    // BaseOffset = Tell - sizeof(_cmsTagBase). The reader is positioned just past
    // the 8-byte type base, so its current position is BaseOffset + 8.
    let base_offset = r.tell() - 8;

    let (input_channels, output_channels, [offset_b, offset_mat, offset_m, offset_c, offset_a]) =
        read_a2b_b2a_header(r)?;

    let mut lut = Pipeline::new(input_channels as usize, output_channels as usize);

    if offset_a != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_a as u64, input_channels)?;
    }
    if offset_c != 0 {
        read_clut_at(
            r,
            &mut lut,
            base_offset + offset_c as u64,
            input_channels,
            output_channels,
        )?;
    }
    if offset_m != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_m as u64, output_channels)?;
    }
    if offset_mat != 0 {
        read_matrix_with_offset(r, &mut lut, base_offset + offset_mat as u64)?;
    }
    if offset_b != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_b as u64, output_channels)?;
    }

    Ok(Tag::Lut(lut))
}

/// lcms2 `Type_LUTB2A_Read` (cmstypes.c:3064). Same five-offset header as mAB,
/// but the stage order is REVERSED and the channel widths differ:
///
/// `B-curves → Matrix → M-curves → CLUT → A-curves`
///
/// Channel widths: B-curves = inputChan, Matrix = 3→3, M-curves = inputChan,
/// CLUT = inputChan→outputChan, A-curves = outputChan. (Note B/M use inputChan
/// here, the mirror of mAB.)
pub fn read_lut_b2a<R: ProfileReader>(r: &mut R, _size: u32) -> Result<Tag> {
    let base_offset = r.tell() - 8;

    let (input_channels, output_channels, [offset_b, offset_mat, offset_m, offset_c, offset_a]) =
        read_a2b_b2a_header(r)?;

    let mut lut = Pipeline::new(input_channels as usize, output_channels as usize);

    if offset_b != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_b as u64, input_channels)?;
    }
    if offset_mat != 0 {
        read_matrix_with_offset(r, &mut lut, base_offset + offset_mat as u64)?;
    }
    if offset_m != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_m as u64, input_channels)?;
    }
    if offset_c != 0 {
        read_clut_at(
            r,
            &mut lut,
            base_offset + offset_c as u64,
            input_channels,
            output_channels,
        )?;
    }
    if offset_a != 0 {
        read_set_of_curves(r, &mut lut, base_offset + offset_a as u64, output_channels)?;
    }

    Ok(Tag::Lut(lut))
}

/// Bounded-model-checking proofs (run with `cargo kani`) that the CLUT
/// size-arithmetic — the home of lcms2's integer-overflow CVE class
/// (CVE-2026-41254 `CubeSize`, the IT8/CUBE count-multiplication bugs) — is a
/// total function for every input: it never panics, never overflows, and
/// upholds the headroom bound the granular-CLUT readers depend on. Where
/// fuzzing samples inputs, these *prove* the property over the whole input
/// space. Compiled only under `cfg(kani)`, so normal builds and `cargo test`
/// never see them and no dependency is added.
#[cfg(kani)]
mod kani_proofs {
    use super::{cube_size, granular_clut_entries};

    /// When `cube_size` returns a nonzero count, that count is `<= u32::MAX / 15`.
    /// This is the invariant `granular_clut_entries` relies on: a subsequent
    /// `output_channels * cube` with `output_channels <= 15` then cannot overflow
    /// `u32`. Kani also verifies the `u64` accumulation itself never overflows and
    /// the `as u32` cast never truncates.
    #[kani::proof]
    fn cube_size_result_leaves_headroom() {
        let dims: [u32; 4] = kani::any();
        let r = cube_size(&dims);
        if r != 0 {
            assert!(r <= u32::MAX / 15);
        }
    }

    /// `granular_clut_entries` is total (no panic/overflow) for any grid and any
    /// output-channel count in the validated `1..=15` range.
    #[kani::proof]
    fn granular_clut_entries_total() {
        let output_channels: u32 = kani::any();
        kani::assume(output_channels <= 15);
        let grid: [u32; 4] = kani::any();
        let _ = granular_clut_entries(output_channels, &grid);
    }

    // `uipow` deliberately has no Kani harness. It is already total by
    // construction — every step is `checked_mul`, which returns `None` on
    // overflow rather than panicking — so there is nothing the type system does
    // not already guarantee. It is also intractable for the SAT backend: the loop
    // is a chain of up to `b` symbolic 32-bit multiplications, and bit-blasted
    // nonlinear symbolic multiply chains blow up CBMC. The function that *does*
    // need proving is `cube_size`, whose manual `u64` guards (not `checked_mul`)
    // are where an overflow bug could actually hide — and that proof is above.
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::io::MemReader;

    /// A 16-input mft1 LUT passes the cmsMAXCHANNELS (16) channel-count check but
    /// exceeds the CLUT interpolation limit (MAX_INPUT_DIMENSIONS = 15). lcms2
    /// rejects it (`cmsERROR_RANGE`, cmsintrp.c:120); tintbox must reject it too,
    /// rather than accept it and then panic in `interp_factory` at eval — which
    /// was both a denial-of-service and a bit-identity divergence.
    #[test]
    fn mft1_with_16_input_channels_is_rejected_not_paniced() {
        // input=16, output=1, clut_points=2; the rest (pad + 36-byte matrix +
        // 16*256 input-table bytes) is zero. The CLUT build is reached and
        // rejected before any CLUT data is read.
        let mut body = vec![0u8; 4200];
        body[0] = 16; // input channels
        body[1] = 1; // output channels
        body[2] = 2; // CLUT grid points
        let mut r = MemReader::new(&body);
        assert!(
            matches!(read_lut8(&mut r, body.len() as u32), Err(Error::Range)),
            "16-input mft1 must be rejected with Error::Range"
        );
    }
}
