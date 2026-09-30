use crate::common::kamino::set_token_balance;
use crate::common::*;
use crate::env::*;
use crate::gm::*;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use litesvm::types::TransactionResult;
use solana_signer::Signer as SvmSigner;
use vanna_oracle::OracleError;
use vanna_validator::ValidatorError;
use vanna_credit_layer::errors::VannaError;

fn ready_trader(env: &mut Env) -> Trader {
    let t = env.trader(1_000 * UNIT);
    env.prepare(&t, &ETH, true);
    env.create_escrow(&t, &ETH, true);
    t
}

fn long_with(env: &mut Env, t: &Trader, params: &OrderParams, edit: impl Fn(&mut Vec<AccountMeta>)) -> TransactionResult {
    let mut cpi = create_order_accounts(&t.venue_account, &ETH, true, true);
    edit(&mut cpi);
    env.execute(t, order_data(&ETH, params), &cpi)
}

#[test]
fn the_output_must_return_to_the_venue_account() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let thief = Pubkey::new_unique();
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let res = long_with(&mut env, &t, &params, |cpi| cpi[1] = AccountMeta::new_readonly(thief, false));
    assert_vanna_error(res, VannaError::InvalidCallAccounts);
}

#[test]
fn only_the_books_collateral() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let other_mint = Pubkey::new_unique();
    let res = long_with(&mut env, &t, &params, |cpi| cpi[8] = AccountMeta::new_readonly(other_mint, false));
    assert_custom_error(res, ValidatorError::InvalidCallAccounts.into());
}

#[test]
fn another_margins_venue_account_cannot_be_used() {
    let mut env = setup();
    let victim = ready_trader(&mut env);
    let t = ready_trader(&mut env);
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let res = long_with(&mut env, &t, &params, |cpi| cpi[0] = AccountMeta::new(victim.venue_account, false));
    assert_vanna_error(res, VannaError::InvalidCallAccounts);
    let mut cpi = create_order_accounts(&t.venue_account, &ETH, true, false);
    cpi[6] = AccountMeta::new(position_account(&victim.venue_account, &ETH, true), false);
    let res = env.execute(&t, order_data(&ETH, &market_order(false, true, 0, usd(300))), &cpi);
    assert_custom_error(res, vanna_oracle::OracleError::InvalidGmTradeAccount.into());
}

#[test]
fn only_market_orders_in_their_markets_and_sides_slot() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let limit = OrderParams { kind: 5, ..market_order(true, true, 100 * UNIT, usd(300)) };
    assert_custom_error(long_with(&mut env, &t, &limit, |_| {}), ValidatorError::CallNotAllowed.into());

    let params = market_order(true, true, 100 * UNIT, usd(300));
    let cpi = create_order_accounts(&t.venue_account, &ETH, true, true);
    for nonce in [vanna_oracle::gmtrade_accounts::order_nonce(ETH.leg, false), vanna_oracle::gmtrade_accounts::order_nonce(BTC.leg, true)] {
        let res = env.execute(&t, create_order_data(nonce, &params, None), &cpi);
        assert_custom_error(res, ValidatorError::CallNotAllowed.into());
    }
}

#[test]
fn leverage_is_capped_per_market() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let over = market_order(true, true, 100 * UNIT, usd(501));
    assert_custom_error(env.order(&t, &ETH, &over), ValidatorError::LeverageTooHigh.into());
    env.order(&t, &ETH, &market_order(true, true, 100 * UNIT, usd(500))).expect("exactly 5x");

    let (eth, _) = snapshot_prices();
    env.execute_increase(&t, &ETH, true, &open_position(&env, &ETH, true, 500.0, 100.0, eth));
    let withdraw = market_order(false, true, 10 * UNIT, 0);
    assert_custom_error(env.order(&t, &ETH, &withdraw), ValidatorError::LeverageTooHigh.into());
    env.order(&t, &ETH, &market_order(false, true, 10 * UNIT, usd(100))).expect("smaller and less collateral: 4.4x");
}

#[test]
fn a_disabled_venue_still_lets_traders_close() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    env.order(&t, &ETH, &market_order(true, true, 100 * UNIT, usd(300))).unwrap();
    let (eth, _) = snapshot_prices();
    env.execute_increase(&t, &ETH, true, &open_position(&env, &ETH, true, 300.0, 100.0, eth));

    let a = env.admin.pubkey();
    send(&mut env.svm, &env.admin, &[ix_enable_venue(&a, false)], &[]).unwrap();
    let more = market_order(true, true, 100 * UNIT, usd(300));
    assert_vanna_error(env.order(&t, &ETH, &more), VannaError::AssetNotCollateralEnabled);
    env.order(&t, &ETH, &market_order(false, true, 0, usd(300))).expect("closing is always allowed");
}

#[test]
fn the_valuation_needs_every_account_of_the_venue_account() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let short_position = position_account(&t.venue_account, &ETH, false);
    let rest: Vec<AccountMeta> = env.execute_rest(&t).into_iter().filter(|m| m.pubkey != short_position).collect();
    let ix = ix_margin_execute(&t.user.pubkey(), &GMTRADE, Some(&STORE), order_data(&ETH, &params), &create_order_accounts(&t.venue_account, &ETH, true, true), &rest, 0);
    let res = send(&mut env.svm, &t.user, &[compute_budget(), fund_venue_account(&t.user.pubkey(), &t.venue_account), ix_create_vault(&t.user.pubkey(), &t.venue_account, &USDC, &anchor_spl::token::ID), ix], &[]);
    assert_custom_error(res, OracleError::InvalidPriceSource.into());
}

