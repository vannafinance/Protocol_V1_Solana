//! Lite / Stocks strategy: leveraged same-asset carry into Kamino.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::external::kamino::{self, KaminoCpiAccounts};
use crate::instructions::borrowing::apply_accrual;
use crate::instructions::swap::{JUPITER, SWAP_SEED};
use crate::math::fixed_point::{mul_div_ceil, mul_div_floor};
use crate::math::health::{
    calculate_health, normalize_token_value, CollateralValuation, DebtValuation,
};
use crate::math::shares::{assets_to_debt_shares_up, debt_shares_to_assets_up};
use crate::oracle::pyth::load_validated_price;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::lite_strategy::{LitePosition, LiteStrategyConfig};
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::state::reserve::Reserve;
use crate::validation::accounts::{
    assert_protocol_action_allowed, assert_reserve_action_allowed, validate_asset_config,
    ProtocolAction,
};
use crate::validation::positions::scan_and_validate_positions;
use crate::validation::token::{
    transfer_out_checked, transfer_out_checked_measured, verify_associated_token_account,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Token, TokenAccount as SplTokenAccount};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// Kamino introspects the instructions sysvar during deposit/redeem.
const INSTRUCTIONS_SYSVAR_ID: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");

/// Leverage bounds: 1x (equity-only, no borrow) to 5x.
const MIN_LEVERAGE_BPS: u64 = 10_000;
const MAX_LEVERAGE_BPS: u64 = 50_000;

