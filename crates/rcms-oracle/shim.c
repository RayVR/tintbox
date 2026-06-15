#include "lcms2_internal.h"
#include <stdint.h>
#include <stdlib.h>
#include <math.h>

/* ---- TEMPORARY transcendental parity probe (slice 3 de-risk) -------------- */
/* These expose the exact C libm functions lcms2's parametric curve evaluator
   uses (pow/log/log10) so a Rust test can compare bit patterns against
   f64::powf/ln/log10. KEPT after the probe: slice 3's parametric tone-curve
   differential tests will want a pow/log oracle regardless of the outcome. */
double rcms_oracle_pow(double x, double y) { return pow(x, y); }
double rcms_oracle_log(double x)           { return log(x); }
double rcms_oracle_log10(double x)         { return log10(x); }

/* ---- Parametric tone-curve evaluator (cmsgamma.c DefaultEvalParametricFn) ----
   Builds a one-segment parametric curve of the given type and evaluates it via
   cmsEvalToneCurveFloat at x. For a curve built by cmsBuildParametricToneCurve
   the single function segment spans (MINUS_INF, PLUS_INF], so any finite x is
   in-domain and EvalSegmentedFn dispatches straight to DefaultEvalParametricFn
   (no table interpolation; nSegments==1 so the 16-bit-table branch of
   cmsEvalToneCurveFloat is skipped). EvalSegmentedFn additionally clamps an
   infinite result to +/-1E22F before the cmsFloat32Number cast. Returns the
   evaluator output as a float, or NAN if lcms2 rejects the type/params (so the
   Rust side can skip those param sets). */
float rcms_oracle_eval_parametric(int type, const double* params, int nparams, float x) {
    (void) nparams; /* lcms2 reads exactly ParameterCount[type] params itself. */
    cmsToneCurve* c = cmsBuildParametricToneCurve(NULL, type, params);
    if (!c) return NAN;
    float y = cmsEvalToneCurveFloat(c, x);
    cmsFreeToneCurve(c);
    return y;
}

/* ---- Tone-curve construction + evaluation (cmsgamma.c / cmsintrp.c) ---------
   These build a curve from a caller-supplied table/params and evaluate it, so the
   Rust side can diff cmsEvalToneCurve16 / cmsEvalToneCurveFloat and the
   materialised Table16 bit-for-bit. */

/* cmsBuildTabulatedToneCurve16 + cmsEvalToneCurve16 at v. */
uint16_t rcms_oracle_tabulated16_eval16(const uint16_t* table, uint32_t n, uint16_t v) {
    cmsToneCurve* c = cmsBuildTabulatedToneCurve16(NULL, n, table);
    if (!c) return 0;
    uint16_t out = cmsEvalToneCurve16(c, v);
    cmsFreeToneCurve(c);
    return out;
}

/* cmsBuildTabulatedToneCurve16 + cmsEvalToneCurveFloat at x. */
float rcms_oracle_tabulated16_eval_float(const uint16_t* table, uint32_t n, float x) {
    cmsToneCurve* c = cmsBuildTabulatedToneCurve16(NULL, n, table);
    if (!c) return NAN;
    float out = cmsEvalToneCurveFloat(c, x);
    cmsFreeToneCurve(c);
    return out;
}

/* cmsBuildTabulatedToneCurveFloat + cmsEvalToneCurveFloat at x. Returns NAN if
   lcms2 rejects the table (n == 0). */
float rcms_oracle_tabulated_float_eval_float(const float* table, uint32_t n, float x) {
    cmsToneCurve* c = cmsBuildTabulatedToneCurveFloat(NULL, n, table);
    if (!c) return NAN;
    float out = cmsEvalToneCurveFloat(c, x);
    cmsFreeToneCurve(c);
    return out;
}

/* cmsBuildParametricToneCurve + cmsEvalToneCurveFloat at x. Returns NAN if lcms2
   rejects the type/params. */
float rcms_oracle_parametric_eval_float(int type, const double* params, float x) {
    cmsToneCurve* c = cmsBuildParametricToneCurve(NULL, type, params);
    if (!c) return NAN;
    float out = cmsEvalToneCurveFloat(c, x);
    cmsFreeToneCurve(c);
    return out;
}

/* Materialise the 16-bit approximation table of a cmsBuildParametricToneCurve
   curve. Writes cmsGetToneCurveEstimatedTable into out (cap entries of room) and
   returns the entry count, or -1 if lcms2 rejects the type/params. */
int32_t rcms_oracle_parametric_table16(int type, const double* params, uint16_t* out, uint32_t cap) {
    cmsToneCurve* c = cmsBuildParametricToneCurve(NULL, type, params);
    if (!c) return -1;
    uint32_t n = cmsGetToneCurveEstimatedTableEntries(c);
    const uint16_t* t = cmsGetToneCurveEstimatedTable(c);
    if (n > cap) { cmsFreeToneCurve(c); return -1; }
    for (uint32_t i = 0; i < n; i++) out[i] = t[i];
    cmsFreeToneCurve(c);
    return (int32_t) n;
}

/* Materialise the 16-bit table of a cmsBuildTabulatedToneCurveFloat curve. */
int32_t rcms_oracle_tabulated_float_table16(const float* table, uint32_t n_in, uint16_t* out, uint32_t cap) {
    cmsToneCurve* c = cmsBuildTabulatedToneCurveFloat(NULL, n_in, table);
    if (!c) return -1;
    uint32_t n = cmsGetToneCurveEstimatedTableEntries(c);
    const uint16_t* t = cmsGetToneCurveEstimatedTable(c);
    if (n > cap) { cmsFreeToneCurve(c); return -1; }
    for (uint32_t i = 0; i < n; i++) out[i] = t[i];
    cmsFreeToneCurve(c);
    return (int32_t) n;
}

/* Fixed-point (cmsplugin.c:383). */
int32_t rcms_oracle_double_to_s15f16(double v) { return (int32_t) _cmsDoubleTo15Fixed16(v); }

double   rcms_oracle_s15f16_to_double(int32_t a) { return _cms15Fixed16toDouble((cmsS15Fixed16Number)a); }
uint16_t rcms_oracle_double_to_8fixed8(double v) { return _cmsDoubleTo8Fixed8(v); }
int32_t  rcms_oracle_to_fixed_domain(int a)       { return _cmsToFixedDomain(a); }
int32_t  rcms_oracle_from_fixed_domain(int32_t a) { return _cmsFromFixedDomain((cmsS15Fixed16Number)a); }

/* Fast-floor hacks (lcms2_internal.h:160-195). */
int      rcms_oracle_quick_floor(double v)         { return _cmsQuickFloor(v); }
uint16_t rcms_oracle_quick_floor_word(double d)    { return _cmsQuickFloorWord(d); }
uint16_t rcms_oracle_quick_saturate_word(double d) { return _cmsQuickSaturateWord(d); }

/* PCS conversions (cmspcs.c). White point + value passed as flat doubles. A NULL
   white point is signalled by passing wp == NULL; lcms2 then defaults to D50. */
void rcms_oracle_xyz2lab(const double* wp, const double xyz[3], double lab[3]) {
    cmsCIEXYZ WP; cmsCIEXYZ XYZ; cmsCIELab Lab;
    XYZ.X = xyz[0]; XYZ.Y = xyz[1]; XYZ.Z = xyz[2];
    if (wp) { WP.X = wp[0]; WP.Y = wp[1]; WP.Z = wp[2]; cmsXYZ2Lab(&WP, &Lab, &XYZ); }
    else    { cmsXYZ2Lab(NULL, &Lab, &XYZ); }
    lab[0] = Lab.L; lab[1] = Lab.a; lab[2] = Lab.b;
}
void rcms_oracle_lab2xyz(const double* wp, const double lab[3], double xyz[3]) {
    cmsCIEXYZ WP; cmsCIEXYZ XYZ; cmsCIELab Lab;
    Lab.L = lab[0]; Lab.a = lab[1]; Lab.b = lab[2];
    if (wp) { WP.X = wp[0]; WP.Y = wp[1]; WP.Z = wp[2]; cmsLab2XYZ(&WP, &XYZ, &Lab); }
    else    { cmsLab2XYZ(NULL, &XYZ, &Lab); }
    xyz[0] = XYZ.X; xyz[1] = XYZ.Y; xyz[2] = XYZ.Z;
}
void rcms_oracle_xyz2xyy(const double xyz[3], double xyy[3]) {
    cmsCIEXYZ XYZ; cmsCIExyY xyY;
    XYZ.X = xyz[0]; XYZ.Y = xyz[1]; XYZ.Z = xyz[2];
    cmsXYZ2xyY(&xyY, &XYZ);
    xyy[0] = xyY.x; xyy[1] = xyY.y; xyy[2] = xyY.Y;
}
void rcms_oracle_xyy2xyz(const double xyy[3], double xyz[3]) {
    cmsCIEXYZ XYZ; cmsCIExyY xyY;
    xyY.x = xyy[0]; xyY.y = xyy[1]; xyY.Y = xyy[2];
    cmsxyY2XYZ(&XYZ, &xyY);
    xyz[0] = XYZ.X; xyz[1] = XYZ.Y; xyz[2] = XYZ.Z;
}
void rcms_oracle_lab2lch(const double lab[3], double lch[3]) {
    cmsCIELab Lab; cmsCIELCh LCh;
    Lab.L = lab[0]; Lab.a = lab[1]; Lab.b = lab[2];
    cmsLab2LCh(&LCh, &Lab);
    lch[0] = LCh.L; lch[1] = LCh.C; lch[2] = LCh.h;
}
void rcms_oracle_lch2lab(const double lch[3], double lab[3]) {
    cmsCIELab Lab; cmsCIELCh LCh;
    LCh.L = lch[0]; LCh.C = lch[1]; LCh.h = lch[2];
    cmsLCh2Lab(&Lab, &LCh);
    lab[0] = Lab.L; lab[1] = Lab.a; lab[2] = Lab.b;
}
/* Lab v4 / v2 encodings (16-bit). */
void rcms_oracle_lab_enc2float_v4(const uint16_t wlab[3], double lab[3]) {
    cmsCIELab Lab; cmsUInt16Number w[3] = { wlab[0], wlab[1], wlab[2] };
    cmsLabEncoded2Float(&Lab, w);
    lab[0] = Lab.L; lab[1] = Lab.a; lab[2] = Lab.b;
}
void rcms_oracle_float2lab_enc_v4(const double lab[3], uint16_t wlab[3]) {
    cmsCIELab Lab; cmsUInt16Number w[3];
    Lab.L = lab[0]; Lab.a = lab[1]; Lab.b = lab[2];
    cmsFloat2LabEncoded(w, &Lab);
    wlab[0] = w[0]; wlab[1] = w[1]; wlab[2] = w[2];
}
void rcms_oracle_lab_enc2float_v2(const uint16_t wlab[3], double lab[3]) {
    cmsCIELab Lab; cmsUInt16Number w[3] = { wlab[0], wlab[1], wlab[2] };
    cmsLabEncoded2FloatV2(&Lab, w);
    lab[0] = Lab.L; lab[1] = Lab.a; lab[2] = Lab.b;
}
void rcms_oracle_float2lab_enc_v2(const double lab[3], uint16_t wlab[3]) {
    cmsCIELab Lab; cmsUInt16Number w[3];
    Lab.L = lab[0]; Lab.a = lab[1]; Lab.b = lab[2];
    cmsFloat2LabEncodedV2(w, &Lab);
    wlab[0] = w[0]; wlab[1] = w[1]; wlab[2] = w[2];
}
/* XYZ 1.15 fixed-point encoding. */
void rcms_oracle_xyz_enc2float(const uint16_t wxyz[3], double xyz[3]) {
    cmsCIEXYZ XYZ; cmsUInt16Number w[3] = { wxyz[0], wxyz[1], wxyz[2] };
    cmsXYZEncoded2Float(&XYZ, w);
    xyz[0] = XYZ.X; xyz[1] = XYZ.Y; xyz[2] = XYZ.Z;
}
void rcms_oracle_float2xyz_enc(const double xyz[3], uint16_t wxyz[3]) {
    cmsCIEXYZ XYZ; cmsUInt16Number w[3];
    XYZ.X = xyz[0]; XYZ.Y = xyz[1]; XYZ.Z = xyz[2];
    cmsFloat2XYZEncoded(w, &XYZ);
    wxyz[0] = w[0]; wxyz[1] = w[1]; wxyz[2] = w[2];
}

