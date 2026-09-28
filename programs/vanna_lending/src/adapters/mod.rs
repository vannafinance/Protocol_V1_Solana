//! Adapters decide which external instructions a margin account may execute.
//!
//! Each adapter inspects the instruction data (its 8-byte discriminator is the "function
//! selector") and the CPI account keys, and returns a [`CallPlan`]: which account signs as the
//! margin, which margin vault the call spends from, which margin vault it credits, and how much it
//! may spend. `margin_execute` enforces the plan, runs the CPI, then re-checks health.

pub mod jupiter;
pub mod kamino;

use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    /// Kamino klend: `deposit_reserve_liquidity` and `redeem_reserve_collateral`.
    KaminoLend,
    /// Jupiter v6 aggregator: exact-input `route` and `shared_accounts_route`.
    Jupiter,
}

/// One margin vault a call touches, as indexes into the CPI accounts. `mint` is `None` when the
/// instruction takes no mint account for that side; the vault's own ATA derivation pins the mint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenLeg {
    pub vault: usize,
    pub mint: Option<usize>,
}

/// An allowed call: the margin signs at `authority`, at most `max_spent` leaves `spent`, and the
/// output arrives in `received`. `price_source` is the account that prices a receipt leg (e.g. the
/// Kamino reserve); it must be the one registered on that receipt asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallPlan {
    pub authority: usize,
    pub spent: TokenLeg,
    pub received: TokenLeg,
    pub price_source: Option<usize>,
    pub max_spent: u64,
}

/// `program_id` is the integration's program; `accounts` are the CPI account keys in order.
pub fn plan_call(adapter: AdapterKind, program_id: &Pubkey, data: &[u8], accounts: &[Pubkey]) -> Result<CallPlan> {
    match adapter {
        AdapterKind::KaminoLend => kamino::plan_call(data, accounts.len()),
        AdapterKind::Jupiter => jupiter::plan_call(program_id, data, accounts),
    }
}
