//! Single entry point for valuing collateral, whatever its `PriceSource`.

use crate::adapters::kamino;
use crate::errors::VannaError;
use crate::math::health::{normalize_token_value, value_to_token_amount};
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::{AssetConfig, PriceSource};
use anchor_lang::prelude::*;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// Returns the price-source account an asset needs, looked up by key among `candidates`.
/// `None` for Pyth-priced assets; an error if a required source is missing.
pub fn find_source_account<'a, 'info>(
    asset: &AssetConfig,
    candidates: &[&'a [AccountInfo<'info>]],
) -> Result<Option<&'a AccountInfo<'info>>> {
    if asset.is_pyth_priced() {
        return Ok(None);
    }
    candidates
        .iter()
        .flat_map(|accounts| accounts.iter())
        .find(|a| a.key() == asset.price_source_account)
        .map(Some)
        .ok_or_else(|| VannaError::InvalidPriceSource.into())
}

/// Reads a receipt asset's exchange rate from its registered price-source account.
pub fn receipt_rate(asset: &AssetConfig, source: Option<&AccountInfo>) -> Result<kamino::ReserveRate> {
    let source = source.ok_or(VannaError::InvalidPriceSource)?;
    require_keys_eq!(source.key(), asset.price_source_account, VannaError::InvalidPriceSource);
    kamino::read_reserve_rate(source, &asset.price_source_program, &asset.mint)
}

/// USD value (nano-USD) of `amount` of the asset, rounded down.
///
/// Pyth:          value = amount × price
/// KaminoReceipt: value = (amount × total_liquidity / collateral_supply) × underlying price,
///                with the underlying amount scaled by the underlying's decimals
pub fn collateral_value(
    asset: &AssetConfig,
    amount: u64,
    price_update: &Account<PriceUpdateV2>,
    source: Option<&AccountInfo>,
    clock: &Clock,
) -> Result<u128> {
    let price = load_validated_price(asset, price_update, clock)?;
    let (priced_amount, decimals) = match asset.price_source {
        PriceSource::Pyth => (amount, asset.decimals),
        PriceSource::KaminoReceipt => {
            let rate = receipt_rate(asset, source)?;
            (rate.underlying_for_receipts(amount)?, rate.liquidity_decimals)
        }
    };
    normalize_token_value(priced_amount, price.price, price.exponent, decimals, false)
}

/// Asset amount worth `value` (nano-USD), rounded down. Inverse of [`collateral_value`].
pub fn amount_for_value(
    asset: &AssetConfig,
    value: u128,
    price_update: &Account<PriceUpdateV2>,
    source: Option<&AccountInfo>,
    clock: &Clock,
) -> Result<u64> {
    let price = load_validated_price(asset, price_update, clock)?;
    match asset.price_source {
        PriceSource::Pyth => value_to_token_amount(value, price.price, price.exponent, asset.decimals, false),
        PriceSource::KaminoReceipt => {
            let rate = receipt_rate(asset, source)?;
            let underlying = value_to_token_amount(value, price.price, price.exponent, rate.liquidity_decimals, false)?;
            rate.receipts_for_underlying(underlying)
        }
    }
}
