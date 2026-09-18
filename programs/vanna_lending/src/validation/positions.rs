use crate::constants::{
    ASSET_SEED, DEBT_SEED, LITE_POSITION_SEED, LITE_STRATEGY_SEED, RESERVE_SEED,
};
use crate::errors::VannaError;
use crate::external::kamino;
use crate::math::health::{normalize_token_value, CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::lite_strategy::{LitePosition, LiteStrategyConfig};
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
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
    let derived = Pubkey::create_program_address(&full_seeds, program_id)
        .map_err(|_| VannaError::InvalidBump)?;
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
    named_lite: Option<(Pubkey, Pubkey)>,
) -> Result<(Vec<ScannedCollateral>, Vec<ScannedDebt>)> {
    let mut cursor = 0usize;
    let mut collaterals = Vec::with_capacity(margin.collateral_count as usize);
    let mut debts = Vec::with_capacity(margin.debt_count as usize);

    for asset_index in margin.active_collateral_indexes() {
        if Some(asset_index) == named_collateral_index {
            continue;
        }
        require!(
            cursor + 3 <= remaining_accounts.len(),
            VannaError::IncompletePositionAccounts
        );
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
        require!(
            asset_config.asset_index == asset_index,
            VannaError::IncompletePositionAccounts
        );

        // InterfaceAccount accepts classic SPL and Token-2022 token-account sizes.
        let vault = InterfaceAccount::<TokenAccount>::try_from(vault_info)?;
        verify_associated_token_account(
            vault_info.key,
            margin_key,
            &asset_config.mint,
            &asset_config.token_program,
        )?;
        require_keys_eq!(
            vault.owner,
            *margin_key,
            VannaError::IncompletePositionAccounts
        );
        require_keys_eq!(
            vault.mint,
            asset_config.mint,
            VannaError::IncompletePositionAccounts
        );

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
            valuation: CollateralValuation { collateral_value },
        });
    }

    for asset_index in margin.active_debt_indexes() {
        if Some(asset_index) == named_debt_index {
            continue;
        }
        require!(
            cursor + 4 <= remaining_accounts.len(),
            VannaError::IncompletePositionAccounts
        );
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
        require!(
            asset_config.asset_index == asset_index,
            VannaError::IncompletePositionAccounts
        );

        let reserve = Account::<Reserve>::try_from(reserve_info)?;
        verify_pda(
            reserve_info.key,
            &[RESERVE_SEED, asset_config.mint.as_ref()],
            reserve.bump,
            program_id,
        )?;
        require_keys_eq!(
            reserve.asset_config,
            asset_info.key(),
            VannaError::IncompletePositionAccounts
        );
        require_keys_eq!(
            asset_config.reserve,
            reserve_info.key(),
            VannaError::IncompletePositionAccounts
        );

        let debt_position = Account::<DebtPosition>::try_from(debt_position_info)?;
        verify_pda(
            debt_position_info.key,
            &[DEBT_SEED, margin_key.as_ref(), reserve_info.key.as_ref()],
            debt_position.bump,
            program_id,
        )?;
        require_keys_eq!(
            debt_position.margin_account,
            *margin_key,
            VannaError::IncompletePositionAccounts
        );
        require_keys_eq!(
            debt_position.reserve,
            *reserve_info.key,
            VannaError::IncompletePositionAccounts
        );

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

    let mut positions = vec![(Pubkey::find_program_address(&[LITE_POSITION_SEED, margin_key.as_ref()], program_id).0, None)];
    for index in margin.lite_indexes()? {
        positions.push((Pubkey::find_program_address(&[LITE_POSITION_SEED, margin_key.as_ref(), &index.to_le_bytes()], program_id).0, Some(index)));
    }
    let mut seen_mints = Vec::new();
    if let Some((_, mint)) = named_lite { seen_mints.push(mint); }
    for (expected, index) in positions {
        if named_lite.map(|p| p.0) == Some(expected) { continue; }
        let (receipt, consumed) = scan_lite_collateral(margin_key, &remaining_accounts[cursor..], program_id, clock, &expected, index)?;
        cursor += consumed;
        if let Some(receipt) = receipt {
            // A stock has exactly one strategy position, including legacy accounts.
            require!(!seen_mints.contains(&receipt.mint), VannaError::DuplicateAssetIndex);
            seen_mints.push(receipt.mint);
            collaterals.push(receipt);
        }
    }

    require!(
        cursor == remaining_accounts.len(),
        VannaError::IncompletePositionAccounts
    );
    Ok((collaterals, debts))
}

