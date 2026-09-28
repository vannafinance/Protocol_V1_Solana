//! `margin_execute`: a margin account calls a whitelisted external program.
//!
//! 1. The program's `Integration` must exist and be enabled.
//! 2. Its adapter must allow the instruction and names the margin vault the call spends from, the
//!    margin vault it credits, and the most it may spend (`CallPlan`).
//! 3. The CPI is signed by the margin PDA only, those two vaults are the only margin-owned token
//!    accounts passed to it, and a receipt leg must use its registered pricing reserve.
//! 4. Afterwards: both vaults are still plain margin-owned accounts, no more than `max_spent` left
//!    the spent vault, the received amount meets `min_received`, the account's health factor is
//!    above 1.10, and the active-asset lists track the new balances.

use crate::adapters::{self, CallPlan};
use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::{calculate_health, CollateralValuation};
use crate::oracle::valuation::{collateral_value, find_source_account};
use crate::risk_engine::scan_and_validate_positions;
use crate::state::asset_config::AssetConfig;
use crate::state::integration::Integration;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::validation::accounts::{assert_protocol_action_allowed, validate_asset_config, ProtocolAction};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

#[derive(Accounts)]
pub struct MarginExecute<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, authority.key().as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [INTEGRATION_SEED, target_program.key().as_ref()], bump = integration.bump)]
    pub integration: Box<Account<'info, Integration>>,
    /// CHECK: bound to the registered integration by the seeds above.
    #[account(executable)]
    pub target_program: UncheckedAccount<'info>,

    #[account(seeds = [ASSET_SEED, spent_mint.key().as_ref()], bump = spent_asset.bump)]
    pub spent_asset: Box<Account<'info, AssetConfig>>,
    pub spent_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = spent_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = spent_token_program
    )]
    pub spent_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub spent_price_update: Box<Account<'info, PriceUpdateV2>>,

    #[account(seeds = [ASSET_SEED, received_mint.key().as_ref()], bump = received_asset.bump)]
    pub received_asset: Box<Account<'info, AssetConfig>>,
    pub received_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = authority,
        associated_token::mint = received_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = received_token_program
    )]
    pub received_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub received_price_update: Box<Account<'info, PriceUpdateV2>>,

    pub spent_token_program: Interface<'info, TokenInterface>,
    pub received_token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// `remaining_accounts` = the external instruction's accounts (`cpi_account_count` of them, in
