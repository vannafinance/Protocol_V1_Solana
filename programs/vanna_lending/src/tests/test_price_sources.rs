//! The non-Pyth price sources for margin collateral:
//! - `ScaledUiAmount` (xStocks such as NVDAx / TSLAx): Token-2022 mints whose raw amount is scaled
//!   by a UI multiplier; one UI token is one share, and the Pyth xStock feed prices one UI token.
//! - `RedemptionRate` (JupSOL): a Pyth rate against a base asset times the base asset's price.
//!
//! Also checks that Token-2022 collateral is liquidated against classic-SPL debt, with the multiplier.

mod common;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_lending::constants::WAD;
use vanna_lending::errors::VannaError;
use vanna_lending::events::{Borrowed, CollateralWithdrawn};
use vanna_lending::math::health::{calculate_health, normalize_token_value, CollateralValuation, DebtValuation};
use vanna_lending::math::interest::accrue;
use vanna_lending::math::shares::debt_shares_to_assets_up;
use vanna_lending::oracle::pyth::canonical_feed_account;

const USDC: u64 = 1_000_000;
const XSTOCK: u64 = 100_000_000; // 8 decimals, like NVDAx / TSLAx
const LST: u64 = 1_000_000_000; // 9 decimals, like JupSOL

const START: i64 = 1_800_000_000;
const NVDAX_FEED: [u8; 32] = [3u8; 32];
const JUPSOL_RATE_FEED: [u8; 32] = [4u8; 32];
/// $200.00 per UI token.
const NVDAX_PRICE: i64 = 20_000_000_000;
/// 1.2 SOL per JupSOL.
const JUPSOL_RATE: i64 = 120_000_000;
/// NVDAx's multiplier on mainnet before and after its last dividend.
const MULTIPLIER: f64 = 1.0009180758490996;
const NEXT_MULTIPLIER: f64 = 1.001701196801074;

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc: Pubkey,
    usdc_price: Pubkey,
    sol: Pubkey,
    /// SOL/USD at its canonical Pyth feed address, the base price of JupSOL.
    sol_price: Pubkey,
}

fn assert_vanna_error(res: TransactionResult, err: VannaError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + err as u32;
    let failure = format!("{:?}", res.expect_err("transaction should have failed").err);
    assert!(failure.contains(&format!("Custom({code})")), "expected error {code}, got {failure}");
}

fn now(svm: &LiteSVM) -> i64 {
    svm.get_sysvar::<Clock>().unix_timestamp
}

/// USDC pool with a lender, SOL registered as a Pyth asset, clock at `START`.
fn setup() -> Env {
    let mut svm = setup_svm();
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = START;
    svm.set_sysvar(&clock);

    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();

    let usdc = create_mint(&mut svm, &admin, &a, USDC_DECIMALS);
    let sol = create_mint(&mut svm, &admin, &a, WSOL_DECIMALS);
    for (mint, feed) in [(usdc, USDC_FEED), (sol, WSOL_FEED)] {
        send(&mut svm, &admin, &[ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[])
            .unwrap();
    }
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &usdc, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let usdc_price = Pubkey::new_unique();
    let sol_price = canonical_feed_account(&WSOL_FEED);
    set_price(&mut svm, &usdc_price, USDC_FEED, USDC_PRICE, 0, -8, START);
    set_price(&mut svm, &sol_price, WSOL_FEED, WSOL_PRICE, 0, -8, START);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc, &admin, &lender.pubkey(), 100_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc, 100_000 * USDC, 1)], &[]).unwrap();

    Env { svm, admin, usdc, usdc_price, sol, sol_price }
}

impl Env {
    /// Registers a collateral-only asset priced by `source`: disabled, source attached, enabled.
    fn register_priced(&mut self, mint: &Pubkey, source: PriceSource, source_account: Pubkey, base: Option<Pubkey>, program: Pubkey) {
        let a = self.admin.pubkey();
        let ix = ix_admin_set_asset_price_source(&a, mint, source, Some(source_account), base, program);
        send(&mut self.svm, &self.admin, &[ix], &[]).expect("set price source");
        let enable = ix_admin_update_asset_config(&a, mint, 0, 8_000, 8_500, 500, 1_000, 3_600, true, false);
        send(&mut self.svm, &self.admin, &[enable], &[]).expect("enable collateral");
    }

