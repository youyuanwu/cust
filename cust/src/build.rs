//! The `cust build` pipeline.
//!
//! Pipeline (per `docs/design/v0.2.md`):
//!
//! 1. Parse `Cust.toml` (already done by `Manifest::load`).
//! 2. Resolve the active profile (default `dev`; `--release` →
//!    `release`).
//! 3. Materialise the prelude to `target/<profile>/prelude.h`.
//! 4. Discover the module graph rooted at `src/lib.c` by walking
//!    `#cust mod` directives.
//! 5. For each module: scan + rewrite via `mod_scanner`, write
//!    the rewritten bytes to
//!    `target/<profile>/build/<crate>/<qname>.preprocessed.c`,
//!    then compile to `<qname>.o`.
//! 6. Archive every `.o` into `target/<profile>/lib<name>.a`.
//! 7. Emit `target/compile_commands.json` (one entry per TU).
//! 8. Stamp `target/.cust-version`.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

use crate::{
    clang::Clang,
    manifest::{CrateKind, Manifest},
    mod_scanner,
    modules::{self, Module},
    plugin::Plugin,
    profile::{ProfileKind, ResolvedProfile},
    target_layout::TargetLayout,
    test_discovery::{self, TestEntry},
    test_runner,
};

/// Inputs handed to `run` by the CLI layer.
pub struct BuildPlan<'a> {
    pub manifest: &'a Manifest,
    pub crate_root: &'a Path,
    pub workspace_root: &'a Path,
    pub profile_kind: ProfileKind,
    pub clang: &'a Clang,
    /// Discovered cust clang plugin, when present. v0.2 treats
    /// "plugin missing" as a silent skip — the v0.1 plugin-less
    /// code path still works for single-module / no-cross-import
    /// crates.
    pub plugin: Option<&'a Plugin>,
    /// When `true`, every TU is compiled with `-fsyntax-only`
    /// instead of producing an object, and no archive / executable
    /// / version stamp / `compile_commands.json` is written.
    /// This is what `cust check` runs.
    pub syntax_only: bool,
    /// Names of dep crates this consumer is allowed to import via
    /// `#cust use <name>;` (V3D-6). Validated against the
    /// scanner's `UseDep` directives. Empty for a crate that
    /// declares no `[dependencies]`. The workspace orchestrator
    /// populates this from the resolved edge list.
    pub deps: &'a [&'a str],
    /// Transitive dep names whose archives must be linked into
    /// this crate's executable (v0.3.1). For lib-only crates this
    /// is unused. `Workspace` orchestrator computes this in topo
    /// order; for non-workspace single-crate builds it's empty.
    pub link_deps: &'a [&'a str],
    /// What this crate produces (lib / bin / lib+bin). Computed
    /// by `Manifest::resolve_kind` at the workspace orchestrator
    /// (or CLI) layer.
    pub kind: CrateKind,
    /// v0.3.2 (V32D-2 / V32D-3): when `true`, the lib half is
    /// compiled with `-DCUST_TEST_BUILD=1` into a fresh
    /// `target/<profile>/test/<crate>/` tree, the driver pre-pass
    /// test scanner runs over every module, a generated
    /// `cust_test_main.c` runner is concatenated + compiled, and
    /// everything is linked into a test executable at
    /// `target/<profile>/test/<crate>/<crate>`. The bin half is
    /// **skipped** (V32D-11 — v0.3.2 only tests the library
    /// half). No archive, no crate header, no
    /// `compile_commands.json`, no `.cust-version` are emitted
    /// in this mode (the non-test `cust build` owns those).
    /// Ignored when `syntax_only` is true.
    pub test_build: bool,
}

/// Outputs `cust build` writes. `objects` and `compile_commands`
/// are reported back so callers can plumb them into future tooling
/// (e.g. `cust test`); `archive` and `executable` are what the
/// CLI prints in the `Finished` line.
#[derive(Debug)]
pub struct BuildOutputs {
    #[allow(dead_code)]
    pub objects: Vec<PathBuf>,
    /// `Some` when the crate has a lib component (`Lib` or
    /// `LibAndBin`); `None` for bin-only crates.
    pub archive: Option<PathBuf>,
    /// `Some` when the crate has a bin component (`Bin` or
    /// `LibAndBin`) and the build was not `syntax_only`; `None`
    /// otherwise.
    pub executable: Option<PathBuf>,
    /// `Some` when `plan.test_build` was true and a test binary
    /// was produced (V32D-4 / V32D-5). The path is
    /// `target/<profile>/test/<crate>/<crate>`.
    pub test_executable: Option<PathBuf>,
    #[allow(dead_code)]
    pub compile_commands: PathBuf,
}

/// One `compile_commands.json` entry.
struct CompileEntry {
    directory: PathBuf,
    file: PathBuf,
    arguments: Vec<String>,
}

// ─── v0.4.2 slice B: driver-side prebuild for the CMake path ─────
//
// Slice B (V42D-16) moves phase-2 codegen + link into CMake. The
// driver still owns phase 1 (surface pass + crate header concat)
// per V42D-2, plus the `#cust use` rewriting to disk so CMake has
// post-rewrite sources to compile (V42D-13 layout —
// `target/<profile>/.rewrite/<crate>/<rel>.c`). The two helpers
// below are the entry points `workspace::build_workspace` calls
// from the build / check paths.

/// Run phase 1 for one workspace member: materialise prelude,
/// surface-pass fixed-point over the lib half (if the plugin is
/// loaded), concatenate the user-facing `<crate>.h`. Idempotent;
/// safe to call before every `cmake --build` (V42D-17 — the
/// driver owns fragment freshness).
///
/// Bin-half modules are NOT surface-passed because nothing reads
/// their surface (bin has no downstream consumers). Matches the
/// existing v0.4.0 behaviour.
pub fn run_phase1(plan: &BuildPlan<'_>) -> Result<()> {
    let profile_override = match plan.profile_kind {
        ProfileKind::Dev => plan.manifest.profile.dev.as_ref(),
        ProfileKind::Release => plan.manifest.profile.release.as_ref(),
    };
    let profile = ResolvedProfile::resolve(plan.profile_kind, profile_override)?;
    let layout = TargetLayout::for_workspace(plan.workspace_root, profile.kind);
    layout.ensure_dirs()?;

    let prelude_path = layout.prelude_path();
    materialise_prelude(&prelude_path)?;

    let crate_name = plan.manifest.package_name();
    let crate_build_dir = layout.build_dir(crate_name);
    fs::create_dir_all(&crate_build_dir)
        .with_context(|| format!("creating `{}`", crate_build_dir.display()))?;

    if let Some(lib_src) = plan.kind.lib_source() {
        if !lib_src.is_file() {
            bail!(
                "library source `{}` not found (set `[lib] path` in Cust.toml to override)",
                lib_src.display()
            );
        }
        let lib_modules =
            modules::discover(plan.crate_root, lib_src).context("discovering lib module graph")?;
        if plan.plugin.is_some() {
            surface_pass_fixed_point(
                plan,
                &profile,
                &prelude_path,
                &crate_build_dir,
                &layout,
                &lib_modules,
            )?;
            write_crate_header(&layout, crate_name, &lib_modules)?;
        }
    }
    // No surface pass on the bin half (nothing downstream
    // consumes bin module surfaces).
    Ok(())
}

/// Write `#cust use`-lowered source files into
/// `target/<profile>/.rewrite/<crate>/<rel>.c` (V42D-13 layout)
/// for every lib + bin module. `CMake` compiles these directly;
/// the original `src/` tree is untouched.
///
/// Mirrors `compile_one_module`'s rewrite logic (validation +
/// directive substitution) but stops short of invoking clang —
/// `CMake`/Ninja owns codegen from v0.4.2 onward (V42D-16). Slice
/// C will fold this into a single `phase1+rewrites` walk; slice
/// B keeps them separate so the diff is bounded.
pub fn write_rewrite_tree(plan: &BuildPlan<'_>) -> Result<()> {
    let layout = TargetLayout::for_workspace(plan.workspace_root, plan.profile_kind);
    let rewrite_root = layout.profile_root.join(".rewrite");

    if let Some(lib_src) = plan.kind.lib_source() {
        let lib_modules =
            modules::discover(plan.crate_root, lib_src).context("discovering lib module graph")?;
        for m in &lib_modules {
            write_one_rewrite(plan, &layout, &rewrite_root, &m.source_path, m, false)?;
        }
    }
    if let Some(bin_src) = plan.kind.bin_source() {
        let bin_modules =
            modules::discover(plan.crate_root, bin_src).context("discovering bin module graph")?;
        for m in &bin_modules {
            write_one_rewrite(plan, &layout, &rewrite_root, &m.source_path, m, true)?;
        }
    }
    Ok(())
}

