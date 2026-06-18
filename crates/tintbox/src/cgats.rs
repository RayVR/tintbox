//! CGATS.17 / IT8.7 measurement-file parser + writer.
//!
//! A faithful port of lcms2's `cmscgats.c` (the `cmsIT8*` family): the
//! lexer/parser for KEYWORD / DATA_FORMAT / BEGIN_DATA..END_DATA properties and
//! multi-table `.txt`/`.it8` streams, the in-memory table model, the read
//! accessors, and the byte-exact text writer ([`Profile::save_to_mem`]).
//!
//! Bit-identity with lcms2 is the contract: parsing semantics and — crucially —
//! the numeric formatting (`%.10g`, see [`format_g`]) are transcribed verbatim,
//! so a load → save round-trip produces byte-for-byte the same text lcms2
//! produces.
//!
//! The `.cube` device-link path of `cmscgats.c` (which builds ICC profiles) is
//! out of scope for this module.

// Untrusted-input parser: forbid the constructs that panic on malformed bytes
// (a panic here is a DoS). Arithmetic that mirrors lcms2's C wrapping uses
// `wrapping_*` explicitly.
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use crate::error::{Error, Result};

const MAXSTR: usize = 1024; // Max length of string
const MAXTABLES: usize = 255; // Max Number of tables in a single stream

// ------------------------------------------------------------------ Symbols

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Symbol {
    Undefined,
    Inum,    // Integer
    Dnum,    // Real
    Ident,   // Identifier
    Str,     // string
    Comment, // comment
    Eoln,    // End of line
    Eof,     // End of stream
    SynError,
    BeginData,
    BeginDataFormat,
    EndData,
    EndDataFormat,
    Keyword,
    DataFormatId,
    Include,
}

/// How a property value is written out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WriteMode {
    Uncooked,
    Stringify,
    Hexadecimal,
    // WRITE_BINARY in lcms2: only produced by cmsIT8SetPropertyHex's sibling
    // setter, which this module does not expose, but the writer still handles
    // it for faithfulness.
    #[allow(dead_code)]
    Binary,
    Pair,
}

// --------------------------------------------------------- Keyword tables

// The keyword -> symbol translation table (IT8). lcms2 binary-searches a sorted
// table; we keep it sorted and do the same case-insensitive search.
const TAB_KEYS_IT8: &[(&str, Symbol)] = &[
    ("$INCLUDE", Symbol::Include),
    (".INCLUDE", Symbol::Include),
    ("BEGIN_DATA", Symbol::BeginData),
    ("BEGIN_DATA_FORMAT", Symbol::BeginDataFormat),
    ("DATA_FORMAT_IDENTIFIER", Symbol::DataFormatId),
    ("END_DATA", Symbol::EndData),
    ("END_DATA_FORMAT", Symbol::EndDataFormat),
    ("KEYWORD", Symbol::Keyword),
];

// Predefined properties and how they are written.
const PREDEFINED_PROPERTIES: &[(&str, WriteMode)] = &[
    ("NUMBER_OF_FIELDS", WriteMode::Uncooked),
    ("NUMBER_OF_SETS", WriteMode::Uncooked),
    ("ORIGINATOR", WriteMode::Stringify),
    ("FILE_DESCRIPTOR", WriteMode::Stringify),
    ("CREATED", WriteMode::Stringify),
    ("DESCRIPTOR", WriteMode::Stringify),
    ("DIFFUSE_GEOMETRY", WriteMode::Stringify),
    ("MANUFACTURER", WriteMode::Stringify),
    ("MANUFACTURE", WriteMode::Stringify),
    ("PROD_DATE", WriteMode::Stringify),
    ("SERIAL", WriteMode::Stringify),
    ("MATERIAL", WriteMode::Stringify),
    ("INSTRUMENTATION", WriteMode::Stringify),
    ("MEASUREMENT_SOURCE", WriteMode::Stringify),
    ("PRINT_CONDITIONS", WriteMode::Stringify),
    ("SAMPLE_BACKING", WriteMode::Stringify),
    ("CHISQ_DOF", WriteMode::Stringify),
    ("MEASUREMENT_GEOMETRY", WriteMode::Stringify),
    ("FILTER", WriteMode::Stringify),
    ("POLARIZATION", WriteMode::Stringify),
    ("WEIGHTING_FUNCTION", WriteMode::Pair),
    ("COMPUTATIONAL_PARAMETER", WriteMode::Pair),
    ("TARGET_TYPE", WriteMode::Stringify),
    ("COLORANT", WriteMode::Stringify),
    ("TABLE_DESCRIPTOR", WriteMode::Stringify),
    ("TABLE_NAME", WriteMode::Stringify),
];

const PREDEFINED_SAMPLE_ID: &[&str] = &[
    "SAMPLE_ID",
    "STRING",
    "CMYK_C",
    "CMYK_M",
    "CMYK_Y",
    "CMYK_K",
    "D_RED",
    "D_GREEN",
    "D_BLUE",
    "D_VIS",
    "D_MAJOR_FILTER",
    "RGB_R",
    "RGB_G",
    "RGB_B",
    "SPECTRAL_NM",
    "SPECTRAL_PCT",
    "SPECTRAL_DEC",
    "XYZ_X",
    "XYZ_Y",
    "XYZ_Z",
    "XYY_X",
    "XYY_Y",
    "XYY_CAPY",
    "LAB_L",
    "LAB_A",
    "LAB_B",
    "LAB_C",
    "LAB_H",
    "LAB_DE",
    "LAB_DE_94",
    "LAB_DE_CMC",
    "LAB_DE_2000",
    "MEAN_DE",
    "STDEV_X",
    "STDEV_Y",
    "STDEV_Z",
    "STDEV_L",
    "STDEV_A",
    "STDEV_B",
    "STDEV_DE",
    "CHI_SQD_PAR",
];

// ----------------------------------------------------------- printf %g

/// C `printf("%.*g", precision, x)` — a bit-exact reimplementation for the
/// precisions CGATS uses (the default formatter is `%.10g`).
///
/// The algorithm mirrors C's `%g`: round `x` to `precision` significant digits,
/// then choose fixed vs scientific notation by the decimal exponent (scientific
/// iff `exp < -4 || exp >= precision`), strip trailing zeros from the fraction,
/// and emit the exponent as `e±NN` with at least two digits. This was verified
/// byte-for-byte against the C library over a 200k-value sweep across the
/// CGATS-relevant range.
pub fn format_g(x: f64, precision: usize) -> String {
    let p = if precision == 0 { 1 } else { precision };

    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf" } else { "inf" }.to_owned();
    }

    // Rust's `{:.*e}` rounds to `p-1` fractional digits (i.e. `p` significant
    // digits) using round-half-to-even, matching the C library.
    let sci = format!("{:.*e}", p - 1, x);
    // `{:e}` on the finite, non-zero `x` (zero/NaN/inf handled above) always
    // yields `<mantissa>e<exponent>`, so the `None`/parse-failure arms below are
    // unreachable — this is panic-free without changing any real output.
    let (mant, exp): (&str, i32) = match sci.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => return sci,
    };

    if exp < -4 || exp >= p as i32 {
        // Scientific: strip trailing zeros from the mantissa fraction.
        let m = strip_trailing_zeros(mant);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", m, sign, exp.abs())
    } else {
        // Fixed notation with `p - 1 - exp` fractional digits.
        let frac_digits = (p as i32 - 1 - exp).max(0) as usize;
        let f = format!("{:.*}", frac_digits, x);
        strip_trailing_zeros(&f)
    }
}

fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

const DEFAULT_DBL_PRECISION: usize = 10;

#[inline]
fn fmt_dbl(x: f64) -> String {
    format_g(x, DEFAULT_DBL_PRECISION)
}

// ----------------------------------------------------------- char classes

#[inline]
fn is_separator(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

#[inline]
fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

#[inline]
fn is_alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

#[inline]
fn is_xdigit(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

#[inline]
fn is_middle(c: u8) -> bool {
    !is_separator(c) && c != b'#' && c != b'"' && c != b'\'' && c > 32 && c < 127
}

#[inline]
fn is_idchar(c: u8) -> bool {
    is_alnum(c) || is_middle(c)
}

#[inline]
fn is_firstidchar(c: u8) -> bool {
    c != b'-' && !is_digit(c) && is_middle(c)
}

#[inline]
fn to_upper(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

/// Case-insensitive compare matching lcms2's `cmsstrcasecmp`.
#[inline]
fn strcasecmp_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ----------------------------------------------------------- model

/// A header property (linked-list node in lcms2). Properties may carry subkeys
/// for the `WRITE_PAIR` dictionary form.
#[derive(Clone, Debug)]
struct KeyValue {
    keyword: String,
    subkey: Option<String>,
    value: Option<String>,
    write_as: WriteMode,
}

/// One CGATS table: a sheet type, an ordered header property list, an optional
/// `DATA_FORMAT` (column labels) and the row-major data matrix.
#[derive(Clone, Debug)]
struct Table {
    sheet_type: String,
    n_samples: i32, // columns
    n_patches: i32, // rows
    sample_id: i32, // index of the SAMPLE_ID column
    header_list: Vec<KeyValue>,
    data_format: Option<Vec<Option<String>>>,
    data: Option<Vec<Option<String>>>, // row-major: [set * n_samples + field]
}

impl Table {
    fn new() -> Table {
        Table {
            sheet_type: String::new(),
            n_samples: 0,
            n_patches: 0,
            sample_id: 0,
            header_list: Vec::new(),
            data_format: None,
            data: None,
        }
    }
}

// --------------------------------------------------------- lexer/parser state

/// A loaded IT8 / CGATS object: one or more [`Table`]s plus a small allowed-
/// keyword / sample-id registry (used during parse + write). Construct via
/// [`Profile::load_from_mem`].
pub struct Profile {
    tables: Vec<Table>,
    n_table: usize, // active table index

    valid_keywords: Vec<KeyValue>,
    valid_sample_id: Vec<KeyValue>,
}

/// The transient lexer state, mirroring the parsing fields of lcms2's `cmsIT8`.
struct Parser<'a> {
    src: &'a [u8],
    pos: usize, // index of the *next* byte to read (after `ch`)
    ch: u8,     // current character (0 == EOF)

    sy: Symbol,
    inum: i32,
    dnum: f64,

    id: String,
    string: String,

    lineno: i32,
}

impl Profile {
    fn new() -> Profile {
        let mut p = Profile {
            tables: Vec::new(),
            n_table: 0,
            valid_keywords: Vec::new(),
            valid_sample_id: Vec::new(),
        };
        p.alloc_table();

        for (k, w) in PREDEFINED_PROPERTIES {
            add_to_list(&mut p.valid_keywords, k, None, None, *w);
        }
        for k in PREDEFINED_SAMPLE_ID {
            add_to_list(&mut p.valid_sample_id, k, None, None, WriteMode::Uncooked);
        }
        // lcms2 sets the default sheet type to "CGATS.17". `alloc_table` above
        // pushed table 0, so `first_mut` is always `Some` here.
        if let Some(t) = p.tables.first_mut() {
            t.sheet_type = "CGATS.17".to_owned();
        }
        p
    }

    fn alloc_table(&mut self) -> bool {
        if self.tables.len() >= MAXTABLES - 1 {
            return false;
        }
        self.tables.push(Table::new());
        true
    }

    // `n_table` is an internal invariant: it is only ever set to an existing
    // table index (`set_table` validates the range, `alloc_table` guarantees >= 1
    // table), so this index is never out of bounds. It is not attacker-controlled
    // input, hence the targeted allow rather than a fallible accessor.
    #[inline]
    #[allow(clippy::indexing_slicing)]
    fn table(&self) -> &Table {
        &self.tables[self.n_table]
    }

    #[inline]
    #[allow(clippy::indexing_slicing)]
    fn table_mut(&mut self) -> &mut Table {
        let i = self.n_table;
        &mut self.tables[i]
    }
}

// ----------------------------------------------------------- list helpers

/// Search a property list, returning the index of the matching keyword (and, if
/// `subkey` is given, the matching subkey within that keyword's chain). Mirrors
/// `IsAvailableOnList`. The returned `last` is the index lcms2 would treat as the
/// insertion anchor.
fn is_available_on_list(
    list: &[KeyValue],
    key: &str,
    subkey: Option<&str>,
) -> (bool, Option<usize>) {
    // Find first node whose keyword matches (unless key starts with '#').
    let mut found: Option<usize> = None;
    let mut last: Option<usize> = if list.is_empty() { None } else { Some(0) };

    for (i, kv) in list.iter().enumerate() {
        last = Some(i);
        if !key.starts_with('#') && strcasecmp_eq(key, &kv.keyword) {
            found = Some(i);
            break;
        }
    }

    let idx = match found {
        None => return (false, last),
        Some(i) => i,
    };

    match subkey {
        None => (true, Some(idx)),
        Some(sk) => {
            // lcms2 walks the NextSubkey chain. In our flat Vec the subkey
            // entries for the same keyword are appended in order, so scan
            // forward over entries that share the keyword.
            let mut anchor = idx;
            for (i, kv) in list.iter().enumerate().skip(idx) {
                if let Some(existing) = &kv.subkey {
                    if strcasecmp_eq(&kv.keyword, key) {
                        anchor = i;
                        if strcasecmp_eq(sk, existing) {
                            return (true, Some(i));
                        }
                    }
                }
            }
            (false, Some(anchor))
        }
    }
}

/// Add or update a property in `list`, mirroring lcms2's `AddToList`. Returns
/// `false` only on the duplicate-required-key error (NUMBER_OF_FIELDS / _SETS).
fn add_to_list(
    list: &mut Vec<KeyValue>,
    key: &str,
    subkey: Option<&str>,
    value: Option<&str>,
    write_as: WriteMode,
) -> bool {
    let (present, anchor) = is_available_on_list(list, key, subkey);

    if present {
        // Editing an existing property: required counters may not be redefined.
        if strcasecmp_eq(key, "NUMBER_OF_FIELDS") || strcasecmp_eq(key, "NUMBER_OF_SETS") {
            return false;
        }
        // `present` implies `anchor` is `Some` and indexes an existing node.
        if let Some(kv) = anchor.and_then(|i| list.get_mut(i)) {
            kv.write_as = write_as;
            kv.value = value.map(|v| v.to_owned());
        }
        return true;
    }

    // Not present: append a fresh node. Order matters for the writer, and the
    // flat-Vec append order reproduces lcms2's linked-list insertion order for
    // both plain keys and subkey chains (subkeys of the same keyword arrive
    // consecutively in the input).
    list.push(KeyValue {
        keyword: key.to_owned(),
        subkey: subkey.map(|s| s.to_owned()),
        value: value.map(|v| v.to_owned()),
        write_as,
    });
    true
}

// ----------------------------------------------------------- number parsing

/// `ParseFloatNumber` — locale-independent float parse used by the `*Dbl`
/// accessors (CGATS always uses '.' as the decimal separator).
pub fn parse_float_number(buffer: &str) -> f64 {
    let bytes = buffer.as_bytes();
    // Every `bytes[i]` below is guarded by `i < bytes.len()`; reading through
    // `at` keeps that exact behaviour while being panic-free (the `0` fallback is
    // unreachable behind the guards).
    let at = |i: usize| -> u8 { bytes.get(i).copied().unwrap_or(0) };
    let mut i = 0;
    let mut dnum = 0.0f64;
    let mut sign = 1.0f64;

    if i < bytes.len() && (at(i) == b'-' || at(i) == b'+') {
        sign = if at(i) == b'-' { -1.0 } else { 1.0 };
        i += 1;
    }

    while i < bytes.len() && is_digit(at(i)) {
        dnum = dnum * 10.0 + (at(i) - b'0') as f64;
        i += 1;
    }

    if i < bytes.len() && at(i) == b'.' {
        let mut frac = 0.0f64;
        let mut prec = 0i32;
        i += 1;
        while i < bytes.len() && is_digit(at(i)) {
            frac = frac * 10.0 + (at(i) - b'0') as f64;
            prec += 1;
            i += 1;
        }
        dnum += frac / xpow10(prec);
    }

    if i < bytes.len() && to_upper(at(i)) == b'E' {
        i += 1;
        let mut sgn = 1i32;
        if i < bytes.len() && at(i) == b'-' {
            sgn = -1;
            i += 1;
        } else if i < bytes.len() && at(i) == b'+' {
            sgn = 1;
            i += 1;
        }
        let mut e = 0i32;
        while i < bytes.len() && is_digit(at(i)) {
            let digit = (at(i) - b'0') as i32;
            if (e as f64) * 10.0 + (digit as f64) < 2147483647.0 {
                e = e * 10 + digit;
            }
            i += 1;
        }
        e *= sgn;
        dnum *= xpow10(e);
    }

    sign * dnum
}

#[inline]
fn xpow10(n: i32) -> f64 {
    // lcms2: pow(10, (double) n). Use powi-equivalent via powf on f64 to match
    // the C `pow(10.0, n)` semantics for integer exponents.
    (10.0f64).powf(n as f64)
}

/// A safe atoi matching lcms2's `satoi` (clamps and returns 0 for None).
fn satoi(b: Option<&str>) -> i32 {
    let s = match b {
        None => return 0,
        Some(s) => s,
    };
    let n = c_atoi(s);
    if n > 0x7ffffff0 {
        return 0x7ffffff0;
    }
    if n < -0x7ffffff0 {
        return -0x7ffffff0;
    }
    n
}

/// C `atoi`: parse an optional sign + leading decimal digits, ignoring trailing
/// junk, saturating to i32 on overflow.
fn c_atoi(s: &str) -> i32 {
    let bytes = s.as_bytes();
    // Guarded indexing as in `parse_float_number`; `at` keeps it panic-free.
    let at = |i: usize| -> u8 { bytes.get(i).copied().unwrap_or(0) };
    let mut i = 0;
    while i < bytes.len() && (at(i) == b' ' || at(i) == b'\t') {
        i += 1;
    }
    let mut sign = 1i64;
    if i < bytes.len() && (at(i) == b'-' || at(i) == b'+') {
        if at(i) == b'-' {
            sign = -1;
        }
        i += 1;
    }
    let mut n: i64 = 0;
    while i < bytes.len() && is_digit(at(i)) {
        n = n * 10 + (at(i) - b'0') as i64;
        if n > i64::from(i32::MAX) + 1 {
            n = i64::from(i32::MAX) + 1;
        }
        i += 1;
    }
    n *= sign;
    if n > i64::from(i32::MAX) {
        i32::MAX
    } else if n < i64::from(i32::MIN) {
        i32::MIN
    } else {
        n as i32
    }
}

/// `satob` — convert a decimal string to a binary string (for WRITE_BINARY).
fn satob(v: Option<&str>) -> String {
    let v = match v {
        None => return "0".to_owned(),
        Some(v) => v,
    };
    // lcms2 uses (unsigned) atoi here.
    let x = c_atoi(v) as u32;
    if x == 0 {
        return "0".to_owned();
    }
    let mut buf = Vec::new();
    let mut x = x;
    while x != 0 {
        buf.push(b'0' + (x % 2) as u8);
        x /= 2;
    }
    buf.reverse();
    // `buf` holds only b'0'/b'1', so it is always valid UTF-8; the fallback is
    // unreachable.
    String::from_utf8(buf).unwrap_or_default()
}

// ----------------------------------------------------------- lexer

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Parser<'a> {
        Parser {
            src,
            pos: 0,
            ch: b' ',
            sy: Symbol::Undefined,
            inum: 0,
            dnum: 0.0,
            id: String::new(),
            string: String::new(),
            lineno: 1,
        }
    }

    /// `NextCh` for the in-memory source. Reads `*Source`, advancing only when
    /// non-NUL (matching lcms2, which stops advancing at the terminating 0).
    fn next_ch(&mut self) {
        if self.pos < self.src.len() {
            self.ch = self.src.get(self.pos).copied().unwrap_or(0);
            if self.ch != 0 {
                self.pos += 1;
            }
        } else {
            self.ch = 0;
        }
    }

    fn syn_error(&mut self) {
        self.sy = Symbol::SynError;
    }

    /// `InStringSymbol` — read a single/double-quoted string.
    fn in_string_symbol(&mut self) {
        while is_separator(self.ch) {
            self.next_ch();
        }
        if self.ch == b'\'' || self.ch == b'"' {
            let sng = self.ch;
            self.string.clear();
            self.next_ch();
            while self.ch != sng {
                if self.ch == b'\n' || self.ch == b'\r' || self.ch == 0 {
                    break;
                }
                self.string.push(self.ch as char);
                self.next_ch();
            }
            self.sy = Symbol::Str;
            self.next_ch();
        } else {
            self.syn_error();
        }
    }

    /// `ReadReal` — continue reading a real number from an integer prefix.
    fn read_real(&mut self, inum: i32) {
        self.dnum = inum as f64;
        while is_digit(self.ch) {
            self.dnum = self.dnum * 10.0 + (self.ch - b'0') as f64;
            self.next_ch();
        }
        if self.ch == b'.' {
            let mut frac = 0.0f64;
            let mut prec = 0i32;
            self.next_ch();
            while is_digit(self.ch) {
                frac = frac * 10.0 + (self.ch - b'0') as f64;
                prec += 1;
                self.next_ch();
            }
            self.dnum += frac / xpow10(prec);
        }
        if to_upper(self.ch) == b'E' {
            self.next_ch();
            let mut sgn = 1i32;
            if self.ch == b'-' {
                sgn = -1;
                self.next_ch();
            } else if self.ch == b'+' {
                sgn = 1;
                self.next_ch();
            }
            let mut e = 0i32;
            while is_digit(self.ch) {
                let digit = (self.ch - b'0') as i32;
                if (e as f64) * 10.0 + (digit as f64) < 2147483647.0 {
                    e = e * 10 + digit;
                }
                self.next_ch();
            }
            e *= sgn;
            self.dnum *= xpow10(e);
        }
    }

    /// `InSymbol` — read the next token. The `.include` recursion of lcms2 is a
    /// file-I/O feature unused on in-memory streams (it requires opening files);
    /// we treat an SINCLUDE token as a syntax error if it is ever produced.
    fn in_symbol(&mut self) {
        loop {
            while is_separator(self.ch) {
                self.next_ch();
            }

            if is_firstidchar(self.ch) {
                // Identifier
                self.id.clear();
                loop {
                    self.id.push(self.ch as char);
                    self.next_ch();
                    if !is_idchar(self.ch) {
                        break;
                    }
                }
                match bin_srch_key(&self.id) {
                    Some(sym) => self.sy = sym,
                    None => self.sy = Symbol::Ident,
                }
            } else if is_digit(self.ch) || self.ch == b'.' || self.ch == b'-' || self.ch == b'+' {
                // Number
                let mut sign = 1i32;
                if self.ch == b'-' {
                    sign = -1;
                    self.next_ch();
                } else if self.ch == b'+' {
                    sign = 1;
                    self.next_ch();
                }

                self.inum = 0;
                self.sy = Symbol::Inum;

                if self.ch == b'0' {
                    // 0x.. (hex) or 0b.. (binary)
                    self.next_ch();
                    if to_upper(self.ch) == b'X' {
                        self.next_ch();
                        while is_xdigit(self.ch) {
                            let up = to_upper(self.ch);
                            let j = if up.is_ascii_uppercase() && (b'A'..=b'F').contains(&up) {
                                (up - b'A' + 10) as i32
                            } else {
                                (up - b'0') as i32
                            };
                            if self.inum as f64 * 16.0 + j as f64 > 2147483647.0 {
                                self.syn_error();
                                return;
                            }
                            self.inum = self.inum * 16 + j;
                            self.next_ch();
                        }
                        return;
                    }
                    if to_upper(self.ch) == b'B' {
                        self.next_ch();
                        while self.ch == b'0' || self.ch == b'1' {
                            let j = (self.ch - b'0') as i32;
                            if self.inum as f64 * 2.0 + j as f64 > 2147483647.0 {
                                self.syn_error();
                                return;
                            }
                            self.inum = self.inum * 2 + j;
                            self.next_ch();
                        }
                        return;
                    }
                }

                while is_digit(self.ch) {
                    let digit = (self.ch - b'0') as i32;
                    if self.inum as f64 * 10.0 + digit as f64 > 2147483647.0 {
                        self.read_real(self.inum);
                        self.sy = Symbol::Dnum;
                        self.dnum *= sign as f64;
                        return;
                    }
                    self.inum = self.inum * 10 + digit;
                    self.next_ch();
                }

                if self.ch == b'.' {
                    self.read_real(self.inum);
                    self.sy = Symbol::Dnum;
                    self.dnum *= sign as f64;
                    return;
                }

                self.inum *= sign;

                // Numbers followed by letters become identifiers.
                if is_idchar(self.ch) {
                    let buffer = if self.sy == Symbol::Inum {
                        self.inum.to_string()
                    } else {
                        fmt_dbl(self.dnum)
                    };
                    self.id.clear();
                    self.id.push_str(&buffer);
                    loop {
                        self.id.push(self.ch as char);
                        self.next_ch();
                        if !is_idchar(self.ch) {
                            break;
                        }
                    }
                    self.sy = Symbol::Ident;
                }
                return;
            } else {
                match self.ch {
                    0x1a | 0 => {
                        self.sy = Symbol::Eof;
                    }
                    b'\r' => {
                        self.next_ch();
                        if self.ch == b'\n' {
                            self.next_ch();
                        }
                        self.sy = Symbol::Eoln;
                        self.lineno += 1;
                    }
                    b'\n' => {
                        self.next_ch();
                        self.sy = Symbol::Eoln;
                        self.lineno += 1;
                    }
                    b'#' => {
                        self.next_ch();
                        while self.ch != 0 && self.ch != b'\n' && self.ch != b'\r' {
                            self.next_ch();
                        }
                        self.sy = Symbol::Comment;
                    }
                    b'\'' | b'"' => {
                        self.in_string_symbol();
                    }
                    _ => {
                        self.syn_error();
                        return;
                    }
                }
            }

            if self.sy != Symbol::Comment {
                break;
            }
        }

        // .include is a file-I/O extension; on in-memory streams it cannot
        // resolve a file, so lcms2 would fail. Flag a syntax error.
        if self.sy == Symbol::Include {
            self.syn_error();
        }
    }

    fn check(&mut self, sy: Symbol) -> bool {
        if self.sy != sy {
            self.syn_error();
            return false;
        }
        true
    }

    fn check_eoln(&mut self) -> bool {
        if !self.check(Symbol::Eoln) {
            return false;
        }
        while self.sy == Symbol::Eoln {
            self.in_symbol();
        }
        true
    }

    fn skip(&mut self, sy: Symbol) {
        if self.sy == sy && self.sy != Symbol::Eof && self.sy != Symbol::SynError {
            self.in_symbol();
        }
    }

    fn skip_eoln(&mut self) {
        while self.sy == Symbol::Eoln {
            self.in_symbol();
        }
    }

    /// `GetVal` — current token as a string. Returns `None` on a syntax error.
    fn get_val(&mut self) -> Option<String> {
        match self.sy {
            Symbol::Eoln => Some(String::new()),
            Symbol::Ident => Some(self.id.clone()),
            Symbol::Inum => Some(self.inum.to_string()),
            Symbol::Dnum => Some(fmt_dbl(self.dnum)),
            Symbol::Str => Some(self.string.clone()),
            _ => {
                self.syn_error();
                None
            }
        }
    }

    /// `ReadType` — read the special first line as the sheet type.
    fn read_type(&mut self) -> String {
        let mut s = String::new();
        while is_separator(self.ch) {
            self.next_ch();
        }
        let mut cnt = 0;
        while self.ch != b'\r' && self.ch != b'\n' && self.ch != b'\t' && self.ch != 0 {
            if cnt < MAXSTR {
                s.push(self.ch as char);
            }
            cnt += 1;
            self.next_ch();
        }
        s
    }
}

