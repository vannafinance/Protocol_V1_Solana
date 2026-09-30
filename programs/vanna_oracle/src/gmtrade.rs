use crate::config::OracleConfig;
use crate::gmtrade_accounts::{
    associated_token_address, order_address, order_exists, position_address, read_market, read_position,
    token_balance, MarketState, PositionState, FUNDING_ADJUSTMENT, USD_UNIT,
};
use crate::price::{checked_pow10, mul_div_ceil, Price, USD_VALUE_DECIMALS};
use crate::pricing::{find_account, get_price};
use crate::OracleError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{PriceResult, PriceQuery, PriceChecks};

pub const MARKET_BOOK_SEED: &[u8] = b"market_book";
pub const MAX_MARKETS: usize = 32;

pub const MAX_LEVERAGE_BPS: u32 = 1_000_000;

#[zero_copy]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarketEntry {
    pub market: Pubkey,
    pub market_token: Pubkey,
    pub index_decimals: u8,
    pub trading_enabled: u8,
    pub padding: [u8; 2],
    pub max_leverage_bps: u32,
    pub index_price: OracleConfig,
}

impl MarketEntry {
    pub fn trading_enabled(&self) -> bool {
        self.trading_enabled != 0
    }
}

#[account(zero_copy)]
pub struct MarketBook {
    pub venue: Pubkey,
    pub gmtrade_program: Pubkey,
    pub collateral_mint: Pubkey,
    pub collateral_decimals: u8,
    pub bump: u8,
    pub market_count: u8,
    pub padding: u8,
    pub collateral_price: OracleConfig,
    pub markets: [MarketEntry; MAX_MARKETS],
}

