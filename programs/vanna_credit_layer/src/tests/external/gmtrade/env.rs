use crate::common::kamino::set_token_balance;
use crate::common::*;
use crate::gm::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::types::TransactionResult;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer as SvmSigner;
use solana_system_interface::instruction::transfer;
use vanna_oracle::MarketBook;

pub(crate) const UNIT: u64 = 1_000_000;
pub(crate) const ORDER_LAMPORTS: u64 = 30_000_000;
pub(crate) const MAX_LEVERAGE_BPS: u32 = 50_000;

pub(crate) struct Env {
    pub(crate) svm: LiteSVM,
    pub(crate) admin: Keypair,
}

pub(crate) fn usdc_oracle() -> OracleConfig {
    OracleConfig {
        scope_prices: SCOPE_USDC,
        scope_chain: scope_chain(&[13]),
        scope_twap_chain: scope_chain(&[456]),
        pyth_price: PYTH_USDC,
        max_age_secs: 180,
        max_twap_divergence_bps: 300,
        max_confidence_bps: 200,
        ..OracleConfig::default()
    }
}

pub(crate) fn eth_oracle() -> OracleConfig {
    OracleConfig {
        scope_prices: SCOPE_ETH,
        scope_chain: scope_chain(&[SCOPE_ETH_PRICE]),
        scope_twap_chain: scope_chain(&[SCOPE_ETH_TWAP]),
        pyth_price: PYTH_ETH,
        max_age_secs: 120,
        max_twap_divergence_bps: 1_000,
        max_confidence_bps: 200,
        ..OracleConfig::default()
    }
}

pub(crate) fn btc_oracle() -> OracleConfig {
    pyth_oracle(&BTC_FEED, 120, 200)
}

pub(crate) fn index_oracle(m: &Market) -> OracleConfig {
    if *m == ETH {
        eth_oracle()
    } else {
        btc_oracle()
    }
}

pub(crate) fn market_book() -> Pubkey {
    MarketBook::address(&STORE)
}

