# Contributing to tintbox

Thanks for your interest in tintbox — a pure-Rust, full-parity reimplementation
of Little CMS (lcms2). This document explains how to contribute and the rules a
contribution has to satisfy. Most of these rules are not bureaucracy: they
protect the one property that makes tintbox worth using.

> **The repo directory is `rcms`; the crate and project are `tintbox`.** The
> name `rcms` was taken on crates.io. Don't "fix" the directory name.

## The prime directive: bit-identity

tintbox is correct **only when it produces the same bytes as lcms2.** This is
verified by the `tintbox-oracle` test crate, which `cc`-builds vendored lcms2
(at tag `lcms2.19.1`) and sweeps every code path against it.

- A change is acceptable only if `cargo test -p tintbox` stays **byte-for-byte
  green**. Performance work makes the *same bytes* faster — it never changes a
  result.
- Code that mirrors an lcms2 behavior **must be byte-identical to lcms2**, not
  merely "close." If you can't match it, it isn't done.
- It is expected and correct that `tintbox-oracle` compiles C during tests. The
  *shipped* crate has no C and no build script. Do not try to remove the C from
  the test build.

New capabilities that **exceed** lcms2 (e.g. spectral, HDR, modern gamut
mapping) are welcome, but they are held to a different bar — see
[Frontier features](#frontier-features-beyond-lcms2) below.

## Hard constraints (a PR that violates these will not be merged)

These are enforced by the type system, the lints, CI, and review:

1. **No `unsafe`.** The shipped crate is `#![forbid(unsafe_code)]`. There are no
   exceptions. Untrusted input (ICC/CGATS/`.cube` parsing) is a **DoS** threat
   model — panics, unbounded allocation, and hangs are bugs, not memory-safety
   issues, and must be eliminated, not "handled."
2. **wasm-clean.** The core must build for `wasm32-unknown-unknown` with no
   filesystem or OS coupling. I/O goes through the abstract reader/writer traits;
   real file access stays behind the `file-io` feature.
3. **No internal threading.** tintbox never threads internally by design (it must
   not oversubscribe consumers that already parallelize). `Transform: Send + Sync`
   so consumers can split a buffer across threads. Do **not** add `rayon`,
   threads, or any internal parallelism.
4. **SIMD is the opt-in `simd` feature only**, via the safe `wide` crate. It must
   stay bit-identical and unsafe-free, and off by default.
5. **No-panic discipline on the parse spine.** The parser modules carry
   `#![deny(clippy::indexing_slicing, unwrap_used, expect_used, panic)]`. Don't
   weaken these; convert any new indexing/unwrap into a real `Error` return.
6. **No proprietary or copyleft-encumbered data — ever, including in tests.**
   tintbox ships the *engine* and *open data only*. Do not add Pantone (or any
   licensed) color/spectral data, GPL/AGPL/SSPL-licensed code, or any material
   you don't have the right to contribute under this project's license. Test
   fixtures must be synthetic or from a permissively-licensed/public source, with
   the license recorded. (See the CLA's IP warranty.)
7. **Don't hand-edit dependency tables.** Use `cargo add` / `cargo remove` /
   `cargo update` so the lockfile stays correct. Editing non-dependency config in
   a manifest is fine.

## Frontier features (beyond lcms2)

Features that go past lcms2's spec ceiling have **no lcms2 oracle to match.** If
you contribute one:

- It must be **opt-in and gated** (a Cargo feature and/or an explicit strategy/
  API), so the default lossless parity paths stay byte-untouched.
- It must carry **its own named correctness reference** (a standard's test
  vectors, a buildable reference implementation such as DemoIccMAX, published
  reference values, etc.) and tests against it. "Looks right" is not a reference.
- It must hold every hard constraint above (no unsafe, wasm-clean, etc.).

Open a discussion issue *before* a large frontier PR so we can agree on the
oracle and the boundary.

## Development setup

```sh
# install the project git hooks once (pre-commit fmt; pre-push fmt+clippy+wasm)
git config core.hooksPath .githooks
```

Before opening a PR, make sure all of the gates CI enforces pass locally:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p tintbox --all-targets --features simd -- -D warnings
cargo test --workspace                       # the full differential suite
cargo build -p tintbox --target wasm32-unknown-unknown
```

## Pull-request process

1. **Branch** from `master`. `master` is the release branch; PRs target it.
2. Keep PRs **focused** — one logical change. Separate mechanical refactors from
   behavioral changes.
3. **Commit messages describe what the change does and why**, in the imperative
   mood. Don't reference plan-step numbers or task IDs; the message should make
   sense to someone reading `git log` years later.
4. **Match the surrounding code** — comment density, naming, and idiom. tintbox
   comments transcribe the lcms2 routine a function mirrors (file:line); keep that
   convention when you touch parity code.
5. Add or update tests. For parity changes, the differential sweep *is* the test;
   make sure your path is actually exercised against the oracle.
6. CI must be green and the **CLA signed** (see below) before merge.

## Security

If you find a vulnerability (a malformed-input panic/OOM/hang, or a byte-identity
divergence reachable from untrusted data), please **do not** open a public issue.
See [`SECURITY.md`](SECURITY.md) for private disclosure, or email the maintainer.

## Contributor License Agreement (required)

tintbox is open-core: the library is and will remain available under the MIT
license, and a separate proprietary layer is built on top of it. To keep that
sustainable — and to preserve the project's ability to add an explicit patent
grant, to relicense the core if ever necessary, and to guarantee a clean IP
chain — **every contributor must agree to the Contributor License Agreement in
[`CLA.md`](CLA.md) before their contribution can be merged.**

You keep the copyright to your contribution. The CLA grants the project a broad
license to use and relicense it (including in the proprietary layer) and asks you
to warrant that the contribution is yours to give. Trivial changes (typo fixes,
formatting) may be accepted at the maintainer's discretion without signing.

How to sign: [the CLA bot will prompt on your first PR / reply to your PR with
the agreement statement in `CLA.md`]. <!-- TODO: wire up CLA Assistant or an
equivalent and replace this line. -->

A **Developer Certificate of Origin** (`Signed-off-by:` via `git commit -s`) is
also required on every commit as a lightweight provenance record, but the DCO
**does not replace** the CLA — only the CLA grants the patent license and the
relicensing right the open-core model depends on.

---

By contributing, you agree your contribution is licensed under the project's MIT
license **and** the terms of [`CLA.md`](CLA.md).
