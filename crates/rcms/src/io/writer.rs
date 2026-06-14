//! Big-endian profile writer (mirror of ProfileReader).
use crate::error::Result;

pub trait ProfileWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
    fn write_u8(&mut self, v: u8) -> Result<()> {
        self.write_all(&[v])
    }
    fn write_u16(&mut self, v: u16) -> Result<()> {
        self.write_all(&v.to_be_bytes())
    }
    fn write_u32(&mut self, v: u32) -> Result<()> {
        self.write_all(&v.to_be_bytes())
    }
    fn write_u64(&mut self, v: u64) -> Result<()> {
        self.write_all(&v.to_be_bytes())
    }
}

/// Grows a Vec<u8> — the in-memory sink. Buffer is private; take it via into_inner().
#[derive(Default)]
pub struct MemWriter {
    buf: Vec<u8>,
}
impl MemWriter {
    pub fn new() -> Self {
        MemWriter { buf: Vec::new() }
    }
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }
}
impl ProfileWriter for MemWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_big_endian() {
        let mut w = MemWriter::new();
        w.write_u32(0x1234_5678).unwrap();
        assert_eq!(w.as_bytes(), &[0x12, 0x34, 0x56, 0x78]);
    }
}
