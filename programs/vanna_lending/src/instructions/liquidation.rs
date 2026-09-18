use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::fixed_point::{mul_div_floor, u64_from_u128};
use crate::math::health::{
    calculate_health, normalize_token_value, value_to_token_amount, CollateralValuation, DebtValuation,
};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::accounts::validate_asset_config;
use crate::validation::positions::scan_and_validate_positions;
use crate::validation::token::{transfer_in_measured, transfer_out_checked_measured, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

#[derive(Accounts)]
pub struct PublicLiquidate<'info> {
    #[account(mut)]
    pub liquidator: Signer<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,

    #[account(seeds = [ASSET_SEED, debt_mint.key().as_ref()], bump = debt_asset_config.bump)]
    pub debt_asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, debt_mint.key().as_ref()], bump = debt_reserve.bump)]
    pub debt_reserve: Box<Account<'info, Reserve>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), debt_reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    pub debt_price_update: Box<Account<'info, PriceUpdateV2>>,
    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = debt_mint,
        token::authority = liquidator,
        token::token_program = token_program
    )]
    pub liquidator_debt_source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = debt_mint,
        token::authority = debt_reserve,
        token::token_program = token_program
    )]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(seeds = [ASSET_SEED, collateral_mint.key().as_ref()], bump = collateral_asset_config.bump)]
    pub collateral_asset_config: Box<Account<'info, AssetConfig>>,
    pub collateral_price_update: Box<Account<'info, PriceUpdateV2>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = liquidator,
        associated_token::mint = collateral_mint,
        associated_token::authority = liquidator,
        associated_token::token_program = token_program,
    )]
    pub liquidator_collateral_destination: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = collateral_mint,
        token::authority = margin_account,
        token::token_program = token_program
    )]
    pub collateral_margin_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Debt and collateral legs must share a token program (classic SPL or Token-2022).
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn public_liquidate(ctx: Context<PublicLiquidate>, max_repay_assets: u64, min_collateral_out: u64) -> Result<()> {
    require!(ctx.accounts.collateral_asset_config.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    require!(max_repay_assets > 0, VannaError::ZeroAmount);
    validate_asset_config(
        &ctx.accounts.debt_asset_config,
        &ctx.accounts.debt_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    validate_asset_config(
        &ctx.accounts.collateral_asset_config,
        &ctx.accounts.collateral_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    verify_associated_token_account(
        &ctx.accounts.collateral_margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.collateral_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;

    let clock = Clock::get()?;
    let accrual = accrue(&ctx.accounts.debt_reserve, clock.unix_timestamp)?;
    ctx.accounts.debt_reserve.total_borrow_assets = accrual.new_total_borrow_assets;
    ctx.accounts.debt_reserve.accrued_protocol_fees = accrual.new_accrued_protocol_fees;
    ctx.accounts.debt_reserve.borrow_index_wad = accrual.new_borrow_index_wad;
    ctx.accounts.debt_reserve.last_update_timestamp = clock.unix_timestamp;

    let margin_key = ctx.accounts.margin_account.key();
    let (other_collaterals, other_debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        Some(ctx.accounts.collateral_asset_config.asset_index),
        Some(ctx.accounts.debt_asset_config.asset_index),
        None,
    )?;
    let other_collateral_valuations: Vec<CollateralValuation> = other_collaterals
        .iter()
        .map(|c| CollateralValuation {
            collateral_value: c.valuation.collateral_value,
        })
        .collect();
    let other_debt_valuations: Vec<DebtValuation> =
        other_debts.iter().map(|d| DebtValuation { debt_value: d.valuation.debt_value }).collect();

    let debt_price = load_validated_price(&ctx.accounts.debt_asset_config, &ctx.accounts.debt_price_update, &clock)?;
    let collateral_price =
        load_validated_price(&ctx.accounts.collateral_asset_config, &ctx.accounts.collateral_price_update, &clock)?;

    let current_debt_assets = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.debt_reserve.total_borrow_shares,
        ctx.accounts.debt_reserve.total_borrow_assets,
    )?;
    require!(current_debt_assets > 0, VannaError::PositionHealthy);
    let current_debt_value = normalize_token_value(
        current_debt_assets,
        debt_price.price,
        debt_price.exponent,
        ctx.accounts.debt_asset_config.decimals,
        true,
    )?;
    let collateral_vault_balance_before = ctx.accounts.collateral_margin_vault.amount;
    let current_collateral_value = normalize_token_value(
        collateral_vault_balance_before,
        collateral_price.price,
        collateral_price.exponent,
        ctx.accounts.collateral_asset_config.decimals,
        false,
    )?;

    let mut pre_debts = other_debt_valuations.clone();
    pre_debts.push(DebtValuation { debt_value: current_debt_value });
    let mut pre_collaterals = other_collateral_valuations.clone();
    pre_collaterals.push(CollateralValuation {
        collateral_value: current_collateral_value,
    });
    let health_before = calculate_health(&pre_collaterals, &pre_debts)?;
    require!(health_before.is_liquidatable(), VannaError::PositionHealthy);

    // Spec §6.5 — bound repayment by the caller's own ceiling, the outstanding debt, and the
    // (currently compiled, pre-audit) close factor.
    let close_factor_cap = u64_from_u128(mul_div_floor(current_debt_assets as u128, CLOSE_FACTOR_BPS as u128, 10_000)?)?;
    let repay_amount = max_repay_assets.min(current_debt_assets).min(close_factor_cap.max(1));
    require!(repay_amount > 0, VannaError::ZeroAmount);

    let repay_value = normalize_token_value(
        repay_amount,
        debt_price.price,
        debt_price.exponent,
        ctx.accounts.debt_asset_config.decimals,
        true,
    )?;
    let bonus_numerator = 10_000u128
        .checked_add(ctx.accounts.collateral_asset_config.liquidation_bonus_bps as u128)
        .ok_or(VannaError::MathOverflow)?;
    let seize_value = mul_div_floor(repay_value, bonus_numerator, 10_000)?;
    let mut seize_amount = value_to_token_amount(
        seize_value,
        collateral_price.price,
        collateral_price.exponent,
        ctx.accounts.collateral_asset_config.decimals,
        false,
    )?;
    seize_amount = seize_amount.min(collateral_vault_balance_before);
    require!(seize_amount >= min_collateral_out, VannaError::SlippageExceeded);

    // Actually pull the repay tokens from the liquidator before mutating any state, so a failed
    // transfer aborts the whole transaction atomically before accounting changes are computed.
    let received_repay = transfer_in_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.debt_mint,
        &ctx.accounts.liquidator_debt_source,
        &mut ctx.accounts.debt_reserve_vault,
        &ctx.accounts.liquidator,
        repay_amount,
    )?;

    let debt_shares_burned = if received_repay >= current_debt_assets {
        ctx.accounts.debt_position.borrow_shares
    } else {
        mul_div_floor(
            received_repay as u128,
            ctx.accounts.debt_reserve.total_borrow_shares,
            ctx.accounts.debt_reserve.total_borrow_assets as u128,
        )?
        .min(ctx.accounts.debt_position.borrow_shares)
    };

    ctx.accounts.debt_reserve.accounted_liquidity_assets = ctx
        .accounts
        .debt_reserve
        .accounted_liquidity_assets
        .checked_add(received_repay)
        .ok_or(VannaError::MathOverflow)?;
    ctx.accounts.debt_reserve.total_borrow_assets =
        ctx.accounts.debt_reserve.total_borrow_assets.saturating_sub(received_repay);
    ctx.accounts.debt_reserve.total_borrow_shares =
        ctx.accounts.debt_reserve.total_borrow_shares.saturating_sub(debt_shares_burned);
    ctx.accounts.debt_position.debit_shares(debt_shares_burned)?;
    if ctx.accounts.debt_position.borrow_shares == 0 {
        ctx.accounts
            .margin_account
            .remove_active_debt(ctx.accounts.debt_asset_config.asset_index)?;
    }

    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];
    let margin_account_info = ctx.accounts.margin_account.to_account_info();
    let seized = transfer_out_checked_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.collateral_mint,
        &ctx.accounts.collateral_margin_vault,
        &mut ctx.accounts.liquidator_collateral_destination,
        &margin_account_info,
        margin_signer_seeds,
        seize_amount,
    )?;
    let post_collateral_vault_balance = collateral_vault_balance_before
        .checked_sub(seized)
        .ok_or(VannaError::MathUnderflow)?;
    if post_collateral_vault_balance == 0 {
        ctx.accounts
            .margin_account
            .remove_active_collateral(ctx.accounts.collateral_asset_config.asset_index)?;
    }

    let post_debt_assets = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.debt_reserve.total_borrow_shares,
        ctx.accounts.debt_reserve.total_borrow_assets,
    )?;
    let post_debt_value = if post_debt_assets == 0 {
        0
    } else {
        normalize_token_value(post_debt_assets, debt_price.price, debt_price.exponent, ctx.accounts.debt_asset_config.decimals, true)?
    };
    let post_collateral_value = if post_collateral_vault_balance == 0 {
        0
    } else {
        normalize_token_value(
            post_collateral_vault_balance,
            collateral_price.price,
            collateral_price.exponent,
            ctx.accounts.collateral_asset_config.decimals,
            false,
        )?
    };
    let mut post_debts = other_debt_valuations;
    post_debts.push(DebtValuation { debt_value: post_debt_value });
    let mut post_collaterals = other_collateral_valuations;
    post_collaterals.push(CollateralValuation {
        collateral_value: post_collateral_value,
    });
    let health_after = calculate_health(&post_collaterals, &post_debts)?;
    require!(
        health_after.total_debt_value == 0 || health_after.liquidation_health_factor_wad > health_before.liquidation_health_factor_wad,
        VannaError::HealthFactorTooLow
    );

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(Liquidated {
        margin_account: ctx.accounts.margin_account.key(),
        liquidator: ctx.accounts.liquidator.key(),
        debt_reserve: ctx.accounts.debt_reserve.key(),
        collateral_mint: ctx.accounts.collateral_mint.key(),
        debt_repaid: received_repay,
        collateral_seized: seized,
        pre_liquidation_health_factor_wad: health_before.liquidation_health_factor_wad,
        post_liquidation_health_factor_wad: health_after.liquidation_health_factor_wad,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}
