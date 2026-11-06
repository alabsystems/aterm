// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;

use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_toolchain::record_lock::RecordLock;

use crate::{
    Accounting, Admit, Claim, Fixed, Ledger, Need, Pace, Polled, Proc, Procs, Reading,
    RetainedClaim, Short, Waiter,
};

const OWNER_ACTOR: &str = "ATERM_RETAINED_MEMORY_OWNER_ACTOR";
const OWNER_PENDING: &str = "ATERM_RETAINED_MEMORY_PENDING_ACTOR";

enum ActorGuard {
    Ready(Box<RetainedClaim>),
    Pending(Box<Claim>),
}
impl ActorGuard {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Ready(guard) => guard.path(),
            Self::Pending(claim) => claim.path(),
        }
    }
    fn descriptor(&self) -> OwnedFd {
        match self {
            Self::Ready(guard) => guard.try_clone_fd().unwrap(),
            Self::Pending(claim) => claim.descriptor().unwrap(),
        }
    }
}

#[test]
fn retained_memory_owner_actor() {
    let Some(root) = std::env::var_os(OWNER_ACTOR).map(std::path::PathBuf::from) else {
        return;
    };
    let ledger = Ledger::at(root.join("ledger"));
    let ordinary = claim(&ledger, need());
    let original = if std::env::var_os(OWNER_PENDING).is_some() {
        crate::ledger::fail_detach_after_bytes(1);
        let (ordinary, _) = ordinary.into_retained_guard().unwrap_err();
        let prefix = std::fs::read(ordinary.path()).unwrap();
        assert!(prefix.ends_with(b"@"));
        crate::ledger::fail_detach_after_bytes(0);
        let (ordinary, _) = ordinary.into_retained_guard().unwrap_err();
        assert_eq!(
            std::fs::read(ordinary.path()).unwrap(),
            prefix,
            "retry failure must never erase an existing intent prefix before owner crash"
        );
        assert!(Ledger::at(ledger.dir()).list().is_err());
        ActorGuard::Pending(Box::new(ordinary))
    } else {
        ActorGuard::Ready(Box::new(ordinary.into_retained_guard().unwrap()))
    };
    let mut connection =
        aterm_uds::kept_out(|| aterm_uds::CtlStream::connect(root.join("handoff.sock"))).unwrap();
    connection.set_nonblocking(false).unwrap();
    connection.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    connection.set_write_timeout(Some(MUST_HAPPEN)).unwrap();
    let descriptor = original.descriptor();
    let leaf = format!(
        "{}\n",
        original.path().file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        aterm_uds::fdpass::send_with_fds(&connection, leaf.as_bytes(), &[descriptor.as_fd()])
            .unwrap(),
        leaf.len()
    );
    let mut byte = [0];
    connection.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"H");
    connection.write_all(b"R").unwrap();
    // The controller kills this actual owner only after it has the descriptor and R.
    // Keep both original guards alive until then; EOF is failure cleanup, not success.
    assert_eq!(connection.read(&mut byte).unwrap(), 0);
    drop(original);
}

struct OwnerChild(aterm_uds::spawnfd::Child);
impl Drop for OwnerChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        until_within(MUST_HAPPEN, "owned memory actor cleanup/reap", || {
            self.0.try_wait().expect("query owned memory actor")
        });
    }
}

#[test]
fn actual_owner_sigkill_retains_original_memory_until_receiver_final_close() {
    owner_crash(false);
}

#[test]
fn actual_owner_crash_after_failed_retry_keeps_intent_error_closed() {
    owner_crash(true);
}

