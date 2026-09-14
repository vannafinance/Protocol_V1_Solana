use crate::constants::{ASSET_SEED, DEBT_SEED, RESERVE_SEED};
use crate::errors::VannaError;
use crate::math::health::{normalize_token_value, CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// One scanned-and-validated collateral position, ready to feed `calculate_health`. Its value
/// comes straight from the margin vault's live SPL balance — there is no separate ledger for
/// collateral (see `instructions/margin.rs` for why that's safe: each vault is private to one
/// `(margin, mint)` pair, unlike the lending `Reserve`'s pooled liquidity vault).
pub struct ScannedCollateral {
    pub asset_index: u16,
    pub mint: Pubkey,
    pub valuation: CollateralValuation,
}

/// One scanned-and-validated debt position, ready to feed `calculate_health`.
pub struct ScannedDebt {
    pub asset_index: u16,
    pub reserve: Pubkey,
    pub current_debt_assets: u64,
    pub valuation: DebtValuation,
}

fn verify_pda(actual: &Pubkey, seeds: &[&[u8]], bump: u8, program_id: &Pubkey) -> Result<()> {
    let bump_seed = [bump];
    let mut full_seeds: Vec<&[u8]> = seeds.to_vec();
    full_seeds.push(&bump_seed);
    let derived =
        Pubkey::create_program_address(&full_seeds, program_id).map_err(|_| VannaError::InvalidBump)?;
    require_keys_eq!(derived, *actual, VannaError::InvalidPda);
    Ok(())
}

/// Spec §6.6 `validate_complete_positions` + the price/valuation loading it depends on, folded
/// into one scan. Walks `margin`'s canonical active-asset arrays and consumes exactly the matching
/// accounts from `remaining_accounts`, in that same order, skipping only the position the caller
/// already holds as a named (Anchor-validated, mutable) account — identified by
/// `named_collateral_index` / `named_debt_index`. Any missing, substituted, reordered, or extra
/// account fails closed.
///
/// Reserves encountered here are accrued in-memory (fresh values feed the health check) but not
/// persisted — only the caller's own named reserve, if any, is written back. This keeps the scan
/// read-only and avoids taking unnecessary write locks on reserves the instruction isn't touching.
pub fn scan_and_validate_positions<'info>(
    margin_key: &Pubkey,
    margin: &MarginAccount,
    remaining_accounts: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
    clock: &Clock,
    named_collateral_index: Option<u16>,
    named_debt_index: Option<u16>,
) -> Result<(Vec<ScannedCollateral>, Vec<ScannedDebt>)> {
    let mut cursor = 0usize;
    let mut collaterals = Vec::with_capacity(margin.collateral_count as usize);
    let mut debts = Vec::with_capacity(margin.debt_count as usize);

    for asset_index in margin.active_collateral_indexes() {
        if Some(asset_index) == named_collateral_index {
            continue;
        }
        require!(cursor + 3 <= remaining_accounts.len(), VannaError::IncompletePositionAccounts);
        let asset_info = &remaining_accounts[cursor];
        let vault_info = &remaining_accounts[cursor + 1];
        let price_info = &remaining_accounts[cursor + 2];
        cursor += 3;

        let asset_config = Account::<AssetConfig>::try_from(asset_info)?;
        verify_pda(
            asset_info.key,
            &[ASSET_SEED, asset_config.mint.as_ref()],
            asset_config.bump,
            program_id,
        )?;
        require!(asset_config.asset_index == asset_index, VannaError::IncompletePositionAccounts);

        let vault = Account::<TokenAccount>::try_from(vault_info)?;
        verify_associated_token_account(vault_info.key, margin_key, &asset_config.mint)?;
        require_keys_eq!(vault.owner, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(vault.mint, asset_config.mint, VannaError::IncompletePositionAccounts);

        let price_account = Account::<PriceUpdateV2>::try_from(price_info)?;
        let validated_price = load_validated_price(&asset_config, &price_account, clock)?;
        let collateral_value = normalize_token_value(
            vault.amount,
            validated_price.price,
            validated_price.exponent,
            asset_config.decimals,
            false,
        )?;

        collaterals.push(ScannedCollateral {
            asset_index,
            mint: asset_config.mint,
            valuation: CollateralValuation {
                collateral_value,
            },
        });
    }

    for asset_index in margin.active_debt_indexes() {
        if Some(asset_index) == named_debt_index {
            continue;
        }
        require!(cursor + 4 <= remaining_accounts.len(), VannaError::IncompletePositionAccounts);
        let asset_info = &remaining_accounts[cursor];
        let reserve_info = &remaining_accounts[cursor + 1];
        let debt_position_info = &remaining_accounts[cursor + 2];
        let price_info = &remaining_accounts[cursor + 3];
        cursor += 4;

        let asset_config = Account::<AssetConfig>::try_from(asset_info)?;
        verify_pda(
            asset_info.key,
            &[ASSET_SEED, asset_config.mint.as_ref()],
            asset_config.bump,
            program_id,
        )?;
        require!(asset_config.asset_index == asset_index, VannaError::IncompletePositionAccounts);

        let reserve = Account::<Reserve>::try_from(reserve_info)?;
        verify_pda(
            reserve_info.key,
            &[RESERVE_SEED, asset_config.mint.as_ref()],
            reserve.bump,
            program_id,
        )?;
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

        let price_account = Account::<PriceUpdateV2>::try_from(price_info)?;
        let validated_price = load_validated_price(&asset_config, &price_account, clock)?;
        let debt_value = normalize_token_value(
            current_debt_assets,
            validated_price.price,
            validated_price.exponent,
            asset_config.decimals,
            true,
        )?;

        debts.push(ScannedDebt {
            asset_index,
            reserve: reserve_info.key(),
            current_debt_assets,
            valuation: DebtValuation { debt_value },
        });
    }

    require!(cursor == remaining_accounts.len(), VannaError::IncompletePositionAccounts);
    Ok((collaterals, debts))
}
