//! Gamut boundary, gamut check, and total-area-coverage (lcms2 `cmssm.c` +
//! `cmsgmt.c`).
//!
//! - [`GamutBoundaryDescriptor`] — Jan Morovic's segment-maxima gamut boundary
//!   (`cmssm.c`): add specified Lab points, interpolate the missing sectors, and
//!   test membership.
//! - [`detect_tac`] — total area coverage of an output profile (`cmsDetectTAC`).
//! - [`create_gamut_check_pipeline`] — the 3→1 gamut-check LUT
//!   (`_cmsCreateGamutCheckPipeline`) wired into the proofing transform's
//!   alarm-color path (see [`crate::transform::Transform::new_proofing`]).

mod check;
mod sm;
pub mod tac;

pub use check::create_gamut_check_pipeline;
pub use sm::GamutBoundaryDescriptor;
pub use tac::detect_tac;
