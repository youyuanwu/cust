//! `Cust.toml` schema and discovery.
//!
//! v0.1 only honours `[package]` (name + version), `[lib]` (path,
//! crate-type), `[clang]` (std, extra-cflags), and `[profile.*]`. The
//! parser is **strict**: any unknown top-level table or field is an
//! error. Known-but-unimplemented sections (`[features]`,
//! `[build-dependencies]`, etc.) are accepted only when empty/absent.
//!
//! v0.3 ([docs/design/v0.3.0.md]) adds `[workspace]` and accepts a
//! strictly-whitelisted shape of `[dependencies]` entries (path
//! deps only); see `validate_v0_3` below for the exact rules.
//!
//! v0.3.1 ([docs/design/v0.3.1.md]) adds `[[bin]]` (single-entry in
//! v0.3.1; multi-bin via `src/bin/*.c` and `[[bin]]` arrays is
//! v0.4+) and `Manifest::resolve_kind` for filesystem-driven crate
//! kind inference (lib / bin / lib+bin).
//!
//! See `docs/design/cust-design.md` §3 and §17 for the canonical
//! schema and current scope.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Name of the manifest file. Walked up from cwd.
pub const MANIFEST_FILE: &str = "Cust.toml";

/// Parsed `Cust.toml`. Only fields the driver uses are exposed
/// as typed accessors; the rest live in private fields to preserve
/// the strict-mode unknown-field check without forcing the driver
/// to model every future field.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `[package]` is optional in v0.3+ to allow virtual workspace
    /// roots (a manifest with only `[workspace]`). For any
    /// non-virtual manifest the loader still requires it.
    #[serde(default)]
    pub package: Option<Package>,

    #[serde(default)]
    pub lib: Option<Lib>,

    /// v0.3.1: `[[bin]]` array. Accepted as an array shape for
    /// forward-compat with the multi-bin v0.4 schema, but
    /// `validate` rejects `len > 1` in v0.3.1 (V31D-3).
    #[serde(default)]
    pub bin: Vec<Bin>,

    #[serde(default)]
    pub clang: Clang,

    #[serde(default)]
    pub profile: Profiles,

    /// v0.3: `[workspace]` table. Validated by `validate_v0_3`.
    #[serde(default)]
    pub workspace: Option<WorkspaceTable>,

    // Known-but-not-yet-supported sections. Accepted only when
    // empty/absent. Populated ones are rejected by `Manifest::load`.
    #[serde(default)]
    pub features: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, toml::Value>,
    #[serde(default, rename = "build-dependencies")]
    pub build_dependencies: BTreeMap<String, toml::Value>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, toml::Value>,
}

/// `[workspace]` table contents. v0.3 freezes `members` only;
/// every other Cargo-style key (`dependencies`, `package`,
/// `default-members`, `resolver`, …) is rejected at parse time
/// with a `v0.4+` pointer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTable {
    /// Literal relative directory paths under the workspace root.
    /// Globs are rejected (V3D-2).
    #[serde(default)]
    pub members: Vec<String>,
}

/// One entry from `[dependencies]`, typed after validation.
///
/// v0.3 only carries path dependencies (V3D-3). `features`,
/// `default-features`, and `optional` are parsed and surfaced
/// for the v0.4 features-graph work; the v0.3 build pipeline
/// ignores them (no feature evaluation yet).
#[derive(Debug, Clone)]
pub struct DepSpec {
    /// The key under `[dependencies]` (e.g. `util` in
    /// `util = { path = "../util" }`). This is the name the
    /// consumer reaches the dep by — `#cust use <name>;`.
    pub name: String,
    /// Relative path string, exactly as written in the manifest.
    /// Resolved by `crate::workspace` against the consumer's
    /// directory.
    pub path: String,
    /// Requested features (v0.4 will evaluate these).
    #[allow(dead_code)]
    pub features: Vec<String>,
    /// `false` if the manifest set `default-features = false`.
    /// Defaults to `true`.
    #[allow(dead_code)]
    pub default_features: bool,
    /// `optional = true` (v0.4 features-graph wires this).
    #[allow(dead_code)]
    pub optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // metadata fields parsed for strict-mode rejection; surfaced later
pub struct Package {
    pub name: String,
    pub version: String,

