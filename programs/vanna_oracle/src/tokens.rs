use crate::config::OracleConfig;
use crate::pricing::get_price;
use crate::OracleError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{PriceResult, PriceQuery};

pub const PRICE_BOOK_SEED: &[u8] = b"price_book";
pub const MAX_SOURCES: usize = 32;

#[zero_copy]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PriceSource {
    pub mint: Pubkey,
    pub decimals: u8,
    pub padding: [u8; 3],
    pub config: OracleConfig,
}

#[account(zero_copy)]
pub struct PriceBook {
    pub bump: u8,
    pub count: u8,
    pub padding: [u8; 2],
    pub sources: [PriceSource; MAX_SOURCES],
}

impl PriceBook {
    pub fn find(&self, mint: &Pubkey) -> Option<&PriceSource> {
        self.sources[..self.count as usize].iter().find(|source| source.mint == *mint)
    }

    pub(crate) fn upsert(&mut self, source: PriceSource) -> Result<()> {
        let count = self.count as usize;
        match self.sources[..count].iter_mut().find(|existing| existing.mint == source.mint) {
            Some(existing) => *existing = source,
            None => {
                require!(count < MAX_SOURCES, OracleError::PriceBookFull);
                self.sources[count] = source;
                self.count += 1;
            }
        }
        Ok(())
    }
}

pub fn get_token_price(book: &PriceBook, query: &PriceQuery, accounts: &[AccountInfo], clock: &Clock) -> Result<PriceResult> {
    require!(query.legs == 0 && !query.exposure_only, OracleError::NotATokenHolding);
    let source = book.find(&query.asset).ok_or(OracleError::UnknownAsset)?;
    let price = get_price(&source.mint, &source.config, accounts, clock)?;
    Ok(PriceResult {
        value: price.value_of(query.amount, source.decimals, query.liability)?,
        checks: price.checks.0,
        open_legs: 0,
    })
}

pub fn price_book_address() -> Pubkey {
    Pubkey::find_program_address(&[PRICE_BOOK_SEED], &crate::ID).0
}
