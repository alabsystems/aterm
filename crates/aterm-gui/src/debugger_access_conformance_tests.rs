// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_settings::OwnerAnswer;
use aterm_models_gui::debugger_access_change_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc;

/// Normalise a mode by the change: its precondition is 0, its target 1, an unrecognised
/// rule 2, and not read 3.
pub(crate) fn norm(mode: DebuggerMode, change: DebuggerChange) -> i64 {
    match mode {
        m if m == change.precondition() => 0,
        m if m == change.target() => 1,
        DebuggerMode::Unrecognized => 2,
        _ => 3,
    }
}

fn change_argv(change: DebuggerChange) -> [&'static str; 2] {
    match change {
        DebuggerChange::On => ["/usr/sbin/DevToolsSecurity", "-enable"],
        DebuggerChange::Off => ["/usr/sbin/DevToolsSecurity", "-disable"],
    }
}

fn denorm(world: i64, change: DebuggerChange) -> DebuggerMode {
    match world {
        0 => change.precondition(),
        1 => change.target(),
        _ => DebuggerMode::Unrecognized,
    }
}

static ALIVE: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
static NEXT_PID: AtomicU32 = AtomicU32::new(3_000_000);

fn alive(child: &DevtoolsChild) -> bool {
    ALIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|set| set.contains(&child.pid))
}

fn set_alive(pid: u32, living: bool) {
    let mut set = ALIVE.lock().unwrap_or_else(|p| p.into_inner());
    let set = set.get_or_insert_with(HashSet::new);
    if living {
        set.insert(pid);
    } else {
        set.remove(&pid);
    }
}

