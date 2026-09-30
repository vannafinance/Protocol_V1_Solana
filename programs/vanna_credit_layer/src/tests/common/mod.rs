#![allow(dead_code)]

pub mod jupiter;
pub mod kamino;
pub mod mainnet;
pub mod oracles;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Clock;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::spl_associated_token_account;
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use pyth_solana_receiver_sdk::price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel};
use solana_account::Account as SvmAccount;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_program_pack::Pack;
use solana_signer::Signer as SvmSigner;
use solana_system_interface::instruction::create_account;
use solana_transaction::versioned::VersionedTransaction;
use vanna_credit_layer::constants::*;
use vanna_credit_layer::state::{AssetConfig, DebtPosition, Integration, MarginAccount, ProtocolConfig, Reserve};

pub use anchor_spl::associated_token::get_associated_token_address;
pub use vanna_credit_layer::state::reserve::RateCurve;
pub use vanna_oracle::pyth::canonical_feed_account;
pub use vanna_oracle::scope::SCOPE_PROGRAM_ID;
pub use vanna_oracle::{OracleConfig, EMPTY_SCOPE_CHAIN};
#[allow(unused_imports)]
pub use vanna_oracle::reference::{kamino_main_market, kamino_scope, known_mints, pyth_feeds};

pub const ORACLE: Pubkey = vanna_oracle::ID;
pub const VALIDATOR: Pubkey = vanna_validator::ID;

pub const USDC_DECIMALS: u8 = 6;
pub const WSOL_DECIMALS: u8 = 9;

pub const USDC_FEED: [u8; 32] = [1u8; 32];
pub const WSOL_FEED: [u8; 32] = [2u8; 32];
pub const USDC_PRICE: i64 = 100_000_000;
pub const WSOL_PRICE: i64 = 20_000_000_000;

pub const DEFAULT_RATE_CURVE: RateCurve = RateCurve {
    linear_coeff_wad: 100_000_000_000_000_000,
    jump_coeff_wad: 300_000_000_000_000_000,
    rate_multiplier_wad: 3_500_000_000_000_000_000,
};

pub fn program_bytes() -> &'static [u8] {
    include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/vanna_credit_layer.so"))
}

macro_rules! deployed {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/", $name, ".so")).as_slice()
    };
}

pub fn agent_programs() -> [(Pubkey, &'static [u8]); 2] {
    [(ORACLE, deployed!("vanna_oracle")), (VALIDATOR, deployed!("vanna_validator"))]
}

pub fn setup_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program(vanna_credit_layer::ID, program_bytes()).unwrap();
    for (id, bytes) in agent_programs() {
        svm.add_program(id, bytes).unwrap();
    }
    write_empty_price_book(&mut svm);
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    set_pyth(&mut svm, USDC_FEED, USDC_PRICE, -8, now);
    set_pyth(&mut svm, WSOL_FEED, WSOL_PRICE, -8, now);
    svm
}

pub fn price_book() -> Pubkey {
    Pubkey::find_program_address(&[vanna_oracle::PRICE_BOOK_SEED], &ORACLE).0
}

fn write_empty_price_book(svm: &mut LiteSVM) {
    use anchor_lang::Discriminator;
    let (key, bump) = Pubkey::find_program_address(&[vanna_oracle::PRICE_BOOK_SEED], &ORACLE);
    let mut data = vec![0u8; 8 + std::mem::size_of::<vanna_oracle::PriceBook>()];
    data[..8].copy_from_slice(vanna_oracle::PriceBook::DISCRIMINATOR);
    data[8] = bump;
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(key, SvmAccount { lamports, data, owner: ORACLE, executable: false, rent_epoch: 0 }).unwrap();
}

pub fn funded_keypair(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
    kp
}

pub fn send(svm: &mut LiteSVM, payer: &Keypair, ixs: &[Instruction], extra_signers: &[&Keypair]) -> TransactionResult {
    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
    let mut signers: Vec<&Keypair> = vec![payer];
    for candidate in extra_signers {
        if !signers.iter().any(|s| s.pubkey() == candidate.pubkey()) {
            signers.push(candidate);
        }
    }
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers).unwrap();
    svm.send_transaction(tx)
}

pub fn advance_time(svm: &mut LiteSVM, seconds: i64) {
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp += seconds;
    svm.set_sysvar(&clock);
}

pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair, authority: &Pubkey, decimals: u8) -> Pubkey {
    let mint_kp = Keypair::new();
    let mint_len = spl_token_interface::state::Mint::LEN;
    let rent = svm.minimum_balance_for_rent_exemption(mint_len);
    let create_ix = create_account(&payer.pubkey(), &mint_kp.pubkey(), rent, mint_len as u64, &spl_token_interface::ID);
    let init_ix =
        spl_token_interface::instruction::initialize_mint2(&spl_token_interface::ID, &mint_kp.pubkey(), authority, None, decimals)
            .unwrap();
    let res = send(svm, payer, &[create_ix, init_ix], &[&mint_kp]);
    assert!(res.is_ok(), "create_mint failed: {res:?}");
    mint_kp.pubkey()
}

pub fn mint_to_wallet(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey, mint_authority: &Keypair, owner: &Pubkey, amount: u64) -> Pubkey {
    let ata = get_associated_token_address(owner, mint);
    let create_ata_ix = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &payer.pubkey(),
        owner,
        mint,
        &spl_token_interface::ID,
    );
    let mint_ix = spl_token_interface::instruction::mint_to(
        &spl_token_interface::ID,
        mint,
        &ata,
        &mint_authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    let res = send(svm, payer, &[create_ata_ix, mint_ix], &[mint_authority]);
    assert!(res.is_ok(), "mint_to_wallet failed: {res:?}");
    ata
}

pub fn token_balance(svm: &LiteSVM, ata: &Pubkey) -> u64 {
    let acc = svm.get_account(ata).expect("token account missing");
    u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
}

pub const TOKEN_2022: Pubkey = anchor_spl::token_2022::ID;

pub fn ata_for(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    anchor_spl::associated_token::get_associated_token_address_with_program_id(owner, mint, token_program)
}

pub fn create_scaled_ui_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    authority: &Keypair,
    decimals: u8,
    multiplier: f64,
    scheduled: Option<(f64, i64)>,
) -> Pubkey {
    use anchor_spl::token_2022::spl_token_2022::extension::{scaled_ui_amount, ExtensionType};
    use anchor_spl::token_2022::spl_token_2022::{instruction::initialize_mint2, state::Mint};
    let mint_kp = Keypair::new();
    let mint = mint_kp.pubkey();
    let len = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::ScaledUiAmount]).unwrap();
    let rent = svm.minimum_balance_for_rent_exemption(len);
    let mut ixs = vec![
        create_account(&payer.pubkey(), &mint, rent, len as u64, &TOKEN_2022),
        scaled_ui_amount::instruction::initialize(&TOKEN_2022, &mint, Some(authority.pubkey()), multiplier).unwrap(),
        initialize_mint2(&TOKEN_2022, &mint, &authority.pubkey(), None, decimals).unwrap(),
    ];
    if let Some((new_multiplier, effective)) = scheduled {
        ixs.push(
            scaled_ui_amount::instruction::update_multiplier(&TOKEN_2022, &mint, &authority.pubkey(), &[], new_multiplier, effective)
                .unwrap(),
        );
    }
    let signers: &[&Keypair] = if scheduled.is_some() { &[&mint_kp, authority] } else { &[&mint_kp] };
    let res = send(svm, payer, &ixs, signers);
    assert!(res.is_ok(), "create_scaled_ui_mint failed: {res:?}");
    mint
}

pub fn mint_to_wallet_with(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    token_program: &Pubkey,
    mint_authority: &Keypair,
    owner: &Pubkey,
    amount: u64,
) -> Pubkey {
    let ata = ata_for(owner, mint, token_program);
    let create_ata_ix =
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(&payer.pubkey(), owner, mint, token_program);
    let mint_ix = anchor_spl::token_2022::spl_token_2022::instruction::mint_to(token_program, mint, &ata, &mint_authority.pubkey(), &[], amount)
        .unwrap();
    let res = send(svm, payer, &[create_ata_ix, mint_ix], &[mint_authority]);
    assert!(res.is_ok(), "mint_to_wallet_with failed: {res:?}");
    ata
}

pub fn assert_vanna_error(res: TransactionResult, err: vanna_credit_layer::errors::VannaError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + err as u32;
    let failure = format!("{:?}", res.expect_err("transaction should have failed").err);
    assert!(failure.contains(&format!("Custom({code})")), "expected error {code}, got {failure}");
}

