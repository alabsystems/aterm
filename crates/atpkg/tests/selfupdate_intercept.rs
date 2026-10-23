// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-edge tests for `atpkg __selfupdate claude <prefix> -- <args…>`.

#![cfg(unix)]

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
// Close non-stdio fds across spawn.
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    config_home: PathBuf,
    registry: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-selfupdate-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let registry = root.path().join("registry");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        let fx = Self {
            root,
            home,
            config_home,
            registry,
            prefix,
        };
        fx.layout()
            .ensure_dir(&fx.prefix)
            .expect("fixture: the 0700 prefix directory");
        fx
    }

    fn layout(&self) -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: self.prefix.clone(),
        }
    }

    fn row() -> &'static atpkg::vendor_direct::VendorSpec {
        atpkg::selfupdate::row_for("claude").expect("claude is rostered")
    }

    fn fake_shim(&self, tool: &str) {
        let layout = self.layout();
        layout.ensure_dir(&layout.bin_dir()).unwrap();
        let shim = layout.shim(&atpkg::store::ToolName::new(tool).unwrap());
        std::fs::write(&shim, b"#!/bin/sh\necho \"fake: $*\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn install_claude(&self, build: u64) {
        let layout = self.layout();
        let dir = layout.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin/claude"),
            b"#!/bin/sh\necho \"fake: $*\"\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &["claude".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        assert_eq!(
            atpkg::ops::active_builds(&layout).get("claude"),
            Some(&build),
            "fixture: the build is active"
        );
    }

    fn verb_argv(&self, program: &str, args: &[&str]) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            OsString::from(atpkg::selfupdate::HIDDEN_VERB),
            OsString::from(program),
            self.prefix.clone().into_os_string(),
            OsString::from("--"),
        ];
        argv.extend(args.iter().map(OsString::from));
        argv
    }

    fn selfupdate(&self, program: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.raw(&self.verb_argv(program, args), env)
    }

    fn command(&self, argv: &[OsString], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.args(argv)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("ATPKG_REGISTRY", format!("dir:{}", self.registry.display()))
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    fn raw(&self, argv: &[OsString], env: &[(&str, &str)]) -> Output {
        self.command(argv, env).output().expect("run the dev atpkg")
    }

    fn lock_held(&self) -> bool {
        self.layout().store_lock().exists()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        text(&out.stdout),
        text(&out.stderr)
    )
}

struct Streamed {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: std::thread::JoinHandle<String>,
    stdout_reader: std::thread::JoinHandle<()>,
    started: Instant,
}

fn stream(mut cmd: Command) -> Streamed {
    let mut child = cmd.spawn().expect("spawn the dev atpkg");
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&stdout);
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let mut err = err;
        let _ = err.read_to_string(&mut text);
        text
    });
    Streamed {
        child,
        stdout,
        stderr,
        stdout_reader,
        started: Instant::now(),
    }
}

