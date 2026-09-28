//! Shared setup for the end-to-end Kamino tests: USDC and SOL lending pools, klend's cUSDC and
//! cSOL registered as collateral, klend whitelisted, and a lender supplying 100,000 USDC.

use crate::common::kamino::*;
use crate::common::mainnet::*;
use crate::common::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_lending::errors::VannaError;

pub(crate) const USDC: u64 = 1_000_000;
pub(crate) const SOL: u64 = 1_000_000_000;

pub(crate) struct Env {
    pub(crate) svm: LiteSVM,
    pub(crate) admin: Keypair,
    pub(crate) usdc_price: Pubkey,
    pub(crate) sol_price: Pubkey,
}

pub(crate) fn assert_vanna_error(res: TransactionResult, err: VannaError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + err as u32;
    let failure = format!("{:?}", res.expect_err("transaction should have failed").err);
    assert!(failure.contains(&format!("Custom({code})")), "expected error {code}, got {failure}");
}

/// Registers a klend cToken as collateral: disabled, then its reserve as price source, then enabled.
pub(crate) fn register_receipt(svm: &mut LiteSVM, admin: &Keypair, r: &KaminoReserve, feed: [u8; 32]) {
    let a = admin.pubkey();
    let mint = r.collateral_mint;
    send(svm, admin, &[ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false)], &[]).unwrap();
    let set = ix_admin_set_asset_price_source(&a, &mint, PriceSource::KaminoReceipt, Some(r.reserve), Some(r.liquidity_mint), KLEND);
    send(svm, admin, &[set], &[]).expect("receipt price source on the real klend reserve");
    send(svm, admin, &[ix_admin_update_asset_config(&a, &mint, 0, 8_000, 8_500, 500, 1_000, 3_600, true, false)], &[]).unwrap();
}

/// Protocol with USDC and SOL pools, cUSDC / cSOL as collateral, klend whitelisted, and a lender
/// supplying 100,000 USDC.
pub(crate) fn setup() -> Env {
    let mut svm = setup_svm();
    load_mainnet(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();

    for (mint, feed) in [(MAINNET_USDC, USDC_FEED), (NATIVE_MINT, WSOL_FEED)] {
        send(&mut svm, &admin, &[ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[]).unwrap();
        send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    }
    register_receipt(&mut svm, &admin, &USDC_RESERVE, USDC_FEED);
    register_receipt(&mut svm, &admin, &SOL_RESERVE, WSOL_FEED);
    send(&mut svm, &admin, &[ix_admin_register_integration(&a, &a, &KLEND, AdapterKind::KaminoLend)], &[]).unwrap();

    let usdc_price = Pubkey::new_unique();
    let sol_price = Pubkey::new_unique();
    set_price(&mut svm, &usdc_price, USDC_FEED, USDC_PRICE, 0, -8, FIXTURE_UNIX_TIMESTAMP);
    set_price(&mut svm, &sol_price, WSOL_FEED, WSOL_PRICE, 0, -8, FIXTURE_UNIX_TIMESTAMP);

    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &MAINNET_USDC, 100_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &MAINNET_USDC, 100_000 * USDC, 1)], &[]).unwrap();

    Env { svm, admin, usdc_price, sol_price }
}

impl Env {
    pub(crate) fn price_of(&self, r: &KaminoReserve) -> Pubkey {
        if r.liquidity_mint == MAINNET_USDC { self.usdc_price } else { self.sol_price }
    }

