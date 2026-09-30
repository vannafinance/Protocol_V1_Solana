use crate::OracleError;
use anchor_lang::prelude::*;

pub const USD_UNIT: u128 = 100_000_000_000_000_000_000;
pub const FUNDING_ADJUSTMENT: u128 = 10_000_000_000;

pub const USER_SEED: &[u8] = b"user";
pub const ORDER_SEED: &[u8] = b"order";
pub const POSITION_SEED: &[u8] = b"position";

pub fn order_nonce(leg: u8, is_long: bool) -> [u8; 32] {
    let mut nonce = [0u8; 32];
    nonce[0] = if is_long { 1 } else { 2 };
    nonce[1] = leg;
    nonce
}

pub fn position_kind(is_long: bool) -> u8 {
    if is_long {
        1
    } else {
        2
    }
}

pub fn user_address(program: &Pubkey, store: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[USER_SEED, store.as_ref(), owner.as_ref()], program).0
}

pub fn order_address(program: &Pubkey, store: &Pubkey, owner: &Pubkey, leg: u8, is_long: bool) -> Pubkey {
    let nonce = order_nonce(leg, is_long);
    Pubkey::find_program_address(&[ORDER_SEED, store.as_ref(), owner.as_ref(), &nonce], program).0
}

pub fn position_address(
    program: &Pubkey,
    store: &Pubkey,
    owner: &Pubkey,
    market_token: &Pubkey,
    collateral: &Pubkey,
    is_long: bool,
) -> Pubkey {
    let kind = [position_kind(is_long)];
    Pubkey::find_program_address(
        &[POSITION_SEED, store.as_ref(), owner.as_ref(), market_token.as_ref(), collateral.as_ref(), &kind],
        program,
    )
    .0
}

