// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg run <tool>` — the engine of `aterm <tool>`, which is how a terminal the
//! `agents/` twin does not lead reaches the managed copy — execs the store binary WITHOUT
//! its shim, so it has to do what the shim and the twin do before their `exec` (audit
//! 2026-09-23):
//!
//! * export the environment the build declares (design S7): `aterm claude` ran Claude
//!   Code with its own updater ON beside a `bin/claude` that ran it off — and on a machine
//!   where a pre-2026-09-23 `repair` had laid `bin/claude` plain, reading the shim would
//!   not have helped, so the build's `<build>.shim-env` sidecar comes first;
//! * answer a vendor's own self-update verb as the FIRST argument the way the twin does
//!   (`atpkg::selfupdate`): `aterm claude install stable` ran the vendor's installer,
//!   which installs a copy the managed name never runs.
//!
//! Driven through the real dev binary over a temp HOME whose default prefix holds a
//! managed `claude` build — a fake that prints the variable and its argv, and exits 7 —
//! with the shim itself run beside it as the control. NEVER THE REAL STORE: `HOME` is a
//! temp directory, `XDG_CONFIG_HOME` an absent one and the registry a local directory,
//! and no ending driven here reaches the update child (a declined shape answers before
//! it).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// The managed build: a legacy index build, as the machine the audit measured had.
const BUILD: u64 = 2_026_092_201;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "atpkg-run-managed-name-{}-{case}",
            std::process::id()
        ));
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

    fn build_dir(&self) -> PathBuf {
        self.layout.build_dir("claude", BUILD)
    }

    /// The store `claude`, its `bin/claude` shim laid by the activation code exporting
    /// `env` (empty ⇒ PLAIN), and the build marked ready.
    fn install_claude(&self, env: &[&str]) {
        let dir = self.build_dir();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin/claude"),
            b"#!/bin/sh\necho \"fake: [${DISABLE_AUTOUPDATER-unset}] $*\"\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let env = atpkg::shim_env::ShimEnv::admit(
            &env.iter().map(|e| (*e).to_string()).collect::<Vec<_>>(),
        )
        .unwrap();
        atpkg::activate::install_shims_env(
            &self.layout,
            &dir,
            &["claude".to_string()],
            atpkg::activate::Aliases::Off,
            &env,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
    }

    /// What the build DECLARES: its `store/claude/<build>.shim-env` sidecar, one
    /// `NAME=VALUE` per line — the file the install lanes write from the signed policy.
    fn declare(&self, entry: &str) {
        let mut name = self
            .build_dir()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        name.push_str(".shim-env");
        std::fs::write(self.build_dir().with_file_name(name), format!("{entry}\n")).unwrap();
    }

    fn shim(&self) -> PathBuf {
        self.layout
            .shim(&atpkg::store::ToolName::new("claude").unwrap())
    }

    /// `program args…` confined to this fixture, the variable under test absent from the
    /// caller's environment.
    fn run(&self, program: &std::path::Path, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .env_remove("DISABLE_AUTOUPDATER")
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run")
    }

    /// `atpkg run claude -- args…`, the argv `aterm claude args…` synthesizes.
    fn atpkg_run(&self, args: &[&str]) -> Output {
        let mut argv = vec!["run", "claude", "--"];
        argv.extend_from_slice(args);
        self.run(std::path::Path::new(env!("CARGO_BIN_EXE_atpkg")), &argv)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The fake's one line, or a panic naming everything the run printed.
fn fake_line(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.starts_with("fake: "))
        .unwrap_or_else(|| panic!("the fake did not run:\n{}", describe(out)))
        .to_string()
}

