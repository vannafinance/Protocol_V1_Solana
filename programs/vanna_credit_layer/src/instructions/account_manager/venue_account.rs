use super::exec::{guard_accounts, invoke_as};
use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::math::health::calculate_health;
use crate::interface::{review, AgentAccounts};
use crate::risk_engine::{collect_holdings, agents_of, split_positions, value_holdings, Holding};
use crate::state::asset_config::AssetConfig;
use crate::state::integration::Integration;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::validation::accounts::validate_asset_config;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use crate::interface::{venue_account_address, CallContext, CallMode, PermitSigner, PriceChecks, Role, VENUE_ACCOUNT_SEED};

#[derive(Accounts)]
pub struct PublicVenueSettle<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(mut, seeds = [MARGIN_SEED, margin_account.authority.as_ref()], bump = margin_account.bump)]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, venue_asset.mint.as_ref()], bump = venue_asset.bump)]
    pub venue_asset: Box<Account<'info, AssetConfig>>,
    #[account(seeds = [ASSET_SEED, settle_mint.key().as_ref()], bump = settle_asset.bump)]
    pub settle_asset: Box<Account<'info, AssetConfig>>,
    pub settle_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: the margin's venue_account at the venue; checked in the handler.
    #[account(mut)]
    pub venue_account: UncheckedAccount<'info>,
    /// CHECK: the venue_account's settle-token ATA (it may not exist); checked in the handler.
    #[account(mut)]
    pub idle: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = caller,
        associated_token::mint = settle_mint,
        associated_token::authority = margin_account,
        associated_token::token_program = token_program
    )]
    pub margin_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: the venue's oracle.
    #[account(executable, address = venue_asset.oracle @ VannaError::InvalidAgentAccounts)]
    pub oracle: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn public_venue_settle<'info>(ctx: Context<'info, PublicVenueSettle<'info>>) -> Result<()> {
    let accounts = &ctx.accounts;
    let venue = &accounts.venue_asset;
    require!(venue.is_venue(), VannaError::NotAVenue);
    require_keys_eq!(accounts.settle_mint.key(), venue.settle_mint, VannaError::InvalidMint);
    let token_program = accounts.token_program.key();
    validate_asset_config(&accounts.settle_asset, &accounts.settle_mint.key(), &token_program)?;
    let margin_key = accounts.margin_account.key();
    let (venue_account, venue_account_bump) = venue_account_address(&margin_key, &venue.mint);
    require_keys_eq!(accounts.venue_account.key(), venue_account, VannaError::InvalidVenueAccount);
    verify_associated_token_account(&accounts.idle.key(), &venue_account, &venue.settle_mint, &token_program)?;

    let venue_index = venue.asset_index;
    let tracked = accounts.margin_account.legs_of(venue_index);
    let holding = Holding::venue(venue, &margin_key, tracked, true);
    let answers = crate::interface::get_price(&accounts.oracle, ctx.remaining_accounts, &[holding.query])?;
    let open_legs = answers[0].open_legs;

    let swept = match accounts.idle.data_is_empty() {
        true => 0,
        false => super::exec::read_token_account(&accounts.idle, &token_program)?.amount,
    };
    let bump = [venue_account_bump];
    let venue_key = venue.mint;
    let seeds: &[&[u8]] = &[VENUE_ACCOUNT_SEED, margin_key.as_ref(), venue_key.as_ref(), &bump];
    if swept > 0 {
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                TransferChecked {
                    from: accounts.idle.to_account_info(),
                    mint: accounts.settle_mint.to_account_info(),
                    to: accounts.margin_vault.to_account_info(),
                    authority: accounts.venue_account.to_account_info(),
                },
                &[seeds],
            ),
            swept,
            accounts.settle_mint.decimals,
        )?;
    }
    let refunded = match accounts.caller.key() == accounts.margin_account.authority {
        true => accounts.venue_account.lamports(),
        false => 0,
    };
    if refunded > 0 {
        system_program::transfer(
            CpiContext::new_with_signer(
                accounts.system_program.key(),
                system_program::Transfer { from: accounts.venue_account.to_account_info(), to: accounts.caller.to_account_info() },
                &[seeds],
            ),
            refunded,
        )?;
    }

    let settle_index = accounts.settle_asset.asset_index;
    let max_assets = accounts.protocol_config.max_assets_per_margin;
    let margin = &mut ctx.accounts.margin_account;
    margin.set_legs(venue_index, open_legs)?;
    let closed = open_legs == 0 && margin.is_collateral_active(venue_index);
    if closed {
        margin.remove_active_collateral(venue_index)?;
    }
    if swept > 0 && !margin.is_collateral_active(settle_index) {
        require!(margin.collateral_count + margin.debt_count < max_assets, VannaError::TooManyAssets);
        margin.add_active_collateral(settle_index)?;
    }

    emit!(VenueSettled {
        margin_account: margin_key,
        venue: venue_key,
        swept,
        lamports_refunded: refunded,
        open_legs,
        closed,
        event_sequence: margin.next_event_sequence()?,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct PublicVenueUnwind<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [MARGIN_SEED, margin_account.authority.as_ref()], bump = margin_account.bump)]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, venue_asset.mint.as_ref()], bump = venue_asset.bump)]
    pub venue_asset: Box<Account<'info, AssetConfig>>,
    #[account(
        seeds = [INTEGRATION_SEED, target_program.key().as_ref()],
        bump = integration.bump,
        has_one = validator @ VannaError::InvalidAgentAccounts
    )]
    pub integration: Box<Account<'info, Integration>>,
    /// CHECK: bound to the registered integration by the seeds above.
    #[account(executable)]
    pub target_program: UncheckedAccount<'info>,
    /// CHECK: the integration's validator.
    #[account(executable)]
    pub validator: UncheckedAccount<'info>,
}

