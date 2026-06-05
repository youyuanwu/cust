//! Workspace discovery and member resolution.
//!
//! v0.3 ([docs/design/v0.3.md]) adds `[workspace]` to `Cust.toml`.
//! A workspace consists of:
//!
//! * A **workspace root** — the directory containing the `Cust.toml`
//!   that declares `[workspace]`. This is the canonical anchor: a
//!   single shared `target/` and a single `Cust.lock` both live
//!   here.
//! * Zero or more **members** — buildable crates listed in
//!   `[workspace] members = [...]`. Each member directory contains
//!   its own `Cust.toml` with a `[package]` section.
//! * Optionally, an **implicit member**: when the workspace root's
//!   manifest itself has `[package]` (root-is-also-a-member shape,
//!   V3D-1 option B), the root counts as a member without needing
//!   to appear in `members`.
//!
//! A *virtual* workspace is one whose root manifest has only
//! `[workspace]` and no `[package]`; per V3D-1, virtual roots must
//! not contain a `src/` directory.
//!
//! Slice A (this file's initial form) implements discovery,
//! validation, and member loading. Slice C wires the result into
//! the build pipeline.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use crate::manifest::{Manifest, ManifestLocation, MANIFEST_FILE};

/// One resolved workspace member.
#[derive(Debug)]
pub struct Member {
    /// Member name (from its `[package].name`).
    pub name: String,
    /// Absolute, canonicalised path to the member directory.
    pub root: PathBuf,
    /// Loaded + validated manifest.
    pub manifest: Manifest,
    /// `true` when this member is the workspace root itself
    /// (root-is-also-a-member shape). Otherwise the member is in
    /// a subdirectory listed in `[workspace] members`.
    pub is_implicit_root: bool,
}

/// A resolved workspace.
#[derive(Debug)]
pub struct Workspace {
    /// Absolute, canonicalised path to the workspace root
    /// directory (the directory containing the root `Cust.toml`).
    pub root: PathBuf,
    /// Absolute path to the root `Cust.toml`.
    pub root_manifest_path: PathBuf,
    /// The workspace root manifest.
    pub root_manifest: Manifest,
    /// Members in declaration order (implicit root first when
    /// present, then `members` entries in their `Cust.toml` order).
    pub members: Vec<Member>,
}

impl Workspace {
    /// Discover the workspace by walking up from `start_dir`.
    ///
    /// Discovery rules:
    ///
    /// * If the nearest `Cust.toml` declares `[workspace]`, that
    ///   directory is the workspace root.
    /// * Otherwise, keep walking. If an ancestor declares
    ///   `[workspace]` *and* lists this crate's directory in its
    ///   `members`, use that ancestor as the root.
    /// * If no enclosing workspace is found, the nearest manifest
    ///   is treated as a **single-crate workspace**: one implicit
    ///   member, no real `[workspace]` table.
    pub fn discover(start_dir: &Path) -> Result<Self> {
        let nearest = Manifest::discover(start_dir)?;
        let nearest_manifest = Manifest::load(&nearest.path)?;

        // Case 1: the nearest manifest itself declares [workspace].
        if nearest_manifest.declares_workspace() {
            return Self::build(&nearest, nearest_manifest);
        }

        // Case 2: walk further up looking for an enclosing
        // [workspace] that lists this dir as a member.
        if let Some(parent) = nearest.dir.parent() {
            if let Some(ws) = Self::find_enclosing_workspace(parent, &nearest.dir)? {
                return Ok(ws);
            }
        }

        // Case 3: standalone single-crate. Treat the nearest
        // manifest's directory as a zero-`[workspace]`-table
        // workspace with one member (the crate itself, if it has
        // a [package]).
        Self::single_crate(&nearest, nearest_manifest)
    }

