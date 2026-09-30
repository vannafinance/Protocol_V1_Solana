use crate::OracleError;
use crate::price::Price;
use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel};

pub struct PythQuote {
    pub price: Price,
    pub ema: Price,
    pub publish_time: i64,
    pub confidence_ok: bool,
}

pub fn read(account: &AccountInfo, max_confidence_bps: u16) -> Result<PythQuote> {
    let update = read_price_update(account)?;
    require!(update.verification_level.gte(VerificationLevel::Full), OracleError::InvalidPriceFeed);
    let message = &update.price_message;
    require!(message.price > 0 && message.ema_price > 0, OracleError::InvalidPrice);
    let confidence_ok = u128::from(message.conf) * 10_000 <= message.price as u128 * u128::from(max_confidence_bps);
    Ok(PythQuote {
        price: Price::new(message.price as u128, message.exponent)?,
        ema: Price::new(message.ema_price as u128, message.exponent)?,
        publish_time: message.publish_time,
        confidence_ok,
    })
}

pub fn read_price_update(account: &AccountInfo) -> Result<PriceUpdateV2> {
    require_keys_eq!(*account.owner, pyth_solana_receiver_sdk::ID, OracleError::InvalidOracleOwner);
    let data = account.try_borrow_data()?;
    PriceUpdateV2::try_deserialize(&mut &data[..]).map_err(|_| OracleError::InvalidPriceFeed.into())
}

pub fn canonical_feed_account(feed_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[&0u16.to_le_bytes(), feed_id], &pyth_solana_receiver_sdk::PYTH_PUSH_ORACLE_ID).0
}
