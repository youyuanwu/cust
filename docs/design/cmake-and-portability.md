# CMake interop & compiler portability

Status: **⚠️ SUPERSEDED on the CMake-as-driver question** by
[v0.4.2.md](v0.4.2.md) (2026‑06‑11). The "CMake is never a
build‑time dependency of cust" rule in §1 / §2.2 / §2.3 / §5 is
**reversed**: starting v0.4.2, cust generates a `CMakeLists.txt`
into the build tree and shells out to CMake + Ninja as the
codegen / link / dep‑tracking backend. See [v0.4.2.md](v0.4.2.md)
for the reversal rationale, the headline counterarguments
addressed (configure latency, two‑sources‑of‑truth risk,
plugin/bitcode glue, diagnostics layering), and the new
boundary between the Rust driver and the generated CMake.

The rest of this doc — §3 (`cust export cmake` for *consumers*),
§4 (GCC support is still a non‑goal), §5 rows 3–5, §6 open
questions, §7 risks — stands unchanged. The export story and
the multi‑compiler discussion were never coupled to the
"CMake‑as‑driver: no" decision; only that one decision flipped.

Original doc preserved below for the historical reasoning record.

Status (original): **design sketch / brainstorm**, 2026‑06‑03
Owner: TBD
Companion to: [cust-design.md](cust-design.md)

This doc collects everything cust has to say about CMake and about
non‑clang compilers (specifically: should we ever support GCC?). Both
topics share the same underlying question — *should cust depend on, or
generate, a meta‑build / multi‑compiler abstraction?* — so they live
together.

---

## 1. TL;DR

* **CMake is never a build‑time dependency of cust.** Not as the
  driver, not as a transient intermediate in `target/`.
* **CMake is a first‑class *export* format** for downstream consumers
  (`cust export cmake --consumable` / `--standalone`). That's where it
  earns its keep — the C/C++ interop story Cargo never solved.
* **GCC support is a non‑goal for v1** and a deliberate non‑goal we
  recommit to in v2+. The features that make cust *cust* (clang plugin,
  annotated‑attribute survival, ThinLTO bitcode rlibs) have no GCC
  equivalent. Adopting CMake on the speculation we might add GCC later
  is paying a real cost today for a fake benefit later.
* If GCC support ever becomes a serious goal, the right preparation is
  **a `Backend` abstraction inside cust**, not a build‑system layer
  underneath it.

The general principle: a meta‑build system (CMake) abstracts over many
build tools and many compilers. Cust commits to *one* build tool
(Ninja or our own scheduler) and *one* compiler (clang). A meta‑layer
over a single concrete pair is pure overhead.

---

## 2. CMake — sibling, not parent

A recurring question: should cust *use* CMake under the hood, or
generate CMake files, or ignore it entirely? The answer is "generate
for export, never depend on."

### 2.1 Where CMake would help

1. **Interop with the existing C/C++ ecosystem.** The biggest single
   win. Emitting a `<crate>Config.cmake` + `<crate>Targets.cmake`
   bundle alongside the static/shared lib means any CMake project
   consumes a cust crate with `find_package(my_crate REQUIRED)`. That
   is the friction Cargo never solved for C/C++ consumers; we can.
2. **IDE reach.** CLion, Visual Studio, Qt Creator, embedded vendor
   IDEs, and Bazel/Buck importers all speak CMake. Generating a
   `CMakeLists.txt` gets project import in those tools for free.
3. **Cross‑compilation toolchain files.** vcpkg, Yocto, Zephyr,
   ESP‑IDF, Android NDK — all of them ship CMake toolchain files. For
   exotic targets, leaning on that ecosystem is cheaper than rebuilding
   it inside cust.
4. **Install / packaging.** `install(TARGETS …)`, CPack, pkg‑config
   generation, the whole `<crate>Config.cmake` package‑config pattern
   are mature. We can ride them rather than reinvent.
5. **`find_package` for system deps.** OpenSSL, zlib, libcurl,
   threads — battle‑tested `FindXxx.cmake` modules already exist.

### 2.2 Where CMake would hurt if it were the driver

