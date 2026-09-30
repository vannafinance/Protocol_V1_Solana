use crate::common::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use flate2::read::GzDecoder;
pub use vanna_oracle::gmtrade_accounts::OrderParams;
use vanna_oracle::gmtrade_accounts::{CLOSE_EMPTY_POSITION, CLOSE_ORDER_V2, MARKET_DECREASE, MARKET_INCREASE, PREPARE_POSITION, PREPARE_USER};
use litesvm::LiteSVM;
use solana_account::Account as SvmAccount;
use solana_last_restart_slot::LastRestartSlot;
use std::io::Read;

#[allow(dead_code)]
mod snapshot {
    include!("../../fixtures/gmtrade/snapshot.rs");
}
pub use snapshot::*;

pub const GMTRADE: Pubkey = pubkey!("Gmso1uvJnLbawvw7yezdfCDcPydwW2s2iqG3w6MDucLo");
pub const STORE: Pubkey = pubkey!("CTDLvGGXnoxvqLyTpGzdGLg9pD6JexKxKXSV8tqqo8bN");
pub const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const SCOPE_USDC: Pubkey = pubkey!("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH");
pub const SCOPE_ETH: Pubkey = pubkey!("3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C");
pub const PYTH_USDC: Pubkey = pubkey!("Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX");
pub const PYTH_ETH: Pubkey = pubkey!("42amVS4KgzR9rA28tkVYqVXjq9Qa8dcZQMbH5EYFX6XC");
pub const SCOPE_ETH_PRICE: u16 = 246;
pub const SCOPE_ETH_TWAP: u16 = 53;
pub const BTC_FEED: [u8; 32] = [0xB4; 32];
pub const BTC_PRICE: f64 = 100_000.0;
pub const USD: u128 = vanna_oracle::gmtrade_accounts::USD_UNIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Market {
    pub name: &'static str,
    pub market: Pubkey,
    pub market_token: Pubkey,
    pub index: Pubkey,
    pub leg: u8,
}

pub const ETH: Market = Market {
    name: "ETH",
    market: pubkey!("6EnZdBzJsGznoh857PuhbrnrzWYGtZe6xMZiQjAPyFGT"),
    market_token: pubkey!("DAY6Qr1FKgJQFvjJAhFUZUWHzx8UbbbkRmt6G6AYswWG"),
    index: pubkey!("EthK4kKnQQUd1Ae1w7sdiMAUaJwq2RMwr7AtscXEdEsF"),
    leg: 0,
};

pub const BTC: Market = Market {
    name: "BTC",
    market: Pubkey::new_from_array([0xB1; 32]),
    market_token: Pubkey::new_from_array([0xB2; 32]),
    index: Pubkey::new_from_array([0xB3; 32]),
    leg: 1,
};

pub const MARKETS: [Market; 2] = [ETH, BTC];

macro_rules! gmtrade_fixture {
    ($key:literal) => {
        ($key, include_bytes!(concat!("../../fixtures/gmtrade/", $key, ".acct")).as_slice())
    };
}

const ACCOUNTS: [(&str, &[u8]); 11] = [
    gmtrade_fixture!("CTDLvGGXnoxvqLyTpGzdGLg9pD6JexKxKXSV8tqqo8bN"),
    gmtrade_fixture!("6EnZdBzJsGznoh857PuhbrnrzWYGtZe6xMZiQjAPyFGT"),
    gmtrade_fixture!("DAY6Qr1FKgJQFvjJAhFUZUWHzx8UbbbkRmt6G6AYswWG"),
    gmtrade_fixture!("EthK4kKnQQUd1Ae1w7sdiMAUaJwq2RMwr7AtscXEdEsF"),
    gmtrade_fixture!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
    gmtrade_fixture!("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH"),
    gmtrade_fixture!("3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C"),
    gmtrade_fixture!("Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX"),
    gmtrade_fixture!("42amVS4KgzR9rA28tkVYqVXjq9Qa8dcZQMbH5EYFX6XC"),
    gmtrade_fixture!("HKyZojVnZWfb4W6teG4RhSLwymK8TPPow15tvJ2QaEUj"),
    gmtrade_fixture!("Gym6sWaM7GQP5J5uXHiKX1NawJ7LoDi5ER4NbZEN33Gi"),
];

