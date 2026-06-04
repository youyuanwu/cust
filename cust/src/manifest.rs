//! `Cust.toml` schema and discovery.
//!
//! v0.1 only honours `[package]` (name + version), `[lib]` (path,
//! crate-type), `[clang]` (std, extra-cflags), and `[profile.*]`. The
//! parser is **strict**: any unknown top-level table or field is an
//! error. Known-but-unimplemented sections (`[features]`,
//! `[dependencies]`, etc.) are accepted only when empty/absent; a
//! populated `[dependencies]` table produces a clear "not yet
//! supported in v0.1" error from the loader.
//!
//! See `docs/design/cust-design.md` §3 and §17 for the canonical
//! schema and v0.1 scope.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Name of the manifest file. Walked up from cwd.
pub const MANIFEST_FILE: &str = "Cust.toml";

/// Parsed `Cust.toml`. Only fields the v0.1 driver uses are exposed
/// as typed accessors; the rest live in `_ignored` to preserve the
/// strict-mode unknown-field check without forcing the driver to
/// model every future field.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub package: Package,

    #[serde(default)]
    pub lib: Option<Lib>,

    #[serde(default)]
    pub clang: Clang,

    #[serde(default)]
    pub profile: Profiles,

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
    #[serde(default)]
    pub workspace: Option<toml::Value>,
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
        manifest.validate_v0_1(path)?;
        Ok(manifest)
    }

    fn validate_v0_1(&self, path: &Path) -> Result<()> {
        let unsupported = |what: &str| -> anyhow::Error {
            anyhow::anyhow!(
                "`{what}` in `{}` is not yet supported in cust v0.1",
                path.display()
            )
        };

        if !self.features.is_empty() {
            return Err(unsupported("[features]"));
        }
        if !self.dependencies.is_empty() {
            return Err(unsupported("[dependencies]"));
        }
        if !self.build_dependencies.is_empty() {
            return Err(unsupported("[build-dependencies]"));
        }
        if !self.dev_dependencies.is_empty() {
            return Err(unsupported("[dev-dependencies]"));
        }
        if self.workspace.is_some() {
            return Err(unsupported("[workspace]"));
        }

        if let Some(lib) = &self.lib {
            if let Some(ct) = &lib.crate_type {
                if ct.iter().any(|s| s != "staticlib") {
                    bail!(
                        "`[lib] crate-type` in `{}` only supports \
                         [\"staticlib\"] in cust v0.1 (got {:?})",
                        path.display(),
                        ct
                    );
                }
            }
        }

        // Package name sanity (matches cargo's rule of thumb: ASCII,
        // [A-Za-z0-9_-], non-empty). Strict enough that the name is
        // safe to splice into filenames.
        let name = &self.package.name;
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!(
                "invalid `[package] name = {name:?}` in `{}` — must be \
                 non-empty ASCII alphanumerics / `_` / `-`",
                path.display()
            );
        }

        Ok(())
    }

    /// Resolved path to the single TU we compile in v0.1. Relative
    /// paths in `[lib].path` are resolved against `crate_root`.
    pub fn lib_source(&self, crate_root: &Path) -> PathBuf {
        let rel = self
            .lib
            .as_ref()
            .and_then(|l| l.path.as_deref())
            .unwrap_or_else(|| Path::new("src/lib.c"));
        crate_root.join(rel)
    }
}
