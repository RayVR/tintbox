//! Float/double pixel formatters, f32 domain.
//!
//! Bit-identical transcriptions of the `*ToFloat` unpack and `*FromFloat` /
//! `*From16` pack routines in `cmspack.c`. The float transform path (lcms2
//! `FloatXFORM`, `cmsxform.c:258`) unpacks one packed pixel into an
//! `[f32; MAX_CHANNELS]`, evaluates the pipeline in float, then packs the
//! `[f32; MAX_CHANNELS]` result back to bytes.
//!
//! Two pack families exist because lcms2 keeps separate stock tables:
//! - **`*FromFloat`** consume the float-evaluated `fOut[]` directly (the
//!   `OutputFormattersFloat` table, used when the *output* format is float).
//! - **`*From16`** (`PackFloatFrom16`/`PackDoubleFrom16`) appear in the float
//!   table for 8/16-bit *output* formats, scaling a 0..1 value by 65535/655.35
//!   into a float/double slot. These are NOT needed by the contiguous
//!   float→packed path here (a float output format always packs via `*FromFloat`),
//!   but are transcribed for the formatter-table parity diffs.
//!
//! Scaling transcribed EXACTLY from cmspack.c (bit-identity-critical):
//! - Lab unpack: `L/100`, `(a+128)/255`, `(b+128)/255`.
//! - Lab pack:   `L*100`, `a*255-128`, `b*255-128`.
//! - XYZ unpack/pack: `v / MAX_ENCODEABLE_XYZ` / `v * MAX_ENCODEABLE_XYZ`.
//! - Generic FLT/DBL: `maximum = IsInkSpace ? 100 : 1`; unpack `v/maximum`,
//!   pack `v*maximum`.
//! - 8/16 → float: `v/255` / `v/65535`.
//!
//! Planar and premultiplied-alpha layouts are out of scope for this task's
//! contiguous-buffer path; the generic functions assume chunky, non-premul.

use super::decode::PixelFormat;
use super::formatters::MAX_CHANNELS;
use crate::compat::floor::{FloorStrategy, Lcms2Floor};
use crate::fixed::from_16_to_8;

/// lcms2 `MAX_ENCODEABLE_XYZ` (lcms2_internal.h:71): `1.0 + 32767/32768`.
pub const MAX_ENCODEABLE_XYZ: f64 = 1.0 + 32767.0 / 32768.0;

/// lcms2 `IsInkSpace` (cmspack.c:1058): true for CMY/CMYK and the MCHn ink
/// spaces (`PT_MCH5..PT_MCH15`). The `PT_*` numeric values: CMY=5, CMYK=6,
/// MCH5..MCH15 = 16..26 (lcms2.h). Anything else returns false.
fn is_ink_space(f: PixelFormat) -> bool {
    let cs = f.colorspace();
    // PT_CMY = 5, PT_CMYK = 6.
    // PT_MCH5..PT_MCH15 = 16..=26 in lcms2.h.
    cs == 5 || cs == 6 || (16..=26).contains(&cs)
}

/// `Unroll8ToFloat` (cmspack.c:1231). 8-bit samples → f32 in `v/255`, with
/// DOSWAP / FLAVOR (`1-v`) / SWAPFIRST / EXTRA. Returns bytes consumed.
pub fn unroll_8_to_float(f: PixelFormat, accum: &[u8], values: &mut [f32; MAX_CHANNELS]) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let swap_first = f.swapfirst();
    let extra = f.extra() as usize;
    let extra_first = do_swap ^ swap_first;
    let mut start = 0usize;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let v = (accum[i + start] as f32) / 255.0f32;
        values[index] = if reverse { 1.0 - v } else { v };
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    (n_chan + extra) * std::mem::size_of::<u8>()
}