const GMSOL_ELF_GZ: &[u8] = include_bytes!("../../fixtures/gmtrade/gmsol_store.so.gz");

pub fn fixture(key: &Pubkey) -> SvmAccount {
    let key = key.to_string();
    let blob = ACCOUNTS.iter().find(|(k, _)| *k == key).expect("fixture").1;
    let owner = Pubkey::new_from_array(blob[..32].try_into().unwrap());
    let lamports = u64::from_le_bytes(blob[32..40].try_into().unwrap());
    SvmAccount { lamports, data: blob[41..].to_vec(), owner, executable: blob[40] == 1, rent_epoch: 0 }
}

pub fn load_gmtrade(svm: &mut LiteSVM) {
    let mut elf = Vec::new();
    GzDecoder::new(GMSOL_ELF_GZ).read_to_end(&mut elf).unwrap();
    svm.add_program(GMTRADE, &elf).unwrap();
    for (key, _) in ACCOUNTS {
        let key: Pubkey = key.parse().unwrap();
        svm.set_account(key, fixture(&key)).unwrap();
    }
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = GMTRADE_SNAPSHOT_UNIX_TIMESTAMP;
    svm.set_sysvar(&clock);
    svm.set_sysvar(&LastRestartSlot { last_restart_slot: MAINNET_LAST_RESTART_SLOT });
    load_btc_market(svm);
}

fn load_btc_market(svm: &mut LiteSVM) {
    let mut market = fixture(&ETH.market);
    let data = &mut market.data;
    let mut name = [0u8; 64];
    name[..18].copy_from_slice(b"BTC/USD[USDC-USDC]");
    data[8 + 16..8 + 80].copy_from_slice(&name);
    data[8 + 80..8 + 112].copy_from_slice(BTC.market_token.as_ref());
    data[8 + 112..8 + 144].copy_from_slice(BTC.index.as_ref());
    svm.set_account(BTC.market, market).unwrap();
    svm.set_account(BTC.index, fixture(&ETH.index)).unwrap();
    set_btc_price(svm, BTC_PRICE);
}

pub fn set_btc_price(svm: &mut LiteSVM, price: f64) {
    set_pyth(svm, BTC_FEED, (price * 1e8).round() as i64, -8, GMTRADE_SNAPSHOT_UNIX_TIMESTAMP);
}

pub const MAINNET_LAST_RESTART_SLOT: u64 = 246_464_040;

pub fn venue_account_of(margin: &Pubkey) -> Pubkey {
    vanna_credit_layer::interface::venue_account_address(margin, &STORE).0
}

pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &GMTRADE).0
}

pub fn user_account(owner: &Pubkey) -> Pubkey {
    vanna_oracle::gmtrade_accounts::user_address(&GMTRADE, &STORE, owner)
}

pub fn store_wallet() -> Pubkey {
    Pubkey::find_program_address(&[b"store_wallet", STORE.as_ref()], &GMTRADE).0
}

pub fn order_account(owner: &Pubkey, m: &Market, is_long: bool) -> Pubkey {
    vanna_oracle::gmtrade_accounts::order_address(&GMTRADE, &STORE, owner, m.leg, is_long)
}

pub fn position_account(owner: &Pubkey, m: &Market, is_long: bool) -> Pubkey {
    vanna_oracle::gmtrade_accounts::position_address(&GMTRADE, &STORE, owner, &m.market_token, &USDC, is_long)
}

pub fn escrow_account(owner: &Pubkey, m: &Market, is_long: bool) -> Pubkey {
    get_associated_token_address(&order_account(owner, m, is_long), &USDC)
}

pub fn idle_account(venue_account: &Pubkey) -> Pubkey {
    get_associated_token_address(venue_account, &USDC)
}

pub fn leg_keys(venue_account: &Pubkey, m: &Market) -> Vec<Pubkey> {
    let mut keys = vec![m.market];
    for is_long in [true, false] {
        keys.extend([order_account(venue_account, m, is_long), escrow_account(venue_account, m, is_long), position_account(venue_account, m, is_long)]);
    }
    keys
}

