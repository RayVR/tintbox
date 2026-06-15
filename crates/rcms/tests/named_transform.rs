//! Differential tests for the named-color (spot-color) transform path
//! (`cmsnamed.c` + `cmsxform.c`), slice9-named.
//!
//! Two directions are exercised, both bit-exact against lcms2:
//!  1. **Device direction** (single named-color profile): a transform whose input
//!     is `TYPE_NAMED_COLOR_INDEX` maps each color index to the spot color's
//!     device colorants (`EvalNamedColor`). The named profile alone routes through
//!     `_cmsReadDevicelinkLUT` (nProfiles == 1).
//!  2. **PCS direction** (named-color profile -> Lab profile): the index maps to
//!     the spot color's PCS Lab triple (`EvalNamedColorPCS` + `LabV2ToV4`), then
//!     the Lab output profile passes it through. The named profile routes through
//!     `_cmsReadInputLUT`.
//!
//! The named-color profile is hand-built in memory (header with device class
//! `nmcl`, color space CMYK, PCS Lab, plus one `ncl2` tag) so both stacks decode
//! the identical bytes. The Lab4 output profile is lcms2's own
//! `cmsCreateLab4Profile` bytes (via the oracle), which rcms also opens.

use rcms::named::NamedColorList;
use rcms::profile::{Profile, RenderingIntent, Tag};
use rcms::sig::Signature;
use rcms::transform::{Flags, Transform};

const SIG_NCL2: u32 = 0x6E63_6C32; // 'ncl2'
const CLASS_NMCL: u32 = 0x6E6D_636C; // 'nmcl'
const SPACE_CMYK: u32 = 0x434D_594B; // 'CMYK'
const SPACE_LAB: u32 = 0x4C61_6220; // 'Lab '
const RCMS_T4_LAB4: i32 = 3;

/// Build a minimal valid named-color ICC profile carrying one `ncl2` tag. The
/// header declares device class `nmcl`, color space `space`, PCS `Lab `.
fn build_named_profile(space: u32, ncl2_payload: &[u8]) -> Vec<u8> {
    let header_len = 128u32;
    let dir_len = 4 + 12; // count + one 12-byte entry
    let data_off = header_len + dir_len;
    let data_off = (data_off + 3) & !3;
    let total = data_off + ncl2_payload.len() as u32;

    let mut b = vec![0u8; total as usize];
    b[0..4].copy_from_slice(&total.to_be_bytes()); // size
    b[8..12].copy_from_slice(&0x0440_0000u32.to_be_bytes()); // version 4.4
    b[12..16].copy_from_slice(&CLASS_NMCL.to_be_bytes()); // deviceClass 'nmcl'
    b[16..20].copy_from_slice(&space.to_be_bytes()); // colorSpace
    b[20..24].copy_from_slice(&SPACE_LAB.to_be_bytes()); // PCS 'Lab '
    b[36..40].copy_from_slice(b"acsp"); // magic

    b[128..132].copy_from_slice(&1u32.to_be_bytes()); // tag count
    b[132..136].copy_from_slice(&SIG_NCL2.to_be_bytes());
    b[136..140].copy_from_slice(&data_off.to_be_bytes());
    b[140..144].copy_from_slice(&(ncl2_payload.len() as u32).to_be_bytes());
    b[data_off as usize..].copy_from_slice(ncl2_payload);
    b
}

fn type_base(sig: &[u8; 4]) -> Vec<u8> {
    let mut v = sig.to_vec();
    v.extend_from_slice(&[0, 0, 0, 0]);
    v
}

fn name32(s: &str) -> Vec<u8> {
    let mut b = [0u8; 32];
    b[..s.len()].copy_from_slice(s.as_bytes());
    b.to_vec()
}

/// The spot colors shared across the tests: name, PCS Lab triple, 4 CMYK device
/// coords. PCS values span the u16 range so any precision divergence in the
/// `/65535.0` round-trip would surface.
fn spot_colors() -> Vec<(&'static str, [u16; 3], [u16; 4])> {
    vec![
        (
            "Cyan",
            [0x8000, 0x2000, 0x9000],
            [0xFFFF, 0x0000, 0x0000, 0x0000],
        ),
        (
            "Magenta",
            [0x5555, 0xC000, 0x4000],
            [0x0000, 0xFFFF, 0x0000, 0x1234],
        ),
        (
            "Yellow",
            [0xE000, 0x7FFF, 0xD000],
            [0x0000, 0x0000, 0xFFFF, 0xABCD],
        ),
        (
            "Spot1",
            [0x0001, 0xFFFE, 0x8001],
            [0x1111, 0x2222, 0x3333, 0x4444],
        ),
        (
            "Black",
            [0x0000, 0x8000, 0x8000],
            [0x0000, 0x0000, 0x0000, 0xFFFF],
        ),
    ]
}

/// Build the ncl2 tag payload (type base + header + per-color records).
fn build_ncl2_payload() -> Vec<u8> {
    let colors = spot_colors();
    let mut p = type_base(b"ncl2");
    p.extend_from_slice(&0u32.to_be_bytes()); // vendorFlag
    p.extend_from_slice(&(colors.len() as u32).to_be_bytes()); // count
    p.extend_from_slice(&4u32.to_be_bytes()); // nDeviceCoords (CMYK)
    p.extend_from_slice(&name32("pre")); // prefix
    p.extend_from_slice(&name32("suf")); // suffix
    for (nm, pcs, dev) in &colors {
        p.extend_from_slice(&name32(nm));
        for v in pcs {
            p.extend_from_slice(&v.to_be_bytes());
        }
        for v in dev {
            p.extend_from_slice(&v.to_be_bytes());
        }
    }
    p
}