fn write_one_rewrite(
    plan: &BuildPlan<'_>,
    layout: &TargetLayout,
    rewrite_root: &Path,
    source_path: &Path,
    m: &Module,
    is_bin_half: bool,
) -> Result<()> {
    let src_text = fs::read_to_string(source_path)
        .with_context(|| format!("reading `{}`", source_path.display()))?;
    let scan = mod_scanner::scan(&src_text, source_path)?;

    let crate_name = plan.manifest.package_name();
    let own_lib_header = layout.crate_header_path(crate_name);
    let rewritten = mod_scanner::rewrite_with(&src_text, source_path, &scan, |d| match &d.kind {
        crate::mod_scanner::DirectiveKind::UseCrate { name } => {
            let frag = layout.fragment_path(crate_name, name);
            Some(format!("#include \"{}\"", frag.display()))
        }
        crate::mod_scanner::DirectiveKind::UseDep { name } => {
            if is_bin_half && plan.kind.has_lib() && name == crate_name {
                return Some(format!("#include \"{}\"", own_lib_header.display()));
            }
            let dep_header = layout
                .dep_dir(name)
                .join("include")
                .join(format!("{name}.h"));
            Some(format!("#include \"{}\"", dep_header.display()))
        }
        crate::mod_scanner::DirectiveKind::Mod { .. } => None,
    });

    // Validate `#cust use <name>;` resolves to a declared dep or
    // the own-crate carve-out (bin half of lib+bin). Same shape
    // compile_one_module enforces; same error format.
    for d in &scan.directives {
        if let crate::mod_scanner::DirectiveKind::UseDep { name } = &d.kind {
            if is_bin_half && plan.kind.has_lib() && name == crate_name {
                continue;
            }
            if !plan.deps.iter().any(|n| n == name) {
                bail!(
                    "{}:{}:{}: `#cust use {name};` refers to a crate not \
                     listed in [dependencies]; add `{name} = {{ path = \"…\" }}`",
                    source_path.display(),
                    d.span.line,
                    d.span.column
                );
            }
        }
    }

    // Output path: target/<profile>/.rewrite/<crate>/<rel>.c
    // where <rel> is the source file's path relative to the
    // crate root, preserving `src/<...>.c` shape. Matches what
    // `cmake_emit::collect_view` already expects.
    let rel = m
        .source_path
        .strip_prefix(plan.crate_root)
        .unwrap_or(&m.source_path);
    let dst = rewrite_root.join(crate_name).join(rel);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }
    write_if_byte_different(&dst, rewritten.as_bytes())?;
    Ok(())
}

/// Write `bytes` to `path` only if the contents differ from
/// what's already on disk (or the file doesn't exist yet). Saves
/// `CMake`/Ninja from spuriously rebuilding TUs whose post-rewrite
/// bytes are unchanged across `cust build` invocations.
fn write_if_byte_different(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(existing) = fs::read(path) {
        if existing == bytes {
            return Ok(());
        }
    }
    fs::write(path, bytes).with_context(|| format!("writing `{}`", path.display()))
}

/// Synthesize the per-member `BuildOutputs` from `layout` for
/// the v0.4.2 `CMake`-driven path. `CMake` produces the actual
/// artifacts at the predictable paths V42D-13 pins
/// (`build/<crate>/lib<crate>.a` via `ARCHIVE_OUTPUT_DIRECTORY`,
/// `<profile_root>/<crate>` via `RUNTIME_OUTPUT_DIRECTORY`); the
/// driver doesn't track per-TU object files (`Ninja` does).
pub fn cmake_outputs_for(plan: &BuildPlan<'_>, layout: &TargetLayout) -> BuildOutputs {
    let crate_name = plan.manifest.package_name();
    let archive = plan.kind.has_lib().then(|| {
        layout
            .build_dir(crate_name)
            .join(format!("lib{crate_name}.a"))
    });
    let executable = plan
        .kind
        .has_bin()
        .then(|| layout.profile_root.join(crate_name));
    BuildOutputs {
        objects: Vec::new(),
        archive,
        executable,
        test_executable: None,
        compile_commands: layout.target_root.join("compile_commands.json"),
    }
}

