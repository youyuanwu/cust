use crate::common::*;

// ─── v0.3 workspace tests ───────────────────────────────────────────

#[test]
fn workspace_builds_all_members_in_topo_order() {
    // app depends on util via path. Build at the workspace root,
    // expect both libs in target/<profile>/build/<member>/ and a
    // dep view symlink at target/<profile>/deps/util/. Plugin-
    // dependent (the cross-crate header is what makes the build
    // work).
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_basic");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    // Per-member archives.
    let util_archive = dir.join("target/debug/build/util/libutil.a");
    let app_archive = dir.join("target/debug/build/app/libapp.a");
    assert!(util_archive.is_file(), "missing {}", util_archive.display());
    assert!(app_archive.is_file(), "missing {}", app_archive.display());

    // Per-member crate headers.
    let util_header = dir.join("target/debug/build/util/include/util.h");
    let app_header = dir.join("target/debug/build/app/include/app.h");
    assert!(util_header.is_file(), "missing {}", util_header.display());
    assert!(app_header.is_file(), "missing {}", app_header.display());

    // Dep view symlink: target/debug/deps/util -> target/debug/build/util.
    let dep_link = dir.join("target/debug/deps/util");
    let link_meta = fs::symlink_metadata(&dep_link).expect("dep symlink not created");
    assert!(
        link_meta.is_symlink(),
        "target/debug/deps/util is not a symlink (got {link_meta:?})"
    );
    let resolved = fs::read_link(&dep_link).unwrap();
    assert!(
        resolved.ends_with("target/debug/build/util"),
        "unexpected symlink target: {}",
        resolved.display()
    );

    // app's archive carries its own [[cust::pub]] symbol; util's
    // remains in util.a (not bundled — library deps are rlib-
    // style per scope item 8).
    let nm = Command::new("nm")
        .arg("--defined-only")
        .arg(&app_archive)
        .stdin(Stdio::null())
        .output()
        .expect("spawn nm");
    let app_syms = String::from_utf8_lossy(&nm.stdout);
    assert!(
        app_syms.contains("app_doubled"),
        "app archive missing app_doubled:\n{app_syms}"
    );

    let nm_util = Command::new("nm")
        .arg("--defined-only")
        .arg(&util_archive)
        .stdin(Stdio::null())
        .output()
        .expect("spawn nm");
    let util_syms = String::from_utf8_lossy(&nm_util.stdout);
    assert!(
        util_syms.contains("util_value"),
        "util archive missing util_value:\n{util_syms}"
    );
}

#[test]
fn workspace_dep_cycle_is_detected() {
    let (_tmp, dir) = stage("workspace_cycle");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "dependency cycle");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Cycle is rendered starting at alphabetically-first name (a).
    assert!(
        stderr.contains("a → b → a") || stderr.contains("a -> b -> a"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn cust_use_dep_without_dependency_entry_is_error() {
    // The workspace has only `app`. `app/src/lib.c` does
    // `#cust use util;` but app has no [dependencies] entry for
    // util. v0.4.5 V45D-3: the rewrite (and its validation) now
    // runs inside a `cust internal rewrite-file` custom command,
    // so the error surfaces through the `cmake --build` phase.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_undeclared_dep");
    let out = cust(&dir, ["build"]);
    assert_build_failure_with(&out, "`#cust use util;`");
    assert_build_failure_with(&out, "not listed in [dependencies]");
}

#[test]
fn workspace_check_runs_for_every_member() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_basic");
    let out = cust(&dir, ["check"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Both members appear in the output.
    assert!(stdout.contains("Checked util"), "{stdout}");
    assert!(stdout.contains("Checked app"), "{stdout}");
    // No archive should be produced.
    assert!(!dir.join("target/debug/build/app/libapp.a").is_file());
}

#[test]
fn workspace_emits_cust_lock_at_root() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_basic");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    let lock = dir.join("Cust.lock");
    assert!(lock.is_file(), "missing {}", lock.display());
    let body = fs::read_to_string(&lock).unwrap();
    assert!(body.contains("lock_format_version = 1"), "{body}");
    // workspace_root is intentionally not recorded — the lock
    // is location-independent (matches Cargo's `Cargo.lock`).
    assert!(
        !body.contains("workspace_root"),
        "workspace_root leaked into lock:\n{body}"
    );
    // Both members appear.
    assert!(body.contains("name = \"app\""), "{body}");
    assert!(body.contains("name = \"util\""), "{body}");
    // Edge recorded.
    assert!(body.contains("dependencies = [\"util\"]"), "{body}");
    // Alphabetical: app before util.
    let app_pos = body.find("name = \"app\"").unwrap();
    let util_pos = body.find("name = \"util\"").unwrap();
    assert!(app_pos < util_pos);
}

#[test]
fn single_crate_does_not_emit_cust_lock() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    assert!(
        !dir.join("Cust.lock").exists(),
        "single-crate project should not produce Cust.lock"
    );
}

#[test]
fn cust_check_does_not_touch_lock() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_basic");
    // First a real build to create the lock.
    assert_success(&cust(&dir, ["build"]));
    let lock = dir.join("Cust.lock");
    assert!(lock.is_file());
    let mtime_before = fs::metadata(&lock).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    // cust check must not rewrite the lockfile.
    assert_success(&cust(&dir, ["check"]));
    let mtime_after = fs::metadata(&lock).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "cust check touched Cust.lock");
}

