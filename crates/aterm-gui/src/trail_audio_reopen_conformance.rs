// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 CONFORMANCE of the derived `TrailAudioReopenLadder` machine
//! (`aterm_spec::derive::trail_audio_reopen_ladder_model`) to the shipping
//! reopen ladder in `trail_audio.rs` (D5).
//!
//! Two halves, because the ladder has two halves in the code:
//!
//! * THE SCHEDULE — `RetryState` and `reopen_after_fault` — is pure, so it is
//!   driven EXHAUSTIVELY on a fabricated clock, with the SHIPPING
//!   `REOPEN_BACKOFF` and with the committed model's two-step budget. Every
//!   real step is projected onto the model and must be a model successor;
//!   the model's guards must agree with the real predicates (`ready`, the
//!   healthy window) in every visited configuration; and the run must visit
//!   every model state, so the binding is two-way.
//! * THE CALL SITES — `worker_loop`, where the ladder is spent, reset and
//!   reported — are driven for real on a worker thread with a scripted
//!   output, and the recorded trace (what the worker's own atomics said at
//!   every platform call) is validated against the model. All three fault
//!   sites appear in the traces: a failed open, a failed push, and the
//!   housekeeping TICK that finds a played device's callback stalled
//!   (`Service::Reopen`, the production path for a stall between keys). A
//!   fault is charged unless it is the one re-push `worker_loop` makes into
//!   a fresh device for the cue that discovered the fault — the validator
//!   admits an uncharged call there and nowhere else (the pushed cue's
//!   index rides in the trace). This is where the first draft of D5 reset
//!   the ladder on a successful OPEN, which no schedule-level test could
//!   see. The same trace also binds the busy stamp (`busy_stale`: an open is
//!   judged at `OPEN_WEDGE_AFTER_MS`, a push or tick at `WEDGE_AFTER_MS`)
//!   and the wire word (`host_state`: `failed` never appears with budget
//!   left). A run ends by closing the channel and joining the worker, so
//!   its final state is read, never sampled after a sleep.
//!
//! WHAT IT FOUND. The two-way check refused the shipped ladder on its first
//! run: the model allowed a state the SHIPPING schedule could not reach
//! (the last backoff wait over, the healthy window not), because in the code
//! the two were the same instant — the window was measured from the FAULT,
//! and the last wait IS the window, so the first delivery after the final
//! reopen handed the whole budget back. A device that plays one block and
//! stalls cycled the ladder forever. `RetryState::delivered` now measures
//! PLAY time from the first delivery after the fault; the
//! `plays-once-then-stalls` worker scenario failed before that fix (the
//! budget was never spent) and passes after it.
//!
//! NEGATIVE CONTROLS, so a pass is never vacuous: the pre-D5 law (one failed
//! open is terminal), the first draft's reset-on-open, an eager reset on any
//! delivery, the window-from-the-fault reset above, and a schedule whose
//! first reopen waits are each run through the schedule half and REJECTED;
//! doctored worker traces carrying the first two defects, a worker that
//! stops charging push faults (which this validator's first draft
//! ACCEPTED, and the test shows it), and an uncharged tick fault are each
//! REJECTED by the trace validator that accepts the real ones. (The trace
//! validator does not observe time, so the window-from-the-fault defect is
//! refused by the schedule half and by `plays-once-then-stalls`'s terminal
//! assertion, not by it.)
//!
//! Wired as a child of `trail_audio` (one `mod` line there) so it can drive
//! the private ladder directly; everything it needs is read, not copied.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::{EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice};
use aterm_spec::derive::{Model, trail_audio_reopen_ladder_model};
use aterm_spec::interp::{self, State};

use super::mac::{Delivery, Service, callback_stalled};
use super::{
    AudioWorkerOutput, BUSY_OPENING, Cue, HostState, OPEN_WEDGE_AFTER_MS, REOPEN_BACKOFF,
    REOPEN_BUDGET, Reopen, RetryState, STALL_AFTER_MS, STATE_DORMANT, STATE_FAILED, TrailAudio,
    WEDGE_AFTER_MS, WorkerFlags, busy_stale, cue_channel, reopen_after_fault, worker_loop,
};

