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
    fn rcms_oracle_read_tag_measurement(
        buf: *const u8,
        len: u32,
        sig: u32,
        out_u32: *mut u32,
        out_f64: *mut f64,
    ) -> i32;
    fn rcms_oracle_mlu_count(buf: *const u8, len: u32, sig: u32) -> i32;
    fn rcms_oracle_mlu_entry(
        buf: *const u8,
        len: u32,
        sig: u32,
        idx: u32,
        lang: *mut u8,
        country: *mut u8,
        units: *mut u16,
        cap: u32,
    ) -> i32;
    fn rcms_oracle_named_color2_info(
        buf: *const u8,
        len: u32,
        sig: u32,
        out_counts: *mut u32,
        prefix: *mut u8,
        suffix: *mut u8,
    ) -> i32;
    fn rcms_oracle_named_color2_color(
        buf: *const u8,
        len: u32,
        sig: u32,
        idx: u32,
        name: *mut u8,
        pcs: *mut u16,
        colorant: *mut u16,
    ) -> i32;
    fn rcms_oracle_seq_count(buf: *const u8, len: u32, sig: u32) -> i32;
    fn rcms_oracle_seq_desc_elem(
        buf: *const u8,
        len: u32,
        sig: u32,
        idx: u32,
        out_u32: *mut u32,
        out_attr: *mut u64,
        mblk: *mut u8,
        mcap: u32,
        mused: *mut u32,
        dblk: *mut u8,
        dcap: u32,
        dused: *mut u32,
    ) -> i32;
    fn rcms_oracle_seq_id_elem(
        buf: *const u8,
        len: u32,
        sig: u32,
        idx: u32,
        profile_id: *mut u8,
        blk: *mut u8,
        cap: u32,
        used: *mut u32,
    ) -> i32;
    fn rcms_oracle_dict_count(buf: *const u8, len: u32, sig: u32) -> i32;
    fn rcms_oracle_dict_entry(
        buf: *const u8,
        len: u32,
        sig: u32,
        idx: u32,
        name_units: *mut u16,
        ncap: u32,
        nn: *mut u32,
        value_units: *mut u16,
        vcap: u32,
        vn: *mut u32,
        dnblk: *mut u8,
        dncap: u32,
        dnused: *mut u32,
        dvblk: *mut u8,
        dvcap: u32,
        dvused: *mut u32,
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

/// lcms2 `cmsReadTag` of a MeasurementType -> the `cmsICCMeasurementConditions`
/// fields, returned as `([Observer, Geometry, IlluminantType], [Bx, By, Bz,
/// Flare])`, or `None` if the tag is absent / unreadable.
pub fn read_tag_measurement(buf: &[u8], sig: u32) -> Option<([u32; 3], [f64; 4])> {
    let mut u = [0u32; 3];
    let mut f = [0f64; 4];
    // SAFETY: buf/len describe a valid readable slice; the out arrays have the
    // exact lengths the C extractor writes (3 u32, 4 f64).
    let ok = unsafe {
        rcms_oracle_read_tag_measurement(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            u.as_mut_ptr(),
            f.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some((u, f))
    } else {
        None
    }
}

/// One MLU translation as lcms2 exposes it: the raw language/country code bytes
/// (`cmsMLUtranslationsCodes`) and the wide string decoded from lcms2's raw
/// UTF-16 code units (`cmsMLUgetWide`) via [`char::decode_utf16`] — the same
/// normalization rcms applies to the identical units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleMluEntry {
    pub language: [u8; 2],
    pub country: [u8; 2],
    pub text: String,
}

/// lcms2 `cmsMLUtranslationsCount` for an `mluc`/`desc` tag, or `None`.
pub fn mlu_count(buf: &[u8], sig: u32) -> Option<u32> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let n = unsafe { rcms_oracle_mlu_count(buf.as_ptr(), buf.len() as u32, sig) };
    if n >= 0 {
        Some(n as u32)
    } else {
        None
    }
}

