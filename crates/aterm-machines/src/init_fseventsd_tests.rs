// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::bind;
use crate::items::defaults::tests::ctx;
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran};
use aterm_converge::{Class, Layer, NoJournal, Scope};

const GIB: u64 = 1 << 30;
const M3: u64 = 128 * GIB;
const STARTED: u64 = 1_790_000_000;
const NOW: u64 = 1_790_400_000;
const STAMPED: &str = "/state/fseventsd-restart";

fn service(pid: u32, footprint: u64) -> Service {
    Service {
        coalition: 7,
        pid,
        start_s: STARTED + u64::from(pid),
        start_us: 0,
        uid: 0,
        name: "fseventsd".into(),
        path: Some(FSEVENTSD.path.into()),
        live: 1,
        footprint,
    }
}

fn running(pid: u32, footprint: u64) -> Look {
    Look::Running(service(pid, footprint), M3)
}

fn item(named: bool) -> Fseventsd {
    Fseventsd {
        named,
        stamp: Some(STAMPED.into()),
    }
}

fn validate(pid: u32, footprint: u64) -> String {
    format!(
        "tty {}",
        Fseventsd::validate(&service(pid, footprint)).display()
    )
}

fn kill(pid: u32, footprint: u64) -> String {
    format!(
        "tty {}",
        Fseventsd::kill(&service(pid, footprint)).display()
    )
}

/// A Mac with a person at its terminal, at [`NOW`], where fseventsd 537 runs.
fn mac() -> Fake {
    let mut sys = Fake::default();
    sys.tty = true;
    sys.now = NOW;
    sys.alive(537).born(537, STARTED + 537);
    sys
}

/// One run of `item` on `sys`, whose looks answer `looks` in turn.
fn run(item: &Fseventsd, sys: &Fake, looks: Vec<Look>, mode: Mode) -> aterm_converge::ItemReport {
    sys.services(looks);
    let ctx = ctx(&[]);
    let bound = bind([(item as &dyn Item, place())], sys, &ctx);
    let mut rows =
        aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
            .unwrap();
    rows.remove(0)
}

/// The commands a run sent to the terminal, its record's writes and removals.
fn acts(sys: &Fake) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter(|c| c.starts_with("tty ") || c.starts_with("write ") || c.starts_with("remove "))
        .collect()
}

/// Named by `--only`, a plan names the one root command and runs nothing; the apply asks
/// the password with a prompt naming fseventsd and its cost, reads it again, signals the
/// very process measured and only then records the restart, and the new one reads back.
#[test]
fn past_the_bar_the_restart_asks_once_and_signals_the_process_measured() {
    let sys = mac();
    let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], Mode::Plan);
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    assert_eq!(
        row.from.as_deref(),
        Some("fseventsd holds 141.0 GiB, past the 8.0 GiB bar")
    );
    assert!(
        row.to.as_deref().is_some_and(|to| to.starts_with(
            "restarted as root once `sudo` takes your password: `sudo kill -TERM 537`; it \
             discards the disk's file-change history"
        )),
        "{row:?}"
    );
    assert_eq!(row.layer, Some(Layer::Workstation));
    assert_eq!(row.class, Some(Class::Elevate));
    assert!(
        acts(&sys).is_empty(),
        "a plan runs nothing: {:?}",
        sys.calls()
    );

    let prompt = Fseventsd::prompt(&service(537, 141 * GIB));
    assert!(
        prompt.starts_with(
            "aterm restarts macOS's fseventsd (pid 537), which holds 141.0 GiB; it discards"
        ) && prompt.ends_with("Password for %u: "),
        "{prompt}"
    );
    sys.on(&validate(537, 141 * GIB), Ran::ok(""));
    sys.on(&kill(537, 141 * GIB), Ran::ok(""));
    let slept = std::sync::atomic::AtomicUsize::new(0);
    sys.on_sleep(move |sys, _| {
        if slept.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
            sys.exited(537);
            sys.services(vec![running(90_001, 3_700_000)]);
        }
    });
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(
        row.to.as_deref(),
        Some("restarted 0 s ago: the new fseventsd (pid 90001) holds 4 MiB")
    );
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("sleep "))
            .count(),
        4,
        "the wait ends once the old one has gone and the new one runs"
    );
    assert_eq!(
        acts(&sys),
        [
            validate(537, 141 * GIB),
            kill(537, 141 * GIB),
            format!("write {STAMPED} 600"),
        ],
        "the password once, the signal to pid 537 alone, then its record"
    );
    assert_eq!(
        Sent::parse(&sys.read_file(std::path::Path::new(STAMPED)).unwrap()),
        Some(Sent {
            pid: 537,
            start_s: STARTED + 537,
            at_s: NOW
        })
    );
}

