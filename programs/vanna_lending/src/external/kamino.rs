//! Hand-rolled CPIs into Kamino klend.
//!
//! Account order differs between deposit and redeem (see KAMINO_INTEGRATION_REFERENCE.md §6).
//! No Kamino SDK — Anchor discriminators + explicit account metas.

use crate::constants::kamino_discriminators;
use crate::errors::VannaError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};

/// CPI accounts for Kamino deposit/redeem. Unused legs may point at the same infos.
#[derive(Clone, Copy)]
pub struct KaminoCpiAccounts<'a, 'info> {
    pub klend_program: &'a AccountInfo<'info>,
    pub owner: &'a AccountInfo<'info>,
    pub lending_market: &'a AccountInfo<'info>,
    pub lending_market_authority: &'a AccountInfo<'info>,
    pub reserve: &'a AccountInfo<'info>,
    pub reserve_liquidity_mint: &'a AccountInfo<'info>,
    pub reserve_liquidity_supply: &'a AccountInfo<'info>,
    pub reserve_collateral_mint: &'a AccountInfo<'info>,
    pub user_liquidity_account: &'a AccountInfo<'info>,
    pub user_collateral_account: &'a AccountInfo<'info>,
    pub collateral_token_program: &'a AccountInfo<'info>,
    pub liquidity_token_program: &'a AccountInfo<'info>,
    pub instruction_sysvar: &'a AccountInfo<'info>,
}

// BPF gives each stack frame a fixed 4KB budget. If this gets inlined into a caller that
// already has a large frame of its own (e.g. `lite_reduce`, with its own several locals),
// the combined frame can exceed that budget and crash with "Access violation in stack
// frame N" — a real, live failure seen on `lite_reduce` (confirmed via a genuine on-chain
// simulation, not a test). `#[inline(never)]` forces this into its own frame instead of
// being folded into the caller's, which is the standard fix for this BPF failure class.
#[inline(never)]
fn account_infos<'a, 'info>(accounts: &KaminoCpiAccounts<'a, 'info>) -> [AccountInfo<'info>; 13] {
    [
        accounts.owner.clone(),
        accounts.reserve.clone(),
        accounts.lending_market.clone(),
        accounts.lending_market_authority.clone(),
        accounts.reserve_liquidity_mint.clone(),
        accounts.reserve_liquidity_supply.clone(),
        accounts.reserve_collateral_mint.clone(),
        accounts.user_liquidity_account.clone(),
        accounts.user_collateral_account.clone(),
        accounts.collateral_token_program.clone(),
        accounts.liquidity_token_program.clone(),
        accounts.instruction_sysvar.clone(),
        accounts.klend_program.clone(),
    ]
}

/// `deposit_reserve_liquidity` — §6 left column. `owner` must sign — either a real wallet
/// (pass an empty `signer_seeds`) or a PDA like the margin account (pass its `signer_seeds`,
/// mirroring `redeem_reserve_collateral` below).
#[inline(never)]
pub fn deposit_reserve_liquidity<'info>(
    accounts: &KaminoCpiAccounts<'_, 'info>,
    liquidity_amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require!(liquidity_amount > 0, VannaError::ZeroAmount);

    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&kamino_discriminators::DEPOSIT_RESERVE_LIQUIDITY);
    data.extend_from_slice(&liquidity_amount.to_le_bytes());

    let metas = vec![
        AccountMeta::new(*accounts.owner.key, true),
        AccountMeta::new(*accounts.reserve.key, false),
        AccountMeta::new_readonly(*accounts.lending_market.key, false),
        AccountMeta::new_readonly(*accounts.lending_market_authority.key, false),
        AccountMeta::new_readonly(*accounts.reserve_liquidity_mint.key, false),
        AccountMeta::new(*accounts.reserve_liquidity_supply.key, false),
        AccountMeta::new(*accounts.reserve_collateral_mint.key, false),
        AccountMeta::new(*accounts.user_liquidity_account.key, false),
        AccountMeta::new(*accounts.user_collateral_account.key, false),
        AccountMeta::new_readonly(*accounts.collateral_token_program.key, false),
        AccountMeta::new_readonly(*accounts.liquidity_token_program.key, false),
        AccountMeta::new_readonly(*accounts.instruction_sysvar.key, false),
    ];

    let ix = Instruction {
        program_id: *accounts.klend_program.key,
        accounts: metas,
        data,
    };
    invoke_signed(&ix, &account_infos(accounts), signer_seeds)?;
    Ok(())
}

