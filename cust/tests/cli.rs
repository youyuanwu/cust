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
    assert!(dir.join("target/debug/build/hello/libhello.a").is_file());
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
    assert!(dir.join("target/debug/build/greet/libgreet.a").is_file());
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
    assert!(dir
        .join("target/debug/build/my-crate/libmy-crate.a")
        .is_file());
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

    let archive = dir.join("target/debug/build/multi_module/libmulti_module.a");
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
fn use_crate_compiles_cross_module_call() {
    // `lib.c` calls a `cust_pub` function defined in `util.c`
    // purely via `#cust use crate::util;` — no manual `extern`
    // declarations. The build pipeline's surface pass + fragment-
    // header `#include` lowering should make this work.
    //
    // Plugin-dependent: skip when not built.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("use_crate_works");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    // Both `cust_pub` symbols must end up in the archive.
    let archive = dir.join("target/debug/build/use_crate_works/libuse_crate_works.a");
    assert!(archive.is_file());
    let nm = Command::new("nm")
        .arg("--defined-only")
        .arg(&archive)
        .stdin(Stdio::null())
        .output()
        .expect("spawn nm");
    let syms = String::from_utf8_lossy(&nm.stdout);
    for sym in ["use_crate_works_total", "use_crate_works_util_get"] {
        assert!(syms.contains(sym), "archive missing `{sym}`:\n{syms}");
    }
}

#[test]
fn use_crate_unknown_name_is_error() {
    let (_tmp, dir) = stage("use_crate_unknown");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "no module named `nope`");
}

#[test]
fn build_emits_concatenated_crate_header() {
    // Plugin-dependent: the crate header is only meaningful when
    // fragment headers exist.
    let Some(_plugin) = plugin_path() else {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    };
    let (_tmp, dir) = stage("use_crate_works");
    assert_success(&cust(&dir, ["build"]));

    let hdr = dir.join("target/debug/build/use_crate_works/include/use_crate_works.h");
    assert!(hdr.is_file(), "missing crate header at {}", hdr.display());

    let body = fs::read_to_string(&hdr).unwrap();
    // Standard include-guard + extern-C wrapper.
    assert!(body.contains("#ifndef USE_CRATE_WORKS_H"), "{body}");
    assert!(body.contains("#define USE_CRATE_WORKS_H"), "{body}");
    assert!(body.contains("extern \"C\" {"), "{body}");
    // Self-contained: pulls in stdint so consumers don't have
    // to include <stdint.h> first.
    assert!(body.contains("#include <stdint.h>"), "{body}");
    // Both modules' public surfaces appear, each banner-tagged.
    assert!(body.contains("/* --- module `lib` --- */"), "{body}");
    assert!(body.contains("/* --- module `util` --- */"), "{body}");
    assert!(
        body.contains("int32_t use_crate_works_total(void);"),
        "{body}"
    );
    assert!(
        body.contains("int32_t use_crate_works_util_get(void);"),
        "{body}"
    );

    // End-to-end: a non-cust consumer can #include the header,
    // link the archive, and the resulting binary actually runs.
    let consumer_src = dir.join("consumer.c");
    fs::write(
        &consumer_src,
        b"#include \"target/debug/build/use_crate_works/include/use_crate_works.h\"\n\
          int main(void) { return use_crate_works_total() == 42 ? 0 : 1; }\n",
    )
    .unwrap();
    let bin = dir.join("consumer");
    let compile = Command::new("clang")
        .args([
            consumer_src.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            dir.join("target/debug/build/use_crate_works/libuse_crate_works.a")
                .to_str()
                .unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("spawn clang");
    assert!(
        compile.status.success(),
        "consumer build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );
    let run = Command::new(&bin)
        .stdin(Stdio::null())
        .output()
        .expect("spawn consumer");
    assert!(run.status.success(), "consumer exited with {}", run.status);
}

#[test]
fn build_without_plugin_skips_crate_header() {
    // No plugin => no fragments => no concatenated header.
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["build"])
        .env("CUST_PLUGIN", "/definitely/does/not/exist")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);
    let hdr = dir.join("target/debug/build/hello/include/hello.h");
    assert!(
        !hdr.exists(),
        "expected NO crate header without plugin, but found {}",
        hdr.display()
    );
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

// ─── v0.2: clang plugin ────────────────────────────────────────────

/// Helper: path to the built `libcust_plugin.so` (next to the
/// `cust` binary itself).
fn plugin_path() -> Option<PathBuf> {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_cust"));
    let candidate = exe.parent()?.join("libcust_plugin.so");
    candidate.is_file().then_some(candidate)
}

#[test]
fn build_without_plugin_skips_fplugin_flag() {
    // Override discovery to point at a nonexistent path — the
    // driver should silently skip the plugin (v0.2 behaviour:
    // plugin is opt-in until cross-module imports require it).
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["build"])
        .env("CUST_PLUGIN", "/definitely/does/not/exist")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    assert!(
        !cc.contains("-fplugin="),
        "expected NO -fplugin flag with CUST_PLUGIN=/definitely/does/not/exist, got:\n{cc}"
    );
}

#[test]
fn build_with_plugin_injects_fplugin_flag() {
    // Only meaningful when the plugin is built — skip otherwise so
    // CI without `cargo run -p plugin-build` doesn't choke.
    let Some(plugin) = plugin_path() else {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    };
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["build"])
        .env("CUST_PLUGIN", &plugin)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    let expected = format!("-fplugin={}", plugin.display());
    assert!(
        cc.contains(&expected),
        "expected `{expected}` in compile_commands.json:\n{cc}"
    );
    // The driver passes the fragment-out arg per TU.
    assert!(
        cc.contains("-fplugin-arg-cust-fragment-out="),
        "expected fragment-out arg in compile_commands.json:\n{cc}"
    );

    // Strongest proof the plugin actually ran: the fragment
    // header for the root module exists on disk.
    let frag = dir.join("target/debug/.h-fragments/hello/lib.cust.h");
    assert!(
        frag.is_file(),
        "expected fragment header at {}",
        frag.display()
    );
    let body = fs::read_to_string(&frag).unwrap();
    assert!(
        body.contains("@generated by cust plugin"),
        "fragment header missing header marker:\n{body}"
    );
}

#[test]
fn plugin_emits_fragment_header_per_module() {
    // Only meaningful when the plugin is built.
    let Some(plugin) = plugin_path() else {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    };
    let (_tmp, dir) = stage("multi_module");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["build"])
        .env("CUST_PLUGIN", &plugin)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);

    let frag_dir = dir.join("target/debug/.h-fragments/multi_module");
    for (qname, expected_sig) in [
        ("lib", "int32_t multi_module_total(void);"),
        ("util", "int32_t multi_module_util_get(void);"),
        ("parser", "int32_t multi_module_parser_count(void);"),
    ] {
        let f = frag_dir.join(format!("{qname}.cust.h"));
        assert!(f.is_file(), "missing fragment header {}", f.display());
        let body = fs::read_to_string(&f).unwrap();
        assert!(
            body.contains(expected_sig),
            "{}: missing signature {expected_sig:?}\n{body}",
            f.display()
        );
    }
}

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

    // app's archive carries its own cust_pub symbol; util's
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
    // util. The build pipeline should reject this at the rewrite
    // step.
    let (_tmp, dir) = stage("workspace_undeclared_dep");
    let out = cust(&dir, ["build"]);
    assert_failure_with(&out, "`#cust use util;`");
    assert_failure_with(&out, "not listed in [dependencies]");
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