    // Accepted but unused in v0.1 (see §17 — locked so future
    // edition semantics can't retroactively reject manifests).
    #[serde(default)]
    pub edition: Option<String>,
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lib {
    /// Defaults to `src/lib.c` when omitted.
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// v0.1 accepts only `["staticlib"]` (the default).
    #[serde(default, rename = "crate-type")]
    pub crate_type: Option<Vec<String>>,
}

/// `[[bin]]` table entry. v0.3.1 accepts a single entry only;
/// multi-bin via `src/bin/*.c` and the multi-entry `[[bin]]`
/// array is deferred to v0.4 (V31D-3 in v0.3.1.md).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bin {
    /// Defaults to `src/main.c` when omitted.
    #[serde(default)]
    #[allow(dead_code)] // consumed by Slice B via resolve_kind
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // extra_ldflags / visibility parsed for forward compat; consumed in v0.2+
pub struct Clang {
    /// `-std=` value. v0.1 default chosen at build time (c23 if
    /// available else c17) — `None` here means "let the driver pick".
    #[serde(default)]
    pub std: Option<String>,
    #[serde(default, rename = "extra-cflags")]
    pub extra_cflags: Vec<String>,
    #[serde(default, rename = "extra-ldflags")]
    pub extra_ldflags: Vec<String>,
    /// Accepted but unused in v0.1 (the prelude pins visibility
    /// behaviour for now).
    #[serde(default)]
    pub visibility: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    #[serde(default)]
    pub dev: Option<Profile>,
    #[serde(default)]
    pub release: Option<Profile>,
}

/// Profile overrides as parsed from `Cust.toml`. None of the fields
/// are required; `crate::profile::Profile` is what the build pipeline
/// actually consumes (with defaults baked in).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // lto / codegen-units / panic parsed for forward compat
pub struct Profile {
    #[serde(default, rename = "opt-level")]
    pub opt_level: Option<toml::Value>,
    #[serde(default)]
    pub debug: Option<String>,
    #[serde(default)]
    pub sanitize: Option<Vec<String>>,
    #[serde(default, rename = "extra-cflags")]
    pub extra_cflags: Option<Vec<String>>,

    // Accepted but ignored in v0.1.
    #[serde(default)]
    pub lto: Option<toml::Value>,
    #[serde(default, rename = "codegen-units")]
    pub codegen_units: Option<toml::Value>,
    #[serde(default)]
    pub panic: Option<String>,
}

/// Where a manifest was found. `dir` is the directory containing
/// `Cust.toml` (the crate root).
#[derive(Debug, Clone)]
pub struct ManifestLocation {
    pub path: PathBuf,
    pub dir: PathBuf,
}

impl Manifest {
    /// Discover `Cust.toml` by walking up from `start_dir` to the
    /// filesystem root. Same algorithm cargo uses.
    pub fn discover(start_dir: &Path) -> Result<ManifestLocation> {
        let mut cur = Some(start_dir);
        while let Some(dir) = cur {
            let candidate = dir.join(MANIFEST_FILE);
            if candidate.is_file() {
                return Ok(ManifestLocation {
                    path: candidate,
                    dir: dir.to_path_buf(),
                });
            }
            cur = dir.parent();
        }
        bail!(
            "could not find `{}` in `{}` or any parent directory",
            MANIFEST_FILE,
            start_dir.display()
        );
    }

    /// Read + parse + validate a manifest from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading `{}`", path.display()))?;
        let manifest: Self =
            toml::from_str(&text).with_context(|| format!("parsing `{}`", path.display()))?;
        manifest.validate(path)?;
        Ok(manifest)
    }

    /// `true` when this manifest declares a `[workspace]` table.
    /// A workspace-only manifest (no `[package]`) is *virtual* in
    /// the sense used by [docs/design/v0.3.0.md](../../../docs/design/v0.3.0.md).
    pub const fn declares_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    /// `true` when this manifest declares a `[package]` table
    /// (i.e. it is itself a buildable crate, not a virtual root).
    pub const fn is_package(&self) -> bool {
        self.package.is_some()
    }

