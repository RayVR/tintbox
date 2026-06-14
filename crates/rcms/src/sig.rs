use core::fmt;

/// A 4-byte ICC signature (big-endian on the wire), stored as the host-order u32
/// of those bytes. Unknown/private signatures round-trip losslessly. The raw
/// field is private: construct via `from_bytes`/`from_raw`, read via `to_raw`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(u32);

impl Signature {
    pub const fn from_bytes(b: [u8; 4]) -> Self { Signature(u32::from_be_bytes(b)) }
    pub const fn from_raw(v: u32) -> Self { Signature(v) }
    pub const fn to_raw(self) -> u32 { self.0 }
    pub const fn to_bytes(self) -> [u8; 4] { self.0.to_be_bytes() }

    pub const LAB_DATA: Signature = Signature::from_bytes(*b"Lab ");
    pub const XYZ_DATA: Signature = Signature::from_bytes(*b"XYZ ");
    pub const RGB_DATA: Signature = Signature::from_bytes(*b"RGB ");
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.to_bytes() {
            let c = byte as char;
            write!(f, "{}", if c.is_ascii_graphic() || c == ' ' { c } else { '?' })?;
        }
        Ok(())
    }
}
impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature(\"{self}\", {:#010x})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fourcc_roundtrip() {
        let s = Signature::from_bytes(*b"Lab ");
        assert_eq!(s.to_raw(), 0x4C61_6220);
        assert_eq!(s.to_string(), "Lab ");
    }
}