/// A 152 GB fseventsd takes its time to end (14.7 s on 2026-10-26): the wait is bounded at
/// a minute and more, and one still ending after it reads back as a restart in progress,
/// never a failure. A second run within minutes of it restarts nothing and asks nothing.
/// NEGATIVE CONTROL: once [`RECENT`] has passed, a run past the bar asks again.
#[test]
fn a_slow_exit_is_a_restart_in_progress_and_no_second_restart() {
    assert!(RESTART_WITHIN >= Duration::from_mins(1));
    let sys = mac();
    sys.on(&validate(537, 141 * GIB), Ran::ok(""));
    sys.on(&kill(537, 141 * GIB), Ran::ok(""));
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert!(
        row.to
            .as_deref()
            .is_some_and(|to| to
                .starts_with("restart in progress: the old fseventsd (pid 537) is still ending")),
        "{row:?}"
    );
    let sleeps = sys
        .calls()
        .iter()
        .filter(|c| c.starts_with("sleep "))
        .count();
    let bound = (RESTART_WITHIN.as_millis() / POLL.as_millis()) as usize;
    assert_eq!(sleeps, bound, "the whole bound, then no more");

    let before = acts(&sys).len();
    for named in [false, true] {
        let row = run(
            &item(named),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    }
    assert_eq!(
        acts(&sys).len(),
        before,
        "no second password, no second signal"
    );

    let mut later = mac();
    later.now = NOW + RECENT.as_secs();
    later.file(
        STAMPED,
        Sent {
            pid: 537,
            start_s: STARTED + 537,
            at_s: NOW,
        }
        .line()
        .as_bytes(),
    );
    let row = run(
        &item(true),
        &later,
        vec![running(537, 141 * GIB)],
        Mode::Plan,
    );
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
}

/// The password can take any time: a fseventsd replaced meanwhile (someone else restarted
/// it) is not signalled, and nothing is recorded.
#[test]
fn a_fseventsd_replaced_while_the_password_was_typed_is_left_alone() {
    let sys = mac();
    sys.on(&validate(537, 141 * GIB), Ran::ok(""));
    let looks = vec![
        running(537, 141 * GIB),
        running(537, 141 * GIB),
        running(90_001, 3_700_000),
    ];
    let row = run(&item(true), &sys, looks, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(acts(&sys), [validate(537, 141 * GIB)], "no signal");
}

/// A fseventsd that ended on its own between the reading and the signal (`kill` finds no
/// such process) is no failure: the wait sees launchd's new one, and nothing is recorded,
/// since this run sent nothing. NEGATIVE CONTROL: a `kill` that fails while the process
/// still runs fails the row, with nothing recorded.
#[test]
fn a_fseventsd_that_ended_before_its_signal_is_no_failure() {
    let sys = mac();
    sys.on(&validate(537, 141 * GIB), Ran::ok(""));
    sys.on(
        &kill(537, 141 * GIB),
        Ran::exit(1, "kill: 537: No such process"),
    );
    sys.exited(537);
    let looks = vec![
        running(537, 141 * GIB),
        running(537, 141 * GIB),
        running(537, 141 * GIB),
        running(90_001, 3_700_000),
    ];
    let row = run(&item(true), &sys, looks, Mode::Apply);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
    assert_eq!(
        acts(&sys),
        [validate(537, 141 * GIB), kill(537, 141 * GIB)],
        "nothing recorded"
    );

    let sys = mac();
    sys.on(&validate(537, 141 * GIB), Ran::ok(""));
    sys.on(
        &kill(537, 141 * GIB),
        Ran::exit(1, "Operation not permitted"),
    );
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert_eq!(acts(&sys), [validate(537, 141 * GIB), kill(537, 141 * GIB)]);
}

/// A refused password restarts nothing and records nothing.
#[test]
fn a_refused_password_restarts_nothing() {
    let sys = mac();
    sys.on(&validate(537, 141 * GIB), Ran::exit(1, "Sorry, try again."));
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .is_some_and(|w| w.contains("nothing was restarted")),
        "{row:?}"
    );
    assert_eq!(acts(&sys), [validate(537, 141 * GIB)]);
}

/// A plain `aterm init` never asks for a password, past either bar: it names the command
/// that restarts it now. Named by `--only`, the same run asks from the margin under the
/// large bar, where the window's watch still holds the service large. NEGATIVE CONTROL:
/// under that margin a named run asks nothing.
#[test]
fn only_a_run_that_names_the_item_asks() {
    let sys = mac();
    for footprint in [20 * GIB, 141 * GIB] {
        let row = run(
            &item(false),
            &sys,
            vec![running(537, footprint)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert!(
            row.explain.as_ref().is_some_and(|e| e
                .why
                .contains("`aterm init --only init.fseventsd` restarts it now")),
            "{row:?}"
        );
    }
    assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
    let row = run(&item(true), &sys, vec![running(537, 20 * GIB)], Mode::Plan);
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 7 * GIB + (512 << 20))],
        Mode::Plan,
    );
    assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    assert_eq!(
        row.from.as_deref(),
        Some("fseventsd holds 7.5 GiB, within an eighth under the 8.0 GiB bar")
    );
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 7 * GIB - (1 << 20))],
        Mode::Plan,
    );
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
}

