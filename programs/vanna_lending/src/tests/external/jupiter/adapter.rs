//! The Jupiter adapter's call plans.

use anchor_lang::prelude::Pubkey;
use vanna_lending::adapters::jupiter::{ROUTE, SHARED_ACCOUNTS_ROUTE};
use vanna_lending::adapters::{plan_call, AdapterKind, CallPlan, TokenLeg};

fn keys(count: usize) -> Vec<Pubkey> {
    (0..count).map(|_| Pubkey::new_unique()).collect()
}

const JUPITER: Pubkey = anchor_lang::prelude::pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

/// A one-step route: `selector | [id] | Vec<RoutePlanStep> | in | quoted_out | slippage | fee`.
fn call(selector: [u8; 8], shared: bool, in_amount: u64, platform_fee_bps: u8) -> Vec<u8> {
    let mut data = selector.to_vec();
    if shared {
        data.push(0);
    }
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&[17, 1, 100, 0, 1]);
    data.extend_from_slice(&in_amount.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    data.extend_from_slice(&50u16.to_le_bytes());
    data.push(platform_fee_bps);
    data
}

/// Route accounts with Jupiter's "unset" marker in the given optional slots, plus AMM accounts.
fn accounts(count: usize, unset: &[usize]) -> Vec<Pubkey> {
    let mut keys = keys(count + 12);
    for i in unset {
        keys[*i] = JUPITER;
    }
    keys
}

fn plan(data: &[u8], accounts: &[Pubkey]) -> anchor_lang::Result<CallPlan> {
    plan_call(AdapterKind::Jupiter, &JUPITER, data, accounts)
}

#[test]
fn route_spends_the_source_and_receives_into_the_destination() {
    let expected = CallPlan {
        authority: 1,
        spent: TokenLeg { vault: 2, mint: None },
        received: TokenLeg { vault: 3, mint: Some(5) },
        price_source: None,
        max_spent: 1_000,
    };
    assert_eq!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[4, 6])).unwrap(), expected);
    // Naming the destination explicitly as the redirect target is the same thing.
    let mut explicit = accounts(9, &[6]);
    explicit[4] = explicit[3];
    assert_eq!(plan(&call(ROUTE, false, 1_000, 0), &explicit).unwrap(), expected);
}

#[test]
fn shared_route_spends_the_source_and_receives_into_the_destination() {
    let expected = CallPlan {
        authority: 2,
        spent: TokenLeg { vault: 3, mint: Some(7) },
        received: TokenLeg { vault: 6, mint: Some(8) },
        price_source: None,
        max_spent: 1_000,
    };
    assert_eq!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(13, &[9])).unwrap(), expected);
}

#[test]
fn output_must_stay_in_the_margin_with_no_platform_fee() {
    // Redirected output, a platform-fee account, or a platform fee.
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[6])).is_err());
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[4])).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(13, &[])).is_err());
    assert!(plan(&call(ROUTE, false, 1_000, 1), &accounts(9, &[4, 6])).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 25), &accounts(13, &[9])).is_err());
}

#[test]
fn refuses_other_instructions_and_malformed_calls() {
    for selector in [
        [208, 51, 239, 151, 123, 43, 237, 92],   // exact_out_route
        [176, 209, 105, 168, 154, 125, 69, 62],  // shared_accounts_exact_out_route
        [150, 86, 71, 116, 167, 93, 14, 104],    // route_with_token_ledger
        [187, 100, 250, 204, 49, 196, 175, 20],  // route_v2
        [209, 152, 83, 147, 124, 254, 216, 233], // shared_accounts_route_v2
        [228, 85, 185, 112, 78, 79, 77, 2],      // set_token_ledger
        [123, 229, 184, 63, 12, 0, 92, 145],     // whirlpool_swap
    ] {
        assert!(plan(&call(selector, false, 1_000, 0), &accounts(13, &[4, 6, 9])).is_err());
    }
    assert!(plan(&call(ROUTE, false, 0, 0), &accounts(9, &[4, 6])).is_err()); // zero in_amount
    assert!(plan(&ROUTE, &accounts(9, &[4, 6])).is_err()); // no route plan
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(0, &[4, 6])[..8]).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(0, &[9])[..12]).is_err());
}