pub(crate) fn ix_open_market_book(admin: &Pubkey, collateral_price: OracleConfig) -> Instruction {
    let mut accounts = vanna_oracle::accounts::OpenMarketBook {
        admin: *admin,
        payer: *admin,
        protocol_config: protocol_config_pda().0,
        market_book: market_book(),
        collateral_mint: USDC,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend(oracle_metas(&collateral_price.accounts().collect::<Vec<_>>()));
    Instruction {
        program_id: ORACLE,
        accounts,
        data: vanna_oracle::instruction::OpenMarketBook { venue: STORE, gmtrade_program: GMTRADE, collateral_price }.data(),
    }
}

pub(crate) fn ix_list_market(admin: &Pubkey, m: &Market, index_mint: &Pubkey, index_price: OracleConfig, max_leverage_bps: u32, trading_enabled: bool) -> Instruction {
    let mut accounts = vanna_oracle::accounts::ListMarket {
        admin: *admin,
        protocol_config: protocol_config_pda().0,
        market_book: market_book(),
        market: m.market,
        index_mint: *index_mint,
    }
    .to_account_metas(None);
    accounts.extend(oracle_metas(&index_price.accounts().collect::<Vec<_>>()));
    Instruction {
        program_id: ORACLE,
        accounts,
        data: vanna_oracle::instruction::ListMarket { index_price, max_leverage_bps, trading_enabled }.data(),
    }
}

pub(crate) fn ix_list(admin: &Pubkey, m: &Market, trading_enabled: bool) -> Instruction {
    ix_list_market(admin, m, &m.index, index_oracle(m), MAX_LEVERAGE_BPS, trading_enabled)
}

pub(crate) fn ix_admin_register_venue(admin: &Pubkey, oracle: &Pubkey) -> Instruction {
    ix_register_venue(admin, &STORE, &USDC, oracle)
}

pub(crate) fn ix_register_venue(admin: &Pubkey, venue: &Pubkey, settle: &Pubkey, oracle: &Pubkey) -> Instruction {
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts: vanna_credit_layer::accounts::AdminRegisterVenue {
            admin: *admin,
            payer: *admin,
            protocol_config: protocol_config_pda().0,
            asset_config: asset_config_pda(venue).0,
            settle_asset: asset_config_pda(settle).0,
            oracle: *oracle,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: vanna_credit_layer::instruction::AdminRegisterVenue { venue: *venue }.data(),
    }
}

pub(crate) fn ix_enable_venue(admin: &Pubkey, enabled: bool) -> Instruction {
    ix_admin_update_asset_config(admin, &STORE, 0, 0, 10_000, 0, enabled, false)
}

pub(crate) fn setup() -> Env {
    let mut svm = setup_svm();
    load_gmtrade(&mut svm);
    let admin = funded_keypair(&mut svm);
    let a = admin.pubkey();
    send(&mut svm, &admin, &[ix_initialize_protocol(&a, &Pubkey::new_unique(), &a, 8)], &[]).unwrap();
    let usdc = ix_admin_register_asset_with(&a, &a, &USDC, &anchor_spl::token::ID, usdc_oracle(), &[], 0, 8_000, 8_500, 500, true, true);
    send(&mut svm, &admin, &usdc, &[]).unwrap();
    send(&mut svm, &admin, &[ix_admin_initialize_reserve(&a, &a, &USDC, DEFAULT_RATE_CURVE, 1_000, 0, 0, 0)], &[]).unwrap();

    send(&mut svm, &admin, &[ix_open_market_book(&a, usdc_oracle())], &[]).expect("market book");
    for m in &MARKETS {
        send(&mut svm, &admin, &[ix_list(&a, m, true)], &[]).unwrap_or_else(|e| panic!("list {}: {:?}", m.name, e.meta.logs));
    }
    send(&mut svm, &admin, &[ix_admin_register_venue(&a, &ORACLE), ix_enable_venue(&a, true)], &[]).expect("venue");
    send(&mut svm, &admin, &[ix_admin_register_integration(&a, &a, &GMTRADE, VALIDATOR)], &[]).unwrap();

    let lender = funded_keypair(&mut svm);
    set_token_balance(&mut svm, &lender.pubkey(), &USDC, 100_000 * UNIT);
    send(&mut svm, &lender, &[ix_lender_supply(&lender.pubkey(), &USDC, 100_000 * UNIT, 1)], &[]).unwrap();
    Env { svm, admin }
}

pub(crate) struct Trader {
    pub(crate) user: Keypair,
    pub(crate) margin: Pubkey,
    pub(crate) venue_account: Pubkey,
}

pub(crate) fn venue_account_keys(venue_account: &Pubkey) -> Vec<Pubkey> {
    let mut keys = vec![market_book()];
    keys.extend(usdc_oracle().accounts());
    keys.push(idle_account(venue_account));
    for m in &MARKETS {
        keys.extend(leg_keys(venue_account, m));
        keys.extend(index_oracle(m).accounts());
    }
    keys
}

pub(crate) fn venue_account_oracle_segment(venue_account: &Pubkey) -> Vec<AccountMeta> {
    oracle_segment(&venue_account_keys(venue_account))
}

pub(crate) fn validator_segment() -> Vec<AccountMeta> {
    agent_segment(&VALIDATOR, &[market_book()])
}

pub(crate) fn venue_group(venue_account: &Pubkey) -> Vec<AccountMeta> {
    vec![AccountMeta::new_readonly(asset_config_pda(&STORE).0, false), AccountMeta::new_readonly(*venue_account, false)]
}

impl Env {
    pub(crate) fn trader(&mut self, collateral: u64) -> Trader {
        let user = funded_keypair(&mut self.svm);
        let u = user.pubkey();
        let (margin, _) = margin_pda(&u);
        set_token_balance(&mut self.svm, &u, &USDC, collateral);
        send(&mut self.svm, &user, &[ix_user_create_margin(&u, &u)], &[]).unwrap();
        send(&mut self.svm, &user, &[ix_user_deposit_collateral(&u, &margin, &USDC, collateral)], &[]).unwrap();
        Trader { user, margin, venue_account: venue_account_of(&margin) }
    }

    pub(crate) fn groups(&self, t: &Trader, skip: &[Pubkey], skip_debt: bool) -> Vec<AccountMeta> {
        let margin = fetch_margin(&self.svm, &t.user.pubkey());
        let usdc_index = fetch_asset_config(&self.svm, &USDC).asset_index;
        let mut groups = Vec::new();
        for index in margin.active_collateral_indexes() {
            let key = if index == usdc_index { USDC } else { STORE };
            if skip.contains(&key) {
                continue;
            }
            groups.extend(match key == USDC {
                true => collateral_group_metas(&USDC, &t.margin),
                false => venue_group(&t.venue_account),
            });
        }
        if margin.debt_count > 0 && !skip_debt {
            groups.extend(debt_group_metas(&USDC, &t.margin));
        }
        groups
    }

    pub(crate) fn health(&self, t: &Trader, skip: &[Pubkey], skip_debt: bool) -> Vec<AccountMeta> {
        let mut metas = self.groups(t, skip, skip_debt);
        metas.extend(venue_account_oracle_segment(&t.venue_account));
        metas
    }

    pub(crate) fn execute_rest(&self, t: &Trader) -> Vec<AccountMeta> {
        let usdc_vault = margin_vault_ata(&t.margin, &USDC);
        let mut metas: Vec<AccountMeta> = self
            .groups(t, &[], false)
            .into_iter()
            .map(|meta| if meta.pubkey == usdc_vault { AccountMeta::new(usdc_vault, false) } else { meta })
            .collect();
        metas.extend(venue_account_oracle_segment(&t.venue_account));
        metas.extend(validator_segment());
        metas
    }

    pub(crate) fn borrow(&mut self, t: &Trader, amount: u64) -> TransactionResult {
        let health = self.health(t, &[USDC], true);
        let ix = ix_user_borrow(&t.user.pubkey(), &t.margin, &USDC, amount, u128::MAX, &health);
        send(&mut self.svm, &t.user, &[compute_budget(), ix], &[])
    }

    pub(crate) fn execute(&mut self, t: &Trader, data: Vec<u8>, cpi: &[AccountMeta]) -> TransactionResult {
        let rest = self.execute_rest(t);
        let u = t.user.pubkey();
        let ix = ix_margin_execute(&u, &GMTRADE, Some(&STORE), data, cpi, &rest, 0);
        let usdc_account = ix_create_vault(&u, &t.venue_account, &USDC, &anchor_spl::token::ID);
        send(&mut self.svm, &t.user, &[compute_budget(), fund_venue_account(&u, &t.venue_account), usdc_account, ix], &[])
    }

    pub(crate) fn prepare(&mut self, t: &Trader, m: &Market, is_long: bool) {
        if self.svm.get_account(&user_account(&t.venue_account)).is_none_or(|a| a.data.is_empty()) {
            self.execute(t, prepare_user_data(), &prepare_user_accounts(&t.venue_account)).expect("prepare_user");
        }
        let params = market_order(true, is_long, 0, 0);
        let res = self.execute(t, prepare_position_data(&params), &prepare_position_accounts(&t.venue_account, m, is_long));
        res.unwrap_or_else(|e| panic!("prepare_position {}: {:?}", m.name, e.meta.logs));
    }

    pub(crate) fn order(&mut self, t: &Trader, m: &Market, params: &OrderParams) -> TransactionResult {
        self.create_escrow(t, m, params.is_long);
        let increase = params.kind == vanna_oracle::gmtrade_accounts::MARKET_INCREASE;
        let cpi = create_order_accounts(&t.venue_account, m, params.is_long, increase);
        self.execute(t, order_data(m, params), &cpi)
    }

    pub(crate) fn create_escrow(&mut self, t: &Trader, m: &Market, is_long: bool) {
        let order = order_account(&t.venue_account, m, is_long);
        let ix = anchor_spl::associated_token::spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &t.user.pubkey(),
            &order,
            &USDC,
            &anchor_spl::token::ID,
        );
        send(&mut self.svm, &t.user, &[ix], &[]).unwrap();
    }

    pub(crate) fn execute_increase(&mut self, t: &Trader, m: &Market, is_long: bool, position: &Position) {
        self.svm.set_account(escrow_account(&t.venue_account, m, is_long), solana_account::Account::default()).unwrap();
        self.svm.set_account(order_account(&t.venue_account, m, is_long), solana_account::Account::default()).unwrap();
        write_position(&mut self.svm, &t.venue_account, m, is_long, position);
    }

    pub(crate) fn open(&mut self, t: &Trader, m: &Market, is_long: bool, collateral: u64, size: u64) -> Position {
        self.prepare(t, m, is_long);
        let res = self.order(t, m, &market_order(true, is_long, collateral, usd(size)));
        res.unwrap_or_else(|e| panic!("{} order: {:?}", m.name, e.meta.logs));
        let fee = size as f64 * 0.0002;
        let position = open_position(self, m, is_long, size as f64, collateral as f64 / 1e6 - fee, index_price(m));
        self.execute_increase(t, m, is_long, &position);
        position
    }

    pub(crate) fn execute_close(&mut self, t: &Trader, m: &Market, is_long: bool, output: u64) {
        write_position(&mut self.svm, &t.venue_account, m, is_long, &Position::EMPTY);
        self.svm.set_account(escrow_account(&t.venue_account, m, is_long), solana_account::Account::default()).unwrap();
        self.svm.set_account(order_account(&t.venue_account, m, is_long), solana_account::Account::default()).unwrap();
        let idle = token_balance_or_zero(&self.svm, &idle_account(&t.venue_account));
        set_token_balance(&mut self.svm, &t.venue_account, &USDC, idle + output);
    }

    pub(crate) fn close(&mut self, t: &Trader, m: &Market, is_long: bool, position: &Position, output: u64) {
        let res = self.order(t, m, &market_order(false, is_long, 0, position.size_in_usd));
        res.unwrap_or_else(|e| panic!("{} close: {:?}", m.name, e.meta.logs));
        self.execute_close(t, m, is_long, output);
    }

    pub(crate) fn settle(&mut self, caller: &Keypair, t: &Trader) -> TransactionResult {
        let ix = ix_public_venue_settle(&caller.pubkey(), &t.margin, &t.venue_account);
        send(&mut self.svm, caller, &[compute_budget(), ix], &[])
    }

    pub(crate) fn margin_usdc(&self, t: &Trader) -> u64 {
        token_balance_or_zero(&self.svm, &margin_vault_ata(&t.margin, &USDC))
    }

    pub(crate) fn venue_index(&self) -> u16 {
        fetch_asset_config(&self.svm, &STORE).asset_index
    }

    pub(crate) fn is_venue_active(&self, t: &Trader) -> bool {
        fetch_margin(&self.svm, &t.user.pubkey()).is_collateral_active(self.venue_index())
    }

    pub(crate) fn legs(&self, t: &Trader) -> u64 {
        fetch_margin(&self.svm, &t.user.pubkey()).legs_of(self.venue_index())
    }

    pub(crate) fn health_factor(&mut self, t: &Trader) -> u128 {
        let health = self.health(t, &[USDC], false);
        let u = t.user.pubkey();
        let ix = ix_user_withdraw_collateral(&u, &t.margin, &USDC, 1, 0, &health);
        let sim = simulate(&mut self.svm, &t.user, &[compute_budget(), ix]).unwrap_or_else(|e| panic!("withdraw simulation: {:?}", e.meta.logs));
        event::<vanna_credit_layer::events::CollateralWithdrawn>(&sim.meta.logs).borrow_health_factor_wad
    }

    pub(crate) fn set_eth_price(&mut self, price: f64) {
        let value = (price * 1e8).round() as u64;
        let scope = fixture(&SCOPE_ETH);
        let ts = |index: u16| {
            let at = 40 + 56 * index as usize;
            i64::from_le_bytes(scope.data[at + 24..at + 32].try_into().unwrap())
        };
        let entries = [(SCOPE_ETH_PRICE, value, 8, ts(SCOPE_ETH_PRICE)), (SCOPE_ETH_TWAP, value, 8, ts(SCOPE_ETH_TWAP))];
        set_scope_prices(&mut self.svm, &SCOPE_ETH, &entries);
        let feed: [u8; 32] = hex_feed(vanna_oracle::reference::gmtrade::eth::PYTH_FEED);
        set_price(&mut self.svm, &PYTH_ETH, feed, value as i64, 0, -8, GMTRADE_SNAPSHOT_UNIX_TIMESTAMP);
    }
}

