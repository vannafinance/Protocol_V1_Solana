mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use common::kamino::set_token_balance;
use common::oracles::*;
use common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_credit_layer::errors::VannaError;
use vanna_credit_layer::events::{Borrowed, Liquidated};
use vanna_credit_layer::math::health::{calculate_health, CollateralValuation, DebtValuation};
use vanna_oracle::reference::known_mints;
use vanna_credit_layer::risk_engine::require_checks;
use vanna_credit_layer::interface::PriceChecks;
use vanna_oracle::{Price, OracleError, SCOPE_CHAIN_UNUSED};
use vanna_oracle::PriceSourceSet;

const USDC: u64 = 1_000_000;
const XSTOCK: u64 = 100_000_000;
const LST: u64 = 1_000_000_000;
const USD: u128 = 1_000_000_000;

const START: i64 = 1_800_000_000;
const SOL_ENTRY: u16 = 3;
const SOL_TWAP: u16 = 455;
const XSTOCK_ENTRY: u16 = 332;
const XSTOCK_TWAP: u16 = 269;
const STAKE_RATE: u16 = 210;
const SCOPE_200: (u64, u64) = (200_000_000, 6);
const JUPSOL_RATE_FEED: [u8; 32] = [4u8; 32];

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc: Pubkey,
    usdc_price: Pubkey,
    sol_price: Pubkey,
    scope: Pubkey,
}

fn usdc_value(amount: u64, round_up: bool) -> u128 {
    Price { value: USDC_PRICE as u64, exponent: -8 }.value_of(amount as u128, 6, round_up).unwrap()
}

fn assert_pricing_error(res: TransactionResult, error: OracleError) {
    assert_custom_error(res, error.into());
}

fn price_source(svm: &LiteSVM, mint: &Pubkey) -> OracleConfig {
    let book = svm.get_account(&price_book()).unwrap();
    let book: &vanna_oracle::PriceBook = bytemuck::from_bytes(&book.data[8..]);
    book.find(mint).expect("priced").config
}

fn health(collateral: &[u128], debt: &[u128]) -> u128 {
    let c: Vec<_> = collateral.iter().map(|v| CollateralValuation { collateral_value: *v }).collect();
    let d: Vec<_> = debt.iter().map(|v| DebtValuation { debt_value: *v }).collect();
    calculate_health(&c, &d).unwrap().borrow_health_factor_wad
}

fn setup() -> Env {
    let mut svm = setup_svm();
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = START;
    svm.set_sysvar(&clock);
    let usdc_price = set_pyth(&mut svm, USDC_FEED, USDC_PRICE, -8, START);
    let sol_price = set_pyth(&mut svm, WSOL_FEED, WSOL_PRICE, -8, START);
    let scope = Pubkey::new_unique();
    let (v, e) = SCOPE_200;
    set_scope_prices(
        &mut svm,
        &scope,
        &[(SOL_ENTRY, v, e, START), (SOL_TWAP, v, e, START), (XSTOCK_ENTRY, v, e, START), (XSTOCK_TWAP, v, e, START), (STAKE_RATE, 1_250_000_000, 9, START)],
    );

    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();
    let usdc = create_mint(&mut svm, &admin, &a, USDC_DECIMALS);
    send(&mut svm, &admin, &ix_admin_register_asset(&a, &a, &usdc, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &usdc, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc, &admin, &lender.pubkey(), 100_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc, 100_000 * USDC, 1)], &[]).unwrap();

    Env { svm, admin, usdc, usdc_price, sol_price, scope }
}

impl Env {
    fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    fn set_scope_at(&mut self, entries: &[(u16, u64, u64)], timestamp: i64) {
        let entries: Vec<_> = entries.iter().map(|(i, v, e)| (*i, *v, *e, timestamp)).collect();
        let scope = self.scope;
        set_scope_prices(&mut self.svm, &scope, &entries);
    }

    fn set_scope(&mut self, entries: &[(u16, u64, u64)]) {
        let now = self.now();
        self.set_scope_at(entries, now);
    }

    fn set_pyth(&mut self, feed: [u8; 32], price: i64, conf: u64) {
        let now = self.now();
        set_price(&mut self.svm, &pyth_account(&feed), feed, price, conf, -8, now);
    }

