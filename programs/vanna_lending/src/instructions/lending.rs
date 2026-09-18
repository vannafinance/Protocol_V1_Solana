use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::interest::accrue;
use crate::math::shares::{
    assets_to_supply_shares_down, available_lender_cash, lender_total_assets, supply_shares_to_assets_down,
};
use crate::state::asset_config::AssetConfig;
use crate::state::protocol_config::ProtocolConfig;
use crate::state::reserve::Reserve;
use crate::validation::accounts::{
    assert_protocol_action_allowed, assert_reserve_action_allowed, validate_asset_config, ProtocolAction,
};
use crate::validation::token::{transfer_in_measured, transfer_out_checked};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{burn, mint_to, Burn, Mint as TokenMint, MintTo, Token, TokenAccount as TokenTokenAccount};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

fn apply_accrual(reserve: &mut Account<Reserve>, now: i64) -> Result<()> {
    let accrual = accrue(reserve, now)?;
    reserve.total_borrow_assets = accrual.new_total_borrow_assets;
    reserve.accrued_protocol_fees = accrual.new_accrued_protocol_fees;
    reserve.borrow_index_wad = accrual.new_borrow_index_wad;
    reserve.last_update_timestamp = now;
    Ok(())
}

// ---------------------------------------------------------------------------
// lender_supply
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LenderSupply<'info> {
    #[account(mut)]
    pub lender: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, token::mint = underlying_mint, token::authority = lender, token::token_program = token_program)]
    pub lender_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = underlying_mint, token::authority = reserve, token::token_program = token_program)]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [SHARE_MINT_SEED, underlying_mint.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, TokenMint>>,
    #[account(
        init_if_needed,
        payer = lender,
        associated_token::mint = share_mint,
        associated_token::authority = lender,
        associated_token::token_program = share_token_program,
    )]
    pub lender_share_account: Box<Account<'info, TokenTokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub share_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn lender_supply(ctx: Context<LenderSupply>, assets: u64, min_shares_out: u64) -> Result<()> {
    require!(assets > 0, VannaError::ZeroAmount);
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::Supply)?;
    assert_reserve_action_allowed(ctx.accounts.reserve.status, ProtocolAction::Supply)?;
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;

    let now = Clock::get()?.unix_timestamp;
    apply_accrual(&mut ctx.accounts.reserve, now)?;

    let lender_total_before = lender_total_assets(
        ctx.accounts.reserve.accounted_liquidity_assets,
        ctx.accounts.reserve.total_borrow_assets,
        ctx.accounts.reserve.accrued_protocol_fees,
    )?;
    let total_share_supply_before = ctx.accounts.share_mint.supply as u128;

    let received = transfer_in_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.underlying_mint,
        &ctx.accounts.lender_token_account,
        &mut ctx.accounts.liquidity_vault,
        &ctx.accounts.lender,
        assets,
    )?;

    let shares_out = assets_to_supply_shares_down(received, total_share_supply_before, lender_total_before)?;
    require!(shares_out > 0, VannaError::SlippageExceeded);
    require!(shares_out >= min_shares_out, VannaError::SlippageExceeded);
    if total_share_supply_before == 0 {
        require!(shares_out >= MIN_INITIAL_SHARES, VannaError::BelowMinimumInitialSupply);
    }

    let projected_total_assets = lender_total_before
        .checked_add(received)
        .ok_or(VannaError::MathOverflow)?;
    require!(
        ctx.accounts.reserve.supply_cap == UNCAPPED || projected_total_assets <= ctx.accounts.reserve.supply_cap,
        VannaError::SupplyCapExceeded
    );

    ctx.accounts.reserve.accounted_liquidity_assets = ctx
        .accounts
        .reserve
        .accounted_liquidity_assets
        .checked_add(received)
        .ok_or(VannaError::MathOverflow)?;

    let mint_key = ctx.accounts.underlying_mint.key();
    let bump = ctx.accounts.reserve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, mint_key.as_ref(), &[bump]]];
    let cpi_accounts = MintTo {
        mint: ctx.accounts.share_mint.to_account_info(),
        to: ctx.accounts.lender_share_account.to_account_info(),
        authority: ctx.accounts.reserve.to_account_info(),
    };
    mint_to(
        CpiContext::new_with_signer(ctx.accounts.share_token_program.key(), cpi_accounts, signer_seeds),
        shares_out,
    )?;

    emit!(LiquiditySupplied {
        reserve: ctx.accounts.reserve.key(),
        lender: ctx.accounts.lender.key(),
        assets: received,
        shares: shares_out,
        total_borrow_assets: ctx.accounts.reserve.total_borrow_assets,
        accounted_liquidity_assets: ctx.accounts.reserve.accounted_liquidity_assets,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lender_redeem
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LenderRedeem<'info> {
    pub lender: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, token::mint = underlying_mint, token::authority = lender, token::token_program = token_program)]
    pub lender_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = underlying_mint, token::authority = reserve, token::token_program = token_program)]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [SHARE_MINT_SEED, underlying_mint.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, TokenMint>>,
    #[account(mut, token::mint = share_mint, token::authority = lender, token::token_program = share_token_program)]
    pub lender_share_account: Box<Account<'info, TokenTokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub share_token_program: Program<'info, Token>,
}