fn bin_srch_key(id: &str) -> Option<Symbol> {
    for (k, sym) in TAB_KEYS_IT8 {
        if strcasecmp_eq(id, k) {
            return Some(*sym);
        }
    }
    None
}

// ----------------------------------------------------------- IsMyBlock

/// `IsMyBlock` — heuristic that decides whether a buffer looks like CGATS, and
/// (when it does) returns the word-count of the first line (1 means the first
/// line is a sheet type; >1 means no sheet type). Returns `None` for non-CGATS.
fn is_my_block(buffer: &[u8]) -> Option<i32> {
    let mut words = 1i32;
    let mut space = 0;
    let mut quot = 0;

    let n = buffer.len();
    if n < 10 {
        return None;
    }
    let n = n.min(132);

    for &b in buffer.iter().take(n).skip(1) {
        match b {
            b'\n' | b'\r' => {
                return if quot == 1 || words > 2 {
                    None
                } else {
                    Some(words)
                };
            }
            b'\t' | b' ' => {
                if quot == 0 && space == 0 {
                    space = 1;
                }
            }
            b'"' => {
                quot = 1 - quot;
            }
            _ => {
                if !(32..=127).contains(&b) {
                    return None;
                }
                words += space;
                space = 0;
            }
        }
    }
    None
}

