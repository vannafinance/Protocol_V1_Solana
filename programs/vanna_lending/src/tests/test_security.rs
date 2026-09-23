mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;

const USDC_FEED: [u8; 32] = [1u8; 32];
const WSOL_FEED: [u8; 32] = [2u8; 32];
const USDC_PRICE: i64 = 100_000_000;
const WSOL_PRICE: i64 = 20_000_000_000;

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
    send(svm, &admin, &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &usdc_mint, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(svm, &admin, &[ix_admin_register_asset(&admin.pubkey(), &admin.pubkey(), &wsol_mint, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &usdc_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]).unwrap();
    send(svm, &admin, &[ix_admin_initialize_reserve(&admin.pubkey(), &admin.pubkey(), &wsol_mint, 0, 1_000, 6_000, 8_000, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price_update = Pubkey::new_unique();
    let wsol_price_update = Pubkey::new_unique();
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(svm, &usdc_price_update, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(svm, &wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    Env { admin, usdc_mint, wsol_mint, usdc_price_update, wsol_price_update }
}

/// The margin vault is a plain Associated Token Account with no separate internal ledger — its
/// live SPL balance *is* the credited collateral (see `instructions/margin.rs`). This is
/// deliberately different from the lending `Reserve`'s pooled vault, which still keeps its own
/// `accounted_liquidity_assets` ledger because it is shared across every lender. Here, a raw
/// donation straight into a margin vault is simply extra collateral for that vault's own owner —
/// it counts immediately, and it is fully withdrawable, exactly like a real deposit.
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

    // A donor sends tokens directly into the margin vault ATA, with no protocol instruction
    // involved at all.
    let donor = funded_keypair(&mut svm);
    let donation_amount = 5 * 10u64.pow(9);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &donor.pubkey(), donation_amount);
    let donor_ata = anchor_spl::associated_token::get_associated_token_address(&donor.pubkey(), &env.wsol_mint);
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

    // The donation is immediately reflected — there is no separate ledger to fall behind it.
    let total = deposit_amount + donation_amount;
    assert_eq!(token_balance(&svm, &margin_vault), total);

    // And it is fully withdrawable, exactly like a real deposit, since the vault balance is the
    // sole source of truth for this borrower's own credited collateral.
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_withdraw_collateral(&borrower.pubkey(), &margin, &env.wsol_mint, &env.wsol_price_update, total, 0, &[])],
        &[],
    );
    assert!(res.is_ok(), "the full donated + deposited balance should be withdrawable: {res:?}");
    assert_eq!(token_balance(&svm, &margin_vault), 0);
}

/// A stale Pyth price update must fail closed rather than being used for a risk decision.
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
    send(&mut svm, &borrower, &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &env.usdc_mint)], &[]).unwrap();

    // Push the WSOL price update far into the past relative to the configured max age (3,600s).
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &env.wsol_price_update, WSOL_FEED, WSOL_PRICE, 0, -8, now - 100_000);

    let remaining = collateral_group_metas(&env.wsol_mint, &margin, &env.wsol_price_update);
    let res = send(
        &mut svm,
        &borrower,
        &[ix_user_borrow(&borrower.pubkey(), &margin, &env.usdc_mint, &env.usdc_price_update, 100 * 10u64.pow(6), u128::MAX, &remaining)],
        &[],
    );
    assert!(res.is_err(), "borrowing against a stale WSOL price must fail");
}

/// Only the margin account's own authority may deposit/withdraw on its behalf.
#[test]
fn wrong_signer_cannot_deposit_for_someone_elses_margin() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let owner = funded_keypair(&mut svm);
    let attacker = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.wsol_mint, &env.admin, &attacker.pubkey(), 100 * 10u64.pow(9));

    let (margin, _) = margin_pda(&owner.pubkey());
    send(&mut svm, &owner, &[ix_user_create_margin(&owner.pubkey(), &owner.pubkey())], &[]).unwrap();

    // `attacker` signs a deposit instruction whose `authority` field names `owner`'s margin, but
    // `attacker` is the transaction signer — the `has_one = authority` constraint must reject it.
    let mut ix = ix_user_deposit_collateral(&owner.pubkey(), &margin, &env.wsol_mint, 1_000_000_000);
    // Swap the signer flag onto the attacker's own key instead of the legitimate owner's.
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == owner.pubkey() && meta.is_signer {
            meta.pubkey = attacker.pubkey();
        }
    }
    let res = send(&mut svm, &attacker, &[ix], &[]);
    assert!(res.is_err(), "a non-owner must not be able to deposit into someone else's margin account");
}

/// The very first supply into a reserve must meet the minimum-initial-shares floor, guarding
/// against a first-depositor share-inflation attack on a freshly initialized pool (spec §6.3).
#[test]
fn first_deposit_below_minimum_shares_is_rejected() {
    let mut svm = setup_svm();
    let env = setup_protocol_with_two_assets(&mut svm);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &env.admin, &env.usdc_mint, &env.admin, &lender.pubkey(), 1_000 * 10u64.pow(6));

    // MIN_INITIAL_SHARES is 1_000 raw units; request just 1 raw unit as the very first supply.
    let res = send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &env.usdc_mint, 1, 1)], &[]);
    assert!(res.is_err(), "a first supply below the minimum-initial-shares floor must be rejected");

    // A first supply that clears the floor succeeds.
    let res = send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &env.usdc_mint, 10_000, 1)], &[]);
    assert!(res.is_ok(), "a first supply above the minimum-initial-shares floor should succeed: {res:?}");
}

/// SECURITY_AUDIT_REPORT.md VAN-SOL-001 (Critical): `admin` must be a real `Signer`, not an
/// unverified instruction argument — otherwise any payer could front-run a fresh deployment and
/// name themselves (or anyone else) as the permanent protocol admin.
#[test]
fn initialization_cannot_be_front_run_without_admins_signature() {
    let mut svm = setup_svm();
    let attacker = funded_keypair(&mut svm);
    let victim_admin = Pubkey::new_unique(); // a pubkey the attacker does not control
    let treasury = Pubkey::new_unique();

    // The attacker submits initialize_protocol naming `victim_admin` as admin, but only the
    // attacker's own keypair actually signs — strip the signer flag Anchor's IDL sets for `admin`
    // to simulate exactly that (same technique as `wrong_signer_cannot_deposit_for_someone_elses_margin`).
    let mut ix = ix_initialize_protocol(&victim_admin, &treasury, &attacker.pubkey(), 8);
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == victim_admin {
            meta.is_signer = false;
        }
    }
    let res = send(&mut svm, &attacker, &[ix], &[]);
    assert!(res.is_err(), "initialize_protocol must require the named admin's own signature, not just its pubkey as data");
}

/// The legitimate case the fix must not break: an admin who actually signs for themselves can
/// still initialize the protocol.
#[test]
fn legitimate_admin_signing_for_themselves_can_initialize() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]);
    assert!(res.is_ok(), "an admin signing for themselves should be able to initialize: {res:?}");
}

/// SECURITY_AUDIT_REPORT.md VAN-SOL-001: a zero-address treasury would make fee collection
/// permanently unusable, so it must be rejected at initialization.
#[test]
fn zero_treasury_is_rejected() {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let res = send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &Pubkey::default(), &admin.pubkey(), 8)], &[]);
    assert!(res.is_err(), "a zero-address treasury must be rejected");
}

/// The singleton `ProtocolConfig` PDA can only ever be `init`'d once — a second attempt (even by
/// the legitimate admin) must fail, so a front-runner also cannot "re-initialize" after the fact.
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
