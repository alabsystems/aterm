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
    defer_return: bool,
    cancelled: usize,
    retired: usize,
    before_arm_error: bool,
    launches: usize,
    #[cfg(target_os = "macos")]
    producer_returns: usize,
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
            defer_return: false,
            cancelled: 0,
            retired: 0,
            before_arm_error: false,
            launches: 0,
            #[cfg(target_os = "macos")]
            producer_returns: 0,
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
    #[cfg(target_os = "macos")]
    fn bind_producer(&mut self, inputs: &mut ProducerInputs) -> io::Result<()> {
        self.validate()?;
        inputs.bind_fixture(&mut self.root, binding())
    }
    #[cfg(target_os = "macos")]
    fn retire_producer(
        &mut self,
        child: &mut CohortChild,
        inputs: &mut Option<ProducerInputs>,
    ) -> io::Result<ProducerRetirement> {
        if self.defer_retire {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let retired = ProducerRetirement::retire_fixture(
            &mut self.claim,
            &mut self.root,
            child,
            inputs,
            binding(),
        )?;
        self.retired += 1;
        Ok(retired)
    }
    #[cfg(target_os = "macos")]
    fn return_producer(
        &mut self,
        retirement: Box<ProducerRetirement>,
    ) -> Result<Box<ReturnedProducer>, Box<ProducerRetirement>> {
        let returned = (|| {
            self.validate()?;
            retirement.check_fixture(&mut self.root, &binding())?;
            self.return_unit(ReturnReceipt::ScopeClosed)
        })();
        match returned {
            Ok(()) => {
                self.producer_returns += 1;
                Ok(retirement.returned_fixture())
            }
            Err(_) => Err(retirement),
        }
    }
    fn before_arm(&mut self) -> io::Result<()> {
        if self.before_arm_error {
            Err(io::Error::other("injected intent acknowledgment loss"))
        } else {
            Ok(())
        }
    }
    fn before_go(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn launch(&mut self, command: &mut spawnfd::Command) -> Result<CohortChild, LaunchError> {
        self.launches += 1;
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
        self.cancelled += 1;
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
        self.claim.retire_subgroup(child)?;
        self.retired += 1;
        Ok(())
    }
    fn return_unit(&mut self, _: ReturnReceipt) -> io::Result<()> {
        if self.defer_return {
            Err(io::Error::other("lost durable return acknowledgment"))
        } else {
            Ok(())
        }
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
    let deadline = Instant::now() + Duration::from_secs(60);
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

// A member is held before GO by a start stopped at its exec, which only Darwin's
// spawn does: elsewhere its launch is an affirmative no-child receipt.
#[test]
#[cfg(target_os = "macos")]
fn actual_bound_retry_keeps_descriptors_until_scoped_retirement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::actual_bound_retry_keeps_descriptors_until_scoped_retirement",
    ) {
        return;
    }
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
        Some(Duration::from_secs(60)),
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
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let progress = member.poll(&mut fixture, None);
        if progress.completed.is_some() {
            assert!(!progress.retired);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        member.group().is_none(),
        "a reaped leader no longer pins its numeric group for memory attribution"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    client.set_nonblocking(true).unwrap();
    let mut view = &client;
    assert!(
        matches!(std::io::Read::read(&mut view,&mut [0;1]),Err(error) if error.kind()==io::ErrorKind::WouldBlock),
        "Finished must not return the caller's borrowed unit before scoped retirement"
    );
    fixture.defer_retire = false;
    fixture.defer_return = true;
    assert!(!member.poll(&mut fixture, None).retired);
    assert_eq!(fixture.retired, 1);
    assert!(!member.poll(&mut fixture, None).retired);
    assert_eq!(
        fixture.retired, 1,
        "return retry must not re-retire native scope"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    fixture.defer_return = false;
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
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::exact_no_child_receipt_voids_only_its_original_subgroup",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.defer_return = true;
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
    assert!(!member.poll(&mut fixture, None).retired);
    assert!(member.scope_closed);
    assert_eq!(fixture.cancelled, 1);
    assert!(!member.poll(&mut fixture, None).retired);
    assert_eq!(
        fixture.cancelled, 1,
        "return retry must not reconstruct NoChild"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    fixture.defer_return = false;
    let progress = done(&mut member, &mut fixture);
    assert!(progress.retired && progress.completed.is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(fixture.claim.finish_cohort().is_err());
}
#[test]
fn before_arm_write_failure_retries_return_without_launching_a_native_child() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::before_arm_write_failure_retries_return_without_launching_a_native_child",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.before_arm_error = true;
    fixture.defer_return = true;
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        spawnfd::command("/usr/bin/true"),
        Resources(dropped.clone(), None),
        events,
        binding(),
        None,
        wake,
    );
    assert!(member.scope_closed);
    assert_eq!(fixture.launches, 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(!member.poll(&mut fixture, None).retired);
    assert_eq!(fixture.launches, 0);
    fixture.defer_return = false;
    assert!(done(&mut member, &mut fixture).retired);
    assert_eq!(fixture.cancelled, 0);
    assert_eq!(fixture.retired, 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}
#[test]
fn cancellation_before_go_does_not_run_target_or_release_early() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::cancellation_before_go_does_not_run_target_or_release_early",
    ) {
        return;
    }
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
#[cfg(target_os = "macos")]
fn live_member_coverage_and_outer_timeout_preserve_original_scope() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::live_member_coverage_and_outer_timeout_preserve_original_scope",
    ) {
        return;
    }
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
    assert!(scope_covers_at(
        peer,
        pid,
        birth,
        Instant::now() - Duration::from_secs(1),
    ));
    assert!(!scope_covers_at(
        peer,
        pid,
        birth,
        Instant::now() - Duration::from_secs(60),
    ));
    assert!(!member.covers(Peer {
        pid,
        birth: birth ^ 1
    }));
    let root_pid = fixture.root.child_mut().id();
    let root_birth = process::read_birth(root_pid as i32).unwrap().0;
    assert!(scope_covers_at(
        Peer {
            pid: root_pid,
            birth: root_birth
        },
        root_pid,
        root_birth,
        Instant::now() - Duration::from_secs(1),
    ));
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
#[cfg(target_os = "macos")]
fn exited_leader_keeps_group_pin_until_other_members_are_positively_exited() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::exited_leader_keeps_group_pin_until_other_members_are_positively_exited",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let marker = fixture._scratch.path().join("background-held");
    let (input, mut hold) = aterm_uds::kept_out(std::io::pipe).unwrap();
    let shell = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sh"));
    let mut command = spawnfd::command(shell);
    command
        .args(["-c", "exec 3<&0; printf 'root\\n' > \"$1\"; while IFS= read -r line <&3; do printf '%s\\n' \"$line\" > \"$1\"; done & exit 0", "fixture"])
        .arg(&marker)
        .stdin(spawnfd::Stdio::from(input))
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null());
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
    std::io::Write::write_all(&mut hold, b"held\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let State::Bound(bound) = &mut member.state else {
            panic!("actual bound member")
        };
        if bound.child.child_mut().exited().unwrap()
            && bound.exit.as_ref().and_then(ExitMoment::get).is_some()
            && std::fs::read(&marker).is_ok_and(|bytes| bytes == b"held\n")
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
    assert!(
        member.producer_retirement.is_none(),
        "group drain is not producer containment"
    );
    let finished = done(&mut member, &mut fixture);
    assert!(finished.retired);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
#[cfg(target_os = "macos")]
fn native_escaped_metadata_actor() {
    let Some(path) = std::env::var_os("ATERM_TEST_METADATA_ESCAPE") else {
        return;
    };
    let name =
        std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str())).unwrap();
    // SAFETY: the child uses only stack buffers and async-signal-safe calls
    // before _exit. The test owns its sole EOF and this process's fork.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        std::fs::write(&path, "fork-denied").unwrap();
        return;
    }
    if pid == 0 {
        unsafe {
            if libc::setsid() < 0 {
                libc::_exit(91);
            }
            let mut digits = [0_u8; 20];
            let mut cursor = digits.len();
            let mut value = libc::getpid() as u32;
            loop {
                cursor -= 1;
                digits[cursor] = b'0' + (value % 10) as u8;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            let fd = libc::open(
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o600,
            );
            if fd < 0 {
                libc::_exit(92);
            }
            libc::write(fd, digits[cursor..].as_ptr().cast(), digits.len() - cursor);
            libc::close(fd);
            let mut byte = 0u8;
            loop {
                let got = libc::read(19, (&mut byte as *mut u8).cast(), 1);
                if got == 0 {
                    break;
                }
                if got < 0 {
                    libc::_exit(93);
                }
            }
            libc::_exit(0);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while !std::path::Path::new(&path).is_file() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(target_os = "macos")]
struct GuardedResources {
    original: Resources,
    prelude: crate::metadata_prelude::Owner,
    gnu: [OwnedFd; 2],
}
#[cfg(target_os = "macos")]
impl MemberResources for GuardedResources {
    fn take_producer_inputs(&mut self) -> io::Result<Option<ProducerInputs>> {
        let target = self
            .prelude
            .take_retirement()
            .ok_or_else(|| io::Error::other("native containment missing"))?;
        let gnu = self
            .gnu
            .iter()
            .map(OwnedFd::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        ProducerInputs::new(target, gnu).map(Some)
    }
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        let extra = self.prelude.extra()?;
        command.inherit_guards_at(vec![
            extra,
            (self.gnu[0].try_clone()?, 3),
            (self.gnu[1].try_clone()?, 4),
        ])?;
        self.prelude.configured(command)
    }
    fn prepare_target(&mut self, child: &mut spawnfd::Child) -> io::Result<bool> {
        self.prelude.poll(child, |pid| {
            let loaded = process::LoadedImage::read(pid as i32)
                .ok_or_else(|| io::Error::other("selected image absent"))?;
            if loaded.matches_file(&std::env::current_exe()?) != Some(true) {
                return Err(io::Error::other("selected image differs"));
            }
            Ok(())
        })
    }
    fn bind_child(&mut self, pid: u32) -> io::Result<()> {
        self.original.bind_child(pid)
    }
}
#[test]
#[cfg(target_os = "macos")]
fn metadata_cannot_return_original_resources_while_a_setsid_child_survives() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::metadata_cannot_return_original_resources_while_a_setsid_child_survives",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut fixture = Fixture::new();
    let path = fixture._scratch.path().join("escaped-pid");
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .args([
            "--exact",
            "actor::owner::member::tests::native_escaped_metadata_actor",
            "--nocapture",
        ])
        .env_clear()
        .env("ATERM_TEST_METADATA_ESCAPE", &path)
        .current_dir(fixture._scratch.path());
    let deadline = Instant::now() + Duration::from_secs(60);
    let (prelude, mut command) =
        crate::metadata_prelude::Owner::new(&target, &[3, 4], deadline).unwrap();
    command
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null());
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        GuardedResources {
            original: Resources(dropped.clone(), None),
            prelude,
            gnu: {
                let (read, write) = aterm_uds::kept_out(std::io::pipe).unwrap();
                [read.into(), write.into()]
            },
        },
        events,
        binding(),
        Some(Duration::from_secs(60)),
        wake,
    );
    let original = member.identity().unwrap();
    loop {
        let progress = member.poll(&mut fixture, None);
        if progress.ready_to_start {
            match member.start(&mut fixture) {
                Ok(started) => {
                    assert_eq!((started.pid, started.birth), original);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("native metadata two-hold preparation: {error}"),
            }
        }
        assert!(Instant::now() < deadline);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "original loan stays held through preparation"
        );
        assert!(
            !path.exists(),
            "selected target has not reached GO during preparation"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    loop {
        let progress = member.poll(&mut fixture, None);
        if progress.drained {
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "fork-denied",
                "same native setsid adversary cannot create an escaped descendant"
            );
            assert!(matches!(
                progress.completed.unwrap().outcome,
                Outcome::Exit(0)
            ));
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert_eq!(
                fixture.producer_returns, 1,
                "actual no-fork retire result consumed once"
            );
            assert!(
                fixture.claim.finish_cohort().is_err(),
                "the live original root still retains its memory claim"
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[cfg(target_os = "macos")]
fn general_producer_profile_refuses_before_go_and_never_mints_retirement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::general_producer_profile_refuses_before_go_and_never_mints_retirement",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.defer_retire = true;
    let marker = fixture._scratch.path().join("must-not-run");
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
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
    let error = member.start_producer(&mut fixture).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!marker.exists());
    assert!(member.producer_retirement.is_none());
    assert_eq!(fixture.producer_returns, 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    fixture.defer_retire = false;
    let progress = done(&mut member, &mut fixture);
    assert!(!progress.go_attempted);
    assert_eq!(fixture.producer_returns, 0);
}

#[test]
#[cfg(target_os = "macos")]
fn native_producer_retirement_retains_originals_until_same_owner_return() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::native_producer_retirement_retains_originals_until_same_owner_return",
    ) {
        return;
    }
    native_producer_return_control(false);
}

#[test]
#[cfg(target_os = "macos")]
fn lost_original_root_cannot_consume_producer_retirement_or_drop_gnu() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::lost_original_root_cannot_consume_producer_retirement_or_drop_gnu",
    ) {
        return;
    }
    native_producer_return_control(true);
}