/// that program's order), followed by the position-scan accounts for the health check.
pub fn margin_execute<'info>(
    mut ctx: Context<'info, MarginExecute<'info>>,
    data: Vec<u8>,
    cpi_account_count: u16,
    min_received: u64,
) -> Result<()> {
    let accounts = &ctx.accounts;
    assert_protocol_action_allowed(accounts.protocol_config.operating_mode, ProtocolAction::ExternalCall)?;
    require!(accounts.integration.enabled, VannaError::IntegrationDisabled);
    validate_asset_config(&accounts.spent_asset, &accounts.spent_mint.key(), &accounts.spent_token_program.key())?;
    validate_asset_config(
        &accounts.received_asset,
        &accounts.received_mint.key(),
        &accounts.received_token_program.key(),
    )?;
    require_keys_neq!(accounts.spent_mint.key(), accounts.received_mint.key(), VannaError::InvalidCallAccounts);
    require!(accounts.received_asset.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    require!(
        accounts.margin_account.is_collateral_active(accounts.spent_asset.asset_index),
        VannaError::IncompletePositionAccounts
    );

    let count = usize::from(cpi_account_count);
    require!(count <= ctx.remaining_accounts.len(), VannaError::InvalidCallAccounts);
    let (cpi_accounts, health_accounts) = ctx.remaining_accounts.split_at(count);
    let cpi_keys: Vec<Pubkey> = cpi_accounts.iter().map(|a| a.key()).collect();
    let plan = adapters::plan_call(accounts.integration.adapter, &accounts.integration.program_id, &data, &cpi_keys)?;

    let margin_key = accounts.margin_account.key();
    let vaults = [accounts.spent_vault.key(), accounts.received_vault.key()];
    check_roles(&plan, cpi_accounts, &margin_key, accounts)?;
    guard_accounts(cpi_accounts, &margin_key, &accounts.authority.key(), &vaults)?;

    let spent_before = accounts.spent_vault.amount;
    let received_before = accounts.received_vault.amount;
    let bump = [accounts.margin_account.bump];
    let authority_key = accounts.authority.key();
    let margin_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &bump]];
    invoke_as_margin(&accounts.target_program, cpi_accounts, data, &margin_key, margin_seeds)?;

    let accounts = &mut ctx.accounts;
    accounts.spent_vault.reload()?;
    accounts.received_vault.reload()?;
    for vault in [&accounts.spent_vault, &accounts.received_vault] {
        require_keys_eq!(vault.owner, margin_key, VannaError::InvalidCallResult);
        require!(vault.delegate.is_none() && vault.close_authority.is_none(), VannaError::InvalidCallResult);
    }
    let amount_spent = spent_before
        .checked_sub(accounts.spent_vault.amount)
        .ok_or(VannaError::InvalidCallResult)?;
    require!(amount_spent <= plan.max_spent, VannaError::InvalidCallResult);
    let amount_received = accounts
        .received_vault
        .amount
        .checked_sub(received_before)
        .ok_or(VannaError::InvalidCallResult)?;
    require!(amount_received >= min_received, VannaError::SlippageExceeded);
    let cap = accounts.received_asset.max_collateral_per_margin;
    require!(
        cap == UNCAPPED || accounts.received_vault.amount <= cap,
        VannaError::CollateralCapExceeded
    );

    let health_factor = check_health(accounts, cpi_accounts, health_accounts, ctx.program_id)?;
    update_active_assets(accounts)?;

    let event_sequence = accounts.margin_account.next_event_sequence()?;
    emit!(MarginExecuted {
        margin_account: margin_key,
        program_id: accounts.integration.program_id,
        adapter: accounts.integration.adapter,
        spent_mint: accounts.spent_mint.key(),
        received_mint: accounts.received_mint.key(),
        amount_spent,
        amount_received,
        borrow_health_factor_wad: health_factor,
        event_sequence,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

/// The accounts at the plan's role indexes must be the margin and the named vaults and mints.
fn check_roles(plan: &CallPlan, cpi: &[AccountInfo], margin: &Pubkey, accounts: &MarginExecute) -> Result<()> {
    let key_at = |i: usize| -> Result<Pubkey> {
        cpi.get(i).map(|a| a.key()).ok_or_else(|| VannaError::InvalidCallAccounts.into())
    };
    require_keys_eq!(key_at(plan.authority)?, *margin, VannaError::InvalidCallAccounts);
    require_keys_eq!(key_at(plan.spent.vault)?, accounts.spent_vault.key(), VannaError::InvalidCallAccounts);
    require_keys_eq!(key_at(plan.received.vault)?, accounts.received_vault.key(), VannaError::InvalidCallAccounts);
    for (mint_slot, mint) in [(plan.spent.mint, accounts.spent_mint.key()), (plan.received.mint, accounts.received_mint.key())] {
        if let Some(index) = mint_slot {
            require_keys_eq!(key_at(index)?, mint, VannaError::InvalidCallAccounts);
        }
    }
    // A receipt leg must go through the reserve that prices it.
    for asset in [&accounts.spent_asset, &accounts.received_asset] {
        if !asset.is_pyth_priced() {
            let index = plan.price_source.ok_or(VannaError::InvalidCallAccounts)?;
            require_keys_eq!(key_at(index)?, asset.price_source_account, VannaError::InvalidCallAccounts);
        }
    }
    Ok(())
}

/// The margin PDA signs the CPI, so it must not reach margin funds beyond this call's two vaults:
/// no Vanna program or Vanna-owned account other than the margin itself, no wallet signer, and
/// no other token account the margin owns. (Delegate / close-authority rights over other token
/// accounts are never granted by the margin, so ownership is the relevant check.)
fn guard_accounts(cpi: &[AccountInfo], margin: &Pubkey, authority: &Pubkey, vaults: &[Pubkey; 2]) -> Result<()> {
    for account in cpi {
        let key = account.key();
        require!(key != crate::ID && key != *authority, VannaError::InvalidCallAccounts);
        if *account.owner == crate::ID {
            require_keys_eq!(key, *margin, VannaError::InvalidCallAccounts);
        }
        if !vaults.contains(&key) {
            require!(!is_token_account_of(account, margin), VannaError::InvalidCallAccounts);
        }
    }
    Ok(())
}

/// Whether `account` is an SPL / Token-2022 token account whose owner is `owner`.
fn is_token_account_of(account: &AccountInfo, owner: &Pubkey) -> bool {
    if *account.owner != anchor_spl::token::ID && *account.owner != anchor_spl::token_2022::ID {
        return false;
    }
    let Ok(data) = account.try_borrow_data() else {
        return false;
    };
    // 165 bytes = token account base layout; Token-2022 extended accounts mark byte 165 = 2.
    let is_token_account = data.len() == 165 || (data.len() > 165 && data[165] == 2);
    is_token_account && data[32..64] == owner.to_bytes()
}

// `#[inline(never)]`: keeps the CPI's account vectors out of the handler's stack frame.
#[inline(never)]
fn invoke_as_margin<'info>(
    program: &AccountInfo<'info>,
    cpi: &[AccountInfo<'info>],
    data: Vec<u8>,
    margin: &Pubkey,
    margin_seeds: &[&[&[u8]]],
) -> Result<()> {
    let metas = cpi
        .iter()
        .map(|a| AccountMeta { pubkey: a.key(), is_signer: a.key() == *margin, is_writable: a.is_writable })
        .collect();
    let ix = Instruction { program_id: program.key(), accounts: metas, data };
    let mut infos = cpi.to_vec();
    infos.push(program.clone());
    invoke_signed(&ix, &infos, margin_seeds)?;
    Ok(())
}

/// Health on post-call balances: every other position from the scan, plus both vaults valued
/// explicitly. Returns the borrow health factor.
#[inline(never)]
fn check_health<'info>(
    accounts: &MarginExecute<'info>,
    cpi_accounts: &'info [AccountInfo<'info>],
    health_accounts: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
) -> Result<u128> {
    let clock = Clock::get()?;
    let named = [accounts.spent_asset.asset_index, accounts.received_asset.asset_index];
    let (mut collaterals, debts) = scan_and_validate_positions(
        &accounts.margin_account.key(),
        &accounts.margin_account,
        health_accounts,
        program_id,
        &clock,
        &named,
        None,
    )?;
    let candidates = [cpi_accounts, health_accounts];
    for (asset, amount, price) in [
        (&accounts.spent_asset, accounts.spent_vault.amount, &accounts.spent_price_update),
        (&accounts.received_asset, accounts.received_vault.amount, &accounts.received_price_update),
    ] {
        let source = find_source_account(asset, &candidates)?;
        collaterals.push(CollateralValuation {
            collateral_value: collateral_value(asset, amount, price, source, &clock)?,
        });
    }
    let health = calculate_health(&collaterals, &debts)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);
    Ok(health.borrow_health_factor_wad)
}

/// A drained spent vault leaves the active list; a funded received vault joins it.
fn update_active_assets(accounts: &mut MarginExecute) -> Result<()> {
    let spent_index = accounts.spent_asset.asset_index;
    let received_index = accounts.received_asset.asset_index;
    let margin = &mut accounts.margin_account;
    if accounts.spent_vault.amount == 0 && margin.is_collateral_active(spent_index) {
        margin.remove_active_collateral(spent_index)?;
    }
    if accounts.received_vault.amount > 0 && !margin.is_collateral_active(received_index) {
        require!(
            margin.collateral_count + margin.debt_count < accounts.protocol_config.max_assets_per_margin,
            VannaError::TooManyAssets
        );
        margin.add_active_collateral(received_index)?;
    }
    Ok(())
}
