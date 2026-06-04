# Implementation language for cust

Status: **design sketch / brainstorm**, 2026‑06‑03
Owner: TBD
Companion to: [cust-design.md](cust-design.md)

In which language(s) do we *build* cust itself? Cust is unavoidably a
polyglot project (the clang plugin is forced to be C++; user crates are
C; build scripts are C). The interesting decision is just the driver
binary.

---

## 1. Decision

* **Driver:** Rust (v0–v2). Reconsidered for a parallel C reimpl in
  v3+ once cust can self‑host.
* **Clang plugin:** C++. Not a choice — `clang::PluginASTAction` is a
  C++ API and the only supported way to plug into clang's AST.
* **User code in cust crates:** C (the whole point of the project).
* **Build scripts (`build.cust.c`):** C, compiled by cust itself.

Three languages, three clean responsibilities.

---

## 2. What cust actually has to do

Before picking a language, enumerate the work the driver does. This
list is what we're really choosing an implementation language *for*:

1. Parse TOML (`Cust.toml`, `Cust.lock`).
2. Walk a dependency graph; resolve versions; fetch from a
   registry/git; verify checksums.
3. Drive a build graph: schedule compilations, manage parallelism,
   hash inputs for caching, emit `build.ninja` for larger crates.
4. Spawn clang processes; capture diagnostics; multiplex stderr;
   surface SARIF or human errors.
5. Talk to clang's libraries — `libclang` (C ABI) for the doc
   extractor, `compile_commands.json` round‑trip tests, etc.
6. Read clang depfiles, atomic FS swaps for fragment‑header stamping,
   content‑addressed cache management.
7. A network client for the registry; TLS; HTTP.
8. Optionally sandbox build scripts (seccomp/landlock on Linux; Job
   objects on Windows when we get there).

Notice: clang plugin work isn't in that list. The plugin is its own
process, written in C++ regardless of driver language.

---

## 3. Why Rust for the driver

1. **Cargo is the literal reference design.** We can vendor or study
   battle‑tested crates for everything peripheral: `toml`, `semver`,
   `clap`, `reqwest`/`ureq` + `rustls`, `rayon` for the parallel
   build scheduler, `notify` for `--watch`, atomic fs primitives,
   depfile parsing. Cargo itself is open source; we get to read the
   reference implementation of the design we're copying.
2. **`libclang` bindings exist** (`clang-sys` / `clang` crates) for
   the doc‑comment extractor, `compile_commands.json` consumers, and
   other libclang‑touching surfaces of the driver.
3. **Untrusted input.** Cust parses third‑party manifests, unpacks
   tarballs from a registry, and shells out to compilers driven by
   user‑controlled flags. Memory safety in that pipeline is genuinely
   valuable. Cargo has had supply‑chain CVEs even with the safety
   net; without it we'd ship the same class of bugs the language
   produces.
4. **Concurrency.** The build scheduler is genuinely parallel — a
   thread pool over module compilations, plus async I/O against the
   registry. Rust's `Send`/`Sync` story makes the locking discipline
   tractable; in C it would dominate development time.
5. **Single static binary on every platform.** Same distribution
   model as `rustup` / `cargo`. Users install once and forget; we
   don't have to chase libc compatibility on every distro.
6. **Iteration speed.** A Rust driver is significantly faster to
   evolve than a C one of comparable scope, even accounting for
   Rust's own compile times. The bottleneck for cust development is
   design churn, not driver runtime.

### What we're explicitly *not* copying from Cargo

"Cargo as reference design" means UX and dataflow, not 1:1 semantics.
The underlying languages differ enough that several Cargo decisions
need deliberate adaptation. The non‑trivial divergence points:

