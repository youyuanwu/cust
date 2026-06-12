//! Diagnostic rewriter (V42D-18) — turns cmake/ninja output into
//! cust-shape diagnostics.
//!
//! Per-line, pure function. Wired in at the cmake/ninja child-
//! process spawn site (`cmake_emit::subprocess`) as a stdout +
//! stderr transformer. Zero I/O here; tests are pure string-in
//! string-out.
//!
//! Slice B (V42D-18 minimum shape): one classifier + three rules
//! (only `Ninja`'s missing-input case rewrites; Compiler/CMake/Other
//! pass through verbatim). RQ-V42-2 locks: ship these three and
//! grow on demand. The block-context-aware story stays out.

use std::borrow::Cow;

/// What kind of tool produced this line. Slice B uses cheap
/// prefix / substring heuristics — no regex, no state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Clang diagnostic in canonical `<file>:<line>:<col>:
    /// (error|warning|note):` shape. Verbatim passthrough — the
    /// diagnostic is already source-relative.
    Compiler,
    /// Line starts with `ninja: ` (typically `ninja: error: ...`
    /// or `ninja: build stopped: ...`).
    Ninja,
    /// Line starts with `CMake Error` / `CMake Warning` — the
    /// `cmake -G Ninja` configure step's signature.
    Cmake,
    /// Anything else: target-printer banners (`[1/8] Building C
    /// object …`), linker prints, etc. Verbatim passthrough.
    Other,
}

/// Classify `line` into one of the four sources. Cheap prefix +
/// substring checks; no allocation.
#[must_use]
pub fn classify(line: &str) -> Source {
    if line.starts_with("ninja: ") {
        Source::Ninja
    } else if line.starts_with("CMake Error") || line.starts_with("CMake Warning") {
        Source::Cmake
    } else if is_compiler_diag(line) {
        Source::Compiler
    } else {
        Source::Other
    }
}

/// True when `line` looks like a clang diagnostic. Looks for
/// `: error:` / `: warning:` / `: note:` anywhere in the line —
/// the canonical `<file>:<line>:<col>: <kind>:` shape clang
/// emits in default output mode.
fn is_compiler_diag(line: &str) -> bool {
    line.contains(": error:") || line.contains(": warning:") || line.contains(": note:")
}

/// Rewrite `line` for display. Pure function. Returns
/// `Cow::Borrowed(line)` when no transform applies (the common
/// case for compiler diags and most ninja output).
///
/// Slice B rules:
///
/// 1. `Source::Compiler` → verbatim.
/// 2. `Source::Ninja` → detect the missing-input case and
///    reformat to a cust-shape diagnostic; everything else
///    passes through verbatim (RQ-V42-2: minimum shape, grow on
///    demand).
/// 3. `Source::Cmake` → verbatim. (Long-term: point configure
///    errors back at the offending `Cust.toml` key — needs a
///    manifest-parse-time key-location table that isn't in
///    v0.4.2 scope.)
/// 4. `Source::Other` → verbatim.
#[must_use]
pub fn rewrite(line: &str) -> Cow<'_, str> {
    match classify(line) {
        Source::Compiler | Source::Cmake | Source::Other => Cow::Borrowed(line),
        Source::Ninja => rewrite_ninja(line),
    }
}

/// Try to rewrite the missing-input ninja error into a cust-
/// shape diagnostic; fall back to verbatim for anything else.
///
/// `Ninja`'s missing-input format (`Ninja` 1.10+):
///
/// ```text
/// ninja: error: '<file>', needed by '<target>', missing and no known rule to make it
/// ```
///
/// We reshape to:
///
/// ```text
/// error[cust]: missing build input
///   --> <file> (needed by `<target>`)
/// ```
///
/// Pattern stolen verbatim from musto
/// (`musto/src/diag_rewrite.cppm`). See
/// `docs/design/prior-art-musto.md` §4.
fn rewrite_ninja(line: &str) -> Cow<'_, str> {
    if let Some(rest) = line.strip_prefix("ninja: error: ") {
        if let Some((file, needed_by)) = parse_missing_input(rest) {
            return Cow::Owned(format!(
                "error[cust]: missing build input\n  --> {file} (needed by `{needed_by}`)"
            ));
        }
    }
    Cow::Borrowed(line)
}