#[test]
fn only_the_registered_agents_are_asked() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let params = market_order(true, true, 100 * UNIT, usd(300));
    let cpi = create_order_accounts(&t.venue_account, &ETH, true, true);
    let impostor = anchor_spl::token::ID;
    let rest: Vec<AccountMeta> = env
        .execute_rest(&t)
        .into_iter()
        .map(|m| if m.pubkey == ORACLE { AccountMeta::new_readonly(impostor, false) } else { m })
        .collect();
    let ix = ix_margin_execute(&t.user.pubkey(), &GMTRADE, Some(&STORE), order_data(&ETH, &params), &cpi, &rest, 0);
    let res = send(&mut env.svm, &t.user, &[compute_budget(), fund_venue_account(&t.user.pubkey(), &t.venue_account), ix_create_vault(&t.user.pubkey(), &t.venue_account, &USDC, &anchor_spl::token::ID), ix], &[]);
    assert_vanna_error(res, VannaError::InvalidAgentAccounts);

    let rest = env.execute_rest(&t);
    let mut ix = ix_margin_execute(&t.user.pubkey(), &GMTRADE, Some(&STORE), order_data(&ETH, &params), &cpi, &rest, 0);
    ix.accounts[5].pubkey = ORACLE;
    let res = send(&mut env.svm, &t.user, &[compute_budget(), fund_venue_account(&t.user.pubkey(), &t.venue_account), ix_create_vault(&t.user.pubkey(), &t.venue_account, &USDC, &anchor_spl::token::ID), ix], &[]);
    assert_vanna_error(res, VannaError::InvalidAgentAccounts);

    let mut ix = ix_public_venue_settle(&t.user.pubkey(), &t.margin, &t.venue_account);
    ix.accounts[9].pubkey = VALIDATOR;
    assert_vanna_error(send(&mut env.svm, &t.user, &[ix], &[]), VannaError::InvalidAgentAccounts);
}

#[test]
fn a_venue_is_never_a_token() {
    let mut env = setup();
    let t = env.trader(1_000 * UNIT);
    let u = t.user.pubkey();
    set_token_balance(&mut env.svm, &u, &USDC, 1_000);
    assert!(send(&mut env.svm, &t.user, &[ix_user_deposit_collateral(&u, &t.margin, &STORE, 1_000)], &[]).is_err());
    let a = env.admin.pubkey();
    let pool = ix_admin_initialize_reserve(&a, &a, &STORE, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0);
    assert!(send(&mut env.svm, &env.admin, &[pool], &[]).is_err());
    let borrowable = ix_admin_update_asset_config(&a, &STORE, 0, 0, 10_000, 0, true, true);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[borrowable], &[]), VannaError::NotAToken);
    let second = ix_register_venue(&a, &Pubkey::new_unique(), &STORE, &ORACLE);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[second], &[]), VannaError::NotAToken);
}

#[test]
fn settle_moves_funds_only_into_the_venue_accounts_own_margin() {
    let mut env = setup();
    let t = ready_trader(&mut env);
    let other = env.trader(10 * UNIT);
    let caller = funded_keypair(&mut env.svm);
    let ix = ix_public_venue_settle(&caller.pubkey(), &other.margin, &t.venue_account);
    assert_vanna_error(send(&mut env.svm, &caller, &[ix], &[]), VannaError::InvalidVenueAccount);
}

#[test]
fn the_market_book_checks_markets_against_gmtrade() {
    let mut svm = setup_svm();
    load_gmtrade(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_open_market_book(&a, usdc_oracle())], &[]).expect("book");

    let list = |index: &Pubkey, cap: u32| ix_list_market(&a, &ETH, index, eth_oracle(), cap, true);
    let res = send(&mut svm, &admin, &[list(&USDC, 50_000)], &[]);
    assert_custom_error(res, OracleError::MarketNotInBook.into());
    for cap in [5_000, 1_000_001] {
        assert_custom_error(send(&mut svm, &admin, &[list(&ETH.index, cap)], &[]), OracleError::InvalidLeverageCap.into());
    }

    let mut not_a_market = list(&ETH.index, 50_000);
    not_a_market.accounts[3].pubkey = USDC;
    assert_custom_error(send(&mut svm, &admin, &[not_a_market], &[]), vanna_oracle::OracleError::InvalidGmTradeAccount.into());
    let stranger = funded_keypair(&mut svm);
    let ix = ix_list_market(&stranger.pubkey(), &ETH, &ETH.index, eth_oracle(), 50_000, true);
    assert_vanna_error(send(&mut svm, &stranger, &[ix], &[]), VannaError::Unauthorized);

    send(&mut svm, &admin, &[list(&ETH.index, 50_000)], &[]).expect("the real market");
    send(&mut svm, &admin, &[ix_list(&a, &BTC, true)], &[]).expect("the BTC market");
    send(&mut svm, &admin, &[list(&ETH.index, 20_000)], &[]).expect("new cap");
    let account = svm.get_account(&market_book()).unwrap();
    let book: &vanna_oracle::MarketBook = bytemuck::from_bytes(&account.data[8..]);
    assert_eq!(book.market_count, 2);
    assert_eq!(book.leg_of(&ETH.market).map(|(leg, e)| (leg, e.max_leverage_bps)), Some((ETH.leg, 20_000)));
    assert_eq!(book.leg_of(&BTC.market).map(|(leg, _)| leg), Some(BTC.leg));
}