| Cargo concept | C / cust analogue | Adaptation |
|---|---|---|
| `mod foo;` (module = compilation unit + namespace) | `#cust mod foo;` (§4) backed by `foo.c` as a separate TU | C has no namespaces; we substitute opaque‑by‑default `[[cust::pub]]` types + per‑crate symbol hiding (§6) to approximate Rust‑style module privacy. |
| `.rmeta` (typed crate metadata) | per‑module `.cust.h` fragment header (text) | Fragment headers are *generated C source*, not typed IR. Byte‑identity stamping replaces typed equivalence; cf. cust‑design.md §4. |
| Per‑function incremental compilation (rustc) | Per‑module incremental (cust) | C's lack of crate‑local generics makes per‑function incremental less valuable; we trade it for simplicity. |
| Mangled, crate‑scoped symbol names | C: flat global namespace + linker version scripts | cust adds `-fvisibility=hidden` everywhere + a generated version script per crate (§6). `[[cust::pub(crate)]]` is implemented at link time, not at compile time. |
| Monomorphisation (generics) | None (C has no generics) | `_Generic`, `[[cust::derive]]`, and code generators in `build.cust.c` are the closest equivalents. Don't try to fake parametric generics in v1. |
| `[lib]` / `[[bin]]` shape | Same TOML shape | Open question: see cust‑design.md §3. The shape is borrowed; the *contents* need a separate spec because C link semantics for `[[bin]]` (libc startup, weak symbols, `--as-needed`) diverge from Rust. |
| `panic` / unwinding | `cust_panic` weak symbol, setjmp/longjmp opt‑in | See cust‑design.md §16 OQ‑4. Cargo's unwind machinery (drop glue, `catch_unwind`) has no C analogue — we ship the panic *interface* but not the per‑allocation cleanup discipline. |
| Build scripts (`build.rs`) | `build.cust.c` (§12) | Rust scripts run memory‑safe; C scripts get a hang‑protection timeout in v1 and sandboxing as OQ‑10. |

The table is not exhaustive but it captures every place a naive
"Cargo, but for C" rewrite would smuggle in a Rust‑specific assumption.
Any new Cargo‑derived feature added to cust should get a row here
before implementation.

---

## 4. Why not C for v1 — the bootstrap problem

The most attractive alternative is "write cust in cust." Compelling
story, but currently impossible: **we'd be building a C build tool
before we have a C build tool**.

The cust source tree would have to live in CMake or hand‑rolled
Makefiles until self‑hosting is ready, at which point we'd port it.
That's a year of incidental work on infrastructure (TOML parser,
semver, HTTP+TLS, registry protocol, content addressing, parallel
scheduler) before the first cust‑specific feature ships. None of that
work differentiates cust from any other build tool.

Cargo made the same choice for the same reason. It could have been
written in C or C++. It wasn't.

Secondary downsides of C for this specific tool:

* **Untrusted‑input attack surface.** Manifest parsers, tarball
  unpackers, and registry clients in plain C have a long history of
  CVEs (`zip-slip`, malformed‑archive RCEs, semver overflow,
  url‑parser ambiguity). Cust has all of these.
* **No package ecosystem.** Every dep is a vendored snapshot, a git
  submodule, or a build‑script download. We become a curation team
  for our own infrastructure.
* **Concurrency primitives.** A correct work‑stealing scheduler over
  the module build graph is real work in C and largely solved in
  Rust.

---

## 5. Why not C++

Tempting, since the plugin must be C++ — unifying to one language
across driver and plugin would simplify the cust build itself. But:

* C++ gives us no infrastructure libraries we don't already get from
  Rust (and several it gives us *worse*, e.g. package management).
* C++ pulls a larger toolchain dependency surface for users who build
  cust from source than a single static Rust binary does.
* Iteration speed on a project this shape is worse than Rust.

The cost of crossing one extra language boundary at the
driver ↔ plugin seam is small if we design that seam well (see §7).

---

## 6. Self‑hosting as a deliberate v3+ goal

