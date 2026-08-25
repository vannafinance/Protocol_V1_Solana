use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::{calculate_health, normalize_token_value, CollateralValuation};
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::validation::accounts::{assert_protocol_action_allowed, validate_asset_config, ProtocolAction};
use crate::validation::positions::scan_and_validate_positions;
use crate::validation::token::{transfer_in_measured, transfer_out_checked, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{close_account, CloseAccount, Mint, Token, TokenAccount};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

// ---------------------------------------------------------------------------
// user_create_margin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCreateMargin<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + MarginAccount::INIT_SPACE,
        seeds = [MARGIN_SEED, authority.key().as_ref()],
        bump
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    pub system_program: Program<'info, System>,
}

pub fn user_create_margin(ctx: Context<UserCreateMargin>) -> Result<()> {
    let bump = ctx.bumps.margin_account;
    ctx.accounts
        .margin_account
        .set_inner(MarginAccount::new_empty(ctx.accounts.authority.key(), bump));

    emit!(MarginCreated {
        margin_account: ctx.accounts.margin_account.key(),
        authority: ctx.accounts.authority.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_close_margin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCloseMargin<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized,
        close = authority
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
}

pub fn user_close_margin(ctx: Context<UserCloseMargin>) -> Result<()> {
    require!(ctx.accounts.margin_account.is_empty(), VannaError::NonEmptyMargin);
    emit!(MarginClosed {
        margin_account: ctx.accounts.margin_account.key(),
        authority: ctx.accounts.authority.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_deposit_collateral
// ---------------------------------------------------------------------------
//
// There is no separate "open collateral position" instruction: the margin vault is a plain
// Associated Token Account, and its first deposit creates it (`init_if_needed`). Collateral is
// not tracked in a parallel ledger — the vault's own live SPL balance *is* the credited amount.
// This is safe specifically because this vault is private to one (margin, mint) pair; it is never
// shared across users the way the lending `Reserve`'s pooled liquidity vault is; that one still
// requires — and keeps — its own internal `accounted_liquidity_assets` ledger.

#[derive(Accounts)]
pub struct UserDepositCollateral<'info> {
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
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = mint, token::authority = authority)]
    pub source_token_account: Box<Account<'info, TokenAccount>>,
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

pub fn user_deposit_collateral(ctx: Context<UserDepositCollateral>, amount: u64) -> Result<()> {
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::CollateralDeposit)?;
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    require!(ctx.accounts.asset_config.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    require!(amount > 0, VannaError::ZeroAmount);
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
    )?;

    let was_zero = ctx.accounts.margin_vault.amount == 0;

    let received = transfer_in_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.mint,
        &ctx.accounts.source_token_account,
        &mut ctx.accounts.margin_vault,
        &ctx.accounts.authority,
        amount,
    )?;

    if was_zero {
        let active_count = ctx.accounts.margin_account.collateral_count + ctx.accounts.margin_account.debt_count;
        require!(
            (active_count as u16) < ctx.accounts.protocol_config.max_assets_per_margin as u16,
            VannaError::TooManyAssets
        );
        ctx.accounts
            .margin_account
            .add_active_collateral(ctx.accounts.asset_config.asset_index)?;
    }

    let cap = ctx.accounts.asset_config.max_collateral_per_margin;
    require!(
        cap == UNCAPPED || ctx.accounts.margin_vault.amount <= cap,
        VannaError::CollateralCapExceeded
    );

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(CollateralDeposited {
        margin_account: ctx.accounts.margin_account.key(),
        mint: ctx.accounts.mint.key(),
        amount: received,
        new_collateral_amount: ctx.accounts.margin_vault.amount,
        event_sequence,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_close_collateral_position
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCloseCollateralPosition<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = mint, token::authority = margin_account)]
    pub margin_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn user_close_collateral_position(ctx: Context<UserCloseCollateralPosition>) -> Result<()> {
    require!(ctx.accounts.margin_vault.amount == 0, VannaError::NonEmptyVault);
    require!(
        !ctx.accounts.margin_account.is_collateral_active(ctx.accounts.asset_config.asset_index),
        VannaError::NonEmptyCollateralPosition
    );
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
    )?;

    let authority_key = ctx.accounts.margin_account.authority;
    let bump = ctx.accounts.margin_account.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[bump]]];

    let cpi_accounts = CloseAccount {
        account: ctx.accounts.margin_vault.to_account_info(),
        destination: ctx.accounts.authority.to_account_info(),
        authority: ctx.accounts.margin_account.to_account_info(),
    };
    close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        cpi_accounts,
        signer_seeds,
    ))?;

    emit!(CollateralPositionClosed {
        margin_account: ctx.accounts.margin_account.key(),
        mint: ctx.accounts.mint.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_withdraw_collateral
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserWithdrawCollateral<'info> {
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
    pub mint: Box<Account<'info, Mint>>,
    /// Pyth price update for the withdrawn asset itself (the other active positions' price
    /// updates are supplied via `remaining_accounts`, see `scan_and_validate_positions`).
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    #[account(mut, token::mint = mint, token::authority = authority)]
    pub destination_token_account: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = mint, token::authority = margin_account)]
    pub margin_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn user_withdraw_collateral(
    ctx: Context<UserWithdrawCollateral>,
    amount: u64,
    min_health_factor_wad: u128,
) -> Result<()> {
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::CollateralWithdraw)?;
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
    )?;
    require!(amount > 0, VannaError::ZeroAmount);
    require!(ctx.accounts.margin_vault.amount >= amount, VannaError::InsufficientCollateral);

    let clock = Clock::get()?;
    let margin_key = ctx.accounts.margin_account.key();
    let (scanned_collaterals, debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        Some(ctx.accounts.asset_config.asset_index),
        None,
    )?;
    let mut collaterals: Vec<CollateralValuation> =
        scanned_collaterals.into_iter().map(|c| c.valuation).collect();

    let projected_amount = ctx
        .accounts
        .margin_vault
        .amount
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;
    let validated_price = load_validated_price(&ctx.accounts.asset_config, &ctx.accounts.price_update, &clock)?;
    let projected_value = normalize_token_value(
        projected_amount,
        validated_price.price,
        validated_price.exponent,
        ctx.accounts.asset_config.decimals,
        false,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: projected_value,
        ltv_bps: ctx.accounts.asset_config.ltv_bps,
        liquidation_threshold_bps: ctx.accounts.asset_config.liquidation_threshold_bps,
    });

    let debt_valuations: Vec<_> = debts.into_iter().map(|d| d.valuation).collect();
    let health = calculate_health(&collaterals, &debt_valuations)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);
    if min_health_factor_wad > 0 {
        require!(health.borrow_health_factor_wad >= min_health_factor_wad, VannaError::HealthFactorTooLow);
    }

    let authority_key = ctx.accounts.margin_account.authority;
    let bump = ctx.accounts.margin_account.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[bump]]];
    let margin_account_info = ctx.accounts.margin_account.to_account_info();
    transfer_out_checked(
        &ctx.accounts.token_program,
        &ctx.accounts.mint,
        &ctx.accounts.margin_vault,
        &ctx.accounts.destination_token_account,
        &margin_account_info,
        signer_seeds,
        amount,
    )?;

    if projected_amount == 0 {
        ctx.accounts
            .margin_account
            .remove_active_collateral(ctx.accounts.asset_config.asset_index)?;
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(CollateralWithdrawn {
        margin_account: ctx.accounts.margin_account.key(),
        mint: ctx.accounts.mint.key(),
        amount,
        new_collateral_amount: projected_amount,
        borrow_health_factor_wad: health.borrow_health_factor_wad,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}
