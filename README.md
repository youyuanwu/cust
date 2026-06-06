# cust

A Cargo-style build system for C — clang-only, by design.

**Status:** experimental (v0.4.0 shipped; design and CLI both still
churning). Linux-first, MIT-licensed.

## What it is

`cust` is to C what `cargo` is to Rust:

- a `Cust.toml` manifest describes the crate,
- `src/lib.c` / `src/main.c` is the unambiguous crate root,
- `#cust mod foo;` and `#cust use crate::foo;` give C a real module
  system (one TU per module, plugin-generated fragment headers — no
  hand-written `.h` files for intra-crate surface),
- `cust build`, `cust run`, `cust test`, `cust new` do what you'd
  expect; the build cache lives under `target/`.

The module system is implemented with a small clang plugin
(`[[cust::pub]]`, `[[cust::pub(crate)]]`) that runs alongside every
TU and emits forward-decl fragments the driver wires together. See
[docs/design/cust-design.md](docs/design/cust-design.md) for the
canonical design and [docs/design/v0.4.0.md](docs/design/v0.4.0.md)
for what's currently shipped.

## Repo layout

```
cust/           # the driver (Rust binary: `cust`)
plugin/         # the clang plugin (C++, loaded via -fplugin=)
plugin-build/   # build helper that locates the plugin .so
cwork/          # in-tree dogfood workspace
  ccrates/cstd/   #   foundational ccrate (cust's core/std analogue)
  examples/       #   `cust run -p hello-cstd`, etc.
docs/design/    # design docs (cust-design.md + per-milestone v0.N.md)
```

## Build & try it

Requires a recent clang/LLVM with plugin support and a Rust
toolchain pinned by [rust-toolchain.toml](rust-toolchain.toml).

```sh
# build driver + plugin
cargo build --bin cust

# build every member of the in-tree workspace
(cd cwork && ../target/debug/cust build)

# run the smoke example
(cd cwork && ../target/debug/cust run -p hello-cstd)
```

## Tests

```sh
cargo test               # driver unit + integration tests
ctest --test-dir plugin/build   # plugin tests
(cd cwork && ../target/debug/cust test)   # in-tree cstd tests
```

## Non-goals (for now)

- GCC, MSVC, or any non-clang backend.
- Cross-language interop beyond exporting a C ABI.
- Reimplementing libc.

## License

MIT — see [LICENSE](LICENSE).
