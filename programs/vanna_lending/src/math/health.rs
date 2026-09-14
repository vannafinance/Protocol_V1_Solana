use super::fixed_point::{checked_pow10, mul_div_floor};
use crate::constants::{BALANCE_TO_BORROW_THRESHOLD_WAD, USD_VALUE_DECIMALS, WAD};
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
/// the rounded-down USD value of the credited amount (see `normalize_token_value`). The canonical
/// Vanna Solidity/Soroban risk formula values every credited asset at its full oracle value and
/// applies one account-level 1.10 collateral-to-debt threshold.
#[derive(Clone, Copy)]
pub struct CollateralValuation {
    pub collateral_value: u128,
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
    /// Mirrors Solidity/Soroban exactly: zero debt is healthy; otherwise HF must be > 1.10.
    pub fn is_borrow_healthy(&self) -> bool {
        self.total_debt_value == 0
            || self.borrow_health_factor_wad > BALANCE_TO_BORROW_THRESHOLD_WAD
    }

    /// The strict healthy check makes equality at 1.10 liquidatable, matching the references.
    pub fn is_liquidatable(&self) -> bool {
        self.total_debt_value > 0
            && self.liquidation_health_factor_wad <= BALANCE_TO_BORROW_THRESHOLD_WAD
    }
}

fn health_factor_wad(numerator: u128, denominator: u128) -> Result<u128> {
    if denominator == 0 {
        Ok(u128::MAX)
    } else {
        // Never convert overflow into "infinite health". The Solidity reference has uint256 and
        // Soroban uses U256; with Solana's u128 accumulator an overflow must fail closed.
        mul_div_floor(numerator, WAD, denominator)
    }
}

/// Canonical Vanna health calculation, matching both reference implementations:
/// `health_factor = total_collateral_usd / total_debt_usd`.
///
/// Borrowed assets held by the margin account are included by the instruction handlers as
/// collateral, so a new borrow increases both sides of the ratio just as it does in Solidity and
/// Soroban. The caller remains responsible for supplying every active position.
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
        // Keep the established snapshot/event field names for client compatibility. Both now
        // contain the same raw collateral total because Vanna uses one account-level threshold.
        borrow_power: total_collateral_value,
        liquidation_collateral_value: total_collateral_value,
        total_debt_value,
        borrow_health_factor_wad: health_factor,
        liquidation_health_factor_wad: health_factor,
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
    fn health_ratio_overflow_fails_closed() {
        let result = calculate_health(
            &[CollateralValuation {
                collateral_value: u128::MAX,
            }],
            &[DebtValuation { debt_value: 1 }],
        );
        assert!(result.is_err());
    }

    #[test]
    fn health_uses_raw_collateral_and_single_reference_threshold() {
        let collaterals = [CollateralValuation {
            collateral_value: 1_000_000_000, // $1
        }];
        let debts = [DebtValuation { debt_value: 900_000_000 }]; // HF = 1.111... > 1.10
        let snap = calculate_health(&collaterals, &debts).unwrap();
        assert_eq!(snap.borrow_power, 1_000_000_000);
        assert_eq!(snap.liquidation_collateral_value, 1_000_000_000);
        assert!(snap.is_borrow_healthy());
        assert!(!snap.is_liquidatable());
    }

    #[test]
    fn equality_at_reference_threshold_is_unhealthy_and_liquidatable() {
        let collaterals = [CollateralValuation {
            collateral_value: 1_100_000_000,
        }];
        let debts = [DebtValuation { debt_value: 1_000_000_000 }];
        let snap = calculate_health(&collaterals, &debts).unwrap();
        assert_eq!(snap.borrow_health_factor_wad, BALANCE_TO_BORROW_THRESHOLD_WAD);
        assert!(!snap.is_borrow_healthy());
        assert!(snap.is_liquidatable());
    }

    #[test]
    fn projected_leverage_matches_solidity_and_soroban() {
        // A $10 wallet deposit at 5x borrows $40. Borrowed funds remain in the
        // margin account, so projected collateral is $50 and debt is $40.
        let five_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 50_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 40_000_000_000,
            }],
        )
        .unwrap();
        assert_eq!(five_x.borrow_health_factor_wad, 1_250_000_000_000_000_000);
        assert!(five_x.is_borrow_healthy());

        // 10x is still above the canonical 1.10 threshold: $100 / $90 = 1.111...
        let ten_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 100_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 90_000_000_000,
            }],
        )
        .unwrap();
        assert!(ten_x.is_borrow_healthy());

        // 11x lands exactly at 1.10 and must fail because the reference uses `>`.
        let eleven_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 110_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 100_000_000_000,
            }],
        )
        .unwrap();
        assert!(!eleven_x.is_borrow_healthy());
        assert!(eleven_x.is_liquidatable());
    }
}
