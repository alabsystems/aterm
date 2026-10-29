// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only Windows pseudoconsole helper binary for `aterm-pty` tests.
//!
//! STOCK EXCEPTION: its work is Windows only, and Trust has no Windows std, so only stock Rust
//! builds it (the Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    std::process::ExitCode::from(64)
}
