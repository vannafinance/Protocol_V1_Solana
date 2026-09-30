use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use crate::common::jupiter::*;
use crate::common::kamino::{set_token_balance, MAINNET_USDC, NATIVE_MINT};
use crate::common::mainnet::*;
use crate::common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_validator::ValidatorError;
use vanna_credit_layer::errors::VannaError;

const USDC: u64 = 1_000_000;
const SOL: u64 = 1_000_000_000;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Route,
    Shared,
}

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc_price: Pubkey,
    sol_price: Pubkey,
}

fn data(kind: Kind, args: RouteArgs) -> Vec<u8> {
    match kind {
        Kind::Route => route_data(args),
        Kind::Shared => shared_route_data(args),
    }
}

fn accounts(kind: Kind, authority: &Pubkey, source: &Pubkey, destination: &Pubkey) -> Vec<AccountMeta> {
    match kind {
        Kind::Route => route_accounts(authority, source, destination),
        Kind::Shared => shared_route_accounts(authority, source, destination),
    }
}

fn direct_swap_output(kind: Kind, sol: u64) -> u64 {
    let mut svm = setup_svm();
    load_mainnet(&mut svm);
    let wallet = funded_keypair(&mut svm);
    let source = set_token_balance(&mut svm, &wallet.pubkey(), &NATIVE_MINT, sol);
    let destination = set_token_balance(&mut svm, &wallet.pubkey(), &MAINNET_USDC, 0);
    let ix = jupiter_direct_ix(data(kind, RouteArgs::exact_in(sol)), accounts(kind, &wallet.pubkey(), &source, &destination), &wallet.pubkey());
    send(&mut svm, &wallet, &[ix], &[]).expect("direct Jupiter swap");
    token_balance(&svm, &destination)
}

fn setup() -> Env {
    let mut svm = setup_svm();
    load_mainnet(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();
    for (mint, feed) in [(MAINNET_USDC, USDC_FEED), (NATIVE_MINT, WSOL_FEED)] {
        send(&mut svm, &admin, &ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
        send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    }
    send(&mut svm, &admin, &[ix_admin_register_integration(&a, &a, &JUPITER, VALIDATOR)], &[]).unwrap();

    let usdc_price = set_pyth(&mut svm, USDC_FEED, USDC_PRICE, -8, FIXTURE_UNIX_TIMESTAMP);
    let sol_price = set_pyth(&mut svm, WSOL_FEED, WSOL_PRICE, -8, FIXTURE_UNIX_TIMESTAMP);

    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &MAINNET_USDC, 100_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &MAINNET_USDC, 100_000 * USDC, 1)], &[]).unwrap();
    Env { svm, admin, usdc_price, sol_price }
}

impl Env {
    fn user_with_collateral(&mut self, mint: Pubkey, amount: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        set_token_balance(&mut self.svm, &user.pubkey(), &mint, amount);
        send(&mut self.svm, &user, &[ix_user_create_margin(&user.pubkey(), &user.pubkey())], &[]).unwrap();
        send(&mut self.svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &mint, amount)], &[]).unwrap();
        (user, margin)
    }

    fn execute(&mut self, user: &Keypair, data: Vec<u8>, cpi: &[AccountMeta]) -> TransactionResult {
        let u = user.pubkey();
        let margin = margin_pda(&u).0;
        let mut groups = margin_groups(&self.svm, &u, &[MAINNET_USDC, NATIVE_MINT]);
        let new_assets = match self.is_active(user, &MAINNET_USDC) {
            true => 0,
            false => {
                groups.extend(collateral_group_metas(&MAINNET_USDC, &margin));
                1
            }
        };
        let rest = with_oracles(groups, &[self.sol_price, self.usdc_price]);
        let ix = ix_margin_execute(&u, &JUPITER, None, data, cpi, &rest, new_assets);
        send(&mut self.svm, user, &[ix_create_vault(&u, &margin, &MAINNET_USDC, &anchor_spl::token::ID), ix], &[])
    }

    fn margin_accounts(&self, kind: Kind, margin: &Pubkey) -> Vec<AccountMeta> {
        accounts(kind, margin, &margin_vault_ata(margin, &NATIVE_MINT), &margin_vault_ata(margin, &MAINNET_USDC))
    }

    fn swap(&mut self, user: &Keypair, kind: Kind, sol: u64) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let cpi = self.margin_accounts(kind, &margin);
        self.execute(user, data(kind, RouteArgs::exact_in(sol)), &cpi)
    }

    fn balance(&self, margin: &Pubkey, mint: &Pubkey) -> u64 {
        self.svm.get_account(&margin_vault_ata(margin, mint)).map_or(0, |_| token_balance(&self.svm, &margin_vault_ata(margin, mint)))
    }

    fn is_active(&self, user: &Keypair, mint: &Pubkey) -> bool {
        let index = fetch_asset_config(&self.svm, mint).asset_index;
        fetch_margin(&self.svm, &user.pubkey()).is_collateral_active(index)
    }
}

