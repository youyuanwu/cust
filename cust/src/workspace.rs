//! `Workspace` discovery and member resolution.
//!
//! v0.3 ([docs/design/v0.3.0.md]) adds `[workspace]` to `Cust.toml`.
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
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use crate::{
    build::{self, BuildOutputs, BuildPlan},
    clang::Clang,
    manifest::{CrateKind, Manifest, ManifestLocation, MANIFEST_FILE},
    plugin::Plugin,
    profile::ProfileKind,
    target_layout::TargetLayout,
};

/// One resolved workspace member.
#[derive(Debug)]
pub struct Member {
    /// `Member` name (from its `[package].name`).
    pub name: String,
    /// Absolute, canonicalised path to the member directory.
    pub root: PathBuf,
    /// Loaded + validated manifest.
    pub manifest: Manifest,
    /// What this member produces (lib / bin / lib+bin). Resolved
    /// once at workspace construction via `Manifest::resolve_kind`
    /// — enforces filesystem invariants (declared source files
    /// exist) early, and lets `cust run` know which members are
    /// runnable without re-walking the disk.
    pub kind: CrateKind,
    /// `true` when this member is the workspace root itself
    /// (root-is-also-a-member shape). Otherwise the member is in
    /// a subdirectory listed in `[workspace] members`.
    #[allow(dead_code)] // surfaced by Slice D (Cust.lock) and `cust build -v` (Slice E)
    pub is_implicit_root: bool,
    /// Names of other members this member depends on, in
    /// declaration order. Resolved by `Workspace::resolve_edges`
    /// (V3D-4: every dep path must point at a sibling member).
    pub deps: Vec<String>,
    /// v0.4.3 V43D-1: integration tests discovered under
    /// `<root>/tests/*.c` (top level only, no recursion), sorted
    /// by stem for deterministic run order. Empty when the member
    /// has no `tests/` dir or it contains no top-level `.c` files.
    /// Populated even for bin-only members at discovery time;
    /// V32D-11 scoping (only lib members get tested) is applied
    /// later by the `CMake`/runner layer (Slice B/C).
    #[allow(dead_code)] // read by Slice B (per-file add_executable emission)
    pub integration_tests: Vec<IntegrationTest>,
}

/// v0.4.3 V43D-1: one integration-test source file under a
/// member's `tests/` directory. One file ⇒ one `CMake` exe target
/// (Slice B) ⇒ one fork-per-test runner (Slice C).
#[derive(Debug, Clone)]
pub struct IntegrationTest {
    /// File stem (basename minus the `.c` extension). Validated
    /// against `[A-Za-z][A-Za-z0-9_-]*` (V43D-8) at discovery
    /// time; used verbatim as the `CMake` target infix
    /// (`<crate>__itest__<stem>`), the on-disk exe name, and the
    /// per-stem cwd directory (V43D-5/V43D-11).
    pub stem: String,
    /// Absolute path to the `tests/<stem>.c` source file.
    #[allow(dead_code)] // read by Slice B (add_executable source path)
    pub source: PathBuf,
}

/// A resolved workspace.
#[derive(Debug)]
pub struct Workspace {
    /// Absolute, canonicalised path to the workspace root
    /// directory (the directory containing the root `Cust.toml`).
    pub root: PathBuf,
    /// Absolute path to the root `Cust.toml`.
    #[allow(dead_code)] // surfaced by Slice D (Cust.lock) for the moved-workspace check
    pub root_manifest_path: PathBuf,
    /// The workspace root manifest.
    #[allow(dead_code)] // surfaced by `is_real_workspace` + Slice D
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
    #[allow(clippy::too_many_lines)] // member-staging + invariant checks read more clearly inline than split
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
            let manifest_clone = clone_manifest(&loc.path)?;
            let kind = manifest_clone
                .resolve_kind(&root)
                .with_context(|| format!("member `{}`", pkg.name))?;
            let m = Member {
                name: pkg.name.clone(),
                root: root.clone(),
                manifest: manifest_clone,
                kind,
                is_implicit_root: true,
                deps: Vec::new(),
                integration_tests: discover_integration_tests(&root, &pkg.name)?,
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
            let kind = member_manifest
                .resolve_kind(&canon)
                .with_context(|| format!("member `{}`", pkg.name))?;
            let integration_tests = discover_integration_tests(&canon, &pkg.name)?;
            members.push(Member {
                name: pkg.name.clone(),
                root: canon,
                manifest: member_manifest,
                kind,
                is_implicit_root: false,
                deps: Vec::new(),
                integration_tests,
            });
        }

        if members.is_empty() {
            bail!(
                "workspace at `{}` has no members; add `[workspace] members = [...]` \
                 or a `[package]` section to the root manifest",
                loc.path.display()
            );
        }