#[test]
fn build_p_filters_to_member_and_transitive_deps() {
    // Workspace has 3 members: util, app (-> util), extra.
    // `cust build -p app` should build util and app but NOT extra.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_three");
    let out = cust(&dir, ["build", "-p", "app"]);
    assert_success(&out);
    assert!(
        dir.join("target/debug/build/util/libutil.a").is_file(),
        "util not built"
    );
    assert!(
        dir.join("target/debug/build/app/libapp.a").is_file(),
        "app not built"
    );
    assert!(
        !dir.join("target/debug/build/extra/libextra.a").exists(),
        "extra should not have been built with -p app"
    );
}

#[test]
fn build_long_package_flag_works() {
    // `--package` is the long form Cargo users will reach for.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_three");
    let out = cust(&dir, ["build", "--package", "util"]);
    assert_success(&out);
    assert!(dir.join("target/debug/build/util/libutil.a").is_file());
    assert!(!dir.join("target/debug/build/app/libapp.a").exists());
    assert!(!dir.join("target/debug/build/extra/libextra.a").exists());
}

#[test]
fn build_p_unknown_member_is_error() {
    let (_tmp, dir) = stage("workspace_three");
    let out = cust(&dir, ["build", "-p", "nope"]);
    assert_failure_with(&out, "unknown workspace member `nope`");
}

#[test]
fn build_rejects_jobs_zero() {
    // v0.4.2 slice D: `--jobs 0` is a usage error; `0` would lower
    // to `cmake --build -j 0` which Ninja interprets as "no
    // limit" via a footgun. Reject up front with a clear hint.
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build", "-j", "0"]);
    assert_failure_with(&out, "`--jobs 0` is not allowed");
}

#[test]
fn build_with_jobs_succeeds() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    // Both forms of the flag work and produce a working artefact.
    let (_tmp, dir) = stage("hello");
    assert_success(&cust(&dir, ["build", "-j", "1"]));
    assert!(dir.join("target/debug/build/hello/libhello.a").is_file());
    assert_success(&cust(&dir, ["build", "--jobs", "2"]));
}

#[test]
fn cust_jobs_env_var_is_consumed() {
    // v0.4.2 slice D: `$CUST_JOBS` is the env fallback when the
    // flag isn't passed; `$CARGO_BUILD_JOBS` is the secondary
    // (Cargo parity). Garbage values produce a clear error.
    let (_tmp, dir) = stage("hello");
    let mut cmd = std::process::Command::new(CUST_BIN);
    cmd.current_dir(&dir);
    cmd.env("CUST_JOBS", "garbage");
    cmd.arg("build");
    let out = cmd.output().expect("spawn cust");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected failure; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(
        stderr.contains("parsing $CUST_JOBS"),
        "stderr did not mention CUST_JOBS:\n{stderr}"
    );
}

#[test]
fn check_p_filters_to_member_and_transitive_deps() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_three");
    let out = cust(&dir, ["check", "-p", "app"]);
    assert_success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Checked util"), "{stdout}");
    assert!(stdout.contains("Checked app"), "{stdout}");
    assert!(!stdout.contains("Checked extra"), "{stdout}");
}
