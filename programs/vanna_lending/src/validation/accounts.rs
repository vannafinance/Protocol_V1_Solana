use crate::constants::CLASSIC_SPL_TOKEN_PROGRAM;
use crate::constants::TOKEN_2022_PROGRAM;
use crate::errors::VannaError;
use crate::state::asset_config::AssetConfig;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::OperatingMode;
use crate::state::reserve::{Reserve, ReserveStatus};
use anchor_lang::prelude::*;

/// Every risk-relevant instruction category, used to drive spec §5.1's operating-mode matrix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProtocolAction {
    Supply,
    Redeem,
    CollateralDeposit,
    CollateralWithdraw,
    Borrow,
    Repay,
    Liquidate,
}

/// Spec §5.1 `assert_protocol_action_allowed`.
pub fn assert_protocol_action_allowed(mode_byte: u8, action: ProtocolAction) -> Result<()> {
    let mode = OperatingMode::from_u8(mode_byte).ok_or(VannaError::InvalidOperatingMode)?;
    use OperatingMode::*;
    use ProtocolAction::*;
    let allowed = match mode {
        Normal => true,
        BorrowPaused => action != Borrow,
        WithdrawOnly => matches!(action, Repay | Redeem | CollateralWithdraw | Liquidate | CollateralDeposit),
        Halted => matches!(action, Repay | CollateralDeposit),
    };
    require!(allowed, VannaError::ProtocolActionPaused);
    Ok(())
}

/// Spec §5.2 `assert_reserve_action_allowed`.
pub fn assert_reserve_action_allowed(status_byte: u8, action: ProtocolAction) -> Result<()> {
    let status = ReserveStatus::from_u8(status_byte).ok_or(VannaError::InvalidReserveStatus)?;
    let allowed = match action {
        ProtocolAction::Supply => status.supply_allowed(),
        ProtocolAction::Redeem => status.redeem_allowed(),
        ProtocolAction::Borrow => status.borrow_allowed(),
        ProtocolAction::Repay => status.repay_allowed(),
        _ => true,
    };
    require!(allowed, VannaError::InvalidReserveStatus);
    Ok(())
}

/// Spec §6.6 `validate_asset_config` — binds asset identity and immutable relations.
pub fn validate_asset_config(asset: &AssetConfig, mint: &Pubkey, token_program: &Pubkey) -> Result<()> {
    require_keys_eq!(asset.mint, *mint, VannaError::InvalidMint);
    require_keys_eq!(asset.token_program, *token_program, VannaError::InvalidTokenProgram);
    require!(
        *token_program == CLASSIC_SPL_TOKEN_PROGRAM || *token_program == TOKEN_2022_PROGRAM,
        VannaError::InvalidTokenProgram
    );
    Ok(())
}

/// Spec §6.6 `validate_reserve_accounts` — verifies reserve keys, authorities, and mints.
pub fn validate_reserve_accounts(
    reserve: &Reserve,
    asset_config_key: &Pubkey,
    underlying_mint: &Pubkey,
    liquidity_vault: &Pubkey,
    share_mint: &Pubkey,
) -> Result<()> {
    require_keys_eq!(reserve.asset_config, *asset_config_key, VannaError::InvalidPda);
    require_keys_eq!(reserve.underlying_mint, *underlying_mint, VannaError::InvalidMint);
    require_keys_eq!(reserve.liquidity_vault, *liquidity_vault, VannaError::InvalidVaultAuthority);
    require_keys_eq!(reserve.share_mint, *share_mint, VannaError::InvalidShareMintAuthority);
    Ok(())
}

/// Spec §6.6 `validate_margin_authority` — authorizes an owner-only margin action.
pub fn validate_margin_authority(margin: &MarginAccount, signer: &Pubkey) -> Result<()> {
    require_keys_eq!(margin.authority, *signer, VannaError::Unauthorized);
    Ok(())
}