pub fn public_venue_unwind<'info>(ctx: Context<'info, PublicVenueUnwind<'info>>, data: Vec<u8>, call_account_count: u16) -> Result<()> {
    let accounts = &ctx.accounts;
    let venue = &accounts.venue_asset;
    require!(venue.is_venue(), VannaError::NotAVenue);
    let margin = &accounts.margin_account;
    let margin_key = margin.key();
    require!(margin.is_collateral_active(venue.asset_index), VannaError::IncompletePositionAccounts);
    let (venue_account, venue_account_bump) = venue_account_address(&margin_key, &venue.mint);

    let count = usize::from(call_account_count);
    require!(count <= ctx.remaining_accounts.len(), VannaError::InvalidCallAccounts);
    let (call_accounts, rest) = ctx.remaining_accounts.split_at(count);
    let (groups, agent_accounts) = split_positions(rest, margin, &[], None)?;
    let clock = Clock::get()?;
    let holdings = collect_holdings(&margin_key, margin, groups, ctx.program_id, &clock, &[], None)?;
    let validator = accounts.validator.key();
    let agents = AgentAccounts::parse(agent_accounts, &agents_of(&holdings, &[validator]))?;

    let valuation = value_holdings(&holdings, &agents)?;
    valuation.require(PriceChecks::LIQUIDATION)?;
    let health = calculate_health(&valuation.collaterals, &valuation.debts)?;
    require!(health.is_liquidatable(), VannaError::PositionHealthy);

    let context = CallContext {
        mode: CallMode::Unwind,
        target_program: accounts.target_program.key(),
        data: data.clone(),
        call_account_count,
        margin: margin_key,
        authority: margin.authority,
        venue: venue.mint,
        venue_account,
        open_legs: margin.legs_of(venue.asset_index),
    };
    let permit = review(&accounts.validator, call_accounts, agents.accounts_of(&validator), &context)?;
    require!(
        permit.signer == PermitSigner::VenueAccount
            && permit.tokens_in.is_empty()
            && permit.tokens_out.is_empty()
            && permit.funding.is_none()
            && permit.opens_leg.is_none(),
        VannaError::InvalidAgentAnswer
    );
    for binding in &permit.bindings {
        require!(binding.role == Role::VenueAccount, VannaError::InvalidAgentAnswer);
        let account = call_accounts.get(usize::from(binding.index)).ok_or(VannaError::InvalidCallAccounts)?;
        require_keys_eq!(account.key(), venue_account, VannaError::InvalidCallAccounts);
    }
    guard_accounts(call_accounts, &margin_key, &margin.authority, &[])?;

    let bump = [venue_account_bump];
    let venue_key = venue.mint;
    let seeds: &[&[&[u8]]] = &[&[VENUE_ACCOUNT_SEED, margin_key.as_ref(), venue_key.as_ref(), &bump]];
    invoke_as(&accounts.target_program, call_accounts, data, &venue_account, seeds)?;

    emit!(VenueUnwound {
        margin_account: margin_key,
        venue: venue_key,
        caller: accounts.caller.key(),
        program_id: accounts.target_program.key(),
        health_factor_wad: health.liquidation_health_factor_wad,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}
