#![allow(dead_code)]

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
use vanna_lending::constants::*;
use vanna_lending::state::{AssetConfig, ProtocolConfig, Reserve};

pub use anchor_spl::associated_token::get_associated_token_address;
pub use vanna_lending::state::reserve::RateCurve;

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

pub const USDC_DECIMALS: u8 = 6;
pub const WSOL_DECIMALS: u8 = 9;

pub const USDC_FEED: [u8; 32] = [1u8; 32];
pub const WSOL_FEED: [u8; 32] = [2u8; 32];
/// $1.00 at exponent -8.
pub const USDC_PRICE: i64 = 100_000_000;
/// $200.00 at exponent -8.
pub const WSOL_PRICE: i64 = 20_000_000_000;

/// 0.1 / 0.3 / 3.5 — ~17.5% borrow APR at 50% utilization, rising steeply toward 175% at 100%.
pub const DEFAULT_RATE_CURVE: RateCurve = RateCurve {
    linear_coeff_wad: 100_000_000_000_000_000,
    jump_coeff_wad: 300_000_000_000_000_000,
    rate_multiplier_wad: 3_500_000_000_000_000_000,
};

// ---------------------------------------------------------------------------
// SVM setup
// ---------------------------------------------------------------------------

pub fn program_bytes() -> &'static [u8] {
    include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/vanna_lending.so"))
}

pub fn setup_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program(vanna_lending::ID, program_bytes()).unwrap();
    svm
}

pub fn funded_keypair(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
    kp
}

pub fn send(svm: &mut LiteSVM, payer: &Keypair, ixs: &[Instruction], extra_signers: &[&Keypair]) -> TransactionResult {
    // Fresh blockhash per call so identical back-to-back transactions aren't rejected as `AlreadyProcessed`.
    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
    // Dedupe signers: `try_new` rejects the same keypair (e.g. admin as payer and mint authority) twice.
    let mut signers: Vec<&Keypair> = vec![payer];
    for candidate in extra_signers {
        if !signers.iter().any(|s| s.pubkey() == candidate.pubkey()) {
            signers.push(candidate);
        }
    }
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers).unwrap();
    svm.send_transaction(tx)
}

/// Advances the clock's `unix_timestamp` only — enough for interest accrual without a slot warp.
pub fn advance_time(svm: &mut LiteSVM, seconds: i64) {
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp += seconds;
    svm.set_sysvar(&clock);
}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

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

/// Creates `owner`'s ATA for `mint` if needed and mints `amount` into it (0 just creates the ATA).
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
    spl_token_interface::state::Account::unpack(&acc.data).unwrap().amount
}

// ---------------------------------------------------------------------------
// Oracle helpers
// ---------------------------------------------------------------------------

/// Writes a Pyth-receiver-owned `PriceUpdateV2` account directly into the SVM (the protocol never
/// CPIs into the receiver, so its program doesn't need to be loaded).
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

// ---------------------------------------------------------------------------
// PDAs
// ---------------------------------------------------------------------------

pub fn protocol_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PROTOCOL_SEED], &vanna_lending::ID)
}

pub fn asset_config_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ASSET_SEED, mint.as_ref()], &vanna_lending::ID)
}

pub fn reserve_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[RESERVE_SEED, mint.as_ref()], &vanna_lending::ID)
}

pub fn share_mint_pda(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SHARE_MINT_SEED, mint.as_ref()], &vanna_lending::ID)
}

/// One margin account per wallet, seeded only by its authority.
pub fn margin_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MARGIN_SEED, authority.as_ref()], &vanna_lending::ID)
}

/// The margin vault is a plain ATA; its live SPL balance is the credited collateral.
pub fn margin_vault_ata(margin: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(margin, mint)
}

pub fn debt_position_pda(margin: &Pubkey, reserve: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[DEBT_SEED, margin.as_ref(), reserve.as_ref()], &vanna_lending::ID)
}

/// The margin's base lite-position PDA, which risk-checked instructions take as a trailing account.
pub fn lite_position_pda(margin: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[LITE_POSITION_SEED, margin.as_ref()], &vanna_lending::ID).0
}

