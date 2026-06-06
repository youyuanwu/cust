# `cust` — A Cargo‑style Build System for C (clang‑only)

Status: **design sketch / brainstorm**, 2026‑06‑03
Owner: TBD

---

## 1. Goals & non‑goals

### Goals

* Bring the ergonomics of a `Cargo.toml` + `src/lib.rs` project to C.
* Single source of truth: a `Cust.toml` manifest describes the crate, its
  dependencies, features, and build settings.
* `src/lib.c` is the unambiguous *root* of a library crate; `src/main.c`
  is the unambiguous root of a binary crate.
* `cust build`, `cust check`, `cust test`, `cust run`, `cust doc`,
  `cust add <dep>`, `cust fmt`, `cust clippy` (→ clang‑tidy) all work
  with no Makefile / CMakeLists in sight.
* Hermetic, reproducible, content‑addressed build cache under `target/`.
* Workspaces of many crates (`[workspace] members = [...]`).

### Non‑goals (at least for v1)

* Supporting GCC, MSVC, or any compiler other than clang. We *use*
  clang‑specific features aggressively; portability is the user's job
  (we can emit portable `compile_commands.json` for those who need it).
* Cross‑language interop beyond "export a C ABI" (no built‑in C++,
  Rust, Swift bridges; those can ship as community crates).
* Reimplementing libc. We assume a hosted toolchain by default and
  expose a `[no‑std]`‑style switch for freestanding targets.

---

## 2. On‑disk layout

```
my_crate/
├── Cust.toml              # manifest
├── Cust.lock              # resolved dep graph (committed for bins, optional for libs)
├── src/
│   ├── lib.c              # crate root (library) — OR
│   ├── main.c             # crate root (binary)
│   ├── prelude.h          # auto‑included into every TU (optional, opt‑in)
│   ├── parser/
│   │   ├── mod.c          # `mod parser;` declared from lib.c maps here
│   │   └── lexer.c        # `mod lexer;` declared from parser/mod.c
│   └── util.c             # `mod util;` declared from lib.c
├── include/
│   └── my_crate.h         # GENERATED public header (do not edit)
├── tests/                 # integration tests, one binary per file
│   └── smoke.c
├── benches/               # one binary per file, linked against the crate
│   └── parse_bench.c
├── examples/              # one binary per file
│   └── hello.c
├── build.cust.c           # optional build script (compiled & run by cust)
└── target/                # all build output; gitignored
    ├── debug/
    ├── release/
    ├── doc/
    └── .cache/            # content‑addressed object cache
```

Mirroring Cargo deliberately: the muscle memory transfers, and so does
the tooling story (one well‑known root makes IDE setup trivial — we just
emit `compile_commands.json` into `target/`).

---

## 3. `Cust.toml` schema (v0)

```toml
[package]
name        = "my_crate"
version     = "0.1.0"
edition     = "2026"            # selects prelude + plugin defaults; semantics TBD (§16 OQ-7)
authors     = ["Alice <a@example.com>"]
description = "A small parser"
license     = "MIT OR Apache-2.0"
repository  = "https://example.com/alice/my_crate"

[lib]
# defaults: path = "src/lib.c", crate-type = ["staticlib"]
crate-type  = ["staticlib", "cdylib"]
# `rlib` analogue: cust's native "fat object" format, see §7.
# Possible values: "staticlib", "cdylib", "bin", "rlib"

[features]
default     = ["json"]
json        = ["dep:cjson"]
simd        = []                # gates `#if CUST_FEATURE_simd`
unsafe-fast = ["simd"]

[dependencies]
cjson       = "1.4"
log         = { version = "0.3", features = ["color"] }
local_thing = { path = "../local_thing" }
git_dep     = { git = "https://example.com/x.git", tag = "v2.0" }

[build-dependencies]
bindgen     = "0.1"             # used only by build.cust.c

[dev-dependencies]
criterion_c = "0.2"

[profile.dev]
opt-level   = 0
debug       = "full"            # → -g3 -gdwarf-5
sanitize    = ["address", "undefined"]
lto         = false

[profile.release]
opt-level   = 3
debug       = "line-tables-only"
lto         = "thin"            # ThinLTO across the whole crate graph
codegen-units = 1               # purely a hint to the driver
panic       = "abort"           # see §11 (test harness aborts on assert)

[target.'cfg(target_os = "linux")'.dependencies]
io_uring    = "0.5"

[clang]
# Compiler flags that survive into every TU of *this* crate
# (deps don't inherit; deps must declare their own).
extra-cflags   = ["-Wall", "-Wextra", "-Wpedantic"]
extra-ldflags  = []
std            = "c23"          # → -std=c23
visibility     = "hidden"       # default for non-pub symbols, see §6

[plugin]
# Cust loads its own clang plugin by default; users can add more.
extra = ["cust-derive-eq", "cust-async"]
```

Open question: do we want a separate `[bin]` / `[[bin]]` table like
Cargo? Yes — same shape, omitted here for brevity.

---

## 4. Module system — the central design question

C has no module system. Rust's `mod foo;` is *the* feature people miss
most when they leave the language. We pick **per‑module compilation
with plugin‑generated fragment headers** as the canonical design and
briefly note what we rejected.

### Chosen design — one TU per module, plugin‑generated headers

Each `#cust mod foo;` introduces a new module backed by `foo.c` (or
`foo/mod.c` for a folder module). Every module is its own translation
unit — compiled by its own clang invocation, producing its own `.bc`
or `.o`. Module privacy maps onto C linkage naturally: `static` in a
module's `.c` file *is* module‑private, exactly as standard C already
implies.

The cust plugin runs on each module twice (logically; see pipeline
below) and emits a **fragment header** containing forward declarations
for every decl annotated `[[cust::pub]]` or `[[cust::pub(crate)]]`.
Downstream modules acquire that surface by writing `#cust use
crate::foo;`, which the cust driver rewrites to `#include
"target/<profile>/.h-fragments/<crate>/foo.cust.h"` before invoking
clang.

Source example:

```c
// src/lib.c
#cust mod util;            // declares submodule — src/util.c
#cust mod parser;          // declares submodule — src/parser/mod.c

#cust use crate::util;     // pulls in target/.h-fragments/<crate>/util.cust.h

[[cust::pub]] int my_crate_init(void);
```

```c
// src/parser/mod.c
#cust mod lexer;
#cust use crate::util;

[[cust::pub(crate)]] typedef struct parser parser;
[[cust::pub(crate)]] parser *parser_new(void);

static int parser_grow(parser *p);  // module‑private, plain C `static`
```

`#cust mod` and `#cust use` are line‑oriented pragmas processed by the
cust driver, not real preprocessor directives — so the surface syntax
is obviously distinct from `#include`, and clang sees only the
rewritten form.

### Build pipeline

A build of a crate runs in three phases. Phase 1 and 2 each parallelise
over modules; phase 3 is the link.

1. **Surface extraction** (cheap). For every module: run clang with
   `-fsyntax-only -fplugin=libcust_plugin.so` plus the union of
   *already‑known* fragment headers from previously‑built sibling
   modules (or the empty set on a clean build). The plugin emits the
   module's `<module>.cust.h` fragment to a tempfile, then atomically
   swaps it into `target/<profile>/.h-fragments/<crate>/`. If the
   resulting bytes are identical to what was already there, nothing
   downstream needs to rebuild — same trick rustc's metadata stamping
   uses.
