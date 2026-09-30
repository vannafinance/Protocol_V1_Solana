use crate::common::kamino::*;
use crate::common::*;
use crate::env::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use solana_signer::Signer as SvmSigner;
use vanna_validator::ValidatorError;
use vanna_credit_layer::errors::VannaError;

#[test]
fn other_klend_instructions_are_refused() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let cpi = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));
    for selector in [
        [129, 199, 4, 2, 222, 39, 26, 46],
        [121, 127, 18, 204, 73, 245, 225, 65],
        [135, 231, 52, 167, 7, 52, 212, 193],
        [75, 93, 93, 220, 34, 150, 218, 196],
        [251, 10, 231, 76, 27, 11, 159, 96],
        [2, 218, 138, 235, 79, 201, 25, 102],
    ] {
        let res = env.execute(&user, USDC_RESERVE.collateral_mint, kamino_call_data(selector, USDC), &cpi);
        assert_custom_error(res, ValidatorError::CallNotAllowed.into());
    }

    let res = env.supply(&user, &USDC_RESERVE, 0);
    assert_custom_error(res, ValidatorError::ZeroAmount.into());
    let mut long = kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC);
    long.push(0);
    let res = env.execute(&user, USDC_RESERVE.collateral_mint, long, &cpi);
    assert_custom_error(res, ValidatorError::CallNotAllowed.into());
    let short = &cpi[..11];
    let res = env.execute(&user, USDC_RESERVE.collateral_mint, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), short);
    assert_custom_error(res, ValidatorError::WrongAccountCount.into());
}

#[test]
fn supply_through_another_reserve_is_refused() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let mut cpi = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));
    cpi[1] = AccountMeta::new(SOL_RESERVE.reserve, false);
    let res = env.execute(&user, USDC_RESERVE.collateral_mint, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), &cpi);
    let failure = res.expect_err("another reserve");
    let klend_failed = format!("Program {KLEND} failed");
    assert!(failure.meta.logs.iter().any(|l| l.starts_with(&klend_failed)), "{:?}", failure.meta.logs);
    assert_eq!(env.balance(&margin, &MAINNET_USDC), 1_000 * USDC);
}

#[test]
fn another_users_margin_cannot_be_used() {
    let mut env = setup();
    let (_victim, victim_margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let (attacker, attacker_margin) = env.user_with_collateral(MAINNET_USDC, 10 * USDC);
    let data = || kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, 500 * USDC);

    let cpi = kamino_supply_accounts(&USDC_RESERVE, &attacker_margin, &margin_vault_ata(&victim_margin, &MAINNET_USDC), &margin_vault_ata(&attacker_margin, &USDC_RESERVE.collateral_mint));
    let res = env.execute(&attacker, USDC_RESERVE.collateral_mint, data(), &cpi);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);

    let cpi = kamino_supply_accounts(&USDC_RESERVE, &victim_margin, &margin_vault_ata(&victim_margin, &MAINNET_USDC), &margin_vault_ata(&victim_margin, &USDC_RESERVE.collateral_mint));
    let res = env.execute(&attacker, USDC_RESERVE.collateral_mint, data(), &cpi);
    assert_vanna_error(res, VannaError::InvalidCallAccounts);

    let mut ix = ix_margin_execute(&attacker.pubkey(), &KLEND, None, data(), &cpi, &env.health(&[]), 0);
    for meta in ix.accounts.iter_mut().filter(|m| m.pubkey == attacker_margin) {
        meta.pubkey = victim_margin;
    }
    assert!(send(&mut env.svm, &attacker, &[ix], &[]).is_err());
    assert_eq!(env.balance(&victim_margin, &MAINNET_USDC), 1_000 * USDC);
}

#[test]
fn privileged_accounts_cannot_be_passed_to_klend() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    set_token_balance(&mut env.svm, &user.pubkey(), &NATIVE_MINT, SOL);
    send(&mut env.svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &NATIVE_MINT, SOL)], &[]).unwrap();
    let base = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));

    for smuggled in [user.pubkey(), margin_vault_ata(&margin, &NATIVE_MINT), reserve_pda(&MAINNET_USDC).0, vanna_credit_layer::ID] {
        let mut cpi = base.clone();
        cpi[3] = AccountMeta::new(smuggled, false);
        let res = env.execute(&user, USDC_RESERVE.collateral_mint, kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, USDC), &cpi);
        assert_vanna_error(res, VannaError::InvalidCallAccounts);
    }
}