        let mut ws = Self {
            root,
            root_manifest_path: loc.path.clone(),
            root_manifest,
            members,
        };
        ws.resolve_edges()?;
        Ok(ws)
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
        // V3D-4 / scope item 9: path deps require a workspace.
        // A single-crate manifest with [dependencies] has nowhere
        // for its deps to point — there are no sibling members.
        if !manifest.dependencies.is_empty() {
            bail!(
                "`{}` has [dependencies] but no enclosing [workspace]; \
                 path dependencies require a [workspace] — add it to a \
                 parent Cust.toml",
                loc.path.display()
            );
        }
        let pkg = manifest.require_package(&loc.path)?;
        let manifest_clone = clone_manifest(&loc.path)?;
        let kind = manifest_clone
            .resolve_kind(&root)
            .with_context(|| format!("member `{}`", pkg.name))?;
        let member = Member {
            name: pkg.name.clone(),
            root: root.clone(),
            manifest: manifest_clone,
            kind,
            is_implicit_root: true,
            deps: Vec::new(),
            integration_tests: discover_integration_tests(&root, &pkg.name)?,
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
    #[allow(dead_code)] // surfaced by Slice D (Cust.lock emission gated on this)
    pub const fn is_real_workspace(&self) -> bool {
        self.root_manifest.workspace.is_some()
    }

    /// For each member, resolve its `[dependencies]` paths
    /// against the *member's own directory* and check that each
    /// result is another member's root directory (V3D-4). Also
    /// checks the dep `name` matches the resolved target's
    /// `[package].name` — names rather than paths are what
    /// `#cust use <name>;` keys off, so they must be consistent.
    ///
    /// Errors raised here:
    ///
    /// * path doesn't exist on disk
    /// * path resolves outside the workspace
    /// * path resolves to a directory that isn't a workspace
    ///   member
    /// * dep name doesn't match the resolved member's package
    ///   name
    /// * member depends on itself
    /// * the consumer is a non-workspace single-crate degeneracy
    ///   and has any deps (this case is also caught by
    ///   `cli::locate`; we double-check here for robustness)
    fn resolve_edges(&mut self) -> Result<()> {
        // Build a lookup: canonicalised member root → (index, name).
        // Members are uniquely-named (build() rejects dups).
        let dir_to_member: BTreeMap<PathBuf, (usize, String)> = self
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| (m.root.clone(), (i, m.name.clone())))
            .collect();

        for i in 0..self.members.len() {
            // Take the member's deps out by value so we can mutate
            // through `&mut self.members[i].deps` later without
            // holding two borrows. Snapshot what we need first.
            let (consumer_name, consumer_root, dep_specs) = {
                let m = &self.members[i];
                (m.name.clone(), m.root.clone(), m.manifest.dep_specs())
            };

            let mut resolved: Vec<String> = Vec::with_capacity(dep_specs.len());
            for spec in &dep_specs {
                let raw = consumer_root.join(&spec.path);
                let canon = raw.canonicalize().with_context(|| {
                    format!(
                        "dependency `{}` of `{consumer_name}`: path `{}` \
                         does not exist or is not accessible (resolved to `{}`)",
                        spec.name,
                        spec.path,
                        raw.display()
                    )
                })?;

                // V3D-4: must be inside the workspace root.
                if !canon.starts_with(&self.root) {
                    bail!(
                        "dependency `{}` of `{consumer_name}` resolves to `{}` \
                         which is not a workspace member; add it to [workspace] \
                         members or move it inside the workspace tree",
                        spec.name,
                        canon.display()
                    );
                }

                // V3D-4: must point at a known member directory.
                let Some((dep_idx, dep_pkg_name)) = dir_to_member.get(&canon) else {
                    bail!(
                        "dependency `{}` of `{consumer_name}` resolves to `{}` \
                         which is not a workspace member; add it to [workspace] \
                         members",
                        spec.name,
                        canon.display()
                    );
                };

                // Self-dep: a member depending on itself.
                if *dep_idx == i {
                    bail!(
                        "member `{consumer_name}` depends on itself via `{}`",
                        spec.name
                    );
                }

                // Name consistency: the [dependencies] key must
                // match the resolved member's [package].name.
                // Otherwise `#cust use <spec.name>;` in the
                // consumer would point at the wrong include file.
                if &spec.name != dep_pkg_name {
                    bail!(
                        "dependency name mismatch: `{consumer_name}` declares \
                         `{} = {{ path = \"{}\" }}` but that path resolves to \
                         member `{dep_pkg_name}`; rename the dependency key to \
                         match",
                        spec.name,
                        spec.path
                    );
                }

                // V31D-6 (v0.3.1): only library members may
                // appear as dependencies. Bin-only members have
                // no surface to import; bin-bin edges are
                // disallowed for the same reason Cargo disallows
                // them — if two bins want to share code, extract
                // a lib member they both depend on.
                let dep_kind = &self.members[*dep_idx].kind;
                if !dep_kind.has_lib() {
                    bail!(
                        "workspace member `{dep_pkg_name}` (bin) cannot be a \
                         dependency of `{consumer_name}` — only library \
                         members may appear in [dependencies]\n  hint: \
                         extract the shared code into a separate lib member"
                    );
                }

                resolved.push(spec.name.clone());
            }

            self.members[i].deps = resolved;
        }

        Ok(())
    }

