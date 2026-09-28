//! Leverage: borrowing from Vanna into Kamino, cToken valuation, whole-account liquidation of
//! Kamino positions, and the full 5x farm walkthrough.

use crate::common::kamino::*;
use crate::common::mainnet::*;
use crate::common::*;
use crate::env::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use solana_signer::Signer as SvmSigner;
use vanna_lending::errors::VannaError;

/// Leverage farming: borrow USDC from Vanna, supply it to Kamino, all health-checked with the
/// cUSDC valued through the real reserve.
#[test]
fn borrowed_usdc_can_be_supplied_to_kamino_and_stays_health_checked() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.open_usdc_debt(&user);
    env.borrow_usdc(&user, 2_000 * USDC, &[]).expect("borrow 2,000 against 1,000 (HF 1.5)");

    let debt = debt_group_metas(&MAINNET_USDC, &margin);
    env.supply(&user, &USDC_RESERVE, 2_500 * USDC, 1, &debt).expect("supply with open debt");
    assert!(env.is_active(&user, &USDC_RESERVE.collateral_mint));

    // Withdrawing every cToken to the wallet would leave ~$500 against $2,000 of debt.
    let usdc_group = collateral_group_metas(&MAINNET_USDC, &margin);
    let health: Vec<AccountMeta> = usdc_group.iter().chain(debt.iter()).cloned().collect();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    set_token_balance(&mut env.svm, &user.pubkey(), &USDC_RESERVE.collateral_mint, 0);
    let withdraw = |amount, oracles: &[Pubkey]| {
        ix_user_withdraw_collateral(&user.pubkey(), &margin, &USDC_RESERVE.collateral_mint, amount, 0, &with_oracles(health.clone(), oracles))
    };
    let ix = withdraw(receipts, &env.oracles());
    assert_vanna_error(send(&mut env.svm, &user, &[ix], &[]), VannaError::HealthFactorTooLow);
    // The receipt can't be valued without its reserve.
    let ix = withdraw(100 * USDC, &[env.usdc_price, env.sol_price]);
    assert_vanna_error(send(&mut env.svm, &user, &[ix], &[]), VannaError::InvalidPriceSource);
    // A small withdrawal keeps HF well above 1.10.
    let ix = withdraw(100 * USDC, &env.oracles());
    send(&mut env.svm, &user, &[ix], &[]).expect("small cUSDC withdrawal stays healthy");
}

/// cSOL (6 decimals) redeems for SOL (9 decimals). 10 SOL at $200 must count as ~$2,000: a
/// $25,000 borrow (HF ≈ 1.08) fails and a $15,000 borrow (HF ≈ 1.13) passes.
#[test]
fn csol_is_valued_at_sol_decimals_through_the_real_reserve() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    env.supply(&user, &SOL_RESERVE, 10 * SOL, 1, &[]).expect("supply SOL via margin_execute");
    env.open_usdc_debt(&user);

    let mut health = env.receipt_group(&margin, &SOL_RESERVE);
    if env.is_active(&user, &NATIVE_MINT) {
        health.extend(collateral_group_metas(&NATIVE_MINT, &margin)); // rounding dust
    }
    assert_vanna_error(env.borrow_usdc(&user, 25_000 * USDC, &health), VannaError::HealthFactorTooLow);
    env.borrow_usdc(&user, 15_000 * USDC, &health).expect("borrow against ~$2,000 of cSOL");
}

