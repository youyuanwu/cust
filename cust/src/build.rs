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
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{bail, Context, Result};

use crate::{
    clang::Clang,
    manifest::{CrateKind, Manifest},
    mod_scanner,
    modules::{self, Module},
    plugin::Plugin,
    profile::{ProfileKind, ResolvedProfile},
    target_layout::{TargetLayout, TestOrigin},
    test_discovery::{self, TestEntry},
    test_runner,
};

/// Inputs handed to driver entry points by the CLI layer.
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
    /// Names of dep crates this consumer is allowed to import via
    /// `#cust use <name>;` (V3D-6). Validated against the
    /// scanner's `UseDep` directives. Empty for a crate that
    /// declares no `[dependencies]`. The workspace orchestrator
    /// populates this from the resolved edge list.
    pub deps: &'a [&'a str],
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
    /// v0.4.3 V43D-5: integration tests discovered under
    /// `<crate>/tests/*.c` (one per file). Used in test-build
    /// mode to rewrite + surface-pass + generate a runner TU per
    /// file; also rewritten in build mode so the `CMakeLists`
    /// `add_executable` source paths exist at configure time.
    /// Empty for members without a `tests/` dir.
    pub integration_tests: &'a [crate::workspace::IntegrationTest],
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
    /// One `(bin-name, path)` per binary target the crate
    /// produces (v0.4.4 V44D-8). Empty for lib-only crates and
    /// for `syntax_only` builds. For a single-bin crate this has
    /// exactly one entry. The CLI prints a `Finished` line per
    /// entry and `cust run --bin` selects by name.
    pub executables: Vec<(String, PathBuf)>,
    /// `Some` when `plan.test_build` was true and a test binary
    /// was produced (V32D-4 / V32D-5). The path is
    /// `target/<profile>/test/<crate>/<crate>`.
    pub test_executable: Option<PathBuf>,
    /// v0.4.3 V43D-5: integration-test executables produced in
    /// test-build mode, one per `tests/<stem>.c`. Empty in build
    /// / check mode and for members without a `tests/` dir.
    pub integration_tests: Vec<IntegrationTestOutput>,
    #[allow(dead_code)]
    pub compile_commands: PathBuf,
}

/// v0.4.3 V43D-5: one built integration-test executable, ready
/// for `cust test` to spawn (V43D-10/V43D-11).
#[derive(Debug)]
pub struct IntegrationTestOutput {
    /// File stem (`tests/<stem>.c` → `<stem>`).
    pub stem: String,
    /// Crate-relative source label for the run banner
    /// (`tests/<stem>.c`).
    pub source_label: String,
    /// Absolute path to the built exe
    /// (`target/<profile>/test/<crate>/<stem>/<stem>`).
    pub exe: PathBuf,
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
    // v0.4.4 V44D-8: each `BinTarget` is its own root; discover +
    // rewrite one module graph per bin (a bin may `#cust use
    // crate::<mod>`). Two bins sharing a module rewrite it to the
    // same path idempotently.
    for bin in plan.kind.bins() {
        let bin_modules = modules::discover(plan.crate_root, &bin.source)
            .with_context(|| format!("discovering bin `{}` module graph", bin.name))?;
        for m in &bin_modules {
            write_one_rewrite(plan, &layout, &rewrite_root, &m.source_path, m, true)?;
        }
    }
    // v0.4.3 V43D-5: rewrite each integration test source into
    // `.rewrite/<crate>/tests/<stem>.c` so the `CMakeLists`
    // `add_executable(<crate>__itest__<stem> ...)` source path
    // exists at configure time (both build and test mode). Only
    // lib members get integration targets (V43D-3); skip the
    // rewrites for bin-only members to match `collect_view`.
    if plan.kind.has_lib() {
        for it in plan.integration_tests {
            write_integration_rewrite(plan, &layout, &rewrite_root, it)?;
        }
    }
    Ok(())
}

