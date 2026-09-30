use crate::common::*;
use crate::env::*;
use crate::gm::*;
use crate::valuation::{assert_close, expected_equity, hf_of};
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use solana_signer::Signer as SvmSigner;
use vanna_validator::ValidatorError;
use vanna_credit_layer::events::VenueSettled;
use vanna_oracle::OracleError;

#[test]
fn one_venue_account_holds_positions_in_every_market() {
    let mut env = setup();
    let t = env.trader(2_000 * UNIT);
    env.borrow(&t, 2_000 * UNIT).unwrap();
    let eth_long = env.open(&t, &ETH, true, 600 * UNIT, 2_400);
    assert_eq!(env.legs(&t), 0b01);
    let btc_short = env.open(&t, &BTC, false, 500 * UNIT, 2_000);
    assert_eq!(env.legs(&t), 0b11, "the BTC order added its market to the venue_account's legs");
    assert!(env.svm.get_account(&position_account(&t.venue_account, &BTC, false)).is_some());

    let (eth, usdc) = snapshot_prices();
    let hf = |env: &Env, eth: f64, btc: f64| {
        let margin_usdc = env.margin_usdc(&t) as f64 / 1e6 * usdc;
        let venue_account = expected_equity(&eth_long, true, eth, usdc) + expected_equity(&btc_short, false, btc, usdc);
        hf_of(margin_usdc + venue_account, 2_000.0 * usdc)
    };
    assert_close(hf(&env, eth, BTC_PRICE), env.health_factor(&t));

    set_btc_price(&mut env.svm, BTC_PRICE * 1.1);
    let btc_up = env.health_factor(&t);
    assert_close(hf(&env, eth, BTC_PRICE * 1.1), btc_up);
    env.set_eth_price(eth * 1.1);
    let both_up = env.health_factor(&t);
    assert_close(hf(&env, eth * 1.1, BTC_PRICE * 1.1), both_up);
    assert!(both_up > btc_up, "the ETH long gains");
}

#[test]
fn settling_stops_tracking_markets_left_empty() {
    let mut env = setup();
    let t = env.trader(2_000 * UNIT);
    let eth_long = env.open(&t, &ETH, true, 400 * UNIT, 1_200);
    let btc_long = env.open(&t, &BTC, true, 300 * UNIT, 900);
    assert_eq!(env.legs(&t), 0b11);

    env.close(&t, &ETH, true, &eth_long, 410 * UNIT);
    let settled = event::<VenueSettled>(&env.settle(&t.user, &t).unwrap().logs);
    assert_eq!((settled.swept, settled.open_legs, settled.closed), (410 * UNIT, 0b10, false));
    assert_eq!(env.legs(&t), 0b10);
    assert!(env.is_venue_active(&t));
    assert_eq!(env.margin_usdc(&t), 1_300 * UNIT + 410 * UNIT);

    env.close(&t, &BTC, true, &btc_long, 290 * UNIT);
    let settled = event::<VenueSettled>(&env.settle(&t.user, &t).unwrap().logs);
    assert_eq!((settled.swept, settled.open_legs, settled.closed), (290 * UNIT, 0, true));
    assert_eq!(env.legs(&t), 0);
    assert!(!env.is_venue_active(&t));
    assert_eq!(env.margin_usdc(&t), 2_000 * UNIT);
}

#[test]
fn a_pending_order_keeps_its_market_tracked() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.prepare(&t, &BTC, false);
    env.order(&t, &BTC, &market_order(true, false, 200 * UNIT, usd(400))).unwrap();
    let settled = event::<VenueSettled>(&env.settle(&t.user, &t).unwrap().logs);
    assert_eq!((settled.swept, settled.open_legs, settled.closed), (0, 0b10, false));
    assert_eq!(env.legs(&t), 0b10);
}

#[test]
fn a_tracked_market_cannot_be_left_out_of_a_valuation() {
    let mut env = setup();
    let t = env.trader(2_000 * UNIT);
    env.borrow(&t, 1_000 * UNIT).unwrap();
    env.open(&t, &ETH, true, 400 * UNIT, 1_200);
    env.open(&t, &BTC, true, 400 * UNIT, 1_200);

    for hidden in [position_account(&t.venue_account, &BTC, true), BTC.market] {
        let health: Vec<AccountMeta> = env.health(&t, &[USDC], false).into_iter().filter(|m| m.pubkey != hidden).collect();
        let ix = ix_user_withdraw_collateral(&t.user.pubkey(), &t.margin, &USDC, UNIT, 0, &health);
        assert_custom_error(send(&mut env.svm, &t.user, &[compute_budget(), ix], &[]), OracleError::InvalidPriceSource.into());
    }
}

#[test]
fn a_market_is_decreased_only_once_opened() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.open(&t, &ETH, true, 200 * UNIT, 600);
    env.prepare(&t, &BTC, true);
    let decrease = market_order(false, true, 0, usd(100));
    assert_custom_error(env.order(&t, &BTC, &decrease), ValidatorError::CallNotAllowed.into());
    env.order(&t, &ETH, &market_order(false, true, 0, usd(100))).expect("ETH is open");
}

#[test]
fn trading_can_be_switched_off_per_market() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    let btc_long = env.open(&t, &BTC, true, 200 * UNIT, 600);
    env.prepare(&t, &ETH, true);

    let a = env.admin.pubkey();
    send(&mut env.svm, &env.admin, &[ix_list(&a, &BTC, false)], &[]).expect("BTC trading off");
    let more = market_order(true, true, 100 * UNIT, usd(300));
    assert_custom_error(env.order(&t, &BTC, &more), ValidatorError::TradingDisabled.into());
    env.order(&t, &ETH, &more).expect("ETH still trades");
    env.order(&t, &BTC, &market_order(false, true, 0, btc_long.size_in_usd)).expect("BTC can still be closed");
}

#[test]
fn each_market_has_its_own_leverage_cap() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    let a = env.admin.pubkey();
    send(&mut env.svm, &env.admin, &[ix_list_market(&a, &BTC, &BTC.index, btc_oracle(), 20_000, true)], &[]).unwrap();
    env.prepare(&t, &BTC, true);
    env.prepare(&t, &ETH, true);
    let three_x = market_order(true, true, 100 * UNIT, usd(300));
    assert_custom_error(env.order(&t, &BTC, &three_x), ValidatorError::LeverageTooHigh.into());
    env.order(&t, &BTC, &market_order(true, true, 100 * UNIT, usd(200))).expect("2x BTC");
    env.order(&t, &ETH, &three_x).expect("3x ETH");
}

#[test]
fn only_listed_markets_trade() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.prepare(&t, &ETH, true);
    env.create_escrow(&t, &ETH, true);
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let mut cpi = create_order_accounts(&t.venue_account, &ETH, true, true);
    cpi[3] = AccountMeta::new(Pubkey::new_unique(), false);
    let res = env.execute(&t, order_data(&ETH, &params), &cpi);
    assert_custom_error(res, ValidatorError::MarketNotListed.into());
}
