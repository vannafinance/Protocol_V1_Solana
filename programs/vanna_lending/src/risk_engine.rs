//! Account-level risk: finds, validates and values every open position of a margin account, the
//! inputs to its health factor (`math::health`). Every price comes from the oracle facade.

use crate::constants::{ASSET_SEED, DEBT_SEED, RESERVE_SEED};
use crate::errors::VannaError;
use crate::math::health::{CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::oracle::{get_price, PriceStatus};
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;

/// Collateral group: `[asset_config, margin_vault]`.
pub const COLLATERAL_GROUP_LEN: usize = 2;
/// Debt group: `[asset_config, reserve, debt_position]`.
pub const DEBT_GROUP_LEN: usize = 3;

/// Every open position's value, and the price checks all of their prices passed.
pub struct Valuation {
    pub collaterals: Vec<CollateralValuation>,
    pub debts: Vec<DebtValuation>,
    pub status: PriceStatus,
}

fn verify_pda(actual: &Pubkey, seeds: &[&[u8]], bump: u8, program_id: &Pubkey) -> Result<()> {
    let bump_seed = [bump];
    let mut full_seeds: Vec<&[u8]> = seeds.to_vec();
    full_seeds.push(&bump_seed);
    let derived = Pubkey::create_program_address(&full_seeds, program_id)
        .map_err(|_| VannaError::InvalidBump)?;
    require_keys_eq!(derived, *actual, VannaError::InvalidPda);
    Ok(())
}

fn load_asset_config<'info>(
    info: &'info AccountInfo<'info>,
    asset_index: u16,
    program_id: &Pubkey,
) -> Result<Account<'info, AssetConfig>> {
    // A group that doesn't parse is a missing or shifted position.
    let asset_config =
        Account::<AssetConfig>::try_from(info).map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
    verify_pda(info.key, &[ASSET_SEED, asset_config.mint.as_ref()], asset_config.bump, program_id)?;
    require!(asset_config.asset_index == asset_index, VannaError::IncompletePositionAccounts);
    Ok(asset_config)
}

/// Accounts the position groups of `margin` take, without the `named_*` positions.
pub fn position_accounts_len(margin: &MarginAccount, named_collaterals: &[u16], named_debt: Option<u16>) -> usize {
    let collaterals = margin.active_collateral_indexes().filter(|i| !named_collaterals.contains(i)).count();
    let debts = margin.active_debt_indexes().filter(|i| Some(*i) != named_debt).count();
    collaterals * COLLATERAL_GROUP_LEN + debts * DEBT_GROUP_LEN
}

/// Splits `accounts` into the position groups and the accounts after them.
pub fn split_positions<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    margin: &MarginAccount,
    named_collaterals: &[u16],
    named_debt: Option<u16>,
) -> Result<(&'a [AccountInfo<'info>], &'a [AccountInfo<'info>])> {
    let len = position_accounts_len(margin, named_collaterals, named_debt);
    require!(len <= accounts.len(), VannaError::IncompletePositionAccounts);
    Ok(accounts.split_at(len))
}

/// Validates and values every open position of `margin`, skipping the `named_*` positions the
/// caller values itself. `positions` must be exactly their groups, in the margin's slot order:
/// any missing, substituted, reordered or extra account fails closed, so no position can be
/// hidden. Prices are read through the oracle facade from `oracle_accounts` (found by key).
///
/// Collateral is valued from the vault's live balance, which is safe without a ledger because
/// each vault is private to one `(margin, mint)` pair. Reserves are accrued in memory only, so
/// the scan takes no write locks.
// `#[inline(never)]`: own BPF stack frame.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn scan_positions<'info>(
    margin_key: &Pubkey,
    margin: &MarginAccount,
    positions: &'info [AccountInfo<'info>],
    oracle_accounts: &[&[AccountInfo<'info>]],
    program_id: &Pubkey,
    clock: &Clock,
    named_collaterals: &[u16],
    named_debt: Option<u16>,
) -> Result<Valuation> {
    let mut cursor = 0usize;
    let mut collaterals = Vec::with_capacity(margin.collateral_count as usize);
    let mut debts = Vec::with_capacity(margin.debt_count as usize);
    let mut status = PriceStatus::ALL_CHECKS;

    for asset_index in margin.active_collateral_indexes() {
        if named_collaterals.contains(&asset_index) {
            continue;
        }
        require!(cursor + COLLATERAL_GROUP_LEN <= positions.len(), VannaError::IncompletePositionAccounts);
        let asset_config = load_asset_config(&positions[cursor], asset_index, program_id)?;
        let vault_info = &positions[cursor + 1];
        cursor += COLLATERAL_GROUP_LEN;

        // InterfaceAccount accepts classic SPL and Token-2022 token-account sizes.
        let vault = InterfaceAccount::<TokenAccount>::try_from(vault_info)
            .map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_associated_token_account(vault_info.key, margin_key, &asset_config.mint, &asset_config.token_program)?;
        require_keys_eq!(vault.owner, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(vault.mint, asset_config.mint, VannaError::IncompletePositionAccounts);

        let price = get_price(&asset_config, oracle_accounts, clock)?;
        status = status.intersection(price.status);
        collaterals.push(CollateralValuation {
            collateral_value: price.value_of(vault.amount, asset_config.decimals, false)?,
        });
    }

    for asset_index in margin.active_debt_indexes() {
        if Some(asset_index) == named_debt {
            continue;
        }
        require!(cursor + DEBT_GROUP_LEN <= positions.len(), VannaError::IncompletePositionAccounts);
        let asset_info = &positions[cursor];
        let asset_config = load_asset_config(asset_info, asset_index, program_id)?;
        let reserve_info = &positions[cursor + 1];
        let debt_position_info = &positions[cursor + 2];
        cursor += DEBT_GROUP_LEN;

        let reserve =
            Account::<Reserve>::try_from(reserve_info).map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_pda(reserve_info.key, &[RESERVE_SEED, asset_config.mint.as_ref()], reserve.bump, program_id)?;
        require_keys_eq!(reserve.asset_config, asset_info.key(), VannaError::IncompletePositionAccounts);
        require_keys_eq!(asset_config.reserve, reserve_info.key(), VannaError::IncompletePositionAccounts);

        let debt_position = Account::<DebtPosition>::try_from(debt_position_info)
            .map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_pda(
            debt_position_info.key,
            &[DEBT_SEED, margin_key.as_ref(), reserve_info.key.as_ref()],
            debt_position.bump,
            program_id,
        )?;
        require_keys_eq!(debt_position.margin_account, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(debt_position.reserve, *reserve_info.key, VannaError::IncompletePositionAccounts);

        let accrual = accrue(&reserve, clock.unix_timestamp)?;
        let current_debt_assets = debt_shares_to_assets_up(
            debt_position.borrow_shares,
            reserve.total_borrow_shares,
            accrual.new_total_borrow_assets,
        )?;

        let price = get_price(&asset_config, oracle_accounts, clock)?;
        status = status.intersection(price.status);
        debts.push(DebtValuation {
            debt_value: price.value_of(current_debt_assets, asset_config.decimals, true)?,
        });
    }

    require!(cursor == positions.len(), VannaError::IncompletePositionAccounts);
    Ok(Valuation { collaterals, debts, status })
}
