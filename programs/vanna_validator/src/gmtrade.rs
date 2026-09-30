use crate::ValidatorError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{find_account, Binding, CallContext, CallMode, CallPermit, Funding, PermitSigner, Role};
use vanna_oracle::gmtrade_accounts::{
    order_nonce, read_market, read_position, PositionState, Reader, CLOSE_EMPTY_POSITION, CLOSE_ORDER_V2,
    CREATE_ORDER_V2, MARKET_DECREASE, MARKET_INCREASE, NO_SWAP, PREPARE_POSITION, PREPARE_USER,
};
use vanna_oracle::{collateral_usd_at_par, MarketBook, MarketEntry};

const CREATE_ORDER_ACCOUNT_COUNT: usize = 25;
const CLOSE_ORDER_ACCOUNT_COUNT: usize = 30;
const BASIS_POINTS: u128 = 10_000;

pub fn review<'info>(context: &CallContext, call: &[AccountInfo<'info>], extra: &'info [AccountInfo<'info>]) -> Result<CallPermit> {
    let venue = context.venue;
    let book_info = find_account(extra, &MarketBook::address(&venue)).ok_or(ValidatorError::UnknownProgram)?;
    let loader = MarketBook::loader(book_info, &venue)?;
    let book = loader.load()?;
    require_keys_eq!(context.target_program, book.gmtrade_program, ValidatorError::UnknownProgram);
    require!(context.venue_account != Pubkey::default(), ValidatorError::CallNotAllowed);
    permit_for(context, call, &book)
}

pub fn permit_for(context: &CallContext, call: &[AccountInfo], book: &MarketBook) -> Result<CallPermit> {
    let data = &context.data;
    require!(data.len() >= 8, ValidatorError::CallNotAllowed);
    let unset = |i: usize| call.get(i).is_some_and(|a| a.key == &book.gmtrade_program);
    let venue_account_only = |bindings: &[u16]| CallPermit {
        signer: PermitSigner::VenueAccount,
        bindings: bindings.iter().map(|index| Binding { index: *index, role: Role::VenueAccount }).collect(),
        tokens_in: vec![],
        tokens_out: vec![],
        funding: None,
        opens_leg: None,
    };
    let owner_only = || -> Result<()> {
        require!(context.mode == CallMode::Owner, ValidatorError::CallNotAllowed);
        Ok(())
    };
    let mut reader = Reader::new(&data[8..]);
    match data[..8].try_into().unwrap() {
        PREPARE_USER => {
            owner_only()?;
            require!(call.len() == 4, ValidatorError::InvalidCallAccounts);
            reader.finish()?;
            Ok(venue_account_only(&[0]))
        }
        CLOSE_EMPTY_POSITION => {
            owner_only()?;
            require!(call.len() == 3, ValidatorError::InvalidCallAccounts);
            reader.finish()?;
            Ok(venue_account_only(&[0]))
        }
        PREPARE_POSITION => {
            owner_only()?;
            require!(call.len() == 5, ValidatorError::InvalidCallAccounts);
            let params = reader.order_params()?;
            reader.finish()?;
            require!(matches!(params.kind, MARKET_INCREASE | MARKET_DECREASE), ValidatorError::CallNotAllowed);
            listed_market(call, 2, params.is_collateral_long, book)?;
            Ok(venue_account_only(&[0]))
        }
        CLOSE_ORDER_V2 => {
            owner_only()?;
            require!(call.len() == CLOSE_ORDER_ACCOUNT_COUNT, ValidatorError::InvalidCallAccounts);
            reader.string()?;
            reader.finish()?;
            require!((24..=27).all(unset), ValidatorError::InvalidCallAccounts);
            Ok(venue_account_only(&[0, 3, 4]))
        }
        CREATE_ORDER_V2 => {
            require!(call.len() == CREATE_ORDER_ACCOUNT_COUNT, ValidatorError::InvalidCallAccounts);
            let nonce = reader.bytes32()?;
            let params = reader.order_params()?;
            let callback_version = reader.option(Reader::u8)?;
            reader.finish()?;
            require!(callback_version.is_none() && (19..=22).all(unset), ValidatorError::CallNotAllowed);
            order_permit(context, call, book, &nonce, &params, &unset)
        }
        _ => err!(ValidatorError::CallNotAllowed),
    }
}