// ---------------------------------------------------------------------------
// admin_register_lite_strategy
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminRegisterLiteStrategy<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Kamino klend program id — stored and re-checked on open/close.
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: Kamino lending market.
    pub lending_market: UncheckedAccount<'info>,
    /// CHECK: PDA authority for the market (`["lma", market]` under klend).
    pub lending_market_authority: UncheckedAccount<'info>,
    /// CHECK: Kamino reserve for this underlying.
    pub kamino_reserve: UncheckedAccount<'info>,
    /// CHECK: Reserve liquidity supply vault.
    pub reserve_liquidity_supply: UncheckedAccount<'info>,
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + LiteStrategyConfig::INIT_SPACE,
        seeds = [LITE_STRATEGY_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub strategy_config: Box<Account<'info, LiteStrategyConfig>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub collateral_token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn admin_register_lite_strategy(ctx: Context<AdminRegisterLiteStrategy>) -> Result<()> {
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require_keys_neq!(
        ctx.accounts.kamino_program.key(),
        Pubkey::default(),
        VannaError::InvalidKaminoProgram
    );
    require_keys_neq!(
        ctx.accounts.kamino_reserve.key(),
        Pubkey::default(),
        VannaError::InvalidKaminoAccounts
    );
    require_keys_neq!(
        ctx.accounts.lending_market.key(),
        Pubkey::default(),
        VannaError::InvalidKaminoAccounts
    );

    let cfg = &mut ctx.accounts.strategy_config;
    cfg.underlying_mint = ctx.accounts.underlying_mint.key();
    cfg.asset_config = ctx.accounts.asset_config.key();
    cfg.kamino_program = ctx.accounts.kamino_program.key();
    cfg.lending_market = ctx.accounts.lending_market.key();
    cfg.lending_market_authority = ctx.accounts.lending_market_authority.key();
    cfg.kamino_reserve = ctx.accounts.kamino_reserve.key();
    cfg.reserve_liquidity_supply = ctx.accounts.reserve_liquidity_supply.key();
    cfg.reserve_collateral_mint = ctx.accounts.reserve_collateral_mint.key();
    cfg.liquidity_token_program = ctx.accounts.token_program.key();
    cfg.collateral_token_program = ctx.accounts.collateral_token_program.key();
    cfg.enabled = true;
    cfg.bump = ctx.bumps.strategy_config;
    cfg.reserved = [0u8; 64];

    emit!(LiteStrategyRegistered {
        strategy_config: cfg.key(),
        underlying_mint: cfg.underlying_mint,
        kamino_reserve: cfg.kamino_reserve,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lite_open
// ---------------------------------------------------------------------------

/// Final `lite_position` seed: empty for the legacy (first) PDA, the asset index for others.
/// `#[inline(never)]` keeps `find_program_address` out of the stack-heavy `try_accounts` frame.
#[inline(never)]
pub fn position_seed(position: &Pubkey, margin: &Pubkey, asset_index: u16) -> Vec<u8> {
    let legacy = Pubkey::find_program_address(&[LITE_POSITION_SEED, margin.as_ref()], &crate::ID).0;
    if *position == legacy { Vec::new() } else { asset_index.to_le_bytes().to_vec() }
}

#[derive(Accounts)]
pub struct LiteOpen<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = underlying_mint,
        token::authority = owner,
        token::token_program = token_program
    )]
    pub user_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = underlying_mint,
        token::authority = reserve,
        token::token_program = token_program
    )]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        constraint = margin_account.authority == owner.key() @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + DebtPosition::INIT_SPACE,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    #[account(
        seeds = [LITE_STRATEGY_SEED, underlying_mint.key().as_ref()],
        bump = lite_strategy.bump,
        constraint = lite_strategy.enabled @ VannaError::LiteStrategyDisabled
    )]
    pub lite_strategy: Box<Account<'info, LiteStrategyConfig>>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + LitePosition::INIT_SPACE,
        seeds = [LITE_POSITION_SEED, margin_account.key().as_ref(), &position_seed(&lite_position.key(), &margin_account.key(), asset_config.asset_index)],
        bump
    )]
    pub lite_position: Box<Account<'info, LitePosition>>,
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    /// CHECK: must match strategy_config.kamino_program.
    #[account(constraint = kamino_program.key() == lite_strategy.kamino_program @ VannaError::InvalidKaminoProgram)]
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: lending market.
    #[account(constraint = lending_market.key() == lite_strategy.lending_market @ VannaError::InvalidKaminoAccounts)]
    pub lending_market: UncheckedAccount<'info>,
    /// CHECK: market authority.
    #[account(constraint = lending_market_authority.key() == lite_strategy.lending_market_authority @ VannaError::InvalidKaminoAccounts)]
    pub lending_market_authority: UncheckedAccount<'info>,
    /// CHECK: kamino reserve.
    #[account(mut, constraint = kamino_reserve.key() == lite_strategy.kamino_reserve @ VannaError::InvalidKaminoAccounts)]
    pub kamino_reserve: UncheckedAccount<'info>,
    /// CHECK: reserve liquidity supply.
    #[account(mut, constraint = reserve_liquidity_supply.key() == lite_strategy.reserve_liquidity_supply @ VannaError::InvalidKaminoAccounts)]
    pub reserve_liquidity_supply: UncheckedAccount<'info>,
    #[account(
        mut,
        constraint = reserve_collateral_mint.key() == lite_strategy.reserve_collateral_mint @ VannaError::InvalidKaminoAccounts
    )]
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = reserve_collateral_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = collateral_token_program,
    )]
    pub user_destination_collateral: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: instructions sysvar.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instruction_sysvar_account: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub collateral_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn lite_open(ctx: Context<LiteOpen>, equity: u64, leverage_bps: u64) -> Result<()> {
    require!(equity > 0, VannaError::ZeroAmount);
    require!(
        (MIN_LEVERAGE_BPS..=MAX_LEVERAGE_BPS).contains(&leverage_bps),
        VannaError::InvalidLeverage
    );
    assert_protocol_action_allowed(
        ctx.accounts.protocol_config.operating_mode,
        ProtocolAction::Borrow,
    )?;
    assert_reserve_action_allowed(ctx.accounts.reserve.status, ProtocolAction::Borrow)?;
    require!(
        ctx.accounts.asset_config.borrow_enabled,
        VannaError::AssetNotBorrowEnabled
    );
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require_keys_eq!(
        ctx.accounts.lite_strategy.liquidity_token_program,
        ctx.accounts.token_program.key(),
        VannaError::InvalidTokenProgram
    );

    let borrow_amount = if leverage_bps == MIN_LEVERAGE_BPS {
        0u64
    } else {
        let total = mul_div_floor(
            equity as u128,
            leverage_bps as u128,
            MIN_LEVERAGE_BPS as u128,
        )?;
        let total_u64 = u64::try_from(total).map_err(|_| VannaError::MathOverflow)?;
        total_u64
            .checked_sub(equity)
            .ok_or(VannaError::MathUnderflow)?
    };
    let deposit_amount = equity
        .checked_add(borrow_amount)
        .ok_or(VannaError::MathOverflow)?;

    let existing = &ctx.accounts.lite_position;
    if existing.margin_account != Pubkey::default() {
        require_keys_eq!(
            existing.margin_account,
            ctx.accounts.margin_account.key(),
            VannaError::NoLitePosition
        );
        require_keys_eq!(
            existing.strategy_config,
            ctx.accounts.lite_strategy.key(),
            VannaError::NoLitePosition
        );
        require_keys_eq!(
            existing.underlying_mint,
            ctx.accounts.underlying_mint.key(),
            VannaError::NoLitePosition
        );
    }
    let previous_shares = existing.debt_shares(ctx.accounts.debt_position.borrow_shares);
    let legacy_debt =
        existing.reserved[17] == 1 || (existing.reserved[16] != 1 && previous_shares > 0);
    let clock = Clock::get()?;

    if ctx.accounts.debt_position.reserve == Pubkey::default() {
        ctx.accounts.debt_position.margin_account = ctx.accounts.margin_account.key();
        ctx.accounts.debt_position.reserve = ctx.accounts.reserve.key();
        ctx.accounts.debt_position.borrow_shares = 0;
        ctx.accounts.debt_position.bump = ctx.bumps.debt_position;
        ctx.accounts.debt_position.reserved = [0u8; 48];
        emit!(DebtPositionOpened {
            margin_account: ctx.accounts.margin_account.key(),
            reserve: ctx.accounts.reserve.key(),
            timestamp: clock.unix_timestamp,
        });
    }

    apply_accrual(&mut ctx.accounts.reserve, clock.unix_timestamp)?;

    let mut new_debt_shares = 0u128;
    if borrow_amount > 0 {
        require!(
            ctx.accounts.reserve.borrow_cap == UNCAPPED
                || ctx
                    .accounts
                    .reserve
                    .total_borrow_assets
                    .checked_add(borrow_amount)
                    .ok_or(VannaError::MathOverflow)?
                    <= ctx.accounts.reserve.borrow_cap,
            VannaError::BorrowCapExceeded
        );
        require!(
            borrow_amount <= ctx.accounts.reserve.accounted_liquidity_assets,
            VannaError::InsufficientLiquidity
        );
        new_debt_shares = assets_to_debt_shares_up(
            borrow_amount,
            ctx.accounts.reserve.total_borrow_shares,
            ctx.accounts.reserve.total_borrow_assets,
        )?;

        let mint_key = ctx.accounts.underlying_mint.key();
        let reserve_bump = ctx.accounts.reserve.bump;
        let reserve_signer_seeds: &[&[&[u8]]] =
            &[&[RESERVE_SEED, mint_key.as_ref(), &[reserve_bump]]];
        let reserve_info = ctx.accounts.reserve.to_account_info();
        let received = transfer_out_checked_measured(
            &ctx.accounts.token_program,
            &ctx.accounts.underlying_mint,
            &ctx.accounts.liquidity_vault,
            &mut ctx.accounts.user_token_account,
            &reserve_info,
            reserve_signer_seeds,
            borrow_amount,
        )?;
        require!(
            received == borrow_amount,
            VannaError::VaultAccountingInvariantFailed
        );

        ctx.accounts.reserve.accounted_liquidity_assets = ctx
            .accounts
            .reserve
            .accounted_liquidity_assets
            .checked_sub(borrow_amount)
            .ok_or(VannaError::MathUnderflow)?;
        ctx.accounts.reserve.total_borrow_assets = ctx
            .accounts
            .reserve
            .total_borrow_assets
            .checked_add(borrow_amount)
            .ok_or(VannaError::MathOverflow)?;
        ctx.accounts.reserve.total_borrow_shares = ctx
            .accounts
            .reserve
            .total_borrow_shares
            .checked_add(new_debt_shares)
            .ok_or(VannaError::MathOverflow)?;

        let was_zero_debt = ctx.accounts.debt_position.borrow_shares == 0;
        ctx.accounts.debt_position.credit_shares(new_debt_shares)?;
        if was_zero_debt {
            let active_count = ctx.accounts.margin_account.collateral_count
                + ctx.accounts.margin_account.debt_count;
            require!(
                (active_count as u16) < ctx.accounts.protocol_config.max_assets_per_margin as u16,
                VannaError::TooManyAssets
            );
            ctx.accounts
                .margin_account
                .add_active_debt(ctx.accounts.asset_config.asset_index)?;
        }
    }

    require!(
        ctx.accounts.user_token_account.amount >= deposit_amount,
        VannaError::InsufficientCollateral
    );

    let ctoken_before = ctx.accounts.user_destination_collateral.amount;
    let cpi = KaminoCpiAccounts {
        klend_program: &ctx.accounts.kamino_program.to_account_info(),
        owner: &ctx.accounts.owner.to_account_info(),
        lending_market: &ctx.accounts.lending_market.to_account_info(),
        lending_market_authority: &ctx.accounts.lending_market_authority.to_account_info(),
        reserve: &ctx.accounts.kamino_reserve.to_account_info(),
        reserve_liquidity_mint: &ctx.accounts.underlying_mint.to_account_info(),
        reserve_liquidity_supply: &ctx.accounts.reserve_liquidity_supply.to_account_info(),
        reserve_collateral_mint: &ctx.accounts.reserve_collateral_mint.to_account_info(),
        user_liquidity_account: &ctx.accounts.user_token_account.to_account_info(),
        user_collateral_account: &ctx.accounts.user_destination_collateral.to_account_info(),
        collateral_token_program: &ctx.accounts.collateral_token_program.to_account_info(),
        liquidity_token_program: &ctx.accounts.token_program.to_account_info(),
        instruction_sysvar: &ctx.accounts.instruction_sysvar_account.to_account_info(),
    };
    kamino::deposit_reserve_liquidity(&cpi, deposit_amount, &[])?;
    ctx.accounts.user_destination_collateral.reload()?;
    let ctoken_received = ctx
        .accounts
        .user_destination_collateral
        .amount
        .checked_sub(ctoken_before)
        .ok_or(VannaError::MathUnderflow)?;
    require!(ctoken_received > 0, VannaError::SlippageExceeded);

    if !position_seed(&ctx.accounts.lite_position.key(), &ctx.accounts.margin_account.key(), ctx.accounts.asset_config.asset_index).is_empty() {
        ctx.accounts.margin_account.register_lite(ctx.accounts.asset_config.asset_index)?;
    }
    // Health includes all other stocks' receipt positions as well as liquid margin assets.
    let margin_key = ctx.accounts.margin_account.key();
    let (other_collaterals, other_debts) = scan_and_validate_positions(
        &margin_key,
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        None,
        Some(ctx.accounts.asset_config.asset_index),
        Some((ctx.accounts.lite_position.key(), ctx.accounts.underlying_mint.key())),
    )?;
    let mut collaterals: Vec<CollateralValuation> =
        other_collaterals.into_iter().map(|c| c.valuation).collect();
    let mut debts: Vec<DebtValuation> = other_debts.into_iter().map(|d| d.valuation).collect();

    let price = load_validated_price(
        &ctx.accounts.asset_config,
        &ctx.accounts.price_update,
        &clock,
    )?;
    let lite_collateral_value = normalize_token_value(
        kamino::receipt_value(
            &ctx.accounts.kamino_reserve.to_account_info(),
            &ctx.accounts.kamino_program.key(),
            &ctx.accounts.underlying_mint.key(),
            &ctx.accounts.reserve_collateral_mint.key(),
            ctx.accounts
                .lite_position
                .kamino_collateral_amount
                .checked_add(ctoken_received)
                .ok_or(VannaError::MathOverflow)?,
        )?,
        price.price,
        price.exponent,
        ctx.accounts.asset_config.decimals,
        false,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: lite_collateral_value,
    });

    let projected_debt_assets = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    if projected_debt_assets > 0 {
        let debt_value = normalize_token_value(
            projected_debt_assets,
            price.price,
            price.exponent,
            ctx.accounts.asset_config.decimals,
            true,
        )?;
        debts.push(DebtValuation { debt_value });
    }

    let health = calculate_health(&collaterals, &debts)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);

    let position = &mut ctx.accounts.lite_position;
    position.margin_account = margin_key;
    position.strategy_config = ctx.accounts.lite_strategy.key();
    position.underlying_mint = ctx.accounts.underlying_mint.key();
    position.kamino_collateral_amount = position
        .kamino_collateral_amount
        .checked_add(ctoken_received)
        .ok_or(VannaError::MathOverflow)?;
    position.deposited_underlying = position
        .deposited_underlying
        .checked_add(deposit_amount)
        .ok_or(VannaError::MathOverflow)?;
    position.equity_underlying = position
        .equity_underlying
        .checked_add(equity)
        .ok_or(VannaError::MathOverflow)?;
    position.bump = ctx.bumps.lite_position;
    position.reserved[17] = u8::from(legacy_debt);
    position.set_debt_shares(
        previous_shares
            .checked_add(new_debt_shares)
            .ok_or(VannaError::MathOverflow)?,
    );

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(LiteOpened {
        margin_account: margin_key,
        strategy_config: ctx.accounts.lite_strategy.key(),
        equity,
        borrowed: borrow_amount,
        deposited: deposit_amount,
        kamino_collateral: ctoken_received,
        debt_shares: new_debt_shares,
        borrow_health_factor_wad: health.borrow_health_factor_wad,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lite_supply
//
// Supplies funds already in the margin vault into Kamino (cross-asset flow: the debt lives in
// another reserve). Only adds collateral value, so no health check is needed.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LiteSupply<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        constraint = margin_account.authority == owner.key() @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    /// Source funds; margin-PDA-owned, so the Kamino CPI signs with the margin seeds.
    #[account(
        mut,
        token::mint = underlying_mint,
        token::authority = margin_account,
        token::token_program = token_program
    )]
    pub margin_source_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Vanna reserve (not Kamino's), read only to attribute a same-transaction borrow.
    #[account(seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    /// `init_if_needed` so unleveraged supply works without a prior borrow of this asset.
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + DebtPosition::INIT_SPACE,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    #[account(
        seeds = [LITE_STRATEGY_SEED, underlying_mint.key().as_ref()],
        bump = lite_strategy.bump,
        constraint = lite_strategy.enabled @ VannaError::LiteStrategyDisabled
    )]
    pub lite_strategy: Box<Account<'info, LiteStrategyConfig>>,
    #[account(
        init_if_needed,
        payer = owner,
        space = 8 + LitePosition::INIT_SPACE,
        seeds = [LITE_POSITION_SEED, margin_account.key().as_ref(), &position_seed(&lite_position.key(), &margin_account.key(), asset_config.asset_index)],
        bump
    )]
    pub lite_position: Box<Account<'info, LitePosition>>,
    /// CHECK: must match strategy_config.kamino_program.
    #[account(constraint = kamino_program.key() == lite_strategy.kamino_program @ VannaError::InvalidKaminoProgram)]
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: lending market.
    #[account(constraint = lending_market.key() == lite_strategy.lending_market @ VannaError::InvalidKaminoAccounts)]
    pub lending_market: UncheckedAccount<'info>,
    /// CHECK: market authority.
    #[account(constraint = lending_market_authority.key() == lite_strategy.lending_market_authority @ VannaError::InvalidKaminoAccounts)]
    pub lending_market_authority: UncheckedAccount<'info>,
    /// CHECK: kamino reserve.
    #[account(mut, constraint = kamino_reserve.key() == lite_strategy.kamino_reserve @ VannaError::InvalidKaminoAccounts)]
    pub kamino_reserve: UncheckedAccount<'info>,
    /// CHECK: reserve liquidity supply.
    #[account(mut, constraint = reserve_liquidity_supply.key() == lite_strategy.reserve_liquidity_supply @ VannaError::InvalidKaminoAccounts)]
    pub reserve_liquidity_supply: UncheckedAccount<'info>,
    #[account(
        mut,
        constraint = reserve_collateral_mint.key() == lite_strategy.reserve_collateral_mint @ VannaError::InvalidKaminoAccounts
    )]
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = reserve_collateral_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = collateral_token_program,
    )]
    pub margin_destination_collateral: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: instructions sysvar.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instruction_sysvar_account: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub collateral_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn lite_supply(ctx: Context<LiteSupply>, amount: u64, attribute_shares_delta: u128) -> Result<()> {
    require!(amount > 0, VannaError::ZeroAmount);
    assert_protocol_action_allowed(
        ctx.accounts.protocol_config.operating_mode,
        ProtocolAction::CollateralDeposit,
    )?;
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require_keys_eq!(
        ctx.accounts.lite_strategy.liquidity_token_program,
        ctx.accounts.token_program.key(),
        VannaError::InvalidTokenProgram
    );
    require!(
        ctx.accounts.margin_source_account.amount >= amount,
        VannaError::InsufficientCollateral
    );

    let existing = &ctx.accounts.lite_position;
    if existing.margin_account != Pubkey::default() {
        require_keys_eq!(
            existing.margin_account,
            ctx.accounts.margin_account.key(),
            VannaError::NoLitePosition
        );
        require_keys_eq!(
            existing.strategy_config,
            ctx.accounts.lite_strategy.key(),
            VannaError::NoLitePosition
        );
        require_keys_eq!(
            existing.underlying_mint,
            ctx.accounts.underlying_mint.key(),
            VannaError::NoLitePosition
        );
    }
    let is_new = existing.margin_account == Pubkey::default();

    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];
    let margin_account_info = ctx.accounts.margin_account.to_account_info();

    let ctoken_before = ctx.accounts.margin_destination_collateral.amount;
    let cpi = KaminoCpiAccounts {
        klend_program: &ctx.accounts.kamino_program.to_account_info(),
        owner: &margin_account_info,
        lending_market: &ctx.accounts.lending_market.to_account_info(),
        lending_market_authority: &ctx.accounts.lending_market_authority.to_account_info(),
        reserve: &ctx.accounts.kamino_reserve.to_account_info(),
        reserve_liquidity_mint: &ctx.accounts.underlying_mint.to_account_info(),
        reserve_liquidity_supply: &ctx.accounts.reserve_liquidity_supply.to_account_info(),
        reserve_collateral_mint: &ctx.accounts.reserve_collateral_mint.to_account_info(),
        user_liquidity_account: &ctx.accounts.margin_source_account.to_account_info(),
        user_collateral_account: &ctx.accounts.margin_destination_collateral.to_account_info(),
        collateral_token_program: &ctx.accounts.collateral_token_program.to_account_info(),
        liquidity_token_program: &ctx.accounts.token_program.to_account_info(),
        instruction_sysvar: &ctx.accounts.instruction_sysvar_account.to_account_info(),
    };
    kamino::deposit_reserve_liquidity(&cpi, amount, margin_signer_seeds)?;
    ctx.accounts.margin_destination_collateral.reload()?;
    let ctoken_received = ctx
        .accounts
        .margin_destination_collateral
        .amount
        .checked_sub(ctoken_before)
        .ok_or(VannaError::MathUnderflow)?;
    require!(ctoken_received > 0, VannaError::SlippageExceeded);

    // A drained source vault must release its collateral slot, or a later deposit of this asset
    // fails with `DuplicateAssetIndex`.
    ctx.accounts.margin_source_account.reload()?;
    if ctx.accounts.margin_source_account.amount == 0
        && ctx.accounts.margin_account.is_collateral_active(ctx.accounts.asset_config.asset_index)
    {
        ctx.accounts.margin_account.remove_active_collateral(ctx.accounts.asset_config.asset_index)?;
    }

    if !position_seed(&ctx.accounts.lite_position.key(), &ctx.accounts.margin_account.key(), ctx.accounts.asset_config.asset_index).is_empty() {
        ctx.accounts.margin_account.register_lite(ctx.accounts.asset_config.asset_index)?;
    }

    let clock = Clock::get()?;
    let position = &mut ctx.accounts.lite_position;
    if is_new {
        position.margin_account = ctx.accounts.margin_account.key();
        position.strategy_config = ctx.accounts.lite_strategy.key();
        position.underlying_mint = ctx.accounts.underlying_mint.key();
        position.bump = ctx.bumps.lite_position;
        // Fresh, unleveraged position: deposited == equity keeps debt_shares() at 0.
        position.set_debt_shares(0);
    }
    position.kamino_collateral_amount = position
        .kamino_collateral_amount
        .checked_add(ctoken_received)
        .ok_or(VannaError::MathOverflow)?;
    position.deposited_underlying = position
        .deposited_underlying
        .checked_add(amount)
        .ok_or(VannaError::MathOverflow)?;
    position.equity_underlying = position
        .equity_underlying
        .checked_add(amount)
        .ok_or(VannaError::MathOverflow)?;

    // Attribute a same-transaction borrow so a later exit repays it from the redemption.
    if attribute_shares_delta > 0 {
        let current_attributed = position.debt_shares(ctx.accounts.debt_position.borrow_shares);
        position.set_debt_shares(
            current_attributed
                .checked_add(attribute_shares_delta)
                .ok_or(VannaError::MathOverflow)?,
        );
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(LiteOpened {
        margin_account: ctx.accounts.margin_account.key(),
        strategy_config: ctx.accounts.lite_strategy.key(),
        equity: amount,
        borrowed: 0,
        deposited: amount,
        kamino_collateral: ctoken_received,
        debt_shares: attribute_shares_delta,
        borrow_health_factor_wad: 0,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lite_reduce_redeem / lite_reduce_repay
//
// A same-asset exit is two instructions sent in one transaction: as one instruction it overflowed
// the BPF stack. The redeemed amount is carried via `LitePosition::{set,take}_pending_redeem`.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LiteReduceRedeem<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init_if_needed, payer = owner,
        associated_token::mint = underlying_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = token_program
    )]
    pub margin_underlying_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        constraint = margin_account.authority == owner.key() @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    #[account(
        seeds = [LITE_STRATEGY_SEED, underlying_mint.key().as_ref()],
        bump = lite_strategy.bump
    )]
    pub lite_strategy: Box<Account<'info, LiteStrategyConfig>>,
    #[account(
        mut,
        seeds = [LITE_POSITION_SEED, margin_account.key().as_ref(), &position_seed(&lite_position.key(), &margin_account.key(), asset_config.asset_index)],
        bump = lite_position.bump
    )]
    pub lite_position: Box<Account<'info, LitePosition>>,
    /// CHECK: must match strategy.
    #[account(constraint = kamino_program.key() == lite_strategy.kamino_program @ VannaError::InvalidKaminoProgram)]
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: lending market.
    #[account(constraint = lending_market.key() == lite_strategy.lending_market @ VannaError::InvalidKaminoAccounts)]
    pub lending_market: UncheckedAccount<'info>,
    /// CHECK: market authority.
    #[account(constraint = lending_market_authority.key() == lite_strategy.lending_market_authority @ VannaError::InvalidKaminoAccounts)]
    pub lending_market_authority: UncheckedAccount<'info>,
    /// CHECK: kamino reserve.
    #[account(mut, constraint = kamino_reserve.key() == lite_strategy.kamino_reserve @ VannaError::InvalidKaminoAccounts)]
    pub kamino_reserve: UncheckedAccount<'info>,
    /// CHECK: reserve liquidity supply.
    #[account(mut, constraint = reserve_liquidity_supply.key() == lite_strategy.reserve_liquidity_supply @ VannaError::InvalidKaminoAccounts)]
    pub reserve_liquidity_supply: UncheckedAccount<'info>,
    #[account(
        mut,
        constraint = reserve_collateral_mint.key() == lite_strategy.reserve_collateral_mint @ VannaError::InvalidKaminoAccounts
    )]
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    #[account(
        mut,
        token::mint = reserve_collateral_mint,
        token::authority = margin_account,
        token::token_program = collateral_token_program
    )]
    pub margin_collateral_account: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: instructions sysvar.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instruction_sysvar_account: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub collateral_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[inline(never)]
