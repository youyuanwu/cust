//! The `cust build` pipeline.
//!
//! Pipeline (per `docs/design/v0.2.md`):
//!
//! 1. Parse `Cust.toml` (already done by `Manifest::load`).
//! 2. Resolve the active profile (default `dev`; `--release` →
//!    `release`).
//! 3. Materialise the prelude to `target/<profile>/prelude.h`.
//! 4. Discover the module graph rooted at `src/lib.c` by walking
//!    `#cust mod` directives.
//! 5. For each module: scan + rewrite via `mod_scanner`, write
//!    the rewritten bytes to
//!    `target/<profile>/build/<crate>/<qname>.preprocessed.c`,
//!    then compile to `<qname>.o`.
//! 6. Archive every `.o` into `target/<profile>/lib<name>.a`.
//! 7. Emit `target/compile_commands.json` (one entry per TU).
//! 8. Stamp `target/.cust-version`.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

use crate::{
    clang::Clang,
    manifest::Manifest,
    mod_scanner,
    modules::{self, Module},
    plugin::Plugin,
    profile::{ProfileKind, ResolvedProfile},
    target_layout::TargetLayout,
};

/// Inputs handed to `run` by the CLI layer.
pub struct BuildPlan<'a> {
    pub manifest: &'a Manifest,
    pub crate_root: &'a Path,
    pub workspace_root: &'a Path,
    pub profile_kind: ProfileKind,
    pub clang: &'a Clang,
    /// Discovered cust clang plugin, when present. v0.2 treats
    /// "plugin missing" as a silent skip — the v0.1 plugin-less
    /// code path still works for single-module / no-cross-import
    /// crates.
    pub plugin: Option<&'a Plugin>,
}

/// Outputs `cust build` writes. `objects` and `compile_commands`
/// are reported back so callers can plumb them into future tooling
/// (e.g. `cust test`); only `archive` is printed today.
#[derive(Debug)]
pub struct BuildOutputs {
    #[allow(dead_code)]
    pub objects: Vec<PathBuf>,
    pub archive: PathBuf,
    #[allow(dead_code)]
    pub compile_commands: PathBuf,
}

/// One `compile_commands.json` entry.
struct CompileEntry {
    directory: PathBuf,
    file: PathBuf,
    arguments: Vec<String>,
}

pub fn run(plan: &BuildPlan<'_>) -> Result<BuildOutputs> {
    // Step 2: resolve profile.
    let profile_override = match plan.profile_kind {
        ProfileKind::Dev => plan.manifest.profile.dev.as_ref(),
        ProfileKind::Release => plan.manifest.profile.release.as_ref(),
    };
    let profile = ResolvedProfile::resolve(plan.profile_kind, profile_override)?;

    let layout = TargetLayout::for_workspace(plan.workspace_root, profile.kind);
    layout.ensure_dirs()?;

    // Step 3: materialise prelude.
    let prelude_path = layout.prelude_path();
    materialise_prelude(&prelude_path)?;

    // Step 4: discover modules.
    let root_source = plan.manifest.lib_source(plan.crate_root);
    if !root_source.is_file() {
        bail!(
            "library source `{}` not found (set `[lib] path` in Cust.toml to override)",
            root_source.display()
        );
    }
    let modules =
        modules::discover(plan.crate_root, &root_source).context("discovering module graph")?;

    let crate_name = &plan.manifest.package.name;
    let crate_build_dir = layout.profile_root.join("build").join(crate_name);
    fs::create_dir_all(&crate_build_dir)
        .with_context(|| format!("creating `{}`", crate_build_dir.display()))?;

    // Step 5: per-TU compile.
    let mut objects: Vec<PathBuf> = Vec::with_capacity(modules.len());
    // Two entries per module: one for the rewritten file (what
    // clang actually compiled) and one paired entry pointing at
    // the user's original source with the same flags but the file
    // path swapped. clangd picks whichever matches the file the
    // editor opened — so editing src/lib.c sees the real flags,
    // not the default fallback set.
    let mut compile_entries: Vec<CompileEntry> = Vec::with_capacity(modules.len() * 2);

    for m in &modules {
        let (rewritten_path, object_path, cflags) =
            compile_one_module(plan, &profile, &prelude_path, &crate_build_dir, m)?;
        objects.push(object_path);

        // Entry 1: the rewritten file (matches what we actually
        // ran). Source argument at the tail is the rewritten path.
        compile_entries.push(CompileEntry {
            directory: plan.crate_root.to_path_buf(),
            file: rewritten_path.clone(),
            arguments: argv_with_clang(plan, &cflags),
        });

        // Entry 2: the original source. Swap the trailing source-
        // file arg for the user's source path so clangd sees the
        // right file when it parses this entry.
        let original_args = swap_source_arg(
            &argv_with_clang(plan, &cflags),
            &rewritten_path,
            &m.source_path,
        );
        compile_entries.push(CompileEntry {
            directory: plan.crate_root.to_path_buf(),
            file: m.source_path.clone(),
            arguments: original_args,
        });
    }

    // Step 6: archive.
    let archive_path = layout.profile_root.join(format!("lib{crate_name}.a"));
    archive_objects(&objects, &archive_path)?;

    // Step 7: compile_commands.json (always at `target/`, never per-
    // profile — pinned by the v0.1 layout block).
    let cc_path = layout.target_root.join("compile_commands.json");
    write_compile_commands(&cc_path, &compile_entries)?;

    // Step 8: stamp .cust-version.
    write_version_stamp(&layout.target_root.join(".cust-version"), plan.clang)?;

    Ok(BuildOutputs {
        objects,
        archive: archive_path,
        compile_commands: cc_path,
    })
}

