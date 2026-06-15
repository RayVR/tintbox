//! Alpha / extra-channel copy for `cmsFLAGS_COPY_ALPHA` (lcms2 `cmsalpha.c`).
//!
//! When a transform carries extra (non-color) channels — e.g. the `A` in
//! RGBA→RGBA — and `cmsFLAGS_COPY_ALPHA` is set, lcms2 copies those extra
//! channels straight from input to output, converting their sample depth
//! (8↔16↔half↔float↔double) but NOT color-transforming them. This module
//! transcribes the two pieces that make that work:
//!
//! * [`alpha_copy_fn`] — `_cmsGetFormatterAlpha` (cmsalpha.c:378): given the
//!   in/out format words it returns the per-sample copier `(dst, src)` from the
//!   6×6 `FormattersAlpha` table (rows/cols indexed by `FormatterPos`).
//! * [`extra_offsets`] — `ComputeIncrementsForChunky` (cmsalpha.c:405)
//!   restricted to the chunky (non-planar) layout: the per-extra-channel byte
//!   offset within one packed pixel, honoring `T_DOSWAP` / `T_SWAPFIRST`.
//!
//! Planar layout (`T_PLANAR`) is **deferred**: `do_transform` only handles
//! contiguous chunky buffers, mirroring lcms2's `ComputeIncrementsForPlanar`
//! being a separate path. [`extra_offsets`] returns `None` for planar formats.

use crate::fixed::{from_16_to_8, from_8_to_16};
use crate::format::decode::PixelFormat;
use crate::format::formatters::MAX_CHANNELS;
use crate::math::half::{float_to_half, half_to_float};

/// lcms2 `_cmsQuickSaturateByte` (cmsalpha.c:36): `floor(d + 0.5)` clamped to
/// `0..=255`. The interior floor reuses `_cmsQuickFloorWord` (the value is in
/// `0..255` here, well within its `0..65535` domain), so we route through the
/// same `Lcms2Floor` magic-constant floor for bit-identity.
fn quick_saturate_byte(d: f64) -> u8 {
    use crate::compat::{FloorStrategy, Lcms2Floor};
    let d = d + 0.5;
    if d <= 0.0 {
        return 0;
    }
    if d >= 255.0 {
        return 255;
    }
    Lcms2Floor::quick_floor_word(d) as u8
}

fn quick_saturate_word(d: f64) -> u16 {
    use crate::compat::{FloorStrategy, Lcms2Floor};
    Lcms2Floor::quick_saturate_word(d)
}

/// `FormatterPos` (cmsalpha.c:351): the row/column index of `fmt` in the
/// `FormattersAlpha` table. `0`=8, `1`=16, `2`=16SE, `3`=HLF, `4`=FLT, `5`=DBL.
/// Returns `None` for unrecognized widths (lcms2 signals an error and bails).
fn formatter_pos(fmt: PixelFormat) -> Option<usize> {
    let b = fmt.bytes();
    let float = fmt.is_float();
    if b == 0 && float {
        return Some(5); // DBL
    }
    if b == 2 && float {
        return Some(3); // HLF
    }
    if b == 4 && float {
        return Some(4); // FLT
    }
    if b == 2 && !float {
        return Some(if fmt.endian16() { 2 } else { 1 }); // 16SE / 16
    }
    if b == 1 && !float {
        return Some(0); // 8
    }
    None
}

/// Big-endian byte swap of a 16-bit word (lcms2 `CHANGE_ENDIAN`, cmsalpha.c:32).
fn change_endian(w: u16) -> u16 {
    w.rotate_left(8)
}

/// One per-sample alpha copier `(dst, src)`, transcribed verbatim from the
/// `FormattersAlpha[6][6]` table (cmsalpha.c:380). Each reads one source sample
/// from `src` and writes one converted destination sample to `dst`; the slices
/// must hold at least the respective sample width.
pub type AlphaCopyFn = fn(&mut [u8], &[u8]);

