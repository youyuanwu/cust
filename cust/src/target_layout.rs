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

/// Which kind of test a discovery sidecar belongs to. Selects the
/// `target/<profile>/.test-discovery/<crate>/…` path scheme
/// (V43D-6): unit sidecars are keyed by module qualified name,
/// integration sidecars by file stem under a `tests/` subdir.
#[derive(Debug, Clone, Copy)]
pub enum TestOrigin<'a> {
    /// Unit test in `src/**.c`, keyed by module qualified name.
    Unit { qualified_name: &'a str },
    /// Integration test in `tests/<stem>.c`, keyed by file stem.
    #[allow(dead_code)] // constructed by Slice C (integration runner-TU sidecar plumbing)
    Integration { stem: &'a str },
}

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

    /// V0.4.0 RQ-V40-2: root directory for test-discovery
    /// sidecar files for `crate_name`.
    /// `target/<profile>/.test-discovery/<crate>/`.
    pub fn test_discovery_dir(&self, crate_name: &str) -> PathBuf {
        self.profile_root.join(".test-discovery").join(crate_name)
    }

    /// Per-test-TU test-discovery sidecar path, keyed by origin
    /// (V0.4.0 RQ-V40-2 for unit modules; v0.4.3 V43D-6 for
    /// integration tests).
    ///
    /// * `TestOrigin::Unit` →
    ///   `target/<profile>/.test-discovery/<crate>/<qname>.cust.tests`.
    /// * `TestOrigin::Integration` →
    ///   `target/<profile>/.test-discovery/<crate>/tests/<stem>.cust.tests`
    ///   (V43D-6 nests integration sidecars under a `tests/`
    ///   subdir so a `tests/<stem>.c` stem can never collide
    ///   with a unit module's qualified name).
    pub fn test_sidecar_path(&self, crate_name: &str, origin: TestOrigin<'_>) -> PathBuf {
        let dir = self.test_discovery_dir(crate_name);
        match origin {
            TestOrigin::Unit { qualified_name } => dir.join(format!("{qualified_name}.cust.tests")),
            TestOrigin::Integration { stem } => {
                dir.join("tests").join(format!("{stem}.cust.tests"))
            }
        }
    }

    /// Incremental-check (CHK-D-3): root directory for the
    /// per-module `.checked` stamps of `crate_name`.
    /// `target/<profile>/.check/<crate>/`. A check stamp is only a
    /// Ninja restat token (its bytes are irrelevant), so the
    /// directory must exist before `cmake -E touch` runs — the
    /// driver creates it (check has no leaf to self-create it).
    pub fn check_dir(&self, crate_name: &str) -> PathBuf {
        self.profile_root.join(".check").join(crate_name)
    }

    /// Incremental-check (CHK-D-3): per-module check stamp path —
    /// `target/<profile>/.check/<crate>/<qualified_name>.checked`.
    /// The custom command's `OUTPUT` (also its `touch` target).
    pub fn check_stamp_path(&self, crate_name: &str, qualified_name: &str) -> PathBuf {
        self.check_dir(crate_name)
            .join(format!("{qualified_name}.checked"))
    }

    /// v0.4.3 V43D-5/V43D-11: per-stem build + run directory for
    /// one integration test exe —
    /// `target/<profile>/test/<crate>/<stem>/`. The exe lands
    /// inside this directory (so the directory doubles as the
    /// exe's cwd, V43D-11), mirroring the unit-test invariant
    /// "cwd = the directory containing the exe" (V32D-4).
    pub fn integration_test_build_dir(&self, member_name: &str, stem: &str) -> PathBuf {
        self.test_build_dir(member_name).join(stem)
    }

    /// v0.4.3 V43D-5: integration test executable path —
    /// `target/<profile>/test/<crate>/<stem>/<stem>`. The nested
    /// `<stem>/` directory is what lets the exe file and the
    /// per-stem cwd directory coexist (a flat
    /// `test/<crate>/<stem>` would have to be both a file and a
    /// directory).
    #[allow(dead_code)] // surfaced by Slice B (add_executable OUTPUT path) / Slice C (spawn)
    pub fn integration_test_executable_path(&self, member_name: &str, stem: &str) -> PathBuf {
        self.integration_test_build_dir(member_name, stem)
            .join(stem)
    }

    /// Per-member crate header
    /// (`target/<profile>/build/<crate>/include/<crate>.h`).
    ///
    /// In v0.2 this lived at `target/<profile>/include/<crate>.h`
    /// (profile-level, single-crate-only). v0.3 moves it under
    /// the member's own build dir so workspace builds can point
    /// the `deps/<name>` symlink at it without collisions
    /// (cust-design.md §5 + v0.3.0.md scope item 6).
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
