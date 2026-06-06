# Common dev targets for cust.
#
# Install just: https://github.com/casey/just
# Usage: `just <recipe>`, or `just` to list recipes.

set shell := ["bash", "-cu"]

# Default: show available recipes.
default:
    @just --list

# --- build --------------------------------------------------------

# Build the cust driver (debug).
build:
    cargo build --bin cust

# Build the cust driver (release).
build-release:
    cargo build --release --bin cust

# Build the clang plugin (libcust_plugin.so) via plugin-build/.
plugin:
    cargo run -p plugin-build

# Build everything: driver + plugin.
all: build plugin

# --- test ---------------------------------------------------------

# Run driver unit + integration tests.
test:
    cargo test

# Run plugin ctest suite.
test-plugin:
    ctest --test-dir plugin/build --output-on-failure

# Run in-tree cwork workspace tests (cstd, examples).
test-cwork: build
    cd cwork && ../target/debug/cust test

# Run every test surface.
test-all: test test-plugin test-cwork

# --- run / dogfood -----------------------------------------------

# Build every member of the in-tree workspace.
cwork-build: build
    cd cwork && ../target/debug/cust build

# Run the hello-cstd example.
hello: build
    cd cwork && ../target/debug/cust run -p hello-cstd

# --- lint / format -----------------------------------------------

# Clippy at the workspace's pinned lint level.
clippy:
    cargo clippy --all-targets --workspace -- -D warnings

# Rustfmt check (non-mutating).
fmt-check:
    cargo fmt --all -- --check

# Rustfmt apply.
fmt:
    cargo fmt --all

# --- housekeeping ------------------------------------------------

# Remove build outputs (cargo + cwork target + plugin build dir).
clean:
    cargo clean
    rm -rf cwork/target plugin/build

# Pre-push gate: format + clippy + all tests.
ci: fmt-check clippy test-all