/// Liquidation accounts for every position of `margin`, in slot order, sweeping to the
/// liquidator's ATAs and repaying the USDC debt from its USDC ATA.
fn whole_account(env: &mut Env, user: &solana_keypair::Keypair, margin: &Pubkey, liquidator: &Pubkey) -> Vec<LiqPosition> {
    let assets = [MAINNET_USDC, NATIVE_MINT, USDC_RESERVE.collateral_mint, SOL_RESERVE.collateral_mint];
    let account = fetch_margin(&env.svm, &user.pubkey());
    let mut positions = Vec::new();
    for index in account.active_collateral_indexes() {
        let mint = assets.into_iter().find(|m| fetch_asset_config(&env.svm, m).asset_index == index).unwrap();
        let destination = get_associated_token_address(liquidator, &mint);
        if env.svm.get_account(&destination).is_none() {
            set_token_balance(&mut env.svm, liquidator, &mint, 0);
        }
        positions.push(liq_collateral(&mint, &anchor_spl::token::ID, margin, &destination));
    }
    let usdc_account = get_associated_token_address(liquidator, &MAINNET_USDC);
    positions.push(liq_debt(&MAINNET_USDC, margin, &usdc_account));
    positions
}

/// cSOL plus the borrowed USDC back a USDC debt. After SOL halves (HF ≈ 1.067), a liquidator with
/// no capital takes the whole account: the swept USDC repays the debt, and the swept cSOL redeems
/// at Kamino for the 10 SOL behind it.
#[test]
fn liquidator_takes_the_whole_account_including_ctokens() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    env.supply(&user, &SOL_RESERVE, 10 * SOL, 1, &[]).unwrap();
    env.open_usdc_debt(&user);
    let mut health = env.receipt_group(&margin, &SOL_RESERVE);
    if env.is_active(&user, &NATIVE_MINT) {
        health.extend(collateral_group_metas(&NATIVE_MINT, &margin));
    }
    env.borrow_usdc(&user, 15_000 * USDC, &health).unwrap();
    // SOL $200 -> $100: HF = (1,000 + 15,000) / 15,000 ≈ 1.067.
    set_price(&mut env.svm, &env.sol_price, WSOL_FEED, WSOL_PRICE / 2, 0, -8, FIXTURE_UNIX_TIMESTAMP);

    let liquidator = funded_keypair(&mut env.svm);
    let usdc_account = set_token_balance(&mut env.svm, &liquidator.pubkey(), &MAINNET_USDC, 0);
    let receipts = env.balance(&margin, &SOL_RESERVE.collateral_mint);
    let positions = whole_account(&mut env, &user, &margin, &liquidator.pubkey());
    let liquidate = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions, &env.oracles());
    send(&mut env.svm, &liquidator, &[liquidate], &[])
        .expect("liquidate the whole account");

    assert_eq!(token_balance(&env.svm, &usdc_account), 0, "the swept 15,000 USDC repaid the 15,000 USDC debt");
    let csol = get_associated_token_address(&liquidator.pubkey(), &SOL_RESERVE.collateral_mint);
    assert_eq!(token_balance(&env.svm, &csol), receipts, "every cSOL swept");
    let account = fetch_margin(&env.svm, &user.pubkey());
    assert_eq!((account.collateral_count, account.debt_count), (0, 0));

    // The liquidator redeems the cSOL at Kamino itself.
    let wsol = get_associated_token_address(&liquidator.pubkey(), &NATIVE_MINT);
    let wsol = if env.svm.get_account(&wsol).is_some() { wsol } else { set_token_balance(&mut env.svm, &liquidator.pubkey(), &NATIVE_MINT, 0) };
    let before = token_balance(&env.svm, &wsol);
    let accounts = kamino_redeem_accounts(&SOL_RESERVE, &liquidator.pubkey(), &csol, &wsol);
    send(&mut env.svm, &liquidator, &[kamino_direct_ix(kamino_call_data(REDEEM_RESERVE_COLLATERAL, receipts), accounts)], &[])
        .expect("liquidator redeems cSOL at klend");
    let lamports = token_balance(&env.svm, &wsol) - before;
    assert!(lamports <= 10 * SOL && lamports + 2_000 >= 10 * SOL, "cSOL redeemed for {lamports} lamports");
}