impl Streamed {
    fn stdout_has(&self, needle: &str) -> bool {
        self.stdout
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(needle))
    }

    fn wait_for_line(&mut self, needle: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.stdout_has(needle) {
            if let Some(status) = self.child.try_wait().expect("poll") {
                panic!(
                    "the child exited ({status}) before printing {needle:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "no {needle:?} on stdout within {within:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn finish(mut self, within: Duration) -> (ExitStatus, Duration, Vec<String>, String) {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the child did not exit within {within:?}; stdout: {:?}",
                    self.stdout.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let elapsed = self.started.elapsed();
        self.stdout_reader.join().unwrap();
        let stderr = self.stderr.join().unwrap();
        let stdout = self.stdout.lock().unwrap().clone();
        (status, elapsed, stdout, stderr)
    }
}

const ANNOUNCE_WITHIN: Duration = Duration::from_secs(60);

fn wait_markers(stdout: &[String]) -> Vec<&str> {
    stdout
        .iter()
        .map(String::as_str)
        .filter(|l| {
            !(l.starts_with("atpkg: machine")
                || l.starts_with("atpkg machine:")
                || l.starts_with("atpkg: Universal Control"))
        })
        .collect()
}

#[test]
fn a_held_store_is_waited_for_and_the_check_runs_once_the_holder_lets_go() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        aterm_uds::host::could_not_run(
            "this build pins no root key, so the package manager is off and waits for nothing",
        );
    }
    let fx = Fixture::new("held-then-released");
    fx.install_claude(2026091601);
    let missing = fx.root.path().join("no-such-registry");
    let guard = atpkg::lock::try_lock_store(&fx.layout()).expect("the fixture holds the lock");
    let registry = format!("dir:{}", missing.display());
    let mut child = stream(fx.command(
        &fx.verb_argv("claude", &["update"]),
        &[
            ("ATPKG_REGISTRY", &registry),
            (atpkg::lock::WAIT_ANNOUNCE_GRACE_SEAM, "0"),
        ],
    ));
    let waiting = format!("atpkg: {}", atpkg::cli::LOCK_WAITING_MARKER);
    child.wait_for_line(&waiting, ANNOUNCE_WITHIN);
    assert!(
        child.child.try_wait().expect("poll").is_none(),
        "the child is still waiting, not exited 75, while the pass is merely busy"
    );
    drop(guard);
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    let shown = format!("status {status:?}\nstdout:\n{stdout:?}\nstderr:\n{stderr}");
    assert_eq!(
        status.code(),
        Some(1),
        "the check ran and failed on its own line: {shown}"
    );
    let markers = wait_markers(&stdout);
    assert_eq!(markers.len(), 2, "the wait lane's two markers: {shown}");
    assert!(
        markers[0].starts_with(&waiting)
            && markers[1].starts_with(&format!("atpkg: {}", atpkg::cli::LOCK_ACQUIRED_MARKER)),
        "the wait lane's two markers, in order, and nothing else of the child's: {shown}"
    );
    assert!(
        !stdout
            .iter()
            .any(|l| l.contains(atpkg::cli::SEED_BUSY_MARKER)),
        "no stand-aside: the wait ended in the lock: {shown}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(atpkg::selfupdate::announce_line(Fixture::row(), "build 2026091601").as_str()),
        "the announce line first: {shown}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(atpkg::selfupdate::incomplete_line(Fixture::row(), Some(2026091601)).as_str()),
        "the incomplete line last: {shown}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains("fake:")),
        "the vendor's verb never ran: {shown}"
    );
    let _ = atpkg::lock::try_lock_store(&fx.layout()).expect("released by the child");
}

#[test]
fn help_forwards_to_the_vendor_after_one_note_and_takes_no_lock() {
    let fx = Fixture::new("help");
    fx.fake_shim("claude");
    let out = fx.selfupdate("claude", &["update", "--help"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(7), "the fake's own code: {shown}");
    assert_eq!(text(&out.stdout), "fake: update --help\n", "{shown}");
    assert_eq!(
        text(&out.stderr),
        format!("{}\n", atpkg::selfupdate::help_line(Fixture::row())),
        "{shown}"
    );
    assert!(!fx.lock_held(), "help takes no lock");

    fx.install_claude(2026091601);
    let layout = fx.layout();
    let claude = atpkg::store::ToolName::new("claude").unwrap();
    let store = layout.build_dir("claude", 2026091601).join("bin/claude");
    let q = |p: &Path| format!("'{}'", p.display());
    std::fs::write(
        layout.agent_shim(&claude),
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  update|upgrade|install)\n    __atpkg={atpkg}\n    \
             if [ -x \"$__atpkg\" ]; then exec \"$__atpkg\" __selfupdate 'claude' {prefix} -- \
             \"$@\"; fi\n    ;;\nesac\nexec {store} \"$@\"\n",
            atpkg = q(Path::new(env!("CARGO_BIN_EXE_atpkg"))),
            prefix = q(&fx.prefix),
            store = q(&store),
        ),
    )
    .unwrap();
    atpkg::activate::reconcile_agents(&layout);
    let mut managed = Command::new(layout.shim(&claude));
    managed
        .args(["update", "--help"])
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("ATPKG_REGISTRY", format!("dir:{}", fx.registry.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (status, _, stdout, stderr) = stream(managed).finish(ANNOUNCE_WITHIN);
    let shown = format!("status {status:?}\nstdout:\n{stdout:?}\nstderr:\n{stderr}");
    assert_eq!(
        status.code(),
        Some(7),
        "the store build's own code: {shown}"
    );
    assert_eq!(stdout, ["fake: update --help"], "{shown}");
    assert_eq!(
        stderr,
        format!("{}\n", atpkg::selfupdate::help_line(Fixture::row())),
        "handed to the verb once, by the managed name: {shown}"
    );
    assert!(!fx.lock_held(), "help takes no lock");
}

#[test]
fn an_unrostered_program_forwards_silently() {
    let fx = Fixture::new("unrostered");
    fx.fake_shim("ay");
    let out = fx.selfupdate("ay", &["update", "--x"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(7), "{shown}");
    assert_eq!(text(&out.stdout), "fake: update --x\n", "{shown}");
    assert_eq!(text(&out.stderr), "", "{shown}");
    assert!(!fx.lock_held());
}

#[test]
fn a_prefix_the_environment_does_not_resolve_is_refused_with_both_named() {
    let fx = Fixture::new("prefix");
    fx.fake_shim("claude");
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
    cmd.arg(atpkg::selfupdate::HIDDEN_VERB)
        .arg("claude")
        .arg(&elsewhere)
        .arg("--")
        .arg("update")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("ATPKG_REGISTRY", format!("dir:{}", fx.registry.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().unwrap();
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(2), "{shown}");
    assert_eq!(
        text(&out.stderr),
        format!(
            "{}\n",
            atpkg::selfupdate::prefix_mismatch_line(Fixture::row(), &elsewhere, &fx.prefix)
        ),
        "{shown}"
    );
    assert_eq!(text(&out.stdout), "", "{shown}");
    assert!(!fx.lock_held());
    assert!(!Path::new(&elsewhere).join("store.lock").exists());
}

#[test]
fn a_name_that_is_not_a_tool_name_is_refused_with_its_line_and_exits_2() {
    let fx = Fixture::new("toolname");
    for name in ["sudo", ""] {
        let argv = vec![
            OsString::from(atpkg::selfupdate::HIDDEN_VERB),
            OsString::from(name),
            fx.prefix.clone().into_os_string(),
            OsString::from("--"),
            OsString::from("update"),
        ];
        let out = fx.raw(&argv, &[]);
        let shown = describe(&out);
        assert_eq!(out.status.code(), Some(2), "{name:?}: {shown}");
        assert_eq!(text(&out.stdout), "", "{name:?}: {shown}");
    }
    assert!(!fx.lock_held());
}

#[test]
fn a_shim_that_cannot_be_run_answers_126_with_the_could_not_run_line() {
    let fx = Fixture::new("noexec");
    fx.fake_shim("claude");
    let layout = fx.layout();
    let shim = layout.shim(&atpkg::store::ToolName::new("claude").unwrap());
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = fx.selfupdate("claude", &["--probe"], &[]);
    let shown = describe(&out);
    assert_eq!(out.status.code(), Some(126), "{shown}");
    assert_eq!(text(&out.stdout), "", "{shown}");
    assert!(!fx.lock_held());
}