pub fn market_order(increase: bool, is_long: bool, collateral: u64, size_usd: u128) -> OrderParams {
    OrderParams {
        kind: if increase { MARKET_INCREASE } else { MARKET_DECREASE },
        decrease_position_swap_type: None,
        execution_lamports: 300_000,
        swap_path_length: 0,
        initial_collateral_delta_amount: collateral,
        size_delta_value: size_usd,
        is_long,
        is_collateral_long: true,
        min_output: None,
        trigger_price: None,
        acceptable_price: None,
        should_unwrap_native_token: false,
        valid_from_ts: None,
    }
}

pub fn create_order_data(nonce: [u8; 32], params: &OrderParams, callback_version: Option<u8>) -> Vec<u8> {
    vanna_oracle::gmtrade_accounts::create_order_data(&nonce, params, callback_version)
}

pub fn order_data(m: &Market, params: &OrderParams) -> Vec<u8> {
    create_order_data(vanna_oracle::gmtrade_accounts::order_nonce(m.leg, params.is_long), params, None)
}

pub fn prepare_position_data(params: &OrderParams) -> Vec<u8> {
    let mut data = PREPARE_POSITION.to_vec();
    params.encode(&mut data);
    data
}

pub fn prepare_user_data() -> Vec<u8> {
    PREPARE_USER.to_vec()
}

pub fn close_order_data(reason: &str) -> Vec<u8> {
    let mut data = CLOSE_ORDER_V2.to_vec();
    reason.to_string().serialize(&mut data).unwrap();
    data
}

pub fn close_empty_position_data() -> Vec<u8> {
    CLOSE_EMPTY_POSITION.to_vec()
}

fn unset() -> AccountMeta {
    AccountMeta::new_readonly(GMTRADE, false)
}

pub fn prepare_user_accounts(venue_account: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*venue_account, false),
        AccountMeta::new_readonly(STORE, false),
        AccountMeta::new(user_account(venue_account), false),
        AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
    ]
}

pub fn prepare_position_accounts(venue_account: &Pubkey, m: &Market, is_long: bool) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*venue_account, false),
        AccountMeta::new_readonly(STORE, false),
        AccountMeta::new_readonly(m.market, false),
        AccountMeta::new(position_account(venue_account, m, is_long), false),
        AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
    ]
}

pub fn create_order_accounts(venue_account: &Pubkey, m: &Market, is_long: bool, increase: bool) -> Vec<AccountMeta> {
    let order = order_account(venue_account, m, is_long);
    let escrow = escrow_account(venue_account, m, is_long);
    let or_unset = |meta: AccountMeta| if increase { meta } else { unset() };
    vec![
        AccountMeta::new(*venue_account, false),
        AccountMeta::new_readonly(*venue_account, false),
        AccountMeta::new_readonly(STORE, false),
        AccountMeta::new(m.market, false),
        AccountMeta::new(user_account(venue_account), false),
        AccountMeta::new(order, false),
        AccountMeta::new(position_account(venue_account, m, is_long), false),
        or_unset(AccountMeta::new_readonly(USDC, false)),
        AccountMeta::new_readonly(USDC, false),
        AccountMeta::new_readonly(USDC, false),
        AccountMeta::new_readonly(USDC, false),
        or_unset(AccountMeta::new(escrow, false)),
        AccountMeta::new(escrow, false),
        AccountMeta::new(escrow, false),
        AccountMeta::new(escrow, false),
        or_unset(AccountMeta::new(idle_account(venue_account), false)),
        AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
        unset(),
        unset(),
        unset(),
        unset(),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(GMTRADE, false),
    ]
}

