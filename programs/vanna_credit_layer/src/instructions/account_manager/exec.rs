use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::interface::{
    review, venue_account_address, AgentAccounts, CallContext, CallMode, CallPermit, Funding, PermitSigner, PriceChecks, Role,
    VENUE_ACCOUNT_SEED,
};
use crate::math::health::calculate_health;
use crate::risk_engine::{
    agents_of, collect_holdings, load_new_tokens, refresh_token_amounts, split_positions, token_amount, value_holdings, Holding,
    NewToken, COLLATERAL_GROUP_LEN,
};
use crate::state::asset_config::AssetConfig;
use crate::state::integration::Integration;
use crate::state::margin_account::MarginAccount;
use crate::state::protocol_config::ProtocolConfig;
use crate::validation::accounts::{assert_protocol_action_allowed, ProtocolAction};
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::token_interface::{self, TokenAccount, TransferChecked};

#[derive(Accounts)]
pub struct MarginExecute<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [PROTOCOL_SEED], bump = protocol_config.bump)]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, authority.key().as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
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
    #[account(seeds = [ASSET_SEED, venue_asset.mint.as_ref()], bump = venue_asset.bump)]
    pub venue_asset: Option<Box<Account<'info, AssetConfig>>>,
    /// CHECK: the margin's venue account at `venue_asset`; checked in the handler.
    #[account(mut)]
    pub venue_account: Option<UncheckedAccount<'info>>,
}

#[derive(Clone, Copy)]
struct Venue {
    mint: Pubkey,
    settle_mint: Pubkey,
    asset_index: u16,
    oracle: Pubkey,
    collateral_enabled: bool,
    account: Pubkey,
    bump: u8,
}

struct TokenUse<'info> {
    vault: &'info AccountInfo<'info>,
    mint: Pubkey,
    before: u64,
    asset_index: u16,
    active: bool,
    collateral_enabled: bool,
    cap: u64,
}

