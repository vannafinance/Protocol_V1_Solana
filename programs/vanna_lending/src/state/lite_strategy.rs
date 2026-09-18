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
    /// Token program of the underlying mint (classic or Token-2022).
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
    /// Versioned attribution stored in reserved bytes; preserves deployed layout.
    /// Legacy positions predate attribution and conservatively cover their stock debt.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debt_attribution_preserves_layout_and_excludes_other_borrows() {
        let mut p = LitePosition {
            margin_account: Pubkey::default(),
            strategy_config: Pubkey::default(),
            underlying_mint: Pubkey::default(),
            kamino_collateral_amount: 40,
            deposited_underlying: 40,
            equity_underlying: 20,
            bump: 0,
            reserved: [0; 64],
        };
        assert_eq!(p.debt_shares(30), 30); // legacy conservative debt
        p.set_debt_shares(20);
        assert_eq!(p.debt_shares(30), 20);
        assert_eq!(p.debt_shares(10), 10); // debt repaid externally
        p.set_debt_shares(0);
        assert_eq!(p.debt_shares(30), 0);
    }
}
