//! Module graph discovery.
//!
//! Given the crate root and the path to `src/lib.c`, recursively
//! walks `#cust mod <name>;` directives and resolves each to a
//! source file. The output is a flat `Vec<Module>` in
//! depth-first declaration order — the multi-module scheduler
//! compiles them in this order.
//!
//! Resolution rules (V2D-4 in `docs/design/v0.2.md`):
//!
//! * For a module declared in a file in directory `D`, `#cust mod
//!   foo;` resolves to `D/foo.c` *or* `D/foo/mod.c`.
//! * If both exist, error (the cost of a rename is much smaller
//!   than the cost of debugging "which file is cust actually
//!   compiling?").
//! * If neither exists, error.
//! * For a folder-form module (`D/foo/mod.c`), the search
//!   directory for its own children is `D/foo/`.
//!
//! `#cust use crate::<name>;` is recorded per module as a string
//! import name. The build pipeline lowers it to an `#include` of
//! the named module's fragment header (`<name>.cust.h`). v0.2
//! restricts `crate::` paths to a single identifier — nested
//! paths like `crate::parser::lexer` parse OK at the scanner
//! level but are rejected at discovery time below.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use crate::mod_scanner::{self, DirectiveKind};

/// One translation unit in the crate.
#[derive(Debug, Clone)]
pub struct Module {
    /// Dotted qualified name. The root module is `"lib"`; a
    /// top-level sibling is `"util"`; a nested submodule is
    /// `"parser.lexer"`.
    pub qualified_name: String,
    /// Absolute, canonicalised path to the source `.c` file.
    pub source_path: PathBuf,
    /// Crate-relative names this module imports via
    /// `#cust use crate::<name>;`. Validated against the
    /// rest of the graph by `discover`.
    pub imports: Vec<String>,
    /// Cross-crate dep names this module imports via
    /// `#cust use <name>;` (V3D-6). Validated by the build
    /// pipeline against the consumer's `[dependencies]` table —
    /// `modules::discover` has no workspace context.
    #[allow(dead_code)]
    // build.rs validates UseDep names via plan.deps; the field is the canonical list per module
    pub dep_imports: Vec<String>,
}

/// Walk the module graph rooted at `root_source` (typically
/// `<crate_root>/src/lib.c`). Returns modules in depth-first
/// declaration order.
pub fn discover(crate_root: &Path, root_source: &Path) -> Result<Vec<Module>> {
    let root_canon = root_source
        .canonicalize()
        .with_context(|| format!("canonicalising root source `{}`", root_source.display()))?;
    let root_search_dir = root_canon.parent().unwrap_or(crate_root).to_path_buf();

    let mut out: Vec<Module> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    // (qualified_name, source_path, search_dir_for_children)
    let mut stack: Vec<(String, PathBuf, PathBuf)> =
        vec![("lib".to_string(), root_canon, root_search_dir)];

    while let Some((qname, path, search_dir)) = stack.pop() {
        if !visited.insert(path.clone()) {
            bail!(
                "module cycle: `{}` is referenced by two different `#cust mod` declarations",
                path.display()
            );
        }

        let src =
            fs::read_to_string(&path).with_context(|| format!("reading `{}`", path.display()))?;
        let scan = mod_scanner::scan(&src, &path)
            .with_context(|| format!("scanning `{}`", path.display()))?;

        // Two passes: collect children first so we can push them in
        // reverse onto the LIFO stack (preserving declaration
        // order in the output), then emit the current module.
        let mut children: Vec<(String, PathBuf, PathBuf)> = Vec::new();
        let mut imports: Vec<String> = Vec::new();
        let mut dep_imports: Vec<String> = Vec::new();
        for d in &scan.directives {
            match &d.kind {
                DirectiveKind::Mod { name } => {
                    let (child_path, child_search_dir) = resolve_child(&search_dir, name)
                        .with_context(|| {
                            format!("resolving `#cust mod {name};` in `{}`", path.display())
                        })?;
                    let child_qname = if qname == "lib" {
                        name.clone()
                    } else {
                        format!("{qname}.{name}")
                    };
                    children.push((child_qname, child_path, child_search_dir));
                }
                DirectiveKind::UseCrate { name } => {
                    imports.push(name.clone());
                }
                DirectiveKind::UseDep { name } => {
                    dep_imports.push(name.clone());
                }
            }
        }

        out.push(Module {
            qualified_name: qname,
            source_path: path,
            imports,
            dep_imports,
        });

        // Reverse so the first declared child is popped first.
        for c in children.into_iter().rev() {
            stack.push(c);
        }
    }

    // Cross-module validation: every `#cust use crate::X;` name
    // must match the *qualified* name of some other module in
    // the crate. v0.2 only supports single-identifier imports;
    // a name containing `.` would have come from a nested
    // qualified name and is rejected with a clearer message.
    let known: HashMap<&str, &Path> = out
        .iter()
        .map(|m| (m.qualified_name.as_str(), m.source_path.as_path()))
        .collect();
    for m in &out {
        for imp in &m.imports {
            if imp.contains('.') {
                bail!(
                    "`#cust use crate::{imp};` in `{}`: nested crate paths are not supported in v0.2",
                    m.source_path.display()
                );
            }
            if !known.contains_key(imp.as_str()) {
                bail!(
                    "`#cust use crate::{imp};` in `{}`: no module named `{imp}` in this crate",
                    m.source_path.display()
                );
            }
            if imp == &m.qualified_name {
                bail!(
                    "`#cust use crate::{imp};` in `{}`: a module cannot import itself",
                    m.source_path.display()
                );
            }
        }
    }

    Ok(out)
}

