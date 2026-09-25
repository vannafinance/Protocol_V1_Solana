use super::fixed_point::{checked_pow10, mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::constants::{BALANCE_TO_BORROW_THRESHOLD_WAD, USD_VALUE_DECIMALS, WAD};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// USD value of a raw token amount, in nano-USD (`USD_VALUE_DECIMALS`).
///
/// value = token_amount * price * 10^(price_exponent - token_decimals + USD_VALUE_DECIMALS)
///
/// Round collateral down (`round_up = false`) and debt up (`round_up = true`).
pub fn normalize_token_value(
    token_amount: u64,
    price: i64,
    price_exponent: i32,
    token_decimals: u8,
    round_up: bool,
) -> Result<u128> {
    require!(price > 0, VannaError::InvalidPrice);

    let base = (token_amount as u128)
        .checked_mul(price as u128)
        .ok_or(VannaError::MathOverflow)?;

    let net_exp = (price_exponent as i64) - (token_decimals as i64) + (USD_VALUE_DECIMALS as i64);

    if net_exp >= 0 {
        let factor = checked_pow10(u32::try_from(net_exp).map_err(|_| VannaError::MathOverflow)?)?;
        base.checked_mul(factor).ok_or_else(|| VannaError::MathOverflow.into())
    } else {
        let factor = checked_pow10(u32::try_from(-net_exp).map_err(|_| VannaError::MathOverflow)?)?;
        if round_up {
            let factor_minus_one = factor.checked_sub(1).ok_or(VannaError::MathOverflow)?;
            let numerator = base.checked_add(factor_minus_one).ok_or(VannaError::MathOverflow)?;
            Ok(numerator.checked_div(factor).ok_or(VannaError::MathOverflow)?)
        } else {
            Ok(base.checked_div(factor).ok_or(VannaError::MathOverflow)?)
        }
    }
}

/// Inverse of `normalize_token_value`: converts a USD value back into raw token units.
/// Liquidation uses it to turn a seize value into a seize token amount.
///
/// token_amount = value / (price * 10^(price_exponent - token_decimals + USD_VALUE_DECIMALS))
pub fn value_to_token_amount(
    value: u128,
    price: i64,
    price_exponent: i32,
    token_decimals: u8,
    round_up: bool,
) -> Result<u64> {
    require!(price > 0, VannaError::InvalidPrice);

    let net_exp = (price_exponent as i64) - (token_decimals as i64) + (USD_VALUE_DECIMALS as i64);
    let (numerator_factor, denom_factor) = if net_exp < 0 {
        (checked_pow10(u32::try_from(-net_exp).map_err(|_| VannaError::MathOverflow)?)?, 1u128)
    } else {
        (1u128, checked_pow10(u32::try_from(net_exp).map_err(|_| VannaError::MathOverflow)?)?)
    };
    let denom = (price as u128).checked_mul(denom_factor).ok_or(VannaError::MathOverflow)?;

    let result = if round_up {
        mul_div_ceil(value, numerator_factor, denom)?
    } else {
        mul_div_floor(value, numerator_factor, denom)?
    };
    u64_from_u128(result)
}

/// One collateral asset's contribution to a health snapshot: its full oracle USD value,
/// rounded down.
#[derive(Clone, Copy)]
pub struct CollateralValuation {
    pub collateral_value: u128,
}

/// One debt position's contribution to a health snapshot: its current USD value, rounded up.
#[derive(Clone, Copy)]
pub struct DebtValuation {
    pub debt_value: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub borrow_power: u128,
    pub liquidation_collateral_value: u128,
    pub total_debt_value: u128,
    pub borrow_health_factor_wad: u128,
    pub liquidation_health_factor_wad: u128,
}

impl HealthSnapshot {
    /// healthy = total_debt == 0 || health_factor > 1.10
    pub fn is_borrow_healthy(&self) -> bool {
        self.total_debt_value == 0
            || self.borrow_health_factor_wad > BALANCE_TO_BORROW_THRESHOLD_WAD
    }

    /// liquidatable = total_debt > 0 && health_factor <= 1.10   (exactly 1.10 is liquidatable)
    pub fn is_liquidatable(&self) -> bool {
        self.total_debt_value > 0
            && self.liquidation_health_factor_wad <= BALANCE_TO_BORROW_THRESHOLD_WAD
    }
}

/// health_factor = collateral * WAD / debt, or `u128::MAX` (infinitely healthy) with no debt.
/// Overflow is an error, never "infinite health", so an oversized value fails closed.
fn health_factor_wad(collateral_value: u128, debt_value: u128) -> Result<u128> {
    if debt_value == 0 {
        Ok(u128::MAX)
    } else {
        mul_div_floor(collateral_value, WAD, debt_value)
    }
}

/// Account health factor (WAD), matching the Solidity and Soroban implementations.
///
/// health_factor = sum(collateral_usd) / sum(debt_usd)
/// healthy       = total_debt == 0 || health_factor > 1.10
///
/// Borrowed funds stay in the margin account and count as collateral, so a borrow raises both
/// sides of the ratio. The caller must pass every active position.
pub fn calculate_health(
    collaterals: &[CollateralValuation],
    debts: &[DebtValuation],
) -> Result<HealthSnapshot> {
    let mut total_collateral_value: u128 = 0;
    for c in collaterals {
        total_collateral_value = total_collateral_value
            .checked_add(c.collateral_value)
            .ok_or(VannaError::MathOverflow)?;
    }

    let mut total_debt_value: u128 = 0;
    for d in debts {
        total_debt_value = total_debt_value
            .checked_add(d.debt_value)
            .ok_or(VannaError::MathOverflow)?;
    }

    let health_factor = health_factor_wad(total_collateral_value, total_debt_value)?;

    Ok(HealthSnapshot {
        // Both fields hold the same total: one account-level threshold means no separate
        // borrow-power discount. The names stay for event/client compatibility.
        borrow_power: total_collateral_value,
        liquidation_collateral_value: total_collateral_value,
        total_debt_value,
        borrow_health_factor_wad: health_factor,
        liquidation_health_factor_wad: health_factor,
    })
}