    fn oracles(&self) -> Vec<Pubkey> {
        vec![self.usdc_price, self.sol_price, self.scope]
    }

    fn register(&mut self, mint: &Pubkey, token_program: &Pubkey, oracle: OracleConfig) -> TransactionResult {
        let ixs = ix_admin_register_collateral(&self.admin.pubkey(), mint, token_program, oracle, &[]);
        send(&mut self.svm, &self.admin, &ixs, &[])
    }

    fn xstock(&mut self) -> (Pubkey, Keypair) {
        let authority = funded_keypair(&mut self.svm);
        let mint = create_scaled_ui_mint(&mut self.svm, &self.admin, &authority, 8, 1.0017, None);
        let scope = self.scope;
        self.register(&mint, &TOKEN_2022, scope_oracle(&scope, &[XSTOCK_ENTRY], &[XSTOCK_TWAP], 300, 500)).expect("register xStock");
        (mint, authority)
    }

    fn collateral(&mut self, oracle: OracleConfig) -> Pubkey {
        let a = self.admin.pubkey();
        let mint = create_mint(&mut self.svm, &self.admin, &a, 9);
        self.register(&mint, &anchor_spl::token::ID, oracle).expect("register collateral");
        mint
    }

    fn scope_sol_with_fallback(&mut self) -> Pubkey {
        let oracle = OracleConfig { pyth_price: self.sol_price, ..scope_oracle(&self.scope, &[SOL_ENTRY], &[SOL_TWAP], 300, 1_000) };
        self.collateral(oracle)
    }

    fn user_with(&mut self, mint: &Pubkey, token_program: &Pubkey, authority: &Keypair, amount: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        mint_to_wallet_with(&mut self.svm, &self.admin, mint, token_program, authority, &user.pubkey(), amount);
        let u = user.pubkey();
        let ixs = [
            ix_user_create_margin(&u, &u),
            ix_user_deposit_collateral_with(&u, &margin, mint, token_program, amount),
        ];
        send(&mut self.svm, &user, &ixs, &[]).expect("deposit collateral");
        (user, margin)
    }

    fn classic_user(&mut self, mint: &Pubkey, amount: u64) -> (Keypair, Pubkey) {
        let admin = self.admin.insecure_clone();
        self.user_with(mint, &anchor_spl::token::ID, &admin, amount)
    }

    fn borrow(&mut self, user: &Keypair, amount: u64, groups: Vec<AccountMeta>, oracles: &[Pubkey]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let ix = ix_user_borrow(&user.pubkey(), &margin, &self.usdc, amount, u128::MAX, &with_oracles(groups, oracles));
        send(&mut self.svm, user, &[ix], &[])
    }

    fn borrow_out(&mut self, user: &Keypair, amount: u64, group: Vec<AccountMeta>) {
        let (margin, oracles) = (margin_pda(&user.pubkey()).0, self.oracles());
        self.borrow(user, amount, group.clone(), &oracles).expect("borrow");
        let mut others = group;
        others.extend(debt_group_metas(&self.usdc, &margin));
        let (admin, usdc) = (self.admin.insecure_clone(), self.usdc);
        mint_to_wallet(&mut self.svm, &admin, &usdc, &admin, &user.pubkey(), 0);
        let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &usdc, amount, 0, &with_oracles(others, &oracles));
        send(&mut self.svm, user, &[ix], &[]).expect("take the borrowed USDC out");
    }
}

#[test]
fn xstock_is_priced_by_scope_per_raw_token() {
    let mut env = setup();
    let (nvdax, authority) = env.xstock();
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let oracles = env.oracles();
    let meta = env.borrow(&user, 100 * USDC, collateral_group_with(&nvdax, &TOKEN_2022, &margin), &oracles).expect("borrow");

    let nvdax_value = Price { value: 200_000_000, exponent: -6 }.value_of(10 * XSTOCK as u128, 8, false).unwrap();
    assert_eq!(nvdax_value, 2_000 * USD);
    let borrowed: Borrowed = event(&meta.logs);
    let expected = health(&[nvdax_value, usdc_value(100 * USDC, false)], &[usdc_value(100 * USDC, true)]);
    assert_eq!(borrowed.borrow_health_factor_wad, expected);
}

