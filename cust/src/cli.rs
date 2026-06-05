//! CLI surface. Six entry points: `build`, `check`, `clean`, `new`,
//! `--version`, `--help`.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    clang::Clang,
    new::{self, CrateKind, NewPlan},
    profile::ProfileKind,
    workspace::{self, Workspace, WorkspaceBuildOptions},
};

/// `cust` — a Cargo-style build system for C (clang-only).
#[derive(Debug, Parser)]
#[command(name = "cust", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Build the crate's staticlib.
    Build(BuildArgs),
    /// Run `clang -fsyntax-only` over the crate root.
    Check(CheckArgs),
    /// Remove the `target/` directory.
    Clean,
    /// Scaffold a new cust crate at `<path>`.
    New(NewArgs),
}

#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// Build with the `release` profile.
    #[arg(long)]
    pub release: bool,
}

#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Check with the `release` profile's flags.
    #[arg(long)]
    pub release: bool,
}

#[derive(Debug, clap::Args)]
pub struct NewArgs {
    /// Where to place the new crate. The directory will be created
    /// if it doesn't exist; if it does, it must be empty.
    pub path: PathBuf,
    /// Override the package name (defaults to the final path
    /// component).
    #[arg(long)]
    pub name: Option<String>,
    /// Create a library crate (currently the only supported kind;
    /// `--bin` waits for the binary target story).
    #[arg(long, default_value_t = true)]
    pub lib: bool,
}

impl Cli {
    pub fn dispatch(self) -> Result<()> {
        match self.command {
            Cmd::Build(args) => run_build(profile_kind(args.release)),
            Cmd::Check(args) => run_check(profile_kind(args.release)),
            Cmd::Clean => run_clean(),
            Cmd::New(args) => run_new(&args),
        }
    }
}

const fn profile_kind(release: bool) -> ProfileKind {
    if release {
        ProfileKind::Release
    } else {
        ProfileKind::Dev
    }
}

/// Locate the workspace by walking up from cwd. Returns a fully
/// resolved `Workspace` (member list + dep edges). For single-
/// crate projects this is a one-implicit-member workspace with
/// no `[workspace]` table.
fn locate(cwd: &Path) -> Result<Workspace> {
    Workspace::discover(cwd)
}

fn run_build(profile_kind: ProfileKind) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        only: None,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    for (name, out) in &outputs.per_member {
        println!(
            "  Finished {name} [{}] -> {}",
            profile_kind.manifest_name(),
            out.archive.display()
        );
    }
    Ok(())
}

fn run_check(profile_kind: ProfileKind) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: true,
        only: None,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;
    for (name, _) in &outputs.per_member {
        println!("  Checked {name}");
    }
    Ok(())
}

fn run_clean() -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let target = ws.root.join("target");
    if target.exists() {
        fs::remove_dir_all(&target).with_context(|| format!("removing `{}`", target.display()))?;
        println!("  Removed {}", target.display());
    } else {
        println!("  Nothing to clean ({} does not exist)", target.display());
    }
    Ok(())
}

fn run_new(args: &NewArgs) -> Result<()> {
    // `--lib` is the only supported kind today; the flag exists so
    // the eventual `--bin` flip is non-breaking. We don't bother
    // matching it.
    let _ = args.lib;

    let plan = NewPlan {
        path: &args.path,
        name: args.name.as_deref(),
        kind: CrateKind::Lib,
    };
    let out = new::run(&plan)?;
    println!("  Created library `{}` at {}", out.name, out.root.display());
    Ok(())
}
