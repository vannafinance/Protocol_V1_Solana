//! Atomic exact-input margin swap. Jupiter receives only a dedicated escrow signer,
//! never the margin PDA signer. All output returns to margin before health is checked.
use crate::{
    constants::*,
    errors::VannaError,
    math::health::{calculate_health, normalize_token_value, CollateralValuation},
    oracle::pyth::load_validated_price,
    state::{
        asset_config::AssetConfig, margin_account::MarginAccount, protocol_config::ProtocolConfig,
    },
    validation::{
        accounts::{assert_protocol_action_allowed, validate_asset_config, ProtocolAction},
        positions::scan_and_validate_positions,
        token::transfer_out_checked,
    },
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount, TokenInterface},
};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

pub const SWAP_SEED: &[u8] = b"margin_swap";
pub const JUPITER: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

#[derive(Accounts)]
pub struct UserMarginSwap<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(mut, seeds = [MARGIN_SEED, authority.key().as_ref()], bump = margin_account.bump, has_one = authority @ VannaError::Unauthorized)]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, input_mint.key().as_ref()], bump = input_config.bump)]
    pub input_config: Box<Account<'info, AssetConfig>>,
    #[account(seeds = [ASSET_SEED, output_mint.key().as_ref()], bump = output_config.bump)]
    pub output_config: Box<Account<'info, AssetConfig>>,
    pub input_mint: Box<InterfaceAccount<'info, Mint>>,
    pub output_mint: Box<InterfaceAccount<'info, Mint>>,
    pub input_price: Box<Account<'info, PriceUpdateV2>>,
    pub output_price: Box<Account<'info, PriceUpdateV2>>,
    #[account(mut, associated_token::mint = input_mint, associated_token::authority = margin_account, associated_token::token_program = input_token_program)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(init_if_needed, payer = authority, associated_token::mint = output_mint, associated_token::authority = margin_account, associated_token::token_program = output_token_program)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: only this isolated PDA signs the external CPI; it controls no margin vaults.
    #[account(seeds = [SWAP_SEED, margin_account.key().as_ref()], bump)]
    pub swap_authority: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = authority, associated_token::mint = input_mint, associated_token::authority = swap_authority, associated_token::token_program = input_token_program)]
    pub input_escrow: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(init_if_needed, payer = authority, associated_token::mint = output_mint, associated_token::authority = swap_authority, associated_token::token_program = output_token_program)]
    pub output_escrow: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: fixed executable Jupiter aggregator.
    #[account(address = JUPITER @ VannaError::InvalidSwapRoute, executable)]
    pub jupiter_program: UncheckedAccount<'info>,
    pub input_token_program: Interface<'info, TokenInterface>,
    pub output_token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn user_margin_swap<'info>(
    ctx: Context<'info, UserMarginSwap<'info>>,
    amount_in: u64,
    min_amount_out: u64,
    route_account_count: u16,
    route_data: Vec<u8>,
) -> Result<()> {
    assert_protocol_action_allowed(
        ctx.accounts.protocol_config.operating_mode,
        ProtocolAction::CollateralWithdraw,
    )?;
    require!(amount_in > 0 && min_amount_out > 0, VannaError::ZeroAmount);
    require_keys_neq!(
        ctx.accounts.input_mint.key(),
        ctx.accounts.output_mint.key(),
        VannaError::InvalidSwapRoute
    );
    validate_asset_config(
        &ctx.accounts.input_config,
        &ctx.accounts.input_mint.key(),
        &ctx.accounts.input_token_program.key(),
    )?;
    validate_asset_config(
        &ctx.accounts.output_config,
        &ctx.accounts.output_mint.key(),
        &ctx.accounts.output_token_program.key(),
    )?;
    require!(
        ctx.accounts.output_config.collateral_enabled,
        VannaError::AssetNotCollateralEnabled
    );
    require!(
        ctx.accounts
            .margin_account
            .is_collateral_active(ctx.accounts.input_config.asset_index),
        VannaError::IncompletePositionAccounts
    );
    require!(
        ctx.accounts.input_vault.amount >= amount_in,
        VannaError::InsufficientCollateral
    );
    let route_count = usize::from(route_account_count);
    require!(
        route_count > 0 && route_count <= ctx.remaining_accounts.len() && route_data.len() >= 8,
        VannaError::InvalidSwapRoute
    );
    let (route_accounts, health_accounts) = ctx.remaining_accounts.split_at(route_count);
    // No Vanna state/vault, wallet signer, or margin signer can be passed to the route.
    // This also prevents a route from re-entering Vanna with privileged accounts.
    for account in route_accounts {
        require!(
            *account.owner != crate::ID && account.key() != crate::ID,
            VannaError::InvalidSwapRoute
        );
        require!(
            account.key() != ctx.accounts.authority.key()
                && account.key() != ctx.accounts.input_vault.key()
                && account.key() != ctx.accounts.output_vault.key(),
            VannaError::InvalidSwapRoute
        );
    }
    let margin_key = ctx.accounts.margin_account.key();
    let owner = ctx.accounts.authority.key();
    let margin_bump = [ctx.accounts.margin_account.bump];
    let margin_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, owner.as_ref(), &margin_bump]];
    let swap_bump = [ctx.bumps.swap_authority];
    let swap_seeds: &[&[&[u8]]] = &[&[SWAP_SEED, margin_key.as_ref(), &swap_bump]];
    // Snapshot donations too: the route may consume exactly this call's input.
    let input_before = ctx.accounts.input_escrow.amount;
    let output_before = ctx.accounts.output_escrow.amount;
    let vault_input_before = ctx.accounts.input_vault.amount;
    let vault_output_before = ctx.accounts.output_vault.amount;
    transfer_out_checked(
        &ctx.accounts.input_token_program,
        &ctx.accounts.input_mint,
        &ctx.accounts.input_vault,
        &ctx.accounts.input_escrow,
        &ctx.accounts.margin_account.to_account_info(),
        margin_seeds,
        amount_in,
    )?;
    ctx.accounts.input_escrow.reload()?;
    require!(
        ctx.accounts.input_escrow.amount.checked_sub(input_before) == Some(amount_in),
        VannaError::InvalidSwapRoute
    );

    let metas = route_accounts
        .iter()
        .map(|a| AccountMeta {
            pubkey: a.key(),
            is_writable: a.is_writable,
            is_signer: a.key() == ctx.accounts.swap_authority.key(),
        })
        .collect();
    let route = Instruction {
        program_id: JUPITER,
        accounts: metas,
        data: route_data,
    };
    let mut infos = route_accounts.to_vec();
    infos.push(ctx.accounts.jupiter_program.to_account_info());
    invoke_signed(&route, &infos, swap_seeds)?;
    ctx.accounts.input_escrow.reload()?;
    ctx.accounts.output_escrow.reload()?;
    require!(
        ctx.accounts.input_escrow.amount == input_before,
        VannaError::InvalidSwapRoute
    );
    for escrow in [&ctx.accounts.input_escrow, &ctx.accounts.output_escrow] {
        require_keys_eq!(
            escrow.owner,
            ctx.accounts.swap_authority.key(),
            VannaError::InvalidSwapRoute
        );
        require!(
            escrow.delegate.is_none() && escrow.close_authority.is_none(),
            VannaError::InvalidSwapRoute
        );
    }
    let output = ctx
        .accounts
        .output_escrow
        .amount
        .checked_sub(output_before)
        .ok_or(VannaError::SlippageExceeded)?;
    require!(output >= min_amount_out, VannaError::SlippageExceeded);
    transfer_out_checked(
        &ctx.accounts.output_token_program,
        &ctx.accounts.output_mint,
        &ctx.accounts.output_escrow,
        &ctx.accounts.output_vault,
        &ctx.accounts.swap_authority.to_account_info(),
        swap_seeds,
        output,
    )?;
    ctx.accounts.input_vault.reload()?;
    ctx.accounts.output_vault.reload()?;
    let received = ctx
        .accounts
        .output_vault
        .amount
        .checked_sub(vault_output_before)
        .ok_or(VannaError::SlippageExceeded)?;
    require!(received >= min_amount_out, VannaError::SlippageExceeded);
    require!(
        vault_input_before.checked_sub(ctx.accounts.input_vault.amount) == Some(amount_in),
        VannaError::InvalidSwapRoute
    );
    let cap = ctx.accounts.output_config.max_collateral_per_margin;
    require!(
        cap == UNCAPPED || ctx.accounts.output_vault.amount <= cap,
        VannaError::CollateralCapExceeded
    );

    // Evaluate after CPI using freshly reloaded vaults. Existing output collateral
    // is included by the complete-position scanner; new output is valued below.
    let clock = Clock::get()?;
    let (scanned, debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        health_accounts,
        ctx.program_id,
        &clock,
        Some(ctx.accounts.input_config.asset_index),
        None,
        None,
    )?;
    let mut collaterals: Vec<_> = scanned.into_iter().map(|c| c.valuation).collect();
    let input_price = load_validated_price(
        &ctx.accounts.input_config,
        &ctx.accounts.input_price,
        &clock,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: normalize_token_value(
            ctx.accounts.input_vault.amount,
            input_price.price,
            input_price.exponent,
            ctx.accounts.input_config.decimals,
            false,
        )?,
    });
    let output_active = ctx
        .accounts
        .margin_account
        .is_collateral_active(ctx.accounts.output_config.asset_index);
    if !output_active {
        let price = load_validated_price(
            &ctx.accounts.output_config,
            &ctx.accounts.output_price,
            &clock,
        )?;
        collaterals.push(CollateralValuation {
            collateral_value: normalize_token_value(
                ctx.accounts.output_vault.amount,
                price.price,
                price.exponent,
                ctx.accounts.output_config.decimals,
                false,
            )?,
        });
    }
    let health = calculate_health(
        &collaterals,
        &debts.into_iter().map(|d| d.valuation).collect::<Vec<_>>(),
    )?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);
    if ctx.accounts.input_vault.amount == 0 {
        ctx.accounts
            .margin_account
            .remove_active_collateral(ctx.accounts.input_config.asset_index)?;
    }
    if !output_active {
        require!(
            (ctx.accounts.margin_account.collateral_count + ctx.accounts.margin_account.debt_count)
                < ctx.accounts.protocol_config.max_assets_per_margin,
            VannaError::TooManyAssets
        );
        ctx.accounts
            .margin_account
            .add_active_collateral(ctx.accounts.output_config.asset_index)?;
    }
    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(MarginSwapped {
        margin_account: margin_key,
        input_mint: ctx.accounts.input_mint.key(),
        output_mint: ctx.accounts.output_mint.key(),
        amount_in,
        amount_out: received,
        event_sequence
    });
    Ok(())
}

#[event]
pub struct MarginSwapped {
    pub margin_account: Pubkey,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub amount_in: u64,
    pub amount_out: u64,
    pub event_sequence: u64,
}
