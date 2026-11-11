//! Progress reporting shared by CLI, GUI and library code.

/// A value in the inclusive `0.0..=1.0` range.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Fraction(f32);

impl Fraction {
    pub const ZERO: Self = Self(0.0);
    pub const ONE: Self = Self(1.0);

    /// Builds a fraction, clamping out-of-range and NaN inputs.
    #[must_use]
    pub fn new(value: f32) -> Self {
        if value.is_nan() {
            Self::ZERO
        } else {
            Self(value.clamp(0.0, 1.0))
        }
    }

    /// Computes `done / total`, returning zero when `total` is zero.
    #[must_use]
    pub fn ratio(done: u64, total: u64) -> Self {
        if total == 0 {
            Self::ZERO
        } else {
            Self::new(done as f32 / total as f32)
        }
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Either a measurable progress value or an operation of unknown duration.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Progress {
    #[default]
    Indeterminate,
    Fraction(Fraction),
}

impl Progress {
    #[must_use]
    pub const fn fraction(value: Fraction) -> Self {
        Self::Fraction(value)
    }

    #[must_use]
    pub fn ratio(done: u64, total: u64) -> Self {
        Self::Fraction(Fraction::ratio(done, total))
    }

    #[must_use]
    pub const fn fraction_value(self) -> Option<f32> {
        match self {
            Self::Indeterminate => None,
            Self::Fraction(fraction) => Some(fraction.get()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_out_of_range_values() {
        assert_eq!(Fraction::new(-1.0).get(), 0.0);
        assert_eq!(Fraction::new(2.0).get(), 1.0);
        assert_eq!(Fraction::new(f32::NAN).get(), 0.0);
    }

    #[test]
    fn computes_ratios_safely() {
        assert_eq!(Fraction::ratio(5, 10).get(), 0.5);
        assert_eq!(Fraction::ratio(1, 0).get(), 0.0);
    }
}