#[test]
fn a_scope_chain_is_the_product_of_its_entries() {
    let mut env = setup();
    let scope = env.scope;
    let lst = env.collateral(scope_oracle(&scope, &[STAKE_RATE, SOL_ENTRY], &[STAKE_RATE, SOL_TWAP], 300, 1_000));
    let (user, margin) = env.classic_user(&lst, 10 * LST);
    let (group, oracles) = (collateral_group_metas(&lst, &margin), env.oracles());
    assert_vanna_error(env.borrow(&user, 25_000 * USDC, group.clone(), &oracles), VannaError::HealthFactorTooLow);
    env.borrow(&user, 24_000 * USDC, group, &oracles).expect("(2,500 + 24,000) / 24,000 ≈ 1.104");
}

#[test]
fn a_pyth_factor_multiplies_the_price() {
    let mut env = setup();
    let now = env.now();
    let rate = set_pyth(&mut env.svm, JUPSOL_RATE_FEED, 120_000_000, -8, now);
    let oracle = OracleConfig { pyth_price: rate, pyth_factor: env.sol_price, max_age_secs: 300, max_confidence_bps: 1_000, ..OracleConfig::default() };
    let jupsol = env.collateral(oracle);
    let (user, margin) = env.classic_user(&jupsol, 10 * LST);
    let group = collateral_group_metas(&jupsol, &margin);

    assert_pricing_error(env.borrow(&user, 100 * USDC, group.clone(), &[env.usdc_price, rate]), OracleError::InvalidPriceSource);
    let oracles = [env.usdc_price, env.sol_price, rate];
    assert_vanna_error(env.borrow(&user, 24_000 * USDC, group.clone(), &oracles), VannaError::HealthFactorTooLow);
    env.borrow(&user, 23_000 * USDC, group, &oracles).expect("(2,400 + 23,000) / 23,000 ≈ 1.104");
}

#[test]
fn scope_is_primary_and_pyth_takes_over_when_it_is_stale() {
    let mut env = setup();
    let hsol = env.scope_sol_with_fallback();
    env.set_pyth(WSOL_FEED, 10_000_000_000, 0);

    let (user, margin) = env.classic_user(&hsol, 10 * LST);
    let group = collateral_group_metas(&hsol, &margin);
    env.borrow(&user, 12_000 * USDC, group, &[env.usdc_price, env.scope]).expect("priced by Scope alone");

    let (v, e) = SCOPE_200;
    env.set_scope_at(&[(SOL_ENTRY, v, e), (SOL_TWAP, v, e)], START - 1_000);
    let (user, margin) = env.classic_user(&hsol, 10 * LST);
    let (group, oracles) = (collateral_group_metas(&hsol, &margin), env.oracles());
    assert_pricing_error(env.borrow(&user, 100 * USDC, group.clone(), &[env.usdc_price, env.scope]), OracleError::InvalidPriceSource);
    assert_vanna_error(env.borrow(&user, 12_000 * USDC, group.clone(), &oracles), VannaError::HealthFactorTooLow);
    env.borrow(&user, 9_000 * USDC, group.clone(), &oracles).expect("priced by the Pyth fallback");

    let old = env.now() - 1_000;
    set_price(&mut env.svm, &env.sol_price.clone(), WSOL_FEED, 10_000_000_000, 0, -8, old);
    assert_vanna_error(env.borrow(&user, USDC, group, &oracles), VannaError::StalePrice);
}

#[test]
fn a_zero_scope_entry_is_unusable() {
    let mut env = setup();
    let (nvdax, authority) = env.xstock();
    let hsol = env.scope_sol_with_fallback();
    let (x_user, x_margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let (s_user, s_margin) = env.classic_user(&hsol, 10 * LST);
    env.set_scope(&[(XSTOCK_ENTRY, 0, 6), (SOL_ENTRY, 0, 6)]);

    let oracles = env.oracles();
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &x_margin);
    assert_pricing_error(env.borrow(&x_user, 100 * USDC, group, &oracles), OracleError::PriceUnavailable);
    env.borrow(&s_user, 100 * USDC, collateral_group_metas(&hsol, &s_margin), &oracles).expect("Pyth fallback");
}