pub fn lender_redeem(ctx: Context<LenderRedeem>, shares: u64, min_assets_out: u64) -> Result<()> {
    require!(shares > 0, VannaError::ZeroAmount);
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::Redeem)?;
    assert_reserve_action_allowed(ctx.accounts.reserve.status, ProtocolAction::Redeem)?;
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require!(
        ctx.accounts.lender_share_account.amount >= shares,
        VannaError::InsufficientShares
    );

    let now = Clock::get()?.unix_timestamp;
    apply_accrual(&mut ctx.accounts.reserve, now)?;

    let lender_total_now = lender_total_assets(
        ctx.accounts.reserve.accounted_liquidity_assets,
        ctx.accounts.reserve.total_borrow_assets,
        ctx.accounts.reserve.accrued_protocol_fees,
    )?;
    let total_share_supply = ctx.accounts.share_mint.supply as u128;

    let assets_out = supply_shares_to_assets_down(shares, total_share_supply, lender_total_now)?;
    require!(assets_out > 0, VannaError::SlippageExceeded);
    require!(assets_out >= min_assets_out, VannaError::SlippageExceeded);

    let available_cash =
        available_lender_cash(ctx.accounts.reserve.accounted_liquidity_assets, ctx.accounts.reserve.accrued_protocol_fees);
    require!(assets_out <= available_cash, VannaError::InsufficientLiquidity);

    let cpi_accounts = Burn {
        mint: ctx.accounts.share_mint.to_account_info(),
        from: ctx.accounts.lender_share_account.to_account_info(),
        authority: ctx.accounts.lender.to_account_info(),
    };
    burn(CpiContext::new(ctx.accounts.share_token_program.key(), cpi_accounts), shares)?;

    ctx.accounts.reserve.accounted_liquidity_assets = ctx
        .accounts
        .reserve
        .accounted_liquidity_assets
        .checked_sub(assets_out)
        .ok_or(VannaError::MathUnderflow)?;

    let mint_key = ctx.accounts.underlying_mint.key();
    let bump = ctx.accounts.reserve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, mint_key.as_ref(), &[bump]]];
    let reserve_account_info = ctx.accounts.reserve.to_account_info();
    transfer_out_checked(
        &ctx.accounts.token_program,
        &ctx.accounts.underlying_mint,
        &ctx.accounts.liquidity_vault,
        &ctx.accounts.lender_token_account,
        &reserve_account_info,
        signer_seeds,
        assets_out,
    )?;

    emit!(LiquidityRedeemed {
        reserve: ctx.accounts.reserve.key(),
        lender: ctx.accounts.lender.key(),
        assets: assets_out,
        shares,
        total_borrow_assets: ctx.accounts.reserve.total_borrow_assets,
        accounted_liquidity_assets: ctx.accounts.reserve.accounted_liquidity_assets,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// public_refresh_reserve
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct PublicRefreshReserve<'info> {
    #[account(mut, seeds = [RESERVE_SEED, reserve.underlying_mint.as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
}

pub fn public_refresh_reserve(ctx: Context<PublicRefreshReserve>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let previous_timestamp = ctx.accounts.reserve.last_update_timestamp;
    apply_accrual(&mut ctx.accounts.reserve, now)?;

    if ctx.accounts.reserve.last_update_timestamp != previous_timestamp {
        emit!(ReserveAccrued {
            reserve: ctx.accounts.reserve.key(),
            total_borrow_assets: ctx.accounts.reserve.total_borrow_assets,
            accounted_liquidity_assets: ctx.accounts.reserve.accounted_liquidity_assets,
            borrow_index_wad: ctx.accounts.reserve.borrow_index_wad,
            accrued_protocol_fees: ctx.accounts.reserve.accrued_protocol_fees,
            timestamp: now,
        });
    }
    Ok(())
}
