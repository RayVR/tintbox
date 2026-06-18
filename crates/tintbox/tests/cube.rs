//! Stage-1 self-consistency tests for the `.cube` → RGB device-link builder
//! (`create_devicelink_from_cube_mem`, a port of lcms2
//! `cmsCreateDeviceLinkFromCubeFile`).
//!
//! These verify the feature is internally correct — tintbox parses a `.cube`,
//! builds the device-link, materialises it in memory (`from_writable` — the
//! evaluation path lcms2 uses; lcms2 cannot *serialise* a 3D-LUT cube
//! device-link at all), and inspects the structure (v4.4 Link/RGB/RGB profile
//! carrying the shaper + float CLUT in `A2B0` and the title in `desc`), plus
//! malformed-input robustness. Byte-for-byte transform parity against lcms2 is
//! the separate differential test.

use tintbox::cgats::create_devicelink_from_cube_mem;
use tintbox::profile::header::{ColorSpace, ProfileClass, RenderingIntent};
use tintbox::profile::{Profile, Tag};
use tintbox::sig::Signature;

/// A `.cube` with a 1D shaper (2 rows) followed by a 2×2×2 3D LUT (8 nodes).
const CUBE_SHAPER_AND_CLUT: &str = "TITLE \"tintbox cube test\"\n\
LUT_1D_SIZE 2\n\
LUT_3D_SIZE 2\n\
0.0 0.0 0.0\n\
1.0 1.0 1.0\n\
0.0 0.0 0.0\n\
1.0 0.0 0.0\n\
0.0 1.0 0.0\n\
1.0 1.0 0.0\n\
0.0 0.0 1.0\n\
1.0 0.0 1.0\n\
0.0 1.0 1.0\n\
1.0 1.0 1.0\n";

/// A `.cube` with only a 3D LUT (no shaper), and an explicit domain.
const CUBE_CLUT_ONLY: &str = "TITLE \"clut only\"\n\
DOMAIN_MIN 0.0 0.0 0.0\n\
DOMAIN_MAX 1.0 1.0 1.0\n\
LUT_3D_SIZE 2\n\
0.0 0.0 0.0\n\
1.0 0.0 0.0\n\
0.0 1.0 0.0\n\
1.0 1.0 0.0\n\
0.0 0.0 1.0\n\
1.0 0.0 1.0\n\
0.0 1.0 1.0\n\
1.0 1.0 1.0\n";

fn sig(s: &[u8; 4]) -> Signature {
    Signature::from_bytes(*s)
}

/// Build the device-link and materialise it as an in-memory [`Profile`] (the
/// evaluation path lcms2 uses — no serialisation, which lcms2 itself cannot do
/// for a 3D-LUT cube).
fn build(cube: &str) -> Profile<'static> {
    let writable = create_devicelink_from_cube_mem(cube.as_bytes())
        .expect("cube should build a device-link profile");
    Profile::from_writable(&writable).expect("device-link should materialise in memory")
}

#[test]
fn cube_builds_a_v44_rgb_devicelink() {
    for cube in [CUBE_SHAPER_AND_CLUT, CUBE_CLUT_ONLY] {
        let profile = build(cube);
        let h = profile.header();

        assert_eq!(h.device_class, ProfileClass::Link, "device class");
        assert_eq!(h.color_space, ColorSpace::Rgb, "colour space");
        assert_eq!(h.pcs, ColorSpace::Rgb, "PCS");
        assert_eq!(h.version, 0x0440_0000, "version 4.4");
        assert_eq!(h.rendering_intent, RenderingIntent::Perceptual, "intent");

        // Exactly the two tags lcms2 writes: desc then A2B0.
        let tags: Vec<u32> = profile.tags().map(|s| s.to_raw()).collect();
        assert_eq!(
            tags,
            vec![sig(b"desc").to_raw(), sig(b"A2B0").to_raw()],
            "tags + order"
        );

        // The A2B0 LUT must read back as a pipeline (proves it serialised validly).
        match profile.read_tag(sig(b"A2B0")) {
            Ok(Tag::Lut(_)) => {}
            other => panic!("A2B0 should be a LUT pipeline, got {other:?}"),
        }
        // And the description must read back.
        assert!(
            profile.read_tag(sig(b"desc")).is_ok(),
            "desc should read back"
        );
    }
}

#[test]
fn cube_devicelink_drives_an_input_lut() {
    // Reading the input LUT exercises the A2B0 pipeline build (shaper + float
    // CLUT + identity B-curves), confirming the device-link is evaluation-ready.
    let profile = build(CUBE_SHAPER_AND_CLUT);
    let lut = tintbox::link::read_input_lut(&profile, 0);
    assert!(lut.is_ok(), "device-link input LUT should build: {lut:?}");
}

