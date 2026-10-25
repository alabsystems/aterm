// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{Action, InitialSetupObserved, Report, SETUP_BUSY};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a press waits for, published as `SetupStatus::queued`.
fn queued_mark() -> u64 {
    SETUP_QUEUED.load(Ordering::Acquire)
}

/// A choice that finds this Mac's setup records held by another setup is shown waiting from that
/// moment (System State design §3.3), never as a run going with its meter and Stop for the ten
/// seconds the profile lock's wait can last; once admitted it waits for nothing.
#[test]
fn a_choice_that_finds_the_records_held_waits_from_that_moment() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| {
        script.persist_hold = true;
        script.persist_finds_lock_held = true;
        script.admits = [Some(5)].into();
    });
    let (receipt, accepted) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    wait_until("the choice's admission begins", || script().persists == 1);
    let status = initial_setup_status();
    assert!(
        status.busy && status.queued,
        "busy {} queued {}: the page would show a run going",
        status.busy,
        status.queued
    );
    change(|script| script.persist_hold = false);
    assert_eq!(accepted.recv_timeout(HANG), Ok(true));
    wait_until("the choice's run starts", || script().runs == [5]);
    assert_eq!(queued_mark(), 0, "an admitted choice waits for nothing");
}

/// Lets the stand-in Restore's other setup go when dropped, so a Restore that is never let go
/// still ends.
struct OtherSetup(Arc<AtomicBool>);

/// What a stand-in Restore that put settings back reports.
fn restored() -> Report {
    Report {
        action: Action::Restore,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: std::path::PathBuf::from("inert-backup"),
        restore: None,
        page: None,
    }
}

/// A stand-in Restore's attempt that found the records another setup holds and waited for them
/// past its own wait: it says so the moment it finds them held (`waits`), as the real one does.
fn held_elsewhere(waits: &mut dyn FnMut()) -> InitialSetupObserved<Option<Report>> {
    waits();
    InitialSetupObserved {
        result: Err(SETUP_BUSY.to_owned()),
        observation: None,
    }
}

/// A stand-in Restore's attempt that holds the records: it asks `holding` before it writes,
/// and puts settings back only when that says it goes on.
fn holding_records(go: bool) -> InitialSetupObserved<Option<Report>> {
    InitialSetupObserved {
        result: Ok(go.then(restored)),
        observation: None,
    }
}

impl Drop for OtherSetup {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// A Restore waiting for another setup is the person's press until they press again: a group
/// pressed meanwhile is their latest intent (System State design §3.4), as a later choice
/// replaces one waiting. So it is whether the Restore found the records held before it asked
/// (it waits quietly) or met them held past its own wait for them: the Restore is let go
/// having changed nothing, and the choice takes the setup slot and runs.
#[test]
fn a_later_choice_replaces_a_restore_waiting_for_another_setup() {
    let _globals = globals();
    for held_before_it_asks in [false, true] {
        let _io = TestIo::install();
        if held_before_it_asks {
            // Held for longer than the test waits: only the later choice ends the wait.
            change(|script| script.records_held.extend([true; 600]));
        }
        let held = OtherSetup(Arc::new(AtomicBool::new(true)));
        let other = Arc::clone(&held.0);
        let busy = take_initial_setup_slot().unwrap();
        let (ended, restore_ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut restores = 0_usize;
            let mut ran = false;
            run_setup_restore(busy, |waits, holding| {
                restores += 1;
                if other.load(Ordering::Acquire) {
                    return held_elsewhere(waits);
                }
                let go = holding();
                ran |= go;
                holding_records(go)
            });
            let _ = ended.send((restores, ran));
        });
        wait_until("the Restore waits for the other setup", || {
            queued_mark() == RESTORE_QUEUED
        });
        change(|script| script.admits = [Some(3)].into());
        let (receipt, accepted) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        let (restores, ran) = restore_ended
            .recv_timeout(HANG)
            .expect("the waiting Restore is let go for the later choice");
        assert_eq!(
            restores >= 1,
            !held_before_it_asks,
            "a Restore that found the records held asked for nothing"
        );
        assert!(!ran, "the Restore changed nothing");
        assert_eq!(accepted.recv_timeout(HANG), Ok(true));
        wait_until("the choice's run starts", || script().runs == [3]);
        assert_ne!(queued_mark(), RESTORE_QUEUED);
        drop(held);
    }
}