/// Mandatory canonical proof (including an empty PDA for accounts without Lite)
/// prevents external collateral omission during liquidation. Separate SBF frame.
#[inline(never)]
fn scan_lite_collateral<'info>(
    margin_key: &Pubkey,
    remaining_accounts: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
    clock: &Clock,
    expected: &Pubkey,
    expected_index: Option<u16>,
) -> Result<(Option<ScannedCollateral>, usize)> {
    let mut cursor = 0;
    let mut result = None;
    require!(
        cursor < remaining_accounts.len(),
        VannaError::IncompletePositionAccounts
    );
    let position_info = &remaining_accounts[cursor];
    cursor += 1;
    require_keys_eq!(*expected, *position_info.key, VannaError::InvalidPda);
    if position_info.owner == program_id {
        require!(
            cursor + 5 <= remaining_accounts.len(),
            VannaError::IncompletePositionAccounts
        );
        let position = Box::new(Account::<LitePosition>::try_from(position_info)?);
        require_keys_eq!(
            position.margin_account,
            *margin_key,
            VannaError::IncompletePositionAccounts
        );
        let strategy_info = &remaining_accounts[cursor];
        let asset_info = &remaining_accounts[cursor + 1];
        let receipt_info = &remaining_accounts[cursor + 2];
        let reserve_info = &remaining_accounts[cursor + 3];
        let price_info = &remaining_accounts[cursor + 4];
        cursor += 5;
        let strategy = Box::new(Account::<LiteStrategyConfig>::try_from(strategy_info)?);
        verify_pda(
            strategy_info.key,
            &[LITE_STRATEGY_SEED, position.underlying_mint.as_ref()],
            strategy.bump,
            program_id,
        )?;
        require_keys_eq!(
            position.strategy_config,
            *strategy_info.key,
            VannaError::InvalidKaminoAccounts
        );
        require_keys_eq!(
            strategy.underlying_mint,
            position.underlying_mint,
            VannaError::InvalidKaminoAccounts
        );
        require_keys_eq!(
            strategy.kamino_reserve,
            *reserve_info.key,
            VannaError::InvalidKaminoAccounts
        );
        let asset = Box::new(Account::<AssetConfig>::try_from(asset_info)?);
        verify_pda(
            asset_info.key,
            &[ASSET_SEED, position.underlying_mint.as_ref()],
            asset.bump,
            program_id,
        )?;
        require_keys_eq!(
            asset.mint,
            position.underlying_mint,
            VannaError::InvalidKaminoAccounts
        );
        require_keys_eq!(
            strategy.asset_config,
            *asset_info.key,
            VannaError::InvalidKaminoAccounts
        );
        if let Some(index) = expected_index {
            require!(asset.asset_index == index, VannaError::IncompletePositionAccounts);
        }
        let receipt = InterfaceAccount::<TokenAccount>::try_from(receipt_info)?;
        verify_associated_token_account(
            receipt_info.key,
            margin_key,
            &strategy.reserve_collateral_mint,
            &strategy.collateral_token_program,
        )?;
        require_keys_eq!(
            *receipt_info.owner,
            strategy.collateral_token_program,
            VannaError::InvalidTokenProgram
        );
        require_keys_eq!(
            receipt.owner,
            *margin_key,
            VannaError::InvalidKaminoAccounts
        );
        require_keys_eq!(
            receipt.mint,
            strategy.reserve_collateral_mint,
            VannaError::InvalidKaminoAccounts
        );
        require!(
            receipt.amount >= position.kamino_collateral_amount,
            VannaError::VaultAccountingInvariantFailed
        );
        let price_account = Account::<PriceUpdateV2>::try_from(price_info)?;
        let price = load_validated_price(&asset, &price_account, clock)?;
        let underlying = kamino::receipt_value(
            reserve_info,
            &strategy.kamino_program,
            &position.underlying_mint,
            &strategy.reserve_collateral_mint,
            position.kamino_collateral_amount,
        )?;
        result = Some(ScannedCollateral {
            asset_index: asset.asset_index,
            mint: asset.mint,
            valuation: CollateralValuation {
                collateral_value: normalize_token_value(
                    underlying,
                    price.price,
                    price.exponent,
                    asset.decimals,
                    false,
                )?,
            },
        });
    } else {
        require!(expected_index.is_none(), VannaError::IncompletePositionAccounts);
        require_keys_eq!(
            *position_info.owner,
            anchor_lang::system_program::ID,
            VannaError::IncompletePositionAccounts
        );
        require!(
            position_info.data_is_empty(),
            VannaError::IncompletePositionAccounts
        );
    }
    Ok((result, cursor))
}
