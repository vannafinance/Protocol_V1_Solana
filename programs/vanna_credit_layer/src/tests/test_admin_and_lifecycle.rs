mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use vanna_credit_layer::errors::VannaError;
use solana_signer::Signer as SvmSigner;

#[test]
fn admin_transfer_and_operating_mode_gate_borrow() {
    let mut svm = setup_svm();
    let old_admin = funded_keypair(&mut svm);
    let new_admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &old_admin, &[ix_initialize_protocol(&old_admin.pubkey(), &treasury, &old_admin.pubkey(), 8)], &[]).unwrap();

    send(&mut svm, &old_admin, &[ix_admin_propose_authority(&old_admin.pubkey(), &new_admin.pubkey())], &[])
        .expect("admin_propose_authority");
    assert_eq!(fetch_protocol_config(&svm).pending_admin, new_admin.pubkey());

    send(&mut svm, &new_admin, &[ix_authority_accept_admin(&new_admin.pubkey())], &[]).expect("authority_accept_admin");
    let cfg = fetch_protocol_config(&svm);
    assert_eq!(cfg.admin, new_admin.pubkey());
    assert_eq!(cfg.pending_admin, Pubkey::default());

    let res = send(&mut svm, &old_admin, &[ix_admin_set_operating_mode(&old_admin.pubkey(), 1)], &[]);
    assert!(res.is_err(), "the superseded admin must no longer be authorized");

    let usdc_mint = create_mint(&mut svm, &new_admin, &new_admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(&mut svm, &new_admin, &new_admin.pubkey(), WSOL_DECIMALS);
    send(&mut svm, &new_admin, &ix_admin_register_asset(&new_admin.pubkey(), &new_admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &new_admin, &ix_admin_register_asset(&new_admin.pubkey(), &new_admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &new_admin, &[ix_admin_initialize_reserve(&new_admin.pubkey(), &new_admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    send(&mut svm, &new_admin, &[ix_admin_initialize_reserve(&new_admin.pubkey(), &new_admin.pubkey(), &wsol_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price_update = pyth_account(&USDC_FEED);
    let wsol_price_update = pyth_account(&WSOL_FEED);
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &new_admin, &usdc_mint, &new_admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &new_admin, &wsol_mint, &new_admin, &borrower.pubkey(), 100 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());
    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &wsol_mint, 10 * 10u64.pow(9))], &[]).unwrap();

    send(&mut svm, &new_admin, &[ix_admin_set_operating_mode(&new_admin.pubkey(), 1)], &[]).expect("set BorrowPaused");
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]).expect("supply still allowed while BorrowPaused");

    let remaining = with_oracles(collateral_group_metas(&wsol_mint, &margin), &[usdc_price_update, wsol_price_update]);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, 100 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    );
    assert!(res.is_err(), "borrowing must be blocked while the protocol is in BorrowPaused mode");

    send(&mut svm, &new_admin, &[ix_admin_set_operating_mode(&new_admin.pubkey(), 0)], &[]).expect("set Normal");
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, 100 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    );
    assert!(res.is_ok(), "borrowing should succeed again once mode is Normal: {res:?}");
}

#[test]
fn admin_config_updates_persist() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    send(&mut svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    send(
        &mut svm,
        &admin,
        &[ix_admin_update_asset_config(&admin.pubkey(), &usdc_mint, 1_000_000 * 10u64.pow(6), 7_500, 8_000, 600, false, true)],
        &[],
    )
    .expect("admin_update_asset_config");
    let asset_config = fetch_asset_config(&svm, &usdc_mint);
    assert_eq!(asset_config.ltv_bps, 7_500);
    assert_eq!(asset_config.liquidation_threshold_bps, 8_000);
    assert_eq!(asset_config.liquidation_bonus_bps, 600);
    assert!(!asset_config.collateral_enabled);
    assert!(asset_config.borrow_enabled);

    let zero_coeff_curve = RateCurve { jump_coeff_wad: 0, ..DEFAULT_RATE_CURVE };
    let res = send(
        &mut svm,
        &admin,
        &[ix_admin_update_reserve_config(&admin.pubkey(), &usdc_mint, zero_coeff_curve, 2_000, 0, 0, 0)],
        &[],
    );
    assert!(res.is_err(), "a rate curve with a zero coefficient must be rejected");

    let new_curve = RateCurve {
        linear_coeff_wad: 200_000_000_000_000_000,
        jump_coeff_wad: 500_000_000_000_000_000,
        rate_multiplier_wad: 2_000_000_000_000_000_000,
    };
    send(
        &mut svm,
        &admin,
        &[ix_admin_update_reserve_config(&admin.pubkey(), &usdc_mint, new_curve, 2_000, 10_000_000 * 10u64.pow(6), 5_000_000 * 10u64.pow(6), 1)],
        &[],
    )
    .expect("admin_update_reserve_config");
    let reserve = fetch_reserve(&svm, &usdc_mint);
    assert_eq!(reserve.rate_curve, new_curve);
    assert_eq!(reserve.reserve_factor_bps, 2_000);
    assert_eq!(reserve.status, 1);
    assert_eq!(reserve.supply_cap, 10_000_000 * 10u64.pow(6));
}

