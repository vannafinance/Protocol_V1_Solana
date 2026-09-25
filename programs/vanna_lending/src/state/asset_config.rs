use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Token identity, oracle identity, and risk limits for one mint.
///
/// `ltv_bps` and `liquidation_threshold_bps` stay in the layout for compatibility only: V1 health
/// uses the account-wide 1.10 threshold. `liquidation_bonus_bps` is still used when seizing.
#[account]
#[derive(InitSpace)]
pub struct AssetConfig {
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub reserve: Pubkey,
    pub price_feed_id: [u8; 32],
    pub max_collateral_per_margin: u64,
    pub ltv_bps: u16,
    pub liquidation_threshold_bps: u16,
    pub liquidation_bonus_bps: u16,
    pub max_confidence_bps: u16,
    pub max_price_age_secs: u32,
    pub asset_index: u16,
    pub decimals: u8,
    pub collateral_enabled: bool,
    pub borrow_enabled: bool,
    pub bump: u8,
    pub reserved: [u8; 96],
}

impl AssetConfig {
    /// Validates the stored risk parameters. LTV and liquidation threshold are not used by V1
    /// health, but are still checked so the stored config stays self-consistent.
    pub fn validate_risk_parameters(
        ltv_bps: u16,
        liquidation_threshold_bps: u16,
        liquidation_bonus_bps: u16,
    ) -> Result<()> {
        require!(liquidation_threshold_bps <= 10_000, VannaError::InvalidRiskParameters);
        require!(ltv_bps < liquidation_threshold_bps, VannaError::InvalidRiskParameters);

        // liquidation_threshold_bps * (10_000 + liquidation_bonus_bps) <= 10_000 * 10_000
        let lhs = (liquidation_threshold_bps as u128)
            .checked_mul(10_000u128.checked_add(liquidation_bonus_bps as u128).unwrap())
            .ok_or(VannaError::MathOverflow)?;
        require!(lhs <= 10_000u128 * 10_000u128, VannaError::InvalidRiskParameters);
        Ok(())
    }
}
