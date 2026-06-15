//! Differential tests for PostScript CSA/CRD generation (`crate::ps`), byte-for-byte
//! against lcms2's `cmsGetPostScriptCSA` / `cmsGetPostScriptCRD`.
//!
//! CSA output is fully deterministic. CRD prepends a wall-clock `ctime()` header
//! unless `cmsFLAGS_NODEFAULTRESOURCEDEF` (`0x0100_0000`) is set, so CRD is tested
//! with that flag for byte-identity (the C path then skips the header + resource-def
//! trailer too).

use rcms::profile::Profile;
use rcms::ps::{get_post_script_crd, get_post_script_csa};

const NODEFAULTRESOURCEDEF: u32 = 0x0100_0000;

/// The testbed profiles (relative to the submodule). A mix of CMYK CLUT (test1/2),
/// 3-channel CLUT (test3/4), Lab (test5), and RGB matrix-shaper (crayons/ibm/new).
const PROFILES: &[&str] = &[
    "test1.icc",
    "test2.icc",
    "test3.icc",
    "test4.icc",
    "test5.icc",
    "crayons.icc",
    "ibm-t61.icc",
    "new.icc",
];

fn load(name: &str) -> Option<Vec<u8>> {
    let path = format!("../../vendor/Little-CMS/testbed/{name}");
    std::fs::read(&path).ok()
}

fn assert_bytes_eq(got: &[u8], want: &[u8], ctx: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "{ctx}: length mismatch rcms={} lcms2={}",
        got.len(),
        want.len()
    );
    if got != want {
        let at = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(0);
        let lo = at.saturating_sub(40);
        let hi = (at + 60).min(got.len());
        panic!(
            "{ctx}: byte mismatch at {at}\n rcms : {:?}\n lcms2: {:?}",
            String::from_utf8_lossy(&got[lo..hi]),
            String::from_utf8_lossy(&want[lo..hi]),
        );
    }
}

#[test]
fn csa_byte_identical() {
    let mut checked = 0;
    for &name in PROFILES {
        let Some(bytes) = load(name) else { continue };
        // Skip profiles rcms can't even open (none expected, but be robust).
        if Profile::open(&bytes).is_err() {
            continue;
        }
        let profile = Profile::open(&bytes).unwrap();

        for intent in 0u32..4 {
            let want = rcms_oracle::get_postscript_csa(&bytes, intent, 0);
            let got = get_post_script_csa(&profile, intent, 0);

            match (got, want) {
                (Ok(g), Some(w)) => {
                    assert_bytes_eq(&g, &w, &format!("CSA {name} intent={intent}"));
                    checked += 1;
                }
                (Err(_), None) => {
                    // Both decline: agreement.
                    checked += 1;
                }
                (Ok(_), None) => panic!("CSA {name} intent={intent}: rcms emitted, lcms2 declined"),
                (Err(e), Some(_)) => {
                    panic!("CSA {name} intent={intent}: rcms declined ({e:?}), lcms2 emitted")
                }
            }
        }
    }
    assert!(checked > 0, "no CSA cases exercised");
}

#[test]
fn crd_byte_identical() {
    let mut checked = 0;
    for &name in PROFILES {
        let Some(bytes) = load(name) else { continue };
        if Profile::open(&bytes).is_err() {
            continue;
        }
        let profile = Profile::open(&bytes).unwrap();

        for intent in 0u32..4 {
            for &extra in &[0u32, 0x2000 /* BPC */] {
                let flags = NODEFAULTRESOURCEDEF | extra;
                let want = rcms_oracle::get_postscript_crd(&bytes, intent, flags);
                let got = get_post_script_crd(&profile, intent, flags);

                match (got, want) {
                    (Ok(g), Some(w)) => {
                        assert_bytes_eq(
                            &g,
                            &w,
                            &format!("CRD {name} intent={intent} flags={flags:#x}"),
                        );
                        checked += 1;
                    }
                    (Err(_), None) => checked += 1,
                    (Ok(_), None) => panic!(
                        "CRD {name} intent={intent} flags={flags:#x}: rcms emitted, lcms2 declined"
                    ),
                    (Err(e), Some(_)) => panic!(
                        "CRD {name} intent={intent} flags={flags:#x}: rcms declined ({e:?}), lcms2 emitted"
                    ),
                }
            }
        }
    }
    assert!(checked > 0, "no CRD cases exercised");
}