/// Under the bar, with none running, unread, off macOS, for an account that is not an
/// administrator, or with fseventsd sharing its coalition (its footprint not its own),
/// nothing runs and nothing asks.
#[test]
fn under_the_bar_or_unsure_nothing_asks() {
    let mut sys = mac();
    let mut shared = service(537, 141 * GIB);
    shared.live = 3;
    for (look, outcome) in [
        (running(537, 3_700_000), Outcome::Ok),
        (running(537, 6 * GIB), Outcome::Ok),
        (Look::NoneRunning, Outcome::Ok),
        (Look::Unread, Outcome::Unknown),
        (Look::Running(shared, M3), Outcome::Unknown),
    ] {
        let row = run(&item(true), &sys, vec![look.clone()], Mode::Apply);
        assert_eq!(row.outcome, outcome, "{look:?}: {row:?}");
    }
    sys.admin = Some(false);
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    sys.admin = None;
    sys.linux = true;
    let row = run(
        &item(true),
        &sys,
        vec![running(537, 141 * GIB)],
        Mode::Apply,
    );
    assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
    assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
}

#[test]
fn without_a_terminal_the_item_is_blocked_on_init_only() {
    let mut sys = mac();
    sys.tty = false;
    for mode in [Mode::Plan, Mode::Apply] {
        let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], mode);
        assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
        assert!(
            row.why
                .as_deref()
                .is_some_and(|why| why.contains("`aterm init --only init.fseventsd`")),
            "{row:?}"
        );
    }
    assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
}

