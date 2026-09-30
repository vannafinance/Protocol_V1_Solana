use crate::env::{usd, MAX_LEVERAGE_BPS};
use crate::gm::*;
use anchor_lang::prelude::{AccountInfo, Pubkey};
use vanna_oracle::gmtrade_accounts::order_nonce;
use vanna_oracle::{MarketBook, MarketEntry};
use vanna_validator::gmtrade::{check_leverage, permit_for};
use vanna_credit_layer::interface::{Binding, CallContext, CallMode, CallPermit, Funding, PermitSigner, Role};

const UNIT: u64 = 1_000_000;

fn book() -> Box<MarketBook> {
    let mut book: Box<MarketBook> = Box::new(bytemuck::Zeroable::zeroed());
    book.venue = STORE;
    book.gmtrade_program = GMTRADE;
    book.collateral_mint = USDC;
    book.collateral_decimals = 6;
    for m in MARKETS {
        book.markets[m.leg as usize] = MarketEntry {
            market: m.market,
            market_token: m.market_token,
            index_decimals: 8,
            trading_enabled: 1,
            padding: [0; 2],
            max_leverage_bps: MAX_LEVERAGE_BPS,
            index_price: Default::default(),
        };
    }
    book.market_count = MARKETS.len() as u8;
    book
}

struct Acct {
    key: Pubkey,
    lamports: u64,
    data: Vec<u8>,
    owner: Pubkey,
}

fn accounts(keys: &[Pubkey]) -> Vec<Acct> {
    keys.iter()
        .map(|key| match *key == ETH.market || *key == BTC.market {
            true => Acct { key: *key, lamports: 1, data: fixture(&ETH.market).data, owner: GMTRADE },
            false => Acct { key: *key, lamports: 0, data: vec![], owner: anchor_lang::system_program::ID },
        })
        .collect()
}

const VENUE_ACCOUNT: Pubkey = Pubkey::new_from_array([0xDE; 32]);

fn context(mode: CallMode, data: &[u8], open_legs: u64) -> CallContext {
    CallContext {
        mode,
        target_program: GMTRADE,
        data: data.to_vec(),
        call_account_count: 0,
        margin: Pubkey::new_unique(),
        authority: Pubkey::new_unique(),
        venue: STORE,
        venue_account: VENUE_ACCOUNT,
        open_legs,
    }
}

fn review_as(mode: CallMode, data: &[u8], keys: &[Pubkey], open_legs: u64) -> anchor_lang::Result<CallPermit> {
    let mut accts = accounts(keys);
    let infos: Vec<AccountInfo> = accts
        .iter_mut()
        .map(|a| AccountInfo::new(&a.key, false, false, &mut a.lamports, &mut a.data, &a.owner, false))
        .collect();
    permit_for(&context(mode, data, open_legs), &infos, &book())
}

fn review(data: &[u8], keys: &[Pubkey], open_legs: u64) -> anchor_lang::Result<CallPermit> {
    review_as(CallMode::Owner, data, keys, open_legs)
}

fn refused(data: &[u8], keys: &[Pubkey]) -> bool {
    review(data, keys, 0b11).is_err()
}

fn order_keys(m: &Market, is_long: bool, unset: &[usize]) -> Vec<Pubkey> {
    let mut keys: Vec<Pubkey> = create_order_accounts(&VENUE_ACCOUNT, m, is_long, true).iter().map(|meta| meta.pubkey).collect();
    for i in unset {
        keys[*i] = GMTRADE;
    }
    keys
}

fn venue_account(index: u16) -> Binding {
    Binding { index, role: Role::VenueAccount }
}

#[test]
fn a_market_increase_is_funded_from_the_venue_account_and_opens_its_leg() {
    for m in MARKETS {
        let params = market_order(true, true, 250 * UNIT, usd(750));
        let permit = review(&order_data(&m, &params), &order_keys(&m, true, &[]), 0).unwrap();
        assert_eq!(
            permit,
            CallPermit {
                signer: PermitSigner::VenueAccount,
                bindings: vec![venue_account(0), venue_account(1)],
                tokens_in: vec![],
                tokens_out: vec![],
                funding: Some(Funding { index: 15, amount: 250 * UNIT }),
                opens_leg: Some(m.leg),
            }
        );
    }
}

