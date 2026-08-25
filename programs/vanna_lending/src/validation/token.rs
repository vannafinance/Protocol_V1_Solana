use crate::errors::VannaError;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};

/// Confirms `actual` is genuinely *the* Associated Token Account for `(owner, mint)` under the
/// classic SPL Token Program — not merely some other token account that happens to share the same
/// mint/authority. Margin collateral vaults are plain ATAs (no PDA of our own protects them), so
/// every instruction and the `remaining_accounts` scan (`validation/positions.rs`) must pin the
/// exact address down explicitly; otherwise a caller could substitute a different token account
/// they also control for the same margin/mint pair and desynchronize it from the one every other
/// instruction actually uses.
pub fn verify_associated_token_account(actual: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Result<()> {
    let expected = get_associated_token_address_with_program_id(owner, mint, &crate::constants::CLASSIC_SPL_TOKEN_PROGRAM);
    require_keys_eq!(*actual, expected, VannaError::InvalidVaultAuthority);
    Ok(())
}

/// Spec §6.6 `transfer_in_measured` — transfers `amount` from a user-authorized source into a
/// protocol-owned vault, then measures the *actual* delta from a reload rather than trusting the
/// requested amount. Under the classic SPL Token Program the delta always equals `amount`, but the
/// measurement is still performed so this helper (and every caller) stays correct if a
/// fee-charging token variant is ever admitted.
pub fn transfer_in_measured<'info>(
    token_program: &Program<'info, Token>,
    mint: &Account<'info, Mint>,
    from: &Account<'info, TokenAccount>,
    to: &mut Account<'info, TokenAccount>,
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

/// Like `transfer_out_checked`, but measures the destination's actual balance delta instead of
/// trusting the requested `amount` — used wherever the delta itself drives protocol accounting
/// (e.g. reducing recorded debt by exactly what a reserve vault received).
#[allow(clippy::too_many_arguments)]
pub fn transfer_out_checked_measured<'info>(
    token_program: &Program<'info, Token>,
    mint: &Account<'info, Mint>,
    from: &Account<'info, TokenAccount>,
    to: &mut Account<'info, TokenAccount>,
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

/// Spec §6.6 `transfer_out_checked` — a PDA-signed exact-output transfer out of a protocol vault.
#[allow(clippy::too_many_arguments)]
pub fn transfer_out_checked<'info>(
    token_program: &Program<'info, Token>,
    mint: &Account<'info, Mint>,
    from: &Account<'info, TokenAccount>,
    to: &Account<'info, TokenAccount>,
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
