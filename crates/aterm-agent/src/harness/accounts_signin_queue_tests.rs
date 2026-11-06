// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::Path;
use std::time::Duration;

fn turn(ticket: &mut SigninTicket) -> SigninTurn {
    match ticket.try_enter().unwrap() {
        SigninAttempt::Acquired(turn) => turn,
        SigninAttempt::Waiting(position) => panic!("ticket still waits: {position:?}"),
    }
}

#[test]
fn manual_fifo_spans_cli_pools_and_blocks_background_barging() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(scratch.path());
    let other = Store::at_for(scratch.path(), super::super::Cli::Kimi, scratch.path());
    let external = store.signin_now().unwrap().unwrap();
    let mut first = store.queue_manual_signin().unwrap();
    let mut second = other.queue_manual_signin().unwrap();
    assert_eq!(
        first.position().unwrap(),
        SigninPosition {
            ahead: 0,
            blocker: SigninBlocker::UnscheduledSignin
        }
    );
    assert_eq!(
        second.position().unwrap(),
        SigninPosition {
            ahead: 1,
            blocker: SigninBlocker::ManualAhead
        }
    );
    assert!(store.try_background_signin().unwrap().is_none());
    drop(external);
    assert!(store.signin_now().unwrap().is_none());
    assert!(
        other
            .clone()
            .bounded_by(&[])
            .signin_within()
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        second.try_enter().unwrap(),
        SigninAttempt::Waiting(_)
    ));
    let active = turn(&mut first);
    assert!(
        first.try_enter().is_err(),
        "one ticket cannot supply a second turn"
    );
    drop(first);
    assert_eq!(
        second.position().unwrap().ahead,
        1,
        "active turn retains its liveness after the waiting handle drops"
    );
    assert!(store.signin_now().unwrap().is_none());
    assert!(store.try_background_signin().unwrap().is_none());
    drop(active);
    assert_eq!(
        second.position().unwrap(),
        SigninPosition {
            ahead: 0,
            blocker: SigninBlocker::None
        }
    );
    assert!(store.try_background_signin().unwrap().is_none());
    drop(turn(&mut second));
    assert!(store.signin_now().unwrap().is_some());
    assert!(other.try_background_signin().unwrap().is_some());
}

#[test]
fn discarded_and_unstarted_tickets_retire_only_their_own_place() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(scratch.path());
    let first = store.queue_manual_signin().unwrap();
    let mut second = store.queue_manual_signin().unwrap();
    let queued: Box<dyn FnOnce()> = Box::new(move || drop(first));
    assert_eq!(second.position().unwrap().ahead, 1);
    drop(queued);
    assert_eq!(second.position().unwrap().ahead, 0);
    drop(turn(&mut second));
    assert!(store.try_background_signin().unwrap().is_some());
}

#[test]
fn manual_wait_reports_the_active_blocker_and_retains_turn_through_begun() {
    use std::cell::{Cell, RefCell};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(scratch.path());
    let active = RefCell::new(store.signin_now().unwrap());
    let calls = Cell::new(0);
    let mut positions = Vec::new();
    let (turn, waited) = store
        .manual_signin_while(
            || {
                calls.set(calls.get() + 1);
                calls.get() < 5
            },
            |position| {
                positions.push(position);
                drop(active.borrow_mut().take());
            },
        )
        .unwrap()
        .expect("the released actual lock admits the waiting ticket");
    assert!(waited);
    assert_eq!(
        positions,
        [SigninPosition {
            ahead: 0,
            blocker: SigninBlocker::UnscheduledSignin
        }]
    );
    let mut next = store.queue_manual_signin().unwrap();
    let begun =
        super::super::super::sign_in_ladder::asked_fixture(super::super::Cli::Claude, "one")
            .begin(super::super::Cli::Claude, "one", turn, &store, 1)
            .unwrap();
    assert_eq!(next.position().unwrap().ahead, 1);
    drop(begun);
    assert_eq!(next.position().unwrap().ahead, 0);
    drop(self::turn(&mut next));
}

#[test]
fn cancelling_manual_wait_abandons_only_its_ticket_without_provider_work() {
    use std::cell::Cell;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(scratch.path());
    let first = store.queue_manual_signin().unwrap();
    let cancelled = Cell::new(false);
    let mut observed = None;
    assert!(
        store
            .manual_signin_while(
                || !cancelled.get(),
                |position| {
                    observed = Some(position);
                    cancelled.set(true);
                },
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        observed,
        Some(SigninPosition {
            ahead: 1,
            blocker: SigninBlocker::ManualAhead
        })
    );
    let next = store.queue_manual_signin().unwrap();
    assert_eq!(next.position().unwrap().ahead, 1);
    drop(first);
    assert_eq!(next.position().unwrap().ahead, 0);
}

const CHILD_STATE: &str = "ATERM_TEST_SIGNIN_QUEUE_STATE";
const READY: &str = "SIGNIN_QUEUE_OWNER_READY";

#[test]
fn manual_ticket_owner_stand_in() {
    let Some(state) = std::env::var_os(CHILD_STATE) else {
        return;
    };
    let _ticket = Store::at(Path::new(&state)).queue_manual_signin().unwrap();
    println!("{READY}");
    use std::io::{Read as _, Write as _};
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]).unwrap();
    // Skip Rust destructors. Only process exit releases this ticket's kernel lock.
    std::process::exit(0);
}

#[test]
fn a_departed_process_is_reclaimed_by_ticket_lock_without_pid_or_clock_authority() {
    use aterm_uds::spawnfd::{Stdio, Tether as _};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let store = Store::at(scratch.path());
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "harness::accounts::signin_queue::tests::manual_ticket_owner_stand_in",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_STATE, scratch.path())
        .process_group(0)
        .tether()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    struct Owned(aterm_uds::spawnfd::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            crate::harness::login_drive::end(&mut self.0);
        }
    }
    let mut child = Owned(command.spawn().unwrap());
    aterm_uds::deadline::prints(&mut child.0, READY, Duration::from_secs(60));
    let mut second = store.queue_manual_signin().unwrap();
    assert_eq!(second.position().unwrap().ahead, 1);
    drop(child.0.stdin.take());
    assert!(child.0.wait().unwrap().success());
    assert_eq!(second.position().unwrap().ahead, 0);
    drop(turn(&mut second));
}

#[test]
fn unsafe_queue_records_refuse_admission_without_changing_the_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    for kind in 0..4 {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let store = Store::at(scratch.path());
        let mut ticket = store.queue_manual_signin().unwrap();
        let file = ticket.root.join(&ticket.name);
        let target = scratch.path().join("outside-target");
        match kind {
            0 => std::fs::write(ticket.root.join("unrecognized"), b"not a ticket").unwrap(),
            1 => {
                std::fs::write(&target, b"preserve").unwrap();
                std::fs::remove_file(&file).unwrap();
                symlink(&target, &file).unwrap();
            }
            2 => std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o664)).unwrap(),
            _ => std::fs::hard_link(&file, &target).unwrap(),
        }
        assert!(ticket.position().is_err());
        assert!(ticket.try_enter().is_err());
        assert!(store.try_background_signin().is_err());
        if kind == 1 {
            assert_eq!(std::fs::read(target).unwrap(), b"preserve");
        }
        if kind == 2 {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o664
            );
        }
    }
}