    /// Return the `[package]` table, erroring with a clear message
    /// when this is a virtual workspace root that has none.
    pub fn require_package(&self, path: &Path) -> Result<&Package> {
        self.package.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "`{}` declares no [package]; a buildable crate must have one",
                path.display()
            )
        })
    }

    /// Package name. Panics if the manifest is a virtual workspace
    /// root; callers reaching the build pipeline must have already
    /// gone through `require_package` (the CLI layer enforces this).
    pub fn package_name(&self) -> &str {
        &self
            .package
            .as_ref()
            .expect("build pipeline invoked on virtual workspace; cli::locate must filter")
            .name
    }

    /// Return the `[dependencies]` table as a list of typed
    /// `DepSpec`s. The TOML shape was already validated by
    /// `validate_dep_spec` during `load`, so the unwrap-ish
    /// extraction below cannot fail in practice — any error
    /// indicates a contract bug between `validate_dep_spec` and
    /// this method, surfaced via `expect` rather than swallowed.
    ///
    /// The `path` field is **as written in the manifest** — a
    /// relative string. Callers (typically `crate::workspace`)
    /// resolve it against the manifest's directory.
    pub fn dep_specs(&self) -> Vec<DepSpec> {
        self.dependencies
            .iter()
            .map(|(name, value)| {
                let table = value
                    .as_table()
                    .expect("validate_dep_spec ensures every dep is a table");
                let path = table
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .expect("validate_dep_spec ensures `path` is a string")
                    .to_string();
                let features = table
                    .get("features")
                    .and_then(toml::Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .map(|v| v.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let default_features = table
                    .get("default-features")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true);
                let optional = table
                    .get("optional")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                DepSpec {
                    name: name.clone(),
                    path,
                    features,
                    default_features,
                    optional,
                }
            })
            .collect()
    }

    fn validate(&self, path: &Path) -> Result<()> {
        // Strict-but-pointable rejections for the schema slices
        // that still aren't wired up.
        let unsupported = |what: &str, when: &str| -> anyhow::Error {
            anyhow::anyhow!(
                "`{what}` in `{}` is not yet supported in cust ({when})",
                path.display()
            )
        };

        if !self.features.is_empty() {
            return Err(unsupported("[features]", "v0.4+"));
        }
        if !self.build_dependencies.is_empty() {
            return Err(unsupported("[build-dependencies]", "v0.4+"));
        }
        if !self.dev_dependencies.is_empty() {
            return Err(unsupported("[dev-dependencies]", "v0.4+"));
        }

        // v0.3 — `[dependencies]` entries are validated for shape
        // here; the *graph* (workspace membership + cycle check)
        // is enforced later by `crate::workspace`. We only reject
        // shapes that v0.3 cannot represent at all.
        for (name, value) in &self.dependencies {
            validate_dep_spec(name, value)
                .with_context(|| format!("in `{}` [dependencies] `{name}`", path.display()))?;
        }

        // A manifest must have at least one of [package] or
        // [workspace]; otherwise it's not a buildable crate and
        // not a workspace root — just a stray TOML file.
        if self.package.is_none() && self.workspace.is_none() {
            bail!(
                "`{}` has neither [package] nor [workspace]; \
                 add a [package] section to make it a buildable crate",
                path.display()
            );
        }

        // [lib] settings only apply when the manifest is a package.
        // We allow [lib] in package-bearing manifests; in virtual
        // roots its presence is a user error (probably copy-paste).
        if let Some(lib) = &self.lib {
            if self.package.is_none() {
                bail!(
                    "`{}` has [lib] but no [package]; [lib] only makes \
                     sense in a buildable crate",
                    path.display()
                );
            }
            if let Some(ct) = &lib.crate_type {
                // v0.3.1 still gates non-staticlib lib outputs:
                // bin is now a [[bin]] table (v0.3.1), cdylib is
                // still v0.4+. [lib] crate-type is a *library*
                // setting and must be staticlib if present.
                if ct.iter().any(|s| s != "staticlib") {
                    bail!(
                        "`[lib] crate-type` in `{}` only supports \
                         [\"staticlib\"] in cust v0.3.1 (got {ct:?}); \
                         binary crates use `[[bin]]`; cdylib is v0.4+",
                        path.display()
                    );
                }
            }
        }

        // [[bin]] settings only apply when the manifest is a
        // package; v0.3.1 also caps the array at length 1
        // (V31D-3 in v0.3.1.md).
        if !self.bin.is_empty() {
            if self.package.is_none() {
                bail!(
                    "`{}` has [[bin]] but no [package]; [[bin]] only \
                     makes sense in a buildable crate",
                    path.display()
                );
            }
            if self.bin.len() > 1 {
                bail!(
                    "`{}` has {} [[bin]] entries; cust v0.3.1 supports \
                     exactly one binary target per crate \
                     (multi-bin via `src/bin/*.c` is v0.4+)",
                    path.display(),
                    self.bin.len()
                );
            }
        }

        // Package name sanity.
        if let Some(pkg) = &self.package {
            if let Err(reason) = validate_package_name(&pkg.name) {
                bail!(
                    "invalid `[package] name = {:?}` in `{}` — {reason}",
                    pkg.name,
                    path.display()
                );
            }
        }

        // [workspace] member-list sanity: literal paths only
        // (V3D-2), no escaping above the workspace root, no
        // duplicates.
        if let Some(ws) = &self.workspace {
            let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for m in &ws.members {
                if m.chars().any(|c| matches!(c, '*' | '?' | '[')) {
                    bail!(
                        "globs in `[workspace] members` are v0.4+ \
                         (list directories literally): `{m}` in `{}`",
                        path.display()
                    );
                }
                if m.is_empty() {
                    bail!(
                        "empty entry in `[workspace] members` of `{}`",
                        path.display()
                    );
                }
                // Reject `/foo` (absolute) and any `..` segment
                // — members must stay inside the workspace root.
                let p = Path::new(m);
                if p.is_absolute() {
                    bail!(
                        "absolute path `{m}` in `[workspace] members` \
                         of `{}` — members must be relative to the \
                         workspace root",
                        path.display()
                    );
                }
                if p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    bail!(
                        "`..` in `[workspace] members` entry `{m}` of `{}` \
                         — members must stay inside the workspace root",
                        path.display()
                    );
                }
                if !seen.insert(m.as_str()) {
                    bail!(
                        "duplicate entry `{m}` in `[workspace] members` of `{}`",
                        path.display()
                    );
                }
            }
        }

        Ok(())
    }

    /// Resolved path to the lib's root TU when this crate has
    /// a library component. Honours `[lib] path` override.
    /// Returns `crate_root.join("src/lib.c")` by default.
    ///
    /// Used to be the single source of truth for the build
    /// pipeline's root; v0.3.1 superseded that with
    /// `Manifest::resolve_kind` (which also handles bin and
    /// lib+bin shapes). Retained as a small helper for callers
    /// that *know* they want the lib-source-or-default path
    /// without rejecting based on disk presence.
    #[allow(dead_code)] // retained for symmetry with resolve_kind; expected to be consumed by v0.4 cust-test
    pub fn lib_source(&self, crate_root: &Path) -> PathBuf {
        let rel = self
            .lib
            .as_ref()
            .and_then(|l| l.path.as_deref())
            .unwrap_or_else(|| Path::new("src/lib.c"));
        crate_root.join(rel)
    }

    /// Resolve which artifact(s) this crate produces based on
    /// the manifest plus the on-disk presence of `src/lib.c` and
    /// `src/main.c`. See [docs/design/v0.3.1.md](../../../docs/design/v0.3.1.md)
    /// V31D-1 for the full decision table.
    ///
    /// Auto-inference rules:
    ///
    /// | `src/lib.c` | `src/main.c` | `[lib]` | `[[bin]]` | Result |
    /// |---|---|---|---|---|
    /// | present | absent  | any | absent | lib-only |
    /// | absent  | present | absent | absent | bin-only |
    /// | present | present | any | absent | lib+bin |
    /// | any | any | any | present | path determined by `[[bin]] path` |
    /// | absent  | absent  | absent | absent | error |
    ///
    /// An explicit `[lib]` or `[[bin]]` table makes the
    /// corresponding component *required*: a missing file at the
    /// declared (or default) path is an error rather than a
    /// silent omission.
    #[allow(dead_code)] // consumed by Slice B (build pipeline kind dispatch)
    pub fn resolve_kind(&self, crate_root: &Path) -> Result<CrateKind> {
        let lib_default = Path::new("src/lib.c");
        let bin_default = Path::new("src/main.c");

        let lib_rel = self.lib.as_ref().and_then(|l| l.path.as_deref());
        let bin_rel = self.bin.first().and_then(|b| b.path.as_deref());

        let lib_path = crate_root.join(lib_rel.unwrap_or(lib_default));
        let bin_path = crate_root.join(bin_rel.unwrap_or(bin_default));

        // Presence of the table itself (not just an explicit
        // `path` field) is the "user wants this component" signal
        // — mirrors how Cargo treats an empty `[lib]` / `[[bin]]`.
        let lib_explicit = self.lib.is_some();
        let bin_explicit = !self.bin.is_empty();

        let lib_exists = lib_path.is_file();
        let bin_exists = bin_path.is_file();

        if lib_explicit && !lib_exists {
            bail!(
                "library source `{}` not found (configured via `[lib]` in `Cust.toml`)",
                lib_path.display()
            );
        }
        if bin_explicit && !bin_exists {
            bail!(
                "binary source `{}` not found (configured via `[[bin]]` in `Cust.toml`)",
                bin_path.display()
            );
        }

        let use_lib = lib_explicit || lib_exists;
        let use_bin = bin_explicit || bin_exists;

        match (use_lib, use_bin) {
            (true, true) => Ok(CrateKind::LibAndBin {
                lib_source: lib_path,
                bin_source: bin_path,
            }),
            (true, false) => Ok(CrateKind::Lib {
                lib_source: lib_path,
            }),
            (false, true) => Ok(CrateKind::Bin {
                bin_source: bin_path,
            }),
            (false, false) => bail!(
                "no library or binary source found in `{}`: neither \
                 `src/lib.c` (lib) nor `src/main.c` (bin) is present \
                 — add one, or set `[lib].path` / `[[bin]].path` in \
                 `Cust.toml`",
                crate_root.display()
            ),
        }
    }
}

