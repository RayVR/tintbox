//! Fixed-point / half storage newtypes. Private raw field; conversions are the
//! only blessed path (bit-identical to lcms2). Half<->float math is in
//! `crate::math::half` (table-based); the `Half` type just wraps the bits.

/// Signed 15.16 fixed point (lcms2 `cmsS15Fixed16Number`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct S15Fixed16(i32);
/// Unsigned 16.16 fixed point (lcms2 `cmsU16Fixed16Number`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct U16Fixed16(u32);
/// Unsigned 8.8 fixed point (lcms2 `cmsU8Fixed8Number`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct U8Fixed8(u16);
/// IEEE half-precision bit pattern (lcms2 stores as `cmsUInt16Number`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Half(u16);

impl S15Fixed16 {
    pub const fn from_raw(bits: i32) -> Self {
        Self(bits)
    }
    pub const fn to_raw(self) -> i32 {
        self.0
    }
    /// lcms2 `_cmsDoubleTo15Fixed16`: floor(v*65536 + 0.5).
    pub fn from_f64(v: f64) -> Self {
        Self((v * 65536.0 + 0.5).floor() as i32)
    }
    /// lcms2 `_cms15Fixed16toDouble`: a / 65536.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 65536.0
    }
}
impl From<S15Fixed16> for f64 {
    fn from(s: S15Fixed16) -> f64 {
        s.to_f64()
    }
}

impl U8Fixed8 {
    pub const fn from_raw(bits: u16) -> Self {
        Self(bits)
    }
    pub const fn to_raw(self) -> u16 {
        self.0
    }
    /// lcms2 `_cmsDoubleTo8Fixed8` (cmsplugin.c:370): (DoubleTo15Fixed16(v) >> 8) & 0xffff.
    pub fn from_f64(v: f64) -> Self {
        Self(((S15Fixed16::from_f64(v).to_raw() >> 8) & 0xffff) as u16)
    }
    /// lcms2 `_cms8Fixed8toDouble` (cmsplugin.c:365): `fixed8 / 256.0`.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 256.0
    }
}

impl U16Fixed16 {
    pub const fn from_raw(bits: u32) -> Self {
        Self(bits)
    }
    pub const fn to_raw(self) -> u32 {
        self.0
    }
}

impl Half {
    pub const fn from_raw(bits: u16) -> Self {
        Self(bits)
    }
    pub const fn to_raw(self) -> u16 {
        self.0
    }
}

/// lcms2 `FROM_8_TO_16` (lcms2_internal.h:125): `(rgb << 8) | rgb`.
/// Replicates an 8-bit sample across both bytes of a 16-bit one.
pub const fn from_8_to_16(rgb: u8) -> u16 {
    ((rgb as u16) << 8) | (rgb as u16)
}

/// lcms2 `FROM_16_TO_8` (lcms2_internal.h:126):
/// `((rgb * 65281 + 8388608) >> 24) & 0xFF`.
/// NOTE: this is the exact integer form lcms2 uses — NOT the older
/// `(v + 0x80) >> 8` rounding. 65281 = 0xFF01, 8388608 = 0x800000.
pub const fn from_16_to_8(rgb: u16) -> u8 {
    (((rgb as u32 * 65281 + 8_388_608) >> 24) & 0xFF) as u8
}

/// lcms2 `_cmsToFixedDomain` (lcms2_internal.h:151): a + (a + 0x7fff)/0xffff.
/// NOTE: the `/0xffff` is integer division, NOT a shift — do not "simplify".
pub const fn to_fixed_domain(a: i32) -> i32 {
    a + (a + 0x7fff) / 0xffff
}
/// lcms2 `_cmsFromFixedDomain` (lcms2_internal.h:152): a - ((a + 0x7fff) >> 16).
pub const fn from_fixed_domain(a: i32) -> i32 {
    a - ((a + 0x7fff) >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s15f16_from_f64_matches_oracle() {
        let mut rng = rcms_oracle::Rng::new(0xF1_4ED);
        for _ in 0..1_000_000 {
            let v = (rng.next_f64_unit() - 0.5) * 80_000.0;
            assert_eq!(
                S15Fixed16::from_f64(v).to_raw(),
                rcms_oracle::double_to_s15f16(v),
                "v={v}"
            );
        }
    }
    #[test]
    fn s15f16_to_f64_matches_oracle() {
        let mut rng = rcms_oracle::Rng::new(7);
        for _ in 0..1_000_000 {
            let a = rng.next_u64() as i32;
            rcms_oracle::assert_f64_bits_eq(
                S15Fixed16::from_raw(a).to_f64(),
                rcms_oracle::s15f16_to_double(a),
                a,
            );
        }
    }
    #[test]
    fn u8fixed8_from_f64_matches_oracle() {
        let mut rng = rcms_oracle::Rng::new(99);
        for _ in 0..1_000_000 {
            let v = rng.next_f64_unit() * 2.0;
            assert_eq!(
                U8Fixed8::from_f64(v).to_raw(),
                rcms_oracle::double_to_8fixed8(v),
                "v={v}"
            );
        }
    }
    #[test]
    fn from_8_to_16_matches_oracle() {
        for v in 0u16..=255 {
            assert_eq!(
                from_8_to_16(v as u8),
                rcms_oracle::from_8_to_16(v as u8),
                "v={v}"
            );
        }
    }
    #[test]
    fn from_16_to_8_matches_oracle() {
        for v in 0u32..=0xffff {
            assert_eq!(
                from_16_to_8(v as u16),
                rcms_oracle::from_16_to_8(v as u16),
                "v={v}"
            );
        }
    }
    #[test]
    fn fixed_domain_matches_oracle() {
        for a in (-200_000..200_000).step_by(7) {
            assert_eq!(to_fixed_domain(a), rcms_oracle::to_fixed_domain(a), "a={a}");
            assert_eq!(
                from_fixed_domain(a),
                rcms_oracle::from_fixed_domain(a),
                "a={a}"
            );
        }
    }
}
