//! Margin account lifecycle: one account per wallet.

use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::state::margin_account::MarginAccount;
use anchor_lang::prelude::*;

// ---------------------------------------------------------------------------
// user_create_margin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCreateMargin<'info> {
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + MarginAccount::INIT_SPACE,
        seeds = [MARGIN_SEED, authority.key().as_ref()],
        bump
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    pub system_program: Program<'info, System>,
}

pub fn user_create_margin(ctx: Context<UserCreateMargin>) -> Result<()> {
    let bump = ctx.bumps.margin_account;
    ctx.accounts
        .margin_account
        .set_inner(MarginAccount::new_empty(ctx.accounts.authority.key(), bump));

    emit!(MarginCreated {
        margin_account: ctx.accounts.margin_account.key(),
        authority: ctx.accounts.authority.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// user_close_margin
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct UserCloseMargin<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized,
        close = authority
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
}

pub fn user_close_margin(ctx: Context<UserCloseMargin>) -> Result<()> {
    require!(ctx.accounts.margin_account.is_empty(), VannaError::NonEmptyMargin);
    emit!(MarginClosed {
        margin_account: ctx.accounts.margin_account.key(),
        authority: ctx.accounts.authority.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