fn resolve_child(search_dir: &Path, name: &str) -> Result<(PathBuf, PathBuf)> {
    let file_form = search_dir.join(format!("{name}.c"));
    let folder_form = search_dir.join(name).join("mod.c");
    let file_exists = file_form.is_file();
    let folder_exists = folder_form.is_file();

    match (file_exists, folder_exists) {
        (true, true) => bail!(
            "ambiguous module `{name}`: both `{}` and `{}` exist; keep exactly one",
            file_form.display(),
            folder_form.display(),
        ),
        (true, false) => {
            let canon = file_form
                .canonicalize()
                .with_context(|| format!("canonicalising `{}`", file_form.display()))?;
            Ok((canon, search_dir.to_path_buf()))
        }
        (false, true) => {
            let canon = folder_form
                .canonicalize()
                .with_context(|| format!("canonicalising `{}`", folder_form.display()))?;
            // Children of `name/mod.c` live in `name/`.
            let child_search_dir = search_dir.join(name);
            Ok((canon, child_search_dir))
        }
        (false, false) => bail!(
            "module `{name}` not found: neither `{}` nor `{}` exists",
            file_form.display(),
            folder_form.display(),
        ),
    }
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
/// shouldn't happen — `discover` validates this — but we guard
/// against it for defence in depth) are treated as if the missing
/// edge weren't there.
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
    // back to discovery order for the leftover. `discover` already
    // rejects #cust mod cycles, and intra-crate fragment includes
    // are forward-decl-only, so this branch shouldn't be reachable
    // in practice.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Write `content` to `dir/<rel>`, creating parent dirs.
    fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn single_module_no_directives() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "int x = 1;\n");

        let mods = discover(crate_root, &root).unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].qualified_name, "lib");
        assert_eq!(mods[0].source_path, root.canonicalize().unwrap());
    }

    #[test]
    fn flat_two_module_crate_file_form() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "#cust mod util;\n");
        let _util = write(crate_root, "src/util.c", "int u = 1;\n");

        let mods = discover(crate_root, &root).unwrap();
        let names: Vec<&str> = mods.iter().map(|m| m.qualified_name.as_str()).collect();
        assert_eq!(names, vec!["lib", "util"]);
    }

    #[test]
    fn flat_two_module_crate_folder_form() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "#cust mod parser;\n");
        let _parser = write(crate_root, "src/parser/mod.c", "int p = 1;\n");

        let mods = discover(crate_root, &root).unwrap();
        let names: Vec<&str> = mods.iter().map(|m| m.qualified_name.as_str()).collect();
        assert_eq!(names, vec!["lib", "parser"]);
    }

    #[test]
    fn nested_modules_use_dotted_names() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let _root = write(crate_root, "src/lib.c", "#cust mod parser;\n");
        let _parser = write(crate_root, "src/parser/mod.c", "#cust mod lexer;\n");
        let _lexer = write(crate_root, "src/parser/lexer.c", "int l = 1;\n");

        let root_path = crate_root.join("src/lib.c");
        let mods = discover(crate_root, &root_path).unwrap();
        let names: Vec<&str> = mods.iter().map(|m| m.qualified_name.as_str()).collect();
        assert_eq!(names, vec!["lib", "parser", "parser.lexer"]);
    }

    #[test]
    fn ambiguous_form_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "#cust mod foo;\n");
        let _file_form = write(crate_root, "src/foo.c", "int f = 1;\n");
        let _folder_form = write(crate_root, "src/foo/mod.c", "int f2 = 1;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("ambiguous module `foo`"), "{e}");
        assert!(e.contains("keep exactly one"), "{e}");
    }

    #[test]
    fn missing_module_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "#cust mod nope;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("module `nope` not found"), "{e}");
    }

    #[test]
    fn use_crate_records_imports() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(
            crate_root,
            "src/lib.c",
            "#cust mod util;\n#cust use crate::util;\n",
        );
        let _util = write(crate_root, "src/util.c", "int u = 1;\n");

        let mods = discover(crate_root, &root).unwrap();
        let lib = mods.iter().find(|m| m.qualified_name == "lib").unwrap();
        assert_eq!(lib.imports, vec!["util".to_string()]);
        let util = mods.iter().find(|m| m.qualified_name == "util").unwrap();
        assert!(util.imports.is_empty());
    }

    #[test]
    fn use_crate_with_unknown_module_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(crate_root, "src/lib.c", "#cust use crate::nope;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("no module named `nope`"), "{e}");
    }

    #[test]
    fn use_crate_self_import_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        // Root module is named "lib" — self-import via crate::lib
        // would otherwise resolve to itself.
        let root = write(crate_root, "src/lib.c", "#cust use crate::lib;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("cannot import itself"), "{e}");
    }

    #[test]
    fn cycle_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        // lib.c → mod a; (resolves to a.c which contains mod a;
        // again, looking up from src/ would resolve to itself if
        // someone created such a layout). Easier: two roots
        // declaring the same module file.
        let root = write(crate_root, "src/lib.c", "#cust mod foo;\n#cust mod foo;\n");
        let _foo = write(crate_root, "src/foo.c", "int f = 1;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("module cycle"), "{e}");
    }

    // ─── graph algorithms (topo_order_modules / module_sccs) ────

    fn mk_mod(name: &str, imports: &[&str]) -> Module {
        Module {
            qualified_name: name.to_string(),
            source_path: std::path::PathBuf::from(format!("/x/{name}.c")),
            imports: imports.iter().map(|s| (*s).to_string()).collect(),
            dep_imports: Vec::new(),
        }
    }

    #[test]
    fn topo_order_modules_preserves_discovery_order_with_no_edges() {
        // No intra-crate imports → discovery order preserved.
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
        let mods = vec![mk_mod("lib", &[]), mk_mod("a", &["b"]), mk_mod("b", &["a"])];
        let sccs = module_sccs(&mods);
        assert_eq!(sccs, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn module_sccs_detects_three_cycle() {
        // a -> b -> c -> a is one SCC of all three.
        let mods = vec![
            mk_mod("a", &["b"]),
            mk_mod("b", &["c"]),
            mk_mod("c", &["a"]),
        ];
        let sccs = module_sccs(&mods);
        assert_eq!(sccs, vec![vec![0, 1, 2]]);
    }
}
