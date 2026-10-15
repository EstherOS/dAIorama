//! dAIorama — one binary, one process per instance (spec `docs/03-architecture.md`).
//!
//! This file is subcommand dispatch only: everything lives in the modules below.
//!
//! Module order is the dependency order: `strings` (text only) → `ipc` → `config` →
//! `manager` → `mcp` → `cli`. Nothing lower depends on anything higher, and `ipc`/`config`
//! are the two leaves everything else builds on.

mod cli;
mod config;
mod ipc;
mod manager;
mod mcp;
mod strings;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    cli::run().await
}
