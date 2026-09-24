// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PACKAGE LOG AT THE REAL PROCESS EDGE (Phase 4, 2026-09-23): the dev `atpkg` binary,
//! run the way each lane runs it, appends a `pass-start` and a `pass-end` naming that lane
//! to `packages.log` in the log directory under its HOME — and a verb the store lock
//! refused leaves one `pass-end` saying it did not run.
//!
//! NEVER THE REAL STORE OR THE REAL LOG. HOME, the config dir and the state dir are temp
//! directories; `<prefix>/declined` makes `seed` exit 0 with one sentence before any index
//! work, so no network and no store mutation happen.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "atpkg-packages-log-lanes-{}-{case}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        layout.ensure_dir(&prefix).unwrap();
        std::fs::write(layout.declined(), b"declined by the test fixture\n").unwrap();
        Self { root, home, prefix }
    }

    /// Where `packages.log` lands for this HOME: the one log-directory rule.
    fn log(&self) -> PathBuf {
        aterm_types::dirs::resolve_logs_dir(aterm_types::dirs::StatePlatform {
            home: Some(self.home.clone()),
            xdg_state_home: Some(self.root.join("state")),
            local_app_data: None,
        })
        .unwrap()
        .join(atpkg::packages_log::LOG_NAME)
    }

    fn run(&self, args: &[&str], spawner: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
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
}

/// A typed verb the store lock refuses leaves one `pass-end` (exit 75), no start.
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
    assert_eq!(
        events[0].get("outcome"),
        Some("did not run: another pass holds the store")
    );
}

/// The child half of the two-process test below: append this process's lines, through the
/// real writer ([`atpkg::packages_log::append_line`]).
#[test]
fn concurrent_appender_child() {
    let Some(dir) = std::env::var_os("ATPKG_TEST_LOG_APPENDER_DIR") else {
        return;
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
}

/// TWO PROCESSES APPENDING AT ONCE interleave whole lines, never halves: every line in the
/// file parses, and each process's count is all there.
///
/// HERE, in its own test binary, and never among the library's unit tests (review of Phase
/// 4, 2026-09-23): a child spawned while another test holds a store lock carries a copy of
/// that lock's descriptor until its exec closes it, and exec'ing a whole test binary took
/// long enough that `lock::tests::the_holder_record_names_a_person_only_while_one_holds_the_store`
/// found its store still locked after its guard had dropped — two full runs in three.
/// Cargo runs test binaries one after another, so no library test runs beside this one.
#[test]
fn two_processes_appending_at_once_interleave_whole_lines() {
    let dir = std::env::temp_dir().join(format!(
        "atpkg-packages-log-concurrent-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    let spawn = || {
        Command::new(&exe)
            .args(["--exact", "concurrent_appender_child", "--test-threads=1"])
            .env("ATPKG_TEST_LOG_APPENDER_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let (mut a, mut b) = (spawn(), spawn());
    assert!(a.wait().unwrap().success() && b.wait().unwrap().success());
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
    let _ = std::fs::remove_dir_all(&dir);
}
