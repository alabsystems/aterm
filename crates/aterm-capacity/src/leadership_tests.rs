// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private native leadership: original lock, committed epoch, real holder death.

use super::*;
use crate::ledger::CohortFixture as Fixture;
use std::{os::unix::fs::DirBuilderExt as _, path::PathBuf};

#[test]
fn competing_owner_refuses_and_takeover_advances_without_touching_reservations() {
    let f = Fixture::new();
    let claim = f.claim();
    let before = std::fs::read(claim.path()).unwrap();
    let first = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    assert_eq!(first.sequence(), 1);
    first.check_claim(&claim).unwrap();
    assert_eq!(
        Fence::acquire(&f.namespace, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let foreign = Fixture::new();
    assert!(first.check_claim(&foreign.claim()).is_err());
    let generation = first.generation();
    drop(first);
    assert_eq!(f.ledger.list().unwrap().len(), 1);
    assert_eq!(std::fs::read(claim.path()).unwrap(), before);
    let second = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    assert_eq!(second.sequence(), 2);
    assert_ne!(second.generation(), generation);
    second.check_claim(&claim).unwrap();
    assert_eq!(std::fs::read(claim.path()).unwrap(), before);
}

#[test]
fn foreign_process_drop_does_not_unlock_a_retained_shared_description() {
    let f = Fixture::new();
    let mut foreign_copy = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    // A duplicate reproduces the shared open-description lifetime of fork
    // without running allocating Rust destructors after a multithreaded fork.
    let retained = aterm_uds::kept_out(|| foreign_copy.lock.try_clone()).unwrap();
    foreign_copy.process = std::process::id().wrapping_add(1);
    drop(foreign_copy);
    assert_eq!(
        Fence::acquire(&f.namespace, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    retained.unlock().unwrap();
    drop(retained);
    assert_eq!(
        Fence::acquire(&f.namespace, Instant::now())
            .unwrap()
            .sequence(),
        2
    );
}

#[test]
fn old_namespace_version_or_missing_old_backend_header_never_falls_back() {
    let f = Fixture::new();
    let container = f.namespace.backend().parent().unwrap().to_path_buf();
    let header = container.join(".capacity-namespace.json");
    let old = std::fs::read_to_string(&header)
        .unwrap()
        .replace("\"v\":4", "\"v\":3");
    for path in [
        header,
        f.namespace.fence(),
        f.namespace.backend().join(".ready"),
    ] {
        std::fs::write(path, &old).unwrap();
    }
    assert!(Namespace::resolve(&f.namespace.fence()).is_err());
    let plain = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir(plain.path().join("capacity-v3.noindex")).unwrap();
    assert!(Namespace::resolve(&plain.path().join("capacity")).is_err());
}

#[test]
fn missing_torn_pending_oversized_and_exhausted_epochs_never_bootstrap_again() {
    for mode in 0..5 {
        let f = Fixture::new();
        let state = f.namespace.backend().join(STATE);
        match mode {
            0 => std::fs::remove_file(&state).unwrap(),
            1 => std::fs::write(&state, b"torn epoch").unwrap(),
            2 => std::fs::write(f.namespace.backend().join(PENDING), b"torn publication").unwrap(),
            3 => std::fs::write(&state, vec![0; RECORD_LEN + 1]).unwrap(),
            4 => std::fs::write(
                &state,
                Epoch {
                    sequence: u64::MAX,
                    generation: Generation([9; 16]),
                }
                .encode(f.namespace.nonce()),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let before = std::fs::read(&state).ok();
        assert!(Fence::acquire(&f.namespace, Instant::now()).is_err());
        assert_eq!(std::fs::read(&state).ok(), before, "no reset or fallback");
        assert!(
            f.namespace.validate().is_ok(),
            "leadership failure is not loss of native custody"
        );
    }
}

#[test]
fn replaced_lock_is_anchored_across_reopen_and_lost_authority_never_revives() {
    let f = Fixture::new();
    let claim = f.claim();
    let first = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    let backend = f.namespace.backend();
    std::fs::rename(backend.join(LOCK), backend.join("original.lock")).unwrap();
    let replacement = namespace::create(&f.namespace.open_backend().unwrap(), LOCK).unwrap();
    drop(replacement);
    assert!(first.check_current().is_err());
    let reopened = Namespace::resolve(&f.namespace.fence()).unwrap().unwrap();
    assert!(Fence::acquire(&reopened, Instant::now()).is_err());
    assert!(f.namespace.validate().is_ok());
    claim.check_cohort_namespace(&f.namespace).unwrap();
    std::fs::remove_file(backend.join(LOCK)).unwrap();
    std::fs::rename(backend.join("original.lock"), backend.join(LOCK)).unwrap();
    assert!(
        first.check_current().is_err(),
        "restoring a path never revives this owner"
    );
    assert_eq!(
        Fence::acquire(&reopened, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(first);
    assert_eq!(
        Fence::acquire(&reopened, Instant::now())
            .unwrap()
            .sequence(),
        2
    );
}

#[test]
fn symlink_foreign_record_and_parent_replacement_refuse_without_releasing_claims() {
    let f = Fixture::new();
    let claim = f.claim();
    let first = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    let state = f.namespace.backend().join(STATE);
    std::fs::rename(&state, state.with_extension("saved")).unwrap();
    std::os::unix::fs::symlink(state.with_extension("saved"), &state).unwrap();
    assert!(first.check_current().is_err());
    assert!(Fence::acquire(&f.namespace, Instant::now()).is_err());
    claim.check_cohort_namespace(&f.namespace).unwrap();
    drop(first);
    std::fs::remove_file(&state).unwrap();
    let bytes = Epoch {
        sequence: 5,
        generation: Generation([5; 16]),
    }
    .encode("00000000000000000000000000000000");
    std::fs::write(&state, bytes).unwrap();
    assert!(Fence::acquire(&f.namespace, Instant::now()).is_err());

    let f = Fixture::new();
    let first = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    let backend = f.namespace.backend();
    std::fs::rename(&backend, backend.with_extension("retained")).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&backend)
        .unwrap();
    assert!(first.check_current().is_err());
    assert!(Fence::acquire(&f.namespace, Instant::now()).is_err());
}

const ACTOR: &str = "ATERM_TEST_CAPACITY_HOST_FENCE";

#[test]
fn leadership_actor_if_requested() {
    let Some(path) = std::env::var_os(ACTOR) else {
        return;
    };
    let namespace = Namespace::resolve(&PathBuf::from(path)).unwrap().unwrap();
    let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
    println!("\nREADY {}", fence.sequence());
    std::io::stdout().flush().unwrap();
    // Parent owns stdin and explicitly requests actual SIGKILL after readiness.
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert_eq!(line, "die\n");
    aterm_uds::process::kill_self_now();
}

#[test]
fn actual_holder_sigkill_allows_next_epoch_and_preserves_live_parent_claim() {
    use aterm_uds::spawnfd::{self, Stdio};
    use std::os::unix::process::ExitStatusExt as _;
    let f = Fixture::new();
    let claim = f.claim();
    let before = std::fs::read(claim.path()).unwrap();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "leadership::tests::leadership_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, f.namespace.fence())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    struct Child(aterm_uds::spawnfd::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut owned = Child(command.spawn().unwrap());
    let child = &mut owned.0;
    aterm_uds::deadline::prints(child, "READY 1", aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        Fence::acquire(&f.namespace, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    child.stdin.as_mut().unwrap().write_all(b"die\n").unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "leadership actor SIGKILL",
        || child.try_wait().unwrap(),
    );
    assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    let next = Fence::acquire(&f.namespace, Instant::now()).unwrap();
    assert_eq!(next.sequence(), 2);
    next.check_claim(&claim).unwrap();
    assert_eq!(std::fs::read(claim.path()).unwrap(), before);
    assert_eq!(f.ledger.list().unwrap().len(), 1);
}

/// A host epoch read that a signal interrupts is read again, as `read_to_end` did: an
/// interruption is no failure of the record.
/// NEGATIVE CONTROL: a `fill` that hands `Interrupted` to its caller (dabbac544e) fails the
/// read: `called `Result::unwrap()` on an `Err` value: Custom { kind: Interrupted, .. }`.
#[test]
fn an_interrupted_epoch_read_is_read_again() {
    struct Interrupts<'a> {
        once: bool,
        rest: &'a [u8],
    }
    impl io::Read for Interrupts<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.once) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.rest.read(buf)
        }
    }
    let record = [7u8; RECORD_LEN];
    let mut buf = [0u8; RECORD_LEN + 1];
    let mut reader = Interrupts {
        once: true,
        rest: &record,
    };
    assert_eq!(fill(&mut reader, &mut buf).unwrap(), RECORD_LEN);
    assert_eq!(buf[..RECORD_LEN], record);
}