pub fn margin_execute<'info>(
    ctx: Context<'info, MarginExecute<'info>>,
    data: Vec<u8>,
    call_account_count: u16,
    new_assets: u8,
) -> Result<()> {
    let accounts = &ctx.accounts;
    assert_protocol_action_allowed(accounts.protocol_config.operating_mode, ProtocolAction::ExternalCall)?;
    require!(accounts.integration.enabled, VannaError::IntegrationDisabled);
    let margin = &accounts.margin_account;
    let margin_key = margin.key();
    let authority_key = accounts.authority.key();
    let venue = venue_of(accounts, &margin_key)?;

    let count = usize::from(call_account_count);
    require!(count <= ctx.remaining_accounts.len(), VannaError::InvalidCallAccounts);
    let (call, rest) = ctx.remaining_accounts.split_at(count);
    let (groups, rest) = split_positions(rest, margin, &[], None)?;
    let new_len = usize::from(new_assets) * COLLATERAL_GROUP_LEN;
    require!(new_len <= rest.len(), VannaError::IncompletePositionAccounts);
    let (new_groups, agent_accounts) = rest.split_at(new_len);
    let clock = Clock::get()?;
    let mut holdings = collect_holdings(&margin_key, margin, groups, ctx.program_id, &clock, &[], None)?;
    let new_tokens = load_new_tokens(new_groups, &margin_key, margin, ctx.program_id)?;
    let validator = accounts.validator.key();
    let mut needed = vec![validator];
    needed.extend(new_tokens.iter().map(|token| token.asset.oracle));
    needed.extend(venue.map(|v| v.oracle));
    let agents = AgentAccounts::parse(agent_accounts, &agents_of(&holdings, &needed))?;

    let open_legs = venue.map_or(0, |v| margin.legs_of(v.asset_index));
    let context = CallContext {
        mode: CallMode::Owner,
        target_program: accounts.target_program.key(),
        data: data.clone(),
        call_account_count,
        margin: margin_key,
        authority: authority_key,
        venue: venue.map_or(Pubkey::default(), |v| v.mint),
        venue_account: venue.map_or(Pubkey::default(), |v| v.account),
        open_legs,
    };
    let permit = review(&accounts.validator, call, agents.accounts_of(&validator), &context)?;
    check_bindings(&permit, call, &margin_key, venue.map(|v| v.account))?;
    let tokens_in = resolve_tokens(&permit.tokens_in, call, margin, &margin_key, groups, &new_tokens)?;
    let tokens_out = resolve_tokens(&permit.tokens_out, call, margin, &margin_key, groups, &new_tokens)?;
    require!(venue.is_none() || (tokens_in.is_empty() && tokens_out.is_empty()), VannaError::InvalidAgentAnswer);
    require!(tokens_out.iter().all(|token| token.active), VannaError::IncompletePositionAccounts);
    require!(tokens_in.iter().all(|token| token.collateral_enabled), VannaError::AssetNotCollateralEnabled);
    require!(
        new_tokens.iter().all(|new| tokens_in.iter().any(|token| token.vault.key == new.vault.key)),
        VannaError::IncompletePositionAccounts
    );
    let vaults: Vec<Pubkey> = tokens_in.iter().chain(&tokens_out).map(|token| token.vault.key()).collect();
    guard_accounts(call, &margin_key, &authority_key, &vaults)?;
    if permit.opens_leg.is_some() {
        let venue = venue.ok_or(VannaError::InvalidAgentAnswer)?;
        require!(venue.collateral_enabled, VannaError::AssetNotCollateralEnabled);
    }
    let funded = match (permit.funding, venue) {
        (Some(funding), Some(venue)) => Some(fund_venue_account(accounts, call, groups, &venue, funding)?),
        (Some(_), None) => return err!(VannaError::InvalidAgentAnswer),
        (None, _) => None,
    };

    match venue {
        None => {
            let seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin.bump]]];
            invoke_as(&accounts.target_program, call, data, &margin_key, seeds)?;
        }
        Some(venue) => {
            let seeds: &[&[&[u8]]] = &[&[VENUE_ACCOUNT_SEED, margin_key.as_ref(), venue.mint.as_ref(), &[venue.bump]]];
            invoke_as(&accounts.target_program, call, data, &venue.account, seeds)?;
        }
    }

    for token in tokens_in.iter().chain(&tokens_out) {
        let vault = TokenAccount::try_deserialize(&mut &token.vault.try_borrow_data()?[..])?;
        require!(
            vault.owner == margin_key && vault.delegate.is_none() && vault.close_authority.is_none(),
            VannaError::InvalidCallResult
        );
    }
    if let Some((destination, before)) = funded {
        require!(token_amount(destination)? >= before, VannaError::InvalidCallResult);
    }

    refresh_token_amounts(&mut holdings, margin, groups)?;
    for token in &new_tokens {
        holdings.push(Holding::token(&token.asset, token_amount(token.vault)?, false));
    }
    let legs = open_legs | permit.opens_leg.map_or(0, |leg| 1u64 << leg);
    let venue_slot = match (venue, &accounts.venue_asset) {
        (Some(v), Some(asset)) => Some(match holdings.iter().position(|h| !h.is_debt && h.asset_index == v.asset_index) {
            Some(slot) => {
                holdings[slot].query.legs = legs;
                slot
            }
            None => {
                holdings.push(Holding::venue(asset, &margin_key, legs, false));
                holdings.len() - 1
            }
        }),
        _ => None,
    };
    let valuation = value_holdings(&holdings, &agents)?;
    if !valuation.debts.is_empty() {
        valuation.require(PriceChecks::ALL)?;
    }
    let health = calculate_health(&valuation.collaterals, &valuation.debts)?;
    require!(health.is_borrow_healthy(), VannaError::HealthFactorTooLow);

    let open_after = venue_slot.map(|slot| valuation.prices[slot].open_legs);
    let max_assets = accounts.protocol_config.max_assets_per_margin;
    let program_id = accounts.integration.program_id;
    let margin = &mut ctx.accounts.margin_account;
    let mut spent = Vec::with_capacity(tokens_out.len() + 1);
    let mut received = Vec::with_capacity(tokens_in.len());
    for token in &tokens_out {
        let balance = token_amount(token.vault)?;
        spent.push(TokenAmount { mint: token.mint, amount: token.before.saturating_sub(balance) });
        if balance == 0 && margin.is_collateral_active(token.asset_index) {
            margin.remove_active_collateral(token.asset_index)?;
        }
    }
    for token in &tokens_in {
        let balance = token_amount(token.vault)?;
        received.push(TokenAmount { mint: token.mint, amount: balance.saturating_sub(token.before) });
        require!(token.cap == UNCAPPED || balance <= token.cap, VannaError::CollateralCapExceeded);
        if balance > 0 {
            add_asset(margin, token.asset_index, max_assets)?;
        }
    }
    if let (Some(venue), Some(funding)) = (venue, permit.funding) {
        spent.push(TokenAmount { mint: venue.settle_mint, amount: funding.amount });
    }
    if let (Some(venue), Some(open)) = (venue, open_after) {
        margin.set_legs(venue.asset_index, open)?;
        if permit.opens_leg.is_some() {
            add_asset(margin, venue.asset_index, max_assets)?;
        }
    }

    let event_sequence = margin.next_event_sequence()?;
    emit!(MarginExecuted {
        margin_account: margin_key,
        program_id,
        validator,
        spent,
        received,
        borrow_health_factor_wad: health.borrow_health_factor_wad,
        event_sequence,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

fn venue_of(accounts: &MarginExecute, margin_key: &Pubkey) -> Result<Option<Venue>> {
    match (&accounts.venue_asset, &accounts.venue_account) {
        (Some(asset), Some(account)) => {
            require!(asset.is_venue(), VannaError::NotAVenue);
            let (address, bump) = venue_account_address(margin_key, &asset.mint);
            require_keys_eq!(account.key(), address, VannaError::InvalidVenueAccount);
            Ok(Some(Venue {
                mint: asset.mint,
                settle_mint: asset.settle_mint,
                asset_index: asset.asset_index,
                oracle: asset.oracle,
                collateral_enabled: asset.collateral_enabled,
                account: address,
                bump,
            }))
        }
        (None, None) => Ok(None),
        _ => err!(VannaError::InvalidVenueAccount),
    }
}

fn add_asset(margin: &mut MarginAccount, asset_index: u16, max_assets: u8) -> Result<()> {
    if !margin.is_collateral_active(asset_index) {
        require!(margin.collateral_count + margin.debt_count < max_assets, VannaError::TooManyAssets);
        margin.add_active_collateral(asset_index)?;
    }
    Ok(())
}

fn check_bindings(permit: &CallPermit, call: &[AccountInfo], margin: &Pubkey, venue_account: Option<Pubkey>) -> Result<()> {
    let expected = if venue_account.is_some() { PermitSigner::VenueAccount } else { PermitSigner::Margin };
    require!(permit.signer == expected, VannaError::InvalidAgentAnswer);
    for binding in &permit.bindings {
        let key = match binding.role {
            Role::Margin => *margin,
            Role::VenueAccount => venue_account.ok_or(VannaError::InvalidAgentAnswer)?,
        };
        let account = call.get(usize::from(binding.index)).ok_or(VannaError::InvalidCallAccounts)?;
        require_keys_eq!(account.key(), key, VannaError::InvalidCallAccounts);
    }
    Ok(())
}

fn resolve_tokens<'info>(
    indexes: &[u16],
    call: &'info [AccountInfo<'info>],
    margin: &MarginAccount,
    margin_key: &Pubkey,
    groups: &'info [AccountInfo<'info>],
    new_tokens: &[NewToken<'info>],
) -> Result<Vec<TokenUse<'info>>> {
    let mut tokens = Vec::with_capacity(indexes.len());
    for index in indexes {
        let vault = call.get(usize::from(*index)).ok_or(VannaError::InvalidCallAccounts)?;
        let slot = margin
            .active_collateral_indexes()
            .enumerate()
            .map(|(k, _)| k * COLLATERAL_GROUP_LEN)
            .find(|at| groups[at + 1].key == vault.key);
        let (mint, asset_index, collateral_enabled, cap, is_venue) = match slot {
            Some(at) => {
                let asset = Account::<AssetConfig>::try_from(&groups[at])?;
                (asset.mint, asset.asset_index, asset.collateral_enabled, asset.max_collateral_per_margin, asset.is_venue())
            }
            None => {
                let new = new_tokens.iter().find(|token| token.vault.key == vault.key).ok_or_else(|| {
                    match is_token_account_of(vault, margin_key) {
                        true => error!(VannaError::IncompletePositionAccounts),
                        false => error!(VannaError::InvalidCallAccounts),
                    }
                })?;
                (new.asset.mint, new.asset.asset_index, new.asset.collateral_enabled, new.asset.max_collateral_per_margin, false)
            }
        };
        require!(!is_venue, VannaError::InvalidCallAccounts);
        let before = token_amount(vault)?;
        tokens.push(TokenUse { vault, mint, before, asset_index, active: slot.is_some(), collateral_enabled, cap });
    }
    Ok(tokens)
}

