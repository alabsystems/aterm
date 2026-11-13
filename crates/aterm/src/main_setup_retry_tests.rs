// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    PassNext, SetupRetryHost, SetupWakes, begin_check_again, run_setup_retry,
    supervise_setup_retry, wake_setup_for_console_with,
};
use std::cell::RefCell;
use std::rc::Rc;

struct State {
    claimed: bool,
    busy: bool,
    finished: bool,
    revision: u64,
    applied: Vec<u64>,
    /// The revisions registered for a Stop, pass by pass.
    registered: Vec<u64>,
    drops: Vec<&'static str>,
}

struct Lease {
    kind: &'static str,
    state: Rc<RefCell<State>>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.state.borrow_mut();
        state.drops.push(self.kind);
        if self.kind == "claim" {
            state.claimed = false;
        } else {
            assert!(
                !state.claimed,
                "release the claim before publishing an idle UI"
            );
            state.busy = false;
        }
    }
}

struct Fake {
    state: Rc<RefCell<State>>,
    initial_error: bool,
    finished_error: bool,
    restart_during_backoff: bool,
    restart_during_claim: bool,
    finished_during_busy: bool,
    completions: usize,
    backoffs: usize,
    failures: usize,
    /// What each scripted pass leaves.
    pass: PassNext,
    parks: usize,
    /// Parks before the worker must have ended; with `restart_during_backoff`, the last one
    /// replaces its revision.
    park_limit: usize,
    /// Passes that left a step to retry, and passes and waits that started the pace afresh.
    retried: usize,
    settled: usize,
    /// Claims to find held elsewhere (a person's restart) before one is had.
    claims_held: usize,
    /// Backoffs before the worker must have ended; with
    /// `restart_during_backoff`, the last one replaces its revision.
    backoff_limit: usize,
    /// What the loop said, pass by pass, of retrying a failure.
    retrying: Vec<bool>,
    /// What it last said of retrying as each backoff began.
    retrying_at_backoff: Vec<Option<bool>>,
    /// The pass panics.
    panic_in_apply: bool,
}

fn fixture() -> (Fake, Lease, Lease) {
    let state = Rc::new(RefCell::new(State {
        claimed: true,
        busy: true,
        finished: false,
        revision: 1,
        applied: Vec::new(),
        registered: Vec::new(),
        drops: Vec::new(),
    }));
    (
        Fake {
            state: Rc::clone(&state),
            initial_error: false,
            finished_error: false,
            restart_during_backoff: false,
            restart_during_claim: false,
            finished_during_busy: false,
            completions: 0,
            backoffs: 0,
            failures: 0,
            pass: PassNext::Retry,
            parks: 0,
            park_limit: 1,
            retried: 0,
            settled: 0,
            claims_held: 0,
            backoff_limit: 1,
            retrying: Vec::new(),
            retrying_at_backoff: Vec::new(),
            panic_in_apply: false,
        },
        Lease {
            kind: "claim",
            state: Rc::clone(&state),
        },
        Lease {
            kind: "busy",
            state,
        },
    )
}

impl SetupRetryHost for Fake {
    type Claim = Lease;
    type Busy = Lease;

    fn revision(&mut self) -> Result<u64, String> {
        if std::mem::take(&mut self.initial_error) {
            Err("the initial revision could not be read".into())
        } else {
            Ok(self.state.borrow().revision)
        }
    }
    fn finished(&mut self) -> Result<bool, String> {
        if std::mem::take(&mut self.finished_error) {
            Err("a temporary completion read failure".into())
        } else {
            Ok(self.state.borrow().finished)
        }
    }
    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        let mut state = self.state.borrow_mut();
        assert!(
            state.busy && !state.claimed,
            "the slot comes before the claim"
        );
        if self.claims_held > 0 {
            self.claims_held -= 1;
            return Ok(None);
        }
        state.claimed = true;
        if self.restart_during_claim {
            state.revision += 1;
        }
        Ok(Some(Lease {
            kind: "claim",
            state: Rc::clone(&self.state),
        }))
    }
    fn busy(&mut self) -> Self::Busy {
        let mut state = self.state.borrow_mut();
        assert!(
            !state.claimed && !state.busy,
            "never wait for the slot while holding the claim"
        );
        state.busy = true;
        if self.finished_during_busy {
            state.finished = true;
        }
        Lease {
            kind: "busy",
            state: Rc::clone(&self.state),
        }
    }
    fn register(&mut self) {
        let mut state = self.state.borrow_mut();
        assert!(state.claimed && state.busy, "registered holding both");
        let revision = state.revision;
        state.registered.push(revision);
    }
    fn apply(&mut self) -> Result<PassNext, String> {
        assert!(!std::mem::take(&mut self.panic_in_apply), "a pass panics");
        let mut state = self.state.borrow_mut();
        assert!(state.claimed && state.busy);
        let revision = state.revision;
        assert_eq!(
            state.registered.last(),
            Some(&revision),
            "a pass runs as the registered run"
        );
        state.applied.push(revision);
        Ok(self.pass)
    }
    fn park(&mut self) -> bool {
        let mut state = self.state.borrow_mut();
        assert!(
            !state.claimed && !state.busy,
            "a parked run must leave the form usable"
        );
        self.parks += 1;
        assert!(
            self.parks <= self.park_limit,
            "a stale worker must end without parking again"
        );
        if self.restart_during_backoff && self.parks == self.park_limit {
            state.revision += 1;
        }
        false
    }
    fn retried(&mut self) {
        self.retried += 1;
    }
    fn settled(&mut self) {
        self.settled += 1;
    }
    fn backoff(&mut self) -> bool {
        let mut state = self.state.borrow_mut();
        assert!(
            !state.claimed && !state.busy,
            "idle retries must leave the form usable"
        );
        self.backoffs += 1;
        self.retrying_at_backoff.push(self.retrying.last().copied());
        assert!(
            self.backoffs <= self.backoff_limit,
            "a stale worker must end without another retry"
        );
        if self.restart_during_backoff && self.backoffs == self.backoff_limit {
            state.revision += 1;
        }
        false
    }
    fn retrying(&mut self, retrying: bool) {
        self.retrying.push(retrying);
    }
    fn failure(&mut self, _: &str) {
        self.failures += 1;
    }
    fn completed(&mut self) {
        assert!(
            self.finished_during_busy,
            "other scripted runs remain pending until replaced"
        );
        self.completions += 1;
    }
}

