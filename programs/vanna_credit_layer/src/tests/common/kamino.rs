use super::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use anchor_lang::solana_program::program_option::COption;
use litesvm::LiteSVM;
use solana_account::Account as SvmAccount;
use solana_program_pack::Pack;

pub const KLEND: Pubkey = pubkey!("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
pub const INSTRUCTIONS_SYSVAR: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");
pub const MAINNET_USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const NATIVE_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
pub const CTOKEN_DECIMALS: u8 = 6;
pub const DEPOSIT_RESERVE_LIQUIDITY: [u8; 8] = [169, 201, 30, 126, 6, 205, 102, 68];
pub const REDEEM_RESERVE_COLLATERAL: [u8; 8] = [234, 117, 181, 125, 185, 142, 220, 29];

#[derive(Clone, Copy)]
pub struct KaminoReserve {
    pub reserve: Pubkey,
    pub liquidity_mint: Pubkey,
    pub liquidity_decimals: u8,
    pub supply_vault: Pubkey,
    pub collateral_mint: Pubkey,
}

pub const MAIN_MARKET: Pubkey = pubkey!("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
pub const MAIN_MARKET_AUTHORITY: Pubkey = pubkey!("9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo");

pub const USDC_RESERVE: KaminoReserve = KaminoReserve {
    reserve: pubkey!("D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59"),
    liquidity_mint: MAINNET_USDC,
    liquidity_decimals: 6,
    supply_vault: pubkey!("Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6"),
    collateral_mint: pubkey!("B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D"),
};

pub const SOL_RESERVE: KaminoReserve = KaminoReserve {
    reserve: pubkey!("d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q"),
    liquidity_mint: NATIVE_MINT,
    liquidity_decimals: 9,
    supply_vault: pubkey!("GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U"),
    collateral_mint: pubkey!("2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1"),
};

pub fn set_token_balance(svm: &mut LiteSVM, owner: &Pubkey, mint: &Pubkey, amount: u64) -> Pubkey {
    let ata = get_associated_token_address(owner, mint);
    let rent = svm.minimum_balance_for_rent_exemption(spl_token_interface::state::Account::LEN);
    let native = *mint == NATIVE_MINT;
    let state = spl_token_interface::state::Account {
        mint: *mint,
        owner: *owner,
        amount,
        delegate: COption::None,
        state: spl_token_interface::state::AccountState::Initialized,
        is_native: if native { COption::Some(rent) } else { COption::None },
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut data = vec![0u8; spl_token_interface::state::Account::LEN];
    spl_token_interface::state::Account::pack(state, &mut data).unwrap();
    let lamports = if native { rent + amount } else { rent };
    svm.set_account(ata, SvmAccount { lamports, data, owner: spl_token_interface::ID, executable: false, rent_epoch: 0 })
        .unwrap();
    ata
}

pub fn kamino_call_data(selector: [u8; 8], amount: u64) -> Vec<u8> {
    let mut data = selector.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data
}

pub fn kamino_supply_accounts(r: &KaminoReserve, owner: &Pubkey, source: &Pubkey, destination: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*owner, false),
        AccountMeta::new(r.reserve, false),
        AccountMeta::new_readonly(MAIN_MARKET, false),
        AccountMeta::new_readonly(MAIN_MARKET_AUTHORITY, false),
        AccountMeta::new_readonly(r.liquidity_mint, false),
        AccountMeta::new(r.supply_vault, false),
        AccountMeta::new(r.collateral_mint, false),
        AccountMeta::new(*source, false),
        AccountMeta::new(*destination, false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR, false),
    ]
}

pub fn kamino_redeem_accounts(r: &KaminoReserve, owner: &Pubkey, source: &Pubkey, destination: &Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*owner, false),
        AccountMeta::new_readonly(MAIN_MARKET, false),
        AccountMeta::new(r.reserve, false),
        AccountMeta::new_readonly(MAIN_MARKET_AUTHORITY, false),
        AccountMeta::new_readonly(r.liquidity_mint, false),
        AccountMeta::new(r.collateral_mint, false),
        AccountMeta::new(r.supply_vault, false),
        AccountMeta::new(*source, false),
        AccountMeta::new(*destination, false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(spl_token_interface::ID, false),
        AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR, false),
    ]
}

pub fn kamino_direct_ix(data: Vec<u8>, mut accounts: Vec<AccountMeta>) -> Instruction {
    accounts[0].is_signer = true;
    Instruction { program_id: KLEND, accounts, data }
}

pub fn reserve_rate(svm: &LiteSVM, r: &KaminoReserve) -> (u128, u64) {
    let data = svm.get_account(&r.reserve).unwrap().data;
    let u64_at = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let u128_at = |i: usize| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    let total_sf = ((u64_at(224) as u128) << 60) + u128_at(232) - u128_at(344) - u128_at(360) - u128_at(376);
    (total_sf >> 60, u64_at(2592))
}

pub fn underlying_for_receipts(svm: &LiteSVM, r: &KaminoReserve, receipts: u64) -> u64 {
    let (total_liquidity, supply) = reserve_rate(svm, r);
    ((receipts as u128) * total_liquidity / supply as u128) as u64
}
