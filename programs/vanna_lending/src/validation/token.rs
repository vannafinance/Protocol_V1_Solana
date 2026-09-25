use crate::errors::VannaError;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

/// Gross transfer amount such that the recipient receives `net_amount` after `mint`'s
/// Token-2022 `TransferFeeConfig` fee for the current epoch. Identity for classic SPL mints
/// and Token-2022 mints without the extension.
///
/// Repays burn debt shares against what the vault actually received, so with a fee-bearing
/// mint (e.g. the 1% PreStocks) transferring exactly the debt would leave part of it open.
/// Grossing up by the inverse fee lets a full repay clear the debt.
pub fn gross_up_for_transfer_fee(mint_ai: &AccountInfo, token_program: &Pubkey, net_amount: u64) -> Result<u64> {
    use anchor_spl::token_2022::spl_token_2022::{
        self,
        extension::{transfer_fee::TransferFeeConfig, BaseStateWithExtensions, StateWithExtensions},
    };
    if *token_program != anchor_spl::token_2022::ID {
        return Ok(net_amount);
    }
    let data = mint_ai.try_borrow_data()?;
    let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
        .map_err(|_| VannaError::InvalidMint)?;
    match mint_state.get_extension::<TransferFeeConfig>() {
        Ok(fee_config) => {
            let epoch = Clock::get()?.epoch;
            let fee = fee_config
                .calculate_inverse_epoch_fee(epoch, net_amount)
                .ok_or(VannaError::MathOverflow)?;
            Ok(net_amount.checked_add(fee).ok_or(VannaError::MathOverflow)?)
        }
        Err(_) => Ok(net_amount),
    }
}

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

/// Transfers `amount` in and returns the destination's measured balance delta, which is the
/// amount actually received under Token-2022 transfer fees.
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

/// PDA-signed transfer out; returns the destination's measured balance delta.
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

/// PDA-signed transfer out without measuring the received amount.
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