/* 3x3 matrix / 3-vector ops (cmsmtrx.c). */
/* Mat3 row-major as 9 doubles; Vec3 as 3 doubles. */
static void load_mat(cmsMAT3* M, const double m[9]) {
    for (int i=0;i<3;i++){ M->v[i].n[0]=m[i*3]; M->v[i].n[1]=m[i*3+1]; M->v[i].n[2]=m[i*3+2]; }
}
static void store_mat(double m[9], const cmsMAT3* M) {
    for (int i=0;i<3;i++){ m[i*3]=M->v[i].n[0]; m[i*3+1]=M->v[i].n[1]; m[i*3+2]=M->v[i].n[2]; }
}
void rcms_oracle_mat3_eval(double out[3], const double m[9], const double v[3]) {
    cmsMAT3 M; cmsVEC3 V, R; load_mat(&M, m);
    V.n[0]=v[0]; V.n[1]=v[1]; V.n[2]=v[2];
    _cmsMAT3eval(&R,&M,&V); out[0]=R.n[0]; out[1]=R.n[1]; out[2]=R.n[2];
}
void rcms_oracle_mat3_per(double out[9], const double a[9], const double b[9]) {
    cmsMAT3 A,B,R; load_mat(&A,a); load_mat(&B,b); _cmsMAT3per(&R,&A,&B); store_mat(out,&R);
}
int rcms_oracle_mat3_inverse(double out[9], const double a[9]) {
    cmsMAT3 A,R; load_mat(&A,a);
    if (!_cmsMAT3inverse(&A,&R)) return 0; store_mat(out,&R); return 1;
}
int rcms_oracle_mat3_solve(double out[3], const double a[9], const double b[3]) {
    cmsMAT3 A; cmsVEC3 B,X; load_mat(&A,a); B.n[0]=b[0]; B.n[1]=b[1]; B.n[2]=b[2];
    if (!_cmsMAT3solve(&X,&A,&B)) return 0; out[0]=X.n[0]; out[1]=X.n[1]; out[2]=X.n[2]; return 1;
}

/* White point from temperature (cmswtpnt.c). Writes [x,y,Y] into out; returns
   1 on success (temp in [4000,25000]) or 0. */
int rcms_oracle_white_point_from_temp(double out[3], double temp_k) {
    cmsCIExyY wp;
    if (!cmsWhitePointFromTemp(&wp, temp_k)) return 0;
    out[0] = wp.x; out[1] = wp.y; out[2] = wp.Y;
    return 1;
}

/* Bradford chromatic adaptation: adapt a color from SourceWhitePt to Illuminant
   (cmsAdaptToIlluminant). All white points / value as [X,Y,Z]. Writes the
   adapted [X,Y,Z] into out; returns 1 on success or 0 (singular adaptation). */
int rcms_oracle_adapt_to_illuminant(double out[3], const double src_wp[3],
                                    const double illuminant[3], const double value[3]) {
    cmsCIEXYZ SrcWP, Ill, Val, Res;
    SrcWP.X = src_wp[0]; SrcWP.Y = src_wp[1]; SrcWP.Z = src_wp[2];
    Ill.X   = illuminant[0]; Ill.Y = illuminant[1]; Ill.Z = illuminant[2];
    Val.X   = value[0]; Val.Y = value[1]; Val.Z = value[2];
    if (!cmsAdaptToIlluminant(&Res, &SrcWP, &Ill, &Val)) return 0;
    out[0] = Res.X; out[1] = Res.Y; out[2] = Res.Z;
    return 1;
}

/* Bradford adaptation matrix (_cmsAdaptationMatrix, NULL cone -> Bradford).
   from/to as [X,Y,Z]; writes the 9 matrix entries (row-major) into out.
   Returns 1 on success or 0 (singular). _cmsAdaptationMatrix is internal but
   exported (CMSCHECKPOINT) so it links here. */
int rcms_oracle_adaptation_matrix(double out[9], const double from[3], const double to[3]) {
    cmsMAT3 M; cmsCIEXYZ From, To;
    From.X = from[0]; From.Y = from[1]; From.Z = from[2];
    To.X   = to[0];   To.Y   = to[1];   To.Z   = to[2];
    if (!_cmsAdaptationMatrix(&M, NULL, &From, &To)) return 0;
    store_mat(out, &M);
    return 1;
}

/* IEEE half<->float (cmshalf.c, table-based van der Zijp method). */
float    rcms_oracle_half_to_float(uint16_t h) { return _cmsHalf2Float(h); }
uint16_t rcms_oracle_float_to_half(float f)    { return _cmsFloat2Half(f); }

/* RFC 1321 MD5 (cmsmd5.c, public API via lcms2_plugin.h, already included by
   lcms2_internal.h above — adding #include "lcms2.h" would redefine symbols). */
void rcms_oracle_md5(uint8_t out[16], const uint8_t* buf, uint32_t len) {
    cmsHANDLE h = cmsMD5alloc(NULL);
    if (!h) { for (int i=0;i<16;i++) out[i] = 0; return; }
    cmsMD5add(h, buf, len);
    cmsProfileID id; cmsMD5finish(&id, h);
    for (int i=0;i<16;i++) out[i] = id.ID8[i];
}

/* I/O big-endian read primitives (cmsplugin.c via in-memory IOHANDLER).
   cmsOpenIOhandlerFromMem/cmsCloseIOhandler are public in lcms2.h;
   _cmsReadUInt16Number/_cmsReadUInt32Number in lcms2_plugin.h — both
   transitively included via lcms2_internal.h above. */
int rcms_oracle_read_u16(const uint8_t* buf, uint32_t len, uint16_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsUInt16Number v; int ok = _cmsReadUInt16Number(io, &v);
    cmsCloseIOhandler(io); *out = v; return ok;
}
int rcms_oracle_read_u32(const uint8_t* buf, uint32_t len, uint32_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsUInt32Number v; int ok = _cmsReadUInt32Number(io, &v);
    cmsCloseIOhandler(io); *out = v; return ok;
}
int rcms_oracle_read_u8(const uint8_t* buf, uint32_t len, uint8_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsUInt8Number v; int ok = _cmsReadUInt8Number(io, &v);
    cmsCloseIOhandler(io); *out = v; return ok;
}
int rcms_oracle_read_u64(const uint8_t* buf, uint32_t len, uint64_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsUInt64Number v; int ok = _cmsReadUInt64Number(io, &v);
    cmsCloseIOhandler(io); *out = v; return ok;
}
/* Returns the raw i32 (s15Fixed16) by re-encoding the double through
   _cmsDoubleTo15Fixed16 is lossy; instead we want the wire i32, so reconstruct
   it: _cmsRead15Fixed16Number yields a double = raw/65536.0, multiply back. */
int rcms_oracle_read_s15f16(const uint8_t* buf, uint32_t len, int32_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsFloat64Number v; int ok = _cmsRead15Fixed16Number(io, &v);
    cmsCloseIOhandler(io);
    /* v == raw / 65536.0 exactly (integer/65536); recover raw via rounding. */
    *out = (int32_t) floor(v * 65536.0 + 0.5);
    return ok;
}
int rcms_oracle_read_xyz(const uint8_t* buf, uint32_t len, double out[3]) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsCIEXYZ xyz; int ok = _cmsReadXYZNumber(io, &xyz);
    cmsCloseIOhandler(io);
    out[0] = xyz.X; out[1] = xyz.Y; out[2] = xyz.Z;
    return ok;
}
int rcms_oracle_read_u16_array(const uint8_t* buf, uint32_t len, uint32_t n, uint16_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    int ok = _cmsReadUInt16Array(io, n, out);
    cmsCloseIOhandler(io); return ok;
}
int rcms_oracle_read_type_base(const uint8_t* buf, uint32_t len, uint32_t* out) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    cmsTagTypeSignature sig = _cmsReadTypeBase(io);
    cmsCloseIOhandler(io); *out = (uint32_t) sig; return 1;
}
/* Seed the handler at `offset` (via Seek) then call _cmsReadAlignment.
   Returns ok flag; writes the resulting Tell into *out_tell. */