/// A farm held entirely in Kamino (cSOL + cUSDC against a USDC debt) goes to the liquidator in one
/// transaction; the liquidator repays the USDC debt and redeems both cTokens at Kamino.
#[test]
fn farm_fully_in_kamino_is_liquidated_in_one_transaction() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    env.supply(&user, &SOL_RESERVE, 10 * SOL, 1, &[]).unwrap();
    env.open_usdc_debt(&user);
    let mut csol = env.receipt_group(&margin, &SOL_RESERVE);
    if env.is_active(&user, &NATIVE_MINT) {
        csol.extend(collateral_group_metas(&NATIVE_MINT, &margin));
    }
    env.borrow_usdc(&user, 1_500 * USDC, &csol).unwrap();
    let health: Vec<AccountMeta> = csol.iter().cloned().chain(debt_group_metas(&MAINNET_USDC, &margin)).collect();
    env.supply(&user, &USDC_RESERVE, 1_500 * USDC, 1, &health).expect("borrowed USDC into Kamino");
    // SOL $200 -> $14: HF = (140 + 1,500) / 1,500 ≈ 1.093.
    set_price(&mut env.svm, &env.sol_price, WSOL_FEED, 1_400_000_000, 0, -8, FIXTURE_UNIX_TIMESTAMP);

    let liquidator = funded_keypair(&mut env.svm);
    let usdc_account = set_token_balance(&mut env.svm, &liquidator.pubkey(), &MAINNET_USDC, 1_500 * USDC);
    let (csol_before, cusdc_before) =
        (env.balance(&margin, &SOL_RESERVE.collateral_mint), env.balance(&margin, &USDC_RESERVE.collateral_mint));
    let positions = whole_account(&mut env, &user, &margin, &liquidator.pubkey());
    let liquidate = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions, &env.oracles());
    send(&mut env.svm, &liquidator, &[liquidate], &[])
        .expect("liquidate the Kamino farm");

    let liq = liquidator.pubkey();
    let cusdc = get_associated_token_address(&liq, &USDC_RESERVE.collateral_mint);
    let csol_account = get_associated_token_address(&liq, &SOL_RESERVE.collateral_mint);
    assert_eq!((token_balance(&env.svm, &csol_account), token_balance(&env.svm, &cusdc)), (csol_before, cusdc_before));
    assert_eq!(fetch_margin(&env.svm, &user.pubkey()).collateral_count, 0);
    assert_eq!(fetch_reserve(&env.svm, &MAINNET_USDC).total_borrow_assets, 0);

    // Both receipt tokens redeem at Kamino.
    let usdc_before = token_balance(&env.svm, &usdc_account);
    let accounts = kamino_redeem_accounts(&USDC_RESERVE, &liq, &cusdc, &usdc_account);
    send(&mut env.svm, &liquidator, &[kamino_direct_ix(kamino_call_data(REDEEM_RESERVE_COLLATERAL, cusdc_before), accounts)], &[])
        .expect("redeem cUSDC");
    let got = token_balance(&env.svm, &usdc_account) - usdc_before;
    assert!(got <= 1_500 * USDC && got + 4 >= 1_500 * USDC, "cUSDC redeemed for {got}");
    let wsol = get_associated_token_address(&liq, &NATIVE_MINT);
    let wsol = if env.svm.get_account(&wsol).is_some() { wsol } else { set_token_balance(&mut env.svm, &liq, &NATIVE_MINT, 0) };
    let accounts = kamino_redeem_accounts(&SOL_RESERVE, &liq, &csol_account, &wsol);
    send(&mut env.svm, &liquidator, &[kamino_direct_ix(kamino_call_data(REDEEM_RESERVE_COLLATERAL, csol_before), accounts)], &[])
        .expect("redeem cSOL");
}

