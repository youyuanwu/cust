//! End-to-end tests against the built `cust` binary.
//!
//! Each test copies one of the fixtures under `tests/fixtures/` into
//! a fresh tempdir and invokes the `cust` binary there. This keeps
//! `target/` artifacts out of the repo and lets tests run in
//! parallel without stepping on each other.
//!
//! Fixtures live in `tests/fixtures/<name>/` and are checked in;
//! happy-path fixtures contain a `Cust.toml` plus `src/lib.c`,
//! error-path fixtures only need whatever the driver reads before
//! failing.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

/// Path to the built `cust` binary. Cargo defines this for any
/// integration test in a crate with a `[[bin]]`.
const CUST_BIN: &str = env!("CARGO_BIN_EXE_cust");

/// Workspace-relative path to the fixtures directory.
fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Copy a fixture into a fresh tempdir and return both handles.
fn stage(fixture: &str) -> (TempDir, PathBuf) {
    let src = fixtures_root().join(fixture);
    assert!(
        src.is_dir(),
        "fixture `{}` not found at {}",
        fixture,
        src.display()
    );
    let tmp = tempfile::Builder::new()
        .prefix(&format!("cust-it-{fixture}-"))
        .tempdir()
        .expect("tempdir");
    copy_tree(&src, tmp.path()).expect("copy fixture");
    let crate_dir = tmp.path().to_path_buf();
    (tmp, crate_dir)
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if ft.is_file() {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

fn cust<I, S>(crate_dir: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(CUST_BIN)
        .args(args)
        .current_dir(crate_dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust")
}

fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "cust exited with {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn assert_failure_with(out: &Output, needle: &str) {
    assert!(
        !out.status.success(),
        "expected failure but cust succeeded:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(needle),
        "stderr did not contain {needle:?}:\n{stderr}",
    );
}

// ─── Happy-path tests ───────────────────────────────────────────────

#[test]
fn build_hello_dev_produces_static_archive() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    let archive = dir.join("target/debug/libhello.a");
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

    assert!(dir.join("target/release/libhello.a").is_file());
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

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    for needle in [
        "\"-fvisibility=hidden\"",
        "\"-include\"",
        "prelude.h",
        "\"-O0\"",
        "\"-g3\"",
        "\"-Wall\"",
        "\"-c\"",
        // The compiled file is the rewritten copy under target/;
        // the `-I` argument anchors `#include` resolution back at
        // the user's `src/` directory.
        "lib.preprocessed.c",
        "/src\"",
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
    assert!(dir.join("target/debug/libhello.a").is_file());
}

// ─── Error-path tests ───────────────────────────────────────────────

#[test]
fn rejects_unknown_top_level_field() {
    let (_tmp, dir) = stage("unknown_field");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "unknown field `bogus`");
}

#[test]
fn rejects_populated_dependencies_section() {
    let (_tmp, dir) = stage("populated_deps");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "`[dependencies]`");
    assert_failure_with(&out, "not yet supported in cust v0.1");
}

#[test]
fn rejects_invalid_package_name() {
    let (_tmp, dir) = stage("bad_name");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "invalid `[package] name");
}

#[test]
fn reports_missing_lib_source() {
    let (_tmp, dir) = stage("missing_source");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "not found");
    assert_failure_with(&out, "lib.c");
}

#[test]
fn reports_missing_manifest() {
    // `tempdir()` directly — no fixture needed, the point is the
    // absence of any `Cust.toml` up the chain.
    let tmp = tempfile::tempdir().unwrap();
    let out = cust(tmp.path(), ["build"]);
    assert_failure_with(&out, "could not find `Cust.toml`");
}

// ─── `cust new` ─────────────────────────────────────────────────────

#[test]
fn new_creates_buildable_lib_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("greet");

    let out = cust(tmp.path(), ["new", "greet"]);
    assert_success(&out);

    assert!(dir.join("Cust.toml").is_file());
    assert!(dir.join("src/lib.c").is_file());
    let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/target"), "{gitignore}");

    // The scaffold should build cleanly with `cust build`.
    assert_success(&cust(&dir, ["build"]));
    assert!(dir.join("target/debug/libgreet.a").is_file());
}

#[test]
fn new_into_existing_empty_directory_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hi");
    fs::create_dir(&dir).unwrap();

    let out = cust(tmp.path(), ["new", "hi"]);
    assert_success(&out);
    assert!(dir.join("Cust.toml").is_file());
}

#[test]
fn new_refuses_to_clobber_nonempty_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("occupied");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("README"), "hello").unwrap();

    let out = cust(tmp.path(), ["new", "occupied"]);
    assert_failure_with(&out, "already exists and is not empty");
    // We must not have written anything.
    assert!(!dir.join("Cust.toml").exists());
}