int rcms_oracle_read_alignment(const uint8_t* buf, uint32_t len, uint32_t offset, uint32_t* out_tell) {
    cmsIOHANDLER* io = cmsOpenIOhandlerFromMem(NULL, (void*)buf, len, "r");
    if (!io) return 0;
    if (!io->Seek(io, offset)) { cmsCloseIOhandler(io); return 0; }
    int ok = _cmsReadAlignment(io);
    *out_tell = io->Tell(io);
    cmsCloseIOhandler(io);
    return ok;
}

/* ICC header getters (cmsio0.c). Open the profile from memory, read its header
   via the public cmsGetHeader* accessors, fill a flat struct, then close it.
   Returns 1 if cmsOpenProfileFromMem succeeded (and *opened set to 1), else 0.
   When open fails, only `opened` is meaningful (0); the other fields are
   left as whatever the caller zeroed them to. This lets the differential test
   compare the "does lcms2 accept this profile?" decision as well as the field
   values for accepted profiles. */
typedef struct {
    uint32_t device_class;
    uint32_t color_space;
    uint32_t pcs;
    uint32_t version;          /* cmsGetEncodedICCversion */
    uint32_t rendering_intent;
    uint32_t flags;
    uint32_t manufacturer;
    uint32_t model;
    uint32_t creator;
    uint64_t attributes;
    uint8_t  profile_id[16];
} rcms_oracle_header;

/* Drive lcms2's header acceptance in isolation from the tag directory. We feed
   the profile's first 128 header bytes followed by a zero tag count, so
   _cmsReadHeader (which is invoked by cmsOpenProfileFromMem) runs its header
   validation (magic, _validatedVersion, version > 0x5000000, validDeviceClass)
   and then its tag-directory loop with TagCount == 0, which trivially succeeds.
   This isolates the *header-level* accept/reject decision: a profile whose
   header is well-formed but whose real tag directory is malformed (e.g. a
   truncated file) is still "header-accepted" here, matching what a header-only
   parser produces. Returns 1 (header accepted, fields written) or 0 (rejected).
   `len` must be >= 128. */
int rcms_oracle_read_header(const uint8_t* buf, uint32_t len, rcms_oracle_header* out) {
    if (len < 128) return 0;
    uint8_t hdr[132];
    for (int i = 0; i < 128; i++) hdr[i] = buf[i];
    hdr[128] = hdr[129] = hdr[130] = hdr[131] = 0; /* TagCount = 0 */
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)hdr, 132);
    if (!p) return 0;
    out->device_class     = (uint32_t) cmsGetDeviceClass(p);
    out->color_space      = (uint32_t) cmsGetColorSpace(p);
    out->pcs              = (uint32_t) cmsGetPCS(p);
    out->version          = cmsGetEncodedICCversion(p);
    out->rendering_intent = cmsGetHeaderRenderingIntent(p);
    out->flags            = cmsGetHeaderFlags(p);
    out->manufacturer     = cmsGetHeaderManufacturer(p);
    out->model            = cmsGetHeaderModel(p);
    out->creator          = cmsGetHeaderCreator(p);
    cmsUInt64Number attr; cmsGetHeaderAttributes(p, &attr);
    out->attributes       = (uint64_t) attr;
    cmsGetHeaderProfileID(p, out->profile_id);
    cmsCloseProfile(p);
    return 1;
}

/* Full open (header + tag directory + duplicate check) over the WHOLE profile
   bytes. cmsOpenProfileFromMem runs _cmsReadHeader, which validates the header
   AND parses the tag directory (sanity skips, link detection, dup rejection).
   Returns 1 if lcms2 accepts the profile, else 0. This is the accept/reject
   decision the rcms Profile::open must agree with. */
int rcms_oracle_open_succeeds(const uint8_t* buf, uint32_t len) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsCloseProfile(p);
    return 1;
}

/* Number of accepted tags in the directory (cmsGetTagCount). Returns -1 if the
   profile cannot be opened. */
int rcms_oracle_tag_count(const uint8_t* buf, uint32_t len) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsInt32Number n = cmsGetTagCount(p);
    cmsCloseProfile(p);
    return (int) n;
}

/* The n-th accepted tag signature (cmsGetTagSignature). Returns 0 if the profile
   cannot be opened or n is out of range. The profile is opened/closed per call;
   callers loop n in [0, tag_count). */
uint32_t rcms_oracle_tag_signature(const uint8_t* buf, uint32_t len, uint32_t n) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    uint32_t sig = (uint32_t) cmsGetTagSignature(p, n);
    cmsCloseProfile(p);
    return sig;
}

/* The on-disk tag TYPE signature for a given tag (cmsGetTagTrueType). Returns 0
   if the profile cannot be opened, the tag is absent, or the type is unknown.
   This lets the differential test pick which tags carry one of this task's
   trivial on-disk types before it asserts the cooked value. */
uint32_t rcms_oracle_tag_true_type(const uint8_t* buf, uint32_t len, uint32_t sig) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    /* _cmsGetTagTrueType only knows the type after the tag has actually been
       read (it inspects TagTypeHandlers[n], which is NULL until cmsReadTag
       populates it). So read the tag first; ignore the cooked value. */
    (void) cmsReadTag(p, (cmsTagSignature) sig);
    uint32_t t = (uint32_t) _cmsGetTagTrueType(p, (cmsTagSignature) sig);
    cmsCloseProfile(p);
    return t;
}

/* ---- Per-type cooked-value extractors (cmsReadTag) ------------------------ */
/* Each opens the profile, reads the named tag, copies the value into a flat
   caller-provided buffer, and returns 1 on success (tag present, read OK) or 0
   otherwise. The differential test calls these only after confirming via
   rcms_oracle_tag_true_type that the on-disk type is the expected one. */

/* XYZType -> 3 doubles [X,Y,Z]. */
int rcms_oracle_read_tag_xyz(const uint8_t* buf, uint32_t len, uint32_t sig, double out[3]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsCIEXYZ* v = (cmsCIEXYZ*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) { out[0]=v->X; out[1]=v->Y; out[2]=v->Z; ok = 1; }
    cmsCloseProfile(p);
    return ok;
}

/* S15Fixed16ArrayType -> doubles. Writes up to `cap` doubles, returns the count
   (number of elements) or -1 on failure. */
int rcms_oracle_read_tag_s15f16(const uint8_t* buf, uint32_t len, uint32_t sig,
                                double* out, uint32_t cap) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsFloat64Number* v = (cmsFloat64Number*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (v) {
        /* lcms2 stores the count nowhere reachable here; the chad/arts tags have
           a fixed ElemCount of 9 and are the only s15f16 tags. The differential
           test passes the on-disk byte size / 4 as the expected count and asks
           for exactly that many, so we copy `cap` elements. */
        for (uint32_t i = 0; i < cap; i++) out[i] = v[i];
        n = (int) cap;
    }
    cmsCloseProfile(p);
    return n;
}

/* SignatureType -> u32. Returns 1/0. */
int rcms_oracle_read_tag_signature(const uint8_t* buf, uint32_t len, uint32_t sig, uint32_t* out) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsSignature* v = (cmsSignature*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) { *out = (uint32_t) *v; ok = 1; }
    cmsCloseProfile(p);
    return ok;
}

/* TextType -> ASCII bytes (no terminator). Writes up to `cap` bytes into out,
   returns the length (excluding the implicit NUL) or -1 on failure. */
int rcms_oracle_read_tag_text(const uint8_t* buf, uint32_t len, uint32_t sig,
                              uint8_t* out, uint32_t cap) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsMLU* mlu = (cmsMLU*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (mlu) {
        cmsUInt32Number sz = cmsMLUgetASCII(mlu, cmsNoLanguage, cmsNoCountry, NULL, 0);
        if (sz > 0 && sz <= cap + 1) {
            cmsMLUgetASCII(mlu, cmsNoLanguage, cmsNoCountry, (char*) out, sz);
            n = (int) (sz - 1); /* drop the trailing NUL */
        }
    }
    cmsCloseProfile(p);
    return n;
}

/* DataType -> flag (u32) + raw bytes. Writes the flag into *flag and up to `cap`
   data bytes into out; returns the data length or -1 on failure. */
int rcms_oracle_read_tag_data(const uint8_t* buf, uint32_t len, uint32_t sig,
                              uint32_t* flag, uint8_t* out, uint32_t cap) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsICCData* v = (cmsICCData*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (v && v->len <= cap) {
        *flag = v->flag;
        for (cmsUInt32Number i = 0; i < v->len; i++) out[i] = v->data[i];
        n = (int) v->len;
    }
    cmsCloseProfile(p);
    return n;
}

/* DateTimeType -> 6 u16 [year,month,day,hours,minutes,seconds] (wire order, the
   ICC dateTimeNumber fields recovered from the decoded struct tm). */
