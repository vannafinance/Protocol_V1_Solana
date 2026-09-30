use crate::ValidatorError;
use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{Binding, CallContext, CallMode, CallPermit, PermitSigner, Role};

pub const KLEND: Pubkey = pubkey!("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");

pub const DEPOSIT_RESERVE_LIQUIDITY: [u8; 8] = [169, 201, 30, 126, 6, 205, 102, 68];
pub const REDEEM_RESERVE_COLLATERAL: [u8; 8] = [234, 117, 181, 125, 185, 142, 220, 29];

const CALL_ACCOUNT_COUNT: u16 = 12;
const CALL_DATA_LEN: usize = 16;

pub fn permit_for(context: &CallContext) -> Result<CallPermit> {
    require!(context.mode == CallMode::Owner, ValidatorError::CallNotAllowed);
    let data = &context.data;
    require!(data.len() == CALL_DATA_LEN, ValidatorError::CallNotAllowed);
    require!(context.call_account_count == CALL_ACCOUNT_COUNT, ValidatorError::WrongAccountCount);
    let amount = u64::from_le_bytes(data[8..16].try_into().unwrap());
    require!(amount > 0, ValidatorError::ZeroAmount);

    require!(
        matches!(data[..8].try_into().unwrap(), DEPOSIT_RESERVE_LIQUIDITY | REDEEM_RESERVE_COLLATERAL),
        ValidatorError::CallNotAllowed
    );
    Ok(CallPermit {
        signer: PermitSigner::Margin,
        bindings: vec![Binding { index: 0, role: Role::Margin }],
        tokens_in: vec![8],
        tokens_out: vec![7],
        funding: None,
        opens_leg: None,
    })
}