// ---------------------------------------------------------------------------
// Instruction builders
// ---------------------------------------------------------------------------

pub fn ix_initialize_protocol(admin: &Pubkey, treasury: &Pubkey, payer: &Pubkey, max_assets_per_margin: u8) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::InitializeProtocol {
            payer: *payer,
            admin: *admin,
            protocol_config,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::InitializeProtocol {
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
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminRegisterAsset {
            admin: *admin,
            payer: *payer,
            protocol_config,
            underlying_mint: *mint,
            asset_config,
            token_program: anchor_spl::token::ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::AdminRegisterAsset {
            price_feed_id,
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            max_confidence_bps,
            max_price_age_secs,
            collateral_enabled,
            borrow_enabled,
        }
        .data(),
    }
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminInitializeReserve {
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
        data: vanna_lending::instruction::AdminInitializeReserve {
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminProposeAuthority { admin: *admin, protocol_config }.to_account_metas(None),
        data: vanna_lending::instruction::AdminProposeAuthority { new_admin: *new_admin }.data(),
    }
}

pub fn ix_authority_accept_admin(pending_admin: &Pubkey) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AuthorityAcceptAdmin { pending_admin: *pending_admin, protocol_config }
            .to_account_metas(None),
        data: vanna_lending::instruction::AuthorityAcceptAdmin {}.data(),
    }
}