    /// Look up a member by name. `None` if absent.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    /// Return member names in reverse-topological order
    /// (dependencies before dependents). Cycle detection raises
    /// `error: dependency cycle: a → b → a` with the cycle
    /// canonicalised to start at the alphabetically-first name
    /// (per scope item 5 in docs/design/v0.3.0.md). Self-cycles
    /// produce `error: dependency cycle: a → a`.
    ///
    /// The returned vector contains every member exactly once.
    pub fn build_order(&self) -> Result<Vec<String>> {
        // Kahn's algorithm: compute in-degrees in the *consumer →
        // dependency* direction (so dep-first is a forward topo),
        // then peel zero-in-degree nodes one at a time. Equivalent
        // to a DFS post-order but easier to reason about.
        let n = self.members.len();
        let name_to_idx: BTreeMap<&str, usize> = self
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.as_str(), i))
            .collect();

        // adj[i] = indices of members that depend on i.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        // in_degree[i] = number of deps i still needs to wait for.
        let mut in_degree: Vec<usize> = vec![0; n];
        for (i, m) in self.members.iter().enumerate() {
            for d in &m.deps {
                // resolve_edges already guarantees d is a member.
                let dep_idx = name_to_idx[d.as_str()];
                adj[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }

        // Seed: every member with no deps. Sort descending by name
        // so `pop()` (which removes the last element) yields
        // names in ascending order — deterministic output across
        // runs.
        let mut ready: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        ready.sort_by(|a, b| self.members[*b].name.cmp(&self.members[*a].name));

        let mut out: Vec<String> = Vec::with_capacity(n);
        while let Some(i) = ready.pop() {
            out.push(self.members[i].name.clone());
            // Collect newly-ready into a temp so we can sort.
            let mut newly_ready: Vec<usize> = Vec::new();
            for &j in &adj[i] {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    newly_ready.push(j);
                }
            }
            // Same descending-then-pop trick.
            newly_ready.sort_by(|a, b| self.members[*b].name.cmp(&self.members[*a].name));
            ready.extend(newly_ready);
            // Keep `ready` sorted (descending) overall so the next
            // pop is the next alphabetical name regardless of
            // which newly_ready batch contributed it.
            ready.sort_by(|a, b| self.members[*b].name.cmp(&self.members[*a].name));
        }

        if out.len() != n {
            // Cycle. Find one for the diagnostic. Start from the
            // alphabetically-first member that still has nonzero
            // in-degree (per the scope item 5 rule).
            let mut remaining: Vec<usize> = (0..n).filter(|&i| in_degree[i] > 0).collect();
            remaining.sort_by(|a, b| self.members[*a].name.cmp(&self.members[*b].name));
            let start = remaining[0];
            let cycle = find_cycle(&self.members, start);
            bail!(
                "dependency cycle: {}",
                cycle
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" → ")
            );
        }

        Ok(out)
    }
}

/// Inputs to `build_workspace`. Mirror of `BuildPlan` minus the
/// per-member fields the orchestrator fills in.
pub struct WorkspaceBuildOptions<'a> {
    pub profile_kind: ProfileKind,
    pub clang: &'a Clang,
    pub plugin: Option<&'a Plugin>,
    /// `true` runs every member with `cust check` semantics
    /// (`-fsyntax-only`, no archive, no `compile_commands.json`).
    pub syntax_only: bool,
    /// `true` runs every member through the v0.3.2 test-build
    /// pipeline (V32D-2 through V32D-7): the lib half is
    /// compiled with `-DCUST_TEST_BUILD=1` into a fresh
    /// `target/<profile>/test/<crate>/` tree, the bin half is
    /// skipped (V32D-11), and the resulting test executable is
    /// at `target/<profile>/test/<crate>/<crate>`. Mutually
    /// exclusive with `syntax_only`.
    pub test_build: bool,
    /// If `Some(name)`, build only `name` and its transitive deps.
    /// `None` builds every member. Used by `cust build -p <member>`
    /// (Slice E).
    pub only: Option<&'a str>,
    /// v0.4.4 V44D-7: if `Some(bin)`, scope the build to the single
    /// binary named `bin` (its `CMake` target + transitive lib deps).
    /// Resolved against `only`'s member. `None` builds all bins.
    /// Ignored in `test_build` / `syntax_only` modes.
    pub bin: Option<&'a str>,
    /// V42D-13 / v0.4.3 roadmap: maximum parallel build jobs.
    /// Lowered to `cmake --build -j <N>`. `None` lets `Ninja`
    /// pick (`nproc`). Ignored in `syntax_only` mode — the
    /// surface pass is already cheap and bypasses `CMake`.
    pub jobs: Option<u32>,
}

/// One member's build outputs, indexed for callers that want to
/// know which archive belongs to which crate.
pub struct WorkspaceBuildOutputs {
    pub per_member: Vec<(String, BuildOutputs)>,
}

