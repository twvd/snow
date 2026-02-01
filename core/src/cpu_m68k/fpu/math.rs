use arpfloat::Float;

pub trait FloatMath {
    fn logbase(&self, base: u64) -> Self;
    fn log2(&self) -> Self;
    fn log10(&self) -> Self;
    fn floor(&self) -> Self;
    fn ceil(&self) -> Self;
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

    /// Round toward negative infinity (floor)
    fn floor(&self) -> Self {
        let truncated = self.trunc();
        if self.is_negative() && *self != truncated {
            truncated - Self::one(self.get_semantics(), false)
        } else {
            truncated
        }
    }

    /// Round toward positive infinity (ceil)
    fn ceil(&self) -> Self {
        let truncated = self.trunc();
        if !self.is_negative() && *self != truncated {
            truncated + Self::one(self.get_semantics(), false)
        } else {
            truncated
        }
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

    #[test]
    fn test_floor() {
        // Positive values
        assert_eq!(Float::from_f64(10000.5).floor().as_f64(), 10000.0);
        assert_eq!(Float::from_f64(10000.9).floor().as_f64(), 10000.0);
        assert_eq!(Float::from_f64(10000.0).floor().as_f64(), 10000.0);
        assert_eq!(Float::from_f64(0.5).floor().as_f64(), 0.0);

        // Negative values
        assert_eq!(Float::from_f64(-10000.5).floor().as_f64(), -10001.0);
        assert_eq!(Float::from_f64(-10000.1).floor().as_f64(), -10001.0);
        assert_eq!(Float::from_f64(-10000.0).floor().as_f64(), -10000.0);
        assert_eq!(Float::from_f64(-0.5).floor().as_f64(), -1.0);
    }

    #[test]
    fn test_ceil() {
        // Positive values
        assert_eq!(Float::from_f64(10000.5).ceil().as_f64(), 10001.0);
        assert_eq!(Float::from_f64(10000.1).ceil().as_f64(), 10001.0);
        assert_eq!(Float::from_f64(10000.0).ceil().as_f64(), 10000.0);
        assert_eq!(Float::from_f64(0.5).ceil().as_f64(), 1.0);

        // Negative values
        assert_eq!(Float::from_f64(-10000.5).ceil().as_f64(), -10000.0);
        assert_eq!(Float::from_f64(-10000.9).ceil().as_f64(), -10000.0);
        assert_eq!(Float::from_f64(-10000.0).ceil().as_f64(), -10000.0);
        assert_eq!(Float::from_f64(-0.5).ceil().as_f64(), 0.0);
    }
}