/// `Unroll16ToFloat` (cmspack.c:1283). 16-bit samples → f32 in `v/65535`.
/// Native-endian (no ENDIAN16 row in the float table). Returns bytes consumed.
pub fn unroll_16_to_float(f: PixelFormat, accum: &[u8], values: &mut [f32; MAX_CHANNELS]) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let swap_first = f.swapfirst();
    let extra = f.extra() as usize;
    let extra_first = do_swap ^ swap_first;
    let mut start = 0usize;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let raw = read_u16(&accum[(i + start) * 2..]);
        let v = (raw as f32) / 65535.0f32;
        values[index] = if reverse { 1.0 - v } else { v };
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    (n_chan + extra) * std::mem::size_of::<u16>()
}

/// `UnrollFloatsToFloat` (cmspack.c:1335). f32 samples → f32 scaled by
/// `1/maximum` (`maximum = IsInkSpace ? 100 : 1`), DOSWAP / FLAVOR / SWAPFIRST /
/// EXTRA. PREMUL not handled (no covered format sets it). Returns bytes consumed.
pub fn unroll_floats_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let swap_first = f.swapfirst();
    let extra = f.extra() as usize;
    let extra_first = do_swap ^ swap_first;
    let maximum: f32 = if is_ink_space(f) { 100.0 } else { 1.0 };
    let mut start = 0usize;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = read_f32(&accum[(i + start) * 4..]);
        v /= maximum;
        values[index] = if reverse { 1.0 - v } else { v };
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    (n_chan + extra) * std::mem::size_of::<f32>()
}

/// `UnrollDoublesToFloat` (cmspack.c:1402). f64 samples → f32 scaled by
/// `1/maximum` in f64 then truncated to f32. Returns bytes consumed.
pub fn unroll_doubles_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let swap_first = f.swapfirst();
    let extra = f.extra() as usize;
    let extra_first = do_swap ^ swap_first;
    let maximum: f64 = if is_ink_space(f) { 100.0 } else { 1.0 };
    let mut start = 0usize;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = read_f64(&accum[(i + start) * 8..]);
        v /= maximum;
        values[index] = if reverse { (1.0 - v) as f32 } else { v as f32 };
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    (n_chan + extra) * std::mem::size_of::<f64>()
}

/// `UnrollLabFloatToFloat` (cmspack.c:1501), chunky. Lab f32 → 0..1 float.
pub fn unroll_lab_float_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    // C: (cmsFloat32Number)((Pt[1] + 128) / 255.0). Pt[i] is f32, `+128` is done
    // in f32 (int promoted to float), the result then promoted to f64 for `/255`.
    values[0] = (read_f32(&accum[0..]) as f64 / 100.0) as f32;
    values[1] = ((read_f32(&accum[4..]) + 128.0f32) as f64 / 255.0) as f32;
    values[2] = ((read_f32(&accum[8..]) + 128.0f32) as f64 / 255.0) as f32;
    (3 + f.extra() as usize) * std::mem::size_of::<f32>()
}

/// `UnrollLabDoubleToFloat` (cmspack.c:1471), chunky. Lab f64 → 0..1 float.
pub fn unroll_lab_double_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    values[0] = (read_f64(&accum[0..]) / 100.0) as f32;
    values[1] = ((read_f64(&accum[8..]) + 128.0) / 255.0) as f32;
    values[2] = ((read_f64(&accum[16..]) + 128.0) / 255.0) as f32;
    (3 + f.extra() as usize) * std::mem::size_of::<f64>()
}

/// `UnrollXYZFloatToFloat` (cmspack.c:1560), chunky. XYZ f32 → 0..1 float.
pub fn unroll_xyz_float_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    values[0] = (read_f32(&accum[0..]) as f64 / MAX_ENCODEABLE_XYZ) as f32;
    values[1] = (read_f32(&accum[4..]) as f64 / MAX_ENCODEABLE_XYZ) as f32;
    values[2] = (read_f32(&accum[8..]) as f64 / MAX_ENCODEABLE_XYZ) as f32;
    (3 + f.extra() as usize) * std::mem::size_of::<f32>()
}

