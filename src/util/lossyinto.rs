/// Trait to allow lossy conversion from a larger to a smaller type,
/// similar to casting to the target type.
pub trait LossyInto<T> {
    fn lossy_into(self) -> T;
}

impl LossyInto<u8> for u32 {
    fn lossy_into(self) -> u8 {
        self as u8
    }
}

impl LossyInto<u16> for u32 {
    fn lossy_into(self) -> u16 {
        self as u16
    }
}

impl LossyInto<u32> for u32 {
    fn lossy_into(self) -> u32 {
        self
    }
}