#[test]
fn jupiter_fixture_swaps_directly_from_a_wallet() {
    for kind in [Kind::Route, Kind::Shared] {
        let out = direct_swap_output(kind, SOL);
        assert!(out > 50 * USDC && out < 500 * USDC, "1 SOL -> {out} USDC base units");
    }
}

#[test]
fn margin_swap_matches_a_direct_swap_for_both_route_kinds() {
    for kind in [Kind::Route, Kind::Shared] {
        let expected = direct_swap_output(kind, SOL);
        let mut env = setup();
        let (user, margin) = env.user_with_collateral(NATIVE_MINT, SOL);

        let meta = env.swap(&user, kind, SOL).expect("swap via margin_execute");
        assert!(meta.compute_units_consumed < 200_000, "fits the default compute budget: {}", meta.compute_units_consumed);

        assert_eq!(env.balance(&margin, &MAINNET_USDC), expected);
        assert_eq!(env.balance(&margin, &NATIVE_MINT), 0);
        assert!(!env.is_active(&user, &NATIVE_MINT), "an emptied input vault leaves the active list");
        assert!(env.is_active(&user, &MAINNET_USDC));
    }
}

#[test]
fn partial_swap_keeps_both_positions() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 3 * SOL);
    env.swap(&user, Kind::Route, SOL).unwrap();
    assert_eq!(env.balance(&margin, &NATIVE_MINT), 2 * SOL, "exactly in_amount is spent");
    assert!(env.is_active(&user, &NATIVE_MINT));
    assert!(env.is_active(&user, &MAINNET_USDC));
}

#[test]
fn swaps_are_health_checked_on_post_trade_balances() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    let sol_group = with_oracles(collateral_group_metas(&NATIVE_MINT, &margin), &[env.sol_price, env.usdc_price]);
    let borrow = ix_user_borrow(&user.pubkey(), &margin, &MAINNET_USDC, 13_000 * USDC, u128::MAX, &sol_group);
    send(&mut env.svm, &user, &[borrow], &[]).expect("borrow 13,000 USDC");

    assert_vanna_error(env.swap(&user, Kind::Route, 10 * SOL), VannaError::HealthFactorTooLow);
    assert_eq!(env.balance(&margin, &NATIVE_MINT), 10 * SOL, "the refused swap rolled back");

    env.swap(&user, Kind::Route, 2 * SOL).expect("a smaller swap stays healthy");
    let mut groups = margin_groups(&env.svm, &user.pubkey(), &[MAINNET_USDC, NATIVE_MINT]);
    groups.truncate(groups.len() - 3);
    let rest = with_oracles(groups, &[env.sol_price, env.usdc_price]);
    let cpi = env.margin_accounts(Kind::Route, &margin);
    let ix = ix_margin_execute(&user.pubkey(), &JUPITER, None, data(Kind::Route, RouteArgs::exact_in(SOL)), &cpi, &rest, 0);
    assert_vanna_error(send(&mut env.svm, &user, &[ix], &[]), VannaError::IncompletePositionAccounts);
}

