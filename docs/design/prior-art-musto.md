# Prior art — musto

**Status:** reference + idea bank, 2026-06-11.
**Companion to:** [v0.4.2.md](v0.4.2.md) (CMake backend),
[cust-design.md §17](cust-design.md) roadmap.
**Subject:** [musto](https://github.com/youyuanwu/musto), a
Cargo-shaped build frontend for C++23 modules that has been
shipping CMake-as-driver since v0.1.0 (currently v0.5.2, ~5
shipped versions over ~9 months).

This doc captures what cust can learn from musto's
implementation. Musto is the closest existing prior art for
the design cust adopted in v0.4.2: a TOML manifest at the
top, CMake/Ninja underneath, the user never writes CMake.
The two projects target different *languages* (C++23 modules
vs cust's annotated-C-with-clang-plugin) but the *driver
architecture* is nearly identical.

Source files referenced live under `/home/user1/code/musto/`;
when this doc cites a file, follow the link to read it in
context.

---

## 1. What's the same, what's different

| Aspect | cust v0.4.2 | musto v0.5.2 |
|---|---|---|
| Language | C (clang ≥ 19) | C++23 with modules (clang 21 / gcc 15) |
| Manifest | `Cust.toml` (Cargo-shape) | `Musto.toml` (Cargo-shape) |
| Lockfile | `Cust.lock` (v0.3+, path-only) | None yet (v0.5.3 planned) |
| Driver lang | Rust | C++23 modules (self-hosted) |
| Build backend | CMake + Ninja (v0.4.2+) | CMake + Ninja (v0.1.0+) |
| Workspace shape | Flat single CMakeLists (V42D-13) | Per-member + `add_subdirectory` umbrella |
| Configure-skip stamp | SHA-256 of generated CMakeLists (V42D-8) | SHA-256 of canonicalised TOML |
| `check` subcommand | Surface pass only, bypass CMake (V42D-15) | Single-TU fast path, multi-TU falls through to CMake |
| Diagnostic rewriter | Slice B / V42D-18 (planned) | Shipped since v0.1.0 |
| Test isolation | Fork-per-test (V32D-7) | One process per `[[test]]`, aggregated exit code |
| Plugin model | clang plugin (`libcust_plugin.so`) | None — pure CMake + compiler |
| External path deps | Not yet (v0.4.6) | Synthetic members under `@external/<name>/` (v0.4.0) |

**The pieces that differ are almost entirely cust-specific
machinery** (clang plugin, fragment headers, `#cust use`
lowering, `[[cust::*]]` attribute survival). The backend
plumbing — manifest → CMake emit → stamp-skip configure →
ninja build → diagnostic rewrite — is the same shape in both
projects, and v0.4.2 implements the shape musto validated.

---

## 2. Per-member CMakeLists with `add_subdirectory` umbrella

**Relevant cust decision:** [V42D-13](v0.4.2.md) (single
workspace CMakeLists, locked).

**Musto's approach:** one CMakeLists per member at
`target/<profile>/.build/<member>/CMakeLists.txt`, glued
together by an umbrella at
`target/<profile>/.build/CMakeLists.txt` with one
`add_subdirectory(<member>)` per member in topological
order. See
[`musto/src/cmake_gen.cppm`](../../../musto/musto/src/cmake_gen.cppm)
`emit_workspace_toplevel()` (lines 386–426) and
[`musto/src/cli.cppm`](../../../musto/musto/src/cli.cppm)
build dispatch (lines 920–1010).

**Umbrella shape (~13 lines):**

```cmake
cmake_minimum_required(VERSION 3.30)
set(CMAKE_EXPERIMENTAL_CXX_IMPORT_STD "...")
project(_musto_workspace LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS ON)
set(CMAKE_CXX_MODULE_STD ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

add_subdirectory(cstd)
add_subdirectory(hello-cstd)
```

The driver issues **one** `cmake -G Ninja -S <umbrella-dir>
-B <umbrella-dir>` and **one** `cmake --build <umbrella-dir>`.
Inter-crate parallelism is identical to cust v0.4.2's flat-
single shape because Ninja walks the `add_subdirectory`
cascade into one graph; configure cost is identical because
CMake processes the subdirs in-process during the one
configure invocation.

### Implication for V42D-13

The rejection of option B in V42D-13 was originally framed
as "worst of both worlds" (configure overhead + `cust build
-p` complications + loses single-file inspectability).
Musto's shipped experience falsifies the first two:

* **Configure overhead is identical**, not multiplied.
  `add_subdirectory` processing happens inside the one
  `cmake -G Ninja` invocation, not in N separate ones.
* **`cust build -p <member>` is unaffected.** Lowers to
  `cmake --build --target <member>` (musto does exactly
  this — `cli.cppm` line 1034) regardless of whether the
  target was declared in a flat CMakeLists or via
  `add_subdirectory`.

The remaining trade is real but smaller than V42D-13's
original prose suggested:

| | Flat single (V42D-13 locked) | Per-member + umbrella (musto) |
|---|---|---|
| Emitter functions | 1 | 2 (`emit()` + `emit_workspace_toplevel()`) |
| Golden files | 1 | N + 1 |
| Single file to grep | Yes | No (must traverse subdirs) |
| File-system shape mirrors source tree | No | Yes |
| Inter-crate parallelism | One Ninja graph | One Ninja graph |
| Configure cost | One | One |

v0.4.2.md V42D-13 was updated 2026-06-11 to soften the
option B rejection prose to reflect this. The flat-single
lock stands on emitter simplicity (one function, one golden
file) and on the "one .cmake file to read when debugging"
property, but musto's experience validates option B as a
defensible alternative if cust ever surfaces a cost option C
hasn't.

---

## 3. Stamp the canonicalised manifest, not the generated CMakeLists

**Relevant cust decision:** [V42D-8](v0.4.2.md)
(configure-skip stamp, SHA-256 of generated CMakeLists).

**Musto's approach:** hash the *canonicalised* `Musto.toml`,
not the generated CMakeLists. See
[`musto/src/cli.cppm`](../../../musto/musto/src/cli.cppm)
build path:

```cpp
auto canon      = musto::manifest::canonicalise(toml_text);
auto canon_hash = musto::paths::sha256_hex(canon);
bool need_configure = !musto::paths::stamp_matches(stamp, canon_hash);
```

`canonicalise()` lives in
[`musto/src/manifest.cppm`](../../../musto/musto/src/manifest.cppm)
(line 1442): re-parses the TOML through toml++ and re-emits
with sorted keys before hashing.

### Trade

| | Hash generated CMakeLists (V42D-8) | Hash canonicalised TOML (musto) |
|---|---|---|
| Implementation cost | Trivial (we generate the file anyway) | Needs a canonical TOML serialiser |
| Version-bump churn | Spurious reconfigure on every cust version (the `@generated by cust vX.Y.Z` string changes) | None — TOML doesn't carry the cust version |
| Catches changes in non-manifest inputs | Yes (plugin path, module set, cflag set — anything that affects the generated file) | No — needs a separate stamp on plugin path / module set |
| Reused for other purposes | No | Yes (cargo's `Cargo.lock` model — hash the canonicalised manifest, store in lockfile) |

### Recommendation for cust

**v0.4.2 sticks with V42D-8 as locked** (hash CMakeLists)
because we don't have a canonical TOML serialiser yet and
the spurious-reconfigure cost on version bumps is
negligible.

**v0.4.6 (registry / `Cust.lock` source hashes) is the
natural point to reconsider.** That milestone needs a
canonical TOML serialiser anyway for lockfile hashing; once
it exists, switching V42D-8's stamp from "hash generated
CMakeLists" to "hash canonicalised TOML + plugin path +
module list" becomes mechanical. This is now recorded in
V42D-8's "Considered alternatives" paragraph and in §10
below.

---

## 4. Diagnostic rewriter — ship it from day one

**Relevant cust decision:** [V42D-18](v0.4.2.md) (planned),
[RQ-V42-2](v0.4.2.md) (partially closed).

**Musto's approach:**
[`musto/src/diag_rewrite.cppm`](../../../musto/musto/src/diag_rewrite.cppm),
~80 LoC, pure function, shipped since v0.1.0. Three classes:

```cpp
enum class source {
    compiler,   // <file>:<line>:<col>: (error|warning|note):
    ninja,      // line starts with "ninja: "
    cmake,      // line starts with "CMake Error" or "CMake Warning"
    other,      // unrecognised
};

source classify(std::string_view line);
std::string rewrite(std::string_view line, std::string_view crate_name);
```

Per-class behaviour:

1. **`compiler`** — verbatim passthrough. Clang/GCC
   diagnostics are already correct and source-relative.
2. **`ninja`** — strip the `ninja: error: ` prefix and
   reformat the missing-input case:
   ```
   error[musto]: missing build input
     --> src/util.cpp.o (needed by `my_crate` staticlib)
   ```
3. **`cmake`** — passthrough today; long-term ambition is to
   point CMake configure errors back at the offending
   `Musto.toml` key (driver knows the mapping per spec §4).
4. **`other`** — verbatim passthrough.

### Wiring

Musto's
[`musto/src/subprocess.cppm`](../../../musto/musto/src/subprocess.cppm)
spawns cmake/ninja via `pipe(2)` + `fork(2)` + `execvp(2)`
on POSIX, drains stdout + stderr line-by-line, and invokes
two `line_sink` callbacks (`on_stdout`, `on_stderr`) per
line. The build dispatch site
([`cli.cppm`](../../../musto/musto/src/cli.cppm) line 615)
plugs `diag_rewrite::rewrite` into the stderr sink:

```cpp
auto cfgr = musto::subprocess::run(
    configure,
    [](std::string_view ln) { std::println("{}", ln); },     // stdout
    [&](std::string_view ln) {                                // stderr
        std::println(std::cerr, "{}",
                     musto::diag_rewrite::rewrite(ln, m.pkg.name));
    });
```

### Implication for V42D-18

Cust v0.4.2 should ship `cust/src/diag_rewrite.rs` in slice
B alongside the CMake/Ninja switchover, **not** as a
follow-up. Three reasons:

1. **No regression by construction.** The
   `compile_one_module` direct-clang path being deleted in
   slice B had no rewriter (didn't need one — clang's output
   was already user-facing). Without a slice-B rewriter,
   every error that surfaces via CMake or Ninja rather than
   directly from clang gets a worse diagnostic than today.
2. **Minimum shape is cheap.** Classifier + verbatim
   passthrough is ~30 LoC. Even with the Ninja missing-input
   rewrite, ≤ 80 LoC and testable as a pure function.
3. **Closes most of RQ-V42-2 by construction.** The open
   question shrinks from "do we need a rewriter at all" to
   "which additional rewrite rules to add" — much
   lower-stakes follow-up.

V42D-18 records the lock. Cust's `subprocess` helper does
not exist today (the direct-clang path uses `std::process`
directly); slice B introduces it. Shape is the same as
musto's: two-callback streaming wrapper around
`Child::stdout`/`Child::stderr`.

---

## 5. Multi-`--target` lowering for `cust build -p <member>`

**Relevant cust decision:** [V42D-13](v0.4.2.md) scope
item 8 (`-p <member>` → `--target <member>`).

**Musto's approach:** `-p <member>` lowers to one
`--target <name>` flag for the library *plus one per
`[[bin]]` plus one per `[[test]]`* the member owns. See
[`cli.cppm`](../../../musto/musto/src/cli.cppm) lines
1034–1052:

```cpp
std::vector<std::string> build_args = {"--build", ws_build.string()};
if (only_idx) {
    auto const& mem = load.members[*only_idx];
    if (has_lib) {
        build_args.push_back("--target");
        build_args.push_back(mem.name);
    }
    for (auto const& bin : mem.mf.bins) {
        build_args.push_back("--target");
        build_args.push_back(bin.name);
    }
    for (auto const& test : mem.mf.tests) {
        build_args.push_back("--target");
        build_args.push_back(test.name);
    }
}
```

### Implication for cust

v0.4.2's `-p <member>` lowering ([V42D-13](v0.4.2.md)
scope item 8) assumes one target per member. That holds
today (cust has one library and at most one bin per crate
in v0.3.1; the bin shares the crate name) but **breaks at
v0.4.4** when multi-bin per crate ships (`src/bin/*.c`,
`[[bin]]` arrays — V31D-3 deferral).

When v0.4.4 lands, lift musto's pattern verbatim: collect
the library target name + every `[[bin]]` + every test
binary into a `build_args` vector, emit a `--target X`
pair per entry. Drop a note in v0.4.4's design doc when it
opens.

---

## 6. Synthetic members for external path deps

**Relevant cust slot:** v0.4.6 (dependency resolver +
registry — currently a single bullet in §17, no design doc
yet).

**Musto's approach (v0.4.0):** crates outside the workspace
tree reached via `[dependencies] foo = { path = "../bar" }`
become **synthetic members** in the internal
`workspace_view`. Each gets:

* A subdirectory at
  `target/<profile>/.build/@external/<name>/` (the `@`
  prefix is legal in both filesystems and CMake
  `add_subdirectory` paths, and sorts before alphanumerics).
* An entry in the topological build order via the same
  3-colour DFS used for first-party members.
* A CMakeLists generated by the same emitter with
  `is_synthetic = true` to suppress per-bin / per-test
  blocks (synthetic members are library-only at emission
  time; external `[[bin]]` / `[[test]]` are ignored).
* A manifest hash folded into the workspace stamp so
  edits to the external crate's `Musto.toml` trigger
  reconfigure.

See [`musto/src/workspace.cppm`](../../../musto/musto/src/workspace.cppm)
for the resolution machinery and the
`is_synthetic` flag plumbing in
[`musto/src/cmake_gen.cppm`](../../../musto/musto/src/cmake_gen.cppm)
(lines 41–47, 250–260).

### Implication for cust

When v0.4.6 (registry + dependency resolver) opens, the
`@external/<name>/` naming convention is worth lifting
verbatim. Specifically:

* **Storage layout for resolved deps:**
  `target/<profile>/cmake/@external/<crate>/` for the
  per-dep generated CMakeLists (if we revisit V42D-13's
  flat-single lock for option B), or **embed external
  crates as further `add_library` entries in the workspace
  CMakeLists with a comment marker** if we stay flat.
* **`is_external` / `is_synthetic` flag on emitter input:**
  suppresses any `[[bin]]` blocks an external crate
  declares; the consumer never wants to build a library
  crate's accidental bin.
* **Stamp inclusion:** external crate manifest hashes fold
  into the workspace configure-skip stamp.
* **Ambiguity rejection:** musto rejects
  `[dependencies] foo = { path = "<workspace-root>" }`
  because the root isn't itself a member. Cust should
  reject the equivalent.

Not a v0.4.2 concern but worth recording so the convention
is consistent when registry work lands.

---

## 7. Feature defines need re-emission per target

**Relevant cust slot:** v0.7+ (`[features]` plumbing —
currently in §17 OQ-N, no design doc yet).

**The gotcha:** CMake's `target_compile_definitions(<lib>
PRIVATE FOO=1)` does NOT propagate `FOO=1` to bins that
PRIVATE-link to `<lib>`. The library's PRIVATE defs apply
only to TUs inside the library target.

**Musto's solution** ([`cmake_gen.cppm`](../../../musto/musto/src/cmake_gen.cppm)
lines 255–266 and 309–319): re-emit the active feature
set on **every** `add_executable` block (bins + tests),
plus the library block:

```cmake
add_library(my_crate STATIC ...)
target_compile_definitions(my_crate PRIVATE
    MUSTO_FEATURE_json=1
    MUSTO_FEATURE_simd=1)

add_executable(my_bin src/main.cpp)
target_link_libraries(my_bin PRIVATE my_crate)
target_compile_definitions(my_bin PRIVATE       # ← re-emitted, not propagated
    MUSTO_FEATURE_json=1
    MUSTO_FEATURE_simd=1)
```

The alternative — using `PUBLIC` or `INTERFACE` on the
library's defs — leaks the defines onto every consumer
including downstream crates, which is wrong for crate-local
feature flags.

### Implication for cust

When `[features]` lands (v0.7?), `cmake_emit.rs` will need
the same per-target re-emission. Bake it into the
`generate()` template from day one rather than discovering
it from a "why isn't `#ifdef CUST_FEATURE_X` working in the
bin" bug report. Worth a sentence in v0.7's design doc
opening — link to this section.

---

## 8. `check` subcommand bypassing CMake — validated

**Relevant cust decision:** [V42D-15](v0.4.2.md) (`cust
check` runs surface pass only, bypasses CMake entirely).

**Musto's approach:**
[`cli.cppm`](../../../musto/musto/src/cli.cppm) lines
520–559. `musto check` takes a "fast path" for single-file
crates that bypasses CMake and invokes `c++
-fsyntax-only -fmodules -std=c++23 <flags> src/lib.cppm`
directly:

```cpp
if (subcommand == "check" && can_use_fast_check(m, *root, *srcs)) {
    // Honor $CXX so the check uses the same compiler the build will.
    char const* cxx_env = std::getenv("CXX");
    std::string cxx = (cxx_env && *cxx_env) ? cxx_env : "c++";
    std::vector<std::string> args = {
        "-std=c++23", "-fsyntax-only", "-Wall", "-Wextra"};
    // ... module flags, feature defines, extra cxxflags ...
    args.push_back(m.lib.path);
    // ... invoke and rewrite diagnostics ...
}
// `musto check` for multi-file crates falls through to the build
// path below — CMake produces the full module graph and reports
// errors at the right source locations (v0.2 §7).
```

Multi-file crates fall through to the CMake path because
C++23 module BMI assembly is CMake/Ninja's job (the scanner
needs to see every TU to resolve `import` edges).

