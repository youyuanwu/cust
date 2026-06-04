//! Driver pre-pass scanner for `#cust …;` directives.
//!
//! This is the v0.2 module loader's input layer (V2D-1, see
//! `docs/design/v0.2.md`). It runs *before* clang and recognises:
//!
//! * `#cust mod <ident>;`         declare a submodule
//! * `#cust use crate::<ident>;`  pull in a sibling module's
//!   fragment header
//!
//! Future extensions (v0.3+): `#cust pub_macro`, `#cust
//! include_generated!(...)`, attribute-form `[[cust::cfg]]` /
//! `[[cust::feature]]` gating on module-level decls.
//!
//! Invariants the scanner commits to up front so later extensions
//! are parser extensions, not rewrites:
//!
//! 1. **Real comment + string-literal state machine.** Three states
//!    (code / line-comment / block-comment), plus string and char
//!    literal recognition with `\` escapes. `\<NEWLINE>` line
//!    continuation honoured everywhere. Not regex.
//! 2. **Real tokeniser for the directive body.** `#cust mod foo;`
//!    is tokenised into `mod`, `foo`, `;` — adding new directive
//!    keywords is a parser branch, not a re-implementation.
//! 3. **Position-preserving rewrites.** Each emitted span is paired
//!    with the source `(line, column)` it came from so the
//!    eventual `#line` directives let clang point diagnostics at
//!    the user's source rather than the rewritten file.
//!
//! Recognition rules (v0.2):
//!
//! * A directive line starts with `#` at **column 0** (no leading
//!   whitespace), followed immediately by `cust`.
//! * The directive runs to the matching `;` at the end of the
//!   logical line. Multi-line directives via `\<NEWLINE>` are
//!   tolerated but not required.
//! * Unknown directives (`#cust frob …;`) are a hard error — never
//!   silently passed through.
//! * The scanner does **not** look inside `#if 0` / `#ifdef`. A
//!   `#cust` line nested inside a dead `#if 0` block is still
//!   processed. Documented loudly.

use std::path::Path;

use anyhow::{bail, Result};

/// One scanned `#cust …;` directive plus its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub kind: DirectiveKind,
    /// Span of the entire directive (from `#` through `;` inclusive).
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveKind {
    /// `#cust mod <ident>;`
    Mod { name: String },
    /// `#cust use crate::<ident>;`
    UseCrate { name: String },
}

/// A byte range in the source plus the source `(line, column)` of
/// its starting byte. Lines + columns are 1-based to match clang
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub byte_start: usize,
    pub byte_end: usize,
    pub line: u32,
    pub column: u32,
}

/// Result of scanning one source file.
#[derive(Debug)]
pub struct ScanResult {
    pub directives: Vec<Directive>,
}

/// Scan `src` for `#cust …;` directives.
///
/// `file` is included in error messages only; the scanner does no
/// I/O of its own.
pub fn scan(src: &str, file: &Path) -> Result<ScanResult> {
    let mut s = Scanner::new(src);
    let mut directives = Vec::new();

    while let Some(line_start) = s.next_logical_line_start() {
        // A `#cust …` directive line starts with `#` at column 0
        // (s.next_logical_line_start ensures we are at column 0;
        // s.in_code() additionally ensures we are not currently
        // inside a block comment).
        if !s.in_code() {
            s.consume_until_real_newline();
            continue;
        }
        if !s.rest().starts_with('#') {
            s.consume_until_real_newline();
            continue;
        }
        // Peek past `#` and optional whitespace to see if this is
        // a `#cust ...` line. Plain `#include`, `#define`, `#if`,
        // etc. are pass-through.
        let after_hash = &s.rest()[1..];
        let trimmed = after_hash.trim_start_matches([' ', '\t']);
        if !trimmed.starts_with("cust") {
            s.consume_until_real_newline();
            continue;
        }
        // Confirm `cust` is followed by whitespace or `;` — not
        // e.g. `#customer`. (`cust` followed by anything else is
        // some other token, not our directive.)
        let after_cust = &trimmed["cust".len()..];
        if !after_cust
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == ';')
        {
            s.consume_until_real_newline();
            continue;
        }

        let directive = s.parse_directive(line_start, file)?;
        directives.push(directive);
    }

    Ok(ScanResult { directives })
}

// ─── Scanner internals ──────────────────────────────────────────────

