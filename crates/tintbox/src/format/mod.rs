//! ICC pixel-format decoding and the common 8/16-bit unpack/pack formatters.
//!
//! [`decode`] holds the format-word decoder ([`PixelFormat`]) and the `TYPE_*`
//! constants; [`formatters`] holds the unpack/pack routines transcribed from
//! lcms2's `cmspack.c`. [`get_input_formatter`] / [`get_output_formatter`]
//! select a formatter for a format word, mirroring lcms2's stock table match.

pub mod alpha;
pub mod decode;
pub mod float;
pub mod formatters;

pub use alpha::AlphaCopyPlan;
pub use decode::{PixelFormat, TYPE_NAMED_COLOR_INDEX};
pub use formatters::MAX_CHANNELS;

/// An unpack formatter: read one packed pixel from `accum` into `values`,
/// returning the number of bytes consumed.
pub type UnpackFn = Box<dyn Fn(&[u8], &mut [u16; MAX_CHANNELS]) -> usize + Send + Sync>;

/// A pack formatter: write one pixel of `values` into `output`, returning the
/// number of bytes produced.
pub type PackFn = Box<dyn Fn(&[u16; MAX_CHANNELS], &mut [u8]) -> usize + Send + Sync>;

/// A float unpack formatter: read one packed pixel into an `f32` value array,
/// returning the number of bytes consumed.
pub type UnpackFloatFn = Box<dyn Fn(&[u8], &mut [f32; MAX_CHANNELS]) -> usize + Send + Sync>;

/// A float pack formatter: write one pixel of `f32` values into `output`,
/// returning the number of bytes produced.
pub type PackFloatFn = Box<dyn Fn(&[f32; MAX_CHANNELS], &mut [u8]) -> usize + Send + Sync>;

/// lcms2 `_cmsFormatterIsFloat` (cmspack.c:4011): a format uses the float
/// (`FloatXFORM`) path iff its `T_FLOAT` bit is set. Note both `*_FLT` (T_BYTES=4)
/// and `*_DBL` (T_BYTES=0) formats carry the float bit.
pub fn formatter_is_float(fmt: u32) -> bool {
    PixelFormat(fmt).is_float()
}

/// Select an unpack formatter for `fmt`, or `None` if this task does not handle
/// it (float/double, planar, premul, named-color, etc. — later tasks).
///
/// Mirrors lcms2's stock `InputFormatters16` table: the plain fixed-channel
/// cases (3/4 channel, no extra/swap/flavor) use the specialized unrollers; all
/// other byte/word chunky combinations fall through to the generic
/// `UnrollChunkyBytes` / `UnrollAnyWords`.
pub fn get_input_formatter(fmt: u32) -> Option<UnpackFn> {
    let f = PixelFormat(fmt);

    // Unsupported here: float/double samples, planar layout, premultiplied alpha.
    if f.is_float() || f.bytes() == 0 || f.planar() || f.premul() {
        return None;
    }

    let n_chan = f.channels() as usize;
    let extra = f.extra() as usize;
    let do_swap = f.doswap();
    let swap_first = f.swapfirst();
    let reverse = f.flavor();
    let endian = f.endian16();

    if n_chan == 0 || n_chan + extra > MAX_CHANNELS {
        return None;
    }

    match f.bytes() {
        1 => {
            // 8-bit. The 1-channel (gray) rows replicate L into wIn[0..2] —
            // match them exactly per lcms2's InputFormatters16, before the
            // generic UnrollChunkyBytes fallthrough.
            if n_chan == 1 && !do_swap && !swap_first && !endian {
                match (extra, reverse) {
                    (0, false) => return Some(Box::new(formatters::unroll_1_byte)),
                    (0, true) => return Some(Box::new(formatters::unroll_1_byte_reversed)),
                    (1, false) => return Some(Box::new(formatters::unroll_1_byte_skip1)),
                    _ => {}
                }
            }
            // Fast path for the plain 3/4-channel cases lcms2 specializes.
            if extra == 0 && !do_swap && !swap_first && !reverse {
                match n_chan {
                    3 => return Some(Box::new(formatters::unroll_3_bytes)),
                    4 => return Some(Box::new(formatters::unroll_4_bytes)),
                    _ => {}
                }
            }
            Some(Box::new(move |accum, values| {
                formatters::unroll_chunky_bytes(
                    n_chan, extra, do_swap, swap_first, reverse, accum, values,
                )
            }))
        }
        2 => {
            // 16-bit. The 1-channel rows that replicate: GRAY_16 (Unroll1Word)
            // and GRAY_16_REV (Unroll1WordReversed). GRAYA_16 (extra) and
            // GRAY_16_SE (endian) have no 1-channel row and fall through to the
            // generic UnrollAnyWords, which does not replicate.
            if n_chan == 1 && extra == 0 && !do_swap && !swap_first && !endian {
                if reverse {
                    return Some(Box::new(formatters::unroll_1_word_reversed));
                } else {
                    return Some(Box::new(formatters::unroll_1_word));
                }
            }
            // Fast path for plain 3/4-channel native-endian cases.
            if extra == 0 && !do_swap && !swap_first && !reverse && !endian {
                match n_chan {
                    3 => return Some(Box::new(formatters::unroll_3_words)),
                    4 => return Some(Box::new(formatters::unroll_4_words)),
                    _ => {}
                }
            }
            Some(Box::new(move |accum, values| {
                formatters::unroll_any_words(
                    n_chan, extra, do_swap, swap_first, reverse, endian, accum, values,
                )
            }))
        }
        _ => None,
    }
}

