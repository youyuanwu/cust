use crate::common::*;

// ─── Happy-path tests ───────────────────────────────────────────────

#[test]
fn build_hello_dev_produces_static_archive() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    let archive = dir.join("target/debug/build/hello/libhello.a");
    assert!(archive.is_file(), "{} missing", archive.display());

    // Per §17, `compile_commands.json` lives at `target/`, not
    // `target/<profile>/`.
    let cc = dir.join("target/compile_commands.json");
    assert!(cc.is_file(), "{} missing", cc.display());

    // Prelude materialised under the profile dir.
    let prelude = dir.join("target/debug/prelude.h");
    assert!(prelude.is_file(), "{} missing", prelude.display());

    // .cust-version stamp at target root.
    let stamp = dir.join("target/.cust-version");
    let stamp_contents = fs::read_to_string(&stamp).expect("read stamp");
    assert!(
        stamp_contents.starts_with("cust "),
        "unexpected stamp: {stamp_contents:?}"
    );
    assert!(
        stamp_contents.contains("clang version"),
        "stamp missing clang line: {stamp_contents:?}"
    );
}

#[test]
fn build_hello_release_uses_release_dir() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build", "--release"]);
    assert_success(&out);

    assert!(dir.join("target/release/build/hello/libhello.a").is_file());
    // Dev profile dir should NOT have been created.
    assert!(!dir.join("target/debug").exists());
}

#[test]
fn check_hello_succeeds() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["check"]);
    assert_success(&out);
}

#[test]
fn check_fails_on_type_error() {
    // incremental-check CHK-D-1: `cust check` is now an
    // error-reporting pass — a type error in a lib module must fail
    // the check (exit non-zero) with the clang diagnostic, the case
    // the old tolerant surface pass (V42D-15) silently passed. The
    // error surfaces through the `cmake --build` phase (Ninja),
    // which cust echoes — so search combined stdout+stderr.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("hello");
    // A clean check first proves the baseline passes.
    assert_success(&cust(&dir, ["check"]));
    // Inject a return-type error into the lib module.
    let lib = dir.join("src/lib.c");
    let mut src = fs::read_to_string(&lib).unwrap();
    src.push_str("\n[[cust::pub]] int32_t hello_broken(void) { return \"nope\"; }\n");
    fs::write(&lib, src).unwrap();
    let out = cust(&dir, ["check"]);
    assert_build_failure_with(&out, "hello_broken");
}

/// incremental-check helper: mtime of a per-module `.checked`
/// stamp (`target/debug/.check/<crate>/<qname>.checked`).
fn check_stamp_mtime(dir: &Path, crate_name: &str, qname: &str) -> std::time::SystemTime {
    let p = dir
        .join("target/debug/.check")
        .join(crate_name)
        .join(format!("{qname}.checked"));
    fs::metadata(&p)
        .unwrap_or_else(|e| panic!("stat {}: {e}", p.display()))
        .modified()
        .unwrap()
}

#[test]
fn check_noop_does_no_work() {
    // incremental-check CHK-D-7: a second `cust check` with no edits
    // runs no check command — Ninja reports nothing to do, so no
    // `.checked` stamp is (re)generated. Probe: the no-op run's
    // output mentions no `.checked` generation.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["check"]));
    let out = cust(&dir, ["check"]);
    assert_success(&out);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        !combined.contains(".checked"),
        "no-op check re-ran a check command:\n{combined}"
    );
}

#[test]
fn check_single_module_incrementality() {
    // incremental-check CHK-D-8: editing one module's body re-fires
    // that module's check command and nothing unrelated. `util`'s
    // body change re-checks `util` but leaves the unrelated
    // `parser` module's stamp untouched.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["check"]));
    let parser_before = check_stamp_mtime(&dir, "multi_module", "parser");
    let util_before = check_stamp_mtime(&dir, "multi_module", "util");

    // Distinct mtimes need a tick of wall-clock separation (stamp
    // mtime has second granularity on most filesystems).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let util_src = dir.join("src/util.c");
    let edited = fs::read_to_string(&util_src)
        .unwrap()
        .replace("return 42;", "return 43;");
    fs::write(&util_src, edited).unwrap();
    assert_success(&cust(&dir, ["check"]));

    assert_ne!(
        util_before,
        check_stamp_mtime(&dir, "multi_module", "util"),
        "edited module's check stamp must be refreshed"
    );
    assert_eq!(
        parser_before,
        check_stamp_mtime(&dir, "multi_module", "parser"),
        "unrelated module's check stamp must stay untouched"
    );
}

#[test]
fn build_fires_no_check_work() {
    // incremental-check CHK-D-4 / verification item 5: `cust_check`
    // is EXCLUDE_FROM_ALL, so `cust build` never fires a check
    // command — no `.checked` stamp is produced by a pure build.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["build"]));
    let check_root = dir.join("target/debug/.check");
    let stamps: Vec<PathBuf> = if check_root.is_dir() {
        walk_files(&check_root)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "checked"))
            .collect()
    } else {
        Vec::new()
    };
    assert!(
        stamps.is_empty(),
        "cust build produced check stamps: {stamps:?}"
    );
}

#[test]
fn clean_removes_target_dir() {
    let (_tmp, dir) = stage("hello");
    assert_success(&cust(&dir, ["build"]));
    assert!(dir.join("target").exists());

    assert_success(&cust(&dir, ["clean"]));
    assert!(!dir.join("target").exists());
}

#[test]
fn clean_is_idempotent_when_target_absent() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["clean"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Nothing to clean"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn compile_commands_json_carries_expected_flags() {
    let (_tmp, dir) = stage("hello");
    assert_success(&cust(&dir, ["build"]));

    // v0.4.2 V42D-12: compile_commands.json is emitted by CMake
    // and the cust driver publishes the legacy `target/`
    // location as a symlink.
    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    for needle in [
        "-fvisibility=hidden",
        "-include",
        "prelude.h",
        "-O0",
        "-g3",
        // The compiled file is the post-`#cust use`-rewrite copy
        // under target/<profile>/.rewrite/<crate>/ (V42D-13).
        "/.rewrite/hello/src/lib.c",
    ] {
        assert!(
            cc.contains(needle),
            "compile_commands.json missing {needle:?}:\n{cc}",
        );
    }
}

#[test]
fn discovers_manifest_from_subdirectory() {
    let (_tmp, dir) = stage("hello");
    let src_dir = dir.join("src");
    let out = cust(&src_dir, ["build"]);
    assert_success(&out);
    // Artifacts land next to Cust.toml (the crate root), NOT next
    // to the cwd we invoked from.
    assert!(dir.join("target/debug/build/hello/libhello.a").is_file());
}
