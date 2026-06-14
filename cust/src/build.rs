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
};

use anyhow::{Context, Result};

use crate::{
    clang::Clang,
    manifest::{CrateKind, Manifest},
    modules::Module,
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
        objects: Vec::new(),
        archive,
        executables,
        test_executable: None,
        integration_tests: Vec::new(),
        compile_commands: layout.target_root.join("compile_commands.json"),
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
pub fn topo_order_modules(modules: &[Module]) -> Vec<&Module> {
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

/// v0.4.5 V45D-6: strongly-connected components of the intra-crate
/// `#cust use crate::<m>;` import graph, by Tarjan's algorithm.
/// Returns one `Vec<usize>` of module indices per SCC. A singleton
/// SCC is an acyclic module (the common case → fine-grained
/// `surface-module` command); an SCC of size > 1 is a
/// `[[cust::pub_repr]]` import cycle that must be surfaced as one
/// coarse `surface-cycle` command (a `DEPENDS` cycle is a hard
/// `CMake` error, so the cycle cannot be a fine-grained DAG).
///
/// Within each returned SCC the indices are sorted ascending, and
/// the SCC list is sorted by each SCC's smallest member index, so
/// the output is deterministic (V45D-15) and — for an all-acyclic
/// crate — preserves discovery order (each singleton `[i]`).
#[must_use]
pub fn module_sccs(modules: &[Module]) -> Vec<Vec<usize>> {
    // Iterative Tarjan over the import graph (avoids deep recursion
    // on large module sets).
    const UNVISITED: i64 = -1;
    let n = modules.len();
    let name_to_idx: std::collections::BTreeMap<&str, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.qualified_name.as_str(), i))
        .collect();
    // Adjacency: edge i -> j when module i `#cust use crate::j`.
    let adj: Vec<Vec<usize>> = modules
        .iter()
        .map(|m| {
            let mut succ: Vec<usize> = m
                .imports
                .iter()
                .filter_map(|imp| name_to_idx.get(imp.as_str()).copied())
                .collect();
            succ.sort_unstable();
            succ.dedup();
            succ
        })
        .collect();

    let mut index: Vec<i64> = vec![UNVISITED; n];
    let mut lowlink: Vec<i64> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index: i64 = 0;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    // Explicit DFS stack of (node, next-successor-cursor).
    for start in 0..n {
        if index[start] != UNVISITED {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(v, ci)) = work.last() {
            if ci == 0 {
                index[v] = next_index;
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if ci < adj[v].len() {
                // Advance this frame's cursor and recurse / relax.
                work.last_mut().unwrap().1 += 1;
                let w = adj[v][ci];
                if index[w] == UNVISITED {
                    work.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w]);
                }
            } else {
                // Done with v: if it's an SCC root, pop the SCC.
                if lowlink[v] == index[v] {
                    let mut comp: Vec<usize> = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comp.sort_unstable();
                    sccs.push(comp);
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }

    // Deterministic order: by smallest member index.
    sccs.sort_by_key(|c| c[0]);
    sccs
}

/// Derive an include-guard macro name from a crate name. Mirrors
/// cargo's `name = "my-crate"` → C-identifier sanitisation: `-`
/// and any other non-alphanumeric becomes `_`, upper-case the
/// whole thing, and suffix `_H`.
pub fn header_guard(crate_name: &str) -> String {
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
pub fn strip_fragment_header_comment(body: &str) -> &str {
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

    #[test]
    fn module_sccs_all_singletons_in_discovery_order() {
        // Acyclic graph: every module is its own SCC; the SCC list
        // is in discovery (index) order so the fine-grained surface
        // command order is unchanged from V45D-4.
        use super::module_sccs;
        let mods = vec![
            mk_mod("lib", &["types"]),
            mk_mod("types", &[]),
            mk_mod("math", &["types"]),
        ];
        let sccs = module_sccs(&mods);
        assert_eq!(sccs, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn module_sccs_detects_two_cycle() {
        // `a` and `b` import each other → one SCC of size 2; `lib`
        // is a singleton. The cycle members are sorted ascending
        // within the SCC, and the SCC list is sorted by smallest
        // member index.
        use super::module_sccs;
        let mods = vec![mk_mod("lib", &[]), mk_mod("a", &["b"]), mk_mod("b", &["a"])];
        let sccs = module_sccs(&mods);
        assert_eq!(sccs, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn module_sccs_detects_three_cycle() {
        // a -> b -> c -> a is one SCC of all three.
        use super::module_sccs;
        let mods = vec![
            mk_mod("a", &["b"]),
            mk_mod("b", &["c"]),
            mk_mod("c", &["a"]),
        ];
        let sccs = module_sccs(&mods);
        assert_eq!(sccs, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn non_convergence_error_is_verbatim() {
        // V45D-6 verification item 7: the §4 message is byte-stable
        // (shared by the in-process fixed-point and surface-cycle).
        let err = crate::generate::non_convergence_error(3, &["a", "b"]);
        let msg = format!("{err}");
        assert_eq!(
            msg,
            "circular `[[cust::pub_repr]]` dependency did not converge\n  \
             in 3 iterations between modules: a, b\n  \
             hint: break the cycle by exporting one side as `[[cust::pub]]`\n        \
             (opaque) instead of `[[cust::pub_repr]]`"
        );
    }
}
