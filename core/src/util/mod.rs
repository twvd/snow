pub mod lossyinto;

/// Type to describe temporal access order
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TemporalOrder {
    HighToLow,
    LowToHigh,
}
