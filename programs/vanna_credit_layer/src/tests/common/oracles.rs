use super::*;
use solana_account::Account as SvmAccount;
use vanna_oracle::{get_price, OraclePrice};
use vanna_oracle::reference::kamino_scope;

include!("../fixtures/oracles/snapshot.rs");

macro_rules! oracle_fixture {
    ($key:literal) => {
        ($key, include_bytes!(concat!("../fixtures/oracles/", $key, ".acct")).as_slice())
    };
}

pub const SCOPE_PRICES: &str = kamino_scope::ORACLE_PRICES;
pub const PYTH_USDC: &str = "Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX";
pub const PYTH_USDT: &str = "HT2PLQBcG5EiCcNSaMHAjSgd9F98ecpATbk4Sk5oYuM";
pub const PYTH_SOL: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
pub const PYTH_JITOSOL: &str = "AxaxyeDT8JnWERSaTKvFXvPKkEdxnamKSqpWbsSjYg1g";
pub const PYTH_JUPSOL_RATE: &str = "D7UqeBmCEmhGXGYfi2y9RfoCa7t1Xw5iZLBeYZ3sxFSe";
pub const PYTH_JUPUSD: &str = "AqSwMCZYnEdnGoFCSLtWVnWf4xyCJuePcztmYWq8SBwp";

pub(crate) const ORACLE_ACCOUNTS: [(&str, &[u8]); 15] = [
    oracle_fixture!("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH"),
    oracle_fixture!("Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX"),
    oracle_fixture!("HT2PLQBcG5EiCcNSaMHAjSgd9F98ecpATbk4Sk5oYuM"),
    oracle_fixture!("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE"),
    oracle_fixture!("AxaxyeDT8JnWERSaTKvFXvPKkEdxnamKSqpWbsSjYg1g"),
    oracle_fixture!("D7UqeBmCEmhGXGYfi2y9RfoCa7t1Xw5iZLBeYZ3sxFSe"),
    oracle_fixture!("AqSwMCZYnEdnGoFCSLtWVnWf4xyCJuePcztmYWq8SBwp"),
    oracle_fixture!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
    oracle_fixture!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"),
    oracle_fixture!("So11111111111111111111111111111111111111112"),
    oracle_fixture!("J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn"),
    oracle_fixture!("jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v"),
    oracle_fixture!("JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD"),
    oracle_fixture!("Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh"),
    oracle_fixture!("XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB"),
];

pub fn key(s: &str) -> Pubkey {
    s.parse().unwrap()
}

pub fn fixture_account(key: &str) -> SvmAccount {
    let blob = ORACLE_ACCOUNTS.iter().find(|(k, _)| *k == key).expect("fixture").1;
    let owner = Pubkey::new_from_array(blob[..32].try_into().unwrap());
    let lamports = u64::from_le_bytes(blob[32..40].try_into().unwrap());
    SvmAccount { lamports, data: blob[41..].to_vec(), owner, executable: blob[40] == 1, rent_epoch: 0 }
}

pub fn load_oracle_fixtures(svm: &mut LiteSVM) {
    for (k, _) in ORACLE_ACCOUNTS {
        svm.set_account(key(k), fixture_account(k)).unwrap();
    }
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = ORACLE_SNAPSHOT_UNIX_TIMESTAMP;
    svm.set_sysvar(&clock);
}

pub struct MainnetAsset {
    pub name: &'static str,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub decimals: u8,
    pub oracle: OracleConfig,
}

fn scope(chains: ([u16; 4], [u16; 4]), max_age_secs: u32, twap_bps: u16, pyth: Option<&str>, factor: Option<&str>) -> OracleConfig {
    OracleConfig {
        scope_prices: key(SCOPE_PRICES),
        scope_chain: chains.0,
        scope_twap_chain: chains.1,
        pyth_price: pyth.map(key).unwrap_or_default(),
        pyth_factor: factor.map(key).unwrap_or_default(),
        max_age_secs,
        max_twap_divergence_bps: twap_bps,
        max_confidence_bps: 200,
        ..OracleConfig::default()
    }
}

pub fn mainnet_assets() -> Vec<MainnetAsset> {
    let spl = anchor_spl::token::ID;
    let t22 = TOKEN_2022;
    vec![
        MainnetAsset { name: "USDC", mint: key(known_mints::USDC), token_program: spl, decimals: 6, oracle: scope(kamino_scope::USDC, 180, 300, Some(PYTH_USDC), None) },
        MainnetAsset { name: "USDT", mint: key(known_mints::USDT), token_program: spl, decimals: 6, oracle: scope(kamino_scope::USDT, 300, 300, Some(PYTH_USDT), None) },
        MainnetAsset { name: "SOL", mint: key(known_mints::WSOL), token_program: spl, decimals: 9, oracle: scope(kamino_scope::SOL, 120, 1_000, Some(PYTH_SOL), None) },
        MainnetAsset { name: "JitoSOL", mint: key(known_mints::JITOSOL), token_program: spl, decimals: 9, oracle: scope(kamino_scope::JITOSOL, 120, 1_000, None, None) },
        MainnetAsset { name: "JupSOL", mint: key(known_mints::JUPSOL), token_program: spl, decimals: 9, oracle: scope(kamino_scope::JUPSOL, 120, 1_000, Some(PYTH_JUPSOL_RATE), Some(PYTH_SOL)) },
        MainnetAsset { name: "JupUSD", mint: key(known_mints::JUPUSD), token_program: spl, decimals: 6, oracle: OracleConfig { max_twap_divergence_bps: 300, ..pyth_oracle_at(PYTH_JUPUSD, 180) } },
        MainnetAsset { name: "NVDAx", mint: key(known_mints::NVDAX), token_program: t22, decimals: 8, oracle: scope(kamino_scope::NVDAX, 300, 500, None, None) },
        MainnetAsset { name: "TSLAx", mint: key(known_mints::TSLAX), token_program: t22, decimals: 8, oracle: scope(kamino_scope::TSLAX, 300, 500, None, None) },
    ]
}

fn pyth_oracle_at(account: &str, max_age_secs: u32) -> OracleConfig {
    OracleConfig { pyth_price: key(account), max_age_secs, max_confidence_bps: 200, ..OracleConfig::default() }
}

pub struct PricedAsset {
    pub mint: Pubkey,
    pub oracle: OracleConfig,
}

pub fn asset_config_for(asset: &MainnetAsset) -> PricedAsset {
    PricedAsset { mint: asset.mint, oracle: asset.oracle }
}

pub fn price_offchain(asset: &PricedAsset, accounts: &[(Pubkey, SvmAccount)], unix_timestamp: i64) -> Result<OraclePrice> {
    let mut owned: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> =
        accounts.iter().map(|(k, a)| (*k, a.lamports, a.data.clone(), a.owner)).collect();
    let infos: Vec<AccountInfo> = owned
        .iter_mut()
        .map(|(key, lamports, data, owner)| AccountInfo::new(key, false, false, lamports, data, owner, false))
        .collect();
    let clock = Clock { unix_timestamp, ..Clock::default() };
    get_price(&asset.mint, &asset.oracle, &infos, &clock)
}

pub fn scope_entry(prices: &SvmAccount, index: u16) -> (u64, u64, i64) {
    let at = 40 + 56 * usize::from(index);
    let u64_at = |o: usize| u64::from_le_bytes(prices.data[at + o..at + o + 8].try_into().unwrap());
    (u64_at(0), u64_at(8), u64_at(24) as i64)
}
