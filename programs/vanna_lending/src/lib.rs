pub mod adapters;
pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod oracle;
pub mod risk_engine;
pub mod state;
pub mod validation;

use anchor_lang::prelude::*;
use adapters::AdapterKind;
use instructions::*;
use state::asset_config::OracleConfig;
use state::reserve::RateCurve;

declare_id!("BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg");

#[program]
pub mod vanna_lending {
    use super::*;

    // -- Admin: protocol -------------------------------------------------------------------------

    pub fn initialize_protocol(ctx: Context<InitializeProtocol>, treasury: Pubkey, max_assets_per_margin: u8) -> Result<()> {
        instructions::admin::initialize_protocol(ctx, treasury, max_assets_per_margin)
    }

    pub fn admin_propose_authority(ctx: Context<AdminProposeAuthority>, new_admin: Pubkey) -> Result<()> {
        instructions::admin::admin_propose_authority(ctx, new_admin)
    }

    pub fn authority_accept_admin(ctx: Context<AuthorityAcceptAdmin>) -> Result<()> {
        instructions::admin::authority_accept_admin(ctx)
    }

    pub fn admin_set_operating_mode(ctx: Context<AdminSetOperatingMode>, new_mode: u8) -> Result<()> {
        instructions::admin::admin_set_operating_mode(ctx, new_mode)
    }

    // -- Admin: assets ---------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn admin_register_asset(
        ctx: Context<AdminRegisterAsset>,
        oracle: OracleConfig,
        max_collateral_per_margin: u64,
        ltv_bps: u16,
        liquidation_threshold_bps: u16,
        liquidation_bonus_bps: u16,
        collateral_enabled: bool,
        borrow_enabled: bool,
    ) -> Result<()> {
        instructions::admin::admin_register_asset(
            ctx,
            oracle,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admin_update_asset_config(
        ctx: Context<AdminUpdateAssetConfig>,
        max_collateral_per_margin: u64,
        ltv_bps: u16,
        liquidation_threshold_bps: u16,
        liquidation_bonus_bps: u16,
        collateral_enabled: bool,
        borrow_enabled: bool,
    ) -> Result<()> {
        instructions::admin::admin_update_asset_config(
            ctx,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        )
    }

    pub fn admin_set_asset_oracle(ctx: Context<AdminSetAssetOracle>, oracle: OracleConfig) -> Result<()> {
        instructions::admin::admin_set_asset_oracle(ctx, oracle)
    }

    // -- Admin: lending pools --------------------------------------------------------------------

    pub fn admin_initialize_reserve(
        ctx: Context<AdminInitializeReserve>,
        rate_curve: RateCurve,
        reserve_factor_bps: u16,
        supply_cap: u64,
        borrow_cap: u64,
        status: u8,
    ) -> Result<()> {
        instructions::admin::admin_initialize_reserve(
            ctx,
            rate_curve,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        )
    }

    pub fn admin_update_reserve_config(
        ctx: Context<AdminUpdateReserveConfig>,
        rate_curve: RateCurve,
        reserve_factor_bps: u16,
        supply_cap: u64,
        borrow_cap: u64,
        status: u8,
    ) -> Result<()> {
        instructions::admin::admin_update_reserve_config(
            ctx,
            rate_curve,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        )
    }

    pub fn admin_collect_protocol_fees(ctx: Context<AdminCollectProtocolFees>, amount: u64) -> Result<()> {
        instructions::admin::admin_collect_protocol_fees(ctx, amount)
    }

    // -- Admin: external integrations ------------------------------------------------------------

    pub fn admin_register_integration(ctx: Context<AdminRegisterIntegration>, adapter: AdapterKind) -> Result<()> {
        instructions::admin::admin_register_integration(ctx, adapter)
    }

    pub fn admin_set_integration_enabled(ctx: Context<AdminSetIntegrationEnabled>, enabled: bool) -> Result<()> {
        instructions::admin::admin_set_integration_enabled(ctx, enabled)
    }

    // -- Lending pool ----------------------------------------------------------------------------

    pub fn lender_supply(ctx: Context<LenderSupply>, assets: u64, min_shares_out: u64) -> Result<()> {
        instructions::lending_pool::lender_supply(ctx, assets, min_shares_out)
    }

    pub fn lender_redeem(ctx: Context<LenderRedeem>, shares: u64, min_assets_out: u64) -> Result<()> {
        instructions::lending_pool::lender_redeem(ctx, shares, min_assets_out)
    }

    pub fn public_refresh_reserve(ctx: Context<PublicRefreshReserve>) -> Result<()> {
        instructions::lending_pool::public_refresh_reserve(ctx)
    }

    // -- Account manager: account ----------------------------------------------------------------

    pub fn user_create_margin(ctx: Context<UserCreateMargin>) -> Result<()> {
        instructions::account_manager::user_create_margin(ctx)
    }

    pub fn user_close_margin(ctx: Context<UserCloseMargin>) -> Result<()> {
        instructions::account_manager::user_close_margin(ctx)
    }

    // -- Account manager: collateral -------------------------------------------------------------

    pub fn user_deposit_collateral(ctx: Context<UserDepositCollateral>, amount: u64) -> Result<()> {
        instructions::account_manager::user_deposit_collateral(ctx, amount)
    }

    pub fn user_withdraw_collateral<'info>(
        ctx: Context<'info, UserWithdrawCollateral<'info>>,
        amount: u64,
        min_health_factor_wad: u128,
    ) -> Result<()> {
        instructions::account_manager::user_withdraw_collateral(ctx, amount, min_health_factor_wad)
    }

    pub fn user_close_collateral_position(ctx: Context<UserCloseCollateralPosition>) -> Result<()> {
        instructions::account_manager::user_close_collateral_position(ctx)
    }

    // -- Account manager: borrow and repay -------------------------------------------------------

    pub fn user_open_debt_position(ctx: Context<UserOpenDebtPosition>) -> Result<()> {
        instructions::account_manager::user_open_debt_position(ctx)
    }

    pub fn user_close_debt_position(ctx: Context<UserCloseDebtPosition>) -> Result<()> {
        instructions::account_manager::user_close_debt_position(ctx)
    }

    pub fn user_borrow<'info>(ctx: Context<'info, UserBorrow<'info>>, assets: u64, max_debt_shares: u128) -> Result<()> {
        instructions::account_manager::user_borrow(ctx, assets, max_debt_shares)
    }

    pub fn user_repay_from_margin(ctx: Context<UserRepayFromMargin>, max_assets: u64, repay_all: bool) -> Result<()> {
        instructions::account_manager::user_repay_from_margin(ctx, max_assets, repay_all)
    }

    pub fn public_repay_from_wallet(
        ctx: Context<PublicRepayFromWallet>,
        max_assets: u64,
        repay_all: bool,
    ) -> Result<()> {
        instructions::account_manager::public_repay_from_wallet(ctx, max_assets, repay_all)
    }

    // -- Account manager: external calls ---------------------------------------------------------

    pub fn margin_execute<'info>(
        ctx: Context<'info, MarginExecute<'info>>,
        data: Vec<u8>,
        cpi_account_count: u16,
        min_received: u64,
    ) -> Result<()> {
        instructions::account_manager::margin_execute(ctx, data, cpi_account_count, min_received)
    }

    // -- Account manager: liquidation ------------------------------------------------------------

    pub fn public_liquidate<'info>(ctx: Context<'info, PublicLiquidate<'info>>) -> Result<()> {
        instructions::account_manager::public_liquidate(ctx)
    }
}
