mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;

const USDC_FEED: [u8; 32] = [1u8; 32];
const WSOL_FEED: [u8; 32] = [2u8; 32];
const USDC_PRICE: i64 = 100_000_000; // $1.00 @ exponent -8
const WSOL_PRICE: i64 = 20_000_000_000; // $200.00 @ exponent -8

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

    let res = send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]);
    assert!(res.is_ok(), "init USDC reserve failed: {res:?}");

    let res = send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]);
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

/// One signed transaction: no margin account yet, no debt position yet — both get created
/// in-flight, collateral lands, and the borrow executes, atomically.
#[test]
fn deposit_and_borrow_creates_margin_and_debt_position_in_one_tx() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&borrower.pubkey());
    // Brand-new margin account: no other active positions to scan, so `remaining` is empty.
    let deposit_amount = 10 * 10u64.pow(9); // 10 WSOL ~= $2,000
    let borrow_amount = 500 * 10u64.pow(6); // 500 USDC, well within 70% LTV of $2,000
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

    let margin_wsol_vault = anchor_spl::associated_token::get_associated_token_address(&margin, &f.wsol_mint);
    let margin_usdc_vault = anchor_spl::associated_token::get_associated_token_address(&margin, &f.usdc_mint);
    assert_eq!(token_balance(&svm, &margin_wsol_vault), deposit_amount);
    assert_eq!(token_balance(&svm, &margin_usdc_vault), borrow_amount);

    let borrower_wsol_wallet = anchor_spl::associated_token::get_associated_token_address(&borrower.pubkey(), &f.wsol_mint);
    assert_eq!(token_balance(&svm, &borrower_wsol_wallet), 90 * 10u64.pow(9));
}

/// A second deposit_and_borrow for the same wallet reuses the now-existing margin account and
/// debt position (the `init_if_needed` "already exists" branch) instead of erroring.
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

    // Second call: deposit + borrow more of the exact same pair. WSOL (the named deposit asset)
    // and USDC's debt slot (the named borrow asset) are excluded from the scan automatically —
    // but the first call's borrowed USDC also became an active COLLATERAL credit (spec §1.2,
    // same as plain `user_borrow`), and that collateral slot is NOT the named one here (only
    // WSOL is), so it must be supplied via `remaining_accounts`, exactly like
    // `test_full_flow.rs`'s withdraw step scans the USDC-as-collateral credit alongside its debt.
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

    let margin_wsol_vault = anchor_spl::associated_token::get_associated_token_address(&margin, &f.wsol_mint);
    let margin_usdc_vault = anchor_spl::associated_token::get_associated_token_address(&margin, &f.usdc_mint);
    assert_eq!(token_balance(&svm, &margin_wsol_vault), first_deposit + second_deposit);
    assert_eq!(token_balance(&svm, &margin_usdc_vault), first_borrow + second_borrow);
}

/// Borrowing far more than the deposited collateral supports must fail closed with the same
/// health-factor gate `user_borrow` enforces — the atomic instruction can't be used to sneak
/// past the RiskEngine.
#[test]
fn deposit_and_borrow_rejects_unhealthy_borrow() {
    let mut svm = setup_svm();
    let f = setup(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &f.admin, &f.wsol_mint, &f.admin, &borrower.pubkey(), 100 * 10u64.pow(9));

    // 1 WSOL ~= $200 deposited, but try to borrow $1,000 of USDC — far past 70% LTV.
    let deposit_amount = 1 * 10u64.pow(9);
    let borrow_amount = 1_000 * 10u64.pow(6);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_deposit_and_borrow(&borrower.pubkey(), &f.wsol_mint, &f.wsol_price_update, &f.usdc_mint, &f.usdc_price_update, deposit_amount, borrow_amount, u128::MAX, &[])],
        &[],
    );
    assert!(res.is_err(), "unhealthy deposit_and_borrow unexpectedly succeeded");
}

/// The instruction is cross-asset only — depositing and borrowing the SAME mint must be rejected
/// rather than silently aliasing the deposit-side and borrow-side accounts.
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
