//! Getting funds back out of Kamino: redeeming through `margin_execute`, withdrawing to the
//! wallet, and closing the positions and the margin account.

use crate::common::kamino::*;
use crate::common::*;
use crate::env::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_lending::errors::VannaError;

fn wallet_balance(env: &Env, owner: &Pubkey, mint: &Pubkey) -> u64 {
    token_balance(&env.svm, &get_associated_token_address(owner, mint))
}

/// Withdraws the margin's whole balance of `mint` to the wallet. `health` lists the margin's other
/// open positions.
fn withdraw_all(env: &mut Env, user: &Keypair, mint: &Pubkey, health: &[AccountMeta]) -> u64 {
    let margin = margin_pda(&user.pubkey()).0;
    let amount = env.balance(&margin, mint);
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, mint, amount, 0, &env.health(health));
    send(&mut env.svm, user, &[ix], &[]).expect("withdraw to wallet");
    amount
}

/// Supply 1,000 USDC to Kamino, redeem every cToken, withdraw the USDC to the wallet, then close
/// both collateral positions and the margin account.
#[test]
fn full_exit_from_kamino_back_to_the_wallet() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 1_000 * USDC, 1, &[]).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    env.redeem(&user, &USDC_RESERVE, receipts, 1, &[]).expect("redeem every cToken");
    assert!(!env.is_active(&user, &USDC_RESERVE.collateral_mint));
    let withdrawn = withdraw_all(&mut env, &user, &MAINNET_USDC, &[]);

    let wallet = wallet_balance(&env, &user.pubkey(), &MAINNET_USDC);
    assert_eq!(wallet, withdrawn);
    assert!(wallet <= 1_000 * USDC && wallet + 4 >= 1_000 * USDC, "round trip lost more than rounding: {wallet}");
    assert!(!env.is_active(&user, &MAINNET_USDC));

    for mint in [USDC_RESERVE.collateral_mint, MAINNET_USDC] {
        send(&mut env.svm, &user, &[ix_user_close_collateral_position(&user.pubkey(), &margin, &mint)], &[]).expect("close position");
        assert!(env.svm.get_account(&margin_vault_ata(&margin, &mint)).is_none_or(|a| a.lamports == 0), "vault closed");
    }
    send(&mut env.svm, &user, &[ix_user_close_margin(&user.pubkey(), &margin)], &[]).expect("close margin");
    assert!(env.svm.get_account(&margin).is_none_or(|a| a.lamports == 0), "margin account closed");
}

/// A year in Kamino: the redeem pays out principal plus klend's interest at the rate klend
/// applied, and all of it reaches the wallet.
#[test]
fn kamino_interest_is_withdrawn_with_the_principal() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 1_000 * USDC, 1, &[]).unwrap();
    let spent = 1_000 * USDC - env.balance(&margin, &MAINNET_USDC);
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    advance_time(&mut env.svm, vanna_lending::constants::SECONDS_PER_YEAR as i64);
    let now = env.svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut env.svm, &env.usdc_price, USDC_FEED, USDC_PRICE, 0, -8, now);

    let meta = env.redeem(&user, &USDC_RESERVE, receipts, 1, &[]).expect("redeem after a year");
    let redeemed: vanna_lending::events::MarginExecuted = event(&meta.logs);
    // USDC out = cTokens × (USDC per cToken), at the rate klend accrued to inside the redeem.
    let (liquidity, supply) = reserve_rate(&env.svm, &USDC_RESERVE);
    let expected = (receipts as u128 * liquidity / supply as u128) as u64;
    assert!(redeemed.amount_received.abs_diff(expected) <= 1, "redeemed {} vs rate-implied {expected}", redeemed.amount_received);
    assert!(redeemed.amount_received > spent, "a year in Kamino earns interest");

    let withdrawn = withdraw_all(&mut env, &user, &MAINNET_USDC, &[]);
    assert_eq!(wallet_balance(&env, &user.pubkey(), &MAINNET_USDC), withdrawn);
    assert_eq!(withdrawn, 1_000 * USDC - spent + redeemed.amount_received);
    println!(
        "supplied {} USDC, redeemed {} USDC after a year (+{} USDC, {:.2}%), withdrew {} USDC",
        usd(spent), usd(redeemed.amount_received), usd(redeemed.amount_received - spent),
        (redeemed.amount_received - spent) as f64 / spent as f64 * 100.0, usd(withdrawn)
    );
}

