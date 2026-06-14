use crate::common::*;

// ─── v0.3.1 Slice B: binary crates ──────────────────────────────────

#[test]
fn bin_only_crate_builds_executable() {
    let (_tmp, dir) = stage("bin_only");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    let exe = dir.join("target/debug/bin_only");
    assert!(exe.is_file(), "missing {}", exe.display());
    // No archive published — bins are leaves.
    assert!(!dir
        .join("target/debug/build/bin_only/libbin_only.a")
        .exists());

    // Run it; cust_main aliases to main, which returns 7.
    let status = std::process::Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(7), "bin exit code");
}

#[test]
fn lib_and_bin_crate_produces_both_artifacts() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("lib_and_bin");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    let archive = dir.join("target/debug/build/demo/libdemo.a");
    let exe = dir.join("target/debug/demo");
    assert!(archive.is_file(), "missing {}", archive.display());
    assert!(exe.is_file(), "missing {}", exe.display());

    // Bin's exit code is the lib's answer (42), proving the bin
    // resolved the lib's [[cust::pub]] surface via the auto-
    // injected -I to the lib's include dir.
    let status = std::process::Command::new(&exe).status().unwrap();
    assert_eq!(status.code(), Some(42));
}

#[test]
fn workspace_bin_path_deps_on_lib() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_bin_lib");
    let out = cust(&dir, ["build"]);
    assert_success(&out);
    // Lib produces an archive; bin produces an executable.
    assert!(dir.join("target/debug/build/util/libutil.a").is_file());
    assert!(dir.join("target/debug/app").is_file());

    // `#cust use util;` in app/src/main.c lowered to an include
    // of util's crate header; the linker pulled in util's
    // archive via --start-group; cust_main returns util_double(21) = 42.
    let status = std::process::Command::new(dir.join("target/debug/app"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));
}

#[test]
fn bin_only_check_does_not_link() {
    let (_tmp, dir) = stage("bin_only");
    let out = cust(&dir, ["check"]);
    assert_success(&out);
    // syntax-only: no executable produced.
    assert!(!dir.join("target/debug/bin_only").exists());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Checked bin_only"), "{stdout}");
}

#[test]
fn build_p_bin_member_links_only_transitive_lib_deps() {
    // workspace_bin_lib has 2 members (util lib, app bin). With
    // -p app the build orchestrator should still pull util in
    // because it's a link-dep of app.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_bin_lib");
    let out = cust(&dir, ["build", "-p", "app"]);
    assert_success(&out);
    assert!(dir.join("target/debug/app").is_file());
    assert!(dir.join("target/debug/build/util/libutil.a").is_file());
}

// ─── v0.3.1 Slice C: cust run + edge rules + cust new --bin ─────────

#[test]
fn cust_run_executes_single_bin_member() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_bin_lib");
    let out = cust(&dir, ["run"]);
    // app's cust_main returns util_double(21) = 42.
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Running"), "{stdout}");
    assert!(stdout.contains("target/debug/app"), "{stdout}");
}

#[test]
fn cust_run_forwards_argv_after_double_dash() {
    let (_tmp, dir) = stage("bin_argv");
    let out = cust(&dir, ["run", "--", "alpha", "beta", "gamma"]);
    // bin_argv returns argc, so exit code should be 4 (program name + 3 args).
    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("argv[1]=alpha"), "{stdout}");
    assert!(stdout.contains("argv[2]=beta"), "{stdout}");
    assert!(stdout.contains("argv[3]=gamma"), "{stdout}");
}

#[test]
fn cust_run_release_uses_release_profile() {
    let (_tmp, dir) = stage("bin_only");
    let out = cust(&dir, ["run", "--release"]);
    // bin_only's cust_main returns 7.
    assert_eq!(
        out.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Release artifact path.
    assert!(dir.join("target/release/bin_only").is_file());
}

#[test]
fn cust_run_p_selects_named_bin() {
    let (_tmp, dir) = stage("multi_bin_ws");
    let out_a = cust(&dir, ["run", "-p", "a"]);
    assert_eq!(out_a.status.code(), Some(11));
    let out_b = cust(&dir, ["run", "-p", "b"]);
    assert_eq!(out_b.status.code(), Some(22));
}

#[test]
fn cust_run_p_on_lib_member_is_error() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("workspace_bin_lib");
    let out = cust(&dir, ["run", "-p", "util"]);
    assert_failure_with(&out, "is a library");
    assert_failure_with(&out, "requires a binary crate");
}

#[test]
fn cust_run_no_bin_member_is_error() {
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    // workspace_basic has only lib members (app + util both lib).
    let (_tmp, dir) = stage("workspace_basic");
    let out = cust(&dir, ["run"]);
    assert_failure_with(&out, "no binary members");
}

#[test]
fn cust_run_multi_bin_without_p_is_error() {
    let (_tmp, dir) = stage("multi_bin_ws");
    let out = cust(&dir, ["run"]);
    assert_failure_with(&out, "multiple binary members");
    assert_failure_with(&out, "found: a, b");
}

#[test]
fn lib_depending_on_bin_is_rejected() {
    let (_tmp, dir) = stage("lib_depends_on_bin");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "(bin) cannot be a dependency");
    assert_failure_with(&out, "only library members");
}

#[test]
fn cust_new_bin_scaffolds_runnable_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("greeter");
    // Scaffold.
    let create_out = cust(tmp.path(), ["new", "--bin", dest.to_str().unwrap()]);
    assert_success(&create_out);
    let stdout = String::from_utf8_lossy(&create_out.stdout);
    assert!(stdout.contains("Created binary"), "{stdout}");
    assert!(dest.join("Cust.toml").is_file());
    assert!(dest.join("src/main.c").is_file());

    // Build + run it.
    let run_out = cust(&dest, ["run"]);
    assert_eq!(
        run_out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let run_stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(run_stdout.contains("hello from greeter"), "{run_stdout}");
}

#[test]
fn cust_new_lib_and_bin_are_mutually_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("conflicted");
    let out = cust(
        tmp.path(),
        ["new", "--lib", "--bin", dest.to_str().unwrap()],
    );
    // Clap rejects with a conflict error.
    assert!(
        !out.status.success(),
        "expected failure, got stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used") || stderr.contains("conflict"),
        "expected conflict error, got: {stderr}"
    );
}

#[test]
fn lib_and_bin_uses_cust_use_self_for_intra_crate_import() {
    // Cargo parity: bin half of a lib+bin crate may write
    // `#cust use <own-package-name>;` to reach its own lib's
    // public surface — same syntax as cross-crate path deps.
    // The lib_and_bin fixture's main.c does exactly this.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("lib_and_bin");
    let body = fs::read_to_string(dir.join("src/main.c")).unwrap();
    assert!(
        body.contains("#cust use demo;"),
        "fixture should demonstrate the self-import form, got:\n{body}"
    );
    let out = cust(&dir, ["run"]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lib_half_cannot_self_import_via_cust_use() {
    // The Cargo-parity carve-out is bin-half only. In the lib
    // half, `#cust use <own-package-name>;` is meaningless and
    // must error like any other unknown dependency name.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("badlib");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cust.toml"),
        "[package]\nname = \"badlib\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.c"),
        "#cust use badlib;\n[[cust::pub]] int x(void) { return 0; }\n",
    )
    .unwrap();
    let out = cust(&dir, ["build"]);
    assert_build_failure_with(&out, "`#cust use badlib;`");
    assert_build_failure_with(&out, "not listed in [dependencies]");
}
