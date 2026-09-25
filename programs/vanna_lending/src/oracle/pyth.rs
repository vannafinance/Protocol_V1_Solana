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
/// The oracle owner check is done by Anchor: `Account<PriceUpdateV2>` requires the account to be
/// owned by the Pyth Solana Receiver program.
pub fn load_validated_price(
    asset: &AssetConfig,
    price_update: &Account<PriceUpdateV2>,
    clock: &Clock,
) -> Result<ValidatedPrice> {
    let price = price_update
        .get_price_no_older_than(clock, asset.max_price_age_secs as u64, &asset.price_feed_id)
        .map_err(map_pyth_error)?;

    require!(price.price > 0, VannaError::InvalidPrice);

    if price.conf > 0 {
        let confidence_bps = (price.conf as u128)
            .checked_mul(10_000)
            .and_then(|v| v.checked_div(price.price as u128))
            .ok_or(VannaError::MathOverflow)?;
        require!(
            confidence_bps <= asset.max_confidence_bps as u128,
            VannaError::ConfidenceTooWide
        );
    }

    Ok(ValidatedPrice {
        price: price.price,
        exponent: price.exponent,
    })
}

fn map_pyth_error(err: GetPriceError) -> anchor_lang::error::Error {
    match err {
        GetPriceError::PriceTooOld => VannaError::StalePrice.into(),
        GetPriceError::MismatchedFeedId => VannaError::InvalidPriceFeed.into(),
        GetPriceError::InsufficientVerificationLevel => VannaError::InvalidPriceFeed.into(),
        _ => VannaError::InvalidPrice.into(),
    }
}
