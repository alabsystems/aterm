// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance: THE LAUNCH'S PRIMER PASS STARTS FROM THE EVENT LOOP AND PRIMES THE
//! CONFIGURED AGENT HOME.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::{headless_boot, launch_isolation};

/// `aterm-gui`'s `HEADLESS_AGENT_PRIME_SEAM`.
const SEAM: &str = "ATERM_DEBUG_HEADLESS_AGENT_PRIME";

/// The opening words of every primer block (`aterm_primer`'s marker line).
const PRIMER_MARK: &str = "<!-- aterm primer v";

/// The private state root the seam asks for (`ATERM_STATE_HOME`).
fn state(root: &Path) -> PathBuf {
    root.join("state-aterm")
}

#[test]
fn the_launchs_primer_pass_starts_from_the_first_park() {
    let inst = headless_boot::boot_with(
        "aterm-agent-prime",
        |root, cmd| {
            std::fs::write(
                root.join("cfg/aterm/aterm.toml"),
                format!("agents_auto_prime = true\n{}", launch_isolation::CONFIG_OFF),
            )
            .expect("the scratch aterm.toml");
            std::fs::create_dir_all(root.join("home/.claude")).expect("detect claude");
            cmd.env("ATERM_STATE_HOME", state(root)).env(SEAM, "1");
        },
        |inst| inst.tmp.join("home/.claude/CLAUDE.md").is_file(),
    );
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    assert!(
        read(inst.tmp.join("home/.claude/CLAUDE.md")).starts_with(PRIMER_MARK),
        "the human's Claude Code got the primer"
    );
}
