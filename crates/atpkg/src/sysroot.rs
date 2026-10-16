// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The fail-loud resolve check for a `sysroot-bundle` member (§10.1): a bundle whose
//! dynamic loader cannot resolve its libraries fails the apply instead of reporting a
//! successful install.

use std::path::Path;

/// Run an installed toolchain binary (`bin`, e.g. `trustc`) with `--version` and require
/// a real exit code, which it reaches only if the dynamic loader resolved every library
/// from the installed bundle.
///
/// # Errors
/// When the binary cannot be spawned or is killed by a signal.
pub fn resolve_check(bin: &Path) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    let status = aterm_uds::spawnfd::command(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("resolve check: cannot spawn {}: {e}", bin.display()))?;
    // Any exit code proves the loader resolved the bundle; a signal death is the
    // dyld/ld.so failure.
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
