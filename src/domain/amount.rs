use anyhow::{Context, Result};
use num_bigint::{BigInt, Sign};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};
use tracing::field::Visit;

const SCALE: i64 = 10_000;

/// Fixed-point amount with 4 decimal places of precision.
///
/// Internally stores value × 10^4 as a `BigInt`.
/// Display and serialization produce the decimal form (e.g. "5.1234").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Amount(BigInt);

impl Amount {
    /// Returns an amount representing zero.
    pub fn zero() -> Self {
        Amount(BigInt::ZERO)
    }

    /// Create from whole units (e.g. `from_major(5)` → "5.0").
    pub fn from_major(units: i64) -> Self {
        Amount(BigInt::from(units * SCALE))
    }

    /// Create from an already-scaled BigInt (internal representation).
    pub fn from_scaled(raw: BigInt) -> Self {
        Amount(raw)
    }

    /// Parse a decimal string like "5.1234" into an Amount.
    /// Truncates beyond 4 decimal places.
    pub fn parse(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        if let Some(dot_pos) = trimmed.find('.') {
            let int_part = &trimmed[..dot_pos];
            let frac_part = &trimmed[dot_pos + 1..];
            let frac_4: String = frac_part
                .chars()
                .take(4)
                .chain(std::iter::repeat('0'))
                .take(4)
                .collect();
            let combined = format!("{}{}", int_part, frac_4);
            let val: i64 = combined.parse().context("Invalid amount")?;
            Ok(Amount(BigInt::from(val)))
        } else {
            let val: i64 = trimmed.parse().context("Invalid amount")?;
            Ok(Amount(BigInt::from(val * SCALE)))
        }
    }

    /// Returns `true` if the amount is strictly greater than zero.
    pub fn is_positive(&self) -> bool {
        self.0.sign() == Sign::Plus
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scale = BigInt::from(SCALE);
        let is_negative = self.0.sign() == Sign::Minus;
        let abs = if is_negative {
            -&self.0
        } else {
            self.0.clone()
        };
        let div = &abs / &scale;
        let rem = &abs % &scale;
        let frac = format!("{:0>4}", rem);
        if is_negative {
            write!(f, "-{}.{}", div, frac)
        } else {
            write!(f, "{}.{}", div, frac)
        }
    }
}

impl<'a, 'b> Add<&'b Amount> for &'a Amount {
    type Output = Amount;
    fn add(self, other: &'b Amount) -> Amount {
        Amount(&self.0 + &other.0)
    }
}

impl<'a> Neg for &'a Amount {
    type Output = Amount;

    fn neg(self) -> Self::Output {
        Amount(-&self.0)
    }
}

impl AddAssign<&Amount> for Amount {
    fn add_assign(&mut self, other: &Amount) {
        self.0 += &other.0;
    }
}

impl<'a, 'b> Sub<&'b Amount> for &'a Amount {
    type Output = Amount;
    fn sub(self, other: &'b Amount) -> Amount {
        Amount(&self.0 - &other.0)
    }
}

impl SubAssign<&Amount> for Amount {
    fn sub_assign(&mut self, other: &Amount) {
        self.0 -= &other.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse() {
        assert_eq!(Amount::parse("5").unwrap(), Amount::from_major(5));
        assert_eq!(
            Amount::parse("5.1").unwrap(),
            Amount::from_scaled(BigInt::from(51000))
        );
        assert_eq!(
            Amount::parse("5.1234").unwrap(),
            Amount::from_scaled(BigInt::from(51234))
        );
        assert_eq!(
            Amount::parse("5.12341").unwrap(),
            Amount::from_scaled(BigInt::from(51234))
        );
        assert_eq!(Amount::parse("0.00001").unwrap(), Amount::zero());
    }

    #[test]
    fn test_parse_negative() {
        assert_eq!(Amount::parse("-5").unwrap(), Amount::from_major(-5));
        assert_eq!(
            Amount::parse("-5.1234").unwrap(),
            Amount::from_scaled(BigInt::from(-51234))
        );
        assert_eq!(Amount::parse("-0.00001").unwrap(), Amount::zero());
    }

    #[test]
    fn test_parse_whitespace() {
        assert_eq!(Amount::parse("  5  ").unwrap(), Amount::from_major(5));
        assert_eq!(
            Amount::parse(" 5.1234 ").unwrap(),
            Amount::from_scaled(BigInt::from(51234))
        );
    }

    #[test]
    fn test_parse_invalid() {
        assert!(Amount::parse("").is_err());
        assert!(Amount::parse("abc").is_err());
        assert!(Amount::parse("1.2.3").is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(Amount::from_major(5).to_string(), "5.0000");
        assert_eq!(
            Amount::from_scaled(BigInt::from(51234)).to_string(),
            "5.1234"
        );
        assert_eq!(
            Amount::from_scaled(BigInt::from(51000)).to_string(),
            "5.1000"
        );
        assert_eq!(Amount::zero().to_string(), "0.0000");
        assert_eq!(Amount::from_major(-5).to_string(), "-5.0000");
        assert_eq!(
            Amount::from_scaled(BigInt::from(-51234)).to_string(),
            "-5.1234"
        );
    }

    #[test]
    fn test_is_positive() {
        assert!(Amount::from_major(1).is_positive());
        assert!(!Amount::zero().is_positive());
        assert!(!Amount::from_major(-1).is_positive());
    }

    #[test]
    fn test_arithmetic() {
        let a = Amount::from_major(10);
        let b = Amount::from_major(3);
        assert_eq!(&a + &b, Amount::from_major(13));
        assert_eq!(&a - &b, Amount::from_major(7));

        let mut c = Amount::from_major(5);
        c += &b;
        assert_eq!(c, Amount::from_major(8));
        c -= &b;
        assert_eq!(c, Amount::from_major(5));
    }

    #[test]
    fn test_ordering() {
        assert!(Amount::from_major(10) > Amount::from_major(5));
        assert!(Amount::from_major(5) < Amount::from_major(10));
        assert!(Amount::from_major(0) >= Amount::zero());
    }
}
