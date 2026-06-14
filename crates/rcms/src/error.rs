//! Errors are values. Allocation-free: `&'static str` / `u32` only, so the core
//! stays no_std/alloc-free and the (not-cold) malformed-profile path never heaps.
//! Rich context (offending value, byte offset) is emitted via the Context logger.

use crate::sig::Signature;
use core::fmt;

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io,
    Truncated { needed: u32, got: u32 },
    BadSignature(Signature),
    UnexpectedSignature { want: Signature, got: Signature },
    BadType(Signature),
    Range,
    Corrupt(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io => write!(f, "i/o error"),
            Error::Truncated { needed, got } => write!(f, "truncated: needed {needed}, got {got}"),
            Error::BadSignature(s) => write!(f, "bad signature: {s}"),
            Error::UnexpectedSignature { want, got } => write!(f, "expected {want}, got {got}"),
            Error::BadType(s) => write!(f, "bad tag type: {s}"),
            Error::Range => write!(f, "value out of range"),
            Error::Corrupt(d) => write!(f, "corrupt: {d}"),
            Error::Unsupported(d) => write!(f, "unsupported: {d}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn displays() {
        assert_eq!(
            Error::Truncated { needed: 4, got: 2 }.to_string(),
            "truncated: needed 4, got 2"
        );
    }
}
