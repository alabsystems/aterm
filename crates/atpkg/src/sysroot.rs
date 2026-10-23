// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dynamic loader resolution check for `sysroot-bundle` members.

use std::path::Path;

/// Runs `bin --version` and requires a non-signal exit code.
///
/// # Errors
/// When the binary cannot be spawned or is killed by a signal. A started child that never
/// exited on its own leaves its build held in place (`store::hold_unproven`).
pub fn resolve_check(bin: &Path) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    let mut child = aterm_uds::spawnfd::command(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("resolve check: cannot spawn {}: {e}", bin.display()))?;
    let why = match child.wait() {
        // Any exit code proves the loader resolved the bundle; a signal death is the
        // dyld/ld.so failure.
        Ok(status) if status.code().is_some() => return Ok(()),
        Ok(_) => String::from("was killed by a signal (unresolved dynamic library?)"),
        Err(e) => format!("could not be waited for ({e})"),
    };
    // Nothing showed it ran, so macOS may still be assessing its first exec.
    crate::store::hold_unproven(bin);
    Err(format!(
        "resolve check: {} {why} — refusing a broken toolchain install; its build stays on \
         disk while macOS may still be assessing that first exec",
        bin.display()
    ))
}
