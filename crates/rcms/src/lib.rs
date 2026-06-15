#![forbid(unsafe_code)]
//! rcms — a pure-Rust reimplementation of Little-CMS (lcms2), bit-identical in
//! numeric output and idiomatic in design. The core contains zero `unsafe`
//! (permanently — SIMD will live in a sibling crate).
//!
//! NOTE: modules are wired in incrementally as slice-1 tasks land, so the crate
//! always compiles. The full module set / prelude is assembled by the final task.

pub mod adapt;
pub mod cam02;
pub mod cgats;
pub mod color;
pub mod compat;
pub mod context;
pub mod curve;
pub mod error;
pub mod fixed;
pub mod format;
pub mod gamut;
pub mod interp;
pub mod io;
pub mod link;
pub mod math;
pub mod named;
pub mod opt;
pub mod pcs;
pub mod pipeline;
pub mod profile;
pub mod ps;
pub mod sig;
pub mod transform;

pub use error::{Error, Result};

/// One-line imports for consumers: `use rcms::prelude::*;`.
pub mod prelude {
    pub use crate::color::{CIELCh, CIELab, CIEXYZTriple, CIExyY, CIExyYTriple, JCh, CIEXYZ};
    pub use crate::context::{Context, Logger};
    pub use crate::curve::{
        build_gamma, build_parametric, build_segmented, build_tabulated_16, build_tabulated_float,
        eval_parametric, reverse_tone_curve, reverse_tone_curve_ex, CurveSegment, ToneCurve,
    };
    pub use crate::error::{Error, Result};
    pub use crate::fixed::{Half, S15Fixed16, U16Fixed16, U8Fixed8};
    pub use crate::io::{ProfileReader, ProfileWriter};
    pub use crate::link::{read_devicelink_lut, read_input_lut, read_output_lut};
    pub use crate::named::{NamedColor, NamedColorList};
    pub use crate::opt::OptimizationStrategy;
    pub use crate::pipeline::{Pipeline, Stage};
    pub use crate::profile::{Header, Profile, Tag};
    pub use crate::sig::Signature;
    pub use crate::transform::{Flags, Transform};
}
