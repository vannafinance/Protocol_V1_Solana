mod common;

use anchor_lang::prelude::*;
use common::*;
use vanna_lending::constants::{MAX_RATE_COEFF_WAD, SECONDS_PER_YEAR, WAD};
use vanna_lending::math::interest::{accrue, borrow_rate_per_second_wad, utilization_wad};
use vanna_lending::state::reserve::Reserve;

fn rate_at(liquidity: u64, borrows: u64) -> u128 {
    borrow_rate_per_second_wad(&DEFAULT_RATE_CURVE, utilization_wad(liquidity, borrows).unwrap()).unwrap()
}

fn reserve_at_half_utilization() -> Reserve {
    Reserve {
        asset_config: Pubkey::default(),
        underlying_mint: Pubkey::default(),
        liquidity_vault: Pubkey::default(),
        share_mint: Pubkey::default(),
        supply_cap: 0,
        borrow_cap: 0,
        accounted_liquidity_assets: 1_000_000,
        total_borrow_assets: 1_000_000,
        total_borrow_shares: 1_000_000,
        borrow_index_wad: WAD,
        accrued_protocol_fees: 0,
        last_update_timestamp: 0,
        rate_curve: DEFAULT_RATE_CURVE,
        reserve_factor_bps: 2_000,
        status: 0,
        bump: 0,
        reserved: [0u8; 128],
    }
}

#[test]
fn utilization_bounds() {
    assert_eq!(utilization_wad(0, 0).unwrap(), 0);
    assert_eq!(utilization_wad(0, 100).unwrap(), WAD);
    assert_eq!(utilization_wad(100, 100).unwrap(), WAD / 2);
}

/// Expected values are computed independently from the reference curve definition
/// (including its round-half-up `u^32` / `u^64` powers).
#[test]
fn rate_matches_reference_curve() {
    assert_eq!(rate_at(1, 0), 0);
    assert_eq!(rate_at(1, 1), 5_545_529_241); // 50% util  -> ~17.5% APR
    assert_eq!(rate_at(2, 8), 8_881_654_909); // 80% util  -> ~28.0% APR
    assert_eq!(rate_at(1, 19), 13_933_518_222); // 95% util -> ~44.0% APR
    assert_eq!(rate_at(0, 1), 55_455_292_386); // 100% util -> ~175% APR
}

#[test]
fn largest_allowed_curve_does_not_overflow() {
    let max = RateCurve {
        linear_coeff_wad: MAX_RATE_COEFF_WAD,
        jump_coeff_wad: MAX_RATE_COEFF_WAD,
        rate_multiplier_wad: MAX_RATE_COEFF_WAD,
    };
    assert!(borrow_rate_per_second_wad(&max, WAD).is_ok());
}

#[test]
fn curve_validation_rejects_zero_and_oversized_coefficients() {
    assert!(DEFAULT_RATE_CURVE.validate().is_ok());
    assert!(RateCurve { linear_coeff_wad: 0, ..DEFAULT_RATE_CURVE }.validate().is_err());
    assert!(RateCurve { jump_coeff_wad: 0, ..DEFAULT_RATE_CURVE }.validate().is_err());
    assert!(RateCurve { rate_multiplier_wad: 0, ..DEFAULT_RATE_CURVE }.validate().is_err());
    assert!(RateCurve { rate_multiplier_wad: MAX_RATE_COEFF_WAD + 1, ..DEFAULT_RATE_CURVE }.validate().is_err());
}

#[test]
fn accrue_one_year_at_half_utilization() {
    let reserve = reserve_at_half_utilization();
    let accrual = accrue(&reserve, SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(accrual.interest_accrued, 175_001); // 17.5% plus round-up
    assert_eq!(accrual.new_total_borrow_assets, 1_175_001);
    assert_eq!(accrual.new_accrued_protocol_fees, 35_000); // 20% reserve factor
    assert_eq!(accrual.new_borrow_index_wad, 1_175_001_000_000_000_000);
}

#[test]
fn accrue_is_a_no_op_without_elapsed_time_or_debt() {
    let reserve = reserve_at_half_utilization();
    assert_eq!(accrue(&reserve, 0).unwrap().interest_accrued, 0);

    let no_debt = Reserve { total_borrow_assets: 0, total_borrow_shares: 0, ..reserve_at_half_utilization() };
    assert_eq!(accrue(&no_debt, SECONDS_PER_YEAR as i64).unwrap().interest_accrued, 0);
}

#[test]
fn accrue_rejects_timestamp_regression() {
    let reserve = Reserve { last_update_timestamp: 100, ..reserve_at_half_utilization() };
    assert!(accrue(&reserve, 99).is_err());
}
