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
pub const LITE_STRATEGY_SEED: &[u8] = b"lite_strategy";
pub const LITE_POSITION_SEED: &[u8] = b"lite_position";

/// Max active collateral (or debt) assets per margin account; bounds health-check cost.
/// Changing it changes `MarginAccount`'s layout and requires fresh margin accounts.
pub const MAX_ASSETS: usize = 24;

/// Fixed-point scale used for `borrow_index_wad` and reported health-factor ratios.
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// healthy = total_collateral_value / total_debt_value > 1.10 (equality is unhealthy)
pub const BALANCE_TO_BORROW_THRESHOLD_WAD: u128 = 1_100_000_000_000_000_000;

/// Internal USD precision (nano-USD). Smaller than WAD so value sums stay far from u128 overflow.
pub const USD_VALUE_DECIMALS: u32 = 9;

pub const BASIS_POINTS: u64 = 10_000;

/// Average Gregorian year (365.2425 days) — the annualization basis of the borrow-rate curve.
pub const SECONDS_PER_YEAR: u64 = 31_556_952;

/// Max `RateCurve` coefficient (10.0), so no admin config can overflow accrual and brick a reserve.
pub const MAX_RATE_COEFF_WAD: u64 = 10_000_000_000_000_000_000;

/// Minimum first-deposit shares, against the first-depositor share-inflation attack.
pub const MIN_INITIAL_SHARES: u64 = 1_000;

/// Sentinel `supply_cap`/`borrow_cap` value meaning "no cap enforced".
pub const UNCAPPED: u64 = 0;

/// Liquidation close factor (50%). Pre-audit value; make it governed config before raising caps.
pub const CLOSE_FACTOR_BPS: u64 = 5_000;

pub const CLASSIC_SPL_TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;

pub const TOKEN_2022_PROGRAM: Pubkey = anchor_spl::token_2022::ID;

/// Reference mint addresses (not enforced on-chain — any mint can be registered by the admin).
pub mod known_mints {
    pub const MAINNET_USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    pub const WSOL: &str = "So11111111111111111111111111111111111111112";
    pub const TSLAX: &str = "XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB";
    pub const GOOGLX: &str = "XsCPL9dNWBMvFtTmwcCA5v3xWPSMEBCszbQdiLLq6aN";
    /// No real Pyth feed exists for the PreStocks below.
    pub const ANTHROPIC: &str = "Pren1FvFX6J3E4kXhJuCiAD5aDmGEb7qJRncwA8Lkhw";
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

/// Kamino main market addresses — reference only; the frontend calls these directly.
/// The USDC reserve is the only one of the four listed by Kamino's API with real liquidity.
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
