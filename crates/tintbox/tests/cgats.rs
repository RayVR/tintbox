//! Differential tests for the CGATS / IT8.7 parser + writer (slice 9).
//!
//! For each synthetic IT8 buffer:
//!  1. Parse with tintbox `Profile::load_from_mem` and lcms2 `cmsIT8LoadFromMem`,
//!     then assert every accessor (sheet type, properties, DATA_FORMAT, data by
//!     row/col and by name, doubles, patch names, table count) agrees.
//!  2. Save both through `save_to_mem` / `cmsIT8SaveToMem` and assert the
//!     serialized text is BYTE-IDENTICAL.

use tintbox::cgats::Profile;
use tintbox_oracle::It8;

/// A representative two-table IT8 stream: header properties (string, double,
/// uncooked, hex), a DATA_FORMAT, and BEGIN_DATA..END_DATA with string + double
/// fields. The second table carries a different sheet type and data.
const SAMPLE_MULTI: &str = "\
IT8.7/2\n\
ORIGINATOR\t\"tintbox test suite\"\n\
DESCRIPTOR\t\"Synthetic IT8 with two tables\"\n\
CREATED\t\"2026-06-14\"\n\
MANUFACTURER\t\"Acme\"\n\
PROD_DATE\t\"2026:06\"\n\
SERIAL\t\"00012345\"\n\
KEYWORD\t\"CUSTOM_FIELD\"\n\
CUSTOM_FIELD\t\"hello world\"\n\
NUMBER_OF_FIELDS\t5\n\
NUMBER_OF_SETS\t3\n\
BEGIN_DATA_FORMAT\n\
SAMPLE_ID\tRGB_R\tRGB_G\tRGB_B\tLAB_L\n\
END_DATA_FORMAT\n\
BEGIN_DATA\n\
A1\t0\t0\t0\t0.5\n\
A2\t255\t128.25\t0\t53.125\n\
A3\t12.5\t-3.0\t100\t99.999\n\
END_DATA\n\
IT8.7/2 SECOND\n\
ORIGINATOR\t\"second table\"\n\
NUMBER_OF_FIELDS\t3\n\
NUMBER_OF_SETS\t2\n\
BEGIN_DATA_FORMAT\n\
SAMPLE_ID\tXYZ_X\tXYZ_Y\n\
END_DATA_FORMAT\n\
BEGIN_DATA\n\
P1\t0.9642\t1.0\n\
P2\t0.3576\t0.7152\n\
END_DATA\n";

/// A no-sheet-type single table (first line has >1 word, so IsMyBlock returns a
/// word count >1 -> nosheet path).
const SAMPLE_NOSHEET: &str = "\
NUMBER_OF_FIELDS 4\n\
NUMBER_OF_SETS 2\n\
BEGIN_DATA_FORMAT\n\
SAMPLE_ID CMYK_C CMYK_M CMYK_Y\n\
END_DATA_FORMAT\n\
BEGIN_DATA\n\
S1 0 10.5 20.25\n\
S2 100 99.99 0.001\n\
END_DATA\n";

/// A WEIGHTING_FUNCTION pair (WRITE_PAIR) plus a hex property.
const SAMPLE_PAIRS: &str = "\
CGATS.17\n\
ORIGINATOR\t\"pairs\"\n\
WEIGHTING_FUNCTION\t\"name, D50; value, 1.0\"\n\
NUMBER_OF_FIELDS\t2\n\
NUMBER_OF_SETS\t2\n\
BEGIN_DATA_FORMAT\n\
SAMPLE_ID\tRGB_R\n\
END_DATA_FORMAT\n\
BEGIN_DATA\n\
X1\t0.25\n\
X2\t128\n\
END_DATA\n";