#[inline]
fn rd16(src: &[u8]) -> u16 {
    u16::from_ne_bytes([src[0], src[1]])
}
#[inline]
fn wr16(dst: &mut [u8], v: u16) {
    dst[..2].copy_from_slice(&v.to_ne_bytes());
}
#[inline]
fn rd_f32(src: &[u8]) -> f32 {
    f32::from_ne_bytes([src[0], src[1], src[2], src[3]])
}
#[inline]
fn wr_f32(dst: &mut [u8], v: f32) {
    dst[..4].copy_from_slice(&v.to_ne_bytes());
}
#[inline]
fn rd_f64(src: &[u8]) -> f64 {
    f64::from_ne_bytes([
        src[0], src[1], src[2], src[3], src[4], src[5], src[6], src[7],
    ])
}
#[inline]
fn wr_f64(dst: &mut [u8], v: f64) {
    dst[..8].copy_from_slice(&v.to_ne_bytes());
}

// --- From 8 ----------------------------------------------------------------
fn copy8(dst: &mut [u8], src: &[u8]) {
    dst[0] = src[0];
}
fn from8to16(dst: &mut [u8], src: &[u8]) {
    wr16(dst, from_8_to_16(src[0]));
}
fn from8to16_se(dst: &mut [u8], src: &[u8]) {
    wr16(dst, change_endian(from_8_to_16(src[0])));
}
fn from8to_flt(dst: &mut [u8], src: &[u8]) {
    wr_f32(dst, src[0] as f32 / 255.0);
}
fn from8to_dbl(dst: &mut [u8], src: &[u8]) {
    wr_f64(dst, src[0] as f64 / 255.0);
}
fn from8to_hlf(dst: &mut [u8], src: &[u8]) {
    let n = src[0] as f32 / 255.0;
    wr16(dst, float_to_half(n));
}

// --- From 16 ---------------------------------------------------------------
fn from16to8(dst: &mut [u8], src: &[u8]) {
    dst[0] = from_16_to_8(rd16(src));
}
fn from16se_to8(dst: &mut [u8], src: &[u8]) {
    dst[0] = from_16_to_8(change_endian(rd16(src)));
}
fn copy16(dst: &mut [u8], src: &[u8]) {
    dst[..2].copy_from_slice(&src[..2]);
}
fn from16to16(dst: &mut [u8], src: &[u8]) {
    wr16(dst, change_endian(rd16(src)));
}
fn from16to_flt(dst: &mut [u8], src: &[u8]) {
    wr_f32(dst, rd16(src) as f32 / 65535.0);
}
fn from16se_to_flt(dst: &mut [u8], src: &[u8]) {
    wr_f32(dst, change_endian(rd16(src)) as f32 / 65535.0);
}
fn from16to_dbl(dst: &mut [u8], src: &[u8]) {
    wr_f64(dst, rd16(src) as f64 / 65535.0);
}
fn from16se_to_dbl(dst: &mut [u8], src: &[u8]) {
    wr_f64(dst, change_endian(rd16(src)) as f64 / 65535.0);
}
fn from16to_hlf(dst: &mut [u8], src: &[u8]) {
    let n = rd16(src) as f32 / 65535.0;
    wr16(dst, float_to_half(n));
}
fn from16se_to_hlf(dst: &mut [u8], src: &[u8]) {
    let n = change_endian(rd16(src)) as f32 / 65535.0;
    wr16(dst, float_to_half(n));
}