#[test]
fn swaps_need_fresh_prices_only_while_in_debt() {
    let mut env = setup();
    let stale = FIXTURE_UNIX_TIMESTAMP - 3_601;
    let (saver, _) = env.user_with_collateral(NATIVE_MINT, 2 * SOL);
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, 10 * SOL);
    let sol_group = with_oracles(collateral_group_metas(&NATIVE_MINT, &margin), &[env.sol_price, env.usdc_price]);
    let borrow = ix_user_borrow(&user.pubkey(), &margin, &MAINNET_USDC, 1_000 * USDC, u128::MAX, &sol_group);
    send(&mut env.svm, &user, &[borrow], &[]).expect("borrow 1,000 USDC");

    let sol_price = env.sol_price;
    set_price(&mut env.svm, &sol_price, WSOL_FEED, WSOL_PRICE, 0, -8, stale);
    env.swap(&saver, Kind::Route, SOL).expect("a debt-free swap goes through on a stale price");
    assert_vanna_error(env.swap(&user, Kind::Route, SOL), VannaError::StalePrice);
}

#[test]
fn jupiters_slippage_bound_is_enforced() {
    let expected = direct_swap_output(Kind::Route, SOL);
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, SOL);

    let cpi = env.margin_accounts(Kind::Route, &margin);
    let greedy = RouteArgs { quoted_out_amount: expected * 2, ..RouteArgs::exact_in(SOL) };
    let failure = env.execute(&user, route_data(greedy), &cpi).expect_err("Jupiter refuses its own slippage");
    let jupiter_failed = format!("Program {JUPITER} failed: custom program error: 0x1771");
    assert!(failure.meta.logs.iter().any(|l| *l == jupiter_failed), "{:?}", failure.meta.logs);
    assert_eq!(env.balance(&margin, &NATIVE_MINT), SOL);
}

#[test]
fn value_cannot_be_diverted_out_of_the_margin() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, SOL);
    let thief = Pubkey::new_unique();
    let thief_usdc = set_token_balance(&mut env.svm, &thief, &MAINNET_USDC, 0);

    for kind in [Kind::Route, Kind::Shared] {
        let with_fee = RouteArgs { platform_fee_bps: 50, ..RouteArgs::exact_in(SOL) };
        let cpi = env.margin_accounts(kind, &margin);
        assert_custom_error(env.execute(&user, data(kind, with_fee), &cpi), ValidatorError::CallNotAllowed.into());

        let fee_slot = if kind == Kind::Route { 6 } else { 9 };
        let mut fee_account = env.margin_accounts(kind, &margin);
        fee_account[fee_slot] = AccountMeta::new(thief_usdc, false);
        assert_custom_error(env.execute(&user, data(kind, RouteArgs::exact_in(SOL)), &fee_account), ValidatorError::InvalidCallAccounts.into());
    }

    let mut redirected = env.margin_accounts(Kind::Route, &margin);
    redirected[4] = AccountMeta::new(thief_usdc, false);
    assert_custom_error(env.execute(&user, route_data(RouteArgs::exact_in(SOL)), &redirected), ValidatorError::InvalidCallAccounts.into());

    let mut shared_elsewhere = env.margin_accounts(Kind::Shared, &margin);
    shared_elsewhere[6] = AccountMeta::new(thief_usdc, false);
    assert_vanna_error(env.execute(&user, shared_route_data(RouteArgs::exact_in(SOL)), &shared_elsewhere), VannaError::InvalidCallAccounts);
    assert_eq!(env.balance(&margin, &NATIVE_MINT), SOL);
}