fn assert_parity(label: &str, src: &str) {
    let bytes = src.as_bytes();

    let mut tintbox = Profile::load_from_mem(bytes)
        .unwrap_or_else(|e| panic!("[{label}] tintbox failed to parse: {e:?}"));
    let lcms = It8::load(bytes).unwrap_or_else(|| panic!("[{label}] lcms2 rejected the buffer"));

    // Table count.
    assert_eq!(
        tintbox.table_count(),
        lcms.table_count(),
        "[{label}] table count"
    );

    for t in 0..lcms.table_count() {
        tintbox.set_table(t).expect("tintbox set_table");
        assert_eq!(lcms.set_table(t), t as i32, "[{label}] lcms set_table");

        // Sheet type.
        assert_eq!(
            tintbox.sheet_type(),
            lcms.sheet_type().unwrap_or_default(),
            "[{label}] table {t} sheet type"
        );

        // Property names + values.
        let lcms_props = {
            let n = lcms.num_samples();
            let _ = n;
            lcms_enum_props(&lcms)
        };
        let rcms_props: Vec<String> = tintbox
            .enum_properties()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            rcms_props, lcms_props,
            "[{label}] table {t} property names/order"
        );

        for key in &rcms_props {
            // Comment pseudo-keys have no comparable value via GetProperty.
            if key.starts_with('#') {
                continue;
            }
            assert_eq!(
                tintbox.get_property(key).map(|s| s.to_string()),
                lcms.get_property(key),
                "[{label}] table {t} property '{key}' value"
            );
            assert_eq!(
                tintbox.get_property_dbl(key).to_bits(),
                lcms.get_property_dbl(key).to_bits(),
                "[{label}] table {t} property '{key}' as double"
            );
        }

        // DATA_FORMAT.
        let n_samples = tintbox.num_samples();
        assert_eq!(
            n_samples,
            lcms.num_samples(),
            "[{label}] table {t} nSamples"
        );
        for c in 0..n_samples {
            assert_eq!(
                tintbox.data_format(c).map(|s| s.to_string()),
                lcms.data_format(c),
                "[{label}] table {t} data_format[{c}]"
            );
        }

        // Data by row/col (string + double) over the full matrix.
        let n_patches = tintbox.get_property_dbl("NUMBER_OF_SETS") as i32;
        for r in 0..n_patches {
            for c in 0..n_samples {
                assert_eq!(
                    tintbox.get_data_rowcol(r, c).map(|s| s.to_string()),
                    lcms.get_data_rowcol(r, c),
                    "[{label}] table {t} data[{r}][{c}] string"
                );
                assert_eq!(
                    tintbox.get_data_rowcol_dbl(r, c).to_bits(),
                    lcms.get_data_rowcol_dbl(r, c).to_bits(),
                    "[{label}] table {t} data[{r}][{c}] double"
                );
            }

            // Patch name by index.
            assert_eq!(
                tintbox.patch_name(r).map(|s| s.to_string()),
                lcms.patch_name(r),
                "[{label}] table {t} patch_name[{r}]"
            );
        }

        // Data by patch + sample name (uses SAMPLE_ID + DATA_FORMAT lookup).
        for r in 0..n_patches {
            if let Some(patch) = tintbox.patch_name(r).map(|s| s.to_string()) {
                for c in 0..n_samples {
                    if let Some(sample) = tintbox.data_format(c).map(|s| s.to_string()) {
                        assert_eq!(
                            tintbox
                                .get_data_by_name(&patch, &sample)
                                .map(|s| s.to_string()),
                            lcms.get_data(&patch, &sample),
                            "[{label}] table {t} data['{patch}']['{sample}'] string"
                        );
                        assert_eq!(
                            tintbox.get_data_dbl(&patch, &sample).to_bits(),
                            lcms.get_data_dbl(&patch, &sample).to_bits(),
                            "[{label}] table {t} data['{patch}']['{sample}'] double"
                        );
                    }
                }
            }
        }
    }

    // Byte-exact save.
    let rcms_bytes = tintbox.save_to_mem();
    let lcms_bytes = lcms.save().expect("lcms2 save");
    assert_eq!(
        rcms_bytes,
        lcms_bytes,
        "[{label}] cmsIT8SaveToMem byte-exact mismatch\nrcms:\n{}\nlcms:\n{}",
        String::from_utf8_lossy(&rcms_bytes),
        String::from_utf8_lossy(&lcms_bytes)
    );
}