const MARKET_LEN: usize = 8 + 9168;
const MARKET_DISCRIMINATOR: [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
const POSITION_LEN: usize = 8 + 672;
const POSITION_DISCRIMINATOR: [u8; 8] = [170, 188, 143, 228, 122, 64, 247, 208];
const ORDER_DISCRIMINATOR: [u8; 8] = [134, 173, 223, 185, 77, 86, 28, 51];

const POOLS_OFFSET: usize = 8 + 1952;
const BORROWING_FACTOR_POOL: usize = 8;
const FUNDING_PER_SIZE_FOR_LONG_POOL: usize = 9;
const FUNDING_PER_SIZE_FOR_SHORT_POOL: usize = 10;
const CLOSE_FEE_FACTOR_OFFSET: usize = 8 + 560;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketState {
    pub store: Pubkey,
    pub market_token: Pubkey,
    pub index_token: Pubkey,
    pub long_token: Pubkey,
    pub short_token: Pubkey,
    pub close_fee_factor: u128,
    pub borrowing_factor: [u128; 2],
    pub funding_per_size: [[u128; 2]; 2],
}

impl MarketState {
    pub fn token_side(&self, mint: &Pubkey) -> Option<bool> {
        if *mint == self.long_token {
            Some(true)
        } else if *mint == self.short_token {
            Some(false)
        } else {
            None
        }
    }
}

pub fn read_market(info: &AccountInfo, program: &Pubkey) -> Result<MarketState> {
    require_keys_eq!(*info.owner, *program, OracleError::InvalidGmTradeAccount);
    let data = info.try_borrow_data()?;
    require!(data.len() == MARKET_LEN, OracleError::InvalidGmTradeAccount);
    require!(data[..8] == MARKET_DISCRIMINATOR, OracleError::InvalidGmTradeAccount);

    let key_at = |i: usize| Pubkey::new_from_array(data[i..i + 32].try_into().unwrap());
    let u128_at = |i: usize| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    let pool = |index: usize| -> [u128; 2] {
        let base = POOLS_OFFSET + 64 * index;
        let (long, short) = (u128_at(base + 32), u128_at(base + 48));
        match data[base + 16] {
            0 => [long, short],
            _ => [long.div_ceil(2), long / 2],
        }
    };
    Ok(MarketState {
        store: key_at(8 + 208),
        market_token: key_at(8 + 80),
        index_token: key_at(8 + 112),
        long_token: key_at(8 + 144),
        short_token: key_at(8 + 176),
        close_fee_factor: u128_at(CLOSE_FEE_FACTOR_OFFSET),
        borrowing_factor: pool(BORROWING_FACTOR_POOL),
        funding_per_size: [pool(FUNDING_PER_SIZE_FOR_LONG_POOL), pool(FUNDING_PER_SIZE_FOR_SHORT_POOL)],
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PositionState {
    pub size_in_tokens: u128,
    pub collateral_amount: u128,
    pub size_in_usd: u128,
    pub borrowing_factor: u128,
    pub funding_fee_amount_per_size: u128,
}

impl PositionState {
    pub fn is_empty(&self) -> bool {
        self.size_in_usd == 0 && self.size_in_tokens == 0 && self.collateral_amount == 0
    }
}

fn is_missing(info: &AccountInfo) -> bool {
    *info.owner == anchor_lang::system_program::ID && info.data_is_empty()
}

pub fn read_position(
    info: &AccountInfo,
    program: &Pubkey,
    owner: &Pubkey,
    market_token: &Pubkey,
    collateral: &Pubkey,
    is_long: bool,
) -> Result<Option<PositionState>> {
    if is_missing(info) {
        return Ok(None);
    }
    require_keys_eq!(*info.owner, *program, OracleError::InvalidGmTradeAccount);
    let data = info.try_borrow_data()?;
    require!(data.len() == POSITION_LEN, OracleError::InvalidGmTradeAccount);
    require!(data[..8] == POSITION_DISCRIMINATOR, OracleError::InvalidGmTradeAccount);
    require!(data[8 + 34] == position_kind(is_long), OracleError::InvalidGmTradeAccount);
    require!(
        data[8 + 48..8 + 80] == owner.to_bytes()
            && data[8 + 80..8 + 112] == market_token.to_bytes()
            && data[8 + 112..8 + 144] == collateral.to_bytes(),
        OracleError::InvalidGmTradeAccount
    );
    let u128_at = |i: usize| u128::from_le_bytes(data[8 + i..8 + i + 16].try_into().unwrap());
    Ok(Some(PositionState {
        size_in_tokens: u128_at(176),
        collateral_amount: u128_at(192),
        size_in_usd: u128_at(208),
        borrowing_factor: u128_at(224),
        funding_fee_amount_per_size: u128_at(240),
    }))
}

pub fn order_exists(info: &AccountInfo, program: &Pubkey) -> Result<bool> {
    if is_missing(info) {
        return Ok(false);
    }
    require_keys_eq!(*info.owner, *program, OracleError::InvalidGmTradeAccount);
    let data = info.try_borrow_data()?;
    require!(data.len() >= 8 && data[..8] == ORDER_DISCRIMINATOR, OracleError::InvalidGmTradeAccount);
    Ok(true)
}

pub fn token_balance(info: &AccountInfo, mint: &Pubkey, owner: &Pubkey) -> Result<u64> {
    if is_missing(info) {
        return Ok(0);
    }
    require!(
        *info.owner == anchor_spl::token::ID || *info.owner == anchor_spl::token_2022::ID,
        OracleError::InvalidGmTradeAccount
    );
    let data = info.try_borrow_data()?;
    require!(data.len() >= 72, OracleError::InvalidGmTradeAccount);
    require!(data[..32] == mint.to_bytes() && data[32..64] == owner.to_bytes(), OracleError::InvalidGmTradeAccount);
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    const ATA_PROGRAM: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    Pubkey::find_program_address(&[owner.as_ref(), anchor_spl::token::ID.as_ref(), mint.as_ref()], &ATA_PROGRAM).0
}

pub const PREPARE_USER: [u8; 8] = [190, 173, 143, 193, 139, 80, 231, 133];
pub const PREPARE_POSITION: [u8; 8] = [178, 215, 55, 90, 137, 15, 108, 15];
pub const CREATE_ORDER_V2: [u8; 8] = [200, 157, 3, 182, 3, 164, 162, 240];
pub const CLOSE_ORDER_V2: [u8; 8] = [213, 217, 98, 100, 225, 205, 76, 184];
pub const CLOSE_EMPTY_POSITION: [u8; 8] = [175, 105, 138, 38, 237, 235, 250, 59];

pub const MARKET_INCREASE: u8 = 3;
pub const MARKET_DECREASE: u8 = 4;
pub const NO_SWAP: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderParams {
    pub kind: u8,
    pub decrease_position_swap_type: Option<u8>,
    pub execution_lamports: u64,
    pub swap_path_length: u8,
    pub initial_collateral_delta_amount: u64,
    pub size_delta_value: u128,
    pub is_long: bool,
    pub is_collateral_long: bool,
    pub min_output: Option<u128>,
    pub trigger_price: Option<u128>,
    pub acceptable_price: Option<u128>,
    pub should_unwrap_native_token: bool,
    pub valid_from_ts: Option<i64>,
}

impl OrderParams {
    pub fn encode(&self, out: &mut Vec<u8>) {
        fn option<T: AnchorSerialize>(out: &mut Vec<u8>, value: &Option<T>) {
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    value.serialize(out).unwrap();
                }
            }
        }
        out.push(self.kind);
        option(out, &self.decrease_position_swap_type);
        out.extend_from_slice(&self.execution_lamports.to_le_bytes());
        out.push(self.swap_path_length);
        out.extend_from_slice(&self.initial_collateral_delta_amount.to_le_bytes());
        out.extend_from_slice(&self.size_delta_value.to_le_bytes());
        out.push(self.is_long as u8);
        out.push(self.is_collateral_long as u8);
        option(out, &self.min_output);
        option(out, &self.trigger_price);
        option(out, &self.acceptable_price);
        out.push(self.should_unwrap_native_token as u8);
        option(out, &self.valid_from_ts);
    }
}

pub fn create_order_data(nonce: &[u8; 32], params: &OrderParams, callback_version: Option<u8>) -> Vec<u8> {
    let mut data = CREATE_ORDER_V2.to_vec();
    data.extend_from_slice(nonce);
    params.encode(&mut data);
    match callback_version {
        None => data.push(0),
        Some(v) => data.extend_from_slice(&[1, v]),
    }
    data
}

pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        require!(self.data.len() >= len, OracleError::MalformedInstruction);
        let (head, tail) = self.data.split_at(len);
        self.data = tail;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => err!(OracleError::MalformedInstruction),
        }
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_le_bytes(self.take(16)?.try_into().unwrap()))
    }

    pub fn bytes32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }

    pub fn string(&mut self) -> Result<()> {
        let len = u32::from_le_bytes(self.take(4)?.try_into().unwrap());
        self.take(len as usize).map(|_| ())
    }

    pub fn option<T>(&mut self, read: fn(&mut Self) -> Result<T>) -> Result<Option<T>> {
        match self.bool()? {
            false => Ok(None),
            true => read(self).map(Some),
        }
    }

    pub fn order_params(&mut self) -> Result<OrderParams> {
        Ok(OrderParams {
            kind: self.u8()?,
            decrease_position_swap_type: self.option(Self::u8)?,
            execution_lamports: self.u64()?,
            swap_path_length: self.u8()?,
            initial_collateral_delta_amount: self.u64()?,
            size_delta_value: self.u128()?,
            is_long: self.bool()?,
            is_collateral_long: self.bool()?,
            min_output: self.option(Self::u128)?,
            trigger_price: self.option(Self::u128)?,
            acceptable_price: self.option(Self::u128)?,
            should_unwrap_native_token: self.bool()?,
            valid_from_ts: self.option(Self::i64)?,
        })
    }

    pub fn finish(&self) -> Result<()> {
        require!(self.data.is_empty(), OracleError::MalformedInstruction);
        Ok(())
    }
}