pub fn assert_custom_error(res: TransactionResult, code: u32) {
    let failure = format!("{:?}", res.expect_err("transaction should have failed").err);
    assert!(failure.contains(&format!("Custom({code})")), "expected error {code}, got {failure}");
}

pub fn event<E: anchor_lang::Event + anchor_lang::AnchorDeserialize + anchor_lang::Discriminator>(logs: &[String]) -> E {
    use anchor_lang::__private::base64::{engine::general_purpose::STANDARD, Engine};
    logs.iter()
        .rev()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|b64| STANDARD.decode(b64).ok())
        .find(|bytes| bytes.starts_with(E::DISCRIMINATOR))
        .map(|bytes| E::try_from_slice(&bytes[E::DISCRIMINATOR.len()..]).unwrap())
        .expect("event not emitted")
}

pub fn set_price(
    svm: &mut LiteSVM,
    pubkey: &Pubkey,
    feed_id: [u8; 32],
    price: i64,
    conf: u64,
    exponent: i32,
    publish_time: i64,
) {
    let update = PriceUpdateV2 {
        write_authority: Pubkey::new_unique(),
        verification_level: VerificationLevel::Full,
        price_message: PriceFeedMessage {
            feed_id,
            ema_price: price,
            ema_conf: conf,
            price,
            conf,
            exponent,
            prev_publish_time: publish_time - 1,
            publish_time,
        },
        posted_slot: 1,
    };
    let mut data = Vec::new();
    update.try_serialize(&mut data).unwrap();
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        *pubkey,
        SvmAccount {
            lamports,
            data,
            owner: pyth_solana_receiver_sdk::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
}

pub fn pyth_account(feed_id: &[u8; 32]) -> Pubkey {
    canonical_feed_account(feed_id)
}

pub fn set_pyth(svm: &mut LiteSVM, feed_id: [u8; 32], price: i64, exponent: i32, publish_time: i64) -> Pubkey {
    let key = pyth_account(&feed_id);
    set_price(svm, &key, feed_id, price, 0, exponent, publish_time);
    key
}

#[allow(clippy::too_many_arguments)]
pub fn set_price_with_ema(
    svm: &mut LiteSVM,
    pubkey: &Pubkey,
    feed_id: [u8; 32],
    price: i64,
    ema_price: i64,
    conf: u64,
    exponent: i32,
    publish_time: i64,
) {
    set_price(svm, pubkey, feed_id, price, conf, exponent, publish_time);
    let mut account = svm.get_account(pubkey).unwrap();
    let mut update = PriceUpdateV2::try_deserialize(&mut &account.data[..]).unwrap();
    update.price_message.ema_price = ema_price;
    account.data.clear();
    update.try_serialize(&mut account.data).unwrap();
    svm.set_account(*pubkey, account).unwrap();
}

pub fn pyth_oracle(feed_id: &[u8; 32], max_age_secs: u32, max_confidence_bps: u16) -> OracleConfig {
    OracleConfig { pyth_price: pyth_account(feed_id), max_age_secs, max_confidence_bps, ..OracleConfig::default() }
}

pub fn scope_chain(entries: &[u16]) -> [u16; 4] {
    let mut chain = EMPTY_SCOPE_CHAIN;
    chain[..entries.len()].copy_from_slice(entries);
    chain
}

pub fn scope_oracle(prices: &Pubkey, chain: &[u16], twap: &[u16], max_age_secs: u32, twap_bps: u16) -> OracleConfig {
    OracleConfig {
        scope_prices: *prices,
        scope_chain: scope_chain(chain),
        scope_twap_chain: scope_chain(twap),
        max_age_secs,
        max_twap_divergence_bps: twap_bps,
        max_confidence_bps: 1_000,
        ..OracleConfig::default()
    }
}

fn scope_entry_offset(index: u16) -> usize {
    40 + 56 * usize::from(index)
}

pub fn set_scope_prices(svm: &mut LiteSVM, key: &Pubkey, entries: &[(u16, u64, u64, i64)]) {
    use vanna_oracle::scope::{ORACLE_PRICES_DISCRIMINATOR, ORACLE_PRICES_LEN};
    let mut account = svm.get_account(key).filter(|a| a.owner == SCOPE_PROGRAM_ID).unwrap_or_else(|| {
        let mut data = vec![0u8; ORACLE_PRICES_LEN];
        data[..8].copy_from_slice(&ORACLE_PRICES_DISCRIMINATOR);
        SvmAccount { lamports: svm.minimum_balance_for_rent_exemption(data.len()), data, owner: SCOPE_PROGRAM_ID, executable: false, rent_epoch: 0 }
    });
    for (index, value, exp, unix_timestamp) in entries {
        let at = scope_entry_offset(*index);
        account.data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        account.data[at + 8..at + 16].copy_from_slice(&exp.to_le_bytes());
        account.data[at + 24..at + 32].copy_from_slice(&(*unix_timestamp as u64).to_le_bytes());
    }
    svm.set_account(*key, account).unwrap();
}

pub fn oracle_metas(keys: &[Pubkey]) -> Vec<AccountMeta> {
    let mut metas: Vec<AccountMeta> = Vec::new();
    for key in keys {
        if !metas.iter().any(|m| m.pubkey == *key) {
            metas.push(AccountMeta::new_readonly(*key, false));
        }
    }
    metas
}

pub fn protocol_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PROTOCOL_SEED], &vanna_credit_layer::ID)
}

