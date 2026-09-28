//! Pyth reader. Assets pin the Pyth push oracle's shard-0 price-feed account for their feed, which
//! Pyth keeps updated and which only ever holds that feed. Only fully verified updates are
//! accepted, and the EMA serves as the TWAP.

use crate::errors::VannaError;
use crate::oracle::price::Price;
use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel};

pub struct PythQuote {
    pub price: Price,
    pub ema: Price,
    pub publish_time: i64,
    /// Confidence interval within `max_confidence_bps` of the price.
    pub confidence_ok: bool,
}

pub fn read(account: &AccountInfo, max_confidence_bps: u16) -> Result<PythQuote> {
    let update = read_price_update(account)?;
    require!(update.verification_level.gte(VerificationLevel::Full), VannaError::InvalidPriceFeed);
    let message = &update.price_message;
    require!(message.price > 0 && message.ema_price > 0, VannaError::InvalidPrice);
    let confidence_ok = u128::from(message.conf) * 10_000 <= message.price as u128 * u128::from(max_confidence_bps);
    Ok(PythQuote {
        price: Price::new(message.price as u128, message.exponent)?,
        ema: Price::new(message.ema_price as u128, message.exponent)?,
        publish_time: message.publish_time,
        confidence_ok,
    })
}

/// Deserializes a `PriceUpdateV2` after checking its owner is the Pyth receiver.
pub fn read_price_update(account: &AccountInfo) -> Result<PriceUpdateV2> {
    require_keys_eq!(*account.owner, pyth_solana_receiver_sdk::ID, VannaError::InvalidOracleOwner);
    let data = account.try_borrow_data()?;
    PriceUpdateV2::try_deserialize(&mut &data[..]).map_err(|_| VannaError::InvalidPriceFeed.into())
}

/// Pyth's shard-0 price-feed account for `feed_id`. Only the Pyth push oracle writes it, and only
/// with updates for that feed.
pub fn canonical_feed_account(feed_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[&0u16.to_le_bytes(), feed_id], &pyth_solana_receiver_sdk::PYTH_PUSH_ORACLE_ID).0
}
