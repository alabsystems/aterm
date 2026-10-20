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
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BUILD: u64 = 2_026_092_401;

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-one-operand-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
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

    /// Record the root of `program`'s build in `status.toml` as its signed one.
    fn record_root(&self, program: &str) {
        let root = atpkg::tree::tree_root(&self.layout.build_dir(program, BUILD)).unwrap();
        let mut status = atpkg::status::Status {
            schema: 1,
            ..Default::default()
        };
        status.programs.insert(
            program.to_string(),
            atpkg::status::ProgramStatus {
                installed_build: Some(BUILD),
                state: "active".into(),
                tree_root: root,
            },
        );
        atpkg::status::write(&self.layout, &status).unwrap();
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
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run atpkg")
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
    // An empty store gives the audit (see the CONTROL below) nothing to fail.
    let out = f.atpkg(&["verify"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
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
    }
    assert!(f.installed("ta"), "no verb above may have acted on `ta`");

    // CONTROL: `verify ta` alone runs the audit, whose exit is its verdict — 1 while no
    // signed root is recorded (fail closed), 0 once the recorded root matches — and a bare
    // `verify` fails when any one program it audits does.
    let out = f.atpkg(&["verify", "ta"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    f.record_root("ta");
    let out = f.atpkg(&["verify", "ta"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    f.install("tb");
    let out = f.atpkg(&["verify"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
}

/// A verb that takes no argument refuses a stray one at the usage exit, before the store
/// lock: `repair --dry-run` (there is no such flag) must not re-lay a single shim or rc
/// line. No bare-`repair` control: repair re-asserts the rustup seam, and `RUSTUP_HOME`
/// is the real one under a rustup-run test.
#[test]
fn a_no_argument_verb_refuses_a_stray_one_and_touches_nothing() {
    let f = Fixture::new("zero");
    f.install("ta");
    let lock = f.layout.store_lock();
    assert!(!lock.exists(), "fixture: no store lock file yet");
    let before = std::fs::read_dir(&f.home)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let out = f.atpkg(&["repair", "--dry-run"]);
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "the usage exit:\n{all}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("unknown argument \"--dry-run\" \u{2014} this verb takes none"),
        "the refusal names the stray argument:\n{all}"
    );
    let mut after = std::fs::read_dir(&f.home)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    // Rosetta writes its translation cache to `$HOME/.cache/rosetta` in every process it
    // runs, whatever `XDG_CACHE_HOME` says (measured 2026-10-19): under a translator that
    // folder is its own, and whether atpkg laid `.cache` is not decided here.
    if let Some(translator) = aterm_uds::process::translator() {
        let cache = std::ffi::OsString::from(".cache");
        let held = std::fs::read_dir(f.home.join(&cache))
            .map(|dir| dir.map(|e| e.unwrap().file_name()).collect::<Vec<_>>());
        if !before.contains(&cache) && held.is_ok_and(|held| held == ["rosetta"]) {
            after.remove(&cache);
            eprintln!(
                "NOT RUN under {translator}: one_operand_verbs::a_no_argument_verb_refuses_a_\
                 stray_one_and_touches_nothing: whether atpkg lays `.cache` in HOME, where the \
                 translator writes its own cache"
            );
        }
    }
    assert_eq!(before, after, "nothing may be laid in HOME:\n{all}");
    assert!(!lock.exists(), "the store lock is never taken:\n{all}");
}
