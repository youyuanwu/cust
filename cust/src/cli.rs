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
    /// v0.4.5 V45D-2: hidden leaf generators invoked by the
    /// generated `CMakeLists` (NOT a public contract). Each
    /// produces one artifact (a `#cust use` rewrite, one module's
    /// surface fragment, or the concatenated crate header) so
    /// Ninja can own generation incrementality.
    #[command(hide = true, subcommand)]
    Internal(InternalCmd),
}

/// v0.4.5 V45D-2: the three `cust internal …` leaf generators.
/// All hidden; the emitter bakes the exact argv into the
/// `add_custom_command` lines.
#[derive(Debug, Subcommand)]
pub enum InternalCmd {
    /// Lower one source file's `#cust use` directives to an
    /// `#include`-only rewrite (V45D-3).
    #[command(hide = true)]
    RewriteFile(RewriteFileArgs),
    /// Surface-compile one module to produce its fragment header
    /// (V45D-4). One-shot — imported fragments must already exist.
    #[command(hide = true)]
    SurfaceModule(SurfaceModuleArgs),
    /// Concatenate per-module fragments into the published crate
    /// header (V45D-5).
    #[command(hide = true)]
    CrateHeader(CrateHeaderArgs),
}

#[derive(Debug, clap::Args)]
pub struct RewriteFileArgs {
    /// The crate the source belongs to (own-lib carve-out).
    #[arg(long)]
    pub crate_name: String,
    /// Source `.c` to lower.
    #[arg(long = "in")]
    pub input: PathBuf,
    /// Rewritten output path.
    #[arg(long)]
    pub out: PathBuf,
    /// `target/<profile>/.h-fragments/<crate>/`.
    #[arg(long)]
    pub frags_dir: PathBuf,
    /// `target/<profile>/deps/`.
    #[arg(long)]
    pub deps_root: PathBuf,
    /// The member's own published header (bin-half carve-out).
    #[arg(long)]
    pub own_lib_header: PathBuf,
    /// Dep crate names this source may `#cust use <dep>;`.
    #[arg(long = "dep")]
    pub deps: Vec<String>,
    /// Lowering the bin half of a lib+bin crate.
    #[arg(long)]
    pub bin_half: bool,
    /// Whether the member has a lib half (gates the carve-out).
    #[arg(long)]
    pub has_lib: bool,
}

#[derive(Debug, clap::Args)]
pub struct SurfaceModuleArgs {
    /// Module source `.c`.
    #[arg(long)]
    pub source: PathBuf,
    /// Where to write the lowered surface TU.
    #[arg(long)]
    pub surface_out: PathBuf,
    /// Where the plugin writes this module's fragment header.
    #[arg(long)]
    pub fragment_out: PathBuf,
    /// `target/<profile>/.h-fragments/<crate>/`.
    #[arg(long)]
    pub frags_dir: PathBuf,
    /// `target/<profile>/deps/`.
    #[arg(long)]
    pub deps_root: PathBuf,
    /// Dep crate names this module may `#cust use <dep>;`.
    #[arg(long = "dep")]
    pub deps: Vec<String>,
    /// `-std=<value>` for the surface compile.
    #[arg(long)]
    pub std: String,
    /// Mid cflags (profile cflags + `[clang] extra-cflags`), in
    /// order. Repeated.
    #[arg(long = "cflag", allow_hyphen_values = true)]
    pub cflags: Vec<String>,
    /// Extra `-I<dir>` include dirs. Repeated.
    #[arg(long = "include")]
    pub includes: Vec<PathBuf>,
    /// The materialised prelude header (`-include`d).
    #[arg(long)]
    pub prelude: PathBuf,
    /// The cust clang plugin `.so`. Omitted ⇒ no plugin (no
    /// fragment emitted — only meaningful for `--no-plugin`).
    #[arg(long)]
    pub plugin: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct CrateHeaderArgs {
    /// The crate whose surface is being published.
    #[arg(long)]
    pub crate_name: String,
    /// Output path for the concatenated `<crate>.h`.
    #[arg(long)]
    pub out: PathBuf,
    /// Fragment header paths in topological order. Repeated.
    #[arg(long = "frag")]
    pub frags: Vec<PathBuf>,
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
    /// Build only the named binary (v0.4.4 V44D-7). Without `-p`,
    /// the bin name must be unique across the workspace.
    #[arg(long = "bin")]
    pub bin: Option<String>,
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
    /// Select which binary to run (v0.4.4 V44D-6). Required when
    /// the selected member has more than one bin. Without `-p`,
    /// the bin name must be unique across the workspace.
    #[arg(long = "bin")]
    pub bin: Option<String>,
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
                args.bin.as_deref(),
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
                args.bin.as_deref(),
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
            Cmd::Internal(cmd) => run_internal(cmd),
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
    bin: Option<&str>,
    jobs: Option<u32>,
    no_plugin: bool,
) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;
    let clang = Clang::discover()?;
    let plugin = resolve_plugin(no_plugin, "build")?;

