pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod interface;
pub mod risk_engine;
pub mod state;
pub mod validation;

use anchor_lang::prelude::*;
use instructions::*;
use state::reserve::RateCurve;

declare_id!("BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg");

#[program]
pub mod vanna_credit_layer {
    use super::*;

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

    #[allow(clippy::too_many_arguments)]
    pub fn admin_register_asset<'info>(
        ctx: Context<'info, AdminRegisterAsset<'info>>,
        max_collateral_per_margin: u64,
        ltv_bps: u16,
        liquidation_threshold_bps: u16,
        liquidation_bonus_bps: u16,
        collateral_enabled: bool,
        borrow_enabled: bool,
    ) -> Result<()> {
        instructions::admin::admin_register_asset(
            ctx,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        )
    }

    pub fn admin_register_venue(ctx: Context<AdminRegisterVenue>, venue: Pubkey) -> Result<()> {
        instructions::admin::admin_register_venue(ctx, venue)
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

    pub fn admin_set_asset_oracle<'info>(ctx: Context<'info, AdminSetAssetOracle<'info>>) -> Result<()> {
        instructions::admin::admin_set_asset_oracle(ctx)
    }

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

    pub fn admin_register_integration(ctx: Context<AdminRegisterIntegration>) -> Result<()> {
        instructions::admin::admin_register_integration(ctx)
    }

    pub fn admin_set_integration_validator(ctx: Context<AdminSetIntegrationValidator>) -> Result<()> {
        instructions::admin::admin_set_integration_validator(ctx)
    }

    pub fn admin_set_integration_enabled(ctx: Context<AdminSetIntegrationEnabled>, enabled: bool) -> Result<()> {
        instructions::admin::admin_set_integration_enabled(ctx, enabled)
    }

    pub fn lender_supply(ctx: Context<LenderSupply>, assets: u64, min_shares_out: u64) -> Result<()> {
        instructions::lending_pool::lender_supply(ctx, assets, min_shares_out)
    }

    pub fn lender_redeem(ctx: Context<LenderRedeem>, shares: u64, min_assets_out: u64) -> Result<()> {
        instructions::lending_pool::lender_redeem(ctx, shares, min_assets_out)
    }

    pub fn public_refresh_reserve(ctx: Context<PublicRefreshReserve>) -> Result<()> {
        instructions::lending_pool::public_refresh_reserve(ctx)
    }

    pub fn user_create_margin(ctx: Context<UserCreateMargin>) -> Result<()> {
        instructions::account_manager::user_create_margin(ctx)
    }

    pub fn user_close_margin(ctx: Context<UserCloseMargin>) -> Result<()> {
        instructions::account_manager::user_close_margin(ctx)
    }

    pub fn user_reclaim_rent(ctx: Context<UserReclaimRent>) -> Result<()> {
        instructions::account_manager::user_reclaim_rent(ctx)
    }

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

    pub fn user_borrow<'info>(ctx: Context<'info, UserBorrow<'info>>, assets: u64, max_debt_shares: u128) -> Result<()> {
        instructions::account_manager::user_borrow(ctx, assets, max_debt_shares)
    }

    pub fn user_repay_from_margin(ctx: Context<UserRepayFromMargin>, max_assets: u64, repay_all: bool) -> Result<()> {
        instructions::account_manager::user_repay_from_margin(ctx, max_assets, repay_all)
    }

    pub fn margin_execute<'info>(
        ctx: Context<'info, MarginExecute<'info>>,
        data: Vec<u8>,
        call_account_count: u16,
        new_assets: u8,
    ) -> Result<()> {
        instructions::account_manager::margin_execute(ctx, data, call_account_count, new_assets)
    }

    pub fn public_venue_settle<'info>(ctx: Context<'info, PublicVenueSettle<'info>>) -> Result<()> {
        instructions::account_manager::public_venue_settle(ctx)
    }

    pub fn public_venue_unwind<'info>(
        ctx: Context<'info, PublicVenueUnwind<'info>>,
        data: Vec<u8>,
        call_account_count: u16,
    ) -> Result<()> {
        instructions::account_manager::public_venue_unwind(ctx, data, call_account_count)
    }

    pub fn public_liquidate<'info>(ctx: Context<'info, PublicLiquidate<'info>>) -> Result<()> {
        instructions::account_manager::public_liquidate(ctx)
    }
}
