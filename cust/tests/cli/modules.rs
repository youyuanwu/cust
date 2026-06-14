use crate::common::*;

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