pub fn ix_admin_set_operating_mode(admin: &Pubkey, new_mode: u8) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminSetOperatingMode { admin: *admin, protocol_config }.to_account_metas(None),
        data: vanna_lending::instruction::AdminSetOperatingMode { new_mode }.data(),
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
    max_confidence_bps: u16,
    max_price_age_secs: u32,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminUpdateAssetConfig { admin: *admin, protocol_config, asset_config }
            .to_account_metas(None),
        data: vanna_lending::instruction::AdminUpdateAssetConfig {
            max_collateral_per_margin,
            ltv_bps,
            liquidation_threshold_bps,
            liquidation_bonus_bps,
            max_confidence_bps,
            max_price_age_secs,
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminUpdateReserveConfig {
            admin: *admin,
            protocol_config,
            underlying_mint: *mint,
            reserve,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::AdminUpdateReserveConfig {
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::AdminCollectProtocolFees {
            admin: *admin,
            protocol_config,
            underlying_mint: *mint,
            reserve,
            liquidity_vault,
            treasury_ata,
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::AdminCollectProtocolFees { amount }.data(),
    }
}

pub fn ix_public_refresh_reserve(mint: &Pubkey) -> Instruction {
    let (reserve, _) = reserve_pda(mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::PublicRefreshReserve { reserve }.to_account_metas(None),
        data: vanna_lending::instruction::PublicRefreshReserve {}.data(),
    }
}

pub fn ix_user_close_collateral_position(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata(margin, mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserCloseCollateralPosition {
            authority: *authority,
            margin_account: *margin,
            asset_config,
            mint: *mint,
            margin_vault,
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::UserCloseCollateralPosition {}.data(),
    }
}

pub fn ix_user_close_debt_position(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserCloseDebtPosition {
            authority: *authority,
            margin_account: *margin,
            asset_config,
            reserve,
            debt_position,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::UserCloseDebtPosition {}.data(),
    }
}

pub fn ix_user_close_margin(authority: &Pubkey, margin: &Pubkey) -> Instruction {
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserCloseMargin { authority: *authority, margin_account: *margin }
            .to_account_metas(None),
        data: vanna_lending::instruction::UserCloseMargin {}.data(),
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::LenderSupply {
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
        data: vanna_lending::instruction::LenderSupply { assets, min_shares_out }.data(),
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
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::LenderRedeem {
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
        data: vanna_lending::instruction::LenderRedeem { shares, min_assets_out }.data(),
    }
}

pub fn ix_user_create_margin(authority: &Pubkey, payer: &Pubkey) -> Instruction {
    let (margin_account, _) = margin_pda(authority);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserCreateMargin {
            authority: *authority,
            payer: *payer,
            margin_account,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::UserCreateMargin {}.data(),
    }
}

/// Creates the margin vault ATA on first use (`init_if_needed`).
pub fn ix_user_deposit_collateral(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, amount: u64) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata(margin, mint);
    let source_token_account = get_associated_token_address(authority, mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserDepositCollateral {
            authority: *authority,
            protocol_config,
            margin_account: *margin,
            asset_config,
            mint: *mint,
            source_token_account,
            margin_vault,
            token_program: anchor_spl::token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::UserDepositCollateral { amount }.data(),
    }
}

pub fn ix_user_open_debt_position(authority: &Pubkey, payer: &Pubkey, margin: &Pubkey, mint: &Pubkey) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserOpenDebtPosition {
            authority: *authority,
            payer: *payer,
            margin_account: *margin,
            asset_config,
            reserve,
            debt_position,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_lending::instruction::UserOpenDebtPosition {}.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_user_borrow(
    authority: &Pubkey,
    margin: &Pubkey,
    mint: &Pubkey,
    price_update: &Pubkey,
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
    let mut accounts = vanna_lending::accounts::UserBorrow {
        authority: *authority,
        protocol_config,
        margin_account: *margin,
        asset_config,
        reserve,
        debt_position,
        price_update: *price_update,
        mint: *mint,
        reserve_vault,
        margin_vault,
        token_program: anchor_spl::token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    accounts.push(AccountMeta::new_readonly(lite_position_pda(margin), false));
    Instruction {
        program_id: vanna_lending::ID,
        accounts,
        data: vanna_lending::instruction::UserBorrow { assets, max_debt_shares }.data(),
    }
}

pub fn ix_user_repay_from_margin(authority: &Pubkey, margin: &Pubkey, mint: &Pubkey, max_assets: u64, repay_all: bool) -> Instruction {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    let margin_vault = margin_vault_ata(margin, mint);
    let reserve_vault = get_associated_token_address(&reserve, mint);
    Instruction {
        program_id: vanna_lending::ID,
        accounts: vanna_lending::accounts::UserRepayFromMargin {
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
        data: vanna_lending::instruction::UserRepayFromMargin { max_assets, repay_all }.data(),
    }
}

pub fn ix_user_withdraw_collateral(
    authority: &Pubkey,
    margin: &Pubkey,
    mint: &Pubkey,
    price_update: &Pubkey,
    amount: u64,
    min_health_factor_wad: u128,
    remaining: &[AccountMeta],
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata(margin, mint);
    let destination_token_account = get_associated_token_address(authority, mint);
    let mut accounts = vanna_lending::accounts::UserWithdrawCollateral {
        authority: *authority,
        protocol_config,
        margin_account: *margin,
        asset_config,
        mint: *mint,
        price_update: *price_update,
        destination_token_account,
        margin_vault,
        token_program: anchor_spl::token::ID,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    accounts.push(AccountMeta::new_readonly(lite_position_pda(margin), false));
    Instruction {
        program_id: vanna_lending::ID,
        accounts,
        data: vanna_lending::instruction::UserWithdrawCollateral { amount, min_health_factor_wad }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_public_liquidate(
    liquidator: &Pubkey,
    margin: &Pubkey,
    debt_mint: &Pubkey,
    debt_price_update: &Pubkey,
    collateral_mint: &Pubkey,
    collateral_price_update: &Pubkey,
    max_repay_assets: u64,
    min_collateral_out: u64,
    remaining: &[AccountMeta],
) -> Instruction {
    let (debt_asset_config, _) = asset_config_pda(debt_mint);
    let (debt_reserve, _) = reserve_pda(debt_mint);
    let (debt_position, _) = debt_position_pda(margin, &debt_reserve);
    let (collateral_asset_config, _) = asset_config_pda(collateral_mint);
    let liquidator_debt_source = get_associated_token_address(liquidator, debt_mint);
    let debt_reserve_vault = get_associated_token_address(&debt_reserve, debt_mint);
    let liquidator_collateral_destination = get_associated_token_address(liquidator, collateral_mint);
    let collateral_margin_vault = margin_vault_ata(margin, collateral_mint);
    let mut accounts = vanna_lending::accounts::PublicLiquidate {
        liquidator: *liquidator,
        margin_account: *margin,
        debt_asset_config,
        debt_reserve,
        debt_position,
        debt_price_update: *debt_price_update,
        debt_mint: *debt_mint,
        liquidator_debt_source,
        debt_reserve_vault,
        collateral_asset_config,
        collateral_price_update: *collateral_price_update,
        collateral_mint: *collateral_mint,
        liquidator_collateral_destination,
        collateral_margin_vault,
        token_program: anchor_spl::token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    accounts.push(AccountMeta::new_readonly(lite_position_pda(margin), false));
    Instruction {
        program_id: vanna_lending::ID,
        accounts,
        data: vanna_lending::instruction::PublicLiquidate { max_repay_assets, min_collateral_out }.data(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ix_user_deposit_and_borrow(
    authority: &Pubkey,
    deposit_mint: &Pubkey,
    deposit_price_update: &Pubkey,
    borrow_mint: &Pubkey,
    borrow_price_update: &Pubkey,
    deposit_amount: u64,
    borrow_amount: u64,
    max_debt_shares: u128,
    remaining: &[AccountMeta],
) -> Instruction {
    let (protocol_config, _) = protocol_config_pda();
    let (margin_account, _) = margin_pda(authority);
    let (deposit_asset_config, _) = asset_config_pda(deposit_mint);
    let deposit_source_token_account = get_associated_token_address(authority, deposit_mint);
    let deposit_margin_vault = margin_vault_ata(&margin_account, deposit_mint);
    let (borrow_asset_config, _) = asset_config_pda(borrow_mint);
    let (borrow_reserve, _) = reserve_pda(borrow_mint);
    let (debt_position, _) = debt_position_pda(&margin_account, &borrow_reserve);
    let borrow_reserve_vault = get_associated_token_address(&borrow_reserve, borrow_mint);
    let borrow_margin_vault = margin_vault_ata(&margin_account, borrow_mint);
    let mut accounts = vanna_lending::accounts::UserDepositAndBorrow {
        authority: *authority,
        protocol_config,
        margin_account,
        deposit_asset_config,
        deposit_mint: *deposit_mint,
        deposit_price_update: *deposit_price_update,
        deposit_source_token_account,
        deposit_margin_vault,
        borrow_asset_config,
        borrow_reserve,
        debt_position,
        borrow_price_update: *borrow_price_update,
        borrow_mint: *borrow_mint,
        borrow_reserve_vault,
        borrow_margin_vault,
        token_program: anchor_spl::token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(remaining);
    accounts.push(AccountMeta::new_readonly(lite_position_pda(&margin_account), false));
    Instruction {
        program_id: vanna_lending::ID,
        accounts,
        data: vanna_lending::instruction::UserDepositAndBorrow { deposit_amount, borrow_amount, max_debt_shares }.data(),
    }
}

// ---------------------------------------------------------------------------
// remaining_accounts builders
// ---------------------------------------------------------------------------

/// One collateral group: `AssetConfig`, margin vault, `PriceUpdateV2` (all read-only).
pub fn collateral_group_metas(mint: &Pubkey, margin: &Pubkey, price_update: &Pubkey) -> Vec<AccountMeta> {
    let (asset_config, _) = asset_config_pda(mint);
    let margin_vault = margin_vault_ata(margin, mint);
    vec![
        AccountMeta::new_readonly(asset_config, false),
        AccountMeta::new_readonly(margin_vault, false),
        AccountMeta::new_readonly(*price_update, false),
    ]
}

/// One debt group: `AssetConfig`, `Reserve`, `DebtPosition`, `PriceUpdateV2` (all read-only).
pub fn debt_group_metas(mint: &Pubkey, margin: &Pubkey, price_update: &Pubkey) -> Vec<AccountMeta> {
    let (asset_config, _) = asset_config_pda(mint);
    let (reserve, _) = reserve_pda(mint);
    let (debt_position, _) = debt_position_pda(margin, &reserve);
    vec![
        AccountMeta::new_readonly(asset_config, false),
        AccountMeta::new_readonly(reserve, false),
        AccountMeta::new_readonly(debt_position, false),
        AccountMeta::new_readonly(*price_update, false),
    ]
}

// ---------------------------------------------------------------------------
// Account fetchers
// ---------------------------------------------------------------------------

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
