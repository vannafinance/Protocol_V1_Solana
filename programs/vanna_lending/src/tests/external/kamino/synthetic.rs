//! Kamino receipt pricing against a synthetic klend reserve (admin rules, exchange rate,
//! decimals), and `margin_execute`'s registry and account guards against a stand-in program.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use crate::common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_account::Account as SvmAccount;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_lending::adapters::kamino::DEPOSIT_RESERVE_LIQUIDITY;
use vanna_lending::errors::VannaError;

/// Stand-in for Kamino's klend program id: owner of the fake reserves below.
fn fake_klend() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

/// Every klend cToken mint has 6 decimals, whatever the underlying's.
const CTOKEN_DECIMALS: u8 = 6;

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc: Pubkey,
    wsol: Pubkey,
    /// cToken of USDC; its reserve holds 2 USDC per cToken.
    kusdc: Pubkey,
    kusdc_reserve: Pubkey,
    /// cToken of SOL (9 decimals behind a 6-decimal cToken); 1 SOL per cToken.
    ksol: Pubkey,
    ksol_reserve: Pubkey,
    usdc_price: Pubkey,
    wsol_price: Pubkey,
}

impl Env {
    /// Every oracle account an asset here reads.
    fn oracles(&self) -> [Pubkey; 4] {
        [self.usdc_price, self.wsol_price, self.kusdc_reserve, self.ksol_reserve]
    }
}

struct FakeReserve {
    liquidity_mint: Pubkey,
    liquidity_decimals: u64,
    collateral_mint: Pubkey,
    total_liquidity: u64,
    collateral_supply: u64,
}

/// A klend `Reserve` (layout version 1, 8,624 bytes) with only the fields Vanna reads.
fn kamino_reserve_data(r: &FakeReserve) -> Vec<u8> {
    let mut data = vec![0u8; 8624];
    data[..8].copy_from_slice(&[43, 242, 204, 202, 26, 247, 59, 127]);
    data[8..16].copy_from_slice(&1u64.to_le_bytes());
    data[128..160].copy_from_slice(r.liquidity_mint.as_ref());
    data[224..232].copy_from_slice(&r.total_liquidity.to_le_bytes());
    data[272..280].copy_from_slice(&r.liquidity_decimals.to_le_bytes());
    data[2560..2592].copy_from_slice(r.collateral_mint.as_ref());
    data[2592..2600].copy_from_slice(&r.collateral_supply.to_le_bytes());
    data
}

fn set_account_data(svm: &mut LiteSVM, key: &Pubkey, data: Vec<u8>) {
    let account = SvmAccount { lamports: 1_000_000_000, data, owner: fake_klend(), executable: false, rent_epoch: 0 };
    svm.set_account(*key, account).unwrap();
}

fn register_disabled_receipt(svm: &mut LiteSVM, admin: &Keypair, mint: &Pubkey, feed: [u8; 32]) {
    let a = admin.pubkey();
    send(svm, admin, &[ix_admin_register_asset(&a, &a, mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false)], &[]).unwrap();
}

/// A receipt's oracle: the underlying's Pyth feed through the (fake) klend reserve's rate.
fn receipt_oracle(reserve: &Pubkey, feed: [u8; 32]) -> OracleConfig {
    OracleConfig { klend_reserve: *reserve, klend_program: fake_klend(), ..pyth_oracle(&feed, 3_600, 1_000) }
}

/// Sets a receipt's oracle; `underlying` is the asset whose `AssetConfig` is passed for the check.
fn set_receipt_oracle(a: &Pubkey, mint: &Pubkey, reserve: &Pubkey, feed: [u8; 32], underlying: Option<Pubkey>) -> anchor_lang::solana_program::instruction::Instruction {
    let extra: Vec<Pubkey> = underlying.iter().map(|m| asset_config_pda(m).0).collect();
    ix_admin_set_asset_oracle(a, mint, receipt_oracle(reserve, feed), &extra)
}

fn register_receipt(svm: &mut LiteSVM, admin: &Keypair, mint: &Pubkey, underlying: &Pubkey, feed: [u8; 32], reserve: &Pubkey) {
    let a = admin.pubkey();
    let ix = ix_admin_register_collateral(&a, mint, &anchor_spl::token::ID, receipt_oracle(reserve, feed), &[asset_config_pda(underlying).0]);
    send(svm, admin, &[ix], &[]).expect("register receipt");
}

