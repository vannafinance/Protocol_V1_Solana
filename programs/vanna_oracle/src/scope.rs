use crate::config::SCOPE_CHAIN_UNUSED;
use crate::OracleError;
use crate::price::Price;
use anchor_lang::prelude::*;

pub const SCOPE_PROGRAM_ID: Pubkey = pubkey!("HFn8GnPADiny6XqUoWE8uRPPxb29ikn4yTuPa9MF2fWJ");
pub const SCOPE_MAX_ENTRIES: usize = 512;
pub const ORACLE_PRICES_DISCRIMINATOR: [u8; 8] = [89, 128, 118, 221, 6, 72, 180, 146];
const PRICES_OFFSET: usize = 40;
const DATED_PRICE_LEN: usize = 56;
pub const ORACLE_PRICES_LEN: usize = PRICES_OFFSET + DATED_PRICE_LEN * SCOPE_MAX_ENTRIES;
const MAX_EXP: u64 = 38;

pub fn is_unused(chain: &[u16; 4]) -> bool {
    chain.iter().all(|entry| *entry == SCOPE_CHAIN_UNUSED)
}

pub fn chain_entries(chain: &[u16; 4]) -> Result<&[u16]> {
    let len = chain.iter().take_while(|entry| **entry != SCOPE_CHAIN_UNUSED).count();
    require!(len > 0, OracleError::InvalidOracleConfig);
    require!(chain[len..].iter().all(|entry| *entry == SCOPE_CHAIN_UNUSED), OracleError::InvalidOracleConfig);
    require!(
        chain[..len].iter().all(|entry| usize::from(*entry) < SCOPE_MAX_ENTRIES),
        OracleError::InvalidOracleConfig
    );
    Ok(&chain[..len])
}

pub fn check_prices_account(account: &AccountInfo) -> Result<()> {
    require_keys_eq!(*account.owner, SCOPE_PROGRAM_ID, OracleError::InvalidOracleOwner);
    let data = account.try_borrow_data()?;
    require!(
        data.len() >= ORACLE_PRICES_LEN && data[..8] == ORACLE_PRICES_DISCRIMINATOR,
        OracleError::InvalidPriceFeed
    );
    Ok(())
}

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
        require!(exp <= MAX_EXP, OracleError::InvalidPrice);
        let price = Price::new(value as u128, -(exp as i32))?;
        product = Some(match product {
            None => price,
            Some(acc) => acc.checked_mul(price)?,
        });
        timestamp = timestamp.min(i64::try_from(unix_timestamp).map_err(|_| OracleError::InvalidPrice)?);
    }
    Ok(product.map(|price| (price, timestamp)))
}
