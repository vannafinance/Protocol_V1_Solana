//! Whole-account liquidation, as in the Solidity AccountManager: once the health factor is at or
//! below 1.10 (including below 1), a liquidator repays every debt in full and receives every asset
//! the margin account holds, in one transaction.

mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_lending::errors::VannaError;
use vanna_lending::events::{DebtRepaid, Liquidated};
use vanna_lending::math::interest::accrue;
use vanna_lending::math::shares::debt_shares_to_assets_up;

const USDC: u64 = 1_000_000;
const SOL: u64 = 1_000_000_000;

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc: Pubkey,
    sol: Pubkey,
    usdc_price: Pubkey,
    sol_price: Pubkey,
}

fn assert_vanna_error(res: TransactionResult, err: VannaError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + err as u32;
    let failure = format!("{:?}", res.expect_err("transaction should have failed").err);
    assert!(failure.contains(&format!("Custom({code})")), "expected error {code}, got {failure}");
}

/// USDC and SOL pools with lenders; SOL at $200.
fn setup() -> Env {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();
    let usdc = create_mint(&mut svm, &admin, &a, USDC_DECIMALS);
    let sol = create_mint(&mut svm, &admin, &a, WSOL_DECIMALS);
    for (mint, feed) in [(usdc, USDC_FEED), (sol, WSOL_FEED)] {
        send(&mut svm, &admin, &[ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[])
            .unwrap();
        send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    }
    let (usdc_price, sol_price) = (Pubkey::new_unique(), Pubkey::new_unique());
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &sol_price, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc, &admin, &lender.pubkey(), 100_000 * USDC);
    mint_to_wallet(&mut svm, &admin, &sol, &admin, &lender.pubkey(), 1_000 * SOL);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc, 100_000 * USDC, 1)], &[]).unwrap();
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &sol, 1_000 * SOL, 1)], &[]).unwrap();
    Env { svm, admin, usdc, sol, usdc_price, sol_price }
}

impl Env {
    fn set_sol_price(&mut self, price: i64) {
        let now = self.svm.get_sysvar::<Clock>().unix_timestamp;
        let (sol_price, usdc_price) = (self.sol_price, self.usdc_price);
        set_price(&mut self.svm, &sol_price, WSOL_FEED, price, 0, -8, now);
        set_price(&mut self.svm, &usdc_price, USDC_FEED, USDC_PRICE, 0, -8, now);
    }

    /// Deposits `sol` SOL and borrows `usdc` USDC, then takes the borrowed USDC out of the margin
    /// so the SOL is the only collateral.
    fn sol_borrower(&mut self, sol: u64, usdc: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        let (admin, sol_mint, usdc_mint) = (self.admin.insecure_clone(), self.sol, self.usdc);
        mint_to_wallet(&mut self.svm, &admin, &sol_mint, &admin, &user.pubkey(), sol);
        mint_to_wallet(&mut self.svm, &admin, &usdc_mint, &admin, &user.pubkey(), 0);
        let u = user.pubkey();
        let ixs = [
            ix_user_create_margin(&u, &u),
            ix_user_deposit_collateral(&u, &margin, &self.sol, sol),
            ix_user_open_debt_position(&u, &u, &margin, &self.usdc),
        ];
        send(&mut self.svm, &user, &ixs, &[]).unwrap();
        let sol_group = collateral_group_metas(&self.sol, &margin, &self.sol_price);
        let borrow = ix_user_borrow(&u, &margin, &self.usdc, &self.usdc_price, usdc, u128::MAX, &sol_group);
        send(&mut self.svm, &user, &[borrow], &[]).unwrap();
        let mut health = sol_group;
        health.extend(debt_group_metas(&self.usdc, &margin, &self.usdc_price));
        let withdraw = ix_user_withdraw_collateral(&u, &margin, &self.usdc, &self.usdc_price, usdc, 0, &health);
        send(&mut self.svm, &user, &[withdraw], &[]).unwrap();
        (user, margin)
    }

    /// A liquidator holding `usdc` USDC, with empty SOL and USDC accounts to receive into.
    fn liquidator(&mut self, usdc: u64) -> (Keypair, Pubkey, Pubkey) {
        let liquidator = funded_keypair(&mut self.svm);
        let (admin, sol, usdc_mint) = (self.admin.insecure_clone(), self.sol, self.usdc);
        let usdc_ata = mint_to_wallet(&mut self.svm, &admin, &usdc_mint, &admin, &liquidator.pubkey(), usdc);
        let sol_ata = mint_to_wallet(&mut self.svm, &admin, &sol, &admin, &liquidator.pubkey(), 0);
        (liquidator, usdc_ata, sol_ata)
    }

