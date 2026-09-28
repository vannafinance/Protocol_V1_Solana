//! Kamino klend adapter and cToken valuation.
//!
//! Allowed calls (klend's account order):
//!
//! deposit_reserve_liquidity(amount)                 redeem_reserve_collateral(amount)
//!   0 owner (margin, signer)                          0 owner (margin, signer)
//!   1 reserve             (price source)              1 lending_market
//!   2 lending_market                                  2 reserve            (price source)
//!   3 lending_market_authority                        3 lending_market_authority
//!   4 reserve_liquidity_mint                          4 reserve_liquidity_mint
//!   5 reserve_liquidity_supply                        5 reserve_collateral_mint
//!   6 reserve_collateral_mint                         6 reserve_liquidity_supply
//!   7 user_source_liquidity    (spent)                7 user_source_collateral     (spent)
//!   8 user_destination_collateral (received)          8 user_destination_liquidity (received)
//!   9 collateral_token_program                        9 collateral_token_program
//!  10 liquidity_token_program                        10 liquidity_token_program
//!  11 instruction_sysvar                             11 instruction_sysvar

use super::{CallPlan, TokenLeg};
use crate::errors::VannaError;
use crate::math::fixed_point::mul_div_floor;
use anchor_lang::prelude::*;

/// sha256("global:deposit_reserve_liquidity")[0..8]
pub const DEPOSIT_RESERVE_LIQUIDITY: [u8; 8] = [169, 201, 30, 126, 6, 205, 102, 68];
/// sha256("global:redeem_reserve_collateral")[0..8]
pub const REDEEM_RESERVE_COLLATERAL: [u8; 8] = [234, 117, 181, 125, 185, 142, 220, 29];
/// sha256("account:Reserve")[0..8]
const RESERVE_DISCRIMINATOR: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];

/// klend `Reserve` account size and layout version these offsets are verified against.
const RESERVE_LEN: usize = 8624;
const RESERVE_VERSION: u64 = 1;

const CALL_ACCOUNT_COUNT: usize = 12;
/// Discriminator + `amount: u64`.
const CALL_DATA_LEN: usize = 16;

pub fn plan_call(data: &[u8], account_count: usize) -> Result<CallPlan> {
    require!(data.len() == CALL_DATA_LEN, VannaError::CallNotAllowed);
    require!(account_count == CALL_ACCOUNT_COUNT, VannaError::InvalidCallAccounts);
    let amount = u64::from_le_bytes(data[8..16].try_into().unwrap());
    require!(amount > 0, VannaError::ZeroAmount);

    let selector: [u8; 8] = data[..8].try_into().unwrap();
    match selector {
        DEPOSIT_RESERVE_LIQUIDITY => Ok(CallPlan {
            authority: 0,
            spent: TokenLeg { vault: 7, mint: Some(4) },
            received: TokenLeg { vault: 8, mint: Some(6) },
            price_source: Some(1),
            max_spent: amount,
        }),
        REDEEM_RESERVE_COLLATERAL => Ok(CallPlan {
            authority: 0,
            spent: TokenLeg { vault: 7, mint: Some(5) },
            received: TokenLeg { vault: 8, mint: Some(4) },
            price_source: Some(2),
            max_spent: amount,
        }),
        _ => err!(VannaError::CallNotAllowed),
    }
}

/// Exchange-rate snapshot of one klend reserve.
pub struct ReserveRate {
    pub liquidity_mint: Pubkey,
    /// Decimals of the underlying. klend cToken mints always have 6 decimals, so underlying
    /// amounts must be valued with these, never with the cToken's.
    pub liquidity_decimals: u8,
    /// Liquidity owed to cToken holders, in underlying base units.
    pub total_liquidity: u128,
    pub collateral_supply: u64,
}

impl ReserveRate {
    /// underlying = floor(receipts × total_liquidity / collateral_supply)
    pub fn underlying_for_receipts(&self, receipts: u64) -> Result<u64> {
        if receipts == 0 {
            return Ok(0);
        }
        require!(self.collateral_supply > 0, VannaError::InvalidKaminoAccounts);
        let value = mul_div_floor(receipts as u128, self.total_liquidity, self.collateral_supply as u128)?;
        u64::try_from(value).map_err(|_| VannaError::MathOverflow.into())
    }

    /// receipts = floor(underlying × collateral_supply / total_liquidity)
    pub fn receipts_for_underlying(&self, underlying: u64) -> Result<u64> {
        if underlying == 0 {
            return Ok(0);
        }
        require!(self.total_liquidity > 0, VannaError::InvalidKaminoAccounts);
        let value = mul_div_floor(underlying as u128, self.collateral_supply as u128, self.total_liquidity)?;
        u64::try_from(value).map_err(|_| VannaError::MathOverflow.into())
    }
}

/// Reads a klend `Reserve` after checking its owner, discriminator, layout and cToken mint. An
/// unexpected layout fails closed rather than risk misreading prices.
///
/// total_liquidity = available + borrowed − protocol fees − referrer fees − pending referrer fees
/// (`_sf` fields are fractions with 60 fractional bits).
pub fn read_reserve_rate(reserve: &AccountInfo, program: &Pubkey, collateral_mint: &Pubkey) -> Result<ReserveRate> {
    require_keys_eq!(*reserve.owner, *program, VannaError::InvalidKaminoAccounts);
    let data = reserve.try_borrow_data()?;
    require!(data.len() == RESERVE_LEN, VannaError::InvalidKaminoAccounts);
    require!(data[..8] == RESERVE_DISCRIMINATOR, VannaError::InvalidKaminoAccounts);
    require!(data[2560..2592] == collateral_mint.to_bytes(), VannaError::InvalidKaminoAccounts);

    let u64_at = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let u128_at = |i: usize| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    require!(u64_at(8) == RESERVE_VERSION, VannaError::InvalidKaminoAccounts);
    let liquidity_decimals = u64_at(272);
    require!(liquidity_decimals <= 18, VannaError::InvalidKaminoAccounts);
    let total_sf = ((u64_at(224) as u128) << 60)
        .checked_add(u128_at(232))
        .ok_or(VannaError::MathOverflow)?
        .checked_sub(u128_at(344))
        .ok_or(VannaError::MathUnderflow)?
        .checked_sub(u128_at(360))
        .ok_or(VannaError::MathUnderflow)?
        .checked_sub(u128_at(376))
        .ok_or(VannaError::MathUnderflow)?;
    Ok(ReserveRate {
        liquidity_mint: Pubkey::new_from_array(data[128..160].try_into().unwrap()),
        liquidity_decimals: liquidity_decimals as u8,
        total_liquidity: total_sf >> 60,
        collateral_supply: u64_at(2592),
    })
}