fn fund_venue_account<'info>(
    accounts: &MarginExecute<'info>,
    call: &'info [AccountInfo<'info>],
    groups: &'info [AccountInfo<'info>],
    venue: &Venue,
    funding: Funding,
) -> Result<(&'info AccountInfo<'info>, u64)> {
    let margin = &accounts.margin_account;
    let at = margin
        .active_collateral_indexes()
        .enumerate()
        .map(|(k, _)| k * COLLATERAL_GROUP_LEN)
        .find(|at| Account::<AssetConfig>::try_from(&groups[*at]).is_ok_and(|asset| asset.mint == venue.settle_mint))
        .ok_or(VannaError::IncompletePositionAccounts)?;
    let settle = Account::<AssetConfig>::try_from(&groups[at])?;
    let source = &groups[at + 1];
    let destination = call.get(usize::from(funding.index)).ok_or(VannaError::InvalidCallAccounts)?;
    verify_associated_token_account(destination.key, &venue.account, &venue.settle_mint, &settle.token_program)?;
    let mint = call.iter().find(|account| *account.key == venue.settle_mint).ok_or(VannaError::InvalidCallAccounts)?;
    let before = token_amount(destination)?;

    let authority_key = accounts.authority.key();
    let seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin.bump]]];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            settle.token_program,
            TransferChecked {
                from: source.clone(),
                mint: mint.clone(),
                to: destination.clone(),
                authority: margin.to_account_info(),
            },
            seeds,
        ),
        funding.amount,
        settle.decimals,
    )?;
    Ok((destination, before))
}

