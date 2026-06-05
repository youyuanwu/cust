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
    /// is unused. Workspace orchestrator computes this in topo
    /// order; for non-workspace single-crate builds it's empty.
    pub link_deps: &'a [&'a str],
    /// What this crate produces (lib / bin / lib+bin). Computed
    /// by `Manifest::resolve_kind` at the workspace orchestrator
    /// (or CLI) layer.
    pub kind: CrateKind,
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
    #[allow(dead_code)]
    pub compile_commands: PathBuf,
}

/// One `compile_commands.json` entry.
struct CompileEntry {
    directory: PathBuf,
    file: PathBuf,
    arguments: Vec<String>,
}

#[allow(clippy::too_many_lines)] // staged lib+bin pipeline; splitting further hurts readability more than it helps
pub fn run(plan: &BuildPlan<'_>) -> Result<BuildOutputs> {
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
        // headers, only relevant for the lib half (the bin half
        // doesn't publish fragments).
        let needs_surface_pass =
            plan.plugin.is_some() && lib_modules.iter().any(|m| !m.imports.is_empty());
        if needs_surface_pass {
            surface_pass(
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
            /* emit_fragments = */ true,
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
            /* emit_fragments = */ false,
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
        compile_commands: cc_path,
    })
}

/// Compile a tree of modules into `.o` files, threading
/// `compile_commands.json` entries through `compile_entries`.
/// `emit_fragments = true` makes the plugin write per-module
/// fragment headers (lib half); `false` skips that (bin half).
/// `extra_includes` is prepended to clang's `-I` set so per-half
/// concerns (e.g. bin → lib include dir) propagate.
/// `is_bin_half` controls intra-crate self-import semantics:
/// when `true`, `#cust use <own-package-name>;` is accepted as
/// Cargo-style "bin reaches its own lib" and lowered to the
/// crate's own lib include (Slice C, V31D-1 follow-up).
#[allow(clippy::too_many_arguments)] // tightly coupled to compile_one_module's surface
fn compile_tree(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    layout: &TargetLayout,
    modules: &[Module],
    emit_fragments: bool,
    extra_includes: &[&Path],
    is_bin_half: bool,
    compile_entries: &mut Vec<CompileEntry>,
) -> Result<Vec<PathBuf>> {
    let crate_name = plan.manifest.package_name();
    let mut objects: Vec<PathBuf> = Vec::with_capacity(modules.len());

    for m in modules {
        let fragment_path = if emit_fragments && plan.plugin.is_some() {
            Some(layout.fragment_path(crate_name, &m.qualified_name))
        } else {
            None
        };
        let (rewritten_path, object_path, cflags) = compile_one_module(
            plan,
            profile,
            prelude,
            crate_build_dir,
            layout,
            fragment_path.as_deref(),
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
    fragment_out: Option<&Path>,
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

    // Make sure the fragment-header destination dir exists before
    // the plugin tries to atomic-rename into it. The plugin can
    // create it too, but doing it driver-side keeps the per-TU
    // critical path lean.
    if let Some(frag) = fragment_out {
        if let Some(parent) = frag.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }
    }

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
        fragment_out,
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
/// to be unresolved (that's why we're emitting fragments in the
/// first place); the codegen pass will fail loudly if any genuine
/// errors remain.
///
/// We deliberately do NOT lower `#cust use crate::X;` to an
/// `#include` here because the imported fragment may not exist
/// yet. v0.2 caps at one surface pass; v0.4 may iterate to a
/// fixed point per cust-design.md §4.
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
        // Blank all directives — no fragment includes in surface
        // pass.
        let rewritten = mod_scanner::rewrite(&src_text, &m.source_path, &scan);

        let surface_path = crate_build_dir.join(format!("{}.surface.c", m.qualified_name));
        fs::write(&surface_path, &rewritten)
            .with_context(|| format!("writing `{}`", surface_path.display()))?;

        let fragment_path = layout.fragment_path(crate_name, &m.qualified_name);
        if let Some(parent) = fragment_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }

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
            Some(&fragment_path),
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

/// Build the clang argv for a single TU. `extra_includes` is a
/// list of dirs that become `-I<dir>` flags before the prelude
/// `-include`. For lib compiles this is the original source dir
/// only; for bin compiles in a lib+bin crate it's the lib's
/// include dir followed by the bin source dir.
/// `fragment_out` (when `Some` and a plugin is also configured)
/// becomes `-fplugin-arg-cust-fragment-out=<path>` so the plugin
/// emits a per-module fragment header.
pub fn build_cflags(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    source: &Path,
    object: &Path,
    extra_includes: &[&Path],
    fragment_out: Option<&Path>,
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
    if let Some(plugin) = plan.plugin {
        flags.push(plugin.fplugin_flag());
        if let Some(path) = fragment_out {
            flags.push(format!("-fplugin-arg-cust-fragment-out={}", path.display()));
        }
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

fn write_version_stamp(path: &Path, clang: &Clang) -> Result<()> {
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
/// `cust_pub` decls produces no fragment, which is fine.
fn write_crate_header(layout: &TargetLayout, crate_name: &str, modules: &[Module]) -> Result<()> {
    use std::fmt::Write as _;

    let path = layout.crate_header_path(crate_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }

    let guard = header_guard(crate_name);
    let mut out = String::new();
    out.push_str("/* @generated by cust — DO NOT EDIT */\n");
    let _ = writeln!(out, "/* Public surface of crate `{crate_name}`. */\n");
    let _ = writeln!(out, "#ifndef {guard}\n#define {guard}\n");
    // Pull in fixed-width integer types and a few other staples
    // so the concatenated decls are self-contained at the consumer
    // call site. Consumers that don't need these pay only the
    // preprocessor cost.
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");

    for m in modules {
        let frag = layout.fragment_path(crate_name, &m.qualified_name);
        let Ok(body) = fs::read_to_string(frag) else {
            continue; // module had no cust_pub decls; plugin emitted nothing
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

    fs::write(&path, out).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
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
}
