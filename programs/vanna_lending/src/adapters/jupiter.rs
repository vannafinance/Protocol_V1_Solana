//! Jupiter v6 aggregator adapter: exact-input swaps from one margin vault into another.
//!
//! Allowed calls (Jupiter's account order; the route's AMM accounts follow these):
//!
//! route                                   shared_accounts_route
//!   0 token_program                         0 token_program
//!   1 user_transfer_authority (margin)      1 program_authority
//!   2 user_source_token_account (spent)     2 user_transfer_authority (margin)
//!   3 user_destination_token_account (rcv)  3 source_token_account (spent)
//!   4 destination_token_account  (unset)    4 program_source_token_account
//!   5 destination_mint                      5 program_destination_token_account
//!   6 platform_fee_account       (unset)    6 destination_token_account (received)
//!   7 event_authority                       7 source_mint
//!   8 program                               8 destination_mint
//!                                           9 platform_fee_account       (unset)
//!                                          10 token_2022_program
//!                                          11 event_authority
//!                                          12 program
//!
//! Jupiter encodes an omitted optional account as its own program id. The output must land in
//! the margin, so the redirect (`destination_token_account` of `route`) and platform-fee accounts
//! must be unset, and the platform fee must be zero.

use super::{CallPlan, TokenLeg};
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// sha256("global:route")[0..8]
pub const ROUTE: [u8; 8] = [229, 23, 203, 151, 122, 227, 173, 42];
/// sha256("global:shared_accounts_route")[0..8]
pub const SHARED_ACCOUNTS_ROUTE: [u8; 8] = [193, 32, 155, 51, 65, 214, 156, 129];

/// Both instructions end with `in_amount: u64, quoted_out_amount: u64, slippage_bps: u16,
/// platform_fee_bps: u8`.
const TAIL_LEN: usize = 19;
/// Selector + the route plan's `Vec` length prefix.
const MIN_HEAD_LEN: usize = 12;
const ROUTE_ACCOUNT_COUNT: usize = 9;
const SHARED_ROUTE_ACCOUNT_COUNT: usize = 13;

pub fn plan_call(program_id: &Pubkey, data: &[u8], accounts: &[Pubkey]) -> Result<CallPlan> {
    require!(data.len() >= MIN_HEAD_LEN + TAIL_LEN, VannaError::CallNotAllowed);
    let tail = &data[data.len() - TAIL_LEN..];
    let in_amount = u64::from_le_bytes(tail[..8].try_into().unwrap());
    let platform_fee_bps = tail[TAIL_LEN - 1];
    require!(in_amount > 0, VannaError::ZeroAmount);
    require!(platform_fee_bps == 0, VannaError::CallNotAllowed);

    let is_unset = |i: usize| accounts.get(i) == Some(program_id);
    let selector: [u8; 8] = data[..8].try_into().unwrap();
    match selector {
        ROUTE => {
            require!(accounts.len() >= ROUTE_ACCOUNT_COUNT, VannaError::InvalidCallAccounts);
            // Output may only go to the user destination (slot 3), and no fee account is allowed.
            require!(is_unset(4) || accounts[4] == accounts[3], VannaError::InvalidCallAccounts);
            require!(is_unset(6), VannaError::InvalidCallAccounts);
            Ok(CallPlan {
                authority: 1,
                spent: TokenLeg { vault: 2, mint: None },
                received: TokenLeg { vault: 3, mint: Some(5) },
                price_source: None,
                max_spent: in_amount,
            })
        }
        SHARED_ACCOUNTS_ROUTE => {
            require!(accounts.len() >= SHARED_ROUTE_ACCOUNT_COUNT, VannaError::InvalidCallAccounts);
            require!(is_unset(9), VannaError::InvalidCallAccounts);
            Ok(CallPlan {
                authority: 2,
                spent: TokenLeg { vault: 3, mint: Some(7) },
                received: TokenLeg { vault: 6, mint: Some(8) },
                price_source: None,
                max_spent: in_amount,
            })
        }
        _ => err!(VannaError::CallNotAllowed),
    }
}