    // v0.4.4 V44D-7: `--bin <name>` resolves to its owning member
    // (scoping the build to that one bin's target).
    let bin_owner = match bin {
        Some(name) => Some(resolve_bin_owner(&ws, package, name)?),
        None => None,
    };
    let only = package.or(bin_owner.as_deref());

    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        test_build: false,
        only,
        bin,
        jobs,
    };
    let outputs = workspace::build_workspace(&ws, &opts)?;

    if let Some(p) = &plugin {
        eprintln!("  Plugin   {}", p.path.display());
    }
    for (name, out) in &outputs.per_member {
        // v0.3.1: a member may produce an archive, an executable,
        // or both. v0.4.4: a member may produce multiple bins.
        // Print whatever was produced (in produce order). When
        // `--bin` scoped the build, report only that bin.
        let label = profile_kind.manifest_name();
        if bin.is_none() {
            if let Some(arch) = &out.archive {
                println!("  Finished {name} [{label}] -> {}", arch.display());
            }
        }
        for (bin_name, exe) in &out.executables {
            if bin.is_none_or(|b| b == bin_name) {
                println!("  Finished {name} [{label}] -> {}", exe.display());
            }
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
        bin: None,
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

/// v0.4.5 V45D-2: dispatch the hidden `cust internal …` leaf
/// generators. Each re-resolves only what its arguments name (no
/// workspace discovery, no `Cust.lock`), produces one artifact,
/// and exits. Invoked solely by the generated `CMakeLists`.
fn run_internal(cmd: InternalCmd) -> Result<()> {
    match cmd {
        InternalCmd::RewriteFile(a) => {
            trace_internal("rewrite-file", &a.out);
            let deps: Vec<&str> = a.deps.iter().map(String::as_str).collect();
            let ctx = crate::generate::RewriteCtx {
                crate_name: &a.crate_name,
                source_path: &a.input,
                out_path: &a.out,
                frags_dir: &a.frags_dir,
                deps_root: &a.deps_root,
                own_lib_header: &a.own_lib_header,
                deps: &deps,
                is_bin_half: a.bin_half,
                has_lib: a.has_lib,
            };
            crate::generate::rewrite_one(&ctx)
        }
        InternalCmd::SurfaceModule(a) => {
            trace_internal("surface-module", &a.fragment_out);
            let clang = Clang::discover()?;
            let plugin = a.plugin.map(|path| crate::plugin::Plugin { path });
            let includes: Vec<&Path> = a.includes.iter().map(PathBuf::as_path).collect();
            // V45D-15: rebuild the exact `build_cflags` argv from
            // the serialised pieces. Object path is irrelevant —
            // `surface_one_module` truncates the trailing
            // `-c -o <obj> <src>`.
            let dummy_obj = a.surface_out.with_extension("surface.o");
            let base_cflags = crate::build::build_cflags_raw(
                &a.std,
                &a.cflags,
                false,
                plugin.as_ref(),
                &a.prelude,
                &a.surface_out,
                &dummy_obj,
                &includes,
                crate::build::PluginOutputs {
                    fragment: Some(&a.fragment_out),
                    test_sidecar: None,
                    module: None,
                },
            );
            let deps: Vec<&str> = a.deps.iter().map(String::as_str).collect();
            let ctx = crate::generate::SurfaceCtx {
                source_path: &a.source,
                surface_out: &a.surface_out,
                fragment_out: &a.fragment_out,
                frags_dir: &a.frags_dir,
                deps_root: &a.deps_root,
                deps: &deps,
                // V45D-4: one-shot leaf — a missing imported
                // fragment is a graph bug, not a recoverable blank.
                require_upstream: true,
            };
            crate::generate::surface_one_module(&ctx, &clang, &base_cflags)
        }
        InternalCmd::CrateHeader(a) => {
            trace_internal("crate-header", &a.out);
            // Derive each fragment's qualified name from its file
            // stem (`<qname>.cust.h`), preserving the emitter's
            // topological `--frag` order.
            let frags: Vec<(String, PathBuf)> = a
                .frags
                .iter()
                .map(|p| {
                    let qname = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.strip_suffix(".cust.h"))
                        .unwrap_or_default()
                        .to_string();
                    (qname, p.clone())
                })
                .collect();
            crate::generate::write_crate_header_concat(&a.crate_name, &a.out, &frags)
        }
    }
}

/// v0.4.5 V45D-12: when `CUST_TRACE_INTERNAL=<path>` is set,
/// append one `<leaf> <output>` line per `internal` leaf
/// invocation. The no-op-build regression test points the env var
/// at a scratch file, builds cwork twice, and asserts the file is
/// untouched by the second build (zero codegen spawns). A no-op in
/// normal runs (the env var is unset). Best-effort: a trace I/O
/// error never fails the generation it is observing.
fn trace_internal(leaf: &str, output: &Path) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("CUST_TRACE_INTERNAL") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{leaf} {}", output.display());
    }
}