#[cfg(target_os = "macos")]
fn native_producer_return_control(lose_owner: bool) {
    crate::runtime::test_endpoint().unwrap();
    let mut fixture = Fixture::new();
    fixture.defer_retire = true;
    fixture.defer_return = true;
    let path = fixture._scratch.path().join("fork-denied");
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .args([
            "--exact",
            "actor::owner::member::tests::native_escaped_metadata_actor",
            "--nocapture",
        ])
        .env_clear()
        .env("ATERM_TEST_METADATA_ESCAPE", &path)
        .current_dir(fixture._scratch.path());
    let deadline = Instant::now() + Duration::from_secs(60);
    let (prelude, mut command) =
        crate::metadata_prelude::Owner::new(&target, &[3, 4], deadline).unwrap();
    command
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null());
    let (read, write) = aterm_uds::kept_out(std::io::pipe).unwrap();
    let mut member = OwnedAttempt::launch(
        &mut fixture,
        command,
        GuardedResources {
            original: Resources(dropped.clone(), None),
            prelude,
            gnu: [read.into(), write.into()],
        },
        events,
        binding(),
        None,
        wake,
    );
    loop {
        match member.start_producer(&mut fixture) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("actual contained producer start: {error}"),
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    loop {
        let progress = member.poll(&mut fixture, None);
        if progress.completed.is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        member.producer_retirement.is_none(),
        "reap alone cannot mint native retirement"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    fixture.defer_retire = false;
    member.poll(&mut fixture, None);
    let retirement = member
        .producer_retirement
        .as_ref()
        .expect("actual durable retirement");
    let mut wrong = Fixture::new();
    assert!(
        retirement
            .check_fixture(&mut wrong.root, &binding())
            .is_err()
    );
    assert_eq!(fixture.producer_returns, 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    if lose_owner {
        use std::os::fd::AsRawFd as _;
        let probe = member.resources.as_ref().unwrap().gnu[0]
            .try_clone()
            .unwrap();
        // Reap only this fixture's actual original root, never a numeric PID.
        fixture.input.take();
        fixture.root.child_mut().wait().unwrap();
        fixture.defer_return = false;
        let progress = member.poll(&mut fixture, None);
        assert!(!progress.retired && progress.uncertain);
        assert!(member.producer_retirement.is_some());
        assert_eq!(fixture.producer_returns, 0);
        drop(member);
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "unknown owner retains originals"
        );
        let mut descriptor = libc::pollfd {
            fd: probe.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut descriptor, 1, 0) },
            0,
            "SAME original GNU writer remains held after unresolved owner drop"
        );
        return;
    }
    fixture.defer_return = false;
    loop {
        if member.poll(&mut fixture, None).drained {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(fixture.producer_returns, 1);
    assert!(member.producer_retirement.is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    member.poll(&mut fixture, None);
    assert_eq!(
        fixture.producer_returns, 1,
        "one original retirement cannot return twice"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn escaped_closed_stream_descendant_cannot_create_producer_retirement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::escaped_closed_stream_descendant_cannot_create_producer_retirement",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let marker = fixture._scratch.path().join("escaped");
    let (reader, writer) = aterm_uds::kept_out(std::io::pipe).unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let (events, _client) = event_pair();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "actor::owner::member::tests::native_escaped_metadata_actor",
            "--nocapture",
        ])
        .env_clear()
        .env("ATERM_TEST_METADATA_ESCAPE", &marker)
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null());
    command
        .inherit_guards_at(vec![(reader.into(), 19)])
        .unwrap();
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let escaped = loop {
        if let Ok(text) = std::fs::read_to_string(&marker)
            && let Ok(pid) = text.parse::<u32>()
        {
            break pid;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    let exit = ExitMoment::watch_then(escaped, Instant::now, wake)
        .expect("actual escaped child exit observer");
    let State::Bound(bound) = &mut member.state else {
        panic!("original child")
    };
    assert_ne!(
        unsafe { libc::getpgid(escaped as i32) },
        bound.child.cohort().group()
    );
    assert!(bound.producer.is_none());
    assert!(
        exit.get().is_none(),
        "closed-stream escaped descendant is genuinely live"
    );
    let mut absent = None;
    let result = fixture.retire_producer(&mut bound.child, &mut absent);
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Unsupported));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert!(member.producer_retirement.is_none());
    // The fixture owns the original stop writer. EOF closes the escaped child;
    // no recycled PID/group signal and no alleged production closure is used.
    drop(writer);
    while exit.get().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    done(&mut member, &mut fixture);
    assert_eq!(fixture.producer_returns, 0);
}

