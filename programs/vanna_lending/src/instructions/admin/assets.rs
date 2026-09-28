//! Asset registry: which tokens the protocol accepts, their risk limits and how they are priced.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::oracle;
use crate::state::asset_config::{AssetConfig, OracleConfig};
use crate::state::protocol_config::ProtocolConfig;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};

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
    pub underlying_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + AssetConfig::INIT_SPACE,
        seeds = [ASSET_SEED, underlying_mint.key().as_ref()],
        bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// Registers a mint with its oracle. `remaining_accounts` = every account `oracle` names (and, for a
/// Kamino receipt, its underlying's `AssetConfig`), for the checks in `oracle::validate_config`.
#[allow(clippy::too_many_arguments)]
pub fn admin_register_asset(
    ctx: Context<AdminRegisterAsset>,
    oracle: OracleConfig,
    max_collateral_per_margin: u64,
    ltv_bps: u16,
    liquidation_threshold_bps: u16,
    liquidation_bonus_bps: u16,
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Result<()> {
    AssetConfig::validate_risk_parameters(ltv_bps, liquidation_threshold_bps, liquidation_bonus_bps)?;

    let protocol_config = &mut ctx.accounts.protocol_config;
    let asset_index = protocol_config.next_asset_index;
    protocol_config.next_asset_index = asset_index.checked_add(1).ok_or(VannaError::MathOverflow)?;

    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.mint = ctx.accounts.underlying_mint.key();
    asset_config.token_program = ctx.accounts.token_program.key();
    asset_config.reserve = Pubkey::default();
    asset_config.max_collateral_per_margin = max_collateral_per_margin;
    asset_config.ltv_bps = ltv_bps;
    asset_config.liquidation_threshold_bps = liquidation_threshold_bps;
    asset_config.liquidation_bonus_bps = liquidation_bonus_bps;
    asset_config.asset_index = asset_index;
    asset_config.decimals = ctx.accounts.underlying_mint.decimals;
    asset_config.collateral_enabled = collateral_enabled;
    asset_config.borrow_enabled = borrow_enabled;
    asset_config.bump = ctx.bumps.asset_config;
    asset_config.oracle = oracle;
    asset_config.reserved = [0u8; 32];
    oracle::validate_config(asset_config, &[ctx.remaining_accounts], ctx.program_id, &Clock::get()?)?;

    emit!(AssetRegistered {
        asset_config: asset_config.key(),
        mint: asset_config.mint,
        asset_index,
        ltv_bps,
        liquidation_threshold_bps,
        liquidation_bonus_bps,
        timestamp: Clock::get()?.unix_timestamp,
    });
    emit!(AssetOracleUpdated {
        asset_config: asset_config.key(),
        oracle,
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
    collateral_enabled: bool,
    borrow_enabled: bool,
) -> Result<()> {
    AssetConfig::validate_risk_parameters(ltv_bps, liquidation_threshold_bps, liquidation_bonus_bps)?;
    // A Kamino receipt is collateral only.
    require!(
        !borrow_enabled || !ctx.accounts.asset_config.oracle.uses_klend(),
        VannaError::UnsupportedPriceSource
    );

    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.max_collateral_per_margin = max_collateral_per_margin;
    asset_config.ltv_bps = ltv_bps;
    asset_config.liquidation_threshold_bps = liquidation_threshold_bps;
    asset_config.liquidation_bonus_bps = liquidation_bonus_bps;
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
// admin_set_asset_oracle
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminSetAssetOracle<'info> {
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

/// Replaces how an asset is priced (the Solidity `OracleFacade.setOracle`). Allowed while the asset
/// is live, so a source can be rotated without pausing it: the new config must pass every check in
/// `oracle::validate_config`, including pricing the asset now. `remaining_accounts` as for
/// `admin_register_asset`.
pub fn admin_set_asset_oracle(ctx: Context<AdminSetAssetOracle>, oracle: OracleConfig) -> Result<()> {
    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.oracle = oracle;
    oracle::validate_config(asset_config, &[ctx.remaining_accounts], ctx.program_id, &Clock::get()?)?;

    emit!(AssetOracleUpdated {
        asset_config: asset_config.key(),
        oracle,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