#[test]
fn oracle_accounts_cannot_be_substituted() {
    let mut env = setup();
    let (nvdax, authority) = env.xstock();
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let fake = Pubkey::new_unique();
    let mut account = env.svm.get_account(&env.scope).unwrap();
    account.data[40 + 56 * XSTOCK_ENTRY as usize..][..8].copy_from_slice(&10_000_000_000u64.to_le_bytes());
    env.svm.set_account(fake, account).unwrap();
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &margin);

    assert_pricing_error(env.borrow(&user, 25_000 * USDC, group.clone(), &[env.usdc_price, fake]), OracleError::InvalidPriceSource);
    let both = [env.usdc_price, fake, env.scope];
    assert_vanna_error(env.borrow(&user, 25_000 * USDC, group, &both), VannaError::HealthFactorTooLow);
}

#[test]
fn stale_prices_block_everything_but_debt_free_withdrawals() {
    let mut env = setup();
    let (nvdax, authority) = env.xstock();
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &margin);
    env.borrow_out(&user, 1_000 * USDC, group.clone());
    let (saver, saver_margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);

    advance_time(&mut env.svm, 301);
    env.set_pyth(USDC_FEED, USDC_PRICE, 0);
    let oracles = env.oracles();
    assert_vanna_error(env.borrow(&user, USDC, group, &oracles), VannaError::StalePrice);
    let debt = with_oracles(debt_group_metas(&env.usdc, &margin), &oracles);
    let ix = ix_user_withdraw_collateral_with(&user.pubkey(), &margin, &nvdax, &TOKEN_2022, XSTOCK, 0, &debt);
    assert_vanna_error(send(&mut env.svm, &user, &[ix], &[]), VannaError::StalePrice);
    let liquidator = funded_keypair(&mut env.svm);
    let positions = [
        liq_collateral(&nvdax, &TOKEN_2022, &margin, &Pubkey::new_unique()),
        liq_debt(&env.usdc, &margin, &Pubkey::new_unique()),
    ];
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions, &oracles);
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::StalePrice);

    let ix = ix_user_withdraw_collateral_with(&saver.pubkey(), &saver_margin, &nvdax, &TOKEN_2022, 10 * XSTOCK, 0, &[]);
    send(&mut env.svm, &saver, &[ix], &[]).expect("a debt-free withdrawal needs no price");
}

#[test]
fn twap_divergence_blocks_borrowing_but_not_liquidation() {
    let mut env = setup();
    let (nvdax, authority) = env.xstock();
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &margin);
    env.borrow_out(&user, 1_500 * USDC, group.clone());

    let (admin, usdc) = (env.admin.insecure_clone(), env.usdc);
    let liquidator = funded_keypair(&mut env.svm);
    let usdc_account = mint_to_wallet(&mut env.svm, &admin, &usdc, &admin, &liquidator.pubkey(), 2_000 * USDC);
    let nvdax_account = mint_to_wallet_with(&mut env.svm, &admin, &nvdax, &TOKEN_2022, &authority, &liquidator.pubkey(), 0);
    let positions = [
        liq_collateral(&nvdax, &TOKEN_2022, &margin, &nvdax_account),
        liq_debt(&usdc, &margin, &usdc_account),
    ];
    let oracles = env.oracles();
    let liquidate = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions, &oracles);

    env.set_scope(&[(XSTOCK_ENTRY, 170_000_000, 6)]);
    assert_vanna_error(env.borrow(&user, USDC, group, &oracles), VannaError::PriceTooDivergentFromTwap);
    assert_vanna_error(send(&mut env.svm, &liquidator, std::slice::from_ref(&liquidate), &[]), VannaError::PositionHealthy);

    env.set_scope(&[(XSTOCK_ENTRY, 160_000_000, 6)]);
    let meta = send(&mut env.svm, &liquidator, &[liquidate], &[]).expect("liquidation needs only a fresh price");
    let liquidated: Liquidated = event(&meta.logs);
    assert_eq!(liquidated.collateral_value, 1_600 * USD);
    assert_eq!(token_balance(&env.svm, &nvdax_account), 10 * XSTOCK);
    assert_eq!(token_balance(&env.svm, &usdc_account), 500 * USDC);
    assert_eq!(fetch_margin(&env.svm, &user.pubkey()).collateral_count, 0);
}

