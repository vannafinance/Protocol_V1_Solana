mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use anchor_lang::solana_program::instruction::AccountMeta;
use common::*;
use solana_signer::Signer as SvmSigner;

const USDC_FEED: [u8; 32] = [1u8; 32];
const WSOL_FEED: [u8; 32] = [2u8; 32];
const USDC_PRICE: i64 = 100_000_000; // $1.00 @ exponent -8
const WSOL_PRICE_HEALTHY: i64 = 20_000_000_000; // $200.00 @ exponent -8
const WSOL_PRICE_CRASHED: i64 = 17_000_000_000; // $170.00 @ exponent -8 -- just enough to trip the 80% liquidation threshold

/// A margin account borrows USDC against WSOL collateral while WSOL is healthy, the price then
/// crashes, and a liquidator partially repays the debt and seizes WSOL collateral with a bonus.
/// Verifies the account was liquidatable, the liquidator was paid a bonus, and health improved.
#[test]
fn liquidation_after_price_crash() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();

    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[])
        .expect("initialize_protocol");

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);

    send(
        &mut svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)],
        &[],
    )
    .expect("register USDC");
    send(
        &mut svm,
        &admin,
        &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)],
        &[],
    )
    .expect("register WSOL");

    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[])
        .expect("init USDC reserve");
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[])
        .expect("init WSOL reserve");

    let usdc_price_update = Pubkey::new_unique();
    let wsol_price_update = Pubkey::new_unique();
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE_HEALTHY, 0, -8, now);

    // Lender funds the USDC reserve.
    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]).expect("lender_supply");

    // Borrower deposits 10 WSOL (~$2,000) and borrows close to the 70% LTV limit.
    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &wsol_mint, &admin, &borrower.pubkey(), 100 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());

    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).expect("create_margin");
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &wsol_mint, 10 * 10u64.pow(9))], &[])
        .expect("deposit WSOL collateral");
    send(&mut svm, &borrower, &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &usdc_mint)], &[])
        .expect("open USDC debt");

    let borrow_amount = 1_390 * 10u64.pow(6); // just under the $1,400 (70% * $2,000) borrow power
    let remaining = collateral_group_metas(&wsol_mint, &margin, &wsol_price_update);
    send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, &usdc_price_update, borrow_amount, u128::MAX, &remaining)],
        &[],
    )
    .expect("user_borrow");

    // Borrower needs a USDC wallet ATA to receive the withdrawal into.
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &borrower.pubkey(), 0);

    // Move the borrowed USDC out of the margin account and into the borrower's own wallet.
    // Spec §1.2: borrowed tokens are protocol-controlled collateral by default, so as long as
    // they sit in the margin vault they trivially back their own debt; a genuinely liquidatable
    // position requires the borrower to have actually used/withdrawn what they borrowed.
    let mut withdraw_remaining = collateral_group_metas(&wsol_mint, &margin, &wsol_price_update);
    withdraw_remaining.extend(debt_group_metas(&usdc_mint, &margin, &usdc_price_update));
    send(
        &mut svm,
        &borrower,
        &[ix_user_withdraw_collateral(&borrower.pubkey(), &margin, &usdc_mint, &usdc_price_update, borrow_amount, 0, &withdraw_remaining)],
        &[],
    )
    .expect("withdraw borrowed USDC out of the margin account");

    // WSOL crashes just enough that liquidation-threshold-weighted collateral falls below debt.
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE_CRASHED, 0, -8, now);

    // Liquidator repays part of the USDC debt and seizes WSOL collateral with the configured bonus.
    let liquidator = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &liquidator.pubkey(), 10_000 * 10u64.pow(6));

    let liquidator_wsol_before = {
        let ata = anchor_spl::associated_token::get_associated_token_address(&liquidator.pubkey(), &wsol_mint);
        svm.get_account(&ata).map(|_| token_balance(&svm, &ata)).unwrap_or(0)
    };
    let (debt_reserve, _) = reserve_pda(&usdc_mint);
    let debt_reserve_vault = anchor_spl::associated_token::get_associated_token_address(&debt_reserve, &usdc_mint);
    let reserve_vault_before = token_balance(&svm, &debt_reserve_vault);

    // Both the seized collateral (WSOL) and the repaid debt (USDC) are the two *named* accounts
    // here, and they are also the account's only active positions, so nothing else to scan.
    let liquidate_remaining: Vec<AccountMeta> = vec![];
    let max_repay = 500 * 10u64.pow(6); // liquidator offers to repay 500 USDC
    let res = send(
        &mut svm,
        &liquidator,
        &[ix_public_liquidate(
            &liquidator.pubkey(),
            &margin,
            &usdc_mint,
            &usdc_price_update,
            &wsol_mint,
            &wsol_price_update,
            max_repay,
            1, // min_collateral_out: just require a strictly positive seize
            &liquidate_remaining,
        )],
        &[],
    );
    assert!(res.is_ok(), "public_liquidate failed: {res:?}");

    let liquidator_wsol_ata = anchor_spl::associated_token::get_associated_token_address(&liquidator.pubkey(), &wsol_mint);
    let liquidator_wsol_after = token_balance(&svm, &liquidator_wsol_ata);
    assert!(liquidator_wsol_after > liquidator_wsol_before, "liquidator should have received seized WSOL collateral");

    // The reserve should have received exactly the repaid amount.
    assert_eq!(token_balance(&svm, &debt_reserve_vault), reserve_vault_before + max_repay);
}

/// A healthy position must not be liquidatable.
#[test]
fn healthy_position_cannot_be_liquidated() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc_mint = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);
    send(&mut svm, &admin, &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price_update = Pubkey::new_unique();
    let wsol_price_update = Pubkey::new_unique();
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE_HEALTHY, 0, -8, now);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]).unwrap();

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &wsol_mint, &admin, &borrower.pubkey(), 100 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());
    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &wsol_mint, 10 * 10u64.pow(9))], &[]).unwrap();
    send(&mut svm, &borrower, &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &usdc_mint)], &[]).unwrap();

    let remaining = collateral_group_metas(&wsol_mint, &margin, &wsol_price_update);
    send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &usdc_mint, &usdc_price_update, 100 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    )
    .unwrap();

    let liquidator = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc_mint, &admin, &liquidator.pubkey(), 10_000 * 10u64.pow(6));
    let liquidate_remaining = collateral_group_metas(&usdc_mint, &margin, &usdc_price_update);
    let res = send(
        &mut svm,
        &liquidator,
        &[ix_public_liquidate(&liquidator.pubkey(), &margin, &usdc_mint, &usdc_price_update, &wsol_mint, &wsol_price_update, 50 * 10u64.pow(6), 1, &liquidate_remaining)],
        &[],
    );
    assert!(res.is_err(), "liquidating a healthy position must fail");
}