/// `redeem_reserve_collateral` — §6 right column. `owner` may be a PDA (`signer_seeds`).
#[inline(never)]
pub fn redeem_reserve_collateral<'info>(
    accounts: &KaminoCpiAccounts<'_, 'info>,
    collateral_amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require!(collateral_amount > 0, VannaError::ZeroAmount);

    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&kamino_discriminators::REDEEM_RESERVE_COLLATERAL);
    data.extend_from_slice(&collateral_amount.to_le_bytes());

    let metas = vec![
        AccountMeta::new(*accounts.owner.key, true),
        AccountMeta::new_readonly(*accounts.lending_market.key, false),
        AccountMeta::new(*accounts.reserve.key, false),
        AccountMeta::new_readonly(*accounts.lending_market_authority.key, false),
        AccountMeta::new_readonly(*accounts.reserve_liquidity_mint.key, false),
        AccountMeta::new(*accounts.reserve_collateral_mint.key, false),
        AccountMeta::new(*accounts.reserve_liquidity_supply.key, false),
        AccountMeta::new(*accounts.user_collateral_account.key, false),
        AccountMeta::new(*accounts.user_liquidity_account.key, false),
        AccountMeta::new_readonly(*accounts.collateral_token_program.key, false),
        AccountMeta::new_readonly(*accounts.liquidity_token_program.key, false),
        AccountMeta::new_readonly(*accounts.instruction_sysvar.key, false),
    ];

    let ix = Instruction {
        program_id: *accounts.klend_program.key,
        accounts: metas,
        data,
    };
    invoke_signed(&ix, &account_infos(accounts), signer_seeds)?;
    Ok(())
}

/// Current underlying claim from klend's zero-copy Reserve layout. Values use
/// Fraction (60 fractional bits). Validate the layout's mint identities as well
/// as the account owner before reading financial fields. Round against collateral.
pub fn receipt_value(
    reserve: &AccountInfo,
    program: &Pubkey,
    underlying: &Pubkey,
    collateral_mint: &Pubkey,
    receipts: u64,
) -> Result<u64> {
    require_keys_eq!(*reserve.owner, *program, VannaError::InvalidKaminoAccounts);
    let data = reserve.try_borrow_data()?;
    require!(data.len() >= 2600, VannaError::InvalidKaminoAccounts);
    require!(
        data[..8] == [43, 242, 204, 202, 26, 247, 59, 127],
        VannaError::InvalidKaminoAccounts
    );
    require!(
        data[128..160] == underlying.to_bytes(),
        VannaError::InvalidKaminoAccounts
    );
    require!(
        data[2560..2592] == collateral_mint.to_bytes(),
        VannaError::InvalidKaminoAccounts
    );
    let u64_at = |i| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let u128_at = |i| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    let supply = u64_at(2592);
    if receipts == 0 {
        return Ok(0);
    }
    require!(supply > 0, VannaError::InvalidKaminoAccounts);
    let total_sf = ((u64_at(224) as u128) << 60)
        .checked_add(u128_at(232))
        .ok_or(VannaError::MathOverflow)?
        .checked_sub(u128_at(344))
        .ok_or(VannaError::MathUnderflow)?
        .checked_sub(u128_at(360))
        .ok_or(VannaError::MathUnderflow)?
        .checked_sub(u128_at(376))
        .ok_or(VannaError::MathUnderflow)?;
    let value =
        crate::math::fixed_point::mul_div_floor(receipts as u128, total_sf >> 60, supply as u128)?;
    u64::try_from(value).map_err(|_| VannaError::MathOverflow.into())
}
