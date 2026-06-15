//! Big-endian profile writer (mirror of ProfileReader).
//!
//! The write side mirrors lcms2's `cmsIOHANDLER` write path. lcms2's serializer
//! (`cmsSaveProfileToIOhandler`, `src/cmsio0.c:1533`) runs TWO passes: pass 1
//! writes through a NULL/counting handler (`cmsOpenIOhandlerFromNULL`) that only
//! advances `UsedSpace`, computing each tag's offset/size and the total; pass 2
//! writes for real. [`CountWriter`] is the counting handler; [`MemWriter`] is
//! the real sink. Both implement [`ProfileWriter`].
//!
//! The fixed-point and structural helpers transcribe lcms2's `cmsplugin.c`
//! primitives: `_cmsWrite15Fixed16Number` (`:337`), `_cmsWriteTypeBase` (`:434`,
//! an 8-byte type-base of the type sig + a zero u32), and `_cmsWriteAlignment`
//! (`:462`, pad 0x00 to the next 4-byte boundary). `position()` plus
//! [`MemWriter::patch_u32`] give the back-patch capability the `mAB`/`mluc`
//! writers need to emit an offset directory ahead of the blocks it points at.

use crate::error::Result;
use crate::fixed::{S15Fixed16, U16Fixed16};

pub trait ProfileWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;

    /// The number of bytes written so far — lcms2's `io->UsedSpace` / `Tell`.
    /// Tag offsets and alignment padding are computed from this.
    fn position(&self) -> usize;

    /// Back-patch a big-endian u32 at a previously recorded position (lcms2's
    /// `Seek(pos)` + `_cmsWriteUInt32Number` + `Seek(end)` dance in the
    /// `mAB`/`mBA`/`mpet` writers, which emit an offset directory ahead of the
    /// blocks it points at). Overwrites bytes already written — it never changes
    /// the total length — so on the counting pass it is a correct no-op (the
    /// default), and on [`MemWriter`] it overwrites in place. Errors via
    /// [`crate::error::Error::Range`] if `pos + 4` exceeds what was written.
    fn patch_u32(&mut self, pos: usize, v: u32) -> Result<()> {
        let _ = (pos, v);
        Ok(())
    }

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

    /// lcms2 `_cmsWriteFloat32Number` (`cmsplugin.c:309`): reinterpret the `f32`'s
    /// bits as a u32 and write big-endian (the union punning + `_cmsAdjustEndianess32`).
    fn write_f32(&mut self, v: f32) -> Result<()> {
        self.write_u32(v.to_bits())
    }

    /// lcms2 `_cmsWrite15Fixed16Number` (`cmsplugin.c:337`): `_cmsDoubleTo15Fixed16`
    /// then big-endian u32.
    fn write_s15fixed16(&mut self, v: f64) -> Result<()> {
        self.write_u32(S15Fixed16::from_f64(v).to_raw() as u32)
    }

    /// Write a pre-encoded s15Fixed16 value (raw fixed bits) big-endian. Used when
    /// the value is already in fixed form (e.g. a parsed `S15Fixed16Array`).
    fn write_s15fixed16_raw(&mut self, v: S15Fixed16) -> Result<()> {
        self.write_u32(v.to_raw() as u32)
    }

    /// Write a pre-encoded u16Fixed16 value (raw fixed bits) big-endian.
    fn write_u16fixed16_raw(&mut self, v: U16Fixed16) -> Result<()> {
        self.write_u32(v.to_raw())
    }

    /// lcms2 `_cmsWriteTypeBase` (`cmsplugin.c:434`): the 8-byte tag-type base —
    /// the type signature big-endian followed by a zeroed u32 (`reserved`).
    fn write_type_base(&mut self, sig: crate::sig::Signature) -> Result<()> {
        self.write_u32(sig.to_raw())?;
        self.write_u32(0)
    }

    /// lcms2 `_cmsWriteAlignment` (`cmsplugin.c:462`): pad with 0x00 up to the next
    /// 4-byte boundary (`_cmsALIGNLONG(At)`). Writes 0..3 zero bytes.
    fn write_alignment(&mut self) -> Result<()> {
        let at = self.position();
        let next = (at + 3) & !3usize; // _cmsALIGNLONG: round up to multiple of 4.
        let pad = next - at;
        for _ in 0..pad {
            self.write_u8(0)?;
        }
        Ok(())
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
    fn position(&self) -> usize {
        self.buf.len()
    }

    /// Back-patch a big-endian u32 at a previously recorded byte position. lcms2's
    /// `mAB`/`mBA`/`mpet` writers emit an offset directory pointing at blocks
    /// written afterward; they record the directory slot's position, write the
    /// blocks, then seek back and overwrite the placeholder with the real offset.
    /// Mirrors a `Seek(pos)` + `Write(u32)` on the underlying IOhandler. Errors
    /// (via `Error::Range`) if `pos + 4` exceeds the buffer.
    fn patch_u32(&mut self, pos: usize, v: u32) -> Result<()> {
        let end = pos.checked_add(4).ok_or(crate::error::Error::Range)?;
        let slot = self
            .buf
            .get_mut(pos..end)
            .ok_or(crate::error::Error::Range)?;
        slot.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
}

/// The counting (NULL) writer — lcms2's `cmsOpenIOhandlerFromNULL` handler. It
/// discards bytes and only tracks `UsedSpace`, so the serializer's pass-1 size
/// computation runs over the SAME code path as pass 2 without allocating a
/// buffer. (lcms2 runs `_cmsWriteHeader` + `SaveTags` through it to learn each
/// tag's offset/size and the total length before the real write.)
#[derive(Default)]
pub struct CountWriter {
    len: usize,
}
impl CountWriter {
    pub fn new() -> Self {
        CountWriter { len: 0 }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
impl ProfileWriter for CountWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.len += bytes.len();
        Ok(())
    }
    fn position(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::Signature;

    #[test]
    fn writes_big_endian() {
        let mut w = MemWriter::new();
        w.write_u32(0x1234_5678).unwrap();
        assert_eq!(w.as_bytes(), &[0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn position_tracks_bytes_written() {
        let mut w = MemWriter::new();
        assert_eq!(w.position(), 0);
        w.write_u32(0).unwrap();
        assert_eq!(w.position(), 4);
        w.write_u8(0).unwrap();
        assert_eq!(w.position(), 5);
    }

    #[test]
    fn alignment_pads_to_four() {
        // At position 5, three pad bytes reach 8.
        let mut w = MemWriter::new();
        w.write_all(&[1, 2, 3, 4, 5]).unwrap();
        w.write_alignment().unwrap();
        assert_eq!(w.as_bytes(), &[1, 2, 3, 4, 5, 0, 0, 0]);
        // Already aligned: no padding.
        let mut w = MemWriter::new();
        w.write_all(&[1, 2, 3, 4]).unwrap();
        w.write_alignment().unwrap();
        assert_eq!(w.as_bytes(), &[1, 2, 3, 4]);
    }

    #[test]
    fn type_base_is_sig_plus_zero() {
        let mut w = MemWriter::new();
        w.write_type_base(Signature::from_bytes(*b"XYZ ")).unwrap();
        assert_eq!(w.as_bytes(), b"XYZ \0\0\0\0");
    }

    #[test]
    fn s15fixed16_matches_double_conversion() {
        // 1.0 -> 0x00010000.
        let mut w = MemWriter::new();
        w.write_s15fixed16(1.0).unwrap();
        assert_eq!(w.as_bytes(), &0x0001_0000u32.to_be_bytes());
    }

    #[test]
    fn count_writer_agrees_with_mem_writer_length() {
        let mut m = MemWriter::new();
        let mut c = CountWriter::new();
        for w in [&mut m as &mut dyn ProfileWriter, &mut c] {
            w.write_type_base(Signature::from_bytes(*b"text")).unwrap();
            w.write_all(b"hello\0").unwrap();
            w.write_alignment().unwrap();
        }
        assert_eq!(m.as_bytes().len(), c.len());
    }

    #[test]
    fn patch_u32_overwrites_in_place() {
        let mut w = MemWriter::new();
        w.write_u32(0).unwrap(); // placeholder at pos 0
        w.write_all(b"body").unwrap();
        w.patch_u32(0, 0xDEAD_BEEF).unwrap();
        assert_eq!(&w.as_bytes()[0..4], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(&w.as_bytes()[4..], b"body");
        // Out of range patch errors.
        assert!(w.patch_u32(100, 0).is_err());
    }
}
