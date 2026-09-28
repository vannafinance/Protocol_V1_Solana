//! Kamino klend integration tests, one test binary:
//! - `adapter`: the Kamino adapter's call plans and cToken math (unit)
//! - `synthetic`: receipt pricing against a synthetic klend reserve, and `margin_execute`'s
//!   registry and guards against a stand-in program
//! - `supply_redeem`, `withdraw`, `leverage`, `refusals`: end to end against the real klend
//!   program and mainnet reserves (`common::mainnet`), through `margin_execute`

#[path = "../../common/mod.rs"]
mod common;
mod env;

mod adapter;
mod leverage;
mod refusals;
mod supply_redeem;
mod synthetic;
mod withdraw;
