//! The Kamino adapter's call plans and the cToken exchange-rate math.

use anchor_lang::prelude::Pubkey;
use vanna_lending::adapters::kamino::{ReserveRate, DEPOSIT_RESERVE_LIQUIDITY, REDEEM_RESERVE_COLLATERAL};
use vanna_lending::adapters::{plan_call, AdapterKind, CallPlan, TokenLeg};

fn keys(count: usize) -> Vec<Pubkey> {
    (0..count).map(|_| Pubkey::new_unique()).collect()
}

fn call(selector: [u8; 8], amount: u64) -> Vec<u8> {
    let mut data = selector.to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    data
}

fn plan(data: &[u8], count: usize) -> anchor_lang::Result<CallPlan> {
    plan_call(AdapterKind::KaminoLend, &Pubkey::new_unique(), data, &keys(count))
}

#[test]
fn supply_spends_liquidity_and_receives_ctokens() {
    let expected = CallPlan {
        authority: 0,
        spent: TokenLeg { vault: 7, mint: Some(4) },
        received: TokenLeg { vault: 8, mint: Some(6) },
        price_source: Some(1),
        max_spent: 100,
    };
    assert_eq!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 100), 12).unwrap(), expected);
}

#[test]
fn redeem_spends_ctokens_and_receives_liquidity() {
    let expected = CallPlan {
        authority: 0,
        spent: TokenLeg { vault: 7, mint: Some(5) },
        received: TokenLeg { vault: 8, mint: Some(4) },
        price_source: Some(2),
        max_spent: 100,
    };
    assert_eq!(plan(&call(REDEEM_RESERVE_COLLATERAL, 100), 12).unwrap(), expected);
}

#[test]
fn refuses_other_instructions_and_malformed_calls() {
    assert!(plan(&call([121, 127, 18, 204, 73, 245, 225, 65], 100), 12).is_err()); // borrow_obligation_liquidity
    assert!(plan(&DEPOSIT_RESERVE_LIQUIDITY, 12).is_err());
    assert!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 100), 11).is_err());
    assert!(plan(&call(DEPOSIT_RESERVE_LIQUIDITY, 0), 12).is_err());
}

#[test]
fn receipt_math_rounds_down_both_ways() {
    // 2_000 liquidity backs 1_000 cTokens: 1 cToken = 2 underlying.
    let rate = ReserveRate {
        liquidity_mint: Default::default(),
        liquidity_decimals: 6,
        total_liquidity: 2_000,
        collateral_supply: 1_000,
    };
    assert_eq!(rate.underlying_for_receipts(10).unwrap(), 20);
    assert_eq!(rate.receipts_for_underlying(20).unwrap(), 10);
    assert_eq!(rate.receipts_for_underlying(21).unwrap(), 10); // floor(10.5)
    assert_eq!(rate.underlying_for_receipts(0).unwrap(), 0);

    let empty = ReserveRate { total_liquidity: 0, collateral_supply: 0, ..rate };
    assert!(empty.underlying_for_receipts(1).is_err());
    assert!(empty.receipts_for_underlying(1).is_err());
}