fn owner_crash(pending: bool) {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("retained-mem-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let listener =
        aterm_uds::kept_out(|| std::os::unix::net::UnixListener::bind(root.join("handoff.sock")))
            .unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    if pending {
        command.env(OWNER_PENDING, "1");
    }
    let mut owner = OwnerChild(
        command
            .args([
                "--exact",
                "retained_claim_tests::retained_memory_owner_actor",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(OWNER_ACTOR, &root)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let (mut connection, _) = until_within(
        MUST_HAPPEN,
        "actual detached memory owner connection",
        || match listener.accept() {
            Ok(connection) => Some(connection),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    owner.0.try_wait().unwrap().is_none(),
                    "owner ended before handoff"
                );
                None
            }
            Err(error) => panic!("owner socket: {error}"),
        },
    );
    // Darwin accepts inherit the listener's nonblocking status. The listener's bounded
    // accept probe must not turn this private readiness stream into a premature WouldBlock.
    connection.set_nonblocking(false).unwrap();
    connection.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    connection.set_write_timeout(Some(MUST_HAPPEN)).unwrap();
    assert_eq!(
        aterm_uds::fdpass::peer_pid(&connection).unwrap(),
        owner.0.id()
    );
    let mut bytes = [0; 256];
    let mut received = aterm_uds::fdpass::recv_with_fds(&connection, &mut bytes, 1).unwrap();
    assert_eq!(received.fds.len(), 1);
    let descriptor = received.fds.pop().unwrap();
    let mut leaf = bytes[..received.bytes].to_vec();
    while !leaf.ends_with(b"\n") {
        assert!(leaf.len() < 96);
        let mut next = [0];
        connection.read_exact(&mut next).unwrap();
        leaf.push(next[0]);
    }
    let leaf = std::str::from_utf8(&leaf)
        .unwrap()
        .strip_suffix('\n')
        .unwrap();
    let ledger = Ledger::at(root.join("ledger"));
    let path = ledger.dir().join(leaf);
    let original_pid = owner.0.id();
    if pending {
        assert!(Ledger::at(ledger.dir()).list().is_err());
        assert!(std::fs::read(&path).unwrap().ends_with(b"@"));
    } else {
        let original = ledger.list().unwrap().into_iter().next().unwrap();
        assert_eq!(original.pid, original_pid);
        assert_eq!(original.accounting, Accounting::Detached);
    }
    let pinned = File::from(descriptor);
    let inode = pinned.metadata().unwrap().ino();
    connection.write_all(b"H").unwrap();
    let mut ready = [0];
    connection.read_exact(&mut ready).unwrap();
    assert_eq!(&ready, b"R");
    owner.0.kill().unwrap();
    let status = until_within(
        MUST_HAPPEN,
        "actual original memory owner SIGKILL/reap",
        || owner.0.try_wait().unwrap(),
    );
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    let machine = Fixed::new(Reading::of_gib(8, 3, 8));
    machine.set_procs(Some(Procs::of(vec![
        Proc {
            pid: original_pid,
            ppid: 1,
            footprint: 2 * crate::floor::GIB,
            whole: 2 * crate::floor::GIB,
            ..Proc::default()
        },
        Proc {
            pid: std::process::id(),
            ppid: original_pid,
            ..Proc::default()
        },
    ])));
    if pending {
        assert!(std::fs::read(&path).unwrap().ends_with(b"@"));
        assert!(
            Ledger::at(ledger.dir()).list().is_err(),
            "fresh reader cannot see Holder after retry/crash"
        );
        match Waiter::enter(&ledger, &machine, need())
            .unwrap()
            .poll(&machine)
        {
            Polled::Waiting(waiter, short) => {
                assert!(
                    short.shorts.iter().any(|s| matches!(s, Short::Ledger(_))),
                    "{short:?}"
                );
                drop(waiter);
            }
            other => panic!("partial detached intent must remain error closed: {other:?}"),
        }
    } else {
        assert_eq!(ledger.list().unwrap()[0].need.mem, 2 * crate::floor::GIB);
        blocked(&ledger, &machine);
    }
    drop(pinned);
    until_within(
        MUST_HAPPEN,
        "received original memory guard final close",
        // A sibling test's fork may briefly retain a CLOEXEC copy. The partial marker must
        // stay error-closed during that interval; success still requires actual free-lock
        // observation by the fresh ledger, never an error interpreted as empty capacity.
        || {
            ledger
                .list()
                .is_ok_and(|records| records.is_empty())
                .then_some(())
        },
    );
    match Waiter::enter(&ledger, &machine, need())
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(claim) => drop(claim),
        other => panic!("actual final close restores the original memory budget: {other:?}"),
    }
    assert!(!path.exists());
}

fn claim(ledger: &Ledger, need: Need) -> Claim {
    crate::admit_with(
        ledger,
        &Fixed::new(Reading::of_gib(8, 6, 8)),
        need,
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .unwrap()
}

fn need() -> Need {
    Need::new("original functional window")
        .mem_gib(2)
        .margin(128 * 1024 * 1024)
}

fn legacy_head_parses(bytes: &[u8]) -> bool {
    let head = std::str::from_utf8(bytes).unwrap().lines().next().unwrap();
    // This is the actual first operation of the old v1 Record::decode, using the same
    // first-party parser; an old reader must fail here before reaching holder accounting.
    aterm_json::from_str::<aterm_json::Value>(head).is_ok()
}

fn poisoned_body(bytes: &[u8]) -> Vec<u8> {
    assert_eq!(bytes[0], b'{');
    let mut poisoned = bytes.to_vec();
    poisoned[0] = b'@';
    poisoned
}

fn blocked(ledger: &Ledger, machine: &Fixed) {
    match Waiter::enter(ledger, machine, need())
        .unwrap()
        .poll(machine)
    {
        Polled::Waiting(waiter, short) => {
            assert!(short.shorts.iter().any(|s| matches!(s, Short::Memory { reserved, .. } if *reserved == 2 * crate::floor::GIB)), "{short:?}");
            drop(waiter);
        }
        other => panic!("actual detached guard must retain its original whole peak: {other:?}"),
    }
}

#[test]
fn original_memory_survives_guard_drop_and_numeric_pid_credit_until_final_close() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path());
    let original = claim(&ledger, need());
    let original_record = original.record().clone();
    let path = original.path().to_owned();
    let original_metadata = std::fs::symlink_metadata(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        legacy_head_parses(&before),
        "ordinary v1 JSON head remains accepted"
    );
    let guard = original.into_retained_guard().unwrap();
    let metadata = guard.metadata().unwrap();
    assert_eq!(
        (metadata.dev(), metadata.ino()),
        (original_metadata.dev(), original_metadata.ino()),
        "consuming the original claim must retain its actual held inode"
    );
    let fd = guard.try_clone_fd().unwrap();
    assert_eq!(guard.record().need, original_record.need);
    assert_eq!(guard.record().accounting, Accounting::Detached);
    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        after,
        [
            poisoned_body(&before).as_slice(),
            crate::ledger::DETACHED_MEMORY
        ]
        .concat()
    );
    assert!(
        !legacy_head_parses(&after),
        "actual old-reader JSON head parsing must refuse detached memory"
    );
    drop(guard);
    let machine = Fixed::new(Reading::of_gib(8, 3, 8));
    machine.set_procs(Some(Procs::of(vec![Proc {
        pid: original_record.pid,
        ppid: 1,
        footprint: 2 * crate::floor::GIB,
        whole: 2 * crate::floor::GIB,
        ..Proc::default()
    }])));
    let listed = ledger.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].accounting, Accounting::Detached);
    assert_eq!(listed[0].need.mem, 2 * crate::floor::GIB);
    blocked(&ledger, &machine);
    assert!(matches!(
        RecordLock::try_lock(
            crate::open_regular_reader(&path).unwrap(),
            crate::ledger::RECORD_MAX
        ),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(fd);
    until_within(
        MUST_HAPPEN,
        "all original memory descriptors closed",
        || ledger.list().unwrap().is_empty().then_some(()),
    );
    let admitted = claim(&ledger, need());
    assert!(
        !path.exists(),
        "the next admission reaps only after actual final guard close"
    );
    drop(admitted);
}

#[test]
fn unsupported_claims_return_the_actual_original_and_cohort_attempt_unchanged() {
    for need in [
        Need::new("zero"),
        need().covers(),
        need().cores(1),
        need().quiet(100.0),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path());
        let original = claim(&ledger, need);
        let path = original.path().to_owned();
        let bytes = std::fs::read(&path).unwrap();
        let (returned, error) = original.into_retained_guard().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(returned.path(), path);
        drop(returned);
        assert!(ledger.list().unwrap().is_empty());
    }
    let fixture = crate::ledger::CohortFixture::new();
    let mut original = fixture.claim();
    let attempt = original.arm_cohort().unwrap();
    let path = original.path().to_owned();
    let before = std::fs::read(&path).unwrap();
    let (mut original, error) = original.into_retained_guard().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let writer = File::from(original.cohort_writer_descriptor().unwrap());
    assert!(writer.metadata().unwrap().is_file());
    original.cancel_cohort(attempt.cancel().unwrap()).unwrap();
    drop(original);
    assert!(fixture.ledger.list().unwrap().is_empty());
}

