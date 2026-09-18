use crate::errors::VannaError;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

/// Confirms `actual` is the Associated Token Account for `(owner, mint)` under `token_program`.
pub fn verify_associated_token_account(
    actual: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<()> {
    let expected = get_associated_token_address_with_program_id(owner, mint, token_program);
    require_keys_eq!(*actual, expected, VannaError::InvalidVaultAuthority);
    Ok(())
}

/// Spec §6.6 `transfer_in_measured` — transfers then measures the vault delta
/// (correct under Token-2022 transfer-fee extensions).
pub fn transfer_in_measured<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    from: &InterfaceAccount<'info, TokenAccount>,
    to: &mut InterfaceAccount<'info, TokenAccount>,
    authority: &Signer<'info>,
    amount: u64,
) -> Result<u64> {
    require!(amount > 0, VannaError::ZeroAmount);
    let balance_before = to.amount;

    let cpi_accounts = TransferChecked {
        from: from.to_account_info(),
        mint: mint.to_account_info(),
        to: to.to_account_info(),
        authority: authority.to_account_info(),
    };
    transfer_checked(
        CpiContext::new(token_program.key(), cpi_accounts),
        amount,
        mint.decimals,
    )?;

    to.reload()?;
    let balance_after = to.amount;
    balance_after
        .checked_sub(balance_before)
        .ok_or_else(|| VannaError::MathUnderflow.into())
}

#[allow(clippy::too_many_arguments)]
pub fn transfer_out_checked_measured<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    from: &InterfaceAccount<'info, TokenAccount>,
    to: &mut InterfaceAccount<'info, TokenAccount>,
    authority: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    amount: u64,
) -> Result<u64> {
    require!(amount > 0, VannaError::ZeroAmount);
    let balance_before = to.amount;

    let cpi_accounts = TransferChecked {
        from: from.to_account_info(),
        mint: mint.to_account_info(),
        to: to.to_account_info(),
        authority: authority.clone(),
    };
    transfer_checked(
        CpiContext::new_with_signer(token_program.key(), cpi_accounts, signer_seeds),
        amount,
        mint.decimals,
    )?;

    to.reload()?;
    let balance_after = to.amount;
    balance_after
        .checked_sub(balance_before)
        .ok_or_else(|| VannaError::MathUnderflow.into())
}

#[allow(clippy::too_many_arguments)]
pub fn transfer_out_checked<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    from: &InterfaceAccount<'info, TokenAccount>,
    to: &InterfaceAccount<'info, TokenAccount>,
    authority: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    require!(amount > 0, VannaError::ZeroAmount);

    let cpi_accounts = TransferChecked {
        from: from.to_account_info(),
        mint: mint.to_account_info(),
        to: to.to_account_info(),
        authority: authority.clone(),
    };
    transfer_checked(
        CpiContext::new_with_signer(token_program.key(), cpi_accounts, signer_seeds),
        amount,
        mint.decimals,
    )?;
    Ok(())
}