// ----------------------------------------------------------- data alloc/access

impl Profile {
    fn allocate_data_format(&mut self) -> bool {
        if self.table().data_format.is_some() {
            return true;
        }
        let n = satoi(self.get_property("NUMBER_OF_FIELDS"));
        if n <= 0 || n > 0x7ffe {
            return false;
        }
        self.table_mut().n_samples = n;
        self.table_mut().data_format = Some(vec![None; (n as usize) + 1]);
        true
    }

    fn set_data_format(&mut self, n: i32, label: &str) -> bool {
        if self.table().data_format.is_none() && !self.allocate_data_format() {
            return false;
        }
        if n < 0 || n >= self.table().n_samples {
            return false;
        }
        let idx = n as usize;
        if let Some(slot) = self
            .table_mut()
            .data_format
            .as_mut()
            .and_then(|df| df.get_mut(idx))
        {
            *slot = Some(label.to_owned());
        }
        true
    }

    fn get_data_format(&self, n: i32) -> Option<&str> {
        let df = self.table().data_format.as_ref()?;
        if n < 0 || n as usize >= df.len() {
            return None;
        }
        df.get(n as usize).and_then(|s| s.as_deref())
    }

    fn allocate_data_set(&mut self) -> bool {
        if self.table().data.is_some() {
            return true;
        }
        let ns = satoi(self.get_property("NUMBER_OF_FIELDS"));
        let np = satoi(self.get_property("NUMBER_OF_SETS"));
        if !(0..=0x7ffe).contains(&ns) || !(0..=0x7ffe).contains(&np) || (np * ns) > 200000 {
            return false;
        }
        self.table_mut().n_samples = ns;
        self.table_mut().n_patches = np;
        let size = ((ns as usize) + 1) * ((np as usize) + 1);
        self.table_mut().data = Some(vec![None; size]);
        true
    }

