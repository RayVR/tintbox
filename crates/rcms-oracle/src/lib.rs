//! Test-only differential oracle: links lcms2 2.19.1 and exposes its primitives
//! for bit-for-bit comparison against `rcms`.

unsafe extern "C" {
    // TEMPORARY transcendental parity probe (slice 3 de-risk). KEPT.
    fn rcms_oracle_pow(x: f64, y: f64) -> f64;
    fn rcms_oracle_log(x: f64) -> f64;
    fn rcms_oracle_log10(x: f64) -> f64;
    fn rcms_oracle_eval_parametric(ty: i32, params: *const f64, nparams: i32, x: f32) -> f32;
    fn rcms_oracle_tabulated16_eval16(table: *const u16, n: u32, v: u16) -> u16;
    fn rcms_oracle_tabulated16_eval_float(table: *const u16, n: u32, x: f32) -> f32;
    fn rcms_oracle_tabulated_float_eval_float(table: *const f32, n: u32, x: f32) -> f32;
    fn rcms_oracle_parametric_eval_float(ty: i32, params: *const f64, x: f32) -> f32;
    fn rcms_oracle_parametric_table16(ty: i32, params: *const f64, out: *mut u16, cap: u32) -> i32;
    fn rcms_oracle_tabulated_float_table16(
        table: *const f32,
        n_in: u32,
        out: *mut u16,
        cap: u32,
    ) -> i32;
    fn rcms_oracle_double_to_s15f16(v: f64) -> i32;
    fn rcms_oracle_s15f16_to_double(a: i32) -> f64;
    fn rcms_oracle_double_to_8fixed8(v: f64) -> u16;
    fn rcms_oracle_to_fixed_domain(a: i32) -> i32;
    fn rcms_oracle_from_fixed_domain(a: i32) -> i32;
    fn rcms_oracle_quick_floor(v: f64) -> i32;
    fn rcms_oracle_quick_floor_word(d: f64) -> u16;
    fn rcms_oracle_quick_saturate_word(d: f64) -> u16;
    fn rcms_oracle_xyz2lab(wp: *const f64, xyz: *const f64, lab: *mut f64);
    fn rcms_oracle_lab2xyz(wp: *const f64, lab: *const f64, xyz: *mut f64);
    fn rcms_oracle_xyz2xyy(xyz: *const f64, xyy: *mut f64);
    fn rcms_oracle_xyy2xyz(xyy: *const f64, xyz: *mut f64);
    fn rcms_oracle_lab2lch(lab: *const f64, lch: *mut f64);
    fn rcms_oracle_lch2lab(lch: *const f64, lab: *mut f64);
    fn rcms_oracle_lab_enc2float_v4(wlab: *const u16, lab: *mut f64);
    fn rcms_oracle_float2lab_enc_v4(lab: *const f64, wlab: *mut u16);
    fn rcms_oracle_lab_enc2float_v2(wlab: *const u16, lab: *mut f64);
    fn rcms_oracle_float2lab_enc_v2(lab: *const f64, wlab: *mut u16);
    fn rcms_oracle_xyz_enc2float(wxyz: *const u16, xyz: *mut f64);
    fn rcms_oracle_float2xyz_enc(xyz: *const f64, wxyz: *mut u16);
    fn rcms_oracle_mat3_eval(out: *mut f64, m: *const f64, v: *const f64);
    fn rcms_oracle_mat3_per(out: *mut f64, a: *const f64, b: *const f64);
    fn rcms_oracle_mat3_inverse(out: *mut f64, a: *const f64) -> i32;
    fn rcms_oracle_mat3_solve(out: *mut f64, a: *const f64, b: *const f64) -> i32;
    fn rcms_oracle_white_point_from_temp(out: *mut f64, temp_k: f64) -> i32;
    fn rcms_oracle_adapt_to_illuminant(
        out: *mut f64,
        src_wp: *const f64,
        illuminant: *const f64,
        value: *const f64,
    ) -> i32;
    fn rcms_oracle_adaptation_matrix(out: *mut f64, from: *const f64, to: *const f64) -> i32;
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
    fn rcms_oracle_tag_read_succeeds(buf: *const u8, len: u32, sig: u32) -> i32;
    fn rcms_oracle_tetra16(
        grid: *const u32,
        n_out: u32,
        table: *const u16,
        table_len: u32,
        input: *const u16,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_tetra_float(
        grid: *const u32,
        n_out: u32,
        table: *const f32,
        table_len: u32,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_interp16(
        grid: *const u32,
        n_in: u32,
        n_out: u32,
        table: *const u16,
        dw_flags: u32,
        input: *const u16,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_interp_float(
        grid: *const u32,
        n_in: u32,
        n_out: u32,
        table: *const f32,
        dw_flags: u32,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
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
    fn rcms_oracle_read_tag_curve(
        buf: *const u8,
        len: u32,
        sig: u32,
        xs: *const f32,
        n: u32,
        ys: *mut f32,
    ) -> i32;
    fn rcms_oracle_read_tag_vcgt(
        buf: *const u8,
        len: u32,
        sig: u32,
        xs: *const f32,
        n: u32,
        ys: *mut f32,
    ) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn rcms_oracle_read_tag_ucrbg(
        buf: *const u8,
        len: u32,
        sig: u32,
        xs: *const f32,
        n: u32,
        ucr_ys: *mut f32,
        bg_ys: *mut f32,
        desc: *mut u8,
        dcap: u32,
        dused: *mut u32,
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
    fn rcms_oracle_pipeline_matrix_eval16(
        rows: u32,
        cols: u32,
        matrix: *const f64,
        offset: *const f64,
        input: *const u16,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_pipeline_matrix_eval_float(
        rows: u32,
        cols: u32,
        matrix: *const f64,
        offset: *const f64,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_pipeline_curves_eval16(
        n_curves: u32,
        tbl_len: u32,
        tables: *const u16,
        input: *const u16,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_pipeline_curves_eval_float(
        n_curves: u32,
        tbl_len: u32,
        tables: *const u16,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_pipeline_curves_matrix_eval16(
        n_curves: u32,
        tbl_len: u32,
        tables: *const u16,
        rows: u32,
        cols: u32,
        matrix: *const f64,
        offset: *const f64,
        input: *const u16,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_pipeline_curves_matrix_eval_float(
        n_curves: u32,
        tbl_len: u32,
        tables: *const u16,
        rows: u32,
        cols: u32,
        matrix: *const f64,
        offset: *const f64,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_clut_stage_eval16(
        grid: *const u32,
        n_in: u32,
        n_out: u32,
        table: *const u16,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_clut_stage_eval_float(
        grid: *const u32,
        n_in: u32,
        n_out: u32,
        table: *const f32,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_labxyz_stage_eval(which: u32, input: *const f32, out: *mut f32) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn rcms_oracle_pipeline_clut_curves_matrix_eval_float(
        grid: *const u32,
        n_in: u32,
        n_out: u32,
        clut_table: *const u16,
        tbl_len: u32,
        curve_tables: *const u16,
        rows: u32,
        matrix: *const f64,
        offset: *const f64,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn rcms_oracle_pipeline_cat_eval_float(
        tbl_len: u32,
        curve_tables: *const u16,
        mat_a: *const f64,
        off_a: *const f64,
        grid: *const u32,
        clut_table: *const u16,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_pipeline_prepend_eval_float(
        tbl_len: u32,
        curve_tables: *const u16,
        mat_a: *const f64,
        off_a: *const f64,
        input: *const f32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_lut_channels(
        buf: *const u8,
        len: u32,
        sig: u32,
        n_in: *mut u32,
        n_out: *mut u32,
    ) -> i32;
    fn rcms_oracle_lut_eval16(
        buf: *const u8,
        len: u32,
        sig: u32,
        inputs: *const u16,
        n_samples: u32,
        out: *mut u16,
    ) -> i32;
    fn rcms_oracle_lut_eval_float(
        buf: *const u8,
        len: u32,
        sig: u32,
        inputs: *const f32,
        n_samples: u32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_read_lut_channels(
        buf: *const u8,
        len: u32,
        which: u32,
        intent: u32,
        n_in: *mut u32,
        n_out: *mut u32,
    ) -> i32;
    fn rcms_oracle_read_lut_eval_float(
        buf: *const u8,
        len: u32,
        which: u32,
        intent: u32,
        inputs: *const f32,
        n_samples: u32,
        out: *mut f32,
    ) -> i32;
    fn rcms_oracle_reverse_tabulated16_eval_float(
        table: *const u16,
        n: u32,
        xs: *const f32,
        nx: u32,
        ys: *mut f32,
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

/// C libm `pow(x, y)` — the exact function lcms2's parametric curve evaluator
/// calls. TEMPORARY transcendental parity probe (slice 3 de-risk); KEPT.
pub fn libm_pow(x: f64, y: f64) -> f64 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_pow(x, y) }
}
/// C libm `log(x)` (natural log). TEMPORARY parity probe; KEPT.
pub fn libm_log(x: f64) -> f64 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_log(x) }
}
/// C libm `log10(x)`. TEMPORARY parity probe; KEPT.
pub fn libm_log10(x: f64) -> f64 {
    // SAFETY: pure C arithmetic, no pointers, no allocation.
    unsafe { rcms_oracle_log10(x) }
}

/// lcms2 `cmsBuildParametricToneCurve` + `cmsEvalToneCurveFloat`: builds a
/// one-segment parametric curve of `ty` with `params` and evaluates it at `x`.
/// Because the segment spans `(MINUS_INF, PLUS_INF]`, this dispatches straight
/// to `DefaultEvalParametricFn` for any finite `x` (the only extra processing is
/// `EvalSegmentedFn`'s infinity clamp to `±1E22` and the final `f32` cast).
/// Returns `None` when lcms2 rejects the type/params (signalled as NaN by the
/// shim), so callers skip those param sets.
pub fn eval_parametric(ty: i32, params: &[f64], x: f32) -> Option<f32> {
    // SAFETY: `params` is a valid readable slice of `params.len()` f64s; C reads
    // exactly `ParameterCount[ty]` of them (the test always supplies at least
    // that many). The curve handle is built and freed entirely inside the call.
    let y = unsafe { rcms_oracle_eval_parametric(ty, params.as_ptr(), params.len() as i32, x) };
    if y.is_nan() {
        None
    } else {
        Some(y)
    }
}

/// lcms2 `cmsBuildTabulatedToneCurve16` + `cmsEvalToneCurve16`: builds a 16-bit
/// tabulated curve from `table` and evaluates it at `v`.
pub fn tabulated16_eval16(table: &[u16], v: u16) -> u16 {
    // SAFETY: `table` is a valid readable slice of `table.len()` u16s C only reads
    // (copied into a curve handle built and freed inside the call).
    unsafe { rcms_oracle_tabulated16_eval16(table.as_ptr(), table.len() as u32, v) }
}

/// lcms2 `cmsBuildTabulatedToneCurve16` + `cmsEvalToneCurveFloat` at `x`.
pub fn tabulated16_eval_float(table: &[u16], x: f32) -> f32 {
    // SAFETY: `table` is a valid readable slice C only reads; the curve handle is
    // built and freed inside the call.
    unsafe { rcms_oracle_tabulated16_eval_float(table.as_ptr(), table.len() as u32, x) }
}

/// lcms2 `cmsBuildTabulatedToneCurveFloat` + `cmsEvalToneCurveFloat` at `x`.
/// Returns `None` when lcms2 rejects the table (empty), signalled as NaN.
pub fn tabulated_float_eval_float(table: &[f32], x: f32) -> Option<f32> {
    // SAFETY: `table` is a valid readable slice C only reads; the curve handle is
    // built and freed inside the call.
    let y =
        unsafe { rcms_oracle_tabulated_float_eval_float(table.as_ptr(), table.len() as u32, x) };
    if y.is_nan() {
        None
    } else {
        Some(y)
    }
}

/// lcms2 `cmsBuildParametricToneCurve` + `cmsEvalToneCurveFloat` at `x`. Returns
/// `None` when lcms2 rejects the type/params (signalled as NaN).
pub fn parametric_eval_float(ty: i32, params: &[f64], x: f32) -> Option<f32> {
    // SAFETY: `params` is a valid readable slice; C reads exactly
    // `ParameterCount[ty]` of them. The curve handle is built and freed inside.
    let y = unsafe { rcms_oracle_parametric_eval_float(ty, params.as_ptr(), x) };
    if y.is_nan() {
        None
    } else {
        Some(y)
    }
}

/// lcms2 `cmsBuildParametricToneCurve` + `cmsGetToneCurveEstimatedTable`: the
/// materialised 16-bit approximation table, or `None` when lcms2 rejects the
/// type/params.
pub fn parametric_table16(ty: i32, params: &[f64]) -> Option<Vec<u16>> {
    let cap = 4096usize; // lcms2's max grid points for a curve.
    let mut out = vec![0u16; cap];
    // SAFETY: `params` is a valid readable slice C reads `ParameterCount[ty]` of;
    // `out` has `cap` u16 of room, which exceeds any table lcms2 materialises. C
    // writes at most `cap` entries and returns the count, or -1 on reject.
    let n = unsafe {
        rcms_oracle_parametric_table16(ty, params.as_ptr(), out.as_mut_ptr(), cap as u32)
    };
    if n < 0 {
        None
    } else {
        out.truncate(n as usize);
        Some(out)
    }
}

/// lcms2 3D CLUT (16-bit) tetrahedral interpolation: builds a single granular
/// CLUT stage (`cmsStageAllocCLut16bitGranular`) with per-axis sample counts
/// `grid` (3 axes) and `n_out` output channels from `table`, wraps it in a
/// 3->n_out pipeline, and evaluates `input` (3 u16) through `cmsPipelineEval16`.
/// Returns the `n_out` u16 outputs, or `None` if lcms2 fails to allocate.
pub fn tetra16(grid: &[u32; 3], n_out: usize, table: &[u16], input: &[u16; 3]) -> Option<Vec<u16>> {
    let mut out = vec![0u16; n_out];
    // SAFETY: `grid` is 3 readable u32; `table` is a readable slice of `table.len()`
    // u16 (copied into the CLUT stage); `input` is 3 readable u16; `out` has `n_out`
    // u16 of room (the pipeline output width). C only reads inputs and writes exactly
    // `n_out` outputs; the stage and pipeline are allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_tetra16(
            grid.as_ptr(),
            n_out as u32,
            table.as_ptr(),
            table.len() as u32,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 3D CLUT (float) tetrahedral interpolation: builds a single granular
/// CLUT stage (`cmsStageAllocCLutFloatGranular`) with per-axis sample counts
/// `grid` (3 axes) and `n_out` output channels from `table`, wraps it in a
/// 3->n_out pipeline, and evaluates `input` (3 f32) through `cmsPipelineEvalFloat`.
/// Returns the `n_out` f32 outputs, or `None` if lcms2 fails to allocate.
pub fn tetra_float(
    grid: &[u32; 3],
    n_out: usize,
    table: &[f32],
    input: &[f32; 3],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_out];
    // SAFETY: `grid` is 3 readable u32; `table` is a readable slice of `table.len()`
    // f32 (copied into the CLUT stage); `input` is 3 readable f32; `out` has `n_out`
    // f32 of room (the pipeline output width). C only reads inputs and writes exactly
    // `n_out` outputs; the stage and pipeline are allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_tetra_float(
            grid.as_ptr(),
            n_out as u32,
            table.as_ptr(),
            table.len() as u32,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 flag word for the trilinear interpolation hint
/// (`CMS_LERP_FLAGS_TRILINEAR`, lcms2_plugin.h). OR this into `dw_flags` to force
/// the 3-input path through `TrilinearInterp16`/`Float` instead of the
/// tetrahedral default — the `cmsStageAllocCLut*` path never sets it.
pub const CMS_LERP_FLAGS_TRILINEAR: u32 = 0x0100;

/// lcms2 generic n-D CLUT interpolation (16-bit): builds a `cmsInterpParams` from
/// the per-axis `grid` (`n_in` axes), `n_out` outputs, `table`, and `dw_flags`,
/// then invokes the `Lerp16` routine `DefaultInterpolatorsFactory` selected. This
/// reaches `BilinearInterp16` (2 in), `TrilinearInterp16` (3 in +
/// [`CMS_LERP_FLAGS_TRILINEAR`]), `TetrahedralInterp16` (3 in, no flag), and
/// `Eval4Inputs`..`Eval15Inputs` (4..15 in). Returns the `n_out` u16 outputs, or
/// `None` if lcms2 fails to compute the params.
pub fn interp16(
    grid: &[u32],
    n_out: usize,
    table: &[u16],
    dw_flags: u32,
    input: &[u16],
) -> Option<Vec<u16>> {
    let mut out = vec![0u16; n_out];
    // SAFETY: `grid` is `n_in` readable u32 (n_in = grid.len()); `table` is a
    // readable slice copied into the interp params; `input` is `n_in` readable u16;
    // `out` has `n_out` u16 of room (the interpolator's output width). C only reads
    // inputs and writes exactly `n_out` outputs; the params are allocated and freed
    // inside the call.
    let ok = unsafe {
        rcms_oracle_interp16(
            grid.as_ptr(),
            grid.len() as u32,
            n_out as u32,
            table.as_ptr(),
            dw_flags,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 generic n-D CLUT interpolation (float). Like [`interp16`] but routes
/// through the `LerpFloat` routines (`CMS_LERP_FLAGS_FLOAT` is OR'd in by the
/// shim). Returns the `n_out` f32 outputs, or `None`.
pub fn interp_float(
    grid: &[u32],
    n_out: usize,
    table: &[f32],
    dw_flags: u32,
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_out];
    // SAFETY: `grid` is `n_in` readable u32; `table` is a readable slice copied into
    // the interp params; `input` is `n_in` readable f32; `out` has `n_out` f32 of
    // room. C only reads inputs and writes exactly `n_out` outputs; the params are
    // allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_interp_float(
            grid.as_ptr(),
            grid.len() as u32,
            n_out as u32,
            table.as_ptr(),
            dw_flags,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 pipeline (`cmsPipelineAlloc` + `cmsStageAllocMatrix` +
/// `cmsPipelineEval16`) holding a single Matrix stage `Cols -> Rows`. `matrix`
/// is row-major `rows * cols` f64; `offset` is `rows` f64 or empty (NULL). `input`
/// is `cols` u16. Returns the `rows` u16 outputs, or `None` if lcms2 rejects.
pub fn pipeline_matrix_eval16(
    rows: usize,
    cols: usize,
    matrix: &[f64],
    offset: Option<&[f64]>,
    input: &[u16],
) -> Option<Vec<u16>> {
    let mut out = vec![0u16; rows];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: `matrix` is `rows*cols` readable f64, `offset` (when Some) is `rows`
    // readable f64 else NULL, `input` is `cols` readable u16; `out` has `rows` u16.
    // C only reads inputs and writes exactly `rows` outputs; the stage and
    // pipeline are allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_pipeline_matrix_eval16(
            rows as u32,
            cols as u32,
            matrix.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Float counterpart of [`pipeline_matrix_eval16`] via `cmsPipelineEvalFloat`.
pub fn pipeline_matrix_eval_float(
    rows: usize,
    cols: usize,
    matrix: &[f64],
    offset: Option<&[f64]>,
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; rows];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: as `pipeline_matrix_eval16` but `input`/`out` are `cols`/`rows` f32.
    let ok = unsafe {
        rcms_oracle_pipeline_matrix_eval_float(
            rows as u32,
            cols as u32,
            matrix.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 pipeline holding a single ToneCurves stage built from `n_curves`
/// 16-bit tabulated curves (`cmsBuildTabulatedToneCurve16`), each of length
/// `tbl_len`, packed contiguously in `tables` (length `n_curves * tbl_len`).
/// `input` is `n_curves` u16. Returns the `n_curves` u16 outputs, or `None`.
pub fn pipeline_curves_eval16(
    n_curves: usize,
    tbl_len: usize,
    tables: &[u16],
    input: &[u16],
) -> Option<Vec<u16>> {
    let mut out = vec![0u16; n_curves];
    // SAFETY: `tables` is `n_curves*tbl_len` readable u16, `input` is `n_curves`
    // readable u16; `out` has `n_curves` u16. C copies the tables into curve
    // handles, evaluates, and frees everything inside the call.
    let ok = unsafe {
        rcms_oracle_pipeline_curves_eval16(
            n_curves as u32,
            tbl_len as u32,
            tables.as_ptr(),
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Float counterpart of [`pipeline_curves_eval16`] via `cmsPipelineEvalFloat`.
pub fn pipeline_curves_eval_float(
    n_curves: usize,
    tbl_len: usize,
    tables: &[u16],
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_curves];
    // SAFETY: as `pipeline_curves_eval16` but `input`/`out` are `n_curves` f32.
    let ok = unsafe {
        rcms_oracle_pipeline_curves_eval_float(
            n_curves as u32,
            tbl_len as u32,
            tables.as_ptr(),
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 pipeline `ToneCurves -> Matrix`: a curves stage (`n_curves` channels,
/// from 16-bit tabulated tables as in [`pipeline_curves_eval16`]) feeding a
/// Matrix stage `cols -> rows` (with `cols == n_curves`). `input` is `n_curves`
/// u16; returns the `rows` u16 outputs, or `None`.
#[allow(clippy::too_many_arguments)]
pub fn pipeline_curves_matrix_eval16(
    n_curves: usize,
    tbl_len: usize,
    tables: &[u16],
    rows: usize,
    cols: usize,
    matrix: &[f64],
    offset: Option<&[f64]>,
    input: &[u16],
) -> Option<Vec<u16>> {
    let mut out = vec![0u16; rows];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: `tables` is `n_curves*tbl_len` readable u16; `matrix` is `rows*cols`
    // readable f64; `offset` (Some) is `rows` readable f64 else NULL; `input` is
    // `n_curves` readable u16; `out` has `rows` u16. All handles freed inside.
    let ok = unsafe {
        rcms_oracle_pipeline_curves_matrix_eval16(
            n_curves as u32,
            tbl_len as u32,
            tables.as_ptr(),
            rows as u32,
            cols as u32,
            matrix.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Float counterpart of [`pipeline_curves_matrix_eval16`].
#[allow(clippy::too_many_arguments)]
pub fn pipeline_curves_matrix_eval_float(
    n_curves: usize,
    tbl_len: usize,
    tables: &[u16],
    rows: usize,
    cols: usize,
    matrix: &[f64],
    offset: Option<&[f64]>,
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; rows];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: as `pipeline_curves_matrix_eval16` but `input`/`out` are f32.
    let ok = unsafe {
        rcms_oracle_pipeline_curves_matrix_eval_float(
            n_curves as u32,
            tbl_len as u32,
            tables.as_ptr(),
            rows as u32,
            cols as u32,
            matrix.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 16-bit CLUT stage evaluated in the float domain
/// (`cmsStageAllocCLut16bitGranular` -> `cmsPipelineEvalFloat`), exercising
/// `EvaluateCLUTfloatIn16` (FromFloatTo16 -> Lerp16 -> From16ToFloat). `grid` is
/// the per-axis sample count (`n_in` entries); `table` is row-major with `n_out`
/// values per node; `input` is `n_in` f32. Returns the `n_out` f32 outputs.
pub fn clut_stage_eval16(
    grid: &[u32],
    n_out: usize,
    table: &[u16],
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_out];
    // SAFETY: `grid` is `n_in` readable u32; `table` is a readable slice copied
    // into the CLUT stage; `input` is `n_in` readable f32; `out` has `n_out` f32.
    // C only reads inputs and writes exactly `n_out` outputs; stage and pipeline
    // are allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_clut_stage_eval16(
            grid.as_ptr(),
            grid.len() as u32,
            n_out as u32,
            table.as_ptr(),
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 float CLUT stage (`cmsStageAllocCLutFloatGranular` ->
/// `cmsPipelineEvalFloat`), exercising `EvaluateCLUTfloat` (direct `LerpFloat`).
pub fn clut_stage_eval_float(
    grid: &[u32],
    n_out: usize,
    table: &[f32],
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_out];
    // SAFETY: as `clut_stage_eval16` but `table`/`input`/`out` are f32.
    let ok = unsafe {
        rcms_oracle_clut_stage_eval_float(
            grid.as_ptr(),
            grid.len() as u32,
            n_out as u32,
            table.as_ptr(),
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 Lab/XYZ conversion stage evaluated in the float domain (1-stage 3->3
/// pipeline -> `cmsPipelineEvalFloat`). `which`: 0 = `_cmsStageAllocLab2XYZ`,
/// 1 = `_cmsStageAllocXYZ2Lab`, 2 = `_cmsStageAllocLabV2ToV4`,
/// 3 = `_cmsStageAllocLabV4ToV2`. `input` is 3 f32; returns 3 f32.
pub fn labxyz_stage_eval(which: u32, input: &[f32; 3]) -> Option<[f32; 3]> {
    let mut out = [0f32; 3];
    // SAFETY: `input` is 3 readable f32; `out` has 3 f32. C only reads inputs and
    // writes exactly 3 outputs; the stage and pipeline are freed inside the call.
    let ok = unsafe { rcms_oracle_labxyz_stage_eval(which, input.as_ptr(), out.as_mut_ptr()) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 combined `CLUT -> ToneCurves -> Matrix` pipeline via
/// `cmsPipelineEvalFloat`. The CLUT is 16-bit (`grid`/`clut_table`, `n_out`
/// channels); the curves stage has `n_out` 16-bit tabulated curves of length
/// `tbl_len`; the matrix is `rows x n_out` (+ optional `offset`). `input` is
/// `n_in` f32; returns `rows` f32.
#[allow(clippy::too_many_arguments)]
pub fn pipeline_clut_curves_matrix_eval_float(
    grid: &[u32],
    n_out: usize,
    clut_table: &[u16],
    tbl_len: usize,
    curve_tables: &[u16],
    rows: usize,
    matrix: &[f64],
    offset: Option<&[f64]>,
    input: &[f32],
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; rows];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: `grid` is `n_in` readable u32; `clut_table` is row-major `n_out`
    // per node; `curve_tables` is `n_out*tbl_len` readable u16; `matrix` is
    // `rows*n_out` f64; `offset` (when Some) is `rows` f64 else NULL; `input` is
    // `n_in` f32; `out` has `rows` f32. C copies everything into stages and frees
    // them inside the call.
    let ok = unsafe {
        rcms_oracle_pipeline_clut_curves_matrix_eval_float(
            grid.as_ptr(),
            grid.len() as u32,
            n_out as u32,
            clut_table.as_ptr(),
            tbl_len as u32,
            curve_tables.as_ptr(),
            rows as u32,
            matrix.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsPipelineCat`: builds A = `[ToneCurves(3, tbl_len) -> Matrix(3x3)]`
/// and B = `[CLut16(3->3)]`, runs `cmsPipelineCat(A, B)`, then evaluates `input`
/// (3 f32) through the catenated A via `cmsPipelineEvalFloat`. `curve_tables` is
/// `3 * tbl_len` u16 (3 contiguous 16-bit tabulated curves); `mat` is 9 row-major
/// f64; `offset` is 3 f64 or `None`; `grid` is 3 per-axis sample counts; the CLUT
/// `table` is `grid-product * 3` u16. Returns the 3 f32 outputs, or `None` on any
/// lcms2 failure.
#[allow(clippy::too_many_arguments)]
pub fn pipeline_cat_eval_float(
    tbl_len: usize,
    curve_tables: &[u16],
    mat: &[f64],
    offset: Option<&[f64]>,
    grid: &[u32],
    clut_table: &[u16],
    input: &[f32],
) -> Option<[f32; 3]> {
    let mut out = [0f32; 3];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: `curve_tables` is `3 * tbl_len` readable u16; `mat` is 9 readable
    // f64; `off_ptr` is null or 3 readable f64; `grid` is 3 readable u32;
    // `clut_table` is grid-product*3 readable u16; `input` is 3 readable f32;
    // `out` has 3 f32. C only reads inputs and writes exactly 3 outputs; the two
    // pipelines and all stages are allocated and freed inside the call.
    let ok = unsafe {
        rcms_oracle_pipeline_cat_eval_float(
            tbl_len as u32,
            curve_tables.as_ptr(),
            mat.as_ptr(),
            off_ptr,
            grid.as_ptr(),
            clut_table.as_ptr(),
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsPipelineInsertStage(.., cmsAT_BEGIN, ..)`: builds
/// `P = [ToneCurves(3, tbl_len)]`, prepends a 3x3 Matrix stage (so the pipeline
/// becomes `[Matrix -> ToneCurves]`), then evaluates `input` (3 f32) via
/// `cmsPipelineEvalFloat`. Arguments as `pipeline_cat_eval_float`. Returns the 3
/// f32 outputs, or `None` on lcms2 failure.
pub fn pipeline_prepend_eval_float(
    tbl_len: usize,
    curve_tables: &[u16],
    mat: &[f64],
    offset: Option<&[f64]>,
    input: &[f32],
) -> Option<[f32; 3]> {
    let mut out = [0f32; 3];
    let off_ptr = offset.map_or(core::ptr::null(), |o| o.as_ptr());
    // SAFETY: as `pipeline_cat_eval_float`, minus the CLUT inputs.
    let ok = unsafe {
        rcms_oracle_pipeline_prepend_eval_float(
            tbl_len as u32,
            curve_tables.as_ptr(),
            mat.as_ptr(),
            off_ptr,
            input.as_ptr(),
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsBuildTabulatedToneCurveFloat` + `cmsGetToneCurveEstimatedTable`: the
/// materialised 16-bit table, or `None` when lcms2 rejects the table.
pub fn tabulated_float_table16(table: &[f32]) -> Option<Vec<u16>> {
    let cap = 4096usize;
    let mut out = vec![0u16; cap];
    // SAFETY: `table` is a valid readable slice C only reads; `out` has `cap` u16,
    // exceeding any table lcms2 materialises. C writes at most `cap` and returns
    // the count, or -1 on reject.
    let n = unsafe {
        rcms_oracle_tabulated_float_table16(
            table.as_ptr(),
            table.len() as u32,
            out.as_mut_ptr(),
            cap as u32,
        )
    };
    if n < 0 {
        None
    } else {
        out.truncate(n as usize);
        Some(out)
    }
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

/// lcms2 `cmsXYZ2Lab`. `wp == None` lets lcms2 default to D50 (it sees a NULL
/// white point). Returns `[L, a, b]`.
pub fn xyz2lab(wp: Option<[f64; 3]>, xyz: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    let wp_ptr = wp.as_ref().map_or(core::ptr::null(), |w| w.as_ptr());
    // SAFETY: xyz/out are valid 3-double arrays C reads/writes; wp_ptr is either
    // null (C defaults to D50) or points at a valid 3-double array C only reads.
    unsafe { rcms_oracle_xyz2lab(wp_ptr, xyz.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsLab2XYZ`. `wp == None` → D50. Returns `[X, Y, Z]`.
pub fn lab2xyz(wp: Option<[f64; 3]>, lab: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    let wp_ptr = wp.as_ref().map_or(core::ptr::null(), |w| w.as_ptr());
    // SAFETY: lab/out are valid 3-double arrays C reads/writes; wp_ptr is null or
    // a valid 3-double array C only reads.
    unsafe { rcms_oracle_lab2xyz(wp_ptr, lab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsXYZ2xyY`. Returns `[x, y, Y]`.
pub fn xyz2xyy(xyz: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: xyz/out are valid 3-double arrays C reads/writes.
    unsafe { rcms_oracle_xyz2xyy(xyz.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsxyY2XYZ`. Returns `[X, Y, Z]`.
pub fn xyy2xyz(xyy: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: xyy/out are valid 3-double arrays C reads/writes.
    unsafe { rcms_oracle_xyy2xyz(xyy.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsLab2LCh`. Returns `[L, C, h]`.
pub fn lab2lch(lab: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: lab/out are valid 3-double arrays C reads/writes.
    unsafe { rcms_oracle_lab2lch(lab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsLCh2Lab`. Returns `[L, a, b]`.
pub fn lch2lab(lch: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: lch/out are valid 3-double arrays C reads/writes.
    unsafe { rcms_oracle_lch2lab(lch.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsLabEncoded2Float` (v4). Returns `[L, a, b]`.
pub fn lab_enc2float_v4(wlab: &[u16; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: wlab is a valid 3-u16 array C reads; out a valid 3-double array C writes.
    unsafe { rcms_oracle_lab_enc2float_v4(wlab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsFloat2LabEncoded` (v4). Returns `[L, a, b]` u16.
pub fn float2lab_enc_v4(lab: &[f64; 3]) -> [u16; 3] {
    let mut out = [0u16; 3];
    // SAFETY: lab is a valid 3-double array C reads; out a valid 3-u16 array C writes.
    unsafe { rcms_oracle_float2lab_enc_v4(lab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsLabEncoded2FloatV2`. Returns `[L, a, b]`.
pub fn lab_enc2float_v2(wlab: &[u16; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: wlab is a valid 3-u16 array C reads; out a valid 3-double array C writes.
    unsafe { rcms_oracle_lab_enc2float_v2(wlab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsFloat2LabEncodedV2`. Returns `[L, a, b]` u16.
pub fn float2lab_enc_v2(lab: &[f64; 3]) -> [u16; 3] {
    let mut out = [0u16; 3];
    // SAFETY: lab is a valid 3-double array C reads; out a valid 3-u16 array C writes.
    unsafe { rcms_oracle_float2lab_enc_v2(lab.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsXYZEncoded2Float`. Returns `[X, Y, Z]`.
pub fn xyz_enc2float(wxyz: &[u16; 3]) -> [f64; 3] {
    let mut out = [0.0f64; 3];
    // SAFETY: wxyz is a valid 3-u16 array C reads; out a valid 3-double array C writes.
    unsafe { rcms_oracle_xyz_enc2float(wxyz.as_ptr(), out.as_mut_ptr()) };
    out
}
/// lcms2 `cmsFloat2XYZEncoded`. Returns `[X, Y, Z]` u16.
pub fn float2xyz_enc(xyz: &[f64; 3]) -> [u16; 3] {
    let mut out = [0u16; 3];
    // SAFETY: xyz is a valid 3-double array C reads; out a valid 3-u16 array C writes.
    unsafe { rcms_oracle_float2xyz_enc(xyz.as_ptr(), out.as_mut_ptr()) };
    out
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

/// lcms2 `cmsWhitePointFromTemp`. Returns `Some([x, y, Y])` for a temperature in
/// `[4000, 25000]`, else `None`.
pub fn white_point_from_temp(temp_k: f64) -> Option<[f64; 3]> {
    let mut out = [0.0f64; 3];
    // SAFETY: out is a valid 3-double array C writes; temp_k is a plain scalar.
    // C writes 3 doubles to out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_white_point_from_temp(out.as_mut_ptr(), temp_k) };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsAdaptToIlluminant`. Adapts `value` (XYZ) from `src_wp` to
/// `illuminant` via Bradford. Returns `None` on singular adaptation.
pub fn adapt_to_illuminant(
    src_wp: &[f64; 3],
    illuminant: &[f64; 3],
    value: &[f64; 3],
) -> Option<[f64; 3]> {
    let mut out = [0.0f64; 3];
    // SAFETY: out/src_wp/illuminant/value are valid 3-double arrays C reads/writes.
    // C writes 3 doubles to out only when it returns nonzero.
    let ok = unsafe {
        rcms_oracle_adapt_to_illuminant(
            out.as_mut_ptr(),
            src_wp.as_ptr(),
            illuminant.as_ptr(),
            value.as_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `_cmsAdaptationMatrix` with a NULL cone matrix (Bradford). Returns the
/// 9 row-major matrix entries, or `None` on singular adaptation.
pub fn adaptation_matrix(from: &[f64; 3], to: &[f64; 3]) -> Option<[f64; 9]> {
    let mut out = [0.0f64; 9];
    // SAFETY: out is a valid 9-double array C writes; from/to are valid 3-double
    // arrays C reads. C writes 9 doubles to out only when it returns nonzero.
    let ok = unsafe { rcms_oracle_adaptation_matrix(out.as_mut_ptr(), from.as_ptr(), to.as_ptr()) };
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

/// Whether lcms2's `cmsReadTag(sig)` returns a non-NULL cooked value. `false`
/// means lcms2 itself rejects the tag's contents (e.g. a malformed `mpet`),
/// distinguishing "rcms bug" from "both stacks correctly reject this tag".
pub fn tag_read_succeeds(buf: &[u8], sig: u32) -> bool {
    // SAFETY: buf/len describe a valid readable slice C only reads; the profile
    // and tag are opened/read/closed entirely inside the call.
    let ok = unsafe { rcms_oracle_tag_read_succeeds(buf.as_ptr(), buf.len() as u32, sig) };
    ok != 0
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

/// lcms2 `cmsReadTag` of a `curv`/`para` tag -> a `cmsToneCurve*`, sampled via
/// `cmsEvalToneCurveFloat` at each `x` in `xs`. Returns the per-point samples (one
/// per `x`), or `None` if lcms2 cannot open the profile or the tag is absent / not
/// tone-curve-backed. This is the bit-exact reference for an rcms `Tag::Curve`'s
/// `eval_float` at the same points.
pub fn read_tag_curve(buf: &[u8], sig: u32, xs: &[f32]) -> Option<Vec<f32>> {
    let mut ys = vec![0.0f32; xs.len()];
    // SAFETY: buf/len describe a valid readable slice C only reads; xs is a valid
    // readable slice of `xs.len()` f32, and ys has exactly that many f32 of room,
    // which is what C writes when it returns nonzero. The cmsToneCurve* C samples
    // is owned by the profile (freed on cmsCloseProfile inside the call).
    let ok = unsafe {
        rcms_oracle_read_tag_curve(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            xs.as_ptr(),
            xs.len() as u32,
            ys.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(ys)
    } else {
        None
    }
}

/// lcms2 `cmsReadTag` of a `vcgt` tag -> a `cmsToneCurve**` (3 R/G/B curves),
/// each sampled via `cmsEvalToneCurveFloat` at every `x` in `xs`. Returns one
/// `Vec<f32>` of length `xs.len()` per channel (`[r, g, b]`), or `None` if lcms2
/// cannot open the profile or the tag is absent / not vcgt-backed.
pub fn read_tag_vcgt(buf: &[u8], sig: u32, xs: &[f32]) -> Option<[Vec<f32>; 3]> {
    let mut ys = vec![0.0f32; xs.len() * 3];
    // SAFETY: buf/len describe a valid readable slice C only reads; xs is a valid
    // readable slice of `xs.len()` f32, and ys has room for 3*xs.len() f32, which
    // is exactly what C writes (row-major, 3 channels) when it returns nonzero.
    // The cmsToneCurve** C samples is owned by the profile (freed on close).
    let ok = unsafe {
        rcms_oracle_read_tag_vcgt(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            xs.as_ptr(),
            xs.len() as u32,
            ys.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return None;
    }
    let n = xs.len();
    Some([
        ys[0..n].to_vec(),
        ys[n..2 * n].to_vec(),
        ys[2 * n..3 * n].to_vec(),
    ])
}

/// A `cmsUcrBg` as lcms2 exposes it: the Ucr/Bg curves sampled at the requested
/// points and the ASCII `Desc` string (`cmsMLUgetASCII` of the no-language entry).
#[derive(Clone, Debug, PartialEq)]
pub struct OracleUcrBg {
    pub ucr: Vec<f32>,
    pub bg: Vec<f32>,
    pub desc: String,
}

/// lcms2 `cmsReadTag` of a `bfd ` (UcrBg) tag -> the Ucr/Bg curves sampled via
/// `cmsEvalToneCurveFloat` at every `x` in `xs`, plus the ASCII `Desc`. Returns
/// `None` if lcms2 cannot open the profile or the tag is absent / not UcrBg-backed.
pub fn read_tag_ucrbg(buf: &[u8], sig: u32, xs: &[f32]) -> Option<OracleUcrBg> {
    let mut ucr = vec![0.0f32; xs.len()];
    let mut bg = vec![0.0f32; xs.len()];
    let dcap = 1usize << 16;
    let mut desc = vec![0u8; dcap];
    let mut dused = 0u32;
    // SAFETY: buf/len describe a valid readable slice C only reads; xs is a valid
    // readable slice; ucr/bg have room for xs.len() f32 each (what C writes); desc
    // has `dcap` bytes and C writes at most `dcap` (NUL-terminated), reporting the
    // byte count (sans NUL) via dused. The cmsUcrBg* is owned by the profile.
    let ok = unsafe {
        rcms_oracle_read_tag_ucrbg(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            xs.as_ptr(),
            xs.len() as u32,
            ucr.as_mut_ptr(),
            bg.as_mut_ptr(),
            desc.as_mut_ptr(),
            dcap as u32,
            &mut dused,
        )
    };
    if ok == 0 {
        return None;
    }
    desc.truncate(dused as usize);
    Some(OracleUcrBg {
        ucr,
        bg,
        desc: desc.iter().map(|&b| b as char).collect(),
    })
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

/// lcms2 `cmsReadTag` of an mft1/mft2 tag -> `cmsPipeline`; reports the
/// pipeline's `(input_channels, output_channels)`, or `None` if the profile
/// cannot be opened or the tag is absent / not pipeline-backed.
pub fn lut_channels(buf: &[u8], sig: u32) -> Option<(u32, u32)> {
    let mut n_in = 0u32;
    let mut n_out = 0u32;
    // SAFETY: buf/len describe a valid readable slice C only reads; n_in/n_out
    // are valid u32 the C writes only when it returns nonzero. The profile and
    // its pipeline are opened and closed entirely inside the call.
    let ok = unsafe {
        rcms_oracle_lut_channels(buf.as_ptr(), buf.len() as u32, sig, &mut n_in, &mut n_out)
    };
    if ok != 0 {
        Some((n_in, n_out))
    } else {
        None
    }
}

/// Evaluate `n_samples` input vectors (`inputs` is `n_samples * n_in` u16
/// row-major) through lcms2's pipeline for the mft1/mft2 `sig` via
/// `cmsPipelineEval16`. Returns the `n_samples * n_out` u16 outputs row-major,
/// or `None` on failure. `n_in`/`n_out` must match [`lut_channels`].
pub fn lut_eval16(
    buf: &[u8],
    sig: u32,
    inputs: &[u16],
    n_samples: usize,
    n_out: usize,
) -> Option<Vec<u16>> {
    let mut out = vec![0u16; n_samples * n_out];
    // SAFETY: buf/len describe a valid readable slice C only reads; `inputs` is
    // `n_samples * n_in` readable u16; `out` has `n_samples * n_out` u16 of room
    // (the pipeline output width). C reads inputs and writes exactly that many
    // outputs; the pipeline is opened and freed inside the call.
    let ok = unsafe {
        rcms_oracle_lut_eval16(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            inputs.as_ptr(),
            n_samples as u32,
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Float counterpart of [`lut_eval16`] via `cmsPipelineEvalFloat`.
pub fn lut_eval_float(
    buf: &[u8],
    sig: u32,
    inputs: &[f32],
    n_samples: usize,
    n_out: usize,
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_samples * n_out];
    // SAFETY: as `lut_eval16` but `inputs`/`out` are f32.
    let ok = unsafe {
        rcms_oracle_lut_eval_float(
            buf.as_ptr(),
            buf.len() as u32,
            sig,
            inputs.as_ptr(),
            n_samples as u32,
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// Which LUT-extraction entry point to drive in [`read_lut_channels`] /
/// [`read_lut_eval_float`]: 0 = `_cmsReadInputLUT`, 1 = `_cmsReadOutputLUT`,
/// 2 = `_cmsReadDevicelinkLUT` (all `cmsio1.c`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadLut {
    Input = 0,
    Output = 1,
    Devicelink = 2,
}

/// lcms2 `_cmsReadInputLUT` / `_cmsReadOutputLUT` / `_cmsReadDevicelinkLUT`
/// (`cmsio1.c`) for `intent`: report whether lcms2 builds a LUT and its
/// `(input_channels, output_channels)`, or `None` if the profile cannot be
/// opened or lcms2 returns NULL for the requested LUT.
pub fn read_lut_channels(buf: &[u8], which: ReadLut, intent: u32) -> Option<(u32, u32)> {
    let mut n_in = 0u32;
    let mut n_out = 0u32;
    // SAFETY: buf/len describe a valid readable slice C only reads; n_in/n_out are
    // valid u32 the C writes only when it returns nonzero. The profile and the
    // built pipeline are opened and freed entirely inside the call.
    let ok = unsafe {
        rcms_oracle_read_lut_channels(
            buf.as_ptr(),
            buf.len() as u32,
            which as u32,
            intent,
            &mut n_in,
            &mut n_out,
        )
    };
    if ok != 0 {
        Some((n_in, n_out))
    } else {
        None
    }
}

/// Build the LUT lcms2's `_cmsRead{Input,Output,Devicelink}LUT` produces for
/// `intent` and evaluate `n_samples` input vectors (`inputs` is
/// `n_samples * n_in` f32 row-major) through it via `cmsPipelineEvalFloat`.
/// Returns the `n_samples * n_out` f32 outputs row-major, or `None` if no LUT.
/// `n_in`/`n_out` must match [`read_lut_channels`].
pub fn read_lut_eval_float(
    buf: &[u8],
    which: ReadLut,
    intent: u32,
    inputs: &[f32],
    n_samples: usize,
    n_out: usize,
) -> Option<Vec<f32>> {
    let mut out = vec![0f32; n_samples * n_out];
    // SAFETY: buf/len describe a valid readable slice C only reads; `inputs` is
    // `n_samples * n_in` readable f32; `out` has `n_samples * n_out` f32 of room
    // (the pipeline output width). C reads inputs and writes exactly that many
    // outputs; the profile and pipeline are opened and freed inside the call.
    let ok = unsafe {
        rcms_oracle_read_lut_eval_float(
            buf.as_ptr(),
            buf.len() as u32,
            which as u32,
            intent,
            inputs.as_ptr(),
            n_samples as u32,
            out.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(out)
    } else {
        None
    }
}

/// lcms2 `cmsBuildTabulatedToneCurve16(table)` + `cmsReverseToneCurve` +
/// `cmsEvalToneCurveFloat`: reverse a 16-bit tabulated curve and evaluate the
/// reversed curve at each `x` in `xs`. Returns the `xs.len()` outputs, or `None`
/// on allocation failure.
pub fn reverse_tabulated16_eval_float(table: &[u16], xs: &[f32]) -> Option<Vec<f32>> {
    let mut ys = vec![0f32; xs.len()];
    // SAFETY: `table` is `n` readable u16, `xs` is `nx` readable f32, `ys` has `nx`
    // f32 of room. C builds/reverses/evaluates a curve and frees it inside the call.
    let ok = unsafe {
        rcms_oracle_reverse_tabulated16_eval_float(
            table.as_ptr(),
            table.len() as u32,
            xs.as_ptr(),
            xs.len() as u32,
            ys.as_mut_ptr(),
        )
    };
    if ok != 0 {
        Some(ys)
    } else {
        None
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

/// TEMPORARY transcendental parity probe (slice 3 de-risk). Sweeps millions of
/// inputs comparing Rust std math (`f64::powf`/`ln`/`log10`) against the C libm
/// the lcms2 oracle links. Run with:
///   cargo test -p rcms-oracle --release transcendental_parity_probe -- --nocapture --ignored
/// Marked `#[ignore]` so it does not run in the normal suite (it is a one-shot
/// architectural probe, not a regression test). Safe to delete once slice 3 has
/// committed to a math strategy.
#[cfg(test)]
mod transcendental_probe {
    use super::{libm_log, libm_log10, libm_pow, Rng};

    /// ULP distance between two finite f64 by their bit representations, using
    /// the IEEE-754 total-order mapping so the integer subtraction is a genuine
    /// ULP count even across the sign boundary.
    fn ulp_delta(a: f64, b: f64) -> u64 {
        monotone(a).wrapping_sub(monotone(b)).unsigned_abs()
    }

    /// Map an f64 to a monotonically increasing i64 (IEEE-754 total order), so
    /// integer difference == ULP count. NaNs are not expected on our inputs.
    fn monotone(x: f64) -> i64 {
        let b = x.to_bits() as i64;
        // For negatives, IEEE bit order runs backwards; flip to a continuous line.
        if b < 0 {
            i64::MIN.wrapping_sub(b)
        } else {
            b
        }
    }

    struct Stats {
        total: u64,
        mismatches: u64,
        max_ulp: u64,
        examples: Vec<(String, u64, u64)>, // (input desc, rust_bits, c_bits)
    }
    impl Stats {
        fn new() -> Self {
            Stats {
                total: 0,
                mismatches: 0,
                max_ulp: 0,
                examples: Vec::new(),
            }
        }
        fn record(&mut self, desc: impl FnOnce() -> String, rust: f64, c: f64) {
            self.total += 1;
            if rust.to_bits() != c.to_bits() {
                self.mismatches += 1;
                let d = ulp_delta(rust, c);
                if d > self.max_ulp {
                    self.max_ulp = d;
                }
                if self.examples.len() < 3 {
                    self.examples.push((desc(), rust.to_bits(), c.to_bits()));
                }
            }
        }
        fn report(&self, name: &str) {
            println!(
                "=== {name}: total={} mismatches={} max_ulp={}",
                self.total, self.mismatches, self.max_ulp
            );
            for (i, (d, rb, cb)) in self.examples.iter().enumerate() {
                println!(
                    "    example[{i}] input={d} rust_bits=0x{rb:016x} c_bits=0x{cb:016x} (ulp={})",
                    ulp_delta(f64::from_bits(*rb), f64::from_bits(*cb))
                );
            }
        }
    }

    #[test]
    #[ignore = "one-shot architectural probe; run explicitly with --nocapture --ignored"]
    fn transcendental_parity_probe() {
        let n_per: u64 = 3_000_000;
        let mut rng = Rng::new(0x0511_33DE_0000_0003_u64 ^ 0x9E37_79B9_7F4A_7C15);

        // ---- pow: x in (0,4], y in [0.2,5.0] ----
        let mut pow_stats = Stats::new();
        for _ in 0..n_per {
            let x = rng.next_f64_unit() * 4.0; // (0,4]
            let x = if x == 0.0 { f64::MIN_POSITIVE } else { x };
            let y = 0.2 + rng.next_f64_unit() * (5.0 - 0.2); // [0.2,5.0]
            let r = x.powf(y);
            let c = libm_pow(x, y);
            pow_stats.record(|| format!("pow(x={x:e}, y={y:e})"), r, c);
        }
        // Edge cases.
        let pow_edges: &[(f64, f64)] = &[
            (1.0, 1.0),
            (1.0, 2.4),
            (1.0, 1.0 / 2.4),
            (0.0, 2.4),
            (f64::MIN_POSITIVE, 2.4),
            (1e-300, 2.4),
            (0.5, 2.4),
            (0.5, 1.0 / 2.4),
            (2.0, 2.4),
            (4.0, 5.0),
            (0.04045, 2.4),
            (0.0031308, 1.0 / 2.4),
        ];
        for &(x, y) in pow_edges {
            let r = x.powf(y);
            let c = libm_pow(x, y);
            pow_stats.record(|| format!("pow_edge(x={x:e}, y={y:e})"), r, c);
        }
        pow_stats.report("pow  (x in (0,4], y in [0.2,5.0])");

        // ---- log (natural): x in (0,10] ----
        let mut log_stats = Stats::new();
        for _ in 0..n_per {
            let x = rng.next_f64_unit() * 10.0;
            let x = if x == 0.0 { f64::MIN_POSITIVE } else { x };
            let r = x.ln();
            let c = libm_log(x);
            log_stats.record(|| format!("log(x={x:e})"), r, c);
        }
        for &x in &[1.0_f64, 2.0, std::f64::consts::E, 0.5, 1e-300, 10.0] {
            let r = x.ln();
            let c = libm_log(x);
            log_stats.record(|| format!("log_edge(x={x:e})"), r, c);
        }
        log_stats.report("log  (x in (0,10])");

        // ---- log10: x in (0,10] ----
        let mut log10_stats = Stats::new();
        for _ in 0..n_per {
            let x = rng.next_f64_unit() * 10.0;
            let x = if x == 0.0 { f64::MIN_POSITIVE } else { x };
            let r = x.log10();
            let c = libm_log10(x);
            log10_stats.record(|| format!("log10(x={x:e})"), r, c);
        }
        for &x in &[1.0_f64, 10.0, 100.0, 0.1, 0.5, 1e-300] {
            let r = x.log10();
            let c = libm_log10(x);
            log10_stats.record(|| format!("log10_edge(x={x:e})"), r, c);
        }
        log10_stats.report("log10 (x in (0,10])");

        println!(
            "\n=== VERDICT: pow_mismatch={}/{} log_mismatch={}/{} log10_mismatch={}/{} ===",
            pow_stats.mismatches,
            pow_stats.total,
            log_stats.mismatches,
            log_stats.total,
            log10_stats.mismatches,
            log10_stats.total
        );
    }
}