// --- From HALF -------------------------------------------------------------
fn from_hlf_to8(dst: &mut [u8], src: &[u8]) {
    let n = half_to_float(rd16(src));
    dst[0] = quick_saturate_byte(n as f64 * 255.0);
}
fn from_hlf_to16(dst: &mut [u8], src: &[u8]) {
    let n = half_to_float(rd16(src));
    wr16(dst, quick_saturate_word(n as f64 * 65535.0));
}
fn from_hlf_to16se(dst: &mut [u8], src: &[u8]) {
    let n = half_to_float(rd16(src));
    let i = quick_saturate_word(n as f64 * 65535.0);
    wr16(dst, change_endian(i));
}
fn from_hlf_to_flt(dst: &mut [u8], src: &[u8]) {
    wr_f32(dst, half_to_float(rd16(src)));
}
fn from_hlf_to_dbl(dst: &mut [u8], src: &[u8]) {
    wr_f64(dst, half_to_float(rd16(src)) as f64);
}

// --- From FLOAT ------------------------------------------------------------
fn from_flt_to8(dst: &mut [u8], src: &[u8]) {
    let n = rd_f32(src);
    dst[0] = quick_saturate_byte(n as f64 * 255.0);
}
fn from_flt_to16(dst: &mut [u8], src: &[u8]) {
    let n = rd_f32(src);
    wr16(dst, quick_saturate_word(n as f64 * 65535.0));
}
fn from_flt_to16se(dst: &mut [u8], src: &[u8]) {
    let n = rd_f32(src);
    let i = quick_saturate_word(n as f64 * 65535.0);
    wr16(dst, change_endian(i));
}
fn copy32(dst: &mut [u8], src: &[u8]) {
    dst[..4].copy_from_slice(&src[..4]);
}
fn from_flt_to_dbl(dst: &mut [u8], src: &[u8]) {
    wr_f64(dst, rd_f32(src) as f64);
}
fn from_flt_to_hlf(dst: &mut [u8], src: &[u8]) {
    wr16(dst, float_to_half(rd_f32(src)));
}

// --- From DOUBLE -----------------------------------------------------------
fn from_dbl_to8(dst: &mut [u8], src: &[u8]) {
    let n = rd_f64(src);
    dst[0] = quick_saturate_byte(n * 255.0);
}
fn from_dbl_to16(dst: &mut [u8], src: &[u8]) {
    // lcms2 writes `n * 65535.0f`; in C the float literal promotes to double, and
    // 65535.0 is exactly representable, so this is the plain `n * 65535.0` double
    // multiply.
    let n = rd_f64(src);
    wr16(dst, quick_saturate_word(n * 65535.0));
}
fn from_dbl_to16se(dst: &mut [u8], src: &[u8]) {
    let n = rd_f64(src);
    let i = quick_saturate_word(n * 65535.0);
    wr16(dst, change_endian(i));
}
fn from_dbl_to_flt(dst: &mut [u8], src: &[u8]) {
    wr_f32(dst, rd_f64(src) as f32);
}
fn from_dbl_to_hlf(dst: &mut [u8], src: &[u8]) {
    let n = rd_f64(src) as f32;
    wr16(dst, float_to_half(n));
}
fn copy64(dst: &mut [u8], src: &[u8]) {
    dst[..8].copy_from_slice(&src[..8]);
}

/// `_cmsGetFormatterAlpha` (cmsalpha.c:378): the per-sample alpha copier for the
/// in/out formats, or `None` if either width is unrecognized.
pub fn alpha_copy_fn(in_fmt: u32, out_fmt: u32) -> Option<AlphaCopyFn> {
    // FormattersAlpha[from][to] (cmsalpha.c:380), transcribed verbatim.
    #[rustfmt::skip]
    const TABLE: [[AlphaCopyFn; 6]; 6] = [
        /* from 8   */ [copy8,        from8to16,   from8to16_se,   from8to_hlf,    from8to_flt,    from8to_dbl ],
        /* from 16  */ [from16to8,    copy16,      from16to16,     from16to_hlf,   from16to_flt,   from16to_dbl],
        /* from 16SE*/ [from16se_to8, from16to16,  copy16,         from16se_to_hlf,from16se_to_flt,from16se_to_dbl],
        /* from HLF */ [from_hlf_to8, from_hlf_to16,from_hlf_to16se,copy16,        from_hlf_to_flt,from_hlf_to_dbl],
        /* from FLT */ [from_flt_to8, from_flt_to16,from_flt_to16se,from_flt_to_hlf,copy32,        from_flt_to_dbl],
        /* from DBL */ [from_dbl_to8, from_dbl_to16,from_dbl_to16se,from_dbl_to_hlf,from_dbl_to_flt,copy64 ],
    ];
    let in_n = formatter_pos(PixelFormat(in_fmt))?;
    let out_n = formatter_pos(PixelFormat(out_fmt))?;
    Some(TABLE[in_n][out_n])
}