#[test]
fn other_jupiter_instructions_are_refused() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, SOL);
    let cpi = env.margin_accounts(Kind::Route, &margin);
    for selector in [
        [208, 51, 239, 151, 123, 43, 237, 92],
        [176, 209, 105, 168, 154, 125, 69, 62],
        [150, 86, 71, 116, 167, 93, 14, 104],
        [187, 100, 250, 204, 49, 196, 175, 20],
        [228, 85, 185, 112, 78, 79, 77, 2],
        [123, 229, 184, 63, 12, 0, 92, 145],
    ] {
        let mut call = route_data(RouteArgs::exact_in(SOL));
        call[..8].copy_from_slice(&selector);
        assert_custom_error(env.execute(&user, call, &cpi), ValidatorError::CallNotAllowed.into());
    }
    assert_custom_error(env.execute(&user, route_data(RouteArgs::exact_in(0)), &cpi), ValidatorError::ZeroAmount.into());
}

#[test]
fn route_accounts_are_pinned_to_the_margin() {
    let mut env = setup();
    let (user, margin) = env.user_with_collateral(NATIVE_MINT, SOL);
    let (_victim, victim_margin) = env.user_with_collateral(NATIVE_MINT, 5 * SOL);
    let call = || route_data(RouteArgs::exact_in(SOL));

    let mut cases = Vec::new();
    let mut victim_source = env.margin_accounts(Kind::Route, &margin);
    victim_source[2] = AccountMeta::new(margin_vault_ata(&victim_margin, &NATIVE_MINT), false);
    cases.push(victim_source);
    let mut victim_signer = env.margin_accounts(Kind::Route, &margin);
    victim_signer[1] = AccountMeta::new_readonly(victim_margin, false);
    cases.push(victim_signer);
    for smuggled in [user.pubkey(), reserve_pda(&MAINNET_USDC).0, vanna_credit_layer::ID] {
        let mut cpi = env.margin_accounts(Kind::Route, &margin);
        cpi[20] = AccountMeta::new(smuggled, false);
        cases.push(cpi);
    }
    for cpi in cases {
        assert_vanna_error(env.execute(&user, call(), &cpi), VannaError::InvalidCallAccounts);
    }
    let mut wrong_mint = env.margin_accounts(Kind::Route, &margin);
    wrong_mint[5] = AccountMeta::new_readonly(NATIVE_MINT, false);
    assert!(env.execute(&user, call(), &wrong_mint).is_err(), "Jupiter refuses a destination of another mint");
    assert_eq!(env.balance(&margin, &NATIVE_MINT), SOL);
    assert_eq!(env.balance(&victim_margin, &NATIVE_MINT), 5 * SOL);
}

#[test]
fn registry_assets_and_operating_mode_gate_swaps() {
    let mut env = setup();
    let (user, _) = env.user_with_collateral(NATIVE_MINT, SOL);
    let a = env.admin.pubkey();

    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &JUPITER, false)], &[]).unwrap();
    assert_vanna_error(env.swap(&user, Kind::Route, SOL), VannaError::IntegrationDisabled);
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &JUPITER, true)], &[]).unwrap();

    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 3)], &[]).unwrap();
    assert_vanna_error(env.swap(&user, Kind::Route, SOL), VannaError::ProtocolActionPaused);
    send(&mut env.svm, &env.admin, &[ix_admin_set_operating_mode(&a, 0)], &[]).unwrap();

    send(&mut env.svm, &env.admin, &[ix_admin_update_asset_config(&a, &MAINNET_USDC, 0, 8_000, 8_500, 500, false, true)], &[]).unwrap();
    assert_vanna_error(env.swap(&user, Kind::Route, SOL), VannaError::AssetNotCollateralEnabled);
    send(&mut env.svm, &env.admin, &[ix_admin_update_asset_config(&a, &MAINNET_USDC, 0, 8_000, 8_500, 500, true, true)], &[]).unwrap();

    let (usdc_only, _) = env.user_with_collateral(MAINNET_USDC, 100 * USDC);
    let margin = margin_pda(&usdc_only.pubkey()).0;
    set_token_balance(&mut env.svm, &margin, &NATIVE_MINT, SOL);
    assert_vanna_error(env.swap(&usdc_only, Kind::Route, SOL), VannaError::IncompletePositionAccounts);
}
