use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Borrow shares for one margin/reserve pair. Their token value grows as the reserve's
/// `borrow_index_wad` accrues; the share count only changes on borrow/repay.
#[account]
#[derive(InitSpace)]
pub struct DebtPosition {
    pub margin_account: Pubkey,
    pub reserve: Pubkey,
    pub borrow_shares: u128,
    pub bump: u8,
    pub reserved: [u8; 48],
}

impl DebtPosition {
    pub fn credit_shares(&mut self, shares: u128) -> Result<()> {
        self.borrow_shares = self.borrow_shares.checked_add(shares).ok_or(VannaError::MathOverflow)?;
        Ok(())
    }

    pub fn debit_shares(&mut self, shares: u128) -> Result<()> {
        self.borrow_shares = self.borrow_shares.checked_sub(shares).ok_or(VannaError::MathUnderflow)?;
        Ok(())
    }
}