/// v0.4.3 V43D-3/V43D-5: rewrite one `tests/<stem>.c` integration
/// source into `.rewrite/<crate>/tests/<stem>.c`. Lowers
/// `#cust use <crate>;` (the CUT itself) to an `#include` of the
/// published `<crate>.h` — the same own-crate carve-out the bin
/// half of a lib+bin crate uses — and `#cust use <dep>;` to the
/// dep's published header. `#cust use crate::<mod>;` is **not**
/// supported (V43D-3: integration tests see the public surface
/// only, not crate-private modules) and lowers to nothing, so a
/// crate-private reference fails to compile with a clear missing-
/// declaration error.
fn write_integration_rewrite(
    plan: &BuildPlan<'_>,
    layout: &TargetLayout,
    rewrite_root: &Path,
    it: &crate::workspace::IntegrationTest,
) -> Result<()> {
    let crate_name = plan.manifest.package_name();
    let own_header = layout.crate_header_path(crate_name);
    let src_text = fs::read_to_string(&it.source)
        .with_context(|| format!("reading `{}`", it.source.display()))?;
    let scan = mod_scanner::scan(&src_text, &it.source)?;

    let rewritten = mod_scanner::rewrite_with(&src_text, &it.source, &scan, |d| match &d.kind {
        crate::mod_scanner::DirectiveKind::UseDep { name } => {
            if name == crate_name {
                Some(format!("#include \"{}\"", own_header.display()))
            } else {
                let dep_header = layout
                    .dep_dir(name)
                    .join("include")
                    .join(format!("{name}.h"));
                Some(format!("#include \"{}\"", dep_header.display()))
            }
        }
        // V43D-3: no crate-private module / fragment access from
        // integration tests; blank `#cust use crate::<mod>;`.
        crate::mod_scanner::DirectiveKind::UseCrate { .. }
        | crate::mod_scanner::DirectiveKind::Mod { .. } => None,
    });

    // Validate `#cust use <dep>;` resolves to the CUT itself or a
    // declared dep — same shape `write_one_rewrite` enforces.
    for d in &scan.directives {
        if let crate::mod_scanner::DirectiveKind::UseDep { name } = &d.kind {
            if name == crate_name || plan.deps.iter().any(|n| n == name) {
                continue;
            }
            bail!(
                "{}:{}:{}: `#cust use {name};` refers to a crate that is \
                 neither `{crate_name}` nor a declared dependency",
                it.source.display(),
                d.span.line,
                d.span.column
            );
        }
    }

    let dst = rewrite_root
        .join(crate_name)
        .join("tests")
        .join(format!("{}.c", it.stem));
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }
    write_if_byte_different(&dst, rewritten.as_bytes())?;
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
    // v0.4.4 V44D-8: one exe per bin at `target/<profile>/<name>`.
    let executables = plan
        .kind
        .bins()
        .iter()
        .map(|b| (b.name.clone(), layout.profile_root.join(&b.name)))
        .collect();
    BuildOutputs {
        objects: Vec::new(),
        archive,
        executables,
        test_executable: None,
        integration_tests: Vec::new(),
        compile_commands: layout.target_root.join("compile_commands.json"),
    }
}

/// V42D-14 test-runner TU generator. Reads the per-module test-
/// discovery sidecars `surface_pass` wrote (when `plan.test_build`
/// is `true`) and renders one `cust_test_main_<crate>.c` into
/// `target/<profile>/cmake/`, where the workspace `CMakeLists`
/// expects to find it.
///
/// Returns the absolute path of the per-member test executable
/// `CMake` will produce (so the caller can plumb it into
/// `BuildOutputs::test_executable` for the test runner to spawn).
/// Returns `None` for bin-only members (V32D-11: those don't get
/// tested in v0.4.x — `cust test` skips them).
pub fn write_test_runner_tu(plan: &BuildPlan<'_>) -> Result<Option<PathBuf>> {
    if !plan.kind.has_lib() {
        return Ok(None);
    }
    let layout = TargetLayout::for_workspace(plan.workspace_root, plan.profile_kind);
    let crate_name = plan.manifest.package_name();
    let Some(lib_src) = plan.kind.lib_source() else {
        return Ok(None);
    };
    let lib_modules =
        modules::discover(plan.crate_root, lib_src).context("discovering lib module graph")?;

    // V40D-6: read every module's test-discovery sidecar.
    // surface_pass wrote these (in test_build mode) before us;
    // a missing sidecar means surface_pass didn't visit that
    // module — bug, hard error.
    let mut tests: Vec<TestEntry> = Vec::new();
    for m in &lib_modules {
        let sidecar_path = layout.test_sidecar_path(
            crate_name,
            TestOrigin::Unit {
                qualified_name: &m.qualified_name,
            },
        );
        let contents = fs::read_to_string(&sidecar_path).with_context(|| {
            format!(
                "reading test-discovery sidecar `{}`",
                sidecar_path.display()
            )
        })?;
        let mut found = test_discovery::parse(&contents, &sidecar_path)?;
        tests.append(&mut found);
    }

    // Render + write the runner TU at the V42D-14 path.
    let cmake_dir = layout.profile_root.join("cmake");
    fs::create_dir_all(&cmake_dir)
        .with_context(|| format!("creating `{}`", cmake_dir.display()))?;
    let runner_path = cmake_dir.join(format!("cust_test_main_{crate_name}.c"));
    let runner_src = test_runner::render_main_c(&tests);
    // Content-skip: keeps CMake's restat happy on no-op rebuilds.
    write_if_byte_different(&runner_path, runner_src.as_bytes())?;

    Ok(Some(layout.test_executable_path(crate_name)))
}