/// A Restore that waited for another setup shows restoring, no longer waiting, from the moment
/// it holds this Mac's setup records, before its first write (System State design §3.4): its
/// groups and Check again then behave as during a run, so no group press is promised to replace
/// a Restore already putting settings back.
#[test]
fn a_waited_restore_shows_restoring_once_it_holds_the_records() {
    let _globals = globals();
    let _io = TestIo::install();
    let held = OtherSetup(Arc::new(AtomicBool::new(true)));
    let other = Arc::clone(&held.0);
    let busy = take_initial_setup_slot().unwrap();
    let (ended, restore_ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut shown_waiting = None;
        run_setup_restore(busy, |waits, holding| {
            if other.load(Ordering::Acquire) {
                return held_elsewhere(waits);
            }
            let go = holding();
            // It holds the records now and is putting preferences back.
            shown_waiting = Some(initial_setup_status().queued);
            holding_records(go)
        });
        let _ = ended.send(shown_waiting);
    });
    wait_until("the Restore waits for the other setup", || {
        queued_mark() == RESTORE_QUEUED
    });
    // The other setup finishes: the Restore takes the records and writes.
    held.0.store(false, Ordering::Release);
    assert_eq!(
        restore_ended.recv_timeout(HANG),
        Ok(Some(false)),
        "a Restore writing is shown restoring, not waiting"
    );
    assert_eq!(queued_mark(), 0);
}

/// A group pressed while a Restore waits for another setup replaces it even where the press
/// lands as the Restore takes the records: the Restore asks once it holds them, before its
/// first write, gives way having changed nothing, and the choice runs (System State design
/// §3.4).
#[test]
fn a_press_made_as_a_waited_restore_takes_the_records_replaces_it() {
    let _globals = globals();
    let _io = TestIo::install();
    let held = OtherSetup(Arc::new(AtomicBool::new(true)));
    let other = Arc::clone(&held.0);
    let busy = take_initial_setup_slot().unwrap();
    let (took, taken) = std::sync::mpsc::channel();
    let (go_on, pressed) = std::sync::mpsc::channel::<()>();
    let (ended, restore_ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ran = false;
        run_setup_restore(busy, |waits, holding| {
            if other.load(Ordering::Acquire) {
                return held_elsewhere(waits);
            }
            // It has the records and has written nothing: the person presses a group now.
            let _ = took.send(());
            let _ = pressed.recv_timeout(HANG);
            let go = holding();
            ran |= go;
            holding_records(go)
        });
        let _ = ended.send(ran);
    });
    wait_until("the Restore waits for the other setup", || {
        queued_mark() == RESTORE_QUEUED
    });
    held.0.store(false, Ordering::Release);
    taken
        .recv_timeout(HANG)
        .expect("the Restore takes the records");
    change(|script| script.admits = [Some(3)].into());
    let (receipt, accepted) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    go_on.send(()).unwrap();
    assert_eq!(
        restore_ended.recv_timeout(HANG),
        Ok(false),
        "the Restore gave way having changed nothing"
    );
    assert_eq!(accepted.recv_timeout(HANG), Ok(true));
    wait_until("the choice's run starts", || script().runs == [3]);
    assert_ne!(queued_mark(), RESTORE_QUEUED);
}

/// A Restore whose own attempt finds this Mac's setup records held by another setup, though the
/// look before it found them free, is shown waiting from that moment (System State design
/// §3.4), as a choice is, never as a Restore going for the ten seconds its wait for them lasts.
#[test]
fn a_restore_whose_own_attempt_finds_the_records_held_waits_from_then() {
    let _globals = globals();
    let _io = TestIo::install();
    let busy = take_initial_setup_slot().unwrap();
    let mut shown_waiting = Vec::new();
    run_setup_restore(busy, |waits, holding| {
        if shown_waiting.is_empty() {
            // A holder took the records after the look: this attempt finds them held.
            let mut seen = false;
            let observed = held_elsewhere(&mut || {
                waits();
                seen = initial_setup_status().queued;
            });
            shown_waiting.push(seen);
            return observed;
        }
        let go = holding();
        shown_waiting.push(initial_setup_status().queued);
        holding_records(go)
    });
    assert_eq!(
        shown_waiting,
        [true, false],
        "waiting as its attempt found the records held, restoring once it held them"
    );
    assert_eq!(queued_mark(), 0);
}