type Key = Vec<(&'static str, i64)>;

fn key(s: &State) -> Key {
    s.iter().map(|(k, v)| (*k, *v)).collect()
}

/// The committed model at the budget a schedule implies.
fn model_for(schedule: &[Duration]) -> Model {
    interp::with_consts(
        &trail_audio_reopen_ladder_model(),
        &[("Budget", schedule.len() as i64)],
    )
}

/// Every state reachable from `from` by the three CLOCK actions — time the
/// harness does not narrate step by step (one real clock jump can cross
/// several model clocks at once).
fn time_closure(m: &Model, from: &BTreeSet<Key>) -> BTreeSet<Key> {
    let mut seen = from.clone();
    let mut queue: VecDeque<Key> = from.iter().cloned().collect();
    while let Some(k) = queue.pop_front() {
        let s: State = k.into_iter().collect();
        for action in ["BackoffElapses", "StaleElapses", "HealthyWindowElapses"] {
            for n in m.successors(action, &s) {
                if seen.insert(key(&n)) {
                    queue.push_back(key(&n));
                }
            }
        }
    }
    seen
}

fn reachable(m: &Model) -> BTreeSet<Key> {
    let mut seen = BTreeSet::from([key(&m.init_state())]);
    let mut queue = VecDeque::from([m.init_state()]);
    while let Some(s) = queue.pop_front() {
        for a in &m.actions {
            for n in m.successors(a.name, &s) {
                if seen.insert(key(&n)) {
                    queue.push_back(n);
                }
            }
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// HALF 1 — the schedule, exhaustively, on a fabricated clock.
// ---------------------------------------------------------------------------

/// A historical or hypothetical defect the schedule harness can replay.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mutation {
    /// What ships.
    None,
    /// The pre-D5 law: a failed OPEN latches failure on the spot.
    TerminalOpenFault,
    /// The first draft of D5: a successful open resets the ladder.
    ResetOnOpen,
    /// Any delivery resets the ladder, healthy window or not.
    EagerReset,
    /// The shipped D5 law until this conformance found it: the healthy
    /// window measured from the FAULT, so the backoff wait counted as play.
    ResetFromFault,
}

/// One real configuration of the ladder: the real `RetryState`, whether the
/// worker holds a device, whether it has exited, the fabricated clock, and
/// the SPEC-side ghosts the code does not keep — the count of reopens since
/// a device last played through a healthy window (kept apart from
/// `attempts` so the code's counter can be checked against it), when the
/// last reopen was scheduled (the `stale` clock), and the `evidence` record:
/// whether a reopen is outstanding and when the device first delivered
/// after it — kept apart from the code's `played_since`, so the model's
/// `ResetOnlyOnPlayedWindow` compares the code's reset against an
/// independent witness.
struct Ladder<'a> {
    retry: RetryState<'a>,
    device: bool,
    failed: bool,
    now: Instant,
    spent: i64,
    fault_at: Option<Instant>,
    outstanding: bool,
    first_delivery: Option<Instant>,
    mutation: Mutation,
}

impl<'a> Ladder<'a> {
    fn fork(&self) -> Self {
        Self {
            retry: RetryState {
                attempts: self.retry.attempts,
                earliest: self.retry.earliest,
                played_since: self.retry.played_since,
                backoff: self.retry.backoff,
            },
            device: self.device,
            failed: self.failed,
            now: self.now,
            spent: self.spent,
            fault_at: self.fault_at,
            outstanding: self.outstanding,
            first_delivery: self.first_delivery,
            mutation: self.mutation,
        }
    }

    fn since(&self, t: Option<Instant>) -> Option<Duration> {
        t.map(|t| self.now.saturating_duration_since(t))
    }

    fn waiting(&self) -> bool {
        !self.retry.ready(self.now)
    }

    fn stale(&self) -> bool {
        self.since(self.fault_at)
            .is_none_or(|d| d >= self.retry.healthy_for())
    }

    /// The healthy window's clock, off the REAL `played_since`.
    fn played(&self) -> i64 {
        match self.since(self.retry.played_since) {
            None => 0,
            Some(d) if d >= self.retry.healthy_for() => 2,
            Some(_) => 1,
        }
    }

    /// The spec's `evidence` record, off the ghost (never the code).
    fn evidence(&self) -> i64 {
        if !self.outstanding {
            return 2;
        }
        match self.since(self.first_delivery) {
            None => 0,
            Some(d) if d >= self.retry.healthy_for() => 2,
            Some(_) => 1,
        }
    }

    fn project(&self) -> State {
        [
            ("attempts", i64::from(self.retry.attempts)),
            ("spent", self.spent),
            ("device", i64::from(self.device)),
            ("waiting", i64::from(self.waiting())),
            ("played", self.played()),
            ("stale", i64::from(self.stale())),
            ("failed", i64::from(self.failed)),
            ("evidence", self.evidence()),
        ]
        .into_iter()
        .collect()
    }

    /// A canonical key for the REAL configuration: the projection plus the
    /// clock offsets the next steps depend on.
    fn config_key(&self) -> (Key, [Option<Duration>; 4]) {
        (
            key(&self.project()),
            [
                self.retry
                    .earliest
                    .map(|t| t.saturating_duration_since(self.now)),
                self.since(self.retry.played_since),
                self.since(self.fault_at),
                self.outstanding
                    .then(|| self.since(self.first_delivery))
                    .flatten(),
            ],
        )
    }

    /// Whether the ENVIRONMENT can produce `action` here, read off the real
    /// predicates. Compared against the model's own `action_enabled`.
    fn possible(&self, action: &str) -> bool {
        match action {
            // A cue reaches a worker holding no device.
            "Cue" => !self.device,
            // A delivery attempt; it only resets anything with a device.
            "CueSounds" => true,
            // A fault is observed only when the worker touches the platform:
            // it holds a device, or it is allowed to attempt an open.
            "DeviceFaults" => !self.failed && (self.device || self.retry.ready(self.now)),
            "BackoffElapses" => self.waiting(),
            "StaleElapses" => !self.stale(),
            "HealthyWindowElapses" => self.played() == 1,
            other => panic!("no action {other}"),
        }
    }

    /// Run the REAL code for `action`.
    fn step(&mut self, action: &str) {
        let budget = self.retry.backoff.len() as i64;
        match action {
            "Cue" => {
                // Sealed ingress and the backoff window both drop the cue.
                if !self.failed && self.retry.ready(self.now) {
                    self.device = true;
                    if self.mutation == Mutation::ResetOnOpen {
                        self.retry.attempts = 0;
                        self.retry.earliest = None;
                        self.retry.played_since = None;
                    }
                }
            }
            "CueSounds" => {
                if self.device {
                    // The spec's reset law reads its own record, taken
                    // BEFORE this delivery is noted.
                    let played_through = self.evidence() == 2;
                    if self.outstanding && self.first_delivery.is_none() {
                        self.first_delivery = Some(self.now);
                    }
                    let reset = match self.mutation {
                        Mutation::EagerReset => true,
                        Mutation::ResetFromFault => self.retry.attempts > 0 && self.stale(),
                        _ => false,
                    };
                    if reset {
                        self.retry.attempts = 0;
                        self.retry.earliest = None;
                        self.retry.played_since = None;
                    } else {
                        self.retry.delivered(self.now);
                    }
                    if played_through {
                        self.spent = 0;
                    }
                }
            }
            "DeviceFaults" => {
                if self.mutation == Mutation::TerminalOpenFault && !self.device {
                    self.failed = true;
                    return;
                }
                let mut output = self.device.then_some(());
                let verdict = reopen_after_fault(&mut output, &mut self.retry, self.now);
                assert!(output.is_none(), "the fault handler throws the device away");
                self.device = false;
                match verdict {
                    Reopen::Now | Reopen::Wait => {
                        self.spent = (self.spent + 1).min(budget + 1);
                        self.fault_at = Some(self.now);
                        self.outstanding = true;
                        self.first_delivery = None;
                    }
                    Reopen::Exhausted => self.failed = true,
                }
            }
            "BackoffElapses" => {
                self.now = self.retry.earliest.expect("inside a window");
            }
            "StaleElapses" => {
                self.now = self.fault_at.expect("a recent fault") + self.retry.healthy_for();
            }
            "HealthyWindowElapses" => {
                self.now =
                    self.retry.played_since.expect("a device playing") + self.retry.healthy_for();
            }
            other => panic!("no action {other}"),
        }
    }
}

/// Explore every real configuration the schedule can reach; check each step
/// against the model. `Ok(visited projections)` or the first refusal.
fn explore_schedule(schedule: &[Duration], mutation: Mutation) -> Result<BTreeSet<Key>, String> {
    let m = model_for(schedule);
    let actions: Vec<&str> = m.actions.iter().map(|a| a.name).collect();
    let start = Ladder {
        retry: RetryState::new(schedule),
        device: false,
        failed: false,
        now: Instant::now(),
        spent: 0,
        fault_at: None,
        outstanding: false,
        first_delivery: None,
        mutation,
    };
    let mut seen = BTreeSet::from([start.config_key()]);
    let mut visited = BTreeSet::from([key(&start.project())]);
    let mut queue = VecDeque::from([start]);
    while let Some(real) = queue.pop_front() {
        let pre = real.project();
        for &action in &actions {
            // The model's guard must be exactly the real environment's
            // possibility — a guard that disagrees with `ready` or the
            // healthy window is a guard the code does not have.
            if m.action_enabled(action, &pre) != real.possible(action) {
                return Err(format!(
                    "{action}: model enabled={} but the real ladder says {} at {pre:?}",
                    m.action_enabled(action, &pre),
                    real.possible(action)
                ));
            }
            if !real.possible(action) {
                continue;
            }
            let mut next = real.fork();
            next.step(action);
            let post = next.project();
            let admitted: BTreeSet<Key> = m.successors(action, &pre).iter().map(key).collect();
            if !time_closure(&m, &admitted).contains(&key(&post)) {
                return Err(format!(
                    "{action} from {pre:?}: the real ladder reached {post:?}, which the model \
                     does not admit"
                ));
            }
            for inv in &m.invariants {
                if !m.check_invariant(inv.name, &post) {
                    return Err(format!("{} violated at {post:?} after {action}", inv.name));
                }
            }
            visited.insert(key(&post));
            if seen.insert(next.config_key()) {
                queue.push_back(next);
            }
        }
    }
    Ok(visited)
}

/// THE SCHEDULE CONFORMS — at the shipping budget with the shipping
/// schedule, and at the committed model's two-step budget — and the run
/// reaches every state the model can.
#[test]
fn the_reopen_schedule_conforms_exhaustively() {
    // The schedule's SHAPE is a model premise, so pin it on the shipping
    // constant: the first reopen is immediate, every later one waits
    // strictly longer, so the last wait is the longest step — the window.
    assert_eq!(REOPEN_BACKOFF.len(), usize::from(REOPEN_BUDGET));
    assert!(REOPEN_BACKOFF[0].is_zero());
    assert!(REOPEN_BACKOFF.windows(2).all(|w| w[0] < w[1]));

    let two_step = [Duration::ZERO, Duration::from_millis(50)];
    for schedule in [&REOPEN_BACKOFF[..], &two_step[..]] {
        let visited = explore_schedule(schedule, Mutation::None)
            .unwrap_or_else(|e| panic!("schedule {schedule:?}: {e}"));
        let space = reachable(&model_for(schedule));
        let missing: Vec<_> = space.difference(&visited).collect();
        assert!(
            missing.is_empty(),
            "schedule {schedule:?}: model states the real ladder never reached: {missing:?}"
        );
        assert_eq!(visited, space, "and it reached nothing else");
    }
}

/// NEGATIVE CONTROLS for the schedule half: each defect is REJECTED.
#[test]
fn the_schedule_conformance_rejects_every_replayed_defect() {
    for mutation in [
        Mutation::TerminalOpenFault,
        Mutation::ResetOnOpen,
        Mutation::EagerReset,
        Mutation::ResetFromFault,
    ] {
        let err = explore_schedule(&REOPEN_BACKOFF, mutation)
            .expect_err("a replayed defect must be rejected");
        eprintln!("{mutation:?} rejected: {err}");
    }
    // A schedule whose FIRST reopen waits breaks the model's premise that a
    // transient fault costs the next key nothing.
    let slow_first = [Duration::from_millis(50), Duration::ZERO];
    assert!(explore_schedule(&slow_first, Mutation::None).is_err());
}

// ---------------------------------------------------------------------------
// HALF 2 — the call sites: a real `worker_loop`, a scripted output.
// ---------------------------------------------------------------------------

/// The shipping schedule's length, compressed to milliseconds so the ladder
/// is EXERCISED rather than slept through.
const FAST: [Duration; REOPEN_BUDGET as usize] = [
    Duration::ZERO,
    Duration::from_millis(1),
    Duration::from_millis(2),
    Duration::from_millis(4),
    Duration::from_millis(8),
    Duration::from_millis(16),
];

/// What the worker did at one platform call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Event {
    OpenOk,
    OpenFailed,
    /// A push, with the index of the cue it carried.
    PushSounded(usize),
    PushFaulted(usize),
    /// The housekeeping tick found the callback stalled or faulted
    /// (`Service::Reopen`) — the fault site between keys.
    TickFaulted,
}

/// The worker's own atomics, read at a platform call (so: the state after
/// everything the worker decided before it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Snapshot {
    reopens_left: u8,
    state: u8,
    busy: u64,
}

