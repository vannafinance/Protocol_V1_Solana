use anchor_lang::prelude::*;

/// Protocol-wide operating mode, stored as `ProtocolConfig::operating_mode`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OperatingMode {
    Normal = 0,
    BorrowPaused = 1,
    WithdrawOnly = 2,
    Halted = 3,
}

impl OperatingMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Normal),
            1 => Some(Self::BorrowPaused),
            2 => Some(Self::WithdrawOnly),
            3 => Some(Self::Halted),
            _ => None,
        }
    }
}

/// Global administration and emergency state. Never written by normal user transactions, so it
/// never becomes a write-lock bottleneck.
#[account]
#[derive(InitSpace)]
pub struct ProtocolConfig {
    pub admin: Pubkey,
    pub pending_admin: Pubkey,
    pub treasury: Pubkey,
    pub operating_mode: u8,
    pub max_assets_per_margin: u8,
    pub next_asset_index: u16,
    pub bump: u8,
    pub reserved: [u8; 128],
}