/// Deposit 1,000 USDC, borrow 4,000 USDC from the Vanna pool (5x), supply all 5,000 to Kamino
/// through `margin_execute`, let a year pass, then redeem through `margin_execute`, repay and
/// withdraw. Every step checks the closed-form expectation against what the program reports.
///
/// klend accrues its own interest by elapsed time inside supply and redeem, so over the year the
/// cUSDC appreciate at Kamino's real rate: the user ends with 1,000 USDC − Vanna interest +
/// Kamino yield. Kamino's rate model isn't re-derived here; its exchange rate is read from the
/// reserve before and after each call, and every cToken amount is checked against it.
#[test]
fn leveraged_kamino_farm_walkthrough() {
    use vanna_lending::constants::{SECONDS_PER_YEAR, WAD};
    use vanna_lending::events::{Borrowed, DebtRepaid, MarginExecuted};
    use vanna_lending::math::fixed_point::{mul_div_ceil, mul_div_floor};
    use vanna_lending::math::interest::{borrow_rate_per_second_wad, utilization_wad};
    use vanna_lending::math::shares::{debt_shares_to_assets_up, lender_total_assets};

    let mut env = setup();
    let debt_group = |margin: &Pubkey| debt_group_metas(&MAINNET_USDC, margin);

    // -- 1. Deposit 1,000 USDC of own capital ---------------------------------------------------
    let equity = 1_000 * USDC;
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, equity);
    println!("1. deposit      equity {} USDC", usd(equity));

    // -- 2. Borrow 4,000 USDC from the Vanna USDC pool: 5x exposure ------------------------------
    let borrowed_amount = 4 * equity;
    env.open_usdc_debt(&user);
    let meta = env.borrow_usdc(&user, borrowed_amount, &[]).expect("borrow 4,000 USDC");
    let borrowed: Borrowed = event(&meta.logs);

    // Theory: borrowed funds stay in the margin, so HF = (1,000 + 4,000) / 4,000 = 1.25.
    let expected_hf = 5 * WAD / 4;
    assert_eq!(borrowed.assets, borrowed_amount);
    assert_eq!(borrowed.debt_shares, borrowed_amount as u128, "first borrow of the pool mints shares 1:1");
    assert_eq!(borrowed.borrow_health_factor_wad, expected_hf);
    assert_eq!(env.balance(&margin, &MAINNET_USDC), equity + borrowed_amount);

    let pool = fetch_reserve(&env.svm, &MAINNET_USDC);
    assert_eq!(pool.accounted_liquidity_assets, 96_000 * USDC);
    assert_eq!(pool.total_borrow_assets, borrowed_amount);
    println!("2. borrow       {} USDC, HF expected {} actual {}", usd(borrowed.assets), hf(expected_hf), hf(borrowed.borrow_health_factor_wad));

    // -- 3. Supply all 5,000 USDC to Kamino through margin_execute ------------------------------
    let exposure = equity + borrowed_amount;
    let (stored_liquidity, stored_supply) = reserve_rate(&env.svm, &USDC_RESERVE);

    let meta = env.supply(&user, &USDC_RESERVE, exposure, 1, &debt_group(&margin)).expect("supply via margin_execute");
    let supplied: MarginExecuted = event(&meta.logs);
    let usdc_left = env.balance(&margin, &MAINNET_USDC);
    let ctokens = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    // Theory: cTokens = spent / (USDC per cToken). klend brought its interest current before
    // minting, so the rate it applied is the reserve's rate after the call (a supply leaves it
    // unchanged up to rounding), a hair above the stored snapshot rate.
    let (kamino_liquidity, kamino_supply) = reserve_rate(&env.svm, &USDC_RESERVE);
    let usdc_per_ctoken = kamino_liquidity as f64 / kamino_supply as f64;
    let expected_ctokens = (supplied.amount_spent as u128 * kamino_supply as u128 / kamino_liquidity) as u64;
    assert!(kamino_liquidity * (stored_supply as u128) >= stored_liquidity * (kamino_supply as u128), "the rate only grows");

    assert_eq!(supplied.program_id, KLEND);
    assert_eq!(supplied.adapter, AdapterKind::KaminoLend);
    assert_eq!(supplied.amount_spent, exposure - usdc_left);
    assert!(supplied.amount_spent <= exposure && supplied.amount_spent + 2 >= exposure, "klend keeps at most rounding dust");
    assert_eq!(supplied.amount_received, ctokens);
    assert!(ctokens.abs_diff(expected_ctokens) <= 1, "cTokens {ctokens} vs rate-implied {expected_ctokens}");
    let ctoken_value = underlying_for_receipts(&env.svm, &USDC_RESERVE, ctokens);
    assert!(ctoken_value <= supplied.amount_spent && ctoken_value + 2 >= supplied.amount_spent);

    // Theory: HF stays 1.25 (value only moved from USDC into cUSDC). Actual: the program's HF must
    // equal the health math applied to the on-chain balances, and 1.25 up to Kamino rounding.
    let recomputed = health_wad(&[usdc_value(ctoken_value, false), usdc_value(usdc_left, false)], &[usdc_value(borrowed_amount, true)]);
    assert_eq!(supplied.borrow_health_factor_wad, recomputed);
    assert!(supplied.borrow_health_factor_wad.abs_diff(expected_hf) <= WAD / 1_000_000, "HF {}", hf(supplied.borrow_health_factor_wad));
    assert!(env.is_active(&user, &USDC_RESERVE.collateral_mint));
    println!(
        "3. kamino       supplied {} USDC at {usdc_per_ctoken:.9} USDC/cUSDC (stored {:.9}) -> {} cUSDC (expected {}), worth {} USDC; HF expected {} actual {}",
        usd(supplied.amount_spent), stored_liquidity as f64 / stored_supply as f64, usd(ctokens), usd(expected_ctokens), usd(ctoken_value),
        hf(expected_hf), hf(supplied.borrow_health_factor_wad)
    );

    // -- 4. One year passes: interest accrues on the Vanna debt ---------------------------------
    advance_time(&mut env.svm, SECONDS_PER_YEAR as i64);
    let now = env.svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut env.svm, &env.usdc_price, USDC_FEED, USDC_PRICE, 0, -8, now);
    send(&mut env.svm, &user, &[ix_public_refresh_reserve(&MAINNET_USDC)], &[]).unwrap();

    // Theory: utilization = 4,000 / 100,000 = 4%. APR = 3.5 × (0.1·u + 0.1·u^32 + 0.3·u^64) ≈ 1.4%,
    // so ≈ 56 USDC of interest, 10% of it (reserve factor) to the protocol.
    let utilization = utilization_wad(96_000 * USDC, borrowed_amount).unwrap();
    assert_eq!(utilization, WAD / 25);
    let rate_per_second = borrow_rate_per_second_wad(&DEFAULT_RATE_CURVE, utilization).unwrap();
    let expected_interest = mul_div_ceil(borrowed_amount as u128, rate_per_second * SECONDS_PER_YEAR as u128, WAD).unwrap() as u64;
    let expected_fee = mul_div_floor(expected_interest as u128, 1_000, 10_000).unwrap() as u64;
    assert!(expected_interest.abs_diff(56 * USDC) <= USDC / 100, "≈ 4,000 × 1.4% = 56 USDC, got {}", usd(expected_interest));

    let pool = fetch_reserve(&env.svm, &MAINNET_USDC);
    let position = fetch_debt_position(&env.svm, &margin, &MAINNET_USDC);
    let debt_now = debt_shares_to_assets_up(position.borrow_shares, pool.total_borrow_shares, pool.total_borrow_assets).unwrap();
    assert_eq!(pool.total_borrow_assets, borrowed_amount + expected_interest);
    assert_eq!(debt_now, borrowed_amount + expected_interest, "the only borrower owes the whole pool debt");
    assert_eq!(pool.accrued_protocol_fees, expected_fee);
    let lenders = lender_total_assets(pool.accounted_liquidity_assets, pool.total_borrow_assets, pool.accrued_protocol_fees).unwrap();
    assert_eq!(lenders, 100_000 * USDC + expected_interest - expected_fee);
    println!(
        "4. after 1 year APR {:.4}%: owe {} USDC = {} borrowed + {} interest ({} to lenders, {} protocol fee)",
        rate_per_second as f64 * SECONDS_PER_YEAR as f64 / 1e16, usd(debt_now), usd(borrowed_amount), usd(expected_interest),
        usd(expected_interest - expected_fee), usd(expected_fee)
    );

    // -- 5. Unwind: redeem from Kamino via margin_execute, repay, withdraw ----------------------
    let meta = env.redeem(&user, &USDC_RESERVE, ctokens, 1, &debt_group(&margin)).expect("redeem via margin_execute");
    let redeemed: MarginExecuted = event(&meta.logs);
    assert_eq!(redeemed.amount_spent, ctokens);
    // Theory: USDC = cTokens × (USDC per cToken), at the rate klend accrued to for the redeem
    // (again the reserve's rate after the call, up to rounding).
    let (liquidity_after, supply_after) = reserve_rate(&env.svm, &USDC_RESERVE);
    let expected_redeem = (ctokens as u128 * liquidity_after / supply_after as u128) as u64;
    assert!(redeemed.amount_received.abs_diff(expected_redeem) <= 1, "redeemed {} vs rate-implied {expected_redeem}", redeemed.amount_received);
    let kamino_yield = redeemed.amount_received as i128 - supplied.amount_spent as i128;
    assert!(kamino_yield > 0, "a year in Kamino earns interest");
    let usdc_back = env.balance(&margin, &MAINNET_USDC);
    // Theory: HF = (5,000 + Kamino yield) / (4,000 + Vanna interest).
    let expected_hf_after = health_wad(&[usdc_value(usdc_back, false)], &[usdc_value(debt_now, true)]);
    assert_eq!(redeemed.borrow_health_factor_wad, expected_hf_after);
    let closed_form = (5_000.0 + kamino_yield as f64 / 1e6) / (debt_now as f64 / 1e6);
    assert!((redeemed.borrow_health_factor_wad as f64 / 1e18 - closed_form).abs() < 1e-5);
    assert!(!env.is_active(&user, &USDC_RESERVE.collateral_mint), "the emptied cUSDC position is closed");

    let meta = send(&mut env.svm, &user, &[ix_user_repay_from_margin(&user.pubkey(), &margin, &MAINNET_USDC, 0, true)], &[]).expect("repay all");
    let repaid: DebtRepaid = event(&meta.logs);
    assert_eq!(repaid.assets, debt_now, "repay-all pays principal + interest exactly");
    assert_eq!(repaid.remaining_debt_shares, 0);
    let pool = fetch_reserve(&env.svm, &MAINNET_USDC);
    assert_eq!(pool.total_borrow_assets, 0);
    assert_eq!(pool.accounted_liquidity_assets, 100_000 * USDC + expected_interest);

    let usdc_after_repay = env.balance(&margin, &MAINNET_USDC);
    assert_eq!(usdc_after_repay, usdc_back - debt_now);
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &MAINNET_USDC, usdc_after_repay, 0, &env.health(&[]));
    send(&mut env.svm, &user, &[ix], &[]).expect("withdraw everything");
    let wallet = token_balance(&env.svm, &get_associated_token_address(&user.pubkey(), &MAINNET_USDC));

    // Theory: the user ends with equity − Vanna interest + Kamino yield, to the base unit.
    assert_eq!(wallet as i128, equity as i128 - expected_interest as i128 + kamino_yield);
    let net = wallet as i128 - equity as i128;
    println!(
        "5. unwind       redeemed {} USDC (Kamino paid {} USDC, {:.2}% on the 5,000), HF {}; repaid {} USDC; withdrew {} USDC",
        usd(redeemed.amount_received), usd(kamino_yield as u64), kamino_yield as f64 / supplied.amount_spent as f64 * 100.0,
        hf(redeemed.borrow_health_factor_wad), usd(repaid.assets), usd(wallet)
    );
    println!(
        "   net result   {}{} USDC on 1,000 equity = Kamino yield {} − Vanna interest {} ({:+.2}%)",
        if net >= 0 { "+" } else { "-" }, usd(net.unsigned_abs() as u64), usd(kamino_yield as u64), usd(expected_interest),
        net as f64 / equity as f64 * 100.0
    );
}
