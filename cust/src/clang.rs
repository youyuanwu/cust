//! Clang discovery + version gate.
//!
//! v0.1 uses system clang only — no vendoring, no installer (§17).
//! Discovery order: `$CC` → `clang` on `PATH`. Minimum version is
//! 17.0 (refused below that; rationale in §17 — `-fplugin=`, C23
//! attribute syntax, `_BitInt`, `#embed` are all stable from 17).

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{anyhow, bail, Context, Result};

/// Lowest clang we accept. Aligned with `docs/design/cust-design.md`
/// §17 toolchain block.
pub const MIN_CLANG_MAJOR: u32 = 17;

/// A discovered clang installation.
#[derive(Debug, Clone)]
pub struct Clang {
    /// Path/argv0 we'll exec to invoke clang.
    pub path: PathBuf,
    /// Major version, e.g. `21` for `Ubuntu clang version 21.1.8`.
    pub version_major: u32,
    /// First line of `clang --version` verbatim — stamped into
    /// `target/.cust-version` for diagnostics.
    pub version_line: String,
}

impl Clang {
    /// Find clang, run `--version`, parse the major, and refuse if
    /// older than `MIN_CLANG_MAJOR`.
    pub fn discover() -> Result<Self> {
        let path = resolve_clang_path();
        let output = Command::new(&path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running `{} --version`", path.display()))?;

        if !output.status.success() {
            bail!(
                "`{} --version` exited with status {}",
                path.display(),
                output.status
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let version_line = stdout
            .lines()
            .next()
            .ok_or_else(|| anyhow!("`{} --version` produced no output", path.display()))?
            .trim()
            .to_string();

        let version_major = parse_clang_major(&version_line).ok_or_else(|| {
            anyhow!(
                "could not parse a clang version from `{}` (output: {:?})",
                path.display(),
                version_line
            )
        })?;

        if version_major < MIN_CLANG_MAJOR {
            bail!(
                "clang at `{}` reports `{}`, but cust v0.1 requires \
                 clang >= {MIN_CLANG_MAJOR}.0",
                path.display(),
                version_line
            );
        }

        Ok(Self {
            path,
            version_major,
            version_line,
        })
    }

    /// Start a `Command` rooted at the discovered clang binary.
    pub fn command(&self) -> Command {
        Command::new(&self.path)
    }

    /// Whether this clang accepts `-std=c23`. v0.1 simply assumes
    /// "yes" when major >= 18; clang 17 shipped `c2x` but not yet
    /// `c23` as a spelling. We fall back to `c17` below 18.
    pub const fn default_std(&self) -> &'static str {
        if self.version_major >= 18 {
            "c23"
        } else {
            "c17"
        }
    }
}

fn resolve_clang_path() -> PathBuf {
    if let Some(cc) = std::env::var_os("CC") {
        if !cc.is_empty() {
            return PathBuf::from(cc);
        }
    }
    // Bare `clang` — `Command` will resolve via `PATH`. We don't
    // probe `which clang` ourselves; if the process spawn fails the
    // `Command::output` error in `discover` will explain.
    PathBuf::from("clang")
}

/// Extract the leading `MAJOR` from a `clang --version` first line.
///
/// Handles all of:
///   "clang version 17.0.6"
///   "Ubuntu clang version 21.1.8"
///   "Homebrew clang version 18.1.8"
///   "Apple clang version 16.0.0 (clang-1600.0.26.4)"
fn parse_clang_major(line: &str) -> Option<u32> {
    let after = line.split("clang version ").nth(1)?;
    let major = after
        .split(|c: char| c == '.' || c.is_whitespace())
        .next()?;
    major.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_clang_major;

    #[test]
    fn parses_vanilla() {
        assert_eq!(parse_clang_major("clang version 17.0.6"), Some(17));
    }

    #[test]
    fn parses_ubuntu_distro_prefix() {
        assert_eq!(parse_clang_major("Ubuntu clang version 21.1.8"), Some(21));
    }

    #[test]
    fn parses_apple_clang() {
        assert_eq!(
            parse_clang_major("Apple clang version 16.0.0 (clang-1600.0.26.4)"),
            Some(16)
        );
    }

    #[test]
    fn rejects_unrelated_line() {
        assert_eq!(parse_clang_major("gcc (Ubuntu 13.2.0)"), None);
    }
}
