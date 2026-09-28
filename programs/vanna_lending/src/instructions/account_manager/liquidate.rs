//! Liquidation of margin accounts at or below the 1.10 health threshold, as in the Solidity
//! AccountManager (`liquidate` → `_liquidate` → `sweepTo`): the liquidator repays every debt in
//! full and receives every asset the account holds, including external-protocol receipts such as
//! Kamino cTokens, in one instruction. There is no close factor, bonus or health-improvement
//! check, so an account can be liquidated at any health factor ≤ 1.10, including below 1: the
//! liquidator's reward is whatever the assets are worth beyond the debt.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::calculate_health;
use crate::math::shares::debt_shares_to_assets_up;
use crate::risk_engine::scan_positions;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::accounts::validate_asset_config;
use crate::validation::token::gross_up_for_transfer_fee;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{transfer_checked, TokenAccount, TransferChecked};

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
}

/// `remaining_accounts`, in this order:
/// 1. The health accounts of every active position (the `risk_engine` layout, nothing skipped),
///    with each margin vault, reserve and debt position writable.
/// 2. Per active collateral, in the same order: `[mint, destination (w), token_program]`. The
///    destination is any token account of that mint, usually the liquidator's ATA.
/// 3. Per active debt, in the same order: `[mint, reserve liquidity vault (w), source (w),
///    token_program]`. The source is a token account the liquidator owns.
///
/// Collateral is swept before debts are repaid, so the liquidator can repay a debt with the same
/// token swept from the account (e.g. USDC) and only needs to bring the difference.
pub fn public_liquidate<'info>(ctx: Context<'info, PublicLiquidate<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let margin_key = ctx.accounts.margin_account.key();

    // Solidity: `if (riskEngine.isAccountHealthy(account)) revert AccountNotLiquidatable()`.
    let (collaterals, debts, health_len) = scan_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        &[],
        None,
    )?;
    let health = calculate_health(&collaterals, &debts)?;
    require!(health.is_liquidatable(), VannaError::PositionHealthy);

    let (groups, settlement) = ctx.remaining_accounts.split_at(health_len);
    let mut settlement = settlement.iter();
    let mut next_settlement = || settlement.next().ok_or(VannaError::IncompletePositionAccounts);
    let mut group = 0usize;

    let liquidator = ctx.accounts.liquidator.to_account_info();
    let margin_info = ctx.accounts.margin_account.to_account_info();
    let margin = &mut ctx.accounts.margin_account;
    let authority_key = margin.authority;
    let margin_bump = [margin.bump];
    let margin_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &margin_bump]];

    // sweepTo(liquidator): every collateral vault is emptied into the liquidator's accounts.
    let collateral_indexes: Vec<u16> = margin.active_collateral_indexes().collect();
    let collaterals_seized = collateral_indexes.len() as u8;
    for index in collateral_indexes {
        let asset = Account::<AssetConfig>::try_from(&groups[group])?;
        let vault = &groups[group + 1];
        group += if asset.is_pyth_priced() { 3 } else { 4 };
        let (mint, destination, token_program) = (next_settlement()?, next_settlement()?, next_settlement()?);
        validate_asset_config(&asset, &mint.key(), &token_program.key())?;

        let amount = InterfaceAccount::<TokenAccount>::try_from(vault)?.amount;
        if amount > 0 {
            transfer(token_program, vault, mint, destination, &margin_info, margin_seeds, amount, asset.decimals)?;
        }
        margin.remove_active_collateral(index)?;
        emit!(CollateralSeized {
            margin_account: margin_key,
            liquidator: liquidator.key(),
            mint: asset.mint,
            amount,
            destination: destination.key(),
            event_sequence: margin.next_event_sequence()?,
            timestamp: clock.unix_timestamp,
        });
    }

    // For each borrow: updateState, repay the full borrow balance, collectFrom, removeBorrow.
    let debt_indexes: Vec<u16> = margin.active_debt_indexes().collect();
    let debts_repaid = debt_indexes.len() as u8;
    for index in debt_indexes {
        let asset = Account::<AssetConfig>::try_from(&groups[group])?;
        let mut reserve = Account::<Reserve>::try_from(&groups[group + 1])?;
        let mut position = Account::<DebtPosition>::try_from(&groups[group + 2])?;
        group += 4;
        let (mint, reserve_vault, source, token_program) =
            (next_settlement()?, next_settlement()?, next_settlement()?, next_settlement()?);
        validate_asset_config(&asset, &mint.key(), &token_program.key())?;
        require_keys_eq!(reserve_vault.key(), reserve.liquidity_vault, VannaError::InvalidVaultAuthority);

        reserve.accrue_interest(clock.unix_timestamp)?;
        let debt = debt_shares_to_assets_up(position.borrow_shares, reserve.total_borrow_shares, reserve.total_borrow_assets)?;
        let gross = gross_up_for_transfer_fee(mint, &token_program.key(), debt)?;
        let vault_before = InterfaceAccount::<TokenAccount>::try_from(reserve_vault)?.amount;
        transfer(token_program, source, mint, reserve_vault, &liquidator, &[], gross, asset.decimals)?;
        let received = InterfaceAccount::<TokenAccount>::try_from(reserve_vault)?
            .amount
            .checked_sub(vault_before)
            .ok_or(VannaError::MathUnderflow)?;
        require!(received >= debt, VannaError::OutstandingDebt);

        let shares = position.borrow_shares;
        reserve.accounted_liquidity_assets = reserve
            .accounted_liquidity_assets
            .checked_add(received)
            .ok_or(VannaError::MathOverflow)?;
        reserve.total_borrow_assets = reserve.total_borrow_assets.saturating_sub(debt);
        reserve.total_borrow_shares = reserve
            .total_borrow_shares
            .checked_sub(shares)
            .ok_or(VannaError::MathUnderflow)?;
        position.debit_shares(shares)?;
        reserve.exit(ctx.program_id)?;
        position.exit(ctx.program_id)?;
        margin.remove_active_debt(index)?;

        emit!(DebtRepaid {
            margin_account: margin_key,
            reserve: reserve.key(),
            payer: liquidator.key(),
            assets: received,
            debt_shares_burned: shares,
            remaining_debt_shares: 0,
            event_sequence: margin.next_event_sequence()?,
            timestamp: clock.unix_timestamp,
        });
    }
    require!(next_settlement().is_err(), VannaError::IncompletePositionAccounts);

    emit!(Liquidated {
        margin_account: margin_key,
        liquidator: liquidator.key(),
        collateral_value: health.liquidation_collateral_value,
        debt_value: health.total_debt_value,
        health_factor_wad: health.liquidation_health_factor_wad,
        collaterals_seized,
        debts_repaid,
        event_sequence: margin.next_event_sequence()?,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transfer<'info>(
    token_program: &AccountInfo<'info>,
    from: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    amount: u64,
    decimals: u8,
) -> Result<()> {
    let accounts = TransferChecked {
        from: from.clone(),
        mint: mint.clone(),
        to: to.clone(),
        authority: authority.clone(),
    };
    transfer_checked(
        CpiContext::new_with_signer(token_program.key(), accounts, signer_seeds),
        amount,
        decimals,
    )
}
