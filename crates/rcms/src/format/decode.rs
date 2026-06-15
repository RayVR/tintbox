//! ICC pixel-format word decoder.
//!
//! Transcribes the `T_*` accessor macros and the `*_SH` packing macros from
//! `lcms2.h:711-736`, plus the common `TYPE_*` constants. The format word lays
//! out (low to high bit):
//!
//! ```text
//!   bits 0-2   B  bytes per sample      (T_BYTES;  0 means double, 4=float w/ FLOAT)
//!   bits 3-6   C  channels              (T_CHANNELS)
//!   bits 7-9   E  extra samples         (T_EXTRA)
//!   bit  10    S  do swap (BGR, KYMC)   (T_DOSWAP)
//!   bit  11    X  swap 16-bit endian    (T_ENDIAN16)
//!   bit  12    P  planar                (T_PLANAR)
//!   bit  13    F  flavor (min-is-white) (T_FLAVOR)
//!   bit  14    Y  swap first            (T_SWAPFIRST)
//!   bits 16-20 T  colorspace (PT_*)     (T_COLORSPACE)
//!   bit  21    O  optimized             (T_OPTIMIZED)
//!   bit  22    A  float                 (T_FLOAT)
//!   bit  23    m  premultiplied alpha   (T_PREMUL)
//! ```

/// A decoded ICC pixel-format word (lcms2 `cmsUInt32Number` format specifier).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelFormat(pub u32);

impl PixelFormat {
    /// The raw 32-bit format word.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// `T_BYTES`: bytes per sample, low 3 bits. 0 = double, 1/2/4 = 8/16/float.
    pub const fn bytes(self) -> u32 {
        self.0 & 7
    }
    /// `T_CHANNELS`: samples per pixel (the color channels, not extras).
    pub const fn channels(self) -> u32 {
        (self.0 >> 3) & 15
    }
    /// `T_EXTRA`: number of extra (alpha) samples.
    pub const fn extra(self) -> u32 {
        (self.0 >> 7) & 7
    }
    /// `T_DOSWAP`: reverse channel order (RGB->BGR, CMYK->KYMC).
    pub const fn doswap(self) -> bool {
        (self.0 >> 10) & 1 != 0
    }
    /// `T_ENDIAN16`: byte-swap 16-bit samples.
    pub const fn endian16(self) -> bool {
        (self.0 >> 11) & 1 != 0
    }
    /// `T_PLANAR`: planar (vs chunky) layout.
    pub const fn planar(self) -> bool {
        (self.0 >> 12) & 1 != 0
    }
    /// `T_FLAVOR`: 0 = min-is-black (chocolate), 1 = min-is-white (vanilla, reversed).
    pub const fn flavor(self) -> bool {
        (self.0 >> 13) & 1 != 0
    }
    /// `T_SWAPFIRST`: move first channel last (ARGB->RGBA, KCMY->CMYK).
    pub const fn swapfirst(self) -> bool {
        (self.0 >> 14) & 1 != 0
    }
    /// `T_COLORSPACE`: the `PT_*` pixel type, 5 bits.
    pub const fn colorspace(self) -> u32 {
        (self.0 >> 16) & 31
    }
    /// `T_OPTIMIZED`: previous optimization already returns the final 8-bit value.
    pub const fn optimized(self) -> bool {
        (self.0 >> 21) & 1 != 0
    }
    /// `T_FLOAT`: floating-point samples.
    pub const fn is_float(self) -> bool {
        (self.0 >> 22) & 1 != 0
    }
    /// `T_PREMUL`: premultiplied alpha.
    pub const fn premul(self) -> bool {
        (self.0 >> 23) & 1 != 0
    }
}

// --- Pixel types (PT_*, lcms2.h:740-769) ---
pub const PT_ANY: u32 = 0;
pub const PT_GRAY: u32 = 3;
pub const PT_RGB: u32 = 4;
pub const PT_CMY: u32 = 5;
pub const PT_CMYK: u32 = 6;
pub const PT_YCBCR: u32 = 7;
pub const PT_YUV: u32 = 8;
pub const PT_XYZ: u32 = 9;
pub const PT_LAB: u32 = 10;
pub const PT_YUVK: u32 = 11;
pub const PT_HSV: u32 = 12;
pub const PT_HLS: u32 = 13;
pub const PT_YXY: u32 = 14;

