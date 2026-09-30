use crate::common::kamino::*;
use crate::common::*;
use crate::env::*;
use anchor_lang::prelude::*;
use solana_signer::Signer as SvmSigner;

#[test]
fn klend_fixture_accepts_a_direct_wallet_supply() {
    let mut env = setup();
    let wallet = funded_keypair(&mut env.svm);
    let usdc = set_token_balance(&mut env.svm, &wallet.pubkey(), &MAINNET_USDC, 100 * USDC);
    let cusdc = set_token_balance(&mut env.svm, &wallet.pubkey(), &USDC_RESERVE.collateral_mint, 0);
    let accounts = kamino_supply_accounts(&USDC_RESERVE, &wallet.pubkey(), &usdc, &cusdc);
    send(&mut env.svm, &wallet, &[kamino_direct_ix(kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, 100 * USDC), accounts)], &[])
        .expect("direct klend supply");
    assert!(token_balance(&env.svm, &cusdc) > 0);
}

#[test]
fn supply_moves_margin_usdc_into_kamino() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let klend_vault_before = token_balance(&env.svm, &USDC_RESERVE.supply_vault);

    let meta = env.supply(&user, &USDC_RESERVE, 400 * USDC).expect("supply via margin_execute");
    println!("margin_execute(supply) compute units: {}", meta.compute_units_consumed);
    assert!(meta.compute_units_consumed < 200_000, "fits the default compute budget");

    let spent = 1_000 * USDC - env.balance(&margin, &MAINNET_USDC);
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    assert!(spent <= 400 * USDC && spent >= 400 * USDC - 2, "spent {spent}");
    assert_eq!(token_balance(&env.svm, &USDC_RESERVE.supply_vault) - klend_vault_before, spent);
    let redeemable = underlying_for_receipts(&env.svm, &USDC_RESERVE, receipts);
    assert!(redeemable <= spent && redeemable + 2 >= spent, "cTokens worth {redeemable} for {spent} spent");
    assert!(env.is_active(&user, &MAINNET_USDC));
    assert!(env.is_active(&user, &USDC_RESERVE.collateral_mint));
}

#[test]
fn redeeming_every_ctoken_returns_the_usdc_and_closes_the_receipt_position() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    let meta = env.redeem(&user, &USDC_RESERVE, receipts).unwrap_or_else(|e| panic!("redeem via margin_execute: {:?}", e.meta.logs));
    eprintln!("CU {}", meta.compute_units_consumed);

    assert_eq!(env.balance(&margin, &USDC_RESERVE.collateral_mint), 0);
    assert!(!env.is_active(&user, &USDC_RESERVE.collateral_mint), "an empty receipt vault leaves the active list");
    let usdc_back = env.balance(&margin, &MAINNET_USDC);
    assert!(usdc_back <= 1_000 * USDC && usdc_back + 4 >= 1_000 * USDC, "round trip lost more than rounding: {usdc_back}");
}

#[test]
fn partial_redeem_keeps_both_positions() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);

    env.redeem(&user, &USDC_RESERVE, receipts / 2).unwrap();

    assert_eq!(env.balance(&margin, &USDC_RESERVE.collateral_mint), receipts - receipts / 2);
    assert!(env.is_active(&user, &MAINNET_USDC));
    assert!(env.is_active(&user, &USDC_RESERVE.collateral_mint));
}
