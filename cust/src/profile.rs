//! Profile resolution + flag mapping.
//!
//! Profile defaults and the field→flag mapping table are pinned in
//! `docs/design/cust-design.md` §17. Only `opt-level`, `debug`,
//! `sanitize`, and `extra-cflags` are honoured in v0.1; other fields
//! (`lto`, `codegen-units`, `panic`) parse but are ignored.

use anyhow::{bail, Result};

use crate::manifest;

/// Which built-in profile to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Dev,
    Release,
}

impl ProfileKind {
    /// Sub-directory name under `target/`.
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Dev => "debug",
            Self::Release => "release",
        }
    }

    pub const fn manifest_name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
        }
    }
}

/// A fully resolved profile, with all v0.1 defaults filled in.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub opt_level: OptLevel,
    pub debug: DebugInfo,
    pub sanitize: Vec<String>,
    pub extra_cflags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

impl OptLevel {
    pub const fn flag(self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::O3 => "-O3",
            Self::Os => "-Os",
            Self::Oz => "-Oz",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugInfo {
    None,
    LineTablesOnly,
    Full,
}

impl DebugInfo {
    /// Flag(s) appended after the opt-level flag.
    pub const fn flags(self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::LineTablesOnly => &["-gline-tables-only"],
            Self::Full => &["-g3", "-gdwarf-5"],
        }
    }
}

impl ResolvedProfile {
    /// Build a profile from the v0.1 defaults plus optional manifest
    /// overrides.
    pub fn resolve(kind: ProfileKind, overrides: Option<&manifest::Profile>) -> Result<Self> {
        let (default_opt, default_debug) = match kind {
            ProfileKind::Dev => (OptLevel::O0, DebugInfo::Full),
            ProfileKind::Release => (OptLevel::O3, DebugInfo::LineTablesOnly),
        };

        let mut p = Self {
            opt_level: default_opt,
            debug: default_debug,
            sanitize: Vec::new(),
            extra_cflags: Vec::new(),
        };

        if let Some(o) = overrides {
            if let Some(v) = &o.opt_level {
                p.opt_level = parse_opt_level(v)?;
            }
            if let Some(d) = &o.debug {
                p.debug = parse_debug(d)?;
            }
            if let Some(s) = &o.sanitize {
                p.sanitize.clone_from(s);
            }
            if let Some(f) = &o.extra_cflags {
                p.extra_cflags.clone_from(f);
            }
        }

        Ok(p)
    }

    /// Profile flags in the order the design table specifies:
    /// `<opt> <debug...> <sanitize?> <extra-cflags...>`.
    pub fn cflags(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(4 + self.extra_cflags.len());
        out.push(self.opt_level.flag().to_string());
        for f in self.debug.flags() {
            out.push((*f).to_string());
        }
        if !self.sanitize.is_empty() {
            out.push(format!("-fsanitize={}", self.sanitize.join(",")));
        }
        out.extend(self.extra_cflags.iter().cloned());
        out
    }
}

fn parse_opt_level(v: &toml::Value) -> Result<OptLevel> {
    match v {
        toml::Value::Integer(i) => match i {
            0 => Ok(OptLevel::O0),
            1 => Ok(OptLevel::O1),
            2 => Ok(OptLevel::O2),
            3 => Ok(OptLevel::O3),
            other => bail!("invalid opt-level integer {other} (expected 0..=3)"),
        },
        toml::Value::String(s) => match s.as_str() {
            "0" => Ok(OptLevel::O0),
            "1" => Ok(OptLevel::O1),
            "2" => Ok(OptLevel::O2),
            "3" => Ok(OptLevel::O3),
            "s" => Ok(OptLevel::Os),
            "z" => Ok(OptLevel::Oz),
            other => bail!("invalid opt-level string {other:?} (expected 0..3, s, z)"),
        },
        other => bail!("invalid opt-level value {other:?} — want integer or string"),
    }
}

fn parse_debug(s: &str) -> Result<DebugInfo> {
    match s {
        "none" => Ok(DebugInfo::None),
        "line-tables-only" => Ok(DebugInfo::LineTablesOnly),
        "full" => Ok(DebugInfo::Full),
        other => bail!("invalid debug = {other:?} (expected none|line-tables-only|full)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{OptLevel, ProfileKind, ResolvedProfile};

    #[test]
    fn dev_defaults() {
        let p = ResolvedProfile::resolve(ProfileKind::Dev, None).unwrap();
        assert_eq!(p.opt_level, OptLevel::O0);
        let flags = p.cflags();
        assert_eq!(flags[0], "-O0");
        assert!(flags.contains(&"-g3".to_string()));
        assert!(flags.contains(&"-gdwarf-5".to_string()));
    }

    #[test]
    fn release_defaults() {
        let p = ResolvedProfile::resolve(ProfileKind::Release, None).unwrap();
        assert_eq!(p.opt_level, OptLevel::O3);
        let flags = p.cflags();
        assert_eq!(flags[0], "-O3");
        assert!(flags.contains(&"-gline-tables-only".to_string()));
    }
}
