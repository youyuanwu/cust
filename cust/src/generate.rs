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
    test_discovery, test_runner,
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

    let rewritten = lower_cust_use(
        ctx.source_path,
        &src_text,
        &scan,
        ctx.frags_dir,
        ctx.deps_root,
        ctx.deps,
        ctx.require_upstream,
    )?;

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

    run_surface_clang(clang, base_cflags, ctx.surface_out, ctx.source_path)
}

/// The `#cust use` → `#include` lowering for the surface pass.
/// `crate::<m>` is included only if `<m>`'s fragment already exists
/// (else blanked, or hard-errored under `require_upstream`);
/// `<dep>` is included when the dep is declared. Shared by
/// [`surface_one_module`] and [`sidecar_one`].
fn lower_cust_use(
    source_path: &Path,
    src_text: &str,
    scan: &mod_scanner::ScanResult,
    frags_dir: &Path,
    deps_root: &Path,
    deps: &[&str],
    require_upstream: bool,
) -> Result<String> {
    let mut missing: Option<String> = None;
    let rewritten = mod_scanner::rewrite_with(src_text, source_path, scan, |d| match &d.kind {
        DirectiveKind::UseCrate { name } => {
            let frag = frags_dir.join(format!("{name}.cust.h"));
            if frag.is_file() {
                Some(format!("#include \"{}\"", frag.display()))
            } else {
                if require_upstream && missing.is_none() {
                    missing = Some(name.clone());
                }
                None
            }
        }
        DirectiveKind::UseDep { name } => {
            if deps.iter().any(|n| n == name) {
                let dep_header = deps_root
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
            source_path.display(),
            frags_dir.join(format!("{name}.cust.h")).display()
        );
    }
    Ok(rewritten)
}

/// Run the surface-pass clang invocation: strip the trailing
/// `-c -o <obj> <src>` (4 args) from `base_cflags`, substitute the
/// `-fsyntax-only` demotions, and compile `compile_target`. The
/// exit status is intentionally ignored — an unresolved
/// cross-module reference is the expected case on a cold fragment
/// dir, and the plugin's `HandleTranslationUnit` runs regardless.
fn run_surface_clang(
    clang: &Clang,
    base_cflags: &[String],
    compile_target: &Path,
    diag_source: &Path,
) -> Result<()> {
    let mut cflags = base_cflags.to_vec();
    let new_len = cflags.len().saturating_sub(4);
    cflags.truncate(new_len);
    cflags.push("-fsyntax-only".to_string());
    cflags.push("-Wno-error".to_string());
    cflags.push("-Wno-implicit-function-declaration".to_string());
    cflags.push(compile_target.display().to_string());

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
                diag_source.display()
            )
        })?;
    Ok(())
}

/// One module's fully-resolved surface-pass inputs, owned so a
/// caller can build a `Vec` once and run the fixed-point loop
/// ([`surface_fixed_point`]) over it without lifetime gymnastics.
/// Both the in-process fixed-point (`build::surface_pass_fixed_point`,
/// for `cust check` / `cust test`) and the `cust internal
/// surface-cycle` leaf (V45D-6) build these and share the same
/// convergence algorithm — no logic fork (V45D-8).
pub struct SurfaceUnit {
    /// Qualified module name (for the non-convergence diagnostic).
    pub qname: String,
    /// Absolute path to the module's source `.c`.
    pub source: PathBuf,
    /// Scratch surface-TU path (`<build>/<qname>.surface.c`).
    pub surface_out: PathBuf,
    /// The fragment header the plugin writes for this module.
    pub fragment_out: PathBuf,
    /// `.h-fragments/<crate>/` (sibling-fragment existence probe).
    pub frags_dir: PathBuf,
    /// `deps/` (cross-crate `#cust use <dep>` lowering root).
    pub deps_root: PathBuf,
    /// Declared dep names this module may `#cust use <dep>;`.
    pub deps: Vec<String>,
    /// The full `build_cflags` argv (ending `-c -o <obj> <src>`);
    /// `surface_one_module` truncates the trailing four and
    /// substitutes the `-fsyntax-only` demotions.
    pub base_cflags: Vec<String>,
}