fn do_redeem_reserve_collateral<'info>(
    accounts: &LiteReduceRedeem<'info>,
    collateral_amount: u64,
    margin_signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let cpi = KaminoCpiAccounts {
        klend_program: &accounts.kamino_program.to_account_info(),
        owner: &accounts.margin_account.to_account_info(),
        lending_market: &accounts.lending_market.to_account_info(),
        lending_market_authority: &accounts.lending_market_authority.to_account_info(),
        reserve: &accounts.kamino_reserve.to_account_info(),
        reserve_liquidity_mint: &accounts.underlying_mint.to_account_info(),
        reserve_liquidity_supply: &accounts.reserve_liquidity_supply.to_account_info(),
        reserve_collateral_mint: &accounts.reserve_collateral_mint.to_account_info(),
        user_liquidity_account: &accounts.margin_underlying_account.to_account_info(),
        user_collateral_account: &accounts.margin_collateral_account.to_account_info(),
        collateral_token_program: &accounts.collateral_token_program.to_account_info(),
        liquidity_token_program: &accounts.token_program.to_account_info(),
        instruction_sysvar: &accounts.instruction_sysvar_account.to_account_info(),
    };
    kamino::redeem_reserve_collateral(&cpi, collateral_amount, margin_signer_seeds)
}

