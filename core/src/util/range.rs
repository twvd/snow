use num::traits::{Bounded, Num, One};
use std::cmp::{max, min};
use std::ops::{Add, Range, RangeInclusive, Sub};

/// Extension trait for Range types to include values
pub trait RangeExtension<T> {
    /// Returns a new range that includes the given value
    fn including(self, value: T) -> Self;

    /// Extends the range in-place to include the given value
    fn extend(&mut self, value: T);

    /// Returns the difference between the end and start of the range
    fn span(&self) -> T;
}

/// Implementation for Range<T> (exclusive end) for numeric types
impl<T> RangeExtension<T> for Range<T>
where
    T: Num + Ord + Copy + Add<Output = T> + Bounded + Sub<Output = T> + One,
{
    fn including(self, value: T) -> Self {
        #[cfg(debug_assertions)]
        if value == T::max_value() {
            panic!("Cannot include maximum value in a non-inclusive range (Range<T>): no representable end bound exists");
        }

        let start = min(self.start, value);
        let end = if value >= self.end {
            value + T::one()
        } else {
            self.end
        };
        start..end
    }

    fn extend(&mut self, value: T) {
        #[cfg(debug_assertions)]
        if value == T::max_value() {
            panic!("Cannot include maximum value in a non-inclusive range (Range<T>): no representable end bound exists");
        }

        self.start = min(self.start, value);
        if value >= self.end {
            self.end = value + T::one();
        }
    }

    fn span(&self) -> T {
        self.end - self.start
    }
}

/// Implementation for RangeInclusive<T> (inclusive end)
impl<T> RangeExtension<T> for RangeInclusive<T>
where
    T: Ord + Copy + Sub<Output = T> + One + Add<Output = T>,
{
    fn including(self, value: T) -> Self {
        min(*self.start(), value)..=max(*self.end(), value)
    }

    fn extend(&mut self, value: T) {
        let start = min(*self.start(), value);
        let end = max(*self.end(), value);
        *self = start..=end;
    }

    fn span(&self) -> T {
        *self.end() - *self.start() + T::one()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_including() {
        // Test with value already in range
        let range = 5..10;
        let extended = range.including(7);
        assert_eq!(extended, 5..10);

        // Test with value below start
        let range = 5..10;
        let extended = range.including(3);
        assert_eq!(extended, 3..10);

        // Test with value at end (exclusive)
        let range = 5..10;
        let extended = range.including(10);
        assert_eq!(extended, 5..11);

        // Test with value above end
        let range = 5..10;
        let extended = range.including(15);
        assert_eq!(extended, 5..16);
    }

    #[test]
    fn test_range_extend() {
        // Test with value already in range
        let mut range = 5..10;
        range.extend(7);
        assert_eq!(range, 5..10);

        // Test with value below start
        let mut range = 5..10;
        range.extend(3);
        assert_eq!(range, 3..10);

        // Test with value at end
        let mut range = 5..10;
        range.extend(10);
        assert_eq!(range, 5..11);

        // Test with value above end
        let mut range = 5..10;
        range.extend(15);
        assert_eq!(range, 5..16);
    }

    #[test]
    fn test_inclusive_range_including() {
        // Test with value already in range
        let range = 5..=10;
        let extended = range.including(7);
        assert_eq!(extended, 5..=10);

        // Test with value below start
        let range = 5..=10;
        let extended = range.including(3);
        assert_eq!(extended, 3..=10);

        // Test with value at end (already included)
        let range = 5..=10;
        let extended = range.including(10);
        assert_eq!(extended, 5..=10);

        // Test with value above end
        let range = 5..=10;
        let extended = range.including(15);
        assert_eq!(extended, 5..=15);
    }

    #[test]
    fn test_inclusive_range_extend() {
        // Test with value already in range
        let mut range = 5..=10;
        range.extend(7);
        assert_eq!(range, 5..=10);

        // Test with value below start
        let mut range = 5..=10;
        range.extend(3);
        assert_eq!(range, 3..=10);

        // Test with value at end
        let mut range = 5..=10;
        range.extend(10);
        assert_eq!(range, 5..=10);

        // Test with value above end
        let mut range = 5..=10;
        range.extend(15);
        assert_eq!(range, 5..=15);
    }

    #[test]
    fn test_edge_cases() {
        // Test range with equal bounds
        let range = 5..5;
        let extended = range.including(10);
        assert_eq!(extended, 5..11);

        // Test inclusive range with equal bounds
        let range = 5..=5;
        let extended = range.including(10);
        assert_eq!(extended, 5..=10);

        // Test with i8
        let range = -5i8..5i8;
        let extended = range.including(10i8);
        assert_eq!(extended, -5i8..11i8);

        // Test that including max value in inclusive range works
        let range = 0i8..=100i8;
        let extended = range.including(i8::max_value());
        assert_eq!(extended, 0i8..=i8::max_value());
    }

    #[test]
    #[should_panic(expected = "Cannot include maximum value in a non-inclusive range")]
    fn test_max_value_in_noninclusive_range_including() {
        let range = 0i8..100i8;
        // This should panic
        range.including(i8::max_value());
    }

    #[test]
    #[should_panic(expected = "Cannot include maximum value in a non-inclusive range")]
    fn test_max_value_in_noninclusive_range_extend() {
        let mut range = 0i8..100i8;
        // This should panic
        range.extend(i8::max_value());
    }

    #[test]
    fn test_chaining_operations() {
        // Test chaining multiple including operations
        let range = 5..10;
        let extended = range.including(3).including(15);
        assert_eq!(extended, 3..16);

        // Test incremental extension
        let mut range = 5..=10;
        range.extend(3);
        range.extend(15);
        assert_eq!(range, 3..=15);
    }

    #[test]
    fn test_range_span() {
        // Test span for standard range
        let range = 5..10;
        assert_eq!(range.span(), 5);

        // Test span for empty range
        let range = 5..5;
        assert_eq!(range.span(), 0);

        // Test span for reversed range (invalid but allowed by Rust)
        let range = 10..5;
        assert_eq!(range.span(), -5);
    }

    #[test]
    fn test_inclusive_range_span() {
        // Test span for standard inclusive range
        let range = 5..=10;
        assert_eq!(range.span(), 6); // 10 - 5 + 1 = 6 elements

        // Test span for single-element range
        let range = 5..=5;
        assert_eq!(range.span(), 1); // 5 - 5 + 1 = 1 element
    }
}
