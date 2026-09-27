// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The fail-loud resolve check for a `sysroot-bundle` member (§10.1): a bundle whose
//! dynamic loader cannot resolve its libraries aborts the apply instead of reporting a
//! successful install.
//!
//! The install-time relocation that used to live here (re-pointing a `rustup-linked`
//! bundle's `toolchain` link, writing its `rust-toolchain-version`, and the `~/.kani`
//! link wiring) went with the `rustup-linked` reloc policy: the trust toolchain ships
//! `self-contained` bundles, so nothing selected that path.

use std::path::Path;

/// The fail-loud post-apply resolve check (§10.1, `[GREENFIELD]` until now): run
/// an installed toolchain binary with `--version` and require it to RUN TO
/// COMPLETION (a real exit code, not a signal death and not a spawn failure) —
/// which it can only do if the dynamic loader resolved every dependency from the
/// installed bundle. A self-contained bundle whose vendored libs are wired wrong
/// would fail to load here and ABORT the apply, instead of the design's feared
/// "lay down a broken toolchain and report SUCCESS". `bin` is the shim/store
/// path to an exposed compiler (e.g. `trust-mc-compiler`, `trustc`).
///
/// # Errors
/// When the binary cannot be spawned, or is killed by a signal (e.g. the dynamic
/// loader aborts on an unresolved library) rather than exiting normally.
pub fn resolve_check(bin: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let status = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("resolve check: cannot spawn {}: {e}", bin.display()))?;
    // A normal exit (any code) proves the loader resolved the bundle; a signal
    // death (code() == None) is the dyld/ld.so failure we must catch.
    if status.code().is_some() {
        Ok(())
    } else {
        Err(format!(
            "resolve check: {} was killed by a signal (unresolved dynamic library?) — \
             refusing a broken toolchain install",
            bin.display()
        ))
    }
}
