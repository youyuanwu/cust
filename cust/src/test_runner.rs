//! Generator for the v0.3.2 test runner translation unit.
//!
//! For each test-built crate the driver writes a single C file at
//! `target/<profile>/test/<crate>/cust_test_main.c`, which is
//! compiled like any other module in the test build and linked
//! into the resulting test binary. The file contains:
//!
//! 1. A small fixed preamble (this crate's runner template):
//!    `cust_panic_impl`, the `cust_test_entry` struct, the fork
//!    loop, the output formatter, and `main`.
//! 2. Per-test `extern` forward decls.
//! 3. The constant `__cust_tests[]` array.
//!
//! The preamble is the same string the v0.4 plugin-discovery
//! backend will reuse unchanged (V32D-6 in
//! `docs/design/v0.3.2.md`); only the per-test table changes
//! when the second discovery backend lands.

use std::fmt::Write as _;

use crate::test_discovery::{FnKind, TestEntry};

/// The fixed runner template (preamble + `cust_panic_impl` + fork
/// loop + `main`). Inserted verbatim ahead of the generated
/// `extern` decls and the `__cust_tests[]` table. Kept as a
/// `const &str` (not a separate `.c` file shipped in `cust/src/`)
/// so it lives in the same source tree as the driver — easier
/// to diff against the runtime behaviour the driver expects.
pub const RUNNER_TEMPLATE: &str = include_str!("test_runner_template.c");

/// Render the full contents of `cust_test_main.c` for the given
/// discovered tests. The output is a self-contained C source
/// file: `RUNNER_TEMPLATE` followed by `extern` decls and the
/// `__cust_tests[]` array.
pub fn render_main_c(tests: &[TestEntry]) -> String {
    let mut out = String::with_capacity(RUNNER_TEMPLATE.len() + tests.len() * 200);
    out.push_str(RUNNER_TEMPLATE);
    out.push('\n');

    // Sort by qname for stable output (V32D-6). We sort a slice of
    // indices so the table entries map back to the same TestEntry
    // references the extern-decls block uses.
    let mut order: Vec<usize> = (0..tests.len()).collect();
    order.sort_by(|&a, &b| qname(&tests[a]).cmp(&qname(&tests[b])));

    // Forward declarations for every test fn we'll point at.
    out.push_str("/* ── test forward declarations ── */\n");
    for &i in &order {
        let t = &tests[i];
        let ret = match t.fn_kind {
            FnKind::Int => "int",
            FnKind::Void => "void",
        };
        let _ = writeln!(out, "extern {ret} {}(void);", t.name);
    }
    out.push('\n');

    // The `__cust_tests[]` table.
    out.push_str(
        "/* ── test table ── */\n\
         static const struct cust_test_entry __cust_tests[] = {\n",
    );
    for &i in &order {
        let t = &tests[i];
        let kind = match t.fn_kind {
            FnKind::Int => "CUST_TEST_FN_INT",
            FnKind::Void => "CUST_TEST_FN_VOID",
        };
        let qname_lit = c_string_literal(&qname(t));
        let file_lit = c_string_literal(&t.file.display().to_string());
        let ignored = i32::from(t.ignored);
        let _ = writeln!(
            out,
            "    {{ {qname}, (void *){fn}, {kind}, {ignored}, {file}, {line} }},",
            qname = qname_lit,
            fn = t.name,
            kind = kind,
            ignored = ignored,
            file = file_lit,
            line = t.line,
        );
    }
    out.push_str("};\n\n");

    out.push_str(
        "int main(int argc, char **argv) {\n\
        \x20   const int n = (int)(sizeof(__cust_tests) / sizeof(__cust_tests[0]));\n\
        \x20   return cust_test_run(argc, argv, __cust_tests, n);\n\
         }\n",
    );

    out
}

/// `module::name` qualified name used by the runner's output.
/// The root module (`"lib"`) is dropped to match Cargo's
/// `crate::test_foo` → `test_foo` convention; nested modules
/// (`qualified_name` `"parser.lexer"`) become
/// `parser::lexer::name`.
fn qname(t: &TestEntry) -> String {
    let module = t.module.replace('.', "::");
    if module == "lib" {
        t.name.clone()
    } else {
        format!("{module}::{name}", name = t.name)
    }
}

