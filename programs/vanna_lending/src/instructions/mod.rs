//! Instruction handlers, grouped like the protocol's contracts: `admin` (configuration),
//! `lending_pool` (supply and redeem), `account_manager` (margin accounts).

pub mod account_manager;
pub mod admin;
pub mod lending_pool;

pub use account_manager::*;
pub use admin::*;
pub use lending_pool::*;
