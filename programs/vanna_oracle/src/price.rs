use crate::klend::ReserveRate;
use crate::OracleError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::PriceChecks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub value: u64,
    pub exponent: i32,
}

impl Price {
    pub fn new(mut value: u128, mut exponent: i32) -> Result<Self> {
        while value > i64::MAX as u128 {
            value /= 10;
            exponent = exponent.checked_add(1).ok_or(OracleError::MathOverflow)?;
        }
        require!(value > 0, OracleError::InvalidPrice);
        Ok(Self { value: value as u64, exponent })
    }

    pub fn checked_mul(self, other: Price) -> Result<Self> {
        let exponent = self.exponent.checked_add(other.exponent).ok_or(OracleError::MathOverflow)?;
        Self::new(self.value as u128 * other.value as u128, exponent)
    }

    pub fn value_of(self, amount: u128, decimals: u8, round_up: bool) -> Result<u128> {
        normalize_token_value(amount, self.value as i64, self.exponent, decimals, round_up)
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    pub price: Price,
    pub receipt_rate: Option<ReserveRate>,
    pub timestamp: i64,
    pub checks: PriceChecks,
}

impl OraclePrice {
    pub fn value_of(&self, amount: u64, decimals: u8, round_up: bool) -> Result<u128> {
        match self.receipt_rate {
            None => self.price.value_of(amount as u128, decimals, round_up),
            Some(rate) => self.price.value_of(rate.underlying_for_receipts(amount)? as u128, rate.liquidity_decimals, round_up),
        }
    }
}

pub const USD_VALUE_DECIMALS: u32 = 9;

pub fn checked_pow10(exp: u32) -> Result<u128> {
    require!(exp <= 38, OracleError::MathOverflow);
    10u128.checked_pow(exp).ok_or_else(|| OracleError::MathOverflow.into())
}

pub fn mul_div_floor(a: u128, b: u128, denom: u128) -> Result<u128> {
    require!(denom != 0, OracleError::DivisionByZero);
    Ok(a.checked_mul(b).ok_or(OracleError::MathOverflow)? / denom)
}

pub fn mul_div_ceil(a: u128, b: u128, denom: u128) -> Result<u128> {
    require!(denom != 0, OracleError::DivisionByZero);
    let product = a.checked_mul(b).ok_or(OracleError::MathOverflow)?;
    Ok(product.div_ceil(denom))
}

pub fn normalize_token_value(amount: u128, price: i64, price_exponent: i32, token_decimals: u8, round_up: bool) -> Result<u128> {
    require!(price > 0, OracleError::InvalidPrice);
    let base = amount.checked_mul(price as u128).ok_or(OracleError::MathOverflow)?;
    let net_exp = price_exponent as i64 - token_decimals as i64 + USD_VALUE_DECIMALS as i64;
    if net_exp >= 0 {
        let factor = checked_pow10(u32::try_from(net_exp).map_err(|_| OracleError::MathOverflow)?)?;
        base.checked_mul(factor).ok_or_else(|| OracleError::MathOverflow.into())
    } else {
        let factor = checked_pow10(u32::try_from(-net_exp).map_err(|_| OracleError::MathOverflow)?)?;
        Ok(if round_up { base.div_ceil(factor) } else { base / factor })
    }
}
