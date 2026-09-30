pub mod gmtrade;
pub mod jupiter;
pub mod kamino;

use anchor_lang::prelude::*;
use vanna_credit_layer::interface::{CallContext, CallPermit};

declare_id!("6fND3vhtstp486iE6rsSNVUox3kcjNNRsSLAN7ZPs7th");

#[program]
pub mod vanna_validator {
    use super::*;

    pub fn review_call<'info>(ctx: Context<'info, ReviewCall>, context: CallContext) -> Result<CallPermit> {
        let count = usize::from(context.call_account_count);
        require!(count <= ctx.remaining_accounts.len(), ValidatorError::InvalidCallAccounts);
        let (call, extra) = ctx.remaining_accounts.split_at(count);
        match context.target_program {
            kamino::KLEND => kamino::permit_for(&context),
            jupiter::JUPITER => {
                let keys: Vec<Pubkey> = call.iter().map(|a| a.key()).collect();
                jupiter::permit_for(&context, &keys)
            }
            _ => gmtrade::review(&context, call, extra),
        }
    }
}

#[derive(Accounts)]
pub struct ReviewCall {}

#[error_code]
pub enum ValidatorError {
    #[msg("This call is not allowed")]
    CallNotAllowed,
    #[msg("Call accounts do not match the allowed layout")]
    InvalidCallAccounts,
    #[msg("Call has the wrong number of accounts")]
    WrongAccountCount,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("No rules for this program")]
    UnknownProgram,
    #[msg("Market is not listed in the venue's market book")]
    MarketNotListed,
    #[msg("New exposure is disabled in this market")]
    TradingDisabled,
    #[msg("Order would leave the position above the market's leverage cap")]
    LeverageTooHigh,
    #[msg("Math overflow")]
    MathOverflow,
}