#[test]
fn failed_marker_is_error_closed_and_only_original_intent_can_retry() {
    for bytes in [0, 1, 9, crate::ledger::DETACHED_MEMORY.len()] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path());
        let original = claim(&ledger, need());
        let path = original.path().to_owned();
        let before = std::fs::read(&path).unwrap();
        let copied_guard = original.descriptor().unwrap();
        crate::ledger::fail_detach_after_bytes(bytes);
        let (mut original, error) = original.into_retained_guard().unwrap_err();
        assert!(error.to_string().contains("injected"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [
                poisoned_body(&before).as_slice(),
                &crate::ledger::DETACHED_MEMORY[..bytes]
            ]
            .concat()
        );
        assert!(
            original.credit(&[]).is_err(),
            "a pending intent cannot mutate ordinary metadata"
        );
        assert!(
            !legacy_head_parses(&std::fs::read(&path).unwrap()),
            "the poison-only cut also refuses the old reader"
        );
        if bytes < crate::ledger::DETACHED_MEMORY.len() {
            assert!(ledger.list().is_err());
        }
        let guard = original.into_retained_guard().unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [
                poisoned_body(&before).as_slice(),
                crate::ledger::DETACHED_MEMORY
            ]
            .concat()
        );
        drop(guard);
        assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Detached);
        drop(copied_guard);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path());
    let original = claim(&ledger, need());
    let path = original.path().to_owned();
    let copied = original.descriptor().unwrap();
    crate::ledger::fail_detach_after_bytes(1);
    let (original, _) = original.into_retained_guard().unwrap_err();
    drop(original);
    assert!(path.exists());
    assert!(
        ledger.list().is_err(),
        "dropping the original pending claim cannot unlock/unlink an unknown intent"
    );
    drop(copied);
}