#[test]
fn a_stop_or_restore_committed_while_acquiring_busy_prevents_a_stale_retry() {
    let (mut host, claim, busy) = fixture();
    host.finished_error = true;
    host.finished_during_busy = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert!(
        state.applied.is_empty(),
        "recheck completion after acquiring busy"
    );
    assert_eq!(host.completions, 1);
    assert_eq!(host.backoffs, 1);
    assert_eq!(state.drops, ["claim", "busy", "busy"]);
    assert!(!state.claimed && !state.busy);
}

#[test]
fn pending_setup_releases_claim_and_busy_before_a_same_plan_rerun_retires_it() {
    let (mut host, claim, busy) = fixture();
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert_eq!(state.applied, [1]);
    assert_eq!(state.drops, ["claim", "busy"]);
    assert!(!state.claimed && !state.busy);
    assert_eq!((host.retried, host.parks, host.backoffs), (1, 1, 0));
}

#[test]
fn a_pass_waiting_on_a_person_parks_instead_of_retrying_every_minute() {
    let (mut host, claim, busy) = fixture();
    host.pass = PassNext::Park;
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert_eq!(state.applied, [1]);
    assert_eq!((host.parks, host.backoffs), (1, 0));
    assert!(!state.claimed && !state.busy);
}

/// The run goes on by itself whatever a pass leaves: a step to retry and a step that waits
/// on a person or a pause of its own both park, a pass with nothing to do backs off, and the
/// worker says it goes on from its first pass to its end, which alone stands it down.
#[test]
fn a_run_says_it_goes_on_by_itself_until_its_worker_ends() {
    for (pass, parks, backoffs) in [
        (PassNext::Retry, 3, 0),
        (PassNext::Park, 3, 0),
        (PassNext::Done, 0, 3),
    ] {
        let (mut host, claim, busy) = fixture();
        host.pass = pass;
        host.park_limit = 3;
        host.backoff_limit = 3;
        // The third wait replaces the run, which ends the worker.
        host.restart_during_backoff = true;
        run_setup_retry(&mut host, Some(claim), busy, 1);
        assert_eq!(host.state.borrow().applied.len(), 3, "{pass:?}");
        assert_eq!((host.parks, host.backoffs), (parks, backoffs), "{pass:?}");
        assert_eq!(
            host.retrying,
            [true, false],
            "{pass:?}: said once, stood down at the end"
        );
    }
}