fn compile_one_module(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    crate_build_dir: &Path,
    m: &Module,
) -> Result<(PathBuf, PathBuf, Vec<String>)> {
    // Read + scan + rewrite. We always rewrite (even when the
    // scanner finds zero directives) so the build pipeline has
    // exactly one code path for "the bytes clang sees".
    let src_text = fs::read_to_string(&m.source_path)
        .with_context(|| format!("reading `{}`", m.source_path.display()))?;
    let scan = mod_scanner::scan(&src_text, &m.source_path)?;
    let rewritten = mod_scanner::rewrite(&src_text, &m.source_path, &scan);

    let rewritten_path = crate_build_dir.join(format!("{}.preprocessed.c", m.qualified_name));
    if let Some(parent) = rewritten_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating `{}`", parent.display()))?;
    }
    fs::write(&rewritten_path, &rewritten)
        .with_context(|| format!("writing `{}`", rewritten_path.display()))?;

    let object_path = crate_build_dir.join(format!("{}.o", m.qualified_name));

    // Honour `#include "x.h"` from the *original* source location —
    // the rewritten file lives in `target/`, so without -I clang
    // resolves relative includes against `target/` rather than the
    // user's source layout.
    let original_dir = m.source_path.parent().unwrap_or(plan.crate_root);
    let cflags = build_cflags(
        plan,
        profile,
        prelude,
        &rewritten_path,
        &object_path,
        Some(original_dir),
    );

    let status = plan
        .clang
        .command()
        .args(&cflags)
        .stdin(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "invoking `{}` for module `{}`",
                plan.clang.path.display(),
                m.qualified_name
            )
        })?;
    if !status.success() {
        bail!(
            "clang exited with status {status} compiling module `{}`",
            m.qualified_name
        );
    }

    Ok((rewritten_path, object_path, cflags))
}

/// Build the clang argv for a single TU. `extra_include` (when
/// `Some`) becomes `-I<dir>` immediately before the prelude
/// `-include` so per-module includes resolve against the original
/// source layout even when we're compiling a rewritten copy from
/// `target/`.
pub fn build_cflags(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    source: &Path,
    object: &Path,
    extra_include: Option<&Path>,
) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();

    // -std=
    let std_flag = plan
        .manifest
        .clang
        .std
        .as_deref()
        .unwrap_or_else(|| plan.clang.default_std());
    flags.push(format!("-std={std_flag}"));

    flags.extend(profile.cflags());
    flags.extend(plan.manifest.clang.extra_cflags.iter().cloned());

    flags.push("-fvisibility=hidden".to_string());
    if let Some(plugin) = plan.plugin {
        flags.push(plugin.fplugin_flag());
    }
    if let Some(dir) = extra_include {
        flags.push(format!("-I{}", dir.display()));
    }
    flags.push("-include".to_string());
    flags.push(prelude.display().to_string());

    flags.push("-c".to_string());
    flags.push("-o".to_string());
    flags.push(object.display().to_string());
    flags.push(source.display().to_string());

    flags
}

