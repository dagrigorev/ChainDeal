//! ChainDeal core: types, hashing, signatures and the escrow smart-contract
//! state machine. Compiled natively into the backend and to WebAssembly for
//! the browser wallet, so both sides agree byte-for-byte on what gets signed.

// Amounts are written as `whole_cents` (e.g. `1000_00` = 1,000.00 DEAL).
#![allow(clippy::inconsistent_digit_grouping)]

pub mod contract;
pub mod crypto;
pub mod types;

pub use contract::{apply_tx, policy_for, ContractError, DealPolicy, WorkingState};
pub use crypto::*;
pub use types::*;
