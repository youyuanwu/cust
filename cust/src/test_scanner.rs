//! Driver pre-pass scanner for `cust_test` / `cust_test_ignore`
//! functions.
//!
//! This is the v0.3.2 test-discovery backend (V32D-2 in
//! `docs/design/v0.3.2.md`). It is the *first* of two discovery
//! backends; plugin v1 in v0.4 joins as a second backend behind
//! the same `__cust_tests[]` contract, with the pre-pass kept
//! as the permanent `cust check --no-plugin` discovery path.
//!
//! Recognition rule (V32D-2):
//!
//! The marker (`cust_test` or `cust_test_ignore`), the return
//! type (`int` or `void`), and the function name must all appear
//! on the same source line. Concretely the scanner matches:
//!
//! ```text
//! ^[ \t]* cust_test(_ignore)?  (int|void)  IDENT  (void)
//! ```
//!
//! where whitespace runs between tokens are arbitrary (spaces +
//! tabs) and the `(void)` parameter list permits whitespace
//! around the keyword. Anything else on the line after the
//! `(void)` is ignored — typically the user types `{` to open
//! the function body — so multi-line bodies are fine, only the
//! signature has to be on one line.
//!
//! The scanner respects C comment and string-literal state so
//! `/* cust_test int foo(void) */` and `"cust_test int foo(void)"`
//! never match. It does **not** look inside `#if 0` (matching
//! `mod_scanner`'s documented stance).
//!
//! Plugin v1 in v0.4 walks an AST and has no such single-line
//! restriction; users who prefer Cargo-style line breaks can
//! opt in once that backend ships.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// Whether the discovered test function returns `int` or `void`.
/// Threads through to the generated runner so it knows whether
/// to inspect the return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    Int,
    Void,
}

/// One discovered test function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    /// Module's qualified name (matches `modules::Module.qualified_name`).
    pub module: String,
    /// Bare function name (no module prefix).
    pub name: String,
    pub fn_kind: FnKind,
    /// `cust_test_ignore`-marked. Cargo-style "ignored" tests
    /// are listed by the runner but never forked.
    pub ignored: bool,
    /// Absolute source file path (typically `Module.source_path`).
    pub file: PathBuf,
    /// 1-based source line of the signature.
    pub line: u32,
}