    /// A user whose margin account holds `amount` of `mint` as collateral.
    pub(crate) fn user_with_collateral(&mut self, mint: Pubkey, amount: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        set_token_balance(&mut self.svm, &user.pubkey(), &mint, amount);
        send(&mut self.svm, &user, &[ix_user_create_margin(&user.pubkey(), &user.pubkey())], &[]).unwrap();
        send(&mut self.svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &mint, amount)], &[]).unwrap();
        (user, margin)
    }

    pub(crate) fn execute(&mut self, user: &Keypair, spent: Pubkey, received: Pubkey, price: Pubkey, data: Vec<u8>, cpi: &[AccountMeta], health: &[AccountMeta], min_received: u64) -> TransactionResult {
        let ix = ix_margin_execute(&user.pubkey(), &KLEND, &spent, &price, &received, &price, data, cpi, health, min_received);
        send(&mut self.svm, user, &[ix], &[])
    }

    /// Supplies `amount` of the reserve's liquidity from the margin into klend.
    pub(crate) fn supply(&mut self, user: &Keypair, r: &KaminoReserve, amount: u64, min_received: u64, health: &[AccountMeta]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let cpi = kamino_supply_accounts(r, &margin, &margin_vault_ata(&margin, &r.liquidity_mint), &margin_vault_ata(&margin, &r.collateral_mint));
        let data = kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, amount);
        self.execute(user, r.liquidity_mint, r.collateral_mint, self.price_of(r), data, &cpi, health, min_received)
    }

    /// Redeems `receipts` cTokens from klend back into the margin.
    pub(crate) fn redeem(&mut self, user: &Keypair, r: &KaminoReserve, receipts: u64, min_received: u64, health: &[AccountMeta]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let cpi = kamino_redeem_accounts(r, &margin, &margin_vault_ata(&margin, &r.collateral_mint), &margin_vault_ata(&margin, &r.liquidity_mint));
        let data = kamino_call_data(REDEEM_RESERVE_COLLATERAL, receipts);
        self.execute(user, r.collateral_mint, r.liquidity_mint, self.price_of(r), data, &cpi, health, min_received)
    }

    pub(crate) fn balance(&self, margin: &Pubkey, mint: &Pubkey) -> u64 {
        token_balance(&self.svm, &margin_vault_ata(margin, mint))
    }

    pub(crate) fn is_active(&self, user: &Keypair, mint: &Pubkey) -> bool {
        let index = fetch_asset_config(&self.svm, mint).asset_index;
        fetch_margin(&self.svm, &user.pubkey()).is_collateral_active(index)
    }

    pub(crate) fn receipt_group(&self, margin: &Pubkey, r: &KaminoReserve) -> Vec<AccountMeta> {
        collateral_group_with_source(&r.collateral_mint, margin, &self.price_of(r), &r.reserve)
    }

    pub(crate) fn borrow_usdc(&mut self, user: &Keypair, amount: u64, health: &[AccountMeta]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let ix = ix_user_borrow(&user.pubkey(), &margin, &MAINNET_USDC, &self.usdc_price, amount, u128::MAX, health);
        send(&mut self.svm, user, &[ix], &[])
    }

    pub(crate) fn open_usdc_debt(&mut self, user: &Keypair) {
        let margin = margin_pda(&user.pubkey()).0;
        send(&mut self.svm, user, &[ix_user_open_debt_position(&user.pubkey(), &user.pubkey(), &margin, &MAINNET_USDC)], &[]).unwrap();
    }
}

/// USDC base units -> nano-USD at the $1.00 oracle price, as the program values it.
pub(crate) fn usdc_value(amount: u64, round_up: bool) -> u128 {
    vanna_lending::math::health::normalize_token_value(amount, USDC_PRICE, -8, 6, round_up).unwrap()
}

/// Health factor (WAD) the program computes for these collateral and debt values.
pub(crate) fn health_wad(collateral_usd: &[u128], debt_usd: &[u128]) -> u128 {
    use vanna_lending::math::health::{calculate_health, CollateralValuation, DebtValuation};
    let collaterals: Vec<_> = collateral_usd.iter().map(|v| CollateralValuation { collateral_value: *v }).collect();
    let debts: Vec<_> = debt_usd.iter().map(|v| DebtValuation { debt_value: *v }).collect();
    calculate_health(&collaterals, &debts).unwrap().borrow_health_factor_wad
}

pub(crate) fn usd(amount: u64) -> String {
    format!("{}.{:06}", amount / USDC, amount % USDC)
}

pub(crate) fn hf(wad: u128) -> String {
    format!("{:.6}", wad as f64 / 1e18)
}
