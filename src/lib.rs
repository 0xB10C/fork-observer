#![cfg_attr(feature = "strict", deny(warnings))]

//! fork-observer's guts, exposed as a library so that the binary, the tests and
//! the benchmarks in `benches/` all exercise the same code.

pub mod activity;
pub mod api;
pub mod backend;
pub mod cache;
pub mod config;
pub mod db;
pub mod error;
pub mod headertree;
pub mod jsonrpc;
pub mod remote_forkobserver;
pub mod rss;
pub mod types;