/// The run's pace counts the passes in a row that left a step to retry, and starts afresh
/// only when a pass leaves none, or a wait asks for a pass now (a person's Check again, a
/// fact the run waits on): never at a wait that ended by itself, so no wake buys the run its
/// short pauses back, and none makes them longer (a pass a wake brought before its pause ran
/// out starts that pause again at the same length: `aterm_cli::macos_doctor::PassRetry`).
#[test]
fn a_run_counts_its_retries_until_a_pass_or_a_wait_starts_them_afresh() {
    struct Paced {
        revision: u64,
        passes: std::collections::VecDeque<PassNext>,
        parks: usize,
        /// The parks that end asking for a pass now.
        fresh: &'static [usize],
        said: Vec<&'static str>,
    }
    impl Paced {
        fn wait(&mut self, what: &'static str) -> bool {
            self.parks += 1;
            self.said.push(what);
            if self.passes.is_empty() {
                self.revision += 1;
            }
            self.fresh.contains(&self.parks)
        }
    }
    impl SetupRetryHost for Paced {
        type Claim = ();
        type Busy = ();
        fn revision(&mut self) -> Result<u64, String> {
            Ok(self.revision)
        }
        fn finished(&mut self) -> Result<bool, String> {
            Ok(false)
        }
        fn claim(&mut self) -> Result<Option<()>, String> {
            Ok(Some(()))
        }
        fn busy(&mut self) {}
        fn register(&mut self) {}
        fn apply(&mut self) -> Result<PassNext, String> {
            self.said.push("pass");
            Ok(self.passes.pop_front().expect("a scripted pass"))
        }
        fn backoff(&mut self) -> bool {
            self.wait("backoff")
        }
        fn retried(&mut self) {
            self.said.push("retried");
        }
        fn settled(&mut self) {
            self.said.push("settled");
        }
        fn park(&mut self) -> bool {
            self.wait("park")
        }
        fn retrying(&mut self, _: bool) {}
        fn failure(&mut self, _: &str) {}
        fn completed(&mut self) {}
    }
    use PassNext::{Done, Park, Retry};
    let mut host = Paced {
        revision: 1,
        passes: [Retry, Retry, Retry, Park, Retry, Done].into(),
        parks: 0,
        fresh: &[2],
        said: Vec::new(),
    };
    run_setup_retry(&mut host, Some(()), (), 1);
    assert_eq!(
        host.said,
        [
            "pass", "retried", "park", // the first retry
            "pass", "retried", "park", "settled", // a fact moved: afresh
            "pass", "retried", "park", // counted from that
            "pass", "settled", "park", // nothing left to retry
            "pass", "retried", "park", // counted from nothing
            "pass", "settled", "backoff", // nothing to do; the run is replaced
        ]
    );
}

/// A TRANSIENT FAILURE NEVER BECOMES A HOLD OR AN ALERT: however long a failure lasts that
/// only the next try can clear (GitHub not answering on this network, a full disk), every
/// wait after a pass that only retried still says the run retries it by itself.
#[test]
fn a_run_that_only_retries_never_stops_retrying_by_itself() {
    struct Retrying {
        revision: u64,
        waits: usize,
        said: Option<bool>,
        said_waiting: Vec<bool>,
    }
    impl Retrying {
        fn wait(&mut self) -> bool {
            self.said_waiting.push(self.said == Some(true));
            self.waits += 1;
            if self.waits == 30 {
                self.revision += 1;
            }
            false
        }
    }
    impl SetupRetryHost for Retrying {
        type Claim = ();
        type Busy = ();
        fn revision(&mut self) -> Result<u64, String> {
            Ok(self.revision)
        }
        fn finished(&mut self) -> Result<bool, String> {
            Ok(false)
        }
        fn claim(&mut self) -> Result<Option<()>, String> {
            Ok(Some(()))
        }
        fn busy(&mut self) {}
        fn register(&mut self) {}
        fn apply(&mut self) -> Result<PassNext, String> {
            Ok(PassNext::Retry)
        }
        fn backoff(&mut self) -> bool {
            self.wait()
        }
        fn retried(&mut self) {}
        fn settled(&mut self) {}
        fn park(&mut self) -> bool {
            self.wait()
        }
        fn retrying(&mut self, retrying: bool) {
            self.said = Some(retrying);
        }
        fn failure(&mut self, _: &str) {}
        fn completed(&mut self) {}
    }
    let mut host = Retrying {
        revision: 1,
        waits: 0,
        said: None,
        said_waiting: Vec::new(),
    };
    // (The pace of the waits is the host's: `a_run_counts_its_retries_until_a_pass_or_a_wait_
    // starts_them_afresh`.)
    run_setup_retry(&mut host, Some(()), (), 1);
    assert_eq!(
        host.said_waiting,
        vec![true; 30],
        "a transient failure stopped being retried by itself"
    );
}

/// A panicking pass unwinds through `StandDown`, which says the run no longer retries; a
/// plain reset after the loop would never run on that path.
#[test]
fn a_pass_that_panics_leaves_nothing_counted_as_retried() {
    let (mut host, claim, busy) = fixture();
    host.panic_in_apply = true;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_setup_retry(&mut host, Some(claim), busy, 1);
    }));
    assert!(unwound.is_err(), "PRECONDITION: the pass panicked");
    assert_eq!(host.retrying, [true, false], "the unwind stands it down");
    let state = host.state.borrow();
    assert!(
        !state.claimed && !state.busy,
        "the claim and the slot go too"
    );
}

