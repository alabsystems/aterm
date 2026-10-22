// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exit nobody is handed is still read as it is seen: the agent's exit while the host
//! thread is down after a panic, and while a park's Commit holds the host.

use super::*;

/// A host over one tab `s-a` its Claude Code runs in, supervised, whose
/// thread panics at its next pass once `boom` is set — its roster read, a
/// test's panic — and at every pass while its `passes_panic` is set, before the pass
/// looks at any tab, whether or not the harness is on
/// ([`Shared::passes_panic`], the test's seam: a tab's own body that panics
/// is that tab's alone, [`tab_caught`]). The session `s-x` is kept for an
/// open tab no agent runs in.
/// Its restart's back-off is held at the `down` gate while that is shut
/// (the back-off seam, on the host thread alone), and is then quick — or,
/// `long`, that long, waited for real and cut short only by what cuts it.
/// `reads` counts its roster reads: one a pass that reads it; `downs` its
/// restart's back-offs begun; `approves` holds the `[harness] approve` each
/// run of a worker's loop ran under, in order.
struct Panicking {
    a: Arc<Acting>,
    host: HostHandle,
    boom: Arc<AtomicBool>,
    down: Arc<PauseGate>,
    reads: Arc<AtomicUsize>,
    downs: Arc<AtomicUsize>,
    approves: Arc<Mutex<Vec<aterm_agent::supervise::config::Approve>>>,
}

fn panicking_host() -> Panicking {
    panicking_host_with(None)
}

fn panicking_host_with(long: Option<Duration>) -> Panicking {
    panicking_host_hooked(long, |_, _| {})
}

/// [`panicking_host_with`], its hooks changed by `tweak` last of all (over
/// the world's and the panics' own) before the host starts.
fn panicking_host_hooked(
    long: Option<Duration>,
    tweak: impl FnOnce(&Arc<Acting>, &mut Hooks),
) -> Panicking {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.open.lock().unwrap().insert("s-x".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let mut h = a.hooks();
    let boom = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let downs = Arc::new(AtomicUsize::new(0));
    let host_thread = format!("{THREAD_PREFIX}host");
    let roster = Arc::clone(&h.roster);
    let (b, r) = (Arc::clone(&boom), Arc::clone(&reads));
    h.roster = Arc::new(move || {
        assert!(
            !b.swap(false, Ordering::SeqCst),
            "the host's pass panics (a test's)"
        );
        r.fetch_add(1, Ordering::SeqCst);
        roster()
    });
    let approves: Arc<Mutex<Vec<_>>> = Arc::default();
    let (body, ran) = (Arc::clone(&h.body), Arc::clone(&approves));
    h.body = Arc::new(move |job: &WorkerJob| {
        ran.lock().unwrap().push(job.opts.policy.approve);
        body(job)
    });
    let down = Arc::new(PauseGate::default());
    let (d, n, backoff) = (
        Arc::clone(&down),
        Arc::clone(&downs),
        Arc::clone(&h.backoff),
    );
    h.backoff = Arc::new(move |attempt| {
        if std::thread::current().name() == Some(host_thread.as_str()) {
            n.fetch_add(1, Ordering::SeqCst);
            d.pass();
            if let Some(long) = long {
                return long;
            }
        }
        backoff(attempt)
    });
    tweak(&a, &mut h);
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, "s-a");
    host.shared
        .kept
        .lock()
        .unwrap()
        .insert("s-x".to_string(), Arc::default());
    Panicking {
        a,
        host,
        boom,
        down,
        reads,
        downs,
        approves,
    }
}

impl Panicking {
    /// The host thread panics at its next pass, and is held down in its
    /// restart's back-off. That pass's panic is the one waited for: its
    /// roster read took `boom`.
    fn panic_and_hold(&self) {
        self.down.shut();
        self.boom.store(true, Ordering::SeqCst);
        ring();
        until("the host down after its panic", || {
            self.down.held() && !self.boom.load(Ordering::SeqCst)
        });
    }

    /// The host thread panics at its next pass, and waits out its
    /// restart's (long) back-off — that pass's panic, its roster read
    /// having taken `boom`. Arm `passes_panic` only after it: armed
    /// before, a pass any other test's ring started between the two panicked
    /// at that seam instead, `boom` was left for the next pass that
    /// reads the roster, and that one — the restarted host's first, under
    /// a policy switched on again — panicked too, into a back-off of an
    /// hour (1 in 50 runs of the exit-watch tests on fc5167e43:
    /// `a_harness_switched_on_again_during_the_hosts_back_off_is_supervised_at_once`).
    fn panic_and_back_off(&self) {
        let downs = self.downs.load(Ordering::SeqCst);
        self.boom.store(true, Ordering::SeqCst);
        ring();
        until("the host in its back-off after its panic", || {
            self.downs.load(Ordering::SeqCst) > downs && !self.boom.load(Ordering::SeqCst)
        });
    }
}

/// Session `s-a` as the host keeps it ([`Kept`]), read under its lock.
fn kept_of<R>(host: &HostHandle, f: impl FnOnce(&Kept) -> R) -> Option<R> {
    let kept = host.shared.kept.lock().unwrap().get("s-a").cloned()?;
    let kept = kept.lock().unwrap();
    Some(f(&kept))
}

/// Whether session `s-a`'s agent's exit is left to the host
/// ([`Kept::unwatched`]).
fn marked(host: &HostHandle) -> bool {
    kept_of(host, |k| k.unwatched.is_some()).unwrap_or(false)
}

/// Whether an exit watch is armed on session `s-a` ([`Kept::exit_watch`]).
fn watch_armed(host: &HostHandle) -> bool {
    kept_of(host, |k| k.exit_watch.is_some()).unwrap_or(false)
}