/// One trace record: the snapshot at the call, and the call's outcome.
type Record = (Snapshot, Event);

struct Probe {
    reopens_left: Arc<AtomicU8>,
    state: Arc<AtomicU8>,
    busy: Arc<AtomicU64>,
    trace: Mutex<Vec<Record>>,
    opens: AtomicUsize,
    pushes: AtomicUsize,
    open_ok: fn(usize) -> bool,
    /// `(push index over the run, push index on THIS device)`.
    push_ok: fn(usize, usize) -> bool,
    /// Whether a RUNNING device's housekeeping tick reports it stalled.
    tick_faults: bool,
}

impl Probe {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            reopens_left: self.reopens_left.load(Ordering::Acquire),
            state: self.state.load(Ordering::Acquire),
            busy: self.busy.load(Ordering::Acquire),
        }
    }

    fn record(&self, snap: Snapshot, event: Event) {
        self.trace.lock().expect("trace lock").push((snap, event));
    }

    fn trace_len(&self) -> usize {
        self.trace.lock().expect("trace lock").len()
    }
}

struct ScriptedOutput {
    probe: Arc<Probe>,
    running: bool,
    /// Pushes this device object has taken.
    pushes: usize,
}

impl AudioWorkerOutput for ScriptedOutput {
    fn push_meta(&mut self, ev: SoundEvent, _meta: EventMeta) -> Delivery {
        let snap = self.probe.snapshot();
        let cue = cue_index(ev);
        let n = self.probe.pushes.fetch_add(1, Ordering::AcqRel);
        let here = self.pushes;
        self.pushes += 1;
        if (self.probe.push_ok)(n, here) {
            self.probe.record(snap, Event::PushSounded(cue));
            self.running = true;
            Delivery::Sounded
        } else {
            self.probe.record(snap, Event::PushFaulted(cue));
            Delivery::Reopen
        }
    }