1. **Two sources of truth.** If `Cust.toml` is canonical and
   `CMakeLists.txt` is generated, fine — but the moment users hand‑edit
   the generated file we have Autotools‑era pain back. Cargo learnt
   this lesson; `build.rs` is *code*, not generated Makefile fragments.
2. **Loss of control over the build graph.** Using CMake as the driver
   means we hand off incremental compilation, dep tracking,
   parallelism, and caching. That works, but:
   * The ThinLTO bitcode pipeline (see cust‑design.md §7) becomes
     awkward — CMake assumes `.o` files, not `.bc` rlibs.
   * `#cust mod` preprocessing, plugin invocation, and the
     section‑based test harness all become `add_custom_command` glue.
     Brittle, hard to debug.
   * Diagnostics get worse: a plugin error surfaces as a Ninja line
     buried under three layers of CMake script.
3. **Slow configure step.** Cargo's edit→build loop feels instant;
   CMake re‑globs, re‑runs `try_compile`, re‑expands generator
   expressions on every reconfigure. For inner‑loop UX this matters.
4. **CMake DSL is not small.** Generator expressions, `target_*` vs
   directory‑level commands, `INTERFACE`/`PUBLIC`/`PRIVATE`, policy
   `cmake_policy(SET CMP….)` — exposing users to all of that the moment
   anything goes wrong defeats the "one TOML file" promise.
5. **Plugin invocation is verbose.** `target_compile_options(... PRIVATE
   -fplugin=$<TARGET_FILE:cust_plugin> ...)` repeated everywhere, plus
   generator‑expression escaping issues, is a steady tax.

### 2.3 What about CMake as a *transient* in `target/`?

A natural fallback: never check in `CMakeLists.txt`, never expose it
to the user, but have cust *generate* one into `target/.build/` at
build time and invoke it. Same posture for the generated header — it
lives in `target/<profile>/include/`, regenerated each build.

This sounds appealing (offload scheduling to a mature tool!) but it
doesn't earn its keep:

* **CMake's remaining job, once we strip dependency tracking, package
  discovery, and toolchain abstraction, is "generate a Ninja file."**
  We can generate one directly. Ninja's file format is small — the
  spec is ~10 directives. A *minimal* emitter (one rule per cflag
  set, `clang -c` invocations) is a few hundred lines. A
  *production* emitter that also handles depfile parsing with escape
  rules, phony intermediate targets, Windows path quoting,
  generated‑header dependency edges, and remote‑build hooks is more
  realistically 1000–2000 lines with tests. Still strictly less code
  than wrapping CMake plus owning its quirks, but the v0.6 milestone
  needs to budget for the production version.
* **We already have the data structure.** Cust must build an in‑memory
  compile graph anyway — for `compile_commands.json`, `cust check`,
  the plugin pipeline, ThinLTO bitcode flow. That graph is one
  `serialize()` call from a `build.ninja` file. Routing it through
  CMake means serialising it twice and parsing it once in between.
* **Configure cost.** Even a tiny generated `CMakeLists.txt` costs
  0.5–2 s to re‑configure (compiler probes, cache writes, source
  globs). Direct Ninja emission skips that — we already know
  everything about clang because we vendored it.
* **Plugin & bitcode plumbing fights CMake's grain.** `-fplugin=…`
  per TU, `--emit-llvm` for rlibs, ThinLTO link consuming `.bc`,
  the `__cust_tests` section discovery — each is an
  `add_custom_command` exception in CMake but two lines of a Ninja
  rule.
* **Diagnostics get longer.** `clang error → ninja line → CMake script
  line → user`. Direct invocation puts clang's output in front of the
  user verbatim, the way Cargo prints rustc errors.
* **CMake becomes a required runtime dep of cust** (and a moving
  target across versions / `cmake_minimum_required` drift). Ninja is
  a ~200 KB single binary we vendor in `~/.cust/toolchain/`.

### 2.4 Execution plan (no CMake in the hot path)

* Build graph lives in memory; canonical for everything.
* Small builds: cust drives clang directly from a worker pool. Faster
  than `fork+exec(ninja)` for tiny graphs.
* Larger builds: emit `target/.build/build.ninja`, then
  `exec ninja -C target/.build`. Same depfile‑based incrementality
  CMake would have given us, none of the configure overhead.
