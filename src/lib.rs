//! makiba: an organization where the person talks to one secretary, built on its base.
//! The library holds the code; `src/main.rs` only parses arguments and calls it (spec section 14).

pub mod agents;
pub mod base;
pub mod build;
pub mod channel;
pub mod cli;
pub mod daemon;
pub mod db;
pub mod down;
pub mod hooks;
pub mod screen;
pub mod spec;
pub mod status_local;
pub mod up;
pub mod work_discovery;
