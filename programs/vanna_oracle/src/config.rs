use anchor_lang::prelude::*;

pub const SCOPE_CHAIN_UNUSED: u16 = u16::MAX;
pub const EMPTY_SCOPE_CHAIN: [u16; 4] = [SCOPE_CHAIN_UNUSED; 4];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Debug, PartialEq, Eq)]
pub struct OracleConfig {
    pub scope_prices: Pubkey,
    pub scope_chain: [u16; 4],
    pub scope_twap_chain: [u16; 4],
    pub pyth_price: Pubkey,
    pub pyth_factor: Pubkey,
    pub klend_reserve: Pubkey,
    pub klend_program: Pubkey,
    pub max_age_secs: u32,
    pub max_twap_divergence_bps: u16,
    pub max_confidence_bps: u16,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            scope_prices: Pubkey::default(),
            scope_chain: EMPTY_SCOPE_CHAIN,
            scope_twap_chain: EMPTY_SCOPE_CHAIN,
            pyth_price: Pubkey::default(),
            pyth_factor: Pubkey::default(),
            klend_reserve: Pubkey::default(),
            klend_program: Pubkey::default(),
            max_age_secs: 0,
            max_twap_divergence_bps: 0,
            max_confidence_bps: 0,
        }
    }
}

impl OracleConfig {
    pub fn uses_scope(&self) -> bool {
        self.scope_prices != Pubkey::default()
    }

    pub fn uses_pyth(&self) -> bool {
        self.pyth_price != Pubkey::default()
    }

    pub fn uses_pyth_factor(&self) -> bool {
        self.pyth_factor != Pubkey::default()
    }

    pub fn uses_klend(&self) -> bool {
        self.klend_reserve != Pubkey::default()
    }

    pub fn is_configured(&self) -> bool {
        self.uses_scope() || self.uses_pyth()
    }

    pub fn twap_check_enabled(&self) -> bool {
        self.max_twap_divergence_bps > 0
    }

    pub fn accounts(&self) -> impl Iterator<Item = Pubkey> {
        [self.scope_prices, self.pyth_price, self.pyth_factor, self.klend_reserve]
            .into_iter()
            .filter(|key| *key != Pubkey::default())
    }

    pub fn same_sources(&self, other: &OracleConfig) -> bool {
        self.scope_prices == other.scope_prices
            && self.scope_chain == other.scope_chain
            && self.scope_twap_chain == other.scope_twap_chain
            && self.pyth_price == other.pyth_price
            && self.pyth_factor == other.pyth_factor
    }
}