2. **Codegen.** For every module whose fragment header *or own source*
   changed (or whose transitive fragment‑header dependencies changed,
   tracked via clang's `-MMD -MF` depfiles in phase 1), run clang again
   with full codegen: `-c -emit-llvm` for ThinLTO bitcode, or `-c` for
   plain object output. Plugin runs here too, but only to enforce
   `[[cust::*]]` semantic contracts — not to emit headers.
3. **Crate header + link.** Concatenate every module's `[[cust::pub]]`
   (not `pub(crate)`) fragment into
   `target/<profile>/build/<crate>/include/<crate>.h`
   (path migration shipped in v0.3 — see v0.3.0.md scope item 6).
   Modules are emitted in **topological order over intra-crate
   `#cust use crate::<mod>;` edges** (Kahn's algorithm, stable on
   ties so the existing DFS-preorder behaviour is preserved for
   crates without intra-crate type deps). This matters because a
   sibling module can export a typedef used by the root or by an
   earlier sibling (cstd's `types` exports `i32`/`usize`, `lib`
   and `math` consume them) — declaration order in the
   concatenated header has to match the type-dependency DAG, not
   the file-discovery order. Link the bitcode/objects into the
   requested `crate-type` artifacts.

This is the same shape as Cargo / rustc: a metadata pass (`rmeta`)
followed by full codegen (`rlib`), with cheap metadata stamping
gating downstream rebuilds.

### Circular module dependencies

Mutual recursion across modules is supported because fragment headers
contain **forward declarations only**. By default:

* `[[cust::pub]] struct foo { … };` exports only the opaque tag
  `struct foo;` into the fragment header. The body stays private to
  the defining module — same model as Rust struct privacy.
* `[[cust::pub(repr)]] struct foo { … };` exports the full struct body
  into the fragment header. Use sparingly; it forces a rebuild of
  every importer when the layout changes (and bakes the layout into
  their object files).
* `[[cust::pub]] inline foo(…) { … }` similarly exports the body.

With opaque‑by‑default exports, modules A and B can each `#cust use`
the other's fragment header without a circular #include problem — the
includes contain only `extern` decls. The two‑phase pipeline (surface
extraction before codegen) also breaks the worst case: phase 1
succeeds with empty inputs the first time around, then iterates until
fixed point.

**Operational definition of "fixed point":** an iteration produces no
byte‑different fragment header for any module compared to the previous
iteration. We cap at three iterations. Empirically, acyclic
`pub(repr)` graphs converge in 1 iteration, 2‑cycles in 2, longer
cycles diverge — so a still‑changing 4th iteration is the design's
definition of a "genuine layout cycle" and produces a diagnostic of
the form:

```
error: circular `[[cust::pub(repr)]]` dependency did not converge
  in 3 iterations between modules: parser::ast, parser::types
  hint: break the cycle by exporting one side as `[[cust::pub]]`
        (opaque) instead of `[[cust::pub(repr)]]`
```

If real‑world crates surface 3‑ or 4‑cycles that *do* converge in 4+
iterations, raise the cap rather than rejecting them — the cap is a
divergence detector, not a complexity limit.

### Incremental compilation

The fragment‑header stamping in phase 1 is what makes incremental
compilation real:

* A change to a module's *body* (private decls, function bodies of
  non‑`pub(repr)` items) does **not** regenerate its fragment header
  → only that module is rebuilt.
* A change to any `[[cust::pub*]]` decl regenerates the fragment
  header → every importer rebuilds, but only those importers.
* Clang's `-MMD -MF` depfiles drive the executor's incremental graph
  for the per‑TU `#include` graph. **Fragment‑header invalidation is
  tracked separately**, as explicit build‑graph edges: every
  per‑module codegen step lists
  `target/<profile>/.h-fragments/<crate>/<importee>.cust.h` as an input
  (one edge per `#cust use crate::<importee>`). When phase 1
  atomically swaps a fragment, the executor invalidates the importers
  via these edges. Relying on the depfile alone would not work
  (depfiles record what was `#include`d, not what re‑exported decls
  changed).

The "executor" is Ninja by default ([cmake-and-portability.md
§2.4](cmake-and-portability.md) explains why) but the design only
requires *some* DAG‑based executor that consumes clang depfiles — a
hand‑rolled scheduler, `samurai`, even `make -j` with `.d` files would
satisfy the same contract.

Finer‑grained incrementality (per‑function) is out of scope; whole‑module
rebuilds are believed to be fast enough at the module sizes typical of
well‑factored C code (low single‑digit kLOC). This is a measurable
assumption, not a proven property — see §16 OQ‑9. The v0.1‑v0.2
prototype phase will benchmark module sizes of 1k/5k/10k/20k LOC and
establish a `cust check`‑time warning when a single module exceeds the
threshold at which incremental rebuilds stop feeling instant.

#### Filesystem case and Unicode normalisation

Module names are case‑sensitive and must map to unique files on every
filesystem (case‑sensitive and case‑insensitive alike). On macOS and
NTFS the driver canonicalises each declared module name via
`realpath()`‑equivalent and Unicode NFC normalisation, then detects
collisions:

```
error: modules `foo` and `Foo` declared in lib.c both resolve to the
  same file (foo.c) on this filesystem
  hint: case-insensitive filesystem detected; rename one module
```

Symbolic links inside `src/` that resolve to a file outside the crate
root are rejected. UTF‑8 module names are accepted but must be NFC.

#### Concurrent builds and `target/` locking