    fn on_tick(&mut self) -> Service {
        if self.running && self.probe.tick_faults {
            // THE STALL WATCHDOG'S VERDICT, taken at the production site
            // (`worker_loop`'s housekeeping arm): the device played and its
            // callback has since gone quiet.
            self.probe.record(self.probe.snapshot(), Event::TickFaulted);
            return Service::Reopen;
        }
        // Park at once: a parked worker takes the blocking `recv` arm, so
        // the trace is exactly the cue path.
        self.running = false;
        Service::Paused
    }

    fn is_running(&self) -> bool {
        self.running
    }
}

/// Cue `n`: its index rides in the hue, so a push record names the cue it
/// carried and the ONE uncharged re-push can be told from any other fault.
fn cue(n: usize) -> Cue {
    SoundEvent {
        style: GlowStyle::Water,
        voice: SoundVoice::Style,
        kind: SoundGesture::Trail(SoundKind::Typed),
        pan: 0.0,
        heat: 0.4,
        hue: n as f32,
        gain: 0.4,
        tone: aterm_effects::tone::Tone::Technical,
        bed: false,
        shifted: false,
    }
    .into()
}

fn cue_index(ev: SoundEvent) -> usize {
    ev.hue as usize
}

/// How the harness paces its cues.
#[derive(Clone, Copy, Debug)]
enum Pace {
    /// A fixed gap between cues.
    Gap(Duration),
    /// After each cue, wait (bounded) until the worker has either reported
    /// a TICK fault, dropped the cue, or failed — so every device is
    /// discovered dead by the housekeeping tick and never by a second push.
    UntilTickFault,
}