/// H28: a pass that panics ends the worker's loop, never the worker: it takes the slot again
/// after a pause and runs the next pass of the same run (the ledger counts the cut-off stage
/// as a crash, so a stage that keeps panicking parks with its own pause). The panicked pass
/// counts on the run's durable pace before the restart waits, so the pause grows with each
/// panic in a row and a relaunch reads the same deadline. Before, the panic left no worker in
/// this process, though the stage it cut off read "Resuming", and then a count in memory that
/// never started afresh timed the restart.
#[test]
fn a_worker_whose_pass_panics_starts_again_and_runs_the_next_pass() {
    let (mut host, claim, busy) = fixture();
    host.panic_in_apply = true;
    host.pass = PassNext::Done;
    host.restart_during_backoff = true;
    let mut restarts = Vec::new();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervise_setup_retry(&mut host, Some(claim), busy, 1, |host| {
            restarts.push(host.retried);
            host.busy()
        });
    }));
    assert!(
        unwound.is_ok(),
        "a panicking pass never ends the worker: it starts again"
    );
    assert_eq!(
        restarts,
        [1],
        "one restart, after the panicked pass counted on the run's pace"
    );
    assert_eq!(
        host.state.borrow().applied,
        [1],
        "the restarted worker ran the run's next pass"
    );
    assert_eq!(host.retrying, [true, false, true, false]);
}

/// While another process holds the claim, the run backs off and still goes on by itself:
/// every backoff, after a pass and for the held claim alike, begins with it saying so.
#[test]
fn a_run_waiting_for_a_claim_held_elsewhere_still_goes_on_by_itself() {
    let (mut host, claim, busy) = fixture();
    host.pass = PassNext::Done;
    host.claims_held = 2;
    host.backoff_limit = 4;
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    assert_eq!(host.state.borrow().applied, [1, 1]);
    assert_eq!(
        host.retrying_at_backoff,
        [Some(true); 4],
        "after the pass, the held claim twice, after the next pass"
    );
}

/// A window's focus gain ends a run's wait only where the run waits on what a person does in
/// System Settings or macOS (an Accounts or Permissions stage left); the startup's waits hear
/// every one. Any other wake ends a run's wait; Check again and a useful native input
/// start its tries afresh.
#[test]
fn a_focus_gain_ends_only_a_run_wait_that_listens_for_it() {
    use std::time::Duration;
    let wakes = SetupWakes::new();
    let seen = wakes.marks();
    wakes.raise_focus();
    assert!(
        !wakes.wait_for_run(seen, false, Duration::ZERO),
        "a park on a transient failure sleeps on"
    );
    assert!(
        wakes.wait_for_run(seen, true, Duration::ZERO),
        "a park on an Accounts or Permissions stage wakes"
    );
    let mut backoff = seen;
    assert!(
        !wakes.back_off_run(&mut backoff, Duration::ZERO),
        "a backoff between two tries waits its minute"
    );
    assert_eq!(wakes.count(), seen.all + 1, "the startup hears it");
    assert!(!wakes.marks().checked_since(seen));
    wakes.raise();
    assert!(wakes.wait_for_run(seen, false, Duration::ZERO));
    assert!(!wakes.marks().checked_since(seen), "no press");
    wakes.raise_check();
    assert!(wakes.marks().checked_since(seen));
}

#[test]
fn a_current_console_return_refreshes_retry_without_becoming_check_again() {
    use std::time::Duration;
    let wakes = SetupWakes::new();
    let seen = wakes.marks();
    wake_setup_for_console_with(
        || Ok(Some(17)),
        |revision| {
            assert_eq!(revision, 17);
            false
        },
        || wakes.raise_input(),
    )
    .unwrap();
    assert!(wakes.wait_for_run(seen, false, Duration::ZERO));
    assert!(wakes.marks().refreshed_since(seen));
    assert!(
        !wakes.marks().checked_since(seen),
        "a local input grants no ledger override"
    );
    let mut consumed = seen;
    assert!(wakes.back_off_run(&mut consumed, Duration::ZERO));
    assert!(consumed.refreshed_since(seen));
    assert!(!wakes.back_off_run(&mut consumed, Duration::ZERO));
    assert!(
        !wakes.marks().refreshed_since(consumed),
        "one recovery, not a retry loop"
    );
    let current = wakes.marks();
    for (revision, stopped) in [(None, false), (Some(18), true)] {
        wake_setup_for_console_with(
            || Ok(revision),
            |read| {
                assert_eq!(Some(read), revision);
                stopped
            },
            || panic!("Off, withdrawn, or stopped preferences wake no run"),
        )
        .unwrap();
    }
    assert!(
        wake_setup_for_console_with(
            || Err("records unread".into()),
            |_| panic!("unknown revision"),
            || panic!("unread scope")
        )
        .is_err()
    );
    assert_eq!(wakes.marks(), current);
}

