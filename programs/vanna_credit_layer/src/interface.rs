use crate::constants::PROTOCOL_SEED;
use crate::errors::VannaError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{get_return_data, invoke},
};

pub const VANNA_PROGRAM_ID: Pubkey = crate::ID;
pub const VENUE_ACCOUNT_SEED: &[u8] = b"venue_account";
const PROTOCOL_CONFIG_DISCRIMINATOR: [u8; 8] = [207, 91, 250, 28, 152, 179, 215, 209];

pub const GET_PRICE: [u8; 8] = [238, 38, 193, 106, 228, 32, 210, 33];
pub const REVIEW_CALL: [u8; 8] = [183, 124, 54, 159, 103, 200, 237, 25];

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceQuery {
    pub asset: Pubkey,
    pub amount: u64,
    pub liability: bool,
    pub venue_account: Pubkey,
    pub legs: u64,
    pub exposure_only: bool,
}

impl PriceQuery {
    pub fn token(asset: Pubkey, amount: u64, liability: bool) -> Self {
        Self { asset, amount, liability, venue_account: Pubkey::default(), legs: 0, exposure_only: false }
    }

    pub fn venue(asset: Pubkey, venue_account: Pubkey, legs: u64, exposure_only: bool) -> Self {
        Self { asset, amount: 0, liability: false, venue_account, legs, exposure_only }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PriceResult {
    pub value: u128,
    pub checks: u8,
    pub open_legs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceChecks(pub u8);

impl PriceChecks {
    pub const NONE: Self = Self(0);
    pub const FRESH: Self = Self(1);
    pub const TWAP_OK: Self = Self(1 << 1);
    pub const CONFIDENCE_OK: Self = Self(1 << 2);
    pub const ALL: Self = Self(Self::FRESH.0 | Self::TWAP_OK.0 | Self::CONFIDENCE_OK.0);
    pub const LIQUIDATION: Self = Self::FRESH;

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn with(self, flag: Self, set: bool) -> Self {
        if set {
            Self(self.0 | flag.0)
        } else {
            self
        }
    }

    pub fn missing(self, required: Self) -> Self {
        Self(required.0 & !self.0)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallMode {
    Owner,
    Unwind,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub mode: CallMode,
    pub target_program: Pubkey,
    pub data: Vec<u8>,
    pub call_account_count: u16,
    pub margin: Pubkey,
    pub authority: Pubkey,
    pub venue: Pubkey,
    pub venue_account: Pubkey,
    pub open_legs: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermitSigner {
    Margin,
    VenueAccount,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Margin,
    VenueAccount,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub index: u16,
    pub role: Role,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Funding {
    pub index: u16,
    pub amount: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CallPermit {
    pub signer: PermitSigner,
    pub bindings: Vec<Binding>,
    pub tokens_in: Vec<u16>,
    pub tokens_out: Vec<u16>,
    pub funding: Option<Funding>,
    pub opens_leg: Option<u8>,
}

pub fn read_answer<T: AnchorDeserialize>(program: &Pubkey) -> Result<T> {
    let (from, data) = get_return_data().ok_or(VannaError::InvalidAgentAnswer)?;
    require_keys_eq!(from, *program, VannaError::InvalidAgentAnswer);
    T::try_from_slice(&data).map_err(|_| VannaError::InvalidAgentAnswer.into())
}

pub fn protocol_admin(protocol_config: &AccountInfo) -> Result<Pubkey> {
    require_keys_eq!(*protocol_config.owner, VANNA_PROGRAM_ID, VannaError::NotProtocolConfig);
    let (expected, _) = Pubkey::find_program_address(&[PROTOCOL_SEED], &VANNA_PROGRAM_ID);
    require_keys_eq!(*protocol_config.key, expected, VannaError::NotProtocolConfig);
    let data = protocol_config.try_borrow_data()?;
    require!(
        data.len() >= 40 && data[..8] == PROTOCOL_CONFIG_DISCRIMINATOR,
        VannaError::NotProtocolConfig
    );
    Ok(Pubkey::new_from_array(data[8..40].try_into().unwrap()))
}

pub fn require_protocol_admin(protocol_config: &AccountInfo, admin: &Signer) -> Result<()> {
    require_keys_eq!(protocol_admin(protocol_config)?, admin.key(), VannaError::Unauthorized);
    Ok(())
}

pub fn venue_account_address(margin: &Pubkey, venue: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VENUE_ACCOUNT_SEED, margin.as_ref(), venue.as_ref()], &VANNA_PROGRAM_ID)
}

pub fn find_account<'a, 'info>(accounts: &'a [AccountInfo<'info>], key: &Pubkey) -> Option<&'a AccountInfo<'info>> {
    accounts.iter().find(|account| account.key == key)
}

pub struct AgentAccounts<'a, 'info> {
    entries: Vec<(&'a AccountInfo<'info>, &'a [AccountInfo<'info>])>,
}

impl<'a, 'info> AgentAccounts<'a, 'info> {
    pub fn parse(accounts: &'a [AccountInfo<'info>], agents: &[Pubkey]) -> Result<Self> {
        let mut starts: Vec<usize> = accounts
            .iter()
            .enumerate()
            .filter(|(_, account)| agents.contains(account.key))
            .map(|(i, _)| i)
            .collect();
        require!(starts.first().is_none_or(|first| *first == 0), VannaError::InvalidAgentAccounts);
        require!(accounts.is_empty() || !starts.is_empty(), VannaError::InvalidAgentAccounts);
        starts.push(accounts.len());
        let mut entries: Vec<(&AccountInfo, &[AccountInfo])> = Vec::with_capacity(starts.len());
        for window in starts.windows(2) {
            let program = &accounts[window[0]];
            require!(program.executable, VannaError::InvalidAgentAccounts);
            require!(!entries.iter().any(|(p, _)| p.key == program.key), VannaError::InvalidAgentAccounts);
            entries.push((program, &accounts[window[0] + 1..window[1]]));
        }
        Ok(Self { entries })
    }

    pub fn get(&self, agent: &Pubkey) -> Result<(&'a AccountInfo<'info>, &'a [AccountInfo<'info>])> {
        self.entries
            .iter()
            .find(|(program, _)| program.key == agent)
            .copied()
            .ok_or_else(|| VannaError::InvalidAgentAccounts.into())
    }

    pub fn accounts_of(&self, agent: &Pubkey) -> &'a [AccountInfo<'info>] {
        self.get(agent).map_or(&[], |(_, accounts)| accounts)
    }

    pub fn all(&self) -> impl Iterator<Item = &'a AccountInfo<'info>> + '_ {
        self.entries.iter().flat_map(|(program, accounts)| std::iter::once(*program).chain(accounts.iter()))
    }