/// A scripted device: `(open succeeds?, push sounds?, a RUNNING device's
/// housekeeping tick reports it stalled?)`.
type Script = (fn(usize) -> bool, fn(usize, usize) -> bool, bool);

/// A finished run: the trace and the worker's final word.
struct Run {
    trace: Vec<Record>,
    last: Snapshot,
}

/// Drive a real `worker_loop` with a scripted output: `cues` cues paced by
/// `pace`, then (optionally) one more after `rest`. The run ENDS by closing
/// the channel and joining the worker, which drains every queued cue before
/// it sees the disconnect, so `last` is the worker's final state, not a
/// sample after a sleep.
fn run_worker(
    (open_ok, push_ok, tick_faults): Script,
    cues: usize,
    pace: Pace,
    rest: Option<Duration>,
) -> Run {
    let (tx, rx) = cue_channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(AtomicU8::new(STATE_DORMANT));
    let busy = Arc::new(AtomicU64::new(0));
    let dropped_backoff = Arc::new(AtomicU64::new(0));
    let reopens_left = Arc::new(AtomicU8::new(REOPEN_BUDGET));
    let probe = Arc::new(Probe {
        reopens_left: Arc::clone(&reopens_left),
        state: Arc::clone(&state),
        busy: Arc::clone(&busy),
        trace: Mutex::new(Vec::new()),
        opens: AtomicUsize::new(0),
        pushes: AtomicUsize::new(0),
        open_ok,
        push_ok,
        tick_faults,
    });
    let worker = {
        let (shutdown, state, busy, dropped_backoff, reopens_left, probe) = (
            Arc::clone(&shutdown),
            Arc::clone(&state),
            Arc::clone(&busy),
            Arc::clone(&dropped_backoff),
            Arc::clone(&reopens_left),
            Arc::clone(&probe),
        );
        std::thread::spawn(move || {
            worker_loop(
                rx,
                WorkerFlags {
                    shutdown: &shutdown,
                    state: &state,
                    busy: &busy,
                    dropped_backoff: &dropped_backoff,
                    reopens_left: &reopens_left,
                },
                7,
                Duration::from_millis(2),
                &FAST,
                move |_| {
                    let snap = probe.snapshot();
                    let n = probe.opens.fetch_add(1, Ordering::AcqRel);
                    if (probe.open_ok)(n) {
                        probe.record(snap, Event::OpenOk);
                        Some(ScriptedOutput {
                            probe: Arc::clone(&probe),
                            running: false,
                            pushes: 0,
                        })
                    } else {
                        probe.record(snap, Event::OpenFailed);
                        None
                    }
                },
            );
        })
    };
    let failed = || state.load(Ordering::Acquire) == STATE_FAILED;
    for n in 0..cues {
        if failed() {
            break;
        }
        match pace {
            Pace::Gap(gap) => {
                let _ = tx.send(cue(n));
                std::thread::sleep(gap);
            }
            Pace::UntilTickFault => {
                let traced = probe.trace_len();
                let dropped = dropped_backoff.load(Ordering::Acquire);
                let _ = tx.send(cue(n));
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let ticked = probe.trace.lock().expect("trace lock")[traced..]
                        .iter()
                        .any(|r| r.1 == Event::TickFaulted);
                    if ticked || failed() || dropped_backoff.load(Ordering::Acquire) != dropped {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "cue {n}: the worker neither ticked a fault, dropped the cue nor failed"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    if let Some(rest) = rest {
        std::thread::sleep(rest);
        let _ = tx.send(cue(cues));
    }
    drop(tx);
    worker.join().expect("worker thread");
    let trace = probe.trace.lock().expect("trace lock").clone();
    Run {
        trace,
        last: probe.snapshot(),
    }
}

/// Where the worker may make an UNCHARGED platform call: only inside the
/// one re-push that follows a charged push fault (`worker_loop`'s
/// `Delivery::Reopen` arm), and only for that same cue.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Repush {
    None,
    /// Directly after a charged push fault of cue `k`: the re-open may fail
    /// uncharged (the next cue's open carries that fault).
    Fresh(usize),
    /// The re-open of cue `k` succeeded: its push may fault uncharged.
    Opened(usize),
}

/// How liberal the validator is about uncharged faults.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Rules {
    /// What this conformance enforces: uncharged only inside the re-push.
    Exact,
    /// NEGATIVE CONTROL: the first draft's rule, any push fault may be
    /// uncharged — kept only to show what it let through.
    AnyPushUncharged,
}

/// Validate a worker trace against the model: there must be a model path —
/// the labelled actions in order, with unnarrated time and dropped cues
/// between them — whose states agree with every snapshot.
fn validate(m: &Model, trace: &[Record], last: Snapshot, rules: Rules) -> Result<(), String> {
    let budget = m.consts.iter().find(|c| c.0 == "Budget").expect("Budget").1;
    // Unnarrated steps: time, and a cue the backoff window or sealed ingress
    // swallowed (a `Cue` that leaves the state as it was).
    let settle = |frontier: &BTreeSet<Key>| -> BTreeSet<Key> {
        let mut seen = time_closure(m, frontier);
        loop {
            let mut grew = false;
            for k in seen.clone() {
                let s: State = k.clone().into_iter().collect();
                for n in m.successors("Cue", &s) {
                    if n == s && seen.insert(key(&n)) {
                        grew = true;
                    }
                }
            }
            let closed = time_closure(m, &seen);
            if closed.len() == seen.len() && !grew {
                return seen;
            }
            seen = closed;
        }
    };
    let agrees = |k: &Key, snap: Snapshot, device: Option<i64>| -> bool {
        let s: BTreeMap<&str, i64> = k.iter().cloned().collect();
        s["attempts"] == budget - i64::from(snap.reopens_left)
            && s["failed"] == i64::from(snap.state == STATE_FAILED)
            && device.is_none_or(|d| s["device"] == d)
    };
    let mut frontier: BTreeSet<(Key, Repush)> =
        BTreeSet::from([(key(&m.init_state()), Repush::None)]);
    for (i, &(snap, event)) in trace.iter().enumerate() {
        let device = match event {
            Event::OpenOk | Event::OpenFailed => 0,
            Event::PushSounded(_) | Event::PushFaulted(_) | Event::TickFaulted => 1,
        };
        let mut next = BTreeSet::new();
        let mut matched = false;
        for repush in frontier.iter().map(|f| f.1).collect::<BTreeSet<_>>() {
            let keys: BTreeSet<Key> = frontier
                .iter()
                .filter(|f| f.1 == repush)
                .map(|f| f.0.clone())
                .collect();
            for k in settle(&keys) {
                if !agrees(&k, snap, Some(device)) {
                    continue;
                }
                matched = true;
                let s: State = k.clone().into_iter().collect();
                // `(model action or "" for an uncharged call, repush after)`.
                let steps: Vec<(&str, Repush)> = match event {
                    Event::OpenOk => vec![(
                        "Cue",
                        match repush {
                            Repush::Fresh(c) => Repush::Opened(c),
                            _ => Repush::None,
                        },
                    )],
                    Event::OpenFailed => {
                        let mut v = vec![("DeviceFaults", Repush::None)];
                        if matches!(repush, Repush::Fresh(_)) {
                            v.push(("", Repush::None));
                        }
                        v
                    }
                    Event::PushSounded(_) => vec![("CueSounds", Repush::None)],
                    Event::PushFaulted(c) => {
                        let mut v = vec![("DeviceFaults", Repush::Fresh(c))];
                        if repush == Repush::Opened(c) || rules == Rules::AnyPushUncharged {
                            v.push(("", Repush::None));
                        }
                        v
                    }
                    // The housekeeping reopen re-pushes nothing.
                    Event::TickFaulted => vec![("DeviceFaults", Repush::None)],
                };
                for (action, after) in steps {
                    if action.is_empty() {
                        next.insert((k.clone(), after));
                        continue;
                    }
                    for n in m.successors(action, &s) {
                        if action == "Cue" && n["device"] != 1 {
                            continue;
                        }
                        next.insert((key(&n), after));
                    }
                }
            }
        }
        if !matched {
            return Err(format!(
                "record {i} ({event:?}): no model state agrees with {snap:?}; frontier was \
                 {frontier:?}"
            ));
        }
        if next.is_empty() {
            return Err(format!("record {i} ({event:?}): no model step"));
        }
        frontier = next;
    }
    let keys: BTreeSet<Key> = frontier.iter().map(|f| f.0.clone()).collect();
    if !settle(&keys).iter().any(|k| agrees(k, last, None)) {
        return Err(format!(
            "final {last:?} agrees with no state of {frontier:?}"
        ));
    }
    Ok(())
}

/// The wire word for a snapshot, through the real `host_state`.
fn wire(snap: Snapshot) -> HostState {
    let (audio, _rx) = TrailAudio::test_ingress();
    audio.state.store(snap.state, Ordering::Release);
    audio.host_state()
}

/// The checks every real trace must pass beyond the model path.
fn assert_trace_facts(label: &str, run: &Run) {
    for &(snap, event) in &run.trace {
        let base = snap.busy & !BUSY_OPENING;
        match event {
            // A device open is stamped AS an open, so a slow one is not a
            // wedge until OPEN_WEDGE_AFTER_MS…
            Event::OpenOk | Event::OpenFailed => {
                assert_ne!(snap.busy & BUSY_OPENING, 0, "{label}: open unstamped");
                assert!(busy_stale(snap.busy, base + WEDGE_AFTER_MS).is_none());
                assert!(busy_stale(snap.busy, base + OPEN_WEDGE_AFTER_MS).is_some());
            }
            // …while a steady-state push, or the housekeeping tick, is judged
            // at WEDGE_AFTER_MS.
            Event::PushSounded(_) | Event::PushFaulted(_) | Event::TickFaulted => {
                assert_ne!(snap.busy, 0, "{label}: platform call unstamped");
                assert_eq!(snap.busy & BUSY_OPENING, 0, "{label}: stamped as open");
                assert!(busy_stale(snap.busy, base + WEDGE_AFTER_MS).is_some());
            }
        }
    }
    // The wire never says `failed` while the budget has reopens left — the
    // pre-D5 reading this ladder retired.
    for snap in run.trace.iter().map(|r| r.0).chain([run.last]) {
        if wire(snap) == HostState::Failed {
            assert_eq!(snap.reopens_left, 0, "{label}: failed with budget left");
        }
    }
}

fn open_always(_: usize) -> bool {
    true
}
fn open_never(_: usize) -> bool {
    false
}
fn open_after_two(n: usize) -> bool {
    n >= 2
}
/// Only the first device ever opens: the re-open after its first fault
/// fails, inside the re-push.
fn open_first_only(n: usize) -> bool {
    n == 0
}
fn push_always(_: usize, _: usize) -> bool {
    true
}
fn push_never(_: usize, _: usize) -> bool {
    false
}
/// The second push of the run fails once; every other push sounds.
fn push_stalls_once(n: usize, _: usize) -> bool {
    n != 1
}
/// Every device plays exactly ONE block and then stalls: the shape the
/// healthy window exists to see through.
fn push_plays_once(_: usize, here: usize) -> bool {
    here == 0
}

/// `(label, script, cues, pace, rest before one last cue)`.
type Scenario = (&'static str, Script, usize, Pace, Option<Duration>);

/// THE CALL SITES CONFORM: eight scripted devices through the real
/// `worker_loop`, each trace a path of the model — including a device
/// discovered dead by the housekeeping TICK (the production path for a
/// stall between keys) and a re-open that fails inside the re-push.
#[test]
fn the_worker_loop_traces_conform_to_the_reopen_ladder() {
    let m = model_for(&FAST);
    let gap = Pace::Gap(Duration::from_millis(3));
    let scenarios: [Scenario; 8] = [
        ("healthy", (open_always, push_always, false), 6, gap, None),
        (
            "open-never-succeeds",
            (open_never, push_always, false),
            60,
            gap,
            None,
        ),
        (
            "opens-but-never-plays",
            (open_always, push_never, false),
            60,
            gap,
            None,
        ),
        (
            "stalls-once-then-heals",
            (open_always, push_stalls_once, false),
            4,
            gap,
            Some(Duration::from_millis(60)),
        ),
        (
            "two-failed-opens-then-plays",
            (open_after_two, push_always, false),
            8,
            gap,
            None,
        ),
        (
            "plays-once-then-stalls",
            (open_always, push_plays_once, false),
            120,
            gap,
            None,
        ),
        (
            "reopen-fails-inside-the-repush",
            (open_first_only, push_never, false),
            60,
            gap,
            None,
        ),
        (
            "plays-once-then-stalls-by-tick",
            (open_always, push_always, true),
            400,
            Pace::UntilTickFault,
            None,
        ),
    ];
    for (label, script, cues, pace, rest) in scenarios {
        let run = run_worker(script, cues, pace, rest);
        assert!(
            !run.trace.is_empty(),
            "{label}: the worker made no platform call"
        );
        validate(&m, &run.trace, run.last, Rules::Exact)
            .unwrap_or_else(|e| panic!("{label}: {e}\ntrace {:?}", run.trace));
        assert_trace_facts(label, &run);
        match label {
            "open-never-succeeds"
            | "opens-but-never-plays"
            | "plays-once-then-stalls"
            | "reopen-fails-inside-the-repush"
            | "plays-once-then-stalls-by-tick" => {
                assert_eq!(run.last.state, STATE_FAILED, "{label}: the budget is spent");
                assert_eq!(run.last.reopens_left, 0);
            }
            "stalls-once-then-heals" => {
                assert_eq!(
                    run.last.reopens_left, REOPEN_BUDGET,
                    "{label}: a device that played through the healthy window has the budget back"
                );
            }
            _ => assert_ne!(run.last.state, STATE_FAILED, "{label}"),
        }
        // The two paths these scenarios exist for were really taken.
        match label {
            "plays-once-then-stalls-by-tick" => {
                let ticks = run
                    .trace
                    .iter()
                    .filter(|r| r.1 == Event::TickFaulted)
                    .count();
                assert_eq!(
                    ticks,
                    usize::from(REOPEN_BUDGET) + 1,
                    "{label}: every device was found dead by the tick"
                );
            }
            "reopen-fails-inside-the-repush" => assert!(
                run.trace.windows(2).any(|w| {
                    matches!(w[0].1, Event::PushFaulted(_)) && w[1].1 == Event::OpenFailed
                }),
                "{label}: the re-open inside the re-push failed"
            ),
            _ => {}
        }
    }
}

/// The stall watchdog's boundary, exactly: silent for `STALL_AFTER_MS`
/// since an armed callback is a stall, and one microsecond less is not.
/// (A pure-function pin; the watchdog's CALL SITE is the housekeeping
/// tick, which `plays-once-then-stalls-by-tick` drives.)
#[test]
fn the_stall_watchdog_boundary_is_exact() {
    let armed = 1_000_000u64;
    assert!(!callback_stalled(
        true,
        armed,
        armed + STALL_AFTER_MS * 1_000 - 1
    ));
    assert!(callback_stalled(
        true,
        armed,
        armed + STALL_AFTER_MS * 1_000
    ));
    assert!(!callback_stalled(
        false,
        armed,
        armed + STALL_AFTER_MS * 10_000
    ));
}

/// NEGATIVE CONTROLS for the call-site half: doctored traces of the
/// historical defects are REJECTED by the validator that accepts the real
/// ones — and so is a worker that stops charging its faults at all, which
/// the first draft of this validator let through.
#[test]
fn the_trace_validator_rejects_the_replayed_defects() {
    let m = model_for(&FAST);
    let gap = Pace::Gap(Duration::from_millis(3));
    let real = run_worker((open_always, push_never, false), 60, gap, None);
    validate(&m, &real.trace, real.last, Rules::Exact).expect("the real trace conforms");

    // THE FIRST DRAFT OF D5: a successful open reset the ladder, so the
    // snapshot at every push after a reopen showed a full budget.
    let mut reset_on_open = real.trace.clone();
    let mut after_open = false;
    for (snap, event) in &mut reset_on_open {
        if after_open {
            snap.reopens_left = REOPEN_BUDGET;
        }
        after_open = *event == Event::OpenOk;
    }
    assert_ne!(reset_on_open, real.trace, "the doctoring changed something");
    assert!(validate(&m, &reset_on_open, real.last, Rules::Exact).is_err());

    // THE PRE-D5 LAW: one failed open, and the worker is failed with the
    // whole budget unspent.
    let dormant = Snapshot {
        reopens_left: REOPEN_BUDGET,
        state: STATE_DORMANT,
        busy: BUSY_OPENING | 1,
    };
    let terminal = Snapshot {
        state: STATE_FAILED,
        ..dormant
    };
    assert!(validate(&m, &[(dormant, Event::OpenFailed)], terminal, Rules::Exact).is_err());
    // …and the same trace with the budget honestly spent by one is accepted
    // as far as it goes, so the rejection above is about the law, not the
    // shape of the trace.
    let reopening = Snapshot {
        reopens_left: REOPEN_BUDGET - 1,
        state: super::STATE_REOPENING,
        ..dormant
    };
    validate(&m, &[(dormant, Event::OpenFailed)], reopening, Rules::Exact)
        .expect("one reopen spent");

    // A WORKER THAT STOPS CHARGING PUSH FAULTS (it keeps the dead device and
    // drops cues): one open, then every push faults with the budget full.
    // The first draft's rule — any push fault may be uncharged — accepts
    // it; the exact rule refuses it at the first fault.
    let pushing = Snapshot {
        reopens_left: REOPEN_BUDGET,
        state: super::STATE_RUNNING,
        busy: 1,
    };
    let mut swallowed = vec![(dormant, Event::OpenOk)];
    swallowed.extend((0..8).map(|c| (pushing, Event::PushFaulted(c))));
    assert!(
        validate(&m, &swallowed, pushing, Rules::AnyPushUncharged).is_ok(),
        "the lenient rule really did let it through"
    );
    let err = validate(&m, &swallowed, pushing, Rules::Exact)
        .expect_err("an uncharged fault outside the re-push must be rejected");
    eprintln!("swallowed push faults rejected: {err}");

    // …and a TICK fault that is not charged is refused the same way.
    let by_tick = run_worker(
        (open_always, push_always, true),
        400,
        Pace::UntilTickFault,
        None,
    );
    validate(&m, &by_tick.trace, by_tick.last, Rules::Exact).expect("the real tick trace");
    let mut uncharged_tick = by_tick.trace.clone();
    let mut after_tick = false;
    for (snap, event) in &mut uncharged_tick {
        if after_tick {
            snap.reopens_left = REOPEN_BUDGET;
        }
        after_tick |= *event == Event::TickFaulted;
    }
    assert_ne!(
        uncharged_tick, by_tick.trace,
        "the doctoring changed something"
    );
    assert!(validate(&m, &uncharged_tick, by_tick.last, Rules::Exact).is_err());
}
