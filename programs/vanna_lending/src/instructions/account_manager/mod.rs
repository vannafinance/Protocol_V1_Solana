//! Everything a margin account does: open and close, move collateral, borrow and repay, call
//! whitelisted external programs, and be liquidated.

pub mod account;
pub mod borrow;
pub mod collateral;
pub mod exec;
pub mod liquidate;

pub use account::*;
pub use borrow::*;
pub use collateral::*;
pub use exec::*;
pub use liquidate::*;
