# tintbox

A pure-Rust, full-parity reimplementation of [Little CMS](https://littlecms.com)
(lcms2 2.19.1) — the ICC color-management engine.

`tintbox` is written from scratch in safe Rust (`#![forbid(unsafe_code)]`), targets
`std` with abstract I/O (so it builds cleanly for `wasm32`), and is **verified
bit-identical to the C library by differential testing** — not "close enough",
byte-for-byte.

## Why

It was built to replace the C lcms2 dependency in a Rust rendering pipeline. The
wins over linking the C library:

- **Memory safety** — no `unsafe` in the shipped crate; the entire attack surface
  of a C image/color library is gone.
- **No libc / clean cross-compilation** — pure Rust + `std`, so `wasm32` and
  other targets build without a C toolchain or platform CMM.
- **Idiomatic Rust API** — owned types, `Result`, traits; no C ABI.
- **Performance headroom** — all-float in-place pipeline evaluation by default,
  with optional lcms2-compatible optimization.

## Bit-identity is the contract

Correctness is defined as *producing the same bytes as lcms2*. A test-only
`tintbox-oracle` crate `cc`-builds the vendored C lcms2 (pinned at tag `lcms2.19.1`)
plus thin shims, and every numeric path is swept against it: pixel transforms,
tag parsing, profile serialization, virtual profiles, black-point detection,
CIECAM02, gamut/TAC, PostScript generation, and more. Where a value depends on a
documented lcms2 quirk (e.g. the 1998 `quick_floor` rounding hack, 1.14
fixed-point matrix-shapers), the quirk is reproduced exactly and isolated behind
a compile-time strategy seam so an alternative can be swapped in and measured.

The shipped `tintbox` crate contains **no C and no `unsafe`**; the C only exists in
the test oracle.

## Status

Feature-complete reimplementation. All subsystems are merged and differentially
tested against lcms2:

| Area | What |
|------|------|
| Profile I/O | Header + tag directory + all tag-type readers **and** byte-exact writers; round-trips through both stacks |
| Tone curves & PCS | All 20 parametric types (+ inverses), tabulated/segmented curves, Lab/XYZ/LCh/xyY, Bradford adaptation |
| Pipelines | `Stage` pipeline, n-D interpolation (tetrahedral/trilinear/…), LUT/MPE tags |
| Transforms | `cmsCreateTransform`/`cmsDoTransform`, all 4 rendering intents, absolute-colorimetric + black-point compensation, black-point detection |
| Pixel formats | Packed `TYPE_*` 8/16/float/double, RGB/CMYK/Gray/Lab/XYZ, swap/flavor/endian, alpha copy |
| Optimization | Swappable strategy: `Accurate` (lossless, default) or `Lcms2Compat` (matches stock lcms2-default incl. the CLUT-baking optimizer) |
| Virtual profiles | sRGB, RGB, gray, Lab2/Lab4, XYZ, NULL, linearization device-link — byte-identical to `cmsCreate*Profile` |
| Peripheral | CGATS/IT8.7, CIECAM02, PostScript CSA/CRD, named/spot colors, gamut boundary + `cmsDetectTAC` + proofing/gamut-check |
| Extensibility | lcms2's plugin categories as idiomatic Rust traits (parametric curves, tag types, rendering intents, optimizers, interpolators) |

## Usage

```rust
use tintbox::prelude::*;
use tintbox::format::decode::{TYPE_RGB_8, TYPE_CMYK_8};

// Build a transform between two profiles and convert packed pixels.
let input = Profile::open(&srgb_icc)?;
let output = Profile::open(&cmyk_icc)?;

let xform = Transform::new_simple_with_formats(
    &input, &output,
    RenderingIntent::Perceptual,
    /* bpc */ true,
    TYPE_RGB_8, TYPE_CMYK_8,
)?;

let n_pixels = pixels.len() / 3;
let mut dst = vec![0u8; n_pixels * 4];
xform.do_transform(&pixels, &mut dst, n_pixels);
```

The default optimization strategy is `Accurate` (full-precision, lossless, and
bit-identical to lcms2 run with `cmsFLAGS_NOOPTIMIZE`). Opt into
`OptimizationStrategy::Lcms2Compat` for drop-in parity with stock lcms2-default.

## Extending tintbox (plugins)

lcms2 is extensible through a C plugin ABI. `tintbox` exposes the same extension
points as **idiomatic Rust traits**, registered on a `Context`:

| Trait | Register | Real-world use |
|-------|----------|----------------|
| `RenderingIntentPlugin` | `ctx.register_intent` | Black/ink preservation, custom gamut mapping |
| `TagTypePlugin` + `TagDescriptor` | `ctx.register_tag_type` | Read/write a vendor's private ICC tags so they survive a round-trip |
| `ParametricCurvePlugin` | `ctx.register_parametric_curve` | A new transfer function (a measured TRC, an HDR curve) |
| `Optimizer` | `ctx.set_optimizer` | Drop in a SIMD/GPU fast path for a pipeline shape you care about |
| `InterpolatorFactory` | `ctx.register_interpolator` | A higher-order CLUT interpolant for smoother gradients |

The registry is consulted at construction/link/read time and resolves to a
concrete value *before* the per-pixel loop runs, so a plugin never slows the hot
path. Crucially, **built-ins are always matched first** — a plugin can only
service an id the engine doesn't already handle, so registering one *cannot
perturb* the bit-identical built-in paths. An empty `Context` behaves exactly
like stock tintbox.

### Motivating example: keep black text black

In CMYK print production, a plain relative-colorimetric conversion re-expresses
100%-K text and hairlines as **4-color "rich black."** On press, all four plates
must then register perfectly or black text shows colored fringing and looks
fuzzy — a classic, expensive prepress headache. The fix is a *black-preserving*
rendering intent (lcms2 ships `INTENT_PRESERVE_K_ONLY_*` as plugins). Here it is
as a Rust trait:

```rust
use std::sync::Arc;
use tintbox::prelude::*;
use tintbox::link::default_icc_intents;

struct PreserveBlack;

impl RenderingIntentPlugin for PreserveBlack {
    fn intent(&self) -> u32 { 10 }  // lcms2 INTENT_PRESERVE_K_ONLY_PERCEPTUAL
    fn description(&self) -> &str { "Preserve pure-K black (print production)" }

    fn link(
        &self,
        ctx: &Context,
        profiles: &[&Profile],
        intents: &[RenderingIntent],
        bpc: &[bool],
        adaptation: &[f64],
        flags: u32,
    ) -> Result<Pipeline> {
        // Reuse the bit-identical built-in color link for the chromatic path…
        let pipeline = default_icc_intents(profiles, intents, bpc, adaptation, flags)?;
        // …then append your K-restoration stage so K-only input stays on the K
        // plate alone. The plugin owns this ink policy; the engine stays generic.
        Ok(pipeline)
    }
}

// Register once; then build transforms with the custom intent number.
let mut ctx = Context::new();
ctx.register_intent(Arc::new(PreserveBlack));

let xform = Transform::new_in(
    &ctx,
    &[&cmyk_press, &cmyk_proof],
    &[RenderingIntent::Other(10), RenderingIntent::Other(10)],
    &[/* bpc */ true, true],
    &[1.0, 1.0],
    Flags::empty(),
)?;
```

The same pattern handles the other categories: register a `TagTypePlugin` to
carry a proprietary spot-ink or spectral tag through a profile round-trip, or
`set_optimizer` to swap in a hand-tuned fast path while every other transform
keeps using the verified default.

## Building & testing

The workspace has two crates: `tintbox` (the library) and `tintbox-oracle` (test-only,
builds the C library for differential comparison).

```bash
# Clone with the vendored lcms2 submodule (required for the oracle).
git submodule update --init --recursive

cargo test --workspace          # full differential suite (builds C lcms2 via cc)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo build -p tintbox --target wasm32-unknown-unknown   # wasm builds without the oracle
```

Building the oracle requires a C compiler (the vendored lcms2 is compiled with
`cc`). The `tintbox` crate itself has no C dependency.

## License

The vendored Little CMS under `vendor/` retains its original MIT license. See
that subtree for upstream copyright.
