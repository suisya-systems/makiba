//! makiba: an organization where the person talks to one secretary, built on herdr-core.
//! The library holds the code; `src/main.rs` only parses arguments and calls it (spec section 14).

/// The usage the binary prints until its subcommands exist.
pub const USAGE: &str = "usage: makiba <command>\n\nNo command is implemented yet.\n";

pub mod agents;
pub mod base;
pub mod db;
pub mod spec;
