use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct Integration {
    pub program_id: Pubkey,
    pub validator: Pubkey,
    pub enabled: bool,
    pub bump: u8,
    pub reserved: [u8; 64],
}