/// The fixed-point iteration cap (default 3, overridable via
/// `CUST_FIXED_POINT_CAP`). Shared so the in-process fixed-point
/// and the `surface-cycle` leaf read the same env var (V40D-11).
#[must_use]
pub fn fixed_point_cap() -> usize {
    std::env::var("CUST_FIXED_POINT_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

/// Run the surface pass over `units` repeatedly until every
/// module's fragment bytes stop changing, or until `cap`
/// iterations elapse (then the §4 non-convergence error). Shared
/// by the in-process fixed-point (`cust check` / `cust test`) and
/// the `cust internal surface-cycle` leaf (V40D-11 / V45D-6). Each
/// module is surfaced with `require_upstream = false` (the loop is
/// the recovery mechanism for an as-yet-unresolved cross-module
/// reference).
///
/// Empirically: an acyclic set converges in 1 iteration; a 2-cycle
/// of `[[cust::pub_repr]]` types needs 2; longer cycles converge by
/// 3 or diverge (the cap catches divergence and raises the §4
/// error). Plugin-side `writeFragmentIfChanged` skips identical
/// bytes, so a no-change iteration costs one stat + read + memcmp
/// per module.
pub fn surface_fixed_point(units: &[SurfaceUnit], clang: &Clang, cap: usize) -> Result<()> {
    // Pre-borrow each unit's owned deps as `&[&str]` for `SurfaceCtx`.
    let dep_refs: Vec<Vec<&str>> = units
        .iter()
        .map(|u| u.deps.iter().map(String::as_str).collect())
        .collect();

    // Snapshot of "fragment bytes after iteration N", indexed
    // parallel to `units`. `None` before the first iteration.
    let mut prev: Option<Vec<Vec<u8>>> = None;

    for iter in 1..=cap {
        for (u, deps) in units.iter().zip(&dep_refs) {
            let ctx = SurfaceCtx {
                source_path: &u.source,
                surface_out: &u.surface_out,
                fragment_out: &u.fragment_out,
                frags_dir: &u.frags_dir,
                deps_root: &u.deps_root,
                deps,
                require_upstream: false,
            };
            surface_one_module(&ctx, clang, &u.base_cflags)?;
        }

        // Snapshot fragments (a missing file → empty vec; the only
        // path to a literally-missing fragment is a clang crash
        // before `HandleTranslationUnit`, which errors above).
        let curr: Vec<Vec<u8>> = units
            .iter()
            .map(|u| std::fs::read(&u.fragment_out).unwrap_or_default())
            .collect();

        if let Some(prev) = &prev {
            let wobbling: Vec<&str> = units
                .iter()
                .zip(prev)
                .zip(&curr)
                .filter(|((_, p), c)| p != c)
                .map(|((u, _), _)| u.qname.as_str())
                .collect();

            if wobbling.is_empty() {
                return Ok(());
            }
            if iter == cap {
                return Err(non_convergence_error(cap, &wobbling));
            }
        }
        prev = Some(curr);
    }

    // Single-iteration case (cap == 1): ran once, no prior to
    // compare against — done.
    Ok(())
}

/// The §4 verbatim non-convergence diagnostic (V40D-11). Factored
/// out so the in-process fixed-point and the `surface-cycle` leaf
/// raise a byte-identical message (verification item 7).
#[must_use]
pub fn non_convergence_error(cap: usize, wobbling: &[&str]) -> anyhow::Error {
    anyhow::anyhow!(
        "circular `[[cust::pub_repr]]` dependency did not converge\n  \
         in {cap} iterations between modules: {}\n  \
         hint: break the cycle by exporting one side as `[[cust::pub]]`\n        \
         (opaque) instead of `[[cust::pub_repr]]`",
        wobbling.join(", ")
    )
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

// ─── v0.4.6 V46D-1: test-discovery sidecar + runner cores ────────

/// Inputs to [`sidecar_one`] — one TU's test-discovery surface
/// pass, producing a `.cust.tests` sidecar (and **no** fragment).
/// Shared by the in-process `cust test` path
/// (`build::surface_pass_integration`, and \u2014 once v0.4.6 migrates
/// it \u2014 the unit sidecar pass) and the hidden
/// `cust internal test-sidecar` leaf (V46D-1) \u2014 no logic fork
/// (V45D-8).
pub struct SidecarCtx<'a> {
    /// The TU to surface-pass: an original `src/**.c` (unit) or an
    /// already-`#cust use`-lowered `.rewrite/<crate>/tests/<stem>.c`
    /// (integration).
    pub source_path: &'a Path,
    /// `Some` for a unit module \u2014 lower `source_path`'s `#cust use`
    /// directives, write the surface TU here, and compile it.
    /// `None` for an integration TU (already rewritten \u2014 compile
    /// `source_path` directly).
    pub surface_out: Option<&'a Path>,
    /// Where the plugin writes the `.cust.tests` sidecar. Always
    /// created (empty when the plugin emits nothing) so a `CMake`
    /// custom-command `OUTPUT` is always satisfied (V46D-1).
    pub sidecar_out: &'a Path,
    /// `.h-fragments/<crate>/` \u2014 sibling-fragment probe for the unit
    /// `#cust use crate::<m>` lowering (ignored when `surface_out`
    /// is `None`).
    pub frags_dir: &'a Path,
    /// `deps/` \u2014 cross-crate `#cust use <dep>` lowering root (unit
    /// only).
    pub deps_root: &'a Path,
    /// Declared dep names the unit module may `#cust use <dep>;`
    /// (unit only).
    pub deps: &'a [&'a str],
}

/// Surface-compile one TU with `-fsyntax-only` + the plugin so it
/// emits its per-TU test-discovery sidecar. Unlike
/// [`surface_one_module`] this writes **no** fragment header
/// (`base_cflags` must carry `test-sidecar-out` but not
/// `fragment-out`) and runs no fixed-point loop (sidecars don't
/// depend on each other). The sidecar is always created so the
/// caller's declared `OUTPUT` exists even when the plugin is
/// absent or discovers zero tests (V46D-1).
pub fn sidecar_one(ctx: &SidecarCtx<'_>, clang: &Clang, base_cflags: &[String]) -> Result<()> {
    let compile_target: PathBuf = if let Some(surface_out) = ctx.surface_out {
        // Unit: lower `#cust use` against fragments on disk, write
        // the surface TU, compile it. `require_upstream = false` \u2014
        // a missing import only weakens recovery (clang falls back
        // to implicit decls); test discovery still works.
        let src_text = std::fs::read_to_string(ctx.source_path)
            .with_context(|| format!("reading `{}`", ctx.source_path.display()))?;
        let scan = mod_scanner::scan(&src_text, ctx.source_path)?;
        let rewritten = lower_cust_use(
            ctx.source_path,
            &src_text,
            &scan,
            ctx.frags_dir,
            ctx.deps_root,
            ctx.deps,
            false,
        )?;
        if let Some(parent) = surface_out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating `{}`", parent.display()))?;
        }
        std::fs::write(surface_out, &rewritten)
            .with_context(|| format!("writing `{}`", surface_out.display()))?;
        surface_out.to_path_buf()
    } else {
        // Integration: `source_path` is already rewritten.
        ctx.source_path.to_path_buf()
    };

    if let Some(parent) = ctx.sidecar_out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }

    run_surface_clang(clang, base_cflags, &compile_target, ctx.source_path)?;

    // V46D-1: guarantee the sidecar exists even if the plugin was
    // absent or discovered zero tests (the runner reads a missing
    // or empty sidecar as "no tests").
    if !ctx.sidecar_out.exists() {
        std::fs::write(ctx.sidecar_out, b"")
            .with_context(|| format!("writing empty sidecar `{}`", ctx.sidecar_out.display()))?;
    }
    Ok(())
}

/// Render one test-runner TU from a set of `.cust.tests` sidecars
/// and write it to `out_path` (byte-skip if unchanged). Reads each
/// sidecar (a missing one counts as zero tests), parses the
/// discovered entries, and renders the runner via
/// [`test_runner::render_main_c`]. Shared by the unit
/// (`write_test_runner_tu`) and integration
/// (`write_integration_runner_tus`) driver paths and the
/// `cust internal test-runner` leaf (V46D-1).
pub fn write_runner_tu(out_path: &Path, sidecars: &[PathBuf]) -> Result<()> {
    let mut tests: Vec<test_discovery::TestEntry> = Vec::new();
    for sidecar in sidecars {
        match std::fs::read_to_string(sidecar) {
            Ok(contents) => {
                let mut found = test_discovery::parse(&contents, sidecar)?;
                tests.append(&mut found);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("reading test sidecar `{}`", sidecar.display()));
            }
        }
    }
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating `{}`", parent.display()))?;
    }
    let src = test_runner::render_main_c(&tests);
    write_if_byte_different(out_path, src.as_bytes())
}