// --- `*_SH` packing macros (lcms2.h:711-722) ---
const fn premul_sh(m: u32) -> u32 {
    m << 23
}
const fn float_sh(a: u32) -> u32 {
    a << 22
}
const fn colorspace_sh(s: u32) -> u32 {
    s << 16
}
const fn swapfirst_sh(s: u32) -> u32 {
    s << 14
}
const fn flavor_sh(s: u32) -> u32 {
    s << 13
}
const fn endian16_sh(e: u32) -> u32 {
    e << 11
}
const fn doswap_sh(e: u32) -> u32 {
    e << 10
}
const fn extra_sh(e: u32) -> u32 {
    e << 7
}
const fn channels_sh(c: u32) -> u32 {
    c << 3
}
const fn bytes_sh(b: u32) -> u32 {
    b
}

// --- Common TYPE_* constants (lcms2.h:776-...) ---
// Gray
pub const TYPE_GRAY_8: u32 = colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(1);
pub const TYPE_GRAY_8_REV: u32 =
    colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(1) | flavor_sh(1);
pub const TYPE_GRAY_16: u32 = colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(2);
pub const TYPE_GRAY_16_REV: u32 =
    colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(2) | flavor_sh(1);
pub const TYPE_GRAY_16_SE: u32 =
    colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(2) | endian16_sh(1);
pub const TYPE_GRAYA_8: u32 = colorspace_sh(PT_GRAY) | extra_sh(1) | channels_sh(1) | bytes_sh(1);
pub const TYPE_GRAYA_16: u32 = colorspace_sh(PT_GRAY) | extra_sh(1) | channels_sh(1) | bytes_sh(2);

// RGB / BGR
pub const TYPE_RGB_8: u32 = colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(1);
pub const TYPE_BGR_8: u32 = colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(1) | doswap_sh(1);
pub const TYPE_RGB_16: u32 = colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(2);
pub const TYPE_RGB_16_SE: u32 =
    colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(2) | endian16_sh(1);
pub const TYPE_BGR_16: u32 = colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(2) | doswap_sh(1);

// RGBA / ARGB / ABGR / BGRA
pub const TYPE_RGBA_8: u32 = colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(1);
pub const TYPE_RGBA_16: u32 = colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(2);
pub const TYPE_ARGB_8: u32 =
    colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(1) | swapfirst_sh(1);
pub const TYPE_ARGB_16: u32 =
    colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(2) | swapfirst_sh(1);
pub const TYPE_ABGR_8: u32 =
    colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(1) | doswap_sh(1);
pub const TYPE_ABGR_16: u32 =
    colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(2) | doswap_sh(1);
pub const TYPE_BGRA_8: u32 = colorspace_sh(PT_RGB)
    | extra_sh(1)
    | channels_sh(3)
    | bytes_sh(1)
    | doswap_sh(1)
    | swapfirst_sh(1);
pub const TYPE_BGRA_16: u32 = colorspace_sh(PT_RGB)
    | extra_sh(1)
    | channels_sh(3)
    | bytes_sh(2)
    | doswap_sh(1)
    | swapfirst_sh(1);

// CMYK / KYMC / KCMY
pub const TYPE_CMYK_8: u32 = colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(1);
pub const TYPE_CMYKA_8: u32 = colorspace_sh(PT_CMYK) | extra_sh(1) | channels_sh(4) | bytes_sh(1);
pub const TYPE_CMYK_8_REV: u32 =
    colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(1) | flavor_sh(1);
pub const TYPE_CMYK_16: u32 = colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(2);
pub const TYPE_CMYK_16_REV: u32 =
    colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(2) | flavor_sh(1);
pub const TYPE_CMYK_16_SE: u32 =
    colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(2) | endian16_sh(1);
pub const TYPE_KYMC_8: u32 = colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(1) | doswap_sh(1);
pub const TYPE_KYMC_16: u32 = colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(2) | doswap_sh(1);
pub const TYPE_KCMY_8: u32 =
    colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(1) | swapfirst_sh(1);
pub const TYPE_KCMY_16: u32 =
    colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(2) | swapfirst_sh(1);

// --- Float / double TYPE_* constants (lcms2.h:945-975) ---
pub const TYPE_GRAY_FLT: u32 = float_sh(1) | colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(4);
pub const TYPE_RGB_FLT: u32 = float_sh(1) | colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(4);
pub const TYPE_RGBA_FLT: u32 =
    float_sh(1) | colorspace_sh(PT_RGB) | extra_sh(1) | channels_sh(3) | bytes_sh(4);
pub const TYPE_BGR_FLT: u32 =
    float_sh(1) | colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(4) | doswap_sh(1);
pub const TYPE_CMYK_FLT: u32 = float_sh(1) | colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(4);
pub const TYPE_LAB_FLT: u32 = float_sh(1) | colorspace_sh(PT_LAB) | channels_sh(3) | bytes_sh(4);
pub const TYPE_XYZ_FLT: u32 = float_sh(1) | colorspace_sh(PT_XYZ) | channels_sh(3) | bytes_sh(4);

