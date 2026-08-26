use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::instructions::borrowing::apply_accrual;
use crate::math::health::{calculate_health, normalize_token_value, CollateralValuation, DebtValuation};
use crate::math::shares::{assets_to_debt_shares_up, debt_shares_to_assets_up};
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::state::reserve::Reserve;
use crate::validation::accounts::{
    assert_protocol_action_allowed, assert_reserve_action_allowed, validate_asset_config, ProtocolAction,
};
use crate::validation::positions::scan_and_validate_positions;
use crate::validation::token::{transfer_in_measured, transfer_out_checked_measured, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

// ---------------------------------------------------------------------------
// user_deposit_and_borrow
// ---------------------------------------------------------------------------
//
// Combined deposit-collateral + open-debt-position (if needed) + borrow, in one signed
// transaction — the Solana equivalent of the Stellar/Soroban sibling contract's
// `deposit_and_borrow_cross`. It also creates the margin account itself on first use, mirroring
// that contract auto-creating an account on a user's first leveraged deposit.
//
// Deliberately CROSS-ASSET ONLY (`deposit_mint != borrow_mint`): with only two supported assets,
// same-asset "loop" leverage would require the deposit-side and borrow-side account slots
// (`asset_config`, margin vault, price update) to alias the exact same underlying account under
// two different Anchor field names. Anchor deserializes each field into its own independent
// in-memory copy, so writing both back at the end of the handler would have the second write
// silently clobber the first — a real correctness bug, not just a redundant check. A same-asset
// leveraged loop is still reachable, just as two separate transactions: `user_open_debt_position`
// (if needed) + `user_borrow`, exactly as before this instruction existed.
#[derive(Accounts)]
pub struct UserDepositAndBorrow<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    /// Created on first use (mirrors `user_create_margin`) — ownership of an already-existing
    /// account is checked by hand in the handler body, since `has_one` can't be combined with
    /// `init_if_needed` here (on the fresh-init path there is no owner yet to check against).
    #[account(
        init_if_needed,
        payer = authority,
        space = 8 + MarginAccount::INIT_SPACE,
        seeds = [MARGIN_SEED, authority.key().as_ref()],
        bump
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,

    // --- Deposit (collateral) side ---
    #[account(seeds = [ASSET_SEED, deposit_mint.key().as_ref()], bump = deposit_asset_config.bump)]
    pub deposit_asset_config: Box<Account<'info, AssetConfig>>,
    pub deposit_mint: Box<Account<'info, Mint>>,
    /// Pyth price update for the deposited asset.
    pub deposit_price_update: Box<Account<'info, PriceUpdateV2>>,
    #[account(mut, token::mint = deposit_mint, token::authority = authority)]
    pub deposit_source_token_account: Box<Account<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = authority,
        associated_token::mint = deposit_mint,
        associated_token::authority = margin_account
    )]
    pub deposit_margin_vault: Box<Account<'info, TokenAccount>>,

    // --- Borrow side ---
    #[account(seeds = [ASSET_SEED, borrow_mint.key().as_ref()], bump = borrow_asset_config.bump)]
    pub borrow_asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, borrow_mint.key().as_ref()], bump = borrow_reserve.bump)]
    pub borrow_reserve: Box<Account<'info, Reserve>>,
    /// Opened on first borrow of this asset (mirrors `user_open_debt_position`).
    #[account(
        init_if_needed,
        payer = authority,
        space = 8 + DebtPosition::INIT_SPACE,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), borrow_reserve.key().as_ref()],
        bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    /// Pyth price update for the borrowed asset.
    pub borrow_price_update: Box<Account<'info, PriceUpdateV2>>,
    pub borrow_mint: Box<Account<'info, Mint>>,
    #[account(mut, token::mint = borrow_mint, token::authority = borrow_reserve)]
    pub borrow_reserve_vault: Box<Account<'info, TokenAccount>>,
    /// Borrowed funds land here as protocol-controlled collateral credit (spec §1.2), same as
    /// plain `user_borrow`.
    #[account(
        init_if_needed,
        payer = authority,
        associated_token::mint = borrow_mint,
        associated_token::authority = margin_account
    )]
    pub borrow_margin_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn user_deposit_and_borrow(
    ctx: Context<UserDepositAndBorrow>,
    deposit_amount: u64,
    borrow_amount: u64,
    max_debt_shares: u128,
) -> Result<()> {
    require!(
        ctx.accounts.deposit_mint.key() != ctx.accounts.borrow_mint.key(),
        VannaError::DuplicateAccount
    );
    require!(deposit_amount > 0, VannaError::ZeroAmount);
    require!(borrow_amount > 0, VannaError::ZeroAmount);

    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::CollateralDeposit)?;
    assert_protocol_action_allowed(ctx.accounts.protocol_config.operating_mode, ProtocolAction::Borrow)?;
    assert_reserve_action_allowed(ctx.accounts.borrow_reserve.status, ProtocolAction::Borrow)?;

    validate_asset_config(
        &ctx.accounts.deposit_asset_config,
        &ctx.accounts.deposit_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    validate_asset_config(
        &ctx.accounts.borrow_asset_config,
        &ctx.accounts.borrow_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require!(ctx.accounts.deposit_asset_config.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    require!(ctx.accounts.borrow_asset_config.borrow_enabled, VannaError::AssetNotBorrowEnabled);

    verify_associated_token_account(
        &ctx.accounts.deposit_margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.deposit_mint.key(),
    )?;
    verify_associated_token_account(
        &ctx.accounts.borrow_margin_vault.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.borrow_mint.key(),
    )?;

    let clock = Clock::get()?;

    // `init_if_needed` zero-initializes a fresh account before the handler runs — `authority`
    // would be the all-zero Pubkey, which no real wallet ever holds, so this is a reliable
    // "freshly created, not yet owned" signal (mirrors `user_create_margin`'s own construction).
    if ctx.accounts.margin_account.authority == Pubkey::default() {
        let bump = ctx.bumps.margin_account;
        ctx.accounts
            .margin_account
            .set_inner(MarginAccount::new_empty(ctx.accounts.authority.key(), bump));
    } else {
        require_keys_eq!(ctx.accounts.margin_account.authority, ctx.accounts.authority.key(), VannaError::Unauthorized);
    }

    // Same freshness signal for the debt position: a real position's `reserve` is never the
    // default Pubkey (mirrors `user_open_debt_position`'s own construction).
    if ctx.accounts.debt_position.reserve == Pubkey::default() {
        ctx.accounts.debt_position.margin_account = ctx.accounts.margin_account.key();
        ctx.accounts.debt_position.reserve = ctx.accounts.borrow_reserve.key();
        ctx.accounts.debt_position.borrow_shares = 0;
        ctx.accounts.debt_position.bump = ctx.bumps.debt_position;
        ctx.accounts.debt_position.reserved = [0u8; 48];
        emit!(DebtPositionOpened {
            margin_account: ctx.accounts.margin_account.key(),
            reserve: ctx.accounts.borrow_reserve.key(),
            timestamp: clock.unix_timestamp,
        });
    }

    // ---- Deposit leg (mirrors user_deposit_collateral) ----
    let deposit_was_zero = ctx.accounts.deposit_margin_vault.amount == 0;
    let deposit_received = transfer_in_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.deposit_mint,
        &ctx.accounts.deposit_source_token_account,
        &mut ctx.accounts.deposit_margin_vault,
        &ctx.accounts.authority,
        deposit_amount,
    )?;

    if deposit_was_zero {
        let active_count = ctx.accounts.margin_account.collateral_count + ctx.accounts.margin_account.debt_count;
        require!(
            (active_count as u16) < ctx.accounts.protocol_config.max_assets_per_margin as u16,
            VannaError::TooManyAssets
        );
        ctx.accounts
            .margin_account
            .add_active_collateral(ctx.accounts.deposit_asset_config.asset_index)?;
    }
    let deposit_cap = ctx.accounts.deposit_asset_config.max_collateral_per_margin;
    require!(
        deposit_cap == UNCAPPED || ctx.accounts.deposit_margin_vault.amount <= deposit_cap,
        VannaError::CollateralCapExceeded
    );

    let deposit_event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(CollateralDeposited {
        margin_account: ctx.accounts.margin_account.key(),
        mint: ctx.accounts.deposit_mint.key(),
        amount: deposit_received,
        new_collateral_amount: ctx.accounts.deposit_margin_vault.amount,
        event_sequence: deposit_event_sequence,
        timestamp: clock.unix_timestamp,
    });

    // ---- Borrow leg (mirrors user_borrow) ----
    apply_accrual(&mut ctx.accounts.borrow_reserve, clock.unix_timestamp)?;

    require!(
        ctx.accounts.borrow_reserve.borrow_cap == UNCAPPED
            || ctx
                .accounts
                .borrow_reserve
                .total_borrow_assets
                .checked_add(borrow_amount)
                .ok_or(VannaError::MathOverflow)?
                <= ctx.accounts.borrow_reserve.borrow_cap,
        VannaError::BorrowCapExceeded
    );
    require!(
        borrow_amount <= ctx.accounts.borrow_reserve.accounted_liquidity_assets,
        VannaError::InsufficientLiquidity
    );

    let new_debt_shares = assets_to_debt_shares_up(
        borrow_amount,
        ctx.accounts.borrow_reserve.total_borrow_shares,
        ctx.accounts.borrow_reserve.total_borrow_assets,
    )?;
    require!(new_debt_shares <= max_debt_shares, VannaError::SlippageExceeded);

    // Every OTHER active collateral/debt this margin account holds — the deposit asset's own
    // collateral slot and the borrow asset's own debt slot are excluded here because they're
    // handled explicitly below via the named accounts, exactly like `user_borrow` excludes its
    // single named asset from this same scan.
    let margin_key = ctx.accounts.margin_account.key();
    let (scanned_collaterals, other_debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        Some(ctx.accounts.deposit_asset_config.asset_index),
        Some(ctx.accounts.borrow_asset_config.asset_index),
    )?;
    let mut collaterals: Vec<CollateralValuation> = scanned_collaterals.into_iter().map(|c| c.valuation).collect();
    let mut debts: Vec<DebtValuation> = other_debts.into_iter().map(|d| d.valuation).collect();

    // Deposit asset's own projected collateral value — `deposit_margin_vault.amount` already
    // reflects the transfer above, so no further projection is needed on top of it.
    let deposit_price = load_validated_price(&ctx.accounts.deposit_asset_config, &ctx.accounts.deposit_price_update, &clock)?;
    let deposit_collateral_value = normalize_token_value(
        ctx.accounts.deposit_margin_vault.amount,
        deposit_price.price,
        deposit_price.exponent,
        ctx.accounts.deposit_asset_config.decimals,
        false,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: deposit_collateral_value,
        ltv_bps: ctx.accounts.deposit_asset_config.ltv_bps,
        liquidation_threshold_bps: ctx.accounts.deposit_asset_config.liquidation_threshold_bps,
    });

    // Borrow asset's own projected debt value (existing + new shares, priced with its own feed).
    let projected_total_borrow_assets = ctx
        .accounts
        .borrow_reserve
        .total_borrow_assets
        .checked_add(borrow_amount)
        .ok_or(VannaError::MathOverflow)?;
    let projected_total_borrow_shares = ctx
        .accounts
        .borrow_reserve
        .total_borrow_shares
        .checked_add(new_debt_shares)
        .ok_or(VannaError::MathOverflow)?;
    let projected_position_shares = ctx
        .accounts
        .debt_position
        .borrow_shares
        .checked_add(new_debt_shares)
        .ok_or(VannaError::MathOverflow)?;
    let projected_debt_assets = debt_shares_to_assets_up(
        projected_position_shares,
        projected_total_borrow_shares,
        projected_total_borrow_assets,
    )?;
    let borrow_price = load_validated_price(&ctx.accounts.borrow_asset_config, &ctx.accounts.borrow_price_update, &clock)?;
    let borrow_debt_value = normalize_token_value(
        projected_debt_assets,
        borrow_price.price,
        borrow_price.exponent,
        ctx.accounts.borrow_asset_config.decimals,
        true,
    )?;
    debts.push(DebtValuation { debt_value: borrow_debt_value });

    // Borrowed funds also land as protocol-controlled collateral credit for the borrow asset
    // itself (spec §1.2, same as plain `user_borrow`) — always a genuinely separate credit from
    // the deposit-side collateral pushed above, since the two mints are required to differ.
    let was_zero_borrow_collateral = ctx.accounts.borrow_margin_vault.amount == 0;
    if ctx.accounts.borrow_asset_config.collateral_enabled {
        let projected_borrow_side_collateral = ctx
            .accounts
            .borrow_margin_vault
            .amount
            .checked_add(borrow_amount)
            .ok_or(VannaError::MathOverflow)?;
        let borrow_side_collateral_value = normalize_token_value(
            projected_borrow_side_collateral,
            borrow_price.price,
            borrow_price.exponent,
            ctx.accounts.borrow_asset_config.decimals,
            false,
        )?;
        collaterals.push(CollateralValuation {
            collateral_value: borrow_side_collateral_value,
            ltv_bps: ctx.accounts.borrow_asset_config.ltv_bps,
            liquidation_threshold_bps: ctx.accounts.borrow_asset_config.liquidation_threshold_bps,
        });
    }

    let health = calculate_health(&collaterals, &debts)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);

    // ---- Commit borrow-side state + token transfer ----
    ctx.accounts.borrow_reserve.accounted_liquidity_assets = ctx
        .accounts
        .borrow_reserve
        .accounted_liquidity_assets
        .checked_sub(borrow_amount)
        .ok_or(VannaError::MathUnderflow)?;
    ctx.accounts.borrow_reserve.total_borrow_assets = projected_total_borrow_assets;
    ctx.accounts.borrow_reserve.total_borrow_shares = projected_total_borrow_shares;

    let was_zero_debt = ctx.accounts.debt_position.borrow_shares == 0;
    ctx.accounts.debt_position.credit_shares(new_debt_shares)?;
    if was_zero_debt {
        let active_count = ctx.accounts.margin_account.collateral_count + ctx.accounts.margin_account.debt_count;
        require!(
            (active_count as u16) < ctx.accounts.protocol_config.max_assets_per_margin as u16,
            VannaError::TooManyAssets
        );
        ctx.accounts
            .margin_account
            .add_active_debt(ctx.accounts.borrow_asset_config.asset_index)?;
    }

    let borrow_mint_key = ctx.accounts.borrow_mint.key();
    let borrow_reserve_bump = ctx.accounts.borrow_reserve.bump;
    let reserve_signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, borrow_mint_key.as_ref(), &[borrow_reserve_bump]]];
    let borrow_reserve_account_info = ctx.accounts.borrow_reserve.to_account_info();
    let borrow_received = transfer_out_checked_measured(
        &ctx.accounts.token_program,
        &ctx.accounts.borrow_mint,
        &ctx.accounts.borrow_reserve_vault,
        &mut ctx.accounts.borrow_margin_vault,
        &borrow_reserve_account_info,
        reserve_signer_seeds,
        borrow_amount,
    )?;

    if was_zero_borrow_collateral
        && ctx.accounts.borrow_asset_config.collateral_enabled
        && !ctx
            .accounts
            .margin_account
            .is_collateral_active(ctx.accounts.borrow_asset_config.asset_index)
    {
        ctx.accounts
            .margin_account
            .add_active_collateral(ctx.accounts.borrow_asset_config.asset_index)?;
    }

    let borrow_event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(Borrowed {
        margin_account: ctx.accounts.margin_account.key(),
        reserve: ctx.accounts.borrow_reserve.key(),
        assets: borrow_received,
        debt_shares: new_debt_shares,
        total_debt_shares: ctx.accounts.debt_position.borrow_shares,
        borrow_health_factor_wad: health.borrow_health_factor_wad,
        event_sequence: borrow_event_sequence,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