#[test]
fn wide_pyth_confidence_blocks_borrowing_but_not_liquidation() {
    let mut env = setup();
    let psol = env.collateral(pyth_oracle(&WSOL_FEED, 300, 100));
    let (user, margin) = env.classic_user(&psol, 10 * LST);
    let group = collateral_group_metas(&psol, &margin);
    env.borrow_out(&user, 1_500 * USDC, group.clone());

    env.set_pyth(WSOL_FEED, 16_000_000_000, 800_000_000);
    let oracles = env.oracles();
    assert_vanna_error(env.borrow(&user, USDC, group, &oracles), VannaError::ConfidenceTooWide);

    let (admin, usdc) = (env.admin.insecure_clone(), env.usdc);
    let liquidator = funded_keypair(&mut env.svm);
    let usdc_account = mint_to_wallet(&mut env.svm, &admin, &usdc, &admin, &liquidator.pubkey(), 2_000 * USDC);
    let psol_account = mint_to_wallet(&mut env.svm, &admin, &psol, &admin, &liquidator.pubkey(), 0);
    let positions = [
        liq_collateral(&psol, &anchor_spl::token::ID, &margin, &psol_account),
        liq_debt(&usdc, &margin, &usdc_account),
    ];
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions, &oracles);
    send(&mut env.svm, &liquidator, &[ix], &[]).expect("liquidation ignores confidence");
    assert_eq!(token_balance(&env.svm, &psol_account), 10 * LST);
}

#[test]
fn oracle_admin_rules() {
    let mut env = setup();
    let a = env.admin.pubkey();
    let mint = create_mint(&mut env.svm, &env.admin, &a, 9);
    let scope = env.scope;
    let register = |env: &mut Env, oracle: OracleConfig| env.register(&mint, &anchor_spl::token::ID, oracle);

    assert_pricing_error(register(&mut env, OracleConfig::default()), OracleError::InvalidOracleConfig);
    assert_pricing_error(register(&mut env, pyth_oracle(&WSOL_FEED, 0, 1_000)), OracleError::InvalidOracleConfig);
    let gap = OracleConfig { scope_chain: [XSTOCK_ENTRY, SCOPE_CHAIN_UNUSED, SOL_ENTRY, SCOPE_CHAIN_UNUSED], ..scope_oracle(&scope, &[XSTOCK_ENTRY], &[], 300, 0) };
    assert_pricing_error(register(&mut env, gap), OracleError::InvalidOracleConfig);
    assert_pricing_error(register(&mut env, scope_oracle(&scope, &[600], &[], 300, 0)), OracleError::InvalidOracleConfig);
    assert_pricing_error(register(&mut env, scope_oracle(&scope, &[XSTOCK_ENTRY], &[], 300, 500)), OracleError::InvalidOracleConfig);
    let factor_only = OracleConfig { pyth_factor: env.sol_price, ..scope_oracle(&scope, &[SOL_ENTRY], &[], 300, 0) };
    assert_pricing_error(register(&mut env, factor_only), OracleError::InvalidOracleConfig);
    let klend_only = OracleConfig { klend_program: Pubkey::new_unique(), ..pyth_oracle(&WSOL_FEED, 300, 1_000) };
    assert_pricing_error(register(&mut env, klend_only), OracleError::InvalidOracleConfig);
    let copy = Pubkey::new_unique();
    let now = env.now();
    set_price(&mut env.svm, &copy, WSOL_FEED, WSOL_PRICE, 0, -8, now);
    let copied = OracleConfig { pyth_price: copy, ..pyth_oracle(&WSOL_FEED, 300, 1_000) };
    assert_pricing_error(register(&mut env, copied), OracleError::InvalidPriceFeed);
    let impostor = Pubkey::new_unique();
    let mut account = env.svm.get_account(&scope).unwrap();
    account.owner = anchor_lang::system_program::ID;
    env.svm.set_account(impostor, account).unwrap();
    assert_pricing_error(register(&mut env, scope_oracle(&impostor, &[SOL_ENTRY], &[], 300, 0)), OracleError::InvalidOracleOwner);
    assert_pricing_error(register(&mut env, scope_oracle(&scope, &[7], &[], 300, 0)), OracleError::PriceUnavailable);

    let config = OracleConfig { pyth_price: env.sol_price, ..scope_oracle(&scope, &[SOL_ENTRY], &[SOL_TWAP], 120, 1_000) };
    let meta = register(&mut env, config).expect("valid oracle");
    assert_eq!(price_source(&env.svm, &mint), config);
    assert_eq!(event::<PriceSourceSet>(&meta.logs).config, config);
    assert_eq!(fetch_asset_config(&env.svm, &mint).oracle, ORACLE);

    let rotated = pyth_oracle(&WSOL_FEED, 60, 200);
    let outsider = funded_keypair(&mut env.svm);
    let ix = ix_admin_set_asset_oracle(&outsider.pubkey(), &mint, rotated, &[]);
    assert!(send(&mut env.svm, &outsider, &[ix], &[]).is_err());
    let ix = ix_admin_set_asset_oracle(&a, &mint, rotated, &[]);
    send(&mut env.svm, &env.admin, &[ix], &[]).expect("rotate");
    assert_eq!(price_source(&env.svm, &mint), rotated);
}

