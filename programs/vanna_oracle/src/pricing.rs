use crate::config::OracleConfig;
use crate::OracleError;
use crate::price::{OraclePrice, Price};
use crate::{klend, pyth, scope};
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::PriceChecks;

struct Quote {
    price: Price,
    twap: Option<(Price, i64)>,
    timestamp: i64,
    confidence_ok: bool,
}

pub fn get_price(mint: &Pubkey, config: &OracleConfig, accounts: &[AccountInfo], clock: &Clock) -> Result<OraclePrice> {
    require!(config.is_configured(), OracleError::PriceUnavailable);
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
    let quote = quote.ok_or(OracleError::PriceUnavailable)?;

    let twap_ok = !config.twap_check_enabled()
        || quote.twap.is_some_and(|(twap, timestamp)| {
            is_fresh(timestamp) && !quote.price.diverges_from(twap, config.max_twap_divergence_bps)
        });
    let checks = PriceChecks::NONE
        .with(PriceChecks::FRESH, is_fresh(quote.timestamp))
        .with(PriceChecks::TWAP_OK, twap_ok)
        .with(PriceChecks::CONFIDENCE_OK, quote.confidence_ok);

    let receipt_rate = match config.uses_klend() {
        true => Some(klend::read_reserve_rate(find_account(accounts, &config.klend_reserve)?, &config.klend_program, mint)?),
        false => None,
    };
    Ok(OraclePrice { price: quote.price, receipt_rate, timestamp: quote.timestamp, checks })
}

pub fn find_account<'a, 'info>(accounts: &'a [AccountInfo<'info>], key: &Pubkey) -> Result<&'a AccountInfo<'info>> {
    accounts.iter().find(|account| account.key == key).ok_or_else(|| OracleError::InvalidPriceSource.into())
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

fn pyth_quote(accounts: &[AccountInfo], config: &OracleConfig) -> Result<Quote> {
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

pub fn validate_config(
    mint: &Pubkey,
    config: &OracleConfig,
    underlying: Option<(&Pubkey, &OracleConfig)>,
    accounts: &[AccountInfo],
    clock: &Clock,
) -> Result<()> {
    require!(config.is_configured() && config.max_age_secs > 0, OracleError::InvalidOracleConfig);
    require!(
        config.max_twap_divergence_bps <= 10_000 && config.max_confidence_bps <= 10_000,
        OracleError::InvalidOracleConfig
    );

    if config.uses_scope() {
        scope::check_prices_account(find_account(accounts, &config.scope_prices)?)?;
        scope::chain_entries(&config.scope_chain)?;
        if !scope::is_unused(&config.scope_twap_chain) {
            scope::chain_entries(&config.scope_twap_chain)?;
        }

        require!(
            !config.twap_check_enabled() || !scope::is_unused(&config.scope_twap_chain),
            OracleError::InvalidOracleConfig
        );
    } else {
        require!(
            scope::is_unused(&config.scope_chain) && scope::is_unused(&config.scope_twap_chain),
            OracleError::InvalidOracleConfig
        );
    }

    require!(config.uses_pyth() || !config.uses_pyth_factor(), OracleError::InvalidOracleConfig);
    for key in [config.pyth_price, config.pyth_factor] {
        if key != Pubkey::default() {
            let update = pyth::read_price_update(find_account(accounts, &key)?)?;
            require_keys_eq!(key, pyth::canonical_feed_account(&update.price_message.feed_id), OracleError::InvalidPriceFeed);
        }
    }

    if config.uses_klend() {
        require!(config.klend_program != Pubkey::default(), OracleError::InvalidOracleConfig);
        let rate = klend::read_reserve_rate(find_account(accounts, &config.klend_reserve)?, &config.klend_program, mint)?;
        let (underlying_mint, underlying) = underlying.ok_or(OracleError::UnsupportedPriceSource)?;
        require_keys_eq!(*underlying_mint, rate.liquidity_mint, OracleError::InvalidKaminoAccounts);
        require!(underlying.same_sources(config), OracleError::InvalidPriceFeed);
    } else {
        require!(config.klend_program == Pubkey::default(), OracleError::InvalidOracleConfig);
    }

    get_price(mint, config, accounts, clock).map(|_| ())
}