/// Whether the restored queue keeps tab `sid`'s attempt as a landing no read
/// has reached ([`RestoredAttempt::landed`]): its step landed, and a look at
/// the agent it landed found it holding its tab unread
/// ([`restored_landing_ends`]). Set only as that look ends, so `true` says it
/// has: the step being counted, or the tab asked about, says neither.
fn landed_unread(host: &HostHandle, sid: &str) -> bool {
    host.shared
        .restored
        .lock()
        .unwrap()
        .attempts
        .iter()
        .any(|attempt| attempt.agent.sid == sid && attempt.landed)
}

/// Whether every worker the host published is stopped ([`State::stops`]).
fn all_stopped(host: &HostHandle) -> bool {
    host.shared
        .lock()
        .stops
        .iter()
        .all(|s| s.stop.load(Ordering::SeqCst))
}

/// [`on`], narrowed: `[harness] approve = "none"`.
fn narrowed() -> SupervisorConfig {
    let mut cfg = on();
    cfg.set("approve", "none").unwrap();
    cfg
}

/// A harness switched off while the host is down hands nothing from before:
/// the host thread panics, the agent crashes while it is down — its record
/// read as the exit was seen and kept — and the owner switches the harness off
/// before the host is back, then on again. The switch is news that ends the
/// back-off ([`State::news`]; held here at the gate until the test opens
/// it), and the restart stands down ([`restart_under_policy`]: the worker
/// stopped for good, the exit watch ended), as the passes after it do;
/// switched on again, the host hands no exit from before: nothing is read,
/// started or relaunched.
#[test]
fn a_harness_switched_off_while_the_host_is_down_hands_nothing_from_before() {
    let p = panicking_host();
    p.panic_and_hold();
    p.a.set(&[]);
    until(
        "the crash's record read as it was seen, the host down",
        || watched_read(&p.host).is_some(),
    );
    p.host.set_config(off());
    p.down.open();
    until("its worker stopped for good, the host back", || {
        p.host.live().is_empty()
    });
    assert_eq!(watched_read(&p.host), None, "no exit watched");
    let (runs, looks) = (
        p.a.runs.load(Ordering::SeqCst),
        p.a.exit_looks.load(Ordering::SeqCst),
    );
    p.host.set_config(on());
    let reads = p.reads.load(Ordering::SeqCst);
    until("two passes of the host supervising again", || {
        ring();
        p.reads.load(Ordering::SeqCst) >= reads + 2
    });
    assert_eq!(
        (
            p.host.live(),
            p.a.runs.load(Ordering::SeqCst),
            p.a.exit_looks.load(Ordering::SeqCst),
            p.a.exit_records.lock().unwrap().len(),
            p.a.relaunched.lock().unwrap().len(),
        ),
        (Vec::<String>::new(), runs, looks, 0, 0),
        "(workers, loops run, exit looks, exits decided, relaunched): nothing from before"
    );
    p.host.shutdown_and_join();
}

/// A harness switched off during the host's back-off, with every restarted pass panicking before
/// it visits its workers, is handled by the restart itself ([`restart_under_policy`]): the
/// carried worker stopped for good, its exit watch ended, nothing left to the host.
#[test]
fn a_harness_switched_off_while_the_host_is_down_stops_what_it_carries_itself() {
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.host.shared.passes_panic.store(true, Ordering::SeqCst);
    assert!(watch_armed(&p.host), "the carried worker's agent watched");
    p.host.set_config(off());
    until(
        "the carried worker stopped for good, its agent's watch ended",
        || all_stopped(&p.host) && !watch_armed(&p.host),
    );
    assert!(!marked(&p.host), "nothing left to the host");
    p.host.shutdown_and_join();
}

/// A harness switched off and on again during the host's (long) back-off, with no workers left
/// to carry, is news: the back-off ends at once and the tab is supervised again.
#[test]
fn a_harness_switched_on_again_during_the_hosts_back_off_is_supervised_at_once() {
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.host.shared.passes_panic.store(true, Ordering::SeqCst);
    let downs = p.downs.load(Ordering::SeqCst);
    p.host.set_config(off());
    until(
        "its run panicked again with nothing to carry, backing off",
        || p.downs.load(Ordering::SeqCst) > downs && all_stopped(&p.host),
    );
    p.host.shared.passes_panic.store(false, Ordering::SeqCst);
    let runs = p.a.runs.load(Ordering::SeqCst);
    p.host.set_config(on());
    until("the tab supervised again: its loop runs", || {
        p.a.runs.load(Ordering::SeqCst) > runs
    });
    p.host.shutdown_and_join();
}

/// A Commit that fails during the host's (long) back-off is news: the restart runs again at once
/// and its first pass starts the tab's worker again.
#[test]
fn a_failed_commit_during_the_hosts_back_off_is_supervised_again_at_once() {
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.host.pause_restored();
    p.host.suspend();
    until(
        "the Commit's stop ended the worker the restart carried, its exit left to the host",
        || marked(&p.host),
    );
    let runs = p.a.runs.load(Ordering::SeqCst);
    // The Commit fails.
    p.host.resume();
    p.host.resume_restored();
    p.host.relaunch_stranded(&|_| "in tab 1".to_string());
    until("the tab supervised again: its loop runs", || {
        p.a.runs.load(Ordering::SeqCst) > runs
    });
    assert_eq!(
        (
            p.host.live(),
            marked(&p.host),
            p.a.relaunched.lock().unwrap().len()
        ),
        (vec!["s-a".to_string()], false, 0),
        "(workers, its exit still left to the host, relaunched): the agent alive, its worker back"
    );
    p.host.shutdown_and_join();
}

/// A policy narrowed during the host's back-off reaches the worker it carries: that worker is
/// handed over at once and its successor's loop runs under the new policy, the news cutting the
/// back-off short. When every restarted pass panics, the restart hands it over itself.
#[test]
fn a_policy_narrowed_while_the_host_is_down_reaches_the_workers_it_carries() {
    use aterm_agent::supervise::config::Approve;
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.host.set_config(narrowed());
    until("its successor's loop runs, under the new policy", || {
        p.approves.lock().unwrap().last() == Some(&Approve::None) && p.host.live() == ["s-a"]
    });
    assert_eq!(
        p.approves.lock().unwrap().clone(),
        [Approve::All, Approve::None],
        "one loop under each policy: the old one handed over, never run again"
    );
    p.host.shutdown_and_join();

    // Every pass of the restarted host panics: the restart hands it over itself.
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.host.shared.passes_panic.store(true, Ordering::SeqCst);
    p.host.set_config(narrowed());
    until(
        "handed over by the restart itself, its exit left to the host",
        || marked(&p.host) && all_stopped(&p.host),
    );
    p.host.shutdown_and_join();
}

