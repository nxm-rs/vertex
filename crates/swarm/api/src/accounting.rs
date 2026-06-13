//! The accounting unit ([`Au`]) and its boundary conversions.
//!
//! Bandwidth accounting operates in **accounting units (AU)**, an internal
//! economic measure derived from Kademlia proximity. AU is not a byte count and
//! not a token amount in wei. [`Au`] is a newtype over `i64` so that AU values
//! cannot be silently mixed with raw integers, byte counts, or wei: the
//! conversions that bridge those domains are named functions and standard
//! [`TryFrom`] impls on this type and are the only sanctioned crossings. The
//! `U256` crossings are fallible because [`Au`] is a signed `i64` while `U256`
//! is unsigned and far larger, so neither direction is total.
//!
//! # Sign semantics
//!
//! [`Au`] is signed. Balances need a sign: a positive balance means the peer
//! owes us, a negative balance means we owe the peer. Non-negative quantities
//! (prices, thresholds, reserves, allowances, payments) are also carried as
//! [`Au`] but constructed through [`Au::from_amount`] so the non-negativity is
//! intentional rather than an implicit cast.
//!
//! # Arithmetic
//!
//! `Add`, `Sub`, `Neg`, and `Sum` are derived. The two multiplications that
//! historically overflowed silently (the proximity price formula and the
//! pseudosettle `rate * elapsed` allowance) go through [`Au::checked_scale`],
//! which returns `None` on overflow so the caller picks an explicit saturation
//! policy. There is deliberately no `Mul` by a raw integer.
//!
//! # Wire format
//!
//! `Display` and serde render an [`Au`] as a plain integer, byte-identical to
//! the raw `i64`/`u64` representation it replaces. AU is an internal
//! representation only; it never changes the bytes on the wire.

use core::fmt;

use alloy_primitives::U256;
use derive_more::{Add, AddAssign, Neg, Sub, SubAssign, Sum};

/// An amount in accounting units (AU).
///
/// Signed: positive means the peer owes us, negative means we owe the peer.
/// See the [module documentation](self) for sign semantics and the boundary
/// conversions.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Add,
    Sub,
    Neg,
    Sum,
    AddAssign,
    SubAssign,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Au(i64);

impl Au {
    /// The zero amount.
    pub const ZERO: Au = Au(0);

    /// Construct an [`Au`] from a signed raw value.
    ///
    /// Use this for balances, which carry a sign. For non-negative quantities
    /// prefer [`Au::from_amount`] so the non-negativity is explicit.
    #[inline]
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Construct a non-negative [`Au`] from an unsigned amount.
    ///
    /// Saturates at [`i64::MAX`] if the amount does not fit a signed 64-bit
    /// value. Prices, thresholds, reserves, allowances, and payments are
    /// non-negative AU and are built through this constructor.
    #[inline]
    #[must_use]
    pub const fn from_amount(amount: u64) -> Self {
        if amount > i64::MAX as u64 {
            Self(i64::MAX)
        } else {
            Self(amount as i64)
        }
    }

    /// The raw signed value.
    ///
    /// Use only at edges (logs, metrics, the wire, and the boundary
    /// conversions in this module). Do not use it to reintroduce raw-integer
    /// arithmetic on AU.
    #[inline]
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// The amount as an unsigned value, clamping negatives to zero.
    ///
    /// Used at the receive side where a non-negative quantity (an owed amount,
    /// a remaining allowance) must be reported as `u64`.
    #[inline]
    #[must_use]
    pub const fn as_amount(self) -> u64 {
        if self.0 < 0 { 0 } else { self.0 as u64 }
    }

    /// `true` if the amount is strictly negative (we owe the peer).
    #[inline]
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// `true` if the amount is strictly positive (the peer owes us).
    #[inline]
    #[must_use]
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// The absolute value as a non-negative amount.
    #[inline]
    #[must_use]
    pub const fn unsigned_abs(self) -> Au {
        Self(self.0.unsigned_abs() as i64)
    }

    /// Checked addition, `None` on overflow.
    #[inline]
    #[must_use]
    pub const fn checked_add(self, rhs: Au) -> Option<Au> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Checked subtraction, `None` on overflow.
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, rhs: Au) -> Option<Au> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Saturating addition.
    #[inline]
    #[must_use]
    pub const fn saturating_add(self, rhs: Au) -> Au {
        Self(self.0.saturating_add(rhs.0))
    }

    /// Saturating subtraction.
    #[inline]
    #[must_use]
    pub const fn saturating_sub(self, rhs: Au) -> Au {
        Self(self.0.saturating_sub(rhs.0))
    }

    /// The smaller of two amounts.
    #[inline]
    #[must_use]
    pub fn min(self, other: Au) -> Au {
        Self(self.0.min(other.0))
    }

    /// The larger of two amounts.
    #[inline]
    #[must_use]
    pub fn max(self, other: Au) -> Au {
        Self(self.0.max(other.0))
    }

    /// Multiply this amount by an unsigned `factor`, returning `None` on
    /// overflow.
    ///
    /// This is the single audited multiplication for AU. The two call sites
    /// that historically overflowed silently use it: the proximity price
    /// formula (`(max_po - proximity + 1) * base_price`) and the pseudosettle
    /// allowance (`refresh_rate * elapsed`). Each caller chooses an explicit
    /// saturation policy on `None` rather than wrapping or saturating to an
    /// effectively infinite value implicitly.
    #[inline]
    #[must_use]
    pub const fn checked_scale(self, factor: u64) -> Option<Au> {
        // Scaling is only meaningful for non-negative amounts (prices, rates).
        if self.0 < 0 {
            return None;
        }
        let factor = if factor > i64::MAX as u64 {
            return None;
        } else {
            factor as i64
        };
        match self.0.checked_mul(factor) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }
}

