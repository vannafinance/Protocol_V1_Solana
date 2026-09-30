use crate::ValidatorError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{Binding, CallContext, CallMode, CallPermit, PermitSigner, Role};

pub const JUPITER: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

pub const ROUTE: [u8; 8] = [229, 23, 203, 151, 122, 227, 173, 42];
pub const SHARED_ACCOUNTS_ROUTE: [u8; 8] = [193, 32, 155, 51, 65, 214, 156, 129];

const TAIL_LEN: usize = 19;
const MIN_HEAD_LEN: usize = 12;
const ROUTE_ACCOUNT_COUNT: usize = 9;
const SHARED_ROUTE_ACCOUNT_COUNT: usize = 13;

pub fn permit_for(context: &CallContext, accounts: &[Pubkey]) -> Result<CallPermit> {
    require!(context.mode == CallMode::Owner, ValidatorError::CallNotAllowed);
    let data = &context.data;
    require!(data.len() >= MIN_HEAD_LEN + TAIL_LEN, ValidatorError::CallNotAllowed);
    let tail = &data[data.len() - TAIL_LEN..];
    let in_amount = u64::from_le_bytes(tail[..8].try_into().unwrap());
    let platform_fee_bps = tail[TAIL_LEN - 1];
    require!(in_amount > 0, ValidatorError::ZeroAmount);
    require!(platform_fee_bps == 0, ValidatorError::CallNotAllowed);

    let is_unset = |i: usize| accounts.get(i) == Some(&context.target_program);
    let bind = |index: u16, role: Role| Binding { index, role };
    let selector: [u8; 8] = data[..8].try_into().unwrap();
    let (margin, received, spent) = match selector {
        ROUTE => {
            require!(accounts.len() >= ROUTE_ACCOUNT_COUNT, ValidatorError::InvalidCallAccounts);
            require!(is_unset(4) || accounts[4] == accounts[3], ValidatorError::InvalidCallAccounts);
            require!(is_unset(6), ValidatorError::InvalidCallAccounts);
            (bind(1, Role::Margin), 3, 2)
        }
        SHARED_ACCOUNTS_ROUTE => {
            require!(accounts.len() >= SHARED_ROUTE_ACCOUNT_COUNT, ValidatorError::InvalidCallAccounts);
            require!(is_unset(9), ValidatorError::InvalidCallAccounts);
            (bind(2, Role::Margin), 6, 3)
        }
        _ => return err!(ValidatorError::CallNotAllowed),
    };
    Ok(CallPermit {
        signer: PermitSigner::Margin,
        bindings: vec![margin],
        tokens_in: vec![received],
        tokens_out: vec![spent],
        funding: None,
        opens_leg: None,
    })
}