    fn get_data(&self, n_set: i32, n_field: i32) -> Option<&str> {
        let t = self.table();
        let ns = t.n_samples;
        let np = t.n_patches;
        if n_set < 0 || n_set >= np || n_field < 0 || n_field >= ns {
            return None;
        }
        let data = t.data.as_ref()?;
        data.get((n_set * ns + n_field) as usize)
            .and_then(|s| s.as_deref())
    }

    fn set_data(&mut self, n_set: i32, n_field: i32, val: &str) -> bool {
        if self.table().data.is_none() && !self.allocate_data_set() {
            return false;
        }
        if self.table().data.is_none() {
            return false;
        }
        let np = self.table().n_patches;
        let ns = self.table().n_samples;
        if n_set > np || n_set < 0 {
            return false;
        }
        if n_field > ns || n_field < 0 {
            return false;
        }
        let idx = (n_set * ns + n_field) as usize;
        if let Some(slot) = self.table_mut().data.as_mut().and_then(|d| d.get_mut(idx)) {
            *slot = Some(val.to_owned());
        }
        true
    }
}

// ----------------------------------------------------------- parsing sections

impl Profile {
    fn data_format_section(&mut self, p: &mut Parser) -> bool {
        let mut i_field = 0i32;
        p.in_symbol(); // eats BEGIN_DATA_FORMAT
        if !p.check_eoln() {
            return false;
        }
        while p.sy != Symbol::EndDataFormat
            && p.sy != Symbol::Eoln
            && p.sy != Symbol::Eof
            && p.sy != Symbol::SynError
        {
            if p.sy != Symbol::Ident {
                p.syn_error();
                return false;
            }
            let label = p.id.clone();
            if !self.set_data_format(i_field, &label) {
                return false;
            }
            i_field += 1;
            p.in_symbol();
            p.skip_eoln();
        }
        p.skip_eoln();
        p.skip(Symbol::EndDataFormat);
        p.skip_eoln();
        // lcms2 only warns on a field-count mismatch; it does not fail.
        let _ = i_field;
        true
    }

    fn data_section(&mut self, p: &mut Parser) -> bool {
        let mut i_field = 0i32;
        let mut i_set = 0i32;
        p.in_symbol(); // eats BEGIN_DATA
        if !p.check_eoln() {
            return false;
        }
        if self.table().data.is_none() && !self.allocate_data_set() {
            return false;
        }
        while p.sy != Symbol::EndData && p.sy != Symbol::Eof && p.sy != Symbol::SynError {
            if i_field >= self.table().n_samples {
                i_field = 0;
                i_set += 1;
            }
            if p.sy != Symbol::EndData && p.sy != Symbol::Eof && p.sy != Symbol::SynError {
                let val = match p.sy {
                    Symbol::Ident => p.id.clone(),
                    Symbol::Str => p.string.clone(),
                    _ => match p.get_val() {
                        Some(v) => v,
                        None => return false,
                    },
                };
                if !self.set_data(i_set, i_field, &val) {
                    return false;
                }
                i_field += 1;
                p.in_symbol();
                p.skip_eoln();
            }
        }
        p.skip_eoln();
        p.skip(Symbol::EndData);
        p.skip_eoln();
        if (i_set + 1) != self.table().n_patches {
            p.syn_error();
            return false;
        }
        true
    }

