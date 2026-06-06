//! V0.4.0 test-discovery sidecar consumer (V40D-6, RQ-V40-2).
//!
//! The plugin (`plugin/src/plugin.cc`) emits per-module
//! tab-separated test entries into
//! `target/<profile>/.test-discovery/<crate>/<module>.cust.tests`
//! during the V40D-5 phase-1 surface pass. This module parses
//! those files into `Vec<TestEntry>` for the runner template.
//!
//! Format per RQ-V40-2:
//!
//! ```text
//! <qname>\t<fn_kind>\t<ignored>\t<file>\t<line>\n
//! ```
//!
//! * `qname` — `module::name`, dropped-`lib`-prefix happens at
//!   runner-template emission time (matches v0.3.2's behaviour).
//! * `fn_kind` — literal `int` or `void`.
//! * `ignored` — `0` or `1`.
//! * `file` — absolute path; the plugin rejects literal tabs
//!   or newlines, so we don't have to escape.
//! * `line` — 1-based decimal.
//!
//! v0.3.2's `TestEntry` and `FnKind` live here now too. Slice D
//! deletes the old `test_scanner` module (V40D-6: the v0.3.2
//! pre-pass scanner is gone, plugin is the only discovery
//! backend).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Whether the discovered test function returns `int` or `void`.
/// Threads through to the generated runner so it knows whether
/// to inspect the return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    Int,
    Void,
}

/// One discovered test function. v0.3.2 shape preserved so the
/// runner-template generator (`test_runner::render_main_c`)
/// doesn't change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    /// Module's qualified name (matches `modules::Module.qualified_name`).
    pub module: String,
    /// Bare function name (no module prefix).
    pub name: String,
    pub fn_kind: FnKind,
    /// `cust::test_ignore`-marked. Cargo-style "ignored" tests
    /// are listed by the runner but never forked.
    pub ignored: bool,
    /// Absolute source file path (typically `Module.source_path`).
    pub file: PathBuf,
    /// 1-based source line of the signature.
    pub line: u32,
}

/// Parse one sidecar file's TSV bytes into `Vec<TestEntry>`.
///
/// `path` is reported in errors (the actual file content lives
/// in `contents`). Blank lines are ignored to be conservative,
/// even though the plugin should never emit them.
pub fn parse(contents: &str, path: &Path) -> Result<Vec<TestEntry>> {
    let mut out = Vec::new();
    for (lineno, raw) in contents.lines().enumerate() {
        if raw.is_empty() {
            continue;
        }
        let mut fields = raw.split('\t');
        let qname = next_field(&mut fields, "qname", path, lineno)?;
        let fn_kind_str = next_field(&mut fields, "fn_kind", path, lineno)?;
        let ignored_str = next_field(&mut fields, "ignored", path, lineno)?;
        let file_str = next_field(&mut fields, "file", path, lineno)?;
        let line_str = next_field(&mut fields, "line", path, lineno)?;
        if fields.next().is_some() {
            bail!(
                "sidecar `{}`: line {} has too many fields (expected 5 tab-separated)",
                path.display(),
                lineno + 1
            );
        }

        let (module, name) = qname.rsplit_once("::").ok_or_else(|| {
            anyhow::anyhow!(
                "sidecar `{}`: line {} qname `{qname}` missing `::` separator",
                path.display(),
                lineno + 1
            )
        })?;
        let fn_kind = match fn_kind_str {
            "int" => FnKind::Int,
            "void" => FnKind::Void,
            other => bail!(
                "sidecar `{}`: line {} fn_kind `{other}` not in {{int, void}}",
                path.display(),
                lineno + 1
            ),
        };
        let ignored = match ignored_str {
            "0" => false,
            "1" => true,
            other => bail!(
                "sidecar `{}`: line {} ignored `{other}` not in {{0, 1}}",
                path.display(),
                lineno + 1
            ),
        };
        let line: u32 = line_str.parse().with_context(|| {
            format!(
                "sidecar `{}`: line {} line column `{line_str}` is not a positive integer",
                path.display(),
                lineno + 1
            )
        })?;

        out.push(TestEntry {
            module: module.to_string(),
            name: name.to_string(),
            fn_kind,
            ignored,
            file: PathBuf::from(file_str),
            line,
        });
    }
    Ok(out)
}

fn next_field<'a>(
    fields: &mut std::str::Split<'a, char>,
    label: &str,
    path: &Path,
    lineno: usize,
) -> Result<&'a str> {
    fields.next().ok_or_else(|| {
        anyhow::anyhow!(
            "sidecar `{}`: line {} missing `{label}` field",
            path.display(),
            lineno + 1
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_entries() {
        let contents = "\
foo::a\tint\t0\t/tmp/foo.c\t10
foo::b\tvoid\t0\t/tmp/foo.c\t20
foo::c\tint\t1\t/tmp/foo.c\t30
";
        let got = parse(contents, Path::new("/tmp/sidecar")).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].module, "foo");
        assert_eq!(got[0].name, "a");
        assert_eq!(got[0].fn_kind, FnKind::Int);
        assert!(!got[0].ignored);
        assert_eq!(got[0].line, 10);
        assert_eq!(got[1].fn_kind, FnKind::Void);
        assert!(got[2].ignored);
    }

    #[test]
    fn nested_module_qname_uses_rsplit() {
        // Sub-module test: `crate::parser::lex::test_foo` should
        // split into module = `crate::parser::lex`, name = `test_foo`.
        let contents = "crate::parser::lex::test_foo\tint\t0\t/tmp/a.c\t1\n";
        let got = parse(contents, Path::new("/tmp/x")).unwrap();
        assert_eq!(got[0].module, "crate::parser::lex");
        assert_eq!(got[0].name, "test_foo");
    }

    #[test]
    fn empty_sidecar_yields_no_entries() {
        let got = parse("", Path::new("/tmp/x")).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn missing_separator_in_qname_errors() {
        let contents = "no_separator\tint\t0\t/tmp/a.c\t1\n";
        let err = parse(contents, Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("missing `::` separator"), "{err}");
    }

    #[test]
    fn bad_fn_kind_errors() {
        let contents = "foo::a\tlong\t0\t/tmp/a.c\t1\n";
        let err = parse(contents, Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("fn_kind `long`"), "{err}");
    }

    #[test]
    fn bad_ignored_errors() {
        let contents = "foo::a\tint\t2\t/tmp/a.c\t1\n";
        let err = parse(contents, Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("ignored `2`"), "{err}");
    }

    #[test]
    fn too_many_fields_errors() {
        let contents = "foo::a\tint\t0\t/tmp/a.c\t1\textra\n";
        let err = parse(contents, Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("too many fields"), "{err}");
    }

    #[test]
    fn too_few_fields_errors() {
        let contents = "foo::a\tint\t0\n";
        let err = parse(contents, Path::new("/tmp/x")).unwrap_err();
        assert!(err.to_string().contains("missing `file`"), "{err}");
    }
}
