# cwork

In-tree cust workspace that hosts everything cust-built in this repo:
the shared library crates the rest of the ecosystem depends on, plus
(soon) the examples.

Layout:

```
cwork/
├── Cust.toml      # [workspace] members = ["ccrates/cstd", ...]
├── Cust.lock      # v1, regenerated on every build (committed)
├── ccrates/       # shared library crates (cust's core/std analogue)
│   └── cstd/      # foundational crate
└── examples/      # (future) sample apps/libs consuming ccrates/
```

## ccrates

The shared library crates other cust projects build on top of —
analogous to Rust's `core`/`std` (but much smaller, and assembled
milestone-by-milestone alongside the driver itself).

Current members:

- [`ccrates/cstd`](ccrates/cstd/) — the foundational crate. Tiny
  today (a couple of obvious primitives gated behind `cust_pub`);
  grows as new cust features land.

## Building

```sh
# from the cust repo root
cargo build --bin cust
(cd cwork && ../target/debug/cust build)
```

The shape mirrors the v0.3 verification fixtures (`workspace_basic`,
`workspace_three`): a top-level `Cust.toml` declares `[workspace]
members` (here with nested paths like `ccrates/cstd`), each member
is a library staticlib, and path deps between members are resolved
against the workspace list.

`target/` is gitignored. `Cust.lock` is **not** gitignored — once
external (registry / git) deps land in v0.4 it will pin source hashes
and should be committed for reproducibility. For the path-only v0.3
form it's still cheap to keep in version control and confirms the
contract from [`docs/spec/cust-lock.md`](../docs/spec/cust-lock.md).
