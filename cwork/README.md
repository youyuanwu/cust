# cwork

In-tree cust workspace that hosts everything cust-built in this repo:
the shared library crates the rest of the ecosystem depends on, plus
the examples that consume them.

Layout:

```
cwork/
├── Cust.toml      # [workspace] members = ["ccrates/cstd", "examples/hello-cstd", ...]
├── Cust.lock      # v1, regenerated on every build (committed)
├── ccrates/       # shared library crates (cust's core/std analogue)
│   └── cstd/      # foundational crate
└── examples/      # bin-crate samples consuming ccrates/
    └── hello-cstd/
```

## ccrates

The shared library crates other cust projects build on top of —
analogous to Rust's `core`/`std` (but much smaller, and assembled
milestone-by-milestone alongside the driver itself).

Current members:

- [`ccrates/cstd`](ccrates/cstd/) — the foundational crate. Tiny
  today (a couple of obvious primitives gated behind `cust_pub`);
  grows as new cust features land.

## examples

Bin crates that demonstrate consuming `ccrates/` members. These
are the first real `cust run`-able workloads in the repo and
act as a sanity sweep across the v0.3.1 bin-crate work
(workspace path-deps, `#cust use <dep>;`, link step).

Current members:

- [`examples/hello-cstd`](examples/hello-cstd/) — bin crate that
  `#cust use cstd;` and exercises a handful of `cstd_*` functions.
  Run from the `cwork/` root with:

  ```sh
  ../target/debug/cust run -p hello-cstd
  ```

## Building

```sh
# from the cust repo root
cargo build --bin cust
(cd cwork && ../target/debug/cust build)             # build every member
(cd cwork && ../target/debug/cust run -p hello-cstd) # run an example
```

The shape mirrors the v0.3 + v0.3.1 verification fixtures: a
top-level `Cust.toml` declares `[workspace] members` (with
nested paths like `ccrates/cstd` and `examples/hello-cstd`),
each member is its own crate, and path deps between members are
resolved against the workspace list.

`target/` is gitignored. `Cust.lock` is **not** gitignored — once
external (registry / git) deps land in v0.4 it will pin source hashes
and should be committed for reproducibility. For the path-only v0.3
form it's still cheap to keep in version control and confirms the
contract from [`docs/spec/cust-lock.md`](../docs/spec/cust-lock.md).
