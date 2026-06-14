//! LUT8 / LUT16 tag readers (lcms2 `Type_LUT8_Read` / `Type_LUT16_Read`,
//! cmstypes.c:2002-2100 / 2307-2395). Both build a `cmsPipeline` from a fixed
//! structure: an optional 3x3 matrix, an input tone-curve set, a CLUT, and an
//! output tone-curve set.
//!
//! The two differ only in the on-disk sample width: LUT8 reads 256-entry 8-bit
//! curve tables and an 8-bit CLUT (each value widened with `FROM_8_TO_16`), and
//! its header has no entry counts; LUT16 reads `InputEntries`/`OutputEntries`
//! u16 counts after `CLUTpoints` and stores 16-bit tables/CLUT verbatim.

use crate::curve::build_tabulated_16;
use crate::error::{Error, Result};
use crate::interp::InterpParams;
use crate::io::ProfileReader;
use crate::pipeline::clut::{Clut, ClutTable};
use crate::pipeline::{Pipeline, Stage};
use crate::profile::tag::Tag;

/// lcms2 `cmsMAXCHANNELS` (lcms2.h:113): the channel-count ceiling both LUT
/// readers reject above.
const CMS_MAXCHANNELS: u32 = 16;

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
    let n_tab_size = uipow(output_channels, clut_points, input_channels)
        .ok_or(Error::Corrupt("LUT CLUT table size overflow"))?;
    if n_tab_size == 0 {
        return Ok(());
    }

    let mut table = vec![0u16; n_tab_size as usize];
    for slot in table.iter_mut() {
        *slot = read_value(r)?;
    }

    // grid = CLUTpoints per input dimension (cmsStageAllocCLut16bit uniforms).
    let grid = vec![clut_points; input_channels as usize];
    let params = InterpParams::new(&grid, input_channels as usize, output_channels as usize);
    lut.insert_stage_at_end(Stage::Clut(Clut {
        table: ClutTable::U16(table),
        params,
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
