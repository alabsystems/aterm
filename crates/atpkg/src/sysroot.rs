// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dynamic loader resolution check for `sysroot-bundle` members.

use std::path::Path;

/// Runs `bin --version` and requires a non-signal exit code.
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