    /// NVDAx-like xStock: `MULTIPLIER` now, `NEXT_MULTIPLIER` from `START + 60`.
    fn nvdax(&mut self) -> (Pubkey, Keypair, Pubkey) {
        let authority = funded_keypair(&mut self.svm);
        let mint = create_scaled_ui_mint(&mut self.svm, &self.admin, &authority, 8, MULTIPLIER, Some((NEXT_MULTIPLIER, START + 60)));
        let a = self.admin.pubkey();
        send(&mut self.svm, &self.admin, &[ix_admin_register_asset_2022(&a, &mint, NVDAX_FEED, 3_600, false)], &[]).unwrap();
        self.register_priced(&mint, PriceSource::ScaledUiAmount, mint, None, TOKEN_2022);
        let (price, t) = (Pubkey::new_unique(), now(&self.svm));
        set_price(&mut self.svm, &price, NVDAX_FEED, NVDAX_PRICE, 0, -8, t);
        (mint, authority, price)
    }

    /// JupSOL-like LST priced as JUPSOL/SOL × SOL/USD.
    fn jupsol(&mut self) -> (Pubkey, Pubkey) {
        let a = self.admin.pubkey();
        let mint = create_mint(&mut self.svm, &self.admin, &a, 9);
        let register = ix_admin_register_asset(&a, &a, &mint, JUPSOL_RATE_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false);
        send(&mut self.svm, &self.admin, &[register], &[]).unwrap();
        let (sol_price, sol) = (self.sol_price, self.sol);
        self.register_priced(&mint, PriceSource::RedemptionRate, sol_price, Some(sol), pyth_solana_receiver_sdk::ID);
        let (rate, t) = (Pubkey::new_unique(), now(&self.svm));
        set_price(&mut self.svm, &rate, JUPSOL_RATE_FEED, JUPSOL_RATE, 0, -8, t);
        (mint, rate)
    }

    /// A user whose margin holds `amount` of `mint` (of `token_program`), with a USDC debt position.
    fn user_with(&mut self, mint: &Pubkey, token_program: &Pubkey, authority: &Keypair, amount: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        mint_to_wallet_with(&mut self.svm, &self.admin, mint, token_program, authority, &user.pubkey(), amount);
        let u = user.pubkey();
        let ixs = [
            ix_user_create_margin(&u, &u),
            ix_user_deposit_collateral_with(&u, &margin, mint, token_program, amount),
            ix_user_open_debt_position(&u, &u, &margin, &self.usdc),
        ];
        send(&mut self.svm, &user, &ixs, &[]).expect("deposit collateral");
        (user, margin)
    }

    fn borrow_usdc(&mut self, user: &Keypair, amount: u64, health: &[AccountMeta]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let ix = ix_user_borrow(&user.pubkey(), &margin, &self.usdc, &self.usdc_price, amount, u128::MAX, health);
        send(&mut self.svm, user, &[ix], &[])
    }

    fn refresh_prices(&mut self, feeds: &[(Pubkey, [u8; 32], i64)]) {
        let t = now(&self.svm);
        for (account, feed, price) in feeds {
            set_price(&mut self.svm, account, *feed, *price, 0, -8, t);
        }
    }
}

fn wad(multiplier: f64) -> u128 {
    (multiplier * WAD as f64) as u128
}

fn usdc_value(amount: u64, round_up: bool) -> u128 {
    normalize_token_value(amount, USDC_PRICE, -8, 6, round_up).unwrap()
}

/// What the program values `raw` xStock units at under `multiplier`.
fn xstock_value(raw: u64, multiplier: f64, price: i64) -> u128 {
    let ui = (raw as u128 * wad(multiplier) / WAD) as u64;
    normalize_token_value(ui, price, -8, 8, false).unwrap()
}

fn health(collateral: &[u128], debt: &[u128]) -> u128 {
    let c: Vec<_> = collateral.iter().map(|v| CollateralValuation { collateral_value: *v }).collect();
    let d: Vec<_> = debt.iter().map(|v| DebtValuation { debt_value: *v }).collect();
    calculate_health(&c, &d).unwrap().borrow_health_factor_wad
}

// ---------------------------------------------------------------------------
// Scaled UI Amount (xStocks)
// ---------------------------------------------------------------------------

