use anchor_lang::prelude::*;

#[constant]
pub const PROTOCOL_SEED: &[u8] = b"protocol";
#[constant]
pub const ASSET_SEED: &[u8] = b"asset";
#[constant]
pub const RESERVE_SEED: &[u8] = b"reserve";
#[constant]
pub const SHARE_MINT_SEED: &[u8] = b"share_mint";
#[constant]
pub const MARGIN_SEED: &[u8] = b"margin";
#[constant]
pub const DEBT_SEED: &[u8] = b"debt";

/// Maximum number of distinct active collateral (or debt) assets a single margin account may hold.
/// Bounds the canonical index arrays so every health check has a predictable, fixed cost.
pub const MAX_ASSETS: usize = 8;

/// Fixed-point scale used for `borrow_index_wad` and reported health-factor ratios.
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// Minimum collateral-to-debt ratio required after a borrow or collateral withdrawal.
///
/// This deliberately mirrors the canonical Solidity `balanceToBorrowThreshold` and
/// Soroban `BALANCE_TO_BORROW_THRESHOLD`: an account is healthy only when
/// `total_collateral_value / total_debt_value > 1.10`. Equality is unhealthy.
pub const BALANCE_TO_BORROW_THRESHOLD_WAD: u128 = 1_100_000_000_000_000_000;

/// Decimal precision used for internal USD valuations (nano-USD: 1e9 per US dollar).
/// Deliberately smaller than WAD so collateral/debt value sums stay far from u128 overflow
/// while retaining far more precision than any supported token's decimals or Pyth exponent.
pub const USD_VALUE_DECIMALS: u32 = 9;

pub const BASIS_POINTS: u64 = 10_000;

pub const SECONDS_PER_YEAR: i64 = 31_536_000;

/// Minimum shares required from the first supply into a reserve, guarding against a
/// first-depositor share-inflation attack on a freshly initialized pool.
pub const MIN_INITIAL_SHARES: u64 = 1_000;

/// Sentinel `supply_cap`/`borrow_cap` value meaning "no cap enforced".
pub const UNCAPPED: u64 = 0;

/// Illustrative V1 liquidation close factor (50%) — spec §6.5/§7.7 explicitly flags this as a
/// value requiring simulation and audit before production, not a frozen protocol parameter.
/// Kept as a compiled constant for now since `Reserve`/`ProtocolConfig` (spec §4) do not define a
/// governed field for it; promote it to a governed config field before raising launch caps.
pub const CLOSE_FACTOR_BPS: u64 = 5_000;

/// Classic SPL Token Program ID. The first release accepts only this token program;
/// Token-2022 mints are rejected until every relevant extension has been reviewed.
pub const CLASSIC_SPL_TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;

/// Reference mint addresses (not enforced on-chain — any mint can be registered by the admin).
/// Kept here so client scripts and tests have one canonical source for the recommended V1 markets.
pub mod known_mints {
    /// Circle's official Devnet USDC mint (6 decimals).
    pub const DEVNET_USDC: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    /// Circle's official Mainnet USDC mint (6 decimals).
    pub const MAINNET_USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    /// Native WSOL mint, identical on every cluster (9 decimals).
    pub const WSOL: &str = "So11111111111111111111111111111111111111112";
}
