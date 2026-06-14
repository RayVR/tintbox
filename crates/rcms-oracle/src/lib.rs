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
    fn rcms_oracle_half_to_float(h: u16) -> f32;
    fn rcms_oracle_float_to_half(f: f32) -> u16;
    fn rcms_oracle_md5(out: *mut u8, buf: *const u8, len: u32);
    fn rcms_oracle_read_u16(buf: *const u8, len: u32, out: *mut u16) -> i32;
    fn rcms_oracle_read_u32(buf: *const u8, len: u32, out: *mut u32) -> i32;
    fn rcms_oracle_read_u8(buf: *const u8, len: u32, out: *mut u8) -> i32;
    fn rcms_oracle_read_u64(buf: *const u8, len: u32, out: *mut u64) -> i32;
    fn rcms_oracle_read_s15f16(buf: *const u8, len: u32, out: *mut i32) -> i32;
    fn rcms_oracle_read_xyz(buf: *const u8, len: u32, out: *mut f64) -> i32;
    fn rcms_oracle_read_u16_array(buf: *const u8, len: u32, n: u32, out: *mut u16) -> i32;
    fn rcms_oracle_read_type_base(buf: *const u8, len: u32, out: *mut u32) -> i32;
    fn rcms_oracle_read_alignment(buf: *const u8, len: u32, offset: u32, out_tell: *mut u32)
        -> i32;
}

/// lcms2 `_cmsDoubleTo15Fixed16`.
pub fn double_to_s15f16(v: f64) -> i32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_double_to_s15f16(v) }
}
pub fn s15f16_to_double(a: i32) -> f64 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_s15f16_to_double(a) }
}
pub fn double_to_8fixed8(v: f64) -> u16 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_double_to_8fixed8(v) }
}
pub fn to_fixed_domain(a: i32) -> i32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_to_fixed_domain(a) }
}
pub fn from_fixed_domain(a: i32) -> i32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
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

/// lcms2 `_cmsHalf2Float`.
pub fn half_to_float(h: u16) -> f32 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_half_to_float(h) }
}
/// lcms2 `_cmsFloat2Half`.
pub fn float_to_half(f: f32) -> u16 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_float_to_half(f) }
}

/// lcms2 `cmsMD5alloc`/`cmsMD5add`/`cmsMD5finish` (RFC 1321 MD5 over `buf`).
pub fn md5(buf: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    // SAFETY: out is a valid 16-byte array; buf/len describe a valid slice; C only reads buf and writes 16 bytes to out.
    unsafe {
        rcms_oracle_md5(out.as_mut_ptr(), buf.as_ptr(), buf.len() as u32);
    }
    out
}

/// lcms2 `_cmsReadUInt16Number` over an in-memory IOHANDLER (big-endian).
pub fn read_u16(buf: &[u8]) -> Option<u16> {
    let mut out = 0u16;
    // SAFETY: buf/len describe a valid slice; out is a valid u16; C writes out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_u16(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsReadUInt32Number` over an in-memory IOHANDLER (big-endian).
pub fn read_u32(buf: &[u8]) -> Option<u32> {
    let mut out = 0u32;
    // SAFETY: buf/len describe a valid slice; out is a valid u32; C writes out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_u32(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `_cmsReadUInt8Number` over an in-memory IOHANDLER.
pub fn read_u8(buf: &[u8]) -> Option<u8> {
    let mut out = 0u8;
    // SAFETY: buf/len describe a valid slice; out is a valid u8; C writes out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_u8(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsReadUInt64Number` over an in-memory IOHANDLER (big-endian).
pub fn read_u64(buf: &[u8]) -> Option<u64> {
    let mut out = 0u64;
    // SAFETY: buf/len describe a valid slice; out is a valid u64; C writes out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_u64(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsRead15Fixed16Number` over an in-memory IOHANDLER; returns the raw
/// wire i32 (the s15Fixed16 value), recovered from the double the C primitive yields.
pub fn read_s15f16(buf: &[u8]) -> Option<i32> {
    let mut out = 0i32;
    // SAFETY: buf/len describe a valid slice; out is a valid i32; C writes out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_s15f16(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsReadXYZNumber` over an in-memory IOHANDLER; returns `[X, Y, Z]` doubles.
pub fn read_xyz(buf: &[u8]) -> Option<[f64; 3]> {
    let mut out = [0.0f64; 3];
    // SAFETY: buf/len describe a valid slice; out is a valid 3-double array C writes when it returns nonzero.
    let ok = unsafe { rcms_oracle_read_xyz(buf.as_ptr(), buf.len() as u32, out.as_mut_ptr()) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsReadUInt16Array` over an in-memory IOHANDLER; reads `n` big-endian u16.
pub fn read_u16_array(buf: &[u8], n: usize) -> Option<Vec<u16>> {
    let mut out = vec![0u16; n];
    // SAFETY: buf/len describe a valid slice; out has capacity for n u16, which is exactly
    // what C writes when it returns nonzero.
    let ok = unsafe {
        rcms_oracle_read_u16_array(buf.as_ptr(), buf.len() as u32, n as u32, out.as_mut_ptr())
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}
/// lcms2 `_cmsReadTypeBase` over an in-memory IOHANDLER; returns the u32 type signature
/// (0 if the underlying read failed — matching the C contract).
pub fn read_type_base(buf: &[u8]) -> u32 {
    let mut out = 0u32;
    // SAFETY: buf/len describe a valid slice; out is a valid u32 C always writes.
    unsafe { rcms_oracle_read_type_base(buf.as_ptr(), buf.len() as u32, &mut out) };
    out
}
/// lcms2 `_cmsReadAlignment`: seeds the in-memory handler at `offset`, then aligns.
/// Returns `(ok, new_tell)`.
pub fn read_alignment(buf: &[u8], offset: u32) -> (bool, u32) {
    let mut tell = 0u32;
    // SAFETY: buf/len describe a valid slice; tell is a valid u32 C always writes (after Seek/align).
    let ok =
        unsafe { rcms_oracle_read_alignment(buf.as_ptr(), buf.len() as u32, offset, &mut tell) };
    (ok != 0, tell)
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
