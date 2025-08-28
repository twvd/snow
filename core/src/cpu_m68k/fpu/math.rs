use arpfloat::Float;

pub trait FloatMath {
    fn logbase(&self, base: u64) -> Self;
    fn log2(&self) -> Self;
    fn log10(&self) -> Self;
}

impl FloatMath for Float {
    fn logbase(&self, base: u64) -> Self {
        self.log() / Self::from_u64(self.get_semantics(), base).log()
    }

    fn log2(&self) -> Self {
        self.logbase(2)
    }

    fn log10(&self) -> Self {
        self.logbase(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arpfloat::Float;

    #[test]
    fn test_log2() {
        let tolerance = 1e-10;
        for i in 1..2000 {
            let f = f64::from(i) / 10.0;
            let error = (Float::from_f64(f).log2().as_f64() - f.log2()).abs();
            assert!(error < tolerance);
        }
    }

    #[test]
    fn test_log10() {
        let tolerance = 1e-15;
        for i in 1..2000 {
            let f = f64::from(i) / 10.0;
            let error = (Float::from_f64(f).log10().as_f64() - f.log10()).abs();
            assert!(error < tolerance);
        }
    }
}