#[test]
fn operating_mode_gates_external_calls() {
    let mut env = setup();
    let (user, _) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let a = env.admin.pubkey();

    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 3)], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC), VannaError::ProtocolActionPaused);

    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 2)], &[]).unwrap();
    env.supply(&user, &USDC_RESERVE, 100 * USDC).expect("withdraw-only still allows moving collateral");
}

#[test]
fn disabled_integration_or_receipt_blocks_calls() {
    let mut env = setup();
    let (user, _) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let a = env.admin.pubkey();

    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &KLEND, false)], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC), VannaError::IntegrationDisabled);
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &KLEND, true)], &[]).unwrap();

    let cusdc = USDC_RESERVE.collateral_mint;
    send(&mut env.svm, &env.admin, &[ix_admin_update_asset_config(&a, &cusdc, 0, 8_000, 8_500, 500, false, false)], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC), VannaError::AssetNotCollateralEnabled);

    let res = send(&mut env.svm, &user, &[ix_admin_set_integration_enabled(&user.pubkey(), &KLEND, false)], &[]);
    assert!(res.is_err());
}

#[test]
fn only_held_collateral_can_be_spent() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);

    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &MAINNET_USDC, 1_000 * USDC, 0, &env.health(&[]));
    send(&mut env.svm, &user, &[ix], &[]).unwrap();
    assert_vanna_error(env.supply(&user, &USDC_RESERVE, 100 * USDC), VannaError::IncompletePositionAccounts);

    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC).unwrap();
    let receipts = env.balance(&margin, &USDC_RESERVE.collateral_mint);
    assert!(env.redeem(&user, &USDC_RESERVE, receipts + 1).is_err(), "klend rejects redeeming more than held");
    assert_eq!(env.balance(&margin, &USDC_RESERVE.collateral_mint), receipts);
}

#[test]
fn receipt_positions_cannot_be_hidden_from_health_checks() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    env.supply(&user, &USDC_RESERVE, 400 * USDC).unwrap();

    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &[]), VannaError::IncompletePositionAccounts);
    let group = env.receipt_group(&margin, &USDC_RESERVE);
    let oracles = [env.usdc_price, env.sol_price, SOL_RESERVE.reserve];
    let ix = ix_user_borrow(&user.pubkey(), &margin, &MAINNET_USDC, 100 * USDC, u128::MAX, &with_oracles(group, &oracles));
    assert_custom_error(send(&mut env.svm, &user, &[ix], &[]), vanna_oracle::OracleError::InvalidPriceSource.into());

    env.borrow_usdc(&user, 100 * USDC, &env.receipt_group(&margin, &USDC_RESERVE)).expect("complete, correctly priced scan");
}

#[test]
fn a_received_token_needs_its_group_and_nothing_extra() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(MAINNET_USDC, 1_000 * USDC);
    let u = user.pubkey();
    let cpi = kamino_supply_accounts(&USDC_RESERVE, &margin, &margin_vault_ata(&margin, &MAINNET_USDC), &margin_vault_ata(&margin, &USDC_RESERVE.collateral_mint));
    let data = || kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, 100 * USDC);
    let vaults = [
        ix_create_vault(&u, &margin, &USDC_RESERVE.collateral_mint, &anchor_spl::token::ID),
        ix_create_vault(&u, &margin, &NATIVE_MINT, &anchor_spl::token::ID),
    ];
    let groups = margin_groups(&env.svm, &u, &env.mints());

    let missing = ix_margin_execute(&u, &KLEND, None, data(), &cpi, &env.health(&groups), 0);
    let res = send(&mut env.svm, &user, &[vaults[0].clone(), vaults[1].clone(), missing], &[]);
    assert_vanna_error(res, VannaError::IncompletePositionAccounts);

    let mut extra = groups.clone();
    extra.extend(collateral_group_metas(&USDC_RESERVE.collateral_mint, &margin));
    extra.extend(collateral_group_metas(&NATIVE_MINT, &margin));
    let ix = ix_margin_execute(&u, &KLEND, None, data(), &cpi, &env.health(&extra), 2);
    let res = send(&mut env.svm, &user, &[vaults[0].clone(), vaults[1].clone(), ix], &[]);
    assert_vanna_error(res, VannaError::IncompletePositionAccounts);
    assert_eq!(env.balance(&margin, &MAINNET_USDC), 1_000 * USDC);
}
