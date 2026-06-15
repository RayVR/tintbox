//! Profile → pipeline linking (lcms2 `cmsio1.c` / `cmscnvrt.c`).
//!
//! This module turns a parsed [`Profile`](crate::profile::Profile) into a
//! processing [`Pipeline`](crate::pipeline::Pipeline): the device→PCS,
//! PCS→device, and device-link LUT extraction that
//! `_cmsReadInputLUT`/`_cmsReadOutputLUT`/`_cmsReadDevicelinkLUT` perform.

pub mod black_point;
pub mod intents;
pub mod profile_lut;

pub use black_point::{
    compute_black_point_compensation, detect_black_point, detect_destination_black_point,
    BlackPoint,
};
pub use intents::{
    add_conversion, compute_absolute_intent, compute_conversion, default_icc_intents,
    is_empty_layer, link_bpc_mutation, link_icc_intents_in, read_chad, read_media_white_point,
};
pub use profile_lut::{read_devicelink_lut, read_input_lut, read_output_lut};