/// `UnrollXYZDoubleToFloat` (cmspack.c:1531), chunky. XYZ f64 → 0..1 float.
pub fn unroll_xyz_double_to_float(
    f: PixelFormat,
    accum: &[u8],
    values: &mut [f32; MAX_CHANNELS],
) -> usize {
    values[0] = (read_f64(&accum[0..]) / MAX_ENCODEABLE_XYZ) as f32;
    values[1] = (read_f64(&accum[8..]) / MAX_ENCODEABLE_XYZ) as f32;
    values[2] = (read_f64(&accum[16..]) / MAX_ENCODEABLE_XYZ) as f32;
    (3 + f.extra() as usize) * std::mem::size_of::<f64>()
}

// ---- Pack: from float-evaluated fOut[] -------------------------------------

/// `PackBytesFromFloat` (cmspack.c:2954). `v = wOut*65535; FLAVOR; saturate→8`.
pub fn pack_bytes_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let mut start = 0usize;
    let mut last: u8 = 0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 * 65535.0;
        if reverse {
            v = 65535.0 - v;
        }
        let vv = from_16_to_8(Lcms2Floor::quick_saturate_word(v));
        last = vv;
        output[i + start] = vv;
    }

    if extra == 0 && swap_first {
        output.copy_within(0..n_chan - 1, 1);
        output[0] = last;
    }

    (n_chan + extra) * std::mem::size_of::<u8>()
}

/// `PackWordsFromFloat` (cmspack.c:3005). `v = wOut*65535; FLAVOR; saturate→16`.
pub fn pack_words_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let mut start = 0usize;
    let mut last: u16 = 0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 * 65535.0;
        if reverse {
            v = 65535.0 - v;
        }
        let vv = Lcms2Floor::quick_saturate_word(v);
        last = vv;
        write_u16(&mut output[(i + start) * 2..], vv);
    }

    if extra == 0 && swap_first {
        output.copy_within(0..(n_chan - 1) * 2, 2);
        write_u16(&mut output[0..], last);
    }

    (n_chan + extra) * std::mem::size_of::<u16>()
}

/// `PackFloatsFromFloat` (cmspack.c:3057). `v = wOut*maximum; FLAVOR`,
/// `maximum = IsInkSpace ? 100 : 1`. Computes in f64, stores f32.
pub fn pack_floats_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let maximum: f64 = if is_ink_space(f) { 100.0 } else { 1.0 };
    let mut start = 0usize;
    let mut last: f32 = 0.0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 * maximum;
        if reverse {
            v = maximum - v;
        }
        last = v as f32;
        write_f32(&mut output[(i + start) * 4..], v as f32);
    }

    if extra == 0 && swap_first {
        output.copy_within(0..(n_chan - 1) * 4, 4);
        write_f32(&mut output[0..], last);
    }

    (n_chan + extra) * std::mem::size_of::<f32>()
}

/// `PackDoublesFromFloat` (cmspack.c:3108). As [`pack_floats_from_float`] but f64.
pub fn pack_doubles_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let maximum: f64 = if is_ink_space(f) { 100.0 } else { 1.0 };
    let mut start = 0usize;
    let mut last: f64 = 0.0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 * maximum;
        if reverse {
            v = maximum - v;
        }
        last = v;
        write_f64(&mut output[(i + start) * 8..], v);
    }

    if extra == 0 && swap_first {
        output.copy_within(0..(n_chan - 1) * 8, 8);
        write_f64(&mut output[0..], last);
    }

    (n_chan + extra) * std::mem::size_of::<f64>()
}

/// `PackLabFloatFromFloat` (cmspack.c:3160), chunky. 0..1 → Lab f32.
pub fn pack_lab_float_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    write_f32(&mut output[0..], (values[0] as f64 * 100.0) as f32);
    write_f32(&mut output[4..], (values[1] as f64 * 255.0 - 128.0) as f32);
    write_f32(&mut output[8..], (values[2] as f64 * 255.0 - 128.0) as f32);
    (3 + f.extra() as usize) * std::mem::size_of::<f32>()
}

/// `PackLabDoubleFromFloat` (cmspack.c:3190), chunky. 0..1 → Lab f64.
pub fn pack_lab_double_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    write_f64(&mut output[0..], values[0] as f64 * 100.0);
    write_f64(&mut output[8..], values[1] as f64 * 255.0 - 128.0);
    write_f64(&mut output[16..], values[2] as f64 * 255.0 - 128.0);
    (3 + f.extra() as usize) * std::mem::size_of::<f64>()
}