fn setup() -> Env {
    let mut svm = setup_svm();
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);
    let kusdc = create_mint(&mut svm, &admin, &admin.pubkey(), CTOKEN_DECIMALS);
    let ksol = create_mint(&mut svm, &admin, &admin.pubkey(), CTOKEN_DECIMALS);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_admin_register_asset(&a, &a, &usdc, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_register_asset(&a, &a, &wsol, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &usdc, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &wsol, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    let kusdc_reserve = Pubkey::new_unique();
    let usdc_rate = FakeReserve {
        liquidity_mint: usdc,
        liquidity_decimals: USDC_DECIMALS as u64,
        collateral_mint: kusdc,
        total_liquidity: 2_000_000,
        collateral_supply: 1_000_000,
    };
    set_account_data(&mut svm, &kusdc_reserve, kamino_reserve_data(&usdc_rate));
    register_receipt(&mut svm, &admin, &kusdc, &usdc, USDC_FEED, &kusdc_reserve);

    let ksol_reserve = Pubkey::new_unique();
    let sol_rate = FakeReserve {
        liquidity_mint: wsol,
        liquidity_decimals: WSOL_DECIMALS as u64,
        collateral_mint: ksol,
        total_liquidity: 1_000 * 10u64.pow(9),
        collateral_supply: 1_000 * 10u64.pow(6),
    };
    set_account_data(&mut svm, &ksol_reserve, kamino_reserve_data(&sol_rate));
    register_receipt(&mut svm, &admin, &ksol, &wsol, WSOL_FEED, &ksol_reserve);

    let usdc_price = pyth_account(&USDC_FEED);
    let wsol_price = pyth_account(&WSOL_FEED);
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_price(&mut svm, &usdc_price, USDC_FEED, USDC_PRICE, 0, -8, now);
    set_price(&mut svm, &wsol_price, WSOL_FEED, WSOL_PRICE, 0, -8, now);

    let lender = funded_keypair(&mut svm);
    mint_to_wallet(&mut svm, &admin, &usdc, &admin, &lender.pubkey(), 100_000 * 10u64.pow(6));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc, 100_000 * 10u64.pow(6), 1)], &[]).unwrap();

    Env { svm, admin, usdc, wsol, kusdc, kusdc_reserve, ksol, ksol_reserve, usdc_price, wsol_price }
}

/// A borrower with a margin account holding `amount` of `mint` as collateral.
fn borrower_with_collateral(env: &mut Env, mint: Pubkey, amount: u64) -> (Keypair, Pubkey) {
    let borrower = funded_keypair(&mut env.svm);
    let (margin, _) = margin_pda(&borrower.pubkey());
    mint_to_wallet(&mut env.svm, &env.admin, &mint, &env.admin, &borrower.pubkey(), amount);
    send(&mut env.svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut env.svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &mint, amount)], &[]).unwrap();
    (borrower, margin)
}

/// Borrows USDC; `groups` are the other positions', every oracle account is added.
fn borrow_usdc(env: &mut Env, borrower: &Keypair, margin: &Pubkey, usdc: u64, groups: &[AccountMeta]) -> TransactionResult {
    let remaining = with_oracles(groups.to_vec(), &env.oracles());
    let ix = ix_user_borrow(&borrower.pubkey(), margin, &env.usdc, usdc * 10u64.pow(6), u128::MAX, &remaining);
    send(&mut env.svm, borrower, &[ix], &[])
}

// ---------------------------------------------------------------------------
// Price sources
// ---------------------------------------------------------------------------