/// Build every workspace member in reverse-topological order
/// (dependencies first). After each producer build, refresh the
/// `target/<profile>/deps/<name>` symlink so downstream consumers
/// reach the producer's outputs at a stable path (V3D-5 option A).
///
/// v0.4.2 slice B (V42D-16) splits the orchestration into three
/// modes:
///
/// * **`test_build`** — keeps the v0.3.2/v0.4.0 per-member loop
///   driving `build::run` (each member builds its own test exe
///   via the in-driver `compile_tree` + `link_executable` path).
///   Slice C will move the test runner under `CMake`.
/// * **`syntax_only`** (`cust check`) — V42D-15: per member,
///   run the surface pass and concat the crate header, no
///   codegen, no `CMake`. Implemented via `build::run_phase1`.
/// * **build** — V42D-13: per member, run phase 1 + write the
///   `.rewrite/` tree, then a SINGLE `cmake -G Ninja` + `cmake
///   --build` invocation drives every member's codegen and link
///   under one `Ninja` graph.
pub fn build_workspace(
    ws: &Workspace,
    opts: &WorkspaceBuildOptions<'_>,
) -> Result<WorkspaceBuildOutputs> {
    let order = ws.build_order()?;

    // Filter for -p <member> scoping.
    let to_build: Vec<String> = if let Some(only) = opts.only {
        let target = ws.member(only).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown workspace member `{only}` — known: [{}]",
                ws.members
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let mut needed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        collect_transitive_deps(ws, target, &mut needed);
        order.into_iter().filter(|n| needed.contains(n)).collect()
    } else {
        order
    };

    let layout = TargetLayout::for_workspace(&ws.root, opts.profile_kind);
    layout.ensure_dirs()?;

    // Test-build mode: keep the v0.4.0 per-member loop. Slice C
    // will move this under CMake (V42D-14).
    if opts.test_build {
        return run_test_build_path(ws, &to_build, opts, &layout);
    }

    // Check mode: V42D-15 — per-member surface pass + crate
    // header concat, no codegen, no CMake.
    if opts.syntax_only {
        return run_check_path(ws, &to_build, opts, &layout);
    }

    // Build mode (V42D-16 + v0.4.5 V45D-10 + v0.4.6 V46D-3): the
    // driver no longer runs `run_phase1`, the lib/bin rewrite pass,
    // **or** the integration-test rewrite pass here — all are now
    // CMake custom commands (V45D-3/V45D-4 + V46D-3), produced
    // lazily by Ninja inside the single `cmake --build`. The
    // residual per-member driver work (V45D-14(b)) is: create the
    // `build/<crate>/` dir, refresh the dep-publish symlink, and
    // synthesise `BuildOutputs`. The prelude is materialised once
    // up front (the surface commands `-include` it).
    build::ensure_prelude(&layout).context("materialising prelude")?;
    let mut per_member: Vec<(String, BuildOutputs)> = Vec::with_capacity(ws.members.len());
    for m in &ws.members {
        let name = &m.name;
        // V45D-14(b): `run_phase1` used to create each member's
        // `build/<crate>/` dir; now that it's gone from the build
        // path, create it here so `refresh_dep_symlink` (which
        // points `deps/<crate>` at it) and the surface commands'
        // `<crate>.surface.c` scratch writes have a home before
        // `cmake --build` runs.
        std::fs::create_dir_all(layout.build_dir(name))
            .with_context(|| format!("creating build dir for member `{name}`"))?;
        with_plan(m, |plan| {
            refresh_dep_symlink(&layout, &m.name, &m.root)
                .with_context(|| format!("publishing dep view for `{name}`"))?;
            // Only report `per_member` entries for the
            // `-p`-scoped subset (matches v0.3 behaviour: cust
            // run / cust test query specific members; siblings
            // outside the scope shouldn't appear in `Finished`
            // lines).
            if to_build.iter().any(|n| n == name) {
                per_member.push((name.clone(), build::cmake_outputs_for(plan, &layout)));
            }
            Ok(())
        })?;
    }

    // Drive the CMake build for the whole workspace (V42D-13:
    // single CMakeLists, single `cmake --build`).
    crate::cmake_emit::emit_and_drive_cmake(
        ws,
        opts.profile_kind,
        opts.clang,
        opts.plugin,
        &crate::cmake_emit::DriveOptions {
            only: opts.only,
            bin: opts.bin,
            jobs: opts.jobs,
            test_build: false,
            check_build: false,
        },
    )
    .context("driving cmake build")?;

    crate::lock::write_lock(ws).context("writing Cust.lock")?;
    // Write the v0.1 `.cust-version` stamp so downstream tooling
    // (and the test suite) can detect which cust + clang built
    // the tree. Per V42D-12 this lives at `target/.cust-version`,
    // shared with the legacy compile_commands.json symlink.
    build::write_version_stamp(&layout.target_root.join(".cust-version"), opts.clang)
        .context("writing .cust-version stamp")?;

    Ok(WorkspaceBuildOutputs { per_member })
}