fn fixture_accounts() -> Vec<(Pubkey, solana_account::Account)> {
    ORACLE_ACCOUNTS.iter().map(|(k, _)| (key(k), fixture_account(k))).collect()
}

fn as_f64(price: Price) -> f64 {
    price.value as f64 * 10f64.powi(price.exponent)
}

#[test]
fn mainnet_every_asset_prices_from_real_scope_and_pyth() {
    let accounts = fixture_accounts();
    let scope = fixture_account(SCOPE_PRICES);
    let mut prices = std::collections::HashMap::new();
    for asset in mainnet_assets() {
        let config = asset_config_for(&asset);
        let price = price_offchain(&config, &accounts, ORACLE_SNAPSHOT_UNIX_TIMESTAMP).unwrap();
        assert_eq!(price.checks, PriceChecks::ALL, "{} passes every check", asset.name);

        let raw = if asset.oracle.uses_scope() {
            let entries = asset.oracle.scope_chain.iter().take_while(|e| **e != u16::MAX);
            entries.map(|e| scope_entry(&scope, *e)).map(|(v, x, _)| v as f64 / 10f64.powi(x as i32)).product::<f64>()
        } else {
            let update = PriceUpdateV2::try_deserialize(&mut &fixture_account(PYTH_JUPUSD).data[..]).unwrap();
            update.price_message.price as f64 * 10f64.powi(update.price_message.exponent)
        };
        let got = as_f64(price.price);
        assert!((got - raw).abs() / raw < 1e-12, "{}: facade {got} vs raw {raw}", asset.name);
        println!("{:8} ${got:.6}  ({}s old)", asset.name, ORACLE_SNAPSHOT_UNIX_TIMESTAMP - price.timestamp);
        prices.insert(asset.name, got);
    }
    for stable in ["USDC", "USDT", "JupUSD"] {
        assert!((0.97..=1.001).contains(&prices[stable]), "{stable} at {}", prices[stable]);
    }
    assert!(prices["JitoSOL"] > prices["SOL"] && prices["JupSOL"] > prices["SOL"], "LSTs above SOL");
    assert!(prices["NVDAx"] > 10.0 && prices["TSLAx"] > 10.0);
}