#[test]
fn a_membership_fact_wakes_the_provisional_profile_without_a_network_change() {
    use std::time::Duration;
    let wakes = SetupWakes::new();
    let mut seen = wakes.marks();
    // Another process may commit authenticated membership on the same LAN. Its state-file
    // notification is a local fact, not a route or window-focus change.
    wakes.raise_facts();
    assert!(wakes.back_off_alab(&mut seen, Duration::ZERO));
    assert!(!wakes.back_off_alab(&mut seen, Duration::ZERO));
}

/// A membership fact ends the profile's local wait, without ending an ordinary retry
/// backoff. Window focus alone supplies no membership evidence.
#[test]
fn membership_facts_wake_scope_reads_but_not_retry_backoffs() {
    use std::time::Duration;
    let wakes = SetupWakes::new();
    let mut seen = wakes.marks();
    wakes.raise_focus();
    assert!(
        !wakes.back_off_alab(&mut seen, Duration::ZERO),
        "a focus gain that does not count: no look"
    );
    assert_eq!(seen, wakes.marks(), "taken as heard");
    wakes.raise_facts();
    assert!(wakes.back_off_alab(&mut seen, Duration::ZERO));
    wakes.raise();
    assert!(wakes.back_off_alab(&mut seen, Duration::ZERO));
    assert!(
        !wakes.back_off_alab(&mut seen, Duration::ZERO),
        "each wake once"
    );
    let mut run = wakes.marks();
    wakes.raise_facts();
    assert!(
        !wakes.back_off_run(&mut run, Duration::ZERO),
        "a local membership hint does not shorten a retry"
    );
}

/// A park whose facts are all watched has a timer only for a retry ahead, never a zero one;
/// a park waiting on a person alone has none. A watch that could not start leaves the park
/// reading its facts once a minute.
#[test]
fn a_park_on_a_person_alone_has_no_timer() {
    use super::park_wait_limit;
    use std::time::Duration;
    assert_eq!(park_wait_limit(None, 100, true, false), None);
    assert_eq!(
        park_wait_limit(Some(130), 100, true, false),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        park_wait_limit(Some(100), 100, true, false),
        Some(Duration::from_secs(1)),
        "a retry the clock calls due now waits a second, never spins"
    );
    assert_eq!(
        park_wait_limit(None, 100, false, false),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        park_wait_limit(Some(110), 100, false, false),
        Some(Duration::from_secs(10))
    );
}

/// The park's timer runs to the first of its mark's next deadline and the run's own pace,
/// and a reading asks for a pass once a source's due deadline is one the worker has not woken
/// for, or once the pace has ended, at its second exactly. NEGATIVE CONTROL: take the later of
/// the two deadlines, or wait for the pace past its end, and a run whose step keeps failing
/// misses its own next pass.
#[test]
fn a_park_times_itself_by_the_first_of_its_deadlines_and_its_pace() {
    use super::{ParkClock, SetupRetryDeadlines};
    let idle = ParkClock::default();
    assert_eq!(idle.next(None), None, "no deadline, no pace: a wake alone");
    assert!(!idle.ready([None; 3], 1_000));
    // The mark's deadline alone.
    assert_eq!(idle.next(Some(1_300)), Some(1_300));
    // The pace alone.
    let paced = ParkClock {
        pace_due: Some(1_120),
        ..ParkClock::default()
    };
    assert_eq!(paced.next(None), Some(1_120));
    assert!(
        !paced.ready([None; 3], 1_119),
        "a second before the pace ends"
    );
    assert!(paced.ready([None; 3], 1_120), "the pace ends: a pass now");
    // Both: whichever comes first.
    assert_eq!(paced.next(Some(1_300)), Some(1_120));
    assert_eq!(paced.next(Some(1_060)), Some(1_060));
    // A due deadline counts once per worker: one it woke for asks nothing of it again.
    let mut woke = SetupRetryDeadlines::default();
    let dues = [None, Some(1_000), None];
    assert!(idle.ready(dues, 1_000), "a due deadline not woken for yet");
    woke.consume(dues);
    let after = ParkClock {
        due_seen: woke,
        ..ParkClock::default()
    };
    assert!(!after.ready(dues, 1_001), "the same deadline, woken for");
    assert!(
        after.ready([Some(1_001), Some(1_000), None], 1_001),
        "another source's deadline still wakes it"
    );
}