/// 10 raw NVDAx tokens are worth 10 × multiplier shares, and the scheduled multiplier takes over
/// at its effective timestamp, exactly as Token-2022 applies it.
#[test]
fn xstock_is_valued_with_the_current_ui_multiplier() {
    let mut env = setup();
    let (nvdax, authority, nvdax_price) = env.nvdax();
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &margin, &nvdax_price, Some(&nvdax));

    // The mint must be in the xStock's health group.
    let without_mint = collateral_group_with(&nvdax, &TOKEN_2022, &margin, &nvdax_price, None);
    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &without_mint), VannaError::IncompletePositionAccounts);

    let meta = env.borrow_usdc(&user, 100 * USDC, &group).expect("borrow against NVDAx");
    let borrowed: Borrowed = event(&meta.logs);
    let expected = health(
        &[xstock_value(10 * XSTOCK, MULTIPLIER, NVDAX_PRICE), usdc_value(100 * USDC, false)],
        &[usdc_value(100 * USDC, true)],
    );
    assert_eq!(borrowed.borrow_health_factor_wad, expected);
    // 10 raw NVDAx = 10.00918075 UI tokens (floored to 8 decimals) × $200 = $2,001.83615.
    assert_eq!(xstock_value(10 * XSTOCK, MULTIPLIER, NVDAX_PRICE), 2_001_836_150_000);

    // Past the effective timestamp the new multiplier applies.
    advance_time(&mut env.svm, 60);
    let (usdc_price, np) = (env.usdc_price, nvdax_price);
    env.refresh_prices(&[(usdc_price, USDC_FEED, USDC_PRICE), (np, NVDAX_FEED, NVDAX_PRICE)]);
    let mut others = collateral_group_metas(&env.usdc, &margin, &env.usdc_price);
    others.extend(debt_group_metas(&env.usdc, &margin, &env.usdc_price));
    let ix = ix_user_withdraw_collateral_with(&user.pubkey(), &margin, &nvdax, &TOKEN_2022, &nvdax_price, None, XSTOCK, 0, &others);
    let meta = send(&mut env.svm, &user, &[ix], &[]).expect("withdraw 1 NVDAx (priced from the mint)");
    let withdrawn: CollateralWithdrawn = event(&meta.logs);

    let reserve = fetch_reserve(&env.svm, &env.usdc);
    let live = accrue(&reserve, now(&env.svm)).unwrap();
    let position = fetch_debt_position(&env.svm, &margin, &env.usdc);
    let debt = debt_shares_to_assets_up(position.borrow_shares, reserve.total_borrow_shares, live.new_total_borrow_assets).unwrap();
    let collateral = [xstock_value(9 * XSTOCK, NEXT_MULTIPLIER, NVDAX_PRICE), usdc_value(100 * USDC, false)];
    assert_eq!(withdrawn.borrow_health_factor_wad, health(&collateral, &[usdc_value(debt, true)]));
    let stale = [xstock_value(9 * XSTOCK, MULTIPLIER, NVDAX_PRICE), usdc_value(100 * USDC, false)];
    assert_ne!(withdrawn.borrow_health_factor_wad, health(&stale, &[usdc_value(debt, true)]), "new multiplier in effect");
    assert_eq!(token_balance(&env.svm, &ata_for(&user.pubkey(), &nvdax, &TOKEN_2022)), XSTOCK);
}

