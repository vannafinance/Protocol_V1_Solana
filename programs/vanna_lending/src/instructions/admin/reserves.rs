//! Lending-pool configuration: pool creation, rate curve and caps, protocol-fee collection.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::state::asset_config::AssetConfig;
use crate::state::protocol_config::ProtocolConfig;
use crate::state::reserve::{RateCurve, Reserve, ReserveStatus};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint as TokenMint, Token};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

// ---------------------------------------------------------------------------
// admin_initialize_reserve
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminInitializeReserve<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [ASSET_SEED, underlying_mint.key().as_ref()],
        bump = asset_config.bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + Reserve::INIT_SPACE,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = underlying_mint,
        associated_token::authority = reserve,
        associated_token::token_program = token_program,
    )]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        mint::decimals = underlying_mint.decimals,
        mint::authority = reserve,
        mint::token_program = share_token_program,
        seeds = [SHARE_MINT_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub share_mint: Box<Account<'info, TokenMint>>,
    pub token_program: Interface<'info, TokenInterface>,
    /// Share mints are always classic SPL.
    pub share_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn admin_initialize_reserve(
    ctx: Context<AdminInitializeReserve>,
    rate_curve: RateCurve,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Result<()> {
    require_keys_eq!(ctx.accounts.asset_config.reserve, Pubkey::default(), VannaError::ReserveAlreadyExists);
    require!(ctx.accounts.asset_config.is_pyth_priced(), VannaError::UnsupportedPriceSource);
    Reserve::validate_rate_config(&rate_curve, reserve_factor_bps)?;
    ReserveStatus::from_u8(status).ok_or(VannaError::InvalidReserveStatus)?;

    let now = Clock::get()?.unix_timestamp;
    let reserve = &mut ctx.accounts.reserve;
    reserve.asset_config = ctx.accounts.asset_config.key();
    reserve.underlying_mint = ctx.accounts.underlying_mint.key();
    reserve.liquidity_vault = ctx.accounts.liquidity_vault.key();
    reserve.share_mint = ctx.accounts.share_mint.key();
    reserve.supply_cap = supply_cap;
    reserve.borrow_cap = borrow_cap;
    reserve.accounted_liquidity_assets = 0;
    reserve.total_borrow_assets = 0;
    reserve.total_borrow_shares = 0;
    reserve.borrow_index_wad = WAD;
    reserve.accrued_protocol_fees = 0;
    reserve.last_update_timestamp = now;
    reserve.rate_curve = rate_curve;
    reserve.reserve_factor_bps = reserve_factor_bps;
    reserve.status = status;
    reserve.bump = ctx.bumps.reserve;
    reserve.reserved = [0u8; 128];

    ctx.accounts.asset_config.reserve = reserve.key();

    emit!(ReserveInitialized {
        reserve: reserve.key(),
        underlying_mint: reserve.underlying_mint,
        liquidity_vault: reserve.liquidity_vault,
        share_mint: reserve.share_mint,
        supply_cap,
        borrow_cap,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_update_reserve_config
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminUpdateReserveConfig<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump = reserve.bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
}

pub fn admin_update_reserve_config(
    ctx: Context<AdminUpdateReserveConfig>,
    rate_curve: RateCurve,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Result<()> {
    Reserve::validate_rate_config(&rate_curve, reserve_factor_bps)?;
    ReserveStatus::from_u8(status).ok_or(VannaError::InvalidReserveStatus)?;

    let reserve = &mut ctx.accounts.reserve;
    let now = Clock::get()?.unix_timestamp;
    reserve.accrue_interest(now)?;

    reserve.rate_curve = rate_curve;
    reserve.reserve_factor_bps = reserve_factor_bps;
    reserve.supply_cap = supply_cap;
    reserve.borrow_cap = borrow_cap;
    reserve.status = status;

    emit!(ReserveConfigUpdated {
        reserve: reserve.key(),
        rate_curve,
        reserve_factor_bps,
        supply_cap,
        borrow_cap,
        status,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_collect_protocol_fees
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminCollectProtocolFees<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump = reserve.bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(mut, token::mint = underlying_mint, token::authority = reserve, token::token_program = token_program)]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = underlying_mint, token::authority = protocol_config.treasury, token::token_program = token_program)]
    pub treasury_ata: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn admin_collect_protocol_fees(ctx: Context<AdminCollectProtocolFees>, amount: u64) -> Result<()> {
    require!(amount > 0, VannaError::ZeroAmount);

    let reserve_account_info = ctx.accounts.reserve.to_account_info();
    let now = Clock::get()?.unix_timestamp;
    let reserve = &mut ctx.accounts.reserve;
    reserve.accrue_interest(now)?;

    let collectible = reserve.accrued_protocol_fees.min(reserve.accounted_liquidity_assets);
    require!(amount <= collectible, VannaError::InsufficientLiquidity);

    reserve.accounted_liquidity_assets = reserve
        .accounted_liquidity_assets
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;
    reserve.accrued_protocol_fees = reserve
        .accrued_protocol_fees
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;

    let mint_key = ctx.accounts.underlying_mint.key();
    let bump = reserve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, mint_key.as_ref(), &[bump]]];
    crate::validation::token::transfer_out_checked(
        &ctx.accounts.token_program,
        &ctx.accounts.underlying_mint,
        &ctx.accounts.liquidity_vault,
        &ctx.accounts.treasury_ata,
        &reserve_account_info,
        signer_seeds,
        amount,
    )?;

    emit!(ProtocolFeesCollected {
        reserve: ctx.accounts.reserve.key(),
        treasury_ata: ctx.accounts.treasury_ata.key(),
        amount,
        timestamp: now,
    });
    Ok(())
}