pub(crate) fn guard_accounts(cpi: &[AccountInfo], margin: &Pubkey, authority: &Pubkey, vaults: &[Pubkey]) -> Result<()> {
    for account in cpi {
        let key = account.key();
        require!(key != crate::ID && key != *authority, VannaError::InvalidCallAccounts);
        if *account.owner == crate::ID {
            require_keys_eq!(key, *margin, VannaError::InvalidCallAccounts);
        }
        if !vaults.contains(&key) {
            require!(!is_token_account_of(account, margin), VannaError::InvalidCallAccounts);
        }
    }
    Ok(())
}

fn is_token_account_of(account: &AccountInfo, owner: &Pubkey) -> bool {
    if *account.owner != anchor_spl::token::ID && *account.owner != anchor_spl::token_2022::ID {
        return false;
    }
    let Ok(data) = account.try_borrow_data() else {
        return false;
    };
    let is_token_account = data.len() == 165 || (data.len() > 165 && data[165] == 2);
    is_token_account && data[32..64] == owner.to_bytes()
}

pub(crate) fn read_token_account(info: &AccountInfo, token_program: &Pubkey) -> Result<TokenAccount> {
    require_keys_eq!(*info.owner, *token_program, VannaError::InvalidCallResult);
    TokenAccount::try_deserialize(&mut &info.try_borrow_data()?[..])
}

#[inline(never)]
pub(crate) fn invoke_as<'info>(
    program: &AccountInfo<'info>,
    cpi: &[AccountInfo<'info>],
    data: Vec<u8>,
    signer: &Pubkey,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let metas = cpi
        .iter()
        .map(|a| AccountMeta { pubkey: a.key(), is_signer: a.key() == *signer, is_writable: a.is_writable })
        .collect();
    let ix = Instruction { program_id: program.key(), accounts: metas, data };
    let mut infos = cpi.to_vec();
    infos.push(program.clone());
    invoke_signed(&ix, &infos, signer_seeds)?;
    Ok(())
}
