//! Build helper for the cust clang plugin.
//!
//! Invoked explicitly: `cargo run -p plugin-build`. We deliberately
//! do NOT run this from a `build.rs` (per V2D-2 in
//! `docs/design/v0.2.md`) — coupling the C++ plugin build to every
//! `cargo build` would slow the Rust inner loop for no benefit and
//! put `CMake` on the user-facing hot path.
//!
//! Defaults assume a Debian-family layout under `/usr/lib/llvm-21`.
//! Override with:
//!
//!   `CUST_LLVM_PREFIX`         — passed as `-DCMAKE_PREFIX_PATH`.
//!   `CUST_PLUGIN_BUILD_DIR`    — out-of-source build directory.
//!   `CUST_PLUGIN_PROFILE`      — `debug` (default) or `release`;
//!                                placement under `target/<profile>/`
//!                                matches `cust build`'s layout.
//!
//! On success, the plugin lands at
//! `target/<profile>/libcust_plugin.so` so the driver picks it up
//! via the same fallback path it uses today (after looking at
//! `$CUST_PLUGIN`).

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

const DEFAULT_LLVM_PREFIX: &str = "/usr/lib/llvm-21";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("plugin-build: error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    // Locate the workspace root by walking up from CARGO_MANIFEST_DIR
    // (the `plugin-build/` crate's own Cargo.toml dir) one level.
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .context("CARGO_MANIFEST_DIR is not set (run via `cargo run -p plugin-build`)")?,
    );
    let workspace_root = manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("no parent for {}", manifest_dir.display()))?
        .to_path_buf();

    let plugin_src = workspace_root.join("plugin");
    if !plugin_src.join("CMakeLists.txt").is_file() {
        bail!(
            "expected `{}/CMakeLists.txt` (plugin source missing)",
            plugin_src.display()
        );
    }

    let profile = env::var("CUST_PLUGIN_PROFILE").unwrap_or_else(|_| "debug".to_string());
    if !matches!(profile.as_str(), "debug" | "release") {
        bail!("CUST_PLUGIN_PROFILE must be `debug` or `release` (got {profile:?})");
    }

    let build_dir =
        env::var("CUST_PLUGIN_BUILD_DIR").map_or_else(|_| plugin_src.join("build"), PathBuf::from);
    fs::create_dir_all(&build_dir)
        .with_context(|| format!("creating `{}`", build_dir.display()))?;

    let llvm_prefix =
        env::var("CUST_LLVM_PREFIX").unwrap_or_else(|_| DEFAULT_LLVM_PREFIX.to_string());

    let cmake_build_type = if profile == "release" {
        "Release"
    } else {
        "Debug"
    };

    // Configure.
    eprintln!(
        "plugin-build: configuring (LLVM prefix = {llvm_prefix}, build dir = {})",
        build_dir.display()
    );
    run_cmd(
        Command::new("cmake")
            .arg("-S")
            .arg(&plugin_src)
            .arg("-B")
            .arg(&build_dir)
            .arg(format!("-DCMAKE_PREFIX_PATH={llvm_prefix}"))
            .arg(format!("-DCMAKE_BUILD_TYPE={cmake_build_type}"))
            // Force clang++ for the plugin so the C++ ABI matches
            // the host clang's. Mixing g++ and clang++ here works
            // most of the time but bites occasionally on
            // std::string / std::vector layout details.
            .arg("-DCMAKE_CXX_COMPILER=clang++"),
        "cmake -S … -B …",
    )?;

    // Build.
    eprintln!("plugin-build: building");
    run_cmd(
        Command::new("cmake").arg("--build").arg(&build_dir),
        "cmake --build …",
    )?;

    // Copy the resulting .so to target/<profile>/ so the driver's
    // default discovery path finds it.
    let built = build_dir.join("libcust_plugin.so");
    if !built.is_file() {
        bail!(
            "build succeeded but `{}` is missing — CMakeLists.txt out of sync?",
            built.display()
        );
    }

    let target_profile_dir = workspace_root.join("target").join(&profile);
    fs::create_dir_all(&target_profile_dir)
        .with_context(|| format!("creating `{}`", target_profile_dir.display()))?;
    let dest = target_profile_dir.join("libcust_plugin.so");

    // `fs::copy` is fine — small file, atomic enough for our
    // single-writer use case.
    fs::copy(&built, &dest)
        .with_context(|| format!("copying `{}` -> `{}`", built.display(), dest.display()))?;

    println!("plugin-build: ok -> {}", dest.display());
    Ok(())
}

fn run_cmd(cmd: &mut Command, label: &str) -> Result<()> {
    let status = cmd
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("spawning `{label}`"))?;
    if !status.success() {
        bail!("`{label}` exited with status {status}");
    }
    Ok(())
}