    /// Build a workspace from the `[workspace]`-bearing manifest
    /// at `loc`. The manifest has already been parsed +
    /// validated by `Manifest::load`.
    fn build(loc: &ManifestLocation, root_manifest: Manifest) -> Result<Self> {
        let root = loc
            .dir
            .canonicalize()
            .with_context(|| format!("canonicalising `{}`", loc.dir.display()))?;

        // V3D-1: virtual roots must not contain src/.
        if !root_manifest.is_package() && root.join("src").exists() {
            bail!(
                "virtual workspace root `{}` must not contain `src/`; \
                 either remove `src/` or add a [package] section",
                loc.path.display()
            );
        }

        let ws_table = root_manifest
            .workspace
            .as_ref()
            .expect("build() requires a [workspace] manifest");

        let mut members: Vec<Member> = Vec::new();
        let mut seen_names: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut seen_dirs: BTreeMap<PathBuf, String> = BTreeMap::new();

        // Implicit root member (root-is-also-a-member shape).
        if root_manifest.is_package() {
            let pkg = root_manifest.require_package(&loc.path)?;
            let m = Member {
                name: pkg.name.clone(),
                root: root.clone(),
                manifest: clone_manifest(&loc.path)?,
                is_implicit_root: true,
            };
            seen_names.insert(m.name.clone(), m.root.clone());
            seen_dirs.insert(m.root.clone(), m.name.clone());
            members.push(m);
        }

        // Listed members.
        for rel in &ws_table.members {
            let member_dir = root.join(rel);
            let canon = member_dir.canonicalize().with_context(|| {
                format!(
                    "workspace member `{rel}` does not exist or is not \
                     accessible at `{}`",
                    member_dir.display()
                )
            })?;
            if !canon.starts_with(&root) {
                bail!(
                    "workspace member `{rel}` resolves outside the \
                     workspace root (`{}`)",
                    canon.display()
                );
            }
            let member_manifest_path = canon.join(MANIFEST_FILE);
            if !member_manifest_path.is_file() {
                bail!(
                    "workspace member `{rel}` is missing `{MANIFEST_FILE}` \
                     (looked at `{}`)",
                    member_manifest_path.display()
                );
            }
            let member_manifest = Manifest::load(&member_manifest_path)?;
            let pkg = member_manifest.require_package(&member_manifest_path)?;
            if let Some(other) = seen_names.get(&pkg.name) {
                bail!(
                    "duplicate member name `{}` (also at `{}`)",
                    pkg.name,
                    other.display()
                );
            }
            if let Some(other) = seen_dirs.get(&canon) {
                bail!(
                    "workspace member dir `{}` is listed twice (as `{}` and `{rel}`)",
                    canon.display(),
                    other
                );
            }
            seen_names.insert(pkg.name.clone(), canon.clone());
            seen_dirs.insert(canon.clone(), pkg.name.clone());
            members.push(Member {
                name: pkg.name.clone(),
                root: canon,
                manifest: member_manifest,
                is_implicit_root: false,
            });
        }

        if members.is_empty() {
            bail!(
                "workspace at `{}` has no members; add `[workspace] members = [...]` \
                 or a `[package]` section to the root manifest",
                loc.path.display()
            );
        }

        Ok(Self {
            root,
            root_manifest_path: loc.path.clone(),
            root_manifest,
            members,
        })
    }

    /// Build a single-crate "workspace" wrapper around a manifest
    /// that has no `[workspace]` table. The crate itself is the
    /// only implicit member; the workspace root *is* the crate
    /// root.
    fn single_crate(loc: &ManifestLocation, manifest: Manifest) -> Result<Self> {
        let root = loc
            .dir
            .canonicalize()
            .with_context(|| format!("canonicalising `{}`", loc.dir.display()))?;
        if !manifest.is_package() {
            bail!(
                "`{}` has neither [package] nor [workspace]; \
                 add a [package] section to make it a buildable crate",
                loc.path.display()
            );
        }
        let pkg = manifest.require_package(&loc.path)?;
        let member = Member {
            name: pkg.name.clone(),
            root: root.clone(),
            manifest: clone_manifest(&loc.path)?,
            is_implicit_root: true,
        };
        Ok(Self {
            root,
            root_manifest_path: loc.path.clone(),
            root_manifest: manifest,
            members: vec![member],
        })
    }

