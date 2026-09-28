use crate::adapters::AdapterKind;
use anchor_lang::prelude::*;

/// Whitelist entry for one external program a margin account may call.
/// Seeds: `[b"integration", program_id]`.
///
/// `adapter` picks the compiled-in validator that decides which of that program's instructions
/// are allowed and which margin vaults they spend from and credit to.
#[account]
#[derive(InitSpace)]
pub struct Integration {
    pub program_id: Pubkey,
    pub adapter: AdapterKind,
    pub enabled: bool,
    pub bump: u8,
    pub reserved: [u8; 64],
}
