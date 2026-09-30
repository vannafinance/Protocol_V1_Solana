use crate::common::*;
use crate::env::*;
use crate::gm::*;
use crate::valuation::{assert_close, expected_equity, hf_of};
use vanna_credit_layer::events::{MarginExecuted, TokenAmount, VenueSettled};

#[test]
fn opens_a_long_and_a_short_through_margin_execute() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.borrow(&t, 1_000 * UNIT).expect("borrow against 1,000 USDC");
    let before = env.health_factor(&t);

    env.prepare(&t, &ETH, true);
    env.prepare(&t, &ETH, false);
    assert!(!env.is_venue_active(&t), "preparing accounts opens no exposure");
    let long = env.order(&t, &ETH, &market_order(true, true, 400 * UNIT, usd(1_200))).expect("3x long");
    let spent = event::<MarginExecuted>(&long.logs);
    assert_eq!(spent.spent, vec![TokenAmount { mint: USDC, amount: 400 * UNIT }]);
    assert!(spent.received.is_empty());
    env.order(&t, &ETH, &market_order(true, false, 300 * UNIT, usd(900))).expect("3x short");

    assert_eq!(token_balance_or_zero(&env.svm, &escrow_account(&t.venue_account, &ETH, true)), 400 * UNIT);
    assert_eq!(token_balance_or_zero(&env.svm, &escrow_account(&t.venue_account, &ETH, false)), 300 * UNIT);
    assert_eq!(env.margin_usdc(&t), 1_300 * UNIT);
    assert!(env.is_venue_active(&t));
    assert_eq!(env.legs(&t), 1 << ETH.leg, "only the ETH market is tracked");
    for is_long in [true, false] {
        let order = env.svm.get_account(&order_account(&t.venue_account, &ETH, is_long)).expect("order created");
        assert_eq!(order.owner, GMTRADE);
    }

    assert_eq!(env.health_factor(&t), before);
}

#[test]
fn a_cancelled_order_refunds_the_venue_account_and_settles_back() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.borrow(&t, 500 * UNIT).unwrap();
    let before = env.health_factor(&t);
    env.prepare(&t, &ETH, true);
    env.order(&t, &ETH, &market_order(true, true, 600 * UNIT, usd(1_800))).unwrap();

    env.execute(&t, close_order_data("cancel"), &close_order_accounts(&t.venue_account, &ETH, true, true)).expect("owner cancels");
    assert_eq!(token_balance_or_zero(&env.svm, &idle_account(&t.venue_account)), 600 * UNIT, "refund lands in the venue_account");
    assert!(env.svm.get_account(&order_account(&t.venue_account, &ETH, true)).is_none_or(|a| a.data.is_empty()), "order closed");
    assert_eq!(env.health_factor(&t), before, "idle collateral still counts");

    let stranger = funded_keypair(&mut env.svm);
    let venue_account_lamports = env.svm.get_account(&t.venue_account).unwrap().lamports;
    let res = env.settle(&stranger, &t).expect("settle");
    let settled = event::<VenueSettled>(&res.logs);
    assert_eq!((settled.swept, settled.lamports_refunded, settled.open_legs, settled.closed), (600 * UNIT, 0, 0, true));
    assert_eq!(env.margin_usdc(&t), 1_500 * UNIT);
    assert!(!env.is_venue_active(&t));
    assert_eq!(env.legs(&t), 0);
    assert_eq!(env.health_factor(&t), before);

    let settled = event::<VenueSettled>(&env.settle(&t.user, &t).expect("settle again").logs);
    assert_eq!((settled.swept, settled.lamports_refunded), (0, venue_account_lamports));
    assert!(env.svm.get_account(&t.venue_account).is_none_or(|a| a.lamports == 0));
}

#[test]
fn an_executed_long_is_valued_at_its_equity_and_closes_back_into_the_margin() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.borrow(&t, 1_000 * UNIT).unwrap();
    let position = env.open(&t, &ETH, true, 500 * UNIT, 2_000);
    let (eth, usdc) = snapshot_prices();
    let margin_usdc = env.margin_usdc(&t) as f64 / 1e6 * usdc;
    let equity = expected_equity(&position, true, eth, usdc);
    assert_close(hf_of(margin_usdc + equity, 1_000.0 * usdc), env.health_factor(&t));

    env.set_eth_price(eth * 1.1);
    let equity_up = expected_equity(&position, true, eth * 1.1, usdc);
    assert!((equity_up - equity - 200.0).abs() < 1.0, "{equity_up} vs {equity}");
    assert_close(hf_of(margin_usdc + equity_up, 1_000.0 * usdc), env.health_factor(&t));

    env.close(&t, &ETH, true, &position, 699 * UNIT);
    let settled = event::<VenueSettled>(&env.settle(&t.user, &t).unwrap().logs);
    assert!(settled.closed);
    assert_eq!(env.margin_usdc(&t), 1_500 * UNIT + 699 * UNIT);
    assert!(!env.is_venue_active(&t));
}

#[test]
fn a_short_gains_when_eth_falls() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.borrow(&t, 1_000 * UNIT).unwrap();
    let position = env.open(&t, &ETH, false, 400 * UNIT, 1_600);
    let (eth, usdc) = snapshot_prices();
    let margin_usdc = env.margin_usdc(&t) as f64 / 1e6 * usdc;
    let at_entry = env.health_factor(&t);
    assert_close(hf_of(margin_usdc + expected_equity(&position, false, eth, usdc), 1_000.0 * usdc), at_entry);

    env.set_eth_price(eth * 0.95);
    let after = env.health_factor(&t);
    assert!(after > at_entry);
    assert_close(hf_of(margin_usdc + expected_equity(&position, false, eth * 0.95, usdc), 1_000.0 * usdc), after);
}

#[test]
fn an_empty_position_account_can_be_closed_and_the_market_left() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    env.prepare(&t, &ETH, true);
    env.order(&t, &ETH, &market_order(true, true, 100 * UNIT, usd(300))).unwrap();
    env.execute_close(&t, &ETH, true, 100 * UNIT);
    env.settle(&t.user, &t).unwrap();
    assert!(!env.is_venue_active(&t));

    advance_time(&mut env.svm, 3_600);
    let (eth, _) = snapshot_prices();
    env.set_eth_price(eth);
    let res = env.execute(&t, close_empty_position_data(), &close_empty_position_accounts(&t.venue_account, &ETH, true));
    match res {
        Ok(_) => assert!(env.svm.get_account(&position_account(&t.venue_account, &ETH, true)).is_none_or(|a| a.data.is_empty())),
        Err(e) => panic!("close_empty_position: {:?}", e.meta.logs),
    }
    assert!(!env.is_venue_active(&t), "a call that places no order opens no exposure");
}