/// Exit step 1: redeems `exit_bps` of the Kamino receipts, scales the position's cost basis,
/// and stashes the redeemed amount for `lite_reduce_repay`.
///
/// Must be followed by `lite_reduce_repay` in the same transaction: debt repayment and the
/// health check happen there.
pub fn lite_reduce_redeem(
    ctx: Context<LiteReduceRedeem>,
    exit_bps: u16,
    min_underlying_out: u64,
) -> Result<()> {
    require!(
        exit_bps > 0 && exit_bps <= 10_000,
        VannaError::InvalidLeverage
    );
    validate_asset_config(
        &ctx.accounts.asset_config,
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.token_program.key(),
    )?;
    require_keys_eq!(
        ctx.accounts.lite_position.margin_account,
        ctx.accounts.margin_account.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        ctx.accounts.lite_position.underlying_mint,
        ctx.accounts.underlying_mint.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        ctx.accounts.lite_position.strategy_config,
        ctx.accounts.lite_strategy.key(),
        VannaError::NoLitePosition
    );
    require!(
        ctx.accounts.lite_position.kamino_collateral_amount > 0,
        VannaError::NoLitePosition
    );
    // An unconsumed pending redeem would otherwise be silently overwritten below.
    require!(
        ctx.accounts.lite_position.reserved[25] == 0,
        VannaError::PendingLiteRedeem
    );
    verify_associated_token_account(
        &ctx.accounts.margin_collateral_account.key(),
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.reserve_collateral_mint.key(),
        &ctx.accounts.collateral_token_program.key(),
    )?;

    let clock = Clock::get()?;
    apply_accrual(&mut ctx.accounts.reserve, clock.unix_timestamp)?;

    let recorded = ctx.accounts.lite_position.kamino_collateral_amount;
    require!(
        ctx.accounts.margin_collateral_account.amount >= recorded,
        VannaError::VaultAccountingInvariantFailed
    );
    let collateral_amount = mul_div_floor(recorded as u128, exit_bps as u128, 10_000)? as u64;
    require!(collateral_amount > 0, VannaError::NoLitePosition);

    let liquidity_before = ctx.accounts.margin_underlying_account.amount;
    let receipts_before = ctx.accounts.margin_collateral_account.amount;
    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] =
        &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];

    do_redeem_reserve_collateral(ctx.accounts, collateral_amount, margin_signer_seeds)?;
    ctx.accounts.margin_collateral_account.reload()?;
    require!(
        receipts_before.checked_sub(ctx.accounts.margin_collateral_account.amount)
            == Some(collateral_amount),
        VannaError::VaultAccountingInvariantFailed
    );
    ctx.accounts.margin_underlying_account.reload()?;
    let redeemed = ctx
        .accounts
        .margin_underlying_account
        .amount
        .checked_sub(liquidity_before)
        .ok_or(VannaError::MathUnderflow)?;
    require!(redeemed >= min_underlying_out, VannaError::SlippageExceeded);

    let position = &mut ctx.accounts.lite_position;
    position.kamino_collateral_amount = recorded - collateral_amount;
    // Scale cost basis by actual burned receipts, preserving the remainder's dust.
    position.deposited_underlying -= mul_div_floor(
        position.deposited_underlying as u128,
        collateral_amount as u128,
        recorded as u128,
    )? as u64;
    position.equity_underlying -= mul_div_floor(
        position.equity_underlying as u128,
        collateral_amount as u128,
        recorded as u128,
    )? as u64;
    position.set_pending_redeem(redeemed);
    Ok(())
}

