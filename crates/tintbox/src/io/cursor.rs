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
        assert_eq!(Some(0x1234), tintbox_oracle::read_u16(&data));
        let mut r = MemReader::new(&data);
        assert_eq!(r.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(Some(0x1234_5678), tintbox_oracle::read_u32(&data));
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

    #[test]
    fn signed_reads_known_patterns() {
        // i8
        let mut r = MemReader::new(&[0xFFu8]);
        assert_eq!(r.read_i8().unwrap(), -1);
        let mut r = MemReader::new(&[0x80u8]);
        assert_eq!(r.read_i8().unwrap(), -128);
        let mut r = MemReader::new(&[0x7Fu8]);
        assert_eq!(r.read_i8().unwrap(), 127);
        // i16 (big-endian)
        let mut r = MemReader::new(&[0xFF, 0xFE]);
        assert_eq!(r.read_i16().unwrap(), -2);
        let mut r = MemReader::new(&[0x80, 0x00]);
        assert_eq!(r.read_i16().unwrap(), i16::MIN);
        // i32 (big-endian)
        let mut r = MemReader::new(&[0xFF, 0xFF, 0xFF, 0xFE]);
        assert_eq!(r.read_i32().unwrap(), -2);
        let mut r = MemReader::new(&[0x80, 0x00, 0x00, 0x00]);
        assert_eq!(r.read_i32().unwrap(), i32::MIN);
    }

    #[test]
    fn read_u16_array_matches_oracle() {
        let data: [u8; 8] = [0x00, 0x01, 0xAB, 0xCD, 0xFF, 0xFF, 0x12, 0x34];
        let n = 4;
        let mut r = MemReader::new(&data);
        let got = r.read_u16_array(n).unwrap();
        // Compare to looping the scalar oracle over advancing slices.
        let mut expect = Vec::new();
        for i in 0..n {
            expect.push(tintbox_oracle::read_u16(&data[i * 2..]).unwrap());
        }
        assert_eq!(got, expect);
        // Also compare against the array oracle primitive directly.
        assert_eq!(got, tintbox_oracle::read_u16_array(&data, n).unwrap());
        assert_eq!(r.tell(), 8);
    }

    #[test]
    fn read_u32_array_matches_oracle() {
        let data: [u8; 12] = [
            0x00, 0x00, 0x00, 0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56, 0x78,
        ];
        let n = 3;
        let mut r = MemReader::new(&data);
        let got = r.read_u32_array(n).unwrap();
        let mut expect = Vec::new();
        for i in 0..n {
            expect.push(tintbox_oracle::read_u32(&data[i * 4..]).unwrap());
        }
        assert_eq!(got, expect);
        assert_eq!(r.tell(), 12);
    }

    #[test]
    fn read_alignment_matches_oracle() {
        // 8-byte buffer; align from each offset whose tell%4 ∈ {0,1,2,3}.
        let data = [0u8; 8];
        for offset in 0u32..8 {
            let (oracle_ok, oracle_tell) = tintbox_oracle::read_alignment(&data, offset);
            let mut r = MemReader::new(&data);
            r.seek(offset as u64).unwrap();
            let res = r.read_alignment();
            assert_eq!(
                res.is_ok(),
                oracle_ok,
                "ok mismatch at offset {offset}: rust={res:?} oracle_ok={oracle_ok}"
            );
            if oracle_ok {
                assert_eq!(
                    r.tell() as u32,
                    oracle_tell,
                    "tell mismatch at offset {offset}"
                );
            }
        }
    }

    #[test]
    fn read_alignment_truncated_matches_oracle() {
        // Buffer of length 6: seeking to offset 5 needs 3 pad bytes to reach 8,
        // but only 1 byte remains -> truncated. lcms2's single Read returns FALSE.
        let data = [0u8; 6];
        let offset = 5u32;
        let (oracle_ok, _) = tintbox_oracle::read_alignment(&data, offset);
        assert!(!oracle_ok, "oracle should fail on truncated pad");
        let mut r = MemReader::new(&data);
        r.seek(offset as u64).unwrap();
        assert!(r.read_alignment().is_err());
    }

    #[test]
    fn read_type_base_matches_oracle() {
        // 'mft2' type sig followed by 4 reserved bytes.
        let data: [u8; 8] = [b'm', b'f', b't', b'2', 0x00, 0x00, 0x00, 0x00];
        let mut r = MemReader::new(&data);
        let sig = r.read_type_base().unwrap();
        assert_eq!(sig, crate::sig::Signature::from_bytes(*b"mft2"));
        assert_eq!(sig.to_raw(), tintbox_oracle::read_type_base(&data));
        assert_eq!(r.tell(), 8, "type base consumes sizeof(_cmsTagBase)==8");
    }

    #[test]
    fn read_ascii_trims_at_nul() {
        // "Hi\0junk" within a 7-byte field -> "Hi".
        let data = [b'H', b'i', 0x00, b'j', b'u', b'n', b'k'];
        let mut r = MemReader::new(&data);
        assert_eq!(r.read_ascii(7).unwrap(), "Hi");
        assert_eq!(r.tell(), 7, "consumes the full field even past the NUL");
        // No NUL -> whole field.
        let data = [b'A', b'B', b'C'];
        let mut r = MemReader::new(&data);
        assert_eq!(r.read_ascii(3).unwrap(), "ABC");
    }
}
