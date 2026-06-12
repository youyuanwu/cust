//! CLI surface. Seven entry points: `build`, `check`, `run`,
//! `test`, `clean`, `new`, `--version`, `--help`.

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
    /// V40D-10: skip loading `libcust_plugin.so`. Compatible only
    /// with `cust check`; rejected by `build` and `test` (both
    /// hard-require the plugin per V40D-12). With this flag set,
    /// `cust check` adds `-Wno-unknown-attributes` so the
    /// unrecognised `[[cust::*]]` attribute spellings don't trip
    /// `-Wunknown-attributes`. Fragment headers are NOT emitted,
    /// test discovery is NOT performed, and `cust check` becomes
    /// a syntax-only escape hatch with no link promise.
    #[arg(long, global = true)]
    pub no_plugin: bool,

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
    /// Build and run the crate's unit tests (v0.3.2).
    Test(TestArgs),
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
    /// Maximum number of parallel build jobs (v0.4.2 V42D-13 +
    /// roadmap v0.4.3). Lowered to `cmake --build -j <N>` so
    /// Ninja owns intra-crate and inter-crate parallelism in
    /// one scheduler. When omitted, Ninja picks (defaults to
    /// `nproc`). Falls back to `$CUST_JOBS` or
    /// `$CARGO_BUILD_JOBS` (Cargo parity) when neither flag
    /// nor env is set.
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<u32>,
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
/// `cust run [-p <member>] [--release] [-j <N>] [-- <args>...]`
/// — builds the workspace, picks a runnable bin member (the
/// only bin if `-p` is omitted; the named member otherwise),
/// then spawns the resulting executable with everything after
/// `--` as argv.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Build (and run) with the `release` profile.
    #[arg(long)]
    pub release: bool,
    /// Select which workspace member to run. Required when more
    /// than one bin member exists.
    #[arg(short = 'p', long = "package")]
    pub package: Option<String>,
    /// Build parallelism. See `cust build --jobs`.
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<u32>,
    /// Arguments forwarded to the spawned binary. Anything after
    /// `--` lands here.
    #[arg(last = true, allow_hyphen_values = true)]
    pub forwarded: Vec<String>,
}