#[test]
fn a_park_observes_broker_readiness_without_a_focus_or_file_wake() {
    use std::cell::Cell;
    use std::time::Duration;
    let broker_ready = Cell::new(false);
    let limit = Cell::new(None);
    let looks = Cell::new(0);
    let ready = super::wait_for_setup_park_change_since(
        Some(&false),
        || {
            limit.set(super::park_wait_limit(None, 100, true, !broker_ready.get()));
            broker_ready.get()
        },
        |_| false,
        |now, before| now != before,
        || true,
        || {
            assert_eq!(
                looks.replace(1),
                0,
                "unchanged conditions run no setup effects"
            );
            assert_eq!(
                limit.get(),
                Some(Duration::from_secs(1)),
                "healthy file watches cannot hide local readiness"
            );
            // The keyboard rests while System State remains behind the terminal.
            // No focus, file or network wake is delivered.
            broker_ready.set(true);
        },
    );
    assert!(ready);
    assert_eq!(looks.get(), 1);
    assert_eq!(
        super::park_wait_limit(None, 100, true, false),
        None,
        "a ready broker needs no readiness polling"
    );
}

/// A watched fact moving ends a park's wait, with or without a timer, and no other wait: a
/// run backing off a failure and the startup's waits sleep on.
#[test]
fn a_watched_fact_ends_only_a_parks_wait() {
    use std::time::Duration;
    static WAKES: SetupWakes = SetupWakes::new();
    let seen = WAKES.marks();
    WAKES.raise_facts();
    assert!(!WAKES.wait_for_run(seen, true, Duration::ZERO));
    let mut backoff = seen;
    assert!(!WAKES.back_off_run(&mut backoff, Duration::ZERO));
    assert!(
        !WAKES.wait(seen.all, Duration::ZERO),
        "the startup hears no fact"
    );
    assert!(WAKES.wait_for_park(seen, seen.facts, false, Some(Duration::ZERO)));
    let looked = WAKES.marks();
    assert!(
        !WAKES.wait_for_park(seen, looked.facts, false, Some(Duration::ZERO)),
        "a fact already looked at ends no later wait"
    );
    let (told, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        told.send(WAKES.wait_for_park(seen, looked.facts, false, None))
            .unwrap();
    });
    WAKES.raise_facts();
    assert_eq!(
        heard.recv_timeout(Duration::from_secs(60)),
        Ok(true),
        "a park with no timer wakes on a watched fact"
    );
}

/// A wake lock a panicking holder poisoned still counts and still waits: a park with no
/// timer sleeps until a fact moves, never returning at once to read its mark again in a spin.
#[test]
fn a_poisoned_wake_lock_still_waits_for_a_fact() {
    use std::time::Duration;
    static WAKES: SetupWakes = SetupWakes::new();
    let _ = std::thread::spawn(|| {
        let _held = WAKES.count.lock();
        panic!("a holder panics");
    })
    .join();
    assert!(WAKES.count.is_poisoned());
    let seen = WAKES.marks();
    let (told, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        told.send(WAKES.wait_for_park(seen, seen.facts, false, None))
            .unwrap();
    });
    WAKES.raise_facts();
    assert_eq!(heard.recv_timeout(Duration::from_secs(60)), Ok(true));
    assert_ne!(WAKES.marks().facts, seen.facts, "the fact was counted");
}

#[derive(Clone, Copy)]
enum RouteChange {
    WhileParked,
    DuringLastPass,
    None,
}

