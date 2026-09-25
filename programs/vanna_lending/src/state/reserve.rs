use crate::constants::{BASIS_POINTS, MAX_RATE_COEFF_WAD};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Per-reserve status, stored as `Reserve::status`; gates which actions the reserve allows.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReserveStatus {
    Active = 0,
    SupplyOnly = 1,
    RepayOnly = 2,
    Frozen = 3,
}

impl ReserveStatus {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Active),
            1 => Some(Self::SupplyOnly),
            2 => Some(Self::RepayOnly),
            3 => Some(Self::Frozen),
            _ => None,
        }
    }

    pub fn supply_allowed(&self) -> bool {
        matches!(self, Self::Active | Self::SupplyOnly)
    }

    pub fn redeem_allowed(&self) -> bool {
        !matches!(self, Self::Frozen)
    }

    pub fn borrow_allowed(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn repay_allowed(&self) -> bool {
        true
    }
}

/// Borrow-rate curve coefficients (WAD). See `math::interest::borrow_rate_per_second_wad`.
/// annual_rate = rate_multiplier * (linear_coeff * (u + u^32) + jump_coeff * u^64)
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateCurve {
    /// Weight of the `u` and `u^32` terms.
    pub linear_coeff_wad: u64,
    /// Weight of the `u^64` term, which dominates only near full utilization.
    pub jump_coeff_wad: u64,
    /// Overall scale applied to the whole polynomial.
    pub rate_multiplier_wad: u64,
}

impl RateCurve {
    pub fn validate(&self) -> Result<()> {
        for coeff in [self.linear_coeff_wad, self.jump_coeff_wad, self.rate_multiplier_wad] {
            require!(coeff > 0 && coeff <= MAX_RATE_COEFF_WAD, VannaError::InvalidRateModel);
        }
        Ok(())
    }
}

/// Accounting and authority for one lending pool.
#[account]
#[derive(InitSpace)]
pub struct Reserve {
    pub asset_config: Pubkey,
    pub underlying_mint: Pubkey,
    pub liquidity_vault: Pubkey,
    pub share_mint: Pubkey,

    pub supply_cap: u64,
    pub borrow_cap: u64,
    pub accounted_liquidity_assets: u64,
    pub total_borrow_assets: u64,
    pub total_borrow_shares: u128,
    pub borrow_index_wad: u128,
    pub accrued_protocol_fees: u64,
    pub last_update_timestamp: i64,

    pub rate_curve: RateCurve,
    pub reserve_factor_bps: u16,
    pub status: u8,
    pub bump: u8,
    pub reserved: [u8; 128],
}

impl Reserve {
    pub fn validate_rate_config(rate_curve: &RateCurve, reserve_factor_bps: u16) -> Result<()> {
        rate_curve.validate()?;
        require!(reserve_factor_bps as u64 <= BASIS_POINTS, VannaError::InvalidRateModel);
        Ok(())
    }

    /// The raw vault balance must never be below the accounted amount: donations can only create
    /// slack, never a shortfall.
    pub fn assert_invariants(&self, raw_vault_amount: u64, share_mint_supply: u64) -> Result<()> {
        require!(
            raw_vault_amount >= self.accounted_liquidity_assets,
            VannaError::VaultAccountingInvariantFailed
        );
        require!(
            (self.total_borrow_shares == 0) == (self.total_borrow_assets == 0),
            VannaError::VaultAccountingInvariantFailed
        );
        require!(
            self.accrued_protocol_fees <= self.accounted_liquidity_assets,
            VannaError::VaultAccountingInvariantFailed
        );
        let _ = share_mint_supply;
        Ok(())
    }
}
