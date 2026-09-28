use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// How an asset's USD value is derived. Every source other than `Pyth` reads one more account,
/// `AssetConfig::price_source_account`. New variants are appended: the index is stored on-chain.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceSource {
    /// value = amount × Pyth price of `price_feed_id`.
    Pyth,
    /// Kamino cToken. value = amount × (reserve liquidity / cToken supply) × Pyth price of the
    /// underlying (`price_feed_id` is the underlying's feed).
    KaminoReceipt,
    /// Token-2022 mint with the Scaled UI Amount extension (xStocks). One UI token is one share,
    /// and the Pyth feed prices one UI token, so
    /// value = amount × current UI multiplier × Pyth price of `price_feed_id`.
    /// The source account is the mint itself.
    ScaledUiAmount,
    /// Token priced as a Pyth redemption rate against a base asset (JupSOL: JUPSOL/SOL.RR × SOL/USD).
    /// value = amount × rate (`price_feed_id`) × base price. The source account is the base feed's
    /// canonical Pyth price-feed account.
    RedemptionRate,
}

/// Token identity, oracle identity, and risk limits for one mint.
///
/// `ltv_bps`, `liquidation_threshold_bps` and `liquidation_bonus_bps` stay in the layout for
/// compatibility only: V1 health uses the account-wide 1.10 threshold, and liquidation hands the
/// liquidator the whole account rather than a bonus.
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
    pub price_source: PriceSource,
    /// The account a non-Pyth source reads (Kamino reserve, the mint itself, or the base price
    /// feed); default for `Pyth`.
    pub price_source_account: Pubkey,
    /// Program that owns `price_source_account` (klend, Token-2022 or the Pyth receiver); default
    /// for `Pyth`.
    pub price_source_program: Pubkey,
    pub reserved: [u8; 31],
}

impl AssetConfig {
    pub fn is_pyth_priced(&self) -> bool {
        self.price_source == PriceSource::Pyth
    }

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
