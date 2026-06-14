//! In-memory reader over a byte slice — the wasm-friendly default source.

use crate::error::{Error, Result};
use crate::io::reader::ProfileReader;

pub struct MemReader<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> MemReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        MemReader { buf, pos: 0 }
    }
}
impl ProfileReader for MemReader<'_> {
    fn read_exact(&mut self, out: &mut [u8]) -> Result<()> {
        let end = self.pos.checked_add(out.len()).ok_or(Error::Range)?;
        if end > self.buf.len() {
            return Err(Error::Truncated {
                needed: out.len() as u32,
                got: (self.buf.len() - self.pos) as u32,
            });
        }
        out.copy_from_slice(&self.buf[self.pos..end]);
        self.pos = end;
        Ok(())
    }
    fn seek(&mut self, pos: u64) -> Result<()> {
        let p = pos as usize;
        if p > self.buf.len() {
            return Err(Error::Range);
        }
        self.pos = p;
        Ok(())
    }
    fn tell(&self) -> u64 {
        self.pos as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn be_reads_match_oracle() {
        let data: [u8; 8] = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
        let mut r = MemReader::new(&data);
        assert_eq!(r.read_u16().unwrap(), 0x1234);
        assert_eq!(Some(0x1234), rcms_oracle::read_u16(&data));
        let mut r = MemReader::new(&data);
        assert_eq!(r.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(Some(0x1234_5678), rcms_oracle::read_u32(&data));
    }
    #[test]
    fn positioned_read_does_not_disturb_then_continues() {
        let data: [u8; 8] = [0, 0, 0, 0, 0xAA, 0xBB, 0xCC, 0xDD];
        let mut r = MemReader::new(&data);
        let mut b = [0u8; 4];
        r.read_at(4, &mut b).unwrap();
        assert_eq!(b, [0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(r.tell(), 8);
    }
    #[test]
    fn truncation_is_an_error() {
        let data = [0u8; 1];
        assert!(MemReader::new(&data).read_u32().is_err());
    }
    #[test]
    fn alignment_pads_correctly() {
        let data = [0u8; 8];
        let mut r = MemReader::new(&data);
        r.read_u8().unwrap();
        r.read_alignment().unwrap();
        assert_eq!(r.tell(), 4);
        r.read_alignment().unwrap();
        assert_eq!(r.tell(), 4);
    }
}