/// Select a pack formatter for `fmt`, or `None` if unhandled (see
/// [`get_input_formatter`]). Mirrors lcms2's stock `OutputFormatters16` table.
pub fn get_output_formatter(fmt: u32) -> Option<PackFn> {
    let f = PixelFormat(fmt);

    if f.is_float() || f.bytes() == 0 || f.planar() || f.premul() {
        return None;
    }

    let n_chan = f.channels() as usize;
    let extra = f.extra() as usize;
    let do_swap = f.doswap();
    let swap_first = f.swapfirst();
    let reverse = f.flavor();
    let endian = f.endian16();

    if n_chan == 0 || n_chan + extra > MAX_CHANNELS {
        return None;
    }

    match f.bytes() {
        1 => {
            if extra == 0 && !do_swap && !swap_first && !reverse {
                match n_chan {
                    3 => return Some(Box::new(formatters::pack_3_bytes)),
                    4 => return Some(Box::new(formatters::pack_4_bytes)),
                    _ => {}
                }
            }
            Some(Box::new(move |values, output| {
                formatters::pack_chunky_bytes(
                    n_chan, extra, do_swap, swap_first, reverse, values, output,
                )
            }))
        }
        2 => {
            if extra == 0 && !do_swap && !swap_first && !reverse && !endian {
                match n_chan {
                    3 => return Some(Box::new(formatters::pack_3_words)),
                    4 => return Some(Box::new(formatters::pack_4_words)),
                    _ => {}
                }
            }
            Some(Box::new(move |values, output| {
                formatters::pack_chunky_words(
                    n_chan, extra, do_swap, swap_first, reverse, endian, values, output,
                )
            }))
        }
        _ => None,
    }
}

