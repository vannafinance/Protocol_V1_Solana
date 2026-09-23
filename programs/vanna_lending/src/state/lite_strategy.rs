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

    /// Splitting a same-asset exit into two instructions (`lite_reduce_redeem` then
    /// `lite_reduce_repay`, one transaction) — see those instructions' doc comments for
    /// why — needs somewhere to carry the real, CPI-measured redeemed amount from the
    /// first instruction to the second (the second can't just re-derive it, since the
    /// margin's underlying vault may hold unrelated pre-existing balance too). Reuses
    /// `reserved` bytes 17..=25 (byte 16 is already the `debt_shares` flag above),
    /// preserving the deployed account layout for existing positions — a real position
    /// closing for the first time under the split flow will read byte 25 as 0 (no pending
    /// redeem), which is the correct default. `target_shares`/`attributed_shares` don't
    /// need to be carried across: nothing in between the two instructions (same
    /// transaction, atomic) can change the inputs they're computed from, so
    /// `lite_reduce_repay` just recomputes them fresh from `exit_bps` + live state.
    pub fn set_pending_redeem(&mut self, redeemed: u64) {
        self.reserved[17..25].copy_from_slice(&redeemed.to_le_bytes());
        self.reserved[25] = 1;
    }
    /// Reads and clears the pending redeem set by `set_pending_redeem`. `None` if there
    /// isn't one (e.g. `lite_reduce_repay` called without a preceding `lite_reduce_redeem`
    /// in the same transaction).
    pub fn take_pending_redeem(&mut self) -> Option<u64> {
        if self.reserved[25] != 1 {
            return None;
        }
        let redeemed = u64::from_le_bytes(self.reserved[17..25].try_into().unwrap());
        self.reserved[17..26].fill(0);
        Some(redeemed)
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

    /// Mirrors `lite_supply`'s attribution bump: `current_attributed + attribute_shares_delta`,
    /// capped on read by `.min(outstanding)`. Exercises a fresh position (starts at 0) and a
    /// top-up on top of an already-attributed position.
    #[test]
    fn debt_attribution_accumulates_across_leveraged_supply_top_ups() {
        let mut p = LitePosition {
            margin_account: Pubkey::default(),
            strategy_config: Pubkey::default(),
            underlying_mint: Pubkey::default(),
            kamino_collateral_amount: 0,
            deposited_underlying: 0,
            equity_underlying: 0,
            bump: 0,
            reserved: [0; 64],
        };
        // Fresh position, first leveraged supply attributes exactly the borrowed shares.
        let current = p.debt_shares(1_000);
        p.set_debt_shares(current + 40);
        assert_eq!(p.debt_shares(1_000), 40);

        // A second leveraged supply (top-up) adds on top rather than overwriting.
        let current = p.debt_shares(1_000);
        p.set_debt_shares(current + 25);
        assert_eq!(p.debt_shares(1_000), 65);

        // Attribution is still capped by whatever's actually outstanding on the reserve.
        assert_eq!(p.debt_shares(50), 50);
    }
}