/// A pass in flight when a Commit's stop comes ends no watch that stop armed:
/// a pass reads the roster — naming the agent in its tab — and before it ends
/// the watches of the agents it names, a park's Commit stops the session's
/// worker, which ends leaving its agent's exit to the host and arms the exit
/// watch. The pass read nothing since: that watch is not its to end. The agent
/// crashes during the hold, a Claude Code started then sweeps its record, and
/// the Commit fails: the crash, read as it was seen, is relaunched.
#[test]
fn a_pass_in_flight_at_a_commits_stop_ends_no_watch_that_stop_armed() {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let mut h = a.hooks();
    let host_thread = format!("{THREAD_PREFIX}host");
    // The host's next pass is held right after its roster read while
    // `hold` is set.
    let gate = Arc::new(PauseGate::default());
    let hold = Arc::new(AtomicBool::new(false));
    let roster = Arc::clone(&h.roster);
    let (g, hd, thread) = (Arc::clone(&gate), Arc::clone(&hold), host_thread.clone());
    h.roster = Arc::new(move || {
        let named = roster();
        if std::thread::current().name() == Some(&thread) && hd.swap(false, Ordering::SeqCst) {
            g.pass();
        }
        named
    });
    // Counts the host's passes past their look at the watches: its start of workers asks the
    // claim epoch first.
    let passed = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&passed);
    h.claim_epoch = Arc::new(move || {
        if std::thread::current().name() == Some(host_thread.as_str()) {
            c.fetch_add(1, Ordering::SeqCst);
        }
        0
    });
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, "s-a");
    gate.shut();
    hold.store(true, Ordering::SeqCst);
    ring();
    until("a pass in flight, its roster read", || gate.held());
    let before = passed.load(Ordering::SeqCst);
    host.pause_restored();
    host.suspend();
    until(
        "the Commit's stop ended the worker, the watch armed",
        || marked(&host) && watch_armed(&host),
    );
    gate.open();
    until("the pass in flight past its look at the watches", || {
        passed.load(Ordering::SeqCst) > before
    });
    assert!(
        watch_armed(&host),
        "the watch the Commit's stop armed ended"
    );
    // The agent crashes while the Commit holds the host.
    a.set(&[]);
    until("the crash read as it was seen, the Commit holding", || {
        watched_read(&host).is_some()
    });
    a.record_gone.store(true, Ordering::SeqCst);
    // The Commit fails.
    host.resume();
    host.resume_restored();
    host.relaunch_stranded(&|_| "in tab 1".to_string());
    until("the crash decided once the host is back", || {
        exit_decided(&a)
    });
    assert_eq!(
        (
            a.exit_records.lock().unwrap().clone(),
            a.relaunched.lock().unwrap().clone(),
        ),
        (vec!["survived"], vec!["s-a".to_string()]),
        "(what the exit left, relaunched)"
    );
    host.shutdown_and_join();
}

/// What the exit watch keeps of session `s-a`'s agent's exit
/// ([`Kept::exit_watch`]): its read, once taken.
pub(super) fn watched_read(host: &HostHandle) -> Option<ExitRecord> {
    let kept = host.shared.kept.lock().unwrap().get("s-a").cloned()?;
    let kept = kept.lock().unwrap();
    kept.exit_watch.as_ref().and_then(|w| w.read.clone())
}

/// The real host's session `s-a` projected onto `HarnessCommitOwed`'s state as
/// [`project_commit_owed`] projects it, plus `down` (the host held in its restart's back-off),
/// `swept` (the crashed agent's record removed), `landed` (an exit watch that hands its exit
/// itself, [`ExitWatched::hands`]) and, while the agent is gone, `seen` (its exit read as it was
/// seen, by the watch or a handed relaunch).
fn project_unhanded(
    state: &aterm_spec::interp::State,
    host: &HostHandle,
    a: &Acting,
    down: bool,
) -> aterm_spec::interp::State {
    let mut seen = project_commit_owed(state, host);
    seen.insert("down", i64::from(down));
    seen.insert("swept", i64::from(a.record_gone.load(Ordering::SeqCst)));
    let landed = kept_of(host, |k| {
        k.exit_watch.as_ref().is_some_and(|w| w.hands.is_some())
    });
    seen.insert("landed", i64::from(landed.unwrap_or(false)));
    if state["agent"] == 0 {
        let read = watched_read(host).is_some_and(|left| left.crashed())
            || a.exit_records.lock().unwrap().contains(&"survived");
        seen.insert("seen", i64::from(read));
    }
    seen
}