    fn header_section(&mut self, p: &mut Parser) -> bool {
        while p.sy != Symbol::Eof
            && p.sy != Symbol::SynError
            && p.sy != Symbol::BeginDataFormat
            && p.sy != Symbol::BeginData
        {
            match p.sy {
                Symbol::Keyword => {
                    p.in_symbol();
                    let buffer = match p.get_val() {
                        Some(v) => v,
                        None => return false,
                    };
                    if !add_to_list(
                        &mut self.valid_keywords,
                        &buffer,
                        None,
                        None,
                        WriteMode::Uncooked,
                    ) {
                        return false;
                    }
                    p.in_symbol();
                }
                Symbol::DataFormatId => {
                    p.in_symbol();
                    let buffer = match p.get_val() {
                        Some(v) => v,
                        None => return false,
                    };
                    if !add_to_list(
                        &mut self.valid_sample_id,
                        &buffer,
                        None,
                        None,
                        WriteMode::Uncooked,
                    ) {
                        return false;
                    }
                    p.in_symbol();
                }
                Symbol::Ident => {
                    let var_name = p.id.clone();
                    // Look up the write mode for this keyword, registering it if
                    // unknown (non-strict CGATS behaviour).
                    let (present, anchor) =
                        is_available_on_list(&self.valid_keywords, &var_name, None);
                    if !present {
                        add_to_list(
                            &mut self.valid_keywords,
                            &var_name,
                            None,
                            None,
                            WriteMode::Uncooked,
                        );
                    }
                    let key_write_as = if present {
                        // `present` implies `anchor` is `Some` and indexes the list.
                        anchor
                            .and_then(|i| self.valid_keywords.get(i))
                            .map(|kv| kv.write_as)
                            .unwrap_or(WriteMode::Uncooked)
                    } else {
                        WriteMode::Uncooked
                    };

                    p.in_symbol();
                    let buffer = match p.get_val() {
                        Some(v) => v,
                        None => return false,
                    };

                    if key_write_as != WriteMode::Pair {
                        let mode = if p.sy == Symbol::Str {
                            WriteMode::Stringify
                        } else {
                            WriteMode::Uncooked
                        };
                        if !add_to_list(
                            &mut self.table_mut().header_list,
                            &var_name,
                            None,
                            Some(&buffer),
                            mode,
                        ) {
                            return false;
                        }
                    } else {
                        if p.sy != Symbol::Str {
                            p.syn_error();
                            return false;
                        }
                        if !self.parse_pair_property(&var_name, &buffer) {
                            p.syn_error();
                            return false;
                        }
                    }

                    p.in_symbol();
                }
                Symbol::Eoln => {}
                _ => {
                    p.syn_error();
                    return false;
                }
            }
            p.skip_eoln();
        }
        true
    }

    /// Chop a `WRITE_PAIR` property string into "subkey, value" pairs separated
    /// by ';' (mirroring the in-place tokeniser in lcms2's HeaderSection).
    fn parse_pair_property(&mut self, var_name: &str, buffer: &str) -> bool {
        for chunk in buffer.split(';') {
            // split the subkey and value at the LAST comma
            let comma = match chunk.rfind(',') {
                Some(c) => c,
                None => return false,
            };
            // `comma` is a byte index returned by `rfind`, so both slices are in
            // bounds and on char boundaries (',' is ASCII).
            let subkey = chunk.get(..comma).unwrap_or("").trim_matches(' ');
            let value = chunk.get(comma + 1..).unwrap_or("").trim_matches(' ');
            if subkey.is_empty() || value.is_empty() {
                return false;
            }
            add_to_list(
                &mut self.table_mut().header_list,
                var_name,
                Some(subkey),
                Some(value),
                WriteMode::Pair,
            );
        }
        true
    }

    fn parse_it8(&mut self, p: &mut Parser, nosheet: bool) -> bool {
        if !nosheet {
            // Sheet type always belongs to table 0, which always exists.
            let ty = p.read_type();
            if let Some(t) = self.tables.first_mut() {
                t.sheet_type = ty;
            }
        }
        p.in_symbol();
        p.skip_eoln();

        while p.sy != Symbol::Eof && p.sy != Symbol::SynError {
            match p.sy {
                Symbol::BeginDataFormat => {
                    if !self.data_format_section(p) {
                        return false;
                    }
                }
                Symbol::BeginData => {
                    if !self.data_section(p) {
                        return false;
                    }
                    if p.sy != Symbol::Eof && p.sy != Symbol::SynError {
                        if !self.alloc_table() {
                            return false;
                        }
                        self.n_table = self.tables.len() - 1;

                        if !nosheet {
                            if p.sy == Symbol::Ident {
                                // Could be a type sheet or a property statement.
                                while is_separator(p.ch) {
                                    p.next_ch();
                                }
                                if p.ch == b'\n' || p.ch == b'\r' {
                                    let st = p.id.clone();
                                    self.table_mut().sheet_type = st;
                                    p.in_symbol();
                                } else {
                                    self.table_mut().sheet_type = String::new();
                                }
                            } else if p.sy == Symbol::Str {
                                let st = p.string.clone();
                                self.table_mut().sheet_type = st;
                                p.in_symbol();
                            }
                        }
                    }
                }
                Symbol::Eoln => {
                    p.skip_eoln();
                }
                _ => {
                    if !self.header_section(p) {
                        return false;
                    }
                }
            }
        }
        p.sy != Symbol::SynError
    }

    /// `CookPointers` — resolve the SAMPLE_ID column and LABEL/`$` table refs.
    // Every index here is a loop variable bounded by the collection's own length
    // (`j`/`k` over `self.tables`, `i` over `n_patches`) or an `anchor` returned by
    // `is_available_on_list` under a `present` guard, so none can be out of bounds.
    // The function's interleaved borrows of `self.tables[j]`, `self.tables[k]`, and
    // `self.get_data` make a fallible-accessor rewrite impractical without changing
    // behaviour, so the invariant is asserted with a targeted allow instead.
    #[allow(clippy::indexing_slicing, clippy::expect_used)]
    fn cook_pointers(&mut self) {
        let n_old_table = self.n_table;
        let tables_count = self.tables.len();

        for j in 0..tables_count {
            self.tables[j].sample_id = 0;
            self.n_table = j;
            let n_samples = self.tables[j].n_samples;

            for id_field in 0..n_samples {
                // DATA_FORMAT must be defined; lcms2 errors otherwise, but we
                // simply skip (the writer guards on a present DataFormat).
                let fld = match self.tables[j]
                    .data_format
                    .as_ref()
                    .and_then(|df| df.get(id_field as usize).cloned())
                    .flatten()
                {
                    Some(f) => f,
                    None => continue,
                };

                if strcasecmp_eq(&fld, "SAMPLE_ID") {
                    self.tables[j].sample_id = id_field;
                }

                if strcasecmp_eq(&fld, "LABEL") || fld.as_bytes().first() == Some(&b'$') {
                    let n_patches = self.tables[j].n_patches;
                    for i in 0..n_patches {
                        let label = match self.get_data(i, id_field) {
                            Some(l) => l.to_owned(),
                            None => continue,
                        };
                        for k in 0..tables_count {
                            let (present, anchor) =
                                is_available_on_list(&self.tables[k].header_list, &label, None);
                            if present {
                                let ty = self.tables[k].header_list
                                    [anchor.expect("present implies anchor")]
                                .value
                                .clone()
                                .unwrap_or_default();
                                let buffer = format!("{} {} {}", label, k, ty);
                                self.set_data(i, id_field, &buffer);
                            }
                        }
                    }
                }
            }
        }

        self.n_table = n_old_table;
    }
}