#[allow(clippy::too_many_lines)] // staged lib+bin pipeline; splitting further hurts readability more than it helps
pub fn run(plan: &BuildPlan<'_>) -> Result<BuildOutputs> {
    if plan.test_build && !plan.syntax_only {
        // V32D-11: bin-only members produce no test artefacts.
        // The workspace orchestrator calls run() for every
        // in-scope member, including bin-only ones; silently
        // return an empty result for those (V32D-12 also
        // requires this — bare `cust test` skips bin-only
        // members without erroring).
        if !plan.kind.has_lib() {
            return Ok(BuildOutputs {
                objects: Vec::new(),
                archive: None,
                executable: None,
                test_executable: None,
                compile_commands: plan
                    .workspace_root
                    .join("target")
                    .join("compile_commands.json"),
            });
        }
        return run_test_build(plan);
    }

    // Step 2: resolve profile.
    let profile_override = match plan.profile_kind {
        ProfileKind::Dev => plan.manifest.profile.dev.as_ref(),
        ProfileKind::Release => plan.manifest.profile.release.as_ref(),
    };
    let profile = ResolvedProfile::resolve(plan.profile_kind, profile_override)?;

    let layout = TargetLayout::for_workspace(plan.workspace_root, profile.kind);
    layout.ensure_dirs()?;

    // Step 3: materialise prelude.
    let prelude_path = layout.prelude_path();
    materialise_prelude(&prelude_path)?;

    let crate_name = plan.manifest.package_name();
    let crate_build_dir = layout.build_dir(crate_name);
    fs::create_dir_all(&crate_build_dir)
        .with_context(|| format!("creating `{}`", crate_build_dir.display()))?;

    // Combined compile_commands.json entries from both halves
    // (lib first, then bin). Two entries per module: one for the
    // rewritten file we actually compiled, one for the original
    // source clangd will open.
    let mut compile_entries: Vec<CompileEntry> = Vec::new();
    let mut all_objects: Vec<PathBuf> = Vec::new();

    // ─── Lib half ───────────────────────────────────────────────
    //
    // Produces objects, an archive, and the concatenated crate
    // header. Skipped for bin-only crates.
    let mut archive_path: Option<PathBuf> = None;
    if let Some(lib_src) = plan.kind.lib_source() {
        if !lib_src.is_file() {
            bail!(
                "library source `{}` not found (set `[lib] path` in Cust.toml to override)",
                lib_src.display()
            );
        }
        let lib_modules =
            modules::discover(plan.crate_root, lib_src).context("discovering lib module graph")?;

        // Surface-extraction pass for cross-module fragment
        // headers. V40D-5 makes fragment emission phase-1-only;
        // v0.3 used to emit fragments as a side effect of the
        // codegen invocation when no cross-module imports were
        // present, but that's no longer permitted (the plugin
        // hard-errors if `fragment-out` arrives in phase 2). We
        // run surface_pass unconditionally whenever the plugin
        // is loaded so the per-crate header concat step in
        // `write_crate_header` below has fragments to read.
        // V40D-11 wraps the call in a fixed-point loop so
        // circular `[[cust::pub_repr]]` dependencies converge
        // (no pub_repr cycles in cwork today — iter 1 always
        // wins — but the loop is in place for the moment one
        // appears).
        if plan.plugin.is_some() {
            surface_pass_fixed_point(
                plan,
                &profile,
                &prelude_path,
                &crate_build_dir,
                &layout,
                &lib_modules,
            )?;
        }

        let objects = compile_tree(
            plan,
            &profile,
            &prelude_path,
            &crate_build_dir,
            &layout,
            &lib_modules,
            /* extra_includes = */ &[],
            /* is_bin_half = */ false,
            &mut compile_entries,
        )?;

        if !plan.syntax_only {
            // v0.3 (workspaces) archive location:
            // `target/<profile>/build/<crate>/lib<crate>.a` so
            // per-member outputs don't collide. The dep-view
            // symlink (V3D-5) exposes this to consumers.
            let archive = crate_build_dir.join(format!("lib{crate_name}.a"));
            archive_objects(&objects, &archive)?;
            archive_path = Some(archive);
        }
        all_objects.extend(objects);

        // Concatenate fragment headers into the user-facing crate
        // header (cust-design.md §5). Runs in *both* build and
        // check mode so downstream workspace members can resolve
        // upstream `#cust use <name>;` includes during their own
        // check pass. Only emit when fragments actually exist
        // (plugin was loaded).
        if plan.plugin.is_some() {
            write_crate_header(&layout, crate_name, &lib_modules)?;
        }
    }

    // ─── Bin half ───────────────────────────────────────────────
    //
    // Produces objects and an executable at
    // `target/<profile>/<crate-name>` (V31D-4). For lib+bin
    // crates the bin compile is given `-I<lib-include-dir>` so
    // `main.c` can `#include "<crate>.h"` to reach the lib's
    // exported surface. The bin half does NOT emit fragment
    // headers (bins have no downstream consumers).
    let mut executable: Option<PathBuf> = None;
    if let Some(bin_src) = plan.kind.bin_source() {
        if !bin_src.is_file() {
            bail!(
                "binary source `{}` not found (set `[[bin]] path` in Cust.toml to override)",
                bin_src.display()
            );
        }
        let bin_modules =
            modules::discover(plan.crate_root, bin_src).context("discovering bin module graph")?;

        // Extra include so the bin can reach the lib's
        // concatenated public header at `<name>.h`.
        let lib_include_dir = layout
            .crate_header_path(crate_name)
            .parent()
            .map(Path::to_path_buf);
        let extra_includes: Vec<&Path> = lib_include_dir
            .as_deref()
            .filter(|_| plan.kind.has_lib())
            .into_iter()
            .collect();

        let objects = compile_tree(
            plan,
            &profile,
            &prelude_path,
            &crate_build_dir,
            &layout,
            &bin_modules,
            &extra_includes,
            /* is_bin_half = */ true,
            &mut compile_entries,
        )?;

        if !plan.syntax_only {
            let exe_path = layout.profile_root.join(crate_name);
            link_executable(
                plan,
                &profile,
                &objects,
                archive_path.as_deref(),
                &layout,
                &exe_path,
            )?;
            executable = Some(exe_path);
        }
        all_objects.extend(objects);
    }

    // Step 7 + 8: compile_commands.json + version stamp. Always
    // at `target/`, never per-profile.
    let cc_path = layout.target_root.join("compile_commands.json");
    if !plan.syntax_only {
        write_compile_commands(&cc_path, &compile_entries)?;
        write_version_stamp(&layout.target_root.join(".cust-version"), plan.clang)?;
    }

    Ok(BuildOutputs {
        objects: all_objects,
        archive: archive_path,
        executable,
        test_executable: None,
        compile_commands: cc_path,
    })
}

/// v0.3.2 test-build pipeline, V40D-6 sidecar-driven.
/// Per V32D-2 / V32D-3 / V32D-4 / V32D-6 / V32D-7 / V32D-11
/// (still the v0.3.2 framing) and v0.4.0 V40D-6 (plugin-only
/// discovery):
///
/// 1. Compile the lib half's TUs with `-DCUST_TEST_BUILD=1` into
///    a fresh `target/<profile>/test/<crate>/` tree. Bin half is
///    skipped (V32D-11 — v0.3.2 only tests the library half).
/// 2. Read the per-module test-discovery sidecars the plugin
///    wrote during `surface_pass` (V40D-6, V40D-5: phase-1 only)
///    into `Vec<TestEntry>`.
/// 3. Render + write + compile the `cust_test_main.c` runner
///    TU with the per-test extern decls and the
///    `__cust_tests[]` table (V32D-6).
/// 4. Link every test-build object plus any transitive dep
///    archive into the test binary at
///    `target/<profile>/test/<crate>/<crate>`.
///
/// No archive, no crate header, no `compile_commands.json`, no
/// version stamp are emitted — those belong to the non-test
/// `cust build` pipeline. The dep-view symlink farm refresh
/// `build_workspace` does after each member-build is also
/// harmless in test-build mode: it points at the *non-test*
/// build dir which may or may not exist; consumers that
/// `#cust use <dep>;` in test code see the dep's normal lib
/// header just like any other consumer would.
fn run_test_build(plan: &BuildPlan<'_>) -> Result<BuildOutputs> {
    let profile_override = match plan.profile_kind {
        ProfileKind::Dev => plan.manifest.profile.dev.as_ref(),
        ProfileKind::Release => plan.manifest.profile.release.as_ref(),
    };
    let profile = ResolvedProfile::resolve(plan.profile_kind, profile_override)?;

    let layout = TargetLayout::for_workspace(plan.workspace_root, profile.kind);
    layout.ensure_dirs()?;

    let prelude_path = layout.prelude_path();
    materialise_prelude(&prelude_path)?;

    let crate_name = plan.manifest.package_name();
    let test_dir = layout.test_build_dir(crate_name);
    fs::create_dir_all(&test_dir).with_context(|| format!("creating `{}`", test_dir.display()))?;

    // V32D-11: bin-only members are rejected by the CLI layer
    // (slice D). Internally we just no-op when there's no lib
    // half — the CLI layer is responsible for the user-facing
    // error message.
    let Some(lib_src) = plan.kind.lib_source() else {
        bail!(
            "cust test internal: member `{crate_name}` has no library half — \
             callers must reject bin-only members before invoking the test build"
        );
    };
    if !lib_src.is_file() {
        bail!(
            "library source `{}` not found (set `[lib] path` in Cust.toml to override)",
            lib_src.display()
        );
    }

    let lib_modules =
        modules::discover(plan.crate_root, lib_src).context("discovering lib module graph")?;

    // Same surface pass + crate-header sequence the lib half
    // normally runs. Tests can `#cust use crate::<sibling>;`
    // exactly like production code. V40D-5: surface_pass runs
    // unconditionally when the plugin is loaded (see the
    // matching change in `run`). V40D-11 fixed-point wrapper.
    if plan.plugin.is_some() {
        surface_pass_fixed_point(
            plan,
            &profile,
            &prelude_path,
            &test_dir,
            &layout,
            &lib_modules,
        )?;
    }

    // Compile the lib half's TUs into the test build dir with
    // -DCUST_TEST_BUILD=1 (added by build_cflags when
    // plan.test_build is true — see build_cflags above).
    let mut compile_entries: Vec<CompileEntry> = Vec::new();
    let mut objects = compile_tree(
        plan,
        &profile,
        &prelude_path,
        &test_dir,
        &layout,
        &lib_modules,
        /* extra_includes = */ &[],
        /* is_bin_half = */ false,
        &mut compile_entries,
    )?;

    // V40D-6: read test entries from the plugin-emitted
    // sidecars. Each module's sidecar lives at
    // `target/<profile>/.test-discovery/<crate>/<qname>.cust.tests`;
    // surface_pass populated them when plan.test_build is true.
    // A module with zero tests still has a (possibly empty)
    // sidecar after surface_pass, so a missing file means
    // surface_pass didn't run that module — bug, hard error.
    let mut tests: Vec<TestEntry> = Vec::new();
    for m in &lib_modules {
        let sidecar_path = layout.test_sidecar_path(crate_name, &m.qualified_name);
        let contents = fs::read_to_string(&sidecar_path).with_context(|| {
            format!(
                "reading test-discovery sidecar `{}`",
                sidecar_path.display()
            )
        })?;
        let mut found = test_discovery::parse(&contents, &sidecar_path)?;
        tests.append(&mut found);
    }

    // Render + write the runner TU. Always emitted (even with
    // zero discovered tests) so the link step has a concrete
    // `main`; the runner prints `running 0 tests` and exits 0
    // in that case, matching Cargo's empty-suite behaviour.
    let main_c_src = test_runner::render_main_c(&tests);
    let main_c_path = test_dir.join("cust_test_main.c");
    fs::write(&main_c_path, main_c_src)
        .with_context(|| format!("writing `{}`", main_c_path.display()))?;

    // Compile the runner TU. We bypass compile_one_module
    // because the runner needs no #cust use rewriting, no
    // fragment emission, no extra includes, and definitely no
    // self-import carve-out. Straight clang -c -o.
    let main_obj = test_dir.join("cust_test_main.o");
    let main_cflags = build_runner_cflags(plan, &profile, &prelude_path, &main_c_path, &main_obj);
    let status = plan
        .clang
        .command()
        .args(&main_cflags)
        .stdin(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "invoking `{}` to compile `cust_test_main.c`",
                plan.clang.path.display()
            )
        })?;
    if !status.success() {
        bail!(
            "clang exited with status {status} compiling the test runner TU `{}`",
            main_c_path.display(),
        );
    }
    objects.push(main_obj);

    // Link everything into the test binary. Direct object link
    // (no own-archive step — saves a write + matches how Cargo
    // builds its own test binaries: cargo test for a lib crate
    // doesn't go through libfoo.rlib, it links the test code
    // against the lib's object files directly). Dep archives
    // come from plan.link_deps as usual.
    let exe_path = layout.test_executable_path(crate_name);
    link_executable(
        plan, &profile, &objects, /* own_archive = */ None, &layout, &exe_path,
    )?;

    Ok(BuildOutputs {
        objects,
        archive: None,
        executable: None,
        test_executable: Some(exe_path),
        compile_commands: layout.target_root.join("compile_commands.json"),
    })
}

