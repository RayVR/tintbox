//! Test-only differential oracle: links lcms2 2.19.1 and exposes its primitives
//! for bit-for-bit comparison against `rcms`.

unsafe extern "C" {
    fn rcms_oracle_double_to_s15f16(v: f64) -> i32;
    fn rcms_oracle_s15f16_to_double(a: i32) -> f64;
    fn rcms_oracle_double_to_8fixed8(v: f64) -> u16;
    fn rcms_oracle_to_fixed_domain(a: i32) -> i32;
    fn rcms_oracle_from_fixed_domain(a: i32) -> i32;
    fn rcms_oracle_quick_floor(v: f64) -> i32;
    fn rcms_oracle_quick_floor_word(d: f64) -> u16;
    fn rcms_oracle_quick_saturate_word(d: f64) -> u16;
    fn rcms_oracle_mat3_eval(out: *mut f64, m: *const f64, v: *const f64);
    fn rcms_oracle_mat3_per(out: *mut f64, a: *const f64, b: *const f64);
    fn rcms_oracle_mat3_inverse(out: *mut f64, a: *const f64) -> i32;
    fn rcms_oracle_mat3_solve(out: *mut f64, a: *const f64, b: *const f64) -> i32;
}

/// lcms2 `_cmsDoubleTo15Fixed16`.
pub fn double_to_s15f16(v: f64) -> i32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_double_to_s15f16(v) }
}
pub fn s15f16_to_double(a: i32) -> f64 {
    unsafe { rcms_oracle_s15f16_to_double(a) }
}
pub fn double_to_8fixed8(v: f64) -> u16 {
    unsafe { rcms_oracle_double_to_8fixed8(v) }
}
pub fn to_fixed_domain(a: i32) -> i32 {
    unsafe { rcms_oracle_to_fixed_domain(a) }
}
pub fn from_fixed_domain(a: i32) -> i32 {
    unsafe { rcms_oracle_from_fixed_domain(a) }
}

/// lcms2 `_cmsQuickFloor`.
pub fn quick_floor(v: f64) -> i32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_quick_floor(v) }
}
/// lcms2 `_cmsQuickFloorWord`.
pub fn quick_floor_word(d: f64) -> u16 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_quick_floor_word(d) }
}
/// lcms2 `_cmsQuickSaturateWord`.
pub fn quick_saturate_word(d: f64) -> u16 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_quick_saturate_word(d) }
}

/// lcms2 `_cmsMAT3eval`.
pub fn mat3_eval(m: &[f64; 9], v: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: out/m/v are valid fixed-size local arrays; their pointers are valid
    // for the 9/3/3 doubles C reads/writes, and C writes exactly 3 doubles to out.
    unsafe {
        rcms_oracle_mat3_eval(out.as_mut_ptr(), m.as_ptr(), v.as_ptr());
    }
    out
}
/// lcms2 `_cmsMAT3per`.
pub fn mat3_per(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut out = [0.0f64; 9];
    // SAFETY: out/a/b are valid fixed-size local arrays; their pointers are valid
    // for the 9 doubles C reads/writes, and C writes exactly 9 doubles to out.
    unsafe {
        rcms_oracle_mat3_per(out.as_mut_ptr(), a.as_ptr(), b.as_ptr());
    }
    out
}
/// lcms2 `_cmsMAT3inverse`. Returns `None` on singular matrix.
pub fn mat3_inverse(a: &[f64; 9]) -> Option<[f64; 9]> {
    let mut out = [0.0f64; 9];
    // SAFETY: out/a are valid fixed-size local arrays; their pointers are valid for
    // the 9 doubles C reads/writes. C writes 9 doubles to out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_mat3_inverse(out.as_mut_ptr(), a.as_ptr()) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsMAT3solve`. Returns `None` on singular matrix.
pub fn mat3_solve(a: &[f64; 9], b: &[f64; 3]) -> Option<[f64; 3]> {
    let mut out = [0.0f64; 3];
    // SAFETY: out/a/b are valid fixed-size local arrays; their pointers are valid for
    // the 9/3 doubles C reads. C writes 3 doubles to out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_mat3_solve(out.as_mut_ptr(), a.as_ptr(), b.as_ptr()) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Deterministic xorshift64* RNG — reproducible sweeps without a dependency.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn next_f64_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Assert two f64 are bit-identical (catches NaN/-0.0/low-bit drift that == hides).
#[track_caller]
pub fn assert_f64_bits_eq(rust: f64, c: f64, ctx: impl core::fmt::Debug) {
    assert_eq!(
        rust.to_bits(),
        c.to_bits(),
        "f64 bit mismatch at {ctx:?}: rust={rust} c={c}"
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn oracle_links() {
        assert_eq!(super::double_to_s15f16(1.0), 65536); // 1.0 -> 65536 in 15.16
    }
}
