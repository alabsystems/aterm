// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance: THE LAUNCH'S PRIMER PASS STARTS FROM THE EVENT LOOP AND
//! RECORDS ITS BUILD (r6 sweep #11).
//!
//! `aterm-gui`'s `spawn::agent_prime_tests` walk the launch's hold and the per-build record
//! on their own clock over scratch folders. Only a real boot shows the wiring between them:
//! `main_entry` takes the launch's cue and holds it, the loop's first park releases it
//! (`App::about_to_wait` → `spawn::release_held_prime`; a headless instance has no first
//! frame to wait for), and the pass's thread primes the human's agents, then every agent
//! identity, then writes `<state>/agents-primed-build`. A headless instance primes only
//! under the `ATERM_DEBUG_HEADLESS_AGENT_PRIME` development seam, with a private
//! `ATERM_STATE_HOME`, a scratch `$HOME` and a scratch `$XDG_CONFIG_HOME`
//! (`headless_agent_prime_runs`).
//!
//! NEGATIVE CONTROL: with the release taken out of `about_to_wait`, the held pass never
//! started, and the boot's wait for the record failed at the hang detector.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, every automatic lane off
//! but the primer, `--no-reroute`, `SHELL=/bin/sh` and a lifeline
//! (`support/launch_isolation.rs`). A headless instance opens no window.

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

/// An agent identity's Claude folder, made as a spawn's admission makes it (0700).
fn identity_claude(root: &Path) -> PathBuf {
    state(root).join("identities/worker/.claude")
}

#[test]
fn the_launchs_primer_pass_starts_from_the_first_park_and_records_its_build() {
    let inst = headless_boot::boot_with(
        "aterm-agent-prime",
        |root, cmd| {
            // The isolation's table turns the primer off; this boot is about it.
            std::fs::write(
                root.join("cfg/aterm/aterm.toml"),
                format!("agents_auto_prime = true\n{}", launch_isolation::CONFIG_OFF),
            )
            .expect("the scratch aterm.toml");
            // Claude Code detected in the scratch home (its folder, no file yet), and one
            // identity with Claude detected.
            std::fs::create_dir_all(root.join("home/.claude")).expect("detect claude");
            let claude = identity_claude(root);
            std::fs::create_dir_all(&claude).expect("the identity");
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut dir = claude.as_path();
                while let Some(parent) = dir.parent().filter(|_| dir != state(root)) {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                        .expect("0700 as an admission makes it");
                    dir = parent;
                }
            }
            cmd.env("ATERM_STATE_HOME", state(root)).env(SEAM, "1");
        },
        |inst| state(&inst.tmp).join("agents-primed-build").is_file(),
    );
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    // The record is written last, by rename, on the pass's thread: both halves are in.
    let record = read(state(&inst.tmp).join("agents-primed-build"));
    assert!(
        record
            .strip_suffix('\n')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        "the record names one build number: {record:?}"
    );
    assert!(
        read(inst.tmp.join("home/.claude/CLAUDE.md")).starts_with(PRIMER_MARK),
        "the human's Claude Code got the primer"
    );
    assert!(
        read(identity_claude(&inst.tmp).join("CLAUDE.md")).starts_with(PRIMER_MARK),
        "the identity's Claude Code got the primer"
    );
}