/// Build the clang argv for the generated `cust_test_main.c`
/// runner TU. Almost the same as `build_cflags` but without the
/// plugin / fragment / module-include plumbing that doesn't
/// apply to the runner.
fn build_runner_cflags(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    source: &Path,
    object: &Path,
) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();

    let std_flag = plan
        .manifest
        .clang
        .std
        .as_deref()
        .unwrap_or_else(|| plan.clang.default_std());
    flags.push(format!("-std={std_flag}"));

    flags.extend(profile.cflags());
    flags.extend(plan.manifest.clang.extra_cflags.iter().cloned());

    flags.push("-fvisibility=hidden".to_string());
    flags.push("-DCUST_TEST_BUILD=1".to_string());

    flags.push("-include".to_string());
    flags.push(prelude.display().to_string());

    flags.push("-c".to_string());
    flags.push("-o".to_string());
    flags.push(object.display().to_string());
    flags.push(source.display().to_string());

    flags
}

/// Codegen-phase compile of every module in `modules`. Does NOT
/// emit fragment headers: V40D-5 makes fragment emission
/// phase-1-only, and the dedicated `surface_pass` (run before
/// this) takes that job. `extra_includes` is prepended to clang's
/// `-I` set so per-half concerns (e.g. bin → lib include dir)
/// propagate. `is_bin_half` controls intra-crate self-import
/// semantics: when `true`, `#cust use <own-package-name>;` is
/// accepted as Cargo-style "bin reaches its own lib" and lowered
/// to the crate's own lib include (Slice C, V31D-1 follow-up).
#[allow(clippy::too_many_arguments)] // tightly coupled to compile_one_module's surface
fn compile_tree(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    layout: &TargetLayout,
    modules: &[Module],
    extra_includes: &[&Path],
    is_bin_half: bool,
    compile_entries: &mut Vec<CompileEntry>,
) -> Result<Vec<PathBuf>> {
    let mut objects: Vec<PathBuf> = Vec::with_capacity(modules.len());

    for m in modules {
        let (rewritten_path, object_path, cflags) = compile_one_module(
            plan,
            profile,
            prelude,
            crate_build_dir,
            layout,
            extra_includes,
            is_bin_half,
            m,
        )?;
        objects.push(object_path);

        compile_entries.push(CompileEntry {
            directory: plan.crate_root.to_path_buf(),
            file: rewritten_path.clone(),
            arguments: argv_with_clang(plan, &cflags),
        });

        let original_args = swap_source_arg(
            &argv_with_clang(plan, &cflags),
            &rewritten_path,
            &m.source_path,
        );
        compile_entries.push(CompileEntry {
            directory: plan.crate_root.to_path_buf(),
            file: m.source_path.clone(),
            arguments: original_args,
        });
    }

    Ok(objects)
}

#[allow(clippy::too_many_arguments)] // tightly coupled to the per-TU pipeline; passing a struct would just move the same fields
fn compile_one_module(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    layout: &TargetLayout,
    extra_includes: &[&Path],
    is_bin_half: bool,
    m: &Module,
) -> Result<(PathBuf, PathBuf, Vec<String>)> {
    // Read + scan + rewrite. We always rewrite (even when the
    // scanner finds zero directives) so the build pipeline has
    // exactly one code path for "the bytes clang sees".
    let src_text = fs::read_to_string(&m.source_path)
        .with_context(|| format!("reading `{}`", m.source_path.display()))?;
    let scan = mod_scanner::scan(&src_text, &m.source_path)?;

    // For `#cust use crate::X;` directives, lower to an `#include`
    // of X's fragment header so the compiler sees the imported
    // module's [[cust::pub]] surface. The surface pass (when run)
    // has already populated the fragments dir.
    //
    // For `#cust use <dep>;` directives (V3D-6, v0.3), lower to
    // an `#include` of the dep's public crate header. The dep is
    // looked up by name against `plan.deps`; an unknown name is
    // a hard error pointing the user at [dependencies].
    //
    // v0.3.1 (Cargo parity): in the bin half of a lib+bin crate,
    // `#cust use <own-package-name>;` is also accepted and lowers
    // to the crate's own concatenated lib header. This is the
    // analogue of Rust's `use my_crate::*;` in src/main.rs.
    let crate_name = plan.manifest.package_name();
    let own_lib_header = layout.crate_header_path(crate_name);
    let rewritten = mod_scanner::rewrite_with(&src_text, &m.source_path, &scan, |d| {
        match &d.kind {
            crate::mod_scanner::DirectiveKind::UseCrate { name } => {
                let frag = layout.fragment_path(crate_name, name);
                Some(format!("#include \"{}\"", frag.display()))
            }
            crate::mod_scanner::DirectiveKind::UseDep { name } => {
                // Bin half importing own lib: point at the local
                // crate header directly (no dep-symlink hop —
                // single-crate non-workspace lib+bin has no
                // symlink farm to resolve through).
                if is_bin_half && plan.kind.has_lib() && name == crate_name {
                    return Some(format!("#include \"{}\"", own_lib_header.display()));
                }
                // External dep: validated below; substitute even
                // when unknown so the diagnostic block below has
                // a chance to point at line:column rather than
                // surface a missing-header compile error.
                let dep_header = layout
                    .dep_dir(name)
                    .join("include")
                    .join(format!("{name}.h"));
                Some(format!("#include \"{}\"", dep_header.display()))
            }
            crate::mod_scanner::DirectiveKind::Mod { .. } => None,
        }
    });

    // Validate every `#cust use <name>;` resolves to either a
    // declared dependency, or (bin half of lib+bin) the crate's
    // own package name. We do this *after* the rewrite so the
    // error message can include the line position, not after a
    // confusing compile failure.
    for d in &scan.directives {
        if let crate::mod_scanner::DirectiveKind::UseDep { name } = &d.kind {
            // Cargo parity carve-out: bin half may use own name.
            if is_bin_half && plan.kind.has_lib() && name == crate_name {
                continue;
            }
            if !plan.deps.iter().any(|n| n == name) {
                bail!(
                    "{}:{}:{}: `#cust use {name};` refers to a crate not \
                     listed in [dependencies]; add `{name} = {{ path = \"…\" }}`",
                    m.source_path.display(),
                    d.span.line,
                    d.span.column
                );
            }
        }
    }

    let rewritten_path = crate_build_dir.join(format!("{}.preprocessed.c", m.qualified_name));
    if let Some(parent) = rewritten_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }
    fs::write(&rewritten_path, &rewritten)
        .with_context(|| format!("writing `{}`", rewritten_path.display()))?;

    let object_path = crate_build_dir.join(format!("{}.o", m.qualified_name));

    // Honour `#include "x.h"` from the *original* source location —
    // the rewritten file lives in `target/`, so without -I clang
    // resolves relative includes against `target/` rather than the
    // user's source layout. `extra_includes` (lib+bin case: the
    // lib's include dir) is prepended ahead of the original-dir
    // include so `#include "<crate>.h"` from main.c hits the lib
    // header rather than any same-named file in the source dir.
    let original_dir = m.source_path.parent().unwrap_or(plan.crate_root);
    let mut includes: Vec<&Path> = Vec::with_capacity(extra_includes.len() + 1);
    includes.extend(extra_includes.iter().copied());
    includes.push(original_dir);
    let mut cflags = build_cflags(
        plan,
        profile,
        prelude,
        &rewritten_path,
        &object_path,
        &includes,
        PluginOutputs::default(),
    );

    // In syntax-only mode (cust check), replace the trailing
    // `-c -o <obj> <src>` (4 args, last entry of build_cflags)
    // with `-fsyntax-only <src>` so clang validates without
    // writing an object.
    if plan.syntax_only {
        let new_len = cflags.len().saturating_sub(4);
        cflags.truncate(new_len);
        cflags.push("-fsyntax-only".to_string());
        cflags.push(rewritten_path.display().to_string());
    }

    let status = plan
        .clang
        .command()
        .args(&cflags)
        .stdin(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "invoking `{}` for module `{}`",
                plan.clang.path.display(),
                m.qualified_name
            )
        })?;
    if !status.success() {
        bail!(
            "clang exited with status {status} compiling module `{}`",
            m.qualified_name
        );
    }

    Ok((rewritten_path, object_path, cflags))
}

