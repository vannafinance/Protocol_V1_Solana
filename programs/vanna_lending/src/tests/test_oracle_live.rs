//! Live check, against mainnet right now, of every price the program uses and of the health
//! factor it computes from them.
//!
//! One RPC call reads the current accounts the program prices from: Kamino's Scope prices, the
//! Pyth feed accounts, the Kamino reserves behind cUSDC / cSOL and the mints. Then:
//! - every asset is priced with the program's own facade (`oracle::get_price`) and compared with
//!   independent market prices: Jupiter's price API, for the xStocks also the stock price × the
//!   token's multiplier, and for the cTokens Kamino's own reserve totals × the underlying's price;
//! - the same accounts are loaded into LiteSVM with the program, one margin account holds all ten
//!   assets, and the health factor the program computes on-chain must equal the one recomputed
//!   from the facade's prices, and be close to the one from market prices.
//!
//! Needs network access (curl), so it is ignored by default:
//!
//!     cargo test --manifest-path programs/vanna_lending/Cargo.toml --test test_oracle_live -- --ignored --nocapture
//!
//! `MAINNET_RPC_URL` overrides the public RPC.

mod common;

use anchor_lang::__private::base64::{engine::general_purpose::STANDARD, Engine};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token_2022::spl_token_2022::extension::scaled_ui_amount::ScaledUiAmountConfig;
use anchor_spl::token_2022::spl_token_2022::extension::{BaseStateWithExtensions, StateWithExtensions};
use common::kamino::{set_token_balance, KaminoReserve, KLEND, SOL_RESERVE, USDC_RESERVE};
use common::oracles::*;
use common::*;
use litesvm::LiteSVM;
use serde_json::{json, Value};
use solana_account::Account as SvmAccount;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use vanna_lending::events::{Borrowed, CollateralWithdrawn};
use vanna_lending::math::health::{calculate_health, CollateralValuation, DebtValuation};
use vanna_lending::oracle::{OraclePrice, PriceStatus};

const JUPITER_PRICE: &str = "https://lite-api.jup.ag/price/v3?ids=";
const KAMINO_METRICS: &str =
    "https://api.kamino.finance/kamino-market/7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF/reserves/metrics?env=mainnet-beta";
const COMPUTE_BUDGET: Pubkey = pubkey!("ComputeBudget111111111111111111111111111111");
/// USDC borrowed against the whole basket.
const BORROW_USDC: u64 = 5_000;

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

fn curl(args: &[&str]) -> Value {
    let out = Command::new("curl").args(["-s", "-m", "30"]).args(args).output().expect("curl must be installed");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("bad JSON from {args:?}: {e}"))
}

