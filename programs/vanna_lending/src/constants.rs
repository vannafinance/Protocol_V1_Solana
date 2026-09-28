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
#[constant]
pub const INTEGRATION_SEED: &[u8] = b"integration";

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

pub const CLASSIC_SPL_TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;

pub const TOKEN_2022_PROGRAM: Pubkey = anchor_spl::token_2022::ID;

/// Reference mint addresses (not enforced on-chain — assets are registered by the admin).
/// Lending pools: USDC, USDT, SOL. Margin collateral: those three plus the collateral-only
/// assets below.
pub mod known_mints {
    pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    pub const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    pub const WSOL: &str = "So11111111111111111111111111111111111111112";
    pub const JITOSOL: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";
    pub const JUPSOL: &str = "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v";
    pub const JUPUSD: &str = "JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD";
    /// xStocks: Token-2022 with the Scaled UI Amount extension (`PriceSource::ScaledUiAmount`).
    pub const NVDAX: &str = "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh";
    pub const TSLAX: &str = "XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB";
}

/// Reference Pyth feed ids (hex). JupSOL has no USD feed: it is priced as JUPSOL/SOL.RR × SOL/USD
/// (`PriceSource::RedemptionRate`). The xStocks use their own token feeds, not the shares'.
pub mod pyth_feeds {
    pub const USDC_USD: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
    pub const USDT_USD: &str = "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b";
    pub const SOL_USD: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    pub const JITOSOL_USD: &str = "67be9f519b95cf24338801051f9a808eff0a578ccb388db73b7f6fe1de019ffb";
    pub const JUPSOL_SOL_RR: &str = "f8d8d6b6c866c8b2624fb5b679ae846738725e5fc887fa8e927c8d8645018a2b";
    pub const JUPUSD_USD: &str = "8ed858a2214e892c9371694fb6c8a9037b6ed4052c4edf209f8cb988484e81d9";
    pub const NVDAX_USD: &str = "4244d07890e4610f46bbde67de8f43a4bf8b569eebe904f136b469f148503b7f";
    pub const TSLAX_USD: &str = "47a156470288850a440df3a6ce85a55917b813a19bb5b31128a33a986566a362";
}

/// Kamino main market addresses — reference only; the live whitelist is the `Integration` registry.
pub mod kamino_main_market {
    pub const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
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
