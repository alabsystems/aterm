// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual start/cancel paths over RAM-only child/lock backends; no child is spawned.
use super::*;
use crate::port::{Fake, Ran, RunFailed, StagedChild, Stat};
use std::io;

#[derive(Clone, Copy)]
enum Fault {
    None,
    Metadata,
    Handoff,
    ForeignHolder,
    Unmeasured,
    Unwind,
}

struct Faults {
    inner: Fake,
    fault: Fault,
}

impl System for Faults {
    fn run(&self, c: &Cmd) -> Result<Ran, RunFailed> {
        self.inner.run(c)
    }
    fn run_tty(&self, c: &Cmd) -> Result<Option<i32>, RunFailed> {
        self.inner.run_tty(c)
    }
    fn read_file(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.inner.read_file(p)
    }
    fn write_file(&self, p: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        if p.ends_with("job.pid") {
            match self.fault {
                Fault::Metadata => return Err(io::Error::other("synthetic job.pid refusal")),
                Fault::Unwind => panic!("synthetic metadata unwind after stage"),
                Fault::ForeignHolder => {
                    self.inner
                        .file("/A/state/run.lock", b"pid=99 run=9-9 start=foreign\n");
                }
                Fault::None | Fault::Handoff | Fault::Unmeasured => {}
            }
        }
        if matches!(self.fault, Fault::Handoff) && p == Path::new("/A/state/run.lock") {
            return Err(io::Error::other("synthetic handoff refusal"));
        }
        self.inner.write_file(p, bytes, mode)
    }
    fn write_user_file(&self, u: &User, p: &Path, b: &[u8], m: u32) -> io::Result<()> {
        self.inner.write_user_file(u, p, b, m)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.inner.remove(p)
    }
    fn create_dir_all(&self, p: &Path, m: u32) -> io::Result<()> {
        self.inner.create_dir_all(p, m)
    }
    fn chown(&self, p: &Path, u: u32, g: u32) -> io::Result<()> {
        self.inner.chown(p, u, g)
    }
    fn stat(&self, p: &Path) -> io::Result<Stat> {
        self.inner.stat(p)
    }
    fn list_dir(&self, p: &Path) -> io::Result<Vec<String>> {
        self.inner.list_dir(p)
    }
    fn sha256(&self, p: &Path) -> io::Result<String> {
        self.inner.sha256(p)
    }
    fn copy_file(&self, f: &Path, t: &Path, m: u32) -> io::Result<()> {
        self.inner.copy_file(f, t, m)
    }
    fn rename(&self, f: &Path, t: &Path) -> io::Result<()> {
        self.inner.rename(f, t)
    }
    fn euid(&self) -> u32 {
        self.inner.euid()
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
    fn create_new(&self, p: &Path, b: &[u8], m: u32) -> io::Result<()> {
        self.inner.create_new(p, b, m)
    }
    fn stage_detached(&self, c: &Cmd, p: &Path) -> Result<Box<dyn StagedChild + '_>, RunFailed> {
        self.inner.stage_detached(c, p)
    }
    fn await_start(&self, limit: Duration) -> Result<(), RunFailed> {
        self.inner.await_start(limit)
    }
    fn spawn_detached(&self, c: &Cmd, p: &Path) -> Result<u32, RunFailed> {
        self.inner.spawn_detached(c, p)
    }
}

fn setup(fault: Fault) -> (Faults, Layout, Opened) {
    let mut inner = Fake::root();
    inner.next_pid = 4242;
    inner.file("/A/state/run.lock", b"pid=17 run=1-5 start=parent\n");
    if !matches!(fault, Fault::Unmeasured) {
        inner.on(
            "/usr/bin/env TZ=UTC0 /bin/ps -o lstart= -p 4242",
            Ran::ok("known child birth\n"),
        );
    }
    let opened = Opened {
        envelope: envelope::parse_body(
            b"schema=1\nhost='U'\nhost_name='h'\nserial=5\nissued=1\nexpires=2\naction='facts'\n",
        )
        .unwrap(),
        principal: "synthetic@ctl".into(),
        key_id: "synthetic".into(),
    };
    (
        Faults { inner, fault },
        Layout { root: "/A".into() },
        opened,
    )
}

fn launch(sys: &Faults, layout: &Layout, opened: &Opened) -> Result<(), String> {
    let mut owner = StartingRun::claim(sys, layout, "1-5", 17)?;
    start(
        sys,
        layout,
        Path::new("/A/agent/aterm"),
        b"synthetic wire",
        opened,
        "1-5",
        &[],
        &mut owner,
    )
}

