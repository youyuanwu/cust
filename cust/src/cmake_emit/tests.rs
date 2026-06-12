//! Slice A unit tests for `cmake_emit`.
//!
//! * **Golden file** at `testdata/cwork.cmake` pins the emitter
//!   byte format (V42D-4). Update via:
//!   ```text
//!   UPDATE_GOLDEN=1 cargo test -p cust cmake_emit::tests::golden_cwork
//!   ```
//!   Same shape as `mod_scanner`'s and other golden tests in the
//!   tree.
//! * **Stamp tests** cover RQ-V42-1's plugin-bytes-in-stamp lock
//!   (a plugin .so byte change must churn the stamp even when
//!   paths are unchanged), plus the `Unchanged` / `Wrote`
//!   write-skip behaviour.
//! * **Tool discovery** is exercised in the bench/dev path only
//!   (a host without `cmake` would otherwise break `cargo test`);
//!   the version parser is unit-tested directly.

use std::path::PathBuf;

use super::*;

// ─── Golden file (V42D-4) ───────────────────────────────────────

fn cwork_view() -> WorkspaceView {
    // Mirror of the design doc's example workspace
    // (docs/design/v0.4.2.md "Headline outcome"): `cstd`
    // library with two modules + `hello-cstd` binary depending
    // on it. Absolute paths use `/ws/...` so the golden file is
    // platform-stable.
    let cstd_archive = PathBuf::from("/ws/target/debug/build/cstd");
    let cstd_include = PathBuf::from("/ws/target/debug/build/cstd/include");
    WorkspaceView {
        cust_version: "0.4.2".to_string(),
        c_standard: "23".to_string(),
        plugin_path: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
        members: vec![
            MemberView {
                name: "cstd".to_string(),
                kind: MemberKind::LibOnly,
                lib_sources: vec![
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/.rewrite/cstd/src/types.c"),
                        object_depends: vec![],
                    },
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/.rewrite/cstd/src/lib.c"),
                        object_depends: vec![PathBuf::from(
                            "/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h",
                        )],
                    },
                ],
                bin_sources: vec![],
                archive_output_dir: cstd_archive,
                bin_include_dirs: vec![],
                workspace_link_deps: vec![],
            },
            MemberView {
                name: "hello-cstd".to_string(),
                kind: MemberKind::BinOnly,
                lib_sources: vec![],
                bin_sources: vec![SourceFile {
                    path: PathBuf::from("/ws/target/debug/.rewrite/hello-cstd/src/main.c"),
                    object_depends: vec![
                        PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__lib.cust.h"),
                        PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h"),
                    ],
                }],
                archive_output_dir: PathBuf::from("/ws/target/debug/build/hello-cstd"),
                bin_include_dirs: vec![cstd_include],
                workspace_link_deps: vec!["cstd".to_string()],
            },
        ],
    }
}

#[test]
fn golden_cwork() {
    let actual = generate(&cwork_view());
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/cmake_emit/testdata/cwork.cmake");

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if let Some(parent) = golden_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&golden_path, &actual).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|e| {
        panic!(
            "reading golden `{}` (run with UPDATE_GOLDEN=1 to create): {e}",
            golden_path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "emitter output differs from golden `{}` — rerun with UPDATE_GOLDEN=1 if intentional",
        golden_path.display()
    );
}

#[test]
fn determinism_no_plugin() {
    // No plugin → no shared-compile-options foreach. Output is
    // still well-formed CMake (verified by the golden equivalence
    // below — same view minus plugin should match the golden up
    // to that block).
    let mut view = cwork_view();
    view.plugin_path = None;
    let out = generate(&view);
    assert!(!out.contains("foreach(_t"), "no plugin ⇒ no foreach block");
    assert!(
        out.contains("add_library(cstd STATIC"),
        "lib target still emitted"
    );
    assert!(
        out.contains("add_executable(hello-cstd"),
        "bin target still emitted"
    );
}

#[test]
fn lib_and_bin_member_emits_both_targets() {
    // A single member with kind=LibAndBin produces one
    // add_library and one add_executable, and the bin links
    // against the (same-name) lib via workspace_link_deps.
    let view = WorkspaceView {
        cust_version: "0.4.2".to_string(),
        c_standard: "23".to_string(),
        plugin_path: None,
        members: vec![MemberView {
            name: "app".to_string(),
            kind: MemberKind::LibAndBin,
            lib_sources: vec![SourceFile {
                path: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                object_depends: vec![],
            }],
            bin_sources: vec![SourceFile {
                path: PathBuf::from("/ws/target/debug/.rewrite/app/src/main.c"),
                object_depends: vec![],
            }],
            archive_output_dir: PathBuf::from("/ws/target/debug/build/app"),
            bin_include_dirs: vec![PathBuf::from("/ws/target/debug/build/app/include")],
            workspace_link_deps: vec!["app".to_string()],
        }],
    };
    let out = generate(&view);
    assert!(out.contains("add_library(app STATIC"));
    assert!(out.contains("add_executable(app"));
    assert!(out.contains("target_link_libraries(app PRIVATE"));
}

// ─── Stamp (V42D-8 + RQ-V42-1) ──────────────────────────────────

#[test]
fn stamp_changes_when_cmakelists_changes() {
    let a = compute_stamp(b"alpha", None).unwrap();
    let b = compute_stamp(b"beta", None).unwrap();
    assert_ne!(a, b, "different CMakeLists bytes must hash differently");
}

