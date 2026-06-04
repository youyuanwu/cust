//! CLI surface. Six entry points: `build`, `check`, `clean`, `new`,
//! `--version`, `--help`.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    build::{self, BuildPlan},
    clang::Clang,
    manifest::Manifest,
    new::{self, CrateKind, NewPlan},
    profile::ProfileKind,
    target_layout::TargetLayout,
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

/// Locate the manifest by walking up from cwd. Returns `(manifest,
/// crate_root, workspace_root)`. In v0.1 the `workspace_root` *is*
/// the crate root — `target/` lives next to `Cust.toml`.
fn locate(cwd: &Path) -> Result<(Manifest, PathBuf, PathBuf)> {
    let loc = Manifest::discover(cwd)?;
    let manifest = Manifest::load(&loc.path)?;
    let crate_root = loc.dir.clone();
    // v0.1: no workspace member discovery; the crate root is also
    // the workspace root for the purposes of `target/` placement.
    let workspace_root = loc.dir;
    Ok((manifest, crate_root, workspace_root))
}

fn run_build(profile_kind: ProfileKind) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let (manifest, crate_root, workspace_root) = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = crate::plugin::Plugin::discover();

    let plan = BuildPlan {
        manifest: &manifest,
        crate_root: &crate_root,
        workspace_root: &workspace_root,
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
    };
    let outputs = build::run(&plan)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    println!(
        "  Finished {} [{}] -> {}",
        manifest.package.name,
        profile_kind.manifest_name(),
        outputs.archive.display()
    );
    Ok(())
}

fn run_check(profile_kind: ProfileKind) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let (manifest, crate_root, workspace_root) = locate(&cwd)?;
    let clang = Clang::discover()?;

    let profile_override = match profile_kind {
        ProfileKind::Dev => manifest.profile.dev.as_ref(),
        ProfileKind::Release => manifest.profile.release.as_ref(),
    };
    let profile = crate::profile::ResolvedProfile::resolve(profile_kind, profile_override)?;

    let layout = TargetLayout::for_workspace(&workspace_root, profile_kind);
    layout.ensure_dirs()?;
    let prelude = layout.prelude_path();
    // Materialise the prelude so `-include` resolves. (We call into
    // the build module via the public `build_cflags` only; the
    // materialise helper is private and duplicating it here would
    // drift — instead we redo the same content-stable write inline.)
    write_prelude(&prelude)?;

    let source = manifest.lib_source(&crate_root);
    if !source.is_file() {
        bail!("library source `{}` not found", source.display());
    }

    // v0.2 `cust check` is still root-only — it does not walk
    // `#cust mod` (full module-graph check waits for v0.5). But it
    // does run the root source through the scanner+rewriter so
    // `#cust mod` lines at the top level don't trip `-fsyntax-only`.
    let src_text = std::fs::read_to_string(&source)
        .with_context(|| format!("reading `{}`", source.display()))?;
    let scan = crate::mod_scanner::scan(&src_text, &source)?;
    let rewritten = crate::mod_scanner::rewrite(&src_text, &source, &scan);
    let rewritten_path = layout.profile_root.join("check.preprocessed.c");
    std::fs::write(&rewritten_path, &rewritten)
        .with_context(|| format!("writing `{}`", rewritten_path.display()))?;

    // Reuse `build_cflags` for parity with `cust build`, but drop
    // `-c -o <obj>` and replace with `-fsyntax-only`.
    let plugin = crate::plugin::Plugin::discover();
    let plan = BuildPlan {
        manifest: &manifest,
        crate_root: &crate_root,
        workspace_root: &workspace_root,
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
    };
    let dummy_obj = layout.profile_root.join("check.o");
    let source_dir = source.parent().unwrap_or(&crate_root);
    // `cust check` skips fragment-header emission — we're only
    // validating syntax, not committing to a build artifact.
    let mut flags = build::build_cflags(
        &plan,
        &profile,
        &prelude,
        &rewritten_path,
        &dummy_obj,
        Some(source_dir),
        None,
    );
    // Strip the trailing `-c -o <obj> <src>` triple (4 args) and
    // re-add `-fsyntax-only <src>`.
    let new_len = flags.len().saturating_sub(4);
    flags.truncate(new_len);
    flags.push("-fsyntax-only".to_string());
    flags.push(rewritten_path.display().to_string());

    let status = clang
        .command()
        .args(&flags)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("invoking `{}`", clang.path.display()))?;
    if !status.success() {
        bail!("clang -fsyntax-only exited with status {status}");
    }
    println!("  Checked {}", manifest.package.name);
    Ok(())
}

fn run_clean() -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let loc = Manifest::discover(&cwd)?;
    let target = loc.dir.join("target");
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

fn write_prelude(dst: &Path) -> Result<()> {
    const PRELUDE: &str = include_str!("prelude.h");
    let needs_write = fs::read_to_string(dst).ok().is_none_or(|s| s != PRELUDE);
    if needs_write {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }
        fs::write(dst, PRELUDE).with_context(|| format!("writing `{}`", dst.display()))?;
    }
    Ok(())
}
