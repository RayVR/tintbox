# tintbox — agent notes

Pure-Rust, full-parity reimplementation of Little CMS (lcms2 2.19.1). The shipped
crate is `#![forbid(unsafe_code)]`, std + abstract I/O, wasm-clean, zero C.

- **Repo dir is `rcms`, crate/project is `tintbox`.** The name "rcms" was taken on
  crates.io; everything user-facing is `tintbox`. Don't "fix" the dir name.

## The one invariant: bit-identity

Correctness = **producing the same bytes as lcms2**, verified by the `tintbox-oracle`
test crate (it `cc`-builds vendored lcms2 at tag `lcms2.19.1` and sweeps every path
against it). This is non-negotiable:

- A change is correct only if the differential sweeps stay byte-for-byte equal.
  Performance work makes the *same bytes* faster — it never changes a result.
- `tintbox-oracle` compiling C **during tests is expected**, not a bug. The shipped
  crate has no C and no build script. Don't try to remove the C from the test build.
- Run `cargo test -p tintbox` before claiming any change is done; the sweeps are there.

## Optimization strategies (`OptimizationStrategy`)

- `AccurateFast` — **the default**. Lossless, byte-identical to `Accurate`, faster.
- `Accurate` — lcms2 `-NOOPTIMIZE`; the minimal single-code-path reference eval.
- `Lcms2Compat` — opt-in, **lossy** (matches stock lcms2-default's devicelink bake).
  Never make this the default; the project values lossless accuracy (shadow fidelity).

## Design constraints — don't violate without asking

- **Single-threaded by design.** The library never threads internally (avoids
  oversubscribing consumers that already parallelize). `Transform: Send + Sync` so
  consumers split the buffer across threads. Don't add internal threading/rayon.
- **SIMD is the opt-in `simd` feature** via the safe `wide` crate — must stay
  bit-identical and unsafe-free. Off by default.

## Performance: measure first

Profile/benchmark the real workload before optimizing — clever kernels here have
repeatedly measured as noise. Use `benches/transform.rs` (criterion) and
`examples/profile_transform.rs`. Don't present run-to-run noise as a win.

## Before pushing

Install hooks once: `git config core.hooksPath .githooks`. CI enforces all of:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`
(plus `-p tintbox --features simd`), and a `wasm32-unknown-unknown` build.

`master` is the release branch, but **don't push to it without explicit
authorization** — pushes to the default branch are gated.