fn stand_in() -> DevtoolsChild {
    DevtoolsChild {
        pid: NEXT_PID.fetch_add(1, Ordering::SeqCst),
        birth: Some(ProcessBirth {
            seconds: 1_790_000_000,
            microseconds: 1,
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Read(i64),
    Spawn(Vec<String>),
}

struct Env {
    change: DebuggerChange,
    world: AtomicI64,
    log: Mutex<Vec<Event>>,
    stage: AtomicI64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    clobbered: i64,
    fresh: i64,
    launches: i64,
    unconsented: i64,
    armed: i64,
    unarmed: i64,
    misadopted: i64,
}

/// The projection's inputs besides the lane.
pub(crate) struct Observed<'a> {
    world: i64,
    cached: DebuggerProbe,
    lock: i64,
    inflight: i64,
    stale: i64,
    seen: &'a Seen,
}

/// The projection (the anchors' `project`).
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Drift",
    reason = "Environment: an administrator, a macOS upgrade or a -disable elsewhere moves \
              the rule back to the change's precondition. The Tier-1 harness moves `world`; \
              the shipping drift watch only records what a probe reads and never re-enables."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SetOutside",
    reason = "Environment: the rule reaches the change's target outside any child of \
              aterm's. The Tier-1 harness moves `world`; the worker's re-read then reads \
              NotDue and spawns nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Customize",
    reason = "Environment: a rule in a shape Apple's tool does not recognise. The Tier-1 \
              harness moves `world`; the worker's re-read then reads NotDue and spawns nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Normalize",
    reason = "Environment: a recognised rule written back. The Tier-1 harness moves `world`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PeerLaunch",
    reason = "Environment: another aterm sharing the config takes the change lock and \
              starts its child. The Tier-1 harness takes the real flock and names a live \
              child in the real change marker; the lane's worker then runs nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PeerExit",
    reason = "Environment: the peer's child exits and its worker releases the lock. The \
              Tier-1 harness drops the flock."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PressWhileInFlight",
    reason = "Buggy = 1 mutant: the lane refuses a press while a child is in flight \
              (on_press: InFlight); the negative control forces one through and the \
              harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SkipRecheck",
    reason = "Buggy = 1 mutant: the only order, devtools_change_with, re-reads -status \
              before it spawns; the negative control runs an order that does not, and the \
              harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "IgnorePeerLock",
    reason = "Buggy = 1 mutant: run_devtools_job takes the change lock before the re-read \
              and holds it through the child; the negative control runs a worker that takes \
              no lock, and the harness fails (the projected lock is not held)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "ClaimFromExitCode",
    reason = "Buggy = 1 mutant: the reducer's observation and the row's claim come only \
              from a probe; the negative control makes the lane take the exit as the \
              target, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "RetryAfterCancel",
    reason = "Buggy = 1 mutant: an unchanged result ends the change (NotChanged) and the \
              host starts a worker only after the owner's yes; the negative control retries \
              without one, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SpendBanked",
    reason = "Buggy = 1 mutant: the lane arms a job only inside begin, on the owner's yes \
              (the scenarios' No/NotAsked/Quiet presses launch nothing); a worker started with no \
              answer is the RetryAfterCancel control's shape."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "AdoptLive",
    reason = "Buggy = 1 mutant: read_records files a change marker as stale only when its \
              child is gone by pid and birth; the negative control runs a records reader \
              that skips that check over a peer's running child, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "ResultWakeLost",
    reason = "Buggy = 1 mutant: the lane's needs_poll holds while a change is under way, \
              whatever the wake did; the negative control drops the re-read while a child \
              is in flight and the harness fails."
)]
pub(crate) fn project(lane: &DebuggerChangeLane, observed: &Observed<'_>) -> State {
    let flow = &lane.flow;
    let change = flow.direction();
    let facts = lane.facts(true, observed.cached, SelfHardening::Hardened);
    let claim = crate::native_settings::debugger_row_states(&facts) == Some(change.target());
    let mut state = State::new();
    state.insert(
        "phase",
        match flow.phase() {
            Phase::Idle => 0,
            Phase::Confirming(_) => 1,
            Phase::Rechecking(_) => 2,
            Phase::InFlight { .. } if lane.result_unread() => 5,
            Phase::InFlight { .. } => 4,
            Phase::Exited(_) => 6,
        },
    );
    state.insert("world", observed.world);
    state.insert("obs", norm(flow.observed(), change));
    state.insert("claim", i64::from(claim));
    state.insert("polling", i64::from(lane.needs_poll()));
    state.insert("lock", observed.lock);
    state.insert("fresh", observed.seen.fresh);
    state.insert("launches", observed.seen.launches);
    state.insert("unconsented", observed.seen.unconsented);
    state.insert("inflight", observed.inflight);
    state.insert("armed", observed.seen.armed);
    state.insert("unarmed", observed.seen.unarmed);
    state.insert("clobbered", observed.seen.clobbered);
    state.insert("stale", observed.stale);
    state.insert("misadopted", observed.seen.misadopted);
    state
}

fn scripted(mode: DebuggerMode) -> DebuggerProbe {
    DebuggerProbe {
        mode,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    }
}

/// One defect a negative control plants in the composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    None,
    SkipRecheck,
    IgnorePeerLock,
    PressWhileInFlight,
    ClaimFromExit,
    DropPollInFlight,
    RetryAfterCancel,
    AdoptLive,
}

struct Worker {
    exit: mpsc::Sender<DevtoolsChangeRun>,
    handle: std::thread::JoinHandle<()>,
    env: Arc<Env>,
    child: DevtoolsChild,
}

