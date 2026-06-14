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
    fn rcms_oracle_read_header(buf: *const u8, len: u32, out: *mut OracleHeader) -> i32;
    fn rcms_oracle_open_succeeds(buf: *const u8, len: u32) -> i32;
    fn rcms_oracle_tag_count(buf: *const u8, len: u32) -> i32;
    fn rcms_oracle_tag_signature(buf: *const u8, len: u32, n: u32) -> u32;
    fn rcms_oracle_tag_true_type(buf: *const u8, len: u32, sig: u32) -> u32;
    fn rcms_oracle_read_tag_xyz(buf: *const u8, len: u32, sig: u32, out: *mut f64) -> i32;
    fn rcms_oracle_read_tag_s15f16(
        buf: *const u8,
        len: u32,
        sig: u32,
        out: *mut f64,
        cap: u32,
    ) -> i32;
    fn rcms_oracle_read_tag_signature(buf: *const u8, len: u32, sig: u32, out: *mut u32) -> i32;
    fn rcms_oracle_read_tag_text(buf: *const u8, len: u32, sig: u32, out: *mut u8, cap: u32)
        -> i32;
    fn rcms_oracle_read_tag_data(
        buf: *const u8,
        len: u32,
        sig: u32,
        flag: *mut u32,
        out: *mut u8,
        cap: u32,
    ) -> i32;
    fn rcms_oracle_read_tag_datetime(buf: *const u8, len: u32, sig: u32, out: *mut u16) -> i32;
    fn rcms_oracle_read_tag_chromaticity(buf: *const u8, len: u32, sig: u32, out: *mut f64) -> i32;
    fn rcms_oracle_read_tag_colorant_order(
        buf: *const u8,
        len: u32,
        sig: u32,
        out: *mut u8,
        cap: u32,
    ) -> i32;
}

/// Flat mirror of `rcms_oracle_header` in shim.c (must match field order/layout).
/// `#[repr(C)]` so the C struct and this agree on layout for the FFI write.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OracleHeader {
    pub device_class: u32,
    pub color_space: u32,
    pub pcs: u32,
    pub version: u32,
    pub rendering_intent: u32,
    pub flags: u32,
    pub manufacturer: u32,
    pub model: u32,
    pub creator: u32,
    pub attributes: u64,
    pub profile_id: [u8; 16],
}

