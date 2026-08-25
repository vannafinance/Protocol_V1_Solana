mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use solana_signer::Signer as SvmSigner;

const USDC_FEED: [u8; 32] = [1u8; 32];
const WSOL_FEED: [u8; 32] = [2u8; 32];
const USDC_PRICE: i64 = 100_000_000; // $1.00 @ exponent -8
const WSOL_PRICE: i64 = 20_000_000_000; // $200.00 @ exponent -8

/// End-to-end happy path across every milestone in the spec:
/// protocol/asset/reserve init -> lender supply -> margin creation -> collateral deposit ->
/// borrow -> partial repay -> collateral withdrawal -> lender redeem.
#[test]
fn full_protocol_flow() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();

    // -- protocol + assets + reserves ------------------------------------
    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]);
    assert!(res.is_ok(), "initialize_protocol failed: {res:?}");

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);

    let res = send(
        &mut svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)],
        &[],
    );
    assert!(res.is_ok(), "register USDC failed: {res:?}");

    let res = send(
        &mut svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)],
        &[],
    );
    assert!(res.is_ok(), "register WSOL failed: {res:?}");

    let res = send(
        &mut svm,
        &admin,
        &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)],
        &[],
    );
    assert!(res.is_ok(), "init USDC reserve failed: {res:?}");

    let res = send(
        &mut svm,
        &admin,
        &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)],
        &[],
    );
    assert!(res.is_ok(), "init WSOL reserve failed: {res:?}");

    let usdc_price_update = Pubkey::new_unique();
    let wsol_price_update = Pubkey::new_unique();
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    // -- lender supplies USDC ---------------------------------------------
    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));

    let res = send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]);
    assert!(res.is_ok(), "lender_supply failed: {res:?}");

    let (reserve_usdc, _) = reserve_pda(&usdc_mint);
    let usdc_reserve_vault = anchor_spl::associated_token::get_associated_token_address(&reserve_usdc, &usdc_mint);
    assert_eq!(token_balance(&svm, &usdc_reserve_vault), 500_000 * 10u64.pow(6));

    // -- borrower creates margin, deposits WSOL collateral -----------------
    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &wsol_mint, &admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&borrower.pubkey());
    let res = send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]);
    assert!(res.is_ok(), "user_create_margin failed: {res:?}");

    // No separate "open collateral position" step: the margin vault is a plain ATA that
    // `user_deposit_collateral` creates on first use.
    let deposit_amount = 10 * 10u64.pow(9); // 10 WSOL ~= $2,000
    let res = send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &wsol_mint, deposit_amount)], &[]);
    assert!(res.is_ok(), "deposit WSOL collateral failed: {res:?}");

    // -- borrower opens the USDC debt position, then borrows --
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &usdc_mint)],
        &[],
    );
    assert!(res.is_ok(), "open USDC debt position failed: {res:?}");

    let borrow_amount = 500 * 10u64.pow(6); // 500 USDC, well within $1,400 borrow power (70% LTV * $2,000)
    let remaining = collateral_group_metas(&wsol_mint, &margin, &wsol_price_update);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, &usdc_price_update, borrow_amount, u128::MAX, &remaining)],
        &[],
    );
    assert!(res.is_ok(), "user_borrow failed: {res:?}");

    let margin_usdc_vault = anchor_spl::associated_token::get_associated_token_address(&margin, &usdc_mint);
    assert_eq!(token_balance(&svm, &margin_usdc_vault), borrow_amount);

    // -- borrower partially repays from margin ----------------------------
    let repay_amount = 200 * 10u64.pow(6);
    let res = send(&mut svm, &borrower, &[ix_user_repay_from_margin(&borrower.pubkey(), &margin, &usdc_mint, repay_amount, false)], &[]);
    assert!(res.is_ok(), "user_repay_from_margin failed: {res:?}");
    assert_eq!(token_balance(&svm, &margin_usdc_vault), borrow_amount - repay_amount);

    // -- borrower withdraws a small amount of WSOL collateral -------------
    // Remaining debt is now backed by both the USDC-as-collateral credit and the still-active
    // USDC debt, neither of which is the named (WSOL) asset, so both must be scanned.
    let mut remaining = collateral_group_metas(&usdc_mint, &margin, &usdc_price_update);
    remaining.extend(debt_group_metas(&usdc_mint, &margin, &usdc_price_update));
    let withdraw_amount = 1 * 10u64.pow(9); // 1 WSOL
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_withdraw_collateral(&borrower.pubkey(), &margin, &wsol_mint, &wsol_price_update, withdraw_amount, 0, &remaining)],
        &[],
    );
    assert!(res.is_ok(), "user_withdraw_collateral failed: {res:?}");

    let borrower_wsol_wallet = anchor_spl::associated_token::get_associated_token_address(&borrower.pubkey(), &wsol_mint);
    assert_eq!(token_balance(&svm, &borrower_wsol_wallet), 90 * 10u64.pow(9) + withdraw_amount);

    // -- lender redeems some shares ----------------------------------------
    let res = send(&mut svm, &lender, &[ix_lender_redeem(&lender.pubkey(), &usdc_mint, 1_000 * 10u64.pow(6), 1)], &[]);
    assert!(res.is_ok(), "lender_redeem failed: {res:?}");
}
