// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native ownership controls: the existing publication image runs in place, never copied.
//! The controlled body starts only its own sleeping leaf, with no compiler or provider.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::process::{self, Identity, ProcessPresence};
use aterm_uds::spawnfd::Stdio;
use atpkg_publish::harness_local::{self, Config, Fixture};

const HANG: Duration = Duration::from_secs(60);

fn read(path: &Path) -> Option<aterm_json::Value> {
    aterm_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn wait(mut ready: impl FnMut() -> bool) {
    let by = Instant::now() + HANG;
    while !ready() {
        assert!(
            Instant::now() < by,
            "native guardian did not reach its required state within the hang detector"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn ended(token: &str) -> bool {
    let expected = Identity::parse(token).expect("fixture identity");
    process::read_identity(expected.pid).as_ref() != Some(&expected)
        && matches!(
            process::process_presence(expected.pid),
            ProcessPresence::Absent | ProcessPresence::Exited
        )
}

struct Running {
    scratch: Option<aterm_tempfile::TempDir>,
    home: PathBuf,
    config: Config,
    launch: PathBuf,
    source_record: PathBuf,
    child: aterm_uds::spawnfd::Child,
}

impl Running {
    fn start(fixture: Fixture) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("local-guardian-native-")
            .tempdir()
            .expect("private scratch");
        let home = scratch
            .path()
            .canonicalize()
            .expect("canonical fixture home");
        aterm_uds::private_file::create_private_dir(&home.join("source")).expect("source");
        let words: Vec<String> = vec![
            "claude-code-alab".into(),
            "--checkout".into(),
            home.join("source").display().to_string(),
            "--out".into(),
            home.join("out").display().to_string(),
            "--prefix".into(),
            home.join("pkg").display().to_string(),
            "--native".into(),
            "--unverified".into(),
            "--memory-mib".into(),
            "256".into(),
            "--disk-mib".into(),
            "1024".into(),
            "--poll-seconds".into(),
            "60".into(),
        ];
        let config = Config::parse(&words).expect("explicit fixture configuration");
        Self::launch(
            Some(scratch),
            home,
            config,
            fixture,
            matches!(fixture, Fixture::HangWithChild),
        )
    }
    fn launch(
        scratch: Option<aterm_tempfile::TempDir>,
        home: PathBuf,
        config: Config,
        fixture: Fixture,
        watch: bool,
    ) -> Self {
        let prepared = harness_local::prepare_fixture(
            Path::new(env!("CARGO_BIN_EXE_atpkg-publish")),
            &home,
            &config,
            watch,
            fixture,
        )
        .expect("prepare fixture");
        let mut command = aterm_uds::spawnfd::command(&prepared.program);
        command
            .args(&prepared.args)
            .env(atpkg::build_admit::LEDGER_ENV, home.join("capacity"))
            .env(atpkg::build_admit::ADMIT_ENV, "now")
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        command.stderr(
            aterm_uds::private_file::open_private_rw(&prepared.directory, "client.stderr")
                .expect("private client log"),
        );
        let child = command.spawn().expect("start native fixture client");
        let mut running = Self {
            scratch,
            home,
            config,
            launch: prepared.directory,
            source_record: prepared.source_record,
            child,
        };
        aterm_uds::deadline::prints(&mut running.child, "local fixture client running", HANG);
        running
    }
    fn other_prefix(&self, fixture: Fixture) -> Self {
        let mut config = self.config.clone();
        config.prefix = self.home.join("other-pkg");
        config.out = self.home.join("other-out");
        Self::launch(None, self.home.clone(), config, fixture, false)
    }
    fn root(&self) -> PathBuf {
        self.config
            .prefix
            .join(".local-harness-manager")
            .join(&self.config.program)
    }
    fn active(&self) -> Option<aterm_json::Value> {
        read(&self.root().join("active.json"))
    }
    fn attempt(&self) -> Option<PathBuf> {
        let active = self.active()?;
        Some(self.root().join(".ubd").join(format!(
            "attempt-{}.noindex",
            active.get("token")?.as_str()?
        )))
    }
    fn owner(&self) -> Option<String> {
        read(&self.launch.join("owner.json"))?
            .as_str()
            .map(str::to_owned)
    }
    fn retired(&self) -> bool {
        let Some(owner) = self.owner() else {
            return false;
        };
        let Some(active) = self.active() else {
            return false;
        };
        let Some(leader) = active
            .get("leader")
            .and_then(aterm_json::Value::as_str)
            .and_then(Identity::parse)
        else {
            return false;
        };
        active.get("settled").and_then(aterm_json::Value::as_bool) == Some(true)
            && process::group_members(leader.pid as u32)
                .is_some_and(|group| group.complete && group.pids.is_empty())
            && ended(&owner)
    }
    fn physically_retired(&self) -> bool {
        let Some(owner) = self.owner() else {
            return false;
        };
        if !ended(&owner) {
            return false;
        }
        let Some(active) = self.active() else {
            // A refused cross-prefix owner never published or spawned an attempt.
            return true;
        };
        let Some(leader) = active
            .get("leader")
            .and_then(aterm_json::Value::as_str)
            .and_then(Identity::parse)
        else {
            return false;
        };
        process::group_members(leader.pid as u32)
            .is_some_and(|group| group.complete && group.pids.is_empty())
    }
    fn finish_fixture(&self) {
        if let Some(dir) = self.attempt() {
            aterm_uds::private_file::write_private(&dir, "finish-fixture.json", b"true")
                .expect("cooperative controlled-body finish");
        }
    }
    fn assert_unchanged_harness(&self) {
        assert!(
            std::fs::symlink_metadata(self.config.prefix.join("store/claude-code-alab/current"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!self.config.prefix.join("agents").exists());
        let status = read(&self.root().join("status.json")).expect("status");
        assert!(status.get("compiled").expect("compiled field").is_null());
        assert!(status.get("selected").expect("selected field").is_null());
        assert_eq!(
            status
                .get("live_adoption")
                .and_then(aterm_json::Value::as_bool),
            Some(false)
        );
        let owner =
            Identity::parse(&self.owner().expect("recorded owner")).expect("owner identity");
        let ledger = aterm_capacity::Ledger::at(self.home.join("capacity"));
        let remaining = ledger
            .of_pid(owner.pid as u32)
            .expect("remaining owner claims");
        for entry in std::fs::read_dir(self.root().join(".ubd")).expect("private attempts") {
            let entry = entry.expect("attempt entry");
            if let Some(request) = read(&entry.path().join("request.json")) {
                let marker = request
                    .get("capacity")
                    .and_then(aterm_json::Value::as_str)
                    .expect("exact claim marker");
                assert!(
                    !remaining.iter().any(|claim| claim.cover_marker() == marker),
                    "retired attempt still reserves capacity"
                );
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        aterm_uds::deadline::kill_within(&mut self.child, HANG);
        self.finish_fixture();
        let by = Instant::now() + HANG;
        while Instant::now() < by && !self.physically_retired() {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !self.physically_retired()
            && let Some(scratch) = self.scratch.take()
        {
            let preserved = scratch.keep();
            eprintln!(
                "unproved guardian retirement preserves {}",
                preserved.display()
            );
        }
    }
}

#[test]
fn once_waits_for_the_real_worker_terminal_state_and_complete_group_retirement() {
    let mut running = Running::start(Fixture::Complete);
    let mut status = None;
    wait(|| {
        status = running.child.try_wait().expect("client wait");
        status.is_some()
    });
    assert!(status.expect("terminal status").success());
    wait(|| running.retired());
    running.assert_unchanged_harness();
    assert!(running.home.is_dir());
}

#[test]
fn client_sigkill_and_cooperative_stop_both_retire_the_leaf_before_releasing_ownership() {
    for hard in [true, false] {
        let mut running = Running::start(Fixture::HangWithChild);
        let mut leaf = None;
        wait(|| {
            leaf = running
                .attempt()
                .and_then(|dir| read(&dir.join("leaf-ready.json")))
                .and_then(|value| value.as_str().map(str::to_owned));
            leaf.is_some()
        });
        let leaf = leaf.expect("native leaf execution proof");
        assert!(
            !ended(&leaf),
            "negative control: the leaf really ran and remained alive"
        );
        if hard {
            running
                .child
                .kill()
                .expect("SIGKILL this owned unreaped client");
        } else {
            // SAFETY: this is our own still-unreaped child, whose PID cannot be reused.
            assert_eq!(
                unsafe { libc::kill(running.child.id() as i32, libc::SIGTERM) },
                0
            );
        }
        let mut terminal = None;
        wait(|| {
            terminal = running.child.try_wait().expect("client terminal state");
            terminal.is_some()
        });
        wait(|| running.retired());
        assert!(ended(&leaf));
        let status = read(&running.root().join("status.json")).expect("settled status");
        assert!(status.get("pending").expect("pending field").is_null());
        running.assert_unchanged_harness();
    }
}

#[test]
fn guardian_sigkill_keeps_other_prefixes_off_the_same_unretired_source() {
    let mut first = Running::start(Fixture::HangWithChild);
    let mut leaf = None;
    wait(|| {
        leaf = first
            .attempt()
            .and_then(|dir| read(&dir.join("leaf-ready.json")))
            .and_then(|value| value.as_str().map(str::to_owned));
        leaf.is_some()
    });
    let leaf = leaf.expect("actual descendant execution");
    let source = std::fs::read(&first.source_record).expect("source-wide active association");
    aterm_uds::private_file::write_private(&first.launch, "crash-owner.json", b"true")
        .expect("request this genuine fixture owner to kill itself");
    wait(|| first.owner().is_some_and(|owner| ended(&owner)));
    wait(|| first.child.try_wait().expect("first client exit").is_some());
    assert!(
        !ended(&leaf),
        "the orphan descendant is still physically live"
    );
    {
        let mut blocked = first.other_prefix(Fixture::Complete);
        let mut terminal = None;
        wait(|| {
            terminal = blocked.child.try_wait().expect("blocked client exit");
            terminal.is_some()
        });
        assert!(!terminal.expect("terminal refusal").success());
        wait(|| blocked.physically_retired());
        assert!(
            blocked.active().is_none(),
            "no second worker or claim was started"
        );
        assert_eq!(std::fs::read(&first.source_record).unwrap(), source);
        assert!(
            !ended(&leaf),
            "refusal must not signal the old unowned group"
        );
    }
    first.finish_fixture();
    wait(|| first.physically_retired());
    assert!(ended(&leaf));
    let mut next = first.other_prefix(Fixture::Complete);
    let mut terminal = None;
    wait(|| {
        terminal = next.child.try_wait().expect("new owner exit");
        terminal.is_some()
    });
    assert!(terminal.expect("new owner completed").success());
    wait(|| next.retired());
    next.assert_unchanged_harness();
}

#[test]
fn failed_stop_publication_retires_the_group_without_a_stop_record() {
    let mut running = Running::start(Fixture::FailedStopPublication);
    let mut terminal = None;
    wait(|| {
        terminal = running.child.try_wait().expect("failed-stop client exit");
        terminal.is_some()
    });
    assert!(!terminal.expect("stop failure is not success").success());
    wait(|| running.retired());
    let attempt = running.root().join(".ubd");
    let mut proved = false;
    for entry in std::fs::read_dir(attempt).expect("attempts") {
        let dir = entry.unwrap().path();
        if let Some(leaf) = read(&dir.join("leaf-ready.json")) {
            let leaf = leaf.as_str().expect("leaf identity");
            assert!(ended(leaf));
            assert!(!dir.join("stop.json").exists());
            assert!(!dir.join("worker-result.json").exists());
            proved = true;
        }
    }
    assert!(
        proved,
        "the actual sleeping descendant ran before the failed stop"
    );
    running.assert_unchanged_harness();
}

#[test]
fn a_success_bit_without_a_complete_terminal_ack_is_refused() {
    let mut running = Running::start(Fixture::IncompleteAck);
    let mut terminal = None;
    wait(|| {
        terminal = running
            .child
            .try_wait()
            .expect("incomplete-ack client exit");
        terminal.is_some()
    });
    assert!(!terminal.expect("incomplete terminal result").success());
    wait(|| running.retired());
    running.assert_unchanged_harness();
}