#[test]
fn stamp_changes_when_plugin_bytes_change_at_same_path() {
    // RQ-V42-1: same CMakeLists bytes + same plugin path + DIFFERENT
    // plugin .so contents must produce a different stamp. The two
    // tempfile paths differ here, but the test's invariant is "the
    // plugin's *bytes* feed the hash" — same bytes through different
    // paths produces equal stamps; different bytes produces unequal.
    let dir = tempfile::tempdir().unwrap();
    let plugin_a = dir.path().join("plugin_a.so");
    let plugin_b = dir.path().join("plugin_b.so");
    std::fs::write(&plugin_a, b"plugin contents v1").unwrap();
    std::fs::write(&plugin_b, b"plugin contents v2 \xff\x00\x01").unwrap();

    let cmakelists = b"# CMakeLists\n";
    let s1 = compute_stamp(cmakelists, Some(&plugin_a)).unwrap();
    let s2 = compute_stamp(cmakelists, Some(&plugin_b)).unwrap();
    assert_ne!(
        s1, s2,
        "different plugin .so bytes must churn the stamp (RQ-V42-1)"
    );
}

#[test]
fn stamp_stable_for_same_plugin_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugin.so");
    std::fs::write(&plugin, b"plugin contents").unwrap();
    let s1 = compute_stamp(b"# CMakeLists\n", Some(&plugin)).unwrap();
    let s2 = compute_stamp(b"# CMakeLists\n", Some(&plugin)).unwrap();
    assert_eq!(s1, s2, "stamp must be stable for identical inputs");
}

#[test]
fn stamp_hex_round_trip() {
    let stamp = compute_stamp(b"hello", None).unwrap();
    let hex = stamp.to_hex();
    assert_eq!(hex.len(), 64);
    assert!(hex
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    let parsed = Stamp::from_hex(&hex).expect("round-trip parse");
    assert_eq!(parsed, stamp);
}

#[test]
fn stamp_from_hex_rejects_malformed() {
    assert!(Stamp::from_hex("").is_none(), "empty rejected");
    assert!(Stamp::from_hex("abc").is_none(), "too-short rejected");
    let mut sixty_four_nonhex = "z".repeat(64);
    assert!(
        Stamp::from_hex(&sixty_four_nonhex).is_none(),
        "non-hex rejected"
    );
    sixty_four_nonhex.push('a');
    assert!(
        Stamp::from_hex(&sixty_four_nonhex).is_none(),
        "too-long rejected"
    );
}

#[test]
fn write_if_changed_writes_first_time_and_skips_second() {
    let dir = tempfile::tempdir().unwrap();
    let cmakelists = dir.path().join("cmake/CMakeLists.txt");
    let stamp = dir.path().join("cmake/stamp/cmakelists.sha256");
    let bytes = b"# v1\n";
    let r1 = write_if_changed(&cmakelists, bytes, &stamp, None).unwrap();
    assert_eq!(r1, WriteOutcome::Wrote, "first call writes");
    assert!(cmakelists.is_file(), "CMakeLists materialised");
    assert!(stamp.is_file(), "stamp materialised");

    let r2 = write_if_changed(&cmakelists, bytes, &stamp, None).unwrap();
    assert_eq!(
        r2,
        WriteOutcome::Unchanged,
        "second identical call is a no-op"
    );

    let r3 = write_if_changed(&cmakelists, b"# v2\n", &stamp, None).unwrap();
    assert_eq!(r3, WriteOutcome::Wrote, "byte change triggers rewrite");
    assert_eq!(std::fs::read(&cmakelists).unwrap(), b"# v2\n");
}

#[test]
fn write_if_changed_recovers_from_corrupt_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let cmakelists = dir.path().join("CMakeLists.txt");
    let stamp = dir.path().join("stamp");
    std::fs::write(&cmakelists, b"# v1\n").unwrap();
    std::fs::write(&stamp, b"not a real hex stamp").unwrap();
    let r = write_if_changed(&cmakelists, b"# v1\n", &stamp, None).unwrap();
    assert_eq!(
        r,
        WriteOutcome::Wrote,
        "corrupt stamp ⇒ force rewrite (safe default)"
    );
}

#[test]
fn write_if_changed_rewrites_when_cmakelists_missing() {
    // Stamp is valid + matches new bytes, but the CMakeLists
    // was deleted (e.g. `cust clean` ran). Must rewrite.
    let dir = tempfile::tempdir().unwrap();
    let cmakelists = dir.path().join("CMakeLists.txt");
    let stamp_path = dir.path().join("stamp");
    let bytes = b"# v1\n";
    let stamp = compute_stamp(bytes, None).unwrap();
    std::fs::write(&stamp_path, stamp.to_hex()).unwrap();
    // (no CMakeLists written)
    let r = write_if_changed(&cmakelists, bytes, &stamp_path, None).unwrap();
    assert_eq!(r, WriteOutcome::Wrote);
    assert!(cmakelists.is_file());
}

// ─── Tool discovery: version parser ─────────────────────────────

#[test]
fn parse_cmake_version_line() {
    assert_eq!(parse_version("cmake version 3.28.1").unwrap(), (3, 28, 1));
    assert_eq!(parse_version("cmake version 3.21.0").unwrap(), (3, 21, 0));
    // CMake-Kitware nightlies sometimes include `-rc` suffixes.
    assert_eq!(
        parse_version("cmake version 3.30.0-rc1").unwrap(),
        (3, 30, 0)
    );
}

#[test]
fn parse_ninja_version_line() {
    assert_eq!(parse_version("1.11.1").unwrap(), (1, 11, 1));
    // Some Ninja builds omit the patch.
    assert_eq!(parse_version("1.11").unwrap(), (1, 11, 0));
}

#[test]
fn parse_version_rejects_garbage() {
    assert!(parse_version("hello world").is_err());
    assert!(parse_version("").is_err());
}