/// Format a list of names as `` `a`, `b`, `c` `` for error
/// messages (v0.4.4 V44D-6).
fn quoted_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// v0.4.4 V44D-6/V44D-7: resolve `--bin <name>` to its owning
/// workspace member. When `package` is `Some`, the search is
/// scoped to that member; otherwise every member is searched and
/// a name owned by two members is an "ambiguous across packages"
/// error.
fn resolve_bin_owner(
    ws: &workspace::Workspace,
    package: Option<&str>,
    bin: &str,
) -> Result<String> {
    let owners: Vec<&str> = ws
        .members
        .iter()
        .filter(|m| package.is_none_or(|p| m.name == p))
        .filter(|m| m.kind.bins().iter().any(|b| b.name == bin))
        .map(|m| m.name.as_str())
        .collect();
    match owners.as_slice() {
        [] => {
            let all: Vec<&str> = ws
                .members
                .iter()
                .flat_map(|m| m.kind.bins())
                .map(|b| b.name.as_str())
                .collect();
            anyhow::bail!(
                "no binary named `{bin}` in the workspace — available \
                 binaries are {}",
                quoted_list(&all)
            )
        }
        [one] => Ok((*one).to_string()),
        many => anyhow::bail!(
            "binary `{bin}` is ambiguous across packages {} — pass \
             `-p <member>` to choose one",
            quoted_list(many)
        ),
    }
}

