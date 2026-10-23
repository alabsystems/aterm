// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that agent exits are recorded while the host thread is down or held by Commit.

use super::*;

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
    fn panic_and_hold(&self) {
        self.down.shut();
        self.boom.store(true, Ordering::SeqCst);
        ring();
        until("the host down after its panic", || {
            self.down.held() && !self.boom.load(Ordering::SeqCst)
        });
    }

    fn panic_and_back_off(&self) {
        let downs = self.downs.load(Ordering::SeqCst);
        self.boom.store(true, Ordering::SeqCst);
        ring();
        until("the host in its back-off after its panic", || {
            self.downs.load(Ordering::SeqCst) > downs && !self.boom.load(Ordering::SeqCst)
        });
    }
}

fn kept_of<R>(host: &HostHandle, f: impl FnOnce(&Kept) -> R) -> Option<R> {
    let kept = host.shared.kept.lock().unwrap().get("s-a").cloned()?;
    let kept = kept.lock().unwrap();
    Some(f(&kept))
}

fn marked(host: &HostHandle) -> bool {
    kept_of(host, |k| k.unwatched.is_some()).unwrap_or(false)
}

fn watch_armed(host: &HostHandle) -> bool {
    kept_of(host, |k| k.exit_watch.is_some()).unwrap_or(false)
}

fn landed_unread(host: &HostHandle, sid: &str) -> bool {
    host.shared
        .restored
        .lock()
        .unwrap()
        .attempts
        .iter()
        .any(|attempt| attempt.agent.sid == sid && attempt.landed)
}

fn all_stopped(host: &HostHandle) -> bool {
    host.shared
        .lock()
        .stops
        .iter()
        .all(|s| s.stop.load(Ordering::SeqCst))
}

fn narrowed() -> SupervisorConfig {
    let mut cfg = on();
    cfg.set("approve", "none").unwrap();
    cfg
}

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

#[test]
fn a_pass_in_flight_at_a_commits_stop_ends_no_watch_that_stop_armed() {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-a".to_string());
    a.set(&[("s-a", Program::Claude)]);
    let mut h = a.hooks();
    let host_thread = format!("{THREAD_PREFIX}host");
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
    a.set(&[]);
    until("the crash read as it was seen, the Commit holding", || {
        watched_read(&host).is_some()
    });
    a.record_gone.store(true, Ordering::SeqCst);
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

/// What the exit watch keeps of session `s-a`'s agent's exit ([`Kept::exit_watch`]): its read, once taken.
pub(super) fn watched_read(host: &HostHandle) -> Option<ExitRecord> {
    let kept = host.shared.kept.lock().unwrap().get("s-a").cloned()?;
    let kept = kept.lock().unwrap();
    kept.exit_watch.as_ref().and_then(|w| w.read.clone())
}

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
/// Commit, is relaunched as the act's once the hold lifts.
#[cfg(target_os = "macos")]
#[test]
fn an_asked_exit_carried_through_a_hold_across_a_commit_is_relaunched_once_it_lifts() {
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
    parent.pause_restored();
    let carried = carried_across(&parent, "s-a");
    parent.suspend();
    parent.shutdown_and_join();
    assert!(
        a.relaunched.lock().unwrap().is_empty(),
        "the parent relaunched nothing"
    );
    let b = Arc::new(Acting::default());
    b.open.lock().unwrap().insert("s-a".to_string());
    b.held.store(true, Ordering::SeqCst);
    let (steps, looks) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut h = b.hooks();
    let (world, n, l) = (Arc::clone(&b), Arc::clone(&steps), Arc::clone(&looks));
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

#[test]
fn a_panic_inside_the_hosts_restart_loses_none_of_its_workers() {
    let p = panicking_host();
    p.host.shared.restart_panics.store(true, Ordering::SeqCst);
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

#[test]
fn an_asked_exit_carried_across_a_commit_is_relaunched_under_relaunch_false() {
    let mut cfg = on();
    cfg.set("relaunch", "false").unwrap();
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert("s-r".to_string());
    let host = HostHandle::start(cfg, false, false, a.hooks());
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