fn argv_with_clang(plan: &BuildPlan<'_>, flags: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(flags.len() + 1);
    argv.push(plan.clang.path.display().to_string());
    argv.extend(flags.iter().cloned());
    argv
}

/// Return a copy of `argv` with any occurrence of `old` (as a full
/// argument string) replaced by `new`. Used to derive the
/// editor-facing `compile_commands` entry: same flags as the real
/// compile, but with the source path (last positional argument,
/// per `build_cflags`) swapped from the rewritten file to the
/// user's original source so clangd matches the file it opened.
fn swap_source_arg(argv: &[String], old: &Path, new: &Path) -> Vec<String> {
    let old_s = old.display().to_string();
    let new_s = new.display().to_string();
    argv.iter()
        .map(|a| {
            if a == &old_s {
                new_s.clone()
            } else {
                a.clone()
            }
        })
        .collect()
}

fn materialise_prelude(dst: &Path) -> Result<()> {
    const PRELUDE: &str = include_str!("prelude.h");
    // Write only if missing or stale (content differs) — keeps the
    // mtime stable so clang's own incremental story doesn't churn.
    let needs_write = fs::read_to_string(dst).map_or(true, |existing| existing != PRELUDE);
    if needs_write {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }
        fs::write(dst, PRELUDE).with_context(|| format!("writing `{}`", dst.display()))?;
    }
    Ok(())
}

fn archive_objects(objects: &[PathBuf], archive: &Path) -> Result<()> {
    let ar = pick_ar();
    // `rcs` = create archive, replace, add index. We pass all
    // objects in one invocation so the archive is built atomically.
    // Pre-remove any stale archive so `rcs` doesn't merge with
    // leftover entries from a previous build with more modules.
    let _ = fs::remove_file(archive);

    let mut cmd = Command::new(&ar);
    cmd.arg("rcs").arg(archive);
    for o in objects {
        cmd.arg(o);
    }
    let status = cmd
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("invoking `{}`", ar.to_string_lossy()))?;
    if !status.success() {
        bail!("{} exited with status {status}", ar.to_string_lossy());
    }
    Ok(())
}

fn pick_ar() -> OsString {
    // Prefer llvm-ar if it's on PATH. We probe by trying `--version`
    // — cheap and avoids carrying a `which` dep.
    let llvm_ar_ok = Command::new("llvm-ar")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if llvm_ar_ok {
        OsString::from("llvm-ar")
    } else {
        OsString::from("ar")
    }
}

fn write_compile_commands(path: &Path, entries: &[CompileEntry]) -> Result<()> {
    // Minimal JSON serialiser tailored to compile_commands.json —
    // avoids pulling in serde_json just to emit an array of
    // {directory, file, arguments} objects. Escapes per RFC 8259 §7.
    let mut out = String::from("[\n");
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  {\n");
        push_json_kv(&mut out, "directory", &e.directory.display().to_string());
        out.push_str(",\n");
        push_json_kv(&mut out, "file", &e.file.display().to_string());
        out.push_str(",\n    \"arguments\": [");
        for (j, a) in e.arguments.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            out.push('"');
            out.push_str(&escape_json(a));
            out.push('"');
        }
        out.push_str("]\n  }");
    }
    out.push_str("\n]\n");

    fs::write(path, out).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

fn push_json_kv(buf: &mut String, key: &str, value: &str) {
    buf.push_str("    \"");
    buf.push_str(key);
    buf.push_str("\": \"");
    buf.push_str(&escape_json(value));
    buf.push('"');
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn write_version_stamp(path: &Path, clang: &Clang) -> Result<()> {
    let contents = format!(
        "cust {}\n{}\n",
        env!("CARGO_PKG_VERSION"),
        clang.version_line
    );
    fs::write(path, contents).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::escape_json;

    #[test]
    fn escapes_quotes_backslashes_controls() {
        assert_eq!(escape_json("hi"), "hi");
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("a\\b"), "a\\\\b");
        assert_eq!(escape_json("a\nb"), "a\\nb");
        assert_eq!(escape_json("\u{0007}"), "\\u0007");
    }
}
