use crate::env::*;
use crate::gm::*;
use vanna_oracle::gmtrade_accounts::{MarketState, PositionState};
use vanna_oracle::{position_equity, Collateral};
use vanna_oracle::Price;

pub(crate) fn expected_equity(p: &Position, is_long: bool, index: f64, usdc: f64) -> f64 {
    let market = fixture(&ETH.market).data;
    let u128_at = |i: usize| u128::from_le_bytes(market[i..i + 16].try_into().unwrap()) as f64;
    let pool = |index: usize| 8 + 1952 + 64 * index;
    let cumulative_borrowing = u128_at(pool(8) + if is_long { 32 } else { 48 });
    let funding_per_size = (u128_at(pool(if is_long { 9 } else { 10 }) + 32) / 2.0).ceil();
    let close_fee_factor = u128_at(8 + 560) / 1e20;

    let size = p.size_in_usd as f64 / 1e20;
    let value = p.size_in_tokens as f64 / 1e8 * index;
    let pnl = if is_long { value - size } else { size - value };
    let borrowing = size * (cumulative_borrowing - p.borrowing_factor as f64).max(0.0) / 1e20;
    let funding_usdc = p.size_in_usd as f64 * (funding_per_size - p.funding_fee_amount_per_size as f64).max(0.0) / 1e30 / 1e6;
    let equity = p.collateral_amount as f64 / 1e6 * usdc + pnl - borrowing - funding_usdc * usdc - size * close_fee_factor;
    equity.max(0.0)
}

pub(crate) fn hf_of(collateral_usd: f64, debt_usd: f64) -> f64 {
    collateral_usd / debt_usd
}

pub(crate) fn assert_close(expected: f64, actual_wad: u128) {
    let actual = actual_wad as f64 / 1e18;
    assert!((expected - actual).abs() / expected < 1e-6, "expected HF {expected}, program {actual}");
}

fn market() -> MarketState {
    MarketState {
        store: STORE,
        market_token: ETH.market_token,
        index_token: ETH.index,
        long_token: USDC,
        short_token: USDC,
        close_fee_factor: 24 * USD / 100_000,
        borrowing_factor: [2 * USD / 100, 3 * USD / 100],
        funding_per_size: [[5_000_000_000_000, 5_000_000_000_000], [0, 0]],
    }
}

fn usd_nano(dollars: f64) -> u128 {
    (dollars * 1e9).round() as u128
}

const USDC_AT_PAR: Collateral = Collateral { price: Price { value: 100_000_000, exponent: -8 }, decimals: 6 };

fn eth(dollars: u64) -> Price {
    Price { value: dollars * 100_000_000, exponent: -8 }
}

fn long_position() -> PositionState {
    PositionState {
        size_in_tokens: 100_000_000,
        collateral_amount: 400_000_000,
        size_in_usd: usd(2_000),
        borrowing_factor: USD / 100,
        funding_fee_amount_per_size: 0,
    }
}

#[test]
fn equity_is_collateral_plus_pnl_less_fees() {
    let m = market();
    let equity = position_equity(&long_position(), true, true, &m, eth(2_100), 8, USDC_AT_PAR).unwrap();
    assert_eq!(equity, usd_nano(400.0 + 100.0 - 20.0 - 1.0 - 0.48));

    let equity = position_equity(&long_position(), false, true, &m, eth(2_100), 8, USDC_AT_PAR).unwrap();
    assert_eq!(equity, usd_nano(400.0 - 100.0 - 40.0 - 0.48));
}

#[test]
fn equity_floors_at_zero_and_an_empty_position_is_worth_nothing() {
    let m = market();
    assert_eq!(position_equity(&long_position(), true, true, &m, eth(1_600), 8, USDC_AT_PAR).unwrap(), 0);
    assert_eq!(position_equity(&PositionState::default(), true, true, &m, eth(2_000), 8, USDC_AT_PAR).unwrap(), 0);
}

#[test]
fn fees_already_paid_are_not_charged_again() {
    let m = market();
    let settled = PositionState {
        borrowing_factor: m.borrowing_factor[0],
        funding_fee_amount_per_size: 5_000_000_000_000,
        ..long_position()
    };
    let equity = position_equity(&settled, true, true, &m, eth(2_000), 8, USDC_AT_PAR).unwrap();
    assert_eq!(equity, usd_nano(400.0 - 0.48));
}

#[test]
fn matches_the_floating_point_model_on_live_positions() {
    let (eth_price, usdc) = snapshot_prices();
    let price = Price { value: (eth_price * 1e8).round() as u64, exponent: -8 };
    let usdc_price = Collateral { price: Price { value: (usdc * 1e8).round() as u64, exponent: -8 }, decimals: 6 };
    let market_account = fixture(&ETH.market);
    let (mut lamports, mut data) = (market_account.lamports, market_account.data.clone());
    let info = anchor_lang::prelude::AccountInfo::new(&ETH.market, false, false, &mut lamports, &mut data, &GMTRADE, false);
    let m = vanna_oracle::gmtrade_accounts::read_market(&info, &GMTRADE).unwrap();
    for is_long in [true, false] {
        let live = live_position(is_long);
        let program = position_equity(&live.state(), is_long, true, &m, price, 8, usdc_price).unwrap() as f64 / 1e9;
        let model = expected_equity(&live, is_long, eth_price, usdc);
        assert!((program - model).abs() < 1e-6 * model.max(1.0), "{program} vs {model}");
        let side = if is_long { "long" } else { "short" };
        println!("live {side}: ${:.2} size, ${:.2} equity", live.size_in_usd as f64 / 1e20, program);
    }
}

#[test]
fn live_positions_count_toward_the_margin_health_factor() {
    let mut env = setup();
    let t = env.trader(2_000 * UNIT);
    env.borrow(&t, 3_000 * UNIT).unwrap();
    env.prepare(&t, &ETH, true);
    env.prepare(&t, &ETH, false);
    env.order(&t, &ETH, &market_order(true, true, 100 * UNIT, usd(300))).unwrap();
    let (long, short) = (live_position(true), live_position(false));
    env.execute_increase(&t, &ETH, true, &long);
    write_position(&mut env.svm, &t.venue_account, &ETH, false, &short);

    let (eth, usdc) = snapshot_prices();
    let margin_usdc = env.margin_usdc(&t) as f64 / 1e6 * usdc;
    let venue_account = expected_equity(&long, true, eth, usdc) + expected_equity(&short, false, eth, usdc);
    assert_close(hf_of(margin_usdc + venue_account, 3_000.0 * usdc), env.health_factor(&t));
}