#[test]
fn receipt_oracle_admin_rules() {
    let mut env = setup();
    let a = env.admin.pubkey();

    let asset = fetch_asset_config(&env.svm, &env.kusdc);
    assert_eq!(asset.oracle, receipt_oracle(&env.kusdc_reserve, USDC_FEED));

    // A live receipt's oracle can be rotated (like Solidity's `setOracle`), still fully checked.
    let rotate = set_receipt_oracle(&a, &env.kusdc, &env.kusdc_reserve, USDC_FEED, Some(env.usdc));
    send(&mut env.svm, &env.admin, &[rotate], &[]).expect("rotate a live receipt's oracle");

    // A receipt can't be made borrowable or get a lending pool.
    let res = send(&mut env.svm, &env.admin, &[ix_admin_update_asset_config(&a, &env.kusdc, 0, 8_000, 8_500, 500, true, true)], &[]);
    assert_vanna_error(res, VannaError::UnsupportedPriceSource);
    let res = send(&mut env.svm, &env.admin, &[ix_admin_initialize_reserve(&a, &a, &env.kusdc, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]);
    assert_vanna_error(res, VannaError::UnsupportedPriceSource);

    // A new cToken must match its reserve and its underlying.
    let kbad = create_mint(&mut env.svm, &env.admin, &a, CTOKEN_DECIMALS);
    register_disabled_receipt(&mut env.svm, &env.admin, &kbad, USDC_FEED);
    let set = |reserve: Pubkey, underlying: Option<Pubkey>| set_receipt_oracle(&a, &kbad, &reserve, USDC_FEED, underlying);
    let bad_reserve = Pubkey::new_unique();
    let fake = FakeReserve {
        liquidity_mint: env.usdc,
        liquidity_decimals: USDC_DECIMALS as u64,
        collateral_mint: kbad,
        total_liquidity: 1,
        collateral_supply: 1,
    };

    // Reserve for another cToken mint.
    assert_vanna_error(send(&mut env.svm, &env.admin, &[set(env.kusdc_reserve, Some(env.usdc))], &[]), VannaError::InvalidKaminoAccounts);
    // Unknown layout version.
    let mut data = kamino_reserve_data(&fake);
    data[8..16].copy_from_slice(&2u64.to_le_bytes());
    set_account_data(&mut env.svm, &bad_reserve, data);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[set(bad_reserve, Some(env.usdc))], &[]), VannaError::InvalidKaminoAccounts);

    set_account_data(&mut env.svm, &bad_reserve, kamino_reserve_data(&fake));
    // The config of the reserve's liquidity mint must be supplied: none, or another asset's.
    assert_vanna_error(send(&mut env.svm, &env.admin, &[set(bad_reserve, None)], &[]), VannaError::InvalidPriceSource);
    assert_vanna_error(send(&mut env.svm, &env.admin, &[set(bad_reserve, Some(env.wsol))], &[]), VannaError::InvalidPriceSource);
    // The reserve's liquidity decimals must match the registered underlying.
    let wrong_decimals = FakeReserve { liquidity_decimals: 9, ..fake };
    set_account_data(&mut env.svm, &bad_reserve, kamino_reserve_data(&wrong_decimals));
    assert_vanna_error(send(&mut env.svm, &env.admin, &[set(bad_reserve, Some(env.usdc))], &[]), VannaError::InvalidKaminoAccounts);

    // A cSOL registered with the USDC feed: reserve, mint and decimals all match SOL, so only the
    // same-sources check stands between it and pricing SOL at $1.
    let kfeed = create_mint(&mut env.svm, &env.admin, &a, CTOKEN_DECIMALS);
    register_disabled_receipt(&mut env.svm, &env.admin, &kfeed, USDC_FEED);
    let feed_reserve = Pubkey::new_unique();
    let sol_reserve = FakeReserve {
        liquidity_mint: env.wsol,
        liquidity_decimals: WSOL_DECIMALS as u64,
        collateral_mint: kfeed,
        total_liquidity: 1,
        collateral_supply: 1,
    };
    set_account_data(&mut env.svm, &feed_reserve, kamino_reserve_data(&sol_reserve));
    let res = send(&mut env.svm, &env.admin, &[set_receipt_oracle(&a, &kfeed, &feed_reserve, USDC_FEED, Some(env.wsol))], &[]);
    assert_vanna_error(res, VannaError::InvalidPriceFeed);
}

