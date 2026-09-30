use crate::constants::{ASSET_SEED, DEBT_SEED, RESERVE_SEED};
use crate::errors::VannaError;
use crate::math::health::{CollateralValuation, DebtValuation};
use crate::math::interest::accrue;
use crate::math::shares::debt_shares_to_assets_up;
use crate::interface::{get_price, AgentAccounts};
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::state::reserve::Reserve;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
use crate::interface::{venue_account_address, PriceResult, PriceQuery, PriceChecks};

pub const COLLATERAL_GROUP_LEN: usize = 2;
pub const DEBT_GROUP_LEN: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct Holding {
    pub asset_index: u16,
    pub oracle: Pubkey,
    pub query: PriceQuery,
    pub is_debt: bool,
}

impl Holding {
    pub fn token(asset: &AssetConfig, amount: u64, is_debt: bool) -> Self {
        Self {
            asset_index: asset.asset_index,
            oracle: asset.oracle,
            query: PriceQuery::token(asset.mint, amount, is_debt),
            is_debt,
        }
    }

    pub fn venue(asset: &AssetConfig, margin: &Pubkey, legs: u64, exposure_only: bool) -> Self {
        let (venue_account, _) = venue_account_address(margin, &asset.mint);
        Self {
            asset_index: asset.asset_index,
            oracle: asset.oracle,
            query: PriceQuery::venue(asset.mint, venue_account, legs, exposure_only),
            is_debt: false,
        }
    }
}

pub struct Valuation {
    pub collaterals: Vec<CollateralValuation>,
    pub debts: Vec<DebtValuation>,
    pub checks: PriceChecks,
    pub prices: Vec<PriceResult>,
}

impl Valuation {
    pub fn require(&self, required: PriceChecks) -> Result<()> {
        require_checks(self.checks, required)
    }
}

pub fn require_checks(checks: PriceChecks, required: PriceChecks) -> Result<()> {
    let missing = checks.missing(required);
    require!(!missing.contains(PriceChecks::FRESH), VannaError::StalePrice);
    require!(!missing.contains(PriceChecks::TWAP_OK), VannaError::PriceTooDivergentFromTwap);
    require!(!missing.contains(PriceChecks::CONFIDENCE_OK), VannaError::ConfidenceTooWide);
    Ok(())
}

pub fn agents_of(holdings: &[Holding], extra: &[Pubkey]) -> Vec<Pubkey> {
    let mut agents: Vec<Pubkey> = Vec::new();
    for key in holdings.iter().map(|h| h.oracle).chain(extra.iter().copied()) {
        if !agents.contains(&key) {
            agents.push(key);
        }
    }
    agents
}

#[inline(never)]
pub fn value_holdings(holdings: &[Holding], agents: &AgentAccounts) -> Result<Valuation> {
    let mut prices = vec![PriceResult::default(); holdings.len()];
    let mut done = vec![false; holdings.len()];
    for i in 0..holdings.len() {
        if done[i] {
            continue;
        }
        let oracle = holdings[i].oracle;
        let members: Vec<usize> = (i..holdings.len()).filter(|j| holdings[*j].oracle == oracle).collect();
        let queries: Vec<PriceQuery> = members.iter().map(|j| holdings[*j].query).collect();
        let (program, accounts) = agents.get(&oracle)?;
        let answers = get_price(program, accounts, &queries)?;
        for (j, answer) in members.into_iter().zip(answers) {
            prices[j] = answer;
            done[j] = true;
        }
    }

    let mut collaterals = Vec::new();
    let mut debts = Vec::new();
    let mut checks = PriceChecks::ALL;
    for (holding, price) in holdings.iter().zip(&prices) {
        if !holding.query.exposure_only {
            checks = checks.intersection(PriceChecks(price.checks));
        }
        match holding.is_debt {
            true => debts.push(DebtValuation { debt_value: price.value }),
            false => collaterals.push(CollateralValuation { collateral_value: price.value }),
        }
    }
    Ok(Valuation { collaterals, debts, checks, prices })
}

fn verify_pda(actual: &Pubkey, seeds: &[&[u8]], bump: u8, program_id: &Pubkey) -> Result<()> {
    let bump_seed = [bump];
    let mut full_seeds: Vec<&[u8]> = seeds.to_vec();
    full_seeds.push(&bump_seed);
    let derived = Pubkey::create_program_address(&full_seeds, program_id)
        .map_err(|_| VannaError::InvalidBump)?;
    require_keys_eq!(derived, *actual, VannaError::InvalidPda);
    Ok(())
}

fn load_asset_config<'info>(
    info: &'info AccountInfo<'info>,
    asset_index: u16,
    program_id: &Pubkey,
) -> Result<Account<'info, AssetConfig>> {
    let asset_config =
        Account::<AssetConfig>::try_from(info).map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
    verify_pda(info.key, &[ASSET_SEED, asset_config.mint.as_ref()], asset_config.bump, program_id)?;
    require!(asset_config.asset_index == asset_index, VannaError::IncompletePositionAccounts);
    Ok(asset_config)
}

pub fn position_accounts_len(margin: &MarginAccount, named_collaterals: &[u16], named_debt: Option<u16>) -> usize {
    let collaterals = margin.active_collateral_indexes().filter(|i| !named_collaterals.contains(i)).count();
    let debts = margin.active_debt_indexes().filter(|i| Some(*i) != named_debt).count();
    collaterals * COLLATERAL_GROUP_LEN + debts * DEBT_GROUP_LEN
}

pub fn split_positions<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    margin: &MarginAccount,
    named_collaterals: &[u16],
    named_debt: Option<u16>,
) -> Result<(&'a [AccountInfo<'info>], &'a [AccountInfo<'info>])> {
    let len = position_accounts_len(margin, named_collaterals, named_debt);
    require!(len <= accounts.len(), VannaError::IncompletePositionAccounts);
    Ok(accounts.split_at(len))
}

