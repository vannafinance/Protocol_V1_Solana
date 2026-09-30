use super::kamino::{MAINNET_USDC, NATIVE_MINT};
use super::*;
use anchor_lang::solana_program::instruction::AccountMeta;

pub const JUPITER: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
pub const WHIRLPOOL: Pubkey = pubkey!("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
pub const EVENT_AUTHORITY: Pubkey = pubkey!("D8cy77BBepLMngZx6ZukaTff5hCt1HrWyKk3Hnd9oitf");
pub const PROGRAM_AUTHORITY: Pubkey = pubkey!("GGztQqQ6pCPaJQnNpXBgELr5cs3WwDakRbh1iEMzjgSJ");
pub const PROGRAM_WSOL: Pubkey = pubkey!("g7dD1FHSemkUQrX1Eak37wzvDjscgBW2pFCENwjLdMX");
pub const PROGRAM_USDC: Pubkey = pubkey!("DVCeozFGbe6ew3eWTnZByjHeYqTq1cvbrB7JJhkLxaRJ");

pub const POOL: Pubkey = pubkey!("Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE");
pub const POOL_VAULT_SOL: Pubkey = pubkey!("EUuUbDcafPrmVTD5M6qoJAoyyNbihBhugADAxRMn5he9");
pub const POOL_VAULT_USDC: Pubkey = pubkey!("2WLWEuKDgkDUccTpbwYp1GToYktiSB1cXvreHUwiSUVP");
pub const POOL_TICK_ARRAYS: [Pubkey; 3] = [
    pubkey!("FdtvWk8j5u1a64YK2Uxk9eXxKZJTwLHDGx8aJPbJyw2Q"),
    pubkey!("6hA1LN1fzCiXqymDiQXeBFn5da1b7STP1L7JmDc6hR3M"),
    pubkey!("D3461zSTVPNdBFPRk2b6zpqQ93g2LW5Kw2potgMdxNJP"),
];
pub const POOL_ORACLE: Pubkey = pubkey!("FoKYKtRpD25TKzBMndysKpgPqbj8AdLXjfpYHXn9PGTX");

pub const ROUTE: [u8; 8] = [229, 23, 203, 151, 122, 227, 173, 42];
pub const SHARED_ACCOUNTS_ROUTE: [u8; 8] = [193, 32, 155, 51, 65, 214, 156, 129];
const SWAP_WHIRLPOOL: u8 = 17;

#[derive(Clone, Copy)]
pub struct RouteArgs {
    pub in_amount: u64,
    pub quoted_out_amount: u64,
    pub slippage_bps: u16,
    pub platform_fee_bps: u8,
}

impl RouteArgs {
    pub fn exact_in(in_amount: u64) -> Self {
        Self { in_amount, quoted_out_amount: 1, slippage_bps: 0, platform_fee_bps: 0 }
    }
}

fn plan_and_tail(args: RouteArgs) -> Vec<u8> {
    let mut data = 1u32.to_le_bytes().to_vec();
    data.extend_from_slice(&[SWAP_WHIRLPOOL, 1, 100, 0, 1]);
    data.extend_from_slice(&args.in_amount.to_le_bytes());
    data.extend_from_slice(&args.quoted_out_amount.to_le_bytes());
    data.extend_from_slice(&args.slippage_bps.to_le_bytes());
    data.push(args.platform_fee_bps);
    data
}

pub fn route_data(args: RouteArgs) -> Vec<u8> {
    let mut data = ROUTE.to_vec();
    data.extend(plan_and_tail(args));
    data
}

pub fn shared_route_data(args: RouteArgs) -> Vec<u8> {
    let mut data = SHARED_ACCOUNTS_ROUTE.to_vec();
    data.push(0);
    data.extend(plan_and_tail(args));
    data
}

fn whirlpool_hop(authority: &Pubkey, sol_account: &Pubkey, usdc_account: &Pubkey) -> Vec<AccountMeta> {
    let mut metas = vec![
        AccountMeta::new_readonly(WHIRLPOOL, false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(*authority, false),
        AccountMeta::new(POOL, false),
        AccountMeta::new(*sol_account, false),
        AccountMeta::new(POOL_VAULT_SOL, false),
        AccountMeta::new(*usdc_account, false),
        AccountMeta::new(POOL_VAULT_USDC, false),
    ];
    metas.extend(POOL_TICK_ARRAYS.iter().map(|t| AccountMeta::new(*t, false)));
    metas.push(AccountMeta::new(POOL_ORACLE, false));
    metas
}

pub fn route_accounts(authority: &Pubkey, source: &Pubkey, destination: &Pubkey) -> Vec<AccountMeta> {
    let mut metas = vec![
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(*authority, false),
        AccountMeta::new(*source, false),
        AccountMeta::new(*destination, false),
        AccountMeta::new_readonly(JUPITER, false),
        AccountMeta::new_readonly(MAINNET_USDC, false),
        AccountMeta::new_readonly(JUPITER, false),
        AccountMeta::new_readonly(EVENT_AUTHORITY, false),
        AccountMeta::new_readonly(JUPITER, false),
    ];
    metas.extend(whirlpool_hop(authority, source, destination));
    metas
}

pub fn shared_route_accounts(authority: &Pubkey, source: &Pubkey, destination: &Pubkey) -> Vec<AccountMeta> {
    let mut metas = vec![
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(PROGRAM_AUTHORITY, false),
        AccountMeta::new_readonly(*authority, false),
        AccountMeta::new(*source, false),
        AccountMeta::new(PROGRAM_WSOL, false),
        AccountMeta::new(PROGRAM_USDC, false),
        AccountMeta::new(*destination, false),
        AccountMeta::new_readonly(NATIVE_MINT, false),
        AccountMeta::new_readonly(MAINNET_USDC, false),
        AccountMeta::new_readonly(JUPITER, false),
        AccountMeta::new_readonly(JUPITER, false),
        AccountMeta::new_readonly(EVENT_AUTHORITY, false),
        AccountMeta::new_readonly(JUPITER, false),
    ];
    metas.extend(whirlpool_hop(&PROGRAM_AUTHORITY, &PROGRAM_WSOL, &PROGRAM_USDC));
    metas
}

pub fn jupiter_direct_ix(data: Vec<u8>, mut accounts: Vec<AccountMeta>, signer: &Pubkey) -> Instruction {
    for meta in accounts.iter_mut().filter(|m| m.pubkey == *signer) {
        meta.is_signer = true;
    }
    Instruction { program_id: JUPITER, accounts, data }
}
