//! The facade's output: a price in one unit whatever its source, and flags saying which quality
//! checks it passed.

use crate::adapters::kamino::ReserveRate;
use crate::errors::VannaError;
use crate::math::health::normalize_token_value;
use anchor_lang::prelude::*;

/// USD per whole token: `value × 10^exponent`, the way Pyth and Scope store prices. `value` is
/// positive and at most `i64::MAX`, so `amount × value` always fits in a `u128`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub value: u64,
    pub exponent: i32,
}

impl Price {
    /// Builds a price from a wider mantissa, dropping trailing digits (rounding down) until it
    /// fits. A zero price is an error: a lending protocol must never value anything at zero.
    pub fn new(mut value: u128, mut exponent: i32) -> Result<Self> {
        while value > i64::MAX as u128 {
            value /= 10;
            exponent = exponent.checked_add(1).ok_or(VannaError::MathOverflow)?;
        }
        require!(value > 0, VannaError::InvalidPrice);
        Ok(Self { value: value as u64, exponent })
    }

    /// `self × other`, rounded down.
    pub fn checked_mul(self, other: Price) -> Result<Self> {
        let exponent = self.exponent.checked_add(other.exponent).ok_or(VannaError::MathOverflow)?;
        Self::new(self.value as u128 * other.value as u128, exponent)
    }

    /// USD value of `amount` raw units, in nano-USD (`USD_VALUE_DECIMALS`), as in the Solidity
    /// `RiskEngine._valueInWei`: price × amount / 10^decimals. Collateral rounds down, debt up.
    pub fn value_of(self, amount: u64, decimals: u8, round_up: bool) -> Result<u128> {
        normalize_token_value(amount, self.value as i64, self.exponent, decimals, round_up)
    }

    /// Whether `|self − other| / self` exceeds `max_bps`. Prices too far apart in magnitude to
    /// compare exactly count as diverging.
    pub fn diverges_from(self, other: Price, max_bps: u16) -> bool {
        let exponent = self.exponent.min(other.exponent);
        let scaled = |p: Price| -> Option<u128> {
            let shift = u32::try_from(p.exponent - exponent).ok()?;
            (p.value as u128).checked_mul(10u128.checked_pow(shift)?)
        };
        let (Some(a), Some(b)) = (scaled(self), scaled(other)) else {
            return true;
        };
        match (a.abs_diff(b).checked_mul(10_000), a.checked_mul(max_bps as u128)) {
            (Some(diff), Some(limit)) => diff > limit,
            _ => true,
        }
    }
}

/// Quality checks a price passed, as in Kamino's `PriceStatusFlags`. A failed check does not fail
/// the read: each instruction decides which checks it needs, so a TWAP alarm during a crash can
/// block borrowing without blocking liquidations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceStatus(pub u8);

impl PriceStatus {
    pub const NONE: Self = Self(0);
    /// No older than the asset's `max_age_secs`.
    pub const FRESH: Self = Self(1);
    /// Within `max_twap_divergence_bps` of a fresh TWAP (or the check is disabled).
    pub const TWAP_OK: Self = Self(1 << 1);
    /// Pyth confidence within `max_confidence_bps` (Scope prices carry no confidence).
    pub const CONFIDENCE_OK: Self = Self(1 << 2);
    /// Borrowing, and withdrawing or trading while in debt.
    pub const ALL_CHECKS: Self = Self(Self::FRESH.0 | Self::TWAP_OK.0 | Self::CONFIDENCE_OK.0);
    /// Liquidation: a fresh price is enough.
    pub const LIQUIDATION_CHECKS: Self = Self::FRESH;

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The checks both prices passed: an account's status is its weakest asset's.
    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn with(self, flag: Self, set: bool) -> Self {
        if set {
            Self(self.0 | flag.0)
        } else {
            self
        }
    }

    /// Fails with the first `required` check this status is missing.
    pub fn require(self, required: Self) -> Result<()> {
        let missing = Self(required.0 & !self.0);
        require!(!missing.contains(Self::FRESH), VannaError::StalePrice);
        require!(!missing.contains(Self::TWAP_OK), VannaError::PriceTooDivergentFromTwap);
        require!(!missing.contains(Self::CONFIDENCE_OK), VannaError::ConfidenceTooWide);
        Ok(())
    }
}

/// An asset's price from the facade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    /// USD per whole token of the asset, or of its underlying for a Kamino receipt.
    pub price: Price,
    /// A Kamino receipt's exchange rate: amounts convert to underlying units first, as klend does.
    pub receipt_rate: Option<ReserveRate>,
    /// Publish time of the source price (the oldest input for a chain).
    pub timestamp: i64,
    pub status: PriceStatus,
}

impl OraclePrice {
    /// USD value of `amount` raw units of the asset (`decimals` = its mint's), in nano-USD: the
    /// Solidity `RiskEngine._valueInWei`. Collateral rounds down, debt up.
    pub fn value_of(&self, amount: u64, decimals: u8, round_up: bool) -> Result<u128> {
        match self.receipt_rate {
            None => self.price.value_of(amount, decimals, round_up),
            Some(rate) => self.price.value_of(rate.underlying_for_receipts(amount)?, rate.liquidity_decimals, round_up),
        }
    }
}
