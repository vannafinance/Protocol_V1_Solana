//! Everything the Kamino path through `margin_execute` must refuse.

use crate::common::kamino::*;
use crate::common::*;
use crate::env::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use solana_signer::Signer as SvmSigner;
use vanna_lending::errors::VannaError;

/// Only supply and redeem are allowed; klend's obligation, borrow and flash-loan paths are not.
#[test]
fn other_klend_instructions_are_refused() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let cpi = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));
    for selector in [
        [129, 199, 4, 2, 222, 39, 26, 46],     // deposit_reserve_liquidity_and_obligation_collateral
        [121, 127, 18, 204, 73, 245, 225, 65], // borrow_obligation_liquidity
        [135, 231, 52, 167, 7, 52, 212, 193],  // flash_borrow_reserve_liquidity
        [75, 93, 93, 220, 34, 150, 218, 196],  // withdraw_obligation_collateral_and_redeem_reserve_collateral
        [251, 10, 231, 76, 27, 11, 159, 96],   // init_obligation
        [2, 218, 138, 235, 79, 201, 25, 102],  // refresh_reserve
    ] {
        let res = env.execute(&user, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, kamino_call_data(selector, USDC), &cpi, &[], 0);
        assert_vanna_error(res, VannaError::CallNotAllowed);
    }
    // Malformed allowed calls: zero amount, extra data, missing accounts.
    let res = env.supply(&user, &USDC_RESERVE, 0, 0, &[]);
    assert_vanna_error(res, VannaError::ZeroAmount);
    let mut long = kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC);
    long.push(0);
    let res = env.execute(&user, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, long, &cpi, &[], 0);
    assert_vanna_error(res, VannaError::CallNotAllowed);
    let short = &cpi[..11];
    let res = env.execute(&user, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), short, &[], 0);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);
}

/// cUSDC is priced by the USDC reserve, so the call must go through that reserve.
#[test]
fn supply_through_another_reserve_is_refused() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let mut cpi = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));
    cpi[1] = AccountMeta::new(SOL_RESERVE.reserve, false);
    let res = env.execute(&user, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), &cpi, &[], 0);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);
}

/// The received amount is measured after klend runs and must meet `min_received`.
#[test]
fn min_received_is_enforced_after_the_real_call() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 400 * USDC, 400 * USDC, &[]), VannaError::SlippageExceeded);
    assert_eq!(env.balance(&margin, &MAINNET_USDC), 1_000 * USDC, "the whole call rolled back");
}

/// Nobody can move another user's margin funds through `margin_execute`.
#[test]
fn another_users_margin_cannot_be_used() {
    let mut env = setup();
    let (_victim, victim_margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let (attacker, attacker_margin) = env.user_with_collateral(MAINNET_USDC, 10 * USDC);
    let data = || kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, 500 * USDC);

    // The victim's vault as the spent vault: it isn't the attacker margin's vault.
    let cpi = kamino_supply_accounts(&USDC_RESERVE, &attacker_margin, &margin_vault_ata(&victim_margin, &MAINNET_USDC), &margin_vault_ata(&attacker_margin, &USDC_RESERVE.collateral_mint));
    let res = env.execute(&attacker, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, data(), &cpi, &[], 0);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);

    // The victim's margin as the signing owner: the attacker can't sign for it.
    let cpi = kamino_supply_accounts(&USDC_RESERVE, &victim_margin, &margin_vault_ata(&victim_margin, &MAINNET_USDC), &margin_vault_ata(&victim_margin, &USDC_RESERVE.collateral_mint));
    let res = env.execute(&attacker, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, data(), &cpi, &[], 0);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);

    // Swapping the victim's margin in as the named margin account breaks its PDA seeds.
    let mut ix = ix_margin_execute(&attacker.pubkey(), &KLEND, &MAINNET_USDC, &env.usdc_price, &USDC_RESERVE.collateral_mint, &env.usdc_price, data(), &cpi, &[], 0);
    for meta in ix.accounts.iter_mut().filter(|m| m.pubkey == attacker_margin) {
        meta.pubkey = victim_margin;
    }
    assert!(send(&mut env.svm, &attacker, &[ix], &[]).is_err());
    assert_eq!(env.balance(&victim_margin, &MAINNET_USDC), 1_000 * USDC);
}

