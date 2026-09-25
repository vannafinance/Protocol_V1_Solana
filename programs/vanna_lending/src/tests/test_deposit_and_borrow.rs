mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;

struct Fixture {
    admin: Keypair,
    usdc_mint: Pubkey,
    wsol_mint: Pubkey,
    usdc_price_update: Pubkey,
    wsol_price_update: Pubkey,
}

fn setup(svm: &mut LiteSVM) -> Fixture {
    let admin = funded_keypair(svm);
    let treasury = Pubkey::new_unique();

    let res = send(svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]);
    assert!(res.is_ok(), "initialize_protocol failed: {res:?}");

    let usdc_mint = create_mint(svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(svm, &admin, &admin.pubkey(), WSOL_DECIMALS);

    let res = send(
        svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)],
        &[],
    );
    assert!(res.is_ok(), "register USDC failed: {res:?}");

    let res = send(
        svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)],
        &[],
    );
    assert!(res.is_ok(), "register WSOL failed: {res:?}");

    let res = send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]);
    assert!(res.is_ok(), "init USDC reserve failed: {res:?}");

    let res = send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]);
    assert!(res.is_ok(), "init WSOL reserve failed: {res:?}");

    let usdc_price_update = Pubkey::new_unique();
    let wsol_price_update = Pubkey::new_unique();
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    // Lender seeds USDC liquidity so the borrower has something to borrow.
    let lender = funded_keypair(svm);
    mint_to_wallet(svm, &admin, &usdc_mint, &admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));
    let res = send(svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]);
    assert!(res.is_ok(), "lender_supply failed: {res:?}");

    Fixture { admin, usdc_mint, wsol_mint, usdc_price_update, wsol_price_update }
}

/// One transaction creates the margin account and debt position, deposits, and borrows atomically.
#[test]
fn deposit_and_borrow_creates_margin_and_debt_position_in_one_tx() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&borrower.pubkey());
    // Brand-new margin account: no other active positions to scan, so `remaining` is empty.
    let deposit_amount = 10 * 10u64.pow(9); // 10 WSOL ~= $2,000
    let borrow_amount = 500 * 10u64.pow(6); // Projected HF = ($2,000 + $500) / $500 = 5.0
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(
            &borrower.pubkey(),
            &f.wsol_mint,
            &f.wsol_price_update,
            &f.usdc_mint,
            &f.usdc_price_update,
            deposit_amount,
            borrow_amount,
            u128::MAX,
            &[],
        )],
        &[],
    );
    assert!(res.is_ok(), "user_deposit_and_borrow failed: {res:?}");

    let margin_wsol_vault = get_associated_token_address(&margin, &f.wsol_mint);
    let margin_usdc_vault = get_associated_token_address(&margin, &f.usdc_mint);
    assert_eq!(token_balance(&svm, &margin_wsol_vault), deposit_amount);
    assert_eq!(token_balance(&svm, &margin_usdc_vault), borrow_amount);

    let borrower_wsol_wallet = get_associated_token_address(&borrower.pubkey(), &f.wsol_mint);
    assert_eq!(token_balance(&svm, &borrower_wsol_wallet), 90 * 10u64.pow(9));
}

/// A second deposit_and_borrow reuses the existing margin account and debt position (`init_if_needed`).
#[test]
fn second_deposit_and_borrow_reuses_existing_margin_and_debt_position() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&borrower.pubkey());
    let first_deposit = 10 * 10u64.pow(9);
    let first_borrow = 200 * 10u64.pow(6);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(&borrower.pubkey(), &f.wsol_mint, &f.wsol_price_update, &f.usdc_mint, &f.usdc_price_update, first_deposit, first_borrow, u128::MAX, &[])],
        &[],
    );
    assert!(res.is_ok(), "first deposit_and_borrow failed: {res:?}");

    // The named WSOL deposit and USDC debt are excluded from the scan, but the first call's
    // borrowed USDC is now an active collateral credit (spec §1.2) and must be passed in
    // `remaining_accounts`.
    let remaining = collateral_group_metas(&f.usdc_mint, &margin, &f.usdc_price_update);
    let second_deposit = 5 * 10u64.pow(9);
    let second_borrow = 100 * 10u64.pow(6);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(&borrower.pubkey(), &f.wsol_mint, &f.wsol_price_update, &f.usdc_mint, &f.usdc_price_update, second_deposit, second_borrow, u128::MAX, &remaining)],
        &[],
    );
    assert!(res.is_ok(), "second deposit_and_borrow failed: {res:?}");

    let margin_wsol_vault = get_associated_token_address(&margin, &f.wsol_mint);
    let margin_usdc_vault = get_associated_token_address(&margin, &f.usdc_mint);
    assert_eq!(token_balance(&svm, &margin_wsol_vault), first_deposit + second_deposit);
    assert_eq!(token_balance(&svm, &margin_usdc_vault), first_borrow + second_borrow);
}

/// The atomic instruction enforces the same health-factor gate as `user_borrow`.
#[test]
fn deposit_and_borrow_rejects_unhealthy_borrow() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    // 1 WSOL ~= $200 deposited. A $2,000 borrow lands exactly at HF 1.10, so
    // $2,500 is unambiguously below the strict reference threshold.
    let deposit_amount = 1 * 10u64.pow(9);
    let borrow_amount = 2_500 * 10u64.pow(6);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(&borrower.pubkey(), &f.wsol_mint, &f.wsol_price_update, &f.usdc_mint, &f.usdc_price_update, deposit_amount, borrow_amount, u128::MAX, &[])],
        &[],
    );
    assert!(res.is_err(), "unhealthy deposit_and_borrow unexpectedly succeeded");
}

/// Depositing and borrowing the same mint is rejected rather than aliasing the two sides' accounts.
#[test]
fn deposit_and_borrow_rejects_same_asset_on_both_sides() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(
            &borrower.pubkey(),
            &f.wsol_mint,
            &f.wsol_price_update,
            &f.wsol_mint,
            &f.wsol_price_update,
            1 * 10u64.pow(9),
            1 * 10u64.pow(8),
            u128::MAX,
            &[],
        )],
        &[],
    );
    assert!(res.is_err(), "same-asset deposit_and_borrow unexpectedly succeeded");
}