/// Parse the "'<file>', needed by '<target>', …" shape.
/// Returns `(file, needed_by)` on success, `None` otherwise.
fn parse_missing_input(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('\'')?;
    let (file, rest) = s.split_once("', needed by '")?;
    let needed_by = rest.split('\'').next()?;
    Some((file, needed_by))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_clang_error() {
        assert_eq!(
            classify("src/lib.c:42:5: error: undeclared identifier 'foo'"),
            Source::Compiler
        );
    }

    #[test]
    fn classify_clang_warning_and_note() {
        assert_eq!(
            classify("src/lib.c:1:1: warning: unused variable"),
            Source::Compiler
        );
        assert_eq!(
            classify("src/lib.c:1:1: note: declared here"),
            Source::Compiler
        );
    }

    #[test]
    fn classify_ninja_error() {
        assert_eq!(
            classify("ninja: error: 'foo.o', needed by 'libcstd.a', missing"),
            Source::Ninja
        );
        assert_eq!(
            classify("ninja: build stopped: subcommand failed."),
            Source::Ninja
        );
    }

    #[test]
    fn classify_cmake_error_and_warning() {
        assert_eq!(
            classify("CMake Error at CMakeLists.txt:5 (project):"),
            Source::Cmake
        );
        assert_eq!(
            classify("CMake Warning at CMakeLists.txt:10:"),
            Source::Cmake
        );
    }

    #[test]
    fn classify_other() {
        assert_eq!(
            classify("[1/8] Building C object cstd.dir/foo.c.o"),
            Source::Other
        );
        assert_eq!(classify(""), Source::Other);
        assert_eq!(
            classify("-- The C compiler identification is Clang 21.0.0"),
            Source::Other
        );
    }

    #[test]
    fn rewrite_compiler_diag_verbatim() {
        let line = "src/lib.c:42:5: error: undeclared identifier 'foo'";
        assert_eq!(rewrite(line), line);
        // Should not allocate.
        assert!(matches!(rewrite(line), Cow::Borrowed(_)));
    }

    #[test]
    fn rewrite_ninja_missing_input() {
        let line = "ninja: error: 'src/util.c.o', needed by 'libcstd.a', missing and no known rule to make it";
        let out = rewrite(line);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(
            out.as_ref(),
            "error[cust]: missing build input\n  --> src/util.c.o (needed by `libcstd.a`)"
        );
    }

    #[test]
    fn rewrite_other_ninja_line_passthrough() {
        let line = "ninja: build stopped: subcommand failed.";
        assert_eq!(rewrite(line), line);
        assert!(matches!(rewrite(line), Cow::Borrowed(_)));
    }

    #[test]
    fn rewrite_cmake_line_passthrough() {
        // Configure-time CMake errors pass through verbatim in
        // v0.4.2; the key-mapping rewrite is a follow-up.
        let line = "CMake Error at CMakeLists.txt:5 (project): something broke";
        assert_eq!(rewrite(line), line);
    }

    #[test]
    fn rewrite_other_passthrough() {
        let line = "[1/8] Building C object cstd.dir/lib.c.o";
        assert_eq!(rewrite(line), line);
    }

    #[test]
    fn parse_missing_input_happy_path() {
        let s = "'a.o', needed by 'lib.a', missing and no known rule";
        assert_eq!(parse_missing_input(s), Some(("a.o", "lib.a")));
    }

    #[test]
    fn parse_missing_input_rejects_malformed() {
        assert_eq!(parse_missing_input("no leading quote"), None);
        assert_eq!(parse_missing_input("'unterminated"), None);
        assert_eq!(parse_missing_input("'a.o', other text"), None);
    }
}
