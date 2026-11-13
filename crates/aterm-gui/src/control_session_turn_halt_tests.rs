// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{TurnIo, cmd_turn};
use crate::session_store::{new_store, test_handle};

struct Ran {
    reply: String,
    writes: usize,
    presses: usize,
}

fn run(line: &str, take_after: Option<usize>, remote: bool) -> Ran {
    let handle = test_handle(0);
    let (term, ctx) = (handle.term.clone(), handle.ctx.clone());
    let sid = handle.sid.as_str().to_owned();
    let store = new_store();
    store.write().unwrap().register(handle);
    let _driving = remote.then(|| {
        crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "0b".repeat(16),
            host: "mac-0b".to_owned(),
        })
    });
    if remote {
        assert_eq!(
            crate::fabric::cmd_hold(
                &store,
                &format!("{sid} on"),
                crate::fabric::HoldIssuer::Remote
            ),
            "OK hold=1\n"
        );
    }
    let take_over = || crate::fabric::person_hold(&*ctx, true, &|| {});
    if take_after == Some(0) {
        take_over();
    }
    let (writes, presses) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let wrote = || {
        if Some(writes.fetch_add(1, Ordering::SeqCst) + 1) == take_after {
            take_over();
        }
        true
    };
    let paste = |_: &str| wrote();
    let key = |_: crate::input::InputEvent| wrote();
    let press = |_: &str| {
        presses.fetch_add(1, Ordering::SeqCst);
        true
    };
    let reply = cmd_turn(
        &term,
        &store,
        0,
        line,
        &ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            ..TurnIo::paste_only()
        },
    );
    Ran {
        reply,
        writes: writes.load(Ordering::SeqCst),
        presses: presses.load(Ordering::SeqCst),
    }
}

#[test]
fn a_hold_set_while_a_turn_runs_presses_nothing_more() {
    const TYPED: &str = "typed=1 cadence=1 cap=5 idle=1 presses=1 submit_window=50 \
                         timeout=60000 hello";
    const PLAIN: &str = "idle=1 presses=1 submit_window=50 timeout=60000 hello";
    for remote in [false, true] {
        let cut = run(TYPED, Some(2), remote);
        assert!(
            cut.reply
                .starts_with("OK 0 turn skipped reason=halted typed=2 pasted=0 submitted=0 "),
            "{}",
            cut.reply
        );
        assert_eq!((cut.writes, cut.presses), (2, 0), "remote={remote}");
        let whole = run(TYPED, None, remote);
        assert!(!whole.reply.contains("reason=halted"), "{}", whole.reply);
        assert_eq!((whole.writes, whole.presses), (5, 1), "remote={remote}");

        // What stands of a plain turn halted after its paste is the paste (third review of
        // the turn-deadline work, 2026-11-06): a drive reads the text left in the composer.
        let cut = run(PLAIN, Some(1), remote);
        assert!(
            cut.reply
                .starts_with("OK 0 turn skipped reason=halted typed=0 pasted=5 submitted=0 "),
            "{}",
            cut.reply
        );
        assert_eq!((cut.writes, cut.presses), (1, 0), "remote={remote}");
        let whole = run(PLAIN, None, remote);
        assert_eq!((whole.writes, whole.presses), (1, 1), "remote={remote}");

        for line in [TYPED, PLAIN] {
            let before = run(line, Some(0), remote);
            assert!(
                before.reply.starts_with("ERR halted ") && before.reply.contains("origin=person"),
                "{}",
                before.reply
            );
            assert_eq!((before.writes, before.presses), (0, 0), "remote={remote}");
        }
    }
}

/// A GUARDED ENTER LOOKS FOR ITS TURN'S SLOT BEFORE EACH TRY (third review of the
/// turn-deadline work, 2026-11-06): retrying behind input queued in order, as it does while a
/// program reads nothing, the guarded submit of a turn `lease release force` preempted stops
/// at the next try, writing nothing, and never presses into the turn that took the slot.
/// NEGATIVE CONTROL: with the slot looked for once, before the first try, it retries until its
/// window ends (`Busy`, here the 60 s hang detector).
#[test]
fn a_guarded_enter_stops_retrying_once_its_turn_is_preempted() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::{GuardedSubmit, guarded_submit};

    let handle = test_handle(0);
    let (term, ctx) = (handle.term.clone(), handle.ctx.clone());
    let guard = crate::control::compile_guard(Some("^$"))
        .expect("the guard compiles")
        .expect("a guard was given");
    ctx.sink.claim_ordered_egress();
    // Each try looks before its lock and under it: the third look, the second try's first,
    // is the slot's loss.
    let looks = AtomicU32::new(0);
    let preempted = || looks.fetch_add(1, Ordering::SeqCst) + 1 >= 3;
    let hang = Instant::now() + Duration::from_secs(60);
    assert!(matches!(
        guarded_submit(&term, &ctx, &*guard, None, hang, None, &preempted),
        GuardedSubmit::Preempted
    ));
    assert_eq!(
        looks.load(Ordering::SeqCst),
        3,
        "the first try met the queue, the second stopped before its lock"
    );
    // CONTROL: never preempted, the queued input runs a short window out.
    let soon = Instant::now() + Duration::from_millis(20);
    assert!(matches!(
        guarded_submit(&term, &ctx, &*guard, None, soon, None, &|| false),
        GuardedSubmit::Busy
    ));
    ctx.sink.retire_ordered_egress();
}

