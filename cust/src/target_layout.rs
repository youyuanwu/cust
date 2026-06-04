//! `target/` directory layout, pinned in `docs/design/cust-design.md`
//! §17 ("`target/` layout in v0.1"):
//!
//! ```text
//! target/
//! ├── .cust-version
//! ├── compile_commands.json
//! ├── debug/
//! │   ├── prelude.h
//! │   ├── build/<crate>/lib.o
//! │   └── lib<name>.a
//! └── release/  (same shape)
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
}