### Implication for V42D-15

Cust is in a *better* position than musto here: the
fragment-header assembly cust's plugin does (analogous to
musto's BMI assembly) lives in the driver, not CMake. So
cust's `cust check` can cover **every** case — single- and
multi-file, single-crate and workspace — without falling
through to CMake. V42D-15 already locks this; musto's
shipped experience confirms the boundary is sensible (they
just don't get to draw it in the same place).

---

## 9. Things cust deliberately doesn't take from musto

Recording these to forestall future "why don't we just do
what musto does" questions:

1. **Single global `CMAKE_EXPERIMENTAL_*` UUID gate.** Musto
   pins the C++ modules UUID at the top of every generated
   CMakeLists. Cust has nothing equivalent — clang plugin
   loading isn't a CMake experimental feature.
2. **`[[fetch]]` source acquisition.** Musto's wrapper-crate
   pattern (declare an upstream source URL, materialise into
   `$MUSTO_HOME/git/<slot>/`, copy into the build tree) is
   their answer to "how do I depend on a non-musto upstream
   library." Cust's equivalent is the v0.4.1 deferred FFI
   work (`cust bindgen` + `links =`); the cust answer is
   "wrap the system header at the C ABI boundary," not
   "build the upstream source from scratch." Different
   target domain, different solution.
3. **CMake 4.x pinning + UUID rotation handling.** Musto
   pins CMake 4.2.3 *exactly* because the
   `CMAKE_EXPERIMENTAL_CXX_IMPORT_STD` UUID rotates per
   CMake release. Cust uses `cmake_minimum_required(VERSION
   3.21)` and accepts any CMake ≥ 3.21 — V42D-9. We
   target a stable CMake feature surface; no per-release
   pinning needed.
4. **Driver self-hosting** (musto's driver is C++23 modules
   built by musto itself). Cust's driver is Rust per
   [implementation-language.md](implementation-language.md);
   self-hosting cust would mean writing the driver in C,
   which is a different style of decision.
5. **macOS / Windows-first ambitions.** Musto's roadmap
   tracks both. Cust is Linux-first per §17 ground rules;
   macOS/Windows are post-v1.

---

## 10. Summary table — what to take, when

| Idea | Cust v0.4.2 | Cust later | Source in musto |
|---|---|---|---|
| CMakeLists-as-output, CMake-as-driver | ✅ V42D-1 .. V42D-17 | — | Whole repo |
| Per-member CMakeLists with umbrella | ❌ rejected, V42D-13 picks flat-single | Migration C → B documented if real cost surfaces | `cmake_gen.cppm` `emit_workspace_toplevel()` |
| Canonical-TOML stamp hashing | ❌ V42D-8 picks CMakeLists hash | ✅ v0.4.6 (when canonical TOML serialiser exists for `Cust.lock`) | `manifest.cppm` `canonicalise()` + `paths.cppm` `sha256_hex` |
| Diagnostic rewriter | ✅ V42D-18 (slice B) | Grow rewrite rules as RQ-V42-2 surfaces them | `diag_rewrite.cppm` |
| Multi-`--target` for `-p <member>` | ❌ deferred (only one bin per crate today) | ✅ v0.4.4 (multi-bin) | `cli.cppm` lines 1034–1052 |
| `@external/<name>` synthetic-member convention | ❌ no external deps yet | ✅ v0.4.6 (registry / external path deps) | `workspace.cppm` + `cmake_gen.cppm` `is_synthetic` |
| Per-target feature-define re-emission | ❌ no features yet | ✅ v0.7+ (`[features]` plumbing) | `cmake_gen.cppm` lines 255–266, 309–319 |
| `check` bypassing CMake | ✅ V42D-15 (covers more cases than musto) | — | `cli.cppm` lines 520–559 |
| Two-callback streaming subprocess wrapper | ✅ V42D-18 implicitly needs one | Generalise as `cust/src/subprocess.rs` | `subprocess.cppm` |

---

## 11. Reading-order recommendations

If you're touching:

* **v0.4.2 emitter (`cmake_emit.rs`):** read
  [`musto/src/cmake_gen.cppm`](../../../musto/musto/src/cmake_gen.cppm)
  end-to-end (~426 lines). Pure-function emitter shape, the
  exact dispatch pattern v0.4.2's emitter will use.
* **v0.4.2 diag rewriter (`diag_rewrite.rs`, V42D-18):**
  read [`musto/src/diag_rewrite.cppm`](../../../musto/musto/src/diag_rewrite.cppm)
  (~80 lines). One classifier, three rewriters, golden-file
  tested.
* **v0.4.2 subprocess wiring:** read
  [`musto/src/subprocess.cppm`](../../../musto/musto/src/subprocess.cppm)
  for the POSIX `pipe(2)`/`fork(2)`/`select(2)` shape with
  per-line stdout+stderr callbacks. Rust's equivalent is
  `std::process::Child` + a `BufReader::lines()` over each
  pipe; the design intent (two callbacks, line-oriented) is
  what to copy.
* **v0.4.6 dependency resolver / external deps:** read
  [`musto/src/workspace.cppm`](../../../musto/musto/src/workspace.cppm)
  for the synthetic-member machinery and the 3-colour DFS
  cycle-detection pattern.
* **v0.7+ `[features]` plumbing:** read
  [`musto/src/cmake_gen.cppm`](../../../musto/musto/src/cmake_gen.cppm)
  lines 255–266 and 309–319 specifically.

---

## 12. Provenance

This doc was assembled 2026-06-11 by walking the musto
repository (`/home/user1/code/musto/`) at HEAD after a fresh
checkout, with no musto-side changes. Section citations
reference line numbers stable at that revision; rebase
against musto's current HEAD before applying any code
patterns verbatim — the project ships regularly and the line
numbers will drift.

The "Implication for cust" subsections are the author's
recommendations after reading musto's design docs
(`docs/design/musto.md` + the per-milestone archive under
`docs/design/archive/`) alongside the implementation. The
"what we don't take" list in §9 is conservative — musto's
choices are good for musto, not necessarily for cust.