/// Select a float unpack formatter for `fmt`, or `None` if unhandled.
///
/// Mirrors lcms2's stock `InputFormattersFloat` table (cmspack.c:3647): the
/// Lab/XYZ float+double rows scale into 0..1; the generic `FLOAT_SH(1)` rows
/// dispatch to [`float::unroll_floats_to_float`] (T_BYTES=4) or
/// [`float::unroll_doubles_to_float`] (T_BYTES=0); 8/16-bit input formats
/// (non-float words) map to [`float::unroll_8_to_float`] / `unroll_16_to_float`
/// (lcms2 selects these for `CMS_PACK_FLAGS_FLOAT` over an integer input). Planar
/// and premultiplied layouts are out of scope here.
pub fn get_input_formatter_float(fmt: u32) -> Option<UnpackFloatFn> {
    let f = PixelFormat(fmt);
    if f.planar() || f.premul() {
        return None;
    }
    let n_chan = f.channels() as usize;
    let extra = f.extra() as usize;
    if n_chan == 0 || n_chan + extra > MAX_CHANNELS {
        return None;
    }

    // Lab/XYZ float + double specialized rows (3-channel, scaled).
    if f.colorspace() == decode::PT_LAB && n_chan == 3 {
        if f.is_float() && f.bytes() == 4 {
            return Some(Box::new(move |a, v| {
                float::unroll_lab_float_to_float(f, a, v)
            }));
        }
        if f.is_float() && f.bytes() == 0 {
            return Some(Box::new(move |a, v| {
                float::unroll_lab_double_to_float(f, a, v)
            }));
        }
    }
    if f.colorspace() == decode::PT_XYZ && n_chan == 3 {
        if f.is_float() && f.bytes() == 4 {
            return Some(Box::new(move |a, v| {
                float::unroll_xyz_float_to_float(f, a, v)
            }));
        }
        if f.is_float() && f.bytes() == 0 {
            return Some(Box::new(move |a, v| {
                float::unroll_xyz_double_to_float(f, a, v)
            }));
        }
    }

    if f.is_float() {
        // Generic float (T_BYTES=4) / double (T_BYTES=0).
        match f.bytes() {
            4 => Some(Box::new(move |a, v| float::unroll_floats_to_float(f, a, v))),
            0 => Some(Box::new(move |a, v| {
                float::unroll_doubles_to_float(f, a, v)
            })),
            _ => None,
        }
    } else {
        // Integer input under the float path (8/16-bit → float).
        match f.bytes() {
            1 => Some(Box::new(move |a, v| float::unroll_8_to_float(f, a, v))),
            2 => Some(Box::new(move |a, v| float::unroll_16_to_float(f, a, v))),
            _ => None,
        }
    }
}

