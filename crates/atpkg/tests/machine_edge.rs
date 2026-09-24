// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `[machine]` settings at the REAL process edge (Phase 3, 2026-09-22): an update pass
//! applies them only when the `[machine]` table changed since the last apply, and their own
//! verb applies them every time.
//!
//! The settings' $HOME walk (up to 60,000 directories / 3 s) and `defaults` used to run at
//! the dispatch edge of EVERY update/seed/install pass. The observable here is the
//! synthetic-home refusal line: this child's `HOME` is a temp directory, so an apply that is
//! ATTEMPTED prints `atpkg: machine settings not applied — …` (and touches nothing on the
//! real machine), and one that is not attempted prints nothing about the machine at all.
//! macOS only — elsewhere there is nothing to apply and nothing is ever printed.
//!
//! NEVER THE REAL STORE: `HOME` is a temp directory, `XDG_CONFIG_HOME` an absent one, and
//! the registry an empty local directory, so the pass finds an empty store and exits 0.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture {
    root: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-machine-edge-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("registry")).unwrap();
        let prefix = atpkg::store::default_prefix(&root.join("home"));
        assert!(prefix.starts_with(&root), "{}", prefix.display());
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        layout.ensure_dir(&prefix).unwrap();
        Self { root, prefix }
    }

    fn stdout(&self, args: &[&str]) -> Vec<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .stdin(Stdio::null())
            .output()
            .expect("run the dev atpkg");
        assert!(
            out.status.success(),
            "{args:?}: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn attempted(lines: &[String]) -> bool {
        lines
            .iter()
            .any(|l| l.starts_with(atpkg::cli::MACHINE_NOT_APPLIED_PREFIX))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Never applied: the pass carries the table (the first pass on a machine configures it).
/// Applied and unchanged: the pass says nothing about the machine. The verb: every time.
#[test]
fn an_update_pass_applies_the_machine_settings_only_when_the_table_changed() {
    let fx = Fixture::new("changed");
    assert!(
        Fixture::attempted(&fx.stdout(&["update"])),
        "never applied on this machine: the pass carries the table"
    );
    std::fs::write(
        fx.prefix.join("machine.applied"),
        atpkg::machine::applied_fingerprint(&atpkg::config::MachineConfig::default()),
    )
    .unwrap();
    let idle = fx.stdout(&["update"]);
    assert!(
        !idle.iter().any(|l| l.starts_with("atpkg: machine")
            || l.starts_with("atpkg machine:")
            || l.starts_with("atpkg noindex:")),
        "an unchanged table rides no pass: {idle:?}"
    );
    assert!(
        Fixture::attempted(&fx.stdout(&["machine", "apply"])),
        "the verb applies every time"
    );
}
