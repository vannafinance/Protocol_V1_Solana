use super::fixed_point::{mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Spec §7.2 — lender's claim on the pool, ignoring unsolicited vault donations.
pub fn lender_total_assets(
    accounted_liquidity_assets: u64,
    total_borrow_assets: u64,
    accrued_protocol_fees: u64,
) -> Result<u64> {
    (accounted_liquidity_assets as u128)
        .checked_add(total_borrow_assets as u128)
        .and_then(|v| v.checked_sub(accrued_protocol_fees as u128))
        .ok_or_else(|| VannaError::MathOverflow.into())
        .and_then(u64_from_u128)
}

pub fn available_lender_cash(accounted_liquidity_assets: u64, accrued_protocol_fees: u64) -> u64 {
    accounted_liquidity_assets.saturating_sub(accrued_protocol_fees)
}

/// Spec §6.3 `lender_supply` share formula: shares are rounded down against the lender.
pub fn assets_to_supply_shares_down(
    assets_received: u64,
    total_share_supply: u128,
    lender_total_assets_before: u64,
) -> Result<u64> {
    if total_share_supply == 0 || lender_total_assets_before == 0 {
        return Ok(assets_received);
    }
    u64_from_u128(mul_div_floor(
        assets_received as u128,
        total_share_supply,
        lender_total_assets_before as u128,
    )?)
}

/// Spec §6.3 `lender_redeem` formula: assets paid out are rounded down.
pub fn supply_shares_to_assets_down(
    shares: u64,
    total_share_supply: u128,
    lender_total_assets: u64,
) -> Result<u64> {
    if total_share_supply == 0 {
        return Ok(0);
    }
    u64_from_u128(mul_div_floor(
        shares as u128,
        lender_total_assets as u128,
        total_share_supply,
    )?)
}

/// Spec §7.3 — new borrow shares are rounded up against the borrower.
pub fn assets_to_debt_shares_up(
    assets: u64,
    total_borrow_shares: u128,
    total_borrow_assets: u64,
) -> Result<u128> {
    if total_borrow_shares == 0 || total_borrow_assets == 0 {
        return Ok(assets as u128);
    }
    mul_div_ceil(assets as u128, total_borrow_shares, total_borrow_assets as u128)
}

/// Spec §7.3 — current debt value for a position; rounded up so debt is never under-counted.
pub fn debt_shares_to_assets_up(
    borrow_shares: u128,
    total_borrow_shares: u128,
    total_borrow_assets: u64,
) -> Result<u64> {
    if total_borrow_shares == 0 {
        require!(borrow_shares == 0, VannaError::VaultAccountingInvariantFailed);
        return Ok(0);
    }
    u64_from_u128(mul_div_ceil(
        borrow_shares,
        total_borrow_assets as u128,
        total_borrow_shares,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_depositor_gets_assets_as_shares() {
        assert_eq!(assets_to_supply_shares_down(1_000, 0, 0).unwrap(), 1_000);
    }

    #[test]
    fn later_depositor_shares_scale_with_exchange_rate() {
        // Pool has 2_000 assets backing 1_000 shares (rate 2:1); depositing 100 assets -> 50 shares.
        assert_eq!(assets_to_supply_shares_down(100, 1_000, 2_000).unwrap(), 50);
    }

    #[test]
    fn redeem_rounds_down() {
        // 3 shares out of 10 total, backing 7 assets -> floor(3*7/10) = 2.
        assert_eq!(supply_shares_to_assets_down(3, 10, 7).unwrap(), 2);
    }

    #[test]
    fn first_borrow_gets_assets_as_shares() {
        assert_eq!(assets_to_debt_shares_up(500, 0, 0).unwrap(), 500);
    }

    #[test]
    fn later_borrow_rounds_up() {
        // 1 total borrow share currently represents 3 assets; borrowing 1 asset -> ceil(1*1/3) = 1.
        assert_eq!(assets_to_debt_shares_up(1, 1, 3).unwrap(), 1);
    }

    #[test]
    fn debt_value_rounds_up() {
        assert_eq!(debt_shares_to_assets_up(1, 3, 7).unwrap(), 3); // ceil(1*7/3) = 3
    }

    #[test]
    fn zero_total_shares_implies_zero_position_debt() {
        assert_eq!(debt_shares_to_assets_up(0, 0, 0).unwrap(), 0);
    }
}
