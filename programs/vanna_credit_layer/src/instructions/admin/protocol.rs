use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::state::protocol_config::{OperatingMode, ProtocolConfig};
use anchor_lang::prelude::*;

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
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
