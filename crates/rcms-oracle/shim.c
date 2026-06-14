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