int rcms_oracle_read_tag_datetime(const uint8_t* buf, uint32_t len, uint32_t sig, uint16_t out[6]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    struct tm* t = (struct tm*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (t) {
        out[0] = (uint16_t) (t->tm_year + 1900);
        out[1] = (uint16_t) (t->tm_mon + 1);
        out[2] = (uint16_t) t->tm_mday;
        out[3] = (uint16_t) t->tm_hour;
        out[4] = (uint16_t) t->tm_min;
        out[5] = (uint16_t) t->tm_sec;
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* ChromaticityType -> 6 doubles [Rx,Ry,Gx,Gy,Bx,By]. */
int rcms_oracle_read_tag_chromaticity(const uint8_t* buf, uint32_t len, uint32_t sig, double out[6]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsCIExyYTRIPLE* v = (cmsCIExyYTRIPLE*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) {
        out[0]=v->Red.x;   out[1]=v->Red.y;
        out[2]=v->Green.x; out[3]=v->Green.y;
        out[4]=v->Blue.x;  out[5]=v->Blue.y;
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* MeasurementType -> cmsICCMeasurementConditions, flattened as
   [Observer, Geometry, IlluminantType] (u32) + [Bx,By,Bz, Flare] (double).
   Returns 1/0. */
int rcms_oracle_read_tag_measurement(const uint8_t* buf, uint32_t len, uint32_t sig,
                                     uint32_t out_u32[3], double out_f64[4]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsICCMeasurementConditions* v =
        (cmsICCMeasurementConditions*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) {
        out_u32[0] = v->Observer;
        out_u32[1] = v->Geometry;
        out_u32[2] = v->IlluminantType;
        out_f64[0] = v->Backing.X;
        out_f64[1] = v->Backing.Y;
        out_f64[2] = v->Backing.Z;
        out_f64[3] = v->Flare;
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* ColorantOrderType -> bytes (the laydown order). lcms2 stores a cmsMAXCHANNELS
   array padded with 0xFF; the meaningful Count is the leading run of non-0xFF
   entries, but to compare against rcms we return exactly the count the on-disk
   tag declared. We cannot recover that count from the cooked array reliably
   (a legitimate entry could be 0xFF), so the differential test reads the raw
   Count from the tag bytes itself; this extractor returns the full padded array
   so the test can compare the first Count entries. Writes up to `cap` bytes,
   returns the number written (cmsMAXCHANNELS) or -1 on failure. */
int rcms_oracle_read_tag_colorant_order(const uint8_t* buf, uint32_t len, uint32_t sig,
                                        uint8_t* out, uint32_t cap) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsUInt8Number* v = (cmsUInt8Number*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (v && cap >= cmsMAXCHANNELS) {
        for (int i = 0; i < cmsMAXCHANNELS; i++) out[i] = v[i];
        n = cmsMAXCHANNELS;
    }
    cmsCloseProfile(p);
    return n;
}

/* ---- MLU / TextDescription (cmsMLU) extractors ---------------------------- */
/* Both cmsSigMultiLocalizedUnicodeType ('mluc') and cmsSigTextDescriptionType
   ('desc') decode to a cmsMLU. cmsMLUtranslationsCount enumerates the records;
   cmsMLUtranslationsCodes yields each record's language/country codes; passing
   those exact codes to cmsMLUgetWide returns that record's wide string. */

/* Number of translations in the tag's MLU, or -1 if the tag is absent / not an
   MLU-backed type. */
int rcms_oracle_mlu_count(const uint8_t* buf, uint32_t len, uint32_t sig) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsMLU* mlu = (cmsMLU*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (mlu) n = (int) cmsMLUtranslationsCount(mlu);
    cmsCloseProfile(p);
    return n;
}

/* Translation `idx` of the tag's MLU. Writes the two language bytes and two
   country bytes (raw, as strFrom16 splits the u16 wire code), and the wide
   string as raw UTF-16 code units (one uint16_t each, NO surrogate pairing —
   exactly the units lcms2 keeps in its wide pool) into `units` (up to `cap`).
   Returns the number of code units written (excluding the implicit NUL), or -1
   on failure. */
int rcms_oracle_mlu_entry(const uint8_t* buf, uint32_t len, uint32_t sig,
                          uint32_t idx, uint8_t lang[2], uint8_t country[2],
                          uint16_t* units, uint32_t cap) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsMLU* mlu = (cmsMLU*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (mlu) {
        char L[3] = {0,0,0};
        char C[3] = {0,0,0};
        if (cmsMLUtranslationsCodes(mlu, idx, L, C)) {
            lang[0] = (uint8_t) L[0]; lang[1] = (uint8_t) L[1];
            country[0] = (uint8_t) C[0]; country[1] = (uint8_t) C[1];

            /* Byte length of the wide string (including the NUL terminator). */
            cmsUInt32Number bytes = cmsMLUgetWide(mlu, L, C, NULL, 0);
            if (bytes >= sizeof(wchar_t)) {
                cmsUInt32Number nchars = bytes / sizeof(wchar_t) - 1; /* drop NUL */
                wchar_t* wide = (wchar_t*) malloc(bytes);
                if (wide) {
                    cmsMLUgetWide(mlu, L, C, wide, bytes);
                    if (nchars <= cap) {
                        for (cmsUInt32Number i = 0; i < nchars; i++)
                            units[i] = (uint16_t) wide[i];
                        n = (int) nchars;
                    }
                    free(wide);
                }
            } else {
                /* Empty wide string is still a valid translation. */
                n = 0;
            }
        }
    }
    cmsCloseProfile(p);
    return n;
}

/* ---- NamedColor2 / ProfileSequence{Desc,Id} / Dictionary extractors -------- */
/* These reach into lcms2's internal structs (lcms2_internal.h, included above)
   for fields the public API does not expose, and serialize nested cmsMLU*
   translations into a flat unit stream the Rust side mirrors. */

/* Serialize one cmsMLU* into out: u32 translation count, then per translation
   2 bytes language, 2 bytes country, u32 nunits, nunits u16 code units (raw,
   truncated wchar->u16 exactly as rcms_oracle_mlu_entry does). Writes into the
   byte buffer `out` (capacity `cap`); returns 0 (and sets *used) or -1 on
   overflow. A NULL mlu serializes as count 0. */
static int serialize_mlu(const cmsMLU* mlu, uint8_t* out, uint32_t cap, uint32_t* used) {
    uint32_t off = 0;
    uint32_t tcount = (mlu != NULL) ? cmsMLUtranslationsCount((cmsMLU*) mlu) : 0;
    if (off + 4 > cap) return -1;
    out[off++] = (tcount >> 24) & 0xff; out[off++] = (tcount >> 16) & 0xff;
    out[off++] = (tcount >> 8) & 0xff;  out[off++] = tcount & 0xff;
    for (uint32_t i = 0; i < tcount; i++) {
        char L[3] = {0,0,0}, C[3] = {0,0,0};
        if (!cmsMLUtranslationsCodes((cmsMLU*) mlu, i, L, C)) return -1;
        if (off + 4 > cap) return -1;
        out[off++] = (uint8_t) L[0]; out[off++] = (uint8_t) L[1];
        out[off++] = (uint8_t) C[0]; out[off++] = (uint8_t) C[1];
        cmsUInt32Number bytes = cmsMLUgetWide((cmsMLU*) mlu, L, C, NULL, 0);
        uint32_t nunits = 0;
        wchar_t* wide = NULL;
        if (bytes >= sizeof(wchar_t)) {
            nunits = bytes / sizeof(wchar_t) - 1; /* drop NUL */
            wide = (wchar_t*) malloc(bytes);
            if (!wide) return -1;
            cmsMLUgetWide((cmsMLU*) mlu, L, C, wide, bytes);
        }
        if (off + 4 > cap) { free(wide); return -1; }
        out[off++] = (nunits >> 24) & 0xff; out[off++] = (nunits >> 16) & 0xff;
        out[off++] = (nunits >> 8) & 0xff;  out[off++] = nunits & 0xff;
        for (uint32_t j = 0; j < nunits; j++) {
            uint16_t u = (uint16_t) wide[j];
            if (off + 2 > cap) { free(wide); return -1; }
            out[off++] = (u >> 8) & 0xff; out[off++] = u & 0xff;
        }
        free(wide);
    }
    *used = off;
    return 0;
}

/* NamedColor2 -> counts + prefix/suffix. Writes [nColors, ColorantCount] into
   out_counts and the 33-byte Prefix/Suffix (NUL-padded). Returns 1/0.
   (vendorFlag is discarded by lcms2 on read, so it is not exposed here.) */
int rcms_oracle_named_color2_info(const uint8_t* buf, uint32_t len, uint32_t sig,
                                  uint32_t out_counts[2], uint8_t prefix[33], uint8_t suffix[33]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsNAMEDCOLORLIST* v = (cmsNAMEDCOLORLIST*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) {
        out_counts[0] = v->nColors;
        out_counts[1] = v->ColorantCount;
        for (int i = 0; i < 33; i++) { prefix[i] = (uint8_t) v->Prefix[i]; suffix[i] = (uint8_t) v->Suffix[i]; }
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* NamedColor2 colour `idx` -> name (33 bytes, NUL-terminated), PCS (3 u16), and
   device colorants (up to cmsMAXCHANNELS u16). Returns 1/0. */
int rcms_oracle_named_color2_color(const uint8_t* buf, uint32_t len, uint32_t sig,
                                   uint32_t idx, uint8_t name[33], uint16_t pcs[3],
                                   uint16_t colorant[16]) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsNAMEDCOLORLIST* v = (cmsNAMEDCOLORLIST*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (v) {
        char Name[256]; cmsUInt16Number PCS[3]; cmsUInt16Number Colorant[16];
        memset(Name, 0, sizeof(Name)); memset(Colorant, 0, sizeof(Colorant));
        if (cmsNamedColorInfo(v, idx, Name, NULL, NULL, PCS, Colorant)) {
            Name[32] = 0;
            for (int i = 0; i < 33; i++) name[i] = (uint8_t) Name[i];
            for (int i = 0; i < 3; i++) pcs[i] = PCS[i];
            for (int i = 0; i < 16; i++) colorant[i] = Colorant[i];
            ok = 1;
        }
    }
    cmsCloseProfile(p);
    return ok;
}

/* ProfileSequenceDesc/Id -> element count (cmsSEQ->n), or -1. */
int rcms_oracle_seq_count(const uint8_t* buf, uint32_t len, uint32_t sig) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsSEQ* seq = (cmsSEQ*) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (seq) n = (int) seq->n;
    cmsCloseProfile(p);
    return n;
}

/* ProfileSequenceDesc element `idx` -> the four fixed fields plus the serialized
   Manufacturer and Model MLUs. out_u32 = [deviceMfg, deviceModel, technology];
   out_attr = attributes (u64). Returns 1/0. */
int rcms_oracle_seq_desc_elem(const uint8_t* buf, uint32_t len, uint32_t sig, uint32_t idx,
                              uint32_t out_u32[3], uint64_t* out_attr,
                              uint8_t* mblk, uint32_t mcap, uint32_t* mused,
                              uint8_t* dblk, uint32_t dcap, uint32_t* dused) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsSEQ* seq = (cmsSEQ*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (seq && idx < seq->n) {
        cmsPSEQDESC* e = &seq->seq[idx];
        out_u32[0] = (uint32_t) e->deviceMfg;
        out_u32[1] = (uint32_t) e->deviceModel;
        out_u32[2] = (uint32_t) e->technology;
        *out_attr = (uint64_t) e->attributes;
        if (serialize_mlu(e->Manufacturer, mblk, mcap, mused) == 0 &&
            serialize_mlu(e->Model, dblk, dcap, dused) == 0)
            ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* ProfileSequenceId element `idx` -> 16-byte ProfileID + serialized Description
   MLU. Returns 1/0. */
int rcms_oracle_seq_id_elem(const uint8_t* buf, uint32_t len, uint32_t sig, uint32_t idx,
                            uint8_t profile_id[16], uint8_t* blk, uint32_t cap, uint32_t* used) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsSEQ* seq = (cmsSEQ*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (seq && idx < seq->n) {
        cmsPSEQDESC* e = &seq->seq[idx];
        for (int i = 0; i < 16; i++) profile_id[i] = e->ProfileID.ID8[i];
        if (serialize_mlu(e->Description, blk, cap, used) == 0) ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* Dictionary -> entry count (length of cmsDictGetEntryList), or -1. */
int rcms_oracle_dict_count(const uint8_t* buf, uint32_t len, uint32_t sig) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return -1;
    cmsHANDLE hDict = (cmsHANDLE) cmsReadTag(p, (cmsTagSignature) sig);
    int n = -1;
    if (hDict) {
        n = 0;
        for (const cmsDICTentry* e = cmsDictGetEntryList(hDict); e != NULL; e = cmsDictNextEntry(e))
            n++;
    }
    cmsCloseProfile(p);
    return n;
}

/* Dictionary entry `idx` (in cmsDictGetEntryList enumeration order) -> name and
   value as raw u16 unit streams, plus serialized DisplayName/DisplayValue MLUs.
   Returns 1/0. */
int rcms_oracle_dict_entry(const uint8_t* buf, uint32_t len, uint32_t sig, uint32_t idx,
                           uint16_t* name_units, uint32_t ncap, uint32_t* nn,
                           uint16_t* value_units, uint32_t vcap, uint32_t* vn,
                           uint8_t* dnblk, uint32_t dncap, uint32_t* dnused,
                           uint8_t* dvblk, uint32_t dvcap, uint32_t* dvused) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsHANDLE hDict = (cmsHANDLE) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (hDict) {
        const cmsDICTentry* e = cmsDictGetEntryList(hDict);
        for (uint32_t i = 0; i < idx && e != NULL; i++) e = cmsDictNextEntry(e);
        if (e != NULL) {
            uint32_t n = 0, v = 0;
            if (e->Name)  { while (e->Name[n])  n++; }
            if (e->Value) { while (e->Value[v]) v++; }
            if (n <= ncap && v <= vcap) {
                for (uint32_t i = 0; i < n; i++) name_units[i]  = (uint16_t) e->Name[i];
                for (uint32_t i = 0; i < v; i++) value_units[i] = (uint16_t) e->Value[i];
                *nn = n; *vn = v;
                if (serialize_mlu(e->DisplayName, dnblk, dncap, dnused) == 0 &&
                    serialize_mlu(e->DisplayValue, dvblk, dvcap, dvused) == 0)
                    ok = 1;
            }
        }
    }
    cmsCloseProfile(p);
    return ok;
}

/* cmsReadTag of a curv/para tag -> a cmsToneCurve*, sampled via
   cmsEvalToneCurveFloat at the n caller-supplied x points (xs[0..n]), written to
   ys[0..n]. Returns 1 on success, 0 if the profile cannot be opened or the tag
   is absent / not tone-curve-backed. cmsReadTag returns a pointer lcms2 owns
   (freed with the profile), so we do NOT free it here. */
int rcms_oracle_read_tag_curve(const uint8_t* buf, uint32_t len, uint32_t sig,
                               const float* xs, uint32_t n, float* ys) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsToneCurve* c = (cmsToneCurve*) cmsReadTag(p, (cmsTagSignature) sig);
    if (!c) { cmsCloseProfile(p); return 0; }
    for (uint32_t i = 0; i < n; i++) ys[i] = cmsEvalToneCurveFloat(c, xs[i]);
    cmsCloseProfile(p);
    return 1;
}

/* cmsReadTag of a vcgt tag -> a cmsToneCurve** (array of 3 R/G/B curves), each
   sampled via cmsEvalToneCurveFloat at the n caller-supplied x points
   (xs[0..n]). Writes 3*n floats to ys row-major (channel 0 first, then 1, 2).
   Returns 1 on success, 0 if the profile cannot be opened or the tag is absent /
   not vcgt-backed. The cmsToneCurve** is owned by the profile (freed on close). */
int rcms_oracle_read_tag_vcgt(const uint8_t* buf, uint32_t len, uint32_t sig,
                              const float* xs, uint32_t n, float* ys) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsToneCurve** curves = (cmsToneCurve**) cmsReadTag(p, (cmsTagSignature) sig);
    if (!curves) { cmsCloseProfile(p); return 0; }
    for (int ch = 0; ch < 3; ch++) {
        if (!curves[ch]) { cmsCloseProfile(p); return 0; }
        for (uint32_t i = 0; i < n; i++)
            ys[ch * n + i] = cmsEvalToneCurveFloat(curves[ch], xs[i]);
    }
    cmsCloseProfile(p);
    return 1;
}

/* cmsReadTag of a bfd (UcrBg) tag -> a cmsUcrBg* { Ucr, Bg, Desc }. The Ucr and
   Bg tone curves are each sampled via cmsEvalToneCurveFloat at the n x points,
   written to ucr_ys[0..n] / bg_ys[0..n]. The Desc MLU's ASCII translation is
   written (NUL-terminated, truncated to dcap) to desc, with the byte count
   (excluding NUL) returned via *dused. Returns 1 on success, 0 otherwise. The
   cmsUcrBg* is owned by the profile (freed on close). */
int rcms_oracle_read_tag_ucrbg(const uint8_t* buf, uint32_t len, uint32_t sig,
                               const float* xs, uint32_t n,
                               float* ucr_ys, float* bg_ys,
                               char* desc, uint32_t dcap, uint32_t* dused) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsUcrBg* v = (cmsUcrBg*) cmsReadTag(p, (cmsTagSignature) sig);
    if (!v || !v->Ucr || !v->Bg) { cmsCloseProfile(p); return 0; }
    for (uint32_t i = 0; i < n; i++) {
        ucr_ys[i] = cmsEvalToneCurveFloat(v->Ucr, xs[i]);
        bg_ys[i]  = cmsEvalToneCurveFloat(v->Bg, xs[i]);
    }
    uint32_t got = 0;
    if (v->Desc) {
        /* cmsMLUgetASCII returns the byte count INCLUDING the NUL terminator. */
        uint32_t want = cmsMLUgetASCII(v->Desc, cmsNoLanguage, cmsNoCountry, NULL, 0);
        if (want > dcap) want = dcap;
        uint32_t wrote = cmsMLUgetASCII(v->Desc, cmsNoLanguage, cmsNoCountry, desc, want);
        got = (wrote > 0) ? wrote - 1 : 0; /* strip the NUL from the reported count */
    } else {
        if (dcap > 0) desc[0] = 0;
    }
    *dused = got;
    cmsCloseProfile(p);
    return 1;
}

/* ---- 3D CLUT tetrahedral interpolation (cmsintrp.c TetrahedralInterp16/Float) -
   Build a single granular CLUT stage with the given per-axis grid points (3 axes)
   and nOut output channels from the caller's table, wrap it in a 3->nOut pipeline,
   and evaluate one input vector through it. For a 3-input non-trilinear CLUT this
   routes through TetrahedralInterp16 / TetrahedralInterpFloat (lcms2's default).
   The grid is the per-axis sample count (nSamples[0..3]); table is laid out
   row-major with nOut output channels per node, matching lcms2's CLUT layout.
   Returns 1 on success, 0 if allocation fails. */
int rcms_oracle_tetra16(const uint32_t* grid /*3*/, uint32_t nOut,
                        const uint16_t* table, uint32_t tableLen,
                        const uint16_t* in /*3*/, uint16_t* out /*nOut*/) {
    (void) tableLen;
    cmsStage* stage = cmsStageAllocCLut16bitGranular(NULL, grid, 3, nOut, table);
    if (!stage) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, 3, nOut);
    if (!lut) { cmsStageFree(stage); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, stage)) {
        cmsStageFree(stage);
        cmsPipelineFree(lut);
        return 0;
    }
    cmsPipelineEval16(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

int rcms_oracle_tetra_float(const uint32_t* grid /*3*/, uint32_t nOut,
                            const float* table, uint32_t tableLen,
                            const float* in /*3*/, float* out /*nOut*/) {
    (void) tableLen;
    cmsStage* stage = cmsStageAllocCLutFloatGranular(NULL, grid, 3, nOut, table);
    if (!stage) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, 3, nOut);
    if (!lut) { cmsStageFree(stage); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, stage)) {
        cmsStageFree(stage);
        cmsPipelineFree(lut);
        return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* ---- Generic n-D CLUT interpolation via _cmsComputeInterpParamsEx -----------
   Drive lcms2's interpolation directly: build a cmsInterpParams from the caller's
   per-axis grid, nIn/nOut, table, and flags, then invoke the function pointer
   DefaultInterpolatorsFactory selected (p->Interpolation.Lerp16/LerpFloat). This
   exercises BilinearInterp16/Float, TrilinearInterp16/Float, Eval4..Eval15, and —
   critically — lets us force CMS_LERP_FLAGS_TRILINEAR for the 3-input path, which
   the cmsStageAllocCLut* path never sets (it hardcodes CMS_LERP_FLAGS_16BITS).
   `dwFlags` is the raw flag word (callers OR in CMS_LERP_FLAGS_FLOAT/TRILINEAR).
   Returns 1 on success, 0 if param computation fails. */
int rcms_oracle_interp16(const uint32_t* grid, uint32_t nIn, uint32_t nOut,
                         const uint16_t* table, uint32_t dwFlags,
                         const uint16_t* in, uint16_t* out) {
    cmsInterpParams* p = _cmsComputeInterpParamsEx(NULL, grid, nIn, nOut,
                                                   table, dwFlags);
    if (!p) return 0;
    p->Interpolation.Lerp16(in, out, p);
    _cmsFreeInterpParams(p);
    return 1;
}

int rcms_oracle_interp_float(const uint32_t* grid, uint32_t nIn, uint32_t nOut,
                             const float* table, uint32_t dwFlags,
                             const float* in, float* out) {
    cmsInterpParams* p = _cmsComputeInterpParamsEx(NULL, grid, nIn, nOut,
                                                   table, dwFlags | CMS_LERP_FLAGS_FLOAT);
    if (!p) return 0;
    p->Interpolation.LerpFloat(in, out, p);
    _cmsFreeInterpParams(p);
    return 1;
}

/* ---- Pipeline (cmsPipeline) differential shims (slice 4 task 2) -------------
   Build real lcms2 pipelines from a Matrix stage (cmsStageAllocMatrix) and/or a
   ToneCurves stage (cmsStageAllocToneCurves over cmsBuildTabulatedToneCurve16),
   then evaluate via cmsPipelineEval16 / cmsPipelineEvalFloat. These exercise the
   exact _LUTeval16 / _LUTevalFloat ping-pong walk and the From16ToFloat /
   FromFloatTo16 boundary conversions we reimplement in rcms::pipeline. */

/* Matrix-only pipeline. `matrix` is row-major Rows*Cols f64; `offset` is Rows
   f64 or NULL. in/out are 16-bit, widths Cols/Rows. */
int rcms_oracle_pipeline_matrix_eval16(uint32_t rows, uint32_t cols,
                                       const double* matrix, const double* offset,
                                       const uint16_t* in, uint16_t* out) {
    cmsStage* st = cmsStageAllocMatrix(NULL, rows, cols, matrix, offset);
    if (!st) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, cols, rows);
    if (!lut) { cmsStageFree(st); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, st)) {
        cmsStageFree(st); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEval16(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

int rcms_oracle_pipeline_matrix_eval_float(uint32_t rows, uint32_t cols,
                                           const double* matrix, const double* offset,
                                           const float* in, float* out) {
    cmsStage* st = cmsStageAllocMatrix(NULL, rows, cols, matrix, offset);
    if (!st) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, cols, rows);
    if (!lut) { cmsStageFree(st); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, st)) {
        cmsStageFree(st); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* Build a ToneCurves stage from `nCurves` 16-bit tabulated tables, each of
   length `tblLen`, packed contiguously in `tables`. Returns the stage or NULL. */
static cmsStage* build_curves_stage(uint32_t nCurves, uint32_t tblLen,
                                    const uint16_t* tables) {
    cmsToneCurve** curves = (cmsToneCurve**) calloc(nCurves, sizeof(cmsToneCurve*));
    if (!curves) return NULL;
    for (uint32_t i = 0; i < nCurves; i++) {
        curves[i] = cmsBuildTabulatedToneCurve16(NULL, tblLen, tables + (size_t)i * tblLen);
        if (!curves[i]) {
            for (uint32_t k = 0; k < i; k++) cmsFreeToneCurve(curves[k]);
            free(curves);
            return NULL;
        }
    }
    cmsStage* st = cmsStageAllocToneCurves(NULL, nCurves, curves);
    /* cmsStageAllocToneCurves dups each curve, so free our originals. */
    for (uint32_t i = 0; i < nCurves; i++) cmsFreeToneCurve(curves[i]);
    free(curves);
    return st;
}

/* ToneCurves-only pipeline (nCurves channels in == out). */
int rcms_oracle_pipeline_curves_eval16(uint32_t nCurves, uint32_t tblLen,
                                       const uint16_t* tables,
                                       const uint16_t* in, uint16_t* out) {
    cmsStage* st = build_curves_stage(nCurves, tblLen, tables);
    if (!st) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nCurves, nCurves);
    if (!lut) { cmsStageFree(st); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, st)) {
        cmsStageFree(st); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEval16(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

int rcms_oracle_pipeline_curves_eval_float(uint32_t nCurves, uint32_t tblLen,
                                           const uint16_t* tables,
                                           const float* in, float* out) {
    cmsStage* st = build_curves_stage(nCurves, tblLen, tables);
    if (!st) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nCurves, nCurves);
    if (!lut) { cmsStageFree(st); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, st)) {
        cmsStageFree(st); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* Combined curves -> matrix pipeline. The curves stage has `nCurves` channels
   (== matrix Cols); the matrix maps Cols -> Rows. in width = nCurves(=cols),
   out width = rows. */
int rcms_oracle_pipeline_curves_matrix_eval16(uint32_t nCurves, uint32_t tblLen,
                                              const uint16_t* tables,
                                              uint32_t rows, uint32_t cols,
                                              const double* matrix, const double* offset,
                                              const uint16_t* in, uint16_t* out) {
    cmsStage* curves = build_curves_stage(nCurves, tblLen, tables);
    if (!curves) return 0;
    cmsStage* mat = cmsStageAllocMatrix(NULL, rows, cols, matrix, offset);
    if (!mat) { cmsStageFree(curves); return 0; }
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nCurves, rows);
    if (!lut) { cmsStageFree(curves); cmsStageFree(mat); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, curves)) {
        cmsStageFree(curves); cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, mat)) {
        cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEval16(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

int rcms_oracle_pipeline_curves_matrix_eval_float(uint32_t nCurves, uint32_t tblLen,
                                                  const uint16_t* tables,
                                                  uint32_t rows, uint32_t cols,
                                                  const double* matrix, const double* offset,
                                                  const float* in, float* out) {
    cmsStage* curves = build_curves_stage(nCurves, tblLen, tables);
    if (!curves) return 0;
    cmsStage* mat = cmsStageAllocMatrix(NULL, rows, cols, matrix, offset);
    if (!mat) { cmsStageFree(curves); return 0; }
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nCurves, rows);
    if (!lut) { cmsStageFree(curves); cmsStageFree(mat); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, curves)) {
        cmsStageFree(curves); cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, mat)) {
        cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* ---- CLUT stage float-domain eval (slice 4 task 3) -------------------------
   Build a single CLUT stage (16-bit or float, n-D granular) wrapped in a
   1-stage nIn->nOut pipeline, and evaluate one input vector via
   cmsPipelineEvalFloat. A 16-bit CLUT therefore exercises EvaluateCLUTfloatIn16
   (FromFloatTo16 -> Lerp16 -> From16ToFloat); a float CLUT exercises
   EvaluateCLUTfloat (direct LerpFloat). `grid` is the per-axis sample count
   (nIn entries); `table` is row-major with nOut values per node. */
int rcms_oracle_clut_stage_eval16(const uint32_t* grid, uint32_t nIn, uint32_t nOut,
                                  const uint16_t* table,
                                  const float* in, float* out) {
    cmsStage* stage = cmsStageAllocCLut16bitGranular(NULL, grid, nIn, nOut, table);
    if (!stage) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nIn, nOut);
    if (!lut) { cmsStageFree(stage); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, stage)) {
        cmsStageFree(stage); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

int rcms_oracle_clut_stage_eval_float(const uint32_t* grid, uint32_t nIn, uint32_t nOut,
                                      const float* table,
                                      const float* in, float* out) {
    cmsStage* stage = cmsStageAllocCLutFloatGranular(NULL, grid, nIn, nOut, table);
    if (!stage) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nIn, nOut);
    if (!lut) { cmsStageFree(stage); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, stage)) {
        cmsStageFree(stage); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* ---- Lab/XYZ conversion stage evals (slice 4 task 3) -----------------------
   Each builds the corresponding lcms2 stage (_cmsStageAllocLab2XYZ etc.) in a
   1-stage 3->3 pipeline and evaluates a 3-float input via cmsPipelineEvalFloat.
   `which`: 0 = Lab2XYZ, 1 = XYZ2Lab, 2 = LabV2ToV4 (matrix form),
   3 = LabV4ToV2 (matrix form). Returns 0 on alloc failure or bad `which`. */
int rcms_oracle_labxyz_stage_eval(uint32_t which, const float* in, float* out) {
    cmsStage* stage = NULL;
    switch (which) {
        case 0: stage = _cmsStageAllocLab2XYZ(NULL); break;
        case 1: stage = _cmsStageAllocXYZ2Lab(NULL); break;
        case 2: stage = _cmsStageAllocLabV2ToV4(NULL); break;
        case 3: stage = _cmsStageAllocLabV4ToV2(NULL); break;
        default: return 0;
    }
    if (!stage) return 0;
    cmsPipeline* lut = cmsPipelineAlloc(NULL, 3, 3);
    if (!lut) { cmsStageFree(stage); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, stage)) {
        cmsStageFree(stage); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* ---- Combined CLUT -> curves -> matrix pipeline (slice 4 task 3) -----------
   A 3-input CLUT stage (16-bit, `grid`/`clutTable`, nOut output channels),
   feeding an nOut-channel ToneCurves stage (16-bit tabulated tables, each
   tblLen long), feeding a rows x nOut matrix (+ optional offset). Evaluated via
   cmsPipelineEvalFloat. Exercises a CLUT stage chained with curves and matrix.
   nOut must equal both the curve count and the matrix cols. */
int rcms_oracle_pipeline_clut_curves_matrix_eval_float(
        const uint32_t* grid, uint32_t nIn, uint32_t nOut,
        const uint16_t* clutTable,
        uint32_t tblLen, const uint16_t* curveTables,
        uint32_t rows, const double* matrix, const double* offset,
        const float* in, float* out) {
    cmsStage* clut = cmsStageAllocCLut16bitGranular(NULL, grid, nIn, nOut, clutTable);
    if (!clut) return 0;
    cmsStage* curves = build_curves_stage(nOut, tblLen, curveTables);
    if (!curves) { cmsStageFree(clut); return 0; }
    cmsStage* mat = cmsStageAllocMatrix(NULL, rows, nOut, matrix, offset);
    if (!mat) { cmsStageFree(clut); cmsStageFree(curves); return 0; }
    cmsPipeline* lut = cmsPipelineAlloc(NULL, nIn, rows);
    if (!lut) { cmsStageFree(clut); cmsStageFree(curves); cmsStageFree(mat); return 0; }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, clut)) {
        cmsStageFree(clut); cmsStageFree(curves); cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, curves)) {
        cmsStageFree(curves); cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    if (!cmsPipelineInsertStage(lut, cmsAT_END, mat)) {
        cmsStageFree(mat); cmsPipelineFree(lut); return 0;
    }
    cmsPipelineEvalFloat(in, out, lut);
    cmsPipelineFree(lut);
    return 1;
}

/* ---- cmsPipelineCat / cmsPipelineInsertStage(AT_BEGIN) (slice 5 task 0) -----
   Exercise the pipeline-construction API that transform building relies on. */

/* Build pipeline A = [ ToneCurves(3, tblLen) -> Matrix(3x3, matA[,offA]) ] and
   pipeline B = [ CLut16(3->3, grid/clutTable) ], then cmsPipelineCat(A, B) and
   evaluate `in` (3 f32) via cmsPipelineEvalFloat, writing 3 f32 to `out`. This
   mirrors the rcms `a.concat(&b)` test: A is 3->3, B is 3->3, so the cat chains
   3==3 and the result is 3->3. Returns 1 on success, 0 on any allocation /
   insert / cat failure. */
int rcms_oracle_pipeline_cat_eval_float(
        uint32_t tblLen, const uint16_t* curveTables,
        const double* matA, const double* offA,
        const uint32_t* grid, const uint16_t* clutTable,
        const float* in, float* out) {
    cmsPipeline* A = cmsPipelineAlloc(NULL, 3, 3);
    cmsPipeline* B = cmsPipelineAlloc(NULL, 3, 3);
    if (!A || !B) { if (A) cmsPipelineFree(A); if (B) cmsPipelineFree(B); return 0; }

    cmsStage* curves = build_curves_stage(3, tblLen, curveTables);
    cmsStage* mat = curves ? cmsStageAllocMatrix(NULL, 3, 3, matA, offA) : NULL;
    cmsStage* clut = mat ? cmsStageAllocCLut16bitGranular(NULL, grid, 3, 3, clutTable) : NULL;
    if (!curves || !mat || !clut) {
        if (curves) cmsStageFree(curves);
        if (mat) cmsStageFree(mat);
        if (clut) cmsStageFree(clut);
        cmsPipelineFree(A); cmsPipelineFree(B);
        return 0;
    }

    int ok = 0;
    if (cmsPipelineInsertStage(A, cmsAT_END, curves) &&
        cmsPipelineInsertStage(A, cmsAT_END, mat) &&
        cmsPipelineInsertStage(B, cmsAT_END, clut) &&
        cmsPipelineCat(A, B)) {
        cmsPipelineEvalFloat(in, out, A);
        ok = 1;
    }
    cmsPipelineFree(A);
    cmsPipelineFree(B);
    return ok;
}

/* Build pipeline P = [ ToneCurves(3, tblLen) ] then prepend a 3x3 Matrix stage
   via cmsPipelineInsertStage(P, cmsAT_BEGIN, mat). The result is
   [ Matrix -> ToneCurves ]. Evaluate `in` (3 f32) via cmsPipelineEvalFloat into
   `out` (3 f32). Mirrors rcms `p.prepend_stage(matrix)`. Returns 1/0. */
int rcms_oracle_pipeline_prepend_eval_float(
        uint32_t tblLen, const uint16_t* curveTables,
        const double* matA, const double* offA,
        const float* in, float* out) {
    cmsPipeline* P = cmsPipelineAlloc(NULL, 3, 3);
    if (!P) return 0;

    cmsStage* curves = build_curves_stage(3, tblLen, curveTables);
    cmsStage* mat = curves ? cmsStageAllocMatrix(NULL, 3, 3, matA, offA) : NULL;
    if (!curves || !mat) {
        if (curves) cmsStageFree(curves);
        if (mat) cmsStageFree(mat);
        cmsPipelineFree(P);
        return 0;
    }

    int ok = 0;
    if (cmsPipelineInsertStage(P, cmsAT_END, curves) &&
        cmsPipelineInsertStage(P, cmsAT_BEGIN, mat)) {
        cmsPipelineEvalFloat(in, out, P);
        ok = 1;
    }
    cmsPipelineFree(P);
    return ok;
}

/* ---- LUT8 / LUT16 tag readers (Type_LUT8_Read / Type_LUT16_Read) -----------
   cmsReadTag of an mft1/mft2 tag returns a cmsPipeline*. These extractors let
   the differential test build the SAME pipeline lcms2 builds and evaluate input
   grids through it, to diff against the rcms Pipeline bit-for-bit. */

/* Report the input/output channel counts of the pipeline lcms2 builds for the
   given mft1/mft2 tag. Returns 1 on success (writes *nIn/*nOut), 0 if the
   profile cannot be opened or the tag is absent / not pipeline-backed. */
int rcms_oracle_lut_channels(const uint8_t* buf, uint32_t len, uint32_t sig,
                             uint32_t* nIn, uint32_t* nOut) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsPipeline* lut = (cmsPipeline*) cmsReadTag(p, (cmsTagSignature) sig);
    int ok = 0;
    if (lut) {
        *nIn = cmsPipelineInputChannels(lut);
        *nOut = cmsPipelineOutputChannels(lut);
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* Evaluate `nSamples` input vectors through lcms2's pipeline for the mft1/mft2
   tag, in the 16-bit domain (cmsPipelineEval16). `inputs` is `nSamples * nIn`
   u16 row-major; `out` receives `nSamples * nOut` u16 row-major. Returns 1 on
   success, 0 otherwise. The pipeline is owned by the profile (freed on close). */
int rcms_oracle_lut_eval16(const uint8_t* buf, uint32_t len, uint32_t sig,
                           const uint16_t* inputs, uint32_t nSamples,
                           uint16_t* out) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsPipeline* lut = (cmsPipeline*) cmsReadTag(p, (cmsTagSignature) sig);
    if (!lut) { cmsCloseProfile(p); return 0; }
    uint32_t nIn = cmsPipelineInputChannels(lut);
    uint32_t nOut = cmsPipelineOutputChannels(lut);
    for (uint32_t s = 0; s < nSamples; s++) {
        cmsPipelineEval16(inputs + (size_t) s * nIn, out + (size_t) s * nOut, lut);
    }
    cmsCloseProfile(p);
    return 1;
}

/* Float counterpart of rcms_oracle_lut_eval16 via cmsPipelineEvalFloat. */
int rcms_oracle_lut_eval_float(const uint8_t* buf, uint32_t len, uint32_t sig,
                               const float* inputs, uint32_t nSamples,
                               float* out) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsPipeline* lut = (cmsPipeline*) cmsReadTag(p, (cmsTagSignature) sig);
    if (!lut) { cmsCloseProfile(p); return 0; }
    uint32_t nIn = cmsPipelineInputChannels(lut);
    uint32_t nOut = cmsPipelineOutputChannels(lut);
    for (uint32_t s = 0; s < nSamples; s++) {
        cmsPipelineEvalFloat(inputs + (size_t) s * nIn, out + (size_t) s * nOut, lut);
    }
    cmsCloseProfile(p);
    return 1;
}

/* Whether lcms2's cmsReadTag returns a non-NULL cooked value for `sig`. Used by
   the comprehensive sweep to distinguish "rcms has a bug" from "lcms2 itself
   rejects this (malformed) tag, so rcms is right to fail too". Returns 1 if the
   profile opens AND cmsReadTag(sig) != NULL, else 0. */
int rcms_oracle_tag_read_succeeds(const uint8_t* buf, uint32_t len, uint32_t sig) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    void* v = cmsReadTag(p, (cmsTagSignature) sig);
    int ok = (v != NULL);
    cmsCloseProfile(p);
    return ok;
}

/* ---- Profile -> pipeline LUT extraction (cmsio1.c) --------------------------
   _cmsReadInputLUT / _cmsReadOutputLUT / _cmsReadDevicelinkLUT are CMSCHECKPOINT-
   exported internals (lcms2_internal.h:964-966). These shims open a profile from
   memory, build the requested LUT for `intent`, and evaluate `nSamples` input
   vectors through it via cmsPipelineEvalFloat. `which`: 0 = input, 1 = output,
   2 = devicelink. `inputs` is `nSamples * nIn` f32 row-major; `out` receives
   `nSamples * nOut` f32 row-major. `nInOut[0]/[1]` receive the pipeline's in/out
   channel counts. Returns 1 on success (LUT built), 0 if the profile cannot be
   opened or lcms2 returns NULL for the requested LUT. */
static cmsPipeline* read_lut_which(cmsHPROFILE p, uint32_t which, uint32_t intent) {
    switch (which) {
        case 0:  return _cmsReadInputLUT(p, intent);
        case 1:  return _cmsReadOutputLUT(p, intent);
        case 2:  return _cmsReadDevicelinkLUT(p, intent);
        default: return NULL;
    }
}

/* Report whether lcms2 builds a LUT for (which, intent) and its channel counts. */
int rcms_oracle_read_lut_channels(const uint8_t* buf, uint32_t len, uint32_t which,
                                  uint32_t intent, uint32_t* nIn, uint32_t* nOut) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsPipeline* lut = read_lut_which(p, which, intent);
    int ok = 0;
    if (lut) {
        *nIn = cmsPipelineInputChannels(lut);
        *nOut = cmsPipelineOutputChannels(lut);
        cmsPipelineFree(lut);
        ok = 1;
    }
    cmsCloseProfile(p);
    return ok;
}

/* Build the LUT for (which, intent) and evaluate nSamples input vectors through
   it in the float domain. Returns 1 on success, 0 if no LUT. */
int rcms_oracle_read_lut_eval_float(const uint8_t* buf, uint32_t len, uint32_t which,
                                    uint32_t intent, const float* inputs,
                                    uint32_t nSamples, float* out) {
    cmsHPROFILE p = cmsOpenProfileFromMem((const void*)buf, len);
    if (!p) return 0;
    cmsPipeline* lut = read_lut_which(p, which, intent);
    if (!lut) { cmsCloseProfile(p); return 0; }
    uint32_t nIn = cmsPipelineInputChannels(lut);
    uint32_t nOut = cmsPipelineOutputChannels(lut);
    for (uint32_t s = 0; s < nSamples; s++) {
        cmsPipelineEvalFloat(inputs + (size_t) s * nIn, out + (size_t) s * nOut, lut);
    }
    cmsPipelineFree(lut);
    cmsCloseProfile(p);
    return 1;
}

/* ---- cmsReverseToneCurve (cmsgamma.c) --------------------------------------
   Build a 16-bit tabulated tone curve from `table` (length n), reverse it via
   cmsReverseToneCurve, and evaluate the reversed curve via cmsEvalToneCurveFloat
   at the `nx` points in `xs`, writing `nx` floats into `ys`. Returns 1 on
   success, 0 on allocation failure. */
int rcms_oracle_reverse_tabulated16_eval_float(const uint16_t* table, uint32_t n,
                                               const float* xs, uint32_t nx,
                                               float* ys) {
    cmsToneCurve* c = cmsBuildTabulatedToneCurve16(NULL, n, table);
    if (!c) return 0;
    cmsToneCurve* rev = cmsReverseToneCurve(c);
    if (!rev) { cmsFreeToneCurve(c); return 0; }
    for (uint32_t i = 0; i < nx; i++) ys[i] = cmsEvalToneCurveFloat(rev, xs[i]);
    cmsFreeToneCurve(rev);
    cmsFreeToneCurve(c);
    return 1;
}

/* ---- Multiprofile transform (cmsxform.c / cmscnvrt.c) ----------------------
   Build an N-profile transform via cmsCreateExtendedTransform with EXPLICIT
   intents, BPC, and adaptation-state arrays, forcing cmsFLAGS_NOOPTIMIZE so the
   un-optimized device-link pipeline runs (the slice-5 differential reference).
   Input/output formats are generic float (FLOAT_SH(1)|PT_ANY|CHANNELS_SH(n)|
   BYTES_SH(4)) so packing/unpacking is the identity FloatXFORM path. Each
   profile is opened from its own memory block (bufs[i], lens[i]); `inChans`/
   `outChans` are the first/last device channel counts; `in`/`out` carry
   nPixels*inChans / nPixels*outChans floats row-major. Returns 1 on success,
   0 if any profile fails to open or the transform cannot be created. */
static uint32_t float_format(uint32_t nChans) {
    return FLOAT_SH(1) | COLORSPACE_SH(PT_ANY) | CHANNELS_SH(nChans) | BYTES_SH(4);
}

int rcms_oracle_transform_eval_float(const uint8_t* const* bufs, const uint32_t* lens,
                                     uint32_t n, const uint32_t* intents,
                                     const int32_t* bpc, const double* adaptation,
                                     const float* in, uint32_t inChans,
                                     float* out, uint32_t outChans, uint32_t nPixels) {
    if (n == 0 || n > 255) return 0;
    cmsHPROFILE* profiles = (cmsHPROFILE*) calloc(n, sizeof(cmsHPROFILE));
    cmsBool*     bpcArr    = (cmsBool*)    calloc(n, sizeof(cmsBool));
    cmsUInt32Number* intArr = (cmsUInt32Number*) calloc(n, sizeof(cmsUInt32Number));
    cmsFloat64Number* adArr = (cmsFloat64Number*) calloc(n, sizeof(cmsFloat64Number));
    if (!profiles || !bpcArr || !intArr || !adArr) {
        free(profiles); free(bpcArr); free(intArr); free(adArr);
        return 0;
    }
    int ok = 1;
    for (uint32_t i = 0; i < n; i++) {
        profiles[i] = cmsOpenProfileFromMem((const void*) bufs[i], lens[i]);
        if (!profiles[i]) ok = 0;
        bpcArr[i] = bpc[i] ? TRUE : FALSE;
        intArr[i] = intents[i];
        adArr[i]  = adaptation[i];
    }

    cmsHTRANSFORM xform = NULL;
    if (ok) {
        xform = cmsCreateExtendedTransform(
            NULL, n, profiles, bpcArr, intArr, adArr,
            NULL, 0, float_format(inChans), float_format(outChans),
            cmsFLAGS_NOOPTIMIZE);
    }
    if (xform) {
        cmsDoTransform(xform, in, out, nPixels);
        cmsDeleteTransform(xform);
    } else {
        ok = 0;
    }
    for (uint32_t i = 0; i < n; i++) if (profiles[i]) cmsCloseProfile(profiles[i]);
    free(profiles); free(bpcArr); free(intArr); free(adArr);
    return ok;
}

/* Same as rcms_oracle_transform_eval_float but 16-bit: input/output formats are
   generic 16-bit (FLOAT_SH(0)|PT_ANY|CHANNELS_SH(n)|BYTES_SH(2)), so the
   PrecalculatedXFORM/CachedXFORM path runs and cmsDoTransform reads/writes u16.
   `in`/`out` carry nPixels*inChans / nPixels*outChans uint16 row-major. Returns 1
   on success, 0 on failure. */
static uint32_t u16_format(uint32_t nChans) {
    return COLORSPACE_SH(PT_ANY) | CHANNELS_SH(nChans) | BYTES_SH(2);
}

int rcms_oracle_transform_eval_16(const uint8_t* const* bufs, const uint32_t* lens,
                                  uint32_t n, const uint32_t* intents,
                                  const int32_t* bpc, const double* adaptation,
                                  const uint16_t* in, uint32_t inChans,
                                  uint16_t* out, uint32_t outChans, uint32_t nPixels) {
    if (n == 0 || n > 255) return 0;
    cmsHPROFILE* profiles = (cmsHPROFILE*) calloc(n, sizeof(cmsHPROFILE));
    cmsBool*     bpcArr    = (cmsBool*)    calloc(n, sizeof(cmsBool));
    cmsUInt32Number* intArr = (cmsUInt32Number*) calloc(n, sizeof(cmsUInt32Number));
    cmsFloat64Number* adArr = (cmsFloat64Number*) calloc(n, sizeof(cmsFloat64Number));
    if (!profiles || !bpcArr || !intArr || !adArr) {
        free(profiles); free(bpcArr); free(intArr); free(adArr);
        return 0;
    }
    int ok = 1;
    for (uint32_t i = 0; i < n; i++) {
        profiles[i] = cmsOpenProfileFromMem((const void*) bufs[i], lens[i]);
        if (!profiles[i]) ok = 0;
        bpcArr[i] = bpc[i] ? TRUE : FALSE;
        intArr[i] = intents[i];
        adArr[i]  = adaptation[i];
    }

    cmsHTRANSFORM xform = NULL;
    if (ok) {
        xform = cmsCreateExtendedTransform(
            NULL, n, profiles, bpcArr, intArr, adArr,
            NULL, 0, u16_format(inChans), u16_format(outChans),
            cmsFLAGS_NOOPTIMIZE);
    }
    if (xform) {
        cmsDoTransform(xform, in, out, nPixels);
        cmsDeleteTransform(xform);
    } else {
        ok = 0;
    }
    for (uint32_t i = 0; i < n; i++) if (profiles[i]) cmsCloseProfile(profiles[i]);
    free(profiles); free(bpcArr); free(intArr); free(adArr);
    return ok;
}

/* ---- Pixel-format unpack/pack formatters (cmspack.c) ----------------------
   Drive lcms2's REAL stock unpack/pack formatters in isolation. The stock
   16-bit formatters read only info->InputFormat / info->OutputFormat (verified
   against cmspack.c: every formatter pulls T_* fields off that one word and
   touches no other transform field), so a zeroed _cmsTRANSFORM with just the
   format word set reproduces exactly what cmsDoTransform's FromInput/ToOutput
   would do for these simple chunky/planar-free types. We fetch the formatter
   via the exported _cmsGetFormatter(NULL, fmt, dir, CMS_PACK_FLAGS_16BITS) and
   call its .Fmt16 pointer. Stride is 0 (chunky single pixel). */

void rcms_oracle_unpack16(uint32_t fmt, const uint8_t* buf, uint16_t* out) {
    _cmsTRANSFORM info;
    memset(&info, 0, sizeof(info));
    info.InputFormat = fmt;
    cmsFormatter fn = _cmsGetFormatter(NULL, fmt, cmsFormatterInput, CMS_PACK_FLAGS_16BITS);
    if (fn.Fmt16 == NULL) return;
    /* Formatter writes only T_CHANNELS(fmt) entries; caller zeroes the rest. */
    fn.Fmt16(&info, out, (cmsUInt8Number*) buf, 0);
}

void rcms_oracle_pack16(uint32_t fmt, const uint16_t* values, uint8_t* out, uint32_t* nbytes) {
    _cmsTRANSFORM info;
    memset(&info, 0, sizeof(info));
    info.OutputFormat = fmt;
    cmsFormatter fn = _cmsGetFormatter(NULL, fmt, cmsFormatterOutput, CMS_PACK_FLAGS_16BITS);
    if (fn.Fmt16 == NULL) { *nbytes = 0; return; }
    /* cmsUInt16Number wOut[] is const-by-contract for packers; cast away const. */
    cmsUInt8Number* end = fn.Fmt16(&info, (cmsUInt16Number*) values, out, 0);
    *nbytes = (uint32_t) (end - out);
}

/* lcms2 FROM_8_TO_16 / FROM_16_TO_8 (lcms2_internal.h:125-126). */
uint16_t rcms_oracle_from_8_to_16(uint8_t v) { return FROM_8_TO_16(v); }
uint8_t  rcms_oracle_from_16_to_8(uint16_t v) { return FROM_16_TO_8(v); }
