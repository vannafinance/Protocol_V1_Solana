//! External-program registry: which programs `margin_execute` may call, and through which adapter.

use crate::adapters::AdapterKind;
use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::state::integration::Integration;
use crate::state::protocol_config::ProtocolConfig;
use anchor_lang::prelude::*;

// ---------------------------------------------------------------------------
// admin_register_integration
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminRegisterIntegration<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump, has_one = admin @ VannaError::Unauthorized)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    /// CHECK: the external program being whitelisted; only its key is stored.
    #[account(executable)]
    pub target_program: UncheckedAccount<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + Integration::INIT_SPACE,
        seeds = [INTEGRATION_SEED, target_program.key().as_ref()],
        bump
    )]
    pub integration: Box<Account<'info, Integration>>,
    pub system_program: Program<'info, System>,
}

pub fn admin_register_integration(ctx: Context<AdminRegisterIntegration>, adapter: AdapterKind) -> Result<()> {
    require_keys_neq!(ctx.accounts.target_program.key(), crate::ID, VannaError::CallNotAllowed);
    let integration = &mut ctx.accounts.integration;
    integration.program_id = ctx.accounts.target_program.key();
    integration.adapter = adapter;
    integration.enabled = true;
    integration.bump = ctx.bumps.integration;
    integration.reserved = [0u8; 64];

    emit!(IntegrationRegistered {
        integration: integration.key(),
        program_id: integration.program_id,
        adapter,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// admin_set_integration_enabled
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdminSetIntegrationEnabled<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump, has_one = admin @ VannaError::Unauthorized)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(mut, seeds = [INTEGRATION_SEED, integration.program_id.as_ref()], bump = integration.bump)]
    pub integration: Box<Account<'info, Integration>>,
}

pub fn admin_set_integration_enabled(ctx: Context<AdminSetIntegrationEnabled>, enabled: bool) -> Result<()> {
    ctx.accounts.integration.enabled = enabled;
    emit!(IntegrationStatusUpdated {
        integration: ctx.accounts.integration.key(),
        enabled,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