/// What a crate produces, after `Manifest::resolve_kind`
/// consults the filesystem. v0.3.1's three shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // variants constructed by Slice B onward
pub enum CrateKind {
    /// Library only — produces `lib<name>.a` and a crate header.
    Lib { lib_source: PathBuf },
    /// Binary only — produces an executable; no archive published
    /// for downstream consumption.
    Bin { bin_source: PathBuf },
    /// Both — lib built first, bin links against it. Crate
    /// header is still published by the lib half.
    LibAndBin {
        lib_source: PathBuf,
        bin_source: PathBuf,
    },
}

impl CrateKind {
    /// `true` when this crate has a library component
    /// (`Lib` or `LibAndBin`).
    #[allow(dead_code)] // consumed by Slice B/C
    pub const fn has_lib(&self) -> bool {
        matches!(self, Self::Lib { .. } | Self::LibAndBin { .. })
    }

    /// `true` when this crate has a binary component
    /// (`Bin` or `LibAndBin`).
    #[allow(dead_code)] // consumed by Slice B/C
    pub const fn has_bin(&self) -> bool {
        matches!(self, Self::Bin { .. } | Self::LibAndBin { .. })
    }

    /// Library source path, if any.
    #[allow(dead_code)] // consumed by Slice B/C
    pub fn lib_source(&self) -> Option<&Path> {
        match self {
            Self::Lib { lib_source } | Self::LibAndBin { lib_source, .. } => Some(lib_source),
            Self::Bin { .. } => None,
        }
    }

