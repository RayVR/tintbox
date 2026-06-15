//! PCS / device color value types. Plain f64, Copy, public fields (no invariant).
//! Conversions between spaces are slice 3 (they need a white point).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIEXYZ {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIExyY {
    pub x: f64,
    pub y: f64,
    pub yy: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIELab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIELCh {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JCh {
    pub j: f64,
    pub c: f64,
    pub h: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIEXYZTriple {
    pub red: CIEXYZ,
    pub green: CIEXYZ,
    pub blue: CIEXYZ,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CIExyYTriple {
    pub red: CIExyY,
    pub green: CIExyY,
    pub blue: CIExyY,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructs() {
        assert_eq!(
            CIEXYZ {
                x: 0.9642,
                y: 1.0,
                z: 0.8249
            }
            .y,
            1.0
        );
    }
}