#[test]
fn interest_accrues_and_fees_are_collectible() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury_authority = funded_keypair(&mut svm);
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury_authority.pubkey(), &admin.pubkey(), 8)], &[]).unwrap();

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);
    send(&mut svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price_update = pyth_account(&USDC_FEED);
    let wsol_price_update = pyth_account(&WSOL_FEED);
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 100_000 * 10u64.pow(6), 1)], &[]).unwrap();

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &wsol_mint, &admin, &borrower.pubkey(), 1_000 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());
    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &wsol_mint, 500 * 10u64.pow(9))], &[]).unwrap();

    let remaining = with_oracles(collateral_group_metas(&wsol_mint, &margin), &[usdc_price_update, wsol_price_update]);
    send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, 50_000 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    )
    .expect("user_borrow");

    advance_time(&mut svm, vanna_credit_layer::constants::SECONDS_PER_YEAR as i64);
    send(&mut svm, &lender, &[ix_public_refresh_reserve(&usdc_mint)], &[]).expect("public_refresh_reserve");

    let reserve = fetch_reserve(&svm, &usdc_mint);
    assert_eq!(reserve.total_borrow_assets, 58_750_000_004);
    assert_eq!(reserve.accrued_protocol_fees, 875_000_000);

    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &treasury_authority.pubkey(), 0);
    let collect_amount = 200 * 10u64.pow(6);
    let res = send(&mut svm, &admin, &[ix_admin_collect_protocol_fees(&admin.pubkey(), &usdc_mint, &treasury_authority.pubkey(), collect_amount)], &[]);
    assert!(res.is_ok(), "admin_collect_protocol_fees failed: {res:?}");
    let treasury_ata = get_associated_token_address(&treasury_authority.pubkey(), &usdc_mint);
    assert_eq!(token_balance(&svm, &treasury_ata), collect_amount);

    let res = send(&mut svm, &admin, &[ix_admin_collect_protocol_fees(&admin.pubkey(), &usdc_mint, &treasury_authority.pubkey(), 1_000_000 * 10u64.pow(6))], &[]);
    assert!(res.is_err(), "collecting beyond accrued fees must be rejected");
}

#[test]
fn full_position_close_lifecycle() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    send(&mut svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let user = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &user.pubkey(), 1_000 * 10u64.pow(6));
    let (margin, _) = margin_pda(&user.pubkey());

    send(&mut svm, &user, &[ix_user_create_margin(&user.pubkey(), &user.pubkey())], &[]).expect("create_margin");
    send(&mut svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &usdc_mint, 500 * 10u64.pow(6))], &[]).expect("deposit_collateral");

    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    let usdc_price_update = pyth_account(&USDC_FEED);
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    send(&mut svm, &user, &[ix_user_withdraw_collateral(&user.pubkey(), &margin, &usdc_mint, 500 * 10u64.pow(6), 0, &oracle_metas(&[usdc_price_update]))], &[])
        .expect("user_withdraw_collateral");

    assert_vanna_error(
        send(&mut svm, &user, &[ix_user_reclaim_rent(&user.pubkey(), &margin, &usdc_mint, false, false)], &[]),
        VannaError::NothingToReclaim,
    );
    send(&mut svm, &user, &[ix_user_reclaim_rent(&user.pubkey(), &margin, &usdc_mint, true, false)], &[]).expect("reclaim vault rent");

    send(&mut svm, &user, &[ix_user_close_margin(&user.pubkey(), &margin)], &[]).expect("user_close_margin");
    assert!(svm.get_account(&margin).is_none(), "margin account should be closed and its rent reclaimed");
}