/// Surface-extraction pass: compile every module with
/// `-fsyntax-only` so the plugin can populate
/// `target/<profile>/.h-fragments/<crate>/<qname>.cust.h` before
/// the codegen pass needs to `#include` them. Tolerant of compile
/// failures — cross-module references in this pass are *expected*
/// to be unresolved on iter 1 (that's why we're emitting fragments
/// in the first place); the codegen pass will fail loudly if any
/// genuine errors remain.
///
/// `#cust use crate::X;` is lowered to an `#include` of `X`'s
/// fragment header **iff that fragment already exists on disk** —
/// otherwise blanked. On iter 1 of the fixed-point loop most
/// sibling fragments are missing, so clang sees unresolved
/// typedefs and falls back to implicit-int recovery (correct
/// fragments arrive on iter 2). V40D-11 wraps this in a fixed-
/// point loop; `surface_pass_fixed_point` is the entry point
/// callers should use.
fn surface_pass(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    layout: &TargetLayout,
    modules: &[Module],
) -> Result<()> {
    let crate_name = plan.manifest.package_name();
    for m in modules {
        let src_text = fs::read_to_string(&m.source_path)
            .with_context(|| format!("reading `{}`", m.source_path.display()))?;
        let scan = mod_scanner::scan(&src_text, &m.source_path)?;
        // Lower `#cust use` directives to `#include`s of the
        // matching fragment / dep header **when the target file
        // already exists on disk**. On iteration 1 of the fixed-
        // point loop, sibling fragments don't exist yet and the
        // directive is blanked (clang sees undeclared identifiers,
        // recovers, and the plugin emits a best-effort fragment).
        // Subsequent iterations pick up the now-written fragments
        // and re-resolve typedef/struct names properly. Without
        // this lowering the fixed-point loop is structurally
        // inert — surface_pass would never see imported types,
        // so a `[[cust::pub]] usize foo(void)` in module M would
        // be exported as `int foo(void)` (clang's implicit-int
        // recovery for undeclared identifiers in declarator
        // position), silently corrupting the published ABI.
        //
        // Cross-crate `#cust use <dep>;` is always included
        // because workspace topo-sort guarantees deps are built
        // (and therefore their headers exist) before this pass
        // runs. Unknown deps are not validated here — codegen
        // does that with a proper line:column diagnostic — and
        // are blanked so a missing path doesn't produce an
        // include-resolution error during the tolerant surface
        // compile.
        let rewritten =
            mod_scanner::rewrite_with(&src_text, &m.source_path, &scan, |d| match &d.kind {
                crate::mod_scanner::DirectiveKind::UseCrate { name } => {
                    let frag = layout.fragment_path(crate_name, name);
                    if frag.is_file() {
                        Some(format!("#include \"{}\"", frag.display()))
                    } else {
                        None
                    }
                }
                crate::mod_scanner::DirectiveKind::UseDep { name } => {
                    if plan.deps.iter().any(|n| n == name) {
                        let dep_header = layout
                            .dep_dir(name)
                            .join("include")
                            .join(format!("{name}.h"));
                        Some(format!("#include \"{}\"", dep_header.display()))
                    } else {
                        None
                    }
                }
                crate::mod_scanner::DirectiveKind::Mod { .. } => None,
            });

        let surface_path = crate_build_dir.join(format!("{}.surface.c", m.qualified_name));
        fs::write(&surface_path, &rewritten)
            .with_context(|| format!("writing `{}`", surface_path.display()))?;

        let fragment_path = layout.fragment_path(crate_name, &m.qualified_name);
        if let Some(parent) = fragment_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }

        // V40D-6 + RQ-V40-2: in test-build mode, also request
        // the per-module test-discovery sidecar. Always emit
        // even when the module has zero tests (writer skips
        // identical bytes, so empty stays empty). The driver
        // reads these in `run_test_build` to populate
        // __cust_tests[].
        let sidecar_path = if plan.test_build {
            let p = layout.test_sidecar_path(crate_name, &m.qualified_name);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating `{}`", parent.display()))?;
            }
            Some(p)
        } else {
            None
        };

        let original_dir = m.source_path.parent().unwrap_or(plan.crate_root);
        // Build flags but adjust: -fsyntax-only instead of `-c -o`.
        // Demote a couple of errors to warnings so unresolved
        // cross-module references in this pass don't stop the
        // plugin from running.
        let dummy_obj = crate_build_dir.join(format!("{}.surface.o", m.qualified_name));
        let includes: [&Path; 1] = [original_dir];
        let mut cflags = build_cflags(
            plan,
            profile,
            prelude,
            &surface_path,
            &dummy_obj,
            &includes,
            PluginOutputs {
                fragment: Some(&fragment_path),
                test_sidecar: sidecar_path.as_deref(),
                module: sidecar_path.as_ref().map(|_| m.qualified_name.as_str()),
            },
        );
        // Strip trailing `-c -o <obj> <src>` (4 args), replace
        // with `-fsyntax-only -Wno-error -Wno-implicit-function-declaration <src>`.
        let new_len = cflags.len().saturating_sub(4);
        cflags.truncate(new_len);
        cflags.push("-fsyntax-only".to_string());
        cflags.push("-Wno-error".to_string());
        cflags.push("-Wno-implicit-function-declaration".to_string());
        cflags.push(surface_path.display().to_string());

        // Run clang. We intentionally DO NOT check the exit
        // status: a non-zero exit here is the expected case for
        // any module that imports from a sibling whose fragment
        // doesn't exist yet. The plugin's HandleTranslationUnit
        // runs regardless of recoverable parse errors, so the
        // fragment gets written either way. We send stderr to
        // /dev/null to avoid drowning the user in expected
        // diagnostics; real errors will resurface in the codegen
        // pass against the same source.
        let _ = plan
            .clang
            .command()
            .args(&cflags)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!(
                    "invoking `{}` for surface pass on module `{}`",
                    plan.clang.path.display(),
                    m.qualified_name
                )
            })?;
    }
    Ok(())
}