Self‑hosting matters for credibility ("the C build tool is built with
itself") and for users who can't or won't install a Rust toolchain
(some embedded vendors, locked‑down distro packagers). But it's not a
v1 problem and shouldn't dictate v1 architecture.

When (and if) demand for a C driver materialises, ship `cust-c` as a
separate product behind the same contracts. It would *not* replace
the Rust driver — both implementations coexist, the way `mrustc`
coexists with `rustc`. Some users will prefer one; we ship both.

Likely sequencing:

* **v0–v2:** Rust driver only. Stabilise contracts (see §7).
* **v3 (optional, demand‑driven):** announce `cust-c` as a goal.
  Bootstrap it by `cust build`ing it with the Rust driver. Land it as
  a separate repo / artifact.
* **v4+:** dual maintenance, with the Rust driver remaining
  authoritative for reference behaviour.

---

## 7. Keeping the door open: stabilise contracts, not code

To make a future C reimplementation cheap, treat the *interfaces*
between components as the public surface, not the Rust crates that
happen to implement them today. Concretely:

1. **Stabilise contracts under `docs/spec/`.** Five public contracts,
   each with its own spec doc, version stamp, semver/breaking‑change
   rules, and deprecation window:
   * `docs/spec/rlib-format.md` — tarball schema, `metadata.json`
     fields, bitcode compatibility rules (`rlib_format_version`,
     `llvm_version`; see cust‑design.md §7).
   * `docs/spec/registry-protocol.md` — HTTP endpoints, response
     shape, index format. Versioned via URL path (`/v1/`, `/v2/`).
   * `docs/spec/target-layout.md` — `target/` directory schema with
     a `target/.cust-version` stamp and a documented migration path.
     Two implementations sharing one `target/` must agree on what's
     there.
   * `docs/spec/cust-toml-schema.md` — manifest schema. The `edition`
     field is the user‑facing migration lever (see cust‑design.md
     §16 OQ‑7).
   * `docs/spec/prelude-abi.md` — prelude type layouts, weak symbol
     names (`cust_panic`, `cust_allocator`), and which prelude items
     are part of the stable ABI vs. private.

   Each spec has a Versioning section defining the version field, the
   breakage policy (semver? schema version?), and what an old
   consumer reading a newer artifact does (clear error vs. forward
   compatibility window). Contracts are tracked as cust‑design.md
   §16 OQ‑12; v1‑blocking.
2. **Driver ↔ plugin seam is a C++ ABI, not a language‑neutral IPC.**
   This was claimed otherwise in earlier drafts. The plugin is an
   in‑process `clang::PluginASTAction` (cust‑design.md §10); clang
   loads it as a dynamic library and hands it AST callbacks. There
   is no opportunity to swap in a JSON/msgpack protocol at this seam
   without abandoning the plugin model. A future C driver therefore
   gets the plugin via cross‑language link or ships a parallel C
   plugin invocation path — not via wire protocol.
3. **Keep the Rust codebase architecturally portable.** Avoid leaning
   on async‑specific patterns where a simple thread pool would do;
   avoid heavy proc‑macro dependencies in the core; structure
   modules so they map cleanly to a hypothetical C version. Cheap
   insurance.
4. **Treat `compile_commands.json` as the universal fallback.** It's
   the surface any external tool (clangd, clang‑tidy, a non‑cust
   build) can rely on. Always emit it; never break it.

---

## 8. Implications for the rest of the design

The "Rust driver, in‑process C++ plugin, file/manifest contracts as
the public surface" stance ripples into a few choices made elsewhere:

* The plugin can't be a thin shim that delegates to a Rust helper;
  whatever the plugin does, it does in C++ against clang's AST. Work
  that would naturally live in the driver — e.g. pre‑parse rewriting
  of `[[cust::cfg]]` and `[[cust::feature]]` — stays in the *driver*
  (a pre‑clang pass), not in the plugin (cust‑design.md §9).
* The plugin's outputs are **files** (fragment headers, generated C
  for test registration), **clang diagnostics** (stderr), and
  **AST mutations that survive into clang's codegen** (derive
  companions). All three are observable from outside the plugin
  process; none require an in‑process callback into the driver.
* The on‑disk `target/` layout is part of the public contract, not
  implementation detail.
* `compile_commands.json` is the universal cross‑implementation /
  cross‑tool fallback. Any clang‑based tool can drive a build from
  it.
* Anything that needs *whole‑program* reasoning (e.g. honest
  `[[cust::no_panic]]`) does not fit the per‑TU plugin model and
  either gets downgraded to a per‑TU heuristic or deferred to a
  separate post‑link analysis tool (cust‑design.md §16 OQ‑4).

---

## 9. Open questions

1. **Which Rust MSRV?** Cargo pins a fairly recent stable. We
   probably want something similar (last‑two‑stable) for the same
   reason — access to current features and ecosystem crates — but
   need to balance against distro packaging.
2. **Vendored LLVM only, or system clang allowed?** A Rust driver
   makes vendoring trivial (`rustup`‑style toolchain). System‑clang
   support would simplify packager workflows but complicates plugin
   ABI pinning. Probably: vendored default, `--use-system-clang`
   opt‑in with documented version range.
3. **Plugin IPC: JSON, msgpack, or Cap'n Proto?** JSON for v0 (trivial
   to debug); revisit if profiling shows the boundary is hot.
4. **Async vs threadpool.** `tokio` for the registry client buys
   little if the build scheduler stays threaded. Possibly: `ureq` +
   threadpool everywhere, no async at all. Simpler to keep portable
   to a future C reimpl.
5. **What about a `cust-cpp` driver?** If a future maintainer prefers
   C++ to C for the reimpl (because the plugin is C++ already), the
   same contracts allow that too. Probably never worth doing, but
   worth noting the design doesn't preclude it.
