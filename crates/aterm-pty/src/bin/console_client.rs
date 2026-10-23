// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only Windows pseudoconsole helper binary for `aterm-pty` tests.

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    if let Some(code) = aterm_pty::serve_console_client() {
        return code;
    }
    std::process::ExitCode::from(64)
}
