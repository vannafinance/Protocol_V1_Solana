use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Marks an unused slot of a Scope chain.
pub const SCOPE_CHAIN_UNUSED: u16 = u16::MAX;
pub const EMPTY_SCOPE_CHAIN: [u16; 4] = [SCOPE_CHAIN_UNUSED; 4];

/// How one asset is priced: its entry in the oracle facade (`oracle::get_price`), the Solana
/// counterpart of `OracleFacade.oracle[token]` in the Solidity protocol.
///
/// price = the Scope chain (primary), or while that is stale, Pyth price × Pyth factor (fallback),
///         then × the klend reserve's cToken exchange rate for Kamino receipts.
///
/// Every account is pinned here, so a transaction can never substitute another one. An unused
/// source is `Pubkey::default()` (accounts) or all-`SCOPE_CHAIN_UNUSED` (chains).
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OracleConfig {
    /// Kamino Scope `OraclePrices` account.
    pub scope_prices: Pubkey,
    /// Price = product of these Scope entries, in order (e.g. JitoSOL = stake rate × SOL/USD).
    pub scope_chain: [u16; 4],
    /// The same price's TWAP / EMA as a chain, for the divergence check.
    pub scope_twap_chain: [u16; 4],
    /// Pyth push (shard 0) price-feed account: the only source when Scope is unused, the
    /// fallback while the Scope price is stale otherwise. Its EMA is the TWAP.
    pub pyth_price: Pubkey,
    /// Optional second Pyth feed multiplied in (JupSOL: JUPSOL/SOL rate × SOL/USD).
    pub pyth_factor: Pubkey,
    /// klend reserve whose cToken exchange rate converts the underlying's price (Kamino receipts).
    pub klend_reserve: Pubkey,
    /// Program owning `klend_reserve`.
    pub klend_program: Pubkey,
    /// A price older than this is not fresh.
    pub max_age_secs: u32,
    /// Max |price − TWAP| / price. 0 disables the check.
    pub max_twap_divergence_bps: u16,
    /// Max Pyth confidence / price.
    pub max_confidence_bps: u16,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            scope_prices: Pubkey::default(),
            scope_chain: EMPTY_SCOPE_CHAIN,
            scope_twap_chain: EMPTY_SCOPE_CHAIN,
            pyth_price: Pubkey::default(),
            pyth_factor: Pubkey::default(),
            klend_reserve: Pubkey::default(),
            klend_program: Pubkey::default(),
            max_age_secs: 0,
            max_twap_divergence_bps: 0,
            max_confidence_bps: 0,
        }
    }
}

impl OracleConfig {
    pub fn uses_scope(&self) -> bool {
        self.scope_prices != Pubkey::default()
    }

    pub fn uses_pyth(&self) -> bool {
        self.pyth_price != Pubkey::default()
    }

    pub fn uses_pyth_factor(&self) -> bool {
        self.pyth_factor != Pubkey::default()
    }

    pub fn uses_klend(&self) -> bool {
        self.klend_reserve != Pubkey::default()
    }

    pub fn is_configured(&self) -> bool {
        self.uses_scope() || self.uses_pyth()
    }

    pub fn twap_check_enabled(&self) -> bool {
        self.max_twap_divergence_bps > 0
    }

    /// Every account `get_price` may read for this asset.
    pub fn accounts(&self) -> impl Iterator<Item = Pubkey> {
        [self.scope_prices, self.pyth_price, self.pyth_factor, self.klend_reserve]
            .into_iter()
            .filter(|key| *key != Pubkey::default())
    }
}

/// Token identity, oracle and risk limits for one mint.
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
    pub max_collateral_per_margin: u64,
    pub ltv_bps: u16,
    pub liquidation_threshold_bps: u16,
    pub liquidation_bonus_bps: u16,
    pub asset_index: u16,
    pub decimals: u8,
    pub collateral_enabled: bool,
    pub borrow_enabled: bool,
    pub bump: u8,
    pub oracle: OracleConfig,
    pub reserved: [u8; 32],
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
