use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::interest::accrue;
use crate::state::asset_config::AssetConfig;
use crate::state::protocol_config::{OperatingMode, ProtocolConfig};
use crate::state::reserve::{Reserve, ReserveStatus};
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{Mint, Token, TokenAccount};

// ---------------------------------------------------------------------------
// initialize_protocol
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// Must sign — this is what makes the resulting `config.admin` authentic rather than an
    /// unverified instruction argument any front-running payer could set to themselves.
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + ProtocolConfig::INIT_SPACE,
        seeds = [PROTOCOL_SEED],
        bump
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_protocol(ctx: Context<InitializeProtocol>, treasury: Pubkey, max_assets_per_margin: u8) -> Result<()> {
    require!(
        max_assets_per_margin > 0 && (max_assets_per_margin as usize) <= MAX_ASSETS,
        VannaError::InvalidRiskParameters
    );
    require_keys_neq!(treasury, Pubkey::default(), VannaError::InvalidTreasury);

    let admin = ctx.accounts.admin.key();
    let config = &mut ctx.accounts.protocol_config;
    config.admin = admin;
    config.pending_admin = Pubkey::default();
    config.treasury = treasury;
    config.operating_mode = OperatingMode::Normal as u8;
    config.max_assets_per_margin = max_assets_per_margin;
    config.next_asset_index = 0;
    config.bump = ctx.bumps.protocol_config;
    config.reserved = [0u8; 128];

    emit!(ProtocolInitialized {
        admin,
        treasury,
        max_assets_per_margin,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_propose_authority
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminProposeAuthority<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
}

pub fn admin_propose_authority(ctx: Context<AdminProposeAuthority>, new_admin: Pubkey) -> Result<()> {
    require_keys_neq!(new_admin, Pubkey::default(), VannaError::InvalidPendingAdmin);
    ctx.accounts.protocol_config.pending_admin = new_admin;
    emit!(AdminProposed {
        current_admin: ctx.accounts.admin.key(),
        pending_admin: new_admin,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// authority_accept_admin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AuthorityAcceptAdmin<'info> {
    pub pending_admin: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = pending_admin @ VannaError::InvalidPendingAdmin
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
}

pub fn authority_accept_admin(ctx: Context<AuthorityAcceptAdmin>) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let previous_admin = config.admin;
    config.admin = config.pending_admin;
    config.pending_admin = Pubkey::default();
    emit!(AdminAccepted {
        previous_admin,
        new_admin: config.admin,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_set_operating_mode
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminSetOperatingMode<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
}

pub fn admin_set_operating_mode(ctx: Context<AdminSetOperatingMode>, new_mode: u8) -> Result<()> {
    OperatingMode::from_u8(new_mode).ok_or(VannaError::InvalidOperatingMode)?;
    let config = &mut ctx.accounts.protocol_config;
    let old_mode = config.operating_mode;
    config.operating_mode = new_mode;
    emit!(OperatingModeChanged {
        old_mode,
        new_mode,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_register_asset
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminRegisterAsset<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub underlying_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + AssetConfig::INIT_SPACE,
        seeds = [ASSET_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn admin_register_asset(
    ctx: Context<AdminRegisterAsset>,
    price_feed_id: [u8; 32],
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    max_confidence_bps: u16,
    max_price_age_secs: u32,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Result<()> {
    require!(price_feed_id != [0u8; 32], VannaError::InvalidPriceFeed);
    AssetConfig::validate_risk_parameters(ltv_bps, liquidation_threshold_bps, liquidation_bonus_bps)?;

    let protocol_config = &mut ctx.accounts.protocol_config;
    let asset_index = protocol_config.next_asset_index;
    protocol_config.next_asset_index = asset_index.checked_add(1).ok_or(VannaError::MathOverflow)?;

    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.mint = ctx.accounts.underlying_mint.key();
    asset_config.token_program = ctx.accounts.token_program.key();
    asset_config.reserve = Pubkey::default();
    asset_config.price_feed_id = price_feed_id;
    asset_config.max_collateral_per_margin = max_collateral_per_margin;
    asset_config.ltv_bps = ltv_bps;
    asset_config.liquidation_threshold_bps = liquidation_threshold_bps;
    asset_config.liquidation_bonus_bps = liquidation_bonus_bps;
    asset_config.max_confidence_bps = max_confidence_bps;
    asset_config.max_price_age_secs = max_price_age_secs;
    asset_config.asset_index = asset_index;
    asset_config.decimals = ctx.accounts.underlying_mint.decimals;
    asset_config.collateral_enabled = collateral_enabled;
    asset_config.borrow_enabled = borrow_enabled;
    asset_config.bump = ctx.bumps.asset_config;
    asset_config.reserved = [0u8; 96];

    emit!(AssetRegistered {
        asset_config: asset_config.key(),
        mint: asset_config.mint,
        asset_index,
        ltv_bps,
        liquidation_threshold_bps,
        liquidation_bonus_bps,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_update_asset_config
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminUpdateAssetConfig<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [ASSET_SEED, asset_config.mint.as_ref()],
        bump = asset_config.bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
}

#[allow(clippy::too_many_arguments)]
pub fn admin_update_asset_config(
    ctx: Context<AdminUpdateAssetConfig>,
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    max_confidence_bps: u16,
    max_price_age_secs: u32,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Result<()> {
    AssetConfig::validate_risk_parameters(ltv_bps, liquidation_threshold_bps, liquidation_bonus_bps)?;

    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.max_collateral_per_margin = max_collateral_per_margin;
    asset_config.ltv_bps = ltv_bps;
    asset_config.liquidation_threshold_bps = liquidation_threshold_bps;
    asset_config.liquidation_bonus_bps = liquidation_bonus_bps;
    asset_config.max_confidence_bps = max_confidence_bps;
    asset_config.max_price_age_secs = max_price_age_secs;
    asset_config.collateral_enabled = collateral_enabled;
    asset_config.borrow_enabled = borrow_enabled;

    emit!(AssetConfigUpdated {
        asset_config: asset_config.key(),
        ltv_bps,
        liquidation_threshold_bps,
        liquidation_bonus_bps,
        max_collateral_per_margin,
        collateral_enabled,
        borrow_enabled,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_initialize_reserve
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminInitializeReserve<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [ASSET_SEED, underlying_mint.key().as_ref()],
        bump = asset_config.bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub underlying_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + Reserve::INIT_SPACE,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = underlying_mint,
        associated_token::authority = reserve
    )]
    pub liquidity_vault: Box<Account<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        mint::decimals = underlying_mint.decimals,
        mint::authority = reserve,
        seeds = [SHARE_MINT_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub share_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn admin_initialize_reserve(
    ctx: Context<AdminInitializeReserve>,
    base_rate_bps: u16,
    slope1_bps: u16,
    slope2_bps: u16,
    optimal_utilization_bps: u16,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Result<()> {
    require_keys_eq!(ctx.accounts.asset_config.reserve, Pubkey::default(), VannaError::ReserveAlreadyExists);
    Reserve::validate_rate_model(base_rate_bps, optimal_utilization_bps, reserve_factor_bps)?;
    require!(slope1_bps <= 10_000 && slope2_bps <= 20_000, VannaError::InvalidRateModel);
    ReserveStatus::from_u8(status).ok_or(VannaError::InvalidReserveStatus)?;

    let now = Clock::get()?.unix_timestamp;
    let reserve = &mut ctx.accounts.reserve;
    reserve.asset_config = ctx.accounts.asset_config.key();
    reserve.underlying_mint = ctx.accounts.underlying_mint.key();
    reserve.liquidity_vault = ctx.accounts.liquidity_vault.key();
    reserve.share_mint = ctx.accounts.share_mint.key();
    reserve.supply_cap = supply_cap;
    reserve.borrow_cap = borrow_cap;
    reserve.accounted_liquidity_assets = 0;
    reserve.total_borrow_assets = 0;
    reserve.total_borrow_shares = 0;
    reserve.borrow_index_wad = WAD;
    reserve.accrued_protocol_fees = 0;
    reserve.last_update_timestamp = now;
    reserve.base_rate_bps = base_rate_bps;
    reserve.slope1_bps = slope1_bps;
    reserve.slope2_bps = slope2_bps;
    reserve.optimal_utilization_bps = optimal_utilization_bps;
    reserve.reserve_factor_bps = reserve_factor_bps;
    reserve.status = status;
    reserve.bump = ctx.bumps.reserve;
    reserve.reserved = [0u8; 128];

    ctx.accounts.asset_config.reserve = reserve.key();

    emit!(ReserveInitialized {
        reserve: reserve.key(),
        underlying_mint: reserve.underlying_mint,
        liquidity_vault: reserve.liquidity_vault,
        share_mint: reserve.share_mint,
        supply_cap,
        borrow_cap,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_update_reserve_config
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminUpdateReserveConfig<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub underlying_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump = reserve.bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
}

#[allow(clippy::too_many_arguments)]
pub fn admin_update_reserve_config(
    ctx: Context<AdminUpdateReserveConfig>,
    base_rate_bps: u16,
    slope1_bps: u16,
    slope2_bps: u16,
    optimal_utilization_bps: u16,
    reserve_factor_bps: u16,
    supply_cap: u64,
    borrow_cap: u64,
    status: u8,
) -> Result<()> {
    Reserve::validate_rate_model(base_rate_bps, optimal_utilization_bps, reserve_factor_bps)?;
    require!(slope1_bps <= 10_000 && slope2_bps <= 20_000, VannaError::InvalidRateModel);
    ReserveStatus::from_u8(status).ok_or(VannaError::InvalidReserveStatus)?;

    let reserve = &mut ctx.accounts.reserve;
    let now = Clock::get()?.unix_timestamp;
    let accrual = accrue(reserve, now)?;
    reserve.total_borrow_assets = accrual.new_total_borrow_assets;
    reserve.accrued_protocol_fees = accrual.new_accrued_protocol_fees;
    reserve.borrow_index_wad = accrual.new_borrow_index_wad;
    reserve.last_update_timestamp = now;

    reserve.base_rate_bps = base_rate_bps;
    reserve.slope1_bps = slope1_bps;
    reserve.slope2_bps = slope2_bps;
    reserve.optimal_utilization_bps = optimal_utilization_bps;
    reserve.reserve_factor_bps = reserve_factor_bps;
    reserve.supply_cap = supply_cap;
    reserve.borrow_cap = borrow_cap;
    reserve.status = status;

    emit!(ReserveConfigUpdated {
        reserve: reserve.key(),
        base_rate_bps,
        slope1_bps,
        slope2_bps,
        optimal_utilization_bps,
        reserve_factor_bps,
        supply_cap,
        borrow_cap,
        status,
        timestamp: now,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_collect_protocol_fees
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminCollectProtocolFees<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [PROTOCOL_SEED],
        bump = protocol_config.bump,
        has_one = admin @ VannaError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    pub underlying_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        seeds = [RESERVE_SEED, underlying_mint.key().as_ref()],
        bump = reserve.bump
    )]
    pub reserve: Box<Account<'info, Reserve>>,
    #[account(mut, token::mint = underlying_mint, token::authority = reserve)]
    pub liquidity_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = underlying_mint, token::authority = protocol_config.treasury)]
    pub treasury_ata: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn admin_collect_protocol_fees(ctx: Context<AdminCollectProtocolFees>, amount: u64) -> Result<()> {
    require!(amount > 0, VannaError::ZeroAmount);

    let reserve_account_info = ctx.accounts.reserve.to_account_info();
    let now = Clock::get()?.unix_timestamp;
    let reserve = &mut ctx.accounts.reserve;
    let accrual = accrue(reserve, now)?;
    reserve.total_borrow_assets = accrual.new_total_borrow_assets;
    reserve.accrued_protocol_fees = accrual.new_accrued_protocol_fees;
    reserve.borrow_index_wad = accrual.new_borrow_index_wad;
    reserve.last_update_timestamp = now;

    let collectible = reserve.accrued_protocol_fees.min(reserve.accounted_liquidity_assets);
    require!(amount <= collectible, VannaError::InsufficientLiquidity);

    reserve.accounted_liquidity_assets = reserve
        .accounted_liquidity_assets
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;
    reserve.accrued_protocol_fees = reserve
        .accrued_protocol_fees
        .checked_sub(amount)
        .ok_or(VannaError::MathUnderflow)?;

    let mint_key = ctx.accounts.underlying_mint.key();
    let bump = reserve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[RESERVE_SEED, mint_key.as_ref(), &[bump]]];
    crate::validation::token::transfer_out_checked(
        &ctx.accounts.token_program,
        &ctx.accounts.underlying_mint,
        &ctx.accounts.liquidity_vault,
        &ctx.accounts.treasury_ata,
        &reserve_account_info,
        signer_seeds,
        amount,
    )?;

    emit!(ProtocolFeesCollected {
        reserve: ctx.accounts.reserve.key(),
        treasury_ata: ctx.accounts.treasury_ata.key(),
        amount,
        timestamp: now,
    });
    Ok(())
}