pub(crate) fn simulate(
    svm: &mut LiteSVM,
    payer: &Keypair,
    ixs: &[Instruction],
) -> std::result::Result<litesvm::types::SimulatedTransactionInfo, litesvm::types::FailedTransactionMetadata> {
    svm.expire_blockhash();
    let msg = solana_message::Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &svm.latest_blockhash());
    let tx = solana_transaction::versioned::VersionedTransaction::try_new(solana_message::VersionedMessage::Legacy(msg), &[payer]).unwrap();
    svm.simulate_transaction(tx)
}

pub(crate) fn snapshot_prices() -> (f64, f64) {
    let entry = |account: &solana_account::Account, index: u16| {
        let at = 40 + 56 * index as usize;
        let value = u64::from_le_bytes(account.data[at..at + 8].try_into().unwrap());
        let exp = u64::from_le_bytes(account.data[at + 8..at + 16].try_into().unwrap());
        value as f64 / 10f64.powi(exp as i32)
    };
    (entry(&fixture(&SCOPE_ETH), SCOPE_ETH_PRICE), entry(&fixture(&SCOPE_USDC), 13))
}

pub(crate) fn index_price(m: &Market) -> f64 {
    if *m == ETH {
        snapshot_prices().0
    } else {
        BTC_PRICE
    }
}