/// v0.4.4 V44D-6: resolve `cust run`'s target to `(member, bin)`.
///
/// Member resolution: `-p <name>` (must be a bin member);
/// `--bin <name>` without `-p` (the unique owner); otherwise the
/// sole bin member. Then the bin within the member: `--bin <name>`
/// (must exist); the sole bin; otherwise the V44D-6 ambiguity
/// error.
fn resolve_run_target(
    ws: &workspace::Workspace,
    package: Option<&str>,
    bin: Option<&str>,
) -> Result<(String, String)> {
    let target_member = if let Some(name) = package {
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
    } else if let Some(bin_name) = bin {
        resolve_bin_owner(ws, None, bin_name)?
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

    let member = ws
        .member(&target_member)
        .expect("target member resolved above");
    let bin_names: Vec<&str> = member.kind.bins().iter().map(|b| b.name.as_str()).collect();
    let target_bin = if let Some(name) = bin {
        if !bin_names.contains(&name) {
            anyhow::bail!(
                "no binary named `{name}` in package `{target_member}` — \
                 available binaries are {}",
                quoted_list(&bin_names)
            );
        }
        name.to_string()
    } else {
        match bin_names.as_slice() {
            [only] => (*only).to_string(),
            // `has_bin` guaranteed above, so the empty case is
            // unreachable; the >1 case is the V44D-6 ambiguity.
            many => anyhow::bail!(
                "could not determine which binary to run in package \
                 `{target_member}`: available binaries are {}. Use \
                 `--bin <NAME>` to select one.",
                quoted_list(many)
            ),
        }
    };
    Ok((target_member, target_bin))
}

/// `cust run` — build the workspace, locate the requested bin
/// member (or the only bin member when `-p` is omitted), then
/// spawn it with anything after `--` forwarded as argv. Exits
/// with the subprocess's exit code so shell scripts and CI
/// behave the same as if the user had run the binary directly.
fn run_run(
    profile_kind: ProfileKind,
    package: Option<&str>,
    bin: Option<&str>,
    jobs: Option<u32>,
    forwarded: &[String],
    no_plugin: bool,
) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;
    let ws = locate(&cwd)?;

    // Resolve the owning member + which bin to run (V44D-6).
    let (target_member, target_bin) = resolve_run_target(&ws, package, bin)?;

    // Build scoped to the resolved bin + its transitive deps.
    let clang = Clang::discover()?;
    let plugin = resolve_plugin(no_plugin, "run")?;
    let opts = WorkspaceBuildOptions {
        profile_kind,
        clang: &clang,
        plugin: plugin.as_ref(),
        syntax_only: false,
        test_build: false,
        only: Some(&target_member),
        bin: Some(&target_bin),
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
        for (bin_name, exe) in &out.executables {
            if bin_name == &target_bin {
                println!("  Finished {name} [{label}] -> {}", exe.display());
            }
        }
    }

    // Locate the executable for the resolved bin. build_workspace
    // visits members in topo order, so the target member is the
    // last entry whose name matches.
    let exe = outputs
        .per_member
        .iter()
        .rev()
        .find(|(name, _)| name == &target_member)
        .and_then(|(_, out)| {
            out.executables
                .iter()
                .find(|(bin_name, _)| bin_name == &target_bin)
                .map(|(_, exe)| exe.as_path())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: bin `{target_bin}` of `{target_member}` built \
                 but produced no executable"
            )
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
        bin: None,
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

        // v0.4.3 V43D-5/V43D-10/V43D-11: run the member's
        // integration-test exes after its unit tests. Each runs
        // with cwd = its own per-stem dir (the directory
        // containing the exe) so output files don't collide.
        for it in &out.integration_tests {
            println!("     Running {} ({})", it.source_label, it.exe.display());
            let mut child = std::process::Command::new(&it.exe);
            if let Some(f) = filter {
                child.arg(f);
            }
            child.args(forwarded);
            if let Some(cwd) = it.exe.parent() {
                child.current_dir(cwd);
            }
            let status = child
                .stdin(std::process::Stdio::null())
                .status()
                .with_context(|| format!("spawning `{}`", it.exe.display()))?;
            if !status.success() {
                overall_failed = true;
                eprintln!("error: integration test `{}` of `{name}` failed", it.stem);
            }
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