/// All translations of an `mluc`/`desc` tag, in `cmsMLUtranslationsCodes` index
/// order, or `None` if the tag is absent / not MLU-backed.
pub fn mlu_entries(buf: &[u8], sig: u32) -> Option<Vec<OracleMluEntry>> {
    let count = mlu_count(buf, sig)?;
    let cap = 1usize << 20; // wide strings in the testbed are far under 1M units
    let mut out = Vec::with_capacity(count as usize);
    for idx in 0..count {
        let mut lang = [0u8; 2];
        let mut country = [0u8; 2];
        let mut units = vec![0u16; cap];
        // SAFETY: buf/len describe a valid readable slice; lang/country are 2-byte
        // arrays and units has `cap` u16 of room — exactly the bounds C respects.
        let n = unsafe {
            rcms_oracle_mlu_entry(
                buf.as_ptr(),
                buf.len() as u32,
                sig,
                idx,
                lang.as_mut_ptr(),
                country.as_mut_ptr(),
                units.as_mut_ptr(),
                cap as u32,
            )
        };
        if n < 0 {
            return None;
        }
        units.truncate(n as usize);
        let text = char::decode_utf16(units.into_iter())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        out.push(OracleMluEntry {
            language: lang,
            country,
            text,
        });
    }
    Some(out)
}

/// A serialized nested MLU as the `serialize_mlu` C helper emits it: a list of
/// translations, each with raw language/country bytes and the wide string decoded
/// from the truncated u16 unit stream via [`char::decode_utf16`] — the same
/// normalization rcms's MLU reader applies. Compare against an rcms `Mlu` by
/// mapping its entries to this shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleMlu {
    pub entries: Vec<OracleMluEntry>,
}

/// Decode a `serialize_mlu` byte block (u32 count; per translation 2 lang, 2
/// country, u32 nunits, nunits×u16-BE) into an [`OracleMlu`].
fn decode_serialized_mlu(blk: &[u8]) -> OracleMlu {
    let mut off = 0usize;
    let rd_u32 = |b: &[u8], o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let count = rd_u32(blk, off);
    off += 4;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let language = [blk[off], blk[off + 1]];
        let country = [blk[off + 2], blk[off + 3]];
        off += 4;
        let nunits = rd_u32(blk, off) as usize;
        off += 4;
        let units =
            (0..nunits).map(|i| u16::from_be_bytes([blk[off + i * 2], blk[off + i * 2 + 1]]));
        let text = char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        off += nunits * 2;
        entries.push(OracleMluEntry {
            language,
            country,
            text,
        });
    }
    OracleMlu { entries }
}

/// One named colour as lcms2 exposes it (`cmsNamedColorInfo`): the root name
/// (NUL-trimmed), the 3×u16 PCS, and `ColorantCount` device coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleNamedColor {
    pub name: String,
    pub pcs: [u16; 3],
    pub device: Vec<u16>,
}

/// A `cmsNAMEDCOLORLIST` as lcms2 exposes it. `vendor_flag` is NOT present:
/// lcms2 discards it on read, so the differential test compares it against the
/// raw on-disk bytes instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleNamedColorList {
    pub prefix: String,
    pub suffix: String,
    pub colors: Vec<OracleNamedColor>,
}

