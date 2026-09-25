use super::fixed_point::{mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::constants::{BASIS_POINTS, SECONDS_PER_YEAR, WAD};
use crate::errors::VannaError;
use crate::state::reserve::{RateCurve, Reserve};
use anchor_lang::prelude::*;

/// Utilization rate: the share of the pool's assets currently lent out, as a WAD fraction.
///
/// utilization = total_borrows / (liquidity + total_borrows)        (0 when the pool is empty)
pub fn utilization_wad(accounted_liquidity_assets: u64, total_borrow_assets: u64) -> Result<u128> {
    let gross = (accounted_liquidity_assets as u128)
        .checked_add(total_borrow_assets as u128)
        .ok_or(VannaError::MathOverflow)?;
    if gross == 0 {
        return Ok(0);
    }
    mul_div_floor(total_borrow_assets as u128, WAD, gross)
}

/// `base^exp` for a WAD-scaled `base`, by square-and-multiply with each step rounded half-up.
fn wad_pow(mut base: u128, mut exp: u32) -> Result<u128> {
    const HALF_WAD: u128 = WAD / 2;
    let wad_mul_round = |a: u128, b: u128| -> Result<u128> {
        let product = a.checked_mul(b).ok_or(VannaError::MathOverflow)?;
        Ok(product.checked_add(HALF_WAD).ok_or(VannaError::MathOverflow)? / WAD)
    };

    if base == 0 {
        return Ok(if exp == 0 { WAD } else { 0 });
    }
    let mut result = if exp % 2 == 1 { base } else { WAD };
    exp /= 2;
    while exp > 0 {
        base = wad_mul_round(base, base)?;
        if exp % 2 == 1 {
            result = wad_mul_round(result, base)?;
        }
        exp /= 2;
    }
    Ok(result)
}

/// Borrow interest rate per second (WAD) at utilization `u`.
///
/// borrow_rate = rate_multiplier * (u * linear_coeff + u^32 * linear_coeff + u^64 * jump_coeff) / SECONDS_PER_YEAR
pub fn borrow_rate_per_second_wad(curve: &RateCurve, util_wad: u128) -> Result<u128> {
    let linear = curve.linear_coeff_wad as u128;
    let jump = curve.jump_coeff_wad as u128;

    let linear_term = mul_div_floor(util_wad, linear, WAD)?;
    let steep_term = mul_div_floor(wad_pow(util_wad, 32)?, linear, WAD)?;
    let jump_term = mul_div_floor(wad_pow(util_wad, 64)?, jump, WAD)?;
    let polynomial = linear_term
        .checked_add(steep_term)
        .and_then(|v| v.checked_add(jump_term))
        .ok_or(VannaError::MathOverflow)?;

    let year_wad = (SECONDS_PER_YEAR as u128)
        .checked_mul(WAD)
        .ok_or(VannaError::MathOverflow)?;
    mul_div_floor(curve.rate_multiplier_wad as u128, polynomial, year_wad)
}

pub struct AccrualResult {
    pub new_total_borrow_assets: u64,
    pub new_accrued_protocol_fees: u64,
    pub new_borrow_index_wad: u128,
    pub interest_accrued: u64,
}

/// Brings a reserve's debt current to `now`. Returns the new state; the caller writes it back.
///
/// growth         = borrow_rate * elapsed_seconds
/// interest       = ceil(total_borrows * growth)                  (rounded in lenders' favor)
/// protocol_fee   = floor(interest * reserve_factor_bps / 10_000)
/// total_borrows' = total_borrows + interest
/// borrow_index'  = borrow_index * total_borrows' / total_borrows
///
/// Growth is simple, not compounded, within one window; compounding comes from frequent accrual.
pub fn accrue(reserve: &Reserve, now: i64) -> Result<AccrualResult> {
    let elapsed = now
        .checked_sub(reserve.last_update_timestamp)
        .ok_or(VannaError::MathOverflow)?;
    require!(elapsed >= 0, VannaError::TimestampRegression);

    if elapsed == 0 || reserve.total_borrow_assets == 0 {
        return Ok(AccrualResult {
            new_total_borrow_assets: reserve.total_borrow_assets,
            new_accrued_protocol_fees: reserve.accrued_protocol_fees,
            new_borrow_index_wad: reserve.borrow_index_wad,
            interest_accrued: 0,
        });
    }

    let util = utilization_wad(reserve.accounted_liquidity_assets, reserve.total_borrow_assets)?;
    let rate_per_second = borrow_rate_per_second_wad(&reserve.rate_curve, util)?;

    let growth_wad = rate_per_second
        .checked_mul(elapsed as u128)
        .ok_or(VannaError::MathOverflow)?;
    let interest = u64_from_u128(mul_div_ceil(reserve.total_borrow_assets as u128, growth_wad, WAD)?)?;

    let protocol_fee = u64_from_u128(mul_div_floor(
        interest as u128,
        reserve.reserve_factor_bps as u128,
        BASIS_POINTS as u128,
    )?)?;

    let new_total_borrow_assets = reserve
        .total_borrow_assets
        .checked_add(interest)
        .ok_or(VannaError::MathOverflow)?;
    let new_accrued_protocol_fees = reserve
        .accrued_protocol_fees
        .checked_add(protocol_fee)
        .ok_or(VannaError::MathOverflow)?;

    let new_borrow_index_wad = mul_div_floor(
        reserve.borrow_index_wad,
        new_total_borrow_assets as u128,
        reserve.total_borrow_assets as u128,
    )
    .unwrap_or(reserve.borrow_index_wad);
    let new_borrow_index_wad = new_borrow_index_wad.max(WAD);

    Ok(AccrualResult {
        new_total_borrow_assets,
        new_accrued_protocol_fees,
        new_borrow_index_wad,
        interest_accrued: interest,
    })
}