/// incremental-check (CHK-D-1/CHK-D-3/CHK-D-4): `cust check` is now
/// a CMake-owned, incremental, error-reporting pass — the reversal
/// of V42D-15. The driver no longer runs `run_phase1`; it
/// materialises the prelude, prepares each member's per-crate build
/// and `.check` dirs and dep symlink, then a single `cmake -G
/// Ninja` configure plus `cmake --build --target cust_check` (or
/// `cust_check_<member>` under `-p`) drives a per-module direct-clang
/// `-fsyntax-only` check in one Ninja graph. Each module's check is
/// a custom command whose `.checked` stamp and `DEPENDS` (the
/// `.rewrite` TU, plugin, build-mode fragments, dep headers) make
/// it incremental and restat-skippable. A type error fails the
/// check (CHK-D-1) — unlike the old tolerant surface pass.
///
/// `-p` on a member with no lib half has nothing to check (no
/// `cust_check_<member>` target exists); the drive is skipped so a
/// missing `--target` never falls back to building `all`.
fn run_check_path(
    ws: &Workspace,
    to_build: &[String],
    opts: &WorkspaceBuildOptions<'_>,
    layout: &TargetLayout,
) -> Result<WorkspaceBuildOutputs> {
    build::ensure_prelude(layout).context("materialising prelude")?;
    let mut per_member: Vec<(String, BuildOutputs)> = Vec::with_capacity(to_build.len());
    for m in &ws.members {
        let name = &m.name;
        std::fs::create_dir_all(layout.build_dir(name))
            .with_context(|| format!("creating build dir for member `{name}`"))?;
        // CHK-D-3: a `cmake -E touch` of the check stamp does not
        // create parent dirs, and check has no leaf to self-create
        // them — so the driver makes each lib member's
        // `.check/<crate>/` tree before the build runs.
        if m.kind.has_lib() {
            std::fs::create_dir_all(layout.check_dir(name))
                .with_context(|| format!("creating check dir for member `{name}`"))?;
        }
        refresh_dep_symlink(layout, &m.name, &m.root)
            .with_context(|| format!("publishing dep view for `{name}`"))?;
        // Report only the `-p`-scoped subset (matches build/run
        // shape — siblings outside scope stay silent).
        if to_build.iter().any(|n| n == name) {
            per_member.push((
                name.clone(),
                BuildOutputs {
                    objects: Vec::new(),
                    archive: None,
                    executables: Vec::new(),
                    test_executable: None,
                    integration_tests: Vec::new(),
                    compile_commands: layout.target_root.join("compile_commands.json"),
                },
            ));
        }
    }

    // CHK-D-4/CHK-D-10: `-p` on a lib-less member checks nothing —
    // skip the drive so `cmake --build` never falls back to `all`.
    // (Without `-p`, the umbrella `cust_check` covers every lib
    // member; if the workspace has no lib half at all there is also
    // nothing to check.)
    let nothing_to_check = opts.only.map_or_else(
        || !ws.members.iter().any(|m| m.kind.has_lib()),
        |only| ws.member(only).is_none_or(|m| !m.kind.has_lib()),
    );
    if !nothing_to_check {
        crate::cmake_emit::emit_and_drive_cmake(
            ws,
            opts.profile_kind,
            opts.clang,
            opts.plugin,
            &crate::cmake_emit::DriveOptions {
                only: opts.only,
                bin: None,
                jobs: opts.jobs,
                test_build: false,
                check_build: true,
            },
        )
        .context("driving cmake check")?;
    }

    // No lock write in check mode (matches v0.3.x behaviour).
    Ok(WorkspaceBuildOutputs { per_member })
}

