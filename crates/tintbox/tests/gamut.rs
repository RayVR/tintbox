//! Differential tests for `tintbox::gamut` (lcms2 `cmssm.c` + `cmsgmt.c`):
//! - `cmsDetectTAC` over the CMYK testbed printers (bit-exact f64).
//! - `cmsCreateProofingTransform` + `cmsDoTransform` over device→proof→device
//!   triples × intents, with and without `cmsFLAGS_GAMUTCHECK` (alarm colors),
//!   byte-identical to lcms2 (NOOPTIMIZE).
//! - the gamut boundary descriptor check-point verdict (`cmsGDBCheckPoint`).

use std::fs;
use std::path::{Path, PathBuf};

use tintbox::format::decode::{TYPE_CMYK_16, TYPE_RGB_16};
use tintbox::gamut::{detect_tac, GamutBoundaryDescriptor};
use tintbox::prelude::CIELab;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::{Flags, Transform};

fn testbed_dir() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/Little-CMS/testbed"
    ))
    .to_path_buf()
}

fn read_bytes(name: &str) -> Vec<u8> {
    fs::read(testbed_dir().join(name)).unwrap()
}

const ALL_INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];

// ---- cmsDetectTAC -----------------------------------------------------------

#[test]
fn detect_tac_bit_exact_vs_lcms2() {
    // The CMYK output-class printers exercise the round-trip + slice-space sweep;
    // RGB profiles return 0 (not output class) on both sides.
    for name in ["test1.icc", "test2.icc", "test3.icc", "crayons.icc"] {
        let bytes = read_bytes(name);
        let p = Profile::open(&bytes).unwrap();
        let mine = detect_tac(&p);
        let theirs = tintbox_oracle::detect_tac(&bytes);
        assert_eq!(
            mine.to_bits(),
            theirs.to_bits(),
            "detect_tac mismatch for {name}: mine={mine} theirs={theirs}"
        );
    }
}

// ---- Proofing transform -----------------------------------------------------

/// A spread of RGB16 inputs covering primaries, neutrals, and saturated colors —
/// some are in-gamut, some out, so the gamut-check alarm path fires.
const RGB_INPUTS: [[u16; 3]; 9] = [
    [0, 0, 0],
    [0xffff, 0xffff, 0xffff],
    [0xffff, 0, 0],
    [0, 0xffff, 0],
    [0, 0, 0xffff],
    [0x8000, 0x4000, 0xc000],
    [0x1234, 0x5678, 0x9abc],
    [0xfedc, 0xba98, 0x7654],
    [0x0101, 0x8080, 0xfefe],
];

fn pack_rgb16(inputs: &[[u16; 3]]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(inputs.len() * 6);
    for px in inputs {
        for &c in px {
            buf.extend_from_slice(&c.to_ne_bytes());
        }
    }
    buf
}

