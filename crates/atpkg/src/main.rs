// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg` entry. The CLI lives in the library (`atpkg::cli::main_entry`)
//! so the one `aterm` binary serves `aterm pkg` in-process; the bundle ships `atpkg` as an argv0
//! symlink onto `aterm`.
//!
//! It names no fleet source (`atpkg::peers::set_fleet`), so a download it runs races only the
//! LAN peers a person wrote in `[packages] peers`, never this Mac's fleet members. The source the
//! `aterm` binary sets (`crates/aterm/src/unit_peers.rs`) is not a call or two away: it reads
//! `aterm-net`'s fleet book, which atpkg does not depend on, and holds every dial until macOS has
//! asked about Local Network (`aterm_cli::macos_doctor::local_network_heard`), and `aterm-cli`
//! depends on atpkg, so this package cannot link it. A copy without that hold could make a
//! download raise macOS's Local Network question, which a download never does.

use std::process::ExitCode;

fn main() -> ExitCode {
    atpkg::cli::main_entry(std::env::args_os().skip(1).collect())
}