fn read_list(profile: &Profile) -> NamedColorList {
    match profile
        .read_tag(Signature::from_raw(SIG_NCL2))
        .expect("ncl2")
    {
        Tag::NamedColor2(l) => l,
        other => panic!("expected NamedColor2, got {other:?}"),
    }
}

#[test]
fn named_list_accessors_match_oracle() {
    let payload = build_ncl2_payload();
    let bytes = build_named_profile(SPACE_CMYK, &payload);
    assert!(
        rcms_oracle::open_succeeds(&bytes),
        "lcms2 must accept the synthetic named-color profile"
    );

    let oracle = rcms_oracle::read_tag_named_color2(&bytes, SIG_NCL2).expect("oracle ncl2");
    let p = Profile::open(&bytes).expect("rcms open");
    let list = read_list(&p);

    // count / colorant_count / prefix / suffix match lcms2.
    assert_eq!(list.count(), oracle.colors.len());
    assert_eq!(list.colorant_count(), 4);
    assert_eq!(list.prefix, oracle.prefix);
    assert_eq!(list.suffix, oracle.suffix);

    // Per-color name / PCS / device match (cmsNamedColorInfo).
    for (r, o) in list.colors.iter().zip(oracle.colors.iter()) {
        assert_eq!(r.name, o.name, "name");
        assert_eq!(r.pcs, o.pcs, "pcs");
        assert_eq!(r.device, o.device, "device");
    }

    // index() (cmsNamedColorIndex), case-insensitive.
    assert_eq!(list.index("cyan"), Some(0));
    assert_eq!(list.index("BLACK"), Some(4));
    assert_eq!(list.index("nope"), None);
}

#[test]
fn named_device_transform_bit_exact() {
    let payload = build_ncl2_payload();
    let bytes = build_named_profile(SPACE_CMYK, &payload);

    let n_colors = spot_colors().len();
    // Sweep every valid index plus two out-of-range indices (lcms2 zero-fills).
    let indices: Vec<u16> = (0..(n_colors as u16 + 2)).collect();

    // rcms: single named-color profile, TYPE_NAMED_COLOR_INDEX -> 4ch device u16.
    let p = Profile::open(&bytes).expect("rcms open");
    let xform = Transform::new_with_formats(
        &[&p],
        &[RenderingIntent::Perceptual],
        &[false],
        &[1.0],
        Flags::NOOPTIMIZE,
        rcms::format::TYPE_NAMED_COLOR_INDEX,
        u16_format(4),
    )
    .expect("rcms named device xform");

    // cmsGetNamedColorList equivalent must surface the same list.
    let got_list = xform.named_color_list().expect("named color list on xform");
    assert_eq!(got_list.count(), n_colors);

    let mut rcms_out = vec![0u16; indices.len() * 4];
    let in_bytes: Vec<u8> = indices.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let mut out_bytes = vec![0u8; rcms_out.len() * 2];
    xform.do_transform(&in_bytes, &mut out_bytes, indices.len());
    for (i, o) in rcms_out.iter_mut().enumerate() {
        *o = u16::from_ne_bytes([out_bytes[i * 2], out_bytes[i * 2 + 1]]);
    }

    let oracle_out = rcms_oracle::named_transform_16(
        &bytes,
        None,
        RenderingIntent::Perceptual.to_raw(),
        &indices,
        4,
    )
    .expect("oracle named device xform");

    assert_eq!(
        rcms_out, oracle_out,
        "named device colorants must be bit-exact"
    );
}

#[test]
fn named_pcs_transform_bit_exact() {
    let payload = build_ncl2_payload();
    let named_bytes = build_named_profile(SPACE_CMYK, &payload);
    let lab_bytes = rcms_oracle::save_virtual_profile(RCMS_T4_LAB4).expect("lab4 profile bytes");

    let n_colors = spot_colors().len();
    let indices: Vec<u16> = (0..(n_colors as u16 + 2)).collect();

    let pn = Profile::open(&named_bytes).expect("rcms open named");
    let pl = Profile::open(&lab_bytes).expect("rcms open lab4");

    let xform = Transform::new_with_formats(
        &[&pn, &pl],
        &[RenderingIntent::Perceptual, RenderingIntent::Perceptual],
        &[false, false],
        &[1.0, 1.0],
        Flags::NOOPTIMIZE,
        rcms::format::TYPE_NAMED_COLOR_INDEX,
        u16_format(3),
    )
    .expect("rcms named pcs xform");

    let mut rcms_out = vec![0u16; indices.len() * 3];
    let in_bytes: Vec<u8> = indices.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let mut out_bytes = vec![0u8; rcms_out.len() * 2];
    xform.do_transform(&in_bytes, &mut out_bytes, indices.len());
    for (i, o) in rcms_out.iter_mut().enumerate() {
        *o = u16::from_ne_bytes([out_bytes[i * 2], out_bytes[i * 2 + 1]]);
    }

    let oracle_out = rcms_oracle::named_transform_16(
        &named_bytes,
        Some(&lab_bytes),
        RenderingIntent::Perceptual.to_raw(),
        &indices,
        3,
    )
    .expect("oracle named pcs xform");

    assert_eq!(rcms_out, oracle_out, "named PCS Lab must be bit-exact");
}

/// Generic `out_chans`-channel 16-bit pixel format (`COLORSPACE_SH(PT_ANY) |
/// CHANNELS_SH(n) | BYTES_SH(2)`), matching the oracle's `u16_format`.
fn u16_format(n: u32) -> u32 {
    // PT_ANY = 0; the format word is CHANNELS_SH(n) | BYTES_SH(2).
    (n << 3) | 2 // CHANNELS_SH(n) = <<3, BYTES_SH(2) = <<0
}