pub(crate) fn hex_feed(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

pub(crate) fn token_balance_or_zero(svm: &LiteSVM, key: &Pubkey) -> u64 {
    match svm.get_account(key) {
        Some(account) if account.data.len() >= 72 => u64::from_le_bytes(account.data[64..72].try_into().unwrap()),
        _ => 0,
    }
}

pub(crate) fn open_position(env: &Env, m: &Market, is_long: bool, size: f64, collateral: f64, price: f64) -> Position {
    let market = env.svm.get_account(&m.market).unwrap().data;
    let u128_at = |i: usize| u128::from_le_bytes(market[i..i + 16].try_into().unwrap());
    let pool = |index: usize| 8 + 1952 + 64 * index;
    let side = if is_long { 32 } else { 48 };
    let funding = u128_at(pool(if is_long { 9 } else { 10 }) + 32);
    Position {
        size_in_tokens: (size / price * 1e8) as u128,
        collateral_amount: (collateral * 1e6) as u128,
        size_in_usd: (size * 1e6) as u128 * (USD / 1_000_000),
        borrowing_factor: u128_at(pool(8) + side),
        funding_fee_amount_per_size: funding.div_ceil(2),
    }
}

pub(crate) fn compute_budget() -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&1_400_000u32.to_le_bytes());
    Instruction { program_id: pubkey!("ComputeBudget111111111111111111111111111111"), accounts: vec![], data }
}