/// Redeem and withdraw in parts: the cToken position stays open until the last redeem.
#[test]
fn withdraw_from_kamino_in_steps() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 1_000 * USDC, 1, &[]).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    let mut wallet = 0;

    for (step, part) in [receipts / 4, receipts / 4, receipts - receipts / 4 * 2].into_iter().enumerate() {
        let expected = underlying_for_receipts(&env.svm, &USDC_RESERVE, part);
        let usdc_before = env.balance(&margin, &MAINNET_USDC);
        env.redeem(&user, &USDC_RESERVE, part, 1, &[]).expect("partial redeem");
        let received = env.balance(&margin, &MAINNET_USDC) - usdc_before;
        assert!(received.abs_diff(expected) <= 1, "step {step}: redeemed {received}, expected {expected}");

        // The cUSDC position stays open until the last redeem. Without debt, a withdrawal reads
        // no positions or prices (Solidity: `if hasNoDebt return true`).
        let last = step == 2;
        assert_eq!(env.is_active(&user, &USDC_RESERVE.collateral_mint), !last);
        wallet += withdraw_all(&mut env, &user, &MAINNET_USDC, &[]);
        assert_eq!(wallet_balance(&env, &user.pubkey(), &MAINNET_USDC), wallet);
    }
    assert_eq!(env.balance(&margin, &USDC_RESERVE.collateral_mint), 0);
    assert!(wallet <= 1_000 * USDC && wallet + 6 >= 1_000 * USDC, "three redeems lost more than rounding: {wallet}");
}

/// The cTokens themselves can be withdrawn to the wallet (priced through their reserve), and the
/// wallet redeems them at Kamino directly.
#[test]
fn ctokens_can_be_withdrawn_and_redeemed_at_kamino_directly() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 1_000 * USDC, 1, &[]).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    let wallet_cusdc = set_token_balance(&mut env.svm, &user.pubkey(), &USDC_RESERVE.collateral_mint, 0);
    // Debt-free: the withdrawal needs neither the receipt's reserve nor any price.
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &USDC_RESERVE.collateral_mint, receipts, 0, &[]);
    send(&mut env.svm, &user, &[ix], &[]).expect("withdraw cUSDC to the wallet");
    assert_eq!(token_balance(&env.svm, &wallet_cusdc), receipts);
    assert!(!env.is_active(&user, &USDC_RESERVE.collateral_mint));

    let wallet_usdc = get_associated_token_address(&user.pubkey(), &MAINNET_USDC);
    let usdc_before = token_balance(&env.svm, &wallet_usdc);
    let accounts = kamino_redeem_accounts(&USDC_RESERVE, &user.pubkey(), &wallet_cusdc, &wallet_usdc);
    send(&mut env.svm, &user, &[kamino_direct_ix(kamino_call_data(REDEEM_RESERVE_COLLATERAL, receipts), accounts)], &[])
        .expect("wallet redeems at klend");
    assert_eq!(token_balance(&env.svm, &wallet_cusdc), 0);
    let redeemed = token_balance(&env.svm, &wallet_usdc) - usdc_before;
    let usdc_left = env.balance(&margin, &MAINNET_USDC);
    assert!(redeemed + usdc_left <= 1_000 * USDC && redeemed + usdc_left + 4 >= 1_000 * USDC, "redeemed {redeemed}");
}

