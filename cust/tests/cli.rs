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

    // v0.4.2 V42D-13: rewrites land under
    // `target/<profile>/.rewrite/<crate>/<rel>.c`. CMake's
    // per-TU object files live under
    // `target/<profile>/cmake/build/CMakeFiles/<target>.dir/...`
    // (an implementation detail of the CMake/Ninja backend;
    // tests don't assert the exact object-file paths).
    let rd = dir.join("target/debug/.rewrite/multi_module/src");
    for name in ["lib.c", "util.c"] {
        assert!(rd.join(name).is_file(), "missing rewrite of {name}");
    }
    assert!(
        rd.join("parser/mod.c").is_file(),
        "missing rewrite of parser/mod.c"
    );

    let archive = dir.join("target/debug/build/multi_module/libmulti_module.a");
    assert!(archive.is_file());

    // All three `[[cust::pub]]` symbols should be in the archive.
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

    // v0.4.2 V42D-12: CMake emits compile_commands.json. One
    // entry per TU (no paired clangd entries — see the test
    // below for the trade-off).
    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    for needle in [
        "/.rewrite/multi_module/src/lib.c",
        "/.rewrite/multi_module/src/util.c",
        "/.rewrite/multi_module/src/parser/mod.c",
    ] {
        assert!(
            cc.contains(needle),
            "compile_commands.json missing {needle:?}"
        );
    }
}

#[test]
fn compile_commands_json_carries_paired_entries_for_clangd() {
    // v0.4.2: CMake-emitted compile_commands.json carries ONE
    // entry per TU per CMake target — the rewritten
    // `.rewrite/.../src/<name>.c` file. Because the v0.4.2
    // refinement makes the per-member `<crate>__test` target
    // unconditional (EXCLUDE_FROM_ALL keeps it out of `cust
    // build` but it still appears in compile_commands.json),
    // each lib source shows up twice: once with the lib
    // target's flags and once with the test target's flags
    // (which add `-DCUST_TEST_BUILD=1`). The runner-TU stub
    // contributes one extra entry.
    //
    // clangd opens the user's original `src/<name>.c` and
    // won't find a matching entry; supporting that needs a
    // post-process step mirroring each entry with the original-
    // source path. Tracked as a v0.4.x follow-up, not a v0.4.2
    // blocker (see docs/design/v0.4.2.md V42D-12).
    let (_tmp, dir) = stage("multi_module");
    assert_success(&cust(&dir, ["build"]));

    let cc = fs::read_to_string(dir.join("target/compile_commands.json")).unwrap();
    let entry_count = cc.matches("\"file\":").count();
    // 3 lib TUs (multi_module target) + 3 lib TUs recompiled
    // for the EXCLUDE_FROM_ALL `multi_module__test` target +
    // 1 runner-TU stub = 7. (v0.3 had six: 3 rewritten + 3
    // paired-originals.)
    assert_eq!(
        entry_count, 7,
        "expected 7 entries (3 lib + 3 test-target lib + 1 runner stub); got:\n{cc}"
    );
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
    // `lib.c` calls a `[[cust::pub]]` function defined in `util.c`
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

    // Both `[[cust::pub]]` symbols must end up in the archive.
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
    // No `#include` injection: the generated crate header is
    // pure declarations. Consumers that reach for system types
    // (stdint.h / stddef.h / stdbool.h) must include them
    // themselves, or the producing crate must export its own
    // `[[cust::pub]] typedef`s (e.g. cstd's i32/usize aliases).
    assert!(
        !body.contains("#include <stdint.h>"),
        "generated header injected <stdint.h>:\n{body}"
    );
    assert!(
        !body.contains("#include <stddef.h>"),
        "generated header injected <stddef.h>:\n{body}"
    );
    assert!(
        !body.contains("#include <stdbool.h>"),
        "generated header injected <stdbool.h>:\n{body}"
    );
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
    // The crate header no longer injects <stdint.h>, so the
    // consumer pulls it in itself before the cust header — this
    // is exactly the new contract (cust-design.md §5: "No
    // `#include` injection").
    let consumer_src = dir.join("consumer.c");
    fs::write(
        &consumer_src,
        b"#include <stdint.h>\n\
          #include \"target/debug/build/use_crate_works/include/use_crate_works.h\"\n\
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
fn surface_pass_resolves_cross_module_typedef() {
    // Regression for the bug fixed alongside this test: the
    // surface pass used to blank every `#cust use` directive
    // unconditionally, so when module M referenced a
    // `[[cust::pub]] typedef` exported by sibling N, clang saw
    // an undeclared identifier in a declarator position and
    // recovered with implicit-int. The plugin then exported the
    // wrong return / parameter type (`int` instead of the real
    // underlying primitive), silently corrupting the ABI of the
    // published `<crate>.h`. The fix lowers `#cust use crate::X;`
    // to an `#include` of X's fragment header **iff** the fragment
    // exists, and relies on the V40D-11 fixed-point loop to
    // converge (iter 1: best-effort blanking; iter 2+: real
    // includes). See [v0.4.0.md](docs/design/v0.4.0.md) V40D-11.
    //
    // Plugin-dependent: skip when not built.
    if plugin_path().is_none() {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    }
    let (_tmp, dir) = stage("cross_module_typedef");
    let out = cust(&dir, ["build"]);
    assert_success(&out);

    let hdr = dir.join("target/debug/build/cross_module_typedef/include/cross_module_typedef.h");
    assert!(hdr.is_file(), "missing crate header at {}", hdr.display());
    let body = fs::read_to_string(&hdr).unwrap();

    // The typedef must propagate to the published header. Either
    // the alias name wins (clang preserves the typedef when it's
    // visible — the common case) or the printer desugars to the
    // underlying primitive (`unsigned long` on every cust-
    // supported platform today). The failure mode this regression
    // pins is the `int` fallback that the pre-fix surface pass
    // produced.
    let signature_ok = body.contains("cmt_usize cmt_mem_size(void);")
        || body.contains("unsigned long cmt_mem_size(void);");
    assert!(
        signature_ok,
        "cross-module typedef regression: published header does not \
         expose `cmt_usize` (or `unsigned long`) for `cmt_mem_size`. \
         Header body:\n{body}"
    );
    // Tight assertion on the failure mode: must NOT be `int`.
    assert!(
        !body.contains("int cmt_mem_size(void);"),
        "regression: `cmt_mem_size` exported with the implicit-int \
         recovery type, not the imported typedef. Header body:\n{body}"
    );
}

#[test]
fn build_without_plugin_emits_v40d12_error() {
    // V0.4.0 V40D-12: plugin is mandatory for `cust build`. With
    // no discoverable plugin the build must hard-error with the
    // verbatim wording (replaces v0.2's silent-skip behaviour
    // that the v0.3.x tests relied on).
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["build"])
        .env("CUST_PLUGIN", "/definitely/does/not/exist")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert!(
        !out.status.success(),
        "expected cust build to fail without plugin, got success\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cust plugin (libcust_plugin.so) not found"),
        "V40D-12 wording missing from stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("cargo run -p plugin-build"),
        "V40D-12 hint missing from stderr:\n{stderr}"
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
fn check_without_plugin_warns_but_succeeds() {
    // V0.4.0 V40D-12: `cust check` keeps its silent-skip path
    // (single-TU syntax-only doesn't strictly need the plugin)
    // but emits a heads-up warning so users discover the
    // problem before `cust build` hard-errors on them.
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["check"])
        .env("CUST_PLUGIN", "/definitely/does/not/exist")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("libcust_plugin.so) not found"),
        "expected plugin-missing warning on stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("hard-error"),
        "warning should mention the build/test hard-error contract:\n{stderr}"
    );

    // `cust check --no-plugin` is also accepted and succeeds
    // silently (no warning, since the user explicitly opted
    // out). Fragment headers are NOT emitted.
    let out2 = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["check", "--no-plugin"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out2);
    let frag_dir = dir.join("target/debug/.h-fragments");
    assert!(
        !frag_dir.exists(),
        "--no-plugin check should NOT create fragment headers, but {} exists",
        frag_dir.display()
    );
}

