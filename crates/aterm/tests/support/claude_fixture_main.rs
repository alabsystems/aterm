// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only native Claude image for owned integration worlds.

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[path = "claude_fixture/host.rs"]
mod host;

fn main() {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if host::run_if_selected() {
        return;
    }
    std::process::exit(2);
}