/// v0.4.3 V43D-4/V43D-5: for each integration test under
/// `<crate>/tests/`, surface-pass the already-rewritten
/// `.rewrite/<crate>/tests/<stem>.c` to emit its test-discovery
/// sidecar, then render + write the per-exe runner TU at
/// `cmake/cust_itest_main_<crate>__<stem>.c`. Returns one
/// `IntegrationTestOutput` per file with the exe path `cust test`
/// will spawn.
///
/// Test-build only (the caller gates on `plan.test_build`). No-op
/// for non-lib members (V43D-3) or members without a `tests/`
/// dir. Assumes `run_phase1` already wrote `<crate>.h` and
/// `write_rewrite_tree` already produced the rewritten sources.
pub fn write_integration_runner_tus(plan: &BuildPlan<'_>) -> Result<Vec<IntegrationTestOutput>> {
    if !plan.kind.has_lib() || plan.integration_tests.is_empty() {
        return Ok(Vec::new());
    }
    let profile_override = match plan.profile_kind {
        ProfileKind::Dev => plan.manifest.profile.dev.as_ref(),
        ProfileKind::Release => plan.manifest.profile.release.as_ref(),
    };
    let profile = ResolvedProfile::resolve(plan.profile_kind, profile_override)?;
    let layout = TargetLayout::for_workspace(plan.workspace_root, plan.profile_kind);
    let prelude_path = layout.prelude_path();
    let crate_name = plan.manifest.package_name();
    let rewrite_root = layout.profile_root.join(".rewrite");
    let cmake_dir = layout.profile_root.join("cmake");
    fs::create_dir_all(&cmake_dir)
        .with_context(|| format!("creating `{}`", cmake_dir.display()))?;

    let mut out = Vec::with_capacity(plan.integration_tests.len());
    for it in plan.integration_tests {
        let rewritten = rewrite_root
            .join(crate_name)
            .join("tests")
            .join(format!("{}.c", it.stem));

        // Surface-pass the rewritten test TU to emit its sidecar.
        // Plugin-only (no plugin ⇒ no [[cust::test]] discovery);
        // the runner just renders zero tests in that case.
        let sidecar_path =
            layout.test_sidecar_path(crate_name, TestOrigin::Integration { stem: &it.stem });
        if plan.plugin.is_some() {
            if let Some(parent) = sidecar_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating `{}`", parent.display()))?;
            }
            surface_pass_integration(plan, &profile, &prelude_path, &rewritten, &sidecar_path)?;
        }

        // Read the sidecar (empty when no plugin / zero tests).
        let tests: Vec<TestEntry> = match fs::read_to_string(&sidecar_path) {
            Ok(contents) => test_discovery::parse(&contents, &sidecar_path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(anyhow::Error::new(e)).with_context(|| {
                    format!("reading integration sidecar `{}`", sidecar_path.display())
                })
            }
        };

        let runner_path = cmake_dir.join(format!("cust_itest_main_{crate_name}__{}.c", it.stem));
        let runner_src = test_runner::render_main_c(&tests);
        write_if_byte_different(&runner_path, runner_src.as_bytes())?;

        out.push(IntegrationTestOutput {
            stem: it.stem.clone(),
            source_label: format!("tests/{}.c", it.stem),
            exe: layout.integration_test_executable_path(crate_name, &it.stem),
        });
    }
    Ok(out)
}

/// Surface-compile one rewritten integration test TU with
/// `-fsyntax-only` + the plugin so the per-file test-discovery
/// sidecar gets written. Unlike the lib surface pass this emits
/// no fragment header (integration tests don't publish surface)
/// and needs no fixed-point loop (a single file, no intra-crate
/// fragment dependencies). The plugin `module` arg is `lib` so
/// the runner renders bare test names (qname drops the root
/// `lib` module, matching unit tests at crate root).
fn surface_pass_integration(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    rewritten: &Path,
    sidecar_path: &Path,
) -> Result<()> {
    let dummy_obj = rewritten.with_extension("surface.o");
    let mut cflags = build_cflags(
        plan,
        profile,
        prelude,
        rewritten,
        &dummy_obj,
        &[],
        PluginOutputs {
            fragment: None,
            test_sidecar: Some(sidecar_path),
            module: Some("lib"),
        },
    );
    // Strip trailing `-c -o <obj> <src>` (4 args), replace with
    // `-fsyntax-only -Wno-error ... <src>` — same demotions the
    // lib surface pass uses so an unresolved decl doesn't stop
    // the plugin from emitting the sidecar.
    let new_len = cflags.len().saturating_sub(4);
    cflags.truncate(new_len);
    cflags.push("-fsyntax-only".to_string());
    cflags.push("-Wno-error".to_string());
    cflags.push("-Wno-implicit-function-declaration".to_string());
    cflags.push(rewritten.display().to_string());

    let _ = plan
        .clang
        .command()
        .args(&cflags)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "invoking `{}` for integration surface pass on `{}`",
                plan.clang.path.display(),
                rewritten.display()
            )
        })?;
    Ok(())
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
            let p = layout.test_sidecar_path(
                crate_name,
                TestOrigin::Unit {
                    qualified_name: &m.qualified_name,
                },
            );
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