fn listed_market<'a>(call: &[AccountInfo], index: usize, is_collateral_long: bool, book: &'a MarketBook) -> Result<(u8, &'a MarketEntry)> {
    let info = call.get(index).ok_or(ValidatorError::InvalidCallAccounts)?;
    let (leg, entry) = book.leg_of(info.key).ok_or(ValidatorError::MarketNotListed)?;
    let market = read_market(info, &book.gmtrade_program)?;
    let side_token = if is_collateral_long { market.long_token } else { market.short_token };
    require_keys_eq!(side_token, book.collateral_mint, ValidatorError::InvalidCallAccounts);
    Ok((leg, entry))
}

fn order_permit(
    context: &CallContext,
    call: &[AccountInfo],
    book: &MarketBook,
    nonce: &[u8; 32],
    params: &vanna_oracle::gmtrade_accounts::OrderParams,
    unset: &dyn Fn(usize) -> bool,
) -> Result<CallPermit> {
    let increase = match params.kind {
        MARKET_INCREASE => true,
        MARKET_DECREASE => false,
        _ => return err!(ValidatorError::CallNotAllowed),
    };
    require!(
        params.swap_path_length == 0
            && params.decrease_position_swap_type.is_none_or(|swap| swap == NO_SWAP)
            && params.trigger_price.is_none()
            && params.valid_from_ts.is_none()
            && !params.should_unwrap_native_token,
        ValidatorError::CallNotAllowed
    );
    let (leg, entry) = listed_market(call, 3, params.is_collateral_long, book)?;
    require!(*nonce == order_nonce(leg, params.is_long), ValidatorError::CallNotAllowed);
    require!(!unset(6), ValidatorError::InvalidCallAccounts);
    let collateral = book.collateral_mint;
    require_keys_eq!(call[8].key(), collateral, ValidatorError::InvalidCallAccounts);

    let position = read_position(&call[6], &book.gmtrade_program, &context.venue_account, &entry.market_token, &collateral, params.is_long)?
        .unwrap_or_default();
    let amount = params.initial_collateral_delta_amount;
    let bindings = vec![Binding { index: 0, role: Role::VenueAccount }, Binding { index: 1, role: Role::VenueAccount }];
    let leg_bit = 1u64 << leg;

    if context.mode == CallMode::Unwind {
        require!(!increase && position.size_in_usd > 0, ValidatorError::CallNotAllowed);
        require!(
            params.size_delta_value == position.size_in_usd
                && amount == 0
                && params.acceptable_price.is_none()
                && params.min_output.is_none(),
            ValidatorError::CallNotAllowed
        );
    }
    let (funding, opens_leg) = if increase {
        require!(entry.trading_enabled(), ValidatorError::TradingDisabled);
        require!(!unset(11), ValidatorError::InvalidCallAccounts);
        require_keys_eq!(call[7].key(), collateral, ValidatorError::InvalidCallAccounts);
        let funding = match amount {
            0 => {
                require!(unset(15), ValidatorError::InvalidCallAccounts);
                None
            }
            _ => Some(Funding { index: 15, amount }),
        };
        (funding, Some(leg))
    } else {
        require!(unset(7) && unset(11) && unset(15), ValidatorError::InvalidCallAccounts);
        require!(context.open_legs & leg_bit != 0, ValidatorError::CallNotAllowed);
        (None, None)
    };
    check_leverage(increase, params.size_delta_value, amount, &position, entry.max_leverage_bps, book.collateral_decimals)?;
    Ok(CallPermit {
        signer: PermitSigner::VenueAccount,
        bindings,
        tokens_in: vec![],
        tokens_out: vec![],
        funding,
        opens_leg,
    })
}

pub fn check_leverage(
    increase: bool,
    size_delta_usd: u128,
    collateral_delta: u64,
    position: &PositionState,
    max_leverage_bps: u32,
    collateral_decimals: u8,
) -> Result<()> {
    let delta = collateral_delta as u128;
    let (size, collateral) = match increase {
        true => (
            position.size_in_usd.checked_add(size_delta_usd).ok_or(ValidatorError::MathOverflow)?,
            position.collateral_amount.checked_add(delta).ok_or(ValidatorError::MathOverflow)?,
        ),
        false => {
            if delta == 0 {
                return Ok(());
            }
            (position.size_in_usd.saturating_sub(size_delta_usd), position.collateral_amount.saturating_sub(delta))
        }
    };
    if size == 0 {
        return Ok(());
    }
    let limit = collateral_usd_at_par(collateral, collateral_decimals)?
        .checked_mul(max_leverage_bps as u128)
        .ok_or(ValidatorError::MathOverflow)?;
    let exposure = size.checked_mul(BASIS_POINTS).ok_or(ValidatorError::MathOverflow)?;
    require!(exposure <= limit, ValidatorError::LeverageTooHigh);
    Ok(())
}