/// V42D-14 + v0.4.6 V46D-5: test-build pipeline lifted fully onto
/// the `CMake` backend. The driver no longer runs any pre-pass —
/// it materialises the prelude, prepares each member's build dir +
/// dep symlink, then a single `cmake -G Ninja` configure + `cmake
/// --build --target <crate>__test …` drives every member's test
/// build in one `Ninja` graph (the `-DCUST_TEST_BUILD=1` define is
/// a per-target compile option, not a configure flag, so flipping
/// between `cust build` and `cust test` never reconfigures). Every
/// test artifact (sidecars, runner TUs, rewrites, fragments, crate
/// header) is a `cust internal …` custom command produced lazily
/// inside that build. Test isolation (per-crate cwd, output
/// capture) still applies at runtime — the executable just lives
/// under the V42D-14 `target/<profile>/test/<crate>/<crate>` path
/// the test runner already expects.
fn run_test_build_path(
    ws: &Workspace,
    to_build: &[String],
    opts: &WorkspaceBuildOptions<'_>,
    layout: &TargetLayout,
) -> Result<WorkspaceBuildOutputs> {
    // v0.4.6 V46D-5: the test-build path now mirrors the build/run
    // path exactly — emit + configure + build. Every generated test
    // artifact (unit + integration sidecars, runner TUs, the lib/bin
    // + integration rewrites, the surface fragments + crate header)
    // is a CMake custom command produced lazily by Ninja inside the
    // single `cmake --build`. The residual driver work is identical
    // to the build path (V45D-14(b)): materialise the prelude once,
    // create each member's `build/<crate>/` dir, refresh the
    // dep-publish symlink, and synthesise `BuildOutputs` (here with
    // the test exe paths plumbed in for the runner to spawn). No
    // driver-side surface pass remains on the test path — `cust
    // check` keeps `run_phase1` (V42D-15), `cust test` no longer
    // calls it.
    build::ensure_prelude(layout).context("materialising prelude")?;
    let mut per_member: Vec<(String, BuildOutputs)> = Vec::with_capacity(to_build.len());
    for m in &ws.members {
        let name = &m.name;
        std::fs::create_dir_all(layout.build_dir(name))
            .with_context(|| format!("creating build dir for member `{name}`"))?;
        with_plan(m, |plan| {
            refresh_dep_symlink(layout, &m.name, &m.root)
                .with_context(|| format!("publishing dep view for `{name}`"))?;
            // v0.4.6 V46D-2/V46D-3: the unit + integration runner
            // TUs are now produced by `cust internal test-runner`
            // CMake commands (their OUTPUTs are SOURCEs of the
            // `<crate>__test` / `<crate>__itest__<stem>` targets).
            // The driver only needs the exe *paths* for the runner
            // to spawn — `None`/empty for bin-only members (V32D-12).
            let crate_name = plan.manifest.package_name();
            let test_exe = plan
                .kind
                .has_lib()
                .then(|| layout.test_executable_path(crate_name));
            let itests: Vec<build::IntegrationTestOutput> = if plan.kind.has_lib() {
                plan.integration_tests
                    .iter()
                    .map(|it| build::IntegrationTestOutput {
                        stem: it.stem.clone(),
                        source_label: format!("tests/{}.c", it.stem),
                        exe: layout.integration_test_executable_path(crate_name, &it.stem),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // Only report `per_member` for members the caller
            // asked about — siblings outside `-p` scope are
            // built but stay silent (matches `cust build`
            // shape).
            if to_build.iter().any(|n| n == name) {
                let mut outputs = build::cmake_outputs_for(plan, layout);
                outputs.test_executable = test_exe;
                outputs.integration_tests = itests;
                per_member.push((name.clone(), outputs));
            }
            Ok(())
        })?;
    }

    // One CMake configure + build invocation drives every
    // member's `<crate>__test` target in one Ninja graph.
    crate::cmake_emit::emit_and_drive_cmake(
        ws,
        opts.profile_kind,
        opts.clang,
        opts.plugin,
        &crate::cmake_emit::DriveOptions {
            only: opts.only,
            bin: None,
            jobs: opts.jobs,
            test_build: true,
            check_build: false,
        },
    )
    .context("driving cmake test build")?;

    Ok(WorkspaceBuildOutputs { per_member })
}

/// Construct a per-member `BuildPlan` for the duration of `f`.
/// Since the incremental-check milestone the driver runs no
/// per-member surface/check pass, so `BuildPlan` shrank to the
/// `manifest` / `kind` / `integration_tests` the residual
/// `BuildOutputs` synthesis still reads.
fn with_plan<R>(m: &Member, f: impl FnOnce(&BuildPlan<'_>) -> Result<R>) -> Result<R> {
    let plan = BuildPlan {
        manifest: &m.manifest,
        kind: m.kind.clone(),
        integration_tests: &m.integration_tests,
    };
    f(&plan)
}

/// `target/<profile>/deps/<name>` → `target/<profile>/build/<name>/`.
///
/// Symlink is recreated on every producer build so the new-member
/// case (no prior symlink) and the moved-member case (stale
/// symlink) are both handled.
fn refresh_dep_symlink(layout: &TargetLayout, dep_name: &str, _producer_root: &Path) -> Result<()> {
    let dep_dir = layout.dep_dir(dep_name);
    let build_dir = layout.build_dir(dep_name);
    if !build_dir.is_dir() {
        bail!(
            "internal: build dir `{}` does not exist after building `{dep_name}`",
            build_dir.display()
        );
    }
    if let Some(parent) = dep_dir.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }
    // Wipe whatever's there. Could be a stale symlink, a real dir
    // (e.g. from a prior v0.4 cache-style layout), or nothing.
    match fs::symlink_metadata(&dep_dir) {
        Ok(meta) => {
            if meta.is_dir() && !meta.is_symlink() {
                fs::remove_dir_all(&dep_dir)
                    .with_context(|| format!("removing stale `{}`", dep_dir.display()))?;
            } else {
                fs::remove_file(&dep_dir)
                    .with_context(|| format!("removing stale `{}`", dep_dir.display()))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("stat `{}`", dep_dir.display()));
        }
    }
    std::os::unix::fs::symlink(&build_dir, &dep_dir).with_context(|| {
        format!(
            "symlink `{}` → `{}`",
            dep_dir.display(),
            build_dir.display()
        )
    })?;
    Ok(())
}

/// Recursive helper for `-p <member>` scoping: collect the named
/// member plus everything in its transitive `deps` closure.
fn collect_transitive_deps(
    ws: &Workspace,
    m: &Member,
    out: &mut std::collections::BTreeSet<String>,
) {
    if !out.insert(m.name.clone()) {
        return;
    }
    for d in &m.deps {
        if let Some(dep) = ws.member(d) {
            collect_transitive_deps(ws, dep, out);
        }
    }
}

/// Walk forward from `start` following the first dep edge until we
/// hit a node we've already seen, then return the cycle slice
/// rotated to begin at the alphabetically-first name in the
/// cycle. The returned vec includes the start name twice (closes
/// the loop visually, e.g. `[\"a\", \"b\", \"a\"]`).
fn find_cycle(members: &[Member], start: usize) -> Vec<String> {
    let name_to_idx: BTreeMap<&str, usize> = members
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.as_str(), i))
        .collect();

    let mut path: Vec<usize> = Vec::new();
    let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cur = start;
    loop {
        if let Some(&first_occurrence) = seen.get(&cur) {
            // cycle is path[first_occurrence..] then back to cur.
            let mut cyc: Vec<String> = path[first_occurrence..]
                .iter()
                .map(|&i| members[i].name.clone())
                .collect();
            // Rotate to start at the alphabetically-first name in
            // the cycle.
            if let Some(min_pos) = (0..cyc.len()).min_by(|&a, &b| cyc[a].cmp(&cyc[b])) {
                cyc.rotate_left(min_pos);
            }
            let first = cyc[0].clone();
            cyc.push(first); // close the loop visually
            return cyc;
        }
        seen.insert(cur, path.len());
        path.push(cur);
        let next_name = members[cur].deps.first().cloned();
        let Some(next_name) = next_name else {
            // Dead end without closing the loop \u2014 shouldn't happen
            // when called on a member with in_degree > 0, but fall
            // back gracefully.
            return vec![members[start].name.clone(), members[start].name.clone()];
        };
        cur = name_to_idx[next_name.as_str()];
    }
}

/// Re-read a manifest from disk. Used when we need a fresh owned
/// `Manifest` for a `Member` even though we already have one
/// borrowed elsewhere (the parser does no I/O of its own).
fn clone_manifest(path: &Path) -> Result<Manifest> {
    Manifest::load(path)
}

/// v0.4.3 V43D-1 + V43D-8: discover integration tests under
/// `<member_root>/tests/`.
///
/// Returns the top-level `*.c` files only — subdirectories
/// (including any future `tests/common/`) are silently ignored
/// (V43D-1 no-recursion + V43D-2 helper-sharing deferred), and
/// non-`.c` files are skipped. The result is sorted by stem so
/// the `Running tests/<file>.c` banner order is deterministic
/// regardless of filesystem readdir order.
///
/// A missing `tests/` directory (or an empty one) yields an empty
/// vec — same as no `tests/` at all (V43D-12).
///
/// Config-time errors (V43D-8):
///
/// * a stem that doesn't match `[A-Za-z][A-Za-z0-9_-]*`;
/// * a stem equal to `crate_name` (would collide with the
///   unit-test exe at `test/<crate>/<crate>`, V43D-5).
fn discover_integration_tests(
    member_root: &Path,
    crate_name: &str,
) -> Result<Vec<IntegrationTest>> {
    let tests_dir = member_root.join("tests");
    let entries = match fs::read_dir(&tests_dir) {
        Ok(rd) => rd,
        // No tests/ dir at all ⇒ no integration tests (V43D-12).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("reading `{}`", tests_dir.display()))
        }
    };

    let mut tests: Vec<IntegrationTest> = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in `{}`", tests_dir.display()))?;
        let path = entry.path();
        // Top level only: `is_file()` follows symlinks but returns
        // false for directories, so subdirectories (V43D-1
        // no-recursion) and anything non-regular are skipped.
        if !path.is_file() {
            continue;
        }
        // V43D-1: only `.c` files; non-`.c` files are ignored.
        if path.extension().and_then(|e| e.to_str()) != Some("c") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 filename `{}`", path.display()))?
            .to_string();

        validate_integration_stem(&stem)?;
        if stem == crate_name {
            bail!(
                "tests/{stem}.c: integration-test stem '{stem}' collides with \
                 the unit-test executable; rename the file"
            );
        }
        tests.push(IntegrationTest { stem, source: path });
    }
    // V43D-1: deterministic alphabetical-by-stem run order.
    tests.sort_by(|a, b| a.stem.cmp(&b.stem));
    Ok(tests)
}