fn rpc(method: &str, params: Value) -> Value {
    let url = std::env::var("MAINNET_RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
    let response = curl(&["-H", "Content-Type: application/json", "-d", &body, &url]);
    response.get("result").cloned().unwrap_or_else(|| panic!("{method} failed: {response}"))
}

// ---------------------------------------------------------------------------
// Assets, live accounts, market references
// ---------------------------------------------------------------------------

/// An asset as registered, and how close its price must be to the market.
struct LiveAsset {
    asset: MainnetAsset,
    /// A lending pool (borrowable).
    pool: bool,
    /// A Kamino cToken's reserve.
    receipt: Option<KaminoReserve>,
    /// Allowed gap to the market reference, in percent.
    tolerance_pct: f64,
    /// Whole tokens the margin account holds in the health-factor test.
    held: u64,
}

/// The eight assets with their production oracles (`common::oracles::mainnet_assets`), then cUSDC
/// and cSOL priced as their underlying through the real Kamino reserves.
///
/// Tolerances: stablecoins 0.5%; SOL 1%; liquid-staked SOL 2% (priced at the stake-pool rate, the
/// market trades slightly below it); xStocks 3% (outside US market hours Chainlink holds the last
/// close while the tokens keep trading); cTokens 2%.
fn live_assets() -> Vec<LiveAsset> {
    let mut assets: Vec<LiveAsset> = mainnet_assets()
        .into_iter()
        .map(|asset| {
            let (pool, tolerance_pct, held) = match asset.name {
                "USDC" => (true, 0.5, 1_000),
                "USDT" => (true, 0.5, 1_000),
                "SOL" => (true, 1.0, 10),
                "JitoSOL" | "JupSOL" => (false, 2.0, 5),
                "JupUSD" => (false, 0.5, 1_000),
                "NVDAx" => (false, 3.0, 10),
                _ => (false, 3.0, 5), // TSLAx
            };
            LiveAsset { asset, pool, receipt: None, tolerance_pct, held }
        })
        .collect();
    // Kamino's SOL reserve mints ~1,150 cSOL per SOL (both cTokens have 6 decimals), so a cSOL is
    // worth cents; hold enough for it to weigh in the health factor.
    for (name, reserve, underlying, held) in [("cUSDC", USDC_RESERVE, "USDC", 1_000), ("cSOL", SOL_RESERVE, "SOL", 10_000)] {
        let base = assets.iter().find(|a| a.asset.name == underlying).unwrap().asset.oracle;
        assets.push(LiveAsset {
            asset: MainnetAsset {
                name,
                mint: reserve.collateral_mint,
                token_program: anchor_spl::token::ID,
                decimals: 6,
                oracle: OracleConfig { klend_reserve: reserve.reserve, klend_program: KLEND, ..base },
            },
            pool: false,
            receipt: Some(reserve),
            tolerance_pct: 2.0,
            held,
        });
    }
    assets
}

/// Every account the program reads for these assets, read in one call, and the time now.
struct Live {
    accounts: Vec<(Pubkey, SvmAccount)>,
    now: i64,
}

impl Live {
    fn account(&self, key: &Pubkey) -> &SvmAccount {
        &self.accounts.iter().find(|(k, _)| k == key).expect("fetched").1
    }
}

fn fetch_live(assets: &[LiveAsset]) -> Live {
    let mut keys: Vec<Pubkey> = assets.iter().flat_map(|a| std::iter::once(a.asset.mint).chain(a.asset.oracle.accounts())).collect();
    keys.sort();
    keys.dedup();
    let names: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    let result = rpc("getMultipleAccounts", json!([names, { "encoding": "base64" }]));
    let accounts = keys
        .iter()
        .zip(result["value"].as_array().unwrap())
        .map(|(key, v)| {
            assert!(!v.is_null(), "{key} not found on mainnet");
            let data = STANDARD.decode(v["data"][0].as_str().unwrap()).unwrap();
            let owner = v["owner"].as_str().unwrap().parse().unwrap();
            let account = SvmAccount { lamports: v["lamports"].as_u64().unwrap(), data, owner, executable: v["executable"].as_bool().unwrap(), rent_epoch: 0 };
            (*key, account)
        })
        .collect();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    Live { accounts, now }
}

/// An independent market price for one whole token.
struct Reference {
    usd: f64,
    /// For an xStock: the stock price × the token's current multiplier (shares per token).
    per_token_from_stock: Option<f64>,
    source: String,
}

/// The current Scaled UI multiplier of a Token-2022 mint, as Token-2022 applies it.
fn ui_multiplier(mint: &SvmAccount, now: i64) -> f64 {
    let state = StateWithExtensions::<anchor_spl::token_2022::spl_token_2022::state::Mint>::unpack(&mint.data).unwrap();
    let config = state.get_extension::<ScaledUiAmountConfig>().unwrap();
    if now >= i64::from(config.new_multiplier_effective_timestamp) {
        f64::from(config.new_multiplier)
    } else {
        f64::from(config.multiplier)
    }
}

fn market_references(assets: &[LiveAsset], live: &Live) -> Vec<Reference> {
    let mints: Vec<String> = assets.iter().filter(|a| a.receipt.is_none()).map(|a| a.asset.mint.to_string()).collect();
    let jupiter = curl(&[&format!("{JUPITER_PRICE}{}", mints.join(","))]);
    let kamino = curl(&[KAMINO_METRICS]);
    let jupiter_usd = |mint: &Pubkey| -> f64 {
        jupiter[mint.to_string()]["usdPrice"].as_f64().unwrap_or_else(|| panic!("no Jupiter price for {mint}"))
    };
    assets
        .iter()
        .map(|a| match &a.receipt {
            None => {
                let stock = jupiter[a.asset.mint.to_string()]["stockData"]["price"].as_f64();
                let per_token_from_stock = stock.map(|s| s * ui_multiplier(live.account(&a.asset.mint), live.now));
                let source = match (stock, per_token_from_stock) {
                    (Some(s), Some(p)) => format!("Jupiter; stock ${s:.2} × multiplier = ${p:.2}"),
                    _ => "Jupiter".into(),
                };
                Reference { usd: jupiter_usd(&a.asset.mint), per_token_from_stock, source }
            }
            Some(r) => {
                let metrics = kamino
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["reserve"] == r.reserve.to_string())
                    .expect("Kamino reserve metrics");
                let total_liquidity: f64 = metrics["totalSupply"].as_str().unwrap().parse().unwrap();
                let ctoken_mint = live.account(&r.collateral_mint);
                let ctoken_supply = u64::from_le_bytes(ctoken_mint.data[36..44].try_into().unwrap()) as f64 / 1e6;
                let rate = total_liquidity / ctoken_supply;
                let underlying = jupiter_usd(&r.liquidity_mint);
                let source = format!("Kamino API {rate:.6} underlying per cToken × Jupiter ${underlying:.4}");
                Reference { usd: rate * underlying, per_token_from_stock: None, source }
            }
        })
        .collect()
}

