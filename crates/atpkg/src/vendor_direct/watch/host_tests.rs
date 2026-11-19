// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// Each point a read reached, by file name.
type Seen = Vec<(&'static str, Step)>;

/// `read()` with `act` run at each point of its rendezvous reads; what it returned and the
/// points, in order.
fn probed<T>(
    mut act: impl FnMut(&'static str, Step) + 'static,
    read: impl FnOnce() -> T,
) -> (T, Seen) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    PROBING.with_borrow_mut(|slot| {
        *slot = Some(Box::new(move |path: &Path, step| {
            let file = [HOSTS_FILE, SEAT_FILE]
                .into_iter()
                .find(|name| path.ends_with(name))
                .unwrap_or_else(|| panic!("a read of {}", path.display()));
            log.borrow_mut().push((file, step));
            act(file, step);
        }));
    });
    let out = read();
    PROBING.with_borrow_mut(|slot| *slot = None);
    let seen = seen.borrow().clone();
    (out, seen)
}

/// A probing hook that runs `act` before the `n`th read.
fn before_read(
    n: usize,
    mut act: impl FnMut() + 'static,
) -> impl FnMut(&'static str, Step) + 'static {
    let mut reads = 0;
    move |_, step| {
        if step == Step::Reads {
            reads += 1;
            if reads == n {
                act();
            }
        }
    }
}

/// A session that holds the seat between rounds.
fn sitting(l: &Layout) -> HostClaim {
    let mut session = HostClaim::new(Host::Session, l, None);
    assert_eq!(session.admit().0, Admit::Yes);
    session.end_round();
    session
}

/// A seated session lets its round go or takes the next before each of the report's reads, as
/// a runner step's back-to-back looks can: the report names the session, in either phase.
#[cfg(target_os = "macos")]
#[test]
fn a_round_that_comes_and_goes_mid_report_reads_as_the_session() {
    let l = layout("round-churn");
    let session = Rc::new(RefCell::new(sitting(&l)));
    for in_round in [false, true] {
        if in_round {
            assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
        }
        let churn = Rc::clone(&session);
        let (verdict, seen) = probed(
            move |_, step| {
                if step == Step::Reads {
                    let mut claim = churn.borrow_mut();
                    if claim.seated_session() {
                        claim.end_round();
                    } else {
                        assert_eq!(claim.admit().0, Admit::Yes, "nothing else holds it");
                    }
                }
            },
            || watcher(&l),
        );
        assert_eq!(
            verdict,
            Watcher::Session(Some(std::process::id())),
            "starting in a round: {in_round}"
        );
        assert_eq!(
            seen,
            [(HOSTS_FILE, Step::Reads), (SEAT_FILE, Step::Reads)],
            "one read of each file, holding nothing"
        );
    }
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The seat changes hands before each of the report's reads (its sitter exits, or a new one
/// sits): the report names a seated session iff one sat when the seat was read.
#[test]
fn a_seat_that_changes_hands_mid_report_reads_as_it_was_read() {
    let l = layout("seat-churn");
    let sitter = Rc::new(RefCell::new(Some(sitting(&l))));
    for (seated, expect) in [
        (true, Watcher::Session(Some(std::process::id()))),
        (false, Watcher::Nobody),
    ] {
        if !seated {
            drop(sitter.borrow_mut().take());
        }
        let (churn, at) = (Rc::clone(&sitter), (*l).clone());
        let (verdict, _) = probed(
            move |_, step| {
                if step == Step::Reads {
                    let mut seat = churn.borrow_mut();
                    let next = if seat.is_some() {
                        None
                    } else {
                        Some(sitting(&at))
                    };
                    drop(std::mem::replace(&mut *seat, next));
                }
            },
            || watcher(&l),
        );
        assert_eq!(verdict, expect, "seated at the start: {seated}");
    }
    drop(sitter);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A session and a window that admit at every point of a report are not turned away by it.
#[cfg(target_os = "macos")]
#[test]
fn hosts_that_admit_mid_report_are_not_turned_away() {
    let l = layout("admit-mid-report");
    let session = Rc::new(RefCell::new(sitting(&l)));
    let refused = Rc::new(RefCell::new(Vec::new()));
    let (admits, log, at) = (Rc::clone(&session), Rc::clone(&refused), (*l).clone());
    let (verdict, _) = probed(
        move |file, step| {
            let mut claim = admits.borrow_mut();
            let session = claim.admit().0;
            claim.end_round();
            let window = HostClaim::new(Host::Window, &at, None).admit().0;
            if session != Admit::Yes || window != Admit::Yes {
                log.borrow_mut().push((file, step, session, window));
            }
        },
        || watcher(&l),
    );
    assert!(
        refused.borrow().is_empty(),
        "(file, step, session, window) where the report turned a host away: {:?}",
        refused.borrow()
    );
    assert_eq!(verdict, Watcher::Session(Some(std::process::id())));
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A window opens before the hosts read or between it and the seat's (in a session's round,
/// which ends to let it in, or between rounds), or one closes at either point: the report
/// names whoever held the hosts file when it was read.
#[cfg(target_os = "macos")]
#[test]
fn a_window_that_comes_or_goes_mid_report_reads_as_the_hosts_file_was_read() {
    let l = layout("window-churn");
    let session = Rc::new(RefCell::new(sitting(&l)));
    let seated = Watcher::Session(Some(std::process::id()));
    let window = Rc::new(RefCell::new(None::<HostClaim>));
    for (in_round, n, expect) in [
        (false, 1, Watcher::Window),
        (false, 2, seated.clone()),
        (true, 1, Watcher::Window),
        (true, 2, seated.clone()),
    ] {
        if in_round {
            assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
        }
        let (claim, opens, at) = (Rc::clone(&session), Rc::clone(&window), (*l).clone());
        let opening = before_read(n, move || {
            claim.borrow_mut().end_round();
            let mut opened = HostClaim::new(Host::Window, &at, None);
            assert_eq!(opened.admit(), (Admit::Yes, None));
            *opens.borrow_mut() = Some(opened);
        });
        assert_eq!(
            probed(opening, || watcher(&l)).0,
            expect,
            "in a round: {in_round}, opened before read {n}"
        );
        drop(window.borrow_mut().take());
    }
    for (n, expect) in [(1, seated.clone()), (2, Watcher::Window)] {
        let mut open = HostClaim::new(Host::Window, &l, None);
        assert_eq!(open.admit(), (Admit::Yes, None));
        *window.borrow_mut() = Some(open);
        let closes = Rc::clone(&window);
        let closing = before_read(n, move || drop(closes.borrow_mut().take()));
        assert_eq!(
            probed(closing, || watcher(&l)).0,
            expect,
            "closed before read {n}"
        );
        drop(window.borrow_mut().take());
    }
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The sample other systems read by: a hold that stays reads as its kind, and one let go
/// before the last take reads Free.
#[test]
fn a_sampled_hold_reads_as_its_kind_or_free_once_let_go() {
    let l = layout("sample");
    let hosts = l.prefix.join(HOSTS_FILE);
    let session = Rc::new(RefCell::new(HostClaim::new(Host::Session, &l, None)));
    assert_eq!(session.borrow_mut().admit().0, Admit::Yes);
    assert_eq!(sample(&hosts), Ok(Probe::Exclusive), "a round held");
    let claim = Rc::clone(&session);
    let (read, seen) = probed(
        before_read(2, move || claim.borrow_mut().end_round()),
        || sample(&hosts),
    );
    assert_eq!(
        read,
        Ok(Probe::Free),
        "a round let go after the refused exclusive take"
    );
    let (reads, holds) = ((HOSTS_FILE, Step::Reads), (HOSTS_FILE, Step::Holds));
    assert_eq!(seen, [reads, reads, holds, reads, holds]);

    let mut open = HostClaim::new(Host::Window, &l, None);
    assert_eq!(open.admit(), (Admit::Yes, None));
    assert_eq!(sample(&hosts), Ok(Probe::Shared), "a window's hold");
    let window = Rc::new(RefCell::new(Some(open)));
    let closes = Rc::clone(&window);
    let (read, _) = probed(
        before_read(3, move || drop(closes.borrow_mut().take())),
        || sample(&hosts),
    );
    assert_eq!(
        read,
        Ok(Probe::Free),
        "a window closed after the shared take"
    );
    drop(window);
    drop(session);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-hosts-{label}-"))
}

#[test]
fn a_window_and_a_session_never_both_hold_the_watch() {
    let l = layout("claims");
    assert_eq!(watcher(&l), Watcher::Nobody, "nothing has ever watched");

    let mut a = HostClaim::new(Host::Session, &l, None);
    let (admit, note) = a.admit();
    assert_eq!(admit, Admit::Yes);
    assert!(note.unwrap().contains("this terminal session watches"));
    assert!(a.seated_session(), "the session holds the exclusive round");
    assert_eq!(
        watcher(&l),
        Watcher::Session(Some(std::process::id())),
        "mid-round, the report names the seat's pid"
    );
    let mut b = HostClaim::new(Host::Session, &l, None);
    let (admit, note) = b.admit();
    assert_eq!(admit, Admit::Later(CADENCE));
    assert!(note.unwrap().contains("another terminal session watches"));
    assert_eq!(b.admit(), (Admit::Later(CADENCE), None), "said once");

    let mut window = HostClaim::new(Host::Window, &l, None);
    assert_eq!(window.admit(), (Admit::Later(BUSY_RETRY), None));
    a.end_round();
    assert!(
        !a.seated_session(),
        "the seat alone does not reserve the space a window may take"
    );
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert_eq!(window.admit(), (Admit::Yes, None));
    assert!(!a.seated_session(), "the window now holds that space");
    assert_eq!(watcher(&l), Watcher::Window);

    let (admit, note) = a.admit();
    assert_eq!(admit, Admit::Later(CADENCE));
    assert!(note.unwrap().contains("an aterm window watches"));
    assert_eq!(a.admit(), (Admit::Later(CADENCE), None));
    let mut second = HostClaim::new(Host::Window, &l, None);
    assert_eq!(second.admit(), (Admit::Yes, None));
    drop(window);
    assert_eq!(
        a.admit().0,
        Admit::Later(CADENCE),
        "one window still watches"
    );
    drop(second);

    let (admit, note) = a.admit();
    assert_eq!(admit, Admit::Yes);
    assert!(note.unwrap().contains("this terminal session watches"));
    a.end_round();
    drop(a);
    assert_eq!(watcher(&l), Watcher::Nobody);
    let (admit, note) = b.admit();
    assert_eq!(admit, Admit::Yes);
    assert!(note.unwrap().contains("this terminal session watches"));
    b.end_round();
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The real claims the model's steps drive: one window and two sessions over one store.
struct Hosts {
    layout: Layout,
    /// What each session claim's waiter rings, as a runner's does: `None` drives claims with no
    /// ring (the model's `Ringed = 0` scenario).
    rest: Option<super::super::Rest>,
    window: Option<HostClaim>,
    sessions: [Option<HostClaim>; 2],
    /// Each session's waiter threads, kept across its claims as its head watch keeps them: the
    /// model's `Exits` lets the session's claim go (`release_host`, Automatic updates turned off),
    /// which cannot interrupt a waiter's blocking take, and its next `Starts` makes a claim over
    /// the same waiters ([`HostClaim::with_waiters`]), which takes on a waiter still blocked on
    /// the lock it is refused. A waiter no claim took on lets go at once of what it takes.
    waiters: [Waiters; 2],
}

/// The model's names for each session's waiter (`q1`/`q2`: 1 blocks on the seat, 2 on the
/// rendezvous), and the actions that let its claim go and admit it.
const Q: [&str; 2] = ["q1", "q2"];
const EXITS: [&str; 2] = ["S1Exits", "S2Exits"];
const ADMITS: [&str; 2] = ["S1Admits", "S2Admits"];

/// The lock a waiter blocks on, as the model's `q1`/`q2` names it.
fn lock_of(standing: Standing) -> i64 {
    if standing == Standing::SeatTaken {
        1
    } else {
        2
    }
}

/// What a path did that the model's state leaves out, so the explorer tells such paths apart: a
/// session's waiters outlive its claims, and a waiter a let-go claim armed either lets its lock
/// go at once when it takes it or is taken on by the session's next claim refused that lock,
/// which the model sees as a waiter armed. Telling them apart drives every reachable step also
/// with a claim whose waiter it took on, and with one refused a lock while its session keeps a
/// waiter blocked on the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Past {
    /// Per session, a bit per lock (1 the seat, 2 the rendezvous) a waiter one of its let-go
    /// claims armed may still block on: somebody else has held it since.
    kept: [i64; 2],
    /// Per session, its live claim waits on a waiter it took on.
    taken: [bool; 2],
}

impl Past {
    /// The past after the model's `action` took `state` to `next`.
    fn after(
        mut self,
        state: &std::collections::BTreeMap<&'static str, i64>,
        action: &str,
        next: &std::collections::BTreeMap<&'static str, i64>,
    ) -> Self {
        let held = |lock: i64| {
            if lock == 1 {
                next["s1"] >= 2 || next["s2"] >= 2 || next["orphan"] == 1
            } else {
                next["w"] == 2 || next["s1"] == 3 || next["s2"] == 3
            }
        };
        for i in 0..2 {
            let (q, q_next) = (state[Q[i]], next[Q[i]]);
            if action == EXITS[i] && matches!(q, 1 | 2) {
                self.kept[i] |= q;
            }
            if action == ADMITS[i] && matches!(q_next, 1 | 2) {
                self.taken[i] = self.kept[i] & q_next != 0;
                self.kept[i] &= !q_next;
            } else if !matches!(q_next, 1 | 2) {
                self.taken[i] = false;
            }
            for lock in [1, 2] {
                if !held(lock) {
                    self.kept[i] &= !lock;
                }
            }
        }
        self
    }
}

/// What a session's claim holds, the lock its waiter handed over counted as its own, and what
/// that waiter does: the model's `q1`/`q2` (0 none, 1 blocks on the seat, 2 on the rendezvous,
/// 3 handed its lock over).
#[derive(Clone, Copy)]
struct Held {
    seat: bool,
    hosts: bool,
    waiter: i64,
}

/// What `claim`'s waiter blocks for while it has handed nothing over.
fn waits_on(claim: &HostClaim) -> Option<Standing> {
    let waiter = claim.waiter.as_ref()?;
    matches!(*waiter.hand.handed.lock().unwrap(), Handed::Waiting).then_some(waiter.standing)
}

fn held(claim: &HostClaim) -> Held {
    let mut held = Held {
        seat: claim.seat.is_some(),
        hosts: claim.hosts.is_some(),
        waiter: 0,
    };
    if let Some(waiter) = &claim.waiter {
        match &*waiter.hand.handed.lock().unwrap() {
            Handed::Waiting if waiter.standing == Standing::SeatTaken => held.waiter = 1,
            Handed::Waiting => held.waiter = 2,
            Handed::Seat(_) => (held.seat, held.waiter) = (true, 3),
            Handed::Hosts(_) => (held.hosts, held.waiter) = (true, 3),
            Handed::Failed => {}
        }
    }
    held
}

impl Hosts {
    fn new(layout: &Layout, ringed: bool) -> Self {
        Self {
            layout: layout.clone(),
            rest: ringed.then(super::super::Rest::inert),
            window: None,
            sessions: [None, None],
            waiters: [Waiters::default(), Waiters::default()],
        }
    }

    /// Take `action`'s real step, then wait as the model's step does for each waiter whose
    /// lock its holder let go to take it.
    fn act(&mut self, action: &str) {
        let session = |name: &str| usize::from(name.starts_with("S2"));
        match action {
            "WindowOpens" => self.window = Some(HostClaim::new(Host::Window, &self.layout, None)),
            "WindowAdmits" => {
                let _ = self.window.as_mut().expect("a window").admit();
            }
            "WindowCloses" => self.window = None,
            "S1Starts" | "S2Starts" => {
                let i = session(action);
                let ring = self.rest.as_ref().map(super::super::Rest::ringer);
                let waiters = self.waiters[i].clone();
                self.sessions[i] = Some(HostClaim::with_waiters(
                    Host::Session,
                    &self.layout,
                    ring,
                    waiters,
                ));
            }
            "S1Admits" | "S2Admits" => {
                let _ = self.sessions[session(action)]
                    .as_mut()
                    .expect("a session")
                    .admit();
            }
            "S1EndsRound" | "S2EndsRound" => self.sessions[session(action)]
                .as_mut()
                .expect("a session")
                .end_round(),
            "S1Exits" | "S2Exits" => {
                drop(self.sessions[session(action)].take().expect("a session"));
            }
            other => panic!("no real step for {other}"),
        }
        aterm_spec::wait::until_within(
            Duration::from_secs(60),
            "each waiter whose lock was let go took it",
            || self.settled().then_some(()),
        );
        for i in 0..2 {
            let running = self.running(i);
            for lock in [1, 2] {
                assert!(
                    running.iter().filter(|&&l| l == lock).count() <= 1,
                    "session {} runs one waiter per lock after {action}: {running:?}",
                    i + 1
                );
            }
            if let Some(standing) = self.sessions[i].as_ref().and_then(waits_on) {
                assert!(
                    running.contains(&lock_of(standing)),
                    "session {}'s claim waits on a waiter that blocks on the lock it was refused \
                     ({standing:?}) after {action}: {running:?}",
                    i + 1
                );
            }
        }
    }

    /// The lock each of session `i`'s waiter threads that has not ended blocks on.
    fn running(&self, i: usize) -> Vec<i64> {
        self.waiters[i]
            .threads
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, thread)| !thread.is_finished())
            .map(|(standing, _)| lock_of(*standing))
            .collect()
    }

    /// Per session, a bit per lock a waiter that is not its live claim's blocks on.
    fn kept(&self) -> [i64; 2] {
        std::array::from_fn(|i| {
            let live = self.sessions[i].as_ref().and_then(waits_on).map(lock_of);
            self.running(i)
                .into_iter()
                .filter(|&lock| Some(lock) != live)
                .fold(0, |bits, lock| bits | lock)
        })
    }

    /// Whether no waiter can still take a lock: a live session's waiter has handed its lock
    /// over unless another live claim holds what it waits on, and every other waiter thread of a
    /// session (one its let-go claims armed and no claim took on since, or one that handed over)
    /// has ended unless another session or the window holds what it waits on.
    fn settled(&self) -> bool {
        let held: Vec<Option<Held>> = self
            .sessions
            .iter()
            .map(|claim| claim.as_ref().map(held))
            .collect();
        let window = self.window.as_ref().is_some_and(|w| w.hosts.is_some());
        let holds = |seat: bool, but: Option<usize>| {
            (!seat && window)
                || held.iter().enumerate().any(|(i, h)| {
                    Some(i) != but && h.is_some_and(|h| if seat { h.seat } else { h.hosts })
                })
        };
        held.iter()
            .enumerate()
            .all(|(i, h)| match h.map(|h| h.waiter) {
                Some(1) => holds(true, Some(i)),
                Some(2) => holds(false, Some(i)),
                _ => true,
            })
            && (0..2).all(|i| {
                let kept = self.kept()[i];
                [1, 2]
                    .into_iter()
                    .all(|lock| kept & lock == 0 || holds(lock == 1, Some(i)))
            })
    }

    fn project(&self) -> std::collections::BTreeMap<&'static str, i64> {
        let window = self
            .window
            .as_ref()
            .map_or(0, |w| if w.hosts.is_some() { 2 } else { 1 });
        let session = |claim: &Option<HostClaim>| {
            claim.as_ref().map(held).map_or((0, 0), |h| {
                let s = if h.hosts {
                    3
                } else if h.seat {
                    2
                } else {
                    1
                };
                (s, h.waiter)
            })
        };
        let (s1, q1) = session(&self.sessions[0]);
        let (s2, q2) = session(&self.sessions[1]);
        [
            ("w", window),
            ("s1", s1),
            ("s2", s2),
            ("q1", q1),
            ("q2", q2),
            ("orphan", 0),
        ]
        .into_iter()
        .collect()
    }
}

/// THE REAL CLAIMS REFINE THE HOSTS MODEL ON EVERY REACHABLE STEP, IN BOTH CONFIGURATIONS: with
/// session claims that have a ring, as every runner's has (the committed `Ringed = 1`: a refused
/// session's waiter blocks on the lock and takes it in the step its holder lets go, and a window
/// is refused while it holds the rendezvous for its session), and with claims that have none (the
/// `Ringed = 0` scenario, which asks again on its own clock). A session's waiters outlive its
/// claims, as its head watch keeps them: `Exits` lets the claim go as `release_host` does, and
/// the next `Starts` makes a claim over the same waiters, so the explorer, telling apart the
/// paths that leave a waiter blocked ([`Past`]), takes every reachable step also with a claim
/// that took on a waiter its session's let-go claim armed, and with one refused the seat while
/// its session keeps a waiter blocked on the rendezvous; each step holds a session to one waiter
/// per lock and a waiting claim to a waiter that blocks on the lock it was refused. NEGATIVE
/// CONTROL: the model's stale-probe mutant breaks NeverBothKinds where the real session stands
/// by. MUTANTS: driving the committed model with claims that have no ring (as this test once
/// did) fails the first refused admission, which arms no waiter; a hosts waiter that does not
/// hand over the rendezvous it took fails the window's close behind a waiting session; arming
/// every refusal on the seat's line takes a kept rendezvous waiter on for the seat, which a claim
/// refused the seat fails; a rendezvous waiter taken on but still naming the claim that armed it
/// lets the rendezvous go when the window closes, and the hang detector fails that step.
#[test]
fn the_real_claims_refine_the_derived_model() {
    use aterm_models_atpkg::atpkg_head_watch_hosts_model;
    let model = atpkg_head_watch_hosts_model();
    let clocked = aterm_spec::interp::with_consts(&model, &[("Ringed", 0)]);
    for (ringed, config) in [(true, &model), (false, &clocked)] {
        refines(config, ringed);
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut probed = buggy.init_state();
    for action in [
        "S1Starts",
        "S1Admits",
        "S1EndsRound",
        "WindowOpens",
        "WindowAdmits",
        "S1RoundsOnStaleProbe",
    ] {
        assert!(buggy.fire(action, &mut probed), "{action}");
    }
    assert!(!buggy.check_invariant("NeverBothKinds", &probed));
    let l = layout("refines-probe");
    let mut hosts = Hosts::new(&l, true);
    for step in [
        "S1Starts",
        "S1Admits",
        "S1EndsRound",
        "WindowOpens",
        "WindowAdmits",
        "S1Admits",
    ] {
        hosts.act(step);
    }
    let real = hosts.project();
    assert_eq!(
        (real["w"], real["s1"], real["q1"]),
        (2, 2, 2),
        "the real session stood by, its waiter blocked on the rendezvous"
    );
    assert!(
        !hosts.sessions[0]
            .as_ref()
            .expect("the session still holds the seat")
            .seated_session(),
        "a held toolchain look cannot borrow the old round's admission"
    );
    assert_ne!(real, probed, "the real hosts never reach the probe's state");
    assert!(model.check_invariant("NeverBothKinds", &real));
    drop(hosts);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// Every reachable step of `model` from every past that reaches its state ([`Past`]), each taken
/// by the real claims (`ringed`: with a ring) along the path the explorer first reached that state
/// and past by, in a store of its own.
fn refines(model: &aterm_spec::derive::Model, ringed: bool) {
    type State = std::collections::BTreeMap<&'static str, i64>;
    let mut paths: std::collections::BTreeMap<(State, Past), Vec<&'static str>> =
        std::collections::BTreeMap::new();
    let mut frontier =
        std::collections::VecDeque::from([((model.init_state(), Past::default()), Vec::new())]);
    while let Some((reached, path)) = frontier.pop_front() {
        if paths.contains_key(&reached) {
            continue;
        }
        let (state, past) = &reached;
        for action in &model.actions {
            let mut next = state.clone();
            if model.fire(action.name, &mut next) {
                let after = past.after(state, action.name, &next);
                let next = (next, after);
                if !paths.contains_key(&next) {
                    let mut longer = path.clone();
                    longer.push(action.name);
                    frontier.push_back((next, longer));
                }
            }
        }
        paths.insert(reached, path);
    }
    assert!(
        paths.len() >= 30,
        "the explorer reached the space (ringed: {ringed}): {}",
        paths.len()
    );
    if ringed {
        let reaches = |what: &str, found: fn(&State, &Past, usize) -> bool| {
            assert!(
                paths
                    .keys()
                    .any(|(state, past)| (0..2).any(|i| found(state, past, i))),
                "a path reaches {what}"
            );
        };
        reaches("a claim that took on a seat waiter", |s, p, i| {
            p.taken[i] && s[Q[i]] == 1
        });
        reaches("a claim that took on a rendezvous waiter", |s, p, i| {
            p.taken[i] && s[Q[i]] == 2
        });
        reaches(
            "a claim refused the seat while its session keeps a rendezvous waiter",
            |s, p, i| p.kept[i] & 2 != 0 && s[Q[i]] == 1,
        );
    }
    let mut steps = Vec::new();
    for ((state, past), path) in &paths {
        for action in &model.actions {
            let mut next = state.clone();
            if model.fire(action.name, &mut next) {
                let after = past.after(state, action.name, &next);
                steps.push((state, past, path, action.name, next, after));
            }
        }
    }
    assert!(
        steps.len() >= 100,
        "every reachable step is taken (ringed: {ringed}): {}",
        steps.len()
    );
    crate::testdir::each_side_by_side(&steps, |i, (state, past, path, action, next, after)| {
        let l = layout(&format!("refines-{ringed}-{i}"));
        let mut hosts = Hosts::new(&l, ringed);
        for step in *path {
            hosts.act(step);
        }
        assert_eq!(
            &hosts.project(),
            *state,
            "reached by {path:?} (ringed: {ringed})"
        );
        let kept = hosts.kept();
        assert!(
            (0..2).all(|s| kept[s] & !past.kept[s] == 0),
            "only a waiter the past keeps blocks for no live claim: {kept:?} by {path:?}"
        );
        hosts.act(action);
        assert_eq!(
            &hosts.project(),
            next,
            "{action} from {state:?} (reached by {path:?}, ringed: {ringed})"
        );
        let kept = hosts.kept();
        assert!(
            (0..2).all(|s| kept[s] & !after.kept[s] == 0),
            "only a waiter the past keeps blocks for no live claim: {kept:?} after {action} \
             by {path:?}"
        );
        drop(hosts);
        let _ = std::fs::remove_dir_all(&l.prefix);
    });
}

#[test]
fn an_unopenable_rendezvous_keeps_the_window_and_stands_the_session_by() {
    let l = layout("unopenable");
    std::fs::create_dir_all(l.prefix.join(HOSTS_FILE)).unwrap();
    let mut window = HostClaim::new(Host::Window, &l, None);
    assert_eq!(window.admit(), (Admit::Yes, None));
    let mut session = HostClaim::new(Host::Session, &l, None);
    let (admit, note) = session.admit();
    assert_eq!(admit, Admit::Later(CADENCE));
    assert!(note.unwrap().contains("could not open"), "said");
    assert_eq!(session.admit(), (Admit::Later(CADENCE), None), "said once");
    assert!(matches!(watcher(&l), Watcher::Unknown(_)));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A REFUSED SESSION WAITS ON A WAITER THAT HANDS IT THE SEAT, AND A WAITER WHOSE CLAIM IS GONE
/// LETS THE SEAT GO. With a ring, a session refused the seat answers `Waits` (no deadline) and
/// keeps answering it, trying nothing, while the seat is held; the seat's release rings it, and
/// its next admission takes the seat its waiter took. A second waiter whose claim was dropped
/// takes the seat when it frees and lets it go at once, so a third session is admitted.
/// NEGATIVE CONTROL: a claim with no ring answers the old minute (`Later`). MUTANT: a waiter that
/// does not ring after its hand-over fails the hang detector; one that keeps what it took after its
/// claim is gone leaves the third session refused (the test sees that waiter take the seat, by the
/// pid it writes, before the third session asks).
#[test]
fn a_refused_session_waits_on_a_waiter_that_hands_it_the_seat() {
    let l = layout("waiter");
    let seated = sitting(&l);
    let rest = super::super::Rest::inert();
    let mut waiting = HostClaim::new(Host::Session, &l, Some(rest.ringer()));
    assert_eq!(waiting.admit().0, Admit::Waits, "refused, it waits");
    assert!(waiting.waits() && !waiting.handed());
    assert_eq!(waiting.admit().0, Admit::Waits, "still held: no try");
    let mut ringless = HostClaim::new(Host::Session, &l, None);
    assert_eq!(
        ringless.admit().0,
        Admit::Later(CADENCE),
        "no ring: the minute"
    );
    drop(ringless);
    drop(seated);
    assert!(
        rest.heard(Duration::from_secs(60), |told| told.rung),
        "the seat's release rang the waiter's host (a hang detector)"
    );
    assert!(waiting.handed() && !waiting.waits());
    assert_eq!(
        waiting.admit().0,
        Admit::Yes,
        "it takes the seat its waiter took"
    );
    assert!(waiting.seated_session());
    waiting.end_round();

    let gone_rest = super::super::Rest::inert();
    let mut gone = HostClaim::new(Host::Session, &l, Some(gone_rest.ringer()));
    assert_eq!(gone.admit().0, Admit::Waits);
    drop(gone);
    // Blank the seat's record (its flock is untouched), so the gone claim's waiter is seen to
    // take the seat when it writes its pid there, before the third session asks.
    let seat = l.prefix.join(SEAT_FILE);
    std::fs::write(&seat, b"").unwrap();
    drop(waiting);
    let pid = format!("{}\n", std::process::id());
    aterm_spec::wait::until_within(
        Duration::from_secs(60),
        "the gone claim's waiter took the seat",
        || (std::fs::read_to_string(&seat).ok()? == pid).then_some(()),
    );
    let mut third = HostClaim::new(Host::Session, &l, None);
    aterm_spec::wait::until_within(
        Duration::from_secs(60),
        "the gone claim's waiter let the seat go",
        || (third.admit().0 == Admit::Yes).then_some(()),
    );
    assert!(third.seated_session());
    assert!(
        gone_rest.take().is_none(),
        "a waiter whose claim is gone rings nobody"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
