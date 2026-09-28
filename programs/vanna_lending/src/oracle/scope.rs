//! Kamino Scope reader. A price is the product of a chain of `OraclePrices` entries, stamped with
//! its oldest entry's time, as klend reads it (`utils/prices/scope.rs`). Scope's keepers refresh
//! the entries; this program only reads them.

use crate::constants::{SCOPE_MAX_ENTRIES, SCOPE_PROGRAM_ID};
use crate::errors::VannaError;
use crate::oracle::price::Price;
use crate::state::asset_config::SCOPE_CHAIN_UNUSED;
use anchor_lang::prelude::*;

/// sha256("account:OraclePrices")[..8]
pub const ORACLE_PRICES_DISCRIMINATOR: [u8; 8] = [89, 128, 118, 221, 6, 72, 180, 146];
/// `discriminator (8) | oracle_mappings (32) | prices: [DatedPrice; 512]`.
const PRICES_OFFSET: usize = 40;
/// `DatedPrice`: `value: u64 | exp: u64 | last_updated_slot: u64 | unix_timestamp: u64 | generic_data: [u8; 24]`.
const DATED_PRICE_LEN: usize = 56;
pub const ORACLE_PRICES_LEN: usize = PRICES_OFFSET + DATED_PRICE_LEN * SCOPE_MAX_ENTRIES;
/// Largest price exponent accepted from Scope (it stores `value / 10^exp`).
const MAX_EXP: u64 = 38;

pub fn is_unused(chain: &[u16; 4]) -> bool {
    chain.iter().all(|entry| *entry == SCOPE_CHAIN_UNUSED)
}

/// The used entries of a chain. An empty chain, a gap, or an entry outside the table is invalid.
pub fn chain_entries(chain: &[u16; 4]) -> Result<&[u16]> {
    let len = chain.iter().take_while(|entry| **entry != SCOPE_CHAIN_UNUSED).count();
    require!(len > 0, VannaError::InvalidOracleConfig);
    require!(chain[len..].iter().all(|entry| *entry == SCOPE_CHAIN_UNUSED), VannaError::InvalidOracleConfig);
    require!(
        chain[..len].iter().all(|entry| usize::from(*entry) < SCOPE_MAX_ENTRIES),
        VannaError::InvalidOracleConfig
    );
    Ok(&chain[..len])
}

/// Checks that `account` is a Scope `OraclePrices` account.
pub fn check_prices_account(account: &AccountInfo) -> Result<()> {
    require_keys_eq!(*account.owner, SCOPE_PROGRAM_ID, VannaError::InvalidOracleOwner);
    let data = account.try_borrow_data()?;
    require!(
        data.len() >= ORACLE_PRICES_LEN && data[..8] == ORACLE_PRICES_DISCRIMINATOR,
        VannaError::InvalidPriceFeed
    );
    Ok(())
}

/// The chain's price and its oldest entry's timestamp. `None` when an entry is zero (never written
/// or cleared by Scope): the facade treats that like a stale price rather than a price of zero.
pub fn read_chain(account: &AccountInfo, chain: &[u16; 4]) -> Result<Option<(Price, i64)>> {
    check_prices_account(account)?;
    let data = account.try_borrow_data()?;
    let mut product: Option<Price> = None;
    let mut timestamp = i64::MAX;
    for entry in chain_entries(chain)? {
        let at = PRICES_OFFSET + DATED_PRICE_LEN * usize::from(*entry);
        let u64_at = |offset: usize| u64::from_le_bytes(data[at + offset..at + offset + 8].try_into().unwrap());
        let (value, exp, unix_timestamp) = (u64_at(0), u64_at(8), u64_at(24));
        if value == 0 {
            return Ok(None);
        }
        require!(exp <= MAX_EXP, VannaError::InvalidPrice);
        let price = Price::new(value as u128, -(exp as i32))?;
        product = Some(match product {
            None => price,
            Some(acc) => acc.checked_mul(price)?,
        });
        timestamp = timestamp.min(i64::try_from(unix_timestamp).map_err(|_| VannaError::InvalidPrice)?);
    }
    Ok(product.map(|price| (price, timestamp)))
}