/// V40D-11 fixed-point loop wrapping `surface_pass`. Iterates
/// the surface pass until the per-module fragment header bytes
/// stop changing, or until the cap (default 3, overridable via
/// `CUST_FIXED_POINT_CAP=<n>` env var) is exceeded.
///
/// Empirically: acyclic crates (every cust crate today) converge
/// in 1 iteration; a 2-cycle of `[[cust::pub_repr]]` types needs
/// 2; longer cycles either converge in 3 or diverge (the cap
/// catches the divergent case and surfaces the §4 verbatim
/// error). Plugin-side `writeFragmentIfChanged` already skips
/// identical bytes, so the per-iteration cost when nothing has
/// changed is one stat + one read + memcmp per module — cheap.
///
/// Implementation note: the design doc V40D-11 specifies scratch
/// `.iter-N/` subdirs with atomic rename on convergence. That's
/// equivalent to the in-memory snapshot used here: both detect
/// "no module's fragment bytes changed between iter N-1 and N."
/// The in-memory variant avoids any directory churn at all,
/// which matches what `writeFragmentIfChanged`'s skip already
/// gives us. If the eventual fixed-point invariant ever needs
/// stronger guarantees (e.g. cross-process visibility) we can
/// revisit; for v0.4.0 the simpler shape is correct.
fn surface_pass_fixed_point(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    layout: &TargetLayout,
    modules: &[Module],
) -> Result<()> {
    use std::collections::HashMap;

    let cap: usize = std::env::var("CUST_FIXED_POINT_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let crate_name = plan.manifest.package_name();

    // Snapshot of "fragment bytes after iteration N" keyed by
    // qualified module name. None on iter 0 (no prior bytes).
    let mut prev: Option<HashMap<String, Vec<u8>>> = None;

    for iter in 1..=cap {
        surface_pass(plan, profile, prelude, crate_build_dir, layout, modules)?;

        // Snapshot the fragments. Missing files become an empty
        // byte vec (a module with no [[cust::pub*]] decls still
        // gets a "header + banner" fragment via the plugin's
        // writer; the only path to a literally-missing file is
        // a clang crash before HandleTranslationUnit, which
        // would have errored above).
        let mut curr: HashMap<String, Vec<u8>> = HashMap::with_capacity(modules.len());
        for m in modules {
            let path = layout.fragment_path(crate_name, &m.qualified_name);
            let bytes = fs::read(&path).unwrap_or_default();
            curr.insert(m.qualified_name.clone(), bytes);
        }

        if let Some(prev_snap) = &prev {
            // Identify still-wobbling modules: fragment bytes
            // differ from previous iteration.
            let wobbling: Vec<&str> = modules
                .iter()
                .filter(|m| {
                    prev_snap.get(&m.qualified_name).map(Vec::as_slice)
                        != curr.get(&m.qualified_name).map(Vec::as_slice)
                })
                .map(|m| m.qualified_name.as_str())
                .collect();

            if wobbling.is_empty() {
                // Converged.
                return Ok(());
            }

            // Cap exceeded — emit the §4 verbatim error.
            if iter == cap {
                bail!(
                    "circular `[[cust::pub_repr]]` dependency did not converge\n  \
                     in {cap} iterations between modules: {}\n  \
                     hint: break the cycle by exporting one side as `[[cust::pub]]`\n        \
                     (opaque) instead of `[[cust::pub_repr]]`",
                    wobbling.join(", ")
                );
            }
        }
        prev = Some(curr);
    }

    // Single-iteration case (cap == 1): we ran once and never
    // had a "previous" to compare against, so we're done.
    Ok(())
}

/// Per-TU plugin output paths threaded through `build_cflags`.
/// All fields are optional; the plugin treats absent paths as
/// "skip that output." `module` is required whenever
/// `test_sidecar` is `Some` (the plugin errors otherwise) so
/// `qname = <module>::<name>` can be emitted; for non-test
/// builds we leave both `None`.
#[derive(Default, Clone, Copy)]
pub struct PluginOutputs<'a> {
    pub fragment: Option<&'a Path>,
    pub test_sidecar: Option<&'a Path>,
    pub module: Option<&'a str>,
}

/// Build the clang argv for a single TU. `extra_includes` is a
/// list of dirs that become `-I<dir>` flags before the prelude
/// `-include`. For lib compiles this is the original source dir
/// only; for bin compiles in a lib+bin crate it's the lib's
/// include dir followed by the bin source dir.
/// `plugin_out` carries the per-TU plugin-arg flags (fragment
/// header path, test-discovery sidecar path, module name).
pub fn build_cflags(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    source: &Path,
    object: &Path,
    extra_includes: &[&Path],
    plugin_out: PluginOutputs<'_>,
) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();

    // -std=
    let std_flag = plan
        .manifest
        .clang
        .std
        .as_deref()
        .unwrap_or_else(|| plan.clang.default_std());
    flags.push(format!("-std={std_flag}"));

    flags.extend(profile.cflags());
    flags.extend(plan.manifest.clang.extra_cflags.iter().cloned());

    flags.push("-fvisibility=hidden".to_string());
    if plan.test_build {
        // v0.3.2 V32D-3 / v0.4.0 V40D-14: -DCUST_TEST_BUILD=1
        // tells the plugin to attach normal external linkage to
        // `[[cust::test]]` decls (so the runner TU can extern
        // them) instead of the InternalLinkageAttr + UnusedAttr
        // it attaches in regular builds. Also gates the prelude's
        // `cust_assert` / `cust_panic` macros to their real
        // implementations (forward-declaring `cust_panic_impl`,
        // defined in the generated runner TU).
        flags.push("-DCUST_TEST_BUILD=1".to_string());
    }
    if let Some(plugin) = plan.plugin {
        flags.push(plugin.fplugin_flag());
        if let Some(path) = plugin_out.fragment {
            flags.push(format!("-fplugin-arg-cust-fragment-out={}", path.display()));
        }
        if let Some(path) = plugin_out.test_sidecar {
            flags.push(format!(
                "-fplugin-arg-cust-test-sidecar-out={}",
                path.display()
            ));
        }
        if let Some(module) = plugin_out.module {
            flags.push(format!("-fplugin-arg-cust-module={module}"));
        }
    } else {
        // V40D-10: without the plugin, clang doesn't know about
        // `[[cust::*]]` attributes — suppress
        // `-Wunknown-attributes` so cust-attribute decls don't
        // drown a `cust check --no-plugin` run in warnings.
        // (Compiles without the plugin still get the cust_*
        // prelude macros, which expand to `annotate(...)` —
        // those work without the plugin; only literal C23
        // `[[cust::*]]` attributes are unrecognised.)
        flags.push("-Wno-unknown-attributes".to_string());
    }
    for dir in extra_includes {
        flags.push(format!("-I{}", dir.display()));
    }
    flags.push("-include".to_string());
    flags.push(prelude.display().to_string());

    flags.push("-c".to_string());
    flags.push("-o".to_string());
    flags.push(object.display().to_string());
    flags.push(source.display().to_string());

    flags
}

fn argv_with_clang(plan: &BuildPlan<'_>, flags: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(flags.len() + 1);
    argv.push(plan.clang.path.display().to_string());
    argv.extend(flags.iter().cloned());
    argv
}

/// Return a copy of `argv` with any occurrence of `old` (as a full
/// argument string) replaced by `new`. Used to derive the
/// editor-facing `compile_commands` entry: same flags as the real
/// compile, but with the source path (last positional argument,
/// per `build_cflags`) swapped from the rewritten file to the
/// user's original source so clangd matches the file it opened.
fn swap_source_arg(argv: &[String], old: &Path, new: &Path) -> Vec<String> {
    let old_s = old.display().to_string();
    let new_s = new.display().to_string();
    argv.iter()
        .map(|a| {
            if a == &old_s {
                new_s.clone()
            } else {
                a.clone()
            }
        })
        .collect()
}

