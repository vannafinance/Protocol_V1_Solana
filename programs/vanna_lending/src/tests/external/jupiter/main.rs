//! Jupiter v6 integration tests, one test binary:
//! - `adapter`: the Jupiter adapter's call plans (unit)
//! - `swaps`: end to end against the real Jupiter and Orca programs and a mainnet pool
//!   (`common::mainnet`), through `margin_execute`

#[path = "../../common/mod.rs"]
mod common;

mod adapter;
mod swaps;
