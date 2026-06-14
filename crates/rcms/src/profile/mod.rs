//! ICC profile parsing. Task 1 lands the 128-byte header; the full `Profile`
//! (tag directory + tag readers) arrives in later slice-2 tasks.

pub mod header;

pub use header::{ColorSpace, DateTime, Header, ProfileClass, RenderingIntent};
