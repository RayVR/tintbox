//! Unpack/pack pixel formatters, 16-bit domain.
//!
//! Bit-identical transcriptions of `cmspack.c`. Unpack formatters read packed
//! bytes into a `cmsMAXCHANNELS`-wide `[u16; N]` and report how many bytes they
//! consumed; pack formatters write a `[u16; N]` value array back to bytes and
//! report how many bytes they produced. The generic `unroll_chunky_bytes` /
//! `unroll_any_words` / `pack_chunky_bytes` / `pack_chunky_words` reproduce
//! lcms2's `UnrollChunkyBytes` / `UnrollAnyWords` / `PackChunkyBytes` /
//! `PackChunkyWords` (all DOSWAP / SWAPFIRST / FLAVOR / ENDIAN16 / EXTRA combos
//! for chunky buffers); the `unroll_N_*` / `pack_N_*` helpers are the fast
//! fixed-channel specializations lcms2 selects for the plain cases.
//!
//! Premultiplied-alpha and planar handling are out of scope here (no `TYPE_*`
//! macro in this task's set sets them); the generic functions ignore PREMUL and
//! assume chunky, matching the formatters lcms2 dispatches for these types.

use crate::fixed::{from_16_to_8, from_8_to_16};

/// Number of value slots a formatter operates on (lcms2 `cmsMAXCHANNELS`).
pub const MAX_CHANNELS: usize = 16;

/// `REVERSE_FLAVOR_16` (cmspack.c:42): `0xffff - v`.
#[inline]
const fn reverse_flavor_16(v: u16) -> u16 {
    0xffff - v
}

/// `CHANGE_ENDIAN` (cmspack.c:38): byte-swap a 16-bit value, i.e. `(v<<8)|(v>>8)`.
/// For u16 this is exactly an 8-bit rotate; `swap_bytes` is bit-identical.
#[inline]
const fn change_endian(v: u16) -> u16 {
    v.swap_bytes()
}

// ---- Specialized 1-channel (monochrome) unpackers (cmspack.c Unroll1*) ----
//
// lcms2's monochrome unrollers duplicate L into wIn[0..2] "for null-transforms"
// (cmspack.c:455). This replication is observable in the unpacked value array,
// so to be bit-identical to lcms2 we reproduce it. It only applies to the
// specific 1-channel table rows; GRAYA_16 / GRAY_16_SE fall through to the
// generic UnrollAnyWords (which does NOT replicate).

/// `Unroll1Byte`: gray L replicated into [0..2]; 1 byte consumed.
pub fn unroll_1_byte(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    let v = from_8_to_16(accum[0]);
    values[0] = v;
    values[1] = v;
    values[2] = v;
    1
}

/// `Unroll1ByteReversed` (GRAY_8_REV): reversed gray, replicated; 1 byte.
pub fn unroll_1_byte_reversed(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    let v = reverse_flavor_16(from_8_to_16(accum[0]));
    values[0] = v;
    values[1] = v;
    values[2] = v;
    1
}

/// `Unroll1ByteSkip1` (GRAYA_8): gray replicated, then skip 1 extra; 2 bytes.
pub fn unroll_1_byte_skip1(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    let v = from_8_to_16(accum[0]);
    values[0] = v;
    values[1] = v;
    values[2] = v;
    2
}

/// `Unroll1Word` (GRAY_16): native-endian gray, replicated; 2 bytes.
pub fn unroll_1_word(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    let v = read_u16_le(&accum[0..]);
    values[0] = v;
    values[1] = v;
    values[2] = v;
    2
}

/// `Unroll1WordReversed` (GRAY_16_REV): reversed native-endian gray, replicated.
pub fn unroll_1_word_reversed(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    let v = reverse_flavor_16(read_u16_le(&accum[0..]));
    values[0] = v;
    values[1] = v;
    values[2] = v;
    2
}

// ---- Specialized byte unpackers (cmspack.c Unroll{3,4}Bytes) ----

/// `Unroll3Bytes`: R,G,B in order; 3 bytes consumed.
pub fn unroll_3_bytes(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    values[0] = from_8_to_16(accum[0]);
    values[1] = from_8_to_16(accum[1]);
    values[2] = from_8_to_16(accum[2]);
    3
}

