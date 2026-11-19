// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build a managed twin names: the binary its shim execs on its last line.

/// Returns the absolute target binary path from the last line of a managed twin shim:
/// `exec '/<path>' "$@"` after a `#!/bin/sh` line in a `sh` twin, and on Windows a `.cmd`
/// twin's forward line (`atpkg::platform::cmd_forward_target`: `@"<path>"` and the tail that
/// ends the batch with its exit code, or a tail atpkg laid before it), absolute as Windows reads
/// a path. A `.cmd` body names nothing on another host, which runs none.
#[must_use]
pub fn twin_binary(twin_text: &str) -> Option<std::path::PathBuf> {
    let last = twin_text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())?
        .trim();
    if twin_text.starts_with("#!/bin/sh\n") {
        let target = atpkg::platform::sh_exec_target(last)?;
        return target
            .to_str()
            .is_some_and(|target| target.starts_with('/'))
            .then_some(target);
    }
    if !cfg!(windows) {
        return None;
    }
    atpkg::platform::cmd_forward_target(last).filter(|path| path.is_absolute())
}

#[cfg(test)]
#[path = "twin_tests.rs"]
mod tests;
