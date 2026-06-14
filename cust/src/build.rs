//! Residual driver-side helpers for the `CMake` build/check/test
//! paths.
//!
//! Since the v0.4.2 `CMake` migration (and, as of the
//! incremental-check milestone, the removal of the last
//! driver-side surface/check pre-pass) this module no longer owns
//! a compile pipeline — every generation/validation step is a
//! `cmake` custom command. What's left here is the supporting
//! machinery the orchestrator and the `cust internal` leaves still
//! call:
//!
//! * carriers handed between the orchestrator and the CLI
//!   ([`BuildPlan`], [`BuildOutputs`], [`IntegrationTestOutput`]);
//! * prelude materialisation ([`ensure_prelude`]) +
//!   byte-stable writes ([`write_if_byte_different`]);
//! * the `BuildOutputs` synthesis for the `CMake` path
//!   ([`cmake_outputs_for`]);
//! * the standalone clang-argv builder the `surface-module` /
//!   `test-sidecar` leaves reproduce ([`build_cflags_raw`]);
//! * the `.cust-version` stamp ([`write_version_stamp`]).
//!
//! Module-graph ordering (`topo_order_modules` / `module_sccs`)
//! now lives in [`crate::modules`]; crate-header string helpers
//! (`header_guard` / `strip_fragment_header_comment`) live in
//! [`crate::generate`] alongside their only caller.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{
    clang::Clang,
    manifest::{CrateKind, Manifest},
    plugin::Plugin,
    target_layout::TargetLayout,
};

/// Inputs handed to the residual CMake-path prebuild by the
/// workspace orchestrator. Since the incremental-check milestone
/// (slice E) every generation/validation step is a `CMake` custom
/// command, so the driver no longer runs any per-member surface or
/// check pass — this carrier shrank to just what
/// [`cmake_outputs_for`] and the test-build path's `BuildOutputs`
/// synthesis still read.
pub struct BuildPlan<'a> {
    pub manifest: &'a Manifest,
    /// What this crate produces (lib / bin / lib+bin). Computed
    /// by `Manifest::resolve_kind` at the workspace orchestrator
    /// (or CLI) layer.
    pub kind: CrateKind,
    /// v0.4.3 V43D-5: integration tests discovered under
    /// `<crate>/tests/*.c` (one per file). The test-build path
    /// reads these to synthesise each `tests/<stem>.c` exe's
    /// `IntegrationTestOutput`. Empty for members without a
    /// `tests/` dir.
    pub integration_tests: &'a [crate::workspace::IntegrationTest],
}

/// Outputs the orchestrator reports back to the CLI for the
/// `Finished` / `Running` lines. `archive` + `executables` are the
/// `cust build` artifacts; `test_executable` + `integration_tests`
/// are the `cust test` exes the runner spawns. `CMake`/`Ninja`
/// produce the files at the V42D-13 paths; the driver only
/// synthesises the predictable paths (it tracks no per-TU objects).
#[derive(Debug)]
pub struct BuildOutputs {
    /// `Some` when the crate has a lib component (`Lib` or
    /// `LibAndBin`); `None` for bin-only crates.
    pub archive: Option<PathBuf>,
    /// One `(bin-name, path)` per binary target the crate
    /// produces (v0.4.4 V44D-8). Empty for lib-only crates and
    /// in `cust check` mode. For a single-bin crate this has
    /// exactly one entry. The CLI prints a `Finished` line per
    /// entry and `cust run --bin` selects by name.
    pub executables: Vec<(String, PathBuf)>,
    /// `Some` in test-build mode when a test binary was produced
    /// (V32D-4 / V32D-5). The path is
    /// `target/<profile>/test/<crate>/<crate>`.
    pub test_executable: Option<PathBuf>,
    /// v0.4.3 V43D-5: integration-test executables produced in
    /// test-build mode, one per `tests/<stem>.c`. Empty in build
    /// / check mode and for members without a `tests/` dir.
    pub integration_tests: Vec<IntegrationTestOutput>,
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
// Slice B (V42D-16) moves phase-2 codegen + link into CMake. As of
// the incremental-check milestone (slice E) the driver no longer
// runs *any* surface/check pre-pass: every generation + validation
// step is a CMake custom command (rewrites + fragments + crate
// headers + test sidecars/runners + the per-module check). The
// driver's residual prebuild work is materialising the prelude
// (`ensure_prelude`), preparing per-member dirs + dep symlinks, and
// synthesising `BuildOutputs` (`cmake_outputs_for`).

/// Write `bytes` to `path` only if the contents differ from
/// what's already on disk (or the file doesn't exist yet). Saves
/// `CMake`/Ninja from spuriously rebuilding TUs whose post-rewrite
/// bytes are unchanged across `cust build` invocations.
pub fn write_if_byte_different(path: &Path, bytes: &[u8]) -> Result<()> {
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
        archive,
        executables,
        test_executable: None,
        integration_tests: Vec::new(),
    }
}

/// Per-TU plugin output paths threaded through `build_cflags_raw`.
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

/// Build the clang argv for a single TU from explicit values (no
/// `BuildPlan` / `ResolvedProfile`) so the hidden `cust internal
/// surface-module` / `test-sidecar` leaves (V45D-2) can reproduce
/// the exact same clang argv from their command-line arguments.
/// `extra_includes` become `-I<dir>` flags before the prelude
/// `-include`; `mid_cflags` is the profile cflags followed by
/// `[clang] extra-cflags`; `plugin_out` carries the per-TU
/// plugin-arg flags (fragment header, test-discovery sidecar,
/// module name). The incremental-check emitter mirrors this shape
/// for the per-module check argv (CHK-D-2), reusing the lib
/// target's `compile_options` rather than this builder.
#[allow(clippy::too_many_arguments)]
pub fn build_cflags_raw(
    std_flag: &str,
    mid_cflags: &[String],
    test_build: bool,
    plugin: Option<&Plugin>,
    prelude: &Path,
    source: &Path,
    object: &Path,
    extra_includes: &[&Path],
    plugin_out: PluginOutputs<'_>,
) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();

    // -std=
    flags.push(format!("-std={std_flag}"));

    flags.extend(mid_cflags.iter().cloned());

    flags.push("-fvisibility=hidden".to_string());
    if test_build {
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
    if let Some(plugin) = plugin {
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
        // drown a plugin-less `cust check` run in warnings.
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

/// v0.4.5 V45D-14(b): materialise the prelude header into the
/// profile root so the `surface-module` custom commands can
/// `-include` it. Dropping `run_phase1` from the build/run path
/// (V45D-10) also drops the prelude write it used to perform, so
/// the driver does it once, here, right before driving `CMake`.
/// Idempotent + content-skipped (mtime-stable when unchanged).
pub fn ensure_prelude(layout: &TargetLayout) -> Result<()> {
    materialise_prelude(&layout.prelude_path())
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