    /// Binary source path, if any.
    #[allow(dead_code)] // consumed by Slice B/C
    pub fn bin_source(&self) -> Option<&Path> {
        match self {
            Self::Bin { bin_source } | Self::LibAndBin { bin_source, .. } => Some(bin_source),
            Self::Lib { .. } => None,
        }
    }
}

/// Validate one `[dependencies]` entry's shape. Accepted v0.3
/// shapes (V3D-3 in `docs/design/v0.3.0.md`):
///
/// * `dep = { path = "…" }`
/// * `dep = { path = "…", features = […] }`
/// * `dep = { path = "…", default-features = false }`
/// * `dep = { path = "…", optional = true }`
/// * any combination of the above.
///
/// Everything else is rejected with a `v0.4+` pointer. In
/// particular: bare semver string `"1.0"`, `version`, `git`,
/// `tag`, `branch`, `rev` are all out of scope for v0.3.
fn validate_dep_spec(name: &str, value: &toml::Value) -> Result<()> {
    // Bare-string shorthand (`dep = "1.0"`) is a version spec,
    // which v0.3 doesn't resolve.
    if value.is_str() {
        bail!(
            "`{name} = \"…\"` is a version spec; version specs are \
             v0.4+ (use `{{ path = \"…\" }}` instead)"
        );
    }
    let Some(table) = value.as_table() else {
        bail!("`{name}` must be a string or a table");
    };

    // path is required — v0.3 has no other dep source.
    let Some(path_val) = table.get("path") else {
        bail!(
            "`{name}` is missing `path = \"…\"`; v0.3 only supports \
             path dependencies"
        );
    };
    if !path_val.is_str() {
        bail!("`{name}.path` must be a string");
    }

    // Reject any out-of-scope keys with an explicit v0.4 pointer.
    for key in table.keys() {
        match key.as_str() {
            "path" | "features" | "default-features" | "optional" => {}
            "version" | "git" | "tag" | "branch" | "rev" => {
                bail!(
                    "`{name}.{key}` is v0.4+; v0.3 supports path \
                     deps only"
                );
            }
            "workspace" => {
                bail!(
                    "`{name}.workspace = true` (workspace dependency \
                     inheritance) is v0.4+"
                );
            }
            other => {
                bail!("unknown key `{name}.{other}` in [dependencies]");
            }
        }
    }

    // Shape checks for the optional keys.
    if let Some(v) = table.get("features") {
        if !v.is_array() || !v.as_array().unwrap().iter().all(toml::Value::is_str) {
            bail!("`{name}.features` must be an array of strings");
        }
    }
    if let Some(v) = table.get("default-features") {
        if !v.is_bool() {
            bail!("`{name}.default-features` must be a boolean");
        }
    }
    if let Some(v) = table.get("optional") {
        if !v.is_bool() {
            bail!("`{name}.optional` must be a boolean");
        }
    }

    Ok(())
}