#[test]
fn a_market_decrease_moves_nothing_in_and_needs_its_leg_open() {
    let params = market_order(false, false, 0, usd(100));
    let keys = order_keys(&BTC, false, &[7, 11, 15]);
    let permit = review(&order_data(&BTC, &params), &keys, 1 << BTC.leg).unwrap();
    assert_eq!((permit.funding, permit.opens_leg), (None, None));
    assert_eq!(permit.bindings, vec![venue_account(0), venue_account(1)]);
    assert!(review(&order_data(&BTC, &params), &keys, 1 << ETH.leg).is_err());
    assert!(refused(&order_data(&BTC, &params), &order_keys(&BTC, false, &[7, 11])));
    assert!(refused(&order_data(&BTC, &params), &order_keys(&BTC, false, &[11, 15])));
}

#[test]
fn an_increase_without_collateral_names_no_source() {
    let grow = market_order(true, true, 0, usd(100));
    assert!(refused(&order_data(&ETH, &grow), &order_keys(&ETH, true, &[15])));

    let params = market_order(true, true, 0, 0);
    let permit = review(&order_data(&ETH, &params), &order_keys(&ETH, true, &[15]), 0).unwrap();
    assert_eq!((permit.funding, permit.opens_leg), (None, Some(ETH.leg)));
    assert!(refused(&order_data(&ETH, &params), &order_keys(&ETH, true, &[])));
}

#[test]
fn only_plain_market_orders() {
    let base = market_order(true, true, UNIT, usd(1));
    let variants = [
        OrderParams { kind: 2, ..base },
        OrderParams { kind: 5, ..base },
        OrderParams { kind: 8, ..base },
        OrderParams { swap_path_length: 1, ..base },
        OrderParams { trigger_price: Some(1), ..base },
        OrderParams { valid_from_ts: Some(1), ..base },
        OrderParams { should_unwrap_native_token: true, ..base },
        OrderParams { decrease_position_swap_type: Some(1), ..base },
    ];
    for params in variants {
        assert!(refused(&order_data(&ETH, &params), &order_keys(&ETH, true, &[])), "{params:?}");
    }

    let limits = OrderParams { acceptable_price: Some(1), min_output: Some(1), execution_lamports: 1_000_000, ..base };
    assert!(review(&order_data(&ETH, &limits), &order_keys(&ETH, true, &[]), 0).is_ok());
}

#[test]
fn orders_use_their_markets_and_sides_slot_without_callbacks() {
    let long = market_order(true, true, UNIT, usd(1));
    let keys = order_keys(&ETH, true, &[]);
    assert!(refused(&create_order_data(order_nonce(ETH.leg, false), &long, None), &keys));
    assert!(refused(&create_order_data(order_nonce(BTC.leg, true), &long, None), &keys));
    assert!(refused(&create_order_data([7; 32], &long, None), &keys));
    assert!(refused(&create_order_data(order_nonce(ETH.leg, true), &long, Some(1)), &keys));
    let mut with_callback = keys.clone();
    with_callback[20] = Pubkey::new_unique();
    assert!(refused(&order_data(&ETH, &long), &with_callback));
}

#[test]
fn only_listed_markets_within_their_caps() {
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let mut keys = order_keys(&ETH, true, &[]);
    keys[3] = Pubkey::new_unique();
    assert!(refused(&order_data(&ETH, &params), &keys));
    let mut keys = order_keys(&ETH, true, &[]);
    keys[8] = Pubkey::new_unique();
    assert!(refused(&order_data(&ETH, &params), &keys), "the book's collateral only");
    let over = market_order(true, true, 100 * UNIT, usd(501));
    assert!(refused(&order_data(&ETH, &over), &order_keys(&ETH, true, &[])));
}

#[test]
fn the_account_list_and_data_must_be_exact() {
    let data = order_data(&ETH, &market_order(true, true, UNIT, usd(1)));
    let mut swap_path = order_keys(&ETH, true, &[]);
    swap_path.push(Pubkey::new_unique());
    assert!(refused(&data, &swap_path));
    assert!(refused(&data, &order_keys(&ETH, true, &[])[..24]));
    let mut trailing = data.clone();
    trailing.push(0);
    assert!(refused(&trailing, &order_keys(&ETH, true, &[])));
    assert!(refused(&data[..data.len() - 1], &order_keys(&ETH, true, &[])));
    assert!(refused(&data, &order_keys(&ETH, true, &[3])));
    assert!(refused(&data, &order_keys(&ETH, true, &[6])));
}

