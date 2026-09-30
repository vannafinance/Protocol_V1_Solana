use super::fixed_point::{mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

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