#[test]
#[cfg(target_os = "macos")]
fn accounted_member_retains_original_wait_until_scope_and_unit_return() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::member::tests::accounted_member_retains_original_wait_until_scope_and_unit_return",
    ) {
        return;
    }
    use aterm_types::resource_admission::AccountedEventFrame;
    for (script, cancel, expected) in [
        ("exit 0", false, Some(0)),
        ("exit 7", false, Some(7)),
        ("exec /bin/sleep 60", true, None),
    ] {
        let mut fixture = Fixture::new();
        fixture.defer_return = true;
        let dropped = Arc::new(AtomicUsize::new(0));
        let (mut events, client) = event_pair();
        events.account().unwrap();
        let mut command = spawnfd::command("/bin/sh");
        command.arg("-c").arg(script);
        let mut member = OwnedAttempt::launch(
            &mut fixture,
            command,
            Resources(dropped.clone(), None),
            events,
            binding(),
            Some(Duration::from_secs(60)),
            wake,
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        while !member.poll(&mut fixture, None).ready_to_start {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        member.start(&mut fixture).unwrap();
        for _ in 0..2 {
            aterm_ctl::resource_admission_transport::read_frame(&client, deadline).unwrap();
        }
        if cancel {
            aterm_ctl::resource_admission_transport::send_cancel(
                &client,
                aterm_types::resource_admission::Cancel { binding: binding() },
                deadline,
            )
            .unwrap();
        }
        loop {
            let progress = member.poll(&mut fixture, None);
            if progress.completed.is_some() && member.scope_closed {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "original resources remain held before durable unit return"
        );
        let State::Bound(bound) = &mut member.state else {
            panic!("original child binding lost")
        };
        let cached = bound
            .child
            .child_mut()
            .try_wait_with_rusage()
            .unwrap()
            .unwrap();
        let observed = bound.completed.as_ref().unwrap().accounting;
        assert_eq!(
            observed.cpu_times.unwrap().user_ns,
            u64::try_from(cached.user.as_nanos()).unwrap()
        );
        assert_eq!(
            observed.cpu_times.unwrap().system_ns,
            u64::try_from(cached.system.as_nanos()).unwrap()
        );
        fixture.defer_return = false;
        assert!(done(&mut member, &mut fixture).drained);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        let frame = AccountedEventFrame::decode(
            &aterm_ctl::resource_admission_transport::read_frame(&client, deadline).unwrap(),
            &binding(),
        )
        .unwrap();
        assert_eq!(frame.observation, observed);
        let Event::Finished { outcome, cause, .. } = frame.finished.event else {
            panic!("no accounted member terminal")
        };
        if let Some(code) = expected {
            assert_eq!(outcome, Outcome::Exit(code));
        } else {
            assert!(matches!(outcome, Outcome::Signal(_)));
        }
        assert_eq!(
            cause,
            if cancel {
                CompletionCause::Cancelled
            } else {
                CompletionCause::Exited
            }
        );
    }
}