    fn debt(&self, margin: &Pubkey, mint: &Pubkey) -> u64 {
        let reserve = fetch_reserve(&self.svm, mint);
        let now = self.svm.get_sysvar::<Clock>().unix_timestamp;
        let live = accrue(&reserve, now).unwrap();
        let position = fetch_debt_position(&self.svm, margin, mint);
        debt_shares_to_assets_up(position.borrow_shares, reserve.total_borrow_shares, live.new_total_borrow_assets).unwrap()
    }
}

#[derive(Clone, Copy)]
struct Keys {
    sol: Pubkey,
    usdc: Pubkey,
    sol_price: Pubkey,
    usdc_price: Pubkey,
}

impl Env {
    fn keys(&self) -> Keys {
        Keys { sol: self.sol, usdc: self.usdc, sol_price: self.sol_price, usdc_price: self.usdc_price }
    }
}

/// SOL collateral swept to the liquidator, USDC debt repaid from the liquidator's wallet.
fn sol_for_usdc(k: Keys, margin: &Pubkey, sol_destination: &Pubkey, usdc_source: &Pubkey) -> Vec<LiqPosition> {
    vec![
        liq_collateral(&k.sol, &anchor_spl::token::ID, margin, &k.sol_price, None, sol_destination),
        liq_debt(&k.usdc, margin, &k.usdc_price, usdc_source),
    ]
}

/// 10 SOL against 1,390 USDC: at $150 per SOL the health factor is 1,500 / 1,390 ≈ 1.079. A day
/// later the liquidator repays the debt with interest and takes all 10 SOL; the pool is made whole.
#[test]
fn unhealthy_account_is_liquidated_whole() {
    let mut env = setup();
    let (user, margin) = env.sol_borrower(10 * SOL, 1_390 * USDC);
    advance_time(&mut env.svm, 86_400);
    env.set_sol_price(15_000_000_000);
    let debt = env.debt(&margin, &env.usdc);
    assert!(debt > 1_390 * USDC, "a day of interest accrued");

    let (liquidator, usdc_ata, sol_ata) = env.liquidator(2_000 * USDC);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &sol_for_usdc(env.keys(), &margin, &sol_ata, &usdc_ata));
    let meta = send(&mut env.svm, &liquidator, &[ix], &[]).expect("liquidate");

    assert_eq!(token_balance(&env.svm, &sol_ata), 10 * SOL, "every SOL swept");
    assert_eq!(token_balance(&env.svm, &usdc_ata), 2_000 * USDC - debt, "debt repaid in full, interest included");
    let repaid: DebtRepaid = event(&meta.logs);
    assert_eq!((repaid.assets, repaid.remaining_debt_shares), (debt, 0));
    let liquidated: Liquidated = event(&meta.logs);
    assert_eq!((liquidated.collaterals_seized, liquidated.debts_repaid), (1, 1));
    assert!(liquidated.health_factor_wad > 1_070_000_000_000_000_000 && liquidated.health_factor_wad < 1_080_000_000_000_000_000);

    let account = fetch_margin(&env.svm, &user.pubkey());
    assert_eq!((account.collateral_count, account.debt_count), (0, 0), "nothing left on the account");
    assert_eq!(fetch_debt_position(&env.svm, &margin, &env.usdc).borrow_shares, 0);
    let pool = fetch_reserve(&env.svm, &env.usdc);
    assert_eq!((pool.total_borrow_assets, pool.total_borrow_shares), (0, 0));
    assert_eq!(pool.accounted_liquidity_assets, 100_000 * USDC - 1_390 * USDC + debt);
}