/// The harness: the real lane, the environment, and the model state it must keep agreeing with.
pub(crate) struct Rig {
    model: Model,
    lane: DebuggerChangeLane,
    change: DebuggerChange,
    fault: Fault,
    dir: PathBuf,
    config: PathBuf,
    world: i64,
    cached: DebuggerProbe,
    seen: Seen,
    consumed: usize,
    worker: Option<Worker>,
    peer: Option<(ChangeLock, DevtoolsChild)>,
    peer_gone: Option<DevtoolsChild>,
    inherited: Option<DevtoolsChild>,
    state: State,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            drop(worker.exit);
            let _ = worker.handle.join();
            set_alive(worker.child.pid, false);
        }
        if let Some((_, child)) = self.peer.take() {
            set_alive(child.pid, false);
        }
        if let Some(child) = self.inherited.take() {
            set_alive(child.pid, false);
        }
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn hang_detector(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "hang: {what} did not happen within a minute"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn scratch() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-debugger-rig-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Rig {
    fn bare(change: DebuggerChange, world: DebuggerMode, fault: Fault) -> Self {
        let dir = scratch();
        let config = dir.join("aterm.toml");
        let mut lane = DebuggerChangeLane::with_liveness(alive);
        lane.flow.direction = change;
        Self {
            model: debugger_access_change_model(),
            lane,
            change,
            fault,
            dir,
            config,
            world: norm(world, change),
            cached: DebuggerProbe::pending(),
            seen: Seen::default(),
            consumed: 0,
            worker: None,
            peer: None,
            peer_gone: None,
            inherited: None,
            state: State::new(),
        }
    }

    /// A fresh lane: records read, nothing observed — the model's initial state.
    pub(crate) fn new(change: DebuggerChange, world: DebuggerMode, fault: Fault) -> Self {
        let mut rig = Self::bare(change, world, fault);
        rig.record_world();
        rig.lane.settle_records(read_records(&rig.config, alive));
        let step = rig.lane.poll(now_secs());
        rig.apply(step);
        let state = rig.now();
        let mut init = rig.model.init_state();
        init.insert("world", rig.world);
        assert_eq!(state, init, "the fresh lane is the model's initial state");
        rig.state = state;
        rig
    }

    fn inherited(change: DebuggerChange, world: DebuggerMode) -> Self {
        let mut rig = Self::bare(change, world, Fault::None);
        let child = stand_in();
        set_alive(child.pid, true);
        let marker = ChangeMarker::new(change, child, 1).unwrap();
        write_debugger_marker(
            &rig.config,
            CHANGE_MARKER,
            &marker.encode(),
            crate::durable::Flush::Durable,
        )
        .unwrap();
        rig.inherited = Some(child);
        rig.seen.launches = 1;
        rig.record_world();
        let records = read_records(&rig.config, alive);
        assert_eq!(records.inherited, Some(marker));
        rig.lane.settle_records(records);
        let step = rig.lane.poll(now_secs());
        rig.apply(step);
        rig.state = rig.now();
        for invariant in &rig.model.invariants {
            assert!(
                rig.model.check_invariant(invariant.name, &rig.state),
                "{}: {:?}",
                invariant.name,
                rig.state
            );
        }
        rig
    }

    fn record_world(&self) {
        let marker = ObservedMarker {
            mode: denorm(self.world, self.change),
            since: 1,
            origin: Origin::Unknown,
        };
        write_debugger_marker(
            &self.config,
            OBSERVED_MARKER,
            &marker.encode(),
            crate::durable::Flush::Ordered,
        )
        .unwrap();
    }

    fn recorded(&self) -> Option<(i64, Origin)> {
        read_debugger_marker(&self.config, OBSERVED_MARKER)
            .and_then(|text| ObservedMarker::parse(&text))
            .map(|m| (norm(m.mode, self.change), m.origin))
    }

    fn lock_now(&self) -> i64 {
        if self.peer.is_some() {
            return 2;
        }
        let marker_live = read_debugger_marker(&self.config, CHANGE_MARKER)
            .and_then(|text| ChangeMarker::parse(&text))
            .is_some_and(|marker| alive(&marker.child));
        match try_change_lock(&self.config) {
            LockTry::Busy => 1,
            LockTry::Held(_) if marker_live => 1,
            LockTry::Held(_) => 0,
            LockTry::Unavailable(error) => panic!("the scratch lock: {error}"),
        }
    }

    fn lock_detail(&self) -> String {
        let marker = read_debugger_marker(&self.config, CHANGE_MARKER)
            .and_then(|text| ChangeMarker::parse(&text));
        let flock = match try_change_lock(&self.config) {
            LockTry::Busy => "busy",
            LockTry::Held(_) => "free",
            LockTry::Unavailable(_) => "unavailable",
        };
        format!(
            "flock {flock}; marker {}",
            marker.map_or_else(
                || "none".to_string(),
                |m| format!("names pid {} (alive: {})", m.child.pid, alive(&m.child))
            )
        )
    }

    fn inflight_now(&self) -> i64 {
        let ours = self
            .worker
            .as_ref()
            .map_or(0, |worker| i64::from(alive(&worker.child)));
        let peer = self
            .peer
            .as_ref()
            .map_or(0, |(_, child)| i64::from(alive(child)));
        let inherited = self.inherited.map_or(0, |child| i64::from(alive(&child)));
        ours + peer + inherited
    }

    fn stale_now(&self) -> i64 {
        i64::from(
            read_debugger_marker(&self.config, CHANGE_MARKER)
                .and_then(|text| ChangeMarker::parse(&text))
                .is_some_and(|marker| !alive(&marker.child) && !self.lane.follows(&marker.child)),
        )
    }

    fn needs_poll(&self) -> bool {
        let real = self.lane.needs_poll();
        if self.fault == Fault::DropPollInFlight
            && matches!(self.lane.flow.phase(), Phase::InFlight { .. })
        {
            return false;
        }
        real
    }

    fn now(&self) -> State {
        let mut state = project(
            &self.lane,
            &Observed {
                world: self.world,
                cached: self.cached,
                lock: self.lock_now(),
                inflight: self.inflight_now(),
                stale: self.stale_now(),
                seen: &self.seen,
            },
        );
        state.insert("polling", i64::from(self.needs_poll()));
        state
    }

    fn apply(&mut self, step: LaneStep) {
        if step.invalidate_probe {
            self.cached = DebuggerProbe::pending();
        }
        settle_markers(
            &self.config,
            step.drift.write,
            step.remove_change_marker.as_ref(),
        );
    }

    fn check_invariants(&self) {
        for invariant in &self.model.invariants {
            assert!(
                self.model.check_invariant(invariant.name, &self.state),
                "{}: {:?}",
                invariant.name,
                self.state
            );
        }
    }

    fn step(&mut self, action: &str, event: impl FnOnce(&mut Self) -> bool) {
        let pre = self.now();
        assert_eq!(
            pre, self.state,
            "{action}: the projection drifted from the model"
        );
        let enabled = self.model.action_enabled(action, &pre);
        let taken = event(self);
        assert_eq!(
            taken,
            enabled,
            "{action}: the composition {} what the model {}: {pre:?}",
            if taken { "took" } else { "refused" },
            if enabled { "allows" } else { "forbids" },
        );
        let post = self.now();
        if taken {
            assert!(
                self.model.successors(action, &pre).contains(&post),
                "{action}: {pre:?} -> {post:?} is not a step the model admits ({})",
                self.lock_detail()
            );
            self.state = post;
        } else {
            assert_eq!(post, pre, "{action}: a refusal changed nothing");
        }
        self.check_invariants();
    }

    fn chain(&self, pre: &State, actions: &[&str]) -> Vec<State> {
        let mut states = vec![pre.clone()];
        for action in actions {
            let next: Vec<State> = states
                .iter()
                .flat_map(|state| self.model.successors(action, state))
                .collect();
            assert!(!next.is_empty(), "{action} enabled after {states:?}");
            states = next;
        }
        states
    }

    fn probe(&mut self) {
        let read = scripted(denorm(self.world, self.change));
        self.step("Probe", |rig| rig.observe(read));
    }

    fn probe_fails(&mut self) {
        let read = DebuggerProbe {
            mode: DebuggerMode::Unknown,
            member: DebuggerMember::Unknown,
            label: DebuggerProbeLabel::TimedOut,
        };
        self.step("ProbeFails", |rig| rig.observe(read));
    }

    fn observe(&mut self, read: DebuggerProbe) -> bool {
        let accepts = self.lane.flow.accepts_probe();
        let step = self.lane.observe(read, now_secs());
        if accepts {
            self.cached = read;
        }
        let outcome = step.outcome;
        self.apply(step);
        if self.fault == Fault::RetryAfterCancel && matches!(outcome, Some(Outcome::NotChanged(_)))
        {
            let job = self
                .lane
                .begin(self.change, OwnerAnswer::yes_for_test())
                .expect("the retry is taken");
            self.start_worker(job);
            let step = self.lane.poll(now_secs());
            self.apply(step);
            self.account();
        }
        accepts
    }

    fn start_worker(&mut self, ticket: ChangeTicket) {
        assert!(self.worker.is_none(), "one worker at a time in the harness");
        let env = Arc::new(Env {
            change: self.change,
            world: AtomicI64::new(self.world),
            log: Mutex::new(Vec::new()),
            stage: AtomicI64::new(0),
        });
        let child = stand_in();
        let (exit, waits) = mpsc::channel::<DevtoolsChangeRun>();
        let job = DevtoolsJob::new(
            ticket,
            ProbeGate::on(),
            self.lane.slot(),
            (self.fault != Fault::IgnorePeerLock).then(|| self.config.clone()),
            Arc::new(|| {}),
            alive,
        );
        let fault = self.fault;
        let shared = Arc::clone(&env);
        let peer_holds_lock = self.peer.is_some();
        let handle = std::thread::Builder::new()
            .name("rig-consent-devtools".into())
            .spawn(move || {
                let env = shared;
                let mut first_clock_read = true;
                run_devtools_job_with_clock(
                    job,
                    |change, _gate, on_spawn| {
                        let recheck = || {
                            let world = env.world.load(Ordering::SeqCst);
                            env.log
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(Event::Read(world));
                            scripted(denorm(world, env.change))
                        };
                        let spawn =
                            |argv: &[&str]| {
                                env.log.lock().unwrap_or_else(|p| p.into_inner()).push(
                                    Event::Spawn(argv.iter().map(|a| (*a).to_string()).collect()),
                                );
                                set_alive(child.pid, true);
                                on_spawn(child);
                                env.stage.store(1, Ordering::SeqCst);
                                let exit = waits.recv().unwrap_or(DevtoolsChangeRun::WaitFailed);
                                set_alive(child.pid, false);
                                exit
                            };
                        if fault == Fault::SkipRecheck {
                            spawn(&change_argv(change))
                        } else {
                            aterm_containment::consent::devtools_change_with_for_test(
                                change, recheck, spawn,
                            )
                        }
                    },
                    || {
                        let first = std::mem::replace(&mut first_clock_read, false);
                        std::time::Instant::now()
                            - if first && peer_holds_lock {
                                Duration::from_secs(61)
                            } else {
                                Duration::ZERO
                            }
                    },
                );
                env.stage.store(2, Ordering::SeqCst);
            })
            .expect("the harness's worker thread");
        let stage = Arc::clone(&env);
        hang_detector("the worker blocking in its child or ending", || {
            stage.stage.load(Ordering::SeqCst) != 0
        });
        self.consumed = 0;
        self.worker = Some(Worker {
            exit,
            handle,
            env,
            child,
        });
    }

    fn account(&mut self) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let events = worker
            .env
            .log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let mut read_precondition = false;
        for event in &events[self.consumed..] {
            match event {
                Event::Read(world) => read_precondition = *world == 0,
                Event::Spawn(argv) => {
                    assert_eq!(
                        argv.iter().map(String::as_str).collect::<Vec<_>>(),
                        change_argv(self.change),
                        "the child runs the change's own argv"
                    );
                    self.seen.launches += 1;
                    if !read_precondition {
                        self.seen.unarmed += 1;
                    }
                    if self.seen.fresh == 0 {
                        self.seen.unconsented += 1;
                    }
                    self.seen.fresh = 0;
                    read_precondition = false;
                }
            }
        }
        self.consumed = events.len();
        self.seen.armed = 0;
        if worker.env.stage.load(Ordering::SeqCst) == 2 {
            self.seen.fresh = 0;
            if let Some(worker) = self.worker.take() {
                worker.handle.join().expect("the worker ends");
            }
        }
    }

    fn press(&mut self, answer: OwnerAnswer) {
        let pre = self.now();
        assert_eq!(
            pre, self.state,
            "press: the projection drifted from the model"
        );
        if self.fault == Fault::PressWhileInFlight
            && matches!(self.lane.flow.phase, Phase::InFlight { .. })
        {
            self.lane.flow.phase = Phase::Idle;
        }
        let press_enabled = self.model.action_enabled("Press", &pre);
        let answered_yes = matches!(answer, OwnerAnswer::Yes(_));
        let answer_label = format!("{answer:?}");
        let begun = self.lane.begin(self.change, answer);
        let pressed = !matches!(
            begun,
            Err(Refusal::RecordsUnread | Refusal::InFlight | Refusal::Busy | Refusal::State(..))
        );
        assert_eq!(
            pressed,
            press_enabled,
            "Press: the lane {} what the model {}: {pre:?}",
            if pressed { "took" } else { "refused" },
            if press_enabled { "allows" } else { "forbids" },
        );
        let targets = match begun {
            Err(Refusal::NotConfirmed) => self.chain(&pre, &["Press", "Decline"]),
            Err(_) => {
                assert_eq!(self.now(), pre, "a refused press changed nothing");
                return;
            }
            Ok(job) => {
                assert!(answered_yes, "a job only on a yes");
                self.seen.fresh = 1;
                self.start_worker(job);
                let launched = self
                    .worker
                    .as_ref()
                    .is_some_and(|w| w.env.stage.load(Ordering::SeqCst) == 1);
                let step = self.lane.poll(now_secs());
                self.apply(step);
                self.account();
                if launched {
                    self.chain(&pre, &["Press", "Consent", "Recheck", "Launch"])
                } else {
                    self.chain(&pre, &["Press", "Consent", "Recheck"])
                }
            }
        };
        let post = self.now();
        assert!(
            targets.contains(&post),
            "press ({answer_label}): {pre:?} -> {post:?} is none of {targets:?}"
        );
        self.state = post;
        self.check_invariants();
    }

    fn exit(&mut self, action: &str) {
        self.step(action, |rig| {
            match action {
                "ChildApplied" => rig.world = 1,
                "ChildClobbers" => {
                    rig.world = 1;
                    rig.seen.clobbered = 1;
                }
                _ => {}
            }
            if let Some(worker) = rig.worker.as_ref() {
                worker.env.world.store(rig.world, Ordering::SeqCst);
                let code = if action == "ChildUnchanged" { 1 } else { 0 };
                worker
                    .exit
                    .send(DevtoolsChangeRun::Exited {
                        code: Some(code),
                        stdout_head: String::new(),
                        stderr_head: String::new(),
                    })
                    .expect("the stand-in waits for its exit");
                let env = Arc::clone(&worker.env);
                hang_detector("the worker ending", || {
                    env.stage.load(Ordering::SeqCst) == 2
                });
                rig.account();
            } else if let Some(child) = rig.inherited {
                set_alive(child.pid, false);
            } else {
                return false;
            }
            true
        });
    }

    fn notice(&mut self) {
        self.step("Notice", |rig| {
            if !rig.needs_poll() {
                return false;
            }
            let before = rig.lane.flow.phase();
            let step = rig.lane.poll(now_secs());
            rig.apply(step);
            if rig.fault == Fault::ClaimFromExit
                && let Phase::Exited(change) = rig.lane.flow.phase
            {
                rig.lane.flow.observed = change.target();
            }
            rig.lane.flow.phase() != before
        });
    }

    fn environment(&mut self, action: &str, world: i64) {
        self.step(action, |rig| {
            rig.world = world;
            if let Some(worker) = rig.worker.as_ref() {
                worker.env.world.store(world, Ordering::SeqCst);
            }
            true
        });
    }

    fn peer_launch(&mut self) {
        self.step("PeerLaunch", |rig| {
            let LockTry::Held(lock) = try_change_lock(&rig.config) else {
                return false;
            };
            if read_debugger_marker(&rig.config, CHANGE_MARKER)
                .and_then(|text| ChangeMarker::parse(&text))
                .is_some_and(|marker| alive(&marker.child))
            {
                return false;
            }
            let child = stand_in();
            set_alive(child.pid, true);
            let marker = ChangeMarker::new(rig.change, child, 1).unwrap();
            write_debugger_marker(
                &rig.config,
                CHANGE_MARKER,
                &marker.encode(),
                crate::durable::Flush::Durable,
            )
            .unwrap();
            rig.peer = Some((lock, child));
            true
        });
    }

    fn peer_exit(&mut self) {
        self.step("PeerExit", |rig| {
            let Some((lock, child)) = rig.peer.take() else {
                return false;
            };
            set_alive(child.pid, false);
            drop(lock);
            rig.peer_gone = Some(child);
            true
        });
    }

    fn peer_settle(&mut self) {
        self.step("PeerSettle", |rig| {
            let Some(child) = rig.peer_gone else {
                return false;
            };
            let before = rig.now();
            settle_markers(&rig.config, None, Some(&child));
            rig.now() != before
        });
    }

    fn reread(&mut self, action: &str) {
        self.step(action, |rig| {
            let before = rig.now();
            let liveness: fn(&DevtoolsChild) -> bool = if rig.fault == Fault::AdoptLive {
                |_| false
            } else {
                alive
            };
            let into = rig.lane.ask_records();
            *into.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(read_records(&rig.config, liveness));
            let step = rig.lane.poll(now_secs());
            rig.apply(step);
            if rig.lane.settling.is_some_and(|child| alive(&child)) {
                rig.seen.misadopted = 1;
            }
            rig.now() != before
        });
    }
}

