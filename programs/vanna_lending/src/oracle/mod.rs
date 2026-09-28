//! Oracle facade: [`get_price`] prices any registered asset from its `OracleConfig`, the Solana
//! counterpart of the Solidity `OracleFacade.getPrice(token)`. Each source has its own reader
//! (`scope`, `pyth`, and the klend exchange rate for Kamino receipts), and callers only ever see
//! an [`OraclePrice`]: a price they value amounts with (`OraclePrice::value_of`) plus the checks
//! it passed.
//!
//! Source selection, as in Kamino:
//! - Scope is primary, and a fresh Scope price is used as is.
//! - Otherwise the Pyth fallback, if configured, is used when it is newer.
//! - The price used carries status flags (fresh, near its TWAP, confident). A price failing a
//!   check is still returned, so each instruction requires only what it needs.
//!
//! Oracle accounts are found by key among the accounts an instruction received, so a client
//! passes each once, in any order. Every key is pinned in the asset's config, and a configured
//! account that is needed but missing is an error, so a caller can never pick a source by
//! leaving one out.

pub mod price;
pub mod pyth;
pub mod scope;

pub use price::{OraclePrice, Price, PriceStatus};

use crate::adapters::kamino;
use crate::constants::ASSET_SEED;
use crate::errors::VannaError;
use crate::state::asset_config::{AssetConfig, OracleConfig};
use anchor_lang::prelude::*;

struct Quote {
    price: Price,
    /// TWAP / EMA and its timestamp.
    twap: Option<(Price, i64)>,
    timestamp: i64,
    confidence_ok: bool,
}

/// The price of `asset`, with the checks it passed.
pub fn get_price(asset: &AssetConfig, accounts: &[&[AccountInfo]], clock: &Clock) -> Result<OraclePrice> {
    let config = &asset.oracle;
    require!(config.is_configured(), VannaError::PriceUnavailable);
    let now = clock.unix_timestamp;
    let is_fresh = |timestamp: i64| now.saturating_sub(timestamp) <= i64::from(config.max_age_secs);

    let mut quote = match config.uses_scope() {
        true => scope_quote(find_account(accounts, &config.scope_prices)?, config)?,
        false => None,
    };
    if config.uses_pyth() && !quote.as_ref().is_some_and(|q| is_fresh(q.timestamp)) {
        let fallback = pyth_quote(accounts, config)?;
        if quote.as_ref().is_none_or(|q| fallback.timestamp > q.timestamp) {
            quote = Some(fallback);
        }
    }
    let quote = quote.ok_or(VannaError::PriceUnavailable)?;

    let twap_ok = !config.twap_check_enabled()
        || quote.twap.is_some_and(|(twap, timestamp)| {
            is_fresh(timestamp) && !quote.price.diverges_from(twap, config.max_twap_divergence_bps)
        });
    let status = PriceStatus::NONE
        .with(PriceStatus::FRESH, is_fresh(quote.timestamp))
        .with(PriceStatus::TWAP_OK, twap_ok)
        .with(PriceStatus::CONFIDENCE_OK, quote.confidence_ok);

    let receipt_rate = match config.uses_klend() {
        true => Some(klend_rate(find_account(accounts, &config.klend_reserve)?, asset)?),
        false => None,
    };
    Ok(OraclePrice { price: quote.price, receipt_rate, timestamp: quote.timestamp, status })
}

/// The account with `key` among `accounts`.
pub fn find_account<'a, 'info>(accounts: &[&'a [AccountInfo<'info>]], key: &Pubkey) -> Result<&'a AccountInfo<'info>> {
    accounts
        .iter()
        .flat_map(|group| group.iter())
        .find(|account| account.key == key)
        .ok_or_else(|| VannaError::InvalidPriceSource.into())
}

fn scope_quote(account: &AccountInfo, config: &OracleConfig) -> Result<Option<Quote>> {
    let Some((price, timestamp)) = scope::read_chain(account, &config.scope_chain)? else {
        return Ok(None);
    };
    let twap = match scope::is_unused(&config.scope_twap_chain) {
        true => None,
        false => scope::read_chain(account, &config.scope_twap_chain)?,
    };
    Ok(Some(Quote { price, twap, timestamp, confidence_ok: true }))
}

