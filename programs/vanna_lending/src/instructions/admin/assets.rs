//! Asset registry: which tokens the protocol accepts, their risk limits and how they are priced.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::oracle::valuation::receipt_rate;
use crate::state::asset_config::{AssetConfig, PriceSource};
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
    asset_config.price_source = PriceSource::Pyth;
    asset_config.price_source_account = Pubkey::default();
    asset_config.price_source_program = Pubkey::default();
    asset_config.reserved = [0u8; 31];

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
    require!(
        !borrow_enabled || ctx.accounts.asset_config.is_pyth_priced(),
        VannaError::UnsupportedPriceSource
    );

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
// admin_set_asset_price_source
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminSetAssetPriceSource<'info> {
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
    /// CHECK: the account a non-Pyth source reads (the Kamino reserve); its owner, layout and
    /// cToken mint are validated below. Omitted for `Pyth`.
    pub source_account: Option<UncheckedAccount<'info>>,
    /// The registered underlying asset of a receipt, whose feed and decimals must match.
    /// Omitted for `Pyth`.
    #[account(seeds = [ASSET_SEED, underlying_asset_config.mint.as_ref()], bump = underlying_asset_config.bump)]
    pub underlying_asset_config: Option<Box<Account<'info, AssetConfig>>>,
}

/// Switches how an asset is valued. Only allowed while the asset is not collateral-enabled, so an
/// asset is never live under the wrong pricing: register it disabled, set the source, then enable
/// it. Non-Pyth assets are collateral-only: no reserve and no borrowing.
pub fn admin_set_asset_price_source(
    mut ctx: Context<AdminSetAssetPriceSource>,
    price_source: PriceSource,
    source_program: Pubkey,
) -> Result<()> {
    let accounts = &mut ctx.accounts;
    require!(!accounts.asset_config.collateral_enabled, VannaError::UnsupportedPriceSource);

    let asset_config = &mut accounts.asset_config;
    asset_config.price_source = price_source;
    match price_source {
        PriceSource::Pyth => {
            asset_config.price_source_account = Pubkey::default();
            asset_config.price_source_program = Pubkey::default();
        }
        PriceSource::KaminoReceipt => {
            require!(
                !asset_config.borrow_enabled && asset_config.reserve == Pubkey::default(),
                VannaError::UnsupportedPriceSource
            );
            let source = accounts.source_account.as_ref().ok_or(VannaError::InvalidPriceSource)?.to_account_info();
            asset_config.price_source_account = source.key();
            asset_config.price_source_program = source_program;
            let rate = receipt_rate(asset_config, Some(&source))?;

            // The cToken is priced with the underlying's feed at the underlying's decimals.
            let underlying = accounts.underlying_asset_config.as_ref().ok_or(VannaError::InvalidPriceSource)?;
            require_keys_eq!(underlying.mint, rate.liquidity_mint, VannaError::InvalidKaminoAccounts);
            require!(underlying.decimals == rate.liquidity_decimals, VannaError::InvalidKaminoAccounts);
            require!(asset_config.price_feed_id == underlying.price_feed_id, VannaError::InvalidPriceFeed);
        }
    }

    emit!(AssetPriceSourceUpdated {
        asset_config: asset_config.key(),
        price_source,
        source_account: asset_config.price_source_account,
        source_program: asset_config.price_source_program,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
