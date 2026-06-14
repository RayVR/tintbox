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