impl MarketBook {
    pub fn address(venue: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[MARKET_BOOK_SEED, venue.as_ref()], &crate::ID).0
    }

    pub fn listed(&self) -> &[MarketEntry] {
        &self.markets[..self.market_count as usize]
    }

    pub fn leg_of(&self, market: &Pubkey) -> Option<(u8, &MarketEntry)> {
        self.listed().iter().enumerate().find(|(_, e)| e.market == *market).map(|(leg, e)| (leg as u8, e))
    }

    pub(crate) fn upsert(&mut self, entry: MarketEntry) -> Result<u8> {
        if let Some((leg, _)) = self.leg_of(&entry.market) {
            self.markets[leg as usize] = entry;
            return Ok(leg);
        }
        let leg = self.market_count as usize;
        require!(leg < MAX_MARKETS, OracleError::MarketBookFull);
        self.markets[leg] = entry;
        self.market_count += 1;
        Ok(leg as u8)
    }

    pub fn loader<'info>(info: &'info AccountInfo<'info>, venue: &Pubkey) -> Result<AccountLoader<'info, Self>> {
        require_keys_eq!(*info.key, Self::address(venue), OracleError::MarketNotInBook);
        AccountLoader::try_from(info)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Collateral {
    pub price: Price,
    pub decimals: u8,
}

impl Collateral {
    fn value(self, amount: u128, round_up: bool) -> Result<u128> {
        self.price.value_of(amount, self.decimals, round_up)
    }
}

#[inline(never)]
fn venue_account_price(book: &MarketBook, query: &PriceQuery, accounts: &[AccountInfo], clock: &Clock) -> Result<PriceResult> {
    require_keys_eq!(query.asset, book.venue, OracleError::NotAVenueHolding);
    require!(query.venue_account != Pubkey::default(), OracleError::NotAVenueHolding);
    require!(query.legs >> book.market_count == 0, OracleError::MarketNotInBook);
    let venue_account = &query.venue_account;
    let mint = &book.collateral_mint;
    let program = &book.gmtrade_program;
    let collateral = match query.exposure_only {
        true => None,
        false => {
            let price = get_price(mint, &book.collateral_price, accounts, clock)?;
            Some((Collateral { price: price.price, decimals: book.collateral_decimals }, price.checks))
        }
    };
    let mut checks = collateral.map_or(PriceChecks::ALL, |(_, checks)| checks);

    let idle = token_balance(find_account(accounts, &associated_token_address(venue_account, mint))?, mint, venue_account)?;
    let mut value = match collateral {
        Some((c, _)) => c.value(idle as u128, false)?,
        None => 0,
    };
    let mut open_legs = 0u64;
    for (leg, entry) in book.listed().iter().enumerate() {
        if query.legs & (1 << leg) == 0 {
            continue;
        }
        let market = read_market(find_account(accounts, &entry.market)?, program)?;
        require_keys_eq!(market.market_token, entry.market_token, OracleError::MarketNotInBook);
        let collateral_side = market.token_side(mint).ok_or(OracleError::MarketNotInBook)?;
        let index = match collateral {
            Some(_) => {
                let price = get_price(&market.index_token, &entry.index_price, accounts, clock)?;
                checks = checks.intersection(price.checks);
                Some(price.price)
            }
            None => None,
        };
        for is_long in [true, false] {
            let order = order_address(program, &market.store, venue_account, leg as u8, is_long);
            let pending = order_exists(find_account(accounts, &order)?, program)?;
            let escrowed = match pending {
                true => token_balance(find_account(accounts, &associated_token_address(&order, mint))?, mint, &order)?,
                false => 0,
            };
            let position_key = position_address(program, &market.store, venue_account, &entry.market_token, mint, is_long);
            let position = read_position(find_account(accounts, &position_key)?, program, venue_account, &entry.market_token, mint, is_long)?
                .unwrap_or_default();
            if pending || !position.is_empty() {
                open_legs |= 1 << leg;
            }
            if let (Some((c, _)), Some(index)) = (collateral, index) {
                let equity = position_equity(&position, is_long, collateral_side, &market, index, entry.index_decimals, c)?;
                value = value
                    .checked_add(c.value(escrowed as u128, false)?)
                    .and_then(|v| v.checked_add(equity))
                    .ok_or(OracleError::MathOverflow)?;
            }
        }
    }
    Ok(PriceResult { value, checks: checks.0, open_legs })
}

fn usd_to_nano(usd: u128, round_up: bool) -> u128 {
    let factor = USD_UNIT / 10u128.pow(USD_VALUE_DECIMALS);
    match round_up {
        true => usd.div_ceil(factor),
        false => usd / factor,
    }
}

pub fn position_equity(
    position: &PositionState,
    is_long: bool,
    collateral_side: bool,
    market: &MarketState,
    index_price: Price,
    index_decimals: u8,
    collateral: Collateral,
) -> Result<u128> {
    if position.is_empty() {
        return Ok(0);
    }
    let side = if is_long { 0 } else { 1 };
    let collateral_value = collateral.value(position.collateral_amount, false)?;
    let tokens_value = index_price.value_of(position.size_in_tokens, index_decimals, !is_long)?;
    let size_usd = usd_to_nano(position.size_in_usd, is_long);
    let size_usd_up = usd_to_nano(position.size_in_usd, true);

    let borrowing_delta = market.borrowing_factor[side].saturating_sub(position.borrowing_factor);
    let borrowing_fee = mul_div_ceil(size_usd_up, borrowing_delta, USD_UNIT)?;
    let funding_delta = market.funding_per_size[side][if collateral_side { 0 } else { 1 }]
        .saturating_sub(position.funding_fee_amount_per_size);
    let funding_amount = mul_div_ceil(position.size_in_usd.div_ceil(FUNDING_ADJUSTMENT), funding_delta, USD_UNIT)?;
    let funding_fee = collateral.value(funding_amount, true)?;
    let close_fee = mul_div_ceil(size_usd_up, market.close_fee_factor, USD_UNIT)?;

    let (gain, cost) = match is_long {
        true => (tokens_value, size_usd),
        false => (size_usd, tokens_value),
    };
    let assets = collateral_value.checked_add(gain).ok_or(OracleError::MathOverflow)?;
    let liabilities = [borrowing_fee, funding_fee, close_fee]
        .into_iter()
        .try_fold(cost, |sum, fee| sum.checked_add(fee))
        .ok_or(OracleError::MathOverflow)?;
    Ok(assets.saturating_sub(liabilities))
}

pub fn collateral_usd_at_par(amount: u128, decimals: u8) -> Result<u128> {
    let scale = checked_pow10(20u32.checked_sub(decimals as u32).ok_or(OracleError::MathOverflow)?)?;
    amount.checked_mul(scale).ok_or_else(|| OracleError::MathOverflow.into())
}

pub fn get_venue_account_price<'info>(query: &PriceQuery, accounts: &'info [AccountInfo<'info>], clock: &Clock) -> Result<PriceResult> {
    let loader = MarketBook::loader(find_account(accounts, &MarketBook::address(&query.asset))?, &query.asset)?;
    let book = loader.load()?;
    venue_account_price(&book, query, accounts, clock)
}
