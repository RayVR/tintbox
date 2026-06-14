#include "lcms2_internal.h"
#include <stdint.h>

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