/// A GUARDED ENTER JUDGES ITS TURN'S SLOT UNDER THE LOCK IT WRITES UNDER (fourth and fifth
/// reviews of the turn-deadline work, 2026-11-06): the slot taken while the try waits for the
/// terminal lock (held here by the test, as another read or write holds it) stops the try
/// there, nothing written,
/// and the submit answers `Preempted`. NEGATIVE CONTROL: with the slot looked for only before
/// the lock, however often, the Enter is written into whatever turn took the slot.
#[test]
#[cfg(unix)]
fn a_guarded_enter_judges_its_turn_s_slot_under_the_lock_it_writes_under() {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::{GuardedSubmit, guarded_submit};

    let (master, mut program) = std::os::unix::net::UnixStream::pair().expect("a pair");
    let handle = crate::session_store::test_handle_on(0, master.as_raw_fd());
    let (term, ctx) = (handle.term.clone(), handle.ctx.clone());
    aterm_pty::set_nonblocking(ctx.sink.master(), true).expect("a non-blocking master");
    ctx.sink.note_master_nonblocking(true);
    program
        .set_nonblocking(true)
        .expect("a non-blocking reader");
    let written = |program: &mut std::os::unix::net::UnixStream| {
        let mut buf = [0u8; 16];
        match program.read(&mut buf) {
            Ok(n) => buf[..n].to_vec(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Vec::new(),
            Err(e) => panic!("reading the program's end: {e}"),
        }
    };
    let guard = crate::control::compile_guard(Some("^$"))
        .expect("the guard compiles")
        .expect("a guard was given");
    let hang = Instant::now() + Duration::from_secs(60);
    // The terminal is held while the try's first look, before the lock, finds the slot; the
    // slot goes, and only then is the terminal let go.
    let looks = AtomicU32::new(0);
    let gone = AtomicBool::new(false);
    // A look is counted once its answer is read, so the first one (before the lock) has
    // found the slot whatever the order of the two threads.
    let preempted = || {
        let lost = gone.load(Ordering::SeqCst);
        looks.fetch_add(1, Ordering::SeqCst);
        lost
    };
    let outcome = std::thread::scope(|s| {
        let held = crate::term_lock(&term);
        let trying = s.spawn(|| guarded_submit(&term, &ctx, &*guard, None, hang, None, &preempted));
        while looks.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < hang, "the try never looked for its slot");
            aterm_uds::deadline::poll_yield();
        }
        gone.store(true, Ordering::SeqCst);
        drop(held);
        trying.join().expect("the try ran")
    });
    assert!(matches!(outcome, GuardedSubmit::Preempted));
    assert!(written(&mut program).is_empty(), "no Enter was written");
    // CONTROL: a slot that stands, the Enter is written.
    assert!(matches!(
        guarded_submit(&term, &ctx, &*guard, None, hang, None, &|| false),
        GuardedSubmit::Pressed
    ));
    assert_eq!(written(&mut program), b"\r");
}

/// What stands of a plain turn halted after its paste is what the paste kept (fourth review
/// of the turn-deadline work, 2026-11-06): one of control characters alone kept none, so its
/// cut says no text stands. NEGATIVE CONTROL: counted from the text as given, it says
/// `pasted=2`, which a drive reads as text left in the composer.
#[test]
fn a_plain_turn_of_control_characters_halted_after_its_paste_leaves_nothing_standing() {
    let cut = run(
        "idle=1 presses=1 submit_window=50 timeout=60000 \u{1}\u{2}",
        Some(1),
        false,
    );
    assert!(
        cut.reply
            .starts_with("OK 0 turn skipped reason=halted submitted=0 "),
        "{}",
        cut.reply
    );
    assert_eq!((cut.writes, cut.presses), (1, 0));
}