fn pyth_quote(accounts: &[&[AccountInfo]], config: &OracleConfig) -> Result<Quote> {
    let base = pyth::read(find_account(accounts, &config.pyth_price)?, config.max_confidence_bps)?;
    if !config.uses_pyth_factor() {
        return Ok(Quote {
            price: base.price,
            twap: Some((base.ema, base.publish_time)),
            timestamp: base.publish_time,
            confidence_ok: base.confidence_ok,
        });
    }
    let factor = pyth::read(find_account(accounts, &config.pyth_factor)?, config.max_confidence_bps)?;
    let timestamp = base.publish_time.min(factor.publish_time);
    Ok(Quote {
        price: base.price.checked_mul(factor.price)?,
        twap: Some((base.ema.checked_mul(factor.ema)?, timestamp)),
        timestamp,
        confidence_ok: base.confidence_ok && factor.confidence_ok,
    })
}

/// The receipt's klend exchange rate (the cToken mint is checked against the asset's).
fn klend_rate(reserve: &AccountInfo, asset: &AssetConfig) -> Result<kamino::ReserveRate> {
    kamino::read_reserve_rate(reserve, &asset.oracle.klend_program, &asset.mint)
}

/// Admin-time checks of an asset's `OracleConfig` (the Solidity `setOracle`): each account is the
/// kind it claims to be, Pyth accounts are the push oracle's own feed accounts, a receipt uses its
/// underlying's sources, and the asset can be priced now. `accounts` holds every configured
/// account and, for a receipt, the underlying's `AssetConfig`.
pub fn validate_config(asset: &AssetConfig, accounts: &[&[AccountInfo]], program_id: &Pubkey, clock: &Clock) -> Result<()> {
    let config = &asset.oracle;
    require!(config.is_configured() && config.max_age_secs > 0, VannaError::InvalidOracleConfig);
    require!(
        config.max_twap_divergence_bps <= 10_000 && config.max_confidence_bps <= 10_000,
        VannaError::InvalidOracleConfig
    );

    if config.uses_scope() {
        scope::check_prices_account(find_account(accounts, &config.scope_prices)?)?;
        scope::chain_entries(&config.scope_chain)?;
        if !scope::is_unused(&config.scope_twap_chain) {
            scope::chain_entries(&config.scope_twap_chain)?;
        }
        // Scope's TWAP comes from its own chain: a divergence check needs one.
        require!(
            !config.twap_check_enabled() || !scope::is_unused(&config.scope_twap_chain),
            VannaError::InvalidOracleConfig
        );
    } else {
        require!(
            scope::is_unused(&config.scope_chain) && scope::is_unused(&config.scope_twap_chain),
            VannaError::InvalidOracleConfig
        );
    }

    require!(config.uses_pyth() || !config.uses_pyth_factor(), VannaError::InvalidOracleConfig);
    for key in [config.pyth_price, config.pyth_factor] {
        if key != Pubkey::default() {
            let update = pyth::read_price_update(find_account(accounts, &key)?)?;
            require_keys_eq!(key, pyth::canonical_feed_account(&update.price_message.feed_id), VannaError::InvalidPriceFeed);
        }
    }

    if config.uses_klend() {
        require!(config.klend_program != Pubkey::default(), VannaError::InvalidOracleConfig);
        // A receipt is collateral only.
        require!(
            asset.reserve == Pubkey::default() && !asset.borrow_enabled,
            VannaError::UnsupportedPriceSource
        );
        let rate = kamino::read_reserve_rate(find_account(accounts, &config.klend_reserve)?, &config.klend_program, &asset.mint)?;
        let (underlying_key, _) = Pubkey::find_program_address(&[ASSET_SEED, rate.liquidity_mint.as_ref()], program_id);
        let underlying_info = find_account(accounts, &underlying_key)?;
        require_keys_eq!(*underlying_info.owner, *program_id, VannaError::InvalidPriceSource);
        let underlying = AssetConfig::try_deserialize(&mut &underlying_info.try_borrow_data()?[..])?;
        require!(underlying.decimals == rate.liquidity_decimals, VannaError::InvalidKaminoAccounts);
        require!(same_sources(&underlying.oracle, config), VannaError::InvalidPriceFeed);
    } else {
        require!(config.klend_program == Pubkey::default(), VannaError::InvalidOracleConfig);
    }

    get_price(asset, accounts, clock).map(|_| ())
}

/// Whether two configs read the same price (ignoring a receipt's exchange rate and the limits).
fn same_sources(a: &OracleConfig, b: &OracleConfig) -> bool {
    a.scope_prices == b.scope_prices
        && a.scope_chain == b.scope_chain
        && a.scope_twap_chain == b.scope_twap_chain
        && a.pyth_price == b.pyth_price
        && a.pyth_factor == b.pyth_factor
}
