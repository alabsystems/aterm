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
            .env_remove("KIMI_CODE_NO_AUTO_UPDATE")
            .env_remove("ATERM_NO_HARNESS")
            .env_remove("ATERM_PARENT_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run")
    }

    fn atpkg_run(&self, args: &[&str]) -> Output {
        let mut argv = vec!["run", "claude", "--"];
        argv.extend_from_slice(args);
        self.run(&aterm_tempfile::bin_exe("atpkg"), &argv)
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

/// `atpkg run` (what `aterm kimi` runs) HANDS A WORD A HARNESS LEADS TO ITS `bin/` SHIM, whose
/// hand-off runs the launch under that harness as a typed `kimi` does: kimi's row keeps no
/// launcher block since HX-09b, so `aterm kimi upgrade` reaches kimi-alab's `run -- upgrade`,
/// which answers it, never Kimi Code's own updater; each gets the build's declared environment
/// though the shim was laid without it. CONTROL: before kimi-alab is installed, and under the
/// shim's `--no-harness`, the store build runs the words as typed.
#[test]
fn run_hands_a_word_a_harness_leads_to_that_harness() {
    let fx = Fixture::new("led");
    let script = |dir: &std::path::Path, tool: &str| {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin").join(tool);
        std::fs::write(
            &exe,
            format!("#!/bin/sh\necho \"{tool}: [${{KIMI_CODE_NO_AUTO_UPDATE-unset}}] $*\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        atpkg::activate::install_shims_env(
            &fx.layout,
            dir,
            &[tool.to_string()],
            atpkg::activate::Aliases::Off,
            &atpkg::shim_env::ShimEnv::NONE,
        )
        .unwrap();
        atpkg::store::mark_build_ready(dir).unwrap();
    };
    let kimi = fx.layout.build_dir("kimi", BUILD);
    script(&kimi, "kimi");
    std::fs::write(
        kimi.with_file_name(format!("{BUILD}.shim-env")),
        "KIMI_CODE_NO_AUTO_UPDATE=1\n",
    )
    .unwrap();
    // bin/kimi as a pass lays it: the twin prelude, with its hand-off to kimi-alab.
    atpkg::activate::reconcile_agents(&fx.layout);
    let shim = fx
        .layout
        .shim(&atpkg::store::ToolName::new("kimi").unwrap());
    let body = std::fs::read_to_string(&shim).unwrap();
    assert!(
        body.contains("bin/kimi-alab' run -- \"$@\""),
        "PRECONDITION: bin/kimi hands its launch to kimi-alab:\n{body}"
    );
    let run = |args: &[&str]| {
        let mut argv = vec!["run", "kimi", "--"];
        argv.extend_from_slice(args);
        let out = fx.run(&aterm_tempfile::bin_exe("atpkg"), &argv);
        assert!(out.status.success(), "{args:?}: {}", describe(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(
        run(&["upgrade"]),
        "kimi: [1] upgrade\n",
        "no kimi-alab installed"
    );
    script(&fx.layout.build_dir("kimi-alab", BUILD), "kimi-alab");
    assert_eq!(
        run(&["upgrade"]),
        "kimi-alab: [1] run -- upgrade\n",
        "the harness gets `run --` and the words as typed"
    );
    assert_eq!(
        run(&["-m", "k2", "fix it"]),
        "kimi-alab: [1] run -- -m k2 fix it\n"
    );
    assert_eq!(run(&["--no-harness", "upgrade"]), "kimi: [1] upgrade\n");
}
