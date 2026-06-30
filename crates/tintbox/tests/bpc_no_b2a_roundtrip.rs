//! Differential regression for the BPC black-point-detection parity bug
//! (docs/bug-bpc-perceptual-black-roundtrip-range.md).
//!
//! Building a BPC transform from a CMYK `Output`-class profile that has `A2B0`
//! but no `B2A0` must succeed and match lcms2 byte-for-byte. lcms2's
//! `BlackPointUsingPerceptualBlack` maps an un-buildable round-trip to a `{0,0,0}`
//! black point and proceeds; tintbox previously propagated `Err` and aborted the
//! whole transform build (a bit-identity violation — lcms2 completes it).

use tintbox::format::decode::{TYPE_CMYK_8, TYPE_RGB_8};
use tintbox::profile::serialize::save_to_mem;
use tintbox::profile::virtuals::build_srgb_profile;
use tintbox::profile::{Profile, RenderingIntent};
use tintbox::transform::Transform;

/// Minimal `prtr`/CMYK/Lab-PCS profile: one `A2B0` mft1 (LUT8) tag, no `B2A0`.
/// in=4 out=3, constant CLUT mapping every CMYK to one Lab value.
fn build_cmyk_a2b0_only(target_l_byte: u8, grid: u8) -> Vec<u8> {
    let (in_chan, out_chan) = (4u8, 3u8);
    let mut lut = Vec::new();
    lut.extend_from_slice(&0x6d66_7431u32.to_be_bytes()); // 'mft1'
    lut.extend_from_slice(&0u32.to_be_bytes()); // reserved
    lut.extend_from_slice(&[in_chan, out_chan, grid, 0]);
    for v in [0x0001_0000i32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x0001_0000] {
        lut.extend_from_slice(&(v as u32).to_be_bytes()); // identity matrix
    }
    for _ in 0..in_chan {
        for i in 0..256u16 {
            lut.push(i as u8);
        }
    } // input curves
    for _ in 0..(grid as usize).pow(in_chan as u32) {
        lut.extend_from_slice(&[target_l_byte, 128, 128]); // constant CLUT
    }
    for _ in 0..out_chan {
        for i in 0..256u16 {
            lut.push(i as u8);
        }
    } // output curves

    let mut p = vec![0u8; 128];
    let total = 128 + 4 + 12 + lut.len() as u32;
    p[0..4].copy_from_slice(&total.to_be_bytes());
    p[8..12].copy_from_slice(&0x0240_0000u32.to_be_bytes()); // v2.4
    p[12..16].copy_from_slice(b"prtr");
    p[16..20].copy_from_slice(b"CMYK");
    p[20..24].copy_from_slice(b"Lab ");
    p[36..40].copy_from_slice(b"acsp");
    p[68..72].copy_from_slice(&0x0000_F6D6u32.to_be_bytes()); // illum X 0.9642
    p[72..76].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // illum Y 1.0
    p[76..80].copy_from_slice(&0x0000_D32Du32.to_be_bytes()); // illum Z 0.8249
    p.extend_from_slice(&1u32.to_be_bytes()); // tag count = 1
    p.extend_from_slice(&0x4132_4230u32.to_be_bytes()); // 'A2B0'
    p.extend_from_slice(&144u32.to_be_bytes()); // offset
    p.extend_from_slice(&(lut.len() as u32).to_be_bytes()); // size
    p.extend_from_slice(&lut);
    p
}

#[test]
fn bpc_transform_without_b2a_matches_lcms2() {
    // sRGB destination, serialized once so both engines use the identical bytes.
    let srgb_bytes = save_to_mem(&build_srgb_profile()).expect("serialize sRGB");
    let cmyk_bytes = build_cmyk_a2b0_only(135, 2);

    // A spread of CMYK_8 input pixels (incl. the K-only black that triggers BPC).
    let input: Vec<u8> = vec![
        0, 0, 0, 255, // pure K black
        0, 0, 0, 0, // paper white
        255, 0, 0, 0, // C
        0, 255, 0, 0, // M
        0, 0, 255, 0, // Y
        64, 128, 192, 32, // a mix
    ];
    let n = input.len() / 4;

    // --- lcms2 reference: build [CMYK, sRGB] with BPC on, RelCol (NOOPTIMIZE). ---
    let mut oracle_out = vec![0u8; n * 3];
    let built = tintbox_oracle::do_transform_packed(
        &[&cmyk_bytes, &srgb_bytes],
        &[1, 1], // INTENT_RELATIVE_COLORIMETRIC = 1, per link
        &[true, true],
        &[1.0, 1.0],
        TYPE_CMYK_8,
        TYPE_RGB_8,
        &input,
        &mut oracle_out,
        n,
    );
    assert!(
        built,
        "lcms2 builds this BPC transform (A2B0-only CMYK); the differential needs it"
    );

    // --- tintbox: the same transform, BPC on. (Was Err(Range) before the fix.) ---
    let src = Profile::open(&cmyk_bytes).expect("Profile::open(cmyk) succeeds");
    let dst = Profile::open(&srgb_bytes).expect("Profile::open(srgb) succeeds");
    let xform = Transform::new_simple_with_formats(
        &src,
        &dst,
        RenderingIntent::RelativeColorimetric,
        /* bpc */ true,
        TYPE_CMYK_8,
        TYPE_RGB_8,
    )
    .expect("BPC transform must build like lcms2 (un-buildable BP round-trip -> {0,0,0})");

    let mut rcms_out = vec![0u8; n * 3];
    xform.do_transform(&input, &mut rcms_out, n);

    assert_eq!(
        rcms_out, oracle_out,
        "BPC CMYK(A2B0-only)->sRGB must be byte-identical to lcms2"
    );
}
