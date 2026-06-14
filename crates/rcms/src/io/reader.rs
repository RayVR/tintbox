//! Positioned, big-endian profile reader. Our own trait (not `std::io`) keeps the
//! core's I/O abstract and OS-free — the parse path never touches the filesystem,
//! so the crate compiles cleanly to wasm32 and file access stays behind the
//! `file-io` feature. ICC is big-endian on the wire. `read_at` (positioned) is a
//! deliberate improvement over lcms2's seek-then-read-only IOHANDLER. seek/tell
//! are u64 (lcms2 uses u32) — intentional widening; do not narrow back.

use crate::color::CIEXYZ;
use crate::error::{Error, Result};
use crate::fixed::S15Fixed16;
use crate::sig::Signature;

pub trait ProfileReader {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()>;
    fn seek(&mut self, pos: u64) -> Result<()>;
    fn tell(&self) -> u64;

    /// Positioned read: read `buf.len()` bytes starting at absolute `off`,
    /// leaving the cursor at `off + buf.len()`. Default = seek + read_exact.
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        self.seek(off)?;
        self.read_exact(buf)
    }

    fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn read_u16(&mut self) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }
    fn read_u32(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }
    fn read_u64(&mut self) -> Result<u64> {
        let mut b = [0u8; 8];
        self.read_exact(&mut b)?;
        Ok(u64::from_be_bytes(b))
    }

    fn read_i8(&mut self) -> Result<i8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(i8::from_be_bytes(b))
    }
    fn read_i16(&mut self) -> Result<i16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(i16::from_be_bytes(b))
    }
    fn read_i32(&mut self) -> Result<i32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(i32::from_be_bytes(b))
    }

    /// Read `n` big-endian u16 (lcms2 `_cmsReadUInt16Array`: loop the scalar read).
    fn read_u16_array(&mut self, n: usize) -> Result<Vec<u16>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.read_u16()?);
        }
        Ok(v)
    }
    /// Read `n` big-endian u32 (no lcms2 array primitive; loop the scalar read).
    fn read_u32_array(&mut self, n: usize) -> Result<Vec<u32>> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.read_u32()?);
        }
        Ok(v)
    }

    /// Read `n` bytes as a Latin-1/ASCII string, truncated at the first NUL.
    /// Matches lcms2's `Type_Text_Read` convention (cmstypes.c:925): it reads
    /// `SizeOfTag` bytes, force-terminates with a NUL, and hands the buffer to
    /// `cmsMLUsetASCII`, which copies up to the first NUL. We replicate that:
    /// the value is the bytes before the first NUL, each byte mapped 1:1 to a
    /// `char` (Latin-1), so the result is always valid UTF-8.
    fn read_ascii(&mut self, n: usize) -> Result<String> {
        let mut buf = vec![0u8; n];
        self.read_exact(&mut buf)?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(n);
        Ok(buf[..end].iter().map(|&b| b as char).collect())
    }

    fn read_s15f16(&mut self) -> Result<S15Fixed16> {
        Ok(S15Fixed16::from_raw(self.read_u32()? as i32))
    }

    /// lcms2 `_cmsReadTypeBase` (cmsplugin.c:421): read the 4-byte type signature,
    /// then skip the 4 reserved bytes (`sizeof(_cmsTagBase)` == 8). Returns the sig.
    fn read_type_base(&mut self) -> Result<Signature> {
        let sig = self.read_u32()?;
        let _reserved = self.read_u32()?;
        Ok(Signature::from_raw(sig))
    }

    /// ICC XYZNumber: three s15Fixed16 decoded via `_cms15Fixed16toDouble`
    /// (verified: lcms2 cmsplugin.c uses 15Fixed16toDouble for XYZNumber).
    fn read_xyz(&mut self) -> Result<CIEXYZ> {
        let x = self.read_s15f16()?.to_f64();
        let y = self.read_s15f16()?.to_f64();
        let z = self.read_s15f16()?.to_f64();
        Ok(CIEXYZ { x, y, z })
    }

    /// Skip ICC 4-byte alignment padding, matching lcms2 `_cmsReadAlignment`
    /// (cmsplugin.c:445) exactly: `NextAligned = (At+3) & !3`, `pad = NextAligned - At`.
    /// `pad == 0` → Ok; `pad > 4` → corrupt; otherwise read exactly `pad` bytes in a
    /// single read (truncation surfaces as the natural `read_exact` error).
    fn read_alignment(&mut self) -> Result<()> {
        let at = self.tell();
        let next_aligned = (at + 3) & !3;
        let pad = next_aligned - at;
        if pad == 0 {
            return Ok(());
        }
        if pad > 4 {
            return Err(Error::Corrupt("alignment > 4"));
        }
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf[..pad as usize])
    }
}
