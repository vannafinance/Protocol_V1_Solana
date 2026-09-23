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
/// Raised from 8 -> 24: `MarginAccount`'s own `#[derive(InitSpace)]` grows the account's
/// allocated size accordingly for newly created accounts (an extra 64 bytes of rent — trivial).
/// This changes the struct's byte layout, though: any margin account created under the old
/// MAX_ASSETS=8 layout has zeroed (not `EMPTY_ASSET_INDEX`-filled) bytes where the new index
/// slots now live, which would misread as "asset index 0 active". Redeploying this requires a
/// fresh margin-account state (a full fork/devnet rebootstrap), not an in-place upgrade of
/// existing accounts.
pub const MAX_ASSETS: usize = 24;

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

/// Classic SPL Token Program ID (share mints always use this).
pub const CLASSIC_SPL_TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;

/// Token-2022 program — required for xStocks (TSLAx, GOOGLx, …).
pub const TOKEN_2022_PROGRAM: Pubkey = anchor_spl::token_2022::ID;

pub const LITE_STRATEGY_SEED: &[u8] = b"lite_strategy";
pub const LITE_POSITION_SEED: &[u8] = b"lite_position";

/// Reference mint addresses (not enforced on-chain — any mint can be registered by the admin).
pub mod known_mints {
    /// Circle's official Mainnet USDC mint (6 decimals) — used on Surfpool mainnet fork.
    pub const MAINNET_USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    /// Native WSOL mint, identical on every cluster (9 decimals).
    pub const WSOL: &str = "So11111111111111111111111111111111111111112";
    /// Tesla xStock (Token-2022, 8 decimals).
    pub const TSLAX: &str = "XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB";
    /// Alphabet xStock (Token-2022, 8 decimals).
    pub const GOOGLX: &str = "XsCPL9dNWBMvFtTmwcCA5v3xWPSMEBCszbQdiLLq6aN";
    /// Anthropic PreStock (prestocks.com, Token-2022, 9 decimals). No real Pyth feed exists.
    pub const ANTHROPIC: &str = "Pren1FvFX6J3E4kXhJuCiAD5aDmGEb7qJRncwA8Lkhw";
    /// OpenAI PreStock (prestocks.com, Token-2022, 9 decimals). No real Pyth feed exists.
    pub const OPENAI: &str = "PreweJYECqtQwBtpxHL171nL2K6umo692gTm7Q3rpgF";
}

/// Kamino xStocks market addresses — reference only; live source of truth is LiteStrategyConfig.
pub mod kamino_reference {
    pub const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
    pub const XSTOCKS_MARKET: &str = "5wJeMrUYECGq41fxRESKALVcHnNX26TAWy4W98yULsua";
    pub const MARKET_AUTHORITY: &str = "2Z7zhqp1eddmHNmEqexftST6DFPWmoL4QqfgiG5uJMJx";

    pub mod tslax {
        pub const RESERVE: &str = "5iTiczqgUegqA3PpoNpotizMbY9n1sRWr3oL6igKvWuf";
        pub const LIQUIDITY_VAULT: &str = "AvhRUjab47DCo9efnzmDha8xUeQFEs36Yywv1x8t3T2W";
        pub const COLLATERAL_MINT: &str = "6bZpUNY1qmbvQBgCmfQJUA377X63ATvnpCHYh8hQnfjC";
    }

    pub mod googlx {
        pub const RESERVE: &str = "4wg6rEkGgHaEuxMduP46C1xFZ24Lnp5YgdNkZAHxFzsN";
        pub const LIQUIDITY_VAULT: &str = "5vjGDURj7kT6HZtoSmfG9NgTak7deQ9u3uWgdktXv32G";
        pub const COLLATERAL_MINT: &str = "FL41HF8KMuMmYHxGgHezsa5MLUKmNSsu32cC8Qru7TnB";
    }
}

/// Kamino main market addresses — reference only. Not wired into any Vanna instruction; the
/// One-Click cross-asset carry trade (deposit/borrow a stock, swap the total into USDC/SOL, then
/// supply it here) calls Kamino's `deposit_reserve_liquidity`/`redeem_reserve_collateral`
/// directly from the frontend (no Vanna CPI), the same permissionless instructions used above,
/// just against these reserves instead of the xStocks market's. Addresses verified by decoding
/// the live Reserve accounts on mainnet (klend-sdk), not just trusting Kamino's REST API — that
/// API's own reserve list returned four different USDC reserves for this one market; this is the
/// one with real liquidity, the other three are effectively retired (~0 available liquidity).
pub mod kamino_main_market {
    pub const MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
    pub const MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";

    pub mod sol {
        pub const RESERVE: &str = "d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q";
        pub const LIQUIDITY_VAULT: &str = "GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U";
        pub const COLLATERAL_MINT: &str = "2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1";
    }

    pub mod usdc {
        pub const RESERVE: &str = "D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59";
        pub const LIQUIDITY_VAULT: &str = "Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6";
        pub const COLLATERAL_MINT: &str = "B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D";
    }
}

/// Anchor sighash discriminators for hand-rolled Kamino CPIs.
pub mod kamino_discriminators {
    /// sha256("global:deposit_reserve_liquidity")[0..8]
    pub const DEPOSIT_RESERVE_LIQUIDITY: [u8; 8] = [169, 201, 30, 126, 6, 205, 102, 68];
    /// sha256("global:redeem_reserve_collateral")[0..8]
    pub const REDEEM_RESERVE_COLLATERAL: [u8; 8] = [234, 117, 181, 125, 185, 142, 220, 29];
}
