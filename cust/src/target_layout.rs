//! `target/` directory layout.
//!
//! v0.1 (pinned in `docs/design/v0.1.md`):
//!
//! ```text
//! target/
//! ├── .cust-version
//! ├── compile_commands.json
//! ├── debug/
//! │   ├── prelude.h
//! │   ├── build/<crate>/<qname>.preprocessed.c
//! │   ├── build/<crate>/<qname>.o
//! │   └── lib<name>.a
//! └── release/  (same shape)
//! ```
//!
//! v0.2 adds (`docs/design/v0.2.md`):
//!
//! ```text
//! target/<profile>/.h-fragments/<crate>/<qname>.cust.h
//! ```

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};

use crate::profile::ProfileKind;

pub struct TargetLayout {
    pub target_root: PathBuf,
    pub profile_root: PathBuf,
    #[allow(dead_code)]
    // available for callers; the build pipeline currently goes through the typed handle
    pub kind: ProfileKind,
}

impl TargetLayout {
    pub fn for_workspace(workspace_root: &std::path::Path, kind: ProfileKind) -> Self {
        let target_root = workspace_root.join("target");
        let profile_root = target_root.join(kind.dir_name());
        Self {
            target_root,
            profile_root,
            kind,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.profile_root)
            .with_context(|| format!("creating `{}`", self.profile_root.display()))?;
        Ok(())
    }

    pub fn prelude_path(&self) -> PathBuf {
        self.profile_root.join("prelude.h")
    }

    /// Root directory for fragment headers for `crate_name`.
    pub fn fragments_dir(&self, crate_name: &str) -> PathBuf {
        self.profile_root.join(".h-fragments").join(crate_name)
    }

    /// `target/<profile>/.h-fragments/<crate>/<qname>.cust.h`.
    pub fn fragment_path(&self, crate_name: &str, qualified_name: &str) -> PathBuf {
        self.fragments_dir(crate_name)
            .join(format!("{qualified_name}.cust.h"))
    }

    /// `target/<profile>/include/<crate>.h` — the user-facing
    /// concatenated crate header (cust-design.md §5).
    pub fn crate_header_path(&self, crate_name: &str) -> PathBuf {
        self.profile_root
            .join("include")
            .join(format!("{crate_name}.h"))
    }
}