/// `Unroll4Bytes`: C,M,Y,K in order; 4 bytes consumed.
pub fn unroll_4_bytes(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    values[0] = from_8_to_16(accum[0]);
    values[1] = from_8_to_16(accum[1]);
    values[2] = from_8_to_16(accum[2]);
    values[3] = from_8_to_16(accum[3]);
    4
}

// ---- Specialized word unpackers (cmspack.c Unroll{3,4}Words) ----

#[inline]
fn read_u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

/// `Unroll3Words`: 3 native-endian 16-bit samples; 6 bytes consumed.
pub fn unroll_3_words(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    values[0] = read_u16_le(&accum[0..]);
    values[1] = read_u16_le(&accum[2..]);
    values[2] = read_u16_le(&accum[4..]);
    6
}

/// `Unroll4Words`: 4 native-endian 16-bit samples; 8 bytes consumed.
pub fn unroll_4_words(accum: &[u8], values: &mut [u16; MAX_CHANNELS]) -> usize {
    values[0] = read_u16_le(&accum[0..]);
    values[1] = read_u16_le(&accum[2..]);
    values[2] = read_u16_le(&accum[4..]);
    values[3] = read_u16_le(&accum[6..]);
    8
}

// ---- Generic chunky byte unpacker (cmspack.c UnrollChunkyBytes) ----