/// lcms2 `cmsReadTag` of a `ncl2` tag, decoded to its named-colour list (sans
/// vendor flag), or `None` if absent / not named-colour-backed.
pub fn read_tag_named_color2(buf: &[u8], sig: u32) -> Option<OracleNamedColorList> {
    let mut counts = [0u32; 2];
    let mut prefix = [0u8; 33];
    let mut suffix = [0u8; 33];
    // SAFETY: buf/len describe a valid readable slice; counts/prefix/suffix are
    // valid 2-u32 / 33-byte arrays C writes only when it returns nonzero.
    let ok = unsafe {
        rcms_oracle_named_color2_info(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            counts.as_mut_ptr(),
            prefix.as_mut_ptr(),
            suffix.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return None;
    }
    let n_colors = counts[0];
    let colorant_count = counts[1] as usize;
    let to_str = |b: &[u8; 33]| -> String {
        let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
        b[..end].iter().map(|&x| x as char).collect()
    };
    let mut colors = Vec::with_capacity(n_colors as usize);
    for idx in 0..n_colors {
        let mut name = [0u8; 33];
        let mut pcs = [0u16; 3];
        let mut colorant = [0u16; 16];
        // SAFETY: name/pcs/colorant are valid 33-byte / 3-u16 / 16-u16 arrays C
        // writes only when it returns nonzero; idx < n_colors.
        let cok = unsafe {
            rcms_oracle_named_color2_color(
                buf.as_ptr(),
                buf.len() as u32,
                sig,
                idx,
                name.as_mut_ptr(),
                pcs.as_mut_ptr(),
                colorant.as_mut_ptr(),
            )
        };
        if cok == 0 {
            return None;
        }
        colors.push(OracleNamedColor {
            name: to_str(&name),
            pcs,
            device: colorant[..colorant_count].to_vec(),
        });
    }
    Some(OracleNamedColorList {
        prefix: to_str(&prefix),
        suffix: to_str(&suffix),
        colors,
    })
}

/// One `cmsPSEQDESC` element of a `pseq` tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleSeqDescItem {
    pub device_mfg: u32,
    pub device_model: u32,
    pub attributes: u64,
    pub technology: u32,
    pub manufacturer: OracleMlu,
    pub model: OracleMlu,
}

/// lcms2 `cmsReadTag` of a `pseq` tag, decoded to its element list, or `None`.
pub fn read_tag_seq_desc(buf: &[u8], sig: u32) -> Option<Vec<OracleSeqDescItem>> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let n = unsafe { rcms_oracle_seq_count(buf.as_ptr(), buf.len() as u32, sig) };
    if n < 0 {
        return None;
    }
    let cap = 1usize << 16;
    let mut items = Vec::with_capacity(n as usize);
    for idx in 0..n as u32 {
        let mut u32s = [0u32; 3];
        let mut attr = 0u64;
        let mut mblk = vec![0u8; cap];
        let mut dblk = vec![0u8; cap];
        let mut mused = 0u32;
        let mut dused = 0u32;
        // SAFETY: all out pointers reference valid local buffers of the declared
        // capacities; C writes within them only when it returns nonzero; idx < n.
        let ok = unsafe {
            rcms_oracle_seq_desc_elem(
                buf.as_ptr(),
                buf.len() as u32,
                sig,
                idx,
                u32s.as_mut_ptr(),
                &mut attr,
                mblk.as_mut_ptr(),
                cap as u32,
                &mut mused,
                dblk.as_mut_ptr(),
                cap as u32,
                &mut dused,
            )
        };
        if ok == 0 {
            return None;
        }
        items.push(OracleSeqDescItem {
            device_mfg: u32s[0],
            device_model: u32s[1],
            attributes: attr,
            technology: u32s[2],
            manufacturer: decode_serialized_mlu(&mblk[..mused as usize]),
            model: decode_serialized_mlu(&dblk[..dused as usize]),
        });
    }
    Some(items)
}

/// One element of a `psid` tag: profile ID and description MLU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleSeqIdItem {
    pub profile_id: [u8; 16],
    pub description: OracleMlu,
}

