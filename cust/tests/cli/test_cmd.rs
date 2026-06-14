use crate::common::*;

// ─── Slice D — v0.3.2 `cust test` end-to-end ──────────────────────
//
// These tests drive the public `cust test` subcommand. The
// command builds every testable workspace member's test binary
// (lib or lib+bin; bin-only members are V32D-12 skipped, or
// V32D-11 rejected with `-p`), then runs each in turn.
// `cust test` itself captures stdout/stderr from the spawned
// binaries via process inheritance, so we assert against
// `cust test`'s own combined output.

#[test]
fn test_subcommand_produces_binary_at_expected_path() {
    let (_tmp, dir) = stage("with_tests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);

    let exe = dir.join("target/debug/test/with_tests/with_tests");
    assert!(exe.is_file(), "test binary missing at {}", exe.display());
    // V32D-4: test build is fully isolated from the normal
    // build tree — no archive, no per-build dir for the
    // lib half.
    assert!(
        !dir.join("target/debug/build/with_tests/libwith_tests.a")
            .exists(),
        "test build should not produce the non-test archive",
    );
}

#[test]
fn test_subcommand_runs_all_tests_with_cargo_shape() {
    let (_tmp, dir) = stage("with_tests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Cargo-shape header + per-test lines + summary.
    assert!(stdout.contains("running 3 tests"), "{stdout}");
    assert!(stdout.contains("test test_add_basic ... ok"), "{stdout}");
    assert!(
        stdout.contains("test test_mul_void_kind ... ok"),
        "{stdout}",
    );
    assert!(stdout.contains("test test_skipped ... ignored"), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 2 passed; 0 failed; 1 ignored"),
        "{stdout}",
    );
}

#[test]
fn test_subcommand_substring_filter() {
    let (_tmp, dir) = stage("with_tests");
    let out = cust(&dir, ["test", "mul"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.contains("running 1 tests"), "{stdout}");
    assert!(
        stdout.contains("test test_mul_void_kind ... ok"),
        "{stdout}",
    );
    // The other two tests should be filtered out, not run.
    assert!(
        !stdout.contains("... ok\n") || stdout.matches("... ok").count() == 1,
        "{stdout}"
    );
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored; 2 filtered out"),
        "{stdout}",
    );
}

#[test]
fn test_subcommand_list_mode() {
    let (_tmp, dir) = stage("with_tests");
    // `--list` is a runner flag; forwarded via `--`.
    let out = cust(&dir, ["test", "--", "--list"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test_add_basic: test"), "{stdout}");
    assert!(stdout.contains("test_mul_void_kind: test"), "{stdout}");
    assert!(stdout.contains("test_skipped: test"), "{stdout}");
    assert!(stdout.contains("3 tests, 0 benchmarks"), "{stdout}");
}

#[test]
fn test_subcommand_failure_isolated_by_fork() {
    // Synthesize a fixture inline with a deliberately failing
    // test sandwiched between passing ones. The runner must
    // execute every test, mark only the failing one FAILED,
    // and `cust test` must exit 1.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("isolation");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cust.toml"),
        "[package]\nname = \"isolation\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        "[[cust::pub]] int answer(void) { return 42; }\n\
         [[cust::test]] int test_pass_first(void) { cust_assert_eq(answer(), 42); return 0; }\n\
         [[cust::test]] int test_will_fail(void)  { cust_assert_eq(answer(), 0); return 0; }\n\
         [[cust::test]] int test_pass_last(void)  { cust_assert_eq(answer(), 42); return 0; }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Fork isolation: the failing test does not stop the others.
    assert!(stdout.contains("test test_pass_first ... ok"), "{stdout}");
    assert!(
        stdout.contains("test test_will_fail ... FAILED"),
        "{stdout}"
    );
    assert!(stdout.contains("test test_pass_last ... ok"), "{stdout}");
    // Summary reflects the one failure.
    assert!(
        stdout.contains("test result: FAILED. 2 passed; 1 failed; 0 ignored"),
        "{stdout}",
    );
    // The forked subprocess writes the assertion message to
    // stderr (cust_panic_impl -> stderr).
    assert!(
        stderr.contains("assertion failed: `(answer()) == (0)`"),
        "{stderr}",
    );
    // `cust test` exits 1 when any member's test binary fails.
    assert_eq!(
        out.status.code(),
        Some(1),
        "wanted exit 1, got {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        out.status.code(),
    );
}

#[test]
fn test_subcommand_rejects_bin_only_with_dash_p() {
    // V32D-11: explicit `-p <bin-only>` is a clear error.
    let (_tmp, dir) = stage("bin_only");
    let out = cust(&dir, ["test", "-p", "bin_only"]);
    assert_failure_with(&out, "is a bin-only crate");
    assert_failure_with(&out, "cust test v0.3.2 only runs unit tests");
}

#[test]
fn test_subcommand_silently_skips_bin_only_workspace_member() {
    // V32D-12: bare `cust test` on a workspace mixing a bin
    // member and a lib member should silently skip the bin and
    // still report success when the lib's tests pass.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mixed");
    fs::create_dir_all(dir.join("lib_member/src")).unwrap();
    fs::create_dir_all(dir.join("bin_member/src")).unwrap();
    fs::write(
        dir.join("Cust.toml"),
        "[workspace]\nmembers = [\"lib_member\", \"bin_member\"]\n",
    )
    .unwrap();
    fs::write(
        dir.join("lib_member/Cust.toml"),
        "[package]\nname = \"lib_member\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("lib_member/src/lib.c"),
        "[[cust::pub]] int answer(void) { return 42; }\n\
         [[cust::test]] int test_basic(void) { cust_assert_eq(answer(), 42); return 0; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("bin_member/Cust.toml"),
        "[package]\nname = \"bin_member\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("bin_member/src/main.c"),
        "[[cust::pub]] int cust_main(void) { return 0; }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Lib member's tests ran:
    assert!(stdout.contains("test test_basic ... ok"), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored"),
        "{stdout}",
    );
    // bin_member should NOT appear in the run output — there
    // should be no `Running …/bin_member/bin_member` line.
    assert!(
        !stdout.contains("test/bin_member/bin_member"),
        "bin-only member should be silently skipped, got:\n{stdout}",
    );
}

#[test]
fn test_subcommand_lib_and_bin_tests_lib_half_only() {
    // V32D-11 carve-out: a lib+bin crate tests the lib half.
    let (_tmp, dir) = stage("lib_and_bin");
    // The existing fixture has no [[cust::test]] functions; we add
    // one to the lib half to confirm the test binary discovers
    // it. The bin half's `cust_main` must NOT be reachable
    // from the test binary (it would clash with the runner's
    // own `main`).
    let lib = dir.join("src/lib.c");
    let mut lib_src = fs::read_to_string(&lib).unwrap();
    lib_src.push_str(
        "\n\
         [[cust::test]] int test_demo_answer(void) { cust_assert_eq(demo_answer(), 42); return 0; }\n",
    );
    fs::write(&lib, lib_src).unwrap();

    let out = cust(&dir, ["test", "-p", "demo"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test test_demo_answer ... ok"), "{stdout}");
}

#[test]
fn test_subcommand_zero_tests_succeeds() {
    // A library member with no [[cust::test]] functions produces a
    // valid (zero-test) binary. The runner prints
    // `running 0 tests` and `test result: ok. 0 passed; 0
    // failed; 0 ignored`, exit 0. Cargo parity.
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("running 0 tests"), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 0 passed; 0 failed; 0 ignored"),
        "{stdout}",
    );
}

// ─── v0.4.3 integration tests (tests/ directory) ────────────────

#[test]
fn itest_runs_unit_and_integration_with_banners() {
    // V43D-1/V43D-4/V43D-5: `cust test` runs the unit test in
    // src/ plus the two integration exes under tests/, in
    // stem-sorted order (basic before extra), each with its own
    // Cargo-shape banner.
    let (_tmp, dir) = stage("with_itests");
    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Unit test banner + the src/ test.
    assert!(
        stdout.contains("Running unittests") || stdout.contains("test/with_itests/with_itests"),
        "missing unit-test run banner:\n{stdout}"
    );
    assert!(
        stdout.contains("test test_secret_is_seven ... ok"),
        "{stdout}"
    );

    // Integration banners use the `tests/<file>.c (<exe>)` shape
    // (V43D-1), and exes land under the per-stem dir (V43D-5).
    assert!(
        stdout.contains("Running tests/basic.c"),
        "missing basic.c banner:\n{stdout}"
    );
    assert!(
        stdout.contains("Running tests/extra.c"),
        "missing extra.c banner:\n{stdout}"
    );
    assert!(
        stdout.contains("test/with_itests/basic/basic"),
        "basic exe not at per-stem path:\n{stdout}"
    );

    // Integration test fns ran (bare names, root module dropped).
    assert!(
        stdout.contains("test test_add_via_public ... ok"),
        "{stdout}"
    );
    assert!(
        stdout.contains("test test_mul_via_public ... ok"),
        "{stdout}"
    );
    assert!(stdout.contains("test test_add_again ... ok"), "{stdout}");

    // Stem-sort order: basic.c banner precedes extra.c banner
    // (V43D-1 deterministic run order).
    let basic_at = stdout.find("Running tests/basic.c").unwrap();
    let extra_at = stdout.find("Running tests/extra.c").unwrap();
    assert!(
        basic_at < extra_at,
        "integration exes not stem-sorted:\n{stdout}"
    );
}

#[test]
fn itest_exes_land_at_per_stem_paths() {
    // V43D-5/V43D-11: one exe per file at
    // target/debug/test/<crate>/<stem>/<stem>, so the exe file
    // and its per-stem cwd directory coexist.
    let (_tmp, dir) = stage("with_itests");
    assert_success(&cust(&dir, ["test"]));

    for stem in ["basic", "extra"] {
        let exe = dir.join(format!("target/debug/test/with_itests/{stem}/{stem}"));
        assert!(
            exe.is_file(),
            "integration exe missing at {}",
            exe.display()
        );
    }
}

#[test]
fn itest_failure_sets_exit_one() {
    // V43D-10: a failing integration test makes `cust test`
    // exit 1, even when unit tests pass.
    let (_tmp, dir) = stage("with_itests");
    fs::write(
        dir.join("tests/failing.c"),
        "#cust use with_itests;\n\
         [[cust::test]] int test_fails(void) { cust_assert_eq(add(1, 1), 99); return 0; }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test test_fails ... FAILED"), "{stdout}");
    assert_eq!(
        out.status.code(),
        Some(1),
        "wanted exit 1\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn itest_cannot_reach_crate_private_symbol() {
    // V43D-3: integration tests link the public surface only.
    // Referencing a crate-private `static` helper (not in
    // <crate>.h) fails to compile.
    let (_tmp, dir) = stage("with_itests");
    fs::write(
        dir.join("tests/reach.c"),
        "#cust use with_itests;\n\
         [[cust::test]] int test_reach(void) { return secret(); }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    assert!(
        !out.status.success(),
        "expected compile failure reaching a crate-private symbol"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("secret"),
        "expected an error mentioning `secret`:\n{combined}"
    );
}

#[test]
fn itest_incremental_rebuild_isolated_per_file() {
    // V43D verification #3: editing one integration file and
    // re-running rebuilds only that exe; the sibling's exe is
    // untouched (Ninja incremental). We assert correctness here
    // (both still pass after editing basic.c); fine-grained
    // mtime checks are covered by the Ninja graph itself.
    let (_tmp, dir) = stage("with_itests");
    assert_success(&cust(&dir, ["test"]));

    // Add a third test fn to basic.c and re-run.
    fs::write(
        dir.join("tests/basic.c"),
        "#cust use with_itests;\n\
         [[cust::test]] int test_add_via_public(void) { cust_assert_eq(add(2, 3), 5); return 0; }\n\
         [[cust::test]] void test_mul_via_public(void) { cust_assert(mul(3, 4) == 12); }\n\
         [[cust::test]] int test_added_later(void) { cust_assert_eq(add(0, 0), 0); return 0; }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test test_added_later ... ok"), "{stdout}");
    assert!(stdout.contains("test test_add_again ... ok"), "{stdout}");
}

#[test]
fn itest_subdirectories_are_ignored() {
    // V43D-1 no-recursion + V43D-2 deferral: a tests/sub/ dir is
    // silently ignored — no error, no extra exe.
    let (_tmp, dir) = stage("with_itests");
    fs::create_dir_all(dir.join("tests/sub")).unwrap();
    fs::write(
        dir.join("tests/sub/nested.c"),
        "#cust use with_itests;\n\
         [[cust::test]] int test_nested(void) { return 0; }\n",
    )
    .unwrap();

    let out = cust(&dir, ["test"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The nested file is NOT run as its own exe.
    assert!(
        !stdout.contains("tests/sub/nested.c"),
        "subdirectory file should be ignored:\n{stdout}"
    );
    assert!(
        !dir.join("target/debug/test/with_itests/nested").exists(),
        "no exe should be emitted for a subdirectory test file"
    );
}

#[test]
fn test_build_excludes_cust_test_symbols_from_normal_archive() {
    // Build the with_tests fixture in NORMAL (non-test) mode and
    // confirm the resulting libwith_tests.a contains the [[cust::pub]]
    // functions but NOT any test_* symbol. V40D-14: [[cust::test]]
    // decays to `static unused` in non-test builds.
    let (_tmp, dir) = stage("with_tests");
    assert_success(&cust(&dir, ["build"]));

    let archive = dir.join("target/debug/build/with_tests/libwith_tests.a");
    assert!(archive.is_file());

    let nm = Command::new("nm")
        .arg(&archive)
        .stdin(Stdio::null())
        .output()
        .expect("spawn nm");
    let symbols = String::from_utf8_lossy(&nm.stdout);

    // [[cust::pub]] functions present:
    assert!(
        symbols.contains("add"),
        "expected `add` in archive:\n{symbols}"
    );
    assert!(
        symbols.contains("mul"),
        "expected `mul` in archive:\n{symbols}"
    );
    // [[cust::test]] functions absent — they're static unused outside
    // the test build.
    for needle in ["test_add_basic", "test_mul_void_kind", "test_skipped"] {
        assert!(
            !symbols.contains(needle),
            "test fn `{needle}` leaked into the non-test archive:\n{symbols}",
        );
    }
}

// ─── v0.4.4 multi-bin (src/bin/*.c, [[bin]] arrays) ─────────────

#[test]
fn multibin_build_produces_all_executables() {
    // V44D-1/V44D-5: `src/main.c` (package bin `multibin`) +
    // `src/bin/extra.c` (extra bin `extra`) both build from one
    // `cust build`, landing at target/debug/{multibin,extra}.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_bin");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    assert!(
        dir.join("target/debug/multibin").is_file(),
        "missing package bin"
    );
    assert!(
        dir.join("target/debug/extra").is_file(),
        "missing extra bin"
    );
    // The lib half is still published as an archive.
    assert!(dir
        .join("target/debug/build/multibin/libmultibin.a")
        .is_file());
}

#[test]
fn multibin_run_without_bin_is_ambiguous() {
    // V44D-6: a member with >1 bin and no `--bin` errors with the
    // Cargo-shape "could not determine which binary to run".
    let (_tmp, dir) = stage("multi_bin");
    let out = cust(&dir, ["run"]);
    assert_failure_with(&out, "could not determine which binary to run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Both bin names are listed, sorted.
    assert!(
        stderr.contains("`extra`") && stderr.contains("`multibin`"),
        "{stderr}"
    );
}

#[test]
fn multibin_run_bin_selects_executable() {
    // V44D-6: `cust run --bin <name>` runs the right exe.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_bin");
    let out = cust(&dir, ["run", "--bin", "extra"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("extra: answer = 42"), "{stdout}");
    assert!(!stdout.contains("main: answer"), "ran wrong bin:\n{stdout}");
}

#[test]
fn multibin_run_unknown_bin_is_error() {
    // V44D-6: an unknown `--bin` lists the available names.
    let (_tmp, dir) = stage("multi_bin");
    let out = cust(&dir, ["run", "--bin", "nope"]);
    assert_failure_with(&out, "no binary named `nope`");
}

#[test]
fn multibin_build_bin_scopes_to_one() {
    // V44D-7: `cust build --bin extra` builds only that target.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_bin");
    let out = cust(&dir, ["build", "--bin", "extra"]);
    assert_success(&out);
    assert!(dir.join("target/debug/extra").is_file());
    // The Finished line reports only the scoped bin.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("extra"), "{stdout}");
    assert!(
        !stdout.contains("-> ") || !stdout.contains("multibin\n"),
        "{stdout}"
    );
}

#[test]
fn multibin_src_bin_subdirs_ignored() {
    // V44D-1 no-recursion / V44D-2 deferral: a `src/bin/sub/`
    // directory produces no bin and no error.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_bin");
    let sub = dir.join("src/bin/sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(
        sub.join("nested.c"),
        "[[cust::pub]] int cust_main(void){return 0;}\n",
    )
    .unwrap();
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    // No `nested` exe emitted.
    assert!(
        !dir.join("target/debug/nested").exists(),
        "subdir bin leaked"
    );
}
