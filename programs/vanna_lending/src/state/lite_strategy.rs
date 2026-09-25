use anchor_lang::prelude::*;

/// Admin-registered mapping from a Vanna underlying mint to one Kamino reserve.
/// Seeds: `[b"lite_strategy", underlying_mint]`.
#[account]
#[derive(InitSpace)]
pub struct LiteStrategyConfig {
    pub underlying_mint: Pubkey,
    pub asset_config: Pubkey,
    pub kamino_program: Pubkey,
    pub lending_market: Pubkey,
    pub lending_market_authority: Pubkey,
    pub kamino_reserve: Pubkey,
    pub reserve_liquidity_supply: Pubkey,
    pub reserve_collateral_mint: Pubkey,
    pub liquidity_token_program: Pubkey,
    /// Kamino receipt (cToken) mint program — always classic SPL in practice.
    pub collateral_token_program: Pubkey,
    pub enabled: bool,
    pub bump: u8,
    pub reserved: [u8; 64],
}

/// One leveraged Kamino carry position for a margin account.
/// Seeds: `[b"lite_position", margin_account]`.
#[account]
#[derive(InitSpace)]
pub struct LitePosition {
    pub margin_account: Pubkey,
    pub strategy_config: Pubkey,
    pub underlying_mint: Pubkey,
    /// Kamino cToken amount held in the margin-owned receipt ATA.
    pub kamino_collateral_amount: u64,
    /// Remaining underlying cost basis across deposits and proportional exits.
    pub deposited_underlying: u64,
    /// Remaining contributed equity across deposits and proportional exits.
    pub equity_underlying: u64,
    pub bump: u8,
    pub reserved: [u8; 64],
}

impl LitePosition {
    /// Debt shares attributed to this position, capped at `outstanding`.
    ///
    /// Stored in `reserved[..16]` with flag byte 16 to preserve the deployed layout. Legacy
    /// positions predate attribution and conservatively claim all outstanding debt if leveraged.
    pub fn debt_shares(&self, outstanding: u128) -> u128 {
        if self.reserved[16] == 1 {
            u128::from_le_bytes(self.reserved[..16].try_into().unwrap()).min(outstanding)
        } else if self.deposited_underlying > self.equity_underlying {
            outstanding
        } else {
            0
        }
    }

    pub fn set_debt_shares(&mut self, shares: u128) {
        self.reserved[..16].copy_from_slice(&shares.to_le_bytes());
        self.reserved[16] = 1;
    }

    /// Records the CPI-measured amount from `lite_reduce_redeem` for `lite_reduce_repay`.
    ///
    /// The repay step can't re-derive it: the margin's underlying vault may also hold unrelated
    /// balance. Stored in `reserved[17..25]` with flag byte 25; existing positions read byte 25
    /// as 0 (no pending redeem), which is the correct default.
    pub fn set_pending_redeem(&mut self, redeemed: u64) {
        self.reserved[17..25].copy_from_slice(&redeemed.to_le_bytes());
        self.reserved[25] = 1;
    }

    /// Reads and clears the pending redeem; `None` if `lite_reduce_redeem` didn't run first.
    pub fn take_pending_redeem(&mut self) -> Option<u64> {
        if self.reserved[25] != 1 {
            return None;
        }
        let redeemed = u64::from_le_bytes(self.reserved[17..25].try_into().unwrap());
        self.reserved[17..26].fill(0);
        Some(redeemed)
    }
}
