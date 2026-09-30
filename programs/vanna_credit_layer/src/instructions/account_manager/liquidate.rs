use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::calculate_health;
use crate::math::shares::debt_shares_to_assets_up;
use crate::interface::AgentAccounts;
use crate::risk_engine::{collect_holdings, agents_of, split_positions, value_holdings, COLLATERAL_GROUP_LEN, DEBT_GROUP_LEN};
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::accounts::validate_asset_config;
use crate::validation::token::gross_up_for_transfer_fee;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{transfer_checked, Mint, TokenAccount, TransferChecked};
use crate::interface::{venue_account_address, PriceChecks, VENUE_ACCOUNT_SEED};

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

pub fn public_liquidate<'info>(ctx: Context<'info, PublicLiquidate<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let margin_key = ctx.accounts.margin_account.key();

    let margin = &ctx.accounts.margin_account;
    let (groups, rest) = split_positions(ctx.remaining_accounts, margin, &[], None)?;
    let settlement_len = margin.active_collateral_indexes().count() * 3 + margin.active_debt_indexes().count() * 4;
    require!(settlement_len <= rest.len(), VannaError::IncompletePositionAccounts);
    let (settlement, agent_accounts) = rest.split_at(settlement_len);
    let holdings = collect_holdings(&margin_key, margin, groups, ctx.program_id, &clock, &[], None)?;
    let agents = AgentAccounts::parse(agent_accounts, &agents_of(&holdings, &[]))?;
    let valuation = value_holdings(&holdings, &agents)?;
    valuation.require(PriceChecks::LIQUIDATION)?;
    let health = calculate_health(&valuation.collaterals, &valuation.debts)?;
    require!(health.is_liquidatable(), VannaError::PositionHealthy);

    let mut settlement = settlement.iter();
    let mut next_settlement = || settlement.next().ok_or(VannaError::IncompletePositionAccounts);
    let mut group = 0usize;

    let liquidator = ctx.accounts.liquidator.to_account_info();
    let margin_info = ctx.accounts.margin_account.to_account_info();
    let margin = &mut ctx.accounts.margin_account;
    let authority_key = margin.authority;
    let margin_bump = [margin.bump];
    let margin_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &margin_bump]];

    let collateral_indexes: Vec<u16> = margin.active_collateral_indexes().collect();
    let collaterals_seized = collateral_indexes.len() as u8;
    for (k, index) in collateral_indexes.into_iter().enumerate() {
        let asset = Account::<AssetConfig>::try_from(&groups[group])?;
        let vault = &groups[group + 1];
        group += COLLATERAL_GROUP_LEN;
        let (mint, destination, token_program) = (next_settlement()?, next_settlement()?, next_settlement()?);

        let amount = if asset.is_venue() {
            require!(valuation.prices[k].open_legs == 0, VannaError::VenuePositionsOpen);
            require_keys_eq!(mint.key(), asset.settle_mint, VannaError::InvalidMint);
            let idle_key = get_associated_token_address_with_program_id(vault.key, mint.key, token_program.key);
            let idle = agents.find(&idle_key);
            let idle_amount = match idle {
                Some(info) if !info.data_is_empty() => InterfaceAccount::<TokenAccount>::try_from(info)?.amount,
                _ => 0,
            };
            if let (Some(idle), true) = (idle, idle_amount > 0) {
                let decimals = Mint::try_deserialize(&mut &mint.try_borrow_data()?[..])?.decimals;
                let (_, venue_account_bump) = venue_account_address(&margin_key, &asset.mint);
                let venue_account_seeds: &[&[&[u8]]] = &[&[VENUE_ACCOUNT_SEED, margin_key.as_ref(), asset.mint.as_ref(), &[venue_account_bump]]];
                transfer(token_program, idle, mint, destination, vault, venue_account_seeds, idle_amount, decimals)?;
            }
            margin.set_legs(index, 0)?;
            idle_amount
        } else {
            validate_asset_config(&asset, &mint.key(), &token_program.key())?;
            let amount = InterfaceAccount::<TokenAccount>::try_from(vault)?.amount;
            if amount > 0 {
                transfer(token_program, vault, mint, destination, &margin_info, margin_seeds, amount, asset.decimals)?;
            }
            amount
        };
        margin.remove_active_collateral(index)?;
        emit!(CollateralSeized {
            margin_account: margin_key,
            liquidator: liquidator.key(),
            mint: mint.key(),
            amount,
            destination: destination.key(),
            event_sequence: margin.next_event_sequence()?,
            timestamp: clock.unix_timestamp,
        });
    }

    let debt_indexes: Vec<u16> = margin.active_debt_indexes().collect();
    let debts_repaid = debt_indexes.len() as u8;
    for index in debt_indexes {
        let asset = Account::<AssetConfig>::try_from(&groups[group])?;
        let mut reserve = Account::<Reserve>::try_from(&groups[group + 1])?;
        let mut position = Account::<DebtPosition>::try_from(&groups[group + 2])?;
        group += DEBT_GROUP_LEN;
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
