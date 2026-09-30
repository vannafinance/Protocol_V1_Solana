use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use crate::common::*;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_account::Account as SvmAccount;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use vanna_validator::{kamino::DEPOSIT_RESERVE_LIQUIDITY, ValidatorError};
use vanna_credit_layer::errors::VannaError;
use vanna_oracle::OracleError;

fn fake_klend() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

const CTOKEN_DECIMALS: u8 = 6;

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    usdc: Pubkey,
    wsol: Pubkey,
    kusdc: Pubkey,
    kusdc_reserve: Pubkey,
    ksol: Pubkey,
    ksol_reserve: Pubkey,
    usdc_price: Pubkey,
    wsol_price: Pubkey,
}

impl Env {
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
    send(svm, admin, &ix_admin_register_asset(&a, &a, mint, feed, 0, 8_000, 8_500, 500, 1_000, 3_600, false, false), &[]).unwrap();
}

fn receipt_oracle(reserve: &Pubkey, feed: [u8; 32]) -> OracleConfig {
    OracleConfig { klend_reserve: *reserve, klend_program: fake_klend(), ..pyth_oracle(&feed, 3_600, 1_000) }
}

fn set_receipt_oracle(a: &Pubkey, mint: &Pubkey, reserve: &Pubkey, feed: [u8; 32]) -> anchor_lang::solana_program::instruction::Instruction {
    ix_set_price_source(a, mint, receipt_oracle(reserve, feed), &[])
}

fn register_receipt(svm: &mut LiteSVM, admin: &Keypair, mint: &Pubkey, feed: [u8; 32], reserve: &Pubkey) {
    let a = admin.pubkey();
    let ixs = ix_admin_register_collateral(&a, mint, &anchor_spl::token::ID, receipt_oracle(reserve, feed), &[]);
    send(svm, admin, &ixs, &[]).expect("register receipt");
}

fn pricing(error: OracleError) -> u32 {
    error.into()
}

fn setup() -> Env {
    let mut svm = setup_svm();
    let token_program = svm.get_account(&anchor_spl::token::ID).expect("SPL Token program");
    svm.add_program(stand_in_program(), &token_program.data).expect("stand-in at klend's address");
    let admin = funded_keypair(&mut svm);
    let treasury = Pubkey::new_unique();
    send(&mut svm, &admin, &[ix_initialize_protocol(&admin.pubkey(), &treasury, &admin.pubkey(), 8)], &[]).unwrap();

    let usdc = create_mint(&mut svm, &admin, &admin.pubkey(), USDC_DECIMALS);
    let wsol = create_mint(&mut svm, &admin, &admin.pubkey(), WSOL_DECIMALS);
    let kusdc = create_mint(&mut svm, &admin, &admin.pubkey(), CTOKEN_DECIMALS);
    let ksol = create_mint(&mut svm, &admin, &admin.pubkey(), CTOKEN_DECIMALS);
    let a = admin.pubkey();
    send(&mut svm, &admin, &ix_admin_register_asset(&a, &a, &usdc, USDC_FEED, 0, 8_000, 8_500, 500, 1_000, 3_600, true, true), &[]).unwrap();
    send(&mut svm, &admin, &ix_admin_register_asset(&a, &a, &wsol, WSOL_FEED, 0, 7_000, 8_000, 500, 1_000, 3_600, true, true), &[]).unwrap();
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
    register_receipt(&mut svm, &admin, &kusdc, USDC_FEED, &kusdc_reserve);

    let ksol_reserve = Pubkey::new_unique();
    let sol_rate = FakeReserve {
        liquidity_mint: wsol,
        liquidity_decimals: WSOL_DECIMALS as u64,
        collateral_mint: ksol,
        total_liquidity: 1_000 * 10u64.pow(9),
        collateral_supply: 1_000 * 10u64.pow(6),
    };
    set_account_data(&mut svm, &ksol_reserve, kamino_reserve_data(&sol_rate));
    register_receipt(&mut svm, &admin, &ksol, WSOL_FEED, &ksol_reserve);

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

fn borrower_with_collateral(env: &mut Env, mint: Pubkey, amount: u64) -> (Keypair, Pubkey) {
    let borrower = funded_keypair(&mut env.svm);
    let (margin, _) = margin_pda(&borrower.pubkey());
    mint_to_wallet(&mut env.svm, &env.admin, &mint, &env.admin, &borrower.pubkey(), amount);
    send(&mut env.svm, &borrower, &[ix_user_create_margin(&borrower.pubkey(), &borrower.pubkey())], &[]).unwrap();
    send(&mut env.svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &mint, amount)], &[]).unwrap();
    (borrower, margin)
}