/// A HEALTHY machine: `bin/claude` exports `DISABLE_AUTOUPDATER=1` (the control) and
/// `atpkg run claude` must too — it exported nothing until 2026-09-23.
#[test]
fn run_exports_what_the_shim_exports() {
    let fx = Fixture::new("healthy");
    fx.install_claude(&["DISABLE_AUTOUPDATER=1"]);
    fx.declare("DISABLE_AUTOUPDATER=1");
    let control = fx.run(&fx.shim(), &["--version"]);
    assert_eq!(
        fake_line(&control),
        "fake: [1] --version",
        "control: the shim"
    );
    let out = fx.atpkg_run(&["--version"]);
    assert_eq!(out.status.code(), Some(7), "{}", describe(&out));
    assert_eq!(fake_line(&out), "fake: [1] --version", "{}", describe(&out));
}

/// A DRIFTED machine: a pre-2026-09-23 `repair` laid `bin/claude` plain over a build
/// that declares `DISABLE_AUTOUPDATER=1` — the shim itself runs without it (the control)
/// — and `atpkg run claude` still exports what the build declares.
#[test]
fn run_exports_what_the_build_declares_over_a_shim_laid_plain() {
    let fx = Fixture::new("drifted");
    fx.install_claude(&[]);
    fx.declare("DISABLE_AUTOUPDATER=1");
    let control = fx.run(&fx.shim(), &["--version"]);
    assert_eq!(
        fake_line(&control),
        "fake: [unset] --version",
        "control: the drifted shim"
    );
    let out = fx.atpkg_run(&["--version"]);
    assert_eq!(fake_line(&out), "fake: [1] --version", "{}", describe(&out));
}

/// THE TWIN'S SELF-UPDATE BLOCK: `aterm claude install stable` is answered by the
/// package manager — declined, exit 2, the exact line the twin's hand-over prints, the
/// vendor's installer never run — and only on the FIRST token: `claude -p update` is a
/// prompt and reaches the tool, with its environment.
#[test]
fn run_answers_a_self_update_verb_the_way_the_twin_does() {
    let fx = Fixture::new("selfupdate");
    fx.install_claude(&[]);
    fx.declare("DISABLE_AUTOUPDATER=1");
    let row = atpkg::selfupdate::row_for("claude").expect("claude is rostered");
    let out = fx.atpkg_run(&["install", "stable"]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(2), "{shown}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{}\n",
            atpkg::selfupdate::declined_line(
                row,
                &atpkg::selfupdate::Decline::NotAChannel {
                    verb: "install",
                    target: "stable",
                },
            )
        ),
        "{shown}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("fake:"),
        "the vendor's installer never runs: {shown}"
    );

    let out = fx.atpkg_run(&["-p", "update"]);
    assert_eq!(out.status.code(), Some(7), "{}", describe(&out));
    assert_eq!(fake_line(&out), "fake: [1] -p update", "{}", describe(&out));
}

/// HELP ON A SELF-UPDATE VERB is the one ending of the plan that runs the tool, and it
/// runs it as every other `aterm claude` is run: the one note, then the vendor's own
/// `--help` from the store binary WITH the build's environment. It used to be forwarded
/// the twin's way, through `bin/claude` — which on a drifted machine (the shim laid
/// plain, the control) ran it with Claude Code's own updater on (review 2026-09-23).
#[test]
fn run_answers_self_update_help_with_the_builds_environment() {
    let fx = Fixture::new("selfupdate-help");
    fx.install_claude(&[]);
    fx.declare("DISABLE_AUTOUPDATER=1");
    let control = fx.run(&fx.shim(), &["update", "--help"]);
    assert_eq!(
        fake_line(&control),
        "fake: [unset] update --help",
        "control: the drifted shim"
    );
    let row = atpkg::selfupdate::row_for("claude").expect("claude is rostered");
    let out = fx.atpkg_run(&["update", "--help"]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(7), "the vendor's help ran: {shown}");
    assert_eq!(fake_line(&out), "fake: [1] update --help", "{shown}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("{}\n", atpkg::selfupdate::help_line(row)),
        "the one note, nothing else: {shown}"
    );
}