/// Validate a `[package] name` value. Returns `Err(reason)` with a
/// human-readable explanation when invalid. Shared by `Manifest::load`
/// (rejecting bad existing manifests) and `cust new` (rejecting bad
/// new-project names before any files are written).
pub fn validate_package_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("name must not be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("name must be ASCII alphanumerics / `_` / `-`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_dep_spec, validate_package_name, CrateKind, Manifest};

    #[test]
    fn accepts_typical_names() {
        for name in ["hello", "hello_world", "hello-world", "abc123", "x"] {
            assert!(validate_package_name(name).is_ok(), "rejected {name:?}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        for name in ["", "has spaces", "unicodé", "a/b", "a.b"] {
            assert!(validate_package_name(name).is_err(), "accepted {name:?}");
        }
    }

    fn parse(text: &str) -> Manifest {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), text).unwrap();
        Manifest::load(tmp.path()).unwrap()
    }

    fn parse_err(text: &str) -> String {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), text).unwrap();
        format!("{:#}", Manifest::load(tmp.path()).unwrap_err())
    }

    #[test]
    fn virtual_workspace_root_parses() {
        let m = parse(
            r#"
[workspace]
members = ["app", "util"]
"#,
        );
        assert!(!m.is_package());
        assert!(m.declares_workspace());
        assert_eq!(
            m.workspace.unwrap().members,
            vec!["app".to_string(), "util".to_string()]
        );
    }

    #[test]
    fn package_and_workspace_both_parse() {
        let m = parse(
            r#"
[package]
name = "root"
version = "0.1.0"

[workspace]
members = ["util"]
"#,
        );
        assert!(m.is_package());
        assert!(m.declares_workspace());
    }

    #[test]
    fn manifest_with_neither_package_nor_workspace_is_error() {
        let e = parse_err(
            r"
[clang]
extra-cflags = []
",
        );
        assert!(e.contains("neither [package] nor [workspace]"), "{e}");
    }

    #[test]
    fn workspace_globs_rejected() {
        let e = parse_err(
            r#"
[workspace]
members = ["crates/*"]
"#,
        );
        assert!(e.contains("globs"), "{e}");
        assert!(e.contains("v0.4+"), "{e}");
    }

    #[test]
    fn workspace_absolute_path_rejected() {
        let e = parse_err(
            r#"
[workspace]
members = ["/abs"]
"#,
        );
        assert!(e.contains("absolute path"), "{e}");
    }

    #[test]
    fn workspace_parent_escape_rejected() {
        let e = parse_err(
            r#"
[workspace]
members = ["../outside"]
"#,
        );
        assert!(e.contains("`..`"), "{e}");
    }

    #[test]
    fn workspace_duplicate_member_rejected() {
        let e = parse_err(
            r#"
[workspace]
members = ["a", "a"]
"#,
        );
        assert!(e.contains("duplicate"), "{e}");
    }

    #[test]
    fn dep_path_only_accepted() {
        validate_dep_spec(
            "util",
            &toml::Value::Table({
                let mut t = toml::value::Table::new();
                t.insert("path".to_string(), toml::Value::String("../util".into()));
                t
            }),
        )
        .unwrap();
    }

    #[test]
    fn dep_with_features_accepted() {
        let mut t = toml::value::Table::new();
        t.insert("path".to_string(), toml::Value::String("../util".into()));
        t.insert(
            "features".to_string(),
            toml::Value::Array(vec![toml::Value::String("json".into())]),
        );
        t.insert("default-features".to_string(), toml::Value::Boolean(false));
        t.insert("optional".to_string(), toml::Value::Boolean(true));
        validate_dep_spec("util", &toml::Value::Table(t)).unwrap();
    }

    #[test]
    fn dep_bare_version_rejected_with_v04_pointer() {
        let v = toml::Value::String("1.0".into());
        let e = format!("{:#}", validate_dep_spec("foo", &v).unwrap_err());
        assert!(e.contains("version specs are v0.4+"), "{e}");
        assert!(e.contains("path"), "{e}");
    }

    #[test]
    fn dep_version_in_table_rejected() {
        let mut t = toml::value::Table::new();
        t.insert("path".to_string(), toml::Value::String("../x".into()));
        t.insert("version".to_string(), toml::Value::String("1.0".into()));
        let e = format!(
            "{:#}",
            validate_dep_spec("foo", &toml::Value::Table(t)).unwrap_err()
        );
        assert!(e.contains("v0.4+"), "{e}");
    }

    #[test]
    fn dep_git_rejected() {
        let mut t = toml::value::Table::new();
        t.insert("path".to_string(), toml::Value::String("../x".into()));
        t.insert(
            "git".to_string(),
            toml::Value::String("https://example".into()),
        );
        let e = format!(
            "{:#}",
            validate_dep_spec("foo", &toml::Value::Table(t)).unwrap_err()
        );
        assert!(e.contains("v0.4+"), "{e}");
    }

    #[test]
    fn dep_workspace_true_rejected() {
        let mut t = toml::value::Table::new();
        t.insert("workspace".to_string(), toml::Value::Boolean(true));
        // also need path-not-required check ordering — workspace
        // is rejected before path check
        let e = format!(
            "{:#}",
            validate_dep_spec("foo", &toml::Value::Table(t)).unwrap_err()
        );
        // The path-missing error fires first in our current
        // implementation, which is fine \u2014 user fixes path,
        // then sees the workspace rejection.
        assert!(e.contains("v0.4") || e.contains("path"), "{e}");
    }

    #[test]
    fn dep_path_missing_rejected() {
        let t = toml::value::Table::new();
        let e = format!(
            "{:#}",
            validate_dep_spec("foo", &toml::Value::Table(t)).unwrap_err()
        );
        assert!(e.contains("missing `path"), "{e}");
    }

    // ─── Slice A (v0.3.1): [[bin]] + CrateKind ──────────────────

    /// Stage a crate root with a manifest and an optional pair of
    /// `src/lib.c` / `src/main.c`. Returns a `(tempdir, crate_root)`
    /// so the temp dir's lifetime outlives the test.
    fn stage_crate(
        manifest_text: &str,
        lib_c: Option<&str>,
        main_c: Option<&str>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("Cust.toml"), manifest_text).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        if let Some(body) = lib_c {
            std::fs::write(src.join("lib.c"), body).unwrap();
        }
        if let Some(body) = main_c {
            std::fs::write(src.join("main.c"), body).unwrap();
        }
        (tmp, root)
    }

    const PKG_TOML: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    #[test]
    fn resolve_kind_lib_only_from_disk() {
        let (_tmp, root) = stage_crate(PKG_TOML, Some("int x;\n"), None);
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let kind = m.resolve_kind(&root).unwrap();
        assert!(kind.has_lib() && !kind.has_bin(), "{kind:?}");
        assert_eq!(kind.lib_source().unwrap(), root.join("src/lib.c"));
        assert_eq!(kind.bin_source(), None);
        assert!(matches!(kind, CrateKind::Lib { .. }));
    }

    #[test]
    fn resolve_kind_bin_only_from_disk() {
        let (_tmp, root) = stage_crate(PKG_TOML, None, Some("int main(void){return 0;}\n"));
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let kind = m.resolve_kind(&root).unwrap();
        assert!(kind.has_bin() && !kind.has_lib(), "{kind:?}");
        assert_eq!(kind.bin_source().unwrap(), root.join("src/main.c"));
        assert!(matches!(kind, CrateKind::Bin { .. }));
    }

    #[test]
    fn resolve_kind_lib_and_bin_when_both_files_present() {
        let (_tmp, root) = stage_crate(
            PKG_TOML,
            Some("int x;\n"),
            Some("int main(void){return 0;}\n"),
        );
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let kind = m.resolve_kind(&root).unwrap();
        assert!(kind.has_lib() && kind.has_bin(), "{kind:?}");
        assert!(matches!(kind, CrateKind::LibAndBin { .. }));
    }

    #[test]
    fn resolve_kind_neither_source_is_error() {
        let (_tmp, root) = stage_crate(PKG_TOML, None, None);
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let e = format!("{:#}", m.resolve_kind(&root).unwrap_err());
        assert!(e.contains("no library or binary source"), "{e}");
        assert!(e.contains("src/lib.c"), "{e}");
        assert!(e.contains("src/main.c"), "{e}");
    }

    #[test]
    fn explicit_lib_table_requires_lib_source() {
        // [lib] table present but src/lib.c missing — even when
        // src/main.c exists. User declared they want a lib;
        // missing source is an error, not a silent demotion.
        let manifest = format!("{PKG_TOML}[lib]\n");
        let (_tmp, root) = stage_crate(&manifest, None, Some("int main(void){return 0;}\n"));
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let e = format!("{:#}", m.resolve_kind(&root).unwrap_err());
        assert!(e.contains("library source"), "{e}");
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn explicit_bin_table_requires_bin_source() {
        let manifest = format!("{PKG_TOML}[[bin]]\n");
        let (_tmp, root) = stage_crate(&manifest, Some("int x;\n"), None);
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let e = format!("{:#}", m.resolve_kind(&root).unwrap_err());
        assert!(e.contains("binary source"), "{e}");
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn explicit_bin_path_override_resolves_to_custom_file() {
        let manifest = format!("{PKG_TOML}[[bin]]\npath = \"src/app.c\"\n");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("Cust.toml"), &manifest).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/app.c"), "int main(void){return 0;}\n").unwrap();
        let m = Manifest::load(&root.join("Cust.toml")).unwrap();
        let kind = m.resolve_kind(&root).unwrap();
        assert_eq!(kind.bin_source().unwrap(), root.join("src/app.c"));
    }

    #[test]
    fn multiple_bin_entries_rejected_in_v0_3_1() {
        let manifest = format!("{PKG_TOML}[[bin]]\n[[bin]]\npath = \"src/other.c\"\n");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Cust.toml");
        std::fs::write(&path, &manifest).unwrap();
        let e = format!("{:#}", Manifest::load(&path).unwrap_err());
        assert!(e.contains("v0.3.1"), "{e}");
        assert!(e.contains("multi-bin") || e.contains("v0.4+"), "{e}");
    }

    #[test]
    fn bin_table_without_package_rejected() {
        // [[bin]] in a virtual workspace root is meaningless.
        let manifest = "[workspace]\nmembers = [\"app\"]\n[[bin]]\n";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Cust.toml");
        std::fs::write(&path, manifest).unwrap();
        let e = format!("{:#}", Manifest::load(&path).unwrap_err());
        assert!(e.contains("[[bin]]"), "{e}");
        assert!(e.contains("[package]"), "{e}");
    }

    #[test]
    fn lib_crate_type_bin_rejected_with_v0_3_1_pointer() {
        // V31D-5: bin output is via [[bin]], not [lib]
        // crate-type. The error message should make that clear.
        let manifest = format!("{PKG_TOML}[lib]\ncrate-type = [\"bin\"]\n");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Cust.toml");
        std::fs::write(&path, &manifest).unwrap();
        let e = format!("{:#}", Manifest::load(&path).unwrap_err());
        assert!(e.contains("staticlib"), "{e}");
        assert!(e.contains("[[bin]]"), "{e}");
    }

    #[test]
    fn lib_crate_type_cdylib_still_rejected() {
        // cdylib is v0.4+; v0.3.1 doesn't change that.
        let manifest = format!("{PKG_TOML}[lib]\ncrate-type = [\"cdylib\"]\n");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Cust.toml");
        std::fs::write(&path, &manifest).unwrap();
        let e = format!("{:#}", Manifest::load(&path).unwrap_err());
        assert!(e.contains("staticlib"), "{e}");
        assert!(e.contains("v0.4+"), "{e}");
    }

    #[test]
    fn bin_with_unknown_subkey_rejected() {
        // [[bin]] currently only accepts `path`; `name` is
        // derived from the package name in v0.3.1.
        let manifest = format!("{PKG_TOML}[[bin]]\nname = \"override\"\n");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Cust.toml");
        std::fs::write(&path, &manifest).unwrap();
        let e = format!("{:#}", Manifest::load(&path).unwrap_err());
        assert!(e.contains("unknown field") || e.contains("name"), "{e}");
    }
}