#[test]
fn mainnet_stale_scope_falls_back_to_real_pyth() {
    let mut accounts = fixture_accounts();
    let scope = accounts.iter_mut().find(|(k, _)| *k == key(SCOPE_PRICES)).unwrap();
    for entry in [kamino_scope_sol(), kamino_scope_nvdax()] {
        let at = 40 + 56 * usize::from(entry) + 24;
        scope.1.data[at..at + 8].copy_from_slice(&((ORACLE_SNAPSHOT_UNIX_TIMESTAMP - 3_600) as u64).to_le_bytes());
    }
    let assets = mainnet_assets();
    let sol = asset_config_for(assets.iter().find(|a| a.name == "SOL").unwrap());
    let price = price_offchain(&sol, &accounts, ORACLE_SNAPSHOT_UNIX_TIMESTAMP).unwrap();
    let pyth = PriceUpdateV2::try_deserialize(&mut &fixture_account(PYTH_SOL).data[..]).unwrap().price_message;
    assert_eq!((price.price.value as i64, price.price.exponent, price.timestamp), (pyth.price, pyth.exponent, pyth.publish_time));
    assert!(price.checks.contains(PriceChecks::FRESH));

    let nvdax = asset_config_for(assets.iter().find(|a| a.name == "NVDAx").unwrap());
    let price = price_offchain(&nvdax, &accounts, ORACLE_SNAPSHOT_UNIX_TIMESTAMP).unwrap();
    assert!(!price.checks.contains(PriceChecks::FRESH));
    assert!(require_checks(price.checks, PriceChecks::LIQUIDATION).is_err(), "even liquidation needs it fresh");
}

fn kamino_scope_sol() -> u16 {
    vanna_oracle::reference::kamino_scope::SOL.0[0]
}

fn kamino_scope_nvdax() -> u16 {
    vanna_oracle::reference::kamino_scope::NVDAX.0[0]
}

#[test]
fn mainnet_assets_register_and_back_a_borrow() {
    let mut svm = setup_svm();
    load_oracle_fixtures(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 16)], &[]).unwrap();
    let assets = mainnet_assets();
    for asset in &assets {
        let pool = ["USDC", "USDT", "SOL"].contains(&asset.name);
        let ixs = ix_admin_register_asset_with(&a, &a, &asset.mint, &asset.token_program, asset.oracle, &[], 0, 8_000, 8_500, 500, true, pool);
        send(&mut svm, &admin, &ixs, &[]).unwrap_or_else(|e| panic!("register {}: {e:?}", asset.name));
    }
    let usdc = key(known_mints::USDC);
    let jitosol = key(known_mints::JITOSOL);
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &usdc, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &usdc, 10_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc, 10_000 * USDC, 1)], &[]).unwrap();

    let user = funded_keypair(&mut svm);
    let (margin, _) = margin_pda(&user.pubkey());
    let u = user.pubkey();
    set_token_balance(&mut svm, &u, &jitosol, 10 * LST);
    let setup = [ix_user_create_margin(&u, &u), ix_user_deposit_collateral(&u, &margin, &jitosol, 10 * LST)];
    send(&mut svm, &user, &setup, &[]).unwrap();
    let oracles = [key(SCOPE_PRICES), key(PYTH_USDC)];
    let ix = ix_user_borrow(&u, &margin, &usdc, 1_000 * USDC, u128::MAX, &with_oracles(collateral_group_metas(&jitosol, &margin), &oracles));
    let meta = send(&mut svm, &user, &[ix], &[]).expect("borrow against JitoSOL at real prices");

    let accounts = fixture_accounts();
    let price = |name: &str| {
        let asset = assets.iter().find(|a| a.name == name).unwrap();
        price_offchain(&asset_config_for(asset), &accounts, ORACLE_SNAPSHOT_UNIX_TIMESTAMP).unwrap()
    };
    let (jito, usdc_price) = (price("JitoSOL"), price("USDC"));
    let expected = health(
        &[jito.value_of(10 * LST, 9, false).unwrap(), usdc_price.value_of(1_000 * USDC, 6, false).unwrap()],
        &[usdc_price.value_of(1_000 * USDC, 6, true).unwrap()],
    );
    let borrowed: Borrowed = event(&meta.logs);
    assert_eq!(borrowed.borrow_health_factor_wad, expected);
    println!("10 JitoSOL = ${:.2}; HF after borrowing 1,000 USDC = {:.4}", jito.value_of(10 * LST, 9, false).unwrap() as f64 / 1e9, expected as f64 / 1e18);
}
