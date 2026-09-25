use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// `floor(a * b / denom)` using checked `u128` arithmetic throughout.
pub fn mul_div_floor(a: u128, b: u128, denom: u128) -> Result<u128> {
    require!(denom != 0, VannaError::DivisionByZero);
    let product = a.checked_mul(b).ok_or(VannaError::MathOverflow)?;
    Ok(product.checked_div(denom).ok_or(VannaError::MathOverflow)?)
}

/// `ceil(a * b / denom)` using checked `u128` arithmetic throughout.
pub fn mul_div_ceil(a: u128, b: u128, denom: u128) -> Result<u128> {
    require!(denom != 0, VannaError::DivisionByZero);
    let product = a.checked_mul(b).ok_or(VannaError::MathOverflow)?;
    let floor = product.checked_div(denom).ok_or(VannaError::MathOverflow)?;
    let remainder = product % denom;
    if remainder == 0 {
        Ok(floor)
    } else {
        floor.checked_add(1).ok_or_else(|| VannaError::MathOverflow.into())
    }
}

/// `10^exp` as a `u128`, rejecting exponents that would not fit.
pub fn checked_pow10(exp: u32) -> Result<u128> {
    require!(exp <= 38, VannaError::MathOverflow);
    10u128
        .checked_pow(exp)
        .ok_or_else(|| VannaError::MathOverflow.into())
}

pub fn u64_from_u128(value: u128) -> Result<u64> {
    u64::try_from(value).map_err(|_| VannaError::MathOverflow.into())
}

pub fn u128_from_i64(value: i64) -> Result<u128> {
    require!(value >= 0, VannaError::MathUnderflow);
    Ok(value as u128)
}