* Generated header lives at `target/<profile>/include/<crate>.h` and
  is regenerated each build; never checked in. Consumers who want it
  stable get it via `cust export cmake --consumable` (which copies
  header + cmake config to a stable export dir) or `cust publish`
  (which seals it into the distributed crate artifact).

---

## 3. `cust export cmake` — the export story

Two modes, both *output formats*, never the driver:

```
cust export cmake [--consumable | --standalone] [--out <dir>]
```

### 3.1 `--consumable` (default, high‑value)

Emit *just* the package‑config artifacts so other CMake projects can
find and link this crate:

```
target/<profile>/cmake/
  my_crateConfig.cmake          # find_package entry point
  my_crateConfigVersion.cmake
  my_crateTargets.cmake         # IMPORTED targets, ABI surface
  my_crateTargets-<profile>.cmake
  pkgconfig/my_crate.pc         # bonus: pkg-config for autotools users
```

The actual `.a` / `.so` is still built by `cust build`. This mode is
the high‑value one and should land early (v0.4 or v0.5).

### 3.2 `--standalone` (degraded, opt‑in)

Emit a full `CMakeLists.txt` that can build the crate end‑to‑end
**without** cust installed. Useful for shipping source drops to
locked‑down environments (embedded vendors, air‑gapped CI). We accept
degraded features:

* no ThinLTO across cust deps,
* plugin checks reduced to whatever portable attrs survive,
* generated header must be pre‑shipped rather than re‑synthesised.

Document this trade clearly so nobody mistakes it for the canonical
build.

### 3.3 Other build‑system targets, same shape

* `cust export ninja` — emit `build.ninja` we wrote ourselves; skip
  CMake entirely. Smaller, faster, full control. Likely *more* useful
  than CMake export for cust‑internal needs (distributed builds,
  remote caching).
* `cust export bazel` / `cust export buck2` — community plugins; not
  core.
* `cust export compile_commands` — already emitted automatically; this
  command just copies it somewhere stable for tools that want it
  outside `target/`.

### 3.4 Implications for the rest of the design

* `Cust.toml` stays the only source of truth; never round‑trip from a
  hand‑edited `CMakeLists.txt`.
* The `[[cust::link(...)]]` attribute and the build‑script
  `cust:cust-link-lib=` line need to carry enough metadata
  (`kind`, `framework`, `whole-archive`) to round‑trip into
  `target_link_libraries(... PUBLIC ...)`.
* **Generated `<crate>Config.cmake` validates features at
  `find_package` time.** Each cust feature is exposed as a CMake
  package `COMPONENT`. The exporter reads the `features` array from
  the rlib's `metadata.json` (cust‑design.md §7) and bakes the list
  into the config file:

  ```cmake
  # my_crateConfig.cmake (generated)
  set(_MY_CRATE_AVAILABLE_FEATURES json simd)
  foreach(_comp ${my_crate_FIND_COMPONENTS})
    if(NOT _comp IN_LIST _MY_CRATE_AVAILABLE_FEATURES)
      set(my_crate_FOUND FALSE)
      set(my_crate_NOT_FOUND_MESSAGE
          "crate `my_crate` was built with features
           [${_MY_CRATE_AVAILABLE_FEATURES}]. 
           Component `${_comp}` is not available.")
      return()
    endif()
  endforeach()
  ```

  Semantics: `COMPONENTS` is conjunctive. `find_package(my_crate
  REQUIRED COMPONENTS json simd)` requires both `json` and `simd`
  to have been enabled in the cust build; absence triggers CMake's
  standard `REQUIRED`‑not‑found path with a clear diagnostic
  pointing at the consumer rather than producing a silent link‑time
  failure.
* If the consumer requests *no* components, the find succeeds and
  exposes whichever features the producer enabled — same as Cargo's
  default‑features semantics for unqualified dependencies.

---

## 4. Does CMake make future GCC support easier?

A secondary motivation often raised for adopting CMake is "it'll make
adding GCC easier later." Short answer: **marginally, and only for the
parts that were already easy.** The hard parts of GCC support are
language‑feature and plugin gaps; CMake does nothing for those.

