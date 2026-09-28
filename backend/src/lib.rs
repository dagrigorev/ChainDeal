//! ChainDeal node library: shared by the `chaindeal-backend` server and the
//! `chaindeal-bulk` history loader.

// Amounts are written as `whole_cents` (e.g. `1000_00` = 1,000.00 DEAL).
#![allow(clippy::inconsistent_digit_grouping)]

pub mod api;
pub mod authz;
pub mod chain;
pub mod db;
pub mod sim;
pub mod synth;
