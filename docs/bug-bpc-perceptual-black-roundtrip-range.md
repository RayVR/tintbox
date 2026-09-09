# BPC transforms with forward-only CMYK profiles

**Fixed in tintbox 0.4.0; included in 0.5.0.**

## Failure

A Black Point Compensation transform from an Output-class CMYK profile with
an `A2B0` device-to-PCS tag, but no reverse `B2A0` tag, could fail with
`Error::Range`. Opening the profile succeeded; construction of the transform
failed during black-point detection.

The perceptual black-point calculation builds a Lab-to-device-to-Lab round
trip. The missing reverse profile direction makes that round trip unavailable.
Before 0.4.0, its error propagated and aborted the parent transform.

## Fix

`black_point_using_perceptual_black` now returns `BlackPoint::Zero` when the
detection round trip cannot be built. `black_point_as_darker_colorant` follows
the same fallback. This mirrors the bundled lcms2 reference implementation's
handling of an unavailable detection transform.

The change allows the parent BPC transform to proceed. It does not synthesize
a reverse profile direction or disable Black Point Compensation.

## Regression coverage

[`bpc_no_b2a_roundtrip.rs`](../crates/tintbox/tests/bpc_no_b2a_roundtrip.rs)
constructs a synthetic Output-class CMYK profile with only `A2B0`, then builds
a relative-colorimetric, BPC-enabled transform to sRGB through tintbox and the
bundled lcms2 oracle. It checks both successful construction and identical output
bytes for black, white, primary colorants, and a mixed CMYK input.

Run it with:

```sh
cargo test -p tintbox --test bpc_no_b2a_roundtrip
```

The original failure surfaced in pdf_oxide's rendering tests for forward-only
CMYK OutputIntent profiles. Releases 0.4.0 and 0.5.0 both contain the fix.