fn borrow_usdc(env: &mut Env, borrower: &Keypair, margin: &Pubkey, usdc: u64, groups: &[AccountMeta]) -> TransactionResult {
    let remaining = with_oracles(groups.to_vec(), &env.oracles());
    let ix = ix_user_borrow(&borrower.pubkey(), margin, &env.usdc, usdc * 10u64.pow(6), u128::MAX, &remaining);
    send(&mut env.svm, borrower, &[ix], &[])
}

#[test]
fn receipt_oracle_admin_rules() {
    let mut env = setup();
    let a = env.admin.pubkey();

    let book = env.svm.get_account(&price_book()).unwrap();
    let book: &vanna_oracle::PriceBook = bytemuck::from_bytes(&book.data[8..]);
    let source = book.find(&env.kusdc).expect("cUSDC in the price book");
    assert_eq!(source.config, receipt_oracle(&env.kusdc_reserve, USDC_FEED));
    assert_eq!(fetch_asset_config(&env.svm, &env.kusdc).oracle, ORACLE);

    let rotate = set_receipt_oracle(&a, &env.kusdc, &env.kusdc_reserve, USDC_FEED);
    send(&mut env.svm, &env.admin, &[rotate], &[]).expect("rotate a live receipt's price source");

    let kbad = create_mint(&mut env.svm, &env.admin, &a, CTOKEN_DECIMALS);
    register_disabled_receipt(&mut env.svm, &env.admin, &kbad, USDC_FEED);
    let set = |reserve: Pubkey| set_receipt_oracle(&a, &kbad, &reserve, USDC_FEED);
    let bad_reserve = Pubkey::new_unique();
    let fake = FakeReserve {
        liquidity_mint: env.usdc,
        liquidity_decimals: USDC_DECIMALS as u64,
        collateral_mint: kbad,
        total_liquidity: 1,
        collateral_supply: 1,
    };

    let res = send(&mut env.svm, &env.admin, &[set(env.kusdc_reserve)], &[]);
    assert_custom_error(res, pricing(OracleError::InvalidKaminoAccounts));
    let mut data = kamino_reserve_data(&fake);
    data[8..16].copy_from_slice(&2u64.to_le_bytes());
    set_account_data(&mut env.svm, &bad_reserve, data);
    assert_custom_error(send(&mut env.svm, &env.admin, &[set(bad_reserve)], &[]), pricing(OracleError::InvalidKaminoAccounts));

    let unknown = FakeReserve { liquidity_mint: Pubkey::new_unique(), ..fake };
    set_account_data(&mut env.svm, &bad_reserve, kamino_reserve_data(&unknown));
    assert_custom_error(send(&mut env.svm, &env.admin, &[set(bad_reserve)], &[]), OracleError::UnknownAsset.into());
    let wrong_decimals = FakeReserve { liquidity_decimals: 9, ..fake };
    set_account_data(&mut env.svm, &bad_reserve, kamino_reserve_data(&wrong_decimals));
    assert_custom_error(send(&mut env.svm, &env.admin, &[set(bad_reserve)], &[]), pricing(OracleError::InvalidKaminoAccounts));

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
    let res = send(&mut env.svm, &env.admin, &[set_receipt_oracle(&a, &kfeed, &feed_reserve, USDC_FEED)], &[]);
    assert_custom_error(res, pricing(OracleError::InvalidPriceFeed));
}

#[test]
fn receipt_collateral_is_valued_at_the_kamino_rate() {
    let mut env = setup();
    let kusdc = env.kusdc;
    let (borrower, margin) = borrower_with_collateral(&mut env, kusdc, 1_000 * 10u64.pow(6));
    let with_source = collateral_group_metas(&env.kusdc, &margin);

    let without_reserve = with_oracles(collateral_group_metas(&env.kusdc, &margin), &[env.usdc_price, env.wsol_price]);
    let ix = ix_user_borrow(&borrower.pubkey(), &margin, &env.usdc, 15_000 * 10u64.pow(6), u128::MAX, &without_reserve);
    assert_custom_error(send(&mut env.svm, &borrower, &[ix], &[]), pricing(OracleError::InvalidPriceSource));

    assert_vanna_error(borrow_usdc(&mut env, &borrower, &margin, 25_000, &with_source), VannaError::HealthFactorTooLow);
    borrow_usdc(&mut env, &borrower, &margin, 15_000, &with_source).expect("borrow against cUSDC at the Kamino rate");
}

#[test]
fn receipt_value_uses_the_underlying_decimals() {
    let mut env = setup();
    let ksol = env.ksol;
    let (borrower, margin) = borrower_with_collateral(&mut env, ksol, 10 * 10u64.pow(6));
    let health = collateral_group_metas(&env.ksol, &margin);

    assert_vanna_error(borrow_usdc(&mut env, &borrower, &margin, 25_000, &health), VannaError::HealthFactorTooLow);
    borrow_usdc(&mut env, &borrower, &margin, 15_000, &health).expect("borrow against cSOL valued at 1 SOL each");
}

