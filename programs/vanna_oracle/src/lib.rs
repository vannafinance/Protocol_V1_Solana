pub mod config;
pub mod gmtrade;
pub mod gmtrade_accounts;
pub mod klend;
pub mod price;
pub mod pricing;
pub mod pyth;
pub mod reference;
pub mod scope;
pub mod tokens;

pub use config::{OracleConfig, EMPTY_SCOPE_CHAIN, SCOPE_CHAIN_UNUSED};
pub use gmtrade::{
    collateral_usd_at_par, position_equity, Collateral, MarketBook, MarketEntry, MARKET_BOOK_SEED, MAX_LEVERAGE_BPS,
    MAX_MARKETS,
};
pub use price::{OraclePrice, Price};
pub use pricing::{find_account, get_price, validate_config};
pub use tokens::{PriceBook, PriceSource, MAX_SOURCES, PRICE_BOOK_SEED};

use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;
use gmtrade_accounts::read_market;
use vanna_credit_layer::interface::{require_protocol_admin, PriceResult, PriceQuery};

declare_id!("FXY5DRfekMUTbUp4uyCFpCZmCccrp6kPq3hTM4GRihnc");

#[program]
pub mod vanna_oracle {
    use super::*;

    pub fn get_price<'info>(
        ctx: Context<'info, GetPrice>,
        queries: Vec<PriceQuery>,
    ) -> Result<Vec<PriceResult>> {
        let accounts = ctx.remaining_accounts;
        let clock = Clock::get()?;
        let is_token = |query: &PriceQuery| query.venue_account == Pubkey::default();
        let price_book = match queries.iter().any(is_token) {
            true => Some(AccountLoader::<PriceBook>::try_from(find_account(accounts, &tokens::price_book_address())?)?),
            false => None,
        };
        let price_book = price_book.as_ref().map(|loader| loader.load()).transpose()?;
        queries
            .iter()
            .map(|query| match &price_book {
                Some(book) if is_token(query) => tokens::get_token_price(book, query, accounts, &clock),
                _ => gmtrade::get_venue_account_price(query, accounts, &clock),
            })
            .collect()
    }

    pub fn open_price_book(ctx: Context<OpenPriceBook>) -> Result<()> {
        require_protocol_admin(&ctx.accounts.protocol_config, &ctx.accounts.admin)?;
        let mut book = ctx.accounts.price_book.load_init()?;
        book.bump = ctx.bumps.price_book;
        Ok(())
    }

    pub fn set_price_source<'info>(ctx: Context<'info, SetPriceSource<'info>>, config: OracleConfig) -> Result<()> {
        require_protocol_admin(&ctx.accounts.protocol_config, &ctx.accounts.admin)?;
        let mint = ctx.accounts.mint.key();
        let mut book = ctx.accounts.price_book.load_mut()?;
        let underlying = match config.uses_klend() {
            true => {
                let reserve = find_account(ctx.remaining_accounts, &config.klend_reserve)?;
                let rate = klend::read_reserve_rate(reserve, &config.klend_program, &mint)?;
                let source = book.find(&rate.liquidity_mint).ok_or(OracleError::UnknownAsset)?;
                require!(source.decimals == rate.liquidity_decimals, OracleError::InvalidKaminoAccounts);
                Some((source.mint, source.config))
            }
            false => None,
        };
        validate_config(
            &mint,
            &config,
            underlying.as_ref().map(|(m, c)| (m, c)),
            ctx.remaining_accounts,
            &Clock::get()?,
        )?;
        book.upsert(PriceSource { mint, decimals: ctx.accounts.mint.decimals, padding: [0; 3], config })?;
        emit!(PriceSourceSet { mint, config });
        Ok(())
    }

    pub fn open_market_book<'info>(
        ctx: Context<'info, OpenMarketBook<'info>>,
        venue: Pubkey,
        gmtrade_program: Pubkey,
        collateral_price: OracleConfig,
    ) -> Result<()> {
        require_protocol_admin(&ctx.accounts.protocol_config, &ctx.accounts.admin)?;
        let mint = ctx.accounts.collateral_mint.key();
        validate_config(&mint, &collateral_price, None, ctx.remaining_accounts, &Clock::get()?)?;
        let mut book = ctx.accounts.market_book.load_init()?;
        book.venue = venue;
        book.gmtrade_program = gmtrade_program;
        book.collateral_mint = mint;
        book.collateral_decimals = ctx.accounts.collateral_mint.decimals;
        book.collateral_price = collateral_price;
        book.bump = ctx.bumps.market_book;
        Ok(())
    }

    pub fn list_market<'info>(
        ctx: Context<'info, ListMarket<'info>>,
        index_price: OracleConfig,
        max_leverage_bps: u32,
        trading_enabled: bool,
    ) -> Result<()> {
        require_protocol_admin(&ctx.accounts.protocol_config, &ctx.accounts.admin)?;
        let mut book = ctx.accounts.market_book.load_mut()?;
        let market = read_market(&ctx.accounts.market, &book.gmtrade_program)?;
        require_keys_eq!(market.store, book.venue, OracleError::MarketNotInBook);
        require_keys_eq!(market.index_token, ctx.accounts.index_mint.key(), OracleError::MarketNotInBook);
        require!(market.token_side(&book.collateral_mint).is_some(), OracleError::MarketNotInBook);
        require!((10_000..=MAX_LEVERAGE_BPS).contains(&max_leverage_bps), OracleError::InvalidLeverageCap);
        validate_config(&market.index_token, &index_price, None, ctx.remaining_accounts, &Clock::get()?)?;

        let entry = MarketEntry {
            market: ctx.accounts.market.key(),
            market_token: market.market_token,
            index_decimals: ctx.accounts.index_mint.decimals,
            trading_enabled: trading_enabled as u8,
            padding: [0; 2],
            max_leverage_bps,
            index_price,
        };
        let leg = book.upsert(entry)?;
        emit!(MarketListed { venue: book.venue, leg, market: entry.market, max_leverage_bps, trading_enabled });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct GetPrice {}

#[derive(Accounts)]
pub struct OpenPriceBook<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Vanna's `ProtocolConfig`, read for its admin.
    pub protocol_config: UncheckedAccount<'info>,
    #[account(init, payer = payer, space = 8 + std::mem::size_of::<PriceBook>(), seeds = [PRICE_BOOK_SEED], bump)]
    pub price_book: AccountLoader<'info, PriceBook>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetPriceSource<'info> {
    pub admin: Signer<'info>,
    /// CHECK: Vanna's `ProtocolConfig`, read for its admin.
    pub protocol_config: UncheckedAccount<'info>,
    #[account(mut, seeds = [PRICE_BOOK_SEED], bump = price_book.load()?.bump)]
    pub price_book: AccountLoader<'info, PriceBook>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
}

#[derive(Accounts)]
#[instruction(venue: Pubkey)]
pub struct OpenMarketBook<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Vanna's `ProtocolConfig`, read for its admin.
    pub protocol_config: UncheckedAccount<'info>,
    #[account(init, payer = payer, space = 8 + std::mem::size_of::<MarketBook>(), seeds = [MARKET_BOOK_SEED, venue.as_ref()], bump)]
    pub market_book: AccountLoader<'info, MarketBook>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ListMarket<'info> {
    pub admin: Signer<'info>,
    /// CHECK: Vanna's `ProtocolConfig`, read for its admin.
    pub protocol_config: UncheckedAccount<'info>,
    #[account(mut, seeds = [MARKET_BOOK_SEED, market_book.load()?.venue.as_ref()], bump = market_book.load()?.bump)]
    pub market_book: AccountLoader<'info, MarketBook>,
    /// CHECK: the GMTrade market; read and checked in the handler.
    pub market: UncheckedAccount<'info>,
    pub index_mint: Box<InterfaceAccount<'info, Mint>>,
}

#[event]
pub struct PriceSourceSet {
    pub mint: Pubkey,
    pub config: OracleConfig,
}

#[event]
pub struct MarketListed {
    pub venue: Pubkey,
    pub leg: u8,
    pub market: Pubkey,
    pub max_leverage_bps: u32,
    pub trading_enabled: bool,
}

#[error_code]
pub enum OracleError {
    #[msg("Price must be positive")]
    InvalidPrice,
    #[msg("Math underflow")]
    MathUnderflow,
    #[msg("Division by zero")]
    DivisionByZero,
    #[msg("Oracle account is not owned by the expected program")]
    InvalidOracleOwner,
    #[msg("Oracle account is not a valid price feed")]
    InvalidPriceFeed,
    #[msg("No configured source returned a usable price")]
    PriceUnavailable,
    #[msg("Price source account is missing or does not match the configured one")]
    InvalidPriceSource,
    #[msg("Oracle configuration is invalid")]
    InvalidOracleConfig,
    #[msg("This price source is not supported for this asset")]
    UnsupportedPriceSource,
    #[msg("Kamino reserve account does not match the receipt or its layout")]
    InvalidKaminoAccounts,
    #[msg("The price book is full")]
    PriceBookFull,
    #[msg("Token has no price source in the book")]
    UnknownAsset,
    #[msg("A token query must not name a venue_account or legs")]
    NotATokenHolding,
    #[msg("Account is not the expected GMTrade market, position or order")]
    InvalidGmTradeAccount,
    #[msg("GMTrade instruction data is malformed")]
    MalformedInstruction,
    #[msg("The market book is full")]
    MarketBookFull,
    #[msg("Market is not this venue's, not in the book, or does not take its collateral")]
    MarketNotInBook,
    #[msg("Leverage cap must be between 1x and 100x")]
    InvalidLeverageCap,
    #[msg("Not a venue_account of this venue")]
    NotAVenueHolding,
    #[msg("Math overflow")]
    MathOverflow,
}