struct Scanner<'a> {
    src: &'a str,
    pos: usize,
    /// 1-based line number of `pos`.
    line: u32,
    /// 1-based column number of `pos`.
    column: u32,
    /// State machine state at `pos`.
    state: State,
    /// Set once `next_logical_line_start` has emitted the very
    /// first line. Distinguishes "pos==0 because we haven't
    /// started" from "pos==0 on a re-entry that should terminate".
    started: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    BlockComment,
    String,
    Char,
}

impl<'a> Scanner<'a> {
    const fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            column: 1,
            state: State::Code,
            started: false,
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn in_code(&self) -> bool {
        self.state == State::Code
    }

    /// Advance one byte (or one UTF-8 char-start; this scanner is
    /// byte-oriented for ASCII tokens but we treat UTF-8 lead
    /// bytes correctly for column counting). Updates the state
    /// machine.
    fn bump(&mut self) {
        let bytes = self.src.as_bytes();
        if self.pos >= bytes.len() {
            return;
        }
        let b = bytes[self.pos];

        // State transitions on this byte's value alone.
        match self.state {
            State::Code => self.tick_code(b),
            State::LineComment => self.tick_line_comment(b),
            State::BlockComment => self.tick_block_comment(b),
            State::String => self.tick_string(b),
            State::Char => self.tick_char(b),
        }

        // Position bookkeeping. `\\\n` continuation: when we see a
        // backslash immediately followed by newline, treat the
        // newline as not starting a new logical line (column resets
        // but the *logical* line continues). For the scanner's
        // purposes this matters for line-comment termination only;
        // the line counter still advances so diagnostics point at
        // the right physical line.
        if b == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            // Don't count UTF-8 continuation bytes toward column.
            if (b & 0xC0) != 0x80 {
                self.column += 1;
            }
        }
        self.pos += 1;
    }

    fn tick_code(&mut self, b: u8) {
        let rest = self.rest();
        // Two-byte starters: //  /*  (must check before consuming b).
        if b == b'/' && rest.len() >= 2 {
            match rest.as_bytes()[1] {
                b'/' => self.state = State::LineComment,
                b'*' => self.state = State::BlockComment,
                _ => {}
            }
        } else if b == b'"' {
            self.state = State::String;
        } else if b == b'\'' {
            self.state = State::Char;
        }
    }

    fn tick_line_comment(&mut self, b: u8) {
        if b == b'\n' {
            // Line continuation: `\` immediately before `\n` keeps
            // us in the comment. Look back one byte.
            if self.pos > 0 && self.src.as_bytes()[self.pos - 1] == b'\\' {
                // Stay in line-comment.
            } else {
                self.state = State::Code;
            }
        }
    }

    fn tick_block_comment(&mut self, b: u8) {
        if b == b'/' && self.pos > 0 && self.src.as_bytes()[self.pos - 1] == b'*' {
            self.state = State::Code;
        }
    }

    const fn tick_string(&mut self, b: u8) {
        if b == b'\\' {
            // Skip the next byte (escape). We bump past it without
            // re-entering tick_string — handled by an explicit
            // double-bump below in advance helpers.
        } else if b == b'"' {
            self.state = State::Code;
        }
    }

    const fn tick_char(&mut self, b: u8) {
        if b == b'\\' {
            // Same as strings — caller handles the double-bump.
        } else if b == b'\'' {
            self.state = State::Code;
        }
    }

    /// Walk to the *start* of the next logical line — i.e. the
    /// byte after the next non-continuation `\n`, OR the very
    /// beginning of the file on the first call. Returns the byte
    /// position of that line start, or None at EOF.
    ///
    /// This is the primary cursor for the outer scan loop.
    fn next_logical_line_start(&mut self) -> Option<usize> {
        // First call: hand back position 0 (only if there's any
        // input to scan). Empty files terminate immediately.
        if !self.started {
            self.started = true;
            if self.src.is_empty() {
                return None;
            }
            return Some(0);
        }
        // If we're sitting on a `\n` (left by a previous call to
        // consume_until_real_newline), step past it.
        if self.rest().starts_with('\n') {
            self.bump();
        }
        if self.pos >= self.src.len() {
            return None;
        }
        Some(self.pos)
    }

    /// Advance to (and onto) the next real newline that terminates
    /// the current logical line. Honours `\<NEWLINE>` continuation
    /// and skips string/char/comment interiors via the state
    /// machine. Leaves `self.pos` pointing AT the newline byte (so
    /// `next_logical_line_start` can step past it).
    ///
    /// The newline test is **state-agnostic** — a physical newline
    /// ends a physical line whether we're in code or in a block
    /// comment. If we exit while still in `BlockComment` state, the
    /// state survives across the newline; the next call resumes
    /// inside the comment.
    fn consume_until_real_newline(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b == b'\n' && !self.is_continuation_newline() {
                // Leave pos AT the newline; `next_logical_line_start`
                // will step over it on the next call (and the bump
                // there fires the state-machine transition for `\n`,
                // which is what closes line-comment state).
                return;
            }
            // Handle string/char escape double-bump.
            if (self.state == State::String || self.state == State::Char) && b == b'\\' {
                self.bump();
                if self.pos < bytes.len() {
                    self.bump();
                }
                continue;
            }
            self.bump();
        }
    }

    fn is_continuation_newline(&self) -> bool {
        self.pos > 0 && self.src.as_bytes()[self.pos - 1] == b'\\'
    }

    /// Parse a `#cust …;` directive starting at the current cursor
    /// (which must point at `#`). On success leaves the cursor on
    /// the byte AT the terminating `;` (`consume_until_real_newline`
    /// will then move us to the next line).
    fn parse_directive(&mut self, line_start: usize, file: &Path) -> Result<Directive> {
        let dir_line = self.line;
        let dir_col = self.column;
        let byte_start = self.pos;

        // Tokenise the directive body — everything from after
        // `#cust` to the `;`, ignoring whitespace. Comments inside
        // a directive body are not supported in v0.2 (they would
        // surprise more than they help); reject them.
        let mut tokens: Vec<Token> = Vec::new();

        // Step past `#`.
        self.bump();
        // Step past optional whitespace.
        self.skip_directive_ws();
        // Step past `cust`.
        for _ in 0..4 {
            self.bump();
        }

        let byte_end = loop {
            self.skip_directive_ws();
            if self.pos >= self.src.len() {
                bail!(
                    "{}:{}:{}: `#cust` directive not terminated by `;` before end of file",
                    file.display(),
                    dir_line,
                    dir_col
                );
            }
            let b = self.src.as_bytes()[self.pos];
            if b == b'\n' {
                bail!(
                    "{}:{}:{}: `#cust` directive not terminated by `;` on its line",
                    file.display(),
                    dir_line,
                    dir_col
                );
            }
            if b == b';' {
                break self.pos;
            }
            if b == b'/' {
                let next = self.src.as_bytes().get(self.pos + 1).copied();
                if next == Some(b'/') || next == Some(b'*') {
                    bail!(
                        "{}:{}:{}: comments inside `#cust` directive bodies are not allowed",
                        file.display(),
                        self.line,
                        self.column
                    );
                }
            }
            tokens.push(self.lex_directive_token(file)?);
        };

        // Parse the token stream into a DirectiveKind.
        let kind = parse_directive_tokens(&tokens, file, dir_line, dir_col)?;

        Ok(Directive {
            kind,
            span: Span {
                byte_start: line_start.min(byte_start),
                byte_end: byte_end + 1, // include the `;`
                line: dir_line,
                column: dir_col,
            },
        })
    }

    fn skip_directive_ws(&mut self) {
        while self.pos < self.src.len() {
            let b = self.src.as_bytes()[self.pos];
            if b == b' ' || b == b'\t' {
                self.bump();
            } else if b == b'\\' && self.src.as_bytes().get(self.pos + 1).copied() == Some(b'\n') {
                // `\<NEWLINE>` line continuation inside a directive
                // — skip both bytes and keep going.
                self.bump();
                self.bump();
            } else {
                break;
            }
        }
    }

    fn lex_directive_token(&mut self, file: &Path) -> Result<Token> {
        let bytes = self.src.as_bytes();
        let start = self.pos;
        let start_line = self.line;
        let start_col = self.column;
        let b = bytes[self.pos];

        // Punctuation: ::
        if b == b':' && bytes.get(self.pos + 1).copied() == Some(b':') {
            self.bump();
            self.bump();
            return Ok(Token {
                kind: TokenKind::ColonColon,
                text: "::".to_string(),
                line: start_line,
                column: start_col,
            });
        }

        // Identifier: [A-Za-z_][A-Za-z0-9_]*
        if b == b'_' || b.is_ascii_alphabetic() {
            while self.pos < bytes.len() {
                let c = bytes[self.pos];
                if c == b'_' || c.is_ascii_alphanumeric() {
                    self.bump();
                } else {
                    break;
                }
            }
            let text = self.src[start..self.pos].to_string();
            return Ok(Token {
                kind: TokenKind::Ident,
                text,
                line: start_line,
                column: start_col,
            });
        }

        bail!(
            "{}:{}:{}: unexpected character {:?} in `#cust` directive body",
            file.display(),
            start_line,
            start_col,
            b as char
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    text: String,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Ident,
    ColonColon,
}

fn parse_directive_tokens(
    tokens: &[Token],
    file: &Path,
    line: u32,
    col: u32,
) -> Result<DirectiveKind> {
    let first = tokens.first().ok_or_else(|| {
        anyhow::anyhow!(
            "{}:{}:{}: empty `#cust` directive",
            file.display(),
            line,
            col
        )
    })?;
    if first.kind != TokenKind::Ident {
        bail!(
            "{}:{}:{}: `#cust` directive must start with a keyword (got {:?})",
            file.display(),
            first.line,
            first.column,
            first.text
        );
    }
    match first.text.as_str() {
        "mod" => parse_mod(tokens, file, line, col),
        "use" => parse_use(tokens, file, line, col),
        other => bail!(
            "{}:{}:{}: unknown `#cust` directive `{other}` (expected `mod` or `use`)",
            file.display(),
            first.line,
            first.column
        ),
    }
}

fn parse_mod(tokens: &[Token], file: &Path, line: u32, col: u32) -> Result<DirectiveKind> {
    // `mod <ident>` — exactly two tokens.
    if tokens.len() != 2 {
        bail!(
            "{}:{}:{}: `#cust mod` expects exactly one module name (got {} token(s))",
            file.display(),
            line,
            col,
            tokens.len()
        );
    }
    let ident = &tokens[1];
    if ident.kind != TokenKind::Ident {
        bail!(
            "{}:{}:{}: `#cust mod` expects an identifier (got {:?})",
            file.display(),
            ident.line,
            ident.column,
            ident.text
        );
    }
    Ok(DirectiveKind::Mod {
        name: ident.text.clone(),
    })
}

fn parse_use(tokens: &[Token], file: &Path, line: u32, col: u32) -> Result<DirectiveKind> {
    // `use crate :: <ident>` — exactly four tokens: ident("use" was
    // consumed as `first`; here tokens[0] is "use"), ident("crate"),
    // `::`, ident.
    if tokens.len() != 4 {
        bail!(
            "{}:{}:{}: `#cust use` expects `crate::<name>` (got {} token(s))",
            file.display(),
            line,
            col,
            tokens.len()
        );
    }
    let crate_tok = &tokens[1];
    let coloncolon = &tokens[2];
    let name = &tokens[3];
    if crate_tok.kind != TokenKind::Ident || crate_tok.text != "crate" {
        bail!(
            "{}:{}:{}: `#cust use` must begin with `crate::` (got {:?})",
            file.display(),
            crate_tok.line,
            crate_tok.column,
            crate_tok.text
        );
    }
    if coloncolon.kind != TokenKind::ColonColon {
        bail!(
            "{}:{}:{}: expected `::` after `crate`",
            file.display(),
            coloncolon.line,
            coloncolon.column
        );
    }
    if name.kind != TokenKind::Ident {
        bail!(
            "{}:{}:{}: `#cust use crate::` expects an identifier",
            file.display(),
            name.line,
            name.column
        );
    }
    Ok(DirectiveKind::UseCrate {
        name: name.text.clone(),
    })
}

// ─── Rewrite helpers ────────────────────────────────────────────────

/// Produce a rewritten copy of `src` with each scanned directive
/// replaced by an empty span, plus `#line N "file"` directives so
/// clang diagnostics still point at the original source.
///
/// Convenience wrapper around `rewrite_with` for callers that
/// want to blank every directive without substitution.
pub fn rewrite(src: &str, file: &Path, result: &ScanResult) -> String {
    rewrite_with(src, file, result, |_| None)
}

/// Same as `rewrite`, but the caller may supply a per-directive
/// replacement: `map_fn(&Directive)` returning `Some(text)`
/// substitutes `text` (verbatim — no trailing newline added) in
/// place of the directive's bytes; returning `None` blanks the
/// directive with whitespace as `rewrite` does.
///
/// Used by the build pipeline to lower `#cust use crate::foo;`
/// into `#include "<fragment-of-foo>"` after a surface-extraction
/// pass has emitted the matching fragment header.
///
/// `#line` re-anchoring runs unconditionally after each directive
/// so user-code diagnostics keep pointing at the original source
/// regardless of how the directive itself was lowered.
pub fn rewrite_with(
    src: &str,
    file: &Path,
    result: &ScanResult,
    mut map_fn: impl FnMut(&Directive) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(src.len() + 64 * result.directives.len());
    let file_str = file.display().to_string();
    let mut cursor = 0;

    // Emit a `#line 1 "file"` at the very top so that even
    // non-directive lines before any rewrite are anchored to the
    // user's source path.
    push_line_directive(&mut out, 1, &file_str);

    for d in &result.directives {
        // Copy `[cursor .. d.span.byte_start)` verbatim.
        out.push_str(&src[cursor..d.span.byte_start]);

        if let Some(replacement) = map_fn(d) {
            // Substitute. We don't try to pad to the original
            // byte width — `#line` directives re-anchor positions
            // anyway. Padding shorter replacements with spaces
            // would only matter for shared-line directives, and
            // v0.2 directives are always on their own line.
            out.push_str(&replacement);
        } else {
            // Blank: replace the directive bytes with spaces
            // (preserving the trailing newline if any) so that
            // anything sharing the line keeps its column layout.
            for byte in &src.as_bytes()[d.span.byte_start..d.span.byte_end] {
                if *byte == b'\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
        }

        // After the directive, re-anchor clang to the *next*
        // physical line in the original source. The directive is
        // single-line in v0.2 so `dir_line + 1` is correct (multi-
        // line continuation case is documented as deferred).
        // We do not emit a #line directive when the directive ran
        // to EOF.
        cursor = d.span.byte_end;
        if cursor < src.len() {
            // Find the next newline so we can emit the #line right
            // after it, not in the middle of a line.
            let nl = src[cursor..]
                .find('\n')
                .map_or(src.len(), |i| cursor + i + 1);
            out.push_str(&src[cursor..nl]);
            push_line_directive(&mut out, d.span.line + 1, &file_str);
            cursor = nl;
        }
    }

    out.push_str(&src[cursor..]);
    out
}

fn push_line_directive(out: &mut String, line: u32, file: &str) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "#line {line} \"{}\"", escape_for_line_directive(file));
}

