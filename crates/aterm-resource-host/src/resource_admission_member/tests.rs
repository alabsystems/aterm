// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::cohort_namespace::Namespace;
use aterm_capacity::{Claim, Ledger, Machine, Need, Polled, Procs, Reading, Waiter};
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Room;
impl Machine for Room {
    fn read(&self, _: Option<&std::path::Path>) -> Reading {
        Reading {
            cores: 8,
            load1: Some(0.0),
            load5: Some(0.0),
            available: Some(40 << 30),
            physical: Some(64 << 30),
            swap_used: Some(0),
            swap_total: Some(0),
            ..Reading::default()
        }
    }
    fn procs(&self) -> Option<Procs> {
        None
    }
}
struct Fixture {
    _scratch: aterm_tempfile::TempDir,
    claim: Claim,
    root: CohortChild,
    input: Option<std::process::ChildStdin>,
    defer_bind: bool,
    defer_retire: bool,
}
impl Fixture {
    fn new() -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("member-")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let home = scratch.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        let mut claim = match Waiter::enter(&ledger, &Room, Need::new("member-test").mem_gib(1))
            .unwrap()
            .poll(&Room)
        {
            Polled::Admitted(claim) => claim,
            other => panic!("private fixture admission: {other:?}"),
        };
        let mut command = spawnfd::command("/bin/cat");
        command
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::null());
        let mut root = claim.launch_cohort(&mut command).unwrap();
        let input = root.child_mut().stdin.take();
        Self {
            _scratch: scratch,
            claim,
            root,
            input,
            defer_bind: false,
            defer_retire: false,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.root.request_group(CohortSignal::Kill);
        let _ = self.root.child_mut().wait();
        let _ = self.claim.finish_cohort();
    }
}
impl MemberCustody for Fixture {
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError> {
        let child = self.claim.launch_subgroup(&mut self.root, command)?;
        if self.defer_bind {
            self.defer_bind = false;
            let (spawned, _) = child.into_parts();
            return Err(LaunchError::Unbound {
                spawned: Box::new(spawned),
                error: io::Error::other("lost bind acknowledgement"),
            });
        }
        Ok(child)
    }
    fn bind(&mut self, child: spawnfd::Spawned) -> Result<CohortChild, LaunchError> {
        self.claim.bind_subgroup(child)
    }
    fn cancel_no_child(&mut self, proof: spawnfd::NotSpawned) -> Result<(), CancelError> {
        if let Err(error) = self.validate() {
            return Err(CancelError::Refused {
                error,
                proof: Box::new(proof),
            });
        }
        self.claim
            .cancel_subgroup(proof)
            .map_err(CancelError::Retained)
    }
    fn retire(&mut self, child: &CohortChild) -> io::Result<()> {
        if self.defer_retire {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.claim.retire_subgroup(child)
    }
    fn validate(&mut self) -> io::Result<()> {
        if self.claim.cohort_bound(self.root.cohort()) && !self.root.child_mut().exited()? {
            Ok(())
        } else {
            Err(io::Error::other("original root is not current"))
        }
    }
}
struct Resources(Arc<AtomicUsize>, Option<std::io::PipeWriter>);
impl Drop for Resources {
    fn drop(&mut self) {
        self.1.take();
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl MemberResources for Resources {
    fn configure(&mut self, _: &mut spawnfd::Command) -> io::Result<()> {
        Ok(())
    }
    fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        let identity = process::read_identity(i32::try_from(pid).unwrap())
            .ok_or_else(|| io::Error::other("actual child missing"))?;
        if identity.pid == pid as i32 {
            Ok(())
        } else {
            Err(io::Error::other("different child"))
        }
    }
}
fn event_pair() -> (Events, aterm_uds::CtlStream) {
    let (server, client) = aterm_uds::CtlStream::pair().unwrap();
    let mut events = Events::new(OwnedFd::from(server), binding()).unwrap();
    events.emit(Event::Queued).unwrap();
    (events, client)
}
fn binding() -> Binding {
    Binding {
        session: "s-a".into(),
        attempt: [1; 16],
    }
}
fn wake() {}
fn done(member: &mut OwnedAttempt<Resources>, fixture: &mut Fixture) -> Progress {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = member.poll(fixture, None);
        if progress.drained {
            return progress;
        }
        assert!(
            Instant::now() < deadline,
            "actual member must retire within hang detector"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn actual_bound_retry_keeps_descriptors_until_scoped_retirement() {
    let mut fixture = Fixture::new();
    fixture.defer_bind = true;
    fixture.defer_retire = true;
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, client) = event_pair();
    let command = spawnfd::command("/usr/bin/true");
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        Resources(dropped.clone(), None),
        events,
        binding(),
        Some(Duration::from_secs(30)),
        wake,
    );
    assert!(member.identity().is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let ready = member.poll(&mut fixture, None);
    assert!(ready.ready_to_start && !ready.uncertain);
    assert!(member.group().is_some());
    member.start(&mut fixture).unwrap();
    for _ in 0..2 {
        aterm_ctl::resource_admission_transport::read_frame(
            &client,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let progress = member.poll(&mut fixture, None);
        if progress.completed.is_some() {
            assert!(!progress.retired);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    client.set_nonblocking(true).unwrap();
    let mut view = &client;
    assert!(
        matches!(std::io::Read::read(&mut view,&mut [0;1]),Err(error) if error.kind()==io::ErrorKind::WouldBlock),
        "Finished must not return the caller's borrowed unit before scoped retirement"
    );
    fixture.defer_retire = false;
    let progress = done(&mut member, &mut fixture);
    assert!(matches!(
        progress.completed.unwrap(),
        Completed {
            outcome: Outcome::Exit(0),
            cause: CompletionCause::Exited,
            ..
        }
    ));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(member.group().is_none());
    assert!(
        fixture.claim.finish_cohort().is_err(),
        "live original root still owns memory"
    );
}
#[test]
fn exact_no_child_receipt_voids_only_its_original_subgroup() {
    let mut fixture = Fixture::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        spawnfd::command("/definitely-missing-member"),
        Resources(dropped.clone(), None),
        events,
        binding(),
        None,
        wake,
    );
    let progress = done(&mut member, &mut fixture);
    assert!(progress.retired && progress.completed.is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(fixture.claim.finish_cohort().is_err());
}
#[test]
fn cancellation_before_go_does_not_run_target_or_release_early() {
    let mut fixture = Fixture::new();
    let marker = fixture._scratch.path().join("must-not-run");
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, client) = event_pair();
    let mut command = spawnfd::command("/usr/bin/touch");
    command.arg(&marker);
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        Resources(dropped.clone(), None),
        events,
        binding(),
        None,
        wake,
    );
    drop(client);
    assert!(member.start(&mut fixture).is_err());
    assert!(!marker.exists());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let result = done(&mut member, &mut fixture);
    assert!(result.completed.is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(!marker.exists());
}

#[test]
fn live_member_coverage_and_outer_timeout_preserve_original_scope() {
    let mut fixture = Fixture::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let (read, write) = std::io::pipe().unwrap();
    let mut command = spawnfd::command("/bin/cat");
    command
        .stdin(spawnfd::Stdio::from(std::fs::File::from(OwnedFd::from(
            read,
        ))))
        .stdout(spawnfd::Stdio::null());
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        Resources(dropped.clone(), Some(write)),
        events,
        binding(),
        None,
        wake,
    );
    let (pid, birth) = member.identity().unwrap();
    let peer = Peer { pid, birth };
    assert_eq!(member.binding(), &binding());
    assert!(
        !member.covers(peer),
        "held-before-GO is not running member coverage"
    );
    member.start(&mut fixture).unwrap();
    assert!(member.covers(peer));
    assert!(!member.covers(Peer {
        pid,
        birth: birth ^ 1
    }));
    let root_pid = fixture.root.child_mut().id();
    let root_birth = process::read_birth(root_pid as i32).unwrap().0;
    assert!(
        !member.covers(Peer {
            pid: root_pid,
            birth: root_birth
        }),
        "a sibling original root is not member coverage"
    );
    member.poll(&mut fixture, Some(CompletionCause::Timeout));
    let result = done(&mut member, &mut fixture);
    assert_eq!(result.completed.unwrap().cause, CompletionCause::Timeout);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn exited_leader_keeps_group_pin_until_other_members_are_positively_exited() {
    let mut fixture = Fixture::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut command = spawnfd::command("/bin/sh");
    command
        .args(["-c", "/bin/sleep 30 & exit 0"])
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null());
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        Resources(dropped.clone(), None),
        events,
        binding(),
        None,
        wake,
    );
    member.start(&mut fixture).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let State::Bound(bound) = &mut member.state else {
            panic!("actual bound member")
        };
        if bound.child.child_mut().exited().unwrap()
            && bound.exit.as_ref().and_then(ExitMoment::get).is_some()
        {
            assert!(
                !crate::resource_memory::other_members_exited(bound.pid),
                "controlled background child is still live"
            );
            bound.cleanup_at = Instant::now().checked_sub(Duration::from_secs(3));
            bound.cause = CompletionCause::Timeout;
            break;
        }
        assert!(
            Instant::now() < deadline,
            "controlled leader exits before hang detector"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let first = member.poll(&mut fixture, None);
    let State::Bound(bound) = &mut member.state else {
        panic!("actual member receipt")
    };
    assert!(
        !bound.reaped && !bound.child.child_mut().is_reaped(),
        "KILL request alone is not a drain census"
    );
    assert!(!first.retired);
    assert!(first.go_attempted && first.launched);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let finished = done(&mut member, &mut fixture);
    assert!(finished.retired);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}