#[test]
fn original_ledger_turn_fences_poison_before_detached_publication() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path());
    let original = claim(&ledger, need());
    let path = original.path().to_owned();
    let before = std::fs::read(&path).unwrap();
    let turn = ledger.hold_turn().unwrap().unwrap();
    assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Holder);
    let (original, error) = original.into_retained_guard().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "an old admission's original turn must finish before even the first poison byte"
    );
    drop(turn);
    let guard = original.into_retained_guard().unwrap();
    assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Detached);
    drop(guard);
}

#[test]
fn detached_marker_without_header_poison_is_never_accepted_as_compatibility() {
    use std::os::unix::fs::FileExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path());
    let original = claim(&ledger, need());
    let path = original.path().to_owned();
    let file = File::from(original.descriptor().unwrap());
    file.write_all_at(
        crate::ledger::DETACHED_MEMORY,
        file.metadata().unwrap().len(),
    )
    .unwrap();
    assert!(
        legacy_head_parses(&std::fs::read(&path).unwrap()),
        "this is the unsafe old encoding negative control"
    );
    assert!(
        ledger.list().is_err(),
        "updated readers cannot reinterpret an unpoisoned detached tail as Holder or Detached"
    );
    drop(original);
}

fn inherited(fd: OwnedFd) -> i32 {
    // SAFETY: this test alone owns fd; only this candidate's inheritance flag is changed.
    assert_eq!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, 0) }, 0);
    fd.into_raw_fd()
}

#[test]
fn actual_inherited_memory_adopts_exact_original_record_without_claim_writer() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let directory = scratch.path().canonicalize().unwrap();
    let ledger = Ledger::at(&directory);
    let original = claim(&ledger, need()).into_retained_guard().unwrap();
    let leaf = original
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let raw = inherited(original.try_clone_fd().unwrap());
    // SAFETY: exclusive inherited offer; the actual original guard remains continuously held.
    let adopted = unsafe { RetainedClaim::adopt_inherited(raw, &ledger, &leaf) }.unwrap();
    assert_eq!(adopted.record(), original.record());
    assert_eq!(
        adopted.metadata().unwrap().ino(),
        original.metadata().unwrap().ino()
    );
    assert!(
        Claim::adopt(original.try_clone_fd().unwrap(), original.path()).is_err(),
        "a detached record cannot be promoted back to an ordinary unlinking writer"
    );
    drop(original);
    assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Detached);
    let raw = inherited(adopted.try_clone_fd().unwrap());
    // SAFETY: known invalid leaf refuses before ownership of this exclusive offer is taken.
    assert!(unsafe { RetainedClaim::adopt_inherited(raw, &ledger, "../foreign.claim") }.is_err());
    // SAFETY: the invalid leaf refusal left the original raw offer untouched and still ours.
    drop(unsafe { OwnedFd::from_raw_fd(raw) });
    drop(adopted);
    until_within(
        MUST_HAPPEN,
        "adopted original memory guard's final close",
        || ledger.list().unwrap().is_empty().then_some(()),
    );
}