#[derive(Accounts)]
pub struct LiteReduceRepay<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [ASSET_SEED, underlying_mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(mut, seeds = [RESERVE_SEED, underlying_mint.key().as_ref()], bump = reserve.bump)]
    pub reserve: Box<Account<'info, Reserve>>,
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
    #[account(
        mut,
        associated_token::mint = underlying_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = token_program
    )]
    pub margin_underlying_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = underlying_mint,
        token::authority = owner,
        token::token_program = token_program
    )]
    pub user_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = underlying_mint,
        token::authority = reserve,
        token::token_program = token_program
    )]
    pub liquidity_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        constraint = margin_account.authority == owner.key() @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), reserve.key().as_ref()],
        bump = debt_position.bump
    )]
    pub debt_position: Box<Account<'info, DebtPosition>>,
    #[account(
        seeds = [LITE_STRATEGY_SEED, underlying_mint.key().as_ref()],
        bump = lite_strategy.bump
    )]
    pub lite_strategy: Box<Account<'info, LiteStrategyConfig>>,
    #[account(
        mut,
        seeds = [LITE_POSITION_SEED, margin_account.key().as_ref(), &position_seed(&lite_position.key(), &margin_account.key(), asset_config.asset_index)],
        bump = lite_position.bump
    )]
    pub lite_position: Box<Account<'info, LitePosition>>,
    /// CHECK: must match strategy; used only to value the remaining receipts for the health check.
    #[account(constraint = kamino_program.key() == lite_strategy.kamino_program @ VannaError::InvalidKaminoProgram)]
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: kamino reserve (read-only valuation).
    #[account(constraint = kamino_reserve.key() == lite_strategy.kamino_reserve @ VannaError::InvalidKaminoAccounts)]
    pub kamino_reserve: UncheckedAccount<'info>,
    #[account(
        constraint = reserve_collateral_mint.key() == lite_strategy.reserve_collateral_mint @ VannaError::InvalidKaminoAccounts
    )]
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// Exit step 2: repays this position's attributed debt from the pending redeem, sends the
/// residual to the wallet, runs the health check, and closes the position on a full exit.
///
/// `exit_bps` must match the preceding `lite_reduce_redeem`. Nothing in between can change the
/// attribution inputs, so `target_shares` is recomputed here rather than taken from the client.
pub fn lite_reduce_repay(ctx: Context<LiteReduceRepay>, exit_bps: u16) -> Result<()> {
    require!(
        exit_bps > 0 && exit_bps <= 10_000,
        VannaError::InvalidLeverage
    );
    require_keys_eq!(
        ctx.accounts.lite_position.margin_account,
        ctx.accounts.margin_account.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        ctx.accounts.lite_position.underlying_mint,
        ctx.accounts.underlying_mint.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        ctx.accounts.lite_position.strategy_config,
        ctx.accounts.lite_strategy.key(),
        VannaError::NoLitePosition
    );

    let clock = Clock::get()?;
    let redeemed = ctx
        .accounts
        .lite_position
        .take_pending_redeem()
        .ok_or(VannaError::NoPendingLiteRedeem)?;
    let liquidity_before = ctx
        .accounts
        .margin_underlying_account
        .amount
        .checked_sub(redeemed)
        .ok_or(VannaError::MathUnderflow)?;

    let attributed_shares = ctx
        .accounts
        .lite_position
        .debt_shares(ctx.accounts.debt_position.borrow_shares);
    let target_shares =
        mul_div_ceil(attributed_shares, exit_bps as u128, 10_000)?;

    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] =
        &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];

    let current_debt_assets = debt_shares_to_assets_up(
        target_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    // Cap at what was redeemed instead of failing: debt is ceil-rounded and accrues independently
    // of Kamino yield, so it can exceed `redeemed` by a few units even on a full exit.
    let repay_amount = current_debt_assets.min(redeemed);
    let mut shares_burned = 0u128;
    if repay_amount > 0 {
        let received = transfer_out_checked_measured(
            &ctx.accounts.token_program,
            &ctx.accounts.underlying_mint,
            &ctx.accounts.margin_underlying_account,
            &mut ctx.accounts.liquidity_vault,
            &ctx.accounts.margin_account.to_account_info(),
            margin_signer_seeds,
            repay_amount,
        )?;
        require!(
            received == repay_amount,
            VannaError::VaultAccountingInvariantFailed
        );
        // Never burn more than `target_shares`: the debt position is shared with ordinary margin
        // borrowing, so burning all `borrow_shares` would erase unrelated debt. A short repay
        // leaves the remainder as tracked debt instead of forgiving it.
        shares_burned = if repay_amount >= current_debt_assets {
            target_shares
        } else {
            mul_div_floor(
                repay_amount as u128,
                ctx.accounts.reserve.total_borrow_shares,
                ctx.accounts.reserve.total_borrow_assets as u128,
            )?
            .min(target_shares)
        };
        ctx.accounts.reserve.accounted_liquidity_assets = ctx
            .accounts
            .reserve
            .accounted_liquidity_assets
            .checked_add(received)
            .ok_or(VannaError::MathOverflow)?;
        ctx.accounts.reserve.total_borrow_assets = ctx
            .accounts
            .reserve
            .total_borrow_assets
            .saturating_sub(received);
        ctx.accounts.reserve.total_borrow_shares = ctx
            .accounts
            .reserve
            .total_borrow_shares
            .saturating_sub(shares_burned);
        ctx.accounts.debt_position.debit_shares(shares_burned)?;
        if ctx.accounts.debt_position.borrow_shares == 0 {
            ctx.accounts
                .margin_account
                .remove_active_debt(ctx.accounts.asset_config.asset_index)?;
        }
    }

    let residual = redeemed
        .checked_sub(repay_amount)
        .ok_or(VannaError::MathUnderflow)?;
    if residual > 0 {
        transfer_out_checked_measured(
            &ctx.accounts.token_program,
            &ctx.accounts.underlying_mint,
            &ctx.accounts.margin_underlying_account,
            &mut ctx.accounts.user_token_account,
            &ctx.accounts.margin_account.to_account_info(),
            margin_signer_seeds,
            residual,
        )?;
    }
    ctx.accounts.margin_underlying_account.reload()?;
    require!(
        ctx.accounts.margin_underlying_account.amount == liquidity_before,
        VannaError::VaultAccountingInvariantFailed
    );
    let position = &mut ctx.accounts.lite_position;
    position.set_debt_shares(attributed_shares - target_shares);

    // Exiting must not remove collateral that backs unrelated margin borrowing.
    let (other_collaterals, other_debts) = scan_and_validate_positions(
        &ctx.accounts.margin_account.key(),
        &ctx.accounts.margin_account,
        ctx.remaining_accounts,
        ctx.program_id,
        &clock,
        None,
        Some(ctx.accounts.asset_config.asset_index),
        Some((position.key(), ctx.accounts.underlying_mint.key())),
    )?;
    let mut collaterals: Vec<CollateralValuation> =
        other_collaterals.into_iter().map(|c| c.valuation).collect();
    let mut debts: Vec<DebtValuation> = other_debts.into_iter().map(|d| d.valuation).collect();
    let price = load_validated_price(
        &ctx.accounts.asset_config,
        &ctx.accounts.price_update,
        &clock,
    )?;
    let remaining_value = kamino::receipt_value(
        &ctx.accounts.kamino_reserve.to_account_info(),
        &ctx.accounts.kamino_program.key(),
        &ctx.accounts.underlying_mint.key(),
        &ctx.accounts.reserve_collateral_mint.key(),
        position.kamino_collateral_amount,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: normalize_token_value(
            remaining_value,
            price.price,
            price.exponent,
            ctx.accounts.asset_config.decimals,
            false,
        )?,
    });
    let remaining_debt = debt_shares_to_assets_up(
        ctx.accounts.debt_position.borrow_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    debts.push(DebtValuation {
        debt_value: normalize_token_value(
            remaining_debt,
            price.price,
            price.exponent,
            ctx.accounts.asset_config.decimals,
            true,
        )?,
    });
    require!(
        calculate_health(&collaterals, &debts)?.is_borrow_healthy(),
        VannaError::HealthFactorTooLow
    );
    if exit_bps == 10_000 {
        if !position_seed(&position.key(), &ctx.accounts.margin_account.key(), ctx.accounts.asset_config.asset_index).is_empty() {
            ctx.accounts.margin_account.unregister_lite(ctx.accounts.asset_config.asset_index)?;
        }
        position.close(ctx.accounts.owner.to_account_info())?;
    }

    let event_sequence = ctx.accounts.margin_account.next_event_sequence()?;
    emit!(LiteClosed {
        margin_account: ctx.accounts.margin_account.key(),
        strategy_config: ctx.accounts.lite_strategy.key(),
        redeemed,
        debt_repaid: repay_amount,
        residual,
        debt_shares_burned: shares_burned,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lite_reduce_and_repay
//
// Atomic cross-asset unwind of a `lite_supply` position: redeem the Kamino receipt (e.g. USDC),
// swap it via Jupiter into the stock (e.g. TSLAx), and repay the stock debt. One instruction so
// the health check sees the post-repay state. Only for positions with no same-mint debt.
//
// Several PDA/constraint checks live in the handler, and each leg is its own `#[inline(never)]`
// function: in `try_accounts` or one handler they overflowed the 4KB BPF stack frame.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LiteReduceAndRepay<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: verified against `[PROTOCOL_SEED]` and deserialized in the handler.
    pub protocol_config: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        constraint = margin_account.authority == owner.key() @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,

    // --- Kamino / yield leg (e.g. USDC or SOL) ---
    #[account(seeds = [ASSET_SEED, yield_mint.key().as_ref()], bump = yield_asset_config.bump)]
    pub yield_asset_config: Box<Account<'info, AssetConfig>>,
    pub yield_mint: Box<InterfaceAccount<'info, Mint>>,
    pub yield_price_update: Box<Account<'info, PriceUpdateV2>>,
    // Not `init_if_needed` (stack cost); the client creates these ATAs idempotently beforehand.
    #[account(
        mut,
        token::mint = yield_mint,
        token::authority = margin_account,
        token::token_program = yield_token_program
    )]
    pub margin_yield_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: verified against `[LITE_STRATEGY_SEED, yield_mint]` and deserialized in the handler.
    pub lite_strategy: UncheckedAccount<'info>,
    // PDA checked in the handler: `position_seed` here pushed `try_accounts` to 4192/4096 bytes.
    #[account(mut)]
    pub lite_position: Box<Account<'info, LitePosition>>,
    /// CHECK: matched against `lite_strategy.kamino_program` in the handler.
    pub kamino_program: UncheckedAccount<'info>,
    /// CHECK: matched against `lite_strategy.lending_market` in the handler.
    pub lending_market: UncheckedAccount<'info>,
    /// CHECK: matched against `lite_strategy.lending_market_authority` in the handler.
    pub lending_market_authority: UncheckedAccount<'info>,
    /// CHECK: matched against `lite_strategy.kamino_reserve` in the handler.
    #[account(mut)]
    pub kamino_reserve: UncheckedAccount<'info>,
    /// CHECK: matched against `lite_strategy.reserve_liquidity_supply` in the handler.
    #[account(mut)]
    pub reserve_liquidity_supply: UncheckedAccount<'info>,
    // Matched against `lite_strategy.reserve_collateral_mint` in the handler.
    #[account(mut)]
    pub reserve_collateral_mint: Box<Account<'info, anchor_spl::token::Mint>>,
    #[account(
        mut,
        token::mint = reserve_collateral_mint,
        token::authority = margin_account,
        token::token_program = collateral_token_program
    )]
    pub margin_collateral_account: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: instructions sysvar.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instruction_sysvar_account: UncheckedAccount<'info>,

    // --- Stock / debt leg (e.g. TSLAx) ---
    /// CHECK: verified against `[ASSET_SEED, stock_mint]` and deserialized in the handler.
    pub stock_asset_config: UncheckedAccount<'info>,
    pub stock_mint: Box<InterfaceAccount<'info, Mint>>,
    pub stock_price_update: Box<Account<'info, PriceUpdateV2>>,
    // PDAs checked in the handler; keeping them here left `try_accounts` 8 bytes over 4096.
    #[account(mut)]
    pub stock_reserve: Box<Account<'info, Reserve>>,
    #[account(mut)]
    pub stock_debt_position: Box<Account<'info, DebtPosition>>,
    #[account(
        mut,
        token::mint = stock_mint,
        token::authority = margin_account,
        token::token_program = stock_token_program
    )]
    pub margin_stock_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = stock_mint,
        token::authority = stock_reserve,
        token::token_program = stock_token_program
    )]
    pub stock_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    // --- Swap escrow leg (Jupiter) ---
    /// CHECK: Jupiter-only escrow signer (same PDA as `user_margin_swap`), controls no margin
    /// vaults; verified against `[SWAP_SEED, margin_account]` in the handler.
    pub swap_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        token::mint = yield_mint,
        token::authority = swap_authority,
        token::token_program = yield_token_program
    )]
    pub yield_escrow: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = stock_mint,
        token::authority = swap_authority,
        token::token_program = stock_token_program
    )]
    pub stock_escrow: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: fixed executable Jupiter aggregator.
    #[account(address = JUPITER @ VannaError::InvalidSwapRoute, executable)]
    pub jupiter_program: UncheckedAccount<'info>,

    pub yield_token_program: Interface<'info, TokenInterface>,
    pub stock_token_program: Interface<'info, TokenInterface>,
    pub collateral_token_program: Program<'info, Token>,
}