impl fmt::Display for Au {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A boundary conversion between [`Au`] and `U256` could not be represented.
///
/// [`Au`] is a signed `i64`; `U256` is unsigned and far larger. Neither
/// direction is total, so the [`TryFrom`] crossings surface the out-of-range
/// value here rather than wrapping or clamping it. The offending value is kept
/// so callers can report it (the swap path maps it onto its own overflow
/// error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuConversionError {
    /// A `U256` exceeded [`i64::MAX`] and cannot be held in an [`Au`].
    U256TooLarge(U256),
    /// A negative [`Au`] has no `U256` representation.
    Negative(Au),
}

impl fmt::Display for AuConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::U256TooLarge(value) => {
                write!(f, "value {value} overflows accounting unit")
            }
            Self::Negative(value) => {
                write!(
                    f,
                    "negative accounting unit {value} has no U256 representation"
                )
            }
        }
    }
}

impl core::error::Error for AuConversionError {}

/// Convert a `U256` (the swap/pseudosettle wire amount) into an [`Au`].
///
/// Fallible: a `U256` can exceed [`i64::MAX`], the largest amount an [`Au`] can
/// hold. Out-of-range values are rejected rather than wrapped so the books stay
/// in sync.
impl TryFrom<U256> for Au {
    type Error = AuConversionError;

    fn try_from(value: U256) -> Result<Self, Self::Error> {
        let raw: u64 = value
            .try_into()
            .map_err(|_| AuConversionError::U256TooLarge(value))?;
        if raw > i64::MAX as u64 {
            return Err(AuConversionError::U256TooLarge(value));
        }
        Ok(Self(raw as i64))
    }
}

/// Convert an [`Au`] into a `U256` wire amount.
///
/// Fallible: a negative [`Au`] (we owe the peer) has no `U256` representation.
/// Non-negative amounts (payments, prices) convert exactly. Callers that build
/// the value through [`Au::from_amount`] know it is non-negative, but the
/// conversion stays fallible so a stray negative balance cannot become a bogus
/// `U256`.
impl TryFrom<Au> for U256 {
    type Error = AuConversionError;

    fn try_from(value: Au) -> Result<Self, Self::Error> {
        if value.0 < 0 {
            return Err(AuConversionError::Negative(value));
        }
        Ok(U256::from(value.0 as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_amount_saturates_above_i64_max() {
        assert_eq!(Au::from_amount(u64::MAX), Au::new(i64::MAX));
        assert_eq!(Au::from_amount(0), Au::ZERO);
        assert_eq!(Au::from_amount(1000), Au::new(1000));
    }

    #[test]
    fn as_amount_clamps_negatives() {
        assert_eq!(Au::new(-5).as_amount(), 0);
        assert_eq!(Au::new(5).as_amount(), 5);
    }

    #[test]
    fn add_sub_neg_compose() {
        assert_eq!(Au::new(10) + Au::new(5), Au::new(15));
        assert_eq!(Au::new(10) - Au::new(5), Au::new(5));
        assert_eq!(-Au::new(10), Au::new(-10));
    }

    #[test]
    fn checked_scale_detects_overflow() {
        assert_eq!(Au::from_amount(100).checked_scale(3), Some(Au::new(300)));
        // The historical infinite-allowance bug: a huge rate times a huge
        // elapsed must report overflow, not saturate to a giant allowance.
        assert_eq!(Au::from_amount(u64::MAX / 2).checked_scale(u64::MAX), None);
        // Negative amounts are not scalable.
        assert_eq!(Au::new(-1).checked_scale(2), None);
    }

    #[test]
    fn checked_add_sub_detect_overflow() {
        assert_eq!(Au::new(i64::MAX).checked_add(Au::new(1)), None);
        assert_eq!(Au::new(i64::MIN).checked_sub(Au::new(1)), None);
        assert_eq!(Au::new(1).checked_add(Au::new(2)), Some(Au::new(3)));
    }

    #[test]
    fn display_is_plain_integer() {
        assert_eq!(Au::new(13_500_000).to_string(), "13500000");
        assert_eq!(Au::new(-42).to_string(), "-42");
    }

    #[test]
    fn u256_au_round_trips_in_range() {
        for amount in [0u64, 1, 1_000, 13_500_000, u64::from(u32::MAX)] {
            let au = Au::try_from(U256::from(amount)).unwrap();
            assert_eq!(au, Au::from_amount(amount));
            assert_eq!(U256::try_from(au).unwrap(), U256::from(amount));
        }
    }

    #[test]
    fn u256_to_au_rejects_above_i64_max() {
        let too_big = U256::from(i64::MAX as u64) + U256::from(1u64);
        assert_eq!(
            Au::try_from(too_big),
            Err(AuConversionError::U256TooLarge(too_big))
        );
        let way_too_big = U256::from(u64::MAX) + U256::from(1u64);
        assert_eq!(
            Au::try_from(way_too_big),
            Err(AuConversionError::U256TooLarge(way_too_big))
        );
    }

    #[test]
    fn au_to_u256_rejects_negative() {
        let neg = Au::new(-1);
        assert_eq!(U256::try_from(neg), Err(AuConversionError::Negative(neg)));
        assert_eq!(U256::try_from(Au::ZERO).unwrap(), U256::ZERO);
    }
}