/// A leveraged Kamino position exits in order: the cTokens can't leave while they back the debt,
/// so redeem, repay, withdraw, then close the debt, both positions and the margin account.
#[test]
fn leveraged_position_exits_kamino_repays_and_closes() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.open_usdc_debt(&user);
    env.borrow_usdc(&user, 1_000 * USDC, &[]).unwrap();
    let debt = debt_group_metas(&MAINNET_USDC, &margin);
    env.supply(&user, &USDC_RESERVE, 1_500 * USDC, 1, &debt).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    // Taking the cUSDC out would leave $500 against $1,000 of debt.
    let usdc_group = collateral_group_metas(&MAINNET_USDC, &margin);
    let health: Vec<_> = usdc_group.iter().chain(debt.iter()).cloned().collect();
    set_token_balance(&mut env.svm, &user.pubkey(), &USDC_RESERVE.collateral_mint, 0);
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &USDC_RESERVE.collateral_mint, receipts, 0, &env.health(&health));
    assert_vanna_error(send(&mut env.svm, &user, &[ix], &[]), VannaError::HealthFactorTooLow);
    // Nor can the debt position or the margin be closed while the debt is open.
    let close_debt = ix_user_close_debt_position(&user.pubkey(), &margin, &MAINNET_USDC);
    assert_vanna_error(send(&mut env.svm, &user, &[close_debt.clone()], &[]), VannaError::OutstandingDebt);
    let close_margin = ix_user_close_margin(&user.pubkey(), &margin);
    assert_vanna_error(send(&mut env.svm, &user, &[close_margin.clone()], &[]), VannaError::NonEmptyMargin);

    env.redeem(&user, &USDC_RESERVE, receipts, 1, &debt).expect("redeem with open debt");
    send(&mut env.svm, &user, &[ix_user_repay_from_margin(&user.pubkey(), &margin, &MAINNET_USDC, 0, true)], &[]).expect("repay all");
    let withdrawn = withdraw_all(&mut env, &user, &MAINNET_USDC, &[]);
    assert!(withdrawn <= 1_000 * USDC && withdrawn + 4 >= 1_000 * USDC, "equity back minus rounding: {withdrawn}");

    send(&mut env.svm, &user, &[close_debt], &[]).expect("close debt position");
    for mint in [USDC_RESERVE.collateral_mint, MAINNET_USDC] {
        send(&mut env.svm, &user, &[ix_user_close_collateral_position(&user.pubkey(), &margin, &mint)], &[]).expect("close position");
    }
    send(&mut env.svm, &user, &[close_margin], &[]).expect("close margin");
    assert!(env.svm.get_account(&margin).is_none_or(|a| a.lamports == 0), "margin account closed");
}

/// SOL supplied to Kamino comes back to the wallet. cSOL has 6 decimals against SOL's 9, so each
/// cToken is worth ~1,000 lamports and the round trip may lose up to about two of them.
#[test]
fn sol_supplied_to_kamino_comes_back_to_the_wallet() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    env.supply(&user, &SOL_RESERVE, 10 * SOL, 1, &[]).unwrap();
    let receipts = env.balance(&margin, &SOL_RESERVE.collateral_mint);

    env.redeem(&user, &SOL_RESERVE, receipts, 1, &[]).expect("redeem every cSOL");
    assert!(!env.is_active(&user, &SOL_RESERVE.collateral_mint));
    let withdrawn = withdraw_all(&mut env, &user, &NATIVE_MINT, &[]);

    let lamports_per_ctoken = underlying_for_receipts(&env.svm, &SOL_RESERVE, 1) + 1;
    assert_eq!(wallet_balance(&env, &user.pubkey(), &NATIVE_MINT), withdrawn);
    assert!(withdrawn <= 10 * SOL && withdrawn + 2 * lamports_per_ctoken >= 10 * SOL, "withdrew {withdrawn} lamports");
}