/// An underwater NVDAx position goes to the liquidator whole: every raw NVDAx (Token-2022) against
/// the full classic-SPL USDC debt. Whether it is liquidatable is decided with the UI multiplier.
#[test]
fn xstock_collateral_is_liquidated_against_usdc_debt() {
    let mut env = setup();
    let (nvdax, authority, nvdax_price) = env.nvdax();
    advance_time(&mut env.svm, 60); // NEXT_MULTIPLIER in effect
    let (usdc_price, np) = (env.usdc_price, nvdax_price);
    env.refresh_prices(&[(usdc_price, USDC_FEED, USDC_PRICE), (np, NVDAX_FEED, NVDAX_PRICE)]);
    let (user, margin) = env.user_with(&nvdax, &TOKEN_2022, &authority, 10 * XSTOCK);
    let group = collateral_group_with(&nvdax, &TOKEN_2022, &margin, &nvdax_price, Some(&nvdax));

    // Borrow $1,500 and take it out: $2,003 of NVDAx against $1,500 of debt (HF ≈ 1.34).
    env.borrow_usdc(&user, 1_500 * USDC, &group).unwrap();
    let mut others = group.clone();
    others.extend(debt_group_metas(&env.usdc, &margin, &env.usdc_price));
    let (admin, usdc) = (env.admin.insecure_clone(), env.usdc);
    mint_to_wallet(&mut env.svm, &admin, &usdc, &admin, &user.pubkey(), 0);
    let ix = ix_user_withdraw_collateral(&user.pubkey(), &margin, &env.usdc, &env.usdc_price, 1_500 * USDC, 0, &others);
    send(&mut env.svm, &user, &[ix], &[]).expect("withdraw the borrowed USDC");

    let liquidator = funded_keypair(&mut env.svm);
    let usdc_account = mint_to_wallet(&mut env.svm, &admin, &usdc, &admin, &liquidator.pubkey(), 2_000 * USDC);
    let nvdax_account = mint_to_wallet_with(&mut env.svm, &admin, &nvdax, &TOKEN_2022, &authority, &liquidator.pubkey(), 0);
    let positions = |price: &Pubkey| {
        vec![
            liq_collateral(&nvdax, &TOKEN_2022, &margin, price, Some(&nvdax), &nvdax_account),
            liq_debt(&usdc, &margin, &usdc_price, &usdc_account),
        ]
    };

    // $164.80: 10 × 1.0017 × $164.80 = $1,650.80 > 1.10 × $1,500, healthy only thanks to the
    // multiplier (without it, $1,648 would be liquidatable).
    env.refresh_prices(&[(np, NVDAX_FEED, 16_480_000_000)]);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions(&nvdax_price));
    assert_vanna_error(send(&mut env.svm, &liquidator, &[ix], &[]), VannaError::PositionHealthy);

    // $160: HF ≈ 1.07. The liquidator repays the whole debt and takes every NVDAx.
    env.refresh_prices(&[(np, NVDAX_FEED, 16_000_000_000)]);
    let ix = ix_public_liquidate(&liquidator.pubkey(), &margin, &positions(&nvdax_price));
    send(&mut env.svm, &liquidator, &[ix], &[]).expect("liquidate Token-2022 collateral against SPL debt");
    assert_eq!(token_balance(&env.svm, &nvdax_account), 10 * XSTOCK);
    assert_eq!(token_balance(&env.svm, &usdc_account), 500 * USDC);
    assert_eq!(token_balance(&env.svm, &margin_vault_ata_with(&margin, &nvdax, &TOKEN_2022)), 0);
    let account = fetch_margin(&env.svm, &user.pubkey());
    assert_eq!((account.collateral_count, account.debt_count), (0, 0));
}

// ---------------------------------------------------------------------------
// Redemption rate (JupSOL)
// ---------------------------------------------------------------------------

/// 10 JupSOL at 1.2 SOL each and $200 per SOL count as $2,400.
#[test]
fn lst_is_valued_as_rate_times_base_price() {
    let mut env = setup();
    let (jupsol, rate) = env.jupsol();
    let admin = env.admin.insecure_clone();
    let (user, margin) = env.user_with(&jupsol, &anchor_spl::token::ID, &admin, 10 * LST);

    // The base SOL/USD account is part of JupSOL's health group, and must be the registered one.
    let wrong_base = collateral_group_with(&jupsol, &anchor_spl::token::ID, &margin, &rate, Some(&env.usdc_price));
    assert_vanna_error(env.borrow_usdc(&user, 100 * USDC, &wrong_base), VannaError::InvalidPriceSource);

    let group = collateral_group_with(&jupsol, &anchor_spl::token::ID, &margin, &rate, Some(&env.sol_price));
    let meta = env.borrow_usdc(&user, 100 * USDC, &group).expect("borrow against JupSOL");
    let borrowed: Borrowed = event(&meta.logs);
    let jupsol_value = normalize_token_value(12 * LST, WSOL_PRICE, -8, 9, false).unwrap();
    assert_eq!(jupsol_value, 2_400_000_000_000, "$2,400");
    let expected = health(&[jupsol_value, usdc_value(100 * USDC, false)], &[usdc_value(100 * USDC, true)]);
    assert_eq!(borrowed.borrow_health_factor_wad, expected);

    // A stale base price fails like any stale price.
    advance_time(&mut env.svm, 3_601);
    let (usdc_price, r) = (env.usdc_price, rate);
    env.refresh_prices(&[(usdc_price, USDC_FEED, USDC_PRICE), (r, JUPSOL_RATE_FEED, JUPSOL_RATE)]);
    assert_vanna_error(env.borrow_usdc(&user, USDC, &group), VannaError::StalePrice);
}

