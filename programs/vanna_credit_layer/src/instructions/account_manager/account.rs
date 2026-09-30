use crate::constants::*;
use crate::errors::VannaError;
use crate::events::*;
use crate::state::asset_config::AssetConfig;
use crate::state::debt_position::DebtPosition;
use crate::state::margin_account::MarginAccount;
use crate::validation::token::verify_associated_token_account;
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{close_account, CloseAccount, Mint, TokenAccount, TokenInterface};

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

#[derive(Accounts)]
pub struct UserReclaimRent<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        seeds = [MARGIN_SEED, margin_account.authority.as_ref()],
        bump = margin_account.bump,
        has_one = authority @ VannaError::Unauthorized
    )]
    pub margin_account: Box<Account<'info, MarginAccount>>,
    #[account(seeds = [ASSET_SEED, mint.key().as_ref()], bump = asset_config.bump)]
    pub asset_config: Box<Account<'info, AssetConfig>>,
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = mint,
        token::authority = margin_account,
        token::token_program = token_program
    )]
    pub margin_vault: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    #[account(
        mut,
        seeds = [DEBT_SEED, margin_account.key().as_ref(), asset_config.reserve.as_ref()],
        bump = debt_position.bump,
        close = authority
    )]
    pub debt_position: Option<Box<Account<'info, DebtPosition>>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn user_reclaim_rent(ctx: Context<UserReclaimRent>) -> Result<()> {
    let accounts = &ctx.accounts;
    let margin = &accounts.margin_account;
    let asset_index = accounts.asset_config.asset_index;
    require!(accounts.margin_vault.is_some() || accounts.debt_position.is_some(), VannaError::NothingToReclaim);

    if let Some(debt_position) = &accounts.debt_position {
        require!(
            debt_position.borrow_shares == 0 && !margin.is_debt_active(asset_index),
            VannaError::OutstandingDebt
        );
    }

    if let Some(vault) = &accounts.margin_vault {
        require!(vault.amount == 0, VannaError::NonEmptyVault);
        require!(!margin.is_collateral_active(asset_index), VannaError::NonEmptyCollateralPosition);
        verify_associated_token_account(&vault.key(), &margin.key(), &accounts.mint.key(), &accounts.token_program.key())?;
        let authority_key = margin.authority;
        let signer_seeds: &[&[&[u8]]] = &[&[MARGIN_SEED, authority_key.as_ref(), &[margin.bump]]];
        close_account(CpiContext::new_with_signer(
            accounts.token_program.key(),
            CloseAccount {
                account: vault.to_account_info(),
                destination: accounts.authority.to_account_info(),
                authority: margin.to_account_info(),
            },
            signer_seeds,
        ))?;
    }

    emit!(RentReclaimed {
        margin_account: margin.key(),
        mint: accounts.mint.key(),
        vault_closed: accounts.margin_vault.is_some(),
        debt_position_closed: accounts.debt_position.is_some(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