/// The program's price of `asset` from the live accounts (its facade, run in this process).
fn facade_price(asset: &LiveAsset, live: &Live) -> OraclePrice {
    price_offchain(&asset_config_for(&asset.asset), &live.accounts, live.now).unwrap()
}

/// USD for one whole token, as the program values it.
fn per_token_usd(price: &OraclePrice, decimals: u8) -> f64 {
    price.value_of(10u64.pow(decimals as u32), decimals, false).unwrap() as f64 / 1e9
}

/// Which source the facade used: Scope while its chain is fresh, else the Pyth fallback.
fn source_used(asset: &LiveAsset, price: &OraclePrice, live: &Live) -> &'static str {
    let oracle = &asset.asset.oracle;
    if !oracle.uses_scope() {
        return "pyth";
    }
    let scope = live.account(&oracle.scope_prices);
    let chain_time = oracle.scope_chain.iter().filter(|e| **e != u16::MAX).map(|e| scope_entry(scope, *e).2).min().unwrap();
    if price.timestamp == chain_time { "scope" } else { "pyth fallback" }
}

fn pct(a: f64, b: f64) -> f64 {
    (a - b) / b * 100.0
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

/// Every asset, the Kamino cTokens included, priced by the program's facade from the current
/// mainnet accounts: every check passes, and the price is within its tolerance of the market.
#[test]
#[ignore = "reads live mainnet data over the network"]
fn live_prices_match_the_market() {
    let assets = live_assets();
    let live = fetch_live(&assets);
    let references = market_references(&assets, &live);

    println!("\n{:8} {:>14} {:>14} {:>8} {:>6}  {:14} market reference", "asset", "program", "market", "diff", "age", "source");
    let mut failures = Vec::new();
    for (asset, reference) in assets.iter().zip(&references) {
        let price = facade_price(asset, &live);
        let usd = per_token_usd(&price, asset.asset.decimals);
        let diff = pct(usd, reference.usd);
        println!(
            "{:8} {:>14.6} {:>14.6} {:>7.3}% {:>5}s  {:14} {}",
            asset.asset.name,
            usd,
            reference.usd,
            diff,
            live.now - price.timestamp,
            source_used(asset, &price, &live),
            reference.source
        );
        if price.status != PriceStatus::ALL_CHECKS {
            failures.push(format!("{}: price checks failed ({:?})", asset.asset.name, price.status));
        }
        if diff.abs() > asset.tolerance_pct {
            failures.push(format!("{}: {usd} is {diff:.3}% from the market {}", asset.asset.name, reference.usd));
        }
        if let Some(from_stock) = reference.per_token_from_stock {
            let diff = pct(usd, from_stock);
            if diff.abs() > asset.tolerance_pct {
                failures.push(format!("{}: {usd} is {diff:.3}% from the stock price × multiplier {from_stock}", asset.asset.name));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---------------------------------------------------------------------------
// Health factor
// ---------------------------------------------------------------------------

/// Creates `owner`'s token account for a Token-2022 mint (with the account extensions the mint
/// requires) and sets its balance.
fn fund_token_2022(svm: &mut LiteSVM, payer: &Keypair, owner: &Pubkey, mint: &Pubkey, amount: u64) {
    let ix = spl_associated_token_account::instruction::create_associated_token_account_idempotent(&payer.pubkey(), owner, mint, &TOKEN_2022);
    send(svm, payer, &[ix], &[]).expect("create the Token-2022 account");
    let account_key = ata_for(owner, mint, &TOKEN_2022);
    let mut account = svm.get_account(&account_key).unwrap();
    account.data[64..72].copy_from_slice(&amount.to_le_bytes());
    svm.set_account(account_key, account).unwrap();
}

fn compute_budget(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET, accounts: vec![], data }
}

fn raw(asset: &LiveAsset, whole: u64) -> u64 {
    whole * 10u64.pow(asset.asset.decimals as u32)
}

/// The margin account's collateral groups in its slot order, without `named`.
fn collateral_groups(svm: &LiteSVM, owner: &Pubkey, margin: &Pubkey, assets: &[LiveAsset], named: &str) -> Vec<AccountMeta> {
    let account = fetch_margin(svm, owner);
    let mut groups = Vec::new();
    for index in account.active_collateral_indexes() {
        let asset = assets.iter().find(|a| fetch_asset_config(svm, &a.asset.mint).asset_index == index).unwrap();
        if asset.asset.name != named {
            groups.extend(collateral_group_with(&asset.asset.mint, &asset.asset.token_program, margin));
        }
    }
    groups
}

fn health_wad(collateral: &[u128], debt: &[u128]) -> u128 {
    let c: Vec<_> = collateral.iter().map(|v| CollateralValuation { collateral_value: *v }).collect();
    let d: Vec<_> = debt.iter().map(|v| DebtValuation { debt_value: *v }).collect();
    calculate_health(&c, &d).unwrap().borrow_health_factor_wad
}

/// One margin account holds all ten assets (the xStocks as real Token-2022 NVDAx / TSLAx, the
/// cTokens against the real Kamino reserves) and borrows USDC, then withdraws an NVDAx. At each
/// step the health factor the program computes on-chain equals the one recomputed from the
/// facade's prices, and is within 2% of the one from market prices.
#[test]
#[ignore = "reads live mainnet data over the network"]
fn live_margin_health_factor_prices_every_asset() {
    let assets = live_assets();
    let live = fetch_live(&assets);
    let references = market_references(&assets, &live);

    let mut svm = setup_svm();
    for (key, account) in &live.accounts {
        svm.set_account(*key, account.clone()).unwrap();
    }
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = live.now;
    svm.set_sysvar(&clock);

    // Protocol, every asset with its production oracle, and a USDC pool with a lender.
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 16)], &[]).unwrap();
    for asset in &assets {
        let extra: Vec<Pubkey> = asset.receipt.iter().map(|r| asset_config_pda(&r.liquidity_mint).0).collect();
        let m = &asset.asset;
        let ix = ix_admin_register_asset_with(&a, &a, &m.mint, &m.token_program, m.oracle, &extra, 0, 8_000, 8_500, 500, true, asset.pool);
        send(&mut svm, &admin, &[ix], &[]).unwrap_or_else(|e| panic!("register {}: {e:?}", m.name));
    }
    let usdc = assets.iter().find(|a| a.asset.name == "USDC").unwrap();
    let usdc_mint = usdc.asset.mint;
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &usdc_mint, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();
    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &usdc_mint, raw(usdc, 50_000));
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &usdc_mint, raw(usdc, 50_000), 1)], &[]).unwrap();

    // A margin account holding every asset.
    let user = funded_keypair(&mut svm);
    let u = user.pubkey();
    let (margin, _) = margin_pda(&u);
    send(&mut svm, &user, &[ix_user_create_margin(&u, &u)], &[]).unwrap();
    for asset in &assets {
        let m = &asset.asset;
        let amount = raw(asset, asset.held);
        if m.token_program == TOKEN_2022 {
            fund_token_2022(&mut svm, &user, &u, &m.mint, amount);
        } else {
            set_token_balance(&mut svm, &u, &m.mint, amount);
        }
        let ix = ix_user_deposit_collateral_with(&u, &margin, &m.mint, &m.token_program, amount);
        send(&mut svm, &user, &[ix], &[]).unwrap_or_else(|e| panic!("deposit {}: {e:?}", m.name));
    }
    send(&mut svm, &user, &[ix_user_open_debt_position(&u, &u, &margin, &usdc_mint)], &[]).unwrap();
    let oracles: Vec<Pubkey> = assets.iter().flat_map(|a| a.asset.oracle.accounts()).collect();

    // What the program must compute: every asset valued at its facade price (USDC projected with
    // the borrowed funds), against the USDC debt.
    let prices: Vec<OraclePrice> = assets.iter().map(|a| facade_price(a, &live)).collect();
    let holdings = |usdc_held: u64, nvdax_held: u64| -> Vec<(usize, u64)> {
        assets
            .iter()
            .enumerate()
            .map(|(i, a)| match a.asset.name {
                "USDC" => (i, usdc_held),
                "NVDAx" => (i, nvdax_held),
                _ => (i, raw(a, a.held)),
            })
            .collect()
    };
    let expected = |held: &[(usize, u64)], debt: u64| -> u128 {
        let collateral: Vec<u128> = held.iter().map(|(i, amount)| prices[*i].value_of(*amount, assets[*i].asset.decimals, false).unwrap()).collect();
        let usdc_index = assets.iter().position(|a| a.asset.name == "USDC").unwrap();
        health_wad(&collateral, &[prices[usdc_index].value_of(debt, 6, true).unwrap()])
    };
    let from_market = |held: &[(usize, u64)], debt: u64| -> f64 {
        let collateral: f64 = held.iter().map(|(i, amount)| *amount as f64 / 10f64.powi(assets[*i].asset.decimals as i32) * references[*i].usd).sum();
        let usdc_index = assets.iter().position(|a| a.asset.name == "USDC").unwrap();
        collateral / (debt as f64 / 1e6 * references[usdc_index].usd)
    };

    // Borrow: USDC is the named asset, the nine others come from the scan.
    let debt = raw(usdc, BORROW_USDC);
    let groups = collateral_groups(&svm, &u, &margin, &assets, "USDC");
    let ix = ix_user_borrow(&u, &margin, &usdc_mint, debt, u128::MAX, &with_oracles(groups, &oracles));
    let meta = send(&mut svm, &user, &[compute_budget(1_400_000), ix], &[]).expect("borrow against the whole basket");
    let borrowed: Borrowed = event(&meta.logs);
    let nvdax = assets.iter().find(|a| a.asset.name == "NVDAx").unwrap();
    let held = holdings(raw(usdc, usdc.held) + debt, raw(nvdax, nvdax.held));

    println!("\n{:8} {:>10} {:>12} {:>14} {:>14}", "asset", "held", "price", "program value", "market value");
    for (i, amount) in &held {
        let (asset, whole) = (&assets[*i], *amount as f64 / 10f64.powi(assets[*i].asset.decimals as i32));
        let value = prices[*i].value_of(*amount, asset.asset.decimals, false).unwrap() as f64 / 1e9;
        let price = per_token_usd(&prices[*i], asset.asset.decimals);
        println!("{:8} {whole:>10.2} {price:>12.4} {value:>14.2} {:>14.2}", asset.asset.name, whole * references[*i].usd);
    }
    let market_hf = from_market(&held, debt);
    println!(
        "debt {BORROW_USDC} USDC -> health factor on-chain {:.6}, from the facade {:.6}, from market prices {market_hf:.6} (CU {})",
        borrowed.borrow_health_factor_wad as f64 / 1e18,
        expected(&held, debt) as f64 / 1e18,
        meta.compute_units_consumed
    );
    assert_eq!(borrowed.assets, debt);
    assert_eq!(borrowed.borrow_health_factor_wad, expected(&held, debt), "on-chain health factor = the facade's");
    assert!(pct(borrowed.borrow_health_factor_wad as f64 / 1e18, market_hf).abs() < 2.0, "health factor close to the market's");

    // Withdraw one NVDAx: the Token-2022 xStock is now the named asset.
    let mut others = collateral_groups(&svm, &u, &margin, &assets, "NVDAx");
    others.extend(debt_group_metas(&usdc_mint, &margin));
    let one = raw(nvdax, 1);
    let ix = ix_user_withdraw_collateral_with(&u, &margin, &nvdax.asset.mint, &TOKEN_2022, one, 0, &with_oracles(others, &oracles));
    let meta = send(&mut svm, &user, &[compute_budget(1_400_000), ix], &[]).expect("withdraw one NVDAx");
    let withdrawn: CollateralWithdrawn = event(&meta.logs);
    let held = holdings(raw(usdc, usdc.held) + debt, raw(nvdax, nvdax.held) - one);
    println!(
        "withdraw 1 NVDAx -> health factor on-chain {:.6}, from the facade {:.6}, from market prices {:.6}",
        withdrawn.borrow_health_factor_wad as f64 / 1e18,
        expected(&held, debt) as f64 / 1e18,
        from_market(&held, debt)
    );
    assert_eq!(withdrawn.borrow_health_factor_wad, expected(&held, debt), "on-chain health factor = the facade's");
    assert!(pct(withdrawn.borrow_health_factor_wad as f64 / 1e18, from_market(&held, debt)).abs() < 2.0);
}