// ---------------------------------------------------------------------------
// Admin rules
// ---------------------------------------------------------------------------

#[test]
fn price_source_admin_rules() {
    let mut env = setup();
    let a = env.admin.pubkey();
    let authority = funded_keypair(&mut env.svm);
    let xstock = create_scaled_ui_mint(&mut env.svm, &env.admin, &authority, 8, 1.0, None);
    send(&mut env.svm, &env.admin, &[ix_admin_register_asset_2022(&a, &xstock, NVDAX_FEED, 3_600, false)], &[]).unwrap();
    let set = |mint: &Pubkey, source: PriceSource, account: Pubkey, base: Option<Pubkey>, program: Pubkey| {
        ix_admin_set_asset_price_source(&a, mint, source, Some(account), base, program)
    };

    // Scaled UI Amount: the source is the asset's own Token-2022 mint, carrying the extension.
    let other_mint = create_scaled_ui_mint(&mut env.svm, &env.admin, &authority, 8, 1.0, None);
    let ix = set(&xstock, PriceSource::ScaledUiAmount, other_mint, None, TOKEN_2022);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let ix = set(&xstock, PriceSource::ScaledUiAmount, xstock, None, anchor_spl::token::ID);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let plain = create_mint(&mut env.svm, &env.admin, &a, 8);
    let register = ix_admin_register_asset(&a, &a, &plain, NVDAX_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false);
    send(&mut env.svm, &env.admin, &[register], &[]).unwrap();
    let ix = set(&plain, PriceSource::ScaledUiAmount, plain, None, TOKEN_2022);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let ix = set(&xstock, PriceSource::ScaledUiAmount, xstock, None, TOKEN_2022);
    send(&mut env.svm, &env.admin, &[ix], &[]).expect("scaled UI source on its own mint");

    // Non-Pyth assets are collateral-only and can't be switched while live.
    let borrowable = ix_admin_update_asset_config(&a, &xstock, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[borrowable], &[]), VannaError::UnsupportedPriceSource);
    let enable = ix_admin_update_asset_config(&a, &xstock, 0, 8_000, 8_500, 500, 1_000, 3_600, true, false);
    send(&mut env.svm, &env.admin, &[enable], &[]).unwrap();
    let ix = set(&xstock, PriceSource::Pyth, Pubkey::default(), None, Pubkey::default());
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::UnsupportedPriceSource);
    let ix = set(&env.usdc, PriceSource::ScaledUiAmount, env.usdc, None, TOKEN_2022);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::UnsupportedPriceSource);

    // Redemption rate: the source is the base asset's canonical Pyth feed account.
    let lst = create_mint(&mut env.svm, &env.admin, &a, 9);
    let register = ix_admin_register_asset(&a, &a, &lst, JUPSOL_RATE_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false);
    send(&mut env.svm, &env.admin, &[register], &[]).unwrap();
    let receiver = pyth_solana_receiver_sdk::ID;
    let ix = set(&lst, PriceSource::RedemptionRate, env.usdc_price, Some(env.sol), receiver);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let ix = set(&lst, PriceSource::RedemptionRate, env.sol_price, Some(env.usdc), receiver);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let ix = set(&lst, PriceSource::RedemptionRate, env.sol_price, Some(env.sol), TOKEN_2022);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[ix], &[]), VannaError::InvalidPriceSource);
    let ix = set(&lst, PriceSource::RedemptionRate, env.sol_price, Some(env.sol), receiver);
    send(&mut env.svm, &env.admin, &[ix], &[]).expect("redemption rate against SOL's canonical feed");
    let asset = fetch_asset_config(&env.svm, &lst);
    assert_eq!(asset.price_source, PriceSource::RedemptionRate);
    assert_eq!(asset.price_source_account, canonical_feed_account(&WSOL_FEED));
}