#[test]
fn proofing_transform_byte_identical_vs_lcms2() {
    // RGB input → CMYK proof → RGB output, every intent, soft-proofing with and
    // without gamut check. Byte-identical do_transform output vs lcms2.
    let in_bytes = read_bytes("test5.icc"); // V2 RGB monitor (matrix shaper)
    let proof_bytes = read_bytes("test1.icc"); // V2 CMYK printer (the gamut)
    let out_bytes = read_bytes("test3.icc"); // V2 RGB/Lab CLUT
    let pin = Profile::open(&in_bytes).unwrap();
    let pproof = Profile::open(&proof_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();

    let in_buf = pack_rgb16(&RGB_INPUTS);
    let n = RGB_INPUTS.len();

    let mut cells = 0;
    for &n_intent in &ALL_INTENTS {
        for &proof_intent in &ALL_INTENTS {
            for &gamutcheck in &[false, true] {
                let mut flags = Flags::NOOPTIMIZE.union(Flags::SOFTPROOFING);
                if gamutcheck {
                    flags = flags.union(Flags::GAMUTCHECK);
                }

                let xform = Transform::new_proofing(
                    &pin,
                    TYPE_RGB_16,
                    &pout,
                    TYPE_RGB_16,
                    &pproof,
                    n_intent,
                    proof_intent,
                    flags,
                )
                .expect("proofing transform must build");

                let mut mine = vec![0u8; n * 6];
                xform.do_transform(&in_buf, &mut mine, n);

                let mut theirs = vec![0u8; n * 6];
                let ok = tintbox_oracle::proofing_transform_packed(
                    &in_bytes,
                    &out_bytes,
                    &proof_bytes,
                    n_intent.to_raw(),
                    proof_intent.to_raw(),
                    gamutcheck,
                    true, // softproofing
                    false,
                    TYPE_RGB_16,
                    TYPE_RGB_16,
                    &in_buf,
                    &mut theirs,
                    n,
                );
                assert!(ok, "lcms2 proofing transform failed to build");
                assert_eq!(
                    mine,
                    theirs,
                    "proofing pixels differ: n_intent={} proof_intent={} gamutcheck={gamutcheck}",
                    n_intent.to_raw(),
                    proof_intent.to_raw()
                );
                cells += 1;
            }
        }
    }
    assert_eq!(cells, 4 * 4 * 2);
}

#[test]
fn proofing_transform_custom_alarm_codes_byte_identical() {
    // With gamut check ON and non-default alarm codes, the alarm substitution must
    // match lcms2 byte-for-byte. RGB input → CMYK proof → CMYK output.
    let in_bytes = read_bytes("test5.icc");
    let proof_bytes = read_bytes("test1.icc");
    let out_bytes = read_bytes("test2.icc"); // CMYK output
    let pin = Profile::open(&in_bytes).unwrap();
    let pproof = Profile::open(&proof_bytes).unwrap();
    let pout = Profile::open(&out_bytes).unwrap();

    let alarm: [u16; 16] = {
        let mut a = [0u16; 16];
        a[0] = 0x1234;
        a[1] = 0xabcd;
        a[2] = 0x00ff;
        a[3] = 0x7fff;
        a
    };

    let in_buf = pack_rgb16(&RGB_INPUTS);
    let n = RGB_INPUTS.len();
    let intent = RenderingIntent::RelativeColorimetric;

    let mut xform = Transform::new_proofing(
        &pin,
        TYPE_RGB_16,
        &pout,
        TYPE_CMYK_16,
        &pproof,
        intent,
        intent,
        Flags::NOOPTIMIZE
            .union(Flags::SOFTPROOFING)
            .union(Flags::GAMUTCHECK),
    )
    .expect("proofing transform must build");
    xform.set_alarm_codes(alarm);

    let mut mine = vec![0u8; n * 8]; // CMYK16
    xform.do_transform(&in_buf, &mut mine, n);

    let mut theirs = vec![0u8; n * 8];
    let ok = tintbox_oracle::proofing_transform_packed_alarm(
        &in_bytes,
        &out_bytes,
        &proof_bytes,
        intent.to_raw(),
        intent.to_raw(),
        true,
        true,
        false,
        &alarm,
        TYPE_RGB_16,
        TYPE_CMYK_16,
        &in_buf,
        &mut theirs,
        n,
    );
    assert!(ok, "lcms2 alarm proofing transform failed");
    assert_eq!(mine, theirs, "custom-alarm proofing pixels differ");
}

// ---- Gamut boundary descriptor ---------------------------------------------

/// Sample a coarse Lab cube and build add/check point sets. The add set seeds the
/// descriptor; the check set probes interior + far-out points.
fn lab_points() -> (Vec<[f64; 3]>, Vec<[f64; 3]>) {
    let mut add = Vec::new();
    // A blobby boundary: an ellipsoid surface of specified points.
    for li in 0..7 {
        let l = li as f64 * 100.0 / 6.0;
        for ai in 0..9 {
            let a = -60.0 + ai as f64 * 120.0 / 8.0;
            for bi in 0..9 {
                let b = -60.0 + bi as f64 * 120.0 / 8.0;
                // Keep a rough sphere of radius ~55 around (L=50,a=0,b=0).
                let r = ((l - 50.0).powi(2) + a * a + b * b).sqrt();
                if r <= 55.0 {
                    add.push([l, a, b]);
                }
            }
        }
    }

    let check = vec![
        [50.0, 0.0, 0.0],    // dead center — in
        [50.0, 10.0, 10.0],  // near center — in
        [50.0, 90.0, 90.0],  // far out — out
        [0.0, 0.0, 0.0],     // black
        [100.0, 0.0, 0.0],   // white
        [25.0, 40.0, -30.0], // mid
        [75.0, -40.0, 30.0], // mid
        [50.0, 200.0, 0.0],  // way out
        [10.0, 5.0, 5.0],    // shadow interior
        [90.0, 3.0, -3.0],   // highlight interior
    ];
    (add, check)
}

#[test]
fn gbd_check_point_matches_lcms2() {
    let (add, check) = lab_points();

    let mut gbd = GamutBoundaryDescriptor::new();
    for p in &add {
        gbd.add_point(&CIELab {
            l: p[0],
            a: p[1],
            b: p[2],
        });
    }
    gbd.compute();

    let mine: Vec<bool> = check
        .iter()
        .map(|p| {
            gbd.check_point(&CIELab {
                l: p[0],
                a: p[1],
                b: p[2],
            })
        })
        .collect();

    let theirs = tintbox_oracle::gbd_check(&add, &check);

    assert_eq!(
        mine, theirs,
        "GBD check-point verdicts differ from lcms2:\n mine={mine:?}\n theirs={theirs:?}"
    );
}