fn materialise_prelude(dst: &Path) -> Result<()> {
    const PRELUDE: &str = include_str!("prelude.h");
    // Write only if missing or stale (content differs) — keeps the
    // mtime stable so clang's own incremental story doesn't churn.
    let needs_write = fs::read_to_string(dst).map_or(true, |existing| existing != PRELUDE);
    if needs_write {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }
        fs::write(dst, PRELUDE).with_context(|| format!("writing `{}`", dst.display()))?;
    }
    Ok(())
}

fn archive_objects(objects: &[PathBuf], archive: &Path) -> Result<()> {
    let ar = pick_ar();
    // `rcs` = create archive, replace, add index. We pass all
    // objects in one invocation so the archive is built atomically.
    // Pre-remove any stale archive so `rcs` doesn't merge with
    // leftover entries from a previous build with more modules.
    let _ = fs::remove_file(archive);

    let mut cmd = Command::new(&ar);
    cmd.arg("rcs").arg(archive);
    for o in objects {
        cmd.arg(o);
    }
    let status = cmd
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("invoking `{}`", ar.to_string_lossy()))?;
    if !status.success() {
        bail!("{} exited with status {status}", ar.to_string_lossy());
    }
    Ok(())
}

/// Link a binary crate's executable from the bin half's objects,
/// the lib half's own archive (when `own_archive` is `Some`), and
/// every transitive dep archive listed in `plan.link_deps` (paths
/// resolved through the workspace dep-view symlink farm).
///
/// Static archives are wrapped in `-Wl,--start-group` /
/// `-Wl,--end-group` so the linker re-scans them as needed,
/// sparing the v0.3.1 driver from computing strict link-time
/// dependency order. `ThinLTO` across crates (v0.6) will revisit
/// this once bitcode rlibs replace `.a` files.
fn link_executable(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    objects: &[PathBuf],
    own_archive: Option<&Path>,
    layout: &TargetLayout,
    exe_path: &Path,
) -> Result<()> {
    let mut cmd = plan.clang.command();

    // Profile-driven flags (debug/opt) propagate to the link line
    // — they're harmless for `clang` driving the link step and
    // match what Cargo does (link with the same `-O`/`-g` flags).
    for f in profile.cflags() {
        cmd.arg(f);
    }
    // User extra ldflags. cflags are intentionally NOT forwarded
    // here — they're compile-only.
    for f in &plan.manifest.clang.extra_ldflags {
        cmd.arg(f);
    }

    cmd.arg("-o").arg(exe_path);

    for o in objects {
        cmd.arg(o);
    }

    // Group own archive + dep archives so the linker can re-scan.
    let dep_archives: Vec<PathBuf> = plan
        .link_deps
        .iter()
        .map(|dep| layout.dep_dir(dep).join(format!("lib{dep}.a")))
        .collect();

    let has_archives = own_archive.is_some() || !dep_archives.is_empty();
    if has_archives {
        cmd.arg("-Wl,--start-group");
        if let Some(a) = own_archive {
            cmd.arg(a);
        }
        for a in &dep_archives {
            cmd.arg(a);
        }
        cmd.arg("-Wl,--end-group");
    }

    let status = cmd.stdin(Stdio::null()).status().with_context(|| {
        format!(
            "invoking `{}` to link `{}`",
            plan.clang.path.display(),
            exe_path.display()
        )
    })?;
    if !status.success() {
        bail!(
            "clang link exited with status {status} for `{}`",
            exe_path.display()
        );
    }
    Ok(())
}

fn pick_ar() -> OsString {
    // Prefer llvm-ar if it's on PATH. We probe by trying `--version`
    // — cheap and avoids carrying a `which` dep.
    let llvm_ar_ok = Command::new("llvm-ar")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if llvm_ar_ok {
        OsString::from("llvm-ar")
    } else {
        OsString::from("ar")
    }
}

fn write_compile_commands(path: &Path, entries: &[CompileEntry]) -> Result<()> {
    // Minimal JSON serialiser tailored to compile_commands.json —
    // avoids pulling in serde_json just to emit an array of
    // {directory, file, arguments} objects. Escapes per RFC 8259 §7.
    let mut out = String::from("[\n");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  {\n");
        push_json_kv(&mut out, "directory", &e.directory.display().to_string());
        out.push_str(",\n");
        push_json_kv(&mut out, "file", &e.file.display().to_string());
        out.push_str(",\n    \"arguments\": [");
        for (j, a) in e.arguments.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            out.push('"');
            out.push_str(&escape_json(a));
            out.push('"');
        }
        out.push_str("]\n  }");
    }
    out.push_str("\n]\n");

    fs::write(path, out).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

fn push_json_kv(buf: &mut String, key: &str, value: &str) {
    buf.push_str("    \"");
    buf.push_str(key);
    buf.push_str("\": \"");
    buf.push_str(&escape_json(value));
    buf.push('"');
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Write `target/.cust-version` containing the cust + clang
/// version strings. Used by `cust clean` (and external tooling)
/// to detect when the cached build was produced by a different
/// toolchain. Idempotent — overwrites unconditionally.
pub fn write_version_stamp(path: &Path, clang: &Clang) -> Result<()> {
    let contents = format!(
        "cust {}\n{}\n",
        env!("CARGO_PKG_VERSION"),
        clang.version_line
    );
    fs::write(path, contents).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

/// Concatenate per-module fragment headers into the single user-
/// facing crate header at `target/<profile>/include/<crate>.h`
/// (cust-design.md §5).
///
/// v0.2 is **naive**: every fragment is included in declaration
/// order, no de-duplication, no `pub` vs `pub(crate)` filtering
/// (the plugin doesn't yet distinguish them in fragment output).
/// The header is wrapped in a standard `#ifndef`/`extern "C"`
/// guard pair so it's safe to `#include` from C and C++.
///
/// Missing fragments are skipped silently — a module with zero
/// `[[cust::pub]]` decls produces no fragment, which is fine.
///
/// **Module order.** Modules are emitted in topological order
/// over their intra-crate `#cust use crate::<mod>;` edges so any
/// type or decl a module's fragment references is declared
/// earlier in the concatenated header. Discovery order (DFS
/// preorder, root first) breaks the moment a sibling module
/// exports a typedef used by the root or by an earlier sibling
/// — that's exactly the pattern cstd needs (types module
/// exports `i32`/`u64`; math and lib use them). Cycles are
/// impossible at this point: the discovery pass (`modules::
/// discover`) already rejects intra-crate `#cust mod` cycles,
/// and intra-crate fragment includes are forward-decl-only so
/// they can't reintroduce one.
fn write_crate_header(layout: &TargetLayout, crate_name: &str, modules: &[Module]) -> Result<()> {
    use std::fmt::Write as _;

    let path = layout.crate_header_path(crate_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }

    let ordered = topo_order_modules(modules);

    let guard = header_guard(crate_name);
    let mut out = String::new();
    out.push_str("/* @generated by cust — DO NOT EDIT */\n");
    let _ = writeln!(out, "/* Public surface of crate `{crate_name}`. */\n");
    let _ = writeln!(out, "#ifndef {guard}\n#define {guard}\n");
    // No `#include` injection: the generated header is pure
    // declarations. Crates whose public surface mentions
    // fixed-width / size / bool types must export their own
    // `[[cust::pub]] typedef`s (mirrors Rust's `pub use` story —
    // every type a consumer reaches for must be reachable via
    // the producer's surface). See cust-design.md §5.
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");

    for m in &ordered {
        let frag = layout.fragment_path(crate_name, &m.qualified_name);
        let Ok(body) = fs::read_to_string(frag) else {
            continue; // module had no [[cust::pub]] decls; plugin emitted nothing
        };
        let _ = writeln!(out, "/* --- module `{}` --- */", m.qualified_name);
        out.push_str(strip_fragment_header_comment(&body));
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("#ifdef __cplusplus\n} /* extern \"C\" */\n#endif\n\n");
    let _ = writeln!(out, "#endif /* {guard} */");

    // v0.4.2: incremental hygiene. CMake/Ninja's `OBJECT_DEPENDS`
    // edges (V42D-6) trigger TU recompiles when the depended-on
    // file's mtime advances; rewriting `cstd.h` unconditionally
    // would re-codegen every consumer's main.c every build even
    // when the public surface didn't change. Compare bytes before
    // touching.
    write_if_byte_different(&path, out.as_bytes())?;
    Ok(())
}

/// Order `modules` so any module appears after every module it
/// `#cust use crate::<…>;`-imports. Stable: ties (modules with
/// the same in-degree) preserve discovery order, so the existing
/// DFS-preorder behaviour is preserved for any crate that
/// doesn't have intra-crate type dependencies.
///
/// Kahn's algorithm. `imports` lists *predecessors* (this module
/// uses them); we count in-degrees as "how many modules I depend
/// on", then repeatedly emit zero-in-degree modules in discovery
/// order. Modules whose imports name non-existent siblings (which
/// shouldn't happen — `modules::discover` validates this — but
/// we guard against it for defence in depth) are treated as if
/// the missing edge weren't there.
fn topo_order_modules(modules: &[Module]) -> Vec<&Module> {
    use std::collections::{BTreeSet, VecDeque};

    // Name → discovery index.
    let name_to_idx: std::collections::BTreeMap<&str, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.qualified_name.as_str(), i))
        .collect();

    // In-degree per module (count of imports that resolve to a
    // sibling in this same crate). Outbound edges from i: for
    // each name in modules[i].imports, edge name → i.
    let mut in_deg: Vec<usize> = vec![0; modules.len()];
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); modules.len()];
    for (i, m) in modules.iter().enumerate() {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for imp in &m.imports {
            if let Some(&j) = name_to_idx.get(imp.as_str()) {
                if seen.insert(j) {
                    successors[j].push(i);
                    in_deg[i] += 1;
                }
            }
        }
    }

    // Initial queue: every module with zero in-degree, in
    // discovery order — keeps ties stable.
    let mut queue: VecDeque<usize> = (0..modules.len()).filter(|&i| in_deg[i] == 0).collect();
    let mut out: Vec<&Module> = Vec::with_capacity(modules.len());

    while let Some(i) = queue.pop_front() {
        out.push(&modules[i]);
        for &succ in &successors[i] {
            in_deg[succ] -= 1;
            if in_deg[succ] == 0 {
                queue.push_back(succ);
            }
        }
    }

    // Cycle defence: if we couldn't drain the whole graph fall
    // back to discovery order for the leftover. modules::discover
    // already rejects #cust mod cycles, and intra-crate fragment
    // includes are forward-decl-only, so this branch shouldn't
    // be reachable in practice.
    if out.len() != modules.len() {
        for (i, m) in modules.iter().enumerate() {
            if !out.iter().any(|seen| std::ptr::eq(*seen, m)) {
                let _ = i;
                out.push(m);
            }
        }
    }

    out
}

