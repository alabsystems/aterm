// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console clients of `aterm.exe`'s front door, for this crate's Windows
//! tests (dev-only, never shipped). A shipped tab starts the `aterm.exe` beside
//! the running image into its pseudoconsole to write the resize guard's record
//! or to leave a dead app's alternate screen; a test binary has none beside it,
//! so its tabs run them from this image (`aterm_pty::set_console_client`).

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    // Not started as a client: nothing to do.
    std::process::ExitCode::from(64)
}
