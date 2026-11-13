// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Destination resolution and private writing for `SIGUSR1` screen snapshots.

#[cfg(unix)]
use crate::control_auth;

#[cfg(unix)]
fn default_snapshot_file_name(pid: u32) -> String {
    format!("aterm_snapshot-{pid}.png")
}

/// Resolve the path the snapshot PNG may be written to, or `None` when unavailable.
#[must_use]
#[cfg(unix)]
pub(crate) fn resolve() -> Option<String> {
    match control_auth::socket_dir() {
        Some(dir) => Some(
            dir.join(default_snapshot_file_name(std::process::id()))
                .to_string_lossy()
                .into_owned(),
        ),
        None => {
            crate::logging::stderr_line!(
                "aterm-gui: no per-user runtime dir (set XDG_RUNTIME_DIR or HOME); \
                 snapshot skipped"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "snapshot_path_tests.rs"]
mod tests;