/// Tier-1 for `HarnessCommitOwed` (aterm-spec): an exit nobody is handed as it comes, the real
/// host driven and each observed state projected onto the model's ([`project_unhanded`]):
///
/// 1. the host down after a panic (`Panic`): its worker lives on, the crash is read as it is
///    seen (`Crash`) before a sweep (`Sweep`), and the first pass back (`Up`) hands it to the
///    worker (`Hand`, `Read`, `Relaunch`);
/// 2. a Commit's hold: the stopped worker leaves the exit to the host (`Suspend`,
///    `EndUnhanded`), the crash is read during the hold, and after the failed Commit (`Resume`,
///    `Rollback`) a worker reads it alone (`HandUnwatched`, `Read`, `Relaunch`);
/// 3. a landing while the host is down: the landed agent is watched (`Relaunch`), crashes
///    (`Crash`, `Sweep`), and the first pass back hands the watch's read (`Up`,
///    `HandUnwatched`).
///
/// Negative control: the mutants `PanicStops`, `CrashUnseen` and `LandedLost` each break
/// `NoOwedRelaunchLost`.
#[test]
fn an_exit_nobody_is_handed_conforms_to_the_commit_owed_model() {
    let model = aterm_models_harness::harness_commit_owed_model();
    let holds = |state: &aterm_spec::interp::State| {
        for invariant in ["NoOwedRelaunchLost", "NoQueuedRelaunchDropped"] {
            assert!(
                model.check_invariant(invariant, state),
                "{invariant}: {state:?}"
            );
        }
    };

    // 1: the host down after a panic.
    let p = panicking_host();
    let mut state = model.init_state();
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, false),
        state,
        "supervised"
    );
    p.panic_and_hold();
    assert!(model.fire("Panic", &mut state), "{state:?}");
    assert_eq!(p.host.live(), ["s-a"], "its worker lives on");
    assert_eq!(project_unhanded(&state, &p.host, &p.a, true), state, "down");
    p.a.set(&[]);
    assert!(model.fire("Crash", &mut state), "{state:?}");
    until(
        "the crash's record read as it was seen, the host down",
        || watched_read(&p.host).is_some(),
    );
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "read as it was seen"
    );
    p.a.record_gone.store(true, Ordering::SeqCst);
    assert!(model.fire("Sweep", &mut state), "{state:?}");
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "swept"
    );
    assert!(!model.action_enabled("Hand", &state), "{state:?}");
    assert!(
        p.a.exit_records.lock().unwrap().is_empty(),
        "nothing handed"
    );
    p.down.open();
    assert!(model.fire("Up", &mut state), "{state:?}");
    until("the crash relaunched, its worker ended", || {
        !p.a.relaunched.lock().unwrap().is_empty() && p.host.live().is_empty()
    });
    for action in ["Hand", "Read", "Relaunch"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, false),
        state,
        "relaunched"
    );
    assert_eq!(
        (state["done"], p.a.exit_records.lock().unwrap().clone()),
        (1, vec!["survived"])
    );
    holds(&state);
    p.host.shutdown_and_join();

    // 2: a Commit's hold.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let host = HostHandle::start(on(), false, false, a.hooks());
    a.attached(&host, "s-a");
    let mut state = model.init_state();
    host.pause_restored();
    host.suspend();
    assert!(model.fire("Suspend", &mut state), "{state:?}");
    until("the Commit's stop ended the worker", || {
        host.live().is_empty()
    });
    assert!(model.fire("EndUnhanded", &mut state), "{state:?}");
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "its agent's exit left to the host"
    );
    a.set(&[]);
    assert!(model.fire("Crash", &mut state), "{state:?}");
    until(
        "the crash's record read as it was seen, the Commit holding",
        || watched_read(&host).is_some(),
    );
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "read as it was seen"
    );
    a.record_gone.store(true, Ordering::SeqCst);
    assert!(model.fire("Sweep", &mut state), "{state:?}");
    host.resume();
    assert!(model.fire("Resume", &mut state), "{state:?}");
    host.resume_restored();
    host.relaunch_stranded(&|_| "in tab 1".to_string());
    assert!(model.fire("Rollback", &mut state), "{state:?}");
    until("the crash relaunched, its worker ended", || {
        !a.relaunched.lock().unwrap().is_empty() && host.live().is_empty()
    });
    for action in ["HandUnwatched", "Read", "Relaunch"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "relaunched"
    );
    assert_eq!(
        (state["done"], a.exit_records.lock().unwrap().clone()),
        (1, vec!["survived"])
    );
    holds(&state);
    host.shutdown_and_join();

    // 3: a landing while the host is down.
    let p = panicking_host();
    let mut state = model.init_state();
    *p.a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    p.a.set(&[]);
    until("its relaunch tried on its back-off", || {
        p.a.relaunched.lock().unwrap().len() >= 2
    });
    for action in ["Crash", "Hand", "Read"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    p.panic_and_hold();
    assert!(model.fire("Panic", &mut state), "{state:?}");
    p.a.relaunches.lock().unwrap().clear();
    until("a try after the panic landed, its agent read", || {
        p.a.owed.load(Ordering::SeqCst) && p.a.read_in("s-a")
    });
    assert!(model.fire("Relaunch", &mut state), "{state:?}");
    until("the landed agent watched", || {
        kept_of(&p.host, |k| {
            k.exit_watch.as_ref().is_some_and(|w| w.hands.is_some())
        })
        .unwrap_or(false)
    });
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "landed, watched"
    );
    let landed = p.a.relaunched.lock().unwrap().len();
    p.a.set(&[("s-a", Program::Claude)]);
    p.a.set(&[]);
    assert!(model.fire("Crash", &mut state), "{state:?}");
    until(
        "the landed agent's crash read as it was seen, the host down",
        || watched_read(&p.host).is_some(),
    );
    p.a.record_gone.store(true, Ordering::SeqCst);
    assert!(model.fire("Sweep", &mut state), "{state:?}");
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "read as it was seen, swept"
    );
    assert!(!model.action_enabled("HandUnwatched", &state), "{state:?}");
    p.down.open();
    assert!(model.fire("Up", &mut state), "{state:?}");
    until(
        "the landed agent's crash relaunched, its worker ended",
        || p.a.relaunched.lock().unwrap().len() > landed && p.host.live().is_empty(),
    );
    for action in ["HandUnwatched", "Read", "Relaunch"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, false),
        state,
        "relaunched again"
    );
    assert_eq!(state["done"], 1);
    assert_eq!(
        p.a.exit_records
            .lock()
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        [&"survived"].into_iter().collect(),
        "every try on a crash read at its exit"
    );
    holds(&state);
    p.host.shutdown_and_join();

    // Negative control: each mutant breaks `NoOwedRelaunchLost`.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let replay = |actions: &[&str]| {
        let mut old = buggy.init_state();
        for action in actions {
            assert!(buggy.fire(action, &mut old), "{action}: {old:?}");
        }
        old
    };
    let stopped = replay(&[
        "PanicStops",
        "Crash",
        "EndUnhanded",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    assert!(!buggy.check_invariant("NoOwedRelaunchLost", &stopped));
    let unseen = replay(&[
        "Suspend",
        "EndUnhanded",
        "CrashUnseen",
        "Sweep",
        "Resume",
        "Rollback",
        "HandUnwatched",
        "Read",
    ]);
    assert!(!buggy.check_invariant("NoOwedRelaunchLost", &unseen));
    let lost = replay(&[
        "Crash",
        "Hand",
        "Read",
        "Panic",
        "Relaunch",
        "LandedLost",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    assert!(!buggy.check_invariant("NoOwedRelaunchLost", &lost));
}

// Landings and exits a cold restore's queue, a Commit or a host panic leaves to nobody.

/// A cold restore's landing that crashes before a pass names it (the host down after a panic) is
/// watched from its landing: its exit is read as it was seen and relaunched once by the
/// relaunch on exit; the queue's step never runs again.
#[test]
fn a_restored_agent_that_crashes_before_a_pass_names_it_is_relaunched() {
    let steps = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&steps);
    let p = panicking_host_hooked(None, move |a, h| {
        let a = Arc::clone(a);
        h.acts.relaunch_restored = Arc::new(move |sid, _, _, _| {
            n.fetch_add(1, Ordering::SeqCst);
            a.adopted_in(sid, false);
            "adopted".to_string()
        });
    });
    p.a.open.lock().unwrap().insert("s-r".to_string());
    p.panic_and_hold();
    p.host.relaunch_restored(vec![restored("s-r", "in tab 2")]);
    until("the restored agent landed, its attempt over", || {
        steps.load(Ordering::SeqCst) == 1 && p.host.pending_restored().is_empty()
    });
    // The roster names it (nobody reads it, the host is down) and it crashes.
    p.a.set(&[("s-a", Program::Claude), ("s-r", Program::Claude)]);
    p.a.set(&[("s-a", Program::Claude)]);
    p.down.open();
    until(
        "the landed agent's crash relaunched once the host is back",
        || {
            p.a.relaunched
                .lock()
                .unwrap()
                .iter()
                .any(|sid| sid == "s-r")
        },
    );
    // Once: its worker ended, the agent it landed watched, and two passes later no second
    // worker or relaunch has started.
    let gone = |host: &HostHandle| !host.live().iter().any(|sid| sid == "s-r");
    let landed_watched = || {
        let kept = p.host.shared.kept.lock().unwrap().get("s-r").cloned();
        kept.is_some_and(|k| {
            k.lock()
                .unwrap()
                .exit_watch
                .as_ref()
                .is_some_and(|w| w.hands.is_some())
        })
    };
    until("its worker ended, the agent it landed watched", || {
        gone(&p.host) && landed_watched()
    });
    let reads = p.reads.load(Ordering::SeqCst);
    until("two whole passes since, no worker for the tab", || {
        ring();
        p.reads.load(Ordering::SeqCst) >= reads + 2 && gone(&p.host)
    });
    assert_eq!(
        (
            steps.load(Ordering::SeqCst),
            p.a.relaunched.lock().unwrap().clone(),
            p.a.exit_records.lock().unwrap().clone(),
        ),
        (1, vec!["s-r".to_string()], vec!["survived"]),
        "(the queue's steps, relaunched on exit, what the exit left)"
    );
    p.host.shutdown_and_join();
}

/// What a seamless update carries of tab `sid` from `parent` to its successor's restored queue:
/// the owed relaunch and the explicit restart's cause, through the leaf's TOML wire
/// ([`crate::restore::AgentRestore`]), as `App::hand_restored_agents` queues it.
#[cfg(target_os = "macos")]
fn carried_across(parent: &HostHandle, sid: &str) -> Vec<RestoredAgent> {
    parent
        .pending_restored_for(sid)
        .map(|owed| {
            let shell = owed.snap.shell;
            let wire =
                aterm_toml::to_string(&crate::restore::AgentRestore::owed(owed.snap, owed.asked))
                    .expect("the leaf's wire");
            let agent: crate::restore::AgentRestore =
                aterm_toml::from_str(&wire).expect("the successor reads it");
            let asked = agent.asked();
            RestoredAgent {
                sid: sid.to_string(),
                snap: agent
                    .snapshot(sid.to_string(), shell)
                    .expect("an agent this build knows"),
                place: "in tab 1".to_string(),
                cold: true,
                asked,
                was: None,
            }
        })
        .into_iter()
        .collect()
}

/// The explicit restart's end, read under a fleet hold and carried across a seamless update's
/// Commit, is relaunched as the act's once the hold lifts. The successor's queue looks at the
/// held tab without attempting or counting a try (`RESTORED_TRIES`), however many looks.
#[cfg(target_os = "macos")]
#[test]
fn an_asked_exit_carried_through_a_hold_across_a_commit_is_relaunched_once_it_lifts() {
    // The parent: the act's end read under the hold, carried.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let parent = HostHandle::start(on(), false, false, a.hooks());
    a.attached(&parent, "s-a");
    a.restart("s-a", restart_stamp::Cause::Asked);
    a.held.store(true, Ordering::SeqCst);
    a.set(&[]);
    until("carried through the hold, owed", || {
        looks_put_off(&a) >= 2 && parent.pending_restored_snapshot_for("s-a").is_some()
    });
    assert_eq!(
        (
            a.relaunched.lock().unwrap().len(),
            a.held_attempts.load(Ordering::SeqCst)
        ),
        (0, 0),
        "nothing attempted under the hold"
    );
    // A seamless update: the park's layout carries it, and the Commit goes
    // ahead while the hold still stands.
    parent.pause_restored();
    let carried = carried_across(&parent, "s-a");
    parent.suspend();
    parent.shutdown_and_join();
    assert!(
        a.relaunched.lock().unwrap().is_empty(),
        "the parent relaunched nothing"
    );
    // The successor, the hold still on the tab it adopted.
    let b = Arc::new(Acting::default());
    b.open.lock().unwrap().insert("s-a".to_string());
    b.held.store(true, Ordering::SeqCst);
    let (steps, looks) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut h = b.hooks();
    let (world, n, l) = (Arc::clone(&b), Arc::clone(&steps), Arc::clone(&looks));
    // aterm's own end's step: its relaunch line answers `wait:held` under a hold.
    h.acts.relaunch_restored = Arc::new(move |sid, _, _, _| {
        n.fetch_add(1, Ordering::SeqCst);
        l.fetch_add(1, Ordering::SeqCst);
        if world.held.load(Ordering::SeqCst) {
            return "wait:held".to_string();
        }
        world.relaunched.lock().unwrap().push(sid.to_string());
        world.adopted_in(sid, false);
        "adopted".to_string()
    });
    let (status, l) = (Arc::clone(&h.acts.status), Arc::clone(&looks));
    let queue = format!("{THREAD_PREFIX}restored");
    h.acts.status = Arc::new(move |sid| {
        if std::thread::current().name() == Some(queue.as_str()) {
            l.fetch_add(1, Ordering::SeqCst);
        }
        status(sid)
    });
    let successor = HostHandle::start(on(), false, false, h);
    successor.relaunch_restored(carried);
    let tried = || {
        successor
            .shared
            .restored
            .lock()
            .unwrap()
            .attempts
            .iter()
            .find(|attempt| attempt.agent.sid == "s-a")
            .map(|attempt| attempt.tried)
    };
    until("the successor's queue looked at the held tab", || {
        looks.load(Ordering::SeqCst) >= 1
    });
    assert_eq!(
        (steps.load(Ordering::SeqCst), tried()),
        (0, Some(0)),
        "(attempted, counted): a look under the hold attempted and counted nothing"
    );
    until("more looks under the hold", || {
        looks.load(Ordering::SeqCst) >= 4
    });
    assert_eq!(
        (steps.load(Ordering::SeqCst), tried()),
        (0, Some(0)),
        "(attempted, counted): however many looks"
    );
    b.held.store(false, Ordering::SeqCst);
    until("relaunched once the hold lifted", || {
        !b.relaunched.lock().unwrap().is_empty()
    });
    assert_eq!(
        (
            b.asked_exits.lock().unwrap().clone(),
            b.relaunched.lock().unwrap().clone(),
            steps.load(Ordering::SeqCst),
        ),
        (
            vec![Some(restart_stamp::Cause::Asked)],
            vec!["s-a".to_string()],
            0
        ),
        "(the cause relaunched on, relaunched, aterm's own end's steps): as the act's"
    );
    successor.shutdown_and_join();
}

/// The host's restart panicking in its back-off (the one hook on the host thread between a run's
/// catch and the next run) loses none of its workers: the one it carries relaunches the crash
/// after it.
#[test]
fn a_panic_in_the_hosts_restart_loses_none_of_its_workers() {
    let (fail, failed) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (f, fd) = (Arc::clone(&fail), Arc::clone(&failed));
    let p = panicking_host_hooked(None, move |_, h| {
        let host_thread = format!("{THREAD_PREFIX}host");
        let backoff = Arc::clone(&h.backoff);
        h.backoff = Arc::new(move |attempt| {
            let pause = backoff(attempt);
            if std::thread::current().name() == Some(host_thread.as_str())
                && f.swap(false, Ordering::SeqCst)
            {
                fd.store(true, Ordering::SeqCst);
                panic!("the host's restart panics (a test's)");
            }
            pause
        });
    });
    fail.store(true, Ordering::SeqCst);
    p.panic_and_hold();
    p.down.open();
    until("the restart panicked", || failed.load(Ordering::SeqCst));
    // The agent crashes after it.
    p.a.set(&[]);
    until("the crash relaunched", || {
        !p.a.relaunched.lock().unwrap().is_empty()
    });
    assert_eq!(
        (
            p.a.runs.load(Ordering::SeqCst),
            p.a.exit_records.lock().unwrap().clone()
        ),
        (1, vec!["survived"]),
        "(its loop's runs, what the exit left): the worker it carried read the crash"
    );
    p.host.shutdown_and_join();
}

/// A panic inside `host_restarted` itself (`Shared::restart_panics`, after it reset the run and
/// held the workers to the policy) is caught and the restart made again: the worker it carried
/// stays the tab's only loop and relaunches the crash after it. The test above panics in the
/// back-off, after the restart returned.
#[test]
fn a_panic_inside_the_hosts_restart_loses_none_of_its_workers() {
    let p = panicking_host();
    p.host.shared.restart_panics.store(true, Ordering::SeqCst);
    // The run panics; its restart panics before its back-off, and the one
    // made again is held in it.
    p.panic_and_hold();
    assert!(
        !p.host.shared.restart_panics.load(Ordering::SeqCst),
        "the restart panicked"
    );
    let reads = p.reads.load(Ordering::SeqCst);
    p.down.open();
    until("the host back, its worker carried", || {
        p.reads.load(Ordering::SeqCst) > reads && p.host.live() == ["s-a"]
    });
    // The agent crashes after it.
    p.a.set(&[]);
    until("the crash relaunched, its worker ended", || {
        !p.a.relaunched.lock().unwrap().is_empty() && p.host.live().is_empty()
    });
    assert_eq!(
        (
            p.a.runs.load(Ordering::SeqCst),
            p.a.exit_records.lock().unwrap().clone()
        ),
        (1, vec!["survived"]),
        "(its loop's runs, what the exit left): the worker it carried read the crash"
    );
    p.host.shutdown_and_join();
}

/// An asked exit carried across a Commit is not refused by `relaunch = false`:
/// the live worker relaunches the explicit restart's end whatever `[harness]
/// relaunch` says (`on_agent_left`'s `allowed`: a person's restart is not
/// refused by it), and so does the successor's restored queue that carries it:
/// the tab's agent is relaunched as the act's. Negative control: a crash's
/// relaunch carried there is the switch's, and dropped.
#[test]
fn an_asked_exit_carried_across_a_commit_is_relaunched_under_relaunch_false() {
    let mut cfg = on();
    cfg.set("relaunch", "false").unwrap();
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-r".to_string());
    let host = HostHandle::start(cfg, false, false, a.hooks());
    // Control: a crash's carried relaunch is the switch's, dropped at once.
    host.relaunch_restored(vec![restored("s-c", "in tab 3")]);
    assert!(
        host.pending_restored().is_empty(),
        "a crash's carried relaunch is the switch's"
    );
    host.relaunch_restored(vec![RestoredAgent {
        asked: Some(restart_stamp::Cause::Asked),
        ..restored("s-r", "in tab 2")
    }]);
    until("the asked exit relaunched, its attempt over", || {
        !a.relaunched.lock().unwrap().is_empty() && host.pending_restored().is_empty()
    });
    assert_eq!(
        (
            a.relaunched.lock().unwrap().clone(),
            a.asked_exits.lock().unwrap().clone()
        ),
        (
            vec!["s-r".to_string()],
            vec![Some(restart_stamp::Cause::Asked)]
        ),
        "(relaunched, on the cause): the act's, under `relaunch = false`"
    );
    host.shutdown_and_join();
}

/// Tier-1 for `HarnessCommitOwed` (aterm-spec): the landings and exits a cold restore's queue, a
/// panic in the host's restart or a failed read would leave to nobody, the real host driven and
/// each observed state projected onto the model's ([`project_unhanded`]):
///
/// 1. the rollback queue's landing crashes before a pass names it (`QueueSteps`, `Crash`,
///    `HandUnwatched`);
/// 2. the host's restart panics as its back-off ends; its worker lives on and relaunches the
///    crash (`Panic`, `Crash`, `Up`, `Hand`);
/// 3. a worker's try lands an agent no read reaches, which crashes and is tried again
///    (`RelaunchUnread`, `Crash`, `Relaunch`);
/// 4. the queue's step lands an agent no read reaches, which crashes and is stepped again until
///    a look reads it (`QueueStepsUnread`, `Crash`, `QueueReadsLanded`).
///
/// Negative control: the mutants `QueueLandedLost`, `RestartPanicStops`, `UnreadLandedLost` and
/// `QueueUnreadLost` each break `NoOwedRelaunchLost`.
#[test]
fn the_landings_of_round_eighteen_conform_to_the_commit_owed_model() {
    let model = aterm_models_harness::harness_commit_owed_model();
    let holds = |state: &aterm_spec::interp::State| {
        for invariant in ["NoOwedRelaunchLost", "NoQueuedRelaunchDropped"] {
            assert!(
                model.check_invariant(invariant, state),
                "{invariant}: {state:?}"
            );
        }
    };
    let fire = |state: &mut aterm_spec::interp::State, actions: &[&str]| {
        for action in actions {
            assert!(model.fire(action, state), "{action}: {state:?}");
        }
    };
    let watching = |host: &HostHandle| {
        kept_of(host, |k| {
            k.exit_watch.as_ref().is_some_and(|w| w.hands.is_some())
        })
        .unwrap_or(false)
    };

    // 1: the rollback queue's landing.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    *a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    let mut h = a.hooks();
    let steps = Arc::new(AtomicUsize::new(0));
    let (world, n) = (Arc::clone(&a), Arc::clone(&steps));
    h.acts.relaunch_restored = Arc::new(move |sid, _, _, _| {
        n.fetch_add(1, Ordering::SeqCst);
        world.adopted_in(sid, false);
        "adopted".to_string()
    });
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, "s-a");
    let mut state = model.init_state();
    a.set(&[]);
    until("its relaunch tried on its back-off", || {
        !a.relaunched.lock().unwrap().is_empty()
    });
    fire(&mut state, &["Crash", "Hand", "Read"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "owed on its back-off"
    );
    host.pause_restored();
    host.suspend();
    fire(&mut state, &["Suspend"]);
    until("the stopped worker decided: stranded", || {
        kept_of(&host, |k| k.commit == Committed::Stranded).unwrap_or(false)
            && host.live().is_empty()
    });
    fire(&mut state, &["Decide"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "stranded"
    );
    // The Commit fails; the queue's step lands, its agent watched.
    a.relaunches.lock().unwrap().clear();
    host.resume();
    host.resume_restored();
    host.relaunch_stranded(&|_| "in tab 1".to_string());
    fire(&mut state, &["Resume", "Rollback"]);
    until("the queue's step landed, its agent watched", || {
        steps.load(Ordering::SeqCst) == 1 && host.pending_restored().is_empty()
    });
    fire(&mut state, &["QueueSteps"]);
    assert!(watching(&host), "the landed agent watched");
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "landed by the queue, watched"
    );
    // Its agent crashes before a pass names it.
    let tried = a.relaunched.lock().unwrap().len();
    a.set(&[]);
    fire(&mut state, &["Crash"]);
    until(
        "the landed agent's crash relaunched, its worker ended",
        || a.relaunched.lock().unwrap().len() > tried && host.live().is_empty(),
    );
    fire(&mut state, &["HandUnwatched", "Read", "Relaunch"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "relaunched"
    );
    assert_eq!(
        (state["done"], steps.load(Ordering::SeqCst)),
        (1, 1),
        "(relaunched, the queue's steps): once each"
    );
    holds(&state);
    host.shutdown_and_join();

    // 2: a panic in the host's restart.
    let fail = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&fail);
    let p = panicking_host_hooked(None, move |_, h| {
        let host_thread = format!("{THREAD_PREFIX}host");
        let backoff = Arc::clone(&h.backoff);
        h.backoff = Arc::new(move |attempt| {
            let pause = backoff(attempt);
            if std::thread::current().name() == Some(host_thread.as_str())
                && f.swap(false, Ordering::SeqCst)
            {
                panic!("the host's restart panics (a test's)");
            }
            pause
        });
    });
    let mut state = model.init_state();
    fail.store(true, Ordering::SeqCst);
    p.panic_and_hold();
    fire(&mut state, &["Panic"]);
    assert_eq!(project_unhanded(&state, &p.host, &p.a, true), state, "down");
    p.a.set(&[]);
    fire(&mut state, &["Crash"]);
    until("the crash read as it was seen, the host down", || {
        watched_read(&p.host).is_some()
    });
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "read as it was seen"
    );
    // The restart panics as its back-off ends, and is made again.
    p.down.open();
    until("the crash relaunched, its worker ended", || {
        !p.a.relaunched.lock().unwrap().is_empty() && p.host.live().is_empty()
    });
    assert!(!fail.load(Ordering::SeqCst), "the restart panicked");
    fire(&mut state, &["Up", "Hand", "Read", "Relaunch"]);
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, false),
        state,
        "relaunched by the worker it carried"
    );
    holds(&state);
    p.host.shutdown_and_join();

    // 3: a landing no read reaches, the host down.
    let p = panicking_host();
    let mut state = model.init_state();
    *p.a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    p.a.set(&[]);
    until("its relaunch tried on its back-off", || {
        p.a.relaunched.lock().unwrap().len() >= 2
    });
    fire(&mut state, &["Crash", "Hand", "Read"]);
    p.panic_and_hold();
    fire(&mut state, &["Panic"]);
    p.a.unreadable.store(true, Ordering::SeqCst);
    *p.a.holds.lock().unwrap() = Some(true);
    let (tried, asked) = (
        p.a.relaunched.lock().unwrap().len(),
        p.a.still_asked.lock().unwrap().len(),
    );
    p.a.relaunches.lock().unwrap().clear();
    until("a try landed an agent no read reaches", || {
        p.a.relaunched.lock().unwrap().len() > tried
            && p.a.still_asked.lock().unwrap().len() > asked
    });
    fire(&mut state, &["RelaunchUnread"]);
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "landed, unread, its worker's still"
    );
    let landed = p.a.relaunched.lock().unwrap().len();
    p.a.crash_unread(&[]);
    fire(&mut state, &["Crash"]);
    until("relaunched again, its agent read and watched", || {
        p.a.relaunched.lock().unwrap().len() > landed && watching(&p.host)
    });
    fire(&mut state, &["Relaunch"]);
    assert_eq!(
        project_unhanded(&state, &p.host, &p.a, true),
        state,
        "landed again, watched"
    );
    holds(&state);
    p.down.open();
    p.host.shutdown_and_join();

    // 4: the rollback queue's landing no read reaches.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    *a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    let mut h = a.hooks();
    let steps = Arc::new(AtomicUsize::new(0));
    let (world, n) = (Arc::clone(&a), Arc::clone(&steps));
    h.acts.relaunch_restored = Arc::new(move |sid, _, _, _| {
        n.fetch_add(1, Ordering::SeqCst);
        world.adopted_in(sid, false);
        // The agent it landed holds its tab (the store names it so).
        *world.holds.lock().unwrap() = Some(true);
        "adopted".to_string()
    });
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, "s-a");
    let mut state = model.init_state();
    a.set(&[]);
    until("its relaunch tried on its back-off", || {
        !a.relaunched.lock().unwrap().is_empty()
    });
    fire(&mut state, &["Crash", "Hand", "Read"]);
    host.pause_restored();
    host.suspend();
    until("the stopped worker decided: stranded", || {
        kept_of(&host, |k| k.commit == Committed::Stranded).unwrap_or(false)
            && host.live().is_empty()
    });
    fire(&mut state, &["Suspend", "Decide"]);
    // The Commit fails; the queue's step lands an agent no read reaches.
    a.unreadable.store(true, Ordering::SeqCst);
    a.relaunches.lock().unwrap().clear();
    host.resume();
    host.resume_restored();
    host.relaunch_stranded(&|_| "in tab 1".to_string());
    fire(&mut state, &["Resume", "Rollback"]);
    until("the queue's step landed, unread, its attempt kept", || {
        steps.load(Ordering::SeqCst) == 1 && landed_unread(&host, "s-a")
    });
    fire(&mut state, &["QueueStepsUnread"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "landed unread, its attempt the exit's holder"
    );
    // It crashes unread: owed again, and the next step lands unread too.
    *a.holds.lock().unwrap() = None;
    a.set(&[]);
    until("stepped again, landed unread again", || {
        steps.load(Ordering::SeqCst) == 2 && landed_unread(&host, "s-a")
    });
    fire(&mut state, &["Crash", "QueueStepsUnread"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "owed again, landed unread again"
    );
    // A later look reads it: the attempt over, the agent watched.
    a.unreadable.store(false, Ordering::SeqCst);
    until("read, watched, the attempt over", || {
        host.pending_restored().is_empty() && watching(&host)
    });
    fire(&mut state, &["QueueReadsLanded"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "read late, watched"
    );
    assert_eq!(
        (steps.load(Ordering::SeqCst), state["done"]),
        (2, 1),
        "(the queue's steps, relaunched)"
    );
    holds(&state);
    host.shutdown_and_join();

    // Negative control: each mutant breaks `NoOwedRelaunchLost`.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let replay = |actions: &[&str]| {
        let mut old = buggy.init_state();
        for action in actions {
            assert!(buggy.fire(action, &mut old), "{action}: {old:?}");
        }
        assert!(
            !buggy.check_invariant("NoOwedRelaunchLost", &old),
            "{actions:?}: {old:?}"
        );
    };
    replay(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Decide",
        "Resume",
        "Rollback",
        "QueueLandedLost",
    ]);
    replay(&[
        "Panic",
        "RestartPanicStops",
        "Crash",
        "EndUnhanded",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    replay(&[
        "Crash",
        "Hand",
        "Read",
        "Panic",
        "UnreadLandedLost",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    replay(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Decide",
        "Resume",
        "Rollback",
        "QueueStepsUnread",
        "QueueUnreadLost",
    ]);
}
