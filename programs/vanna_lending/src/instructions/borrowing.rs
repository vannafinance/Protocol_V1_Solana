use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::fixed_point::mul_div_floor;
use crate::math::health::{calculate_health, normalize_token_value, CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::{assets_to_debt_shares_up, debt_shares_to_assets_up};
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::state::reserve::Reserve;
use crate::validation::accounts::{
    assert_protocol_action_allowed, assert_reserve_action_allowed, validate_asset_config, ProtocolAction,
};
use crate::validation::positions::scan_and_validate_positions;
use crate::validation::token::{transfer_in_measured, transfer_out_checked_measured, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

// `pub(crate)` (not private) so `instructions::composite::user_deposit_and_borrow` can reuse the
// exact same accrual step instead of duplicating it.
pub(crate) fn apply_accrual(reserve: &mut Account<Reserve>, now: i64) -> Result<()> {
    let accrual = accrue(reserve, now)?;
    reserve.total_borrow_assets = accrual.new_total_borrow_assets;
    reserve.accrued_protocol_fees = accrual.new_accrued_protocol_fees;
    reserve.borrow_index_wad = accrual.new_borrow_index_wad;
    reserve.last_update_timestamp = now;
    Ok(())
}

// ---------------------------------------------------------------------------
// user_open_debt_position
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserOpenDebtPosition<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, asset_config.mint.as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(
        seeds = [RESERVE_SEED, asset_config.mint.as_ref()],
        bump = reserve.bump,
        constraint = asset_config.reserve == reserve.key() @ VannaError::InvalidPda
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        init,
        payer = payer,
        space = 8 + DebtPosition::INIT_SPACE,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    pub system_program: Program<'info, System>,
}

pub fn user_open_debt_position(ctx: Context<UserOpenDebtPosition>) -> Result<()> {
    require!(ctx.accounts.asset_config.borrow_enabled, VannaError::AssetNotBorrowEnabled);

    let position = &mut ctx.accounts.debt_position;
    position.margin_account = ctx.accounts.margin_account.key();
    position.reserve = ctx.accounts.reserve.key();
    position.borrow_shares = 0;
    position.bump = ctx.bumps.debt_position;
    position.reserved = [0u8; 48];

    emit!(DebtPositionOpened {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.reserve.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_close_debt_position
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCloseDebtPosition<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, asset_config.mint.as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(
        seeds = [RESERVE_SEED, asset_config.mint.as_ref()],
        bump = reserve.bump,
        constraint = asset_config.reserve == reserve.key() @ VannaError::InvalidPda
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump,
        close = authority
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
}

pub fn user_close_debt_position(ctx: Context<UserCloseDebtPosition>) -> Result<()> {
    require!(ctx.accounts.debt_position.borrow_shares == 0, VannaError::OutstandingDebt);
    require!(
        !ctx.accounts.margin_account.is_debt_active(ctx.accounts.asset_config.asset_index),
        VannaError::OutstandingDebt
    );

    emit!(DebtPositionClosed {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.reserve.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_borrow
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserBorrow<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    /// Pyth price update for the borrowed asset itself.
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = mint, token::authority = reserve)]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,
    /// Borrowed funds land here as protocol-controlled collateral credit (spec §1.2). Created on
    /// first use if this margin account has never held this asset before.
    #[account(
        init_if_needed,
        payer = authority,
        associated_token::mint = mint,
        associated_token::authority = margin_account
    )]
    pub margin_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn user_borrow(ctx: Context<UserBorrow>, assets: u64, max_debt_shares: u128) -> Result<()> {
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::Borrow)?;
    assert_reserve_action_allowed(ctx.accounts.reserve.status, ProtocolAction::Borrow)?;
    require!(ctx.accounts.asset_config.borrow_enabled, VannaError::AssetNotBorrowEnabled);
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
    )?;
    require!(assets > 0, VannaError::ZeroAmount);

    let clock = Clock::get()?;
    apply_accrual(&mut ctx.accounts.reserve, clock.unix_timestamp)?;

    require!(
        ctx.accounts.reserve.borrow_cap == UNCAPPED
            || ctx
                .accounts
                .reserve
                .total_borrow_assets
                .checked_add(assets)
                .ok_or(VannaError::MathOverflow)?
                <= ctx.accounts.reserve.borrow_cap,
        VannaError::BorrowCapExceeded
    );
    require!(assets <= ctx.accounts.reserve.accounted_liquidity_assets, VannaError::InsufficientLiquidity);

    let new_debt_shares = assets_to_debt_shares_up(
        assets,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    require!(new_debt_shares <= max_debt_shares, VannaError::SlippageExceeded);

    let was_zero_collateral = ctx.accounts.margin_vault.amount == 0;

    let margin_key = ctx.accounts.margin_account.key();
    let (scanned_collaterals, other_debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        Some(ctx.accounts.asset_config.asset_index),
        Some(ctx.accounts.asset_config.asset_index),
    )?;
    let mut collaterals: Vec<CollateralValuation> =
        scanned_collaterals.into_iter().map(|c| c.valuation).collect();
    let mut debts: Vec<DebtValuation> = other_debts.into_iter().map(|d| d.valuation).collect();

    let projected_total_borrow_assets = ctx
        .accounts
        .reserve
        .total_borrow_assets
        .checked_add(assets)
        .ok_or(VannaError::MathOverflow)?;
    let projected_total_borrow_shares = ctx
        .accounts
        .reserve
        .total_borrow_shares
        .checked_add(new_debt_shares)
        .ok_or(VannaError::MathOverflow)?;
    let projected_position_shares = ctx
        .accounts
        .debt_position
        .borrow_shares
        .checked_add(new_debt_shares)
        .ok_or(VannaError::MathOverflow)?;
    let projected_debt_assets = debt_shares_to_assets_up(
        projected_position_shares,
        projected_total_borrow_shares,
        projected_total_borrow_assets,
    )?;

    let validated_price = load_validated_price(&ctx.accounts.asset_config, &ctx.accounts.price_update, &clock)?;
    let debt_value = normalize_token_value(
        projected_debt_assets,
        validated_price.price,
        validated_price.exponent,
        ctx.accounts.asset_config.decimals,
        true,
    )?;
    debts.push(DebtValuation { debt_value });

    if ctx.accounts.asset_config.collateral_enabled {
        let projected_collateral_amount = ctx
            .accounts
            .margin_vault
            .amount
            .checked_add(assets)
            .ok_or(VannaError::MathOverflow)?;
        let collateral_value = normalize_token_value(
            projected_collateral_amount,
            validated_price.price,
            validated_price.exponent,
            ctx.accounts.asset_config.decimals,
            false,
        )?;
        collaterals.push(CollateralValuation {
            collateral_value,
        });
    }

    let health = calculate_health(&collaterals, &debts)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);

    ctx.accounts.reserve.accounted_liquidity_assets = ctx
        .accounts
        .reserve
        .accounted_liquidity_assets
        .checked_sub(assets)
        .ok_or(VannaError::MathUnderflow)?;
    ctx.accounts.reserve.total_borrow_assets = projected_total_borrow_assets;
    ctx.accounts.reserve.total_borrow_shares = projected_total_borrow_shares;

    let was_zero_debt = ctx.accounts.debt_position.borrow_shares == 0;
    ctx.accounts.debt_position.credit_shares(new_debt_shares)?;
    if was_zero_debt {
        let active_count = ctx.accounts.margin_account.collateral_count + ctx.accounts.margin_account.debt_count;
        require!(
            (active_count as u16) < ctx.accounts.protocol_config.max_assets_per_margin as u16,
            VannaError::TooManyAssets
        );
        ctx.accounts
            .margin_account
            .add_active_debt(ctx.accounts.asset_config.asset_index)?;
    }

    let mint_key = ctx.accounts.mint.key();
    let reserve_bump = ctx.accounts.reserve.bump;
    let reserve_signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, mint_key.as_ref(), &[reserve_bump]]];
    let reserve_account_info = ctx.accounts.reserve.to_account_info();
    let received = transfer_out_checked_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.mint,
        &ctx.accounts.reserve_vault,
        &mut ctx.accounts.margin_vault,
        &reserve_account_info,
        reserve_signer_seeds,
        assets,
    )?;

    if was_zero_collateral
        && ctx.accounts.asset_config.collateral_enabled
        && !ctx
            .accounts
            .margin_account
            .is_collateral_active(ctx.accounts.asset_config.asset_index)
    {
        ctx.accounts
            .margin_account
            .add_active_collateral(ctx.accounts.asset_config.asset_index)?;
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(Borrowed {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.reserve.key(),
        assets: received,
        debt_shares: new_debt_shares,
        total_debt_shares: ctx.accounts.debt_position.borrow_shares,
        borrow_health_factor_wad: health.borrow_health_factor_wad,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_repay_from_margin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserRepayFromMargin<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = mint, token::authority = margin_account)]
    pub margin_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = reserve)]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn user_repay_from_margin(ctx: Context<UserRepayFromMargin>, max_assets: u64, repay_all: bool) -> Result<()> {
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
    )?;
    let now = Clock::get()?.unix_timestamp;
    apply_accrual(&mut ctx.accounts.reserve, now)?;

    let current_debt_assets = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    require!(current_debt_assets > 0, VannaError::ZeroAmount);

    let requested = if repay_all { current_debt_assets } else { max_assets.min(current_debt_assets) };
    let target_repay = requested.min(ctx.accounts.margin_vault.amount);
    require!(target_repay > 0, VannaError::ZeroAmount);
    let vault_balance_before = ctx.accounts.margin_vault.amount;

    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];
    let margin_account_info = ctx.accounts.margin_account.to_account_info();
    let received = transfer_out_checked_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.mint,
        &ctx.accounts.margin_vault,
        &mut ctx.accounts.reserve_vault,
        &margin_account_info,
        margin_signer_seeds,
        target_repay,
    )?;

    let shares_to_burn = if received >= current_debt_assets {
        ctx.accounts.debt_position.borrow_shares
    } else {
        mul_div_floor(
            received as u128,
            ctx.accounts.reserve.total_borrow_shares,
            ctx.accounts.reserve.total_borrow_assets as u128,
        )?
        .min(ctx.accounts.debt_position.borrow_shares)
    };

    ctx.accounts.reserve.accounted_liquidity_assets = ctx
        .accounts
        .reserve
        .accounted_liquidity_assets
        .checked_add(received)
        .ok_or(VannaError::MathOverflow)?;
    ctx.accounts.reserve.total_borrow_assets =
        ctx.accounts.reserve.total_borrow_assets.saturating_sub(received);
    ctx.accounts.reserve.total_borrow_shares =
        ctx.accounts.reserve.total_borrow_shares.saturating_sub(shares_to_burn);
    ctx.accounts.debt_position.debit_shares(shares_to_burn)?;

    let new_vault_balance = vault_balance_before.checked_sub(received).ok_or(VannaError::MathUnderflow)?;
    if new_vault_balance == 0
        && ctx
            .accounts
            .margin_account
            .is_collateral_active(ctx.accounts.asset_config.asset_index)
    {
        ctx.accounts
            .margin_account
            .remove_active_collateral(ctx.accounts.asset_config.asset_index)?;
    }
    if ctx.accounts.debt_position.borrow_shares == 0 {
        ctx.accounts
            .margin_account
            .remove_active_debt(ctx.accounts.asset_config.asset_index)?;
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(DebtRepaid {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.reserve.key(),
        payer: ctx.accounts.authority.key(),
        assets: received,
        debt_shares_burned: shares_to_burn,
        remaining_debt_shares: ctx.accounts.debt_position.borrow_shares,
        event_sequence,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// public_repay_from_wallet
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct PublicRepayFromWallet<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = mint, token::authority = payer)]
    pub payer_token_account: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = reserve)]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn public_repay_from_wallet(ctx: Context<PublicRepayFromWallet>, max_assets: u64, repay_all: bool) -> Result<()> {
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    let now = Clock::get()?.unix_timestamp;
    apply_accrual(&mut ctx.accounts.reserve, now)?;

    let current_debt_assets = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    require!(current_debt_assets > 0, VannaError::ZeroAmount);

    let target_repay = if repay_all { current_debt_assets } else { max_assets.min(current_debt_assets) };
    require!(target_repay > 0, VannaError::ZeroAmount);

    let received = transfer_in_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.mint,
        &ctx.accounts.payer_token_account,
        &mut ctx.accounts.reserve_vault,
        &ctx.accounts.payer,
        target_repay,
    )?;

    let shares_to_burn = if received >= current_debt_assets {
        ctx.accounts.debt_position.borrow_shares
    } else {
        mul_div_floor(
            received as u128,
            ctx.accounts.reserve.total_borrow_shares,
            ctx.accounts.reserve.total_borrow_assets as u128,
        )?
        .min(ctx.accounts.debt_position.borrow_shares)
    };

    ctx.accounts.reserve.accounted_liquidity_assets = ctx
        .accounts
        .reserve
        .accounted_liquidity_assets
        .checked_add(received)
        .ok_or(VannaError::MathOverflow)?;
    ctx.accounts.reserve.total_borrow_assets =
        ctx.accounts.reserve.total_borrow_assets.saturating_sub(received);
    ctx.accounts.reserve.total_borrow_shares =
        ctx.accounts.reserve.total_borrow_shares.saturating_sub(shares_to_burn);
    ctx.accounts.debt_position.debit_shares(shares_to_burn)?;

    if ctx.accounts.debt_position.borrow_shares == 0 {
        ctx.accounts
            .margin_account
            .remove_active_debt(ctx.accounts.asset_config.asset_index)?;
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(DebtRepaid {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.reserve.key(),
        payer: ctx.accounts.payer.key(),
        assets: received,
        debt_shares_burned: shares_to_burn,
        remaining_debt_shares: ctx.accounts.debt_position.borrow_shares,
        event_sequence,
        timestamp: now,
    });
    Ok(())
}
