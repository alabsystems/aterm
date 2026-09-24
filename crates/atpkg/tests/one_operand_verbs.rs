// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A one-name verb handed several names refuses them all, and does nothing.
//!
//! Measured on m3 (2026-09-23): `aterm pkg uninstall ty ay clean` removed `ty`, printed
//! `atpkg: uninstalled ty`, exited 0 — and `ay` and `clean` were still installed. The
//! arm read `args.get(1)` and dropped the rest without a word, like every other verb
//! whose grammar is one operand. `install` already refused a second program in its own
//! parser; the dispatch edge now refuses for the rest, before the store lock.
//!
//! Driven through the real dev binary over a temp HOME whose default prefix holds two
//! managed programs. The control, `uninstall ta` alone, really removes `ta` — so the
//! refusal is not a fixture that could not have uninstalled anything. NEVER THE REAL
//! STORE: `HOME` is a temp directory, `XDG_CONFIG_HOME` an absent one and the registry a
//! local directory that does not exist, so nothing here can reach the network.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BUILD: u64 = 2_026_092_401;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-one-operand-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let layout = atpkg::store::Layout {
            prefix: atpkg::store::default_prefix(&home),
        };
        assert!(
            layout.prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            layout.prefix.display()
        );
        layout.ensure_dir(&layout.prefix).unwrap();
        Self { root, home, layout }
    }

    /// A ready store build of `program` with its `bin/<program>` shim laid.
    fn install(&self, program: &str) {
        let dir = self.layout.build_dir(program, BUILD);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let bin = dir.join("bin").join(program);
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        atpkg::activate::install_shims_env(
            &self.layout,
            &dir,
            &[program.to_string()],
            atpkg::activate::Aliases::Off,
            &atpkg::shim_env::ShimEnv::admit(&[]).unwrap(),
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
    }

    /// Whether `program` is still on this machine: its store tree AND its shim.
    fn installed(&self, program: &str) -> bool {
        let shim = self
            .layout
            .shim(&atpkg::store::ToolName::new(program).unwrap());
        self.layout.prefix.join("store").join(program).exists()
            && std::fs::symlink_metadata(shim).is_ok()
    }

    fn atpkg(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run atpkg")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn uninstall_of_two_names_refuses_both_and_removes_neither() {
    let f = Fixture::new("uninstall");
    f.install("ta");
    f.install("tb");
    assert!(f.installed("ta") && f.installed("tb"), "fixture");

    let out = f.atpkg(&["uninstall", "ta", "tb"]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "the usage exit:\n{all}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("atpkg uninstall: one operand at a time (got 2: \"ta\" \"tb\")")
            && stderr.contains("nothing was done"),
        "the refusal names the verb, the count and every name:\n{all}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("uninstalled"),
        "nothing may be reported removed:\n{all}"
    );
    assert!(
        f.installed("ta") && f.installed("tb"),
        "NEITHER program may be removed — acting on the first name was the bug:\n{all}"
    );
    assert!(
        !f.layout.prefix.join("removed").exists(),
        "no removal intent may be recorded for a refused command:\n{all}"
    );

    // CONTROL: the same fixture DOES uninstall one name, so the refusal above is the
    // guard speaking, not a fixture that could not have removed anything.
    let one = f.atpkg(&["uninstall", "ta"]);
    let one_all = text(&one);
    assert_eq!(one.status.code(), Some(0), "{one_all}");
    assert!(
        !f.installed("ta"),
        "the control really uninstalls:\n{one_all}"
    );
    assert!(
        f.installed("tb"),
        "and only what it was asked to:\n{one_all}"
    );
}

#[test]
fn every_one_name_verb_refuses_a_second_name_at_the_usage_exit() {
    let f = Fixture::new("verbs");
    f.install("ta");
    for verb in [
        "which",
        "uninstall",
        "tree-root",
        "update",
        "rollback",
        "pin",
        "unpin",
        "verify",
        "unlink",
    ] {
        let out = f.atpkg(&[verb, "ta", "tb"]);
        let all = text(&out);
        assert_eq!(out.status.code(), Some(2), "`{verb} ta tb`:\n{all}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains(&format!("atpkg {verb}: one operand at a time (got 2")),
            "`{verb} ta tb` names its own verb:\n{all}"
        );
    }
    assert!(f.installed("ta"), "no verb above may have acted on `ta`");
}
