//! Fast-floor strategies. `Lcms2Floor` replicates lcms2_internal.h:160-195
//! bit-for-bit (magic constant + SIGNED i32 arithmetic shift). `NativeFloor` is
//! the correct-but-divergent f64::floor implementation we'd write absent parity.

/// One contract, two implementations. Associated fns → monomorphized, zero-cost.
pub trait FloorStrategy {
    fn quick_floor(val: f64) -> i32;
    fn quick_floor_word(d: f64) -> u16;
    fn quick_saturate_word(d: f64) -> u16;
}

/// Bit-identical to lcms2 (DEFAULT). The 1998 magic-number / type-pun hack.
pub struct Lcms2Floor;

const MAGIC: f64 = 68719476736.0 * 1.5; // 2^36 * 1.5 (lcms2_internal.h:165)

impl FloorStrategy for Lcms2Floor {
    fn quick_floor(val: f64) -> i32 {
        // lcms2 reads the low 32 bits as a SIGNED int and arithmetic-shifts >>16.
        // A logical u32>>16 is WRONG for negatives (which quick_floor_word always
        // generates). Rust `i32 >> 16` is arithmetic — match it exactly.
        let low = (val + MAGIC).to_bits() as u32 as i32;
        low >> 16
    }
    fn quick_floor_word(d: f64) -> u16 {
        // lcms2_internal.h:184: (cmsUInt16Number)quick_floor(d-32767.0) + 32767U.
        // Agreement of wrapping_add relies on the u16 return-type truncation.
        (Self::quick_floor(d - 32767.0) as u16).wrapping_add(32767)
    }
    fn quick_saturate_word(d: f64) -> u16 {
        let d = d + 0.5;
        if d <= 0.0 {
            return 0;
        }
        if d >= 65535.0 {
            return 0xffff;
        }
        Self::quick_floor_word(d)
    }
}

/// The native alternative — correct true-floor, NOT bit-identical at boundaries.
/// Kept compiled so the divergence harness can quantify the difference.
pub struct NativeFloor;
impl FloorStrategy for NativeFloor {
    fn quick_floor(val: f64) -> i32 {
        val.floor() as i32
    }
    fn quick_floor_word(d: f64) -> u16 {
        (Self::quick_floor(d - 32767.0) as u16).wrapping_add(32767)
    }
    fn quick_saturate_word(d: f64) -> u16 {
        let d = d + 0.5;
        if d <= 0.0 {
            return 0;
        }
        if d >= 65535.0 {
            return 0xffff;
        }
        Self::quick_floor_word(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcms2_floor_matches_oracle() {
        let mut rng = tintbox_oracle::Rng::new(0x5EED);
        for _ in 0..2_000_000 {
            let v = (rng.next_f64_unit() - 0.5) * 65_000.0;
            assert_eq!(
                Lcms2Floor::quick_floor(v),
                tintbox_oracle::quick_floor(v),
                "v={v}"
            );
        }
    }

    #[test]
    fn lcms2_floor_word_matches_oracle_over_lut_domain() {
        let mut rng = tintbox_oracle::Rng::new(0xABCD);
        for _ in 0..2_000_000 {
            let d = rng.next_f64_unit() * 65_535.0;
            assert_eq!(
                Lcms2Floor::quick_floor_word(d),
                tintbox_oracle::quick_floor_word(d),
                "d={d}"
            );
        }
        for d in [0.0, 0.5, 32766.9, 32767.0, 32767.5, 65534.9, 65535.0] {
            assert_eq!(
                Lcms2Floor::quick_floor_word(d),
                tintbox_oracle::quick_floor_word(d),
                "d={d}"
            );
        }
    }

    #[test]
    fn lcms2_saturate_word_matches_oracle() {
        let mut rng = tintbox_oracle::Rng::new(0xF00D);
        for _ in 0..2_000_000 {
            let d = (rng.next_f64_unit() - 0.1) * 80_000.0;
            assert_eq!(
                Lcms2Floor::quick_saturate_word(d),
                tintbox_oracle::quick_saturate_word(d),
                "d={d}"
            );
        }
    }

    /// DIVERGENCE HARNESS: measure where the native strategy differs from the
    /// lcms2 hack over the LUT-index domain. Documents the cost of switching.
    #[test]
    fn divergence_native_vs_lcms2_is_bounded_and_reported() {
        let mut diffs = 0u64;
        let mut samples = 0u64;
        let mut d = 0.0;
        while d <= 65535.0 {
            let a = Lcms2Floor::quick_floor_word(d);
            let b = NativeFloor::quick_floor_word(d);
            assert_eq!(
                a,
                tintbox_oracle::quick_floor_word(d),
                "lcms2 strategy drifted from oracle at d={d}"
            );
            if a != b {
                diffs += 1;
            }
            samples += 1;
            d += 0.25;
        }
        eprintln!("native vs lcms2 floor: {diffs}/{samples} samples differ");
        assert!(
            diffs * 1000 < samples,
            "native floor diverges far more than expected: {diffs}/{samples}"
        );
    }
}