/// 1,000 cUSDC at 2 USDC each back $2,000. Borrowing $15,000 keeps HF at (2,000 + 15,000) /
/// 15,000 ≈ 1.13, which only passes if the cTokens are valued through the Kamino rate
/// (at 1:1 it would be ≈ 1.07).
#[test]
fn receipt_collateral_is_valued_at_the_kamino_rate() {
    let mut env = setup();
    let kusdc = env.kusdc;
    let (borrower, margin) = borrower_with_collateral(&mut env, kusdc, 1_000 * 10u64.pow(6));
    send(&mut env.svm, &borrower, &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &env.usdc)], &[]).unwrap();
    let with_source = collateral_group_metas(&env.kusdc, &margin);

    // Without the reserve account the cToken can't be valued, so the scan fails closed.
    let without_reserve = with_oracles(collateral_group_metas(&env.kusdc, &margin), &[env.usdc_price, env.wsol_price]);
    let ix = ix_user_borrow(&borrower.pubkey(), &margin, &env.usdc, 15_000 * 10u64.pow(6), u128::MAX, &without_reserve);
    assert_vanna_error(send(&mut env.svm, &borrower, &[ix], &[]), VannaError::InvalidPriceSource);

    // Above the limit: (2,000 + 25,000) / 25,000 = 1.08.
    assert_vanna_error(borrow_usdc(&mut env, &borrower, &margin, 25_000, &with_source), VannaError::HealthFactorTooLow);
    borrow_usdc(&mut env, &borrower, &margin, 15_000, &with_source).expect("borrow against cUSDC at the Kamino rate");
}

/// 10 cSOL (6-decimal cToken) redeem for 10 SOL (9-decimal underlying) = $2,000. Valued with the
/// cToken's decimals instead, the 10e9 lamports would read as 10,000 SOL ($2M), and a $25,000
/// borrow would pass. It must fail: (2,000 + 25,000) / 25,000 = 1.08.
#[test]
fn receipt_value_uses_the_underlying_decimals() {
    let mut env = setup();
    let ksol = env.ksol;
    let (borrower, margin) = borrower_with_collateral(&mut env, ksol, 10 * 10u64.pow(6));
    send(&mut env.svm, &borrower, &[ix_user_open_debt_position(&borrower.pubkey(), &borrower.pubkey(), &margin, &env.usdc)], &[]).unwrap();
    let health = collateral_group_metas(&env.ksol, &margin);

    assert_vanna_error(borrow_usdc(&mut env, &borrower, &margin, 25_000, &health), VannaError::HealthFactorTooLow);
    borrow_usdc(&mut env, &borrower, &margin, 15_000, &health).expect("borrow against cSOL valued at 1 SOL each");
}

// ---------------------------------------------------------------------------
// margin_execute
// ---------------------------------------------------------------------------

/// The SPL Token program stands in for Kamino: it's executable, so it can be registered, and any
/// call that clears every Vanna check reaches it and fails there on the unknown instruction.
fn stand_in_program() -> Pubkey {
    anchor_spl::token::ID
}

fn kamino_deposit_data(amount: u64) -> Vec<u8> {
    let mut data = DEPOSIT_RESERVE_LIQUIDITY.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data
}

/// Accounts in klend `deposit_reserve_liquidity` order, depositing USDC for cUSDC.
fn kamino_deposit_accounts(env: &Env, margin: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*margin, false),
        AccountMeta::new(env.kusdc_reserve, false),
        AccountMeta::new_readonly(Pubkey::new_unique(), false),
        AccountMeta::new_readonly(Pubkey::new_unique(), false),
        AccountMeta::new_readonly(env.usdc, false),
        AccountMeta::new(Pubkey::new_unique(), false),
        AccountMeta::new(env.kusdc, false),
        AccountMeta::new(margin_vault_ata(margin, &env.usdc), false),
        AccountMeta::new(margin_vault_ata(margin, &env.kusdc), false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new_readonly(pubkey!("Sysvar1nstructions1111111111111111111111111"), false),
    ]
}

fn execute(env: &mut Env, borrower: &Keypair, data: Vec<u8>, cpi: &[AccountMeta]) -> TransactionResult {
    let oracles = oracle_metas(&env.oracles());
    let ix = ix_margin_execute(&borrower.pubkey(), &stand_in_program(), &env.usdc, &env.kusdc, data, cpi, &oracles, 0);
    send(&mut env.svm, borrower, &[ix], &[])
}