/// The margin signs the call, so the wallet, Vanna state and other margin vaults can't ride along.
#[test]
fn privileged_accounts_cannot_be_passed_to_klend() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    // A second margin vault (SOL) that must stay out of reach.
    set_token_balance(&mut env.svm, &user.pubkey(), &NATIVE_MINT, SOL);
    send(&mut env.svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &NATIVE_MINT, SOL)], &[]).unwrap();
    let base = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));

    for smuggled in [user.pubkey(), margin_vault_ata(&margin, &NATIVE_MINT), reserve_pda(&MAINNET_USDC).0, vanna_lending::ID] {
        let mut cpi = base.clone();
        cpi[3] = AccountMeta::new(smuggled, false); // the market-authority slot
        let res = env.execute(&user, MAINNET_USDC, USDC_RESERVE.collateral_mint, env.usdc_price, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), &cpi, &[], 0);
        assert_vanna_error(res, VannaError::InvalidCallAccounts);
    }
}

#[test]
fn operating_mode_gates_external_calls() {
    let mut env = setup();
    let (user, _) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let a = env.admin.pubkey();

    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 3)], &[]).unwrap(); // Halted
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC, 1, &[]), VannaError::ProtocolActionPaused);

    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 2)], &[]).unwrap(); // WithdrawOnly
    env.supply(&user, &USDC_RESERVE, 100 * USDC, 1, &[]).expect("withdraw-only still allows moving collateral");
}

#[test]
fn disabled_integration_or_receipt_blocks_calls() {
    let mut env = setup();
    let (user, _) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let a = env.admin.pubkey();

    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &KLEND, false)], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC, 1, &[]), VannaError::IntegrationDisabled);
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &KLEND, true)], &[]).unwrap();

    // A receipt that isn't collateral-enabled can't be received.
    let cusdc = USDC_RESERVE.collateral_mint;
    send(&mut env.svm, &env.admin, &[ix_admin_update_asset_config(&a, &cusdc, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false)], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC, 1, &[]), VannaError::AssetNotCollateralEnabled);

    // Non-admins can't toggle the integration.
    let res = send(&mut env.svm, &user, &[ix_admin_set_integration_enabled(&user.pubkey(), &KLEND, false)], &[]);
    assert!(res.is_err());
}

/// Only tracked collateral can be spent, and klend itself refuses to redeem more than is held.
#[test]
fn only_held_collateral_can_be_spent() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);

    // Withdraw everything: the vault stays but is no longer active collateral.
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &MAINNET_USDC, &env.usdc_price, 1_000 * USDC, 0, &[]);
    send(&mut env.svm, &user, &[ix], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC, 1, &[]), VannaError::IncompletePositionAccounts);

    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC, 1, &[]).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    assert!(env.redeem(&user, &USDC_RESERVE, receipts + 1, 1, &[]).is_err(), "klend rejects redeeming more than held");
    assert_eq!(env.balance(&margin, &USDC_RESERVE.collateral_mint), receipts);
}

/// A cUSDC position can't be hidden or mispriced in later health checks.
#[test]
fn receipt_positions_cannot_be_hidden_from_health_checks() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC, 1, &[]).unwrap();
    env.open_usdc_debt(&user);

    // Omitted entirely.
    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &[]), VannaError::IncompletePositionAccounts);
    // Without its reserve account.
    let no_source = collateral_group_metas(&USDC_RESERVE.collateral_mint, &margin, &env.usdc_price);
    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &no_source), VannaError::IncompletePositionAccounts);
    // With another reserve substituted for its price source.
    let wrong_source = collateral_group_with_source(&USDC_RESERVE.collateral_mint, &margin, &env.usdc_price, &SOL_RESERVE.reserve);
    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &wrong_source), VannaError::InvalidPriceSource);

    env.borrow_usdc(&user, 100 * USDC, &env.receipt_group(&margin, &USDC_RESERVE)).expect("complete, correctly priced scan");
}