pub(crate) fn fund_venue_account(payer: &Pubkey, venue_account: &Pubkey) -> Instruction {
    transfer(payer, venue_account, ORDER_LAMPORTS)
}

pub(crate) fn ix_public_venue_settle(caller: &Pubkey, margin: &Pubkey, venue_account: &Pubkey) -> Instruction {
    let mut accounts = vanna_credit_layer::accounts::PublicVenueSettle {
        caller: *caller,
        protocol_config: protocol_config_pda().0,
        margin_account: *margin,
        venue_asset: asset_config_pda(&STORE).0,
        settle_asset: asset_config_pda(&USDC).0,
        settle_mint: USDC,
        venue_account: *venue_account,
        idle: idle_account(venue_account),
        margin_vault: margin_vault_ata(margin, &USDC),
        oracle: ORACLE,
        token_program: anchor_spl::token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    accounts.extend(oracle_metas(&venue_account_keys(venue_account)));
    Instruction { program_id: vanna_credit_layer::ID, accounts, data: vanna_credit_layer::instruction::PublicVenueSettle {}.data() }
}

pub(crate) fn ix_public_venue_unwind(caller: &Pubkey, margin: &Pubkey, data: Vec<u8>, cpi: &[AccountMeta], rest: &[AccountMeta]) -> Instruction {
    let mut accounts = vanna_credit_layer::accounts::PublicVenueUnwind {
        caller: *caller,
        margin_account: *margin,
        venue_asset: asset_config_pda(&STORE).0,
        integration: integration_pda(&GMTRADE),
        target_program: GMTRADE,
        validator: VALIDATOR,
    }
    .to_account_metas(None);
    accounts.extend_from_slice(cpi);
    accounts.extend_from_slice(rest);
    Instruction {
        program_id: vanna_credit_layer::ID,
        accounts,
        data: vanna_credit_layer::instruction::PublicVenueUnwind { data, call_account_count: cpi.len() as u16 }.data(),
    }
}

pub(crate) fn usd(dollars: u64) -> u128 {
    dollars as u128 * USD
}
