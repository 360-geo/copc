/// Newtype over f64 GPS time. Implements Copy, PartialEq, PartialOrd.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct GpsTime(pub f64);