The fragment‑stamping invariant ("byte‑identical → no downstream
rebuild") assumes a single writer per fragment. Workspace builds with
`cust build -j N` take a **per‑crate exclusive lock** on
`target/<crate>/.lock` for the duration of phase 1 + phase 2; this
serialises any two builds of the same crate while still allowing
different crates in a workspace to build in parallel. `cust check` and
`cust build` against the same crate also contend for this lock. The
lock is advisory (`flock`/`LockFileEx`) and held by a long‑lived
driver process; a crashed driver leaves a stale lock that the next
driver invocation reclaims after checking that the pid is gone.

#### Features and modules

Features gate decls, not modules. `#cust mod foo;` always compiles
`foo.c`; gating the *contents* of `foo.c` with `[[cust::feature("x")]]`
or traditional `#ifdef` is how an unused feature drops out of codegen.
To gate the *existence* of a sibling module, wrap the `#cust mod`
itself: `[[cust::feature("experimental")]] #cust mod fancy;`. Fragment
headers are part of the per‑crate cache key, so toggling features
invalidates the affected fragments and triggers rebuilds of importers.

**Note on attribute timing:** `[[cust::feature(...)]]` and
`[[cust::cfg(...)]]` are processed by the **driver pre‑pass**, not by
the in‑process clang plugin, because the preprocessor runs before the
plugin sees the AST. See §9 for the responsibility split.

### Considered alternatives (rejected)

**Unity build with text splicing.** Synthesize a top‑level
`my_crate.driver.c` that `#include`s every module's `.c` and compile
as a single TU. Trivial to implement and gets whole‑crate optimisation
for free, but incremental compilation is coarse (any change rebuilds
the whole crate) and `static` semantics shift (TU‑private now means
*crate*‑private, not module‑private — surprising for C programmers).
Kept as a possible `cust build --unity` mode for tiny crates / debug
builds, but not the default.

**Clang's `-fmodules` + module maps.** Generate `module.modulemap`
from `Cust.toml` and the directory tree. Clang already implements
cached PCMs, so this would be fast — but the C support is rougher than
ObjC/C++ (diagnostics get strange, edge cases bite). We may revisit
once the chosen design is stable; for now, our plugin‑generated
fragment headers give us equivalent isolation without subjecting users
to modules‑specific bugs.

---

## 5. Public API — the crate header

The per‑module fragment headers from §4 are an internal mechanism. For
downstream consumers we also synthesise a single crate‑level header at
`target/<profile>/include/<crate>.h`, formed by concatenating just the
`[[cust::pub]]` (not `pub(crate)`) decls from every module's fragment.

```c
// src/lib.c
[[cust::pub]] typedef struct mc_parser mc_parser;
[[cust::pub]] mc_parser *mc_parser_new(void);
[[cust::pub]] void       mc_parser_free(mc_parser *);
                int      mc_internal_helper(void);   // not exported
```

→ `target/<profile>/include/my_crate.h` (generated each build):

```c
/* @generated by cust 0.1.0 — DO NOT EDIT */
#ifndef MY_CRATE_H
#define MY_CRATE_H
#ifdef __cplusplus
extern "C" {
#endif

typedef struct mc_parser mc_parser;
mc_parser *mc_parser_new(void);
void       mc_parser_free(mc_parser *);

#ifdef __cplusplus
}
#endif
#endif
```

The generated header lives in `target/` and is **not** checked in (see
[cmake-and-portability.md](cmake-and-portability.md)). Stable copies
for downstream consumers are produced on demand by:

* `cust export cmake --consumable` — copies the header into the export
  bundle alongside the `<crate>Config.cmake` artifacts.
* `cust publish` — seals the header into the distributed crate.

### No `#include` injection

The generated crate header is **pure declarations**. cust does
**not** inject `#include <stdint.h>`, `<stddef.h>`, `<stdbool.h>`,
or any other system header into it. Consequence: if a crate's
public surface mentions a type that lives in a system header
(e.g. `int32_t`, `size_t`, `bool`), the crate must either

1. export its own `cust_pub typedef` alias for that type — the
   Cargo-parity pattern (`cstd` defines bare `i32`, `u64`,
   `usize`, … as `cust_pub typedef`s, so a consumer that
   `#cust use cstd;` reaches the aliases by name without
   needing `<stdint.h>`), or
2. accept that consumers must `#include <stdint.h>` themselves
   before the `#cust use <crate>;` directive that lowers to
   `#include "<crate>.h"`.

Rationale: include injection silently couples every cust crate
to libc, breaks the future `freestanding = true` profile
(§16 OQ-8), and creates a hidden contract (consumers come to
rely on the injected includes, then break when we remove
them). Forcing the producer to be explicit about its surface
types keeps the contract honest. Clang's `__INT32_TYPE__` /
`__UINT64_TYPE__` / `__SIZE_TYPE__` builtin macros let a crate
define type aliases without itself including `<stdint.h>` —
the pretty-printer resolves them to the underlying primitive
(`int`, `unsigned long`, …) when emitting the typedef into the
fragment header.

---

## 6. Visibility & symbol hygiene

C's default `extern` linkage is the equivalent of `pub` in Rust — i.e.
the wrong default. We fix this with two layers:

1. **Compile‑time:** pass `-fvisibility=hidden` to clang. Decls marked
   `[[cust::pub]]` expand (via a prelude macro) to
   `__attribute__((visibility("default")))`, restoring export.
2. **Link‑time:** generate a linker version script from the same
   `[[cust::pub]]` set and pass it via `-Wl,--version-script=...`. This
   catches the case where a dependency leaks symbols even if it forgot
   to set hidden visibility itself.

For static‑lib output we additionally run `llvm-objcopy
--localize-hidden` on each object, so users who link the `.a` into
their own binary don't see internal symbols.

`[[cust::pub(crate)]]` is the per‑module compilation analogue of
Rust's `pub(crate)`: the symbol needs real external linkage at the
`.o` level (so sibling modules in the same crate can resolve it at
link time) but is hidden from the final artifact. Implemented by
leaving visibility default during per‑module codegen, then localising
those specific symbols at the crate link step.

---

## 7. Dependency & artifact model

* All deps are **source dependencies**, fetched into `~/.cust/registry/`
  (mirror of `~/.cargo/registry/`). Pre‑built blobs are out of scope
  for v1; reproducibility wins.
* Each dep is built as its own crate (its own `Cust.toml`), producing:
  * an **rlib‑equivalent** artifact: `lib<name>.cust` — a tarball of
    LLVM bitcode object files (`.bc`), the generated public header,
    and a `metadata.json` file. Stored in `target/<profile>/deps/`.
  * optionally a `staticlib` / `cdylib` if `crate-type` requests them.
* Final link: cust drives clang with all the bitcode files; ThinLTO
  inlines across crate boundaries the same way `cargo build --release`
  does. This is the single biggest "feels like Rust" win.

### rlib `metadata.json` schema

```jsonc
{
  "rlib_format_version": 1,        // increments on breaking format change
  "crate_name":   "my_crate",
  "crate_version":"0.1.0",
  "features":     ["json"],        // features enabled at build time
  "llvm_version": "19.1.0",        // see "bitcode compatibility" below
  "target_triple":"x86_64-unknown-linux-gnu",
  "cflags_exported": [],           // see "flag inheritance" below
  "link_deps":    [ { "name": "z", "kind": "dylib" } ]
}
```

### Bitcode compatibility

LLVM bitcode is **not stable across major LLVM versions** (and not
formally stable across minor versions either, though minor bumps rarely
break in practice). ThinLTO summary metadata, in particular, drifts
with every release.

Cust enforces this at the rlib boundary, not at the codegen boundary:

* Every rlib records `llvm_version` in `metadata.json` at build time.
* Before any ThinLTO link, the driver checks every consumed rlib's
  `llvm_version` against the currently active vendored LLVM. On
  mismatch:

  ```
  error: rlib `foo 1.2.3` was built with LLVM 18.1.0 but the active
    toolchain is LLVM 19.1.0; bitcode is not compatible.
    hint: run `cust clean && cust build` to rebuild deps with the
          current toolchain
  ```

* On `rustup`‑style toolchain swap, the driver invalidates all rlibs
  with mismatched `llvm_version` automatically (deletes them from the
  per‑user `target/<profile>/deps/` cache; user re‑fetches/rebuilds).

The `rlib_format_version` is independent of `llvm_version`: it covers
changes to *our* tarball schema (e.g. adding new metadata fields). Old
cust drivers reading a newer `rlib_format_version` fail with a clear
error; newer drivers reading an older version accept it within the
documented compatibility window (see `docs/spec/rlib-format.md`, OQ‑4).

Lock file (`Cust.lock`) is the same idea as Cargo's: pinned versions,
SHA256 of the source tarball, content addresses of build inputs.

---

## 8. Clang features we lean on (inventory)

These are the existing clang capabilities we exploit. None require a
plugin; the plugin (§10) layers extra semantics on top.

| Feature | Used for |
|---|---|
| `-fvisibility=hidden` + `__attribute__((visibility(...)))` | default‑private symbols (§6) |
| `-flto=thin` | cross‑crate inlining (§7) |
| `-fsanitize=address,undefined,thread,memory,leak` | `cust test --sanitize` |
| `-fsanitize-coverage=trace-pc-guard` | `cust fuzz` (later) |
| `-fprofile-instr-generate` + `llvm-profdata` + `llvm-cov` | `cust test --coverage` |
| `-ftime-trace` | `cust build --timings` (Chrome trace viewer) |
| `-fsyntax-only` | `cust check` (fast no‑codegen pass); also drives §4 phase 1 surface extraction |
| `-MMD -MF` | per‑module incremental dep tracking (fragment header invalidation) |
| `-fdiagnostics-format=sarif` (or `-fdiagnostics-print-source-range-info`) | machine‑readable errors for IDEs |
| `-fcolor-diagnostics` | pretty terminal output |
| `__attribute__((annotate("...")))` | survives into LLVM IR; lets plugins recognise our attrs even after macro expansion |
| `__attribute__((cleanup(fn)))` | basis for prelude `defer!()` macro |
| `__attribute__((overloadable))` | clang‑only C extension; lets us implement Rust‑style `_Generic`‑free overloads |
| `__attribute__((warn_unused_result))` | underpins our `[[cust::must_use]]` |
| `__attribute__((nonnull / returns_nonnull / format / malloc / pure / const))` | richer types in the prelude (`Box(T)`, `Option(T)` macros) |
| `__builtin_expect`, `__builtin_unreachable`, `__builtin_constant_p` | prelude `likely!()`, `unreachable!()`, `const_assert!()` |
| `_Generic` (C11) | type‑directed dispatch in the prelude |
| `_BitInt(N)` (C23) | exact‑width integer support |
| `#embed` (C23) | `include_bytes!()` equivalent without build scripts |
| `__has_attribute`, `__has_builtin`, `__has_include` | feature detection in the prelude |
| Blocks (`^{}`) | optional `[[cust::closure]]` support for higher‑order APIs |
| `-Xclang -load <plugin.so> -Xclang -add-plugin <name>` | how we install our plugin (§10) |
| `compile_commands.json` (we emit it) | clangd / IDE integration for free |

---

## 9. Custom `cust::` attribute catalogue

All cust attributes are spelled with C23 attribute syntax
`[[cust::name(...)]]`. The prelude maps each to a portable expansion
that combines `__attribute__((annotate("cust::name(...)")))` (so the
plugin can see them) with the appropriate real clang attribute (so they
have semantic teeth even without the plugin loaded).

### Where each attribute is processed

cust attributes are not all processed by the same component. Three
phases exist, and the catalogue below states which phase owns each
attribute:

* **Driver pre‑pass.** A line‑oriented text/preprocessor pass cust
  runs before invoking clang, used for attributes that must influence
  *what gets parsed* (preprocessor‑level decisions). Examples:
  `[[cust::cfg]]`, `[[cust::feature]]`, `[[cust::pub_macro]]`.
* **Clang plugin (AST).** The in‑process `PluginASTAction` (§10),
  used for everything that needs the parsed AST: visibility lifting,
  fragment header synthesis, test discovery, derive codegen,
  per‑TU semantic checks.
* **Linker / post‑link.** Visibility version scripts, localisation,
  link‑only attribute checks.

### Catalogue (initial; not exhaustive)

| Attribute | Applies to | Phase | Meaning |
|---|---|---|---|
| `[[cust::pub]]` | any decl | plugin | Exported from the crate; lifts visibility, adds to generated `.h`. |
| `[[cust::pub(crate)]]` | any decl | plugin | Exported to other modules in this crate only. |
| `[[cust::pub(repr)]]` | struct/enum | plugin | Like `pub`, but exports the *body* of the type (not just the opaque tag) into the fragment header. Forces importer rebuilds on layout change — use sparingly. See §4. |
| `[[cust::pub_macro]]` | object‑like macro | **driver pre‑pass** | Exported via generated `.h`. Because macro definitions are erased by clang's preprocessor before the plugin sees the AST, the driver pre‑pass tokenises the source itself and extracts the macro definitions verbatim into the fragment header. (Future: switch to clang `PPCallbacks` once the surface is stable.) |
| `[[cust::test]]` | function returning `void` or `int` | plugin | Test fn; auto‑collected by `cust test` harness (§11). |
| `[[cust::bench]]` | function | plugin | Discoverable benchmark. |
| `[[cust::cfg(expr)]]` | any decl/stmt | **driver pre‑pass** | Compile‑in‑or‑out based on features/targets. The driver evaluates `expr` against the resolved feature set and rewrites the attribute to a conventional `#if CUST_CFG_<hash>` / `#endif` region *before* invoking clang, so it gates parsing the same way hand‑written `#ifdef` would. |
| `[[cust::feature(name)]]` | any decl | **driver pre‑pass** | Shorthand for `cfg(feature = "name")`; same mechanism. |
| `[[cust::must_use]]` | fn or type | plugin + clang | Maps to `__attribute__((warn_unused_result))` (clang native); plugin additionally checks type‑level enforcement (e.g. ignored return wrapped in a struct). |
| `[[cust::no_panic]]` | fn | plugin (per‑TU heuristic) | The plugin walks calls reachable from the annotated fn *within the same translation unit*. If any reaches `cust_panic` directly or via a `static` callee, warn. Calls through `extern` functions, function pointers, or other TUs are **not** checked at plugin time; §16 OQ‑4 covers a possible post‑link reachability pass. Document any function annotated `no_panic` accordingly. |
| `[[cust::const]]` | fn | plugin | Plugin enforces that the body is a clang‑constexpr‑equivalent subset; allows use in `[[cust::static_assert]]`. |
| `[[cust::deprecated("msg")]]` | any decl | plugin + clang | Maps to clang's deprecated attr; the plugin honours `since = ".."`. |
| `[[cust::derive(Eq, Hash, Debug)]]` | struct/enum | plugin | Plugin generates `_eq`, `_hash`, `_debug` companion fns. |
| `[[cust::repr(C \| packed \| transparent)]]` | struct/enum | plugin | Layout control, validated by the plugin. |
| `[[cust::unsafe]]` | fn | plugin | Marks the fn as needing an `unsafe { ... }` (macro) at call sites; plugin enforces. |
| `[[cust::link("name", kind = "static")]]` | extern decl | plugin + linker | Equivalent of `#[link]` in Rust; the plugin records the request and the driver adds `-lname` (or static link) at the final step. |
| `[[cust::ctor]]` / `[[cust::dtor]]` | fn | plugin + clang | Wraps `__attribute__((constructor/destructor))` with ordering metadata. |
| `[[cust::asm_export("symbol")]]` | fn | plugin | Force a specific asm symbol name; bypass mangling rules. |
| `[[cust::doc("...")]]` | any decl | plugin | Doc comment alternative for places where `///`‑style comments would be lost (e.g., inside macros). |

### Fallback when the plugin is not loaded

Why both `annotate(...)` *and* a real clang attribute? Because the
plugin may be disabled (`cust check --no-plugin`, or a downstream user
importing our generated `.h` with their own toolchain). When the
plugin is loaded you get the full semantics. When it is not:

* `clang`‑native attributes (e.g. `must_use` → `warn_unused_result`)
  still enforce their portion of the contract.
* `[[cust::pub]]` and `[[cust::pub(crate)]]` still get visibility
  lifting via the prelude macro (which expands unconditionally to
  `__attribute__((visibility("default")))`); fragment header synthesis
  is skipped, so the crate cannot be built but `cust check` still
  type‑checks single TUs.
* `[[cust::test]]`, `[[cust::derive]]`, `[[cust::pub_macro]]`,
  `[[cust::no_panic]]`, `[[cust::const]]` all become **silent no‑ops**.
  This is acceptable for `cust check --no-plugin` (a fast lint pass)
  but not for `cust build`, which requires the plugin.
* Driver pre‑pass attributes (`cfg`, `feature`) work either way —
  they're processed before clang is invoked at all.

Dual‑path correctness is enforced by a test matrix: each attribute has
`test_<attr>_with_plugin`, `test_<attr>_without_plugin`, and
`test_<attr>_fallback_behavior` tests; divergences are documented in
`docs/ATTRIBUTE-SEMANTICS.md`.

### Prelude macro shape for `[[cust::pub]]` (and why there are two)

The v0.x prelude exposes `[[cust::pub]]` via two macro spellings,
picked by decl kind:

* `cust_pub` — for **functions and variables**. Expands to
  `__attribute__((visibility("default"), annotate("cust::pub")))`.
  Visibility lifts the symbol over the crate-wide
  `-fvisibility=hidden`; the annotate carries the plugin signal.
* `cust_pub_t` — for **type declarations** (`typedef`, `struct`,
  `union`, `enum`). Expands to
  `__attribute__((annotate("cust::pub")))` only — no visibility.
  Type decls have no linkage; applying `visibility("default")`
  to a `typedef` produces a clang
  `'visibility' attribute ignored [-Wignored-attributes]`
  warning. cstd's `types` module hits this 12 times if you use
  the wrong macro.

Both forms expand to the same plugin annotation, so the plugin
(§10) treats `cust_pub` and `cust_pub_t` decls identically when
building the fragment header. The macro split is purely about
which underlying clang attribute is meaningful for the decl
kind. A future plugin v2 that makes `[[cust::pub]]` decl-kind-aware
(emitting the right combination automatically) can collapse
these into a single macro (§16 OQ — to be filed).

---

## 10. The cust clang plugin

We ship one canonical clang plugin, `libcust_plugin.so`. It is an
**in‑process `clang::PluginASTAction`** loaded via `-fplugin=...`
(clang ≥ 19) or `-Xclang -load ...` for older versions. The plugin runs
inside the clang process; communication is via clang's C++ AST and
`DiagnosticsEngine`, not via stdin/stdout IPC. This is deliberate —
the alternative (out‑of‑process AST serialisation) is roughly an order
of magnitude more design surface for negligible v1 benefit.

Consequences of "plugin is in‑process C++" propagated to the rest of
the design:

* Pre‑parse work (`[[cust::cfg]]`, `[[cust::feature]]`,
  `[[cust::pub_macro]]` extraction) is done by the **driver pre‑pass**,
  not by the plugin (§9).
* Anything that requires whole‑program reasoning (e.g. honest
  `[[cust::no_panic]]`) is either downgraded to a per‑TU heuristic or
  deferred to a post‑link analysis pass (§16 OQ‑4).
* A future C reimplementation of the driver does not get a
  language‑boundary IPC for free; it must either link against the
  same C++ plugin (cross‑language link) or grow its own parallel C++
  plugin invocation path. See
  [implementation-language.md](implementation-language.md) §7.

### Plugin jobs

The plugin performs four jobs, all with file or in‑binary outputs (no
driver‑side in‑process callbacks required):

1. **AST inspection.** Emits diagnostics when an attribute contract is
   violated (e.g. `must_use` return value discarded; non‑`C` `repr` on
   a struct passed across the ABI boundary; per‑TU `no_panic`
   reachability). Output channel: clang's `DiagnosticsEngine` (i.e.
   stderr, picked up by the driver via clang's stderr stream).
2. **Fragment header synthesis.** Pretty‑prints forward declarations
   of `[[cust::pub]]`, `[[cust::pub(crate)]]`, and `[[cust::pub(repr)]]`
   items into `target/<profile>/.h-fragments/<crate>/<module>.cust.h`.
   Atomic swap + byte‑identical comparison gate downstream rebuilds
   (§4 phase 1). The driver later concatenates the `[[cust::pub]]`
   subset across all modules into the crate header
   `target/<profile>/include/<crate>.h` (§5). Output channel: files.
3. **Test discovery.** Collects `[[cust::test]]` / `cust_test`
   functions and serialises them into a static
   `__cust_tests[]` table emitted into the per‑crate generated
   runner TU (`cust_test_main.c`), one entry per discovered
   test (`(qname, fn_ptr, fn_kind, ignored, file, line)`).
   The runner's `main` iterates the table and forks per test
   (§11). Two backends sit behind this same table contract:
   * **Driver pre‑pass** (shipped in v0.3.2) — a line‑oriented
     regex scanner over module sources. Restrictions: the
     marker, return type (`int` or `void`), and function name
     must all appear on one source line. Lives forever as the
     `cust check --no-plugin` discovery path (§9 fallback).
     See [v0.3.2.md](v0.3.2.md) V32D‑2 for the full contract.
   * **Plugin v1** (v0.4) — walks the AST, no single‑line
     restriction, recognises both the `cust_test` macro and
     the `[[cust::test]]` attribute spelling. Joins as a
     second backend; the pre‑pass stays in tree.
   Earlier drafts of this paragraph proposed ctor‑based
   registration as a workaround for section‑layout instability
   across object formats; the static‑table emission both
   backends use today sidesteps that problem entirely (driver
   or plugin already knows the full test list at TU generation
   time, no runtime registration required).
   Output channel: emitted C, compiled into the test binary.
4. **Derive‑style codegen.** For `[[cust::derive(...)]]`, the plugin
   appends new top‑level decls (e.g. `T_eq`, `T_hash`, `T_debug`) to
   the AST before codegen. Strictly additive: never mutates user code.
   Output channel: in‑process AST, materialised in the bitcode / object
   that clang emits for the TU.

### Phase‑mode detection

The same `libcust_plugin.so` is loaded in two contexts:

* **Phase 1 (surface extraction):** clang invoked with `-fsyntax-only`.
  The plugin emits fragment headers and runs cheap AST checks. It
  must *not* emit derive codegen (there's no codegen to attach to).
* **Phase 2 (codegen):** clang invoked with `-c -emit-llvm` (or `-c`).
  The plugin runs full semantic checks and derive codegen. It must
  *not* re‑emit fragment headers (phase 1 already settled them; a
  re‑emit would invalidate the stamping invariant).

The plugin detects which phase it is in via clang's `CompilerInstance`
API — specifically, `instance.getFrontendOpts().ProgramAction ==
ParseSyntaxOnly` distinguishes phase 1 from phase 2. A unit test in
the plugin's test suite (`test_phase_isolation.cc`) invokes the plugin
twice on the same TU in the two modes and asserts that phase 1 outputs
are a subset of what phase 2 would have produced, and that phase 2
produces no fragment headers.

Failures from the plugin surface as real clang diagnostics, so they
look and behave like any other clang error.

### Procedural macros — a stretch goal

Rust's proc macros run before parsing; clang plugins run after. We
*can* fake the proc‑macro experience by:

* Recognising a magic function decl with `[[cust::macro]]`,
* In the plugin, splicing in the generated AST nodes (similar to how
  `[[cust::derive]]` works).

This is awkward for token‑stream‑shaped macros (e.g., DSLs), so for v1
we keep proc macros out of scope and lean on `_Generic`, `[[cust::derive]]`,
and code generators run via `build.cust.c`.

---

## 11. `cust test`

* Test files live in `tests/` (each file = one integration test
  binary, linked against the crate's public surface only — same model
  as Cargo) and in any `src/**.c` file via `[[cust::test]]` fns
  (internal/unit tests, linked against private surface).
* The driver auto‑generates a `main` that iterates the registered
  tests (§10 job #3), with optional parallel execution, `--filter`,
  JUnit XML output, and a per‑test timeout.
* **Per‑test process isolation.** Each `[[cust::test]]` runs in its
  own forked subprocess (`fork()` on POSIX; `CreateProcess` on
  Windows). This is what makes `--timeout` and assertion behaviour
  well‑defined:
  * Timeout expiry: parent sends `SIGTERM`, waits 100 ms, sends
    `SIGKILL`. The OS releases the subprocess's file descriptors,
    memory, sockets, and any temp files it left in `$TMPDIR`. Files
    written to `$OUT_DIR` are preserved for inspection.
  * `cust_panic` (assertion failure) aborts only the test's
    subprocess; the harness records the failure and moves on to the
    next test. A test file with 100 tests and 1 failing assertion
    runs all 100 tests and reports 1 failure.
  * The default is fork‑based isolation; opt out per‑test with
    `[[cust::test(inline)]]` for the rare test whose setup cost is
    too high to fork (e.g. test that depends on a built‑up shared
    in‑process cache). Inline tests are documented as "first failure
    aborts subsequent inline tests in the same file."
* Sanitizers, coverage, and the address‑/UB‑sanitiser blacklist are
  set via `[profile.test]` in `Cust.toml`.
* `cust test --doc` (later) compiles `///` code blocks from doc
  comments as standalone TUs, the way `rustdoc` does. Powered by a
  separate libclang‑based doc‑comment extractor.

### v0.3.2 implementation — driver pre‑pass discovery

What ships today (v0.3.2) is the unit‑test subset of the above:

* Tests colocated in `src/**.c`, marked with the `cust_test` or
  `cust_test_ignore` macros (the `[[cust::test]]` attribute
  spelling arrives with plugin v1 in v0.4 — RQ‑V32‑1).
* Discovery via the **driver pre‑pass** scanner
  (`cust/src/test_scanner.rs`) — line‑oriented regex; the marker,
  return type (`int` or `void`), and function name must fit on
  one source line. V32D‑2 trade‑off; plugin v1 lifts it.
* Cargo‑shape CLI: `cust test [-p <member>] [<filter>] [-- --list]`
  with substring filter, exit code 0/1, and a Cargo‑format
  per‑binary summary (`test result: ok. N passed; M failed; …`).
* **Fork‑per‑test** isolation (V32D‑7) — Linux only,
  `fork`/`waitpid`/`_exit(101)`. This is stricter than stock
  `cargo test` (which runs threads inside one process); it matches
  `cargo‑nextest`'s "one test per process" model and survives a
  test‑side `SIGSEGV` / `abort()` without taking the binary down.
* No `[profile.test]`, no per‑test timeout, no `--nocapture`,
  no `--exact`, no multi‑filter, no parallel test execution, no
  integration tests under `tests/` — all deferred to v0.4.

The runner template lives in `cust/src/test_runner_template.c`
(included into the driver via `include_str!`) and is concatenated
ahead of per‑test `extern` decls + a static `__cust_tests[]`
table + the runner's `main`. Output path:
`target/<profile>/test/<crate>/<crate>` (V32D‑4 + V32D‑5,
resolved in favour of V32D‑4's "fully fresh build tree").

Full locked V32D‑N decisions, deferrals, and the verification
target live in [v0.3.2.md](v0.3.2.md). Plugin v1 in v0.4 joins
as a *second* discovery backend behind the same
`__cust_tests[]` contract; the v0.3.2 pre‑pass stays in tree as
the `cust check --no-plugin` discovery path (§9).

---

## 12. Build scripts

`build.cust.c` is the cargo `build.rs` equivalent. The driver
*compiles it with cust itself* (yes, a tiny self‑hosting moment) and
runs it; its stdout is parsed line‑by‑line:

```
cust:rerun-if-changed=schema.json
cust:rustc-link-lib=static=foo
cust:cust-cflag=-DGENERATED_PARSER=1
cust:warning=Couldn't find libfoo, falling back to bundled copy
```

Scripts can emit *generated source files* into
`$OUT_DIR` (`target/<profile>/build/<crate>/out`); a small
`#cust include_generated!("parser.gen.c");` directive (line‑oriented
pragma like `#cust mod`) splices them into the crate at the requested
location.

### Timeout (hang protection)

Build scripts are executed with a **default wall‑clock timeout of 300
seconds** (5 minutes), configurable per‑crate via:

```toml
[profile.dev]
build-script-timeout = 600    # seconds; 0 disables (not recommended)
```

On expiry the driver sends `SIGTERM`, waits 10 seconds, then `SIGKILL`,
and reports:

```
error: build script for crate `foo` exceeded timeout (300s)
  hint: increase build-script-timeout, or check for an infinite loop
        or a hung network call in build.cust.c
```

The script's stdout up to the timeout is retained for debugging in
`target/<profile>/build/<crate>/stdout.txt`. This is a **hang‑protection
measure**, not a security boundary; sandboxing of build scripts is
tracked separately in §16 OQ‑10.

---

## 13. Tooling integration

* **clangd**: cust always writes a fresh `compile_commands.json` into
  `target/`. Symlink (or `.clangd` file) at the repo root points to
  it. No extra setup.
* **clang‑tidy**: `cust clippy` is just clang‑tidy with a curated
  config (`.clang-tidy` shipped in `~/.cust/`). The cust plugin
  contributes a few custom checks (`cust-no-raw-malloc`,
  `cust-prefer-arena`, …).
* **clang‑format**: `cust fmt` shells out to clang‑format with a
  cust‑provided `.clang-format`. Users can override.
* **lldb / gdb**: full `-g` by default in dev; we ship a tiny lldb
  formatter script for `Option(T)` / `Result(T,E)` / `Vec(T)` prelude
  types so `p some_vec` looks like Rust's `p some_vec`.
* **CI**: `cust ci` runs `check`, `clippy`, `fmt --check`, `test
  --all-features`, and `test --no-default-features` in one go.

---

## 14. CMake & compiler portability

See the companion doc:
[cmake-and-portability.md](cmake-and-portability.md).

Headline decisions (full reasoning in that doc):

* CMake is **never** in the build hot path — not as the driver, not as
  a transient intermediate in `target/`. We drive clang directly and
  emit `build.ninja` for larger crates.
* CMake **is** a first‑class *export* format for downstream consumers
  (`cust export cmake --consumable` / `--standalone`).
* GCC support is a deliberate non‑goal. CMake does not meaningfully
  ease a future GCC port; the hard parts (plugin parity, attribute
  survival, bitcode rlibs) are architectural. If GCC ever becomes a
  goal we introduce a `Backend` abstraction inside cust.

The `[[cust::link(...)]]` attribute and the build‑script
`cust:cust-link-lib=` line must carry enough metadata (`kind`,
`framework`, `whole-archive`) to round‑trip into
`target_link_libraries(... PUBLIC ...)` in the exported CMake bundle.

---

## 15. Implementation language

See the companion doc:
[implementation-language.md](implementation-language.md).

Headline decisions (full reasoning in that doc):

* **Driver:** Rust (v0–v2). Cargo is the literal reference design;
  the ecosystem gives us TOML, semver, HTTP+TLS, the registry client,
  and the parallel scheduler for free. Memory safety matters because
  cust handles untrusted manifests, tarballs, and registry responses.
* **Clang plugin:** C++. Forced — `clang::PluginASTAction` is a C++
  API and there is no other supported way to plug into clang's AST.
* **User code in cust crates:** C. Build scripts (`build.cust.c`): C
  too, compiled by cust itself.
* **Self‑hosting (`cust-c`)** is a deliberate v3+ goal, not a v1
  requirement. We keep the door open by stabilising *contracts*
  (rlib bitcode format, registry protocol, `target/` layout,
  `Cust.toml` schema) rather than crate boundaries.

**Important caveat on the driver ↔ plugin seam:** the plugin is a
C++ `clang::PluginASTAction` (§10), loaded in‑process by clang. There
is no language‑boundary IPC at this seam; the Rust driver couples to
the plugin via clang's C++ ABI. A future C reimplementation of the
driver therefore either (a) links against the same C++ plugin
(cross‑language link) or (b) ships a parallel C plugin invocation
path. Earlier drafts of this document proposed a line‑oriented
JSON/msgpack IPC at this seam; that was incorrect — in‑process plugin
actions cannot work that way. The actual portability story is the
*file‑based* and *manifest‑based* contracts (rlib format, `target/`
layout, `Cust.toml`), not the plugin protocol.

---

## 16. Open design questions

Each question is tagged with a v1‑gating class:

* **ⓐ v1‑blocking** — must be answered before v1.0 because the answer
  influences ABI, file format, or core semantics that can't be changed
  later without a breaking release.
* **ⓑ v1.x‑safe** — can ship incrementally inside the v1.x line
  without breaking v1.0 binaries.
* **ⓒ post‑v1** — lower priority; explicitly out of scope for v1.

Unclassified questions are tracked but not yet triaged.

1. **OQ‑1 ⓒ Non‑ASCII identifiers in `[[cust::derive(...)]]`?**
   Restrict to ASCII for v1; revisit when we add a stable reflection
   story.
2. **OQ‑2 ⓑ Portable `enum` discriminant size.** Clang accepts
   `enum E : uint8_t { … }` (C23). Lean on that, fail on older `-std`.
3. **OQ‑3 ⓐ Memory model for prelude `Vec`/`Box`/`Arc`.** Default
   allocator trait via weak symbol (`cust_allocator`), overridable by
   linking a different impl — same trick the Rust `#[global_allocator]`
   mechanism uses at the symbol level. The *choice* is v1‑blocking
   because it bakes into every crate that uses the prelude types;
   changing it later means an ABI break across the ecosystem.
4. **OQ‑4 ⓐ Panic story.** `cust_panic(file, line, msg)` is
   weak‑linked; default impl prints+aborts. `[profile.*] panic =
   "abort" | "unwind"` selects whether the prelude installs
   setjmp/longjmp unwinding. v1.0 must commit to *which modes exist*
   even if it ships with only `abort` implemented — adding `unwind`
   in v1.1 is an ABI break otherwise. Whole‑program `[[cust::no_panic]]`
   reachability proof (vs. the per‑TU heuristic in §9) is a
   post‑v1 enhancement, gated on a post‑link analysis tool.
5. **OQ‑5 ⓑ Re‑exports** (`pub use other_crate::foo;`). v1 workaround:
   include their header from your header. A real re‑export requires
   the plugin to merge generated headers across crates — additive,
   safe to add post‑v1.0.
6. **OQ‑6 ⓑ Workspace inheritance.** Cargo's `workspace.dependencies`
   is convenient; mirror it (`[workspace.dependencies]` then
   `dep = { workspace = true }`). Additive to the manifest schema;
   safe in v1.x.
7. **OQ‑7 ⓐ Editions semantics.** `Cust.toml` carries an `edition`
   field but the design currently says only "selects prelude + plugin
   defaults". Before v1.0 we must define: what concretely changes
   between editions (prelude API surface, default `-Wflags`, default
   plugin lints, default visibility); the migration tooling story
   (`cust fix --edition NEXT`); the support window (how many editions
   are maintained concurrently); and how the edition value
   participates in artifact compatibility (does an `edition=2027`
   rlib link cleanly into an `edition=2026` consumer?). A separate
   `docs/EDITION-DESIGN.md` is the planned home for this.
8. **OQ‑8 ⓐ `no_std` analogue.** A `[package] freestanding = true`
   flag that forbids `<stdio.h>` etc.; the prelude swaps to a
   freestanding subset and the linker no longer pulls in libc
   startup. v1‑blocking only to the extent that the prelude must be
   *split* before v1.0 (core vs. hosted) — actually shipping the
   freestanding profile can come in v1.x. If we ship a monolithic
   prelude in v1.0 we can't add `freestanding = true` without a break.
9. **OQ‑9 ⓑ Module size warning threshold.** §4 incremental
   compilation assumes "typical C module sizes" are fast enough.
   Prototype‑phase benchmark must establish a concrete threshold for
   a `cust check`‑time warning; not v1‑blocking because the warning
   is advisory.
10. **OQ‑10 ⓑ Build‑script sandboxing.** §12 ships hang‑protection
    (timeout) in v1; sandboxing (`unshare`/seccomp/landlock on Linux;
    Job objects on Windows) is tracked separately. Adding sandboxing
    post‑v1 is safe as long as the sandbox policy is conservative
    enough not to break existing build scripts; the design must
    document which syscalls scripts are allowed to rely on.
11. **OQ‑11 ⓑ Editor support beyond clangd.** A `cust-analyzer` LSP
    shim that wraps clangd and adds `cust::` attribute completions,
    `mod` navigation, and feature‑gated dead‑code dimming.
12. **OQ‑12 ⓐ Contract surface versioning policy.** The five public
    contracts (rlib `metadata.json` schema, registry protocol,
    `target/` layout, `Cust.toml` schema, prelude ABI) need a
    documented versioning + deprecation policy before v1.0 ships. Each
    contract gets a `docs/spec/<name>.md` doc with: version field
    location, semver/breaking‑change rules, deprecation window,
    detection mechanism for cross‑version consumption (e.g. how an
    old cust driver reading a newer rlib fails clearly rather than
    silently corrupting). The `rlib_format_version` and
    `target/.cust-version` fields exist in v1; this OQ is about
    formalising the *policy*, not just the version slots.

(CMake / GCC portability questions live in
[cmake-and-portability.md](cmake-and-portability.md) §6.)

---

## 17. MVP roadmap

Per-version detail lives in companion docs as it gets written.
Roadmap bullets here are deliberately short:

* **v0.1 — driver skeleton.** ✅ **shipped.** Single-file library
  crate (`src/lib.c` only, no modules, no deps), system clang, write
  `staticlib`. `[[cust::pub]]` is a no-op preprocessor macro (no
  plugin yet). Full locked scope, shipped deltas, and verification
  notes in [v0.1.md](v0.1.md).
* **v0.2 — module loader & plugin v0.** ✅ **shipped.** Per-module
  scheduler walks `#cust mod` / `#cust use crate::…` (line-oriented
  driver pragmas via `mod_scanner`). Plugin v0
  (`libcust_plugin.so`) emits per-module `.cust.h` fragments into
  `target/<profile>/.h-fragments/<crate>/`; the build pipeline
  runs a surface pass then lowers each `#cust use crate::X;` to
  `#include "<X.cust.h>"` for codegen. Per-crate concatenated
  header (path moved under `target/<profile>/build/<crate>/include/`
  in v0.3). (`cust new` shipped in v0.1.) Full shipped details,
  deferrals, and verification in [v0.2.md](v0.2.md).
* **v0.3 — workspaces & path dependencies.** ✅ **shipped.**
  `[workspace] members = […]` in `Cust.toml`; path deps
  (`dep = { path = "…" }`) resolved against the workspace member
  list; topological multi-crate build with `target/<profile>/deps/<name>`
  symlink farm publishing each member's archive + crate header.
  New cross-crate import directive `#cust use <name>;` lowered
  by the driver to an `#include` of the dep's public header.
  `Cust.lock` (v1 schema, path-only form) emitted at the
  workspace root — contract documented in
  [../spec/cust-lock.md](../spec/cust-lock.md). `cust build -p`
  / `--package` scopes to one member + transitive deps. Per-member
  outputs moved to `target/<profile>/build/<member>/`. Full
  shipped details, locked V3D-N decisions, deferrals, and
  verification in [v0.3.0.md](v0.3.0.md).
* **v0.3.1 — binary crates & `cust run`.** ✅ **shipped.**
  Small single-purpose milestone slotted between v0.3 and v0.4.
  `[[bin]]` table (single entry; multi-bin via `src/bin/*.c`
  deferred to v0.4) plus auto-inference: `src/main.c` →
  bin-only, `src/lib.c` + `src/main.c` → lib+bin (Cargo shape).
  Bin output at `target/<profile>/<crate>` (Cargo parity).
  `cust run [-p <member>] [--release] [-- <argv>…]` builds the
  target bin + transitive deps, spawns it inheriting stdio,
  exits with the child's exit code. `cust new --bin` lights up
  the previously-reserved flag. V31D-6 enforces lib-only deps
  (lib → bin / bin → bin edges rejected at edge-resolution).
  Cargo-parity intra-crate self-import: `#cust use <own-name>;`
  in the bin half of a lib+bin crate resolves to the local lib
  header (mirrors Rust's `use my_crate::*;` in `src/main.rs`).
  `Cust.lock` schema unchanged (V31D-10 — bin-vs-lib is not a
  property of the resolved graph). Full shipped details, locked
  V31D-N decisions, and verification in [v0.3.1.md](v0.3.1.md).
* **v0.3.2 — `cust test` (Rust-style unit tests).** ✅ **shipped.**
  Small single-purpose milestone slotted between v0.3.1 and
  v0.4. `cust_test` / `cust_test_ignore` macros + Cargo-shape
  `cust test [-p <member>] [<filter>] [-- --list]` subcommand
  with substring filter, `--list`, and a Cargo-format per-binary
  summary. Tests colocated in `src/**.c`, discovered by a
  **driver pre-pass** scanner (V32D-2) — the macro,
  return type (`int` or `void`), and function name fit on one
  source line. Plugin v1 in v0.4 joins as a *second* discovery
  backend behind the same `__cust_tests[]` table contract; the
  pre-pass stays in tree as the `cust check --no-plugin` path.
  **Fork-per-test** isolation (V32D-7) — Linux only,
  `fork`/`waitpid`/`_exit(101)`, stricter than stock `cargo
  test` (matches `cargo-nextest`'s "one test per process"
  model). V32D-11 rejects `cust test -p <bin-only>`; V32D-12
  silently skips bin-only members in workspace-bare runs.
  Test build is fully isolated in
  `target/<profile>/test/<crate>/` (V32D-4) with
  `-DCUST_TEST_BUILD=1` activating the prelude's test branch
  so `cust_test`-marked functions stop decaying to
  `static unused`. No `Cust.toml` schema changes. Per-test
  timeout, `--nocapture`, `--exact`, multi-filter, parallel
  test execution, `tests/` integration tests, and
  `[[cust::test(inline)]]` all deferred to v0.4. Full shipped
  details, locked V32D-N decisions, and verification in
  [v0.3.2.md](v0.3.2.md).
* **v0.4 — dependency resolver, registry, plugin v1, tests.**
  Brings the *network* half of dep work that v0.3 deferred:
  initial registry wire protocol (`Index` trait, `file://` first
  per V3D-1's deferral), `cust add`, semver version resolution,
  `Cust.lock` source hashes, `[workspace.dependencies]`
  inheritance (OQ-6). Plus build scripts (`build.cust.c`) with
  the §12 hang-protection timeout, plugin v1 (full AST-based
  fragment header synthesis, `[[cust::pub(repr)]]` opt-in body
  export), circular-dep fixed-point loop with the convergence
  criterion (§4), plugin-v1 second test-discovery backend
  (replaces the v0.3.2 pre-pass scanner's single-line regex
  with an AST walk; both backends coexist behind the same
  `__cust_tests[]` contract — V32D-2), `tests/` integration
  tests, `[[cust::test(inline)]]` + `should_panic` + per-test
  timeout (need `[profile.test]` plumbing), `--nocapture` /
  `--exact` / multi-filter / `--test-threads N`, and `-jN`
  parallelism (within and across crates). Multi-bin per crate
  (`src/bin/*.c`, `[[bin]]` arrays) also lands here (V31D-3
  deferral from v0.3.1). v0.3.1 + v0.3.2 are the prerequisites
  for the multi-target / parallel-test-harness work in this
  milestone.
* **v0.5 — sanitizers, coverage, profiles, `cust check`.**
* **v0.6 — ThinLTO across the dep graph, bitcode rlib format with
  `metadata.json` including `rlib_format_version` and `llvm_version`
  (§7). Toolchain-swap rlib invalidation.**
* **v0.7 — `[[cust::derive]]`, `[[cust::must_use]]`, `[[cust::no_panic]]`
  (per-TU heuristic) plugin checks; clang-tidy integration.**
* **v0.8 — `cust export cmake --consumable` and generated-source
  splicing beyond `include_generated!`.** (Workspaces shipped in
  v0.3, build scripts in v0.4.)
* **v0.9 — `cust export ninja` for our own use** plus any deferred
  cmake-export polish from v0.8.
* **v0.10 — close the v1-blocking open questions (§16 OQ-3, OQ-4,
  OQ-7, OQ-8, OQ-12): allocator policy, panic mode commitments,
  editions semantics, freestanding prelude split, contract
  versioning policy. Each lands as a `docs/spec/<name>.md` doc.**
* **v1.0 — docs (`cust doc` via libclang), stable plugin ABI, registry
  hosting story.**

---

## 18. Risks

* **Clang plugin ABI is unstable** between major LLVM versions. v0.1
  side‑steps this by using system clang and no plugin (see
  [v0.1.md](v0.1.md)). Once the plugin lands in v0.2, the
  pinned‑clang question reopens; if ABI churn proves painful in
  v0.4–v0.7 we revisit vendoring (`rustup` pattern, separate
  SECURITY‑PATCH‑POLICY doc). Until then, the minimum clang version
  (currently 17) is the contract.
* **Macros vs attributes** — clang attributes can't always attach where
  C programmers expect (e.g., before a `typedef struct { … } X;`
  declaration). The prelude has to provide both attribute and macro
  spellings for several items; the plugin must canonicalise.
* **Dual‑attribute surface** — each `[[cust::*]]` attribute is enforced
  in two places (clang‑native attr + cust plugin). Per‑attribute test
  matrix (with/without plugin, multiple clang versions) and
  `docs/ATTRIBUTE-SEMANTICS.md` document divergences. Grows with the
  catalogue; tax compounds across minor versions.
* **Build scripts are arbitrary C.** Hang‑protection timeout ships in
  v1 (§12); sandboxing is tracked in OQ‑10.
* **Doc cross‑references** — `§6` / `§7` style refs across files are
  fragile under section renumbering. CI is expected to validate the
  refs (parse `^## \d+\.` headers, fail on dangling `§N`).

(CMake interop risks and the GCC‑support pressure risk live in
[cmake-and-portability.md](cmake-and-portability.md) §7.)

---

## 19. Suggested next steps

1. Lock down the `Cust.toml` v0 schema as a separate doc (`schema.md`)
   so the driver and the plugin can share a single serde‑style model.
2. Prototype the `#cust mod` / `#cust use` driver pragmas and the
   per‑module surface‑extraction pipeline (§4) against three throwaway
   sample crates: a parser, a tiny HTTP client, and a SIMD math
   kernel. Each one stresses a different part of the design (modules
   + circular `pub` graphs, deps, feature flags).
3. Write the plugin skeleton (`PluginASTAction` + a single
   `RecursiveASTVisitor` that collects `[[clang::annotate("cust::pub")]]`
   decls and prints them). That's the smallest end‑to‑end demo that
   proves the whole approach.