    /// Search ancestor directories for a `Cust.toml` whose
    /// `[workspace]` table lists `member_dir` as a member.
    ///
    /// Returns `Ok(Some(ws))` if found, `Ok(None)` if no enclosing
    /// workspace claims this directory.
    fn find_enclosing_workspace(start_from: &Path, member_dir: &Path) -> Result<Option<Self>> {
        let member_canon = member_dir
            .canonicalize()
            .with_context(|| format!("canonicalising `{}`", member_dir.display()))?;
        let mut cur = Some(start_from);
        while let Some(dir) = cur {
            let candidate = dir.join(MANIFEST_FILE);
            if candidate.is_file() {
                let m = Manifest::load(&candidate)?;
                if m.declares_workspace() {
                    let ws_root_canon = dir
                        .canonicalize()
                        .with_context(|| format!("canonicalising `{}`", dir.display()))?;
                    let table = m.workspace.as_ref().unwrap();
                    let claims = table.members.iter().any(|rel| {
                        ws_root_canon
                            .join(rel)
                            .canonicalize()
                            .is_ok_and(|c| c == member_canon)
                    });
                    if claims {
                        let loc = ManifestLocation {
                            path: candidate,
                            dir: dir.to_path_buf(),
                        };
                        return Self::build(&loc, m).map(Some);
                    }
                    // Found an enclosing [workspace] but it doesn't
                    // list this dir; stop walking (Cargo's rule).
                    return Ok(None);
                }
            }
            cur = dir.parent();
        }
        Ok(None)
    }

    /// `true` when this workspace has a real `[workspace]` table
    /// (i.e. is not a single-crate degeneracy).
    pub const fn is_real_workspace(&self) -> bool {
        self.root_manifest.workspace.is_some()
    }
}

/// Re-read a manifest from disk. Used when we need a fresh owned
/// `Manifest` for a `Member` even though we already have one
/// borrowed elsewhere (the parser does no I/O of its own).
fn clone_manifest(path: &Path) -> Result<Manifest> {
    Manifest::load(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn single_crate_no_workspace_table_is_one_implicit_member() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.c", "int x = 1;\n");

        let ws = Workspace::discover(root).unwrap();
        assert!(!ws.is_real_workspace());
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0].name, "solo");
        assert!(ws.members[0].is_implicit_root);
    }

    #[test]
    fn virtual_workspace_lists_each_member() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[workspace]\nmembers = [\"app\", \"util\"]\n",
        );
        write(
            root,
            "app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");
        write(
            root,
            "util/Cust.toml",
            "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        );
        write(root, "util/src/lib.c", "int u = 1;\n");

        let ws = Workspace::discover(root).unwrap();
        assert!(ws.is_real_workspace());
        let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["app", "util"]);
        assert!(ws.members.iter().all(|m| !m.is_implicit_root));
    }

    #[test]
    fn root_is_also_a_member_implicit_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"root\"\nversion = \"0.1.0\"\n\
             [workspace]\nmembers = [\"util\"]\n",
        );
        write(root, "src/lib.c", "int r = 1;\n");
        write(
            root,
            "util/Cust.toml",
            "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        );
        write(root, "util/src/lib.c", "int u = 1;\n");

        let ws = Workspace::discover(root).unwrap();
        let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["root", "util"]);
        assert!(ws.members[0].is_implicit_root);
        assert!(!ws.members[1].is_implicit_root);
    }

    #[test]
    fn virtual_root_with_src_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"app\"]\n");
        // virtual root must not have src/
        write(root, "src/lib.c", "int x = 1;\n");
        write(
            root,
            "app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("virtual workspace root"), "{e}");
        assert!(e.contains("must not contain `src/`"), "{e}");
    }

    #[test]
    fn discover_from_member_subdir_finds_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"app\"]\n");
        write(
            root,
            "app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");

        // Discover from inside the member directory.
        let ws = Workspace::discover(&root.join("app")).unwrap();
        assert!(ws.is_real_workspace());
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0].name, "app");
    }

    #[test]
    fn missing_member_dir_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"nope\"]\n");
        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("workspace member `nope`"), "{e}");
        assert!(
            e.contains("does not exist") || e.contains("not accessible"),
            "{e}"
        );
    }

    #[test]
    fn member_without_package_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"app\"]\n");
        // member has no [package]
        write(root, "app/Cust.toml", "[workspace]\nmembers = []\n");
        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("declares no [package]"), "{e}");
    }

    #[test]
    fn duplicate_member_names_are_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");
        write(
            root,
            "a/Cust.toml",
            "[package]\nname = \"same\"\nversion = \"0.1.0\"\n",
        );
        write(root, "a/src/lib.c", "int a = 1;\n");
        write(
            root,
            "b/Cust.toml",
            "[package]\nname = \"same\"\nversion = \"0.1.0\"\n",
        );
        write(root, "b/src/lib.c", "int b = 1;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("duplicate member name `same`"), "{e}");
    }
}
