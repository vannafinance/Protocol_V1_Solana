use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::interface::get_price;
use crate::state::asset_config::{AssetConfig, AssetKind};
use crate::state::protocol_config::ProtocolConfig;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};
use crate::interface::PriceQuery;

fn check_oracle<'info>(oracle: &AccountInfo<'info>, accounts: &[AccountInfo<'info>], asset: &AssetConfig) -> Result<()> {
    let one = 10u64.checked_pow(asset.decimals as u32).ok_or(VannaError::MathOverflow)?;
    let answers = get_price(oracle, accounts, &[PriceQuery::token(asset.mint, one, false)])?;
    require!(answers[0].value > 0, VannaError::InvalidAgentAnswer);
    Ok(())
}

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
    /// CHECK: the agent that values this token; it must price it now.
    #[account(executable)]
    pub oracle: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn admin_register_asset<'info>(
    ctx: Context<'info, AdminRegisterAsset<'info>>,
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
    asset_config.kind = AssetKind::Token;
    asset_config.oracle = ctx.accounts.oracle.key();
    asset_config.settle_mint = Pubkey::default();
    asset_config.reserved = [0u8; 32];
    check_oracle(&ctx.accounts.oracle, ctx.remaining_accounts, asset_config)?;

    let timestamp = Clock::get()?.unix_timestamp;
    emit!(AssetRegistered {
        asset_config: asset_config.key(),
        mint: asset_config.mint,
        asset_index,
        ltv_bps,
        liquidation_threshold_bps,
        liquidation_bonus_bps,
        timestamp,
    });
    emit!(AssetOracleUpdated { asset_config: asset_config.key(), oracle: asset_config.oracle, timestamp });
    Ok(())
}

#[derive(Accounts)]
#[instruction(venue: Pubkey)]
pub struct AdminRegisterVenue<'info> {
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
    #[account(
        init,
        payer = payer,
        space = 8 + AssetConfig::INIT_SPACE,
        seeds = [ASSET_SEED, venue.as_ref()],
        bump
    )]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    #[account(seeds = [ASSET_SEED, settle_asset.mint.as_ref()], bump = settle_asset.bump)]
    pub settle_asset: Box<Account<'info, AssetConfig>>,
    /// CHECK: the agent that values venue_accounts at this venue.
    #[account(executable)]
    pub oracle: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn admin_register_venue(ctx: Context<AdminRegisterVenue>, venue: Pubkey) -> Result<()> {
    let settle = &ctx.accounts.settle_asset;
    require!(!settle.is_venue(), VannaError::NotAToken);

    let protocol_config = &mut ctx.accounts.protocol_config;
    let asset_index = protocol_config.next_asset_index;
    protocol_config.next_asset_index = asset_index.checked_add(1).ok_or(VannaError::MathOverflow)?;

    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.mint = venue;
    asset_config.token_program = Pubkey::default();
    asset_config.reserve = Pubkey::default();
    asset_config.max_collateral_per_margin = UNCAPPED;
    asset_config.ltv_bps = 0;
    asset_config.liquidation_threshold_bps = 10_000;
    asset_config.liquidation_bonus_bps = 0;
    asset_config.asset_index = asset_index;
    asset_config.decimals = 0;
    asset_config.collateral_enabled = false;
    asset_config.borrow_enabled = false;
    asset_config.bump = ctx.bumps.asset_config;
    asset_config.kind = AssetKind::Venue;
    asset_config.oracle = ctx.accounts.oracle.key();
    asset_config.settle_mint = settle.mint;
    asset_config.reserved = [0u8; 32];

    let timestamp = Clock::get()?.unix_timestamp;
    emit!(VenueRegistered {
        asset_config: asset_config.key(),
        venue,
        asset_index,
        settle_mint: settle.mint,
        timestamp,
    });
    emit!(AssetOracleUpdated { asset_config: asset_config.key(), oracle: asset_config.oracle, timestamp });
    Ok(())
}

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
    require!(!borrow_enabled || !ctx.accounts.asset_config.is_venue(), VannaError::NotAToken);

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
    /// CHECK: the new oracle.
    #[account(executable)]
    pub oracle: UncheckedAccount<'info>,
}

pub fn admin_set_asset_oracle<'info>(ctx: Context<'info, AdminSetAssetOracle<'info>>) -> Result<()> {
    let asset_config = &mut ctx.accounts.asset_config;
    asset_config.oracle = ctx.accounts.oracle.key();
    if !asset_config.is_venue() {
        check_oracle(&ctx.accounts.oracle, ctx.remaining_accounts, asset_config)?;
    }
    emit!(AssetOracleUpdated {
        asset_config: asset_config.key(),
        oracle: asset_config.oracle,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
