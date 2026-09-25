use crate::constants::MAX_ASSETS;
use crate::errors::VannaError;
use anchor_lang::prelude::*;

/// Sentinel marking an empty slot in the canonical active-asset index arrays.
pub const EMPTY_ASSET_INDEX: u16 = u16::MAX;

pub const MARGIN_STATUS_ACTIVE: u8 = 0;

/// One isolated borrowing portfolio. Seeded only by its authority, so each wallet has exactly one
/// (no subaccounts).
#[account]
#[derive(InitSpace)]
pub struct MarginAccount {
    pub authority: Pubkey,
    pub status: u8,
    pub collateral_count: u8,
    pub debt_count: u8,
    pub collateral_asset_indexes: [u16; MAX_ASSETS],
    pub debt_asset_indexes: [u16; MAX_ASSETS],
    pub event_sequence: u64,
    pub bump: u8,
    pub reserved: [u8; 96],
}

impl MarginAccount {
    pub fn new_empty(authority: Pubkey, bump: u8) -> Self {
        Self {
            authority,
            status: MARGIN_STATUS_ACTIVE,
            collateral_count: 0,
            debt_count: 0,
            collateral_asset_indexes: [EMPTY_ASSET_INDEX; MAX_ASSETS],
            debt_asset_indexes: [EMPTY_ASSET_INDEX; MAX_ASSETS],
            event_sequence: 0,
            bump,
            reserved: [0u8; 96],
        }
    }

    pub fn is_collateral_active(&self, asset_index: u16) -> bool {
        self.collateral_asset_indexes.contains(&asset_index)
    }

    pub fn is_debt_active(&self, asset_index: u16) -> bool {
        self.debt_asset_indexes.contains(&asset_index)
    }

    pub fn add_active_collateral(&mut self, asset_index: u16) -> Result<()> {
        require!(!self.is_collateral_active(asset_index), VannaError::DuplicateAssetIndex);
        let slot = self
            .collateral_asset_indexes
            .iter_mut()
            .find(|v| **v == EMPTY_ASSET_INDEX)
            .ok_or(VannaError::TooManyAssets)?;
        *slot = asset_index;
        self.collateral_count = self
            .collateral_count
            .checked_add(1)
            .ok_or(VannaError::MathOverflow)?;
        Ok(())
    }

    pub fn remove_active_collateral(&mut self, asset_index: u16) -> Result<()> {
        let slot = self
            .collateral_asset_indexes
            .iter_mut()
            .find(|v| **v == asset_index)
            .ok_or(VannaError::IncompletePositionAccounts)?;
        *slot = EMPTY_ASSET_INDEX;
        self.collateral_count = self
            .collateral_count
            .checked_sub(1)
            .ok_or(VannaError::MathUnderflow)?;
        Ok(())
    }

    pub fn add_active_debt(&mut self, asset_index: u16) -> Result<()> {
        require!(!self.is_debt_active(asset_index), VannaError::DuplicateAssetIndex);
        let slot = self
            .debt_asset_indexes
            .iter_mut()
            .find(|v| **v == EMPTY_ASSET_INDEX)
            .ok_or(VannaError::TooManyAssets)?;
        *slot = asset_index;
        self.debt_count = self.debt_count.checked_add(1).ok_or(VannaError::MathOverflow)?;
        Ok(())
    }

    pub fn remove_active_debt(&mut self, asset_index: u16) -> Result<()> {
        let slot = self
            .debt_asset_indexes
            .iter_mut()
            .find(|v| **v == asset_index)
            .ok_or(VannaError::IncompletePositionAccounts)?;
        *slot = EMPTY_ASSET_INDEX;
        self.debt_count = self.debt_count.checked_sub(1).ok_or(VannaError::MathUnderflow)?;
        Ok(())
    }

    pub fn active_collateral_indexes(&self) -> impl Iterator<Item = u16> + '_ {
        self.collateral_asset_indexes.iter().copied().filter(|v| *v != EMPTY_ASSET_INDEX)
    }

    pub fn active_debt_indexes(&self) -> impl Iterator<Item = u16> + '_ {
        self.debt_asset_indexes.iter().copied().filter(|v| *v != EMPTY_ASSET_INDEX)
    }

    /// Lite-position registry packed into `reserved`: a count byte followed by up to `MAX_ASSETS`
    /// little-endian u16 indexes. All-zero legacy accounts have none indexed; their legacy PDA is
    /// always scanned.
    pub fn lite_indexes(&self) -> Result<Vec<u16>> {
        let count = self.reserved[0] as usize;
        require!(count <= MAX_ASSETS, VannaError::IncompletePositionAccounts);
        let mut indexes = Vec::with_capacity(count);
        for i in 0..count {
            let offset = 1 + 2 * i;
            let index = u16::from_le_bytes([self.reserved[offset], self.reserved[offset + 1]]);
            require!(!indexes.contains(&index), VannaError::DuplicateAssetIndex);
            indexes.push(index);
        }
        Ok(indexes)
    }

    pub fn register_lite(&mut self, index: u16) -> Result<()> {
        let indexes = self.lite_indexes()?;
        if indexes.contains(&index) {
            return Ok(());
        }
        require!(indexes.len() < MAX_ASSETS, VannaError::TooManyAssets);
        let offset = 1 + 2 * indexes.len();
        self.reserved[offset..offset + 2].copy_from_slice(&index.to_le_bytes());
        self.reserved[0] += 1;
        Ok(())
    }

    pub fn unregister_lite(&mut self, index: u16) -> Result<()> {
        let mut indexes = self.lite_indexes()?;
        require!(indexes.contains(&index), VannaError::IncompletePositionAccounts);
        indexes.retain(|i| *i != index);
        self.reserved[..1 + 2 * MAX_ASSETS].fill(0);
        self.reserved[0] = indexes.len() as u8;
        for (i, index) in indexes.iter().enumerate() {
            self.reserved[1 + 2 * i..3 + 2 * i].copy_from_slice(&index.to_le_bytes());
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.collateral_count == 0 && self.debt_count == 0 && self.reserved[0] == 0
    }

    pub fn next_event_sequence(&mut self) -> Result<u64> {
        self.event_sequence = self.event_sequence.checked_add(1).ok_or(VannaError::MathOverflow)?;
        Ok(self.event_sequence)
    }
}