pub const TYPE_GRAY_DBL: u32 = float_sh(1) | colorspace_sh(PT_GRAY) | channels_sh(1) | bytes_sh(0);
pub const TYPE_RGB_DBL: u32 = float_sh(1) | colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(0);
pub const TYPE_BGR_DBL: u32 =
    float_sh(1) | colorspace_sh(PT_RGB) | channels_sh(3) | bytes_sh(0) | doswap_sh(1);
pub const TYPE_CMYK_DBL: u32 = float_sh(1) | colorspace_sh(PT_CMYK) | channels_sh(4) | bytes_sh(0);
pub const TYPE_LAB_DBL: u32 = float_sh(1) | colorspace_sh(PT_LAB) | channels_sh(3) | bytes_sh(0);
pub const TYPE_XYZ_DBL: u32 = float_sh(1) | colorspace_sh(PT_XYZ) | channels_sh(3) | bytes_sh(0);

/// `TYPE_NAMED_COLOR_INDEX` (lcms2.h:944): the named-color transform's INPUT
/// format — one 16-bit color INDEX per pixel (`CHANNELS_SH(1) | BYTES_SH(2)`,
/// colorspace `PT_ANY`/0). The 1-channel u16 unpack reads the index into the
/// pipeline's single input channel; the `NamedColor` stage maps it to PCS/device.
pub const TYPE_NAMED_COLOR_INDEX: u32 = channels_sh(1) | bytes_sh(2);

// Float marker (used by decoder tests).
pub const fn float_marker(a: u32) -> u32 {
    float_sh(a)
}
pub const fn premul_marker(m: u32) -> u32 {
    premul_sh(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_rgb_8() {
        let f = PixelFormat(TYPE_RGB_8);
        assert_eq!(f.colorspace(), PT_RGB);
        assert_eq!(f.channels(), 3);
        assert_eq!(f.bytes(), 1);
        assert_eq!(f.extra(), 0);
        assert!(!f.doswap());
        assert!(!f.swapfirst());
        assert!(!f.flavor());
        assert!(!f.endian16());
        assert!(!f.planar());
        assert!(!f.is_float());
        assert!(!f.premul());
    }

    #[test]
    fn decode_rgba_8() {
        let f = PixelFormat(TYPE_RGBA_8);
        assert_eq!(f.channels(), 3);
        assert_eq!(f.extra(), 1);
        assert_eq!(f.bytes(), 1);
    }

    #[test]
    fn decode_bgr_8_doswap() {
        let f = PixelFormat(TYPE_BGR_8);
        assert!(f.doswap());
        assert!(!f.swapfirst());
        assert_eq!(f.channels(), 3);
    }

    #[test]
    fn decode_argb_8_swapfirst() {
        let f = PixelFormat(TYPE_ARGB_8);
        assert!(f.swapfirst());
        assert!(!f.doswap());
        assert_eq!(f.extra(), 1);
    }

    #[test]
    fn decode_bgra_8_doswap_swapfirst() {
        let f = PixelFormat(TYPE_BGRA_8);
        assert!(f.doswap());
        assert!(f.swapfirst());
    }

    #[test]
    fn decode_cmyk_16() {
        let f = PixelFormat(TYPE_CMYK_16);
        assert_eq!(f.colorspace(), PT_CMYK);
        assert_eq!(f.channels(), 4);
        assert_eq!(f.bytes(), 2);
    }

    #[test]
    fn decode_cmyk_8_rev_flavor() {
        let f = PixelFormat(TYPE_CMYK_8_REV);
        assert!(f.flavor());
        assert_eq!(f.channels(), 4);
    }

    #[test]
    fn decode_gray_16_se_endian() {
        let f = PixelFormat(TYPE_GRAY_16_SE);
        assert!(f.endian16());
        assert_eq!(f.channels(), 1);
        assert_eq!(f.bytes(), 2);
    }

    #[test]
    fn decode_kymc_8() {
        let f = PixelFormat(TYPE_KYMC_8);
        assert!(f.doswap());
        assert!(!f.swapfirst());
        assert_eq!(f.colorspace(), PT_CMYK);
    }

    #[test]
    fn decode_kcmy_16() {
        let f = PixelFormat(TYPE_KCMY_16);
        assert!(f.swapfirst());
        assert!(!f.doswap());
        assert_eq!(f.bytes(), 2);
    }

    #[test]
    fn decode_float_and_premul_markers() {
        let f = PixelFormat(TYPE_RGBA_8 | float_marker(1) | premul_marker(1));
        assert!(f.is_float());
        assert!(f.premul());
    }
}
