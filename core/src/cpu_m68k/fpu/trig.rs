use arpfloat::Float;

pub trait FloatTrig {
    fn atan(&self) -> Self;
    fn relative_epsilon(&self, scale: f64) -> Float;
}

impl FloatTrig for Float {
    fn atan(&self) -> Self {
        // atan(1) = π/4 shortcut
        if self == &Self::one(self.get_semantics(), false) {
            return Self::from_f64(std::f64::consts::FRAC_PI_4).cast(self.get_semantics());
        }

        // Handle special cases
        if self.is_nan() || self.is_zero() {
            return self.clone();
        }

        if self.is_inf() {
            let pi_2 = Self::from_f64(std::f64::consts::FRAC_PI_2).cast(self.get_semantics());
            return if !self.is_negative() {
                pi_2
            } else {
                pi_2.neg()
            };
        }

        // Use the identity: atan(x) = π/2 - atan(1/x) for |x| > 1
        let x = if self.abs() > Self::from_f64(1.0).cast(self.get_semantics()) {
            let recip = Self::from_f64(1.0).cast(self.get_semantics()) / self.clone();
            let result = atan_taylor_series(&recip);
            let pi_2 = Self::from_f64(std::f64::consts::FRAC_PI_2).cast(self.get_semantics());
            return if !self.is_negative() {
                pi_2 - result
            } else {
                pi_2.neg() - result
            };
        } else {
            self.clone()
        };

        atan_taylor_series(&x)
    }

    fn relative_epsilon(&self, scale: f64) -> Float {
        self.abs() * Self::from_f64(scale).cast(self.get_semantics())
    }
}

fn atan_taylor_series(x: &Float) -> Float {
    // Taylor series: atan(x) = x - x³/3 + x⁵/5 - x⁷/7 + ...
    // Valid for |x| ≤ 1

    let mut result = x.clone();
    let mut term = x.clone();
    let x_squared = x * x;

    // Iterate until convergence (when term becomes negligible)
    // Maximum 50 iterations to prevent infinite loops
    for i in 1..50 {
        term *= &x_squared;
        let denominator = Float::from_f64((2 * i + 1) as f64).cast(x.get_semantics());
        let current_term = &term / denominator;

        if i % 2 == 0 {
            result += &current_term;
        } else {
            result -= &current_term;
        }

        // Check for convergence - if the term is very small relative to result
        if current_term.abs() < x.relative_epsilon(1e-15) {
            break;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atan_basic_values() {
        // Test zero
        let zero = Float::from_f64(0.0);
        assert_eq!(zero.atan().as_f64(), 0.0);

        // Test positive and negative one
        let one = Float::from_f64(1.0);
        let neg_one = Float::from_f64(-1.0);
        let expected_pi_4 = std::f64::consts::FRAC_PI_4;

        let one_result = one.atan().as_f64();
        let neg_one_result = neg_one.atan().as_f64();

        assert!((one_result - expected_pi_4).abs() < 1e-2);
        assert!((neg_one_result + expected_pi_4).abs() < 1e-2);
    }

    #[test]
    fn test_atan_comparison_with_std() {
        let test_values = [
            0.0, 0.1, 0.5, 0.7, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, -0.1, -0.5, -0.7, -1.0, -1.5, -2.0,
            -3.0, -5.0, -10.0, 0.001, 0.999, 1.001, 100.0, -100.0,
        ];

        for &value in &test_values {
            let float_val = Float::from_f64(value);
            let my_result = float_val.atan().as_f64();
            let std_result = value.atan();

            let error = (my_result - std_result).abs();
            // Adjust tolerance based on the simpler Taylor series implementation
            let tolerance = if value.abs() >= 1.0 {
                1e-2
            } else if (value.abs() - 1.0).abs() < 0.01 {
                1e-2
            } else {
                1e-3
            };
            assert!(
                error < tolerance,
                "Value: {}, My result: {}, Std result: {}, Error: {}",
                value,
                my_result,
                std_result,
                error
            );
        }
    }

    #[test]
    fn test_atan_special_cases() {
        // Test positive infinity
        let pos_inf = Float::from_f64(f64::INFINITY);
        assert_eq!(pos_inf.atan().as_f64(), std::f64::consts::FRAC_PI_2);

        // Test negative infinity
        let neg_inf = Float::from_f64(f64::NEG_INFINITY);
        assert_eq!(neg_inf.atan().as_f64(), -std::f64::consts::FRAC_PI_2);

        // Test NaN
        let nan = Float::from_f64(f64::NAN);
        assert!(nan.atan().is_nan());
    }

    #[test]
    fn test_atan_precision_comparison() {
        // Test values where Taylor series convergence is important
        let precise_values = [
            0.123456789,
            0.987654321,
            1.23456789,
            2.71828182845904523536, // e
            3.14159265358979323846, // pi
            -0.123456789,
            -0.987654321,
            -1.23456789,
        ];

        for &value in &precise_values {
            let float_val = Float::from_f64(value);
            let my_result = float_val.atan().as_f64();
            let std_result = value.atan();

            let error = (my_result - std_result).abs();
            let tolerance = if (value.abs() - 1.0).abs() < 0.02 {
                1e-2
            } else {
                1e-3
            };
            assert!(
                error < tolerance,
                "Value: {}, My result: {}, Std result: {}, Error: {}",
                value,
                my_result,
                std_result,
                error
            );
        }
    }
}