/// True sample size in bytes (lcms2 `trueBytesSize`, cmsalpha.c:48): `T_BYTES`,
/// except `0` (double) means 8.
fn true_bytes_size(fmt: PixelFormat) -> usize {
    match fmt.bytes() {
        0 => 8,
        b => b as usize,
    }
}

/// `ComputeIncrementsForChunky` (cmsalpha.c:405), restricted to the per-pixel
/// starting byte offset of each extra channel within one packed (chunky) pixel.
/// Returns `(offsets, count)` where `offsets[..count]` are the byte offsets of
/// the `T_EXTRA` extra channels, or `None` for planar formats / out-of-range
/// channel counts (caller treats `None` as "no copy possible").
///
/// We omit the inter-pixel `ComponentPointerIncrements` (always `pixelSize`),
/// since `do_transform` strides by the whole pixel itself.
pub fn extra_offsets(fmt: u32) -> Option<([usize; MAX_CHANNELS], usize)> {
    let f = PixelFormat(fmt);
    if f.planar() {
        return None; // planar deferred
    }
    let extra = f.extra() as usize;
    let nchannels = f.channels() as usize;
    let total_chans = nchannels + extra;
    if total_chans == 0 || total_chans >= MAX_CHANNELS {
        return None;
    }

    let channel_size = true_bytes_size(f);

    // channels[i] = logical position i, after DOSWAP / SWAPFIRST (cmsalpha.c:428).
    let mut channels = [0usize; MAX_CHANNELS];
    for (i, c) in channels.iter_mut().enumerate().take(total_chans) {
        *c = if f.doswap() { total_chans - i - 1 } else { i };
    }

    // SWAPFIRST: ROL of positions (cmsalpha.c:439).
    if f.swapfirst() && total_chans > 1 {
        let tmp = channels[0];
        for i in 0..total_chans - 1 {
            channels[i] = channels[i + 1];
        }
        channels[total_chans - 1] = tmp;
    }

    // Scale by channel size (cmsalpha.c:449).
    if channel_size > 1 {
        for c in channels.iter_mut().take(total_chans) {
            *c *= channel_size;
        }
    }

    // The extra channels are the last `extra` entries (cmsalpha.c:454).
    let mut offsets = [0usize; MAX_CHANNELS];
    offsets[..extra].copy_from_slice(&channels[nchannels..nchannels + extra]);
    Some((offsets, extra))
}

/// Precomputed extra-channel copy plan for a transform: the per-sample copier
/// plus the source/destination byte offsets within one packed pixel and the
/// number of extra channels actually copied (`min(in_extra, out_extra)`).
///
/// Built once at transform construction via [`AlphaCopyPlan::build`]; applied
/// per pixel by [`AlphaCopyPlan::copy_pixel`].
pub struct AlphaCopyPlan {
    copy_fn: AlphaCopyFn,
    src_offsets: [usize; MAX_CHANNELS],
    dst_offsets: [usize; MAX_CHANNELS],
    n_extra: usize,
}

