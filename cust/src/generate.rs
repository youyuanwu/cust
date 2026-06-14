//! Shared generation cores (v0.4.5 V45D-2).
//!
//! The three leaf generators that produce a single artifact each —
//! a `#cust use`-lowered rewrite, one module's surface fragment,
//! and the concatenated crate header — live here so both the
//! in-process driver callers (`build::run_phase1`,
//! `build::write_rewrite_tree`, `cust check`, the `cust test`
//! path) and the hidden `cust internal …` CLI leaves (V45D-2)
//! call the *same* code. No logic fork (V45D-8).
//!
//! These functions take **explicit paths** rather than a
//! `BuildPlan` / `TargetLayout`, so the CLI leaves — which have
//! no workspace context, only the arguments the `CMakeLists`
//! emitter baked into the command line — can drive them directly.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{bail, Context, Result};

use crate::{
    build::write_if_byte_different,
    clang::Clang,
    mod_scanner::{self, DirectiveKind},
};

/// Inputs to [`rewrite_one`] — the `#cust use`-lowering pass that
/// produces one `.rewrite/<crate>/<rel>.c` (wraps the former
/// `build::write_one_rewrite` body).
pub struct RewriteCtx<'a> {
    /// The crate this source belongs to (for the own-lib carve-out).
    pub crate_name: &'a str,
    /// Absolute path to the source `.c` to lower.
    pub source_path: &'a Path,
    /// Absolute path of the rewritten output file.
    pub out_path: &'a Path,
    /// `target/<profile>/.h-fragments/<crate>/` — `#cust use
    /// crate::<m>` lowers to an `#include` of `<frags_dir>/<m>.cust.h`.
    pub frags_dir: &'a Path,
    /// `target/<profile>/deps/` — `#cust use <dep>` lowers to an
    /// `#include` of `<deps_root>/<dep>/include/<dep>.h`.
    pub deps_root: &'a Path,
    /// The member's own published header — `#cust use <crate>` from
    /// the bin half of a lib+bin crate lowers to this (carve-out).
    pub own_lib_header: &'a Path,
    /// Dep crate names this source may `#cust use <dep>;` (validated).
    pub deps: &'a [&'a str],
    /// `true` when lowering the bin half of a lib+bin crate (enables
    /// the own-crate carve-out).
    pub is_bin_half: bool,
    /// Whether the member has a lib half (gates the carve-out).
    pub has_lib: bool,
}

/// Lower every `#cust use` directive in `ctx.source_path` to an
/// `#include` and write the result to `ctx.out_path` (byte-skip
/// if unchanged). Validates that each `#cust use <dep>;` resolves
/// to a declared dependency (or the own-crate carve-out).
pub fn rewrite_one(ctx: &RewriteCtx<'_>) -> Result<()> {
    let src_text = std::fs::read_to_string(ctx.source_path)
        .with_context(|| format!("reading `{}`", ctx.source_path.display()))?;
    let scan = mod_scanner::scan(&src_text, ctx.source_path)?;

    let rewritten =
        mod_scanner::rewrite_with(&src_text, ctx.source_path, &scan, |d| match &d.kind {
            DirectiveKind::UseCrate { name } => {
                let frag = ctx.frags_dir.join(format!("{name}.cust.h"));
                Some(format!("#include \"{}\"", frag.display()))
            }
            DirectiveKind::UseDep { name } => {
                if ctx.is_bin_half && ctx.has_lib && name == ctx.crate_name {
                    return Some(format!("#include \"{}\"", ctx.own_lib_header.display()));
                }
                let dep_header = ctx
                    .deps_root
                    .join(name)
                    .join("include")
                    .join(format!("{name}.h"));
                Some(format!("#include \"{}\"", dep_header.display()))
            }
            DirectiveKind::Mod { .. } => None,
        });

    // Validate `#cust use <name>;` resolves to a declared dep or
    // the own-crate carve-out (bin half of lib+bin).
    for d in &scan.directives {
        if let DirectiveKind::UseDep { name } = &d.kind {
            if ctx.is_bin_half && ctx.has_lib && name == ctx.crate_name {
                continue;
            }
            if !ctx.deps.iter().any(|n| n == name) {
                bail!(
                    "{}:{}:{}: `#cust use {name};` refers to a crate not \
                     listed in [dependencies]; add `{name} = {{ path = \"…\" }}`",
                    ctx.source_path.display(),
                    d.span.line,
                    d.span.column
                );
            }
        }
    }

    if let Some(parent) = ctx.out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }
    write_if_byte_different(ctx.out_path, rewritten.as_bytes())
}

/// Inputs to [`surface_one_module`] — one module's surface pass
/// (wraps the former per-module body of `build::surface_pass`).
pub struct SurfaceCtx<'a> {
    /// Absolute path to the module's source `.c`.
    pub source_path: &'a Path,
    /// Where to write the intermediate `#cust use`-lowered surface
    /// translation unit (`<crate_build_dir>/<qname>.surface.c`).
    pub surface_out: &'a Path,
    /// Where the plugin writes this module's fragment header. The
    /// directory is created if needed; the bytes are the plugin's
    /// job (it byte-skips identical writes).
    pub fragment_out: &'a Path,
    /// `target/<profile>/.h-fragments/<crate>/` — used to test
    /// whether an imported sibling's fragment exists yet.
    pub frags_dir: &'a Path,
    /// `target/<profile>/deps/` — cross-crate `#cust use <dep>`
    /// lowers through here.
    pub deps_root: &'a Path,
    /// Dep crate names this module may `#cust use <dep>;`.
    pub deps: &'a [&'a str],
    /// V45D-4: when `true` (the one-shot CMake-DAG leaf), a
    /// `#cust use crate::<m>;` whose fragment is absent is a hard
    /// error (a missing `DEPENDS` edge ⇒ emitter bug) rather than a
    /// silently-blanked include. When `false` (the fixed-point
    /// callers — `surface_pass_fixed_point`, `cust check`), the
    /// missing include is blanked because the loop is the recovery
    /// mechanism.
    pub require_upstream: bool,
}

