//! Account-level risk: finds, validates and values every open position of a margin account, the
//! inputs to its health factor (`math::health`).

use crate::constants::{ASSET_SEED, DEBT_SEED, RESERVE_SEED};
use crate::errors::VannaError;
use crate::math::health::{normalize_token_value, CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::oracle::pyth::load_validated_price;
use crate::oracle::valuation::collateral_value;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

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
    let asset_config = Account::<AssetConfig>::try_from(info)?;
    verify_pda(info.key, &[ASSET_SEED, asset_config.mint.as_ref()], asset_config.bump, program_id)?;
    require!(asset_config.asset_index == asset_index, VannaError::IncompletePositionAccounts);
    Ok(asset_config)
}

/// Validates and values every open position of `margin` from `remaining_accounts`, skipping the
/// `named_*` positions the caller values itself. Any missing, substituted, reordered or extra
/// account fails closed, so no position can be hidden.
///
/// Per collateral: `[asset_config, margin_vault, price_update]`, plus the price-source account
/// when the asset is not Pyth-priced. Per debt: `[asset_config, reserve, debt_position,
/// price_update]`. Collateral is valued from the vault's live balance, which is safe without a
/// ledger because each vault is private to one `(margin, mint)` pair. Reserves are accrued in
/// memory only, so the scan takes no write locks.
// `#[inline(never)]`: own BPF stack frame.
#[inline(never)]
pub fn scan_and_validate_positions<'info>(
    margin_key: &Pubkey,
    margin: &MarginAccount,
    remaining_accounts: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
    clock: &Clock,
    named_collaterals: &[u16],
    named_debt: Option<u16>,
) -> Result<(Vec<CollateralValuation>, Vec<DebtValuation>)> {
    let mut cursor = 0usize;
    let mut collaterals = Vec::with_capacity(margin.collateral_count as usize);
    let mut debts = Vec::with_capacity(margin.debt_count as usize);

    for asset_index in margin.active_collateral_indexes() {
        if named_collaterals.contains(&asset_index) {
            continue;
        }
        require!(cursor + 3 <= remaining_accounts.len(), VannaError::IncompletePositionAccounts);
        let asset_config = load_asset_config(&remaining_accounts[cursor], asset_index, program_id)?;
        let vault_info = &remaining_accounts[cursor + 1];
        let price_info = &remaining_accounts[cursor + 2];
        cursor += 3;
        let source = if asset_config.is_pyth_priced() {
            None
        } else {
            require!(cursor < remaining_accounts.len(), VannaError::IncompletePositionAccounts);
            cursor += 1;
            Some(&remaining_accounts[cursor - 1])
        };

        // InterfaceAccount accepts classic SPL and Token-2022 token-account sizes.
        let vault = InterfaceAccount::<TokenAccount>::try_from(vault_info)?;
        verify_associated_token_account(vault_info.key, margin_key, &asset_config.mint, &asset_config.token_program)?;
        require_keys_eq!(vault.owner, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(vault.mint, asset_config.mint, VannaError::IncompletePositionAccounts);

        let price_account = Account::<PriceUpdateV2>::try_from(price_info)?;
        collaterals.push(CollateralValuation {
            collateral_value: collateral_value(&asset_config, vault.amount, &price_account, source, clock)?,
        });
    }

    for asset_index in margin.active_debt_indexes() {
        if Some(asset_index) == named_debt {
            continue;
        }
        require!(cursor + 4 <= remaining_accounts.len(), VannaError::IncompletePositionAccounts);
        let asset_info = &remaining_accounts[cursor];
        let asset_config = load_asset_config(asset_info, asset_index, program_id)?;
        let reserve_info = &remaining_accounts[cursor + 1];
        let debt_position_info = &remaining_accounts[cursor + 2];
        let price_info = &remaining_accounts[cursor + 3];
        cursor += 4;

        let reserve = Account::<Reserve>::try_from(reserve_info)?;
        verify_pda(reserve_info.key, &[RESERVE_SEED, asset_config.mint.as_ref()], reserve.bump, program_id)?;
        require_keys_eq!(reserve.asset_config, asset_info.key(), VannaError::IncompletePositionAccounts);
        require_keys_eq!(asset_config.reserve, reserve_info.key(), VannaError::IncompletePositionAccounts);

        let debt_position = Account::<DebtPosition>::try_from(debt_position_info)?;
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

        // Only Pyth-priced assets can have a reserve, so debt is always valued from Pyth.
        let price_account = Account::<PriceUpdateV2>::try_from(price_info)?;
        let validated_price = load_validated_price(&asset_config, &price_account, clock)?;
        debts.push(DebtValuation {
            debt_value: normalize_token_value(
                current_debt_assets,
                validated_price.price,
                validated_price.exponent,
                asset_config.decimals,
                true,
            )?,
        });
    }

    require!(cursor == remaining_accounts.len(), VannaError::IncompletePositionAccounts);
    Ok((collaterals, debts))
}