/// Below a health factor of 1 the account can still be liquidated: the liquidator pays the whole
/// debt and takes collateral worth less, and the lenders are made whole.
#[test]
fn account_below_health_factor_one_is_liquidated() {
    let mut env = setup();
    let (_user, margin) = env.sol_borrower(10 * SOL, 1_390 * USDC);
    env.set_sol_price(10_000_000_000); // $100: HF = 1,000 / 1,390 ≈ 0.72
    let (liquidator, usdc_ata, sol_ata) = env.liquidator(2_000 * USDC);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &sol_for_usdc(env.keys(), &margin, &sol_ata, &usdc_ata));
    let meta = send(&mut env.svm, &liquidator, &[ix], &[]).expect("liquidate below HF 1");
    let liquidated: Liquidated = event(&meta.logs);
    assert!(liquidated.health_factor_wad < 730_000_000_000_000_000);
    assert_eq!(token_balance(&env.svm, &sol_ata), 10 * SOL);
    let pool = fetch_reserve(&env.svm, &env.usdc);
    assert_eq!((pool.total_borrow_assets, pool.accounted_liquidity_assets), (0, 100_000 * USDC));
}

#[test]
fn healthy_account_cannot_be_liquidated() {
    let mut env = setup();
    let (_user, margin) = env.sol_borrower(10 * SOL, 1_390 * USDC); // $200: HF ≈ 1.44
    let (liquidator, usdc_ata, sol_ata) = env.liquidator(2_000 * USDC);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &sol_for_usdc(env.keys(), &margin, &sol_ata, &usdc_ata));
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::PositionHealthy);
}

/// Two debts (USDC, SOL) and two collaterals (SOL, USDC) settle in one transaction. Collateral is
/// swept first, so the liquidator repays the SOL debt with swept SOL and brings no SOL of its own.
#[test]
fn every_debt_and_collateral_settles_in_one_transaction() {
    let mut env = setup();
    let user = funded_keypair(&mut env.svm);
    let (margin, _) = margin_pda(&user.pubkey());
    let (admin, sol, usdc) = (env.admin.insecure_clone(), env.sol, env.usdc);
    mint_to_wallet(&mut env.svm, &admin, &sol, &admin, &user.pubkey(), 10 * SOL);
    mint_to_wallet(&mut env.svm, &admin, &usdc, &admin, &user.pubkey(), 100 * USDC);
    let u = user.pubkey();
    let setup_ixs = [
        ix_user_create_margin(&u, &u),
        ix_user_deposit_collateral(&u, &margin, &sol, 10 * SOL),
        ix_user_deposit_collateral(&u, &margin, &usdc, 100 * USDC),
        ix_user_open_debt_position(&u, &u, &margin, &usdc),
        ix_user_open_debt_position(&u, &u, &margin, &sol),
    ];
    send(&mut env.svm, &user, &setup_ixs, &[]).unwrap();
    let sol_group = collateral_group_metas(&sol, &margin, &env.sol_price);
    let usdc_group = collateral_group_metas(&usdc, &margin, &env.usdc_price);
    send(&mut env.svm, &user, &[ix_user_borrow(&u, &margin, &usdc, &env.usdc_price, 1_000 * USDC, u128::MAX, &sol_group)], &[]).unwrap();
    let mut others = usdc_group.clone();
    others.extend(debt_group_metas(&usdc, &margin, &env.usdc_price));
    send(&mut env.svm, &user, &[ix_user_borrow(&u, &margin, &sol, &env.sol_price, 2 * SOL, u128::MAX, &others)], &[]).unwrap();
    // SOL $200 -> $110: collateral 12 SOL + 1,100 USDC = $2,420; debt $1,000 + 2 SOL = $1,220 -> HF 1.98.
    // Take 1,000 USDC out first so the account is thin: $1,420 / $1,220 ≈ 1.16, then crash to $95.
    let mut health = sol_group.clone();
    health.extend(debt_group_metas(&usdc, &margin, &env.usdc_price));
    health.extend(debt_group_metas(&sol, &margin, &env.sol_price));
    send(&mut env.svm, &user, &[ix_user_withdraw_collateral(&u, &margin, &usdc, &env.usdc_price, 1_000 * USDC, 0, &health)], &[]).unwrap();
    env.set_sol_price(9_500_000_000); // 12 SOL × $95 + 100 USDC = $1,240 vs $1,000 + $190 -> HF ≈ 1.04

    let (liquidator, usdc_ata, sol_ata) = env.liquidator(1_000 * USDC);
    let positions = [
        liq_collateral(&sol, &anchor_spl::token::ID, &margin, &env.sol_price, None, &sol_ata),
        liq_collateral(&usdc, &anchor_spl::token::ID, &margin, &env.usdc_price, None, &usdc_ata),
        liq_debt(&usdc, &margin, &env.usdc_price, &usdc_ata),
        liq_debt(&sol, &margin, &env.sol_price, &sol_ata),
    ];
    let meta = send(&mut env.svm, &liquidator, &[ix_public_liquidate(&liquidator.pubkey(), &margin, &positions)], &[])
        .expect("liquidate two debts and two collaterals");
    let liquidated: Liquidated = event(&meta.logs);
    assert_eq!((liquidated.collaterals_seized, liquidated.debts_repaid), (2, 2));
    assert_eq!(token_balance(&env.svm, &sol_ata), 12 * SOL - 2 * SOL, "swept 12 SOL, repaid the 2 SOL debt from them");
    assert_eq!(token_balance(&env.svm, &usdc_ata), 1_000 * USDC + 100 * USDC - 1_000 * USDC);
    let account = fetch_margin(&env.svm, &u);
    assert_eq!((account.collateral_count, account.debt_count), (0, 0));
}

