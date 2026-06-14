# cust — CMake-owned incremental `cust check` (direct-clang check pass + per-module check stamps)

**Status:** 📝 **draft** (opened 2026-06-14).
**Parent doc:** [cust-design.md §17 roadmap](cust-design.md).
**Belongs to milestone:** *unscheduled candidate.* The v0.4.7
slot is taken by the dependency resolver + registry; this work
naturally slots at v0.4.8 or later, after the resolver lands.
This file is the focused design for **wiring `cust check` into the
CMake graph** so it gains true incrementality via a per-module
check stamp.
**Builds directly on:** [v0.4.5.md](v0.4.5.md) (CMake-owned
fragment + rewrite generation) and
[v0.4.6-test-codegen.md](v0.4.6-test-codegen.md) (the test path's
custom-command migration). This milestone applies the **same**
`add_custom_command` + `EXCLUDE_FROM_ALL`-anchor shape to the one
remaining driver-side pre-pass: `cust check`.
**Reverses:** [V42D-15](v0.4.2.md) ("`cust check` stays out of
CMake entirely"), the same way v0.4.5 reversed V42D-17
(driver-owned generation) for the build path. The reversal is
deliberate and the original rejection rationale is re-examined in
§3.
**Minimum clang / CMake / Ninja:** unchanged from v0.4.5.

This is the incremental-check design record — scope, design
decisions (**CHK-D-N**), open questions (**RQ-CHK-N**), and the
verification target.

---

## 0. Prerequisite (shipped) — plugin now mandatory for `cust check`

Before this milestone two plugin-less paths were **removed**: the
explicit `--no-plugin` opt-out flag (V40D-10) **and** the implicit
"plugin missing → warn and run syntax-only" fallback on `cust
check`. Rationale: once `cust check` becomes a real plugin-backed
pass (CHK-D-1), any plugin-less variant skips exactly the
cust-specific phase-1 AST checks, so it could *pass code the real
check rejects* — a false green strictly worse than no check.
After this change the plugin is **mandatory for every subcommand**
that resolves it (`build` / `check` / `test` / `run`): a missing
plugin is the V40D-12 hard error, uniformly. This collapsed
`resolve_plugin` to a single always-required path (non-optional
return, no `subcommand` branching, no warn path), removed the
global `Cli` flag and the `--no-plugin` tests, and converted the
`cust check` warn-and-proceed test into a hard-error test. V40D-10
is marked superseded in [v0.4.0.md](v0.4.0.md) and the contract in
[cust-design.md §10](cust-design.md) updated.

---

## 1. Headline outcome

Today [`run_check_path`](../../cust/src/workspace.rs#L746) runs
[`build::run_phase1`](../../cust/src/build.rs#L141) per member on
**every** invocation: the full surface fixed-point
([`surface_fixed_point`](../../cust/src/generate.rs#L353)) plus a
crate-header concat, with **no** mtime check, stamp, or skip
guard. Every `cust check` re-parses the whole crate from scratch —
`clang -fsyntax-only -fplugin=…` for every module, every run.

Worse, the pass it runs is **tolerant by construction**:
[`run_surface_clang`](../../cust/src/build.rs#L300) ignores clang's
exit status (`let _ = …status()`) and nulls stderr, and
[`lower_cust_use`](../../cust/src/generate.rs#L208) blanks an
undeclared `#cust use` rather than erroring (check runs
`require_upstream = false`). `surface_fixed_point` only ever
returns `Err` for `[[cust::pub_repr]]` non-convergence. So
`cust check` today reports **only** gross structural problems
(missing lib source, malformed module graph, divergent cycle) and
**silently swallows** type errors, missing symbols, and plugin
phase-1 diagnostics.

After this milestone:

1. **`cust check` reports real diagnostics** (CHK-D-1, the
   load-bearing precondition): an error-reporting `-fsyntax-only`
   + plugin compile per module — stderr inherited, exit status
   honoured, no tolerance flags. A type error fails `cust check`.
2. **`cust check` is incremental** (CHK-D-3): each module's check
   compile is a CMake custom command whose `OUTPUT` is a
   `.checked` stamp and whose `DEPENDS` are the lowered TU + plugin
   + upstream fragments. Ninja's `restat` owns the skip.
3. **No new `cust internal` leaf** (CHK-D-2): the check compile is
   a **direct `${CMAKE_C_COMPILER}` invocation**, not a cust
   subprocess. The only cust-specific step — `#cust use` lowering
   — is *already* emitted as the v0.4.5 `rewrite-file` command,
   whose `.rewrite/<crate>/src/*.c` output check reuses verbatim.

```console
$ cust check            # cold
    Checking cstd v0.4.8
    Finished check

$ cust check            # nothing changed
    Finished check       # 0 clang spawns, 0 plugin spawns

$ # edit one module's body
$ cust check
    Checking cstd v0.4.8
    Finished check       # only the edited module re-checked
```

(Console text is illustrative; the exact `Checking` / `Finished`
lines reuse the existing check reporting.)

---

## 2. What this milestone ships

In-scope:

* **`cust check` becomes a meaningful, error-reporting pass**
  (CHK-D-1). A non-tolerant `-fsyntax-only` + plugin compile per
  module: stderr inherited, exit status honoured, diagnostics
  surfaced. This is a deliberate **behaviour change** — check now
  fails on errors it previously swallowed.
* **No new hidden `cust internal` leaf** (CHK-D-2). The check
  compile is a **direct `${CMAKE_C_COMPILER}` invocation** baked
  into the `CMakeLists`, not a cust subprocess. The only
  cust-specific step — `#cust use` → `#include` lowering — is
  *already* the v0.4.5 `rewrite-file` command; check reuses its
  `.rewrite/<crate>/src/*.c` output (the very TU the lib target
  compiles), so it adds **zero** new generation logic.
* **Per-module check-stamp custom commands** (CHK-D-3): one
  direct-clang `add_custom_command` per lib module, `OUTPUT` a
  `.checked` stamp, `DEPENDS` = the module's `.rewrite` TU +
  plugin + the module's upstream **build-mode** fragments (the
  v0.4.5 `surface-module` / `surface-cycle` outputs). A second
  `touch` `COMMAND` stamps success.
* **A `cust_check` aggregate target** (CHK-D-4): per-member
  `cust_check_<member>` targets plus an umbrella `cust_check`, all
  `EXCLUDE_FROM_ALL` so they never participate in `cust build`.
  `cust check` = emit + configure + `cmake --build --target
  cust_check` (or `cust_check_<m>` for `-p <m>`, CHK-D-10).
* **Check reuses the build-mode fragment + crate-header DAG**
  (CHK-D-5): cross-module `#cust use crate::X` resolves through
  the build-mode `X.cust.h`; cross-crate `#cust use <dep>`
  resolves through the dep's `crate-header` `OUTPUT`. Check emits
  *check passes only*, never its own fragments.
* **No-op `cust check` = zero codegen spawns** (CHK-D-7): the
  V45D-12 / V46D-8 property extended to the check path, with its
  own regression test.
* **cwork check dogfood** (CHK-D-9): `cust check` over cwork is
  incremental and reports a deliberately-injected type error.

Out of scope (deferred / unchanged):

* **Strict bin-half check** (RQ-CHK-3). Check stays a lib-surface
  pass (V44D-9); wiring a check command over the bin half is a
  separate, additive step.
* **A driver-side fast path for trivial crates** (RQ-CHK-2). One
  code path (always CMake) for simplicity; revisit only if
  configure cost is shown to dominate on small crates.
* **The `--no-plugin` flag** — **removed** as a prerequisite to
  this milestone (§0), so it is not a design variable here. The
  implicit missing-plugin fallback was also removed: `cust check`
  now hard-errors without a plugin (V40D-12), like build/test/run.
* **Test-target checking** (`cust check` over `tests/`/unit-test
  bodies) — out of scope; `cust test` already compiles those.

---

## 3. Why now / boundary — re-examining V42D-15

[V42D-15](v0.4.2.md) locked `cust check` *out* of CMake on three
grounds. Each is re-examined against this design:

1. **"The work check does == the surface pass, which already
   shells to clang."** Still true — and that is *why* the wiring
   is mechanical, not why it should stay driver-side. The lib
   sources are *already* lowered to fragment-resolved
   `.rewrite/<crate>/src/*.c` TUs by the v0.4.5 `rewrite-file`
   commands (it is exactly what the lib target compiles); a check
   command is just a second consumer of those TUs that runs clang
   with `-fsyntax-only` instead of `-c`. Nothing new is generated.
2. **"No second tree, no second stamp."** Honoured. This design
   uses the **same** emitted `CMakeLists.txt` and the **same**
   V42D-8 configure-skip stamp as `cust build`. `cust_check` is
   just another target in that one file — there is no second
   build tree and no second configure stamp.
3. **"Avoid configure cost + stamp flap."** The flap objection
   was specifically against option C (a `-DCUST_CHECK_ONLY=ON`
   **cache variable**), which would churn the configure stamp
   every time the user alternated `cust check` and `cust build`.
   This design uses a **target**, not a cache variable: alternating
   `cust check` / `cust build` against the same crate reuses one
   configured tree and one stamp — **no flap**. The residual
   configure cost is paid once and amortised across repeat runs
   (RQ-CHK-2 covers the trivial-crate corner).

**Why now:** the build path (v0.4.5) and test path (v0.4.6) are
both proven against cwork. `cust check` is the *last* unconditional
driver-side pre-pass, and it is both the slowest-scaling
(full re-parse every run) and the least useful (silently tolerant).
Wiring it in closes the "all generation is a CMake custom command"
story and, more importantly, makes `cust check` actually catch
errors.

**Not a new build system / not a layout change.** Same CMake +
Ninja backend, same emitted file. The change moves "check" from
*before CMake, every run, tolerant* to *a CMake target, incremental,
error-reporting*.

---

## 4. Design decisions (CHK-D-N)

### CHK-D-1 — `cust check` becomes error-reporting (the precondition)

**Proposed — load-bearing.** A stamp for a vacuous status is
pointless, so the first and most important change is making
`cust check` *mean* something. The check compile is a non-tolerant
variant of the surface pass:

* `-fsyntax-only -fplugin=<plugin.so>` (no codegen), **without**
  `-Wno-error` / `-Wno-implicit-function-declaration` (the
  tolerances [`run_surface_clang`](../../cust/src/build.rs#L300)
  adds for the build-mode surface DAG).
* **stderr inherited** (not `Stdio::null()`), so clang +
  plugin diagnostics reach the user.
* **exit status honoured** — a non-zero clang exit fails the
  command → fails the Ninja build → `cust check` exits non-zero.

The TU it compiles is the **lowered** form (`#cust use` →
`#include`) — the `.rewrite/<crate>/src/<m>.c` the v0.4.5
`rewrite-file` command already produces and the lib target already
compiles ([`rewrite_one`](../../cust/src/generate.rs#L69) lowers
`#cust use crate::X` to an `#include` of `X`'s fragment header).
Check consumes that TU **verbatim**, so it validates *exactly* the
bytes the build compiles — cross-module references resolve to real
declarations, and diagnostics carry original-source positions
because the rewrite emits `#line N "<orig>"` re-anchors
([`mod_scanner::rewrite_with`](../../cust/src/mod_scanner.rs#L658)),
so clang points at `src/<m>.c`, not the `.rewrite` path. No
check-specific lowering is performed (CHK-D-2, RQ-CHK-4).

**Behaviour change, called out.** Check previously passed crates
with type errors; it now fails them. This is the headline value,
but it must land with: a verification test (CHK-D-9), a
cust-design.md §10 note (check is no longer "tolerant"), and a
v0.4.4 V44D-9 cross-reference (the "tolerant lib-surface pass"
characterisation is superseded for the lib half).

### CHK-D-2 — No new leaf: direct `${CMAKE_C_COMPILER}` check compile

**Proposed.** The check compile is **not** a `cust internal` leaf.
cust has exactly one piece of custom logic the check compile would
otherwise need — `#cust use` → `#include` lowering — and the v0.4.5
`rewrite-file` command *already* performs it, emitting the
fragment-resolved `.rewrite/<crate>/src/<m>.c` the lib target
compiles. Everything else in a check compile is plain clang flags
the emitter already knows how to bake:

| concern | who handles it |
| --- | --- |
| `#cust use` lowering | the existing `rewrite-file` command (reused, not re-run) |
| `-std=` / profile cflags / `-fvisibility` / `-include <prelude>` / user `-D` / dep `-I` | a **standalone clang argv** the emitter bakes into the command (see the hazard below) |
| `-fplugin=<plugin.so>` + phase-1 AST checks | **clang** loads the plugin; cust is not in the loop |
| no fragment output | omit the `-fplugin-arg-cust-fragment-out=…` flag (plugin writes nothing — CHK-D-5) |
| `-fsyntax-only`, honoured exit, inherited stderr | clang's defaults — *no* wrapper, which is the whole point of CHK-D-1 |
| touch the stamp on success | a second `${CMAKE_COMMAND} -E touch` `COMMAND` |

So the check command invokes clang the **same way the real
toolchain compile does** (same compiler + same `-fplugin`, minus
codegen), with a `touch` chained after it. There is no
`check-module` subcommand, no new `Cmd::Internal` variant, and no
second lowering implementation to keep in sync. This is the one
generation/validation step in the post-v0.4.6 pipeline that is
*not* a `cust internal …` leaf — justified because, uniquely, it
adds no logic clang doesn't already provide (RQ-CHK-6).

**Hazard — a custom command inherits none of the target flag
machinery.** The lib *target*'s flags are applied by CMake as
target properties: `-std` comes from
[`set(CMAKE_C_STANDARD 23)`](../../cust/src/cmake_emit.rs#L636)
(applied only to `add_library` / `add_executable` targets), and
the rest from
[`target_compile_options(<t> PRIVATE …)`](../../cust/src/cmake_emit.rs#L2081)
with `SHELL:-fplugin=…` wrappers. A direct `${CMAKE_C_COMPILER}`
call inside `add_custom_command` inherits **none** of these — not
`CMAKE_C_STANDARD`, not `target_compile_options`, not the `SHELL:`
escape. A naive "reuse the target's `compile_options` +
`-fsyntax-only`" would therefore compile under clang's **default**
C standard instead of C23, which both masks real diagnostics and
invents fake ones — directly defeating CHK-D-1.

The correct mirror is the lib target's own
[`compile_options`](../../cust/src/cmake_emit.rs#L2081) (built by
`build_member_compile_options`, **already in `cmake_emit`** — the
emitter does *not* need to port `build::build_cflags_raw`). The
check argv is that exact list, with three deltas: (a) **prepend an
explicit `-std=<std>`** — the one flag a custom command does *not*
inherit; (b) **strip the `SHELL:` wrapper** off `-fplugin=<abs>`
(`add_custom_command` does its own argv splitting, so the escape
is unwanted); (c) **append `-fsyntax-only` + the `.rewrite`
source** in place of the target's implicit `-c -o <obj>`. Every
other token — profile cflags, `-fvisibility=hidden`, `-include
<prelude>`, **and `-Wno-unknown-attributes`** — carries through
*verbatim*, because the goal (CHK-D-1) is to validate the bytes
the build compiles under the flags the build uses. Reusing
`compile_options` rather than re-deriving an argv makes the two
**incapable of drifting**: there is no second flag-assembly path.

**Plugin is required for the check command, not optional.** The
§0 removal made the plugin mandatory at the *driver* layer, but
the *emitter* still types `plugin_path` as `Option` (test views
lack a real `.so`). The check command bakes `-fplugin` exactly
like the lib compile, so the `CheckCommand` builder must treat a
`None` plugin as "emit no check command for this module" — never
a pluginless `-fsyntax-only`, which would be the very false green
§0 deleted, smuggled back into the check path. CHK-D-10 states the
rule and the slice-A test that pins it.

The baked clang argv carries the **full** `build_cflags` set for
the V45D-15 reason (a check that omits a user `-D` could see a
different set of declarations) and is emitted with stable arg
ordering so the `CMakeLists` bytes — and the V42D-8 configure-skip
stamp — stay reproducible. The trace hook (`CUST_TRACE_INTERNAL`)
does not apply (no cust subprocess fires); the no-op property
(CHK-D-7) is instead observed via Ninja's "no work to do" / a
clang-spawn probe (slice D picks the exact method).

### CHK-D-3 — Per-module check-stamp custom commands

**Proposed.** For each lib module of a member with a lib half:

```cmake
add_custom_command(
    OUTPUT  "<chk>/<crate>/<qname>.checked"
    COMMAND "<clang>" -std=<std> <profile+extra cflags>
            -fvisibility=hidden -include "<prelude.h>"
            -fplugin="<plugin.so>" -Wno-unknown-attributes
            <user -D / dep -I>           # NO fragment-out; NO -Wno-error
            -fsyntax-only "<rw>/<crate>/src/<qname>.c"   # the rewrite-file OUTPUT
    COMMAND ${CMAKE_COMMAND} -E touch "<chk>/<crate>/<qname>.checked"
    DEPENDS "<rw>/<crate>/src/<qname>.c" "<plugin.so>"
            "<frag>/<crate>/<imp>.cust.h"…   # imported modules' BUILD-mode fragments
    VERBATIM)
```

The clang argv is the **standalone** form
([`build_cflags_raw`](../../cust/src/build.rs#L405) shape, CHK-D-2
hazard) baked in full — `<clang>` is the compiler binary (either
the `plugin::discover`-adjacent `Clang::discover()` path the driver
already resolves, or the `${CMAKE_C_COMPILER}` variable — both name
the same binary; impl picks one), `-std=<std>` is explicit (a
custom command does **not** inherit `CMAKE_C_STANDARD`), `-fplugin`
carries no `SHELL:` prefix (`add_custom_command` does its own argv
splitting), and there is **no** `-fplugin-arg-cust-fragment-out`
(CHK-D-5) and **no** `-Wno-error` /
`-Wno-implicit-function-declaration` (CHK-D-1).

**`-Wno-unknown-attributes` *is* present — it mirrors the build.**
[`build_member_compile_options`](../../cust/src/cmake_emit.rs#L2081)
emits `-Wno-unknown-attributes` **unconditionally** (even with the
plugin loaded — a V42D-5 defensive default), so the real lib
target compile carries it. Since the check argv is that compile's
`compile_options` verbatim (CHK-D-2), it carries it too. Keeping
it is the *correct* choice: dropping it would make check stricter
than the build (surfacing unknown-attribute warnings the build
suppresses), forking the "check passes ⇔ build passes" contract.
This is the lib *target*'s flag builder, distinct from the
driver-side [`build::build_cflags_raw`](../../cust/src/build.rs#L437)
(which gates the same flag behind its plugin-less `else` branch) —
the check mirrors the target, not the driver path.

Two `COMMAND`s, ordered: the clang check, then the stamp `touch`.
Ninja runs the second only if the first **succeeds** — a non-zero
clang exit aborts the command, the `OUTPUT` stays unproduced, and
the check re-fires next run (exactly how a failed compile never
produces its `.o`; RQ-CHK-5). No driver logic is needed to gate
the stamp on success.

The `DEPENDS` are: the module's `.rewrite` TU (a `rewrite-file`
`OUTPUT`), the plugin, and the module's **imported build-mode
fragments** (the v0.4.5 `surface-module` / `surface-cycle`
outputs). Note the `.rewrite` TU `#include`s those fragments by
absolute path, so the fragment `DEPENDS` is what tells Ninja to
materialise the surface DAG first and to re-check when an upstream
fragment changes — the same edge the lib object compile carries via
`OBJECT_DEPENDS` (V42D-6). The check path never produces its own
fragments (CHK-D-5).

The stamp lives under a new `target/<profile>/.check/<crate>/`
tree (a `check_stamp_path(crate, qname)` helper on
[`TargetLayout`](../../cust/src/target_layout.rs#L51), alongside
`fragments_dir` / `test_discovery_dir`). It is *only* a Ninja
restat token — its bytes are irrelevant (touch is enough); its
mtime + the `DEPENDS` graph carry all the incrementality.

For a `[[cust::pub_repr]]` cycle (an SCC of size > 1), each member
module still gets its own check command; they all `DEPENDS` the
cycle's `surface-cycle` fragment outputs (which the fixed-point
loop produces together). No coarse check command is needed — a
check compile is per-TU and order-independent once the fragments
exist.

### CHK-D-4 — `cust_check` aggregate target(s) + `EXCLUDE_FROM_ALL`

**Proposed.** A check stamp is an `OUTPUT` no library/binary target
lists as a source, so — unlike a runner TU (V46D-4) — it needs an
explicit anchor. To preserve `-p` scoping (CHK-D-10), emit one
target per member plus an umbrella:

```cmake
add_custom_target(cust_check_<c1>
    DEPENDS "<chk>/<c1>/<q1>.checked" "<chk>/<c1>/<q2>.checked" …)
add_custom_target(cust_check_<c2>
    DEPENDS "<chk>/<c2>/…"…)
add_custom_target(cust_check DEPENDS cust_check_<c1> cust_check_<c2> …)
set_target_properties(cust_check cust_check_<c1> cust_check_<c2> …
    PROPERTIES EXCLUDE_FROM_ALL TRUE)
```

`cust check` builds `cust_check`; `cust check -p <m>` builds
`cust_check_<m>` (CHK-D-10).

Consequences:

* **`cust build` runs zero check work** — `cust_check` is not in
  `all`, so a build never fires a check command. (The build-mode
  `surface-module` + `rewrite-file` commands it shares *are* in
  `all`, so a prior build warms the rewrites + fragments for free,
  but the reverse is not true: check never drags a link into
  `all`.)
* **`cust check` fires exactly the needed commands** — it runs
  `cmake --build --target cust_check`, whose `DEPENDS` pull each
  per-module check command (re-firing only stale ones) → whose
  `DEPENDS` pull the `.rewrite` TUs + build-mode fragments
  (materialising any missing ones). The whole check is one Ninja
  sub-DAG rooted at `cust_check`.

`run_check_path` collapses to **emit + configure + build** (mirror
V46D-5): materialise the prelude + `ensure_dirs`, refresh the
dep-symlink, emit the `CMakeLists` (now including the check
commands + targets), configure (skipped when unchanged, V42D-8),
then `cmake --build --target cust_check` (or `cust_check_<m>` when
`-p <m>` is given, CHK-D-10). No driver-side surface pass remains.
`build::run_phase1` is **deleted** once nothing calls it.

### CHK-D-5 — Check reuses build-mode fragments + crate headers

**Proposed (mirror V46D-7).** The check compile resolves
cross-module and cross-crate references through artifacts the
build path already produces:

* **Cross-module** (`#cust use crate::X`) → the build-mode
  `<frag>/<crate>/X.cust.h` (a `surface-module` / `surface-cycle`
  `OUTPUT`). The check command `DEPENDS` it (and the `.rewrite` TU
  `#include`s it by absolute path).
* **Cross-crate** (`#cust use <dep>`) → the dep member's published
  crate header `<dep>.h` (a `crate-header` `OUTPUT`), reached
  through the dep-view symlink (`deps-root`). The check command
  `DEPENDS` the dep's `crate-header` output.

Therefore check **does not regenerate fragments or crate headers**
— it consumes them. This avoids the "multiple rules generate the
same `.cust.h`" Ninja error (the same trap V46D-7 navigates) and
means a warm build makes a cold check cheap. Unlike the test path
(`-DCUST_TEST_BUILD=1`), check uses the **plain** build-mode
cflags — there is no check-specific define, so the fragments it
consumes are exactly the ones `cust build` consumes. No guard test
is needed (there is no second define to diverge).

### CHK-D-6 — No fragment/header content or layout change

**Proposed (mirror V45D / V46D-6).** This milestone adds a
`.check/` stamp tree and nothing else new on disk. Fragments,
crate headers, rewrites, and their paths are untouched. The only
observable behaviour deltas are (a) `cust check` now reports
diagnostics and can fail (CHK-D-1) and (b) it is incremental
(CHK-D-3). No golden-`cwork.cmake` *fragment* lines change; the
golden gains the per-module check command block + `cust_check`
target block (a pure addition, asserted by the slice-B golden
update).

### CHK-D-7 — No-op `cust check` spawns zero codegen processes

**Proposed (verification target, mirror V45D-12 / V46D-8).** A
second `cust check` with no source change must spawn **zero**
clang and **zero** plugin processes. Because the check compile is
a direct `${CMAKE_C_COMPILER}` command (not a `cust internal`
leaf), the `CUST_TRACE_INTERNAL` trace-file method does not
observe it; the no-op property is verified instead by Ninja
reporting "no work to do" for `--target cust_check` (and, for the
shared `rewrite-file` / `surface-module` leaves, the trace file
staying untouched). Slice D fixes the exact probe.

### CHK-D-8 — Single-module check incrementality

**Proposed (verification target).** Editing one module's body
re-fires that module's check command (and any downstream module
whose fragment changed, via the existing build-mode surface DAG
restat) and nothing else. Editing a *comment-only* line that
leaves the lowered `.rewrite` bytes identical re-fires nothing
past the re-rewrite (the existing `rewrite-file` byte-skip means
an unchanged rewrite output does not restat-trigger the check
compile downstream).

### CHK-D-9 — cwork check dogfood

**Proposed (mirror V45D-13 / V46D-9).** `cust check` over cwork:
(a) passes clean on the unmodified tree, (b) is incremental (no-op
= 0 spawns, single-module edit = 1 check command), and (c) **fails
with a surfaced diagnostic** when a type error is injected into one
cstd module — the regression that proves CHK-D-1 is real and not
vacuous.

### CHK-D-10 — `-p` scoping under the single-target model

**Proposed.** `cust check -p <member>` interacts with the
aggregate-target model and must be handled explicitly. Today
[`run_check_path`](../../cust/src/workspace.rs#L746) honours the
`-p`-scoped subset (it only checks the named members). A single
workspace-wide `cust_check` target cannot express that.
**Resolved: emit one aggregate target per member —
`cust_check_<member>`, each `DEPENDS` only that member's
`.checked` stamps — plus an umbrella `cust_check` that `DEPENDS`
all of them. `cust check` (no `-p`) builds `cust_check`; `cust
check -p <m>` builds `cust_check_<m>`.** This mirrors how the
build path already selects per-member `--target`s via
`DriveOptions.only`, so no new dispatch shape is introduced.

The plugin flag is always emitted: the plugin is mandatory for
`cust check` (§0), so there is no plugin-less check command to
emit and `build_member_compile_options` always carries the
`-fplugin` token.

**Emitter-layer rule (the one subtlety the §0 removal leaves
open).** The removal made the plugin mandatory at the *driver*
layer — [`resolve_plugin`](../../cust/src/cli.rs#L505) now returns
a non-optional `Plugin` and every caller passes `plugin:
Some(&plugin)`. But the *emitter* layer still models the plugin as
optional ([`WorkspaceView.plugin_path:
Option<PathBuf>`](../../cust/src/cmake_emit.rs#L67), `SurfaceCmd.plugin:
Option<PathBuf>`), because the emitter's own unit tests construct
views without a real `.so`. The new `CheckCommand` builder (slice
A) lives in that emitter layer, so it must **treat the plugin path
as required at construction**: when `plugin_path` is `None` it
emits *no* check command for the module (never a plugin-less
`-fsyntax-only` command). This keeps the false-green §0 deleted
from silently reappearing inside the check path via an
`Option`-shaped builder — the production driver always supplies
`Some`, and a `None` (test-only) view simply produces no check
commands rather than a tolerance-free-but-pluginless one. Slice
A's argv-shape test asserts a `Some(plugin)` view bakes the
`-fplugin` token, and that a `None` view emits zero check
commands.

---

## 5. Open questions (RQ-CHK-N) — resolved

Following the project convention (simple solution, match the
existing grain, or defer):

### RQ-CHK-1 — Stamp granularity: per-module or per-crate?

**Resolved: per-module.** Matches the build-mode surface DAG
grain (V45D-4) and gives single-module incrementality (CHK-D-8). A
per-crate stamp would re-check the whole crate on any one-module
edit — the coarse grain v0.4.5 RQ-V45-1 already rejected for
fragments.

### RQ-CHK-2 — Keep a driver-side fast path for trivial crates?

**Resolved: no, accept one code path.** `cust check` always goes
through CMake (emit + configure + `--target cust_check`). The
configure cost is real but paid once and amortised; a second code
path would duplicate the dispatch and risk drift. If a trivial
crate's configure ever measurably dominates, revisit — but YAGNI
until shown. (This is the honest cost of reversing V42D-15; §3
argues it is acceptable because there is no second tree or stamp
flap.)

### RQ-CHK-3 — Strict bin-half check now, or lib-surface only?

**Resolved: lib-surface only, defer bins.** Today's check is a
tolerant lib-surface pass (V44D-9); this milestone makes the *lib*
half error-reporting + incremental and leaves the bin half exactly
as scoped. Wiring a check command over bin modules is purely
additive (emit the same direct-clang command for each bin TU's
`.rewrite` output, anchor on `cust_check`) and earns its own
decision when scheduled.

### RQ-CHK-4 — Compile a fresh lowering, or reuse the build `.rewrite`?

**Resolved: reuse the build-path `.rewrite` verbatim.** The lib
half's `.rewrite/<crate>/src/<m>.c` (a `rewrite-file` `OUTPUT`,
[`rewrite_one`](../../cust/src/generate.rs#L69)) is *already*
`#cust use`-lowered to fragment-header `#include`s — it is the
exact TU the lib target compiles. Reusing it (rather than
re-lowering inside a check leaf) means check validates the
identical bytes the build compiles, adds no second lowering
implementation, and needs no `cust` subprocess (RQ-CHK-6).
Compiling the *raw* source is a non-starter — `#cust use` is not
valid C.

### RQ-CHK-5 — Does a failed check leave a stale stamp?

**Resolved: no — failure never touches the stamp.** The check
command is two ordered `COMMAND`s (clang, then
`${CMAKE_COMMAND} -E touch`); Ninja runs the `touch` only if clang
exits zero (CHK-D-3). A failed check aborts before the touch, so
the `OUTPUT` stays unproduced and the check re-fires next run —
identical to how a failed compile never produces its `.o`. No
"passed once, cached forever" hazard, and no driver logic needed
to enforce it.

### RQ-CHK-6 — Direct `${CMAKE_C_COMPILER}` command, or a `check-module` leaf?

**Resolved: direct clang command, no leaf.** Every other
generation step is a `cust internal …` leaf because each carries
logic clang lacks (fixed-point iteration, fragment concatenation,
test discovery). The check compile is the lone exception: its only
cust-specific input — lowering — is already supplied by the
`rewrite-file` command it depends on (RQ-CHK-4), and everything
else is plain clang flags the emitter bakes. A `check-module` leaf
would be a pure `exec(clang)` wrapper that adds a process layer,
a second cflags-assembly path to keep in sync with the baked argv,
and a swallow-the-exit-code risk — the very failure mode CHK-D-1
exists to remove. Invoking clang directly (the same way CMake
invokes the real compile) lets diagnostics and exit status flow
straight through, which *is* the CHK-D-1 requirement. The cost —
check is the one step not uniformly a leaf — is acceptable and
called out in CHK-D-2 / §8.

---

## 6. Verification target

A CMake-owned incremental check is correct when:

1. **Meaningful failure.** `cust check` over a crate with a type
   error in a lib module **exits non-zero** and prints the clang +
   plugin diagnostic (CHK-D-1) — the case today's tolerant pass
   silently passes.
2. **Clean pass parity.** `cust check` over a clean crate exits
   zero and reports each checked member, as today.
3. **No-op check = zero codegen spawns** (CHK-D-7). A second
   `cust check` with no edits runs no clang and no plugin process
   (Ninja reports nothing to do for `cust_check`; the shared
   `internal` leaves leave the trace file empty).
4. **Single-module incrementality** (CHK-D-8). Editing one
   module's body re-fires that module's check command and nothing
   unrelated.
5. **`cust build` runs zero check work** (CHK-D-4). A build (cold
   or no-op) produces no `.checked` stamp and fires no check
   command — `cust_check` is outside `all`.
6. **Fragment/crate-header reuse holds** (CHK-D-5). Check consumes
   the build-mode fragments + crate headers (no duplicate
   `.h-fragments` / crate-header rule); a warm `cust build`
   followed by a cold `cust check` re-checks but does not
   regenerate fragments.
7. **cwork dogfood green** (CHK-D-9), including the injected-error
   regression and the no-op/single-module incrementality checks.

---

## 7. Implementation slices (planned)

Same A→E cadence as v0.4.2 … v0.4.6:

| Slice | Scope |
| --- | --- |
| **A** | Layout helper + check-argv builder + emitter scaffolding (no behaviour change). Add `check_stamp_path(crate, qname)` (and a `check_dir(crate)` helper) to `TargetLayout`; the `.check/<crate>/` dir is created by the driver before `cmake --build` (slice C) since a `cmake -E touch` does **not** create parent dirs and check has no leaf to self-create it. **Reuse the lib target's `compile_options`** ([`build_member_compile_options`](../../cust/src/cmake_emit.rs#L2081), already in `cmake_emit` — no port from `build.rs`) to build the check argv: that list verbatim, with an explicit `-std=` prepended, the `SHELL:` wrapper stripped off `-fplugin`, and `-fsyntax-only` + the `.rewrite` source appended (CHK-D-2). So the argv keeps profile + extra cflags, `-fvisibility`, `-include <prelude>`, bare `-fplugin`, **and** `-Wno-unknown-attributes` (mirrors the build), with **no** fragment-out and **no** `-Wno-error`. Add a `CheckCommand` struct + `MemberView.check_commands` to `cmake_emit` (populated, but `emit_*` not yet wired into `generate`). Tests: argv-shape unit test asserting a `CheckCommand` for a cwork module bakes an explicit `-std=` (the **`-std` regression guard**, CHK-D-2), `-fsyntax-only`, the plugin flag with no `SHELL:` and no fragment-out, no `-Wno-error`, `-Wno-unknown-attributes` present (CHK-D-3), and the `.rewrite` source; a drift-guard asserting the argv's middle equals `build_member_compile_options` (modulo the `-std`/`SHELL:`/`-fsyntax-only` deltas); plus a `None`-plugin view emits **zero** check commands (CHK-D-10 emitter-layer rule). No driver / golden change. |
| **B** | Emit the check commands + targets. Wire `emit_check_commands` into `generate` (per lib module: direct clang check `COMMAND` + `touch` `COMMAND`; OUTPUT = `.checked`; DEPENDS = `.rewrite` TU + plugin + imported build-mode fragments + dep crate-headers, CHK-D-3/CHK-D-5) + the per-member `cust_check_<member>` targets and the umbrella `cust_check`, all `EXCLUDE_FROM_ALL` (CHK-D-4/CHK-D-10). Golden `cwork.cmake` gains the check block + targets (pure addition, CHK-D-6). No driver change yet — emitted but not driven. |
| **C** | Drive check through CMake. `run_check_path` collapses to emit + configure + `cmake --build --target cust_check` (or `cust_check_<m>` for `-p`, CHK-D-4/CHK-D-10); delete the per-member `run_phase1` call from the check path. Cross-module/cross-crate resolution verified through the `.rewrite` + fragment + dep crate-header `DEPENDS`. cwork `cust check` green end-to-end, and **fails** on an injected type error (CHK-D-1). |
| **D** | Incrementality + isolation properties. No-op check = 0 spawns (CHK-D-7, via the chosen Ninja "no work" / clang-spawn probe), single-module incrementality (CHK-D-8), `cust build` fires zero check work (CHK-D-4 / verification item 5), the injected-error regression hardened (CHK-D-1 / verification item 1). |
| **E** | Cleanup + docs closeout + dogfood (CHK-D-9). Delete `run_phase1` outright once nothing calls it (the test path dropped it in v0.4.6; check is its last caller). Verify §6 scenarios against cwork. Flip this file's status to shipped; patch cust-design.md §17 + §10 (check is no longer tolerant) + the V42D-15 / V44D-9 cross-references. |

Slice-by-slice deltas + commit hashes land here once the slices
ship, in the "Shipped deltas" shape v0.4.3 … v0.4.6 use.

---

## 8. Risks / notes

* **Custom commands inherit no target flags (CHK-D-2 hazard).**
  The single highest-risk detail: a `${CMAKE_C_COMPILER}` call in
  an `add_custom_command` does **not** pick up `CMAKE_C_STANDARD`,
  `target_compile_options`, or the `SHELL:` escape that the lib
  *target* relies on. The check command reuses the lib target's
  `compile_options` (`build_member_compile_options`) verbatim and
  prepends an **explicit `-std=`**, or it silently checks under the
  wrong C standard — masking real errors and inventing fake ones,
  defeating CHK-D-1. Slice A's argv-shape test pins the explicit
  `-std=` as a regression guard; reusing `compile_options` (rather
  than re-deriving an argv) means there is no second flag-assembly
  path, so check and the real lib compile cannot drift.
* **Behaviour change blast radius (CHK-D-1).** Making check fail
  on errors it previously swallowed could surprise existing
  workflows / CI that ran `cust check` expecting it to always pass.
  This is intended (a check that can't fail is theatre), but the
  closeout (slice E) must call it out prominently in the milestone
  notes and cust-design.md §10.
* **`run_phase1` deletion (slice E).** v0.4.6 removed the test
  path's `run_phase1` call but kept the function for the check
  path. This milestone removes its last caller; the deletion is
  the clean signal that *all* driver-side pre-passes are gone and
  every generation/validation step is a CMake custom command.
* **Configure cost on trivial crates (RQ-CHK-2).** Accepted. The
  one-time configure may dominate a sub-second surface pass on a
  toy crate; the amortised steady state (no-op check = 0 spawns,
  no reconfigure) is the case that matters.
* **No fragment divergence guard needed (CHK-D-5).** Unlike the
  test path (V46D-7's `-DCUST_TEST_BUILD` divergence risk), check
  uses the plain build-mode cflags, so it consumes the identical
  fragments `cust build` does — there is no second define that
  could ever fork the published surface.
* **Check is the one non-leaf step (RQ-CHK-6).** Every other
  generation/validation step is a `cust internal …` leaf; the
  check compile is a direct `${CMAKE_C_COMPILER}` command. This
  asymmetry is deliberate (check adds no logic clang lacks once
  the `.rewrite` TU exists) but is worth flagging for anyone
  scanning the emitter expecting uniformity. The check argv reuses
  the real lib compile's `compile_options` directly — slice A does
  not factor a second assembly, so they cannot drift.