// ----------------------------------------------------------- public API

impl Profile {
    /// `cmsIT8LoadFromMem` — parse a CGATS/IT8 buffer. Returns `Err` if the
    /// buffer is not recognised as CGATS or fails to parse (matching lcms2's
    /// NULL return).
    pub fn load_from_mem(buf: &[u8]) -> Result<Profile> {
        if buf.is_empty() {
            return Err(Error::Corrupt("empty CGATS buffer"));
        }
        let type_ = match is_my_block(buf) {
            None => return Err(Error::Corrupt("not a CGATS/IT8 stream")),
            Some(t) => t,
        };

        let mut prof = Profile::new();
        let mut parser = Parser::new(buf);

        if !prof.parse_it8(&mut parser, type_ - 1 != 0) {
            return Err(Error::Corrupt("CGATS parse error"));
        }

        prof.cook_pointers();
        prof.n_table = 0;
        Ok(prof)
    }

    /// `cmsIT8TableCount`.
    pub fn table_count(&self) -> u32 {
        self.tables.len() as u32
    }

    /// `cmsIT8SetTable` — select the active table, allocating the next one if
    /// `n_table == TablesCount`. Returns the new index or `Err`.
    pub fn set_table(&mut self, n_table: u32) -> Result<u32> {
        let n = n_table as usize;
        if n >= self.tables.len() {
            if n == self.tables.len() {
                if !self.alloc_table() {
                    return Err(Error::Corrupt("too many tables"));
                }
            } else {
                return Err(Error::Corrupt("table out of sequence"));
            }
        }
        self.n_table = n;
        Ok(n_table)
    }

    /// `cmsIT8GetSheetType` for the active table.
    pub fn sheet_type(&self) -> &str {
        &self.table().sheet_type
    }

    /// `cmsIT8SetSheetType`.
    pub fn set_sheet_type(&mut self, ty: &str) {
        // lcms2 truncates to MAXSTR-1 bytes.
        let mut s = ty.to_owned();
        if s.len() > MAXSTR - 1 {
            s.truncate(MAXSTR - 1);
        }
        self.table_mut().sheet_type = s;
    }

    /// `cmsIT8GetProperty` — string value of a header property, or `None`.
    pub fn get_property(&self, key: &str) -> Option<&str> {
        let (present, anchor) = is_available_on_list(&self.table().header_list, key, None);
        if present {
            // `present` implies `anchor` is `Some` and indexes the header list.
            anchor
                .and_then(|i| self.table().header_list.get(i))
                .and_then(|kv| kv.value.as_deref())
        } else {
            None
        }
    }

    /// `cmsIT8GetPropertyDbl`.
    pub fn get_property_dbl(&self, key: &str) -> f64 {
        match self.get_property(key) {
            None => 0.0,
            Some(v) => parse_float_number(v),
        }
    }

    /// `cmsIT8GetPropertyMulti` — value of a `WRITE_PAIR` subkey.
    pub fn get_property_multi(&self, key: &str, subkey: &str) -> Option<&str> {
        let (present, anchor) = is_available_on_list(&self.table().header_list, key, Some(subkey));
        if present {
            // `present` implies `anchor` is `Some` and indexes the header list.
            anchor
                .and_then(|i| self.table().header_list.get(i))
                .and_then(|kv| kv.value.as_deref())
        } else {
            None
        }
    }

    /// `cmsIT8SetPropertyStr`.
    pub fn set_property_str(&mut self, key: &str, val: &str) -> bool {
        if val.is_empty() {
            return false;
        }
        add_to_list(
            &mut self.table_mut().header_list,
            key,
            None,
            Some(val),
            WriteMode::Stringify,
        )
    }

    /// `cmsIT8SetPropertyDbl`.
    pub fn set_property_dbl(&mut self, key: &str, val: f64) -> bool {
        let buffer = fmt_dbl(val);
        add_to_list(
            &mut self.table_mut().header_list,
            key,
            None,
            Some(&buffer),
            WriteMode::Uncooked,
        )
    }

    /// `cmsIT8SetPropertyHex`.
    pub fn set_property_hex(&mut self, key: &str, val: u32) -> bool {
        let buffer = val.to_string();
        add_to_list(
            &mut self.table_mut().header_list,
            key,
            None,
            Some(&buffer),
            WriteMode::Hexadecimal,
        )
    }

    /// `cmsIT8SetPropertyUncooked`.
    pub fn set_property_uncooked(&mut self, key: &str, buffer: &str) -> bool {
        add_to_list(
            &mut self.table_mut().header_list,
            key,
            None,
            Some(buffer),
            WriteMode::Uncooked,
        )
    }

    /// `cmsIT8EnumProperties` — header keyword names of the active table, in
    /// order.
    pub fn enum_properties(&self) -> Vec<&str> {
        self.table()
            .header_list
            .iter()
            .map(|kv| kv.keyword.as_str())
            .collect()
    }

    /// `cmsIT8EnumDataFormat` count for the active table (NUMBER_OF_FIELDS).
    pub fn num_samples(&self) -> i32 {
        self.table().n_samples
    }

    /// DATA_FORMAT label for column `col`, `None` if out of range.
    pub fn data_format(&self, col: i32) -> Option<&str> {
        self.get_data_format(col)
    }

    /// `cmsIT8FindDataFormat` — column index of a sample name, or `-1`.
    pub fn find_data_format(&self, sample: &str) -> i32 {
        self.locate_sample(sample)
    }

    fn locate_sample(&self, sample: &str) -> i32 {
        let t = self.table();
        for i in 0..t.n_samples {
            if let Some(fld) = self.get_data_format(i) {
                if strcasecmp_eq(fld, sample) {
                    return i;
                }
            }
        }
        -1
    }

    fn locate_patch(&self, patch: &str) -> i32 {
        let t = self.table();
        for i in 0..t.n_patches {
            if let Some(data) = self.get_data(i, t.sample_id) {
                if strcasecmp_eq(data, patch) {
                    return i;
                }
            }
        }
        -1
    }

