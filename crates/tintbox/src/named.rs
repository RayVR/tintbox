//! Named-color (spot-color) lists, transcribed from lcms2 `src/cmsnamed.c`
//! (the `cmsNAMEDCOLORLIST` portion).
//!
//! A [`NamedColorList`] is an ordered table of spot colors, each carrying a root
//! name plus a fixed PCS Lab triple (3×u16) and `colorant_count` device colorant
//! coordinates (u16). The list also stores a shared `prefix`/`suffix` that apps
//! wrap around the root name. This is the same value the `ncl2` tag decodes to
//! (read by [`crate::profile::types`], written in slice 7), re-exported here with
//! the public list API and the C builder (`cmsAllocNamedColorList` /
//! `cmsAppendNamedColor`).
//!
//! The list feeds the named-color transform path: a transform whose input is a
//! named-color profile maps a color INDEX to either the PCS Lab triple
//! (`EvalNamedColorPCS`) or the device colorants (`EvalNamedColor`) via the
//! [`Stage::NamedColor`](crate::pipeline::Stage::NamedColor) pipeline stage.

pub use crate::profile::tag::{NamedColor, NamedColorList};

/// lcms2 `cmsMAXCHANNELS` (`include/lcms2.h`): the device-colorant channel cap.
pub const MAX_CHANNELS: usize = 16;

/// lcms2 `cmsMAX_PATH` (`include/lcms2.h`): the root-name buffer size. Names are
/// truncated to `cmsMAX_PATH - 1` bytes by `cmsAppendNamedColor`.
pub const MAX_PATH: usize = 256;

impl NamedColorList {
    /// `cmsAllocNamedColorList` (cmsnamed.c:756): create an empty list with the
    /// given device-colorant count and prefix/suffix. Returns `None` when
    /// `colorant_count > cmsMAXCHANNELS` (the C returns NULL).
    ///
    /// lcms2 force-terminates `Prefix`/`Suffix` at index 32 after a
    /// `strncpy(.., sizeof-1)` of 33 bytes, so each affix keeps at most its
    /// leading 32 bytes (the buffer is `char[33]`). We truncate to 32 bytes to
    /// match, stopping at the first interior NUL as `strncpy` would.
    pub fn alloc(colorant_count: usize, prefix: &str, suffix: &str) -> Option<NamedColorList> {
        if colorant_count > MAX_CHANNELS {
            return None;
        }
        Some(NamedColorList {
            vendor_flag: 0,
            prefix: clamp_affix(prefix),
            suffix: clamp_affix(suffix),
            colors: Vec::new(),
            colorant_count,
        })
    }

    /// `cmsAppendNamedColor` (cmsnamed.c:823): push one spot color. `pcs` is the
    /// 3-channel PCS Lab triple (zeros when `None`); `colorant` supplies the first
    /// `colorant_count` device coordinates (zeros when `None` or short). `name`
    /// is the root name, truncated to `cmsMAX_PATH - 1` bytes like the C
    /// `strncpy(.., cmsMAX_PATH-1)`.
    pub fn append(&mut self, name: &str, pcs: Option<[u16; 3]>, colorant: Option<&[u16]>) {
        let mut device = vec![0u16; self.colorant_count];
        if let Some(c) = colorant {
            for (i, d) in device.iter_mut().enumerate() {
                if i < c.len() {
                    *d = c[i];
                }
            }
        }
        let pcs = pcs.unwrap_or([0, 0, 0]);

        // strncpy(.., cmsMAX_PATH-1) then NUL-terminate at cmsMAX_PATH-1: keep at
        // most 255 bytes, stopping at the first interior NUL.
        let name = clamp_name(name);

        self.colors.push(NamedColor { name, pcs, device });
    }
}

/// lcms2's affix store: `char Prefix[33]`, filled by `strncpy(.., 32)` then a
/// hard NUL at index 32. So the kept value is the input up to the first NUL,
/// capped at 32 bytes.
fn clamp_affix(s: &str) -> String {
    clamp_cstr(s, 32)
}

/// lcms2's name store: `char Name[cmsMAX_PATH]` filled by `strncpy(.., 255)`
/// then a hard NUL at index 255. Kept value is the input up to the first NUL,
/// capped at 255 bytes.
fn clamp_name(s: &str) -> String {
    clamp_cstr(s, MAX_PATH - 1)
}

/// `strncpy(dst, src, max)` semantics for the stored string: take up to `max`
/// bytes, stop at the first NUL. Truncates on a UTF-8 boundary to stay valid (the
/// test/builder inputs are ASCII, so the boundary cap never trims mid-codepoint
/// in practice; the cap mirrors the C byte count).
fn clamp_cstr(s: &str, max: usize) -> String {
    let bytes = s.as_bytes();
    let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let mut end = nul.min(max);
    // Keep the slice on a char boundary (defensive; ASCII inputs never hit this).
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_rejects_too_many_colorants() {
        assert!(NamedColorList::alloc(MAX_CHANNELS + 1, "", "").is_none());
        assert!(NamedColorList::alloc(MAX_CHANNELS, "", "").is_some());
    }

    #[test]
    fn append_and_accessors() {
        let mut list = NamedColorList::alloc(2, "PRE", "SUF").unwrap();
        list.append(
            "Cyan",
            Some([0x0101, 0x0202, 0x0303]),
            Some(&[0x1111, 0x2222]),
        );
        list.append("Mag", None, None);

        assert_eq!(list.count(), 2);
        assert_eq!(list.colorant_count(), 2);
        assert_eq!(list.prefix, "PRE");
        assert_eq!(list.suffix, "SUF");

        assert_eq!(list.colors[0].pcs, [0x0101, 0x0202, 0x0303]);
        assert_eq!(list.colors[0].device, vec![0x1111, 0x2222]);
        // Missing PCS/colorant become zeros.
        assert_eq!(list.colors[1].pcs, [0, 0, 0]);
        assert_eq!(list.colors[1].device, vec![0, 0]);

        // index() is case-insensitive (cmsstrcasecmp).
        assert_eq!(list.index("cyan"), Some(0));
        assert_eq!(list.index("MAG"), Some(1));
        assert_eq!(list.index("absent"), None);
    }

    #[test]
    fn append_short_colorant_zero_pads() {
        let mut list = NamedColorList::alloc(3, "", "").unwrap();
        list.append("x", None, Some(&[0xAAAA]));
        assert_eq!(list.colors[0].device, vec![0xAAAA, 0, 0]);
    }
}
