// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN EXIT NOBODY IS HANDED IS STILL READ AS IT IS SEEN (round seventeen):
//! the agent's exit while the host thread is down after a panic, and while a
//! park's Commit holds the host.

use super::*;

/// The looks ONE read of a record that survived its exit takes
/// ([`relaunch::exit_record`]: the first, then one each `EXIT_LOOK` until
/// `EXIT_SETTLE` has passed): once that many are taken
/// ([`Acting::exit_looked`]), the record there at each, the read says
/// `survived`, whatever becomes of the record after.
pub(super) fn a_whole_settle() -> usize {
    let looks = relaunch::EXIT_SETTLE.as_millis() / relaunch::EXIT_LOOK.as_millis();
    usize::try_from(looks).unwrap_or(usize::MAX) + 1
}

/// A host over one tab `s-a` its Claude Code runs in, supervised, whose
/// thread panics at its next pass once `boom` is set — its roster read, a
/// test's panic — and at every pass while `always` is set (its look at
/// whether a tab with no worker is open, [`forget_closed`]'s, which a pass
/// takes before it visits its workers whether or not the harness is on:
/// the session `s-x` is kept for an open tab no agent runs in, so every
/// pass asks it).
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
    always: Arc<AtomicBool>,
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
    let always = Arc::new(AtomicBool::new(false));
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
    let open = Arc::clone(&h.acts.open);
    let (every, thread) = (Arc::clone(&always), host_thread.clone());
    h.acts.open = Arc::new(move |sid: &str| {
        assert!(
            !(every.load(Ordering::SeqCst) && std::thread::current().name() == Some(&thread)),
            "every pass of the host panics (a test's)"
        );
        open(sid)
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
        always,
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
    /// having taken `boom`. Arm `always` only after it: armed
    /// before, a pass any other test's ring started between the two panicked
    /// at its `open` look instead, `boom` was left for the next pass that
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

/// A CRASH WHILE THE HOST IS DOWN AFTER A PANIC IS RELAUNCHED (the residual
/// stated on `Workers`, round fourteen): the host thread panics, and while
/// its restart waits out its back-off the supervised agent crashes. The
/// restarted host's first pass reads the agent gone since as an exit, and
/// it is relaunched on its conversation, on the record its exit left.
///
/// FAILS ON 1afcdade3: dropping the dead host's workers stopped every one —
/// no hand-over, no Commit — so each ended leaving no exit to the host
/// ([`Kept::unwatched`]); the restarted host found the roster naming no
/// agent in the tab and nothing marked there, and the crash was never
/// relaunched, with nothing said (the wait for it runs out).
#[test]
fn a_crash_while_the_host_is_down_after_a_panic_is_relaunched() {
    let p = panicking_host();
    p.panic_and_hold();
    // The agent crashes while nothing of the host runs.
    p.a.set(&[]);
    p.down.open();
    until("the crash relaunched once the host is back", || {
        !p.a.relaunched.lock().unwrap().is_empty()
    });
    assert_eq!(
        (
            p.a.relaunched.lock().unwrap().clone(),
            p.a.exit_records.lock().unwrap().clone(),
        ),
        (vec!["s-a".to_string()], vec!["survived"]),
        "(relaunched, what the exit left)"
    );
    p.host.shutdown_and_join();
}

/// ITS RECORD IS READ AS THE EXIT IS SEEN, NOT WHEN THE HOST IS BACK (D2 of
/// the 2026-09-26 live test, in the panic's window): the agent crashes while
/// the host is down, and a Claude Code started meanwhile removes the dead
/// agent's session record — as any Claude Code that starts does — before
/// the host is back. The crash's record was read at its exit, so it is
/// relaunched as a crash, never left as a graceful exit.
///
/// FAILS ON 1afcdade3: nothing reads the exit while the host is down (the
/// wait for its read runs out).
#[test]
fn the_record_of_a_crash_while_the_host_is_down_is_read_as_it_is_seen() {
    let p = panicking_host();
    p.panic_and_hold();
    p.a.set(&[]);
    until(
        "the crash's record read as it was seen, the host down",
        || p.a.exit_looked.load(Ordering::SeqCst) >= a_whole_settle(),
    );
    // Another Claude Code starts, and sweeps the dead agent's record.
    p.a.record_gone.store(true, Ordering::SeqCst);
    p.down.open();
    until("the crash decided once the host is back", || {
        !p.a.exit_records.lock().unwrap().is_empty()
    });
    assert_eq!(
        (
            p.a.exit_records.lock().unwrap().clone(),
            p.a.relaunched.lock().unwrap().clone(),
        ),
        (vec!["survived"], vec!["s-a".to_string()]),
        "(what the exit left, relaunched)"
    );
    p.host.shutdown_and_join();
}

/// THE EXIT-ONLY WORKER'S READ IS THE ONE TAKEN AS THE EXIT WAS SEEN (D2 of
/// the 2026-09-26 live test, in a Commit's hold; the residual stated in the
/// robustness handoff's §4): a park's Commit stops the session's worker
/// with no exit handed, the agent crashes while the Commit holds the host,
/// and a Claude Code started during the hold removes the dead agent's
/// record. The Commit fails; the host hands the exit to a worker that reads
/// it alone ([`exit_main`]), which relaunches it as the crash it was.
///
/// FAILS ON 1afcdade3: no pass of a suspended host hands an exit and
/// nothing else reads one, so the record was read only by the worker the
/// resumed host started — after the sweep, as a graceful exit, never
/// relaunched (the wait for the read during the hold runs out).
#[test]
fn a_crash_while_a_commit_holds_the_host_is_read_as_it_is_seen() {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let host = HostHandle::start(on(), false, false, a.hooks());
    a.attached(&host, "s-a");
    host.pause_restored();
    host.suspend();
    until("the Commit's stop ended the worker", || {
        host.live().is_empty()
    });
    // The agent crashes while the Commit holds the host.
    a.set(&[]);
    until(
        "the crash's record read as it was seen, the Commit holding the host",
        || a.exit_looked.load(Ordering::SeqCst) >= a_whole_settle(),
    );
    // Another Claude Code starts during the hold, and sweeps the record.
    a.record_gone.store(true, Ordering::SeqCst);
    // The Commit fails.
    host.resume();
    host.resume_restored();
    host.relaunch_stranded(&|_| "in tab 1".to_string());
    until("the crash decided once the host is back", || {
        !a.exit_records.lock().unwrap().is_empty()
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

/// A RELAUNCH A WORKER'S BACK-OFF OWES WHEN THE HOST PANICS IS STILL MADE
/// (round seventeen; the same drop as above): the agent crashed, its worker
/// read the crash and tries its relaunch on its back-off (each try
/// `wait:shell-prompt`), and the host thread panics. The worker runs on
/// through the host's back-off, and its next try lands.
///
/// FAILS ON 1afcdade3: the panic's drop stopped the worker, whose back-off
/// read the stop as one for good: it settled what it owed and tried no more
/// (the wait for a try after the panic runs out).
#[test]
fn a_relaunch_owed_when_the_host_panics_is_still_made() {
    let p = panicking_host();
    *p.a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    p.a.set(&[]);
    until("its relaunch tried on its back-off", || {
        p.a.relaunched.lock().unwrap().len() >= 2
    });
    p.panic_and_hold();
    // The shell takes the line now: the next try lands.
    let tried = p.a.relaunched.lock().unwrap().len();
    p.a.relaunches.lock().unwrap().clear();
    until("a try after the panic, landed", || {
        p.a.relaunched.lock().unwrap().len() > tried && p.a.owed.load(Ordering::SeqCst)
    });
    assert_eq!(
        p.a.exit_records
            .lock()
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        [&"survived"].into_iter().collect(),
        "every try on the crash read at its exit"
    );
    p.down.open();
    p.host.shutdown_and_join();
}

/// A HARNESS SWITCHED OFF WHILE THE HOST IS DOWN HANDS NOTHING FROM BEFORE
/// (round seventeen; the hazard the residual on [`Workers`] named): the host
/// thread panics, the agent crashes while it is down — its record read as
/// the exit was seen and kept — and the owner switches the harness off
/// before the host is back, then on again. The switch is news that ends the
/// back-off ([`State::news`]; held here at the gate until the test opens
/// it), and the restart stands down ([`restart_under_policy`]: the worker
/// stopped for good, the exit watch ended), as the passes after it do;
/// switched on again, the host hands no exit from before: nothing is read,
/// started or relaunched.
///
/// THE CONTROL of the panic's reproductions: a mark the panic left on the
/// session would have outlived the switch (the restart gave up then, and no
/// pass cleared it), and the next host switched on would hand that exit.
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

/// A HARNESS SWITCHED OFF WHILE THE HOST IS DOWN STOPS WHAT IT CARRIES
/// ITSELF (round seventeen, the review of its first cut): the host thread
/// panics and waits out a long back-off, every pass of its restarted run
/// panicking again before it visits its workers, and the owner switches the
/// harness off. The switch is news: the back-off ends at once, and the
/// restart itself stops the worker it carried for good and ends its agent's
/// exit watch ([`restart_under_policy`]) — no pass gets so far — leaving
/// nothing to the host. THE CONTROL for the restart's own stand-down: the
/// control above holds its back-off at a gate and runs whole passes after
/// it, which do the same stops.
#[test]
fn a_harness_switched_off_while_the_host_is_down_stops_what_it_carries_itself() {
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.always.store(true, Ordering::SeqCst);
    assert!(watch_armed(&p.host), "the carried worker's agent watched");
    p.host.set_config(off());
    until(
        "the carried worker stopped for good, its agent's watch ended",
        || all_stopped(&p.host) && !watch_armed(&p.host),
    );
    assert!(!marked(&p.host), "nothing left to the host");
    p.host.shutdown_and_join();
}

/// A HARNESS SWITCHED ON AGAIN DURING THE HOST'S BACK-OFF IS SUPERVISED AT
/// ONCE (round seventeen, the review of its first cut): the host thread
/// panics, the owner switches the harness off — the restart stops what it
/// carried — and its run panics again with nothing left to carry, so it
/// waits out its next (long) back-off; then the owner switches the harness
/// on again. The switch is news: the back-off ends at once, and the tab is
/// supervised again.
///
/// FAILS ON 70537fad5: a switch cut the back-off only while the restart
/// carried workers to stop, so the switch-on with none waited out the
/// whole back-off (the wait for the tab's loop runs out). Before that cut
/// (1afcdade3) the switch-off ended the host thread, and the switch-on
/// started a fresh one at once.
#[test]
fn a_harness_switched_on_again_during_the_hosts_back_off_is_supervised_at_once() {
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.always.store(true, Ordering::SeqCst);
    let downs = p.downs.load(Ordering::SeqCst);
    p.host.set_config(off());
    until(
        "its run panicked again with nothing to carry, backing off",
        || p.downs.load(Ordering::SeqCst) > downs && all_stopped(&p.host),
    );
    p.always.store(false, Ordering::SeqCst);
    let runs = p.a.runs.load(Ordering::SeqCst);
    p.host.set_config(on());
    until("the tab supervised again: its loop runs", || {
        p.a.runs.load(Ordering::SeqCst) > runs
    });
    p.host.shutdown_and_join();
}

/// A COMMIT THAT FAILS DURING THE HOST'S BACK-OFF IS SUPERVISED AGAIN AT
/// ONCE (round seventeen, the review of its first cut): the host thread
/// panics and waits out a long back-off (an hour, as after a few panics), a
/// park's Commit comes — its stop ends the worker the restart carried,
/// leaving its agent's exit to the host — and fails. The failed Commit's
/// resume is news the restart acts on: its run starts again at once, and its
/// first pass starts the tab's worker again.
///
/// FAILS ON 70537fad5: the back-off was cut by a shutdown or a switch-off
/// alone, so the restart waited out its hour with no worker on the tab —
/// nobody answered its boxes or ended its turns (the wait for its loop to
/// run again runs out). Before that cut (1afcdade3) the suspend ended the
/// host thread, and the resume started a fresh one at once.
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

/// A POLICY NARROWED WHILE THE HOST IS DOWN REACHES THE WORKERS IT CARRIES
/// (round seventeen, the review of its first cut): the host thread panics
/// and waits out a long back-off, and the owner narrows `[harness]`
/// (`approve = "none"`) meanwhile. The worker the restart carried, started
/// under the policy before, is handed over at once — no press, answer or
/// continuation of its loop's under a policy the owner has since narrowed —
/// and its successor's loop runs under the new one, the narrowing's news
/// cutting the back-off short. And when every pass of the restarted host
/// panics again, the restart hands it over itself: no pass need get so far.
///
/// FAILS ON 70537fad5: only a pass hands a worker over on a policy change
/// ([`visit_workers`]), and the back-off was cut by a shutdown or a
/// switch-off alone: the carried worker ran on under the old policy until
/// the host was back — its whole back-off (the wait for its successor's loop
/// runs out). Before that cut (1afcdade3) the panic stopped every worker.
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

    // Every pass of the restarted host panics: the restart hands it over.
    let p = panicking_host_with(Some(Duration::from_secs(3600)));
    p.panic_and_back_off();
    p.always.store(true, Ordering::SeqCst);
    p.host.set_config(narrowed());
    until(
        "handed over by the restart itself, its exit left to the host",
        || marked(&p.host) && all_stopped(&p.host),
    );
    p.host.shutdown_and_join();
}

/// AN AGENT A RELAUNCH LANDS WHILE THE HOST IS DOWN IS WATCHED UNTIL A PASS
/// NAMES IT (round seventeen, the review of its first cut): the host
/// thread panics while a worker tries its crashed agent's relaunch on its
/// back-off; the worker runs on through the host's back-off and its next try
/// lands — and the agent that landed crashes too, before the host is back.
/// No pass read it back, so no worker was handed its exit and no mark left
/// it to the host, but it was watched: the restarted host's first pass hands
/// that exit, read as it was seen, and it is relaunched.
///
/// FAILS ON 70537fad5: the landing left the agent to the roster
/// ([`runs_again`]), whose naming starts its worker, and the host's restart
/// watched only the agents of the workers it carried: the landed agent's
/// crash was read by nobody, never relaunched and never said (the wait for
/// its relaunch runs out). Not a regression — before that cut the panic lost
/// even the first crash — and with the host up the window is the moment
/// before the next pass names the agent (the residual [`runs_again`] stated).
#[test]
fn an_agent_landed_while_the_host_is_down_that_crashes_is_relaunched() {
    let p = panicking_host();
    *p.a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    p.a.set(&[]);
    until("its relaunch tried on its back-off", || {
        p.a.relaunched.lock().unwrap().len() >= 2
    });
    p.panic_and_hold();
    // The shell takes the line now: the next try lands, and its agent is
    // read.
    p.a.relaunches.lock().unwrap().clear();
    until("a try after the panic landed, its agent read", || {
        p.a.owed.load(Ordering::SeqCst) && p.a.read_in("s-a")
    });
    let landed = p.a.relaunched.lock().unwrap().len();
    // The roster names it — nobody reads it, the host is down — and it
    // crashes, still before the host is back.
    p.a.set(&[("s-a", Program::Claude)]);
    p.a.set(&[]);
    p.down.open();
    until(
        "the landed agent's crash relaunched once the host is back",
        || p.a.relaunched.lock().unwrap().len() > landed,
    );
    assert_eq!(
        p.a.exit_records
            .lock()
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        [&"survived"].into_iter().collect(),
        "every try on a crash read at its exit"
    );
    p.host.shutdown_and_join();
}

/// A PASS IN FLIGHT WHEN A COMMIT'S STOP COMES ENDS NO WATCH THAT STOP ARMED
/// (round seventeen, the review of its first cut): a pass reads the roster
/// — naming the agent in its tab — and before it ends the watches of the
/// agents it names, a park's Commit stops the session's worker, which ends
/// leaving its agent's exit to the host and arms the exit watch. The pass
/// read nothing since: that watch is not its to end. The agent crashes
/// during the hold, a Claude Code started then sweeps its record, and the
/// Commit fails: the crash, read as it was seen, is relaunched.
///
/// FAILS ON 70537fad5: the pass ended every watch of a session its roster
/// named, the one armed after its read too; no pass of a suspended host ends
/// or arms another, so the crash was read only once handed after the failed
/// Commit, after the sweep — `removed`, never relaunched (the watch is
/// asserted still armed right after the pass).
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
    // Counts the host's passes past their look at the watches: its start
    // of workers asks the claim epoch first.
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
        !a.exit_records.lock().unwrap().is_empty()
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

/// The real host's session `s-a` projected onto `HarnessCommitOwed`'s
/// state as [`project_commit_owed`] projects it, and — round seventeen —
/// `down` (the host thread held in its restart's back-off), `swept` (a
/// Claude Code started since removed the crashed agent's record),
/// `landed` (an exit watch that hands its exit itself: a landing's,
/// [`ExitWatched::hands`]) and, while the agent is gone, `seen` (what its
/// exit left read as it was seen: the exit watch keeps a crash's read, or
/// a relaunch was handed one).
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

/// TIER-1 for `HarnessCommitOwed`'s round seventeen (aterm-spec): an exit
/// nobody is handed as it comes, the real host driven and each observed
/// state projected onto the model's ([`project_unhanded`]) —
///
/// 1. THE HOST DOWN AFTER A PANIC (`Panic`): its worker lives on; the agent
///    crashes and what it left is read as the exit is seen (`Crash`), a
///    Claude Code started meanwhile sweeps the record (`Sweep`), and no
///    exit is handed until the host's first pass back (`Up`) hands it to
///    the worker (`Hand`), which reads the crash (`Read`) and relaunches it
///    (`Relaunch`);
/// 2. A COMMIT'S HOLD: the worker the Commit stopped ends leaving the exit
///    to the host (`Suspend`, `EndUnhanded`), the agent crashes during the
///    hold and its record is read then (`Crash`) and swept (`Sweep`); the
///    Commit fails (`Resume`, `Rollback`) and the host hands the exit to a
///    worker that reads it alone (`HandUnwatched`, `Read`), which
///    relaunches it (`Relaunch`);
/// 3. A LANDING WHILE THE HOST IS DOWN (the review of round seventeen's
///    first cut): the worker reads its agent's crash and tries its relaunch
///    on its back-off (`Crash`, `Hand`, `Read`), the host thread panics
///    (`Panic`), the next try lands — the worker ends, the agent it started
///    watched (`Relaunch`) — and that agent crashes too (`Crash`) and its
///    record is swept (`Sweep`); the host's first pass back (`Up`) hands the
///    exit the watch read to a worker that reads it alone
///    (`HandUnwatched`, `Read`), which relaunches it (`Relaunch`).
///
/// FAILS WITHOUT THE FIX: in the first the panic stopped the worker, which
/// ended marking nothing — the host lists no worker while it is down, and
/// nothing reads the exit (the wait for its read runs out); in the second
/// nothing reads the exit during the hold, and the worker handed it after
/// reads the swept record as a graceful exit, relaunching nothing; in the
/// third nothing watched the landed agent, and its crash was read by nobody.
///
/// NEGATIVE CONTROL: those three reads are the dead mutants `PanicStops`,
/// `CrashUnseen` and `LandedLost`, each breaking `NoOwedRelaunchLost`.
#[test]
fn an_exit_nobody_is_handed_conforms_to_the_commit_owed_model() {
    let model = aterm_spec::derive::harness_commit_owed_model();
    let holds = |state: &aterm_spec::interp::State| {
        for invariant in ["NoOwedRelaunchLost", "NoQueuedRelaunchDropped"] {
            assert!(
                model.check_invariant(invariant, state),
                "{invariant}: {state:?}"
            );
        }
    };

    // ONE: the host down after a panic.
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

    // TWO: a Commit's hold.
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

    // THREE: a landing while the host is down.
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

    // NEGATIVE CONTROL: the panic that stopped the workers, and the exit
    // read only once handed after the hold.
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

// ---------------------------------------------------------------------------
// Round eighteen: the landings and the exits a cold restore's queue, a
// Commit or a host panic still left to nobody.
// ---------------------------------------------------------------------------

/// A RESTORED AGENT THAT CRASHES BEFORE A PASS NAMES IT IS RELAUNCHED
/// (round eighteen; the residual round seventeen stated: the cold
/// restore's queue lands agents on its own thread with no watch). The host
/// thread panics and is held in its back-off — no pass runs — while the cold
/// restore's queue, on a thread of its own, relaunches the agent of tab
/// `s-r`: the step lands (`adopted`), and the agent it started is named and
/// then crashes, still before the host is back. Watched from its landing,
/// its exit is read as it was seen and the host's first pass back hands it:
/// relaunched once, by the relaunch on exit, on what its crash left — the
/// queue's own step never tried again.
///
/// FAILS ON 745d63b36: the queue ended its attempt at the landing and
/// nothing watched the agent it started — no pass read it back, so no
/// worker was handed its exit, and nothing marked it the host's: its crash
/// was read by nobody and never relaunched (the wait for its relaunch runs
/// out).
#[test]
fn a_restored_agent_that_crashes_before_a_pass_names_it_is_relaunched() {
    let steps = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&steps);
    let p = panicking_host_hooked(None, move |a, h| {
        let a = Arc::clone(a);
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
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
    // The roster names it — nobody reads it, the host is down — and it
    // crashes, still before the host is back.
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
    // Once: the worker handed that exit ended, the agent its relaunch
    // landed watched, and two whole passes since with no worker for the tab
    // — no second worker, and no second relaunch, can follow.
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

/// What a seamless update's layout carries of tab `sid` from `parent` to
/// its successor's Commit, through the leaf's wire
/// ([`crate::restore::AgentRestore`], TOML) — the relaunch the parent owes
/// the tab, and the explicit restart's cause where its act ended the agent
/// — as the successor queues it on its restored queue
/// (`App::hand_restored_agents`), on the adopted shell.
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
                snap: agent.snapshot(sid.to_string(), shell),
                place: "in tab 1".to_string(),
                cold: true,
                asked,
            }
        })
        .into_iter()
        .collect()
}

/// AN ASKED EXIT CARRIED THROUGH A HAND ACROSS A COMMIT IS NEVER A COUNTED
/// RETRY (round eighteen; ruling 20's (iv), not fixed until now). A person's
/// own `restart` ends the agent of tab `s-a`, and its exit is read under a
/// fleet hold: carried through it (ruling 20) — owed, nothing attempted. A
/// seamless update parks, its layout carries the relaunch, and its Commit
/// goes ahead while the hold still stands; the successor queues it at its
/// Commit. Its queue's looks at the tab while the hold stands attempt
/// nothing and count nothing, as the live worker's do — however many: never
/// the row the counted budget ends in (`RESTORED_TRIES`, about 91 s) — and
/// once the hold lifts it is relaunched as the act's.
///
/// FAILS ON 745d63b36: the layout carried the snapshot alone, and the
/// successor's queue stepped it as aterm's own end, its relaunch line's look
/// answering `wait:held` — a COUNTED retry: the first look under the hold is
/// a try (the attempt and its count below), and past eight of them the tab
/// was said not to have come back, the agent the person asked to restart
/// left at its shell.
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
    // aterm's own end's step: its relaunch line looks at the tab as every
    // relaunch line does (`wait:held` under a hold).
    h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
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

/// A PANIC IN THE HOST'S RESTART LOSES NONE OF ITS WORKERS (round
/// eighteen; the residual round seventeen stated: a panic past the
/// restart's own `catch_unwind` still dropped the workers, `Workers`' drop
/// the backstop). The host thread panics in a pass, and its restart panics
/// too, before the host is back: in its back-off, the one hook the stretch
/// between a run's catch and the next run calls on the host thread
/// (`host_restarted` calls none a test can fail). The worker it carries
/// lives on, its tab still supervised: the agent's crash after it is
/// relaunched.
///
/// FAILS ON 745d63b36: the restart's panic unwound the host thread, and
/// the workers it held dropped with it — each stopped for good, handing
/// nothing — and nothing started the host again: the crash was read by
/// nobody (the wait for its relaunch runs out).
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

/// A PANIC INSIDE THE HOST'S RESTART LOSES NONE OF ITS WORKERS (the review
/// of round eighteen's first cut: the panic above comes from the back-off,
/// after `host_restarted` has returned, so it pins the catch around the
/// restart alone). Here the restart itself panics — after it has reset what
/// the restarted run starts afresh and held the workers to the policy
/// (`Shared::restart_panics`, the test's seam) — and is caught, made again a
/// pause later, and the host is back. The worker it carried lives on, the
/// only loop that ever ran for the tab: the agent's crash after it is read
/// by that worker and relaunched.
///
/// A RESTART THAT MOVED THE WORKERS into a value of its own (round
/// seventeen's `host_restarted`, which returned the restarted state) dropped
/// them with that value at such a panic — each stopped for good, handing
/// nothing — and the next pass started a fresh worker for the tab: a
/// second run of its loop.
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

/// A LANDED AGENT NO READ REACHES IS NOT LEFT TO NOBODY (round eighteen;
/// the residual round seventeen stated: a landing whose new agent could not
/// be read arms nothing). The worker tries its crashed agent's relaunch on
/// its back-off, and the host thread panics; the worker's next try lands
/// while the host is down — the agent it started holds its tab, but no read
/// of it succeeds (the control socket refusing it, say) — and that agent
/// crashes too, still before the host is back. Its exit is the relaunch's
/// still: tried again, on the crash the first exit left.
///
/// FAILS ON 745d63b36: the landing whose agent could not be read ended the
/// worker with nothing watching it — no process to watch, and no mark (the
/// agent alive in its tab): its crash was read by nobody, never relaunched
/// and never said (the wait for its relaunch runs out).
#[test]
fn a_landed_agent_no_read_reaches_that_crashes_is_relaunched() {
    let p = panicking_host();
    *p.a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    p.a.set(&[]);
    until("its relaunch tried on its back-off", || {
        p.a.relaunched.lock().unwrap().len() >= 2
    });
    p.panic_and_hold();
    // The next try lands an agent that holds its tab and that no read
    // reaches.
    p.a.unreadable.store(true, Ordering::SeqCst);
    *p.a.holds.lock().unwrap() = Some(true);
    let (tried, asked) = (
        p.a.relaunched.lock().unwrap().len(),
        p.a.still_asked.lock().unwrap().len(),
    );
    p.a.relaunches.lock().unwrap().clear();
    until("a try after the panic landed, and was looked at", || {
        p.a.owed.load(Ordering::SeqCst)
            && p.a.relaunched.lock().unwrap().len() > tried
            && p.a.still_asked.lock().unwrap().len() > asked
    });
    let landed = p.a.relaunched.lock().unwrap().len();
    // It crashes, still before the host is back; the next agent can be read.
    *p.a.holds.lock().unwrap() = None;
    p.a.set(&[]);
    p.a.unreadable.store(false, Ordering::SeqCst);
    until("the unread landing's crash relaunched", || {
        p.a.relaunched.lock().unwrap().len() > landed
    });
    assert_eq!(
        p.a.exit_records
            .lock()
            .unwrap()
            .iter()
            .collect::<BTreeSet<_>>(),
        [&"survived"].into_iter().collect(),
        "every try on the crash read at its exit"
    );
    p.down.open();
    p.host.shutdown_and_join();
}

/// A RESTORED LANDING NO READ REACHES IS NOT LEFT TO NOBODY (round
/// eighteen, the review of its first cut). The host thread panics and is
/// held in its back-off — no pass runs — while the cold restore's queue
/// relaunches the agent of tab `s-r`: its step lands an agent that holds
/// its tab but that no read reaches (the control socket refusing it, say),
/// and that agent crashes, still unread, before the host is back. Its
/// exit is its own relaunch's again, as a worker's landing's is: the
/// queue's attempt, which its landing did not end, steps it again — once —
/// and the agent that step lands, read, is watched.
///
/// FAILS ON 95d62f656: the landing ended its attempt, and the thread that
/// read it again gave up at the crash (the agent no longer held its tab)
/// with nothing holding the exit — no watch, no mark, no worker, no
/// attempt: the crash was relaunched by nobody and said to nobody (the
/// wait for the queue's second step runs out).
#[test]
fn a_restored_landing_no_read_reaches_that_crashes_is_relaunched_again() {
    let steps = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&steps);
    let p = panicking_host_hooked(None, move |a, h| {
        let a = Arc::clone(a);
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
            n.fetch_add(1, Ordering::SeqCst);
            a.adopted_in(sid, false);
            // The agent it landed holds its tab (the store names it so).
            *a.holds.lock().unwrap() = Some(true);
            "adopted".to_string()
        });
    });
    p.a.open.lock().unwrap().insert("s-r".to_string());
    p.panic_and_hold();
    p.a.unreadable.store(true, Ordering::SeqCst);
    let asked = p.a.still_asked.lock().unwrap().len();
    p.host.relaunch_restored(vec![restored("s-r", "in tab 2")]);
    until(
        "the step landed an agent no read reaches, looked at",
        || steps.load(Ordering::SeqCst) == 1 && p.a.still_asked.lock().unwrap().len() > asked,
    );
    // It crashes, unread, still before the host is back; the agent the next
    // step lands can be read.
    *p.a.holds.lock().unwrap() = None;
    p.a.set(&[("s-a", Program::Claude)]);
    p.a.unreadable.store(false, Ordering::SeqCst);
    until("the restored landing's crash stepped again", || {
        steps.load(Ordering::SeqCst) >= 2
    });
    let watched = || {
        let kept = p.host.shared.kept.lock().unwrap().get("s-r").cloned();
        kept.is_some_and(|k| {
            k.lock()
                .unwrap()
                .exit_watch
                .as_ref()
                .is_some_and(|w| w.hands.is_some())
        })
    };
    until("its agent read, watched, the attempt over", || {
        p.host.pending_restored().is_empty() && watched()
    });
    // The attempt is off the queue — the one stepper — and the agent it
    // landed runs: no step and no relaunch on exit can follow.
    assert_eq!(
        (
            steps.load(Ordering::SeqCst),
            p.a.relaunched.lock().unwrap().clone()
        ),
        (2, Vec::<String>::new()),
        "(the queue's steps, relaunched on exit): stepped again once"
    );
    p.down.open();
    p.host.shutdown_and_join();
}

