// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `machine` verb, run as a PROCESS.
//!
//! The window consumes the bytes `atpkg machine` writes to stdout; the other tests of this
//! feature are unit tests over pure functions and a fake `defaults` port, so this pins the
//! seam: a reword here would leave the card reading "not read yet" forever.
//!
//! THE MACHINE IS NOT TOUCHED. Every child here runs with a temp `HOME`, and the
//! synthetic-home guard refuses on exactly that basis — `defaults` writes the ACCOUNT's
//! per-host domain and ignores `$HOME` (measured 2026-09-14), so a test that let the
//! apply through would disable Universal Control on whatever Mac ran it. The refusal is
//! what these tests assert, and `store_lock_wait.rs` asserts the same edge for a pass.
//! Each test asserts what the verb does on a Mac (off macOS it reads and applies
//! nothing), so the file is macOS's.
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

/// THE RECORD THE WINDOW READS, produced by the real verb and parsed by the real
/// parser — the two halves of the contract, met in one test.
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

/// A CONFIG THAT DOES NOT PARSE STOPS THE VERB, on both spellings. Both `[machine]`
/// defaults act, so a file nobody could read must not be treated as "no opt-outs set".
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
