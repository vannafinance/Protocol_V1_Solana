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

pub const MAX_ASSETS: usize = 24;

pub const WAD: u128 = 1_000_000_000_000_000_000;

pub const BALANCE_TO_BORROW_THRESHOLD_WAD: u128 = 1_100_000_000_000_000_000;

pub const USD_VALUE_DECIMALS: u32 = 9;

pub const BASIS_POINTS: u64 = 10_000;

pub const SECONDS_PER_YEAR: u64 = 31_556_952;

pub const MAX_RATE_COEFF_WAD: u64 = 10_000_000_000_000_000_000;

pub const MIN_INITIAL_SHARES: u64 = 1_000;

pub const UNCAPPED: u64 = 0;

pub const CLASSIC_SPL_TOKEN_PROGRAM: Pubkey = anchor_spl::token::ID;

pub const TOKEN_2022_PROGRAM: Pubkey = anchor_spl::token_2022::ID;