#[test]
fn a_start_commits_only_after_metadata_and_exact_lock_handoff() {
    let (sys, layout, opened) = setup(Fault::None);
    launch(&sys, &layout, &opened).unwrap();
    let holder = state::holder(&sys, &layout).unwrap();
    assert_eq!(
        holder,
        state::Holder {
            pid: 4242,
            run: "1-5".into(),
            start: Some("known_child_birth".into())
        }
    );
    assert_eq!(
        sys.inner
            .read_file(Path::new("/A/state/runs/1-5/job.pid"))
            .unwrap(),
        b"4242\n"
    );
    let calls = sys.inner.calls();
    let at = |needle: &str| {
        calls
            .iter()
            .position(|call| call == needle)
            .unwrap_or_else(|| panic!("{needle}: {calls:?}"))
    };
    assert!(at("write /A/state/runs/1-5/job.pid 644") < at("write /A/state/run.lock 644"));
    assert!(at("write /A/state/run.lock 644") < at("stage committed 4242"));
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("stage aborted") || call == "remove /A/state/run.lock")
    );
}

#[test]
fn an_uncommitted_start_aborts_before_releasing_its_exact_holder() {
    let (sys, layout, opened) = setup(Fault::None);
    launch(&sys, &layout, &opened).unwrap();
    assert!(
        sys.inner
            .calls()
            .iter()
            .any(|call| call == "stage committed 4242")
    );
    let mut observations = Vec::new();
    for fault in [
        Fault::Metadata,
        Fault::Handoff,
        Fault::Unmeasured,
        Fault::None,
    ] {
        let (sys, layout, opened) = setup(fault);
        if matches!(fault, Fault::None) {
            sys.inner.stage_commit_refuses(true);
        }
        let result = launch(&sys, &layout, &opened);
        let calls = sys.inner.calls();
        let abort = calls.iter().position(|call| call == "stage aborted 4242");
        let release = calls
            .iter()
            .position(|call| call == "remove /A/state/run.lock");
        observations.push((
            result.is_err(),
            state::holder(&sys, &layout).is_none(),
            matches!((abort,release),(Some(a),Some(r)) if a < r),
            !calls.iter().any(|call| call == "stage committed 4242"),
        ));
    }
    assert_eq!(
        observations,
        vec![(true, true, true, true); 4],
        "all actual failure exits preserve abort-before-unlock ordering"
    );
}

#[test]
fn unproven_abort_and_replaced_holders_never_release_a_run_lock() {
    for (fault, pid) in [(Fault::Metadata, 17), (Fault::None, 4242)] {
        let (sys, layout, opened) = setup(fault);
        sys.inner
            .stage_abort_refuses(true)
            .stage_commit_refuses(true);
        let why = launch(&sys, &layout, &opened).unwrap_err();
        assert!(
            why.contains("cleanup unproven") && why.contains("lock retained"),
            "{why}"
        );
        assert_eq!(state::holder(&sys, &layout).unwrap().pid, pid);
        let calls = sys.inner.calls();
        assert!(calls.iter().any(|call| call == "stage abort unproven 4242"));
        assert!(calls.iter().any(|call| call == "stage reaper custody 4242"));
        assert!(!calls.iter().any(|call| call == "remove /A/state/run.lock"));
    }

    let (sys, layout, opened) = setup(Fault::ForeignHolder);
    let why = launch(&sys, &layout, &opened).unwrap_err();
    assert!(why.contains("holder changed"), "{why}");
    assert_eq!(
        sys.inner.read_file(&layout.lock()).unwrap(),
        b"pid=99 run=9-9 start=foreign\n"
    );
    let calls = sys.inner.calls();
    assert!(calls.iter().any(|call| call == "stage aborted 4242"));
    assert!(
        !calls
            .iter()
            .any(|call| call == "remove /A/state/run.lock" || call == "stage committed 4242")
    );
}

#[test]
fn unwinding_after_stage_keeps_abort_before_unlock_custody() {
    let (sys, layout, opened) = setup(Fault::Unwind);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        launch(&sys, &layout, &opened)
    }));
    assert!(result.is_err());
    let calls = sys.inner.calls();
    let a = calls
        .iter()
        .position(|call| call == "stage aborted 4242")
        .unwrap();
    let r = calls
        .iter()
        .position(|call| call == "remove /A/state/run.lock")
        .unwrap();
    assert!(a < r);
    assert!(state::holder(&sys, &layout).is_none());
    assert!(!calls.iter().any(|call| call == "stage committed 4242"));
}
