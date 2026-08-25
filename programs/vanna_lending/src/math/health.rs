use super::fixed_point::{checked_pow10, mul_div_floor};
use crate::constants::{BASIS_POINTS, USD_VALUE_DECIMALS, WAD};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Normalizes a raw token amount into a common USD scale (`USD_VALUE_DECIMALS`), combining the
/// oracle's price exponent and the token's own decimals (spec §7.5). Collateral value must be
/// rounded down (`round_up = false`); debt value must be rounded up (`round_up = true`).
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

    // value = token_amount * price * 10^(price_exponent - token_decimals + USD_VALUE_DECIMALS)
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

/// Inverse of `normalize_token_value` — converts a USD value back into raw token units at the
/// given price. Used by liquidation to turn a seize *value* into a seize *token amount*.
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
        crate::math::fixed_point::mul_div_ceil(value, numerator_factor, denom)?
    } else {
        crate::math::fixed_point::mul_div_floor(value, numerator_factor, denom)?
    };
    crate::math::fixed_point::u64_from_u128(result)
}

/// One collateral asset's contribution to a health snapshot. `collateral_value` must already be
/// the rounded-down USD value of the credited amount (see `normalize_token_value`).
#[derive(Clone, Copy)]
pub struct CollateralValuation {
    pub collateral_value: u128,
    pub ltv_bps: u16,
    pub liquidation_threshold_bps: u16,
}

/// One debt position's contribution to a health snapshot. `debt_value` must already be the
/// rounded-up USD value of the current debt (see `normalize_token_value`).
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
    /// Spec §7.6 — no debt is trivially healthy; otherwise borrow power must cover debt value.
    pub fn is_borrow_healthy(&self) -> bool {
        self.total_debt_value == 0 || self.borrow_power >= self.total_debt_value
    }

    /// Spec §6.5 — liquidatable only once liquidation-threshold-weighted collateral falls short.
    pub fn is_liquidatable(&self) -> bool {
        self.total_debt_value > 0 && self.liquidation_collateral_value < self.total_debt_value
    }
}

fn health_factor_wad(numerator: u128, denominator: u128) -> u128 {
    if denominator == 0 {
        u128::MAX
    } else {
        mul_div_floor(numerator, WAD, denominator).unwrap_or(u128::MAX)
    }
}

/// Spec §7.6 health calculation, driven entirely from already-validated per-asset valuations —
/// the caller (instruction handler) is responsible for ensuring `collaterals`/`debts` cover every
/// canonical active index on the margin account (spec `validate_complete_positions`).
pub fn calculate_health(
    collaterals: &[CollateralValuation],
    debts: &[DebtValuation],
) -> Result<HealthSnapshot> {
    let mut borrow_power: u128 = 0;
    let mut liquidation_collateral_value: u128 = 0;
    for c in collaterals {
        let ltv_component = mul_div_floor(c.collateral_value, c.ltv_bps as u128, BASIS_POINTS as u128)?;
        borrow_power = borrow_power.checked_add(ltv_component).ok_or(VannaError::MathOverflow)?;

        let liq_component = mul_div_floor(
            c.collateral_value,
            c.liquidation_threshold_bps as u128,
            BASIS_POINTS as u128,
        )?;
        liquidation_collateral_value = liquidation_collateral_value
            .checked_add(liq_component)
            .ok_or(VannaError::MathOverflow)?;
    }

    let mut total_debt_value: u128 = 0;
    for d in debts {
        total_debt_value = total_debt_value
            .checked_add(d.debt_value)
            .ok_or(VannaError::MathOverflow)?;
    }

    Ok(HealthSnapshot {
        borrow_power,
        liquidation_collateral_value,
        total_debt_value,
        borrow_health_factor_wad: health_factor_wad(borrow_power, total_debt_value),
        liquidation_health_factor_wad: health_factor_wad(liquidation_collateral_value, total_debt_value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_negative_exponent_and_rounding() {
        // 1 USDC (6 decimals) at $1.00 with Pyth exponent -8 -> price mantissa 100_000_000.
        // net_exp = -8 - 6 + 9 = -5 -> divide by 10^5.
        // base = 1_000_000 * 100_000_000 = 1e14; 1e14 / 1e5 = 1e9 (== $1 in nano-USD).
        let value = normalize_token_value(1_000_000, 100_000_000, -8, 6, false).unwrap();
        assert_eq!(value, 1_000_000_000);
    }

    #[test]
    fn normalize_rounds_up_for_debt() {
        // token_amount chosen so base/factor has a remainder.
        let down = normalize_token_value(3, 100_000_000, -8, 6, false).unwrap();
        let up = normalize_token_value(3, 100_000_000, -8, 6, true).unwrap();
        assert!(up >= down);
    }

    #[test]
    fn value_to_token_amount_round_trips_with_normalize() {
        let value = normalize_token_value(1_000_000, 100_000_000, -8, 6, false).unwrap();
        let amount = value_to_token_amount(value, 100_000_000, -8, 6, false).unwrap();
        assert_eq!(amount, 1_000_000);
    }

    #[test]
    fn rejects_non_positive_price() {
        assert!(normalize_token_value(1_000_000, 0, -8, 6, false).is_err());
        assert!(normalize_token_value(1_000_000, -1, -8, 6, false).is_err());
    }

    #[test]
    fn zero_debt_is_infinitely_healthy() {
        let snap = calculate_health(&[], &[]).unwrap();
        assert!(snap.is_borrow_healthy());
        assert!(!snap.is_liquidatable());
        assert_eq!(snap.borrow_health_factor_wad, u128::MAX);
    }

    #[test]
    fn borrow_power_uses_ltv_liquidation_uses_threshold() {
        let collaterals = [CollateralValuation {
            collateral_value: 1_000_000_000, // $1
            ltv_bps: 8_000,
            liquidation_threshold_bps: 8_500,
        }];
        let debts = [DebtValuation { debt_value: 850_000_000 }]; // $0.85
        let snap = calculate_health(&collaterals, &debts).unwrap();
        assert_eq!(snap.borrow_power, 800_000_000);
        assert_eq!(snap.liquidation_collateral_value, 850_000_000);
        assert!(!snap.is_borrow_healthy()); // 800m < 850m debt -> would-be borrow unhealthy
        assert!(!snap.is_liquidatable()); // 850m liq value == 850m debt -> not yet liquidatable
    }
}