fn scenarios(fault: Fault) {
    for change in [DebuggerChange::On, DebuggerChange::Off] {
        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.press(OwnerAnswer::yes_for_test());
        rig.probe();
        rig.press(OwnerAnswer::yes_for_test());
        rig.press(OwnerAnswer::yes_for_test());
        rig.probe();
        rig.notice();
        rig.exit("ChildApplied");
        rig.probe();
        rig.press(OwnerAnswer::yes_for_test());
        rig.notice();
        assert!(
            rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: the marker outlives the taken result"
        );
        rig.press(OwnerAnswer::yes_for_test());
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(change)));
        assert_eq!(rig.recorded(), Some((1, Origin::Aterm)), "{change:?}");
        assert!(
            !rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: and goes once the re-probe is recorded"
        );
        assert_eq!(rig.state["claim"], 1, "{change:?}: the re-probe says it");

        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.probe();
        rig.press(OwnerAnswer::No);
        rig.press(OwnerAnswer::NotAsked);
        rig.press(OwnerAnswer::Quiet);
        rig.press(OwnerAnswer::yes_for_test());
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
        assert_eq!(rig.seen.launches, 1);

        for (moved, world) in [("Customize", 2), ("SetOutside", 1)] {
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.environment(moved, world);
            rig.press(OwnerAnswer::yes_for_test());
            assert_eq!(rig.seen.launches, 0, "{change:?} {moved}: nothing launched");
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
        }

        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.probe();
        rig.press(OwnerAnswer::yes_for_test());
        rig.environment("Customize", 2);
        rig.exit("ChildClobbers");
        rig.notice();
        rig.probe();
        assert_eq!(rig.state["clobbered"], 1);

        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.probe();
        rig.press(OwnerAnswer::yes_for_test());
        rig.exit("ChildApplied");
        rig.notice();
        rig.probe_fails();
        assert_eq!(rig.state["claim"], 0, "a failed probe claims nothing");
        rig.probe();
        rig.environment("Drift", 0);
        rig.probe();
        rig.environment("Customize", 2);
        rig.environment("Normalize", 0);
        rig.probe();
        rig.press(OwnerAnswer::yes_for_test());
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();

        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.probe();
        rig.peer_launch();
        rig.press(OwnerAnswer::yes_for_test());
        assert_eq!(
            rig.lane.flow.last(),
            Some(Outcome::PeerInFlight(change)),
            "{change:?}"
        );
        rig.peer_exit();
        rig.press(OwnerAnswer::yes_for_test());
        rig.peer_launch();
        rig.exit("ChildApplied");
        rig.notice();
        rig.probe();

        let mut rig = Rig::inherited(change, change.precondition());
        rig.press(OwnerAnswer::yes_for_test());
        rig.peer_launch();
        rig.notice();
        rig.exit("ChildUnchanged");
        rig.notice();
        assert!(
            rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: the dead predecessor's marker stays until its change is recorded"
        );
        rig.probe();
        assert!(
            !rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: and goes once it is"
        );
        assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
        assert_eq!(
            rig.state["launches"], 1,
            "the predecessor's launch stays counted"
        );

        let mut rig = Rig::new(change, change.precondition(), fault);
        rig.probe();
        rig.reread("Adopt");
        rig.peer_launch();
        rig.reread("Follow");
        rig.press(OwnerAnswer::yes_for_test());
        rig.peer_exit();
        rig.notice();
        rig.probe();
        assert!(
            !rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: the followed change's marker goes with its re-probe"
        );
        rig.peer_launch();
        rig.peer_exit();
        rig.reread("Adopt");
        assert!(
            rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: the adopted change's marker stays until its re-probe is recorded"
        );
        rig.press(OwnerAnswer::yes_for_test());
        rig.reread("Adopt");
        assert!(
            rig.dir.join(CHANGE_MARKER).exists(),
            "a re-read of a change this lane follows leaves its marker for the re-probe"
        );
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
        assert!(
            !rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: and goes once it is"
        );
        rig.peer_launch();
        rig.peer_exit();
        rig.peer_settle();
        rig.reread("Adopt");
        rig.press(OwnerAnswer::yes_for_test());
        rig.exit("ChildApplied");
        rig.notice();
        rig.peer_launch();
        rig.peer_exit();
        rig.reread("Discard");
        assert!(
            !rig.dir.join(CHANGE_MARKER).exists(),
            "{change:?}: the stranger's dead marker went"
        );
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(change)));
    }
}

#[cfg(unix)]
#[test]
fn debugger_access_conforms_to_the_model() {
    let _patience = patience_for_test(Duration::ZERO);
    scenarios(Fault::None);
}

#[cfg(unix)]
#[test]
fn the_harness_fails_every_faulty_composition() {
    let _patience = patience_for_test(Duration::ZERO);
    for fault in [
        Fault::SkipRecheck,
        Fault::IgnorePeerLock,
        Fault::PressWhileInFlight,
        Fault::ClaimFromExit,
        Fault::DropPollInFlight,
        Fault::RetryAfterCancel,
        Fault::AdoptLive,
    ] {
        let caught = std::panic::catch_unwind(|| match fault {
            Fault::PressWhileInFlight => {
                let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                rig.probe();
                rig.press(OwnerAnswer::yes_for_test());
                rig.press(OwnerAnswer::yes_for_test());
            }
            Fault::RetryAfterCancel => {
                let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                rig.probe();
                rig.press(OwnerAnswer::yes_for_test());
                rig.exit("ChildUnchanged");
                rig.notice();
                rig.probe();
            }
            _ => scenarios(fault),
        });
        assert!(
            caught.is_err(),
            "{fault:?}: the harness passed a faulty composition"
        );
    }
}
