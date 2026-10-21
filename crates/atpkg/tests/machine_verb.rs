// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-level integration tests for `atpkg machine` on macOS.
//!
//! Runs `atpkg machine` with a temporary `HOME` to verify stdout record
//! parsing (`MachineState::parse`), synthetic-home refusal on `--apply`
//! (since `defaults` writes to the account's per-host domain regardless of
//! `$HOME`), and non-zero exit on malformed config files.
#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::process::{Command, Stdio};

/// A temp `HOME` with nothing in it: no config, no store.
struct Fixture {
    root: aterm_tempfile::TempDir,
    home: std::path::PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-machine-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("fixture home");
        Self { root, home }
    }

    fn run(&self, args: &[&str]) -> (bool, Vec<String>, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn dev atpkg");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Verify `atpkg machine` output parses via `MachineState::parse`.
#[test]
fn the_read_prints_exactly_one_parseable_record_and_exits_zero() {
    let fx = Fixture::new("read");
    let (ok, stdout, stderr) = fx.run(&["machine"]);
    assert!(ok, "the read never fails: {stderr}");

    let records: Vec<atpkg::machine::MachineState> = stdout
        .iter()
        .filter_map(|line| {
            line.strip_prefix("atpkg: ")
                .and_then(|rest| rest.strip_prefix(atpkg::cli::MACHINE_STATE_MARKER))
                .and_then(atpkg::machine::parse_machine_state)
        })
        .collect();
    assert_eq!(
        records.len(),
        1,
        "exactly one `machine-state:` record, and it parses: {stdout:?}"
    );
    let state = &records[0];
    assert!(!state.config_unreadable, "{stdout:?}");
    // And the prose above it: the verdict prefix the card quotes, on its own lines.
    assert!(
        stdout
            .iter()
            .any(|l| l.starts_with(atpkg::cli::MACHINE_VERDICT_PREFIX)),
        "{stdout:?}"
    );
}

/// The apply REFUSES under a synthetic home, exits 0, and writes nothing —
/// the guard that keeps every integration suite off the developer's real machine.
#[test]
fn the_apply_refuses_a_synthetic_home_and_writes_nothing() {
    let fx = Fixture::new("apply");
    let (ok, stdout, stderr) = fx.run(&["machine", "apply"]);
    assert!(ok, "a refusal is not a failure: {stderr}");
    let refusals: Vec<&String> = stdout
        .iter()
        .filter(|l| l.starts_with(atpkg::cli::MACHINE_NOT_APPLIED_PREFIX))
        .collect();
    assert_eq!(refusals.len(), 1, "{stdout:?}");
    assert!(
        !stdout
            .iter()
            .any(|l| l.contains(atpkg::machine::UNIVERSAL_CONTROL_ENTRY)),
        "nothing was written: {stdout:?}"
    );
}

/// An unparseable config causes `atpkg machine` (with or without `--apply`) to exit non-zero.
#[test]
fn an_unparseable_config_refuses_on_both_spellings() {
    let fx = Fixture::new("badconfig");
    let config_dir = fx.root.path().join("config").join("aterm");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::write(config_dir.join("aterm.toml"), b"[machine\nnot = toml\n").expect("write");
    for args in [&["machine"][..], &["machine", "apply"][..]] {
        let (ok, stdout, stderr) = fx.run(args);
        assert!(ok, "{args:?}: {stderr}");
        assert!(
            stdout
                .iter()
                .any(|l| l.contains("does not parse") || l.contains("not applied")),
            "{args:?} must refuse: {stdout:?}"
        );
    }
}