/// Select a float pack formatter for `fmt`, or `None` if unhandled.
///
/// Mirrors lcms2's stock `OutputFormattersFloat` table (cmspack.c:3815): the
/// Lab/XYZ rows; the generic `FLOAT_SH(1)` rows ([`float::pack_floats_from_float`]
/// / `pack_doubles_from_float`); and the 8/16-bit output rows, which in lcms2's
/// float table are [`float::pack_bytes_from_float`] / `pack_words_from_float`
/// (consuming the float-evaluated `fOut[]`). Planar/premul out of scope.
pub fn get_output_formatter_float(fmt: u32) -> Option<PackFloatFn> {
    let f = PixelFormat(fmt);
    if f.planar() || f.premul() {
        return None;
    }
    let n_chan = f.channels() as usize;
    let extra = f.extra() as usize;
    if n_chan == 0 || n_chan + extra > MAX_CHANNELS {
        return None;
    }

    if f.colorspace() == decode::PT_LAB && n_chan == 3 {
        if f.is_float() && f.bytes() == 4 {
            return Some(Box::new(move |v, o| {
                float::pack_lab_float_from_float(f, v, o)
            }));
        }
        if f.is_float() && f.bytes() == 0 {
            return Some(Box::new(move |v, o| {
                float::pack_lab_double_from_float(f, v, o)
            }));
        }
    }
    if f.colorspace() == decode::PT_XYZ && n_chan == 3 {
        if f.is_float() && f.bytes() == 4 {
            return Some(Box::new(move |v, o| {
                float::pack_xyz_float_from_float(f, v, o)
            }));
        }
        if f.is_float() && f.bytes() == 0 {
            return Some(Box::new(move |v, o| {
                float::pack_xyz_double_from_float(f, v, o)
            }));
        }
    }

    if f.is_float() {
        match f.bytes() {
            4 => Some(Box::new(move |v, o| float::pack_floats_from_float(f, v, o))),
            0 => Some(Box::new(move |v, o| {
                float::pack_doubles_from_float(f, v, o)
            })),
            _ => None,
        }
    } else {
        match f.bytes() {
            1 => Some(Box::new(move |v, o| float::pack_bytes_from_float(f, v, o))),
            2 => Some(Box::new(move |v, o| float::pack_words_from_float(f, v, o))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode::*;
    use super::*;
    use tintbox_oracle::Rng;

    /// Bytes one packed pixel of `fmt` occupies (color + extra channels).
    fn pixel_bytes(fmt: u32) -> usize {
        let f = PixelFormat(fmt);
        let chans = (f.channels() + f.extra()) as usize;
        chans * f.bytes() as usize
    }

    /// All (covered) input/output format words this task implements.
    fn covered_formats() -> Vec<(&'static str, u32)> {
        vec![
            ("GRAY_8", TYPE_GRAY_8),
            ("GRAY_8_REV", TYPE_GRAY_8_REV),
            ("GRAY_16", TYPE_GRAY_16),
            ("GRAY_16_REV", TYPE_GRAY_16_REV),
            ("GRAY_16_SE", TYPE_GRAY_16_SE),
            ("GRAYA_8", TYPE_GRAYA_8),
            ("GRAYA_16", TYPE_GRAYA_16),
            ("RGB_8", TYPE_RGB_8),
            ("BGR_8", TYPE_BGR_8),
            ("RGB_16", TYPE_RGB_16),
            ("RGB_16_SE", TYPE_RGB_16_SE),
            ("BGR_16", TYPE_BGR_16),
            ("RGBA_8", TYPE_RGBA_8),
            ("RGBA_16", TYPE_RGBA_16),
            ("ARGB_8", TYPE_ARGB_8),
            ("ARGB_16", TYPE_ARGB_16),
            ("ABGR_8", TYPE_ABGR_8),
            ("ABGR_16", TYPE_ABGR_16),
            ("BGRA_8", TYPE_BGRA_8),
            ("BGRA_16", TYPE_BGRA_16),
            ("CMYK_8", TYPE_CMYK_8),
            ("CMYKA_8", TYPE_CMYKA_8),
            ("CMYK_8_REV", TYPE_CMYK_8_REV),
            ("CMYK_16", TYPE_CMYK_16),
            ("CMYK_16_REV", TYPE_CMYK_16_REV),
            ("CMYK_16_SE", TYPE_CMYK_16_SE),
            ("KYMC_8", TYPE_KYMC_8),
            ("KYMC_16", TYPE_KYMC_16),
            ("KCMY_8", TYPE_KCMY_8),
            ("KCMY_16", TYPE_KCMY_16),
        ]
    }

    #[test]
    fn unpack_matches_oracle_all_formats() {
        let mut rng = Rng::new(0x00FA_CE16);
        for (name, fmt) in covered_formats() {
            let unpack = get_input_formatter(fmt)
                .unwrap_or_else(|| panic!("no input formatter for {name} ({fmt:#x})"));
            let nbytes = pixel_bytes(fmt);
            for _ in 0..50_000 {
                let buf: Vec<u8> = (0..nbytes).map(|_| (rng.next_u64() & 0xff) as u8).collect();

                let mut got = [0u16; MAX_CHANNELS];
                let consumed = unpack(&buf, &mut got);
                assert_eq!(consumed, nbytes, "{name}: bytes consumed");

                let want = tintbox_oracle::unpack16(fmt, &buf);
                assert_eq!(got, want, "{name} ({fmt:#x}) unpack of {buf:02x?}");
            }
        }
    }

    #[test]
    fn pack_matches_oracle_all_formats() {
        let mut rng = Rng::new(0x0009_ACC1);
        for (name, fmt) in covered_formats() {
            let pack = get_output_formatter(fmt)
                .unwrap_or_else(|| panic!("no output formatter for {name} ({fmt:#x})"));
            let nbytes = pixel_bytes(fmt);
            for _ in 0..50_000 {
                // Random 16-bit values for the color channels; extras left zero.
                let mut values = [0u16; MAX_CHANNELS];
                let f = PixelFormat(fmt);
                for v in values.iter_mut().take(f.channels() as usize) {
                    *v = (rng.next_u64() & 0xffff) as u16;
                }

                // Pre-fill output so we exercise the extra-byte handling exactly
                // as lcms2 does (it leaves extra bytes untouched). Both sides
                // start from the same buffer.
                let init: Vec<u8> = (0..nbytes).map(|_| (rng.next_u64() & 0xff) as u8).collect();

                let mut got = init.clone();
                let produced = pack(&values, &mut got);
                assert_eq!(produced, nbytes, "{name}: bytes produced");

                let mut want = init.clone();
                let mut wide = [0u16; tintbox_oracle::MAX_CHANNELS];
                wide[..MAX_CHANNELS].copy_from_slice(&values);
                let wn = tintbox_oracle::pack16(fmt, &wide, &mut want);
                assert_eq!(wn, nbytes, "{name}: oracle bytes produced");

                assert_eq!(got, want, "{name} ({fmt:#x}) pack of {values:04x?}");
            }
        }
    }

    /// All (covered) float/double formats this task implements.
    fn covered_float_formats() -> Vec<(&'static str, u32)> {
        vec![
            ("GRAY_FLT", TYPE_GRAY_FLT),
            ("RGB_FLT", TYPE_RGB_FLT),
            ("RGBA_FLT", TYPE_RGBA_FLT),
            ("BGR_FLT", TYPE_BGR_FLT),
            ("CMYK_FLT", TYPE_CMYK_FLT),
            ("LAB_FLT", TYPE_LAB_FLT),
            ("XYZ_FLT", TYPE_XYZ_FLT),
            ("GRAY_DBL", TYPE_GRAY_DBL),
            ("RGB_DBL", TYPE_RGB_DBL),
            ("BGR_DBL", TYPE_BGR_DBL),
            ("CMYK_DBL", TYPE_CMYK_DBL),
            ("LAB_DBL", TYPE_LAB_DBL),
            ("XYZ_DBL", TYPE_XYZ_DBL),
        ]
    }

    /// Sample byte count of one packed pixel for `fmt` (handles double=8).
    fn pixel_bytes_any(fmt: u32) -> usize {
        let f = PixelFormat(fmt);
        let sample = match f.bytes() {
            0 => 8,
            b => b as usize,
        };
        (f.channels() + f.extra()) as usize * sample
    }

    /// A random float in a representative range for unpacking. Lab/XYZ floats
    /// live in Lab/XYZ units, generic float in 0..1ish; we span a broad range to
    /// exercise the scaling and the f64→f32 truncation exactly.
    fn rand_f32(rng: &mut Rng) -> f32 {
        // Uniform-ish in [-200, 200] from 32 random bits.
        let u = (rng.next_u64() & 0xffff_ffff) as u32;
        let unit = (u as f64) / (u32::MAX as f64); // 0..1
        ((unit * 400.0) - 200.0) as f32
    }

    #[test]
    fn unpack_float_matches_oracle_all_formats() {
        let mut rng = Rng::new(0x00F1_0A77);
        for (name, fmt) in covered_float_formats() {
            let unpack = get_input_formatter_float(fmt)
                .unwrap_or_else(|| panic!("no float input formatter for {name} ({fmt:#x})"));
            let nbytes = pixel_bytes_any(fmt);
            let f = PixelFormat(fmt);
            let is_dbl = f.bytes() == 0;
            for _ in 0..50_000 {
                // Build a packed pixel as random f32/f64 samples.
                let mut buf = vec![0u8; nbytes];
                let n_samples = (f.channels() + f.extra()) as usize;
                for s in 0..n_samples {
                    let val = rand_f32(&mut rng);
                    if is_dbl {
                        buf[s * 8..s * 8 + 8].copy_from_slice(&(val as f64).to_le_bytes());
                    } else {
                        buf[s * 4..s * 4 + 4].copy_from_slice(&val.to_le_bytes());
                    }
                }

                let mut got = [0f32; MAX_CHANNELS];
                let consumed = unpack(&buf, &mut got);
                assert_eq!(consumed, nbytes, "{name}: bytes consumed");

                let want = tintbox_oracle::unpack_float(fmt, &buf);
                for ch in 0..MAX_CHANNELS {
                    assert_eq!(
                        got[ch].to_bits(),
                        want[ch].to_bits(),
                        "{name} ({fmt:#x}) unpack ch{ch}: rust={} lcms2={}",
                        got[ch],
                        want[ch]
                    );
                }
            }
        }
    }

    #[test]
    fn pack_float_matches_oracle_all_formats() {
        let mut rng = Rng::new(0x00F1_AC44);
        for (name, fmt) in covered_float_formats() {
            let pack = get_output_formatter_float(fmt)
                .unwrap_or_else(|| panic!("no float output formatter for {name} ({fmt:#x})"));
            let nbytes = pixel_bytes_any(fmt);
            let f = PixelFormat(fmt);
            for _ in 0..50_000 {
                // Pipeline output values are 0..1 (post-eval); span a bit beyond
                // to exercise FLAVOR/scale edges.
                let mut values = [0f32; MAX_CHANNELS];
                for v in values.iter_mut().take(f.channels() as usize) {
                    let u = (rng.next_u64() & 0xffff_ffff) as u32;
                    *v = ((u as f64) / (u32::MAX as f64)) as f32 * 1.2 - 0.1;
                }

                // Pre-fill output identically on both sides (extra bytes untouched).
                let init: Vec<u8> = (0..nbytes).map(|_| (rng.next_u64() & 0xff) as u8).collect();

                let mut got = init.clone();
                let produced = pack(&values, &mut got);
                assert_eq!(produced, nbytes, "{name}: bytes produced");

                let mut want = init.clone();
                let mut wide = [0f32; tintbox_oracle::MAX_CHANNELS];
                wide[..MAX_CHANNELS].copy_from_slice(&values);
                let wn = tintbox_oracle::pack_float(fmt, &wide, &mut want);
                assert_eq!(wn, nbytes, "{name}: oracle bytes produced");

                assert_eq!(got, want, "{name} ({fmt:#x}) pack of {values:?}");
            }
        }
    }

    #[test]
    fn float_path_selection_matches_lcms2() {
        // _cmsFormatterIsFloat: true iff T_FLOAT bit set (FLT and DBL both set it).
        for (_n, fmt) in covered_float_formats() {
            assert!(formatter_is_float(fmt), "{fmt:#x} should be float");
        }
        for (_n, fmt) in covered_formats() {
            assert!(!formatter_is_float(fmt), "{fmt:#x} should NOT be float");
        }
    }

    #[test]
    fn roundtrip_unpack_then_pack() {
        // For lossless formats (no FLAVOR/byte-narrowing surprises), unpack then
        // pack must reproduce the original bytes for the color channels. We test
        // the 16-bit no-extra formats where the mapping is a clean involution.
        let lossless_16: &[(&str, u32)] = &[
            ("RGB_16", TYPE_RGB_16),
            ("BGR_16", TYPE_BGR_16),
            ("RGB_16_SE", TYPE_RGB_16_SE),
            ("CMYK_16", TYPE_CMYK_16),
            ("CMYK_16_REV", TYPE_CMYK_16_REV),
            ("CMYK_16_SE", TYPE_CMYK_16_SE),
            ("KYMC_16", TYPE_KYMC_16),
            ("KCMY_16", TYPE_KCMY_16),
            ("GRAY_16", TYPE_GRAY_16),
            ("GRAY_16_SE", TYPE_GRAY_16_SE),
        ];
        let mut rng = Rng::new(0x0001_11D7);
        for &(name, fmt) in lossless_16 {
            let unpack = get_input_formatter(fmt).unwrap();
            let pack = get_output_formatter(fmt).unwrap();
            let nbytes = pixel_bytes(fmt);
            for _ in 0..20_000 {
                let buf: Vec<u8> = (0..nbytes).map(|_| (rng.next_u64() & 0xff) as u8).collect();
                let mut values = [0u16; MAX_CHANNELS];
                unpack(&buf, &mut values);
                let mut out = vec![0u8; nbytes];
                pack(&values, &mut out);
                assert_eq!(out, buf, "{name} roundtrip");
            }
        }
    }
}
