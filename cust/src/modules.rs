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
//! `#cust use crate::<name>;` directives are recognised by the
//! scanner but rejected at discovery time in v0.2 — cross-module
//! imports require the fragment-header machinery, which lands with
//! the plugin later in the milestone.

use std::{
    collections::HashSet,
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
                    bail!(
                        "`#cust use crate::{name};` in `{}`: cross-module imports require the cust plugin (lands later in v0.2)",
                        path.display()
                    );
                }
            }
        }

        out.push(Module {
            qualified_name: qname,
            source_path: path,
        });

        // Reverse so the first declared child is popped first.
        for c in children.into_iter().rev() {
            stack.push(c);
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
    fn use_crate_is_rejected_in_v0_2() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_root = tmp.path();
        let root = write(
            crate_root,
            "src/lib.c",
            "#cust mod util;\n#cust use crate::util;\n",
        );
        let _util = write(crate_root, "src/util.c", "int u = 1;\n");

        let e = format!("{:#}", discover(crate_root, &root).unwrap_err());
        assert!(e.contains("#cust use crate::util"), "{e}");
        assert!(e.contains("require the cust plugin"), "{e}");
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
}