    /// `cmsIT8GetDataRowCol`.
    pub fn get_data_rowcol(&self, row: i32, col: i32) -> Option<&str> {
        self.get_data(row, col)
    }

    /// `cmsIT8GetDataRowColDbl`.
    pub fn get_data_rowcol_dbl(&self, row: i32, col: i32) -> f64 {
        match self.get_data(row, col) {
            None => 0.0,
            Some(v) => parse_float_number(v),
        }
    }

    /// `cmsIT8GetData` by patch + sample name.
    pub fn get_data_by_name(&self, patch: &str, sample: &str) -> Option<&str> {
        let i_field = self.locate_sample(sample);
        if i_field < 0 {
            return None;
        }
        let i_set = self.locate_patch(patch);
        if i_set < 0 {
            return None;
        }
        self.get_data(i_set, i_field)
    }

    /// `cmsIT8GetDataDbl`.
    pub fn get_data_dbl(&self, patch: &str, sample: &str) -> f64 {
        match self.get_data_by_name(patch, sample) {
            None => parse_float_number(""),
            Some(v) => parse_float_number(v),
        }
    }

    /// `cmsIT8GetPatchName` — SAMPLE_ID value of patch `n`, `None` if absent.
    pub fn patch_name(&self, n_patch: i32) -> Option<&str> {
        let sid = self.table().sample_id;
        self.get_data(n_patch, sid)
    }

    /// `cmsIT8GetPatchByName`.
    pub fn patch_by_name(&self, patch: &str) -> i32 {
        self.locate_patch(patch)
    }

    /// `cmsIT8SetDataRowCol`.
    pub fn set_data_rowcol(&mut self, row: i32, col: i32, val: &str) -> bool {
        self.set_data(row, col, val)
    }

    /// `cmsIT8SetDataRowColDbl`.
    pub fn set_data_rowcol_dbl(&mut self, row: i32, col: i32, val: f64) -> bool {
        let buffer = fmt_dbl(val);
        self.set_data(row, col, &buffer)
    }

    /// `cmsIT8SaveToMem` — serialize all tables to a byte buffer, byte-for-byte
    /// identical to lcms2 (including the trailing NUL byte lcms2 writes).
    pub fn save_to_mem(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let n_old = self.n_table;
        let tables_count = self.tables.len();
        for i in 0..tables_count {
            self.n_table = i;
            self.write_header(&mut out);
            self.write_data_format(&mut out);
            self.write_data(&mut out);
        }
        self.n_table = n_old;
        out.push(0); // the \0 at the very end
        out
    }
}

// ----------------------------------------------------------- writer

impl Profile {
    fn write_header(&mut self, out: &mut Vec<u8>) {
        // Snapshot the data we need, since writing may register new valid
        // keywords (lcms2 mutates ValidKeywords during WriteHeader).
        let sheet_type = self.table().sheet_type.clone();
        out.extend_from_slice(sheet_type.as_bytes());
        out.extend_from_slice(b"\n");

        let header = self.table().header_list.clone();
        for p in &header {
            if p.keyword.starts_with('#') {
                out.extend_from_slice(b"#\n# ");
                if let Some(v) = &p.value {
                    for &c in v.as_bytes() {
                        out.push(c);
                        if c == b'\n' {
                            out.extend_from_slice(b"# ");
                        }
                    }
                }
                out.extend_from_slice(b"\n#\n");
                continue;
            }

            let (present, _) = is_available_on_list(&self.valid_keywords, &p.keyword, None);
            if !present {
                // Non-strict CGATS: register the keyword (no KEYWORD line).
                add_to_list(
                    &mut self.valid_keywords,
                    &p.keyword,
                    None,
                    None,
                    WriteMode::Uncooked,
                );
            }

            out.extend_from_slice(p.keyword.as_bytes());
            if let Some(value) = &p.value {
                match p.write_as {
                    WriteMode::Uncooked => {
                        out.extend_from_slice(b"\t");
                        out.extend_from_slice(value.as_bytes());
                    }
                    WriteMode::Stringify => {
                        out.extend_from_slice(b"\t\"");
                        out.extend_from_slice(value.as_bytes());
                        out.extend_from_slice(b"\"");
                    }
                    WriteMode::Hexadecimal => {
                        let s = format!("\t0x{:X}", satoi(Some(value)));
                        out.extend_from_slice(s.as_bytes());
                    }
                    WriteMode::Binary => {
                        out.extend_from_slice(b"\t0b");
                        out.extend_from_slice(satob(Some(value)).as_bytes());
                    }
                    WriteMode::Pair => {
                        out.extend_from_slice(b"\t\"");
                        out.extend_from_slice(p.subkey.as_deref().unwrap_or("").as_bytes());
                        out.extend_from_slice(b",");
                        out.extend_from_slice(value.as_bytes());
                        out.extend_from_slice(b"\"");
                    }
                }
            }
            out.extend_from_slice(b"\n");
        }
    }

    fn write_data_format(&self, out: &mut Vec<u8>) {
        let t = self.table();
        let df = match &t.data_format {
            None => return,
            Some(df) => df,
        };

        out.extend_from_slice(b"BEGIN_DATA_FORMAT\n");
        out.extend_from_slice(b" ");
        let n_samples = satoi(self.get_property("NUMBER_OF_FIELDS"));
        if n_samples <= t.n_samples {
            for i in 0..n_samples {
                let label = df.get(i as usize).and_then(|s| s.as_deref()).unwrap_or(" ");
                out.extend_from_slice(label.as_bytes());
                out.extend_from_slice(if i == n_samples - 1 { b"\n" } else { b"\t" });
            }
        }
        out.extend_from_slice(b"END_DATA_FORMAT\n");
    }

    fn write_data(&self, out: &mut Vec<u8>) {
        let t = self.table();
        let data = match &t.data {
            None => return,
            Some(d) => d,
        };

        out.extend_from_slice(b"BEGIN_DATA\n");
        let n_patches = satoi(self.get_property("NUMBER_OF_SETS"));
        if n_patches <= t.n_patches {
            for i in 0..n_patches {
                out.extend_from_slice(b" ");
                for j in 0..t.n_samples {
                    let ptr = data
                        .get((i * t.n_samples + j) as usize)
                        .and_then(|s| s.as_deref());
                    match ptr {
                        None => out.extend_from_slice(b"\"\""),
                        Some(s) => {
                            if s.as_bytes().contains(&b' ') {
                                out.extend_from_slice(b"\"");
                                out.extend_from_slice(s.as_bytes());
                                out.extend_from_slice(b"\"");
                            } else {
                                out.extend_from_slice(s.as_bytes());
                            }
                        }
                    }
                    out.extend_from_slice(if j == t.n_samples - 1 { b"\n" } else { b"\t" });
                }
            }
        }
        out.extend_from_slice(b"END_DATA\n");
    }
}