/// `UnrollChunkyBytes`. Handles DOSWAP (reverse channel order), SWAPFIRST /
/// EXTRA (extra samples lead or trail; `ExtraFirst = DoSwap ^ SwapFirst`),
/// FLAVOR (reverse each sample), and the `Extra == 0 && SwapFirst` rotate.
/// PREMUL is not handled (no covered TYPE_* sets it). Returns bytes consumed.
pub fn unroll_chunky_bytes(
    n_chan: usize,
    extra: usize,
    do_swap: bool,
    swap_first: bool,
    reverse: bool,
    accum: &[u8],
    values: &mut [u16; MAX_CHANNELS],
) -> usize {
    let extra_first = do_swap ^ swap_first;
    let mut pos = 0usize;

    if extra_first {
        pos += extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = from_8_to_16(accum[pos]);
        if reverse {
            v = reverse_flavor_16(v);
        }
        values[index] = v;
        pos += 1;
    }

    if !extra_first {
        pos += extra;
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    pos
}

// ---- Generic 16-bit unpacker (cmspack.c UnrollAnyWords) ----

/// `UnrollAnyWords`. Like [`unroll_chunky_bytes`] but for 16-bit samples and
/// with ENDIAN16 byte-swap. Returns bytes consumed.
#[allow(clippy::too_many_arguments)]
pub fn unroll_any_words(
    n_chan: usize,
    extra: usize,
    do_swap: bool,
    swap_first: bool,
    reverse: bool,
    swap_endian: bool,
    accum: &[u8],
    values: &mut [u16; MAX_CHANNELS],
) -> usize {
    let extra_first = do_swap ^ swap_first;
    let mut pos = 0usize;

    if extra_first {
        pos += extra * 2;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = read_u16_le(&accum[pos..]);
        if swap_endian {
            v = change_endian(v);
        }
        values[index] = if reverse { reverse_flavor_16(v) } else { v };
        pos += 2;
    }

    if !extra_first {
        pos += extra * 2;
    }

    if extra == 0 && swap_first {
        let tmp = values[0];
        values.copy_within(1..n_chan, 0);
        values[n_chan - 1] = tmp;
    }

    pos
}

// ---- Specialized byte packers (cmspack.c Pack{3,4}Bytes) ----

/// `Pack3Bytes`: 3 bytes written.
pub fn pack_3_bytes(values: &[u16; MAX_CHANNELS], output: &mut [u8]) -> usize {
    output[0] = from_16_to_8(values[0]);
    output[1] = from_16_to_8(values[1]);
    output[2] = from_16_to_8(values[2]);
    3
}

/// `Pack4Bytes`: 4 bytes written.
pub fn pack_4_bytes(values: &[u16; MAX_CHANNELS], output: &mut [u8]) -> usize {
    output[0] = from_16_to_8(values[0]);
    output[1] = from_16_to_8(values[1]);
    output[2] = from_16_to_8(values[2]);
    output[3] = from_16_to_8(values[3]);
    4
}

// ---- Specialized word packers (cmspack.c Pack{3,4}Words) ----

#[inline]
fn write_u16_le(b: &mut [u8], v: u16) {
    let [lo, hi] = v.to_le_bytes();
    b[0] = lo;
    b[1] = hi;
}

/// `Pack3Words`: 3 native-endian 16-bit samples; 6 bytes written.
pub fn pack_3_words(values: &[u16; MAX_CHANNELS], output: &mut [u8]) -> usize {
    write_u16_le(&mut output[0..], values[0]);
    write_u16_le(&mut output[2..], values[1]);
    write_u16_le(&mut output[4..], values[2]);
    6
}

/// `Pack4Words`: 4 native-endian 16-bit samples; 8 bytes written.
pub fn pack_4_words(values: &[u16; MAX_CHANNELS], output: &mut [u8]) -> usize {
    write_u16_le(&mut output[0..], values[0]);
    write_u16_le(&mut output[2..], values[1]);
    write_u16_le(&mut output[4..], values[2]);
    write_u16_le(&mut output[6..], values[3]);
    8
}

// ---- Generic chunky byte packer (cmspack.c PackChunkyBytes) ----

/// `PackChunkyBytes`. Handles DOSWAP, SWAPFIRST / EXTRA, FLAVOR, and the
/// `Extra == 0 && SwapFirst` rotate. PREMUL is not handled. The extra-sample
/// bytes are left untouched (lcms2 only advances over them). Returns bytes
/// written (color channels + extra).
pub fn pack_chunky_bytes(
    n_chan: usize,
    extra: usize,
    do_swap: bool,
    swap_first: bool,
    reverse: bool,
    values: &[u16; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let extra_first = do_swap ^ swap_first;
    // `swap1` in C is the original output pointer (offset 0).
    let mut pos = 0usize;
    let mut last_v: u16 = 0;

    if extra_first {
        pos += extra;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index];
        if reverse {
            v = reverse_flavor_16(v);
        }
        last_v = v;
        output[pos] = from_16_to_8(v);
        pos += 1;
    }

    if !extra_first {
        pos += extra;
    }

    if extra == 0 && swap_first {
        // memmove(swap1+1, swap1, nChan-1); *swap1 = FROM_16_TO_8(v);
        output.copy_within(0..n_chan - 1, 1);
        output[0] = from_16_to_8(last_v);
    }

    pos
}

// ---- Generic 16-bit packer (cmspack.c PackChunkyWords) ----

/// `PackChunkyWords`. Like [`pack_chunky_bytes`] but 16-bit with ENDIAN16.
/// Returns bytes written.
#[allow(clippy::too_many_arguments)]
pub fn pack_chunky_words(
    n_chan: usize,
    extra: usize,
    do_swap: bool,
    swap_first: bool,
    reverse: bool,
    swap_endian: bool,
    values: &[u16; MAX_CHANNELS],
    output: &mut [u8],
) -> usize {
    let extra_first = do_swap ^ swap_first;
    let mut pos = 0usize;
    let mut last_v: u16 = 0;

    if extra_first {
        pos += extra * 2;
    }

    for i in 0..n_chan {
        let index = if do_swap { n_chan - i - 1 } else { i };
        let mut v = values[index];
        if swap_endian {
            v = change_endian(v);
        }
        if reverse {
            v = reverse_flavor_16(v);
        }
        last_v = v;
        write_u16_le(&mut output[pos..], v);
        pos += 2;
    }

    if !extra_first {
        pos += extra * 2;
    }

    if extra == 0 && swap_first {
        // memmove over 16-bit units: shift first (nChan-1) words up by one word.
        output.copy_within(0..(n_chan - 1) * 2, 2);
        write_u16_le(&mut output[0..], last_v);
    }

    pos
}