    pub fn find(&self, key: &Pubkey) -> Option<&'a AccountInfo<'info>> {
        self.all().find(|account| account.key == key)
    }
}

#[inline(never)]
pub fn get_price<'info>(
    oracle: &AccountInfo<'info>,
    accounts: &[AccountInfo<'info>],
    queries: &[PriceQuery],
) -> Result<Vec<PriceResult>> {
    let mut data = GET_PRICE.to_vec();
    queries.to_vec().serialize(&mut data)?;
    invoke_readonly(oracle, accounts, data)?;
    let answers: Vec<PriceResult> = read_answer(oracle.key)?;
    require!(answers.len() == queries.len(), VannaError::InvalidAgentAnswer);
    for (answer, query) in answers.iter().zip(queries) {
        require!(answer.open_legs & !query.legs == 0, VannaError::InvalidAgentAnswer);
    }
    Ok(answers)
}

#[inline(never)]
pub fn review<'info>(
    validator: &AccountInfo<'info>,
    call_accounts: &[AccountInfo<'info>],
    extra: &[AccountInfo<'info>],
    context: &CallContext,
) -> Result<CallPermit> {
    let mut data = REVIEW_CALL.to_vec();
    context.serialize(&mut data)?;
    let accounts: Vec<AccountInfo<'info>> = call_accounts.iter().chain(extra).cloned().collect();
    invoke_readonly(validator, &accounts, data)?;
    read_answer(validator.key)
}

fn invoke_readonly<'info>(program: &AccountInfo<'info>, accounts: &[AccountInfo<'info>], data: Vec<u8>) -> Result<()> {
    let metas = accounts.iter().map(|a| AccountMeta::new_readonly(a.key(), false)).collect();
    let ix = Instruction { program_id: program.key(), accounts: metas, data };
    let mut infos = accounts.to_vec();
    infos.push(program.clone());
    invoke(&ix, &infos)?;
    Ok(())
}