fn stand_in_program() -> Pubkey {
    crate::common::kamino::KLEND
}

fn kamino_deposit_data(amount: u64) -> Vec<u8> {
    let mut data = DEPOSIT_RESERVE_LIQUIDITY.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data
}

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
    let u = borrower.pubkey();
    let margin = margin_pda(&u).0;
    let mut rest = margin_groups(&env.svm, &u, &[env.usdc, env.wsol, env.kusdc, env.ksol]);
    rest.extend(collateral_group_metas(&env.kusdc, &margin));
    rest.extend(oracle_segment(&env.oracles()));
    let ix = ix_margin_execute(&u, &stand_in_program(), None, data, cpi, &rest, 1);
    send(&mut env.svm, borrower, &[ix_create_vault(&u, &margin, &env.kusdc, &anchor_spl::token::ID), ix], &[])
}

#[test]
fn integration_registry_is_admin_only() {
    let mut env = setup();
    let outsider = funded_keypair(&mut env.svm);
    let res = send(
        &mut env.svm,
        &outsider,
        &[ix_admin_register_integration(&outsider.pubkey(), &outsider.pubkey(), &stand_in_program(), VALIDATOR)],
        &[],
    );
    assert!(res.is_err(), "only the admin can whitelist a program");

    let a = env.admin.pubkey();
    let res = send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &vanna_credit_layer::ID, VALIDATOR)], &[]);
    assert_vanna_error(res, VannaError::CallNotAllowed);

    send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &stand_in_program(), VALIDATOR)], &[]).unwrap();
    let integration = fetch_integration(&env.svm, &stand_in_program());
    assert_eq!(integration.program_id, stand_in_program());
    assert_eq!(integration.validator, VALIDATOR);
    assert!(integration.enabled);
}

#[test]
fn margin_execute_enforces_registry_validator_and_account_guards() {
    let mut env = setup();
    let usdc = env.usdc;
    let (borrower, margin) = borrower_with_collateral(&mut env, usdc, 1_000 * 10u64.pow(6));
    let deposit = kamino_deposit_data(500 * 10u64.pow(6));
    let cpi = kamino_deposit_accounts(&env, &margin);

    assert!(execute(&mut env, &borrower, deposit.clone(), &cpi).is_err());

    let a = env.admin.pubkey();
    send(&mut env.svm, &env.admin, &[ix_admin_register_integration(&a, &a, &stand_in_program(), VALIDATOR)], &[]).unwrap();
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &stand_in_program(), false)], &[]).unwrap();
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &cpi), VannaError::IntegrationDisabled);
    send(&mut env.svm, &env.admin, &[ix_admin_set_integration_enabled(&a, &stand_in_program(), true)], &[]).unwrap();

    let mut other = deposit.clone();
    other[..8].copy_from_slice(&[121, 127, 18, 204, 73, 245, 225, 65]);
    assert_custom_error(execute(&mut env, &borrower, other, &cpi), ValidatorError::CallNotAllowed.into());

    let mut wrong_signer = cpi.clone();
    wrong_signer[0] = AccountMeta::new(Pubkey::new_unique(), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &wrong_signer), VannaError::InvalidCallAccounts);

    let mut wrong_vault = cpi.clone();
    wrong_vault[7] = AccountMeta::new(Pubkey::new_unique(), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &wrong_vault), VannaError::InvalidCallAccounts);

    mint_to_wallet(&mut env.svm, &env.admin, &env.wsol, &env.admin, &borrower.pubkey(), 10u64.pow(9));
    send(&mut env.svm, &borrower, &[ix_user_deposit_collateral(&borrower.pubkey(), &margin, &env.wsol, 10u64.pow(9))], &[]).unwrap();
    let mut smuggled = cpi.clone();
    smuggled[5] = AccountMeta::new(margin_vault_ata(&margin, &env.wsol), false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &smuggled), VannaError::InvalidCallAccounts);

    let mut vanna_state = cpi.clone();
    vanna_state[2] = AccountMeta::new_readonly(reserve_pda(&env.usdc).0, false);
    assert_vanna_error(execute(&mut env, &borrower, deposit.clone(), &vanna_state), VannaError::InvalidCallAccounts);

    let failure = format!("{:?}", execute(&mut env, &borrower, deposit, &cpi).expect_err("stand-in rejects").err);
    assert!(failure.contains("Custom(12)"), "must fail inside the external program: {failure}");
}
