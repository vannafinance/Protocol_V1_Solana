mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;

struct Env {
    admin: Keypair,
    usdc_mint: Pubkey,
    wsol_mint: Pubkey,
    usdc_price_update: Pubkey,
    wsol_price_update: Pubkey,
}

fn setup_protocol_with_two_assets(svm: &mut litesvm::LiteSVM) -> Env {
    let admin = funded_keypair(svm);
    let treasury = Pubkey::new_unique();
    send(svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc_mint = create_mint(svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol_mint = create_mint(svm, &admin, &admin.pubkey(), WSOL_DECIMALS);
    send(svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(svm, &admin, &ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price_update = pyth_account(&USDC_FEED);
    let wsol_price_update = pyth_account(&WSOL_FEED);
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    Env { admin, usdc_mint, wsol_mint, usdc_price_update, wsol_price_update }
}

#[test]
fn donation_into_margin_vault_becomes_live_collateral() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &borrower.pubkey(), 100 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());
    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();

    let deposit_amount = 3 * 10u64.pow(9);
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &env.wsol_mint, deposit_amount)], &[])
        .expect("deposit_collateral");

    let donor = funded_keypair(&mut svm);
    let donation_amount = 5 * 10u64.pow(9);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &donor.pubkey(), donation_amount);
    let donor_ata = get_associated_token_address(&donor.pubkey(), &env.wsol_mint);
    let margin_vault = margin_vault_ata(&margin, &env.wsol_mint);
    let donation_ix = spl_token_interface::instruction::transfer(
        &spl_token_interface::ID,
        &donor_ata,
        &margin_vault,
        &donor.pubkey(),
        &[],
        donation_amount,
    )
    .unwrap();
    let res = send(&mut svm, &donor, &[donation_ix], &[]);
    assert!(res.is_ok(), "raw donation transfer failed: {res:?}");

    let total = deposit_amount + donation_amount;
    assert_eq!(token_balance(&svm, &margin_vault), total);

    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_withdraw_collateral(&borrower.pubkey(), &margin, &env.wsol_mint, total, 0, &oracle_metas(&[env.wsol_price_update]))],
        &[],
    );
    assert!(res.is_ok(), "the full donated + deposited balance should be withdrawable: {res:?}");
    assert_eq!(token_balance(&svm, &margin_vault), 0);
}

#[test]
fn stale_oracle_price_is_rejected() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.usdc_mint, &env.admin, &lender.pubkey(), 1_000_000 * 10u64.pow(6));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &env.usdc_mint, 500_000 * 10u64.pow(6), 1)], &[]).unwrap();

    let borrower = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &borrower.pubkey(), 100 * 10u64.pow(9));
    let (margin, _) = margin_pda(&borrower.pubkey());
    send(&mut svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &env.wsol_mint, 10 * 10u64.pow(9))], &[]).unwrap();

    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &env.wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now - 100_000);

    let remaining = with_oracles(collateral_group_metas(&env.wsol_mint, &margin), &[env.usdc_price_update, env.wsol_price_update]);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &env.usdc_mint, 100 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    );
    assert_vanna_error(res, vanna_credit_layer::errors::VannaError::StalePrice);
}

#[test]
fn wrong_signer_cannot_deposit_for_someone_elses_margin() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let owner = funded_keypair(&mut svm);
    let attacker = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &attacker.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&owner.pubkey());
    send(&mut svm, &owner, &[ix_user_create_margin(&owner.pubkey(), &owner.pubkey())], &[]).unwrap();

    let mut ix = ix_user_deposit_collateral(&owner.pubkey(), &margin, &env.wsol_mint, 1_000_000_000);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == owner.pubkey() && meta.is_signer {
            meta.pubkey = attacker.pubkey();
        }
    }
    let res = send(&mut svm, &attacker, &[ix], &[]);
    assert!(res.is_err(), "a non-owner must not be able to deposit into someone else's margin account");
}

#[test]
fn first_deposit_below_minimum_shares_is_rejected() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.usdc_mint, &env.admin, &lender.pubkey(), 1_000 * 10u64.pow(6));

    let res = send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &env.usdc_mint, 1, 1)], &[]);
    assert!(res.is_err(), "a first supply below the minimum-initial-shares floor must be rejected");

    let res = send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &env.usdc_mint, 10_000, 1)], &[]);
    assert!(res.is_ok(), "a first supply above the minimum-initial-shares floor should succeed: {res:?}");
}

#[test]
fn initialization_cannot_be_front_run_without_admins_signature() {
    let mut svm = setup_svm();
    let attacker = funded_keypair(&mut svm);
    let victim_admin = Pubkey::new_unique();
    let treasury = Pubkey::new_unique();

    let mut ix = ix_initialize_protocol(&victim_admin, &treasury, &attacker.pubkey(), 8);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == victim_admin {
            meta.is_signer = false;
        }
    }
    let res = send(&mut svm, &attacker, &[ix], &[]);
    assert!(res.is_err(), "initialize_protocol must require the named admin's own signature, not just its pubkey as data");
}

#[test]
fn legitimate_admin_signing_for_themselves_can_initialize() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]);
    assert!(res.is_ok(), "an admin signing for themselves should be able to initialize: {res:?}");
}

#[test]
fn zero_treasury_is_rejected() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &Pubkey::default(), &admin.pubkey(), 8)], &[]);
    assert!(res.is_err(), "a zero-address treasury must be rejected");
}

#[test]
fn initialization_cannot_execute_twice() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[])
        .expect("first initialize_protocol should succeed");

    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]);
    assert!(res.is_err(), "a second initialize_protocol call must fail — the PDA already exists");
}
