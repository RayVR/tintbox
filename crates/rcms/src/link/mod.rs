//! Profile → pipeline linking (lcms2 `cmsio1.c` / `cmscnvrt.c`).
//!
//! This module turns a parsed [`Profile`](crate::profile::Profile) into a
//! processing [`Pipeline`](crate::pipeline::Pipeline): the device→PCS,
//! PCS→device, and device-link LUT extraction that
//! `_cmsReadInputLUT`/`_cmsReadOutputLUT`/`_cmsReadDevicelinkLUT` perform.

pub mod profile_lut;

pub use profile_lut::{read_devicelink_lut, read_input_lut, read_output_lut};
