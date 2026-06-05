//! `cust` — a Cargo-style build system for C (clang-only).
//!
//! v0.1 driver. See `docs/design/cust-design.md` §17 for the locked
//! scope. The shape on disk is:
//!
//!   cust build / check / clean   (the only subcommands)
//!   Cust.toml + src/lib.c        (the only crate shape)
//!   target/<profile>/lib<n>.a    (the only artifact)

mod build;
mod clang;
mod cli;
mod manifest;
mod mod_scanner;
mod modules;
mod new;
mod plugin;
mod profile;
mod target_layout;
mod workspace;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match cli.dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
