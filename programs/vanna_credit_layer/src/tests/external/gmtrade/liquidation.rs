use crate::common::kamino::set_token_balance;
use crate::common::*;
use crate::env::*;
use crate::gm::*;
use crate::valuation::{expected_equity, hf_of};
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_validator::ValidatorError;
use vanna_credit_layer::errors::VannaError;
use vanna_credit_layer::events::{VenueUnwound, Liquidated};

fn levered_long(env: &mut Env) -> (Trader, Position) {
    let t = env.trader(200 * UNIT);
    env.borrow(&t, 1_000 * UNIT).unwrap();
    let position = env.open(&t, &ETH, true, 1_199 * UNIT, 4_796);
    (t, position)
}

fn unwind_ixs(env: &Env, liquidator: &Keypair, t: &Trader, m: &Market, is_long: bool, data: Vec<u8>, cpi: &[AccountMeta]) -> Vec<Instruction> {
    let mut rest = env.groups(t, &[], false);
    rest.extend(venue_account_oracle_segment(&t.venue_account));
    rest.extend(validator_segment());
    let escrow = anchor_spl::associated_token::spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &liquidator.pubkey(),
        &order_account(&t.venue_account, m, is_long),
        &USDC,
        &anchor_spl::token::ID,
    );
    vec![
        compute_budget(),
        escrow,
        fund_venue_account(&liquidator.pubkey(), &t.venue_account),
        ix_public_venue_unwind(&liquidator.pubkey(), &t.margin, data, cpi, &rest),
    ]
}

fn close_position_ixs(env: &Env, liquidator: &Keypair, t: &Trader, m: &Market, is_long: bool, size_usd: u128) -> Vec<Instruction> {
    let data = order_data(m, &market_order(false, is_long, 0, size_usd));
    unwind_ixs(env, liquidator, t, m, is_long, data, &create_order_accounts(&t.venue_account, m, is_long, false))
}

fn liquidate_ix(liquidator: &Keypair, t: &Trader) -> Instruction {
    let l = liquidator.pubkey();
    let usdc_account = get_associated_token_address(&l, &USDC);
    let venue = LiqPosition {
        health: venue_group(&t.venue_account),
        settlement: vec![
            AccountMeta::new_readonly(USDC, false),
            AccountMeta::new(usdc_account, false),
            AccountMeta::new_readonly(anchor_spl::token::ID, false),
        ],
    };
    let positions = [
        liq_collateral(&USDC, &anchor_spl::token::ID, &t.margin, &usdc_account),
        venue,
        liq_debt(&USDC, &t.margin, &usdc_account),
    ];
    let mut ix = ix_public_liquidate(&l, &t.margin, &positions, &venue_account_keys(&t.venue_account));
    let idle = idle_account(&t.venue_account);
    ix.accounts.iter_mut().filter(|m| m.pubkey == idle).for_each(|m| m.is_writable = true);
    ix
}

#[test]
fn a_liquidatable_margin_unwinds_its_venue_account_then_is_liquidated() {
    let mut env = setup();
    let (t, position) = levered_long(&mut env);
    let liquidator = funded_keypair(&mut env.svm);
    set_token_balance(&mut env.svm, &liquidator.pubkey(), &USDC, 2_000 * UNIT);

    let ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, position.size_in_usd);
    assert_vanna_error(send(&mut env.svm, &liquidator, &ixs, &[]), VannaError::PositionHealthy);

    let (eth, usdc) = snapshot_prices();
    env.set_eth_price(eth * 0.97);
    let hf = hf_of(usdc + expected_equity(&position, true, eth * 0.97, usdc), 1_000.0 * usdc);
    assert!(hf < 1.1 && hf > 1.0, "{hf}");
    let health = env.health(&t, &[USDC], false);
    let probe = ix_user_withdraw_collateral(&t.user.pubkey(), &t.margin, &USDC, 1, 0, &health);
    assert_vanna_error(send(&mut env.svm, &t.user, &[compute_budget(), probe], &[]), VannaError::HealthFactorTooLow);

    let liquidate = liquidate_ix(&liquidator, &t);
    let res = send(&mut env.svm, &liquidator, &[compute_budget(), liquidate.clone()], &[]);
    assert_vanna_error(res, VannaError::VenuePositionsOpen);

    let ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, position.size_in_usd);
    let res = send(&mut env.svm, &liquidator, &ixs, &[]).expect("anyone unwinds a liquidatable margin's venue_account");
    let unwound = event::<VenueUnwound>(&res.logs);
    assert_eq!((unwound.venue, unwound.caller, unwound.program_id), (STORE, liquidator.pubkey(), GMTRADE));
    let order = env.svm.get_account(&order_account(&t.venue_account, &ETH, true)).expect("close order placed");
    assert_eq!(order.owner, GMTRADE);

    env.execute_close(&t, &ETH, true, 1_053 * UNIT);
    let before = token_balance(&env.svm, &get_associated_token_address(&liquidator.pubkey(), &USDC));
    let res = send(&mut env.svm, &liquidator, &[compute_budget(), liquidate], &[]).expect("liquidation");
    let liquidated = event::<Liquidated>(&res.logs);
    assert_eq!((liquidated.collaterals_seized, liquidated.debts_repaid), (2, 1));
    let after = token_balance(&env.svm, &get_associated_token_address(&liquidator.pubkey(), &USDC));
    assert_eq!(after + 1_000 * UNIT, before + 1_054 * UNIT);
    let margin = fetch_margin(&env.svm, &t.user.pubkey());
    assert!(margin.is_empty());
    assert_eq!(env.legs(&t), 0);
    assert_eq!(token_balance_or_zero(&env.svm, &idle_account(&t.venue_account)), 0);
}