#[test]
fn build_no_plugin_flag_is_rejected_with_v40d10_wording() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["build", "--no-plugin"]);
    assert!(
        !out.status.success(),
        "cust build --no-plugin should be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`--no-plugin` is incompatible with `cust build`"),
        "V40D-10 wording missing:\n{stderr}"
    );
}

#[test]
fn test_no_plugin_flag_is_rejected_with_v40d10_wording() {
    let (_tmp, dir) = stage("hello");
    let out = cust(&dir, ["test", "--no-plugin"]);
    assert!(
        !out.status.success(),
        "cust test --no-plugin should be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`--no-plugin` is incompatible with `cust test`"),
        "V40D-10 wording missing:\n{stderr}"
    );
}

#[test]
fn build_without_plugin_skips_fplugin_flag() {
    // Only meaningful when the plugin is built — skip otherwise so
    // CI without `cargo run -p plugin-build` doesn't choke.
    let Some(plugin) = plugin_path() else {
        eprintln!("plugin not built — skipping (run `cargo run -p plugin-build`)");
        return;
    };
    let _ = plugin; // not used; we want to verify --no-plugin suppresses -fplugin

    // `cust build --no-plugin` is rejected by V40D-10 and `cust
    // check` doesn't write compile_commands.json. The cleanest
    // observable in-tree signal is the fragment-headers dir:
    // present after `cust build`, absent after `cust check
    // --no-plugin`.
    let (_tmp, dir) = stage("hello");
    let out = Command::new(env!("CARGO_BIN_EXE_cust"))
        .args(["check", "--no-plugin"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn cust");
    assert_success(&out);

    let frag_dir = dir.join("target/debug/.h-fragments");
    assert!(
        !frag_dir.exists(),
        "--no-plugin should suppress fragment header emission, but {} exists",
        frag_dir.display()
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
    // V40D-5: fragment headers are emitted by the dedicated
    // surface_pass (phase 1 / -fsyntax-only) and the plugin
    // hard-errors if `fragment-out` arrives during codegen.
    // `compile_commands.json` records codegen invocations, so
    // it must NOT contain the fragment-out arg.
    assert!(
        !cc.contains("-fplugin-arg-cust-fragment-out="),
        "compile_commands.json contains codegen fragment-out arg (V40D-5 violation):\n{cc}"
    );

    // Strongest proof the plugin actually ran: the fragment
    // header for the root module exists on disk (written by
    // surface_pass).
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
    assert_failure_with(&out, "`#cust use badlib;`");
    assert_failure_with(&out, "not listed in [dependencies]");
}

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