### 4.1 What CMake actually buys for multi‑compiler support

CMake's compiler abstraction covers, roughly:

* Locating the compiler (`CMAKE_C_COMPILER`).
* Per‑compiler flag dispatch via generator expressions:
  `$<$<C_COMPILER_ID:GNU>:-Wfoo>` vs `$<$<C_COMPILER_ID:Clang>:-Wbar>`.
* Knowing which warning flags / sanitizer spellings / LTO flags each
  compiler accepts.
* Toolchain files (`-DCMAKE_TOOLCHAIN_FILE=…`) for cross‑compilers.

That is real work, and CMake does it well. But notice the shape: it's
all **command‑line flag plumbing**. It is the *easy* part of supporting
a second compiler.

### 4.2 What's hard about GCC support — and what CMake doesn't help with

The cust design leans on clang for things that have no GCC equivalent:

| Cust feature | Clang | GCC | CMake helps? |
|---|---|---|---|
| `libcust_plugin.so` via `-fplugin=…` (`PluginASTAction`) | Stable C++ AST API | GCC plugins exist but are GPL‑only, totally different API (GIMPLE passes, no AST after parse), no `RecursiveASTVisitor` analogue | **No** |
| `__attribute__((annotate("cust::*")))` surviving into IR | Yes | Not supported — attribute doesn't exist | **No** |
| `__attribute__((overloadable))` | Yes | Not supported | **No** |
| Blocks `^{ … }` | Yes (with `-fblocks`) | Not supported | **No** |
| ThinLTO bitcode rlibs (`.bc` files) | LLVM bitcode | GCC has GIMPLE LTO, incompatible format | **No** |
| `-ftime-trace` Chrome traces | Yes | Not supported | **No** |
| `-fsanitize=…` flag spellings | Clang set | Mostly same, with drift (`-fsanitize=memory` is clang‑only) | Trivially |
| Section‑based test discovery (`__cust_tests`) | `__attribute__((section(...)))` | Same | Already portable |
| `-fvisibility=hidden`, `cleanup`, `nonnull`, `format`, `warn_unused_result` | Yes | Yes | Already portable |
| `_Generic`, `_BitInt`, `#embed` (C23) | Yes | Yes (recent versions) | Already portable |
| `compile_commands.json` consumption | Yes | Yes | N/A |

The pattern is stark: the things CMake abstracts (flag spellings) were
already easy. The things that make cust *cust* — the plugin, the
annotated‑attribute survival, the bitcode rlib pipeline — have no GCC
counterpart, and no build system can paper over that.

### 4.3 The deeper problem: a GCC port isn't a build‑system question

Adding GCC support means making three serious architectural decisions:

1. **What to do about the plugin.** Options:
   * (a) write a parallel GCC plugin (GPL‑3, completely different API,
     much weaker AST access);
   * (b) drop plugin features on GCC and reduce `[[cust::*]]`
     attributes to no‑ops plus what GCC enforces natively
     (`warn_unused_result`, `nonnull`, …);
   * (c) ship a `libclang`‑based pre‑pass that processes the source
     for plugin semantics, then hand the cleaned source to GCC.

   Each has large implications and none is a CMake‑shaped problem.

2. **What to do about the rlib format.** Bitcode rlibs are
   LLVM‑specific. For GCC we'd need to either ship `.o`‑based rlibs
   (losing ThinLTO across crates) or maintain two artifact formats
   per dep.

3. **What to do about generated headers.** They're produced by the
   plugin walking the AST. Without the plugin (GCC mode), we need
   either the libclang pre‑pass to keep running, or a separate parser,
   or a "checked in header" requirement on GCC builds.

CMake would help us write
`if(CMAKE_C_COMPILER_ID STREQUAL "GNU") target_compile_options(... -Wno-attributes) endif()`.
That's the *last* 5 % of the work.

### 4.4 What actually makes GCC support easier later

Design choices we'd make *in cust itself*, regardless of build system:

1. **Two‑tier feature set in the prelude.** Tag each `[[cust::*]]`
   attribute as `plugin‑required` / `clang‑native` / `portable`. The
   "portable" subset works on GCC unchanged.