/// `PackXYZFloatFromFloat` (cmspack.c:3292), chunky. 0..1 → XYZ f32.
pub fn pack_xyz_float_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    write_f32(
        &mut output[0..],
        (values[0] as f64 * MAX_ENCODEABLE_XYZ) as f32,
    );
    write_f32(
        &mut output[4..],
        (values[1] as f64 * MAX_ENCODEABLE_XYZ) as f32,
    );
    write_f32(
        &mut output[8..],
        (values[2] as f64 * MAX_ENCODEABLE_XYZ) as f32,
    );
    (3 + f.extra() as usize) * std::mem::size_of::<f32>()
}

/// `PackXYZDoubleFromFloat` (cmspack.c:3322), chunky. 0..1 → XYZ f64.
pub fn pack_xyz_double_from_float(
    f: PixelFormat,
    values: &[f32; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    write_f64(&mut output[0..], values[0] as f64 * MAX_ENCODEABLE_XYZ);
    write_f64(&mut output[8..], values[1] as f64 * MAX_ENCODEABLE_XYZ);
    write_f64(&mut output[16..], values[2] as f64 * MAX_ENCODEABLE_XYZ);
    (3 + f.extra() as usize) * std::mem::size_of::<f64>()
}

// ---- Pack: 8/16-bit output into a float/double slot (the `*From16` table) ---

/// `PackFloatFrom16` (cmspack.c:2899). The 16-bit-eval output written to a float
/// slot, scaled by `1/maximum` (`maximum = IsInkSpace ? 655.35 : 65535`). Used in
/// the float OUTPUT table for non-float output formats; transcribed for parity.
pub fn pack_float_from_16(
    f: PixelFormat,
    values: &[u16; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let maximum: f64 = if is_ink_space(f) { 655.35 } else { 65535.0 };
    let mut start = 0usize;
    let mut last: f32 = 0.0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 / maximum;
        if reverse {
            v = maximum - v;
        }
        last = v as f32;
        write_f32(&mut output[(i + start) * 4..], v as f32);
    }

    if extra == 0 && swap_first {
        output.copy_within(0..(n_chan - 1) * 4, 4);
        write_f32(&mut output[0..], last);
    }

    (n_chan + extra) * std::mem::size_of::<f32>()
}

/// `PackDoubleFrom16` (cmspack.c:2846). As [`pack_float_from_16`] but f64.
pub fn pack_double_from_16(
    f: PixelFormat,
    values: &[u16; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let n_chan = f.channels() as usize;
    let do_swap = f.doswap();
    let reverse = f.flavor();
    let extra = f.extra() as usize;
    let swap_first = f.swapfirst();
    let extra_first = do_swap ^ swap_first;
    let maximum: f64 = if is_ink_space(f) { 655.35 } else { 65535.0 };
    let mut start = 0usize;
    let mut last: f64 = 0.0;

    if extra_first {
        start = extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index] as f64 / maximum;
        if reverse {
            v = maximum - v;
        }
        last = v;
        write_f64(&mut output[(i + start) * 8..], v);
    }

    if extra == 0 && swap_first {
        output.copy_within(0..(n_chan - 1) * 8, 8);
        write_f64(&mut output[0..], last);
    }

    (n_chan + extra) * std::mem::size_of::<f64>()
}

// ---- Little-endian scalar reads/writes (host is LE; lcms2 reads native) -----

#[inline]
fn read_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
#[inline]
fn write_u16(b: &mut [u8], v: u16) {
    b[..2].copy_from_slice(&v.to_le_bytes());
}
#[inline]
fn read_f32(b: &[u8]) -> f32 {
    f32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
#[inline]
fn write_f32(b: &mut [u8], v: f32) {
    b[..4].copy_from_slice(&v.to_le_bytes());
}
#[inline]
fn read_f64(b: &[u8]) -> f64 {
    f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
#[inline]
fn write_f64(b: &mut [u8], v: f64) {
    b[..8].copy_from_slice(&v.to_le_bytes());
}