#[test]
fn competing_restarts_cannot_authorize_or_signal_beside_the_first() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut sys = mac();
    sys.service_restart_lock = Some(dir.path().join("restart.lock"));
    sys.services(vec![running(537, 141 * GIB)]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let first = AtomicBool::new(true);
    sys.tty_model(move |sys, cmd| {
        if cmd.args.iter().any(|arg| arg == "-TERM") {
            if first.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            sys.exited(537);
            sys.services(vec![running(90_001, 3_700_000)]);
        }
        Some(Ran::ok(""))
    });
    let restart = item(true);
    std::thread::scope(|scope| {
        let sys = &sys;
        let restart = &restart;
        let first = scope.spawn(move || restart.apply(sys, &ctx(&[])));
        let entered = entered_rx.recv_timeout(Duration::from_secs(60));
        let (done_tx, done_rx) = mpsc::channel();
        let second = scope.spawn(move || {
            let result = restart.apply(sys, &ctx(&[]));
            done_tx.send(()).unwrap();
            result
        });
        let done = done_rx.recv_timeout(Duration::from_secs(60));
        // Always release the first run before judging either result, including a mutant
        // that blocks behind its password/signal transaction.
        let _ = release_tx.send(());
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        entered.unwrap();
        done.unwrap();
        first.unwrap();
        assert!(
            second.is_err(),
            "the competing transaction must stand aside"
        );
    });
    let transcript = acts(&sys);
    let commands: Vec<_> = transcript
        .iter()
        .filter(|call| call.starts_with("tty "))
        .cloned()
        .collect();
    assert_eq!(commands, [validate(537, 141 * GIB), kill(537, 141 * GIB)]);
    assert_eq!(
        Sent::parse(&sys.read_file(std::path::Path::new(STAMPED)).unwrap()),
        Some(Sent {
            pid: 537,
            start_s: STARTED + 537,
            at_s: NOW,
        })
    );
    restart.apply(&sys, &ctx(&[])).unwrap();
    assert_eq!(acts(&sys), transcript);
}

#[test]
fn a_lock_failure_opens_no_terminal_and_failed_auth_releases_the_lock() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut sys = mac();
    sys.service_restart_lock = Some(dir.path().to_path_buf());
    sys.services(vec![running(537, 141 * GIB)]);
    sys.tty_model(|_, _| Some(Ran::ok("")));
    assert!(item(true).apply(&sys, &ctx(&[])).is_err());
    assert!(acts(&sys).is_empty());
    assert!(sys.read_file(std::path::Path::new(STAMPED)).is_err());

    let lock = dir.path().join("restart.lock");
    let mut refused = mac();
    refused.service_restart_lock = Some(lock.clone());
    refused.services(vec![running(537, 141 * GIB)]);
    refused.tty_model(|_, _| Some(Ran::exit(1, "")));
    assert!(item(true).apply(&refused, &ctx(&[])).is_err());
    assert_eq!(acts(&refused), [validate(537, 141 * GIB)]);
    assert!(refused.read_file(std::path::Path::new(STAMPED)).is_err());
    aterm_update_core::FileLock::acquire_within(&lock, Duration::ZERO).unwrap();
}

#[test]
fn a_record_is_one_line_of_three_numbers() {
    let sent = Sent {
        pid: 537,
        start_s: 1,
        at_s: 2,
    };
    assert_eq!(Sent::parse(sent.line().as_bytes()), Some(sent));
    for junk in ["", "537 1", "537 1 2 3", "x 1 2"] {
        assert_eq!(Sent::parse(junk.as_bytes()), None, "{junk:?}");
    }
}

/// A record stamped after now (a clock set back, or a forged record) is no recent restart,
/// so it does not hold the next one off for ever. NEGATIVE CONTROL: a record stamped a
/// second ago still does.
#[test]
fn a_future_timestamp_in_the_stamp_record_does_not_suppress_a_restart() {
    let sys = mac();
    sys.file(
        STAMPED,
        Sent {
            pid: 537,
            start_s: STARTED + 537,
            at_s: NOW - 1,
        }
        .line()
        .as_bytes(),
    );
    let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], Mode::Plan);
    assert_ne!(row.outcome, Outcome::Planned, "{row:?}");
    for future_at in [NOW + 1, u64::MAX] {
        sys.file(
            STAMPED,
            Sent {
                pid: 537,
                start_s: STARTED + 537,
                at_s: future_at,
            }
            .line()
            .as_bytes(),
        );
        let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], Mode::Plan);
        assert_eq!(
            row.outcome,
            Outcome::Planned,
            "future_at={future_at}: {row:?}"
        );
    }
}
