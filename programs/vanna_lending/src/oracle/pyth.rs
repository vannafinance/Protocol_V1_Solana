use crate::errors::VannaError;
use crate::state::asset_config::AssetConfig;
use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::error::GetPriceError;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

pub struct ValidatedPrice {
    pub price: i64,
    pub exponent: i32,
}

/// Loads the asset's price, failing closed on feed-ID mismatch, staleness, non-positive price,
/// or a confidence interval wider than `max_confidence_bps` of the price.
///
/// The oracle owner check is done by the caller: `Account<PriceUpdateV2>` requires the Pyth
/// Solana Receiver as owner, and [`read_price_update`] checks it for unchecked accounts.
pub fn load_validated_price(asset: &AssetConfig, price_update: &PriceUpdateV2, clock: &Clock) -> Result<ValidatedPrice> {
    load_feed_price(price_update, &asset.price_feed_id, asset, clock)
}

/// Same checks as [`load_validated_price`] for another feed (a redemption-rate asset's base
/// price), with the asset's age and confidence limits.
pub fn load_feed_price(
    price_update: &PriceUpdateV2,
    feed_id: &[u8; 32],
    limits: &AssetConfig,
    clock: &Clock,
) -> Result<ValidatedPrice> {
    let price = price_update
        .get_price_no_older_than(clock, limits.max_price_age_secs as u64, feed_id)
        .map_err(map_pyth_error)?;

    require!(price.price > 0, VannaError::InvalidPrice);

    if price.conf > 0 {
        let confidence_bps = (price.conf as u128)
            .checked_mul(10_000)
            .and_then(|v| v.checked_div(price.price as u128))
            .ok_or(VannaError::MathOverflow)?;
        require!(
            confidence_bps <= limits.max_confidence_bps as u128,
            VannaError::ConfidenceTooWide
        );
    }

    Ok(ValidatedPrice {
        price: price.price,
        exponent: price.exponent,
    })
}

/// Deserializes a `PriceUpdateV2` from an account that Anchor has not checked.
pub fn read_price_update(info: &AccountInfo) -> Result<PriceUpdateV2> {
    require_keys_eq!(*info.owner, pyth_solana_receiver_sdk::ID, VannaError::InvalidOracleOwner);
    let data = info.try_borrow_data()?;
    PriceUpdateV2::try_deserialize(&mut &data[..]).map_err(|_| VannaError::InvalidPriceFeed.into())
}

/// Pyth's canonical price-feed account for `feed_id` (shard 0). Only the Pyth push oracle writes
/// it, and only with updates for that feed, so its address pins the feed.
pub fn canonical_feed_account(feed_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[&0u16.to_le_bytes(), feed_id], &pyth_solana_receiver_sdk::PYTH_PUSH_ORACLE_ID).0
}

fn map_pyth_error(err: GetPriceError) -> anchor_lang::error::Error {
    match err {
        GetPriceError::PriceTooOld => VannaError::StalePrice.into(),
        GetPriceError::MismatchedFeedId => VannaError::InvalidPriceFeed.into(),
        GetPriceError::InsufficientVerificationLevel => VannaError::InvalidPriceFeed.into(),
        _ => VannaError::InvalidPrice.into(),
    }
}
