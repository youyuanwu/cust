//! `cust new <path> [--lib] [--name <name>]`.
//!
//! Scaffolds a new cust crate at `<path>`. The directory may exist
//! (e.g. a freshly `mkdir`-ed one) but must be empty, *or* must not
//! exist yet — we refuse to clobber any file we did not create.
//!
//! In v0.2 only `--lib` (a staticlib crate) is supported. `--bin`
//! waits for `cust run` and the binary target story.
//!
//! Generated layout:
//!
//! ```text
//! <path>/
//! ├── .gitignore        # just `/target`
//! ├── Cust.toml         # [package] name + version = "0.1.0"
//! └── src/
//!     └── lib.c         # one cust_pub function so `cust build` works
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use crate::manifest::validate_package_name;

/// Inputs to the `new` command.
pub struct NewPlan<'a> {
    /// Destination directory. May or may not exist; if it exists it
    /// must be empty.
    pub path: &'a Path,
    /// Package name. If `None`, derived from the final path
    /// component.
    pub name: Option<&'a str>,
    /// Only `Lib` in v0.2.
    #[allow(dead_code)] // currently always `Lib`; field exists so `--bin` flip is non-breaking
    pub kind: CrateKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateKind {
    Lib,
}

/// Report what `cust new` actually wrote — handy for tests.
#[derive(Debug)]
pub struct NewOutputs {
    pub root: PathBuf,
    pub name: String,
}

pub fn run(plan: &NewPlan<'_>) -> Result<NewOutputs> {
    let name = resolve_name(plan)?;
    if let Err(reason) = validate_package_name(&name) {
        bail!(
            "invalid package name {name:?}: {reason} \
             (override with --name <NAME> if the directory name is not a valid crate name)"
        );
    }

    let root = plan.path.to_path_buf();
    prepare_root(&root)?;

    // We've already verified the root is empty (or freshly created),
    // so any file we touch from here on is ours.
    write_file_new(&root.join("Cust.toml"), &cust_toml(&name))?;
    write_file_new(&root.join(".gitignore"), GITIGNORE)?;

    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).with_context(|| format!("creating `{}`", src_dir.display()))?;
    write_file_new(&src_dir.join("lib.c"), &lib_c(&name))?;

    Ok(NewOutputs { root, name })
}

fn resolve_name(plan: &NewPlan<'_>) -> Result<String> {
    if let Some(n) = plan.name {
        return Ok(n.to_string());
    }
    let derived = plan
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot derive a package name from `{}` — pass --name <NAME>",
                plan.path.display()
            )
        })?;
    Ok(derived.to_string())
}

/// Ensure `root` either does not exist (we'll create it) or exists
/// and is empty.
fn prepare_root(root: &Path) -> Result<()> {
    match fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                bail!(
                    "destination `{}` already exists and is not empty",
                    root.display()
                );
            }
            // exists & empty — keep it
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).with_context(|| format!("creating `{}`", root.display()))?;
            Ok(())
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!("inspecting `{}`", root.display()))),
    }
}

/// Write `contents` to `path`, failing if `path` already exists.
/// Belt-and-braces: the empty-dir check should already have ruled
/// this out, but a TOCTOU-style failure here is safer than silently
/// stomping a file we don't own.
fn write_file_new(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write as _;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating `{}`", path.display()))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

const GITIGNORE: &str = "/target\n";

fn cust_toml(name: &str) -> String {
    format!(
        "[package]\n\
         name    = \"{name}\"\n\
         version = \"0.1.0\"\n"
    )
}

fn lib_c(name: &str) -> String {
    // Use the crate name in the symbol so two crates side-by-side
    // don't pick the same default symbol and break a future link
    // step. Sanitise `-` to `_` since `-` is invalid in C
    // identifiers.
    let sym = name.replace('-', "_");
    format!(
        "#include <stdint.h>\n\
         \n\
         cust_pub int32_t {sym}_add(int32_t a, int32_t b) {{\n    \
             return a + b;\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{cust_toml, lib_c};

    #[test]
    fn cust_toml_contains_name_and_version() {
        let t = cust_toml("hello");
        assert!(t.contains("name    = \"hello\""), "{t}");
        assert!(t.contains("version = \"0.1.0\""), "{t}");
    }

    #[test]
    fn lib_c_sanitises_dashes_in_symbol() {
        let c = lib_c("my-crate");
        assert!(c.contains("my_crate_add"), "{c}");
        assert!(c.contains("cust_pub"), "{c}");
    }
}
