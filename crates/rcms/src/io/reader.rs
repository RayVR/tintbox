//! Positioned, big-endian profile reader. Our own trait (not std::io) so the core
//! stays no_std-portable; ICC is big-endian on the wire. `read_at` (positioned)
//! is a deliberate improvement over lcms2's seek-then-read-only IOHANDLER. seek/tell
//! are u64 (lcms2 uses u32) — intentional widening; do not narrow back.

use crate::color::CIEXYZ;
use crate::error::Result;
use crate::fixed::S15Fixed16;

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

    fn read_s15f16(&mut self) -> Result<S15Fixed16> {
        Ok(S15Fixed16::from_raw(self.read_u32()? as i32))
    }

    /// ICC XYZNumber: three s15Fixed16 decoded via `_cms15Fixed16toDouble`
    /// (verified: lcms2 cmsplugin.c uses 15Fixed16toDouble for XYZNumber).
    fn read_xyz(&mut self) -> Result<CIEXYZ> {
        let x = self.read_s15f16()?.to_f64();
        let y = self.read_s15f16()?.to_f64();
        let z = self.read_s15f16()?.to_f64();
        Ok(CIEXYZ { x, y, z })
    }

    /// Skip ICC 4-byte alignment padding from the current position.
    fn read_alignment(&mut self) -> Result<()> {
        let pad = (4 - (self.tell() % 4)) % 4;
        for _ in 0..pad {
            self.read_u8()?;
        }
        Ok(())
    }
}