pub fn lite_reduce_and_repay<'info>(
    ctx: Context<'info, LiteReduceAndRepay<'info>>,
    exit_bps: u16,
    min_yield_out: u64,
    min_stock_out: u64,
    route_account_count: u16,
    route_data: Vec<u8>,
) -> Result<()> {
    require!(
        exit_bps > 0 && exit_bps <= 10_000,
        VannaError::InvalidLeverage
    );

    let program_id = ctx.program_id;
    let remaining_accounts = ctx.remaining_accounts;

    let (protocol_config, stock_asset_config, swap_authority_bump) =
        lra_validate_and_load(ctx.accounts, program_id)?;

    let clock = Clock::get()?;
    apply_accrual(&mut ctx.accounts.stock_reserve, clock.unix_timestamp)?;

    let margin_key = ctx.accounts.margin_account.key();
    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = [ctx.accounts.margin_account.bump];
    let margin_signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &margin_bump]];
    let swap_bump = [swap_authority_bump];
    let swap_signer_seeds: &[&[&[u8]]] = &[&[SWAP_SEED, margin_key.as_ref(), &swap_bump]];

    let (recorded, collateral_amount, redeemed) =
        lra_leg1_redeem(ctx.accounts, margin_signer_seeds, exit_bps, min_yield_out)?;

    let (swapped_out, route_count, _yield_leftover) = lra_leg2_swap(
        ctx.accounts,
        remaining_accounts,
        margin_signer_seeds,
        swap_signer_seeds,
        redeemed,
        min_stock_out,
        route_account_count,
        route_data,
    )?;
    let (_, health_accounts) = remaining_accounts.split_at(route_count);

    let debt_repaid = lra_leg3_repay(ctx.accounts, margin_signer_seeds, stock_asset_config.asset_index)?;

    lra_finalize(
        ctx.accounts,
        health_accounts,
        program_id,
        &clock,
        &stock_asset_config,
        &protocol_config,
        margin_key,
        recorded,
        collateral_amount,
        exit_bps,
        redeemed,
        swapped_out,
        debt_repaid,
    )
}

/// Deserializes an owned `T` after checking the account owner.
///
/// Avoids `Account<'info, T>`, whose lifetime would borrow the whole accounts struct for `'info`
/// and conflict with the mutable borrows the later legs need.
#[inline(never)]
fn load_checked<T: AccountSerialize + AccountDeserialize + Owner>(info: &AccountInfo) -> Result<T> {
    require_keys_eq!(*info.owner, T::owner(), VannaError::InvalidPda);
    let data = info.try_borrow_data()?;
    let mut slice: &[u8] = &data;
    T::try_deserialize(&mut slice)
}

fn lra_validate_and_load<'info>(
    accounts: &LiteReduceAndRepay<'info>,
    program_id: &Pubkey,
) -> Result<(ProtocolConfig, AssetConfig, u8)> {
    // ---- PDA checks + deserialization ----
    let (protocol_config_pda, _) = Pubkey::find_program_address(&[PROTOCOL_SEED], program_id);
    require_keys_eq!(protocol_config_pda, accounts.protocol_config.key(), VannaError::InvalidPda);
    let protocol_config: ProtocolConfig = load_checked(accounts.protocol_config.as_ref())?;

    let (stock_asset_config_pda, _) = Pubkey::find_program_address(
        &[ASSET_SEED, accounts.stock_mint.key().as_ref()],
        program_id,
    );
    require_keys_eq!(stock_asset_config_pda, accounts.stock_asset_config.key(), VannaError::InvalidPda);
    let stock_asset_config: AssetConfig = load_checked(accounts.stock_asset_config.as_ref())?;

    let (lite_strategy_pda, _) = Pubkey::find_program_address(
        &[LITE_STRATEGY_SEED, accounts.yield_mint.key().as_ref()],
        program_id,
    );
    require_keys_eq!(lite_strategy_pda, accounts.lite_strategy.key(), VannaError::InvalidPda);
    let lite_strategy: LiteStrategyConfig = load_checked(accounts.lite_strategy.as_ref())?;
    require_keys_eq!(
        accounts.kamino_program.key(),
        lite_strategy.kamino_program,
        VannaError::InvalidKaminoProgram
    );
    require_keys_eq!(
        accounts.lending_market.key(),
        lite_strategy.lending_market,
        VannaError::InvalidKaminoAccounts
    );
    require_keys_eq!(
        accounts.lending_market_authority.key(),
        lite_strategy.lending_market_authority,
        VannaError::InvalidKaminoAccounts
    );
    require_keys_eq!(
        accounts.kamino_reserve.key(),
        lite_strategy.kamino_reserve,
        VannaError::InvalidKaminoAccounts
    );
    require_keys_eq!(
        accounts.reserve_liquidity_supply.key(),
        lite_strategy.reserve_liquidity_supply,
        VannaError::InvalidKaminoAccounts
    );
    require_keys_eq!(
        accounts.reserve_collateral_mint.key(),
        lite_strategy.reserve_collateral_mint,
        VannaError::InvalidKaminoAccounts
    );

    let (swap_authority_pda, swap_authority_bump) = Pubkey::find_program_address(
        &[SWAP_SEED, accounts.margin_account.key().as_ref()],
        program_id,
    );
    require_keys_eq!(swap_authority_pda, accounts.swap_authority.key(), VannaError::InvalidPda);

    assert_protocol_action_allowed(
        protocol_config.operating_mode,
        ProtocolAction::CollateralWithdraw,
    )?;
    validate_asset_config(
        &accounts.yield_asset_config,
        &accounts.yield_mint.key(),
        &accounts.yield_token_program.key(),
    )?;
    validate_asset_config(
        &stock_asset_config,
        &accounts.stock_mint.key(),
        &accounts.stock_token_program.key(),
    )?;
    require_keys_neq!(
        accounts.yield_mint.key(),
        accounts.stock_mint.key(),
        VannaError::InvalidSwapRoute
    );
    assert_lite_position_pda(
        &accounts.lite_position.key(),
        &accounts.margin_account.key(),
        accounts.yield_asset_config.asset_index,
        program_id,
    )?;
    assert_stock_debt_pdas(
        &accounts.stock_reserve.key(),
        &accounts.stock_debt_position.key(),
        &accounts.stock_mint.key(),
        &accounts.margin_account.key(),
        program_id,
    )?;
    require_keys_eq!(
        accounts.lite_position.margin_account,
        accounts.margin_account.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        accounts.lite_position.underlying_mint,
        accounts.yield_mint.key(),
        VannaError::NoLitePosition
    );
    require_keys_eq!(
        accounts.lite_position.strategy_config,
        accounts.lite_strategy.key(),
        VannaError::NoLitePosition
    );
    require!(
        accounts.lite_position.kamino_collateral_amount > 0,
        VannaError::NoLitePosition
    );
    // Only pure-supply positions; one with same-mint debt exits via `lite_reduce_redeem`/`_repay`.
    require!(
        accounts.lite_position.deposited_underlying == accounts.lite_position.equity_underlying,
        VannaError::InvalidLeverage
    );
    verify_associated_token_account(
        &accounts.margin_collateral_account.key(),
        &accounts.margin_account.key(),
        &accounts.reserve_collateral_mint.key(),
        &accounts.collateral_token_program.key(),
    )?;

    Ok((protocol_config, stock_asset_config, swap_authority_bump))
}

