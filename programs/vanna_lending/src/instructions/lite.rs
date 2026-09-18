//! Lite / Stocks strategy: leveraged same-asset carry into Kamino.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::external::kamino::{self, KaminoCpiAccounts};
use crate::instructions::borrowing::apply_accrual;
use crate::math::fixed_point::mul_div_floor;
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
use crate::validation::token::{transfer_out_checked_measured, verify_associated_token_account};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Token, TokenAccount as SplTokenAccount};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;

/// Instructions sysvar — Kamino introspects this during deposit/redeem.
const INSTRUCTIONS_SYSVAR_ID: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");

/// Max leverage 5x (50_000 bps). Min is 1x (10_000) which is equity-only deposit.
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

/// Empty final seed preserves the original PDA and its bump. Additional stocks use
/// the immutable asset index. Constraints still enforce the complete canonical PDA.
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

    // Ensure user still holds enough equity+borrow to deposit.
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
    kamino::deposit_reserve_liquidity(&cpi, deposit_amount)?;
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
// lite_close
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LiteClose<'info> {
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
        init_if_needed, payer = owner,
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

pub fn lite_close(ctx: Context<LiteClose>, min_underlying_out: u64) -> Result<()> {
    lite_reduce(ctx, 10_000, min_underlying_out)
}

pub fn lite_reduce(ctx: Context<LiteClose>, exit_bps: u16, min_underlying_out: u64) -> Result<()> {
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
    let attributed_shares = ctx
        .accounts
        .lite_position
        .debt_shares(ctx.accounts.debt_position.borrow_shares);
    let target_shares =
        crate::math::fixed_point::mul_div_ceil(attributed_shares, exit_bps as u128, 10_000)?;
    require!(collateral_amount > 0, VannaError::NoLitePosition);

    let liquidity_before = ctx.accounts.margin_underlying_account.amount;
    let receipts_before = ctx.accounts.margin_collateral_account.amount;
    let authority_key = ctx.accounts.margin_account.authority;
    let margin_bump = ctx.accounts.margin_account.bump;
    let margin_signer_seeds: &[&[&[u8]]] =
        &[&[MARGIN_SEED, authority_key.as_ref(), &[margin_bump]]];

    let cpi = KaminoCpiAccounts {
        klend_program: &ctx.accounts.kamino_program.to_account_info(),
        owner: &ctx.accounts.margin_account.to_account_info(),
        lending_market: &ctx.accounts.lending_market.to_account_info(),
        lending_market_authority: &ctx.accounts.lending_market_authority.to_account_info(),
        reserve: &ctx.accounts.kamino_reserve.to_account_info(),
        reserve_liquidity_mint: &ctx.accounts.underlying_mint.to_account_info(),
        reserve_liquidity_supply: &ctx.accounts.reserve_liquidity_supply.to_account_info(),
        reserve_collateral_mint: &ctx.accounts.reserve_collateral_mint.to_account_info(),
        user_liquidity_account: &ctx.accounts.margin_underlying_account.to_account_info(),
        user_collateral_account: &ctx.accounts.margin_collateral_account.to_account_info(),
        collateral_token_program: &ctx.accounts.collateral_token_program.to_account_info(),
        liquidity_token_program: &ctx.accounts.token_program.to_account_info(),
        instruction_sysvar: &ctx.accounts.instruction_sysvar_account.to_account_info(),
    };
    kamino::redeem_reserve_collateral(&cpi, collateral_amount, margin_signer_seeds)?;
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

    let current_debt_assets = debt_shares_to_assets_up(
        target_shares,
        ctx.accounts.reserve.total_borrow_shares,
        ctx.accounts.reserve.total_borrow_assets,
    )?;
    require!(
        redeemed >= current_debt_assets,
        VannaError::InsufficientCollateral
    );
    let repay_amount = current_debt_assets;
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
        shares_burned = target_shares;
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
