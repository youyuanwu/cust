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

    /// Per-member crate header
    /// (`target/<profile>/build/<crate>/include/<crate>.h`).
    ///
    /// In v0.2 this lived at `target/<profile>/include/<crate>.h`
    /// (profile-level, single-crate-only). v0.3 moves it under
    /// the member's own build dir so workspace builds can point
    /// the `deps/<name>` symlink at it without collisions
    /// (cust-design.md §5 + v0.3.md scope item 6).
    pub fn crate_header_path(&self, crate_name: &str) -> PathBuf {
        self.build_dir(crate_name)
            .join("include")
            .join(format!("{crate_name}.h"))
    }

    /// `target/<profile>/deps/<dep_name>/` — the workspace-shared
    /// view of a dep's outputs. v0.3 makes this a symlink to the
    /// dep's own build dir (V3D-5 option A); consumers reach
    /// `<dep_dir>/include/<dep>.h` and `<dep_dir>/lib<dep>.a`
    /// through it.
    pub fn dep_dir(&self, dep_name: &str) -> PathBuf {
        self.profile_root.join("deps").join(dep_name)
    }

    /// Producer build directory for a member \u2014
    /// `target/<profile>/build/<member>/`. Per-member outputs land
    /// here (objects, fragment dir, include dir, archive).
    pub fn build_dir(&self, member_name: &str) -> PathBuf {
        self.profile_root.join("build").join(member_name)
    }

    /// Test-build directory for a member —
    /// `target/<profile>/test/<member>/`. v0.3.2 V32D-4: the test
    /// build is a fully fresh build tree per member, never
    /// colocated with the normal lib build, so objects produced
    /// with `-DCUST_TEST_BUILD=1` don't collide with the
    /// non-test ones. Houses the per-TU objects, the generated
    /// `cust_test_main.c` runner, and the resulting test
    /// executable.
    pub fn test_build_dir(&self, member_name: &str) -> PathBuf {
        self.profile_root.join("test").join(member_name)
    }

    /// Test executable path for `member_name`. V32D-5 specified
    /// `target/<profile>/test/<crate>` (no extension) and V32D-4
    /// specified the build tree at the same path; the two
    /// conflict and v0.3.2 resolves it in favour of V32D-4's
    /// "fully fresh build tree" framing — the executable lives
    /// inside the build tree at
    /// `target/<profile>/test/<crate>/<crate>`. Still a plain
    /// `<crate>` name (V32D-5's `ps` argument carries over).
    pub fn test_executable_path(&self, member_name: &str) -> PathBuf {
        self.test_build_dir(member_name).join(member_name)
    }
}