#[inline(never)]
pub fn collect_holdings<'info>(
    margin_key: &Pubkey,
    margin: &MarginAccount,
    positions: &'info [AccountInfo<'info>],
    program_id: &Pubkey,
    clock: &Clock,
    named_collaterals: &[u16],
    named_debt: Option<u16>,
) -> Result<Vec<Holding>> {
    let mut cursor = 0usize;
    let mut holdings = Vec::with_capacity(margin.collateral_count as usize + margin.debt_count as usize);

    for asset_index in margin.active_collateral_indexes() {
        if named_collaterals.contains(&asset_index) {
            continue;
        }
        require!(cursor + COLLATERAL_GROUP_LEN <= positions.len(), VannaError::IncompletePositionAccounts);
        let asset_config = load_asset_config(&positions[cursor], asset_index, program_id)?;
        let holder_info = &positions[cursor + 1];
        cursor += COLLATERAL_GROUP_LEN;

        if asset_config.is_venue() {
            let holding = Holding::venue(&asset_config, margin_key, margin.legs_of(asset_index), false);
            require_keys_eq!(*holder_info.key, holding.query.venue_account, VannaError::IncompletePositionAccounts);
            holdings.push(holding);
            continue;
        }

        let vault = InterfaceAccount::<TokenAccount>::try_from(holder_info)
            .map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_associated_token_account(holder_info.key, margin_key, &asset_config.mint, &asset_config.token_program)?;
        require_keys_eq!(vault.owner, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(vault.mint, asset_config.mint, VannaError::IncompletePositionAccounts);
        holdings.push(Holding::token(&asset_config, vault.amount, false));
    }

    for asset_index in margin.active_debt_indexes() {
        if Some(asset_index) == named_debt {
            continue;
        }
        require!(cursor + DEBT_GROUP_LEN <= positions.len(), VannaError::IncompletePositionAccounts);
        let asset_info = &positions[cursor];
        let asset_config = load_asset_config(asset_info, asset_index, program_id)?;
        let reserve_info = &positions[cursor + 1];
        let debt_position_info = &positions[cursor + 2];
        cursor += DEBT_GROUP_LEN;

        let reserve =
            Account::<Reserve>::try_from(reserve_info).map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_pda(reserve_info.key, &[RESERVE_SEED, asset_config.mint.as_ref()], reserve.bump, program_id)?;
        require_keys_eq!(reserve.asset_config, asset_info.key(), VannaError::IncompletePositionAccounts);
        require_keys_eq!(asset_config.reserve, reserve_info.key(), VannaError::IncompletePositionAccounts);

        let debt_position = Account::<DebtPosition>::try_from(debt_position_info)
            .map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_pda(
            debt_position_info.key,
            &[DEBT_SEED, margin_key.as_ref(), reserve_info.key.as_ref()],
            debt_position.bump,
            program_id,
        )?;
        require_keys_eq!(debt_position.margin_account, *margin_key, VannaError::IncompletePositionAccounts);
        require_keys_eq!(debt_position.reserve, *reserve_info.key, VannaError::IncompletePositionAccounts);

        let accrual = accrue(&reserve, clock.unix_timestamp)?;
        let current_debt_assets = debt_shares_to_assets_up(
            debt_position.borrow_shares,
            reserve.total_borrow_shares,
            accrual.new_total_borrow_assets,
        )?;
        holdings.push(Holding::token(&asset_config, current_debt_assets, true));
    }

    require!(cursor == positions.len(), VannaError::IncompletePositionAccounts);
    Ok(holdings)
}

pub struct NewToken<'info> {
    pub asset: Account<'info, AssetConfig>,
    pub vault: &'info AccountInfo<'info>,
}

pub fn load_new_tokens<'info>(
    groups: &'info [AccountInfo<'info>],
    margin_key: &Pubkey,
    margin: &MarginAccount,
    program_id: &Pubkey,
) -> Result<Vec<NewToken<'info>>> {
    let mut tokens: Vec<NewToken<'info>> = Vec::new();
    for group in groups.chunks_exact(COLLATERAL_GROUP_LEN) {
        let asset =
            Account::<AssetConfig>::try_from(&group[0]).map_err(|_| error!(VannaError::IncompletePositionAccounts))?;
        verify_pda(group[0].key, &[ASSET_SEED, asset.mint.as_ref()], asset.bump, program_id)?;
        require!(
            !asset.is_venue()
                && !margin.is_collateral_active(asset.asset_index)
                && !tokens.iter().any(|token| token.asset.asset_index == asset.asset_index),
            VannaError::IncompletePositionAccounts
        );
        verify_associated_token_account(group[1].key, margin_key, &asset.mint, &asset.token_program)?;
        token_amount(&group[1])?;
        tokens.push(NewToken { asset, vault: &group[1] });
    }
    Ok(tokens)
}

pub fn refresh_token_amounts(holdings: &mut [Holding], margin: &MarginAccount, groups: &[AccountInfo]) -> Result<()> {
    for (k, _) in margin.active_collateral_indexes().enumerate() {
        if holdings[k].query.venue_account == Pubkey::default() {
            holdings[k].query.amount = token_amount(&groups[k * COLLATERAL_GROUP_LEN + 1])?;
        }
    }
    Ok(())
}

pub fn token_amount(info: &AccountInfo) -> Result<u64> {
    require!(
        *info.owner == anchor_spl::token::ID || *info.owner == anchor_spl::token_2022::ID,
        VannaError::IncompletePositionAccounts
    );
    Ok(TokenAccount::try_deserialize(&mut &info.try_borrow_data()?[..])?.amount)
}
