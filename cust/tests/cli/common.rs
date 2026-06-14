//! Shared helpers for the `cust` end-to-end test suite.
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
//!
//! The test functions themselves live in the sibling modules
//! (`build_check`, `manifest`, `scaffold`, …) declared from the
//! `cli.rs` crate root. Items here are re-exported via
//! `use crate::common::*;` in each of those modules.

pub use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub use tempfile::TempDir;

/// Path to the built `cust` binary. Cargo defines this for any
/// integration test in a crate with a `[[bin]]`.
pub const CUST_BIN: &str = env!("CARGO_BIN_EXE_cust");

/// Workspace-relative path to the fixtures directory.
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Copy a fixture into a fresh tempdir and return both handles.
pub fn stage(fixture: &str) -> (TempDir, PathBuf) {
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

pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
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

/// Recursively collect every regular file under `root`.
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

pub fn cust<I, S>(crate_dir: &Path, args: I) -> Output
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

pub fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "cust exited with {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

pub fn assert_failure_with(out: &Output, needle: &str) {
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

/// Like `assert_failure_with` but searches stdout **and** stderr.
/// v0.4.5 V45D-3: errors raised inside a `cust internal` custom
/// command (e.g. an undeclared `#cust use <dep>;`) surface through
/// Ninja's build output, which cust echoes on stdout — same as any
/// other build error (a clang compile error, a link failure). Use
/// this for errors that originate in the `cmake --build` phase.
pub fn assert_build_failure_with(out: &Output, needle: &str) {
    assert!(
        !out.status.success(),
        "expected failure but cust succeeded:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        combined.contains(needle),
        "build output did not contain {needle:?}:\n{combined}",
    );
}

/// Path to the built `libcust_plugin.so` (next to the `cust` binary
/// itself). Returns `None` when the plugin isn't built, letting
/// plugin-dependent tests skip cleanly.
pub fn plugin_path() -> Option<PathBuf> {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_cust"));
    let candidate = exe.parent()?.join("libcust_plugin.so");
    candidate.is_file().then_some(candidate)
}
