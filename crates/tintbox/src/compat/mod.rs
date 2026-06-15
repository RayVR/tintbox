//! Compatibility seam: behaviors kept ONLY to match lcms2 bit-for-bit live here
//! as swappable strategies. Selection is compile-time (zero-cost — no dyn
//! dispatch on hot paths). Default = bit-identical; alternatives are measured by
//! the divergence harness, not guessed at.

pub mod floor;

pub use floor::{FloorStrategy, Lcms2Floor, NativeFloor};

/// The active floor strategy. Default is the bit-identical lcms2 hack; the
/// `native-floor` feature swaps in the f64::floor-based implementation.
#[cfg(not(feature = "native-floor"))]
pub type Floor = Lcms2Floor;
#[cfg(feature = "native-floor")]
pub type Floor = NativeFloor;