#[test]
fn integration_registry_is_admin_only() {
    let mut env = setup();
    let outsider = funded_keypair(&mut env.svm);
    let res = send(
        &mut env.svm,
        &outsider,
        &[ix_admin_register_integration(&outsider.pubkey(), &outsider.pubkey(), &stand_in_program(), AdapterKind::KaminoLend)],
        &[],
    );
    assert!(res.is_err(), "only the admin can whitelist a program");

    // Vanna itself can never be a call target.
    let a = env.admin.pubkey();
    let res = send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &vanna_lending::ID, AdapterKind::KaminoLend)], &[]);
    assert_vanna_error(res, VannaError::CallNotAllowed);

    send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &stand_in_program(), AdapterKind::KaminoLend)], &[]).unwrap();
    let integration = fetch_integration(&env.svm, &stand_in_program());
    assert_eq!(integration.program_id, stand_in_program());
    assert_eq!(integration.adapter, AdapterKind::KaminoLend);
    assert!(integration.enabled);
}

#[test]
fn margin_execute_enforces_registry_adapter_and_account_guards() {
    let mut env = setup();
    let usdc = env.usdc;
    let (borrower, margin) = borrower_with_collateral(&mut env, usdc, 1_000 * 10u64.pow(6));
    let deposit = kamino_deposit_data(500 * 10u64.pow(6));
    let cpi = kamino_deposit_accounts(&env, &margin);

    // Not registered yet.
    assert!(execute(&mut env, &borrower, deposit.clone(), &cpi).is_err());

    let a = env.admin.pubkey();
    send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &stand_in_program(), AdapterKind::KaminoLend)], &[]).unwrap();
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &stand_in_program(), false)], &[]).unwrap();
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &cpi), VannaError::IntegrationDisabled);
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &stand_in_program(), true)], &[]).unwrap();

    // The adapter only allows deposit / redeem.
    let mut other = deposit.clone();
    other[..8].copy_from_slice(&[121, 127, 18, 204, 73, 245, 225, 65]);
    assert_vanna_error(execute(&mut env, &borrower, other, &cpi), VannaError::CallNotAllowed);

    // The signer slot must be the margin account.
    let mut wrong_signer = cpi.clone();
    wrong_signer[0] = AccountMeta::new(Pubkey::new_unique(), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &wrong_signer), VannaError::InvalidCallAccounts);

    // The spent vault slot must be the margin's USDC vault.
    let mut wrong_vault = cpi.clone();
    wrong_vault[7] = AccountMeta::new(Pubkey::new_unique(), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &wrong_vault), VannaError::InvalidCallAccounts);

    // The call must go through the reserve that prices the received cToken.
    let mut other_reserve = cpi.clone();
    other_reserve[1] = AccountMeta::new(env.ksol_reserve, false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &other_reserve), VannaError::InvalidCallAccounts);

    // No other margin token account may reach the margin-signed call.
    mint_to_wallet(&mut env.svm, &env.admin, &env.wsol, &env.admin, &borrower.pubkey(), 10u64.pow(9));
    send(&mut env.svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &env.wsol, 10u64.pow(9))], &[]).unwrap();
    let mut smuggled = cpi.clone();
    smuggled[5] = AccountMeta::new(margin_vault_ata(&margin, &env.wsol), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &smuggled), VannaError::InvalidCallAccounts);

    // Nor can Vanna-owned state (here the USDC reserve).
    let mut vanna_state = cpi.clone();
    vanna_state[2] = AccountMeta::new_readonly(reserve_pda(&env.usdc).0, false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &vanna_state), VannaError::InvalidCallAccounts);

    // A well-formed call clears every Vanna check and reaches the external program, signed by
    // the margin PDA; the stand-in then rejects the Kamino instruction it doesn't know with
    // SPL Token's `InvalidInstruction` (12), not a Vanna error (6000+).
    let failure = format!("{:?}", execute(&mut env, &borrower, deposit, &cpi).expect_err("stand-in rejects").err);
    assert!(failure.contains("Custom(12)"), "must fail inside the external program: {failure}");
}
