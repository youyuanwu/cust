//! End-to-end tests against the built `cust` binary.
//!
//! This is the crate root for the `cli` integration-test binary. The
//! actual tests are grouped by theme into the sibling modules under
//! `tests/cli/`; shared fixtures-staging and assertion helpers live in
//! `common`. Keeping everything in one test binary (rather than one
//! file per top-level `tests/*.rs`) means a single compile + link for
//! the whole suite.
//!
//! `#[path]` attributes are required because, as a `tests/*.rs` crate
//! root, plain `mod foo;` would resolve to `tests/foo.rs` (a separate
//! test binary) rather than `tests/cli/foo.rs`.

#[path = "cli/common.rs"]
mod common;

#[path = "cli/binaries.rs"]
mod binaries;
#[path = "cli/build_check.rs"]
mod build_check;
#[path = "cli/internal.rs"]
mod internal;
#[path = "cli/manifest.rs"]
mod manifest;
#[path = "cli/modules.rs"]
mod modules;
#[path = "cli/plugin.rs"]
mod plugin;
#[path = "cli/scaffold.rs"]
mod scaffold;
#[path = "cli/test_cmd.rs"]
mod test_cmd;
#[path = "cli/workspace.rs"]
mod workspace;
