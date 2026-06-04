//! The v0.1 `cust build` pipeline.
//!
//! 7 steps, in order (see `docs/design/cust-design.md` §17 "What
//! `cust build` actually does in v0.1"):
//!
//! 1. Parse `Cust.toml` (already done by `Manifest::load`).
//! 2. Resolve the active profile (default `dev`; `--release` →
//!    `release`).
//! 3. Materialise the prelude to `target/<profile>/prelude.h`.
//! 4. `clang <profile-flags> -c -fvisibility=hidden -include
//!    prelude.h -o build/<crate>/lib.o src/lib.c`.
//! 5. `llvm-ar` (or `ar`) rcs `target/<profile>/lib<name>.a lib.o`.
//! 6. Emit `target/compile_commands.json`.
//! 7. Stamp `target/.cust-version`.

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
}

/// Outputs `cust build` writes — handy for tests and for `cust check`
/// to share materialisation helpers. `object` and `compile_commands`
/// are reported back so callers can plumb them into future tooling
/// (e.g. `cust test`); only `archive` is printed today.
#[derive(Debug)]
pub struct BuildOutputs {
    #[allow(dead_code)]
    pub object: PathBuf,
    pub archive: PathBuf,
    #[allow(dead_code)]
    pub compile_commands: PathBuf,
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

    // Step 4: compile.
    let source = plan.manifest.lib_source(plan.crate_root);
    if !source.is_file() {
        bail!(
            "library source `{}` not found (set `[lib] path` in Cust.toml to override)",
            source.display()
        );
    }
    let crate_name = &plan.manifest.package.name;
    let crate_build_dir = layout.profile_root.join("build").join(crate_name);
    fs::create_dir_all(&crate_build_dir)
        .with_context(|| format!("creating `{}`", crate_build_dir.display()))?;
    let object_path = crate_build_dir.join("lib.o");

    let cflags = build_cflags(plan, &profile, &prelude_path, &source, &object_path);

    let status = plan
        .clang
        .command()
        .args(&cflags)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("invoking `{}`", plan.clang.path.display()))?;
    if !status.success() {
        bail!("clang exited with status {status}");
    }

    // Step 5: archive.
    let archive_path = layout.profile_root.join(format!("lib{crate_name}.a"));
    archive_object(&object_path, &archive_path)?;

    // Step 6: compile_commands.json (always at `target/`, never per-
    // profile — §17 layout block).
    let cc_path = layout.target_root.join("compile_commands.json");
    write_compile_commands(&cc_path, plan, &cflags, &source)?;

    // Step 7: stamp .cust-version.
    write_version_stamp(&layout.target_root.join(".cust-version"), plan.clang)?;

    Ok(BuildOutputs {
        object: object_path,
        archive: archive_path,
        compile_commands: cc_path,
    })
}

/// Used by `cust check` — drops the `-c -o` pair and adds
/// `-fsyntax-only`. Returns the cflags so callers can invoke clang
/// themselves.
pub fn build_cflags(
    plan: &BuildPlan<'_>,
    profile: &ResolvedProfile,
    prelude: &Path,
    source: &Path,
    object: &Path,
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
    flags.push("-include".to_string());
    flags.push(prelude.display().to_string());

    flags.push("-c".to_string());
    flags.push("-o".to_string());
    flags.push(object.display().to_string());
    flags.push(source.display().to_string());

    flags
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

fn archive_object(object: &Path, archive: &Path) -> Result<()> {
    let ar = pick_ar();
    // `rcs` = create archive, replace, add index.
    let status = Command::new(&ar)
        .arg("rcs")
        .arg(archive)
        .arg(object)
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

fn write_compile_commands(
    path: &Path,
    plan: &BuildPlan<'_>,
    flags: &[String],
    source: &Path,
) -> Result<()> {
    // Minimal JSON serialiser tailored to compile_commands.json — we
    // only need a one-element array of {directory, file, arguments}.
    // Avoids pulling in serde_json just for this. Escapes per RFC
    // 8259 §7.
    let mut out = String::from("[\n  {\n");
    push_json_kv(
        &mut out,
        "directory",
        &plan.crate_root.display().to_string(),
    );
    out.push_str(",\n");
    push_json_kv(&mut out, "file", &source.display().to_string());
    out.push_str(",\n    \"arguments\": [");

    let mut argv: Vec<String> = Vec::with_capacity(flags.len() + 1);
    argv.push(plan.clang.path.display().to_string());
    argv.extend(flags.iter().cloned());

    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        out.push_str(&escape_json(a));
        out.push('"');
    }
    out.push_str("]\n  }\n]\n");

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