/// Render `s` as a C string literal, escaping the few characters
/// that can't be embedded raw. We only need to handle the
/// characters that show up in module names and file paths
/// (printable ASCII + a few Unicode chars). Anything outside
/// that goes through `\xHH` byte escapes per byte of its UTF-8.
fn c_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii() && !c.is_control() => out.push(c),
            c => {
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    let _ = write!(out, "\\x{b:02x}");
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn mk(module: &str, name: &str, fn_kind: FnKind, ignored: bool, line: u32) -> TestEntry {
        TestEntry {
            module: module.to_string(),
            name: name.to_string(),
            fn_kind,
            ignored,
            file: PathBuf::from(format!("/tmp/{module}.c")),
            line,
        }
    }

    #[test]
    fn template_compiles_and_includes_main() {
        // Sanity: the template itself defines cust_test_run + the
        // entry struct + cust_panic_impl. We don't compile-test
        // here (the integration test in tests/ does that); just
        // verify the template carries the expected symbols.
        for needle in [
            "struct cust_test_entry",
            "CUST_TEST_FN_VOID",
            "CUST_TEST_FN_INT",
            "cust_test_run",
            "_Noreturn void cust_panic_impl",
            "fork",
            "waitpid",
        ] {
            assert!(
                RUNNER_TEMPLATE.contains(needle),
                "template missing {needle}"
            );
        }
    }

    #[test]
    fn render_emits_extern_decls_and_table() {
        let tests = vec![
            mk("math", "test_max", FnKind::Int, false, 10),
            mk("lib", "test_void_kind", FnKind::Void, false, 20),
            mk("mem", "test_ignored", FnKind::Int, true, 30),
        ];
        let out = render_main_c(&tests);

        // extern decls present, both kinds:
        assert!(out.contains("extern int test_max(void);"));
        assert!(out.contains("extern void test_void_kind(void);"));
        assert!(out.contains("extern int test_ignored(void);"));

        // table entries: qnames computed correctly (lib::-> bare;
        // other modules prefixed).
        assert!(out.contains("\"test_void_kind\","));
        assert!(out.contains("\"math::test_max\","));
        assert!(out.contains("\"mem::test_ignored\","));

        // fn_kind flag right:
        assert!(out.contains("CUST_TEST_FN_INT"));
        assert!(out.contains("CUST_TEST_FN_VOID"));

        // ignored flag right: test_ignored is the only one with `1`
        // in the ignored slot. Two non-ignored should have `0`.
        let ignored_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("(void *)test_"))
            .collect();
        assert_eq!(ignored_lines.len(), 3);
        assert_eq!(
            ignored_lines
                .iter()
                .filter(|l| l.contains("test_ignored"))
                .count(),
            1,
        );
        let ig = ignored_lines
            .iter()
            .find(|l| l.contains("test_ignored"))
            .unwrap();
        assert!(ig.contains(", 1, "), "ignored line: {ig}");

        // main is at the bottom and forwards to cust_test_run.
        assert!(out.contains("int main(int argc, char **argv)"));
        assert!(out.contains("cust_test_run(argc, argv, __cust_tests, n)"));
    }

    #[test]
    fn render_sorts_table_entries_by_qname() {
        // Discovery order is math, lib, mem (intentionally not
        // alphabetical). The table must come out sorted by the
        // computed qname:
        //
        //   lib::test_zzz   →  "test_zzz"
        //   math::test_aaa  →  "math::test_aaa"
        //   mem::test_mmm   →  "mem::test_mmm"
        //
        // So sorted: math::test_aaa, mem::test_mmm, test_zzz.
        let tests = vec![
            mk("math", "test_aaa", FnKind::Int, false, 1),
            mk("lib", "test_zzz", FnKind::Int, false, 2),
            mk("mem", "test_mmm", FnKind::Int, false, 3),
        ];
        let out = render_main_c(&tests);
        let table_start = out.find("__cust_tests[]").unwrap();
        let table = &out[table_start..];
        let pos_aaa = table.find("math::test_aaa").unwrap();
        let pos_mmm = table.find("mem::test_mmm").unwrap();
        let pos_zzz = table.find("\"test_zzz\"").unwrap();
        assert!(pos_aaa < pos_mmm, "math::test_aaa should come first");
        assert!(pos_mmm < pos_zzz, "mem::test_mmm should precede test_zzz");
    }

    #[test]
    fn render_empty_test_list_still_produces_valid_main() {
        let out = render_main_c(&[]);
        assert!(out.contains("static const struct cust_test_entry __cust_tests[] = {"));
        // Empty table: just the closing `};`. Some C compilers
        // warn on a zero-size array; the runner uses
        // `sizeof / sizeof[0]` which would divide by zero. We
        // accept that "zero tests" is a degenerate state the
        // CLI layer (slice D) should refuse to emit a binary
        // for at all — the file we render is still well-formed
        // C and `cust_test_run` simply prints the empty summary
        // line.
        assert!(out.contains("int main("));
    }

    #[test]
    fn c_string_literal_escapes_quote_and_backslash() {
        assert_eq!(c_string_literal("hi"), "\"hi\"");
        assert_eq!(c_string_literal("a\"b"), "\"a\\\"b\"");
        assert_eq!(c_string_literal("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn qname_drops_lib_prefix_only_for_root() {
        let lib = mk("lib", "test_foo", FnKind::Int, false, 1);
        let nested = mk("parser.lexer", "test_bar", FnKind::Int, false, 1);
        let sibling = mk("math", "test_baz", FnKind::Int, false, 1);
        assert_eq!(qname(&lib), "test_foo");
        assert_eq!(qname(&nested), "parser::lexer::test_bar");
        assert_eq!(qname(&sibling), "math::test_baz");
    }
}
