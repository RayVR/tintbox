use std::path::PathBuf;

fn main() {
    // Parity preconditions (spec §4). Fail loudly rather than diff a different
    // code path: the fast-floor path assumes little-endian, and matrix parity
    // assumes FLT_EVAL_METHOD==0 (SSE2+/NEON; 32-bit x87 evaluates intermediates
    // in 80-bit extended precision and is unsupported).
    if cfg!(target_endian = "big") {
        panic!("rcms-oracle requires a little-endian host to match lcms2's pinned config");
    }

    let lcms = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/Little-CMS");
    let src = lcms.join("src");

    let mut build = cc::Build::new();
    build
        .include(&src)
        .include(lcms.join("include"))
        .flag_if_supported("-ffp-contract=off")
        .define("CMS_NO_PTHREADS", None)
        .warnings(false);

    for entry in std::fs::read_dir(&src).expect("read lcms2 src") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("c") {
            build.file(path);
        }
    }
    build.file("shim.c");
    build.compile("rcms_oracle_lcms2");
}