#[inline(never)]
fn assert_lite_position_pda(
    position: &Pubkey,
    margin: &Pubkey,
    asset_index: u16,
    program_id: &Pubkey,
) -> Result<()> {
    let seed = position_seed(position, margin, asset_index);
    let (pda, _) = Pubkey::find_program_address(
        &[LITE_POSITION_SEED, margin.as_ref(), seed.as_slice()],
        program_id,
    );
    require_keys_eq!(pda, *position, VannaError::InvalidPda);
    Ok(())
}

#[inline(never)]
fn assert_stock_debt_pdas(
    reserve: &Pubkey,
    debt: &Pubkey,
    mint: &Pubkey,
    margin: &Pubkey,
    program_id: &Pubkey,
) -> Result<()> {
    let (reserve_pda, _) =
        Pubkey::find_program_address(&[RESERVE_SEED, mint.as_ref()], program_id);
    require_keys_eq!(reserve_pda, *reserve, VannaError::InvalidPda);
    let (debt_pda, _) = Pubkey::find_program_address(
        &[DEBT_SEED, margin.as_ref(), reserve.as_ref()],
        program_id,
    );
    require_keys_eq!(debt_pda, *debt, VannaError::InvalidPda);
    Ok(())
}

#[inline(never)]
fn lra_leg1_redeem<'info>(
    accounts: &mut LiteReduceAndRepay<'info>,
    margin_signer_seeds: &[&[&[u8]]],
    exit_bps: u16,
    min_yield_out: u64,
) -> Result<(u64, u64, u64)> {
    // ---- Leg 1: redeem `exit_bps`% of the Kamino receipt into the margin's own yield vault ----
    let recorded = accounts.lite_position.kamino_collateral_amount;
    let collateral_amount = mul_div_floor(recorded as u128, exit_bps as u128, 10_000)? as u64;
    require!(collateral_amount > 0, VannaError::NoLitePosition);

    let yield_before = accounts.margin_yield_vault.amount;
    let receipts_before = accounts.margin_collateral_account.amount;
    let redeem_cpi = KaminoCpiAccounts {
        klend_program: &accounts.kamino_program.to_account_info(),
        owner: &accounts.margin_account.to_account_info(),
        lending_market: &accounts.lending_market.to_account_info(),
        lending_market_authority: &accounts.lending_market_authority.to_account_info(),
        reserve: &accounts.kamino_reserve.to_account_info(),
        reserve_liquidity_mint: &accounts.yield_mint.to_account_info(),
        reserve_liquidity_supply: &accounts.reserve_liquidity_supply.to_account_info(),
        reserve_collateral_mint: &accounts.reserve_collateral_mint.to_account_info(),
        user_liquidity_account: &accounts.margin_yield_vault.to_account_info(),
        user_collateral_account: &accounts.margin_collateral_account.to_account_info(),
        collateral_token_program: &accounts.collateral_token_program.to_account_info(),
        liquidity_token_program: &accounts.yield_token_program.to_account_info(),
        instruction_sysvar: &accounts.instruction_sysvar_account.to_account_info(),
    };
    kamino::redeem_reserve_collateral(&redeem_cpi, collateral_amount, margin_signer_seeds)?;
    accounts.margin_collateral_account.reload()?;
    require!(
        receipts_before.checked_sub(accounts.margin_collateral_account.amount) == Some(collateral_amount),
        VannaError::VaultAccountingInvariantFailed
    );
    accounts.margin_yield_vault.reload()?;
    let redeemed = accounts
        .margin_yield_vault
        .amount
        .checked_sub(yield_before)
        .ok_or(VannaError::MathUnderflow)?;
    require!(redeemed >= min_yield_out, VannaError::SlippageExceeded);

    Ok((recorded, collateral_amount, redeemed))
}

#[inline(never)]
fn lra_leg2_swap<'info>(
    accounts: &mut LiteReduceAndRepay<'info>,
    remaining_accounts: &'info [AccountInfo<'info>],
    margin_signer_seeds: &[&[&[u8]]],
    swap_signer_seeds: &[&[&[u8]]],
    redeemed: u64,
    min_stock_out: u64,
    route_account_count: u16,
    route_data: Vec<u8>,
) -> Result<(u64, usize, u64)> {
    // ---- Leg 2: swap the redeemed yield asset into the stock asset via Jupiter ----
    let stock_vault_before = accounts.margin_stock_vault.amount;
    let yield_escrow_before = accounts.yield_escrow.amount;

    transfer_out_checked(
        &accounts.yield_token_program,
        &accounts.yield_mint,
        &accounts.margin_yield_vault,
        &accounts.yield_escrow,
        &accounts.margin_account.to_account_info(),
        margin_signer_seeds,
        redeemed,
    )?;
    accounts.yield_escrow.reload()?;
    require!(
        accounts.yield_escrow.amount.checked_sub(yield_escrow_before) == Some(redeemed),
        VannaError::InvalidSwapRoute
    );
    let yield_escrow_funded = accounts.yield_escrow.amount;

    let route_count = usize::from(route_account_count);
    require!(
        route_count > 0 && route_count <= remaining_accounts.len() && route_data.len() >= 8,
        VannaError::InvalidSwapRoute
    );
    let (route_accounts, _health_accounts) = remaining_accounts.split_at(route_count);
    // Same guard as `user_margin_swap`: no Vanna-owned account, program id, wallet signer or
    // margin vault may be passed to the route.
    for account in route_accounts {
        require!(
            *account.owner != crate::ID && account.key() != crate::ID,
            VannaError::InvalidSwapRoute
        );
        require!(
            account.key() != accounts.owner.key()
                && account.key() != accounts.margin_yield_vault.key()
                && account.key() != accounts.margin_stock_vault.key(),
            VannaError::InvalidSwapRoute
        );
    }
    let output_before = accounts.stock_escrow.amount;
    let metas = route_accounts
        .iter()
        .map(|a| AccountMeta {
            pubkey: a.key(),
            is_writable: a.is_writable,
            is_signer: a.key() == accounts.swap_authority.key(),
        })
        .collect();
    let route = Instruction {
        program_id: JUPITER,
        accounts: metas,
        data: route_data,
    };
    let mut infos = route_accounts.to_vec();
    infos.push(accounts.jupiter_program.to_account_info());
    invoke_signed(&route, &infos, swap_signer_seeds)?;
    accounts.yield_escrow.reload()?;
    accounts.stock_escrow.reload()?;
    // The route amount comes from a client-side estimate of `redeemed`, so it may not drain the
    // escrow exactly; only require that the route never added to it.
    require!(
        accounts.yield_escrow.amount <= yield_escrow_funded,
        VannaError::InvalidSwapRoute
    );
    for escrow in [&accounts.yield_escrow, &accounts.stock_escrow] {
        require_keys_eq!(
            escrow.owner,
            accounts.swap_authority.key(),
            VannaError::InvalidSwapRoute
        );
        require!(
            escrow.delegate.is_none() && escrow.close_authority.is_none(),
            VannaError::InvalidSwapRoute
        );
    }
    let swapped_out = accounts
        .stock_escrow
        .amount
        .checked_sub(output_before)
        .ok_or(VannaError::SlippageExceeded)?;
    require!(swapped_out >= min_stock_out, VannaError::SlippageExceeded);

    transfer_out_checked(
        &accounts.stock_token_program,
        &accounts.stock_mint,
        &accounts.stock_escrow,
        &accounts.margin_stock_vault,
        &accounts.swap_authority.to_account_info(),
        swap_signer_seeds,
        swapped_out,
    )?;
    accounts.margin_stock_vault.reload()?;
    // Underflow check only, not equality with `swapped_out`: a Token-2022 transfer-fee mint
    // delivers strictly less (same as `user_margin_swap`).
    accounts
        .margin_stock_vault
        .amount
        .checked_sub(stock_vault_before)
        .ok_or(VannaError::VaultAccountingInvariantFailed)?;

    // Sweep unconsumed yield asset back to the margin vault; `lra_finalize` counts it as collateral.
    let yield_leftover = accounts.yield_escrow.amount;
    if yield_leftover > 0 {
        transfer_out_checked(
            &accounts.yield_token_program,
            &accounts.yield_mint,
            &accounts.yield_escrow,
            &accounts.margin_yield_vault,
            &accounts.swap_authority.to_account_info(),
            swap_signer_seeds,
            yield_leftover,
        )?;
        accounts.margin_yield_vault.reload()?;
    }

    Ok((swapped_out, route_count, yield_leftover))
}

