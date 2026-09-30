use super::fixed_point::mul_div_floor;
use crate::constants::{BALANCE_TO_BORROW_THRESHOLD_WAD, WAD};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

#[derive(Clone, Copy)]
pub struct CollateralValuation {
    pub collateral_value: u128,
}

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
    pub fn is_borrow_healthy(&self) -> bool {
        self.total_debt_value == 0
            || self.borrow_health_factor_wad > BALANCE_TO_BORROW_THRESHOLD_WAD
    }

    pub fn is_liquidatable(&self) -> bool {
        self.total_debt_value > 0
            && self.liquidation_health_factor_wad <= BALANCE_TO_BORROW_THRESHOLD_WAD
    }
}

fn health_factor_wad(collateral_value: u128, debt_value: u128) -> Result<u128> {
    if debt_value == 0 {
        Ok(u128::MAX)
    } else {
        mul_div_floor(collateral_value, WAD, debt_value)
    }
}

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
        borrow_power: total_collateral_value,
        liquidation_collateral_value: total_collateral_value,
        total_debt_value,
        borrow_health_factor_wad: health_factor,
        liquidation_health_factor_wad: health_factor,
    })
}
