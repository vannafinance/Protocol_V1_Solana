use anchor_lang::prelude::Pubkey;
use vanna_validator::jupiter::{permit_for, ROUTE, SHARED_ACCOUNTS_ROUTE};
use vanna_credit_layer::interface::{Binding, CallContext, CallMode, CallPermit, PermitSigner, Role};

fn keys(count: usize) -> Vec<Pubkey> {
    (0..count).map(|_| Pubkey::new_unique()).collect()
}

const JUPITER: Pubkey = anchor_lang::prelude::pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

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

fn accounts(count: usize, unset: &[usize]) -> Vec<Pubkey> {
    let mut keys = keys(count + 12);
    for i in unset {
        keys[*i] = JUPITER;
    }
    keys
}

fn plan(data: &[u8], accounts: &[Pubkey]) -> anchor_lang::Result<CallPermit> {
    let context = CallContext {
        mode: CallMode::Owner,
        target_program: JUPITER,
        data: data.to_vec(),
        call_account_count: accounts.len() as u16,
        margin: Pubkey::new_unique(),
        authority: Pubkey::new_unique(),
        venue: Pubkey::default(),
        venue_account: Pubkey::default(),
        open_legs: 0,
    };
    permit_for(&context, accounts)
}

fn permit(margin: u16, received: u16, spent: u16) -> CallPermit {
    CallPermit {
        signer: PermitSigner::Margin,
        bindings: vec![Binding { index: margin, role: Role::Margin }],
        tokens_in: vec![received],
        tokens_out: vec![spent],
        funding: None,
        opens_leg: None,
    }
}

#[test]
fn route_spends_the_source_and_receives_into_the_destination() {
    let expected = permit(1, 3, 2);
    assert_eq!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[4, 6])).unwrap(), expected);
    let mut explicit = accounts(9, &[6]);
    explicit[4] = explicit[3];
    assert_eq!(plan(&call(ROUTE, false, 1_000, 0), &explicit).unwrap(), expected);
}

#[test]
fn shared_route_spends_the_source_and_receives_into_the_destination() {
    let expected = permit(2, 6, 3);
    assert_eq!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(13, &[9])).unwrap(), expected);
}

#[test]
fn output_must_stay_in_the_margin_with_no_platform_fee() {
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[6])).is_err());
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(9, &[4])).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(13, &[])).is_err());
    assert!(plan(&call(ROUTE, false, 1_000, 1), &accounts(9, &[4, 6])).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 25), &accounts(13, &[9])).is_err());
}

#[test]
fn refuses_other_instructions_and_malformed_calls() {
    for selector in [
        [208, 51, 239, 151, 123, 43, 237, 92],
        [176, 209, 105, 168, 154, 125, 69, 62],
        [150, 86, 71, 116, 167, 93, 14, 104],
        [187, 100, 250, 204, 49, 196, 175, 20],
        [209, 152, 83, 147, 124, 254, 216, 233],
        [228, 85, 185, 112, 78, 79, 77, 2],
        [123, 229, 184, 63, 12, 0, 92, 145],
    ] {
        assert!(plan(&call(selector, false, 1_000, 0), &accounts(13, &[4, 6, 9])).is_err());
    }
    assert!(plan(&call(ROUTE, false, 0, 0), &accounts(9, &[4, 6])).is_err());
    assert!(plan(&ROUTE, &accounts(9, &[4, 6])).is_err());
    assert!(plan(&call(ROUTE, false, 1_000, 0), &accounts(0, &[4, 6])[..8]).is_err());
    assert!(plan(&call(SHARED_ACCOUNTS_ROUTE, true, 1_000, 0), &accounts(0, &[9])[..12]).is_err());
}
