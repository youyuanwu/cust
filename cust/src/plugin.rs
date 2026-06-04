//! Plugin discovery.
//!
//! Per V2D-2 in `docs/design/v0.2.md`, the cust clang plugin lives
//! at `<cust-install-prefix>/target/<profile>/libcust_plugin.so`
//! after a successful `cargo run -p plugin-build`. The driver
//! locates it via `std::env::current_exe()` — relative to itself,
//! not the user's crate — so a user crate's own `target/` directory
//! doesn't shadow or hide the plugin.
//!
//! Discovery order:
//!
//!   1. `$CUST_PLUGIN` (absolute path, if set and non-empty).
//!   2. `<dir of cust binary>/libcust_plugin.so`.
//!
//! Discovery is **silent** when the plugin is absent: the driver
//! just builds without the plugin. This keeps the v0.2 driver
//! useful even before the plugin is built. Once cross-module
//! imports (`#cust use crate::…;`) actually need the plugin to
//! emit fragment headers, the driver will upgrade "plugin
//! missing" from silent skip to a hard error with a hint pointing
//! at `cargo run -p plugin-build`.

use std::path::PathBuf;

/// A located plugin shared object.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub path: PathBuf,
}

impl Plugin {
    /// Discover the plugin via `$CUST_PLUGIN` or alongside the
    /// `cust` binary itself. Returns `None` if neither is present.
    pub fn discover() -> Option<Self> {
        if let Some(env_path) = std::env::var_os("CUST_PLUGIN") {
            if !env_path.is_empty() {
                let p = PathBuf::from(env_path);
                if p.is_file() {
                    return Some(Self { path: p });
                }
                // `CUST_PLUGIN` set but pointing nowhere: treat as
                // "user opted in but botched the path" — silent skip
                // for v0.2 (the day this becomes load-bearing we'll
                // bail! here instead).
                return None;
            }
        }

        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let candidate = dir.join("libcust_plugin.so");
        if candidate.is_file() {
            return Some(Self { path: candidate });
        }
        None
    }

    /// Flag fragment to add to the clang command line.
    pub fn fplugin_flag(&self) -> String {
        format!("-fplugin={}", self.path.display())
    }
}
