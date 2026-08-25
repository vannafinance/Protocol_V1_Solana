use super::fixed_point::{mul_div_ceil, mul_div_floor, u64_from_u128};
use crate::constants::{BASIS_POINTS, SECONDS_PER_YEAR, WAD};
use crate::errors::VannaError;
use crate::state::reserve::Reserve;
use anchor_lang::prelude::*;

/// Spec §7.4 utilization, in basis points. Zero when the pool is empty.
pub fn utilization_bps(accounted_liquidity_assets: u64, total_borrow_assets: u64) -> Result<u64> {
    let gross = (accounted_liquidity_assets as u128)
        .checked_add(total_borrow_assets as u128)
        .ok_or(VannaError::MathOverflow)?;
    if gross == 0 {
        return Ok(0);
    }
    u64_from_u128(mul_div_floor(
        total_borrow_assets as u128,
        BASIS_POINTS as u128,
        gross,
    )?)
}

/// Spec §7.4 kink interest-rate model, in basis points (annualized).
pub fn kink_rate_bps(
    utilization_bps: u64,
    base_rate_bps: u16,
    slope1_bps: u16,
    slope2_bps: u16,
    optimal_utilization_bps: u16,
) -> Result<u64> {
    let optimal = optimal_utilization_bps as u64;
    let base = base_rate_bps as u128;
    if utilization_bps <= optimal {
        if optimal == 0 {
            return u64_from_u128(base);
        }
        let slope_component = mul_div_floor(slope1_bps as u128, utilization_bps as u128, optimal as u128)?;
        u64_from_u128(base.checked_add(slope_component).ok_or(VannaError::MathOverflow)?)
    } else {
        let excess_room = (BASIS_POINTS as u64)
            .checked_sub(optimal)
            .ok_or(VannaError::MathUnderflow)?;
        require!(excess_room > 0, VannaError::InvalidRateModel);
        let excess_utilization = utilization_bps.checked_sub(optimal).ok_or(VannaError::MathUnderflow)?;
        let slope2_component = mul_div_floor(
            slope2_bps as u128,
            excess_utilization as u128,
            excess_room as u128,
        )?;
        let total = base
            .checked_add(slope1_bps as u128)
            .and_then(|v| v.checked_add(slope2_component))
            .ok_or(VannaError::MathOverflow)?;
        u64_from_u128(total)
    }
}

pub struct AccrualResult {
    pub new_total_borrow_assets: u64,
    pub new_accrued_protocol_fees: u64,
    pub new_borrow_index_wad: u128,
    pub interest_accrued: u64,
}

/// Spec §7.4 accrual: elapsed interest, protocol fee split, and borrow-index growth.
/// Callers must reject a `now` earlier than `reserve.last_update_timestamp` (`TimestampRegression`)
/// before calling this — it assumes `elapsed >= 0`.
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

    let util = utilization_bps(reserve.accounted_liquidity_assets, reserve.total_borrow_assets)?;
    let rate_bps = kink_rate_bps(
        util,
        reserve.base_rate_bps,
        reserve.slope1_bps,
        reserve.slope2_bps,
        reserve.optimal_utilization_bps,
    )?;

    // interest = ceil(total_borrow_assets * rate_bps * elapsed / (10_000 * seconds_per_year))
    let numerator = (reserve.total_borrow_assets as u128)
        .checked_mul(rate_bps as u128)
        .and_then(|v| v.checked_mul(elapsed as u128))
        .ok_or(VannaError::MathOverflow)?;
    let denominator = (BASIS_POINTS as u128)
        .checked_mul(SECONDS_PER_YEAR as u128)
        .ok_or(VannaError::MathOverflow)?;
    let interest = u64_from_u128(mul_div_ceil(numerator, 1, denominator)?)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utilization_is_zero_for_empty_pool() {
        assert_eq!(utilization_bps(0, 0).unwrap(), 0);
    }

    #[test]
    fn utilization_full_when_all_borrowed() {
        assert_eq!(utilization_bps(0, 100).unwrap(), 10_000);
    }

    #[test]
    fn kink_rate_at_zero_utilization_is_base_rate() {
        assert_eq!(kink_rate_bps(0, 200, 400, 6_000, 8_000).unwrap(), 200);
    }

    #[test]
    fn kink_rate_at_optimal_utilization_is_base_plus_slope1() {
        assert_eq!(kink_rate_bps(8_000, 200, 400, 6_000, 8_000).unwrap(), 600);
    }

    #[test]
    fn kink_rate_at_full_utilization_is_base_plus_slope1_plus_slope2() {
        assert_eq!(kink_rate_bps(10_000, 200, 400, 6_000, 8_000).unwrap(), 6_600);
    }

    #[test]
    fn kink_rate_mid_second_slope() {
        // Halfway between optimal (8000) and 10000 -> half of slope2 added.
        assert_eq!(kink_rate_bps(9_000, 200, 400, 6_000, 8_000).unwrap(), 3_600);
    }
}