fn retry_with_route_change(pending_secrets: bool, change: RouteChange) {
    use super::{wait_for_setup_park_change_since, with_setup_pass_mark};
    use std::cell::Cell;

    struct Routed {
        inner: Fake,
        pending_secrets: bool,
        change: RouteChange,
        network: Cell<u8>,
        before_pass: Option<(u64, Option<u8>)>,
        waits: usize,
    }

    impl SetupRetryHost for Routed {
        type Claim = Lease;
        type Busy = Lease;

        fn revision(&mut self) -> Result<u64, String> {
            self.inner.revision()
        }
        fn finished(&mut self) -> Result<bool, String> {
            self.inner.finished()
        }
        fn claim(&mut self) -> Result<Option<Lease>, String> {
            self.inner.claim()
        }
        fn busy(&mut self) -> Lease {
            self.inner.busy()
        }
        fn register(&mut self) {
            self.inner.register();
        }
        fn apply(&mut self) -> Result<PassNext, String> {
            let result = with_setup_pass_mark(
                &mut self.before_pass,
                || (1, self.pending_secrets.then(|| self.network.get())),
                || {
                    let result = self.inner.apply();
                    if matches!(self.change, RouteChange::DuringLastPass)
                        && self.inner.state.borrow().applied.len() == 1
                    {
                        self.network.set(2);
                    }
                    result
                },
            );
            if self.inner.parks != 0 {
                // The route or explicit wake let the next pass finish.
                self.inner.state.borrow_mut().finished = true;
            }
            result
        }
        fn backoff(&mut self) -> bool {
            self.inner.backoff()
        }
        fn retried(&mut self) {
            self.inner.retried();
        }
        fn settled(&mut self) {
            self.inner.settled();
        }
        fn park(&mut self) -> bool {
            self.inner.park();
            let state = Rc::clone(&self.inner.state);
            let wakes = Cell::new(0);
            wait_for_setup_park_change_since(
                self.before_pass.take().as_ref(),
                // Membership remains Unconfigured. Pending network work
                // adds a route to the CLI's opaque park mark.
                || (1, self.pending_secrets.then(|| self.network.get())),
                |_| false,
                |current, before| current != before,
                || wakes.get() == 0,
                || {
                    self.waits += 1;
                    assert!(
                        !matches!(self.change, RouteChange::DuringLastPass),
                        "the fact changed inside the last pass; do not wait for another change"
                    );
                    assert!(self.waits <= 3, "the wake must end this wait");
                    let mut state = state.borrow_mut();
                    assert!(!state.claimed && !state.busy);
                    assert_eq!(state.applied.len(), 1, "a retry parks at once");
                    if matches!(self.change, RouteChange::None) {
                        // An unchanged failure stays parked. Stop ends the
                        // inert run without another setup attempt.
                        state.finished = true;
                        wakes.set(1);
                    } else if self.waits == 2 {
                        self.network.set(2);
                    } else if self.waits == 3 {
                        // A permissions-only run ignores that route. Its
                        // unchanged mark waits until an explicit wake.
                        assert!(!self.pending_secrets);
                        wakes.set(1);
                    }
                },
            );
            false
        }
        fn retrying(&mut self, retrying: bool) {
            self.inner.retrying(retrying);
        }
        fn failure(&mut self, why: &str) {
            self.inner.failure(why);
        }
        fn completed(&mut self) {
            self.inner.completions += 1;
        }
    }

    let (inner, claim, busy) = fixture();
    let mut host = Routed {
        inner,
        pending_secrets,
        change,
        network: Cell::new(1),
        before_pass: None,
        waits: 0,
    };
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let (waits, extra_pass) = match change {
        RouteChange::WhileParked => (if pending_secrets { 2 } else { 3 }, 1),
        RouteChange::DuringLastPass => (0, 1),
        RouteChange::None => (1, 0),
    };
    assert_eq!(host.waits, waits);
    assert_eq!(host.inner.parks, 1);
    assert_eq!(host.inner.backoffs, 0);
    assert_eq!(host.inner.state.borrow().applied.len(), 1 + extra_pass);
    assert_eq!(host.inner.completions, 1);
}

#[test]
fn a_route_change_wakes_a_retrying_run_without_another_pass_while_parked() {
    for pending_secrets in [true, false] {
        retry_with_route_change(pending_secrets, RouteChange::WhileParked);
    }
}

#[test]
fn late_setup_facts_changed_inside_the_last_pass_wake_before_waiting() {
    retry_with_route_change(true, RouteChange::DuringLastPass);
}

#[test]
fn late_setup_facts_unchanged_stay_parked() {
    retry_with_route_change(true, RouteChange::None);
}

#[test]
fn a_parked_retry_deadline_wakes_once_and_an_advanced_deadline_wakes_again() {
    use super::{SetupRetryDeadlines, wait_for_setup_park_change};
    use std::cell::Cell;

    let mut seen = SetupRetryDeadlines::default();
    let first = wait_for_setup_park_change(
        || [Some(100), None, None],
        |mark| seen.unseen(*mark),
        || true,
        || panic!("an already-due attempt must wake at park entry"),
    );
    seen.consume(first);

    let waits = Cell::new(0);
    wait_for_setup_park_change(
        || [Some(100), None, None],
        |mark| seen.unseen(*mark),
        || waits.get() < 2,
        || waits.set(waits.get() + 1),
    );
    assert_eq!(waits.get(), 2, "the same stale due token stays parked");

    let next = wait_for_setup_park_change(
        || [Some(100), Some(600), None],
        |mark| seen.unseen(*mark),
        || true,
        || panic!("a stale Secrets token must not hide a new Mac settings deadline"),
    );
    seen.consume(next);
    seen.consume([Some(100), None, None]);
    assert!(
        !seen.unseen([Some(100), Some(600), None]),
        "spent once, even if absent briefly"
    );

    let later = wait_for_setup_park_change(
        || [Some(200), Some(600), None],
        |mark| seen.unseen(*mark),
        || true,
        || panic!("the next Secrets attempt owns its own new deadline"),
    );
    seen.consume(later);
    assert!(!seen.unseen([Some(200), Some(600), None]));
    assert!(seen.unseen([Some(200), Some(1_200), None]));
    // A parked stage's pause is a source of its own: no other token hides it.
    assert!(seen.unseen([Some(200), Some(600), Some(600)]));
    seen.consume([None, None, Some(600)]);
    assert!(!seen.unseen([Some(200), Some(600), Some(600)]));
}