/// Arguments for `cust test` (v0.3.2 V32D-9 / V32D-10).
///
/// `cust test [-p <member>] [--release] [<filter>] [-- <runner-args>...]`
/// builds every testable member's test binary (lib or lib+bin;
/// bin-only members are skipped silently per V32D-12, unless
/// `-p <bin-only>` is explicit per V32D-11) and runs each in
/// turn. `<filter>` is forwarded as the first runner argv
/// (substring match against `module::name`); everything after
/// `--` is appended after that.
#[derive(Debug, clap::Args)]
pub struct TestArgs {
    /// Build (and run) with the `release` profile.
    #[arg(long)]
    pub release: bool,
    /// Restrict the test run to one workspace member. Bin-only
    /// members named here are rejected with the V32D-11 error.
    #[arg(short = 'p', long = "package")]
    pub package: Option<String>,
    /// Build parallelism. See `cust build --jobs`.
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<u32>,
    /// Substring filter forwarded to the runner. Matches against
    /// the runner's `module::name` qualified name (V32D-9).
    pub filter: Option<String>,
    /// Extra arguments forwarded to the runner after the filter.
    /// `--list` is the only v0.3.2 runner flag; other names are
    /// passed through for forward compatibility.
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
        let no_plugin = self.no_plugin;
        match self.command {
            Cmd::Build(args) => run_build(
                profile_kind(args.release),
                args.package.as_deref(),
                resolve_jobs(args.jobs)?,
                no_plugin,
            ),
            Cmd::Check(args) => run_check(
                profile_kind(args.release),
                args.package.as_deref(),
                no_plugin,
            ),
            Cmd::Run(args) => run_run(
                profile_kind(args.release),
                args.package.as_deref(),
                resolve_jobs(args.jobs)?,
                &args.forwarded,
                no_plugin,
            ),
            Cmd::Test(args) => run_test(
                profile_kind(args.release),
                args.package.as_deref(),
                resolve_jobs(args.jobs)?,
                args.filter.as_deref(),
                &args.forwarded,
                no_plugin,
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

/// V42D-13 / v0.4.3-roadmap `--jobs` resolution.
///
/// Precedence (matches Cargo's `--jobs` story):
/// 1. Explicit `--jobs N` on the command line.
/// 2. `$CUST_JOBS` (cust-native name).
/// 3. `$CARGO_BUILD_JOBS` (Cargo parity — lets users keep one
///    env var across both ecosystems).
/// 4. Nothing — Ninja picks (`nproc`).
///
/// Errors on a non-positive integer or a non-numeric value in
/// either env var so the user gets a clear diagnostic instead of
/// silent fall-through. `--jobs 0` is rejected.
fn resolve_jobs(cli: Option<u32>) -> Result<Option<u32>> {
    if let Some(n) = cli {
        if n == 0 {
            anyhow::bail!("`--jobs 0` is not allowed (use `--jobs 1` for serial)");
        }
        return Ok(Some(n));
    }
    for env_var in ["CUST_JOBS", "CARGO_BUILD_JOBS"] {
        if let Some(raw) = env::var_os(env_var) {
            let s = raw.to_string_lossy();
            let parsed: u32 = s
                .parse()
                .with_context(|| format!("parsing ${env_var}={s:?} as a positive integer"))?;
            if parsed == 0 {
                anyhow::bail!("${env_var}=0 is not allowed (use 1 for serial)");
            }
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

/// Locate the workspace by walking up from cwd. Returns a fully
/// resolved `Workspace` (member list + dep edges). For single-
/// crate projects this is a one-implicit-member workspace with
/// no `[workspace]` table.
fn locate(cwd: &Path) -> Result<Workspace> {
    Workspace::discover(cwd)
}

/// V40D-10 + V40D-12 plugin resolution. Subcommand contract:
///
///   * `build` / `test` — plugin is mandatory. `--no-plugin`
///     is rejected with the V40D-10 verbatim error. Plugin
///     missing on disk is the V40D-12 hard error.
///   * `check` / `run` — plugin is optional. `--no-plugin`
///     skips discovery (clean `Ok(None)`). Plugin missing
///     when not explicitly disabled emits a warning so users
///     hear about it before it bites them on `cust build`.
///
/// `run` reuses this because it always builds first; building
/// requires the plugin, so `run --no-plugin` is rejected too.
fn resolve_plugin(no_plugin: bool, subcommand: &str) -> Result<Option<crate::plugin::Plugin>> {
    let requires_plugin = matches!(subcommand, "build" | "test" | "run");

    if no_plugin && requires_plugin {
        // V40D-10 rejection wording.
        anyhow::bail!(
            "`--no-plugin` is incompatible with `cust {subcommand}` \
             (fragment headers and/or test discovery require the plugin)\n  \
             hint: drop `--no-plugin`, or use `cust check --no-plugin` \
             for a syntax-only pass"
        );
    }

    if no_plugin {
        // `cust check --no-plugin`: caller wants the syntax-only
        // escape hatch. Skip discovery entirely.
        return Ok(None);
    }

    let plugin = crate::plugin::Plugin::discover();

    if requires_plugin && plugin.is_none() {
        // V40D-12 verbatim wording.
        let env_value = std::env::var("CUST_PLUGIN").unwrap_or_else(|_| "not set".to_string());
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .map_or_else(|| "<unknown>".to_string(), |p| p.display().to_string());
        anyhow::bail!(
            "cust plugin (libcust_plugin.so) not found\n  \
             searched:\n    \
             $CUST_PLUGIN: {env_value}\n    \
             {exe_dir}/libcust_plugin.so: not found\n  \
             hint: build the plugin with `cargo run -p plugin-build`"
        );
    }

    if !requires_plugin && plugin.is_none() {
        eprintln!(
            "warning: cust plugin (libcust_plugin.so) not found — `cust {subcommand}` \
             will proceed without it. `cust build` and `cust test` will hard-error \
             until the plugin is built (`cargo run -p plugin-build`)."
        );
    }

    Ok(plugin)
}

fn run_build(
    profile_kind: ProfileKind,
    package: Option<&str>,
    jobs: Option<u32>,
    no_plugin: bool,
) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = resolve_plugin(no_plugin, "build")?;

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        test_build: false,
        only: package,
        jobs,
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

fn run_check(profile_kind: ProfileKind, package: Option<&str>, no_plugin: bool) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = resolve_plugin(no_plugin, "check")?;

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: true,
        test_build: false,
        only: package,
        // V42D-15: `cust check` bypasses CMake entirely — the
        // jobs field has no consumer here.
        jobs: None,
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
fn run_run(
    profile_kind: ProfileKind,
    package: Option<&str>,
    jobs: Option<u32>,
    forwarded: &[String],
    no_plugin: bool,
) -> Result<()> {
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
    let plugin = resolve_plugin(no_plugin, "run")?;
    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        test_build: false,
        only: Some(&target_name),
        jobs,
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

/// `cust test` — build every testable workspace member's test
/// binary (lib or lib+bin; bin-only members are skipped per
/// V32D-12, unless explicitly named via `-p` per V32D-11),
/// then run each one in turn with `[filter] + forwarded` as
/// argv.
///
/// Exit code: 0 if every test binary exited 0; 1 if any
/// member's test binary exited non-zero. Bare `cust test` on
/// a workspace with no testable members (only bin-only crates)
/// exits 0 — that matches Cargo's behaviour ("0 tests" is not
/// itself an error).
fn run_test(
    profile_kind: ProfileKind,
    package: Option<&str>,
    jobs: Option<u32>,
    filter: Option<&str>,
    forwarded: &[String],
    no_plugin: bool,
) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;

    // V32D-11: explicit `-p <bin-only>` is an error before we
    // build anything. Lib+bin is fine (we test the lib half).
    if let Some(name) = package {
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
        if !m.kind.has_lib() {
            anyhow::bail!(
                "workspace member `{name}` is a bin-only crate; \
                 cust test v0.3.2 only runs unit tests in library crates \
                 (lib+bin members test their library half only)"
            );
        }
    }

    let clang = Clang::discover()?;
    let plugin = resolve_plugin(no_plugin, "test")?;
    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        test_build: true,
        only: package,
        jobs,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    for (name, out) in &outputs.per_member {
        if let Some(test_exe) = &out.test_executable {
            println!("  Finished {name} [test] -> {}", test_exe.display());
        }
    }

    // Run each test binary in turn. We honour the workspace
    // build order (deps first), since a test depending on a
    // sibling dep wants the dep's tests to have already passed
    // anyway. Members without a test_executable (bin-only
    // skipped per V32D-12) drop out here naturally.
    let mut overall_failed = false;
    for (name, out) in &outputs.per_member {
        let Some(test_exe) = out.test_executable.as_deref() else {
            continue;
        };

        println!("     Running {}", test_exe.display());

        // argv = [filter?, forwarded...]. The runner parses
        // `<filter>` as its single positional and treats every
        // following non-flag token as either ignored or a
        // future flag (V32D-10).
        let mut child = std::process::Command::new(test_exe);
        if let Some(f) = filter {
            child.arg(f);
        }
        child.args(forwarded);

        let status = child
            .stdin(std::process::Stdio::null())
            .status()
            .with_context(|| format!("spawning `{}`", test_exe.display()))?;

        if !status.success() {
            overall_failed = true;
            // Don't bail; keep running so the user sees every
            // member's status in one pass (matches Cargo's
            // `cargo test --workspace` behaviour).
            eprintln!("error: test binary for `{name}` failed");
        }
    }

    if overall_failed {
        std::process::exit(1);
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