#[test]
fn malformed_cube_is_rejected_not_panicking() {
    // (Empty input is *not* here: lcms2 accepts an empty cube too, building a
    // degenerate profile — so rejecting it would be a divergence.)
    let cases: [&[u8]; 4] = [
        b"TITLE",                        // truncated
        b"LUT_3D_SIZE 999999\n0 0 0\n",  // size out of the 2..=65 bound
        b"LUT_3D_SIZE 2\n0 0 0\n",       // too few nodes
        b"LUT_1D_INPUT_RANGE 0.0 2.0\n", // unsupported range
    ];
    for (i, c) in cases.iter().enumerate() {
        // Must return Err, never panic.
        assert!(
            create_devicelink_from_cube_mem(c).is_err(),
            "case {i} should be rejected"
        );
    }
}

/// Tiny deterministic xorshift PRNG — reproducible failures, no `rand` dep.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

#[test]
fn mutated_cube_does_not_panic() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let seed = CUBE_SHAPER_AND_CLUT.as_bytes();
    let mut rng = Rng(0x0CB_FACE);
    let iters: usize = std::env::var("TINTBOX_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    for i in 0..iters {
        let mut buf = seed.to_vec();
        match rng.below(3) {
            0 => buf.truncate(rng.below(buf.len().max(1))),
            1 => {
                for _ in 0..=rng.below(4) {
                    let idx = rng.below(buf.len().max(1));
                    if idx < buf.len() {
                        buf[idx] ^= (rng.next_u64() & 0xFF) as u8;
                    }
                }
            }
            // Splice a long digit run toward the numeric-overflow guards.
            _ => {
                let idx = rng.below(buf.len().max(1));
                let digits = vec![b'9'; 1 + rng.below(40)];
                buf.splice(idx..idx, digits);
            }
        }
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = create_devicelink_from_cube_mem(&buf);
        }));
        assert!(r.is_ok(), "PANIC on mutated cube at iter {i}");
    }
}

/// A 2×2×2 cube with distinct corner outputs, so non-corner inputs interpolate
/// to non-trivial values — exercising the CLUT interpolation under the
/// differential test (an identity cube would pass trivially).
const CUBE_NONTRIVIAL: &str = "TITLE \"nontrivial\"\n\
LUT_3D_SIZE 2\n\
0.05 0.10 0.15\n\
0.80 0.20 0.10\n\
0.10 0.75 0.20\n\
0.90 0.85 0.05\n\
0.15 0.20 0.70\n\
0.85 0.10 0.80\n\
0.20 0.90 0.75\n\
0.95 0.90 0.85\n";

/// The differential cross-check: tintbox's in-memory `.cube` device-link must
/// transform pixels byte-for-byte the same as lcms2's, over a sweep of inputs.
/// Both use the lossless (NOOPTIMIZE) path — tintbox's default AccurateFast is
/// byte-identical to it.
#[test]
fn cube_transform_matches_lcms2() {
    use tintbox::format::decode::TYPE_RGB_8;
    use tintbox::transform::{Flags, Transform};
    const INTENT_PERCEPTUAL: u32 = 0;

    for cube in [CUBE_NONTRIVIAL, CUBE_SHAPER_AND_CLUT, CUBE_CLUT_ONLY] {
        let profile = build(cube);
        let xform = Transform::new_with_formats(
            &[&profile],
            &[RenderingIntent::Perceptual],
            &[false],
            &[1.0],
            Flags::empty(),
            TYPE_RGB_8,
            TYPE_RGB_8,
        )
        .expect("device-link transform should build");

        let n = 256usize;
        let mut input = Vec::with_capacity(n * 3);
        for i in 0..n {
            let v = i as u8;
            input.extend_from_slice(&[v, v.wrapping_mul(2), 255u8.wrapping_sub(v)]);
        }
        let mut tb = vec![0u8; n * 3];
        xform.do_transform(&input, &mut tb, n);

        let mut oracle = vec![0u8; n * 3];
        let ok = tintbox_oracle::cube_transform(
            cube.as_bytes(),
            TYPE_RGB_8,
            TYPE_RGB_8,
            INTENT_PERCEPTUAL,
            &input,
            &mut oracle,
            n,
        );
        assert!(ok, "lcms2 cube transform failed to run");
        assert_eq!(tb, oracle, "cube transform diverges from lcms2");
    }
}