#[test]
fn a_same_network_retry_wakes_when_due_while_a_held_attempt_has_no_timer() {
    use super::wait_for_setup_park_change;
    use std::cell::Cell;

    for timed in [true, false] {
        let waits = Cell::new(0);
        let mark = wait_for_setup_park_change(
            || (1, (timed && waits.get() >= 2).then_some(100)),
            |mark| mark.1.is_some(),
            || waits.get() < 3,
            || waits.set(waits.get() + 1),
        );
        assert_eq!(waits.get(), if timed { 2 } else { 3 });
        assert_eq!(mark.1, timed.then_some(100));
    }
}

#[test]
fn a_revision_changed_during_reacquisition_prevents_any_apply() {
    let (mut host, claim, busy) = fixture();
    host.finished_error = true;
    host.restart_during_claim = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert!(state.applied.is_empty());
    assert_eq!(state.drops, ["claim", "busy", "claim", "busy"]);
    assert!(!state.claimed && !state.busy);
    assert_eq!(host.failures, 1);
}

#[test]
fn a_revision_read_error_releases_resources_before_retrying_the_admitted_run() {
    let (mut host, claim, busy) = fixture();
    host.initial_error = true;
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert!(state.applied.is_empty());
    assert_eq!(state.drops, ["claim", "busy"]);
    assert!(!state.claimed && !state.busy);
    assert_eq!(host.failures, 1);
    assert_eq!(host.backoffs, 1);
}

#[test]
fn authorization_fence_does_not_adopt_a_run_replaced_before_retry_entry() {
    let (mut host, claim, busy) = fixture();
    host.state.borrow_mut().revision = 2;
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    assert!(
        host.state.borrow().applied.is_empty(),
        "a late baseline read must not adopt revision 2 with revision 1's password"
    );
}

#[test]
fn the_slot_comes_before_the_claim_and_a_held_claim_gives_the_slot_back() {
    // The worker holds the slot but not the claim, which a person's
    // restart holds: that restart claims while holding the slot, so the
    // worker must give the slot back rather than keep it and wait.
    let (mut host, claim, busy) = fixture();
    drop(claim);
    host.state.borrow_mut().drops.clear();
    host.claims_held = 1;
    host.backoff_limit = 2;
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, None, busy, 1);
    let state = host.state.borrow();
    assert_eq!(state.applied, [1]);
    assert_eq!(state.drops, ["busy", "claim", "busy"]);
    assert!(!state.claimed && !state.busy);
    assert_eq!(
        (host.backoffs, host.parks),
        (1, 1),
        "a backoff for the held claim, then the pass's retry parks on the run's pace"
    );
}

#[test]
fn a_run_registers_for_stop_only_once_its_revision_is_current() {
    // The startup's run waited for the slot while a person's choice
    // admitted revision 2 and registered it: the stale run ends without
    // taking revision 2's place, so a Stop still stops revision 2.
    let (mut host, claim, busy) = fixture();
    drop(claim);
    host.state.borrow_mut().revision = 2;
    run_setup_retry(&mut host, None, busy, 1);
    let state = host.state.borrow();
    assert!(
        state.registered.is_empty(),
        "a replaced run never registers"
    );
    assert!(state.applied.is_empty());
    drop(state);
    // The current run registers before each pass, holding the slot and
    // the claim.
    let (mut host, claim, busy) = fixture();
    host.restart_during_backoff = true;
    run_setup_retry(&mut host, Some(claim), busy, 1);
    let state = host.state.borrow();
    assert_eq!(state.registered, [1]);
    assert_eq!(state.applied, [1]);
}

#[test]
fn a_backoff_returns_early_once_per_wake() {
    let wakes = SetupWakes::new();
    let mut seen = wakes.count();
    // A wake during the pass (Check again, a reading that found a fix)
    // ends the first backoff at once; the one-minute limit is a hang
    // detector, never reached.
    wakes.raise();
    assert!(wakes.back_off(&mut seen, std::time::Duration::from_secs(60)));
    // With no new wake the next backoff waits its whole limit, so a read
    // or claim that keeps failing never spins the loop.
    assert!(!wakes.back_off(&mut seen, std::time::Duration::from_millis(1)));
    wakes.raise();
    assert!(wakes.back_off(&mut seen, std::time::Duration::from_secs(60)));
}

#[test]
fn check_again_says_its_reading_and_its_check_apart() {
    let order = RefCell::new(Vec::new());
    // A pass holds the setup slot: the forced reading was queued, but the
    // check was refused. Check again started; nothing is labelled a check.
    let started = begin_check_again(
        || {
            order.borrow_mut().push("observe");
            true
        },
        || order.borrow_mut().push("wake"),
        || false,
    );
    assert_eq!(*order.borrow(), ["observe", "wake"]);
    assert_eq!(
        started,
        aterm_gui::CheckStarted {
            reading: true,
            check: false
        }
    );
    let started = begin_check_again(|| false, || {}, || true);
    assert_eq!(
        started,
        aterm_gui::CheckStarted {
            reading: false,
            check: true
        }
    );
}
