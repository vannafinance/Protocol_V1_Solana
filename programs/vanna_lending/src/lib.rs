pub mod constants;
pub mod errors;
pub mod events;
pub mod external;
pub mod instructions;
pub mod math;
pub mod oracle;
pub mod state;
pub mod validation;

use anchor_lang::prelude::*;
use instructions::*;

declare_id!("BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg");

#[program]
pub mod vanna_lending {
    use super::*;

    pub fn user_margin_swap<'info>(ctx: Context<'info, UserMarginSwap<'info>>, amount_in: u64, min_amount_out: u64, route_account_count: u16, route_data: Vec<u8>) -> Result<()> {
        instructions::swap::user_margin_swap(ctx, amount_in, min_amount_out, route_account_count, route_data)
    }

    // -- Governance -----------------------------------------------------

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
    pub fn admin_register_asset(
        ctx: Context<AdminRegisterAsset>,
        price_feed_id: [u8; 32],
        max_collateral_per_margin: u64,
        ltv_bps: u16,
        liquidation_threshold_bps: u16,
        liquidation_bonus_bps: u16,
        max_confidence_bps: u16,
        max_price_age_secs: u32,
        collateral_enabled: bool,
        borrow_enabled: bool,
    ) -> Result<()> {
        instructions::admin::admin_register_asset(
            ctx,
            price_feed_id,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            max_confidence_bps,
            max_price_age_secs,
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
        max_confidence_bps: u16,
        max_price_age_secs: u32,
        collateral_enabled: bool,
        borrow_enabled: bool,
    ) -> Result<()> {
        instructions::admin::admin_update_asset_config(
            ctx,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            max_confidence_bps,
            max_price_age_secs,
            collateral_enabled,
            borrow_enabled,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admin_initialize_reserve(
        ctx: Context<AdminInitializeReserve>,
        base_rate_bps: u16,
        slope1_bps: u16,
        slope2_bps: u16,
        optimal_utilization_bps: u16,
        reserve_factor_bps: u16,
        supply_cap: u64,
        borrow_cap: u64,
        status: u8,
    ) -> Result<()> {
        instructions::admin::admin_initialize_reserve(
            ctx,
            base_rate_bps,
            slope1_bps,
            slope2_bps,
            optimal_utilization_bps,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admin_update_reserve_config(
        ctx: Context<AdminUpdateReserveConfig>,
        base_rate_bps: u16,
        slope1_bps: u16,
        slope2_bps: u16,
        optimal_utilization_bps: u16,
        reserve_factor_bps: u16,
        supply_cap: u64,
        borrow_cap: u64,
        status: u8,
    ) -> Result<()> {
        instructions::admin::admin_update_reserve_config(
            ctx,
            base_rate_bps,
            slope1_bps,
            slope2_bps,
            optimal_utilization_bps,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        )
    }

    pub fn admin_collect_protocol_fees(ctx: Context<AdminCollectProtocolFees>, amount: u64) -> Result<()> {
        instructions::admin::admin_collect_protocol_fees(ctx, amount)
    }

    pub fn admin_register_lite_strategy(ctx: Context<AdminRegisterLiteStrategy>) -> Result<()> {
        instructions::lite::admin_register_lite_strategy(ctx)
    }

    // -- Margin lifecycle -------------------------------------------------

    pub fn user_create_margin(ctx: Context<UserCreateMargin>) -> Result<()> {
        instructions::margin::user_create_margin(ctx)
    }

    pub fn user_close_margin(ctx: Context<UserCloseMargin>) -> Result<()> {
        instructions::margin::user_close_margin(ctx)
    }

    pub fn user_close_collateral_position(ctx: Context<UserCloseCollateralPosition>) -> Result<()> {
        instructions::margin::user_close_collateral_position(ctx)
    }

    pub fn user_deposit_collateral(ctx: Context<UserDepositCollateral>, amount: u64) -> Result<()> {
        instructions::margin::user_deposit_collateral(ctx, amount)
    }

    pub fn user_withdraw_collateral(
        ctx: Context<UserWithdrawCollateral>,
        amount: u64,
        min_health_factor_wad: u128,
    ) -> Result<()> {
        instructions::margin::user_withdraw_collateral(ctx, amount, min_health_factor_wad)
    }

    // -- Lender operations -------------------------------------------------

    pub fn lender_supply(ctx: Context<LenderSupply>, assets: u64, min_shares_out: u64) -> Result<()> {
        instructions::lending::lender_supply(ctx, assets, min_shares_out)
    }

    pub fn lender_redeem(ctx: Context<LenderRedeem>, shares: u64, min_assets_out: u64) -> Result<()> {
        instructions::lending::lender_redeem(ctx, shares, min_assets_out)
    }

    pub fn public_refresh_reserve(ctx: Context<PublicRefreshReserve>) -> Result<()> {
        instructions::lending::public_refresh_reserve(ctx)
    }

    // -- Borrow and repay -------------------------------------------------

    pub fn user_open_debt_position(ctx: Context<UserOpenDebtPosition>) -> Result<()> {
        instructions::borrowing::user_open_debt_position(ctx)
    }

    pub fn user_close_debt_position(ctx: Context<UserCloseDebtPosition>) -> Result<()> {
        instructions::borrowing::user_close_debt_position(ctx)
    }

    pub fn user_borrow(ctx: Context<UserBorrow>, assets: u64, max_debt_shares: u128) -> Result<()> {
        instructions::borrowing::user_borrow(ctx, assets, max_debt_shares)
    }

    pub fn user_repay_from_margin(ctx: Context<UserRepayFromMargin>, max_assets: u64, repay_all: bool) -> Result<()> {
        instructions::borrowing::user_repay_from_margin(ctx, max_assets, repay_all)
    }

    pub fn public_repay_from_wallet(
        ctx: Context<PublicRepayFromWallet>,
        max_assets: u64,
        repay_all: bool,
    ) -> Result<()> {
        instructions::borrowing::public_repay_from_wallet(ctx, max_assets, repay_all)
    }

    // -- Liquidation -------------------------------------------------------

    pub fn public_liquidate(ctx: Context<PublicLiquidate>, max_repay_assets: u64, min_collateral_out: u64) -> Result<()> {
        instructions::liquidation::public_liquidate(ctx, max_repay_assets, min_collateral_out)
    }

    // -- Composite -----------------------------------------------------------

    pub fn user_deposit_and_borrow(
        ctx: Context<UserDepositAndBorrow>,
        deposit_amount: u64,
        borrow_amount: u64,
        max_debt_shares: u128,
    ) -> Result<()> {
        instructions::composite::user_deposit_and_borrow(ctx, deposit_amount, borrow_amount, max_debt_shares)
    }

    // -- Lite / Stocks (Kamino) ----------------------------------------------

    pub fn lite_open(ctx: Context<LiteOpen>, equity: u64, leverage_bps: u64) -> Result<()> {
        instructions::lite::lite_open(ctx, equity, leverage_bps)
    }

    pub fn lite_supply(ctx: Context<LiteSupply>, amount: u64, attribute_shares_delta: u128) -> Result<()> {
        instructions::lite::lite_supply(ctx, amount, attribute_shares_delta)
    }

    pub fn lite_reduce(ctx: Context<LiteClose>, exit_bps: u16, min_underlying_out: u64) -> Result<()> {
        instructions::lite::lite_reduce(ctx, exit_bps, min_underlying_out)
    }

    pub fn lite_close(ctx: Context<LiteClose>, min_underlying_out: u64) -> Result<()> {
        instructions::lite::lite_close(ctx, min_underlying_out)
    }

    pub fn lite_reduce_and_repay<'info>(
        ctx: Context<'info, LiteReduceAndRepay<'info>>,
        exit_bps: u16,
        min_yield_out: u64,
        min_stock_out: u64,
        route_account_count: u16,
        route_data: Vec<u8>,
    ) -> Result<()> {
        instructions::lite::lite_reduce_and_repay(
            ctx,
            exit_bps,
            min_yield_out,
            min_stock_out,
            route_account_count,
            route_data,
        )
    }
}