/// Scan `src` for `cust_test` / `cust_test_ignore` signatures.
///
/// `module` and `file` are recorded verbatim into each emitted
/// `TestEntry`; the scanner does no I/O of its own.
#[allow(clippy::too_many_lines)] // state machine reads more clearly inline than split
pub fn scan(src: &str, module: &str, file: &Path) -> Result<Vec<TestEntry>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut state = LexState::Code;
    let mut at_line_start = true;
    let mut line: u32 = 1;
    let mut i = 0usize;

    while i < bytes.len() {
        match state {
            LexState::Code => {
                // Honour `\<newline>` line continuation everywhere.
                if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    line += 1;
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                    at_line_start = true;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'/' && i + 1 < bytes.len() {
                    match bytes[i + 1] {
                        b'/' => {
                            state = LexState::LineComment;
                            i += 2;
                            continue;
                        }
                        b'*' => {
                            state = LexState::BlockComment;
                            i += 2;
                            continue;
                        }
                        _ => {}
                    }
                }
                if bytes[i] == b'"' {
                    state = LexState::StringLit;
                    at_line_start = false;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'\'' {
                    state = LexState::CharLit;
                    at_line_start = false;
                    i += 1;
                    continue;
                }
                // Skip horizontal whitespace at the start of a line
                // without leaving line-start mode.
                if at_line_start && matches!(bytes[i], b' ' | b'\t') {
                    i += 1;
                    continue;
                }
                if at_line_start {
                    if let Some((next_i, entry)) = try_match_signature(bytes, i, line, module, file)
                    {
                        out.push(entry);
                        i = next_i;
                        at_line_start = false;
                        continue;
                    }
                }
                at_line_start = false;
                i += 1;
            }
            LexState::LineComment => {
                // Line comment continues until an unescaped newline.
                if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    line += 1;
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                    at_line_start = true;
                    state = LexState::Code;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            LexState::BlockComment => {
                if bytes[i] == b'\n' {
                    line += 1;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    state = LexState::Code;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            LexState::StringLit => {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    if bytes[i + 1] == b'\n' {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    // Unterminated string literal; let clang complain.
                    line += 1;
                    state = LexState::Code;
                    at_line_start = true;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'"' {
                    state = LexState::Code;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            LexState::CharLit => {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    if bytes[i + 1] == b'\n' {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                    state = LexState::Code;
                    at_line_start = true;
                    i += 1;
                    continue;
                }
                if bytes[i] == b'\'' {
                    state = LexState::Code;
                    i += 1;
                    continue;
                }
                i += 1;
            }
        }
    }

    // Duplicate-name detection within the same module — saves the
    // user from a confusing C linker error in slice C, and matches
    // Rust's `error[E0428]: the name ... is defined multiple times`.
    for i in 0..out.len() {
        for j in (i + 1)..out.len() {
            if out[i].name == out[j].name {
                bail!(
                    "duplicate test function `{}` in module `{}` (`{}` line {} and line {})",
                    out[i].name,
                    module,
                    file.display(),
                    out[i].line,
                    out[j].line,
                );
            }
        }
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexState {
    Code,
    LineComment,
    BlockComment,
    StringLit,
    CharLit,
}

/// Try to match `cust_test[_ignore] (int|void) IDENT ( void )` at
/// `start`. Returns `(byte_index_just_past_the_match, entry)` on
/// success; `None` otherwise. Does not consume on failure.
fn try_match_signature(
    bytes: &[u8],
    start: usize,
    line: u32,
    module: &str,
    file: &Path,
) -> Option<(usize, TestEntry)> {
    // 1. The marker keyword.
    let (marker, after_marker) = match_one_of(bytes, start, &["cust_test_ignore", "cust_test"])?;
    let ignored = marker == "cust_test_ignore";
    // Marker must be followed by whitespace (i.e. the next char is
    // not an identifier-continuation character).
    if !next_is_word_boundary(bytes, after_marker) {
        return None;
    }
    let i = skip_inline_ws(bytes, after_marker);

    // 2. Return type: `int` or `void`.
    let (ret, after_ret) = match_one_of(bytes, i, &["int", "void"])?;
    if !next_is_word_boundary(bytes, after_ret) {
        return None;
    }
    let fn_kind = if ret == "int" {
        FnKind::Int
    } else {
        FnKind::Void
    };
    let i = skip_inline_ws(bytes, after_ret);

    // 3. Identifier (function name).
    let (name, after_name) = match_ident(bytes, i)?;
    let i = skip_inline_ws(bytes, after_name);

    // 4. `( void )` parameter list.
    if bytes.get(i) != Some(&b'(') {
        return None;
    }
    let i = skip_inline_ws(bytes, i + 1);
    let (void_kw, after_void) = match_one_of(bytes, i, &["void"])?;
    debug_assert_eq!(void_kw, "void");
    if !next_is_word_boundary(bytes, after_void) {
        return None;
    }
    let i = skip_inline_ws(bytes, after_void);
    if bytes.get(i) != Some(&b')') {
        return None;
    }
    let next = i + 1;

    Some((
        next,
        TestEntry {
            module: module.to_string(),
            name: name.to_string(),
            fn_kind,
            ignored,
            file: file.to_path_buf(),
            line,
        },
    ))
}

/// Try to match one of `candidates` (longest first preferred —
/// callers pass the longest candidate first to avoid `cust_test`
/// shadowing `cust_test_ignore`). Returns the matched literal and
/// the index just past it.
fn match_one_of<'a>(bytes: &[u8], at: usize, candidates: &[&'a str]) -> Option<(&'a str, usize)> {
    for &cand in candidates {
        let cb = cand.as_bytes();
        if at + cb.len() <= bytes.len() && &bytes[at..at + cb.len()] == cb {
            return Some((cand, at + cb.len()));
        }
    }
    None
}

/// Match an identifier `[A-Za-z_][A-Za-z0-9_]*`. Returns the
/// borrowed slice + the index just past it.
fn match_ident(bytes: &[u8], at: usize) -> Option<(&str, usize)> {
    if at >= bytes.len() {
        return None;
    }
    let first = bytes[at];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = at + 1;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    // SAFETY of from_utf8_unchecked avoided — the bytes are ASCII
    // identifier chars by construction, so str::from_utf8 cannot
    // fail. Using checked form keeps unsafe out of the module.
    let s = std::str::from_utf8(&bytes[at..end]).ok()?;
    Some((s, end))
}

/// Skip spaces and tabs (but not newlines). Returns the new
/// index.
fn skip_inline_ws(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

/// True if the byte at `at` is not an identifier-continuation
/// character (i.e. matching `cust_test` was a complete keyword
/// match and not a prefix of `cust_testify`).
fn next_is_word_boundary(bytes: &[u8], at: usize) -> bool {
    bytes
        .get(at)
        .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_simple(src: &str) -> Vec<TestEntry> {
        scan(src, "mymod", Path::new("/tmp/mymod.c")).expect("scan ok")
    }

    #[test]
    fn matches_basic_int_test() {
        let src = "cust_test int test_foo(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test_foo");
        assert_eq!(entries[0].fn_kind, FnKind::Int);
        assert!(!entries[0].ignored);
        assert_eq!(entries[0].line, 1);
        assert_eq!(entries[0].module, "mymod");
    }

    #[test]
    fn matches_void_test() {
        let src = "cust_test void test_bar(void) { }\n";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fn_kind, FnKind::Void);
    }

    #[test]
    fn matches_ignored_test() {
        let src = "cust_test_ignore int test_skip(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].ignored);
        assert_eq!(entries[0].name, "test_skip");
    }

    #[test]
    fn cust_test_must_be_separate_token_from_ignore() {
        // `cust_testify` must NOT match `cust_test` because the
        // marker has to be followed by whitespace, not an
        // identifier-continuation char.
        let src = "cust_testify int test_oops(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn marker_must_be_at_line_start() {
        // Leading non-whitespace (e.g. an arbitrary stray token)
        // disqualifies the line.
        let src = "x cust_test int test_foo(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn leading_whitespace_is_fine() {
        let src = "    \tcust_test int test_foo(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test_foo");
    }

    #[test]
    fn signature_must_be_on_one_line() {
        // V32D-2 trade-off: multi-line signatures are rejected.
        // The body itself can sprawl freely; only the prototype
        // has to fit on one line.
        let src = "cust_test int\n    test_split(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn requires_void_parameter_list() {
        // A bare `()` is *not* accepted in v0.3.2 — V32D-3 specs
        // `(void)` only, matching the macro-spec example.
        let src_bare = "cust_test int test_foo() { return 0; }\n";
        assert!(scan_simple(src_bare).is_empty());

        let src_args = "cust_test int test_foo(int x) { return x; }\n";
        assert!(scan_simple(src_args).is_empty());
    }

    #[test]
    fn ignores_match_inside_block_comment() {
        let src = "/* cust_test int test_in_comment(void) */ int main(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn ignores_match_inside_line_comment() {
        let src = "// cust_test int test_in_comment(void) {\nint main(void) { return 0; }\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn ignores_match_inside_string_literal() {
        let src = "const char *s = \"cust_test int test_in_str(void) { }\";\n";
        let entries = scan_simple(src);
        assert!(entries.is_empty());
    }

    #[test]
    fn block_comment_does_not_break_line_counter() {
        let src = "\
/* line 1
   line 2
   line 3 */
cust_test int test_after_comment(void) { return 0; }
";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].line, 4);
    }

    #[test]
    fn multiple_tests_in_one_module() {
        let src = "\
cust_test int test_a(void) { return 0; }
cust_test void test_b(void) { }
cust_test_ignore int test_c(void) { return 0; }
";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "test_a");
        assert_eq!(entries[0].line, 1);
        assert_eq!(entries[1].name, "test_b");
        assert_eq!(entries[1].fn_kind, FnKind::Void);
        assert_eq!(entries[1].line, 2);
        assert_eq!(entries[2].name, "test_c");
        assert!(entries[2].ignored);
        assert_eq!(entries[2].line, 3);
    }

    #[test]
    fn whitespace_inside_paren_void_is_ok() {
        let src = "cust_test int test_foo (  void  ) { return 0; }\n";
        let entries = scan_simple(src);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test_foo");
    }

    #[test]
    fn rejects_non_test_function_kinds() {
        // Return type must be `int` or `void` exactly. `long`,
        // `size_t`, `static int` etc. are rejected at the scanner
        // level — those would have to be plain functions, not
        // marked with `cust_test`.
        for src in [
            "cust_test long test_foo(void) { return 0; }\n",
            "cust_test size_t test_foo(void) { return 0; }\n",
            "cust_test static int test_foo(void) { return 0; }\n",
        ] {
            assert!(scan_simple(src).is_empty(), "should reject: {src:?}");
        }
    }

    #[test]
    fn duplicate_test_names_error() {
        let src = "\
cust_test int test_foo(void) { return 0; }
cust_test void test_foo(void) { }
";
        let err = scan(src, "m", Path::new("/tmp/m.c")).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("duplicate test function `test_foo`"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn empty_source_returns_no_tests() {
        assert!(scan_simple("").is_empty());
        assert!(scan_simple("\n\n\n").is_empty());
        assert!(scan_simple("int main(void) { return 0; }\n").is_empty());
    }
}