#[inline(never)]
fn lra_leg3_repay<'info>(
    accounts: &mut LiteReduceAndRepay<'info>,
    margin_signer_seeds: &[&[&[u8]]],
    stock_asset_index: u16,
) -> Result<u64> {
    // ---- Leg 3: repay the stock debt from whatever the swap produced ----
    let current_debt_assets = debt_shares_to_assets_up(
        accounts.stock_debt_position.borrow_shares,
        accounts.stock_reserve.total_borrow_shares,
        accounts.stock_reserve.total_borrow_assets,
    )?;
    let target_repay = current_debt_assets.min(accounts.margin_stock_vault.amount);
    let mut debt_repaid = 0u64;
    if target_repay > 0 {
        debt_repaid = transfer_out_checked_measured(
            &accounts.stock_token_program,
            &accounts.stock_mint,
            &accounts.margin_stock_vault,
            &mut accounts.stock_reserve_vault,
            &accounts.margin_account.to_account_info(),
            margin_signer_seeds,
            target_repay,
        )?;
        let shares_burned = if debt_repaid >= current_debt_assets {
            accounts.stock_debt_position.borrow_shares
        } else {
            mul_div_floor(
                debt_repaid as u128,
                accounts.stock_reserve.total_borrow_shares as u128,
                accounts.stock_reserve.total_borrow_assets as u128,
            )?
            .min(accounts.stock_debt_position.borrow_shares)
        };
        accounts.stock_reserve.accounted_liquidity_assets = accounts
            .stock_reserve
            .accounted_liquidity_assets
            .checked_add(debt_repaid)
            .ok_or(VannaError::MathOverflow)?;
        accounts.stock_reserve.total_borrow_assets =
            accounts.stock_reserve.total_borrow_assets.saturating_sub(debt_repaid);
        accounts.stock_reserve.total_borrow_shares =
            accounts.stock_reserve.total_borrow_shares.saturating_sub(shares_burned);
        accounts.stock_debt_position.debit_shares(shares_burned)?;
        if accounts.stock_debt_position.borrow_shares == 0 {
            accounts.margin_account.remove_active_debt(stock_asset_index)?;
        }
    }

    Ok(debt_repaid)
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn lra_finalize<'info>(
    accounts: &mut LiteReduceAndRepay<'info>,
    health_accounts: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
    clock: &Clock,
    stock_asset_config: &AssetConfig,
    protocol_config: &ProtocolConfig,
    margin_key: Pubkey,
    recorded: u64,
    collateral_amount: u64,
    exit_bps: u16,
    redeemed: u64,
    swapped_out: u64,
    debt_repaid: u64,
) -> Result<()> {
    // ---- Position bookkeeping ----
    // deposited == equity held before; scaling both by the same fraction keeps debt_shares() at 0.
    accounts.lite_position.kamino_collateral_amount = recorded - collateral_amount;
    accounts.lite_position.deposited_underlying -= mul_div_floor(
        accounts.lite_position.deposited_underlying as u128,
        collateral_amount as u128,
        recorded as u128,
    )? as u64;
    accounts.lite_position.equity_underlying -= mul_div_floor(
        accounts.lite_position.equity_underlying as u128,
        collateral_amount as u128,
        recorded as u128,
    )? as u64;

    // ---- One final health check: fold in the two touched legs' POST-repay state ----
    let lite_position_key = accounts.lite_position.key();
    let (other_collaterals, other_debts) = scan_and_validate_positions(
        &margin_key,
        &accounts.margin_account,
        health_accounts,
        program_id,
        clock,
        None,
        Some(stock_asset_config.asset_index),
        Some((lite_position_key, accounts.yield_mint.key())),
    )?;
    let mut collaterals: Vec<CollateralValuation> =
        other_collaterals.into_iter().map(|c| c.valuation).collect();
    let mut debts: Vec<DebtValuation> = other_debts.into_iter().map(|d| d.valuation).collect();

    let yield_price = load_validated_price(
        &accounts.yield_asset_config,
        &accounts.yield_price_update,
        clock,
    )?;
    let remaining_kamino_value = kamino::receipt_value(
        &accounts.kamino_reserve.to_account_info(),
        &accounts.kamino_program.key(),
        &accounts.yield_mint.key(),
        &accounts.reserve_collateral_mint.key(),
        accounts.lite_position.kamino_collateral_amount,
    )?;
    collaterals.push(CollateralValuation {
        collateral_value: normalize_token_value(
            remaining_kamino_value,
            yield_price.price,
            yield_price.exponent,
            accounts.yield_asset_config.decimals,
            false,
        )?,
    });

    let stock_price = load_validated_price(
        stock_asset_config,
        &accounts.stock_price_update,
        clock,
    )?;
    let remaining_stock_debt = debt_shares_to_assets_up(
        accounts.stock_debt_position.borrow_shares,
        accounts.stock_reserve.total_borrow_shares,
        accounts.stock_reserve.total_borrow_assets,
    )?;
    debts.push(DebtValuation {
        debt_value: normalize_token_value(
            remaining_stock_debt,
            stock_price.price,
            stock_price.exponent,
            stock_asset_config.decimals,
            true,
        )?,
    });

    // Leftover stock after repay is ordinary margin collateral; activate its index if needed.
    let stock_leftover = accounts.margin_stock_vault.amount;
    let stock_active = accounts
        .margin_account
        .is_collateral_active(stock_asset_config.asset_index);
    if stock_leftover > 0 {
        collaterals.push(CollateralValuation {
            collateral_value: normalize_token_value(
                stock_leftover,
                stock_price.price,
                stock_price.exponent,
                stock_asset_config.decimals,
                false,
            )?,
        });
        if !stock_active {
            require!(
                (accounts.margin_account.collateral_count + accounts.margin_account.debt_count)
                    < protocol_config.max_assets_per_margin,
                VannaError::TooManyAssets
            );
            accounts
                .margin_account
                .add_active_collateral(stock_asset_config.asset_index)?;
        }
    }

    // Same for the yield vault, which includes anything `lra_leg2_swap` swept back.
    let yield_leftover = accounts.margin_yield_vault.amount;
    let yield_active = accounts
        .margin_account
        .is_collateral_active(accounts.yield_asset_config.asset_index);
    if yield_leftover > 0 {
        collaterals.push(CollateralValuation {
            collateral_value: normalize_token_value(
                yield_leftover,
                yield_price.price,
                yield_price.exponent,
                accounts.yield_asset_config.decimals,
                false,
            )?,
        });
        if !yield_active {
            require!(
                (accounts.margin_account.collateral_count + accounts.margin_account.debt_count)
                    < protocol_config.max_assets_per_margin,
                VannaError::TooManyAssets
            );
            let yield_asset_index = accounts.yield_asset_config.asset_index;
            accounts.margin_account.add_active_collateral(yield_asset_index)?;
        }
    }

    require!(
        calculate_health(&collaterals, &debts)?.is_borrow_healthy(),
        VannaError::HealthFactorTooLow
    );

    if exit_bps == 10_000 {
        if !position_seed(&lite_position_key, &margin_key, accounts.yield_asset_config.asset_index).is_empty() {
            accounts
                .margin_account
                .unregister_lite(accounts.yield_asset_config.asset_index)?;
        }
        accounts.lite_position.close(accounts.owner.to_account_info())?;
    }

    let event_sequence = accounts.margin_account.next_event_sequence()?;
    emit!(LiteReducedAndRepaid {
        margin_account: margin_key,
        strategy_config: accounts.lite_strategy.key(),
        redeemed,
        swapped_out,
        debt_repaid,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}