/// lcms2 property enumeration for the active table.
fn lcms_enum_props(lcms: &It8) -> Vec<String> {
    lcms.enum_properties()
}

#[test]
fn parity_multi_table() {
    assert_parity("multi", SAMPLE_MULTI);
}

#[test]
fn parity_no_sheet() {
    assert_parity("nosheet", SAMPLE_NOSHEET);
}

#[test]
fn parity_pairs() {
    assert_parity("pairs", SAMPLE_PAIRS);
}

#[test]
fn rejects_non_cgats() {
    // Binary garbage / too short: both must reject.
    let junk = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a";
    assert!(Profile::load_from_mem(junk).is_err());
    assert!(It8::load(junk).is_none());
}

/// The `%.10g` formatter must match the C library bit-for-bit. Spot-check the
/// values the round-trip exercises plus a few edge cases.
#[test]
fn format_g_matches_c() {
    // These literals are what lcms2's snprintf("%.10g", x) produces.
    let cases: &[(f64, &str)] = &[
        (0.5, "0.5"),
        (53.125, "53.125"),
        (99.999, "99.999"),
        (0.001, "0.001"),
        (128.25, "128.25"),
        (0.9642, "0.9642"),
        (1.0, "1"),
        (255.0, "255"),
        (-3.0, "-3"),
        (0.0, "0"),
        (1234567890123.0, "1.23456789e+12"),
        (0.00001234, "1.234e-05"),
        (1e20, "1e+20"),
    ];
    for &(x, want) in cases {
        assert_eq!(tintbox::cgats::format_g(x, 10), want, "format_g({x})");
    }
}

/// Randomized fuzz: build IT8 buffers whose data cells are random doubles in the
/// CGATS-relevant range (these go through `%.10g` on both parse and save), then
/// assert load accessors + byte-exact save parity. Surfaces any `%.10g` rounding
/// divergence that the fixed fixtures might miss.
#[test]
fn parity_fuzz_doubles() {
    let mut state: u64 = 0x9e3779b97f4a7c15;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };

    for iter in 0..400 {
        let n_fields = 2 + (next() % 4) as usize; // 2..=5 (incl. SAMPLE_ID)
        let n_sets = 1 + (next() % 4) as usize; // 1..=4

        let mut src = String::from("CGATS.17\n");
        src.push_str("ORIGINATOR\t\"fuzz\"\n");
        src.push_str(&format!("NUMBER_OF_FIELDS\t{n_fields}\n"));
        src.push_str(&format!("NUMBER_OF_SETS\t{n_sets}\n"));
        src.push_str("BEGIN_DATA_FORMAT\n");
        src.push_str("SAMPLE_ID");
        for c in 1..n_fields {
            src.push_str(&format!("\tFIELD_{c}"));
        }
        src.push('\n');
        src.push_str("END_DATA_FORMAT\n");
        src.push_str("BEGIN_DATA\n");
        for r in 0..n_sets {
            src.push_str(&format!("P{r}"));
            for _c in 1..n_fields {
                // Random double: random sign, mantissa, exponent in [-6, 6].
                let bits = next();
                let m = (bits & 0xfffff) as f64 / 1000.0;
                let e = ((bits >> 20) % 13) as i32 - 6;
                let sgn = if bits & (1 << 40) != 0 { -1.0 } else { 1.0 };
                let x = sgn * m * 10f64.powi(e);
                // Emit with enough digits that the parser sees the full value;
                // both stacks re-format it via %.10g identically.
                src.push_str(&format!("\t{x:.17e}"));
            }
            src.push('\n');
        }
        src.push_str("END_DATA\n");

        let label = format!("fuzz#{iter}");
        assert_parity(&label, &src);
    }
}