/// lcms2 `cmsOpenProfileFromMem` + the `cmsGetHeader*` accessors. Returns the
/// header fields lcms2 exposes for an accepted profile, or `None` when lcms2
/// rejects the profile (so the differential test can compare the accept/reject
/// decision itself, not just field values).
pub fn read_header(buf: &[u8]) -> Option<OracleHeader> {
    let mut out = OracleHeader::default();
    // SAFETY: buf/len describe a valid readable slice that C only reads (it copies
    // it into an in-memory profile). `out` is a valid, properly-aligned
    // `OracleHeader` whose layout matches the C `rcms_oracle_header` (both repr(C),
    // same field order). C writes every field only when it returns nonzero. The
    // profile handle is opened and closed entirely inside the C call.
    let ok = unsafe { rcms_oracle_read_header(buf.as_ptr(), buf.len() as u32, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Full lcms2 `cmsOpenProfileFromMem` over the WHOLE profile bytes (header +
/// tag directory + duplicate check). `true` if lcms2 accepts the profile. This
/// is the accept/reject decision `Profile::open` must agree with.
pub fn open_succeeds(buf: &[u8]) -> bool {
    // SAFETY: buf/len describe a valid readable slice C only reads (copies into
    // an in-memory profile, opened and closed entirely inside the call).
    let ok = unsafe { rcms_oracle_open_succeeds(buf.as_ptr(), buf.len() as u32) };
    ok != 0
}

/// lcms2 `cmsGetTagCount` over the accepted directory, or `None` if the profile
/// cannot be opened.
pub fn tag_count(buf: &[u8]) -> Option<u32> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let n = unsafe { rcms_oracle_tag_count(buf.as_ptr(), buf.len() as u32) };
    if n < 0 {
        None
    } else {
        Some(n as u32)
    }
}

/// The accepted tag signatures (`cmsGetTagSignature` looped over the count), or
/// `None` if the profile cannot be opened.
pub fn tag_signatures(buf: &[u8]) -> Option<Vec<u32>> {
    let n = tag_count(buf)?;
    let mut sigs = Vec::with_capacity(n as usize);
    for i in 0..n {
        // SAFETY: buf/len describe a valid readable slice C only reads; i < n.
        let sig = unsafe { rcms_oracle_tag_signature(buf.as_ptr(), buf.len() as u32, i) };
        sigs.push(sig);
    }
    Some(sigs)
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

/// lcms2 `_cmsGetTagTrueType`: the on-disk tag-type signature for a tag, or
/// `None` if the profile cannot be opened, the tag is absent, or the type is
/// unknown. Used to pick which tags carry one of the trivial on-disk types.
pub fn tag_true_type(buf: &[u8], sig: u32) -> Option<u32> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let t = unsafe { rcms_oracle_tag_true_type(buf.as_ptr(), buf.len() as u32, sig) };
    if t == 0 {
        None
    } else {
        Some(t)
    }
}

/// lcms2 `cmsReadTag` of an XYZType tag -> `[X,Y,Z]`, or `None` on failure.
pub fn read_tag_xyz(buf: &[u8], sig: u32) -> Option<[f64; 3]> {
    let mut out = [0.0f64; 3];
    // SAFETY: buf/len describe a valid readable slice; out is a valid 3-double
    // array C writes only when it returns nonzero.
    let ok =
        unsafe { rcms_oracle_read_tag_xyz(buf.as_ptr(), buf.len() as u32, sig, out.as_mut_ptr()) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of an S15Fixed16ArrayType -> `n` doubles, or `None`.
pub fn read_tag_s15f16(buf: &[u8], sig: u32, n: usize) -> Option<Vec<f64>> {
    let mut out = vec![0.0f64; n];
    // SAFETY: buf/len describe a valid readable slice; out has room for n doubles,
    // which is exactly what C writes when it returns >= 0.
    let got = unsafe {
        rcms_oracle_read_tag_s15f16(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            out.as_mut_ptr(),
            n as u32,
        )
    };
    if got >= 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a SignatureType -> the u32 signature, or `None`.
pub fn read_tag_signature(buf: &[u8], sig: u32) -> Option<u32> {
    let mut out = 0u32;
    // SAFETY: buf/len describe a valid readable slice; out is a valid u32 C writes
    // only when it returns nonzero.
    let ok =
        unsafe { rcms_oracle_read_tag_signature(buf.as_ptr(), buf.len() as u32, sig, &mut out) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a TextType -> the ASCII bytes (no NUL), or `None`.
pub fn read_tag_text(buf: &[u8], sig: u32) -> Option<Vec<u8>> {
    // The longest text tag in the testbed is well under 64 KiB; use a generous cap.
    let cap = 65536usize;
    let mut out = vec![0u8; cap];
    // SAFETY: buf/len describe a valid readable slice; out has `cap` bytes, and C
    // writes at most `cap` bytes (it bails when the string exceeds cap+1).
    let n = unsafe {
        rcms_oracle_read_tag_text(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            out.as_mut_ptr(),
            cap as u32,
        )
    };
    if n >= 0 {
        out.truncate(n as usize);
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a DataType -> `(flag, bytes)`, or `None`.
pub fn read_tag_data(buf: &[u8], sig: u32) -> Option<(u32, Vec<u8>)> {
    let cap = 1usize << 20;
    let mut out = vec![0u8; cap];
    let mut flag = 0u32;
    // SAFETY: buf/len describe a valid readable slice; out has `cap` bytes and C
    // writes at most `cap` (it bails when len > cap); flag is a valid u32.
    let n = unsafe {
        rcms_oracle_read_tag_data(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            &mut flag,
            out.as_mut_ptr(),
            cap as u32,
        )
    };
    if n >= 0 {
        out.truncate(n as usize);
        Some((flag, out))
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a DateTimeType -> `[year,month,day,hours,minutes,seconds]`.
pub fn read_tag_datetime(buf: &[u8], sig: u32) -> Option<[u16; 6]> {
    let mut out = [0u16; 6];
    // SAFETY: buf/len describe a valid readable slice; out is a valid 6-u16 array
    // C writes only when it returns nonzero.
    let ok = unsafe {
        rcms_oracle_read_tag_datetime(buf.as_ptr(), buf.len() as u32, sig, out.as_mut_ptr())
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a ChromaticityType -> `[Rx,Ry,Gx,Gy,Bx,By]`, or `None`.
pub fn read_tag_chromaticity(buf: &[u8], sig: u32) -> Option<[f64; 6]> {
    let mut out = [0.0f64; 6];
    // SAFETY: buf/len describe a valid readable slice; out is a valid 6-double
    // array C writes only when it returns nonzero.
    let ok = unsafe {
        rcms_oracle_read_tag_chromaticity(buf.as_ptr(), buf.len() as u32, sig, out.as_mut_ptr())
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a ColorantOrderType -> the full cmsMAXCHANNELS (16) byte
/// laydown-order array (0xFF-padded past the declared count), or `None`.
pub fn read_tag_colorant_order(buf: &[u8], sig: u32) -> Option<Vec<u8>> {
    let cap = 16usize; // cmsMAXCHANNELS
    let mut out = vec![0u8; cap];
    // SAFETY: buf/len describe a valid readable slice; out has cmsMAXCHANNELS bytes,
    // exactly what C writes when it returns nonzero.
    let n = unsafe {
        rcms_oracle_read_tag_colorant_order(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            out.as_mut_ptr(),
            cap as u32,
        )
    };
    if n >= 0 {
        out.truncate(n as usize);
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