#[test]
fn new_with_dash_in_path_sanitises_c_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("my-crate");

    assert_success(&cust(tmp.path(), ["new", "my-crate"]));

    let lib = fs::read_to_string(dir.join("src/lib.c")).unwrap();
    assert!(lib.contains("my_crate_add"), "{lib}");
    assert!(!lib.contains("my-crate_add"), "{lib}");

    // And `cust build` still works on the result.
    assert_success(&cust(&dir, ["build"]));
    assert!(dir.join("target/debug/libmy-crate.a").is_file());
}

#[test]
fn new_with_explicit_name_overrides_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dirname");

    assert_success(&cust(
        tmp.path(),
        ["new", "dirname", "--name", "actual_name"],
    ));

    let toml = fs::read_to_string(dir.join("Cust.toml")).unwrap();
    assert!(toml.contains("name    = \"actual_name\""), "{toml}");
}

#[test]
fn new_rejects_invalid_name() {
    let tmp = tempfile::tempdir().unwrap();
    let out = cust(tmp.path(), ["new", "weird", "--name", "has spaces"]);
    assert_failure_with(&out, "invalid package name");
    // The directory should not have been left half-populated.
    assert!(!tmp.path().join("weird/Cust.toml").exists());
}

// ─── v0.2: multi-module ────────────────────────────────────────────

#[test]
fn build_multi_module_compiles_all_tus() {
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["build"]));

    // Each module gets its own rewritten source + object.
    let bd = dir.join("target/debug/build/multi_module");
    for name in ["lib", "util", "parser"] {
        assert!(
            bd.join(format!("{name}.preprocessed.c")).is_file(),
            "missing {name}.preprocessed.c"
        );
        assert!(bd.join(format!("{name}.o")).is_file(), "missing {name}.o");
    }

    let archive = dir.join("target/debug/libmulti_module.a");
    assert!(archive.is_file());

    // All three `cust_pub` symbols should be in the archive.
    let nm = Command::new("nm")
        .arg("--defined-only")
        .arg(&archive)
        .stdin(Stdio::null())
        .output()
        .expect("spawn nm");
    let syms = String::from_utf8_lossy(&nm.stdout);
    for sym in [
        "multi_module_total",
        "multi_module_util_get",
        "multi_module_parser_count",
    ] {
        assert!(
            syms.contains(sym),
            "archive missing symbol `{sym}`:\n{syms}",
        );
    }
}

#[test]
fn build_multi_module_emits_one_compile_command_per_tu() {
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["build"]));

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    for needle in [
        "lib.preprocessed.c",
        "util.preprocessed.c",
        "parser.preprocessed.c",
    ] {
        assert!(
            cc.contains(needle),
            "compile_commands.json missing {needle:?}"
        );
    }
}

#[test]
fn compile_commands_json_carries_paired_entries_for_clangd() {
    // For each module the driver emits TWO entries: one for the
    // rewritten `.preprocessed.c` (matches what was actually
    // compiled) and one for the user's original source (lets
    // clangd find matching flags when the editor opens src/*.c).
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["build"]));

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    // Crude but sufficient: the rewritten path and the original
    // source path should both appear as the "file" value for each
    // of the three modules.
    for original in ["src/lib.c", "src/util.c", "src/parser/mod.c"] {
        assert!(
            cc.contains(original),
            "compile_commands.json missing original source `{original}`:\n{cc}"
        );
    }
    // Six "file": entries (3 modules × 2). Count them.
    let n = cc.matches("\"file\":").count();
    assert_eq!(n, 6, "expected 6 file entries, got {n}:\n{cc}");
}

#[test]
fn rejects_both_file_and_folder_form_module() {
    let (_tmp, dir) = stage("module_ambiguous");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "ambiguous module `foo`");
    assert_failure_with(&out, "keep exactly one");
}

#[test]
fn reports_missing_module_source() {
    let (_tmp, dir) = stage("module_missing");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "module `nope` not found");
}

#[test]
fn rejects_use_crate_in_v0_2() {
    let (_tmp, dir) = stage("use_crate_not_supported");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "#cust use crate::util");
    assert_failure_with(&out, "require the cust plugin");
}

#[test]
fn check_passes_through_cust_mod_directives() {
    // `cust check` on a root that contains `#cust mod foo;` must
    // not blow up on the directive — the scanner-rewrite path
    // strips it before clang sees it.
    let (_tmp, dir) = stage("multi_module");
    let out = cust(&dir, ["check"]);
    assert_success(&out);
}
