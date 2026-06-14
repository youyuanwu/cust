use crate::common::*;

// ─── v0.4.5: hidden `cust internal` leaf generators (slice A) ──────

/// Build the staged `cross_module_typedef` fixture and return the
/// canonicalised crate dir (so generated paths match the driver's
/// canonicalised layout). Skips by returning `None` when the
/// plugin isn't built.
fn build_cmt_fixture() -> Option<(TempDir, PathBuf)> {
    plugin_path()?;
    let (tmp, dir) = stage("cross_module_typedef");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    let canon = dir.canonicalize().expect("canonicalise crate dir");
    Some((tmp, canon))
}

#[test]
fn internal_rewrite_file_matches_build_output() {
    // V45D-2/V45D-3: the hidden `rewrite-file` leaf produces a
    // rewrite byte-identical to the driver's in-process
    // `write_rewrite_tree`, because both call `generate::rewrite_one`.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let in_process = dir.join("target/debug/.rewrite/cross_module_typedef/src/mem.c");
    let expected = fs::read(&in_process).expect("in-process rewrite of mem.c");

    let frags_dir = dir.join("target/debug/.h-fragments/cross_module_typedef");
    let deps_root = dir.join("target/debug/deps");
    let own_lib_header =
        dir.join("target/debug/build/cross_module_typedef/include/cross_module_typedef.h");
    let leaf_out = dir.join("target/debug/leaf-mem.c");
    let out = cust(
        &dir,
        [
            "internal",
            "rewrite-file",
            "--crate-name",
            "cross_module_typedef",
            "--in",
            dir.join("src/mem.c").to_str().unwrap(),
            "--out",
            leaf_out.to_str().unwrap(),
            "--frags-dir",
            frags_dir.to_str().unwrap(),
            "--deps-root",
            deps_root.to_str().unwrap(),
            "--own-lib-header",
            own_lib_header.to_str().unwrap(),
            "--has-lib",
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_out).expect("leaf rewrite output");
    assert_eq!(
        got, expected,
        "rewrite-file leaf output differs from in-process rewrite"
    );
}

#[test]
fn internal_crate_header_matches_build_output() {
    // V45D-2/V45D-5: the `crate-header` leaf concatenates the same
    // bytes as the in-process `write_crate_header`.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let in_process =
        dir.join("target/debug/build/cross_module_typedef/include/cross_module_typedef.h");
    let expected = fs::read(&in_process).expect("in-process crate header");

    // Fragments in topological order: types (imported by mem) must
    // precede mem; lib is the root. The driver's topo order for
    // this crate is [lib, types, mem] (discovery order with the
    // types→mem edge already satisfied).
    let frags_dir = dir.join("target/debug/.h-fragments/cross_module_typedef");
    let leaf_out = dir.join("target/debug/leaf-header.h");
    let out = cust(
        &dir,
        [
            "internal",
            "crate-header",
            "--crate-name",
            "cross_module_typedef",
            "--out",
            leaf_out.to_str().unwrap(),
            "--frag",
            frags_dir.join("lib.cust.h").to_str().unwrap(),
            "--frag",
            frags_dir.join("types.cust.h").to_str().unwrap(),
            "--frag",
            frags_dir.join("mem.cust.h").to_str().unwrap(),
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_out).expect("leaf header output");
    assert_eq!(
        got, expected,
        "crate-header leaf output differs from in-process header"
    );
}

#[test]
fn internal_surface_module_matches_build_output() {
    // V45D-2/V45D-4: the `surface-module` leaf reproduces the
    // driver's fragment for a module byte-for-byte (same
    // `generate::surface_one_module` + `build_cflags_raw`).
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let plugin = plugin_path().unwrap();
    let in_process = dir.join("target/debug/.h-fragments/cross_module_typedef/types.cust.h");
    let expected = fs::read(&in_process).expect("in-process types fragment");

    let frags_dir = dir.join("target/debug/.h-fragments/cross_module_typedef");
    let deps_root = dir.join("target/debug/deps");
    let prelude = dir.join("target/debug/prelude.h");
    let leaf_frag = dir.join("target/debug/leaf-types.cust.h");
    let surface_out = dir.join("target/debug/leaf-types.surface.c");
    let out = cust(
        &dir,
        [
            "internal",
            "surface-module",
            "--source",
            dir.join("src/types.c").to_str().unwrap(),
            "--surface-out",
            surface_out.to_str().unwrap(),
            "--fragment-out",
            leaf_frag.to_str().unwrap(),
            "--frags-dir",
            frags_dir.to_str().unwrap(),
            "--deps-root",
            deps_root.to_str().unwrap(),
            "--std",
            "c23",
            "--cflag",
            "-O0",
            "--cflag",
            "-g3",
            "--cflag",
            "-gdwarf-5",
            "--include",
            dir.join("src").to_str().unwrap(),
            "--prelude",
            prelude.to_str().unwrap(),
            "--plugin",
            plugin.to_str().unwrap(),
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_frag).expect("leaf fragment output");
    assert_eq!(
        got, expected,
        "surface-module leaf fragment differs from in-process fragment"
    );
}

#[test]
fn internal_surface_module_requires_upstream_fragment() {
    // V45D-4 (verification item 12): the one-shot leaf hard-errors
    // when an imported fragment is absent (a missing DEPENDS edge),
    // rather than silently blanking the include.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let plugin = plugin_path().unwrap();
    // Point `--frags-dir` at an empty dir so `types.cust.h` (which
    // `mem` imports) is absent.
    let empty_frags = dir.join("target/debug/empty-frags");
    fs::create_dir_all(&empty_frags).unwrap();
    let deps_root = dir.join("target/debug/deps");
    let prelude = dir.join("target/debug/prelude.h");
    let leaf_frag = dir.join("target/debug/leaf-mem.cust.h");
    let surface_out = dir.join("target/debug/leaf-mem.surface.c");
    let out = cust(
        &dir,
        [
            "internal",
            "surface-module",
            "--source",
            dir.join("src/mem.c").to_str().unwrap(),
            "--surface-out",
            surface_out.to_str().unwrap(),
            "--fragment-out",
            leaf_frag.to_str().unwrap(),
            "--frags-dir",
            empty_frags.to_str().unwrap(),
            "--deps-root",
            deps_root.to_str().unwrap(),
            "--std",
            "c23",
            "--cflag",
            "-O0",
            "--prelude",
            prelude.to_str().unwrap(),
            "--plugin",
            plugin.to_str().unwrap(),
        ],
    );
    assert_failure_with(&out, "does not exist on disk");
}

// ─── v0.4.6: hidden `cust internal test-{sidecar,runner}` (slice A) ─

#[test]
fn internal_test_sidecar_unit_matches_build_output() {
    // V46D-1/V46D-6: the `test-sidecar --kind unit` leaf produces a
    // `.cust.tests` sidecar byte-identical to the one the driver's
    // in-process test surface pass writes. Both run the same
    // `generate::sidecar_one`; the fragment flag the build-mode pass
    // also sets doesn't affect sidecar bytes (V46D-7).
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_tests");
    // `cust test` populates the in-process unit sidecar (driver-side
    // this milestone — slice A doesn't move it yet).
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let dir = dir.canonicalize().expect("canonicalise crate dir");
    let plugin = plugin_path().unwrap();

    let in_process = dir.join("target/debug/.test-discovery/with_tests/lib.cust.tests");
    let expected = fs::read(&in_process).expect("in-process unit sidecar");

    let frags_dir = dir.join("target/debug/.h-fragments/with_tests");
    let deps_root = dir.join("target/debug/deps");
    let prelude = dir.join("target/debug/prelude.h");
    let leaf_sidecar = dir.join("target/debug/leaf-lib.cust.tests");
    let surface_out = dir.join("target/debug/leaf-lib.surface.c");
    let out = cust(
        &dir,
        [
            "internal",
            "test-sidecar",
            "--crate-name",
            "with_tests",
            "--kind",
            "unit",
            "--module",
            "lib",
            "--source",
            dir.join("src/lib.c").to_str().unwrap(),
            "--surface-out",
            surface_out.to_str().unwrap(),
            "--sidecar-out",
            leaf_sidecar.to_str().unwrap(),
            "--frags-dir",
            frags_dir.to_str().unwrap(),
            "--deps-root",
            deps_root.to_str().unwrap(),
            "--std",
            "c23",
            "--cflag",
            "-O0",
            "--cflag",
            "-g3",
            "--cflag",
            "-gdwarf-5",
            "--cflag",
            "-Wall",
            "--cflag",
            "-Wextra",
            "--include",
            dir.join("src").to_str().unwrap(),
            "--prelude",
            prelude.to_str().unwrap(),
            "--plugin",
            plugin.to_str().unwrap(),
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_sidecar).expect("leaf unit sidecar");
    assert_eq!(
        got, expected,
        "test-sidecar unit leaf differs from in-process sidecar"
    );
}

#[test]
fn internal_test_sidecar_integration_matches_build_output() {
    // V46D-1/V46D-6: the `test-sidecar --kind integration` leaf
    // produces a sidecar byte-identical to the driver's in-process
    // `surface_pass_integration` (both call `generate::sidecar_one`
    // with `surface_out = None` on the already-rewritten TU).
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_itests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let dir = dir.canonicalize().expect("canonicalise crate dir");
    let plugin = plugin_path().unwrap();

    let in_process = dir.join("target/debug/.test-discovery/with_itests/tests/basic.cust.tests");
    let expected = fs::read(&in_process).expect("in-process integration sidecar");

    // The leaf surface-passes the *rewritten* test TU (V46D-3).
    let rewritten = dir.join("target/debug/.rewrite/with_itests/tests/basic.c");
    let frags_dir = dir.join("target/debug/.h-fragments/with_itests");
    let deps_root = dir.join("target/debug/deps");
    let prelude = dir.join("target/debug/prelude.h");
    let leaf_sidecar = dir.join("target/debug/leaf-basic.cust.tests");
    let out = cust(
        &dir,
        [
            "internal",
            "test-sidecar",
            "--crate-name",
            "with_itests",
            "--kind",
            "integration",
            "--source",
            rewritten.to_str().unwrap(),
            "--sidecar-out",
            leaf_sidecar.to_str().unwrap(),
            "--frags-dir",
            frags_dir.to_str().unwrap(),
            "--deps-root",
            deps_root.to_str().unwrap(),
            "--std",
            "c23",
            "--cflag",
            "-O0",
            "--cflag",
            "-g3",
            "--cflag",
            "-gdwarf-5",
            "--cflag",
            "-Wall",
            "--cflag",
            "-Wextra",
            "--prelude",
            prelude.to_str().unwrap(),
            "--plugin",
            plugin.to_str().unwrap(),
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_sidecar).expect("leaf integration sidecar");
    assert_eq!(
        got, expected,
        "test-sidecar integration leaf differs from in-process sidecar"
    );
}

#[test]
fn internal_test_runner_matches_build_output() {
    // V46D-1: the `test-runner` leaf renders a runner TU
    // byte-identical to the driver's in-process
    // `write_test_runner_tu` (both call `generate::write_runner_tu`).
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_tests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let dir = dir.canonicalize().expect("canonicalise crate dir");

    let in_process = dir.join("target/debug/cmake/cust_test_main_with_tests.c");
    let expected = fs::read(&in_process).expect("in-process runner TU");

    let sidecar = dir.join("target/debug/.test-discovery/with_tests/lib.cust.tests");
    let leaf_out = dir.join("target/debug/leaf-runner.c");
    let out = cust(
        &dir,
        [
            "internal",
            "test-runner",
            "--crate-name",
            "with_tests",
            "--out",
            leaf_out.to_str().unwrap(),
            "--sidecar",
            sidecar.to_str().unwrap(),
        ],
    );
    assert_success(&out);
    let got = fs::read(&leaf_out).expect("leaf runner output");
    assert_eq!(
        got, expected,
        "test-runner leaf differs from in-process runner"
    );
}

#[test]
fn test_build_reuses_build_mode_fragment() {
    // V46D-7 guard: a module's published surface fragment is
    // identical whether produced by `cust build` (build-mode) or
    // `cust test` (`-DCUST_TEST_BUILD=1`). If a future change ever
    // makes pub surface conditional on `CUST_TEST_BUILD`, this fails
    // loudly — that is the day RQ-V46-3 (namespaced test fragments)
    // becomes necessary. Both passes write the same fragment path,
    // so equal bytes ⇒ the test build can safely reuse the
    // build-mode fragment (V46D-7) instead of regenerating it.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_tests");

    let out = cust(&dir, ["build"]);
    assert_success(&out);
    let frag = dir.join("target/debug/.h-fragments/with_tests/lib.cust.h");
    let build_mode = fs::read(&frag).expect("build-mode fragment");

    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let test_mode = fs::read(&frag).expect("test-mode fragment");

    assert_eq!(
        build_mode, test_mode,
        "build-mode and test-mode fragments differ — pub surface must \
         not depend on CUST_TEST_BUILD (RQ-V46-3)"
    );
}

// ─── v0.4.5: slice C — incremental generation properties ─────────

/// Like `cust` but with extra environment variables applied. Used
/// to point `CUST_TRACE_INTERNAL` at a scratch trace file so a test
/// can observe which `internal` leaves a build spawned (V45D-12).
fn cust_env<I, S>(crate_dir: &Path, args: I, envs: &[(&str, &Path)]) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(CUST_BIN);
    cmd.args(args).current_dir(crate_dir).stdin(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn cust")
}

#[test]
fn noop_build_spawns_zero_internal_leaves() {
    // V45D-12 (verification item 2): a second `cust build` with no
    // source change must spawn zero `internal` codegen leaves. The
    // trace file named by `CUST_TRACE_INTERNAL` stays untouched
    // (never created) because no rewrite-file / surface-module /
    // crate-header command re-fires.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let trace = dir.join("target/debug/noop-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["build"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    assert!(
        !trace.is_file(),
        "no-op build spawned internal leaves (trace file was written):\n{}",
        fs::read_to_string(&trace).unwrap_or_default()
    );
}

#[test]
fn crate_header_republishes_via_all_anchor() {
    // V45D-14 (verification item 11): the published `<crate>.h` is
    // anchored by `add_custom_target(<crate>_header ALL)`, not by
    // any compile target consuming it as a tracked CMake input.
    // Deleting it and rebuilding must regenerate it — proving the
    // ALL anchor keeps the header in the default build graph.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    let header = dir.join("target/debug/build/cross_module_typedef/include/cross_module_typedef.h");
    assert!(header.is_file(), "crate header should exist after build");
    fs::remove_file(&header).expect("remove published header");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    assert!(
        header.is_file(),
        "crate header was not republished by the ALL anchor after deletion"
    );
}

#[test]
fn extra_cflag_change_refires_surface() {
    // V45D-15 (verification item 13): editing `[clang] extra-cflags`
    // changes the `surface-module` command line, so the next build
    // re-fires every surface command (no stale fragment). Observed
    // via the trace file — a `surface-module` leaf must appear.
    let Some((_tmp, dir)) = build_cmt_fixture() else {
        eprintln!("plugin not built — skipping");
        return;
    };
    // Append an `[clang] extra-cflags` entry to the manifest.
    let manifest = dir.join("Cust.toml");
    let mut toml = fs::read_to_string(&manifest).expect("read Cust.toml");
    toml.push_str("\n[clang]\nextra-cflags = [\"-DEXTRA_V045=1\"]\n");
    fs::write(&manifest, &toml).expect("write Cust.toml");

    let trace = dir.join("target/debug/cflag-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["build"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    let traced = fs::read_to_string(&trace).unwrap_or_default();
    assert!(
        traced.contains("surface-module"),
        "extra-cflags change did not re-fire any surface-module command:\n{traced}"
    );
}

// ─── v0.4.6: slice D — no-op test + single-module incrementality ──

#[test]
fn noop_test_build_spawns_zero_internal_leaves() {
    // V46D-8 (verification item 2): a second `cust test` with no
    // source change must spawn zero `internal` codegen leaves —
    // including the unit + integration test-sidecar / test-runner
    // commands. The trace file named by `CUST_TRACE_INTERNAL` stays
    // untouched on the second run.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_itests");
    // Cold test build populates every sidecar / runner / fragment.
    let out = cust(&dir, ["test"]);
    assert_success(&out);

    // Second `cust test`: nothing changed → zero codegen leaves.
    let trace = dir.join("target/debug/noop-test-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["test"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    assert!(
        !trace.is_file(),
        "no-op `cust test` spawned internal leaves (trace was written):\n{}",
        fs::read_to_string(&trace).unwrap_or_default()
    );
}

#[test]
fn cust_build_runs_zero_test_generation() {
    // V46D-4 (verification item 4): the test custom commands are
    // anchored only by the `EXCLUDE_FROM_ALL` test targets, so a
    // `cust build` (cold or no-op) must never fire a test-sidecar
    // or test-runner leaf — even for a crate that has unit and
    // integration tests.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_itests");
    let trace = dir.join("target/debug/build-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["build"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    let traced = fs::read_to_string(&trace).unwrap_or_default();
    assert!(
        !traced.contains("test-sidecar") && !traced.contains("test-runner"),
        "cust build fired test-generation leaves:\n{traced}"
    );
}

#[test]
fn single_module_test_edit_refires_only_that_sidecar() {
    // V46D-8 (verification item 3): editing one module's
    // `[[cust::test]]` body reruns that module's `test-sidecar` +
    // the crate `test-runner`, and not the sibling modules'
    // sidecars. Observed via the trace file.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("with_itests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);

    // Append a new unit test to the lib module's source.
    let lib = dir.join("src/lib.c");
    let mut src = fs::read_to_string(&lib).expect("read src/lib.c");
    src.push_str("\n[[cust::test]] int test_added_in_slice_d(void) { return 0; }\n");
    fs::write(&lib, &src).expect("write src/lib.c");

    let trace = dir.join("target/debug/edit-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["test"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    let traced = fs::read_to_string(&trace).unwrap_or_default();
    // The edited module's sidecar re-fired…
    assert!(
        traced.contains("test-sidecar") && traced.contains("lib.cust.tests"),
        "edited module's unit test-sidecar did not re-fire:\n{traced}"
    );
    // …and the crate runner re-aggregated.
    assert!(
        traced.contains("test-runner"),
        "crate test-runner did not re-fire after a test edit:\n{traced}"
    );
    // The new test actually runs (the runner picked it up).
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("test_added_in_slice_d"),
        "newly-added test was not discovered:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ─── v0.4.5: slice D — cyclic-SCC fallback (V45D-6) ──────────────

#[test]
fn pub_repr_cycle_builds_via_surface_cycle() {
    // V45D-6 (verification item 7): a `[[cust::pub_repr]]` import
    // cycle (modules `a` ↔ `b`) cannot be a fine-grained DAG (a
    // DEPENDS cycle is a hard CMake error), so the emitter coarsens
    // it into a single `internal surface-cycle` command. The crate
    // builds (the cycle converges via the fixed-point loop), and
    // both pub_repr structs reach the published header.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("pub_repr_cycle");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    // The generated graph carries one coarse cycle command whose
    // OUTPUT is *both* cycle fragments, and no fine-grained
    // surface-module command for `a` or `b` (only `lib`, the
    // acyclic singleton, stays fine-grained).
    let cmakelists = dir.join("target/debug/cmake/CMakeLists.txt");
    let cml = fs::read_to_string(&cmakelists).expect("read generated CMakeLists");
    assert!(
        cml.contains("internal surface-cycle"),
        "no surface-cycle command emitted for the 2-cycle:\n{cml}"
    );
    assert!(
        cml.contains("--module a --source") && cml.contains("--module b --source"),
        "surface-cycle command does not cover both cycle modules:\n{cml}"
    );
    // `a` / `b` must NOT each have their own surface-module command
    // (they are produced by the coarse cycle command instead); only
    // `lib` is fine-grained. Scope this to `internal surface-module`
    // command blocks — the per-module `internal test-sidecar`
    // commands (V46D-2) legitimately carry a `--source` for every
    // module, cycle members included, so a bare `--source` filter
    // would false-positive on them.
    let mut current_leaf = "";
    let mut surface_module_sources: Vec<&str> = Vec::new();
    for l in cml.lines() {
        if let Some(idx) = l.find("internal ") {
            current_leaf = l[idx + "internal ".len()..].trim();
        }
        if l.trim_start().starts_with("--source") && current_leaf == "surface-module" {
            surface_module_sources.push(l);
        }
    }
    assert!(
        surface_module_sources.iter().all(|l| l.contains("lib.c")),
        "a fine-grained surface-module command leaked for a cycle member:\n{surface_module_sources:?}"
    );

    // Both pub_repr structs reach the published crate header.
    let header = dir.join("target/debug/build/pub_repr_cycle/include/pub_repr_cycle.h");
    let h = fs::read_to_string(&header).expect("read published header");
    assert!(
        h.contains("struct ca {"),
        "ca struct missing from header:\n{h}"
    );
    assert!(
        h.contains("struct cb {"),
        "cb struct missing from header:\n{h}"
    );
}

#[test]
fn pub_repr_cycle_noop_build_is_incremental() {
    // The coarse cycle command participates in restat like any
    // other: a second build with no edits re-fires nothing
    // (V45D-12 holds for the cyclic path too).
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping");
        return;
    }
    let (_tmp, dir) = stage("pub_repr_cycle");
    assert_success(&cust(&dir, ["build"]));

    let trace = dir.join("target/debug/noop-cycle-trace.txt");
    let _ = fs::remove_file(&trace);
    let out = cust_env(&dir, ["build"], &[("CUST_TRACE_INTERNAL", &trace)]);
    assert_success(&out);
    assert!(
        !trace.is_file(),
        "no-op build of the cyclic crate spawned internal leaves:\n{}",
        fs::read_to_string(&trace).unwrap_or_default()
    );
}