#[test]
fn an_unwind_only_closes_whole_positions() {
    let mut env = setup();
    let (t, position) = levered_long(&mut env);
    let liquidator = funded_keypair(&mut env.svm);
    let (eth, _) = snapshot_prices();
    env.set_eth_price(eth * 0.97);
    let refused = |env: &mut Env, params: OrderParams| {
        let increase = params.kind == vanna_oracle::gmtrade_accounts::MARKET_INCREASE;
        let cpi = create_order_accounts(&t.venue_account, &ETH, true, increase);
        let ixs = unwind_ixs(env, &liquidator, &t, &ETH, true, order_data(&ETH, &params), &cpi);
        assert_custom_error(send(&mut env.svm, &liquidator, &ixs, &[]), ValidatorError::CallNotAllowed.into());
    };
    let size = position.size_in_usd;
    refused(&mut env, market_order(false, true, 0, size / 2));
    refused(&mut env, market_order(true, true, 0, usd(100)));
    refused(&mut env, OrderParams { acceptable_price: Some(1), ..market_order(false, true, 0, size) });
    refused(&mut env, OrderParams { min_output: Some(1), ..market_order(false, true, 0, size) });

    let ixs = unwind_ixs(&env, &liquidator, &t, &ETH, true, prepare_user_data(), &prepare_user_accounts(&t.venue_account));
    assert_custom_error(send(&mut env.svm, &liquidator, &ixs, &[]), ValidatorError::CallNotAllowed.into());

    let other = Pubkey::new_unique();
    let mut ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, size);
    let unwind = ixs.last_mut().unwrap();
    unwind.accounts.iter_mut().filter(|m| m.pubkey == t.venue_account).take(1).for_each(|m| m.pubkey = other);
    assert!(send(&mut env.svm, &liquidator, &ixs, &[]).is_err());

    let mut ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, size);
    let unwind = ixs.last_mut().unwrap();
    let venue_config = asset_config_pda(&STORE).0;
    let at = unwind.accounts.iter().rposition(|m| m.pubkey == venue_config).unwrap();
    unwind.accounts.remove(at);
    assert!(send(&mut env.svm, &liquidator, &ixs, &[]).is_err());
}

#[test]
fn a_venue_account_trading_two_markets_is_unwound_market_by_market() {
    let mut env = setup();
    let t = env.trader(200 * UNIT);
    env.borrow(&t, 1_000 * UNIT).unwrap();
    let eth_long = env.open(&t, &ETH, true, 600 * UNIT, 2_400);
    let btc_long = env.open(&t, &BTC, true, 599 * UNIT, 2_396);
    assert_eq!(env.legs(&t), 0b11);
    let liquidator = funded_keypair(&mut env.svm);
    set_token_balance(&mut env.svm, &liquidator.pubkey(), &USDC, 2_000 * UNIT);

    let (eth, _) = snapshot_prices();
    env.set_eth_price(eth * 0.97);
    set_btc_price(&mut env.svm, BTC_PRICE * 0.97);

    let ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, eth_long.size_in_usd);
    send(&mut env.svm, &liquidator, &ixs, &[]).expect("unwind ETH");
    env.execute_close(&t, &ETH, true, 520 * UNIT);
    let liquidate = liquidate_ix(&liquidator, &t);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[compute_budget(), liquidate.clone()], &[]), VannaError::VenuePositionsOpen);

    let ixs = close_position_ixs(&env, &liquidator, &t, &BTC, true, btc_long.size_in_usd);
    send(&mut env.svm, &liquidator, &ixs, &[]).expect("unwind BTC");
    env.execute_close(&t, &BTC, true, 530 * UNIT);

    let before = token_balance(&env.svm, &get_associated_token_address(&liquidator.pubkey(), &USDC));
    send(&mut env.svm, &liquidator, &[compute_budget(), liquidate], &[]).expect("liquidation");
    let after = token_balance(&env.svm, &get_associated_token_address(&liquidator.pubkey(), &USDC));
    assert_eq!(after + 1_000 * UNIT, before + (1 + 520 + 530) * UNIT);
    assert!(fetch_margin(&env.svm, &t.user.pubkey()).is_empty());
    assert_eq!(env.legs(&t), 0);
}

#[test]
fn tokens_sent_to_an_empty_order_slot_neither_count_nor_stall_liquidation() {
    let mut env = setup();
    let (t, position) = levered_long(&mut env);
    let (eth, _) = snapshot_prices();

    let short_escrow = escrow_account(&t.venue_account, &ETH, false);
    set_token_balance(&mut env.svm, &order_account(&t.venue_account, &ETH, false), &USDC, 5_000 * UNIT);
    assert_eq!(token_balance(&env.svm, &short_escrow), 5_000 * UNIT);

    let liquidator = funded_keypair(&mut env.svm);
    set_token_balance(&mut env.svm, &liquidator.pubkey(), &USDC, 2_000 * UNIT);
    env.set_eth_price(eth * 0.97);
    let ixs = close_position_ixs(&env, &liquidator, &t, &ETH, true, position.size_in_usd);
    send(&mut env.svm, &liquidator, &ixs, &[]).expect("still liquidatable");
    env.execute_close(&t, &ETH, true, 1_053 * UNIT);
    let liquidate = liquidate_ix(&liquidator, &t);
    send(&mut env.svm, &liquidator, &[compute_budget(), liquidate], &[]).expect("the empty slot does not stall liquidation");
}
