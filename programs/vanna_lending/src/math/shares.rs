//! Supply-share and borrow-share conversions.
//!
//! The `_down` / `_up` suffix is the rounding direction. Every conversion rounds against the user
//! (fewer shares or assets out, more debt in), so rounding can never drain the pool.

use super::fixed_point::{mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Total assets owed to lenders. Unsolicited vault donations are not counted.
///
/// lender_total_assets = liquidity + total_borrows - accrued_protocol_fees
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

/// Cash lenders can withdraw right now.
///
/// available_cash = max(liquidity - accrued_protocol_fees, 0)
pub fn available_lender_cash(accounted_liquidity_assets: u64, accrued_protocol_fees: u64) -> u64 {
    accounted_liquidity_assets.saturating_sub(accrued_protocol_fees)
}

/// Supply shares minted for a deposit, rounded down against the lender.
///
/// shares = floor(assets * total_supply_shares / lender_total_assets)   (1:1 for the first deposit)
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

/// Assets paid out when redeeming supply shares, rounded down against the lender.
///
/// assets = floor(shares * lender_total_assets / total_supply_shares)
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

/// Borrow shares issued for a new borrow, rounded up against the borrower.
///
/// borrow_shares = ceil(assets * total_borrow_shares / total_borrows)   (1:1 for the first borrow)
///
/// Shares are fixed at borrow time. Interest raises `total_borrows`, so each share's debt grows.
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

/// Current debt of a position, rounded up so debt is never under-counted.
///
/// debt = ceil(borrow_shares * total_borrows / total_borrow_shares)
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
