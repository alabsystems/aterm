// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `packages.log` records `pass-start` and `pass-end` across lanes in an isolated temp store.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-packages-log-lanes-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        layout.ensure_dir(&prefix).unwrap();
        std::fs::write(layout.declined(), b"declined by the test fixture\n").unwrap();
        Self { root, home, prefix }
    }

    fn log(&self) -> PathBuf {
        self.log_under(Some(self.state_home()))
    }

    fn home_log(&self) -> PathBuf {
        self.log_under(None)
    }

    fn state_home(&self) -> PathBuf {
        self.root.path().join("aterm-state")
    }

    fn log_under(&self, state_home: Option<PathBuf>) -> PathBuf {
        aterm_types::dirs::resolve_logs_dir(
            state_home,
            aterm_types::dirs::StatePlatform {
                home: Some(self.home.clone()),
                xdg_state_home: Some(self.root.path().join("state")),
                local_app_data: None,
            },
        )
        .unwrap()
        .join(atpkg::packages_log::LOG_NAME)
    }

    fn run(&self, args: &[&str], spawner: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("ATERM_STATE_HOME", self.state_home())
            .env_remove("ATPKG_DISABLE")
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(spawner) = spawner {
            cmd.env(atpkg::cli::SPAWNER_PID_ENV, spawner);
        }
        cmd.output().expect("run dev atpkg")
    }

    fn events(&self) -> Vec<atpkg::packages_log::Entry> {
        let text = std::fs::read_to_string(self.log()).unwrap_or_default();
        text.lines()
            .map(|l| atpkg::packages_log::parse_line(l).unwrap_or_else(|| panic!("{l:?}")))
            .collect()
    }
}

#[test]
fn every_lane_logs_its_pass_start_and_end() {
    let fx = Fixture::new("lanes");
    let progress = fx.prefix.join("progress.json");
    let progress = progress.to_str().unwrap();
    let me = std::process::id().to_string();
    let cases: [(&[&str], Option<&str>, &str); 3] = [
        (&["seed"], None, "typed"),
        (
            &["seed", "--wait-lock", "5", "--progress-file", progress],
            Some(atpkg::cli::SPAWNER_DETACHED),
            "session",
        ),
        (
            &["seed", "--wait-lock", "5", "--progress-file", progress],
            Some(&me),
            "window",
        ),
    ];
    for (args, spawner, lane) in cases {
        let before = fx.events().len();
        let out = fx.run(args, spawner);
        assert!(out.status.success(), "{lane}: {out:?}");
        let new = &fx.events()[before..];
        let kinds: Vec<(&str, Option<&str>, Option<&str>)> = new
            .iter()
            .map(|e| (e.kind.as_str(), e.get("lane"), e.get("verb")))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("pass-start", Some(lane), Some("seed")),
                ("pass-end", Some(lane), Some("seed")),
            ],
            "{lane}"
        );
        assert_eq!(new[1].get("exit"), Some("0"));
        assert!(new.iter().all(|e| e.at.is_some()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(fx.log()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the log is the owner's alone");
    }
    assert_ne!(fx.log(), fx.home_log());
    assert!(!fx.home_log().exists(), "{}", fx.home_log().display());
}

#[test]
fn a_pass_the_lock_refused_says_it_did_not_run() {
    let fx = Fixture::new("refused");
    let _held = atpkg::lock::try_lock_store(&atpkg::store::Layout {
        prefix: fx.prefix.clone(),
    })
    .expect("the fixture holds the lock");
    let out = fx.run(&["seed"], None);
    assert_eq!(
        out.status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT))
    );
    let events = fx.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].kind, "pass-end");
    assert_eq!(events[0].get("exit"), Some("75"));
}

fn concurrent_appender_child_if_asked() -> bool {
    let Some(dir) = std::env::var_os("ATPKG_TEST_LOG_APPENDER_DIR") else {
        return false;
    };
    let dir = PathBuf::from(dir);
    let t = atpkg::packages_log::Transition {
        program: "claude".into(),
        from: Some("2.1.278".into()),
        to: Some("2.1.280".into()),
        source: "Anthropic latest".into(),
        result: "updated",
        reason: "r".repeat(300),
    };
    for _ in 0..400 {
        let line = atpkg::packages_log::render(
            1_790_000_000,
            std::process::id(),
            &atpkg::packages_log::Event::Program(&t),
        );
        atpkg::packages_log::append_line(&dir, &line, false).unwrap();
    }
    true
}

struct Reaped(Child);

impl Reaped {
    fn wait(&mut self) -> ExitStatus {
        self.0.wait().expect("wait for the appender")
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn two_processes_appending_at_once_interleave_whole_lines() {
    if concurrent_appender_child_if_asked() {
        return;
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atpkg-packages-log-concurrent-")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
    let exe = std::env::current_exe().unwrap();
    let spawn = || {
        Reaped(
            Command::new(&exe)
                .args([
                    "--exact",
                    "two_processes_appending_at_once_interleave_whole_lines",
                    "--test-threads=1",
                ])
                .env("ATPKG_TEST_LOG_APPENDER_DIR", dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn an appender"),
        )
    };
    let mut a = spawn();
    let mut b = spawn();
    let (status_a, status_b) = (a.wait(), b.wait());
    assert!(
        status_a.success() && status_b.success(),
        "{status_a:?} {status_b:?}"
    );
    let text = std::fs::read_to_string(dir.join(atpkg::packages_log::LOG_NAME)).unwrap();
    let mut per_pid = std::collections::BTreeMap::<u32, usize>::new();
    for line in text.lines() {
        let entry =
            atpkg::packages_log::parse_line(line).unwrap_or_else(|| panic!("torn line: {line:?}"));
        assert_eq!(entry.get("reason").map(str::len), Some(300));
        *per_pid.entry(entry.pid).or_default() += 1;
    }
    assert_eq!(per_pid.len(), 2, "{per_pid:?}");
    assert!(per_pid.values().all(|n| *n == 400), "{per_pid:?}");
}
