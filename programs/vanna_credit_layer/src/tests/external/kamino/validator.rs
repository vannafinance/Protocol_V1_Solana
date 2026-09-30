use anchor_lang::prelude::Pubkey;
use vanna_validator::kamino::{permit_for, DEPOSIT_RESERVE_LIQUIDITY, REDEEM_RESERVE_COLLATERAL};
use vanna_credit_layer::interface::{Binding, CallContext, CallMode, CallPermit, PermitSigner, Role};
use vanna_oracle::klend::ReserveRate;

fn call(selector: [u8; 8], amount: u64) -> Vec<u8> {
    let mut data = selector.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data
}

fn plan(data: &[u8], count: u16) -> anchor_lang::Result<CallPermit> {
    plan_as(CallMode::Owner, data, count)
}

fn plan_as(mode: CallMode, data: &[u8], count: u16) -> anchor_lang::Result<CallPermit> {
    permit_for(&CallContext {
        mode,
        target_program: Pubkey::new_unique(),
        data: data.to_vec(),
        call_account_count: count,
        margin: Pubkey::new_unique(),
        authority: Pubkey::new_unique(),
        venue: Pubkey::default(),
        venue_account: Pubkey::default(),
        open_legs: 0,
    })
}

fn permit() -> CallPermit {
    CallPermit {
        signer: PermitSigner::Margin,
        bindings: vec![Binding { index: 0, role: Role::Margin }],
        tokens_in: vec![8],
        tokens_out: vec![7],
        funding: None,
        opens_leg: None,
    }
}

#[test]
fn supply_spends_liquidity_and_receives_ctokens() {
    assert_eq!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 100), 12).unwrap(), permit());
}

#[test]
fn redeem_spends_ctokens_and_receives_liquidity() {
    assert_eq!(plan(&call(REDEEM_RESERVE_COLLATERAL, 100), 12).unwrap(), permit());
}

#[test]
fn refuses_other_instructions_and_malformed_calls() {
    assert!(plan(&call([121, 127, 18, 204, 73, 245, 225, 65], 100), 12).is_err());
    assert!(plan(&DEPOSIT_RESERVE_LIQUIDITY, 12).is_err());
    assert!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 100), 11).is_err());
    assert!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 0), 12).is_err());
    assert!(plan_as(CallMode::Unwind, &call(REDEEM_RESERVE_COLLATERAL, 100), 12).is_err());
}

#[test]
fn receipt_math_rounds_down_both_ways() {
    let rate = ReserveRate {
        liquidity_mint: Default::default(),
        liquidity_decimals: 6,
        total_liquidity: 2_000,
        collateral_supply: 1_000,
    };
    assert_eq!(rate.underlying_for_receipts(10).unwrap(), 20);
    assert_eq!(rate.receipts_for_underlying(20).unwrap(), 10);
    assert_eq!(rate.receipts_for_underlying(21).unwrap(), 10);
    assert_eq!(rate.underlying_for_receipts(0).unwrap(), 0);

    let empty = ReserveRate { total_liquidity: 0, collateral_supply: 0, ..rate };
    assert!(empty.underlying_for_receipts(1).is_err());
    assert!(empty.receipts_for_underlying(1).is_err());
}