2. **Abstract the compiler driver behind a `Backend` interface** in
   cust's code, with `ClangBackend` as the only impl in v1. Adding
   `GccBackend` later becomes a localised change — no different from
   what CMake's `Compiler*.cmake` files do, but written in our own
   language with our own types and our own error messages.
3. **Keep the rlib format pluggable.** Already implied by `crate-type`;
   we just need a `crate-type = "object-rlib"` fallback that ships
   `.o` instead of `.bc`.
4. **Emit `compile_commands.json` faithfully** — the real cross‑compiler
   bridge. A GCC user who hits a cust feature we haven't ported yet can
   at least drive their own GCC build from the compile DB.

If GCC support ever becomes a serious goal, doing it via a new backend
implementation gives us full control over the trade‑offs (which
features are dropped, how rlibs work, how the plugin is faked or
skipped). Going via CMake forces those decisions to be expressed as
CMake configuration, which is a much worse place to make architectural
choices.

---

## 5. Decision summary

| Question | Answer |
|---|---|
| Use CMake as the build driver? | **No.** Loses control of the build graph, slow configure, brittle plugin/bitcode glue, two sources of truth. |
| Use CMake as a *transient* in `target/`? | **No.** Its remaining job is "emit Ninja" — we can emit Ninja directly from a graph we already maintain. CMake adds configure overhead, an extra runtime dep, and an extra layer in diagnostics. |
| Generate CMake from cust for *consumers*? | **Yes.** `cust export cmake --consumable` is the high‑value mode; `--standalone` is the degraded fallback for source drops. |
| Adopt CMake speculatively to ease future GCC support? | **No.** CMake helps with flag spellings — the easy 5 %. The hard 95 % (plugin, attribute survival, bitcode rlibs) is architectural and lives in cust regardless. |
| What if GCC support does become a goal? | Introduce a `Backend` abstraction inside cust. Keep the prelude tiered (portable / clang‑native / plugin‑required). Add a `crate-type = "object-rlib"` to give up ThinLTO gracefully. None of this requires CMake. |

---

## 6. Open questions

1. **CMake export fidelity.** Which subset of `Cust.toml` round‑trips
   losslessly into `<crate>Config.cmake`? `[features]` and
   `[dependencies]` are easy; `build.cust.c` side effects (generated
   source, custom link flags) are the hard case for `--standalone`
   mode.
2. **pkg‑config first‑class?** Should `cust export pkgconfig` be its
   own command (autotools users, embedded distros) or stay folded
   inside `cust export cmake --consumable`?
3. **Ninja vendoring.** Vendor a pinned Ninja binary in
   `~/.cust/toolchain/`, or require the system one? Pinned is more
   reproducible but adds a per‑platform release artifact.
4. **GCC "portable mode" demand‑driven.** Do we ever ship a
   `cust build --portable` switch that *only* emits the portable
   attribute subset (no plugin), as a smoke test for the eventual GCC
   port? Low cost; gives us early signal about which features creep
   into "plugin‑required" by accident.

---

## 7. Risks specific to CMake / multi‑compiler

* **CMake users will ask us to support arbitrary CMake idioms in
  `--standalone` export.** Resist; document the supported subset and
  fail loudly on the rest. The `--consumable` path covers 90 % of
  interop need without us climbing inside CMake's DSL.
* **CMake version drift.** Even for exported files, the
  `cmake_minimum_required` pin needs to be conservative enough for
  enterprise consumers but recent enough that we can use `IMPORTED`
  target features. Settle on a single floor (e.g. CMake 3.21) and
  document it.
* **"Just add GCC, it can't be that hard."** Periodic pressure from
  users. Standing answer: GCC support is an architectural commitment
  worth several months of work and an ongoing maintenance tax; the
  escape hatch is the `compile_commands.json` we always emit, which
  lets a third party drive any compiler they like.
* **Generator‑expression escaping bugs in `--standalone` output.**
  Cust must produce CMake source that's robust under spaces in paths,
  `;`‑separated lists, and Windows path semantics. Add a CMake‑side
  smoke test that builds each release's exported `--standalone`
  template on Linux/macOS/Windows.