#[test]
fn setup_and_cleanup_calls_are_signed_by_the_venue_account() {
    let keys = |metas: Vec<anchor_lang::solana_program::instruction::AccountMeta>| metas.iter().map(|m| m.pubkey).collect::<Vec<_>>();
    let venue_account_only = |bindings: Vec<Binding>| CallPermit {
        signer: PermitSigner::VenueAccount,
        bindings,
        tokens_in: vec![],
        tokens_out: vec![],
        funding: None,
        opens_leg: None,
    };

    assert_eq!(review(&prepare_user_data(), &keys(prepare_user_accounts(&VENUE_ACCOUNT)), 0).unwrap(), venue_account_only(vec![venue_account(0)]));
    let prepare = prepare_position_data(&market_order(true, false, 0, 0));
    assert_eq!(review(&prepare, &keys(prepare_position_accounts(&VENUE_ACCOUNT, &BTC, false)), 0).unwrap(), venue_account_only(vec![venue_account(0)]));
    let mut unlisted = keys(prepare_position_accounts(&VENUE_ACCOUNT, &BTC, false));
    unlisted[2] = Pubkey::new_unique();
    assert!(refused(&prepare, &unlisted));
    let close_empty = keys(close_empty_position_accounts(&VENUE_ACCOUNT, &ETH, true));
    assert_eq!(review(&close_empty_position_data(), &close_empty, 0).unwrap(), venue_account_only(vec![venue_account(0)]));

    let mut close = keys(close_order_accounts(&VENUE_ACCOUNT, &ETH, true, true));
    assert_eq!(review(&close_order_data("cancel"), &close, 0b01).unwrap(), venue_account_only(vec![venue_account(0), venue_account(3), venue_account(4)]));
    close[25] = Pubkey::new_unique();
    assert!(refused(&close_order_data("cancel"), &close));
}

#[test]
fn an_unwind_may_only_close_a_whole_position() {
    let keys = order_keys(&ETH, true, &[7, 11, 15]);
    let close = market_order(false, true, 0, usd(100));
    assert!(review_as(CallMode::Unwind, &order_data(&ETH, &close), &keys, 0b01).is_err());
    let setup = prepare_user_accounts(&VENUE_ACCOUNT).iter().map(|m| m.pubkey).collect::<Vec<_>>();
    assert!(review_as(CallMode::Unwind, &prepare_user_data(), &setup, 0b01).is_err());
    let increase = market_order(true, true, 0, usd(100));
    assert!(review_as(CallMode::Unwind, &order_data(&ETH, &increase), &order_keys(&ETH, true, &[15]), 0b01).is_err());
}

#[test]
fn every_other_gmtrade_instruction_is_refused() {
    let others: [(&str, [u8; 8]); 5] = [
        ("update_order_v2", [195, 175, 207, 33, 171, 246, 41, 176]),
        ("create_deposit", [157, 30, 11, 129, 16, 166, 115, 75]),
        ("create_withdrawal", [247, 103, 160, 95, 42, 161, 108, 91]),
        ("create_glv_deposit", [170, 67, 137, 159, 159, 116, 48, 86]),
        ("claim_fees_from_market", [245, 167, 45, 29, 37, 215, 168, 32]),
    ];
    let keys: Vec<Pubkey> = (0..25).map(|_| Pubkey::new_unique()).collect();
    for (name, selector) in others {
        assert!(refused(&selector, &keys), "{name}");
    }
}

#[test]
fn leverage_is_size_over_collateral_at_par() {
    let empty = vanna_oracle::gmtrade_accounts::PositionState::default();
    assert!(check_leverage(true, usd(500), 100 * UNIT, &empty, 50_000, 6).is_ok());
    assert!(check_leverage(true, usd(501), 100 * UNIT, &empty, 50_000, 6).is_err());
    let open = vanna_oracle::gmtrade_accounts::PositionState { size_in_usd: usd(500), collateral_amount: 100_000_000, ..empty };
    assert!(check_leverage(false, usd(10), 0, &open, 50_000, 6).is_ok());
    assert!(check_leverage(false, usd(500), 100 * UNIT, &open, 50_000, 6).is_ok());
    assert!(check_leverage(false, 0, 10 * UNIT, &open, 50_000, 6).is_err());
    assert!(check_leverage(false, usd(100), 10 * UNIT, &open, 50_000, 6).is_ok());
}
