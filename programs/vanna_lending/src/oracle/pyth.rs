use crate::errors::VannaError;
use crate::state::asset_config::AssetConfig;
use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::error::GetPriceError;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// Re-export so `Account<'info, PriceUpdateV2>` in instruction account structs automatically
/// enforces "oracle account owner is the expected Pyth receiver program" (spec §8 item 1) —
/// Anchor checks `account_info.owner == PriceUpdateV2::owner()`, which this crate ties to the
/// Pyth Solana Receiver program ID.
pub use pyth_solana_receiver_sdk::ID as PYTH_RECEIVER_PROGRAM_ID;

pub struct ValidatedPrice {
    pub price: i64,
    pub exponent: i32,
}

/// Spec §8 `load_validated_price` — feed-ID match, freshness, sign and confidence, all fail-closed.
/// Owner check happens implicitly through the `Account<'info, PriceUpdateV2>` type in the caller's
/// account struct; this function only needs to apply the asset-specific business rules.
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
