use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::calculate_health;
use crate::interface::AgentAccounts;
use crate::risk_engine::{collect_holdings, agents_of, split_positions, value_holdings, Holding};
use crate::state::asset_config::AssetConfig;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::validation::accounts::{assert_protocol_action_allowed, validate_asset_config, ProtocolAction};
use crate::validation::token::{transfer_in_measured, transfer_out_checked, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use crate::interface::PriceChecks;

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
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = mint,
        token::authority = authority,
        token::token_program = token_program
    )]
    pub source_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = authority,
        associated_token::mint = mint,
        associated_token::authority = margin_account,
        associated_token::token_program = token_program,
    )]
    pub margin_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn user_deposit_collateral(ctx: Context<UserDepositCollateral>, amount: u64) -> Result<()> {
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::CollateralDeposit)?;
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    require!(!ctx.accounts.asset_config.is_venue(), VannaError::VenueAssetNotTransferable);
    require!(ctx.accounts.asset_config.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    require!(amount > 0, VannaError::ZeroAmount);
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
        &ctx.accounts.token_program.key(),
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
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = mint,
        token::authority = authority,
        token::token_program = token_program
    )]
    pub destination_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = mint,
        token::authority = margin_account,
        token::token_program = token_program
    )]
    pub margin_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn user_withdraw_collateral<'info>(
    ctx: Context<'info, UserWithdrawCollateral<'info>>,
    amount: u64,
    min_health_factor_wad: u128,
) -> Result<()> {
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::CollateralWithdraw)?;
    validate_asset_config(&ctx.accounts.asset_config, &ctx.accounts.mint.key(), &ctx.accounts.token_program.key())?;
    require!(!ctx.accounts.asset_config.is_venue(), VannaError::VenueAssetNotTransferable);
    verify_associated_token_account(
        &ctx.accounts.margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require!(amount > 0, VannaError::ZeroAmount);
    require!(ctx.accounts.margin_vault.amount >= amount, VannaError::InsufficientCollateral);

    let clock = Clock::get()?;
    let projected_amount = ctx
        .accounts
        .margin_vault
        .amount
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;

    let health_factor = if ctx.accounts.margin_account.debt_count == 0 {
        u128::MAX
    } else {
        let margin_key = ctx.accounts.margin_account.key();
        let asset_index = ctx.accounts.asset_config.asset_index;
        let (positions, agent_accounts) =
            split_positions(ctx.remaining_accounts, &ctx.accounts.margin_account, &[asset_index], None)?;
        let mut holdings = collect_holdings(
            &margin_key,
            &ctx.accounts.margin_account,
            positions,
            ctx.program_id,
            &clock,
            &[asset_index],
            None,
        )?;
        holdings.push(Holding::token(&ctx.accounts.asset_config, projected_amount, false));
        let agents = AgentAccounts::parse(agent_accounts, &agents_of(&holdings, &[]))?;
        let valuation = value_holdings(&holdings, &agents)?;
        valuation.require(PriceChecks::ALL)?;
        let health = calculate_health(&valuation.collaterals, &valuation.debts)?;
        require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);
        health.borrow_health_factor_wad
    };
    if min_health_factor_wad > 0 {
        require!(health_factor >= min_health_factor_wad, VannaError::HealthFactorTooLow);
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
        borrow_health_factor_wad: health_factor,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}
