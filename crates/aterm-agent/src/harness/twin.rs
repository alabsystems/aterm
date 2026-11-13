// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build a managed twin names: the binary its shim execs on its last line.

use std::path::Path;

/// Returns the absolute target binary path from the last line of a managed twin shim:
/// `exec '/<path>' "$@"` after a `#!/bin/sh` line in a `sh` twin, and on Windows
/// `@"<path>" %* & @exit /b` (or the legacy ` %*` tail) in a `.cmd` twin, absolute as Windows
/// reads a path. A `.cmd` body names nothing on another host, which runs none.
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
    let (path, tail) = last.strip_prefix("@\"")?.split_once('"')?;
    (matches!(tail, " %* & @exit /b" | " %*") && Path::new(path).is_absolute())
        .then(|| std::path::PathBuf::from(path))
}

#[cfg(test)]
#[path = "twin_tests.rs"]
mod tests;
