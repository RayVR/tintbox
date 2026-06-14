//! Optional std::fs convenience. Disabled for wasm/no-fs builds.
use crate::error::{Error, Result};

/// Read a whole file for parsing via `MemReader`.
pub fn read_file(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|_| Error::Io)
}