fn escape_for_line_directive(file: &str) -> String {
    // `#line` accepts a C string literal; escape `\` and `"` to be
    // safe on Windows paths.
    let mut out = String::with_capacity(file.len());
    for c in file.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.c")
    }

    fn scan_ok(src: &str) -> ScanResult {
        scan(src, &p()).expect("scan should succeed")
    }

    fn scan_err(src: &str) -> String {
        format!("{:#}", scan(src, &p()).unwrap_err())
    }

    #[test]
    fn empty_input() {
        let r = scan_ok("");
        assert!(r.directives.is_empty());
    }

    #[test]
    fn plain_c_no_directives() {
        let src = "int main(void) { return 0; }\n";
        let r = scan_ok(src);
        assert!(r.directives.is_empty());
    }

    #[test]
    fn recognises_mod_directive() {
        let src = "#cust mod util;\n";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
        assert_eq!(
            r.directives[0].kind,
            DirectiveKind::Mod {
                name: "util".to_string()
            }
        );
        assert_eq!(r.directives[0].span.line, 1);
        assert_eq!(r.directives[0].span.column, 1);
    }

    #[test]
    fn recognises_use_crate_directive() {
        let src = "#cust use crate::parser;\n";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
        assert_eq!(
            r.directives[0].kind,
            DirectiveKind::UseCrate {
                name: "parser".to_string()
            }
        );
    }

    #[test]
    fn allows_whitespace_after_hash() {
        // `#  cust mod x;` — `#` at column 0 plus spaces before
        // `cust` is fine (matches how C devs sometimes format).
        let src = "#  cust mod x;\n";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
    }

    #[test]
    fn requires_hash_at_column_zero() {
        // Leading whitespace before `#` → not a directive.
        let src = " #cust mod x;\n";
        let r = scan_ok(src);
        assert!(r.directives.is_empty(), "{:?}", r.directives);
    }

    #[test]
    fn multiple_directives() {
        let src = "\
#cust mod util;
#cust mod parser;
#cust use crate::util;

int main(void) { return 0; }
";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 3);
        assert!(matches!(
            r.directives[0].kind,
            DirectiveKind::Mod { ref name } if name == "util"
        ));
        assert!(matches!(
            r.directives[2].kind,
            DirectiveKind::UseCrate { ref name } if name == "util"
        ));
        // Line tracking still accurate.
        assert_eq!(r.directives[1].span.line, 2);
        assert_eq!(r.directives[2].span.line, 3);
    }

    #[test]
    fn ignores_directives_in_block_comment() {
        let src = "\
/* #cust mod hidden;
   #cust use crate::also_hidden;
*/
#cust mod visible;
";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
        assert!(matches!(
            r.directives[0].kind,
            DirectiveKind::Mod { ref name } if name == "visible"
        ));
    }

    #[test]
    fn ignores_directives_in_line_comment() {
        let src = "// #cust mod nope;\n#cust mod yes;\n";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
        assert!(matches!(
            r.directives[0].kind,
            DirectiveKind::Mod { ref name } if name == "yes"
        ));
    }

    #[test]
    fn ignores_directive_inside_string_literal() {
        // A `#cust` inside a string literal is not a directive
        // *because the previous line opens a string that wasn't
        // closed on that line*. The state machine must not lose
        // track across newlines inside the string.
        //
        // Note: in real C this would be a syntax error (unterminated
        // string), but the scanner is upstream of clang and should
        // not crash on it.
        let src = "char *s = \"hello;\n#cust mod x;\";\n";
        let r = scan_ok(src);
        // We're still inside a string when we hit `#cust mod x;`
        // on line 2, so it should NOT be recognised.
        assert!(r.directives.is_empty(), "{:?}", r.directives);
    }

    #[test]
    fn line_continuation_in_line_comment() {
        // A `//` comment continues onto the next line via `\\<NL>`.
        let src = "// commented \\\n#cust mod still_in_comment;\n#cust mod visible;\n";
        let r = scan_ok(src);
        assert_eq!(r.directives.len(), 1);
        assert!(matches!(
            r.directives[0].kind,
            DirectiveKind::Mod { ref name } if name == "visible"
        ));
    }

    #[test]
    fn cust_substring_in_other_token_not_matched() {
        // `#customer` is not `#cust …`.
        let src = "#customer = 1;\n";
        let r = scan_ok(src);
        assert!(r.directives.is_empty());
    }

    #[test]
    fn unknown_directive_is_hard_error() {
        let e = scan_err("#cust frob foo;\n");
        assert!(e.contains("unknown `#cust` directive `frob`"), "{e}");
    }

    #[test]
    fn mod_without_name_is_error() {
        let e = scan_err("#cust mod ;\n");
        assert!(e.contains("expects exactly one module name"), "{e}");
    }

    #[test]
    fn use_without_crate_prefix_is_error() {
        let e = scan_err("#cust use parser;\n");
        assert!(e.contains("expects `crate::<name>`"), "{e}");
    }

    #[test]
    fn missing_semicolon_is_error() {
        let e = scan_err("#cust mod foo\n");
        assert!(e.contains("not terminated by `;`"), "{e}");
    }

    #[test]
    fn comment_inside_directive_is_error() {
        let e = scan_err("#cust mod /* sneaky */ foo;\n");
        assert!(
            e.contains("comments inside `#cust` directive bodies are not allowed"),
            "{e}"
        );
    }

    #[test]
    fn rewrite_blanks_directive_and_anchors_line_directive() {
        let src = "#cust mod util;\nint x = 1;\n";
        let r = scan_ok(src);
        let out = rewrite(src, &p(), &r);
        // Should start with the top-of-file #line anchor.
        assert!(out.starts_with("#line 1 \"test.c\"\n"), "{out:?}");
        // The directive line should be all spaces (and the `\n`
        // preserved) — not contain `#cust` anymore.
        assert!(
            !out.contains("#cust"),
            "rewritten output still contains `#cust`: {out:?}"
        );
        // And there should be a re-anchor #line after the
        // directive's newline.
        assert!(
            out.contains("#line 2 \"test.c\""),
            "missing re-anchor #line: {out:?}"
        );
        // The non-directive line should still be in the output
        // verbatim.
        assert!(out.contains("int x = 1;"), "{out:?}");
    }

    #[test]
    fn rewrite_preserves_byte_alignment_within_directive_line() {
        // If the directive shares its line with trailing content,
        // we don't currently support that — the rewrite still
        // produces something sane (whitespace through the `;`).
        // This test pins the simple single-directive-on-its-own-
        // line case.
        let src = "#cust mod a;\n";
        let r = scan_ok(src);
        let out = rewrite(src, &p(), &r);
        // Directive bytes (12 of them: `#cust mod a;`) should all
        // be spaces in the output, followed by the original `\n`.
        assert!(out.contains(&" ".repeat(12)), "{out:?}");
    }
}
