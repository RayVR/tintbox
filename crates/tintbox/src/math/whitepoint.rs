//! D50 reference white. lcms2 ships ONLY D50 as a constant (include/lcms2.h:292-294);
//! D65 etc. are COMPUTED via cmsWhitePointFromTemp (slice 3), not constants.

use crate::color::CIEXYZ;

/// lcms2 `cmsD50X/Y/Z`.
pub const D50: CIEXYZ = CIEXYZ {
    x: 0.9642,
    y: 1.0,
    z: 0.8249,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn d50_values() {
        assert_eq!((D50.x, D50.y, D50.z), (0.9642, 1.0, 0.8249));
    }
}
