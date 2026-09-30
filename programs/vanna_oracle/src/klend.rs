use crate::OracleError;
use crate::price::mul_div_floor;
use anchor_lang::prelude::*;

const RESERVE_DISCRIMINATOR: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];
const RESERVE_LEN: usize = 8624;
const RESERVE_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReserveRate {
    pub liquidity_mint: Pubkey,
    pub liquidity_decimals: u8,
    pub total_liquidity: u128,
    pub collateral_supply: u64,
}

impl ReserveRate {
    pub fn underlying_for_receipts(&self, receipts: u64) -> Result<u64> {
        if receipts == 0 {
            return Ok(0);
        }
        require!(self.collateral_supply > 0, OracleError::InvalidKaminoAccounts);
        let value = mul_div_floor(receipts as u128, self.total_liquidity, self.collateral_supply as u128)?;
        u64::try_from(value).map_err(|_| OracleError::MathOverflow.into())
    }

    pub fn receipts_for_underlying(&self, underlying: u64) -> Result<u64> {
        if underlying == 0 {
            return Ok(0);
        }
        require!(self.total_liquidity > 0, OracleError::InvalidKaminoAccounts);
        let value = mul_div_floor(underlying as u128, self.collateral_supply as u128, self.total_liquidity)?;
        u64::try_from(value).map_err(|_| OracleError::MathOverflow.into())
    }
}

pub fn read_reserve_rate(reserve: &AccountInfo, program: &Pubkey, collateral_mint: &Pubkey) -> Result<ReserveRate> {
    require_keys_eq!(*reserve.owner, *program, OracleError::InvalidKaminoAccounts);
    let data = reserve.try_borrow_data()?;
    require!(data.len() == RESERVE_LEN, OracleError::InvalidKaminoAccounts);
    require!(data[..8] == RESERVE_DISCRIMINATOR, OracleError::InvalidKaminoAccounts);
    require!(data[2560..2592] == collateral_mint.to_bytes(), OracleError::InvalidKaminoAccounts);

    let u64_at = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let u128_at = |i: usize| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    require!(u64_at(8) == RESERVE_VERSION, OracleError::InvalidKaminoAccounts);
    let liquidity_decimals = u64_at(272);
    require!(liquidity_decimals <= 18, OracleError::InvalidKaminoAccounts);
    let total_sf = ((u64_at(224) as u128) << 60)
        .checked_add(u128_at(232))
        .ok_or(OracleError::MathOverflow)?
        .checked_sub(u128_at(344))
        .ok_or(OracleError::MathUnderflow)?
        .checked_sub(u128_at(360))
        .ok_or(OracleError::MathUnderflow)?
        .checked_sub(u128_at(376))
        .ok_or(OracleError::MathUnderflow)?;
    Ok(ReserveRate {
        liquidity_mint: Pubkey::new_from_array(data[128..160].try_into().unwrap()),
        liquidity_decimals: liquidity_decimals as u8,
        total_liquidity: total_sf >> 60,
        collateral_supply: u64_at(2592),
    })
}
