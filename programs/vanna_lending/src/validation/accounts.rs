use crate::constants::{CLASSIC_SPL_TOKEN_PROGRAM, TOKEN_2022_PROGRAM};
use crate::errors::VannaError;
use crate::state::asset_config::AssetConfig;
use crate::state::protocol_config::OperatingMode;
use crate::state::reserve::ReserveStatus;
use anchor_lang::prelude::*;

/// Risk-relevant instruction categories, gated by the operating-mode and reserve-status matrices.
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

/// Fails if the reserve status disallows `action`. Non-reserve actions are always allowed.
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

/// Binds the passed mint and token program to the registered asset config.
pub fn validate_asset_config(asset: &AssetConfig, mint: &Pubkey, token_program: &Pubkey) -> Result<()> {
    require_keys_eq!(asset.mint, *mint, VannaError::InvalidMint);
    require_keys_eq!(asset.token_program, *token_program, VannaError::InvalidTokenProgram);
    require!(
        *token_program == CLASSIC_SPL_TOKEN_PROGRAM || *token_program == TOKEN_2022_PROGRAM,
        VannaError::InvalidTokenProgram
    );
    Ok(())
}