/// AN ASKED EXIT CARRIED ACROSS A COMMIT IS NOT REFUSED BY `relaunch =
/// false` (round eighteen, the review of its first cut): the live worker
/// relaunches the explicit restart's end whatever `[harness] relaunch`
/// says (`on_agent_left`'s `allowed`: a person's restart is not refused by
/// it), and so does the successor's restored queue that carries it: the
/// tab's agent relaunched as the act's. NEGATIVE CONTROL: a crash's
/// relaunch carried there is the switch's, and dropped, as always.
///
/// FAILS ON 95d62f656: the queue refused every agent under `relaunch =
/// false` — the asked one dropped with no row, never relaunched (the wait
/// for its relaunch runs out).
#[test]
fn an_asked_exit_carried_across_a_commit_is_relaunched_under_relaunch_false() {
    let mut cfg = on();
    cfg.set("relaunch", "false").unwrap();
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-r".to_string());
    let host = HostHandle::start(cfg, false, false, a.hooks());
    // NEGATIVE CONTROL: a crash's, dropped at once — nothing queued.
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

/// TIER-1 for `HarnessCommitOwed`'s round eighteen (aterm-spec): the
/// landings and the exits a cold restore's queue, a host panic's restart
/// or a read that fails left to nobody, the real host driven and each
/// observed state projected onto the model's ([`project_unhanded`]) —
///
/// 1. THE ROLLBACK QUEUE'S LANDING: the agent crashes and its worker owes
///    its relaunch on its back-off (`Crash`, `Hand`, `Read`); a park's
///    Commit stops the worker, which decides stranded (`Suspend`,
///    `Decide`); the Commit fails and the rollback queues the relaunch
///    (`Resume`, `Rollback`), which the queue's step lands — its agent
///    watched (`QueueSteps`) — and that agent crashes before a pass names
///    it (`Crash`): the host hands the exit the watch read to a worker that
///    reads it alone, and it is relaunched (`HandUnwatched`, `Read`,
///    `Relaunch`);
/// 2. A PANIC IN THE HOST'S RESTART: the host thread panics (`Panic`), the
///    agent crashes while it is down (`Crash`), and its restart panics too
///    as it ends its back-off — nothing changes in the model, its worker
///    lives on — then the host is back (`Up`) and hands the exit to that
///    worker, which relaunches it (`Hand`, `Read`, `Relaunch`);
/// 3. A LANDING NO READ REACHES while the host is down: the worker's try
///    lands an agent it cannot read (`RelaunchUnread`), which crashes
///    (`Crash`) — its own relaunch's again — and the worker's next try
///    lands an agent it reads, watched (`Relaunch`);
/// 4. THE ROLLBACK QUEUE'S LANDING NO READ REACHES (the review of round
///    eighteen's first cut): the queue's step lands an agent that holds its
///    tab unread, its attempt kept (`QueueStepsUnread`); it crashes unread
///    — the attempt owed again (`Crash`) — and the next step lands another
///    no read reaches yet (`QueueStepsUnread`), which a later look reads:
///    the attempt over, the agent watched (`QueueReadsLanded`).
///
/// FAILS WITHOUT THE FIX: in the first nothing watched the queue's landing
/// and its crash was read by nobody; in the second the restart's panic
/// dropped the worker, and the crash was read by nobody; in the third the
/// unread landing ended the worker, and its crash was read by nobody; in
/// the fourth the queue's unread landing ended its attempt, and its crash
/// was held by nobody (each a wait that runs out).
///
/// NEGATIVE CONTROL: those four reads are the dead mutants
/// `QueueLandedLost`, `RestartPanicStops`, `UnreadLandedLost` and
/// `QueueUnreadLost`, each breaking `NoOwedRelaunchLost`.
#[test]
fn the_landings_of_round_eighteen_conform_to_the_commit_owed_model() {
    let model = aterm_spec::derive::harness_commit_owed_model();
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

    // ONE: the rollback queue's landing.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    *a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    let mut h = a.hooks();
    let steps = Arc::new(AtomicUsize::new(0));
    let (world, n) = (Arc::clone(&a), Arc::clone(&steps));
    h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
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

    // TWO: a panic in the host's restart.
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

    // THREE: a landing no read reaches, the host down.
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
    *p.a.holds.lock().unwrap() = None;
    p.a.set(&[]);
    fire(&mut state, &["Crash"]);
    p.a.unreadable.store(false, Ordering::SeqCst);
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

    // FOUR: the rollback queue's landing no read reaches.
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    *a.relaunches.lock().unwrap() = vec!["wait:shell-prompt"; 100_000].into();
    let mut h = a.hooks();
    let steps = Arc::new(AtomicUsize::new(0));
    let (world, n) = (Arc::clone(&a), Arc::clone(&steps));
    h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
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
    let landed_unread = |host: &HostHandle| {
        host.shared
            .restored
            .lock()
            .unwrap()
            .attempts
            .iter()
            .any(|attempt| attempt.agent.sid == "s-a" && attempt.landed)
    };
    until("the queue's step landed, unread, its attempt kept", || {
        steps.load(Ordering::SeqCst) == 1 && landed_unread(&host)
    });
    fire(&mut state, &["QueueStepsUnread"]);
    assert_eq!(
        project_unhanded(&state, &host, &a, false),
        state,
        "landed unread, its attempt the exit's holder"
    );
    // It crashes unread: owed again, stepped again — and the agent that
    // step lands holds its tab unread too.
    *a.holds.lock().unwrap() = None;
    a.set(&[]);
    until("stepped again, landed unread again", || {
        steps.load(Ordering::SeqCst) == 2 && landed_unread(&host)
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

    // NEGATIVE CONTROL: the reads before round eighteen.
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