/// Surface-compile one module: lower its `#cust use` directives
/// against fragments already on disk, write the surface TU, then
/// run `clang -fsyntax-only` + plugin (via `base_cflags`) so the
/// plugin emits the fragment header. `base_cflags` is the full
/// `build_cflags` argv (ending in `-c -o <obj> <src>`); the trailing
/// four args are replaced with the `-fsyntax-only` demotions, exactly
/// as the in-process surface pass does.
///
/// The clang exit status is intentionally ignored: an unresolved
/// cross-module reference is the expected case on a cold fragment
/// dir, and the plugin's `HandleTranslationUnit` runs regardless.
pub fn surface_one_module(
    ctx: &SurfaceCtx<'_>,
    clang: &Clang,
    base_cflags: &[String],
) -> Result<()> {
    let src_text = std::fs::read_to_string(ctx.source_path)
        .with_context(|| format!("reading `{}`", ctx.source_path.display()))?;
    let scan = mod_scanner::scan(&src_text, ctx.source_path)?;

    let rewritten = lower_surface(ctx, &src_text, &scan)?;

    if let Some(parent) = ctx.surface_out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }
    std::fs::write(ctx.surface_out, &rewritten)
        .with_context(|| format!("writing `{}`", ctx.surface_out.display()))?;

    if let Some(parent) = ctx.fragment_out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }

    // Strip trailing `-c -o <obj> <src>` (4 args), replace with
    // `-fsyntax-only -Wno-error -Wno-implicit-function-declaration <src>`.
    let mut cflags = base_cflags.to_vec();
    let new_len = cflags.len().saturating_sub(4);
    cflags.truncate(new_len);
    cflags.push("-fsyntax-only".to_string());
    cflags.push("-Wno-error".to_string());
    cflags.push("-Wno-implicit-function-declaration".to_string());
    cflags.push(ctx.surface_out.display().to_string());

    let _ = clang
        .command()
        .args(&cflags)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| {
            format!(
                "invoking `{}` for surface pass on `{}`",
                clang.path.display(),
                ctx.source_path.display()
            )
        })?;
    Ok(())
}

/// The `#cust use` → `#include` lowering for the surface pass.
/// `crate::<m>` is included only if `<m>`'s fragment already exists
/// (else blanked, or hard-errored under `require_upstream`);
/// `<dep>` is included when the dep is declared. See `SurfaceCtx`.
fn lower_surface(
    ctx: &SurfaceCtx<'_>,
    src_text: &str,
    scan: &mod_scanner::ScanResult,
) -> Result<String> {
    let mut missing: Option<String> = None;
    let rewritten = mod_scanner::rewrite_with(src_text, ctx.source_path, scan, |d| match &d.kind {
        DirectiveKind::UseCrate { name } => {
            let frag = ctx.frags_dir.join(format!("{name}.cust.h"));
            if frag.is_file() {
                Some(format!("#include \"{}\"", frag.display()))
            } else {
                if ctx.require_upstream && missing.is_none() {
                    missing = Some(name.clone());
                }
                None
            }
        }
        DirectiveKind::UseDep { name } => {
            if ctx.deps.iter().any(|n| n == name) {
                let dep_header = ctx
                    .deps_root
                    .join(name)
                    .join("include")
                    .join(format!("{name}.h"));
                Some(format!("#include \"{}\"", dep_header.display()))
            } else {
                None
            }
        }
        DirectiveKind::Mod { .. } => None,
    });

    if let Some(name) = missing {
        bail!(
            "{}: `#cust use crate::{name};` imports a fragment that does \
             not exist on disk (`{}`); the build graph is missing a \
             `DEPENDS` edge for it (internal: surface-module run before \
             its upstream)",
            ctx.source_path.display(),
            ctx.frags_dir.join(format!("{name}.cust.h")).display()
        );
    }
    Ok(rewritten)
}

/// Concatenate the per-module fragment headers `frags` (each an
/// `(qualified_name, fragment_path)` pair, **already in topological
/// order**) into the single published crate header at `out_path`
/// (wraps the former `build::write_crate_header` body). Byte-skips
/// an unchanged write. Missing fragments are skipped silently (a
/// module with zero `[[cust::pub]]` decls produces none).
pub fn write_crate_header_concat(
    crate_name: &str,
    out_path: &Path,
    frags: &[(String, PathBuf)],
) -> Result<()> {
    use std::fmt::Write as _;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }

    let guard = crate::build::header_guard(crate_name);
    let mut out = String::new();
    out.push_str("/* @generated by cust — DO NOT EDIT */\n");
    let _ = writeln!(out, "/* Public surface of crate `{crate_name}`. */\n");
    let _ = writeln!(out, "#ifndef {guard}\n#define {guard}\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");

    for (qname, frag) in frags {
        let Ok(body) = std::fs::read_to_string(frag) else {
            continue; // module had no [[cust::pub]] decls; plugin emitted nothing
        };
        let _ = writeln!(out, "/* --- module `{qname}` --- */");
        out.push_str(crate::build::strip_fragment_header_comment(&body));
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("#ifdef __cplusplus\n} /* extern \"C\" */\n#endif\n\n");
    let _ = writeln!(out, "#endif /* {guard} */");

    write_if_byte_different(out_path, out.as_bytes())
}
