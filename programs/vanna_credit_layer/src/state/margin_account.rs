use crate::constants::MAX_ASSETS;
use crate::errors::VannaError;
use anchor_lang::prelude::*;

pub const EMPTY_ASSET_INDEX: u16 = u16::MAX;

pub const MARGIN_STATUS_ACTIVE: u8 = 0;
pub const MAX_VENUES: usize = 6;

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VenueLegs {
    pub asset_index: u16,
    pub legs: u64,
}

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
    pub venue_legs: [VenueLegs; MAX_VENUES],
    pub reserved: [u8; 36],
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
            venue_legs: [VenueLegs::default(); MAX_VENUES],
            reserved: [0u8; 36],
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

    pub fn is_empty(&self) -> bool {
        self.collateral_count == 0 && self.debt_count == 0
    }

    pub fn legs_of(&self, asset_index: u16) -> u64 {
        self.venue_legs
            .iter()
            .find(|entry| entry.legs != 0 && entry.asset_index == asset_index)
            .map_or(0, |entry| entry.legs)
    }

    pub fn set_legs(&mut self, asset_index: u16, legs: u64) -> Result<()> {
        if let Some(entry) = self.venue_legs.iter_mut().find(|e| e.legs != 0 && e.asset_index == asset_index) {
            entry.legs = legs;
            return Ok(());
        }
        if legs == 0 {
            return Ok(());
        }
        let entry = self.venue_legs.iter_mut().find(|e| e.legs == 0).ok_or(VannaError::TooManyAssets)?;
        *entry = VenueLegs { asset_index, legs };
        Ok(())
    }

    pub fn next_event_sequence(&mut self) -> Result<u64> {
        self.event_sequence = self.event_sequence.checked_add(1).ok_or(VannaError::MathOverflow)?;
        Ok(self.event_sequence)
    }
}