/// The liquidation accounts must cover every position exactly: nothing missing, extra, swapped
/// or pointing at the wrong vault.
#[test]
fn liquidation_accounts_are_validated() {
    let mut env = setup();
    let (user, margin) = env.sol_borrower(10 * SOL, 1_390 * USDC);
    env.set_sol_price(15_000_000_000);
    let (liquidator, usdc_ata, sol_ata) = env.liquidator(2_000 * USDC);
    let l = liquidator.pubkey();
    let k = env.keys();
    let positions = || sol_for_usdc(k, &margin, &sol_ata, &usdc_ata);

    // Debt left out entirely.
    let ix = ix_public_liquidate(&l, &margin, &positions()[..1]);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::IncompletePositionAccounts);
    // Settlement accounts missing.
    let mut short = positions();
    short[1].settlement.pop();
    let ix = ix_public_liquidate(&l, &margin, &short);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::IncompletePositionAccounts);
    // An extra trailing account.
    let mut long = positions();
    long[1].settlement.push(anchor_lang::solana_program::instruction::AccountMeta::new_readonly(Pubkey::new_unique(), false));
    let ix = ix_public_liquidate(&l, &margin, &long);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::IncompletePositionAccounts);
    // Repayment sent to a vault that isn't the pool's.
    let mut wrong_vault = positions();
    wrong_vault[1].settlement[1].pubkey = usdc_ata;
    let ix = ix_public_liquidate(&l, &margin, &wrong_vault);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::InvalidVaultAuthority);
    // Wrong mint for the collateral.
    let mut wrong_mint = positions();
    wrong_mint[0].settlement[0].pubkey = env.usdc;
    let ix = ix_public_liquidate(&l, &margin, &wrong_mint);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::InvalidMint);
    // A liquidator who can't cover the debt: the whole transaction reverts.
    let (poor, poor_usdc, poor_sol) = env.liquidator(100 * USDC);
    let ix = ix_public_liquidate(&poor.pubkey(), &margin, &sol_for_usdc(env.keys(), &margin, &poor_sol, &poor_usdc));
    assert!(send(&mut env.svm, &poor, &[ix], &[]).is_err());
    assert_eq!(fetch_margin(&env.svm, &user.pubkey()).collateral_count, 1, "nothing moved");
    assert_eq!(token_balance(&env.svm, &poor_sol), 0);

    let ix = ix_public_liquidate(&l, &margin, &positions());
    send(&mut env.svm, &liquidator, &[ix], &[]).expect("correct accounts liquidate");
}

/// Like Solidity's `liquidate`, liquidation is never paused, and it sweeps an asset even after
/// the admin stopped accepting it as collateral.
#[test]
fn liquidation_is_never_paused_and_sweeps_disabled_collateral() {
    let mut env = setup();
    let (_user, margin) = env.sol_borrower(10 * SOL, 1_390 * USDC);
    env.set_sol_price(15_000_000_000);
    let a = env.admin.pubkey();
    let disable = ix_admin_update_asset_config(&a, &env.sol, 0, 8_000, 8_500, 500, 1_000, 3_600, false, true);
    send(&mut env.svm, &env.admin, &[disable, ix_admin_set_operating_mode(&a, 3)], &[]).unwrap();

    let (liquidator, usdc_ata, sol_ata) = env.liquidator(2_000 * USDC);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &sol_for_usdc(env.keys(), &margin, &sol_ata, &usdc_ata));
    send(&mut env.svm, &liquidator, &[ix], &[]).expect("liquidate while Halted and SOL collateral-disabled");
    assert_eq!(token_balance(&env.svm, &sol_ata), 10 * SOL);
}
