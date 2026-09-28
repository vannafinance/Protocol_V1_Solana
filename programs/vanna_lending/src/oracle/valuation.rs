//! Single entry point for valuing collateral, whatever its `PriceSource`.

use crate::adapters::kamino;
use crate::constants::WAD;
use crate::errors::VannaError;
use crate::math::fixed_point::{checked_pow10, mul_div_floor, u64_from_u128};
use crate::math::health::normalize_token_value;
use crate::oracle::pyth::{load_feed_price, load_validated_price, read_price_update, ValidatedPrice};
use crate::state::asset_config::{AssetConfig, PriceSource};
use anchor_lang::prelude::*;
use anchor_spl::token_2022::spl_token_2022::{
    extension::{scaled_ui_amount::ScaledUiAmountConfig, BaseStateWithExtensions, StateWithExtensions},
    state::Mint,
};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// Largest Scaled UI Amount multiplier accepted (a 1,000,000:1 split).
const MAX_UI_MULTIPLIER: f64 = 1_000_000.0;

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

/// The account an asset's valuation reads: `explicit` if passed, otherwise the asset's own mint
/// when the mint is its source (Scaled UI Amount), so callers that already hold the mint need
/// no extra account.
pub fn source_or_mint<'a, 'info>(
    asset: &AssetConfig,
    explicit: Option<&'a AccountInfo<'info>>,
    mint: &'a AccountInfo<'info>,
) -> Option<&'a AccountInfo<'info>> {
    explicit.or((asset.price_source == PriceSource::ScaledUiAmount && mint.key() == asset.price_source_account).then_some(mint))
}

/// The registered source account, checked by key.
fn registered_source<'a, 'info>(asset: &AssetConfig, source: Option<&'a AccountInfo<'info>>) -> Result<&'a AccountInfo<'info>> {
    let source = source.ok_or(VannaError::InvalidPriceSource)?;
    require_keys_eq!(source.key(), asset.price_source_account, VannaError::InvalidPriceSource);
    Ok(source)
}

/// Reads a receipt asset's exchange rate from its registered price-source account.
pub fn receipt_rate(asset: &AssetConfig, source: Option<&AccountInfo>) -> Result<kamino::ReserveRate> {
    let source = registered_source(asset, source)?;
    kamino::read_reserve_rate(source, &asset.price_source_program, &asset.mint)
}

/// Current Scaled UI Amount multiplier of a Token-2022 mint, as WAD. Like Token-2022 itself, the
/// scheduled `new_multiplier` applies from its effective timestamp on; before that, `multiplier`.
pub fn ui_multiplier_wad(mint: &AccountInfo, now: i64) -> Result<u128> {
    require_keys_eq!(*mint.owner, anchor_spl::token_2022::ID, VannaError::InvalidPriceSource);
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<Mint>::unpack(&data).map_err(|_| VannaError::InvalidPriceSource)?;
    let config = state.get_extension::<ScaledUiAmountConfig>().map_err(|_| VannaError::InvalidPriceSource)?;
    let multiplier: f64 = if now >= i64::from(config.new_multiplier_effective_timestamp) {
        config.new_multiplier.into()
    } else {
        config.multiplier.into()
    };
    require!(
        multiplier.is_finite() && multiplier > 0.0 && multiplier <= MAX_UI_MULTIPLIER,
        VannaError::InvalidPriceSource
    );
    let wad = (multiplier * WAD as f64) as u128;
    require!(wad > 0, VannaError::InvalidPriceSource);
    Ok(wad)
}

/// A redemption-rate asset's base price, read from its pinned base price-feed account.
fn base_price(asset: &AssetConfig, source: Option<&AccountInfo>, clock: &Clock) -> Result<ValidatedPrice> {
    let update = read_price_update(registered_source(asset, source)?)?;
    let feed_id = update.price_message.feed_id;
    load_feed_price(&update, &feed_id, asset, clock)
}

/// amount × price × 10^exponent, rounded down: converts an amount through a Pyth rate.
fn apply_rate(amount: u64, rate: &ValidatedPrice) -> Result<u64> {
    let exponent = rate.exponent;
    let scaled = if exponent >= 0 {
        (amount as u128)
            .checked_mul(rate.price as u128)
            .and_then(|v| v.checked_mul(checked_pow10(exponent as u32).ok()?))
            .ok_or(VannaError::MathOverflow)?
    } else {
        mul_div_floor(amount as u128, rate.price as u128, checked_pow10(exponent.unsigned_abs())?)?
    };
    u64_from_u128(scaled)
}

/// USD value (nano-USD) of `amount` of the asset, rounded down.
///
/// Pyth:           value = amount × price
/// KaminoReceipt:  value = (amount × total_liquidity / collateral_supply) × underlying price,
///                 with the underlying amount scaled by the underlying's decimals
/// ScaledUiAmount: value = (amount × UI multiplier) × price per UI token
/// RedemptionRate: value = (amount × rate) × base price
pub fn collateral_value(
    asset: &AssetConfig,
    amount: u64,
    price_update: &PriceUpdateV2,
    source: Option<&AccountInfo>,
    clock: &Clock,
) -> Result<u128> {
    let price = load_validated_price(asset, price_update, clock)?;
    match asset.price_source {
        PriceSource::Pyth => normalize_token_value(amount, price.price, price.exponent, asset.decimals, false),
        PriceSource::KaminoReceipt => {
            let rate = receipt_rate(asset, source)?;
            let underlying = rate.underlying_for_receipts(amount)?;
            normalize_token_value(underlying, price.price, price.exponent, rate.liquidity_decimals, false)
        }
        PriceSource::ScaledUiAmount => {
            let multiplier = ui_multiplier_wad(registered_source(asset, source)?, clock.unix_timestamp)?;
            let ui_amount = u64_from_u128(mul_div_floor(amount as u128, multiplier, WAD)?)?;
            normalize_token_value(ui_amount, price.price, price.exponent, asset.decimals, false)
        }
        PriceSource::RedemptionRate => {
            let base = base_price(asset, source, clock)?;
            let base_amount = apply_rate(amount, &price)?;
            normalize_token_value(base_amount, base.price, base.exponent, asset.decimals, false)
        }
    }
}