pub fn close_order_accounts(venue_account: &Pubkey, m: &Market, is_long: bool, increase: bool) -> Vec<AccountMeta> {
    let order = order_account(venue_account, m, is_long);
    let escrow = escrow_account(venue_account, m, is_long);
    let refund = idle_account(venue_account);
    let initial = |meta: AccountMeta| if increase { meta } else { unset() };
    let output = |meta: AccountMeta| if increase { unset() } else { meta };
    vec![
        AccountMeta::new(*venue_account, false),
        AccountMeta::new(STORE, false),
        AccountMeta::new(store_wallet(), false),
        AccountMeta::new(*venue_account, false),
        AccountMeta::new(*venue_account, false),
        AccountMeta::new(*venue_account, false),
        AccountMeta::new(user_account(venue_account), false),
        unset(),
        AccountMeta::new(order, false),
        initial(AccountMeta::new_readonly(USDC, false)),
        output(AccountMeta::new_readonly(USDC, false)),
        AccountMeta::new_readonly(USDC, false),
        AccountMeta::new_readonly(USDC, false),
        initial(AccountMeta::new(escrow, false)),
        output(AccountMeta::new(escrow, false)),
        AccountMeta::new(escrow, false),
        AccountMeta::new(escrow, false),
        initial(AccountMeta::new(refund, false)),
        output(AccountMeta::new(refund, false)),
        AccountMeta::new(refund, false),
        AccountMeta::new(refund, false),
        AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        AccountMeta::new_readonly(anchor_spl::token::ID, false),
        AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
        unset(),
        unset(),
        unset(),
        unset(),
        AccountMeta::new_readonly(event_authority(), false),
        AccountMeta::new_readonly(GMTRADE, false),
    ]
}

pub fn close_empty_position_accounts(venue_account: &Pubkey, m: &Market, is_long: bool) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*venue_account, false),
        AccountMeta::new_readonly(STORE, false),
        AccountMeta::new(position_account(venue_account, m, is_long), false),
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct Position {
    pub size_in_tokens: u128,
    pub collateral_amount: u128,
    pub size_in_usd: u128,
    pub borrowing_factor: u128,
    pub funding_fee_amount_per_size: u128,
}

impl Position {
    pub const EMPTY: Self = Self { size_in_tokens: 0, collateral_amount: 0, size_in_usd: 0, borrowing_factor: 0, funding_fee_amount_per_size: 0 };

    pub fn state(&self) -> vanna_oracle::gmtrade_accounts::PositionState {
        vanna_oracle::gmtrade_accounts::PositionState {
            size_in_tokens: self.size_in_tokens,
            collateral_amount: self.collateral_amount,
            size_in_usd: self.size_in_usd,
            borrowing_factor: self.borrowing_factor,
            funding_fee_amount_per_size: self.funding_fee_amount_per_size,
        }
    }
}

fn live_position_key(is_long: bool) -> Pubkey {
    if is_long { LIVE_LONG_POSITION } else { LIVE_SHORT_POSITION }.parse().unwrap()
}

pub fn write_position(svm: &mut LiteSVM, owner: &Pubkey, m: &Market, is_long: bool, position: &Position) {
    let mut account = fixture(&live_position_key(is_long));
    let kind = [if is_long { 1 } else { 2 }];
    let (key, bump) = Pubkey::find_program_address(
        &[b"position", STORE.as_ref(), owner.as_ref(), m.market_token.as_ref(), USDC.as_ref(), &kind],
        &GMTRADE,
    );
    let data = &mut account.data;
    data[8 + 1] = bump;
    data[8 + 48..8 + 80].copy_from_slice(owner.as_ref());
    data[8 + 80..8 + 112].copy_from_slice(m.market_token.as_ref());
    for (offset, value) in [
        (176, position.size_in_tokens),
        (192, position.collateral_amount),
        (208, position.size_in_usd),
        (224, position.borrowing_factor),
        (240, position.funding_fee_amount_per_size),
    ] {
        data[8 + offset..8 + offset + 16].copy_from_slice(&value.to_le_bytes());
    }
    svm.set_account(key, account).unwrap();
}

pub fn live_position(is_long: bool) -> Position {
    let data = fixture(&live_position_key(is_long)).data;
    let u128_at = |i: usize| u128::from_le_bytes(data[8 + i..8 + i + 16].try_into().unwrap());
    Position {
        size_in_tokens: u128_at(176),
        collateral_amount: u128_at(192),
        size_in_usd: u128_at(208),
        borrowing_factor: u128_at(224),
        funding_fee_amount_per_size: u128_at(240),
    }
}
