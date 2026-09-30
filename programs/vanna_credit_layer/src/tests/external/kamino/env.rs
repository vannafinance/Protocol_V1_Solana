use crate::common::kamino::*;
use crate::common::mainnet::*;
use crate::common::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;

pub(crate) const USDC: u64 = 1_000_000;
pub(crate) const SOL: u64 = 1_000_000_000;

pub(crate) struct Env {
    pub(crate) svm: LiteSVM,
    pub(crate) admin: Keypair,
    pub(crate) usdc_price: Pubkey,
    pub(crate) sol_price: Pubkey,
}

pub(crate) fn receipt_oracle(r: &KaminoReserve, feed: [u8; 32], klend: Pubkey) -> OracleConfig {
    OracleConfig { klend_reserve: r.reserve, klend_program: klend, ..pyth_oracle(&feed, 3_600, 1_000) }
}

pub(crate) fn register_receipt(svm: &mut LiteSVM, admin: &Keypair, r: &KaminoReserve, feed: [u8; 32]) {
    let a = admin.pubkey();
    let underlying = asset_config_pda(&r.liquidity_mint).0;
    let ix = ix_admin_register_collateral(&a, &r.collateral_mint, &anchor_spl::token::ID, receipt_oracle(r, feed, KLEND), &[underlying]);
    send(svm, admin, &ix, &[]).expect("receipt priced through the real klend reserve");
}

pub(crate) fn setup() -> Env {
    let mut svm = setup_svm();
    load_mainnet(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();

    for (mint, feed) in [(MAINNET_USDC, USDC_FEED), (NATIVE_MINT, WSOL_FEED)] {
        send(&mut svm, &admin, &ix_admin_register_asset(&a, &a, &mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
        send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    }
    register_receipt(&mut svm, &admin, &USDC_RESERVE, USDC_FEED);
    register_receipt(&mut svm, &admin, &SOL_RESERVE, WSOL_FEED);
    send(&mut svm, &admin, &[ix_admin_register_integration(&a, &a, &KLEND, VALIDATOR)], &[]).unwrap();

    let usdc_price = set_pyth(&mut svm, USDC_FEED, USDC_PRICE, -8, FIXTURE_UNIX_TIMESTAMP);
    let sol_price = set_pyth(&mut svm, WSOL_FEED, WSOL_PRICE, -8, FIXTURE_UNIX_TIMESTAMP);

    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &MAINNET_USDC, 100_000 * USDC);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &MAINNET_USDC, 100_000 * USDC, 1)], &[]).unwrap();

    Env { svm, admin, usdc_price, sol_price }
}

impl Env {
    pub(crate) fn oracles(&self) -> [Pubkey; 4] {
        [self.usdc_price, self.sol_price, USDC_RESERVE.reserve, SOL_RESERVE.reserve]
    }

    pub(crate) fn health(&self, groups: &[AccountMeta]) -> Vec<AccountMeta> {
        with_oracles(groups.to_vec(), &self.oracles())
    }

    pub(crate) fn user_with_collateral(&mut self, mint: Pubkey, amount: u64) -> (Keypair, Pubkey) {
        let user = funded_keypair(&mut self.svm);
        let (margin, _) = margin_pda(&user.pubkey());
        set_token_balance(&mut self.svm, &user.pubkey(), &mint, amount);
        send(&mut self.svm, &user, &[ix_user_create_margin(&user.pubkey(), &user.pubkey())], &[]).unwrap();
        send(&mut self.svm, &user, &[ix_user_deposit_collateral(&user.pubkey(), &margin, &mint, amount)], &[]).unwrap();
        (user, margin)
    }

    pub(crate) fn mints(&self) -> [Pubkey; 4] {
        [MAINNET_USDC, NATIVE_MINT, USDC_RESERVE.collateral_mint, SOL_RESERVE.collateral_mint]
    }

    pub(crate) fn execute(&mut self, user: &Keypair, received: Pubkey, data: Vec<u8>, cpi: &[AccountMeta]) -> TransactionResult {
        let u = user.pubkey();
        let margin = margin_pda(&u).0;
        let mut groups = margin_groups(&self.svm, &u, &self.mints());
        let registered = self.svm.get_account(&asset_config_pda(&received).0).is_some();
        let new_assets = match registered && !self.is_active(user, &received) {
            true => {
                groups.extend(collateral_group_metas(&received, &margin));
                1
            }
            false => 0,
        };
        let ix = ix_margin_execute(&u, &KLEND, None, data, cpi, &self.health(&groups), new_assets);
        send(&mut self.svm, user, &[ix_create_vault(&u, &margin, &received, &anchor_spl::token::ID), ix], &[])
    }

    pub(crate) fn supply(&mut self, user: &Keypair, r: &KaminoReserve, amount: u64) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let cpi = kamino_supply_accounts(r, &margin, &margin_vault_ata(&margin, &r.liquidity_mint), &margin_vault_ata(&margin, &r.collateral_mint));
        let data = kamino_call_data(DEPOSIT_RESERVE_LIQUIDITY, amount);
        self.execute(user, r.collateral_mint, data, &cpi)
    }

    pub(crate) fn redeem(&mut self, user: &Keypair, r: &KaminoReserve, receipts: u64) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let cpi = kamino_redeem_accounts(r, &margin, &margin_vault_ata(&margin, &r.collateral_mint), &margin_vault_ata(&margin, &r.liquidity_mint));
        let data = kamino_call_data(REDEEM_RESERVE_COLLATERAL, receipts);
        self.execute(user, r.liquidity_mint, data, &cpi)
    }

    pub(crate) fn balance(&self, margin: &Pubkey, mint: &Pubkey) -> u64 {
        token_balance(&self.svm, &margin_vault_ata(margin, mint))
    }

    pub(crate) fn is_active(&self, user: &Keypair, mint: &Pubkey) -> bool {
        let index = fetch_asset_config(&self.svm, mint).asset_index;
        fetch_margin(&self.svm, &user.pubkey()).is_collateral_active(index)
    }

    pub(crate) fn receipt_group(&self, margin: &Pubkey, r: &KaminoReserve) -> Vec<AccountMeta> {
        collateral_group_metas(&r.collateral_mint, margin)
    }

    pub(crate) fn borrow_usdc(&mut self, user: &Keypair, amount: u64, groups: &[AccountMeta]) -> TransactionResult {
        let margin = margin_pda(&user.pubkey()).0;
        let ix = ix_user_borrow(&user.pubkey(), &margin, &MAINNET_USDC, amount, u128::MAX, &self.health(groups));
        send(&mut self.svm, user, &[ix], &[])
    }
}

pub(crate) fn usdc_value(amount: u64, round_up: bool) -> u128 {
    vanna_oracle::price::normalize_token_value(amount as u128, USDC_PRICE, -8, 6, round_up).unwrap()
}

pub(crate) fn health_wad(collateral_usd: &[u128], debt_usd: &[u128]) -> u128 {
    use vanna_credit_layer::math::health::{calculate_health, CollateralValuation, DebtValuation};
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
