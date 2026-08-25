use anchor_lang::prelude::*;

#[event]
pub struct ProtocolInitialized {
    pub admin: Pubkey,
    pub treasury: Pubkey,
    pub max_assets_per_margin: u8,
    pub timestamp: i64,
}

#[event]
pub struct AdminProposed {
    pub current_admin: Pubkey,
    pub pending_admin: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct AdminAccepted {
    pub previous_admin: Pubkey,
    pub new_admin: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct OperatingModeChanged {
    pub old_mode: u8,
    pub new_mode: u8,
    pub timestamp: i64,
}

#[event]
pub struct AssetRegistered {
    pub asset_config: Pubkey,
    pub mint: Pubkey,
    pub asset_index: u16,
    pub ltv_bps: u16,
    pub liquidation_threshold_bps: u16,
    pub liquidation_bonus_bps: u16,
    pub timestamp: i64,
}

#[event]
pub struct AssetConfigUpdated {
    pub asset_config: Pubkey,
    pub ltv_bps: u16,
    pub liquidation_threshold_bps: u16,
    pub liquidation_bonus_bps: u16,
    pub max_collateral_per_margin: u64,
    pub collateral_enabled: bool,
    pub borrow_enabled: bool,
    pub timestamp: i64,
}

#[event]
pub struct ReserveInitialized {
    pub reserve: Pubkey,
    pub underlying_mint: Pubkey,
    pub liquidity_vault: Pubkey,
    pub share_mint: Pubkey,
    pub supply_cap: u64,
    pub borrow_cap: u64,
    pub timestamp: i64,
}

#[event]
pub struct ReserveConfigUpdated {
    pub reserve: Pubkey,
    pub base_rate_bps: u16,
    pub slope1_bps: u16,
    pub slope2_bps: u16,
    pub optimal_utilization_bps: u16,
    pub reserve_factor_bps: u16,
    pub supply_cap: u64,
    pub borrow_cap: u64,
    pub status: u8,
    pub timestamp: i64,
}

#[event]
pub struct ReserveAccrued {
    pub reserve: Pubkey,
    pub total_borrow_assets: u64,
    pub accounted_liquidity_assets: u64,
    pub borrow_index_wad: u128,
    pub accrued_protocol_fees: u64,
    pub timestamp: i64,
}

#[event]
pub struct ProtocolFeesCollected {
    pub reserve: Pubkey,
    pub treasury_ata: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct MarginCreated {
    pub margin_account: Pubkey,
    pub authority: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct MarginClosed {
    pub margin_account: Pubkey,
    pub authority: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct CollateralPositionClosed {
    pub margin_account: Pubkey,
    pub mint: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DebtPositionOpened {
    pub margin_account: Pubkey,
    pub reserve: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DebtPositionClosed {
    pub margin_account: Pubkey,
    pub reserve: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct CollateralDeposited {
    pub margin_account: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub new_collateral_amount: u64,
    pub event_sequence: u64,
    pub timestamp: i64,
}

#[event]
pub struct CollateralWithdrawn {
    pub margin_account: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub new_collateral_amount: u64,
    pub borrow_health_factor_wad: u128,
    pub event_sequence: u64,
    pub timestamp: i64,
}

#[event]
pub struct LiquiditySupplied {
    pub reserve: Pubkey,
    pub lender: Pubkey,
    pub assets: u64,
    pub shares: u64,
    pub total_borrow_assets: u64,
    pub accounted_liquidity_assets: u64,
    pub timestamp: i64,
}

#[event]
pub struct LiquidityRedeemed {
    pub reserve: Pubkey,
    pub lender: Pubkey,
    pub assets: u64,
    pub shares: u64,
    pub total_borrow_assets: u64,
    pub accounted_liquidity_assets: u64,
    pub timestamp: i64,
}

#[event]
pub struct Borrowed {
    pub margin_account: Pubkey,
    pub reserve: Pubkey,
    pub assets: u64,
    pub debt_shares: u128,
    pub total_debt_shares: u128,
    pub borrow_health_factor_wad: u128,
    pub event_sequence: u64,
    pub timestamp: i64,
}

#[event]
pub struct DebtRepaid {
    pub margin_account: Pubkey,
    pub reserve: Pubkey,
    pub payer: Pubkey,
    pub assets: u64,
    pub debt_shares_burned: u128,
    pub remaining_debt_shares: u128,
    pub event_sequence: u64,
    pub timestamp: i64,
}

#[event]
pub struct Liquidated {
    pub margin_account: Pubkey,
    pub liquidator: Pubkey,
    pub debt_reserve: Pubkey,
    pub collateral_mint: Pubkey,
    pub debt_repaid: u64,
    pub collateral_seized: u64,
    pub pre_liquidation_health_factor_wad: u128,
    pub post_liquidation_health_factor_wad: u128,
    pub event_sequence: u64,
    pub timestamp: i64,
}
