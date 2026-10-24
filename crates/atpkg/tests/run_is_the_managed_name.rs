// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `atpkg run <tool>` environment export and self-update interception.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BUILD: u64 = 2_026_092_201;

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-run-managed-name-{case}-"))
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

    fn build_dir(&self) -> PathBuf {
        self.layout.build_dir("claude", BUILD)
    }

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

    fn run(&self, program: &std::path::Path, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env_remove("DISABLE_AUTOUPDATER")
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run")
    }

    fn atpkg_run(&self, args: &[&str]) -> Output {
        let mut argv = vec!["run", "claude", "--"];
        argv.extend_from_slice(args);
        self.run(std::path::Path::new(env!("CARGO_BIN_EXE_atpkg")), &argv)
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

fn fake_line(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.starts_with("fake: "))
        .unwrap_or_else(|| panic!("the fake did not run:\n{}", describe(out)))
        .to_string()
}

#[test]
fn run_exports_what_the_build_declares_over_a_shim_laid_plain() {
    let fx = Fixture::new("drifted");
    fx.install_claude(&[]);
    fx.declare("DISABLE_AUTOUPDATER=1");
    for (args, forwarded, defaulted) in [
        (&["--version"][..], "--version", false),
        (
            &["-p", "probe"][..],
            "--dangerously-skip-permissions -p probe",
            true,
        ),
    ] {
        let control = fx.run(&fx.shim(), args);
        assert_eq!(
            fake_line(&control),
            format!("fake: [unset] {forwarded}"),
            "control: the drifted shim"
        );
        let out = fx.atpkg_run(args);
        assert_eq!(
            fake_line(&out),
            format!("fake: [1] {forwarded}"),
            "{}",
            describe(&out)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).contains("aterm: yolo (default): running"),
            defaulted,
            "{}",
            describe(&out)
        );
    }
}

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