/// v0.4.3 V43D-8: an integration-test file stem must match
/// `[A-Za-z][A-Za-z0-9_-]*` so it's safe as a `CMake` target infix
/// (`<crate>__itest__<stem>`) and an on-disk filename. Stricter
/// than `manifest::validate_package_name`, which also permits a
/// leading digit / `_` / `-`.
fn validate_integration_stem(stem: &str) -> Result<()> {
    let mut chars = stem.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        bail!(
            "tests/{stem}.c: stem '{stem}' must match [A-Za-z][A-Za-z0-9_-]* \
             (used as the CMake target name + on-disk filename)"
        );
    }
    Ok(())
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

    #[test]
    fn path_dep_resolves_to_sibling_member() {
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
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\nutil = { path = \"../util\" }\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");
        write(
            root,
            "util/Cust.toml",
            "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        );
        write(root, "util/src/lib.c", "int u = 1;\n");

        let ws = Workspace::discover(root).unwrap();
        let app = ws.member("app").unwrap();
        assert_eq!(app.deps, vec!["util".to_string()]);
        let util = ws.member("util").unwrap();
        assert!(util.deps.is_empty());
    }

    #[test]
    fn path_dep_outside_workspace_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        // Workspace at outer/proj/, but app deps on outer/util/.
        write(
            outer,
            "proj/Cust.toml",
            "[workspace]\nmembers = [\"app\"]\n",
        );
        write(
            outer,
            "proj/app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\nutil = { path = \"../../util\" }\n",
        );
        write(outer, "proj/app/src/lib.c", "int a = 1;\n");
        write(
            outer,
            "util/Cust.toml",
            "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        );
        write(outer, "util/src/lib.c", "int u = 1;\n");

        let e = format!(
            "{:#}",
            Workspace::discover(&outer.join("proj")).unwrap_err()
        );
        assert!(e.contains("not a workspace member"), "{e}");
        assert!(e.contains("util"), "{e}");
    }

    #[test]
    fn path_dep_to_non_member_inside_workspace_is_error() {
        // helper/ exists under the workspace root but isn't listed
        // as a member.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"app\"]\n");
        write(
            root,
            "app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\nhelper = { path = \"../helper\" }\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");
        write(
            root,
            "helper/Cust.toml",
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\n",
        );
        write(root, "helper/src/lib.c", "int h = 1;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("not a workspace member"), "{e}");
        assert!(e.contains("helper"), "{e}");
    }

    #[test]
    fn path_dep_name_mismatch_is_error() {
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
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\naliased = { path = \"../util\" }\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");
        write(
            root,
            "util/Cust.toml",
            "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
        );
        write(root, "util/src/lib.c", "int u = 1;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("name mismatch"), "{e}");
        assert!(e.contains("rename the dependency key"), "{e}");
    }

    #[test]
    fn path_dep_self_loop_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"loner\"]\n");
        write(
            root,
            "loner/Cust.toml",
            "[package]\nname = \"loner\"\nversion = \"0.1.0\"\n\
             [dependencies]\nloner = { path = \".\" }\n",
        );
        write(root, "loner/src/lib.c", "int x = 1;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("depends on itself"), "{e}");
    }

    #[test]
    fn path_dep_missing_on_disk_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "Cust.toml", "[workspace]\nmembers = [\"app\"]\n");
        write(
            root,
            "app/Cust.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
             [dependencies]\nutil = { path = \"../util\" }\n",
        );
        write(root, "app/src/lib.c", "int a = 1;\n");
        // `../util` does not exist.

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("does not exist"), "{e}");
        assert!(e.contains("util"), "{e}");
    }

    // ---- v0.4.3 integration-test discovery (V43D-1, V43D-8) ----

    /// Helper: build a single-crate workspace named `crate_name`
    /// with the given `tests/` files written, then return the
    /// discovered member's `integration_tests`.
    fn discover_itests(crate_name: &str, files: &[(&str, &str)]) -> Vec<IntegrationTest> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            &format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\n"),
        );
        write(root, "src/lib.c", "int x = 1;\n");
        for (rel, body) in files {
            write(root, rel, body);
        }
        let ws = Workspace::discover(root).unwrap();
        ws.members.into_iter().next().unwrap().integration_tests
    }

    #[test]
    fn no_tests_dir_yields_no_integration_tests() {
        let itests = discover_itests("solo", &[]);
        assert!(itests.is_empty());
    }

    #[test]
    fn empty_tests_dir_yields_no_integration_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.c", "int x = 1;\n");
        // Create an empty tests/ directory (no .c files).
        std::fs::create_dir_all(root.join("tests")).unwrap();

        let ws = Workspace::discover(root).unwrap();
        assert!(ws.members[0].integration_tests.is_empty());
    }

    #[test]
    fn top_level_c_files_are_discovered_sorted_by_stem() {
        let itests = discover_itests(
            "solo",
            &[
                ("tests/zeta.c", "int z;\n"),
                ("tests/alpha.c", "int a;\n"),
                ("tests/middle.c", "int m;\n"),
            ],
        );
        let stems: Vec<&str> = itests.iter().map(|t| t.stem.as_str()).collect();
        assert_eq!(stems, vec!["alpha", "middle", "zeta"]);
        // Sources are absolute and point at the right files.
        assert!(itests[0].source.ends_with("tests/alpha.c"));
        assert!(itests[0].source.is_absolute());
    }

    #[test]
    fn non_c_files_are_ignored() {
        let itests = discover_itests(
            "solo",
            &[
                ("tests/basic.c", "int b;\n"),
                ("tests/README.md", "notes\n"),
                ("tests/data.txt", "x\n"),
                ("tests/header.h", "int h;\n"),
            ],
        );
        let stems: Vec<&str> = itests.iter().map(|t| t.stem.as_str()).collect();
        assert_eq!(stems, vec!["basic"]);
    }

    #[test]
    fn subdirectories_under_tests_are_ignored() {
        // V43D-1 no-recursion + V43D-2 helper-sharing deferred:
        // a tests/common/mod.c (and any other subdir .c) is
        // silently skipped, not discovered as an exe.
        let itests = discover_itests(
            "solo",
            &[
                ("tests/basic.c", "int b;\n"),
                ("tests/common/mod.c", "int helper;\n"),
                ("tests/sub/deep.c", "int d;\n"),
            ],
        );
        let stems: Vec<&str> = itests.iter().map(|t| t.stem.as_str()).collect();
        assert_eq!(stems, vec!["basic"]);
    }

    #[test]
    fn invalid_stem_leading_digit_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.c", "int x = 1;\n");
        write(root, "tests/1bad.c", "int b;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("tests/1bad.c"), "{e}");
        assert!(e.contains("[A-Za-z][A-Za-z0-9_-]*"), "{e}");
    }

    #[test]
    fn invalid_stem_bad_char_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.c", "int x = 1;\n");
        write(root, "tests/has.dot.c", "int b;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("tests/has.dot.c"), "{e}");
        assert!(e.contains("must match"), "{e}");
    }

    #[test]
    fn stem_colliding_with_crate_name_is_error() {
        // V43D-8: tests/<crate>.c would collide with the
        // unit-test exe path test/<crate>/<crate>.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "Cust.toml",
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.c", "int x = 1;\n");
        write(root, "tests/solo.c", "int b;\n");

        let e = format!("{:#}", Workspace::discover(root).unwrap_err());
        assert!(e.contains("collides with"), "{e}");
        assert!(e.contains("unit-test executable"), "{e}");
    }

    #[test]
    fn valid_stems_with_hyphen_and_underscore_are_accepted() {
        let itests = discover_itests(
            "solo",
            &[
                ("tests/alloc_pressure.c", "int a;\n"),
                ("tests/round-trip.c", "int r;\n"),
            ],
        );
        let stems: Vec<&str> = itests.iter().map(|t| t.stem.as_str()).collect();
        assert_eq!(stems, vec!["alloc_pressure", "round-trip"]);
    }
}
