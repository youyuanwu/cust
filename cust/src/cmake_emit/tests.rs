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

#[allow(clippy::too_many_lines)] // mirror-of-design fixture; splitting hurts readability
fn cwork_view() -> WorkspaceView {
    // Mirror of the design doc's example workspace
    // (docs/design/v0.4.2.md "Headline outcome"): `cstd`
    // library with two modules + `hello-cstd` binary depending
    // on it. Absolute paths use `/ws/...` so the golden file is
    // platform-stable.
    let cstd_archive = PathBuf::from("/ws/target/debug/build/cstd");
    let cstd_include = PathBuf::from("/ws/target/debug/build/cstd/include");
    let profile_root = PathBuf::from("/ws/target/debug");
    // Per-member compile options match what build_member_compile_options
    // emits for cwork's default dev profile (no overrides).
    let compile_options = vec![
        "-O0".to_string(),
        "-g3".to_string(),
        "-fvisibility=hidden".to_string(),
        "-include".to_string(),
        "/ws/target/debug/prelude.h".to_string(),
        "SHELL:-fplugin=/ws/target/debug/libcust_plugin.so".to_string(),
        "-Wno-unknown-attributes".to_string(),
    ];
    // Integration-test compile options append the test-build
    // define (V43D + V42D-14 parity).
    let itest_options = {
        let mut o = compile_options.clone();
        o.push("-DCUST_TEST_BUILD=1".to_string());
        o
    };
    WorkspaceView {
        cust_version: "0.4.2".to_string(),
        c_standard: "23".to_string(),
        plugin_path: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
        cust_exe: PathBuf::from("/ws/bin/cust"),
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
                bins: vec![],
                archive_output_dir: cstd_archive,
                runtime_output_dir: profile_root.clone(),
                bin_include_dirs: vec![],
                workspace_link_deps: vec![],
                lib_workspace_deps: vec![],
                compile_options: compile_options.clone(),
                test_target: None,
                // v0.4.3 V43D-5: two integration tests
                // (alphabetical-by-stem, V43D-1) exercising the
                // `add_executable(<crate>__itest__<stem>
                // EXCLUDE_FROM_ALL ...)` shape from the design's
                // §4 example.
                integration_tests: vec![
                    IntegrationTestView {
                        target_name: "cstd__itest__alloc_pressure".to_string(),
                        output_name: "alloc_pressure".to_string(),
                        sources: vec![
                            SourceFile {
                                path: PathBuf::from(
                                    "/ws/target/debug/.rewrite/cstd/tests/alloc_pressure.c",
                                ),
                                object_depends: vec![],
                            },
                            SourceFile {
                                path: PathBuf::from(
                                    "/ws/target/debug/cmake/cust_itest_main_cstd__alloc_pressure.c",
                                ),
                                object_depends: vec![],
                            },
                        ],
                        include_dirs: vec![PathBuf::from("/ws/target/debug/build/cstd/include")],
                        link_deps: vec!["cstd".to_string()],
                        compile_options: itest_options.clone(),
                        runtime_output_dir: PathBuf::from(
                            "/ws/target/debug/test/cstd/alloc_pressure",
                        ),
                    },
                    IntegrationTestView {
                        target_name: "cstd__itest__basic".to_string(),
                        output_name: "basic".to_string(),
                        sources: vec![
                            SourceFile {
                                path: PathBuf::from("/ws/target/debug/.rewrite/cstd/tests/basic.c"),
                                object_depends: vec![],
                            },
                            SourceFile {
                                path: PathBuf::from(
                                    "/ws/target/debug/cmake/cust_itest_main_cstd__basic.c",
                                ),
                                object_depends: vec![],
                            },
                        ],
                        include_dirs: vec![PathBuf::from("/ws/target/debug/build/cstd/include")],
                        link_deps: vec!["cstd".to_string()],
                        compile_options: itest_options,
                        runtime_output_dir: PathBuf::from("/ws/target/debug/test/cstd/basic"),
                    },
                ],
                rewrites: vec![
                    RewriteCommand {
                        out: PathBuf::from("/ws/target/debug/.rewrite/cstd/src/types.c"),
                        origin: PathBuf::from("/ws/cstd/src/types.c"),
                        crate_name: "cstd".to_string(),
                        frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/cstd"),
                        deps_root: PathBuf::from("/ws/target/debug/deps"),
                        own_lib_header: PathBuf::from("/ws/target/debug/build/cstd/include/cstd.h"),
                        deps: vec![],
                        is_bin_half: false,
                        has_lib: true,
                        integration: false,
                    },
                    RewriteCommand {
                        out: PathBuf::from("/ws/target/debug/.rewrite/cstd/src/lib.c"),
                        origin: PathBuf::from("/ws/cstd/src/lib.c"),
                        crate_name: "cstd".to_string(),
                        frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/cstd"),
                        deps_root: PathBuf::from("/ws/target/debug/deps"),
                        own_lib_header: PathBuf::from("/ws/target/debug/build/cstd/include/cstd.h"),
                        deps: vec![],
                        is_bin_half: false,
                        has_lib: true,
                        integration: false,
                    },
                ],
                // v0.4.5 V45D-4: one surface command per lib
                // module. `types` has no imports; `lib` imports
                // `types` (its fragment is a DEPENDS edge).
                surface_commands: vec![
                    SurfaceCommand {
                        source: PathBuf::from("/ws/cstd/src/types.c"),
                        surface_out: PathBuf::from("/ws/target/debug/build/cstd/types.surface.c"),
                        fragment_out: PathBuf::from(
                            "/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h",
                        ),
                        frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/cstd"),
                        deps_root: PathBuf::from("/ws/target/debug/deps"),
                        deps: vec![],
                        std: "c23".to_string(),
                        cflags: vec!["-O0".to_string(), "-g3".to_string()],
                        includes: vec![PathBuf::from("/ws/cstd/src")],
                        prelude: PathBuf::from("/ws/target/debug/prelude.h"),
                        plugin: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
                        import_fragments: vec![],
                        dep_headers: vec![],
                    },
                    SurfaceCommand {
                        source: PathBuf::from("/ws/cstd/src/lib.c"),
                        surface_out: PathBuf::from("/ws/target/debug/build/cstd/lib.surface.c"),
                        fragment_out: PathBuf::from(
                            "/ws/target/debug/.h-fragments/cstd/cstd__lib.cust.h",
                        ),
                        frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/cstd"),
                        deps_root: PathBuf::from("/ws/target/debug/deps"),
                        deps: vec![],
                        std: "c23".to_string(),
                        cflags: vec!["-O0".to_string(), "-g3".to_string()],
                        includes: vec![PathBuf::from("/ws/cstd/src")],
                        prelude: PathBuf::from("/ws/target/debug/prelude.h"),
                        plugin: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
                        import_fragments: vec![PathBuf::from(
                            "/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h",
                        )],
                        dep_headers: vec![],
                    },
                ],
                // V45D-5: the published header concatenates both
                // fragments in topological order (types before lib).
                crate_header: Some(CrateHeaderCommand {
                    crate_name: "cstd".to_string(),
                    out: PathBuf::from("/ws/target/debug/build/cstd/include/cstd.h"),
                    frags: vec![
                        PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h"),
                        PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__lib.cust.h"),
                    ],
                }),
                surface_cycles: vec![],
                test_sidecars: vec![],
                test_runner: None,
                integration_test_sidecars: vec![],
                integration_test_runners: vec![],
                check_commands: vec![],
            },
            MemberView {
                name: "hello-cstd".to_string(),
                kind: MemberKind::BinOnly,
                lib_sources: vec![],
                bins: vec![BinView {
                    target_name: "hello-cstd".to_string(),
                    output_name: "hello-cstd".to_string(),
                    sources: vec![SourceFile {
                        path: PathBuf::from("/ws/target/debug/.rewrite/hello-cstd/src/main.c"),
                        object_depends: vec![
                            PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__lib.cust.h"),
                            PathBuf::from("/ws/target/debug/.h-fragments/cstd/cstd__types.cust.h"),
                        ],
                    }],
                }],
                archive_output_dir: PathBuf::from("/ws/target/debug/build/hello-cstd"),
                runtime_output_dir: profile_root,
                bin_include_dirs: vec![cstd_include],
                workspace_link_deps: vec!["cstd".to_string()],
                lib_workspace_deps: vec![],
                compile_options,
                test_target: None,
                integration_tests: vec![],
                rewrites: vec![RewriteCommand {
                    out: PathBuf::from("/ws/target/debug/.rewrite/hello-cstd/src/main.c"),
                    origin: PathBuf::from("/ws/hello-cstd/src/main.c"),
                    crate_name: "hello-cstd".to_string(),
                    frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/hello-cstd"),
                    deps_root: PathBuf::from("/ws/target/debug/deps"),
                    own_lib_header: PathBuf::from(
                        "/ws/target/debug/build/hello-cstd/include/hello-cstd.h",
                    ),
                    deps: vec!["cstd".to_string()],
                    is_bin_half: false,
                    has_lib: false,
                    integration: false,
                }],
                surface_commands: vec![],
                crate_header: None,
                surface_cycles: vec![],
                test_sidecars: vec![],
                test_runner: None,
                integration_test_sidecars: vec![],
                integration_test_runners: vec![],
                check_commands: vec![],
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
    // No plugin → the `SHELL:-fplugin=…` entry drops out of
    // each member's compile_options. Output is still well-formed.
    let mut view = cwork_view();
    view.plugin_path = None;
    for m in &mut view.members {
        m.compile_options.retain(|o| !o.contains("-fplugin="));
        for it in &mut m.integration_tests {
            it.compile_options.retain(|o| !o.contains("-fplugin="));
        }
    }
    let out = generate(&view);
    assert!(
        !out.contains("-fplugin="),
        "no plugin ⇒ no plugin flag in output"
    );
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
    let profile_root = PathBuf::from("/ws/target/debug");
    let view = WorkspaceView {
        cust_version: "0.4.2".to_string(),
        c_standard: "23".to_string(),
        plugin_path: None,
        cust_exe: PathBuf::from("/ws/bin/cust"),
        members: vec![MemberView {
            name: "app".to_string(),
            kind: MemberKind::LibAndBin,
            lib_sources: vec![SourceFile {
                path: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                object_depends: vec![],
            }],
            bins: vec![BinView {
                target_name: "app-bin".to_string(),
                output_name: "app".to_string(),
                sources: vec![SourceFile {
                    path: PathBuf::from("/ws/target/debug/.rewrite/app/src/main.c"),
                    object_depends: vec![],
                }],
            }],
            archive_output_dir: PathBuf::from("/ws/target/debug/build/app"),
            runtime_output_dir: profile_root,
            bin_include_dirs: vec![PathBuf::from("/ws/target/debug/build/app/include")],
            workspace_link_deps: vec!["app".to_string()],
            lib_workspace_deps: vec![],
            compile_options: vec!["-O0".to_string()],
            test_target: None,
            integration_tests: vec![],
            rewrites: vec![
                RewriteCommand {
                    out: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                    origin: PathBuf::from("/ws/app/src/lib.c"),
                    crate_name: "app".to_string(),
                    frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/app"),
                    deps_root: PathBuf::from("/ws/target/debug/deps"),
                    own_lib_header: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                    deps: vec![],
                    is_bin_half: false,
                    has_lib: true,
                    integration: false,
                },
                RewriteCommand {
                    out: PathBuf::from("/ws/target/debug/.rewrite/app/src/main.c"),
                    origin: PathBuf::from("/ws/app/src/main.c"),
                    crate_name: "app".to_string(),
                    frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/app"),
                    deps_root: PathBuf::from("/ws/target/debug/deps"),
                    own_lib_header: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                    deps: vec![],
                    is_bin_half: true,
                    has_lib: true,
                    integration: false,
                },
            ],
            // V45D-4: the lib half's single module surfaces; the
            // bin half (main.c) is never surface-passed.
            surface_commands: vec![SurfaceCommand {
                source: PathBuf::from("/ws/app/src/lib.c"),
                surface_out: PathBuf::from("/ws/target/debug/build/app/lib.surface.c"),
                fragment_out: PathBuf::from("/ws/target/debug/.h-fragments/app/app__lib.cust.h"),
                frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/app"),
                deps_root: PathBuf::from("/ws/target/debug/deps"),
                deps: vec![],
                std: "c23".to_string(),
                cflags: vec!["-O0".to_string()],
                includes: vec![PathBuf::from("/ws/app/src")],
                prelude: PathBuf::from("/ws/target/debug/prelude.h"),
                plugin: None,
                import_fragments: vec![],
                dep_headers: vec![],
            }],
            crate_header: Some(CrateHeaderCommand {
                crate_name: "app".to_string(),
                out: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                frags: vec![PathBuf::from(
                    "/ws/target/debug/.h-fragments/app/app__lib.cust.h",
                )],
            }),
            surface_cycles: vec![],
            test_sidecars: vec![],
            test_runner: None,
            integration_test_sidecars: vec![],
            integration_test_runners: vec![],
            check_commands: vec![],
        }],
    };
    let out = generate(&view);
    assert!(out.contains("add_library(app STATIC"));
    assert!(
        out.contains("add_executable(app-bin"),
        "lib+bin uses -bin suffix to avoid CMake target name collision"
    );
    assert!(out.contains("OUTPUT_NAME app"));
    assert!(out.contains("target_link_libraries(app-bin PRIVATE"));
    // V45D-3: bin-half rewrite carries `--bin-half`; lib does not.
    assert!(
        out.contains("internal rewrite-file"),
        "rewrite custom commands emitted"
    );
    assert!(out.contains("--bin-half"), "bin-half flag present");
}

#[test]
#[allow(clippy::too_many_lines)] // verbose fabricated view; readability over splitting
fn unit_test_sidecar_and_runner_commands_emitted() {
    // v0.4.6 V46D-2: a lib member with unit tests emits one
    // `internal test-sidecar --kind unit` custom command per lib
    // module plus one per-crate `internal test-runner` command
    // whose OUTPUT is the runner TU (a SOURCE of `<crate>__test`).
    let profile_root = PathBuf::from("/ws/target/debug");
    let view = WorkspaceView {
        cust_version: "0.4.6".to_string(),
        c_standard: "23".to_string(),
        plugin_path: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
        cust_exe: PathBuf::from("/ws/bin/cust"),
        members: vec![MemberView {
            name: "app".to_string(),
            kind: MemberKind::LibOnly,
            lib_sources: vec![SourceFile {
                path: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                object_depends: vec![],
            }],
            bins: vec![],
            archive_output_dir: PathBuf::from("/ws/target/debug/build/app"),
            runtime_output_dir: profile_root,
            bin_include_dirs: vec![],
            workspace_link_deps: vec![],
            lib_workspace_deps: vec![],
            compile_options: vec!["-O0".to_string()],
            test_target: Some(TestTargetView {
                target_name: "app__test".to_string(),
                sources: vec![
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                        object_depends: vec![],
                    },
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/cmake/cust_test_main_app.c"),
                        object_depends: vec![],
                    },
                ],
                include_dirs: vec![PathBuf::from("/ws/target/debug/build/app/include")],
                link_deps: vec![],
                compile_options: vec!["-O0".to_string(), "-DCUST_TEST_BUILD=1".to_string()],
                runtime_output_dir: PathBuf::from("/ws/target/debug/test/app"),
            }),
            integration_tests: vec![],
            rewrites: vec![],
            surface_commands: vec![],
            surface_cycles: vec![],
            crate_header: Some(CrateHeaderCommand {
                crate_name: "app".to_string(),
                out: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                frags: vec![PathBuf::from(
                    "/ws/target/debug/.h-fragments/app/app__lib.cust.h",
                )],
            }),
            test_sidecars: vec![TestSidecarCommand {
                crate_name: "app".to_string(),
                module: "lib".to_string(),
                source: PathBuf::from("/ws/app/src/lib.c"),
                surface_out: PathBuf::from("/ws/target/debug/build/app/lib.test-surface.c"),
                sidecar_out: PathBuf::from("/ws/target/debug/.test-discovery/app/lib.cust.tests"),
                frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/app"),
                deps_root: PathBuf::from("/ws/target/debug/deps"),
                deps: vec![],
                std: "c23".to_string(),
                cflags: vec!["-O0".to_string()],
                includes: vec![PathBuf::from("/ws/app/src")],
                prelude: PathBuf::from("/ws/target/debug/prelude.h"),
                plugin: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
                import_fragments: vec![],
                dep_headers: vec![],
            }],
            test_runner: Some(TestRunnerCommand {
                crate_name: "app".to_string(),
                out: PathBuf::from("/ws/target/debug/cmake/cust_test_main_app.c"),
                sidecars: vec![PathBuf::from(
                    "/ws/target/debug/.test-discovery/app/lib.cust.tests",
                )],
            }),
            integration_test_sidecars: vec![],
            integration_test_runners: vec![],
            check_commands: vec![],
        }],
    };
    let out = generate(&view);
    // The unit sidecar command (OUTPUT = the .cust.tests sidecar).
    assert!(
        out.contains("internal test-sidecar"),
        "test-sidecar command emitted"
    );
    assert!(out.contains("--kind unit"), "unit kind flag present");
    assert!(
        out.contains("OUTPUT \"/ws/target/debug/.test-discovery/app/lib.cust.tests\""),
        "sidecar OUTPUT is the .cust.tests path"
    );
    // The distinct test-surface scratch (must not collide with the
    // build-mode .surface.c — V46D-2).
    assert!(
        out.contains("lib.test-surface.c"),
        "test sidecar uses a distinct .test-surface.c scratch path"
    );
    // The per-crate runner command (OUTPUT = the runner TU, which
    // the `app__test` target lists as a source → anchor).
    assert!(
        out.contains("internal test-runner"),
        "test-runner command emitted"
    );
    assert!(
        out.contains("OUTPUT \"/ws/target/debug/cmake/cust_test_main_app.c\""),
        "runner OUTPUT is the runner TU consumed by app__test"
    );
    assert!(
        out.contains("--sidecar \"/ws/target/debug/.test-discovery/app/lib.cust.tests\""),
        "runner depends on the unit sidecar"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // verbose fabricated view; readability over splitting
fn integration_test_sidecar_runner_and_rewrite_commands_emitted() {
    // v0.4.6 V46D-3/RQ-V46-4: a lib member with an integration test
    // emits an integration `rewrite-file` command (`--integration`),
    // a per-stem `test-sidecar --kind integration` command, and a
    // per-stem `test-runner` command whose OUTPUT is the
    // `cust_itest_main_<crate>__<stem>.c` runner TU.
    let profile_root = PathBuf::from("/ws/target/debug");
    let view = WorkspaceView {
        cust_version: "0.4.6".to_string(),
        c_standard: "23".to_string(),
        plugin_path: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
        cust_exe: PathBuf::from("/ws/bin/cust"),
        members: vec![MemberView {
            name: "app".to_string(),
            kind: MemberKind::LibOnly,
            lib_sources: vec![SourceFile {
                path: PathBuf::from("/ws/target/debug/.rewrite/app/src/lib.c"),
                object_depends: vec![],
            }],
            bins: vec![],
            archive_output_dir: PathBuf::from("/ws/target/debug/build/app"),
            runtime_output_dir: profile_root,
            bin_include_dirs: vec![],
            workspace_link_deps: vec![],
            lib_workspace_deps: vec![],
            compile_options: vec!["-O0".to_string()],
            test_target: None,
            integration_tests: vec![IntegrationTestView {
                target_name: "app__itest__basic".to_string(),
                output_name: "basic".to_string(),
                sources: vec![
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/.rewrite/app/tests/basic.c"),
                        object_depends: vec![],
                    },
                    SourceFile {
                        path: PathBuf::from("/ws/target/debug/cmake/cust_itest_main_app__basic.c"),
                        object_depends: vec![],
                    },
                ],
                include_dirs: vec![PathBuf::from("/ws/target/debug/build/app/include")],
                link_deps: vec!["app".to_string()],
                compile_options: vec!["-O0".to_string(), "-DCUST_TEST_BUILD=1".to_string()],
                runtime_output_dir: PathBuf::from("/ws/target/debug/test/app/basic"),
            }],
            rewrites: vec![RewriteCommand {
                out: PathBuf::from("/ws/target/debug/.rewrite/app/tests/basic.c"),
                origin: PathBuf::from("/ws/app/tests/basic.c"),
                crate_name: "app".to_string(),
                frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/app"),
                deps_root: PathBuf::from("/ws/target/debug/deps"),
                own_lib_header: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                deps: vec![],
                is_bin_half: false,
                has_lib: true,
                integration: true,
            }],
            surface_commands: vec![],
            surface_cycles: vec![],
            crate_header: Some(CrateHeaderCommand {
                crate_name: "app".to_string(),
                out: PathBuf::from("/ws/target/debug/build/app/include/app.h"),
                frags: vec![PathBuf::from(
                    "/ws/target/debug/.h-fragments/app/app__lib.cust.h",
                )],
            }),
            test_sidecars: vec![],
            test_runner: None,
            integration_test_sidecars: vec![IntegrationTestSidecarCommand {
                crate_name: "app".to_string(),
                stem: "basic".to_string(),
                source: PathBuf::from("/ws/target/debug/.rewrite/app/tests/basic.c"),
                sidecar_out: PathBuf::from(
                    "/ws/target/debug/.test-discovery/app/tests/basic.cust.tests",
                ),
                std: "c23".to_string(),
                cflags: vec!["-O0".to_string()],
                prelude: PathBuf::from("/ws/target/debug/prelude.h"),
                plugin: Some(PathBuf::from("/ws/target/debug/libcust_plugin.so")),
                header_deps: vec![PathBuf::from("/ws/target/debug/build/app/include/app.h")],
            }],
            integration_test_runners: vec![TestRunnerCommand {
                crate_name: "app".to_string(),
                out: PathBuf::from("/ws/target/debug/cmake/cust_itest_main_app__basic.c"),
                sidecars: vec![PathBuf::from(
                    "/ws/target/debug/.test-discovery/app/tests/basic.cust.tests",
                )],
            }],
            check_commands: vec![],
        }],
    };
    let out = generate(&view);
    // V45D-3 completion: the integration rewrite carries --integration.
    assert!(
        out.contains("internal rewrite-file") && out.contains("--integration"),
        "integration rewrite-file command with --integration emitted"
    );
    // The integration sidecar (OUTPUT = the per-stem .cust.tests).
    assert!(out.contains("--kind integration"), "integration kind flag");
    assert!(out.contains("--stem basic"), "stem flag present");
    assert!(
        out.contains("OUTPUT \"/ws/target/debug/.test-discovery/app/tests/basic.cust.tests\""),
        "integration sidecar OUTPUT is the per-stem .cust.tests path"
    );
    // It DEPENDS on the published crate header (V46D-3).
    assert!(
        out.contains("\"/ws/target/debug/build/app/include/app.h\""),
        "integration sidecar depends on the published crate header"
    );
    // The per-stem runner (OUTPUT = the itest runner TU, a source
    // of the `app__itest__basic` target → anchor).
    assert!(
        out.contains("OUTPUT \"/ws/target/debug/cmake/cust_itest_main_app__basic.c\""),
        "integration runner OUTPUT is the per-stem runner TU"
    );
}