impl AlphaCopyPlan {
    /// Build the copy plan for `in_fmt` → `out_fmt`, or `None` if no extra-channel
    /// copy applies (either side has no extra channels, a planar layout, or an
    /// unrecognized sample width). Mirrors `_cmsHandleExtraChannels`'s guards
    /// (cmsalpha.c:561-579): equal/non-zero extra counts and a valid alpha
    /// formatter. The `min` of the two extra counts is copied so a mismatch is
    /// memory-safe (lcms2 returns early on a mismatch; we conservatively copy the
    /// common channels).
    pub fn build(in_fmt: u32, out_fmt: u32) -> Option<AlphaCopyPlan> {
        let in_extra = PixelFormat(in_fmt).extra() as usize;
        let out_extra = PixelFormat(out_fmt).extra() as usize;
        if in_extra == 0 || out_extra == 0 {
            return None;
        }
        let (src_offsets, src_n) = extra_offsets(in_fmt)?;
        let (dst_offsets, dst_n) = extra_offsets(out_fmt)?;
        let copy_fn = alpha_copy_fn(in_fmt, out_fmt)?;
        Some(AlphaCopyPlan {
            copy_fn,
            src_offsets,
            dst_offsets,
            n_extra: src_n.min(dst_n),
        })
    }

    /// Copy the extra channels of one packed pixel from `src_pixel` to
    /// `dst_pixel`, depth-converting per the plan's copier. `src_pixel` /
    /// `dst_pixel` must each hold at least one packed pixel of the respective
    /// format (the offsets index within one pixel).
    pub fn copy_pixel(&self, src_pixel: &[u8], dst_pixel: &mut [u8]) {
        for k in 0..self.n_extra {
            let s = self.src_offsets[k];
            let d = self.dst_offsets[k];
            (self.copy_fn)(&mut dst_pixel[d..], &src_pixel[s..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::decode::*;

    #[test]
    fn formatter_pos_widths() {
        assert_eq!(formatter_pos(PixelFormat(TYPE_RGBA_8)), Some(0));
        assert_eq!(formatter_pos(PixelFormat(TYPE_RGBA_16)), Some(1));
        assert_eq!(formatter_pos(PixelFormat(TYPE_RGBA_FLT)), Some(4));
        // 16SE
        let se = TYPE_RGBA_16 | (1 << 11);
        assert_eq!(formatter_pos(PixelFormat(se)), Some(2));
    }

    #[test]
    fn rgba8_extra_offset_is_after_color() {
        // RGBA_8: 3 color + 1 extra, channel size 1 -> alpha at byte 3.
        let (off, n) = extra_offsets(TYPE_RGBA_8).unwrap();
        assert_eq!(n, 1);
        assert_eq!(off[0], 3);
    }

    #[test]
    fn argb8_extra_offset_is_first() {
        // ARGB_8 (SWAPFIRST): the alpha is the FIRST byte (offset 0).
        let (off, n) = extra_offsets(TYPE_ARGB_8).unwrap();
        assert_eq!(n, 1);
        assert_eq!(off[0], 0);
    }

    #[test]
    fn rgba16_extra_offset_scaled() {
        // RGBA_16: 3 color + 1 extra, channel size 2 -> alpha at byte 6.
        let (off, n) = extra_offsets(TYPE_RGBA_16).unwrap();
        assert_eq!(n, 1);
        assert_eq!(off[0], 6);
    }

    #[test]
    fn cmyka8_extra_offset() {
        let (off, n) = extra_offsets(TYPE_CMYKA_8).unwrap();
        assert_eq!(n, 1);
        assert_eq!(off[0], 4);
    }

    #[test]
    fn planar_is_deferred() {
        let planar = TYPE_RGBA_8 | (1 << 12);
        assert!(extra_offsets(planar).is_none());
    }

    #[test]
    fn copy8_8_to_16_uses_from_8_to_16() {
        let f = alpha_copy_fn(TYPE_RGBA_8, TYPE_RGBA_16).unwrap();
        let mut dst = [0u8; 2];
        f(&mut dst, &[0xABu8]);
        assert_eq!(rd16(&dst), from_8_to_16(0xAB));
    }
}
