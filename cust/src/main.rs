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
// v0.2 module loader input layer; not yet wired into the build
// pipeline. The wiring lands in the same release.
#[allow(dead_code)]
mod mod_scanner;
mod new;
mod profile;
mod target_layout;

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
