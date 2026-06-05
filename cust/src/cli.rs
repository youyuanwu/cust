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
    /// Build the crate's staticlib and/or binary.
    Build(BuildArgs),
    /// Run `clang -fsyntax-only` over the crate root.
    Check(CheckArgs),
    /// Build the crate's binary and run it. Arguments after `--`
    /// are forwarded as `argv` to the spawned executable.
    Run(RunArgs),
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
    /// Restrict the build to one workspace member and its
    /// transitive path dependencies. Without this flag every
    /// member is built.
    #[arg(short = 'p', long = "package")]
    pub package: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Check with the `release` profile's flags.
    #[arg(long)]
    pub release: bool,
    /// Restrict the check to one workspace member and its
    /// transitive path dependencies. Without this flag every
    /// member is checked.
    #[arg(short = 'p', long = "package")]
    pub package: Option<String>,
}

/// Arguments for `cust run`.
///
/// `cust run [-p <member>] [--release] [-- <args>...]` — builds
/// the workspace, picks a runnable bin member (the only bin if
/// `-p` is omitted; the named member otherwise), then spawns
/// the resulting executable with everything after `--` as argv.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Build (and run) with the `release` profile.
    #[arg(long)]
    pub release: bool,
    /// Select which workspace member to run. Required when more
    /// than one bin member exists.
    #[arg(short = 'p', long = "package")]
    pub package: Option<String>,
    /// Arguments forwarded to the spawned binary. Anything after
    /// `--` lands here.
    #[arg(last = true, allow_hyphen_values = true)]
    pub forwarded: Vec<String>,
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
    /// Create a library crate (the default when no kind flag is
    /// passed). Mutually exclusive with `--bin`.
    #[arg(long, conflicts_with = "bin")]
    pub lib: bool,
    /// Create a binary crate. Mutually exclusive with `--lib`.
    #[arg(long)]
    pub bin: bool,
}

impl Cli {
    pub fn dispatch(self) -> Result<()> {
        match self.command {
            Cmd::Build(args) => run_build(profile_kind(args.release), args.package.as_deref()),
            Cmd::Check(args) => run_check(profile_kind(args.release), args.package.as_deref()),
            Cmd::Run(args) => run_run(
                profile_kind(args.release),
                args.package.as_deref(),
                &args.forwarded,
            ),
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

fn run_build(profile_kind: ProfileKind, package: Option<&str>) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        only: package,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    for (name, out) in &outputs.per_member {
        // v0.3.1: a member may produce an archive, an executable,
        // or both. Print whatever was produced (in produce order).
        let label = profile_kind.manifest_name();
        if let Some(arch) = &out.archive {
            println!("  Finished {name} [{label}] -> {}", arch.display());
        }
        if let Some(exe) = &out.executable {
            println!("  Finished {name} [{label}] -> {}", exe.display());
        }
    }
    Ok(())
}

fn run_check(profile_kind: ProfileKind, package: Option<&str>) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: true,
        only: package,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;
    for (name, _) in &outputs.per_member {
        println!("  Checked {name}");
    }
    Ok(())
}

/// `cust run` — build the workspace, locate the requested bin
/// member (or the only bin member when `-p` is omitted), then
/// spawn it with anything after `--` forwarded as argv. Exits
/// with the subprocess's exit code so shell scripts and CI
/// behave the same as if the user had run the binary directly.
fn run_run(profile_kind: ProfileKind, package: Option<&str>, forwarded: &[String]) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;

    // Pick the target bin member.
    //
    // * `-p <name>`: must exist and be a bin (or lib+bin).
    // * no `-p`: workspace must contain exactly one bin member.
    let target_name = if let Some(name) = package {
        let m = ws.member(name).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown workspace member `{name}` — known: [{}]",
                ws.members
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if !m.kind.has_bin() {
            anyhow::bail!(
                "workspace member `{name}` is a library — `cust run` \
                 requires a binary crate"
            );
        }
        name.to_string()
    } else {
        let bins: Vec<&str> = ws
            .members
            .iter()
            .filter(|m| m.kind.has_bin())
            .map(|m| m.name.as_str())
            .collect();
        match bins.as_slice() {
            [] => anyhow::bail!(
                "workspace contains no binary members; `cust run` \
                 requires a `[[bin]]` target or a `src/main.c`"
            ),
            [only] => (*only).to_string(),
            multiple => anyhow::bail!(
                "workspace contains multiple binary members; \
                 pass `-p <name>` to choose one (found: {})",
                multiple.join(", ")
            ),
        }
    };

    // Build with -p scoping so we only build the target bin and
    // its transitive deps.
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();
    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        only: Some(&target_name),
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    for (name, out) in &outputs.per_member {
        let label = profile_kind.manifest_name();
        if let Some(arch) = &out.archive {
            println!("  Finished {name} [{label}] -> {}", arch.display());
        }
        if let Some(exe) = &out.executable {
            println!("  Finished {name} [{label}] -> {}", exe.display());
        }
    }

    // Locate the executable for the target member. build_workspace
    // visits members in topo order, so the target bin is the last
    // entry whose name matches.
    let exe = outputs
        .per_member
        .iter()
        .rev()
        .find_map(|(name, out)| {
            (name == &target_name)
                .then_some(out.executable.as_deref())
                .flatten()
        })
        .ok_or_else(|| {
            anyhow::anyhow!("internal: `{target_name}` built but produced no executable")
        })?;

    println!("     Running {}", exe.display());

    // Inherit stdio. We exit with the child's code (or 128+signal
    // on POSIX) so shell scripts see the same exit semantics as
    // running the binary directly. Signal forwarding is v0.5+
    // (deferral in v0.3.1.md).
    let status = std::process::Command::new(exe)
        .args(forwarded)
        .status()
        .with_context(|| format!("spawning `{}`", exe.display()))?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    // Killed by signal. Bash-style: exit 128 + signum if known.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = status.signal() {
            std::process::exit(128 + sig);
        }
    }
    // Fallback.
    std::process::exit(1);
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
    // --bin / --lib are mutually exclusive at the clap layer
    // (conflicts_with). Default when neither is passed is --lib
    // (Cargo parity).
    let kind = if args.bin {
        CrateKind::Bin
    } else {
        CrateKind::Lib
    };

    let plan = NewPlan {
        path: &args.path,
        name: args.name.as_deref(),
        kind,
    };
    let out = new::run(&plan)?;
    let label = match kind {
        CrateKind::Lib => "library",
        CrateKind::Bin => "binary",
    };
    println!("  Created {label} `{}` at {}", out.name, out.root.display());
    Ok(())
}