/// lcms2 `cmsReadTag` of a `psid` tag, decoded to its element list, or `None`.
pub fn read_tag_seq_id(buf: &[u8], sig: u32) -> Option<Vec<OracleSeqIdItem>> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let n = unsafe { rcms_oracle_seq_count(buf.as_ptr(), buf.len() as u32, sig) };
    if n < 0 {
        return None;
    }
    let cap = 1usize << 16;
    let mut items = Vec::with_capacity(n as usize);
    for idx in 0..n as u32 {
        let mut profile_id = [0u8; 16];
        let mut blk = vec![0u8; cap];
        let mut used = 0u32;
        // SAFETY: profile_id is a valid 16-byte array; blk has `cap` bytes; C
        // writes within them only when it returns nonzero; idx < n.
        let ok = unsafe {
            rcms_oracle_seq_id_elem(
                buf.as_ptr(),
                buf.len() as u32,
                sig,
                idx,
                profile_id.as_mut_ptr(),
                blk.as_mut_ptr(),
                cap as u32,
                &mut used,
            )
        };
        if ok == 0 {
            return None;
        }
        items.push(OracleSeqIdItem {
            profile_id,
            description: decode_serialized_mlu(&blk[..used as usize]),
        });
    }
    Some(items)
}

/// One dictionary entry as lcms2 exposes it (`cmsDictGetEntryList` order, which
/// is the REVERSE of the on-disk record order — see [`OracleDict::entries`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleDictEntry {
    pub name: String,
    pub value: String,
    pub display_name: Option<OracleMlu>,
    pub display_value: Option<OracleMlu>,
}

/// A `cmsHANDLE` dictionary as lcms2 exposes it. `entries` is in
/// `cmsDictGetEntryList` order (reverse of on-disk); reverse it to compare
/// against an rcms `Dict` (which stores on-disk order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleDict {
    pub entries: Vec<OracleDictEntry>,
}

/// Decode a wide u16 unit stream into a `String` via [`char::decode_utf16`].
fn units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// lcms2 `cmsReadTag` of a `dict`/`meta` tag, decoded to its entry list (in
/// `cmsDictGetEntryList` enumeration order), or `None`. A display MLU is `None`
/// when lcms2 stored a NULL MLU for that column. lcms2 reports the empty MLU and
/// the absent MLU identically here (a serialized count of 0 → an `OracleMlu`
/// with no entries); the test maps an empty `OracleMlu` to `None` to match rcms,
/// which stores `None` for a 0-size/absent cell.
pub fn read_tag_dict(buf: &[u8], sig: u32) -> Option<OracleDict> {
    // SAFETY: buf/len describe a valid readable slice C only reads.
    let n = unsafe { rcms_oracle_dict_count(buf.as_ptr(), buf.len() as u32, sig) };
    if n < 0 {
        return None;
    }
    let cap = 1usize << 16;
    let mut entries = Vec::with_capacity(n as usize);
    for idx in 0..n as u32 {
        let mut name_units = vec![0u16; cap];
        let mut value_units = vec![0u16; cap];
        let mut nn = 0u32;
        let mut vn = 0u32;
        let mut dnblk = vec![0u8; cap];
        let mut dvblk = vec![0u8; cap];
        let mut dnused = 0u32;
        let mut dvused = 0u32;
        // SAFETY: all out pointers reference valid local buffers of the declared
        // capacities; C writes within them only when it returns nonzero; idx < n.
        let ok = unsafe {
            rcms_oracle_dict_entry(
                buf.as_ptr(),
                buf.len() as u32,
                sig,
                idx,
                name_units.as_mut_ptr(),
                cap as u32,
                &mut nn,
                value_units.as_mut_ptr(),
                cap as u32,
                &mut vn,
                dnblk.as_mut_ptr(),
                cap as u32,
                &mut dnused,
                dvblk.as_mut_ptr(),
                cap as u32,
                &mut dvused,
            )
        };
        if ok == 0 {
            return None;
        }
        let to_opt_mlu = |m: OracleMlu| if m.entries.is_empty() { None } else { Some(m) };
        entries.push(OracleDictEntry {
            name: units_to_string(&name_units[..nn as usize]),
            value: units_to_string(&value_units[..vn as usize]),
            display_name: to_opt_mlu(decode_serialized_mlu(&dnblk[..dnused as usize])),
            display_value: to_opt_mlu(decode_serialized_mlu(&dvblk[..dvused as usize])),
        });
    }
    Some(OracleDict { entries })
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
