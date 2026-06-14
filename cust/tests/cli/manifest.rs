use crate::common::*;

// ─── Error-path tests ───────────────────────────────────────────────

#[test]
fn rejects_unknown_top_level_field() {
    let (_tmp, dir) = stage("unknown_field");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "unknown field `bogus`");
}

#[test]
fn rejects_populated_dependencies_section() {
    // The fixture's manifest contains `something = "1.0"` (a bare
    // version spec). v0.3 rejects this at parse time with a v0.4
    // pointer: version specs are not in v0.3's scope.
    let (_tmp, dir) = stage("populated_deps");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "version specs are v0.4+");
    assert_failure_with(&out, "path");
}

#[test]
fn rejects_path_dep_without_workspace() {
    // A path-form dep in a single-crate (non-workspace) Cust.toml.
    // Parses cleanly (V3D-3 shape is valid), then the CLI's
    // locate() catches the missing [workspace] and points the
    // user at adding one to a parent manifest.
    let (_tmp, dir) = stage("path_dep_no_workspace");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "no enclosing [workspace]");
    assert_failure_with(&out, "path dependencies require a [workspace]");
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
    // v0.3.1: error mentions both candidate sources (lib + bin)
    // because either is acceptable.
    assert_failure_with(&out, "src/lib.c");
    assert_failure_with(&out, "src/main.c");
}

#[test]
fn reports_missing_manifest() {
    // `tempdir()` directly — no fixture needed, the point is the
    // absence of any `Cust.toml` up the chain.
    let tmp = tempfile::tempdir().unwrap();
    let out = cust(tmp.path(), ["build"]);
    assert_failure_with(&out, "could not find `Cust.toml`");
}