#[test]
fn integration_test_target_emits_exclude_from_all_exe() {
    // V43D-5: one `add_executable(<crate>__itest__<stem>
    // EXCLUDE_FROM_ALL ...)` per integration file, linking the
    // CUT's lib archive (not recompiling lib sources).
    let out = generate(&cwork_view());
    // Both integration targets present, alphabetical by stem.
    assert!(
        out.contains("add_executable(cstd__itest__alloc_pressure EXCLUDE_FROM_ALL"),
        "alloc_pressure itest target emitted"
    );
    assert!(
        out.contains("add_executable(cstd__itest__basic EXCLUDE_FROM_ALL"),
        "basic itest target emitted"
    );
    // alloc_pressure precedes basic (stem sort order, V43D-1).
    let alloc_at = out.find("cstd__itest__alloc_pressure").unwrap();
    let basic_at = out.find("cstd__itest__basic").unwrap();
    assert!(alloc_at < basic_at, "integration targets sorted by stem");
    // OUTPUT_NAME is the bare stem; exe lands under the per-stem
    // dir (V43D-5/V43D-11).
    assert!(out.contains("OUTPUT_NAME basic"));
    assert!(out.contains("RUNTIME_OUTPUT_DIRECTORY \"/ws/target/debug/test/cstd/basic\""));
    // V43D-3: links the CUT lib, not recompiling its sources.
    assert!(out.contains("target_link_libraries(cstd__itest__basic PRIVATE"));
    assert!(
        !out.contains("\"/ws/target/debug/.rewrite/cstd/src/lib.c\"\n)\nset_target_properties(cstd__itest__basic"),
        "integration exe must not recompile lib sources"
    );
    // Sources: rewritten test + runner TU.
    assert!(out.contains("/ws/target/debug/.rewrite/cstd/tests/basic.c"));
    assert!(out.contains("/ws/target/debug/cmake/cust_itest_main_cstd__basic.c"));
    // Test-build define present.
    assert!(out.contains("-DCUST_TEST_BUILD=1"));
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

// ─── incremental-check: check-command argv (CHK-D-2/CHK-D-3) ────

/// A `MemberGenCtx` shaped like a cwork lib member, for the
/// check-argv unit tests. `plugin` is the only knob the tests vary
/// (CHK-D-10).
fn check_gen_ctx(plugin: Option<PathBuf>) -> MemberGenCtx {
    MemberGenCtx {
        frags_dir: PathBuf::from("/ws/target/debug/.h-fragments/cstd"),
        deps_root: PathBuf::from("/ws/target/debug/deps"),
        own_lib_header: PathBuf::from("/ws/target/debug/build/cstd/include/cstd.h"),
        deps: vec![],
        has_lib: true,
        std: "c23".to_string(),
        mid_cflags: vec!["-O0".to_string(), "-g".to_string()],
        prelude: PathBuf::from("/ws/target/debug/prelude.h"),
        plugin,
    }
}

#[test]
fn check_argv_bakes_explicit_std_and_syntax_only() {
    let plugin = PathBuf::from("/ws/target/debug/libcust_plugin.so");
    let rewrite_tu = PathBuf::from("/ws/target/debug/.rewrite/cstd/src/lib.c");
    let ctx = check_gen_ctx(Some(plugin.clone()));
    let argv = build_check_argv(&ctx, &plugin, &rewrite_tu);

    // CHK-D-2 regression guard: explicit -std prepended (a custom
    // command does NOT inherit CMAKE_C_STANDARD).
    assert_eq!(argv.first().unwrap(), "-std=c23", "explicit -std prepended");

    // CHK-D-1: -fsyntax-only check of the .rewrite TU, which is the
    // final argv token.
    assert_eq!(
        argv.last().unwrap(),
        &rewrite_tu.display().to_string(),
        "the .rewrite TU is the last argv token"
    );
    let syntax_idx = argv.iter().position(|a| a == "-fsyntax-only").unwrap();
    assert_eq!(
        syntax_idx,
        argv.len() - 2,
        "-fsyntax-only immediately precedes the source"
    );

    // Bare -fplugin (no SHELL: wrapper — add_custom_command splits
    // its own argv).
    assert!(
        argv.iter()
            .any(|a| a == &format!("-fplugin={}", plugin.display())),
        "bare -fplugin present"
    );
    assert!(
        !argv.iter().any(|a| a.starts_with("SHELL:")),
        "no SHELL: wrapper in a custom-command argv"
    );

    // -Wno-unknown-attributes present — mirrors the build
    // (build_member_compile_options emits it unconditionally, CHK-D-3).
    assert!(
        argv.iter().any(|a| a == "-Wno-unknown-attributes"),
        "-Wno-unknown-attributes mirrors the lib compile"
    );

    // No codegen, no fragment-out, no tolerance flags (CHK-D-1/CHK-D-5).
    assert!(
        !argv.iter().any(|a| a.contains("fragment-out")),
        "no -fplugin-arg-cust-fragment-out"
    );
    assert!(!argv.iter().any(|a| a == "-Wno-error"), "no -Wno-error");
    assert!(
        !argv.iter().any(|a| a == "-c" || a == "-o"),
        "no codegen -c / -o"
    );

    // Prelude force-included.
    let inc = argv.iter().position(|a| a == "-include").unwrap();
    assert_eq!(
        argv[inc + 1],
        ctx.prelude.display().to_string(),
        "prelude is force-included"
    );
}

#[test]
fn check_argv_mirrors_lib_compile_options_no_drift() {
    // Drift guard (CHK-D-2): the check argv's middle equals the lib
    // target's compile_options verbatim, modulo the -std prefix, the
    // SHELL: wrapper on -fplugin, and the -fsyntax-only + source
    // suffix. Reuses the real build_member_compile_options so the
    // two flag paths cannot silently diverge.
    let profile =
        crate::profile::ResolvedProfile::resolve(crate::profile::ProfileKind::Dev, None).unwrap();
    let manifest: crate::manifest::Manifest = toml::from_str("").unwrap();
    let prelude = PathBuf::from("/ws/target/debug/prelude.h");
    let plugin_path = PathBuf::from("/ws/target/debug/libcust_plugin.so");
    let plugin = crate::plugin::Plugin {
        path: plugin_path.clone(),
    };

    let opts = build_member_compile_options(&profile, &manifest, &prelude, Some(&plugin));

    let mut mid_cflags = profile.cflags();
    mid_cflags.extend(manifest.clang.extra_cflags.iter().cloned());
    let ctx = MemberGenCtx {
        frags_dir: PathBuf::from("/x"),
        deps_root: PathBuf::from("/x"),
        own_lib_header: PathBuf::from("/x"),
        deps: vec![],
        has_lib: true,
        std: "c23".to_string(),
        mid_cflags,
        prelude,
        plugin: Some(plugin_path.clone()),
    };
    let rewrite_tu = PathBuf::from("/ws/target/debug/.rewrite/cstd/src/lib.c");
    let argv = build_check_argv(&ctx, &plugin_path, &rewrite_tu);

    // Recover compile_options from the check argv: drop -std (front)
    // + -fsyntax-only + source (back), and re-wrap -fplugin in SHELL:.
    let middle: Vec<String> = argv[1..argv.len() - 2]
        .iter()
        .map(|a| {
            a.strip_prefix("-fplugin=")
                .map_or_else(|| a.clone(), |rest| format!("SHELL:-fplugin={rest}"))
        })
        .collect();
    assert_eq!(
        middle, opts,
        "check argv must mirror the lib target compile_options (no drift)"
    );
}

#[test]
fn check_command_omitted_without_plugin() {
    // CHK-D-10 emitter-layer rule: a plugin-less view yields NO
    // check command for the module (never a plugin-less
    // -fsyntax-only), while a plugin-bearing view yields one whose
    // OUTPUT is the module's .checked stamp.
    let layout = TargetLayout::for_workspace(std::path::Path::new("/ws"), ProfileKind::Dev);
    let rewrite_tu = PathBuf::from("/ws/target/debug/.rewrite/cstd/src/lib.c");
    let frag_depends = vec![PathBuf::from(
        "/ws/target/debug/.h-fragments/cstd/mem.cust.h",
    )];

    let none = check_command_for(
        &check_gen_ctx(None),
        &layout,
        "cstd",
        "cstd__lib",
        &rewrite_tu,
        &frag_depends,
    );
    assert!(none.is_none(), "no plugin ⇒ no check command (CHK-D-10)");

    let plugin = PathBuf::from("/ws/target/debug/libcust_plugin.so");
    let some = check_command_for(
        &check_gen_ctx(Some(plugin.clone())),
        &layout,
        "cstd",
        "cstd__lib",
        &rewrite_tu,
        &frag_depends,
    )
    .expect("plugin present ⇒ a check command");
    assert_eq!(
        some.stamp_out,
        layout.check_stamp_path("cstd", "cstd__lib"),
        "OUTPUT is the per-module .checked stamp"
    );
    assert_eq!(some.rewrite_tu, rewrite_tu, "DEPENDS the .rewrite TU");
    assert_eq!(some.plugin, plugin, "DEPENDS the plugin");
    assert_eq!(
        some.frag_depends, frag_depends,
        "DEPENDS the build-mode fragments + dep headers (CHK-D-5)"
    );
}
