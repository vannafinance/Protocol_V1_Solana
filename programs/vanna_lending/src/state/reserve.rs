use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Spec §5.2 reserve status matrix.
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

/// Spec §4.3 `Reserve` — accounting and authority for one lending pool.
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

    pub base_rate_bps: u16,
    pub slope1_bps: u16,
    pub slope2_bps: u16,
    pub optimal_utilization_bps: u16,
    pub reserve_factor_bps: u16,
    pub status: u8,
    pub bump: u8,
    pub reserved: [u8; 128],
}

impl Reserve {
    pub fn validate_rate_model(
        base_rate_bps: u16,
        optimal_utilization_bps: u16,
        reserve_factor_bps: u16,
    ) -> Result<()> {
        require!(optimal_utilization_bps > 0 && optimal_utilization_bps < 10_000, VannaError::InvalidRateModel);
        require!(reserve_factor_bps <= 10_000, VannaError::InvalidRateModel);
        require!(base_rate_bps <= 10_000, VannaError::InvalidRateModel);
        Ok(())
    }

    /// Spec §4.3 invariant: the raw vault balance must never be less than the accounted amount —
    /// donations can only ever create slack, never a shortfall.
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
