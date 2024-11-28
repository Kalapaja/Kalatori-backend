// The only purpose of this file is to prevent importing items from `main.rs`. If we'll ever need to
// add Rust integration tests, the prevention enables to avoid fixing imports throughout the
// codebase and make changes only to this file.

mod arguments;
mod chain;
mod database;
mod definitions;
mod error;
mod handlers;
mod server;
mod signer;
mod state;
mod utils;