/// Derive an include-guard macro name from a crate name. Mirrors
/// cargo's `name = "my-crate"` → C-identifier sanitisation: `-`
/// and any other non-alphanumeric becomes `_`, upper-case the
/// whole thing, and suffix `_H`.
fn header_guard(crate_name: &str) -> String {
    let mut s = String::with_capacity(crate_name.len() + 2);
    for c in crate_name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    s.push_str("_H");
    s
}

/// Strip the per-fragment `@generated by cust plugin` banner so
/// the concatenated crate header has just one top-level banner.
/// The fragment plugin's banner is exactly two lines starting
/// with `/* @generated` and `/* Forward declarations of`, plus a
/// blank — see `plugin/src/plugin.cc::buildFragmentContents`.
fn strip_fragment_header_comment(body: &str) -> &str {
    body.strip_prefix("/* @generated by cust plugin — DO NOT EDIT */\n")
        .and_then(|s| s.strip_prefix("/* Forward declarations of [[cust::pub]] items. */\n"))
        .map_or(body, |s| s.trim_start_matches('\n'))
}

#[cfg(test)]
mod tests {
    use super::escape_json;

    #[test]
    fn escapes_quotes_backslashes_controls() {
        assert_eq!(escape_json("hi"), "hi");
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("a\\b"), "a\\\\b");
        assert_eq!(escape_json("a\nb"), "a\\nb");
        assert_eq!(escape_json("\u{0007}"), "\\u0007");
    }

    #[test]
    fn header_guard_basic() {
        use super::header_guard;
        assert_eq!(header_guard("hello"), "HELLO_H");
    }

    #[test]
    fn header_guard_sanitises_dashes() {
        use super::header_guard;
        assert_eq!(header_guard("my-crate"), "MY_CRATE_H");
    }

    #[test]
    fn header_guard_sanitises_other_punctuation() {
        use super::header_guard;
        assert_eq!(header_guard("a.b.c"), "A_B_C_H");
        assert_eq!(header_guard("foo123"), "FOO123_H");
    }

    #[test]
    fn strip_fragment_header_comment_strips_known_banner() {
        use super::strip_fragment_header_comment;
        let input = "/* @generated by cust plugin — DO NOT EDIT */\n\
                     /* Forward declarations of [[cust::pub]] items. */\n\
                     \n\
                     int foo(void);\n";
        assert_eq!(strip_fragment_header_comment(input), "int foo(void);\n");
    }

    #[test]
    fn strip_fragment_header_comment_passes_unknown_through() {
        use super::strip_fragment_header_comment;
        let input = "int foo(void);\n";
        assert_eq!(strip_fragment_header_comment(input), input);
    }

    fn mk_mod(name: &str, imports: &[&str]) -> super::Module {
        super::Module {
            qualified_name: name.to_string(),
            source_path: std::path::PathBuf::from(format!("/x/{name}.c")),
            imports: imports.iter().map(|s| (*s).to_string()).collect(),
            dep_imports: Vec::new(),
        }
    }

    #[test]
    fn topo_order_modules_preserves_discovery_order_with_no_edges() {
        // No intra-crate imports → discovery order preserved.
        use super::topo_order_modules;
        let mods = vec![mk_mod("lib", &[]), mk_mod("a", &[]), mk_mod("b", &[])];
        let out: Vec<&str> = topo_order_modules(&mods)
            .iter()
            .map(|m| m.qualified_name.as_str())
            .collect();
        assert_eq!(out, ["lib", "a", "b"]);
    }

    #[test]
    fn topo_order_modules_pulls_imported_module_to_front() {
        // lib uses types; types must appear before lib in the
        // concatenated header.
        use super::topo_order_modules;
        let mods = vec![
            mk_mod("lib", &["types"]),
            mk_mod("types", &[]),
            mk_mod("math", &["types"]),
        ];
        let out: Vec<&str> = topo_order_modules(&mods)
            .iter()
            .map(|m| m.qualified_name.as_str())
            .collect();
        assert_eq!(out, ["types", "lib", "math"]);
    }

    #[test]
    fn topo_order_modules_keeps_ties_in_discovery_order() {
        // Two roots-of-the-DAG: order between them follows
        // discovery order.
        use super::topo_order_modules;
        let mods = vec![mk_mod("a", &[]), mk_mod("b", &[]), mk_mod("c", &["a", "b"])];
        let out: Vec<&str> = topo_order_modules(&mods)
            .iter()
            .map(|m| m.qualified_name.as_str())
            .collect();
        assert_eq!(out, ["a", "b", "c"]);
    }

    #[test]
    fn topo_order_modules_ignores_unresolved_imports() {
        // An import naming a non-sibling (shouldn't happen — discovery
        // rejects this — but the orderer must be robust).
        use super::topo_order_modules;
        let mods = vec![mk_mod("lib", &["ghost"]), mk_mod("real", &[])];
        let out: Vec<&str> = topo_order_modules(&mods)
            .iter()
            .map(|m| m.qualified_name.as_str())
            .collect();
        assert_eq!(out, ["lib", "real"]);
    }
}