pub fn asset_config_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ASSET_SEED, mint.as_ref()], &vanna_credit_layer::ID)
}

pub fn reserve_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[RESERVE_SEED, mint.as_ref()], &vanna_credit_layer::ID)
}

pub fn share_mint_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SHARE_MINT_SEED, mint.as_ref()], &vanna_credit_layer::ID)
}

pub fn margin_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARGIN_SEED, authority.as_ref()], &vanna_credit_layer::ID)
}

pub fn margin_vault_ata(margin: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(margin, mint)
}

pub fn margin_vault_ata_with(margin: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    ata_for(margin, mint, token_program)
}

pub fn debt_position_pda(margin: &Pubkey, reserve: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[DEBT_SEED, margin.as_ref(), reserve.as_ref()], &vanna_credit_layer::ID)
}

pub fn integration_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[INTEGRATION_SEED, program_id.as_ref()], &vanna_credit_layer::ID).0
}

pub fn ix_initialize_protocol(admin: &Pubkey, treasury: &Pubkey, payer: &Pubkey, max_assets_per_margin: u8) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::InitializeProtocol {
            payer: *payer,
            admin: *admin,
            protocol_config,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::InitializeProtocol {
            treasury: *treasury,
            max_assets_per_margin,
        }
        .data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_admin_register_asset(
    admin: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    price_feed_id: [u8; 32],
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    max_confidence_bps: u16,
    max_price_age_secs: u32,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Vec<Instruction> {
    ix_admin_register_asset_with(
        admin,
        payer,
        mint,
        &anchor_spl::token::ID,
        pyth_oracle(&price_feed_id, max_price_age_secs, max_confidence_bps),
        &[],
        max_collateral_per_margin,
        ltv_bps,
        liquidation_threshold_bps,
        liquidation_bonus_bps,
        collateral_enabled,
        borrow_enabled,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn ix_admin_register_asset_with(
    admin: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    oracle: OracleConfig,
    extra: &[Pubkey],
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Vec<Instruction> {
    let oracle_accounts: Vec<Pubkey> = oracle.accounts().collect();
    vec![
        ix_set_price_source(admin, mint, oracle, extra),
        ix_register_token(
            admin,
            payer,
            mint,
            token_program,
            &oracle_accounts,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        ),
    ]
}

pub fn ix_set_price_source(admin: &Pubkey, mint: &Pubkey, oracle: OracleConfig, extra: &[Pubkey]) -> Instruction {
    let mut accounts = vanna_oracle::accounts::SetPriceSource {
        admin: *admin,
        protocol_config: protocol_config_pda().0,
        price_book: price_book(),
        mint: *mint,
    }
    .to_account_metas(None);
    accounts.extend(oracle_metas(&oracle.accounts().chain(extra.iter().copied()).collect::<Vec<_>>()));
    Instruction {
        program_id: ORACLE,
        accounts,
        data: vanna_oracle::instruction::SetPriceSource { config: oracle }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_register_token(
    admin: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    oracle_accounts: &[Pubkey],
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let mut accounts = vanna_credit_layer::accounts::AdminRegisterAsset {
        admin: *admin,
        payer: *payer,
        protocol_config,
        underlying_mint: *mint,
        asset_config,
        oracle: ORACLE,
        token_program: *token_program,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend(oracle_metas(&std::iter::once(price_book()).chain(oracle_accounts.iter().copied()).collect::<Vec<_>>()));
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts,
        data: vanna_credit_layer::instruction::AdminRegisterAsset {
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        }
        .data(),
    }
}

pub fn ix_admin_register_collateral(admin: &Pubkey, mint: &Pubkey, token_program: &Pubkey, oracle: OracleConfig, extra: &[Pubkey]) -> Vec<Instruction> {
    ix_admin_register_asset_with(admin, admin, mint, token_program, oracle, extra, 0, 8_000, 8_500, 500, true, false)
}

pub fn ix_admin_set_asset_oracle(admin: &Pubkey, mint: &Pubkey, oracle: OracleConfig, extra: &[Pubkey]) -> Instruction {
    ix_set_price_source(admin, mint, oracle, extra)
}

#[allow(clippy::too_many_arguments)]
pub fn ix_admin_initialize_reserve(
    admin: &Pubkey,
    payer: &Pubkey,
    mint: &Pubkey,
    rate_curve: RateCurve,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (share_mint, _) = share_mint_pda(mint);
    let liquidity_vault = get_associated_token_address(&reserve, mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminInitializeReserve {
            admin: *admin,
            payer: *payer,
            protocol_config,
            asset_config,
            underlying_mint: *mint,
            reserve,
            liquidity_vault,
            share_mint,
            token_program: anchor_spl::token::ID,
            share_token_program: anchor_spl::token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminInitializeReserve {
            rate_curve,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        }
        .data(),
    }
}

pub fn ix_admin_propose_authority(admin: &Pubkey, new_admin: &Pubkey) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminProposeAuthority { admin: *admin, protocol_config }.to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminProposeAuthority { new_admin: *new_admin }.data(),
    }
}

pub fn ix_authority_accept_admin(pending_admin: &Pubkey) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AuthorityAcceptAdmin { pending_admin: *pending_admin, protocol_config }
            .to_account_metas(None),
        data: vanna_credit_layer::instruction::AuthorityAcceptAdmin {}.data(),
    }
}

pub fn ix_admin_set_operating_mode(admin: &Pubkey, new_mode: u8) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminSetOperatingMode { admin: *admin, protocol_config }.to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminSetOperatingMode { new_mode }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_admin_update_asset_config(
    admin: &Pubkey,
    mint: &Pubkey,
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminUpdateAssetConfig { admin: *admin, protocol_config, asset_config }
            .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminUpdateAssetConfig {
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            collateral_enabled,
            borrow_enabled,
        }
        .data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_admin_update_reserve_config(
    admin: &Pubkey,
    mint: &Pubkey,
    rate_curve: RateCurve,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (reserve, _) = reserve_pda(mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminUpdateReserveConfig {
            admin: *admin,
            protocol_config,
            underlying_mint: *mint,
            reserve,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminUpdateReserveConfig {
            rate_curve,
            reserve_factor_bps,
            supply_cap,
            borrow_cap,
            status,
        }
        .data(),
    }
}

pub fn ix_admin_collect_protocol_fees(admin: &Pubkey, mint: &Pubkey, treasury: &Pubkey, amount: u64) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (reserve, _) = reserve_pda(mint);
    let liquidity_vault = get_associated_token_address(&reserve, mint);
    let treasury_ata = get_associated_token_address(treasury, mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminCollectProtocolFees {
            admin: *admin,
            protocol_config,
            underlying_mint: *mint,
            reserve,
            liquidity_vault,
            treasury_ata,
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminCollectProtocolFees { amount }.data(),
    }
}

pub fn ix_public_refresh_reserve(mint: &Pubkey) -> Instruction {
    let (reserve, _) = reserve_pda(mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::PublicRefreshReserve { reserve }.to_account_metas(None),
        data: vanna_credit_layer::instruction::PublicRefreshReserve {}.data(),
    }
}

pub fn ix_user_reclaim_rent(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, vault: bool, debt_position: bool) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::UserReclaimRent {
            authority: *authority,
            margin_account: *margin,
            asset_config,
            mint: *mint,
            margin_vault: vault.then(|| margin_vault_ata(margin, mint)),
            debt_position: debt_position.then(|| debt_position_pda(margin, &reserve).0),
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::UserReclaimRent {}.data(),
    }
}

pub fn ix_user_close_margin(authority: &Pubkey, margin: &Pubkey) -> Instruction {
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::UserCloseMargin { authority: *authority, margin_account: *margin }
            .to_account_metas(None),
        data: vanna_credit_layer::instruction::UserCloseMargin {}.data(),
    }
}

pub fn ix_lender_supply(lender: &Pubkey, mint: &Pubkey, assets: u64, min_shares_out: u64) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (share_mint, _) = share_mint_pda(mint);
    let liquidity_vault = get_associated_token_address(&reserve, mint);
    let lender_token_account = get_associated_token_address(lender, mint);
    let lender_share_account = get_associated_token_address(lender, &share_mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::LenderSupply {
            lender: *lender,
            protocol_config,
            asset_config,
            reserve,
            underlying_mint: *mint,
            lender_token_account,
            liquidity_vault,
            share_mint,
            lender_share_account,
            token_program: anchor_spl::token::ID,
            share_token_program: anchor_spl::token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::LenderSupply { assets, min_shares_out }.data(),
    }
}

pub fn ix_lender_redeem(lender: &Pubkey, mint: &Pubkey, shares: u64, min_assets_out: u64) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (share_mint, _) = share_mint_pda(mint);
    let liquidity_vault = get_associated_token_address(&reserve, mint);
    let lender_token_account = get_associated_token_address(lender, mint);
    let lender_share_account = get_associated_token_address(lender, &share_mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::LenderRedeem {
            lender: *lender,
            protocol_config,
            asset_config,
            reserve,
            underlying_mint: *mint,
            lender_token_account,
            liquidity_vault,
            share_mint,
            lender_share_account,
            token_program: anchor_spl::token::ID,
            share_token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::LenderRedeem { shares, min_assets_out }.data(),
    }
}

pub fn ix_user_create_margin(authority: &Pubkey, payer: &Pubkey) -> Instruction {
    let (margin_account, _) = margin_pda(authority);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::UserCreateMargin {
            authority: *authority,
            payer: *payer,
            margin_account,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::UserCreateMargin {}.data(),
    }
}

pub fn ix_user_deposit_collateral(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, amount: u64) -> Instruction {
    ix_user_deposit_collateral_with(authority, margin, mint, &anchor_spl::token::ID, amount)
}

pub fn ix_user_deposit_collateral_with(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, token_program: &Pubkey, amount: u64) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata_with(margin, mint, token_program);
    let source_token_account = ata_for(authority, mint, token_program);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::UserDepositCollateral {
            authority: *authority,
            protocol_config,
            margin_account: *margin,
            asset_config,
            mint: *mint,
            source_token_account,
            margin_vault,
            token_program: *token_program,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::UserDepositCollateral { amount }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_user_borrow(
    authority: &Pubkey,
    margin: &Pubkey,
    mint: &Pubkey,
    assets: u64,
    max_debt_shares: u128,
    remaining: &[AccountMeta],
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    let reserve_vault = get_associated_token_address(&reserve, mint);
    let margin_vault = margin_vault_ata(margin, mint);
    let mut accounts = vanna_credit_layer::accounts::UserBorrow {
        authority: *authority,
        protocol_config,
        margin_account: *margin,
        asset_config,
        reserve,
        debt_position,
        mint: *mint,
        reserve_vault,
        margin_vault,
        token_program: anchor_spl::token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts,
        data: vanna_credit_layer::instruction::UserBorrow { assets, max_debt_shares }.data(),
    }
}

pub fn ix_user_repay_from_margin(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, max_assets: u64, repay_all: bool) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    let margin_vault = margin_vault_ata(margin, mint);
    let reserve_vault = get_associated_token_address(&reserve, mint);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::UserRepayFromMargin {
            authority: *authority,
            margin_account: *margin,
            asset_config,
            reserve,
            debt_position,
            mint: *mint,
            margin_vault,
            reserve_vault,
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::UserRepayFromMargin { max_assets, repay_all }.data(),
    }
}

pub fn ix_user_withdraw_collateral(
    authority: &Pubkey,
    margin: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    min_health_factor_wad: u128,
    remaining: &[AccountMeta],
) -> Instruction {
    ix_user_withdraw_collateral_with(authority, margin, mint, &anchor_spl::token::ID, amount, min_health_factor_wad, remaining)
}

pub fn ix_user_withdraw_collateral_with(
    authority: &Pubkey,
    margin: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    amount: u64,
    min_health_factor_wad: u128,
    remaining: &[AccountMeta],
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata_with(margin, mint, token_program);
    let destination_token_account = ata_for(authority, mint, token_program);
    let mut accounts = vanna_credit_layer::accounts::UserWithdrawCollateral {
        authority: *authority,
        protocol_config,
        margin_account: *margin,
        asset_config,
        mint: *mint,
        destination_token_account,
        margin_vault,
        token_program: *token_program,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts,
        data: vanna_credit_layer::instruction::UserWithdrawCollateral { amount, min_health_factor_wad }.data(),
    }
}

pub fn ix_public_liquidate(liquidator: &Pubkey, margin: &Pubkey, positions: &[LiqPosition], oracles: &[Pubkey]) -> Instruction {
    let mut accounts = vanna_credit_layer::accounts::PublicLiquidate { liquidator: *liquidator, margin_account: *margin }
        .to_account_metas(None);
    for p in positions {
        accounts.extend_from_slice(&p.health);
    }
    for p in positions {
        accounts.extend_from_slice(&p.settlement);
    }
    accounts.extend(oracle_segment(oracles));
    Instruction { program_id: vanna_credit_layer::ID, accounts, data: vanna_credit_layer::instruction::PublicLiquidate {}.data() }
}

pub struct LiqPosition {
    pub health: Vec<AccountMeta>,
    pub settlement: Vec<AccountMeta>,
}

pub fn liq_collateral(mint: &Pubkey, token_program: &Pubkey, margin: &Pubkey, destination: &Pubkey) -> LiqPosition {
    let mut health = collateral_group_with(mint, token_program, margin);
    health[1].is_writable = true;
    LiqPosition {
        health,
        settlement: vec![
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*token_program, false),
        ],
    }
}

pub fn liq_debt(mint: &Pubkey, margin: &Pubkey, source: &Pubkey) -> LiqPosition {
    let mut health = debt_group_metas(mint, margin);
    health[1].is_writable = true;
    health[2].is_writable = true;
    let reserve = reserve_pda(mint).0;
    LiqPosition {
        health,
        settlement: vec![
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(get_associated_token_address(&reserve, mint), false),
            AccountMeta::new(*source, false),
            AccountMeta::new_readonly(anchor_spl::token::ID, false),
        ],
    }
}

pub fn ix_admin_register_integration(admin: &Pubkey, payer: &Pubkey, target_program: &Pubkey, validator: Pubkey) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminRegisterIntegration {
            admin: *admin,
            payer: *payer,
            protocol_config,
            target_program: *target_program,
            validator,
            integration: integration_pda(target_program),
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminRegisterIntegration {}.data(),
    }
}

pub fn ix_admin_set_integration_enabled(admin: &Pubkey, target_program: &Pubkey, enabled: bool) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminSetIntegrationEnabled {
            admin: *admin,
            protocol_config,
            integration: integration_pda(target_program),
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminSetIntegrationEnabled { enabled }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_margin_execute(
    authority: &Pubkey,
    target_program: &Pubkey,
    venue: Option<&Pubkey>,
    data: Vec<u8>,
    cpi_accounts: &[AccountMeta],
    rest: &[AccountMeta],
    new_assets: u8,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (margin, _) = margin_pda(authority);
    let mut accounts = vanna_credit_layer::accounts::MarginExecute {
        authority: *authority,
        protocol_config,
        margin_account: margin,
        integration: integration_pda(target_program),
        target_program: *target_program,
        validator: VALIDATOR,
        venue_asset: venue.map(|venue| asset_config_pda(venue).0),
        venue_account: venue.map(|venue| vanna_credit_layer::interface::venue_account_address(&margin, venue).0),
    }
    .to_account_metas(None);
    accounts.extend_from_slice(cpi_accounts);
    accounts.extend_from_slice(rest);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts,
        data: vanna_credit_layer::instruction::MarginExecute { data, call_account_count: cpi_accounts.len() as u16, new_assets }.data(),
    }
}

pub fn ix_create_vault(payer: &Pubkey, margin: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Instruction {
    spl_associated_token_account::instruction::create_associated_token_account_idempotent(payer, margin, mint, token_program)
}

pub fn margin_groups(svm: &LiteSVM, authority: &Pubkey, mints: &[Pubkey]) -> Vec<AccountMeta> {
    let (margin_key, _) = margin_pda(authority);
    let margin = fetch_margin(svm, authority);
    let registered: Vec<(Pubkey, AssetConfig)> = mints
        .iter()
        .filter(|mint| svm.get_account(&asset_config_pda(mint).0).is_some())
        .map(|mint| (*mint, fetch_asset_config(svm, mint)))
        .collect();
    let mint_of = |index: u16| registered.iter().find(|(_, asset)| asset.asset_index == index).expect("mint of an active asset").clone();
    let mut groups = Vec::new();
    for index in margin.active_collateral_indexes() {
        let (mint, asset) = mint_of(index);
        groups.extend(match asset.is_venue() {
            true => vec![
                AccountMeta::new_readonly(asset_config_pda(&mint).0, false),
                AccountMeta::new_readonly(vanna_credit_layer::interface::venue_account_address(&margin_key, &mint).0, false),
            ],
            false => collateral_group_with(&mint, &asset.token_program, &margin_key),
        });
    }
    for index in margin.active_debt_indexes() {
        groups.extend(debt_group_metas(&mint_of(index).0, &margin_key));
    }
    groups
}

pub fn collateral_group_metas(mint: &Pubkey, margin: &Pubkey) -> Vec<AccountMeta> {
    collateral_group_with(mint, &anchor_spl::token::ID, margin)
}

pub fn collateral_group_with(mint: &Pubkey, token_program: &Pubkey, margin: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(asset_config_pda(mint).0, false),
        AccountMeta::new_readonly(margin_vault_ata_with(margin, mint, token_program), false),
    ]
}

pub fn debt_group_metas(mint: &Pubkey, margin: &Pubkey) -> Vec<AccountMeta> {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    vec![
        AccountMeta::new_readonly(asset_config, false),
        AccountMeta::new_readonly(reserve, false),
        AccountMeta::new_readonly(debt_position, false),
    ]
}

pub fn with_oracles(mut groups: Vec<AccountMeta>, oracles: &[Pubkey]) -> Vec<AccountMeta> {
    groups.extend(oracle_segment(oracles));
    groups
}

pub fn agent_segment(program: &Pubkey, accounts: &[Pubkey]) -> Vec<AccountMeta> {
    let mut metas = vec![AccountMeta::new_readonly(*program, false)];
    metas.extend(oracle_metas(accounts));
    metas
}

pub fn oracle_segment(oracles: &[Pubkey]) -> Vec<AccountMeta> {
    let accounts: Vec<Pubkey> = std::iter::once(price_book()).chain(oracles.iter().copied()).collect();
    agent_segment(&ORACLE, &accounts)
}

fn fetch<T: AccountDeserialize>(svm: &LiteSVM, pubkey: &Pubkey) -> T {
    let data = svm.get_account(pubkey).expect("account missing").data;
    T::try_deserialize(&mut data.as_slice()).unwrap()
}

pub fn fetch_protocol_config(svm: &LiteSVM) -> ProtocolConfig {
    fetch(svm, &protocol_config_pda().0)
}

pub fn fetch_asset_config(svm: &LiteSVM, mint: &Pubkey) -> AssetConfig {
    fetch(svm, &asset_config_pda(mint).0)
}

pub fn fetch_reserve(svm: &LiteSVM, mint: &Pubkey) -> Reserve {
    fetch(svm, &reserve_pda(mint).0)
}

pub fn fetch_integration(svm: &LiteSVM, program_id: &Pubkey) -> Integration {
    fetch(svm, &integration_pda(program_id))
}

pub fn fetch_margin(svm: &LiteSVM, authority: &Pubkey) -> MarginAccount {
    fetch(svm, &margin_pda(authority).0)
}

pub fn fetch_debt_position(svm: &LiteSVM, margin: &Pubkey, mint: &Pubkey) -> DebtPosition {
    fetch(svm, &debt_position_pda(margin, &reserve_pda(mint).0).0)
}
