#![forbid(unsafe_code)]
//! rcms — a pure-Rust reimplementation of Little-CMS (lcms2), bit-identical in
//! numeric output and idiomatic in design. The core contains zero `unsafe`
//! (permanently — SIMD will live in a sibling crate).
//!
//! NOTE: modules are wired in incrementally as slice-1 tasks land, so the crate
//! always compiles. The full module set / prelude is assembled by the final task.

pub mod sig;

/// One-line imports for consumers: `use rcms::prelude::*;`.
pub mod prelude {
    pub use crate::sig::Signature;
}
