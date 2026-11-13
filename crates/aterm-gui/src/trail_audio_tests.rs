// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::{EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice};

use super::mac::{
    BLOCK_S, Delivery, QueueCycle, Service, block_lead_s, callback_stalled, prime_and_start,
    stop_and_reclaim,
};
use super::{
    AudioWorkerOutput, CLOSED_BEFORE_WAIT, COMMAND_CAPACITY, Cue, DETACH_DEADLINE_OVERRIDE,
    HostState, LAST_TEARDOWN, ParkedMark, REOPEN_BUDGET, STATE_DORMANT, STATE_FAILED, STATE_PAUSED,
    STATE_REOPENING, STATE_RUNNING, STATE_STOPPED, TrailAudio, WEDGE_REVIVES, WorkerFlags,
    WorkerTeardown, cue_channel, monotonic_ms, worker_loop,
};

#[test]
fn a_dormant_host_is_not_live_on_the_wire() {
    let (mut audio, _rx) = TrailAudio::test_ingress();
    assert_eq!(audio.host_state(), HostState::Opening);
    assert_eq!(wire(audio.host_state()), "opening");

    for (word, want) in [
        (STATE_RUNNING, HostState::Running),
        (STATE_PAUSED, HostState::Paused),
        (STATE_FAILED, HostState::Failed),
        (STATE_STOPPED, HostState::Stopped),
        (STATE_DORMANT, HostState::Opening),
    ] {
        audio.state.store(word, Ordering::Release);
        assert_eq!(audio.host_state(), want, "state word {word}");
    }

    audio.state.store(STATE_RUNNING, Ordering::Release);
    audio.busy.store(1, Ordering::Release);
    assert_eq!(audio.host_state(), HostState::Wedged);

    audio.tx = None;
    assert_eq!(audio.host_state(), HostState::Inert);
    assert_eq!(wire(audio.host_state()), "inert");
}

fn wire(s: HostState) -> &'static str {
    s.label()
}

#[test]
fn every_host_state_has_a_wire_word() {
    let mut seen = std::collections::BTreeSet::new();
    for s in HostState::ALL {
        let w = wire(s);
        assert!(!w.is_empty(), "{s:?}");
        assert!(seen.insert(w), "{w} is spelled twice");
    }
    assert_eq!(seen.len(), HostState::ALL.len());
    assert_eq!(wire(HostState::Running), "live");
    for s in [HostState::Inert, HostState::Opening, HostState::Failed] {
        assert_ne!(wire(s), "live", "{s:?}");
    }
}

fn cue() -> SoundEvent {
    SoundEvent {
        style: GlowStyle::Water,
        voice: SoundVoice::Style,
        kind: SoundGesture::Trail(SoundKind::Typed),
        pan: 0.0,
        heat: 0.4,
        hue: 0.0,
        gain: 0.4,
        tone: aterm_effects::tone::Tone::Technical,
        bed: true,
        shifted: false,
    }
}

fn wait_until(label: &str, pred: impl Fn() -> bool) {
    let seen = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(1),
        || pred().then_some(()),
    );
    assert!(seen.is_some(), "timed out: {label}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeBuffer {
    Available,
    Rendered { audible: bool },
    Queued { audible: bool },
}

struct CallbackFreeQueue {
    buffers: [FakeBuffer; super::BUFFER_COUNT],
    cue_audible: bool,
    recycling: bool,
    running: bool,
    unsafe_writes: usize,
    operations: usize,
    recycle_epoch: u64,
    callback_epoch: u64,
    stale_enqueues: usize,
    enqueue_in_flight: bool,
    stop_overlaps: usize,
    /// The fake's µs clock, advanced by the test; what a stamp records.
    clock_us: u64,
    /// The block admission stamp — the fake's `Shared::block_start_us`.
    block_start_us: u64,
    /// Stamps taken by `prime_and_start` (one per prime is the law).
    block_stamps: usize,
    /// Set by a stamp, cleared by a stop: a `render_post_cue` that runs
    /// while it is clear is a prime rendering against a stale stamp.
    stamped_since_stop: bool,
    renders_before_stamp: usize,
}

impl CallbackFreeQueue {
    fn new() -> Self {
        Self {
            buffers: [FakeBuffer::Available; super::BUFFER_COUNT],
            cue_audible: false,
            recycling: false,
            running: false,
            unsafe_writes: 0,
            operations: 0,
            recycle_epoch: 0,
            callback_epoch: 0,
            stale_enqueues: 0,
            enqueue_in_flight: false,
            stop_overlaps: 0,
            clock_us: 1_000_000,
            block_start_us: 0,
            block_stamps: 0,
            stamped_since_stop: false,
            renders_before_stamp: 0,
        }
    }

    fn callback_renders_block(&mut self) {
        assert!(self.running && self.recycling);
        self.block_start_us = self.clock_us;
    }

    fn lead_now_s(&self) -> f32 {
        block_lead_s(self.running, self.block_start_us, self.clock_us)
    }

    fn apply_audible_cue(&mut self) {
        self.cue_audible = true;
    }

    fn available(&self) -> usize {
        self.buffers
            .iter()
            .filter(|buffer| matches!(buffer, FakeBuffer::Available))
            .count()
    }

    fn queued(&self) -> usize {
        self.buffers
            .iter()
            .filter(|buffer| matches!(buffer, FakeBuffer::Queued { .. }))
            .count()
    }

    fn callback_enqueue_begins(&mut self) {
        self.operations += 1;
        assert!(self.recycling);
        assert!(!self.enqueue_in_flight);
        self.enqueue_in_flight = true;
    }

    fn callback_enqueue_ends(&mut self) {
        self.operations += 1;
        assert!(self.enqueue_in_flight);
        self.enqueue_in_flight = false;
    }

    fn stop_without_gate_for_negative_control(&mut self) {
        self.operations += 1;
        if self.enqueue_in_flight {
            self.stop_overlaps += 1;
        }
        self.running = false;
        self.recycling = false;
        self.enqueue_in_flight = false;
        self.stamped_since_stop = false;
    }

    fn old_callback_returns(&mut self) {
        if self.callback_epoch == self.recycle_epoch {
            self.stale_enqueues += 1;
        }
    }
}

impl QueueCycle for CallbackFreeQueue {
    fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    fn stamp_block_start(&mut self) {
        self.operations += 1;
        self.block_start_us = self.clock_us;
        self.block_stamps += 1;
        self.stamped_since_stop = true;
    }

    fn render_post_cue(&mut self, index: usize) -> bool {
        self.operations += 1;
        if !self.stamped_since_stop {
            self.renders_before_stamp += 1;
        }
        if self.buffers[index] != FakeBuffer::Available {
            self.unsafe_writes += 1;
            return false;
        }
        let audible = self.cue_audible;
        self.buffers[index] = FakeBuffer::Rendered { audible };
        audible
    }

    fn enqueue_buffer(&mut self, index: usize) -> bool {
        self.operations += 1;
        let FakeBuffer::Rendered { audible } = self.buffers[index] else {
            return false;
        };
        self.buffers[index] = FakeBuffer::Queued { audible };
        true
    }

    fn set_callback_recycling(&mut self, enabled: bool) {
        self.operations += 1;
        if self.recycling != enabled {
            self.recycle_epoch = self.recycle_epoch.wrapping_add(1);
        }
        self.recycling = enabled;
        if enabled && self.callback_epoch == 0 {
            self.callback_epoch = self.recycle_epoch;
        }
    }

    fn start_queue(&mut self) -> bool {
        self.operations += 1;
        if self.queued() != self.buffers.len() || !self.recycling {
            return false;
        }
        self.running = true;
        true
    }

    fn stop_immediate(&mut self) -> bool {
        self.operations += 1;
        if self.recycling || self.enqueue_in_flight {
            return false;
        }
        self.buffers.fill(FakeBuffer::Available);
        self.running = false;
        self.cue_audible = false;
        self.stamped_since_stop = false;
        true
    }
}

#[test]
fn audio_queue_post_cue_prime_conforms_with_callback_free_fake() {
    use super::trail_audio_conformance::project_start_latency;

    let model = aterm_models_misc::trail_audio_start_latency_model();
    let mut state = model.init_state();
    let mut queue = CallbackFreeQueue::new();
    let project = |phase: i64, audible_buffer: usize, queue: &CallbackFreeQueue| {
        project_start_latency(
            phase,
            queue.available(),
            queue.queued(),
            queue.recycling,
            queue.running,
            audible_buffer,
            queue.unsafe_writes,
            queue.recycle_epoch,
            queue.callback_epoch,
            queue.stale_enqueues,
            queue.enqueue_in_flight,
            queue.stop_overlaps,
        )
    };
    assert_eq!(state, project(0, 0, &queue));

    queue.apply_audible_cue();
    assert!(model.fire("CueCold", &mut state));
    assert_eq!(state, project(1, 0, &queue));
    let cold = prime_and_start(&mut queue).expect("cold prime/start");
    assert_eq!(cold.first_audible_buffer, Some(1));
    assert_eq!(queue.block_stamps, 1, "a cold prime stamps the block start");
    assert_eq!(
        queue.renders_before_stamp, 0,
        "and stamps it BEFORE it renders"
    );
    assert!(model.fire("PrimeCold", &mut state));
    assert!(model.fire("StartCold", &mut state));
    assert_eq!(state, project(3, 1, &queue));

    queue.callback_enqueue_begins();
    assert!(model.fire("CallbackEnqueueBegins", &mut state));
    assert_eq!(state, project(3, 1, &queue));
    assert!(!model.action_enabled("StopIdle", &state));
    queue.callback_enqueue_ends();
    assert!(model.fire("CallbackEnqueueEnds", &mut state));
    assert_eq!(state, project(3, 1, &queue));

    assert!(stop_and_reclaim(&mut queue));
    assert!(model.fire("StopIdle", &mut state));
    assert_eq!(state, project(4, 0, &queue));
    let operations_at_idle = queue.operations;
    assert!(model.fire("ParkIdle", &mut state));
    assert_eq!(queue.operations, operations_at_idle, "idle has zero work");
    assert_eq!(state, project(4, 0, &queue));

    queue.apply_audible_cue();
    assert!(model.fire("CueResume", &mut state));
    assert_eq!(state, project(5, 0, &queue));
    let resumed = prime_and_start(&mut queue).expect("resume prime/start");
    assert_eq!(resumed.first_audible_buffer, Some(1));
    assert_eq!(
        queue.block_stamps, 2,
        "a resume prime stamps the block start again"
    );
    assert_eq!(
        queue.renders_before_stamp, 0,
        "and never renders against the parked stamp"
    );
    assert!(model.fire("PrimeResume", &mut state));
    assert!(model.fire("StartResume", &mut state));
    assert_eq!(state, project(7, 1, &queue));
    queue.old_callback_returns();
    assert!(model.fire("OldCallbackReturns", &mut state));
    assert_eq!(state, project(8, 1, &queue));
    assert_eq!(queue.unsafe_writes, 0);
    assert_eq!(queue.stale_enqueues, 0);
    for invariant in [
        "BufferOwnershipConserved",
        "AudibleWithinOneBuffer",
        "WritesRequireAvailableOwnership",
        "StaleCallbackCannotReenqueue",
        "StopNeverOverlapsEnqueue",
    ] {
        assert!(model.check_invariant(invariant, &state), "{invariant}");
    }

    let mut retained = CallbackFreeQueue::new();
    for index in 0..retained.buffer_count() {
        assert!(!retained.render_post_cue(index));
        assert!(retained.enqueue_buffer(index));
    }
    retained.set_callback_recycling(true);
    assert!(retained.start_queue());
    retained.apply_audible_cue();
    assert_eq!(retained.queued() + 1, 4);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut bad = buggy.init_state();
    for action in ["CueCold", "PrimeCold"] {
        assert!(buggy.fire(action, &mut bad));
    }
    assert_eq!(bad["audible_buffer"], retained.queued() as i64 + 1);
    assert!(!buggy.check_invariant("AudibleWithinOneBuffer", &bad));

    assert!(buggy.fire("StartCold", &mut bad));
    retained.callback_enqueue_begins();
    assert!(buggy.fire("CallbackEnqueueBegins", &mut bad));
    retained.stop_without_gate_for_negative_control();
    assert!(buggy.fire("StopIdle", &mut bad));
    assert_eq!(retained.stop_overlaps, 1);
    assert!(!buggy.check_invariant("StopNeverOverlapsEnqueue", &bad));
    let mut refused = CallbackFreeQueue::new();
    refused.apply_audible_cue();
    prime_and_start(&mut refused).expect("prime/start before the refused stop");
    refused.callback_enqueue_begins();
    assert!(!stop_and_reclaim(&mut refused));
    assert_eq!((refused.available(), refused.queued()), (0, 3));
    assert_eq!(
        (bad["available"], bad["queued"]),
        (3, refused.queued() as i64)
    );
    assert!(!buggy.check_invariant("BufferOwnershipConserved", &bad));
    retained.apply_audible_cue();
    assert!(buggy.fire("CueResume", &mut bad));
    assert!(!retained.render_post_cue(0));
    assert_eq!(retained.unsafe_writes, 1);
    assert!(buggy.fire("PrimeResume", &mut bad));
    assert!(!buggy.check_invariant("WritesRequireAvailableOwnership", &bad));
    assert!(buggy.fire("StartResume", &mut bad));
    retained.old_callback_returns();
    assert_eq!(retained.stale_enqueues, 1);
    assert!(buggy.fire("OldCallbackReturns", &mut bad));
    assert!(!buggy.check_invariant("StaleCallbackCannotReenqueue", &bad));
}

#[test]
fn a_resume_prime_restamps_the_block_so_the_next_cues_pre_roll_is_its_true_offset() {
    const MS: u64 = 1_000;
    let mut queue = CallbackFreeQueue::new();

    assert_eq!(queue.lead_now_s(), 0.0);

    queue.apply_audible_cue();
    assert_eq!(
        queue.lead_now_s(),
        0.0,
        "the cue that wakes the queue has no pre-roll"
    );
    prime_and_start(&mut queue).expect("cold prime/start");
    assert_eq!(
        queue.block_start_us,
        1_000 * MS,
        "the cold prime stamped its own instant"
    );
    queue.clock_us += 4 * MS;
    assert!(
        (queue.lead_now_s() - 0.004).abs() < 1e-6,
        "4 ms into the primed block"
    );

    queue.clock_us += 500 * MS;
    queue.callback_renders_block();
    let last_callback_us = queue.clock_us;
    queue.clock_us += 7 * MS;
    assert!((queue.lead_now_s() - 0.007).abs() < 1e-6);
    queue.clock_us += 20 * MS;
    assert_eq!(
        queue.lead_now_s(),
        BLOCK_S,
        "a late callback clamps to one block"
    );

    assert!(stop_and_reclaim(&mut queue));
    queue.clock_us += 5_000 * MS;
    assert_eq!(
        queue.lead_now_s(),
        0.0,
        "a parked queue hands out no pre-roll"
    );
    assert_eq!(
        queue.block_start_us, last_callback_us,
        "the parked stamp is seconds old"
    );

    queue.apply_audible_cue();
    let resume_us = queue.clock_us;
    prime_and_start(&mut queue).expect("resume prime/start");
    assert_eq!(
        queue.block_start_us, resume_us,
        "the resume prime re-stamped"
    );
    queue.clock_us += 3 * MS;
    let key2 = queue.lead_now_s();
    assert!(
        (key2 - 0.003).abs() < 1e-6,
        "key 2 measures 3 ms from the prime, got {key2}"
    );

    let retired = block_lead_s(true, last_callback_us, queue.clock_us);
    assert_eq!(
        retired, BLOCK_S,
        "the retired prime clamped key 2 to a full block"
    );
    assert!(
        retired - key2 > 0.007,
        "the asymmetry this pin keeps out: {retired} vs {key2}"
    );

    queue.clock_us += 8 * MS;
    queue.callback_renders_block();
    queue.clock_us += 2 * MS;
    assert!((queue.lead_now_s() - 0.002).abs() < 1e-6);
}

#[derive(Default)]
struct FakeShared {
    opens: AtomicUsize,
    pushes: AtomicUsize,
    ticks: AtomicUsize,
    pause_on_tick: AtomicBool,
    fail_open: AtomicBool,
    fail_push: AtomicBool,
    /// While set, `push` spins in place — the deterministic stand-in for a
    /// platform call that has stopped returning (the wedge).
    block_push: AtomicBool,
    /// The last cue's `at_ms`, so a test can prove the side-car arrived.
    last_at_ms: std::sync::atomic::AtomicU32,
    /// One-shot: the next `push_meta` reports `Reopen` without consuming
    /// the cue, standing in for a stalled or faulted queue.
    stall_push_once: AtomicBool,
    /// One-shot: the next `on_tick` reports `Reopen`.
    stall_tick_once: AtomicBool,
}

struct FakeOutput {
    shared: Arc<FakeShared>,
    running: bool,
}

impl AudioWorkerOutput for FakeOutput {
    fn push_meta(&mut self, _ev: SoundEvent, meta: EventMeta) -> Delivery {
        self.shared.last_at_ms.store(meta.at_ms, Ordering::Release);
        self.shared.pushes.fetch_add(1, Ordering::Release);
        while self.shared.block_push.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if self.shared.stall_push_once.swap(false, Ordering::AcqRel) {
            return Delivery::Reopen;
        }
        if self.shared.fail_push.load(Ordering::Relaxed) {
            return Delivery::Reopen;
        }
        self.running = true;
        Delivery::Sounded
    }

    fn on_tick(&mut self) -> Service {
        self.shared.ticks.fetch_add(1, Ordering::Relaxed);
        if self.shared.stall_tick_once.swap(false, Ordering::AcqRel) {
            return Service::Reopen;
        }
        if self.shared.pause_on_tick.load(Ordering::Relaxed) {
            self.running = false;
        }
        if self.running {
            Service::Running
        } else {
            Service::Paused
        }
    }

    fn is_running(&self) -> bool {
        self.running
    }
}

type FakeWorkerHandles = (
    std::sync::mpsc::SyncSender<Cue>,
    Arc<AtomicBool>,
    Arc<AtomicU8>,
    Arc<std::sync::atomic::AtomicU64>,
    std::thread::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicU64>,
    Arc<AtomicU8>,
);

const FAKE_BACKOFF: [std::time::Duration; REOPEN_BUDGET as usize] = [
    std::time::Duration::ZERO,
    std::time::Duration::from_millis(1),
    std::time::Duration::from_millis(2),
    std::time::Duration::from_millis(4),
    std::time::Duration::from_millis(8),
    std::time::Duration::from_millis(16),
];

fn spawn_fake_worker(shared: Arc<FakeShared>) -> FakeWorkerHandles {
    let (tx, rx) = cue_channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(AtomicU8::new(STATE_DORMANT));
    let busy = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let dropped_backoff = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let reopens_left = Arc::new(AtomicU8::new(REOPEN_BUDGET));
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_state = Arc::clone(&state);
    let worker_busy = Arc::clone(&busy);
    let worker_dropped_backoff = Arc::clone(&dropped_backoff);
    let worker_reopens_left = Arc::clone(&reopens_left);
    let worker = std::thread::spawn(move || {
        let factory_shared = Arc::clone(&shared);
        worker_loop(
            rx,
            WorkerFlags {
                shutdown: &worker_shutdown,
                state: &worker_state,
                busy: &worker_busy,
                dropped_backoff: &worker_dropped_backoff,
                reopens_left: &worker_reopens_left,
            },
            7,
            std::time::Duration::from_millis(2),
            &FAKE_BACKOFF,
            move |_| {
                factory_shared.opens.fetch_add(1, Ordering::Relaxed);
                if factory_shared.fail_open.load(Ordering::Relaxed) {
                    None
                } else {
                    Some(FakeOutput {
                        shared: Arc::clone(&factory_shared),
                        running: false,
                    })
                }
            },
        );
    });
    (
        tx,
        shutdown,
        state,
        busy,
        worker,
        dropped_backoff,
        reopens_left,
    )
}

#[test]
fn shipping_ingress_conforms_to_bounded_drop_newest_model() {
    use std::sync::atomic::Ordering;

    use super::trail_audio_conformance::project_ingress;

    let model = aterm_models_misc::trail_audio_lifecycle_model();
    let (mut audio, rx) = TrailAudio::test_ingress();
    let mut abstract_state = project_ingress(0, 0, false);
    for abstract_queued in 1..=2 {
        assert!(model.action_enabled("PushCueAvailable", &abstract_state));
        audio.push(cue());
        let next = project_ingress(abstract_queued, 0, false);
        assert!(
            model
                .successors("PushCueAvailable", &abstract_state)
                .contains(&next),
            "real available enqueue must be admitted by the model"
        );
        abstract_state = next;
    }
    for _ in 2..COMMAND_CAPACITY {
        audio.push(cue());
    }
    assert_eq!(audio.dropped_full.load(Ordering::Relaxed), 0);
    assert!(model.action_enabled("PushCueFull", &abstract_state));
    audio.push(cue());
    let after_full = project_ingress(2, 1, true);
    assert!(
        model
            .successors("PushCueFull", &abstract_state)
            .contains(&after_full),
        "real full enqueue must preserve the queue and account the dropped newest cue"
    );
    assert_eq!(rx.try_iter().count(), COMMAND_CAPACITY);

    let silent_loss = project_ingress(2, 0, true);
    assert!(
        !model
            .successors("PushCueFull", &abstract_state)
            .contains(&silent_loss)
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mutant = buggy
        .successors("PushCueFull", &abstract_state)
        .into_iter()
        .next()
        .expect("mutant full transition");
    assert!(
        buggy
            .invariants
            .iter()
            .any(|invariant| { !buggy.check_invariant(invariant.name, &mutant) })
    );
}

#[test]
fn drop_counter_saturates_instead_of_wrapping() {
    use std::sync::atomic::Ordering;

    let (mut audio, _rx) = TrailAudio::test_ingress();
    for _ in 0..COMMAND_CAPACITY {
        audio.push(cue());
    }
    audio.dropped_full.store(u64::MAX, Ordering::Relaxed);
    audio.push(cue());
    assert_eq!(audio.dropped_full.load(Ordering::Relaxed), u64::MAX);
}

#[test]
fn disconnected_ingress_fails_closed_once() {
    let (mut audio, rx) = TrailAudio::test_ingress();
    drop(rx);
    audio.push(cue());
    assert!(audio.tx.is_none());
    assert_eq!(audio.state(), STATE_FAILED);
    audio.push(cue());
    assert!(audio.tx.is_none());
}

#[test]
fn worker_lifecycle_conforms_and_parks_without_polling() {
    use super::trail_audio_conformance::project_worker;

    let shared = Arc::new(FakeShared::default());
    let (tx, shutdown, state, _busy, worker, _dropped_backoff, _reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));
    wait_until("initial park", || ParkedMark::holds(&state));
    assert_eq!(shared.opens.load(Ordering::Relaxed), 0);
    assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
    assert_eq!(shared.ticks.load(Ordering::Relaxed), 0);

    tx.send(cue().into()).unwrap();
    wait_until("worker start", || {
        state.load(Ordering::Acquire) == STATE_RUNNING && shared.pushes.load(Ordering::Relaxed) == 1
    });
    assert_eq!(shared.opens.load(Ordering::Relaxed), 1);

    let model = aterm_models_misc::trail_audio_lifecycle_model();
    let before_start = project_worker(1, 0, false, false, 0, false, false, false, false);
    let after_start = project_worker(0, 0, false, true, 0, true, true, false, false);
    assert!(
        model
            .successors("WorkerStart", &before_start)
            .contains(&after_start)
    );
    let lost_deadline = project_worker(0, 0, false, true, 0, false, true, false, false);
    assert!(
        !model
            .successors("WorkerStart", &before_start)
            .contains(&lost_deadline),
        "negative control: running without its service deadline is rejected"
    );

    shared.pause_on_tick.store(true, Ordering::Release);
    wait_until("idle pause", || {
        state.load(Ordering::Acquire) == STATE_PAUSED
    });
    let ticks_at_pause = shared.ticks.load(Ordering::Relaxed);
    let running_deadline = ticks_at_pause > 0;
    assert!(running_deadline, "the running worker services a deadline");
    wait_until("paused park", || ParkedMark::holds(&state));
    let paused_deadline = shared.ticks.load(Ordering::Relaxed) != ticks_at_pause;
    assert!(
        !paused_deadline,
        "paused worker blocks on recv instead of polling a timeout"
    );
    let before_pause = project_worker(0, 0, false, true, 2, running_deadline, false, false, false);
    let after_pause = project_worker(0, 0, false, false, 2, paused_deadline, false, false, true);
    assert!(
        model
            .successors("PauseIdle", &before_pause)
            .contains(&after_pause)
    );
    assert!(
        model
            .successors("ParkIdle", &after_pause)
            .contains(&after_pause)
    );
    let polling_pause = project_worker(0, 0, false, false, 2, running_deadline, false, false, true);
    assert!(
        !model
            .successors("PauseIdle", &before_pause)
            .contains(&polling_pause)
    );
    assert!(
        aterm_spec::interp::with_buggy(&model, 1)
            .successors("PauseIdle", &before_pause)
            .contains(&polling_pause)
    );
    assert!(!model.check_invariant("RunningOwnsOneDeadline", &polling_pause));

    shared.pause_on_tick.store(false, Ordering::Release);
    tx.send(cue().into()).unwrap();
    wait_until("worker resume", || {
        state.load(Ordering::Acquire) == STATE_RUNNING && shared.pushes.load(Ordering::Relaxed) == 2
    });
    assert_eq!(shared.opens.load(Ordering::Relaxed), 1);

    shutdown.store(true, Ordering::Release);
    let _ = tx.send(cue().into());
    worker.join().unwrap();
    assert_eq!(state.load(Ordering::Acquire), STATE_STOPPED);
}

#[test]
fn worker_device_failure_is_retried_with_backoff_then_terminal_at_budget() {
    let shared = Arc::new(FakeShared::default());
    shared.fail_open.store(true, Ordering::Relaxed);
    let (tx, _shutdown, state, _busy, worker, dropped_backoff, reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));

    tx.send(cue().into()).unwrap();
    wait_until("first fault reopens", || {
        state.load(Ordering::Acquire) == STATE_REOPENING
    });
    assert!(
        tx.send(cue().into()).is_ok(),
        "ingress stays open while budget remains"
    );

    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.load(Ordering::Acquire) != STATE_FAILED && std::time::Instant::now() < deadline {
        let _ = tx.send(cue().into());
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        state.load(Ordering::Acquire),
        STATE_FAILED,
        "the budget must eventually be spent"
    );
    worker.join().unwrap();

    assert_eq!(
        shared.opens.load(Ordering::Relaxed),
        usize::from(REOPEN_BUDGET) + 1,
        "the first open plus exactly the budget's worth of REopens"
    );
    assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
    assert_eq!(reopens_left.load(Ordering::Acquire), 0);
    assert!(
        tx.send(cue().into()).is_err(),
        "an exhausted budget still seals ingress"
    );
    assert!(
        dropped_backoff.load(Ordering::Relaxed) > 0,
        "cues lost to a down device must be counted"
    );
}

#[test]
fn a_stalled_device_is_discovered_by_the_next_key_and_that_key_is_still_heard() {
    let shared = Arc::new(FakeShared::default());
    let (tx, shutdown, state, _busy, worker, _dropped_backoff, reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));

    tx.send(cue().into()).unwrap();
    wait_until("first open", || {
        shared.opens.load(Ordering::Relaxed) == 1 && shared.pushes.load(Ordering::Acquire) == 1
    });

    shared.stall_push_once.store(true, Ordering::Release);
    tx.send(cue().into()).unwrap();
    wait_until("reopen after stall", || {
        shared.opens.load(Ordering::Relaxed) == 2 && shared.pushes.load(Ordering::Acquire) == 3
    });
    wait_until("running again", || {
        state.load(Ordering::Acquire) == STATE_RUNNING
    });
    assert_eq!(
        reopens_left.load(Ordering::Acquire),
        REOPEN_BUDGET - 1,
        "the reopen it just spent stays spent"
    );

    std::thread::sleep(std::time::Duration::from_millis(40));
    let before = shared.pushes.load(Ordering::Acquire);
    tx.send(cue().into()).unwrap();
    wait_until("a cue delivered past the healthy window", || {
        shared.pushes.load(Ordering::Acquire) > before
    });
    wait_until("the budget is handed back", || {
        reopens_left.load(Ordering::Acquire) == REOPEN_BUDGET
    });

    shutdown.store(true, Ordering::Release);
    let _ = tx.send(cue().into());
    worker.join().unwrap();
}

#[test]
fn a_healthy_device_is_never_reopened() {
    let shared = Arc::new(FakeShared::default());
    let (tx, shutdown, _state, _busy, worker, dropped_backoff, reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));
    for _ in 0..8 {
        tx.send(cue().into()).unwrap();
    }
    wait_until("eight healthy pushes", || {
        shared.pushes.load(Ordering::Acquire) == 8
    });
    assert_eq!(shared.opens.load(Ordering::Relaxed), 1);
    assert_eq!(reopens_left.load(Ordering::Acquire), REOPEN_BUDGET);
    assert_eq!(dropped_backoff.load(Ordering::Relaxed), 0);
    shutdown.store(true, Ordering::Release);
    let _ = tx.send(cue().into());
    worker.join().unwrap();
}

#[test]
fn a_device_that_opens_but_never_plays_spends_the_budget_and_stops() {
    let shared = Arc::new(FakeShared::default());
    shared.fail_push.store(true, Ordering::Relaxed);
    let (tx, _shutdown, state, _busy, worker, dropped_backoff, reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));

    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.load(Ordering::Acquire) != STATE_FAILED && std::time::Instant::now() < deadline {
        let _ = tx.send(cue().into());
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        state.load(Ordering::Acquire),
        STATE_FAILED,
        "a device that never plays must spend the budget and stop"
    );
    worker.join().unwrap();

    assert!(
        shared.opens.load(Ordering::Relaxed) <= usize::from(REOPEN_BUDGET) + 1,
        "opens must be bounded by the budget, got {}",
        shared.opens.load(Ordering::Relaxed)
    );
    assert_eq!(reopens_left.load(Ordering::Acquire), 0);
    assert!(
        tx.send(cue().into()).is_err(),
        "exhaustion still seals ingress"
    );
    assert!(dropped_backoff.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_slow_device_open_is_not_a_wedge() {
    use super::{BUSY_OPENING, OPEN_WEDGE_AFTER_MS, WEDGE_AFTER_MS, busy_stale};

    let t0 = 1_000_000u64;
    assert!(busy_stale(0, t0 + 10 * OPEN_WEDGE_AFTER_MS).is_none());
    assert_eq!(
        busy_stale(t0, t0 + WEDGE_AFTER_MS),
        Some(WEDGE_AFTER_MS),
        "a steady-state call at the threshold IS a wedge — the control"
    );
    assert!(
        busy_stale(t0 | BUSY_OPENING, t0 + WEDGE_AFTER_MS).is_none(),
        "an open of the same age must not read as a wedge"
    );
    assert!(
        busy_stale(t0 | BUSY_OPENING, t0 + OPEN_WEDGE_AFTER_MS - 1).is_none(),
        "one millisecond under its own threshold"
    );
    assert_eq!(
        busy_stale(t0 | BUSY_OPENING, t0 + OPEN_WEDGE_AFTER_MS),
        Some(OPEN_WEDGE_AFTER_MS),
        "an open past OPEN_WEDGE_AFTER_MS is a wedge again — the longer \
         window is a different price, not an exemption"
    );

    let opening = monotonic_ms().saturating_sub(WEDGE_AFTER_MS).max(1) | BUSY_OPENING;

    let (audio, _rx) = TrailAudio::test_ingress();
    audio.busy.store(opening & !BUSY_OPENING, Ordering::Release);
    assert!(
        audio.busy_stale_ms().is_some(),
        "the same stamp WITHOUT the open bit is a wedge — the live control"
    );

    let (mut audio, _rx) = TrailAudio::test_ingress();
    audio.busy.store(opening, Ordering::Release);
    assert!(audio.busy_stale_ms().is_none());
    let before = audio.revives_spent();
    audio.push(cue());
    assert_eq!(
        audio.revives_spent(),
        before,
        "an opening worker must not be abandoned"
    );
    assert!(audio.is_live(), "…nor its ingress sealed");
}

#[test]
fn callback_stall_verdict_is_exact_on_a_fake_clock() {
    const MS: u64 = 1_000;
    let armed = 10 * MS;
    assert!(!callback_stalled(false, armed, armed + 10_000 * MS));
    assert!(!callback_stalled(true, 0, 10_000 * MS));
    assert!(!callback_stalled(
        true,
        armed,
        armed + super::STALL_AFTER_MS * MS - 1
    ));
    assert!(callback_stalled(
        true,
        armed,
        armed + super::STALL_AFTER_MS * MS
    ));
    assert!(!callback_stalled(true, 10_000 * MS, 1));
}

#[test]
fn reopen_attempts_respect_their_backoff_window() {
    let mut output: Option<()> = Some(());
    let backoff = [
        std::time::Duration::ZERO,
        std::time::Duration::from_millis(50),
    ];
    let mut retry = super::RetryState::new(&backoff);
    let t0 = std::time::Instant::now();

    assert_eq!(
        super::reopen_after_fault(&mut output, &mut retry, t0),
        super::Reopen::Now
    );
    assert!(output.is_none(), "the device object is thrown away");
    assert!(retry.ready(t0));
    assert_eq!(retry.left(), 1);

    output = Some(());
    assert_eq!(
        super::reopen_after_fault(&mut output, &mut retry, t0),
        super::Reopen::Wait
    );
    assert!(!retry.ready(t0 + std::time::Duration::from_millis(49)));
    assert!(retry.ready(t0 + std::time::Duration::from_millis(50)));
    assert_eq!(retry.left(), 0);

    assert_eq!(
        super::reopen_after_fault(&mut output, &mut retry, t0),
        super::Reopen::Exhausted
    );
}

#[test]
fn an_inert_host_counts_no_sealed_drops() {
    let mut audio = TrailAudio::new(false);
    assert_eq!(audio.state(), STATE_DORMANT);
    assert!(audio.worker.is_none(), "an inert host constructs no worker");
    for _ in 0..100 {
        audio.push(cue());
    }
    assert_eq!(audio.dropped_cues(), 0);
}

#[test]
fn replacing_with_inert_audio_disconnects_old_ingress() {
    let (mut audio, rx) = TrailAudio::test_ingress();
    audio.push(cue());

    audio.replace(false);

    assert_eq!(
        rx.try_iter().count(),
        1,
        "the accepted cue remains accounted"
    );
    assert!(
        matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the old cue ingress is synchronously closed"
    );
    assert!(audio.is_inert_for_test());
    assert_eq!(audio.state(), STATE_DORMANT);
}

#[test]
fn platform_busy_stamps_calls_and_clears_at_park() {
    let shared = Arc::new(FakeShared::default());
    shared.block_push.store(true, Ordering::Release);
    let (tx, shutdown, state, busy, worker, _dropped_backoff, _reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));

    tx.send(cue().into()).unwrap();
    wait_until("worker entered the blocked platform call", || {
        shared.pushes.load(Ordering::Relaxed) == 1
    });
    let mark = busy.load(Ordering::Acquire);
    assert_ne!(mark, 0, "a worker inside a platform call is busy");
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert_eq!(
        busy.load(Ordering::Acquire),
        mark,
        "the stamp is the call's ENTRY instant, not a heartbeat"
    );

    shared.block_push.store(false, Ordering::Release);
    shared.pause_on_tick.store(true, Ordering::Release);
    wait_until("idle pause", || {
        state.load(Ordering::Acquire) == STATE_PAUSED
    });
    assert_eq!(
        busy.load(Ordering::Acquire),
        0,
        "a parked worker is not busy — park is healthy by definition"
    );

    shutdown.store(true, Ordering::Release);
    let _ = tx.send(cue().into());
    worker.join().unwrap();
}

#[test]
fn young_platform_call_is_not_a_wedge() {
    let (mut audio, rx) = TrailAudio::test_ingress();
    for _ in 0..COMMAND_CAPACITY {
        audio.push(cue());
    }
    audio
        .busy
        .store(monotonic_ms().max(1), std::sync::atomic::Ordering::Release);
    audio.push(cue());
    assert!(audio.wedged_for().is_none());
    assert_eq!(audio.revives_left, WEDGE_REVIVES, "no revival spent");
    assert!(audio.tx.is_some(), "ingress stays open");
    assert_eq!(audio.dropped_cues(), 1, "the drop is still accounted");
    assert_eq!(rx.try_iter().count(), COMMAND_CAPACITY);
}

#[test]
fn wedged_full_ingress_revives_within_budget() {
    let (mut audio, rx) = TrailAudio::test_ingress();
    audio.revives_left = 1;
    for _ in 0..COMMAND_CAPACITY {
        audio.push(cue());
    }
    audio.busy.store(1, std::sync::atomic::Ordering::Release);
    assert!(
        audio.wedged_for().is_some(),
        "a stale platform call over open ingress is the wedge"
    );

    audio.push(cue());
    assert_eq!(audio.revives_left, 0, "one revival spent");
    assert!(audio.tx.is_some(), "fresh ingress is open");
    assert!(audio.worker.is_some(), "a fresh worker exists");
    assert!(audio.wedged_for().is_none(), "the fresh worker is not busy");
    assert_eq!(audio.state(), STATE_DORMANT);
    assert_eq!(rx.try_iter().count(), COMMAND_CAPACITY);
    assert!(
        matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the wedged channel is abandoned with its worker"
    );
}

#[test]
fn wedge_with_spent_budget_seals_ingress() {
    let (mut audio, _rx) = TrailAudio::test_ingress();
    audio.revives_left = 0;
    for _ in 0..COMMAND_CAPACITY {
        audio.push(cue());
    }
    audio.busy.store(1, std::sync::atomic::Ordering::Release);
    audio.push(cue());
    assert!(audio.tx.is_none(), "ingress is sealed");
    assert!(!audio.is_live());
    assert!(audio.wedged_for().is_none(), "sealed outranks wedged");
    assert_eq!(audio.state(), STATE_FAILED);
}

#[test]
fn dropping_a_wedged_host_detaches_instead_of_freezing() {
    let (mut audio, _rx) = TrailAudio::test_ingress();
    let (keeper_tx, keeper_rx) = std::sync::mpsc::channel::<()>();
    audio.worker = Some(std::thread::spawn(move || {
        let _ = keeper_rx.recv();
    }));
    audio.busy.store(1, std::sync::atomic::Ordering::Release);

    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(audio);
        let _ = answer_tx.send(LAST_TEARDOWN.with(std::cell::Cell::get));
    });
    let teardown = answer_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(keeper_tx);
    dropper.join().expect("the drop does not panic");
    assert_eq!(
        teardown,
        Ok(Some(WorkerTeardown::DetachedWedged)),
        "a wedged worker is detached immediately, never joined or waited out"
    );
}

#[test]
fn dropping_a_healthy_host_stays_synchronous() {
    let audio = TrailAudio::new(true);
    let parked_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !ParkedMark::holds(&audio.state) {
        assert!(
            std::time::Instant::now() < parked_by,
            "the idle worker never parked"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    LAST_TEARDOWN.with(|last| last.set(None));
    DETACH_DEADLINE_OVERRIDE.with(|stretch| stretch.set(Some(aterm_spec::wait::MUST_HAPPEN)));
    let state = std::sync::Arc::clone(&audio.state);
    CLOSED_BEFORE_WAIT.with(|closed| closed.set(None));
    drop(audio);
    DETACH_DEADLINE_OVERRIDE.with(|stretch| stretch.set(None));
    assert_eq!(
        LAST_TEARDOWN.with(std::cell::Cell::get),
        Some(WorkerTeardown::Joined),
        "a parked worker is woken by the closing channel and joined on teardown"
    );
    assert_eq!(
        CLOSED_BEFORE_WAIT.with(std::cell::Cell::get),
        Some(true),
        "the drop waited for its worker before closing the channel that wakes it"
    );
    assert!(
        super::took_disconnect_exit(&state),
        "the parked worker left some other way than its idle recv() disconnecting"
    );
}

#[test]
fn a_plain_push_carries_the_identity_side_car_and_a_stamped_one_its_stamp() {
    let mut audio = TrailAudio::capturing_for_test();
    audio.push(cue());
    audio.push_meta(
        cue(),
        EventMeta {
            at_ms: 4242,
            glyph_class: 2,
            rank: 20,
            pan_from: -0.5,
            block_lead_s: 0.0,
            flow: 0.0,
        },
    );
    let captured = audio.take_captured_with_meta_for_test();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].1, EventMeta::default(), "push stamps nothing");
    assert_eq!(captured[1].1.at_ms, 4242);
    assert_eq!(captured[1].1.glyph_class, 2);
    assert_eq!(captured[1].1.rank, 20);
    assert_eq!(captured[1].1.pan_from, -0.5);
    assert_eq!(
        Cue::from(cue()).meta,
        EventMeta::default(),
        "the channel's identity conversion is the identity side-car"
    );
}

#[test]
fn a_stamped_cue_reaches_the_worker_output_with_its_stamp() {
    use std::sync::atomic::Ordering;

    let shared = Arc::new(FakeShared::default());
    let (tx, shutdown, _state, _busy, worker, _dropped_backoff, _reopens_left) =
        spawn_fake_worker(Arc::clone(&shared));
    tx.send(Cue {
        ev: cue(),
        meta: EventMeta {
            at_ms: 31_337,
            glyph_class: 1,
            rank: 40,
            pan_from: 0.0,
            block_lead_s: 0.0,
            flow: 0.0,
        },
    })
    .unwrap();
    wait_until("stamped push", || {
        shared.pushes.load(Ordering::Acquire) == 1
    });
    assert_eq!(shared.last_at_ms.load(Ordering::Acquire), 31_337);
    shutdown.store(true, Ordering::Release);
    drop(tx);
    worker.join().unwrap();
}

use std::ffi::c_void;

use super::mac::{
    DeviceHealth, OwnedQueue, PROP_IS_RUNNING, QueueOwnerCalls, RunningEdge, RunningProbe,
    running_listener, running_probe,
};

fn fire(edge: &RunningEdge, id: u32) {
    running_listener(
        std::ptr::from_ref(edge).cast_mut().cast::<c_void>(),
        std::ptr::null_mut(),
        id,
    );
}

fn no_read() -> Option<bool> {
    panic!("IsRunning was read on a path that must make no platform call")
}

#[test]
fn the_listener_raises_one_edge_for_its_own_property_only() {
    let edge = RunningEdge::new();
    fire(&edge, 0x6171_7266); // 'aqrf' — some other property
    assert!(!edge.take(), "a foreign property raises nothing");
    running_listener(std::ptr::null_mut(), std::ptr::null_mut(), PROP_IS_RUNNING);
    assert!(!edge.take(), "a null userdata is ignored, not dereferenced");
    fire(&edge, PROP_IS_RUNNING);
    fire(&edge, PROP_IS_RUNNING);
    assert!(edge.take(), "the edge is raised");
    assert!(
        !edge.take(),
        "two notifications before a read are ONE edge: it says 'go and look'"
    );
}

#[test]
fn the_is_running_probe_claims_a_stop_only_after_seeing_the_run() {
    use RunningProbe::{Abstain, NotYetRunning, Running, StoppedUnderUs};
    for seen in [false, true] {
        for called_back in [false, true] {
            assert_eq!(running_probe(seen, called_back, None), Abstain);
            assert_eq!(running_probe(seen, called_back, Some(true)), Running);
        }
        assert_eq!(running_probe(true, seen, Some(false)), StoppedUnderUs);
    }
    assert_eq!(running_probe(false, false, Some(false)), NotYetRunning);
    assert_eq!(
        running_probe(false, true, Some(false)),
        Abstain,
        "a stop reading that contradicts callbacks we saw claims nothing"
    );
}

#[test]
fn a_queue_stopped_under_us_is_seen_on_the_next_service_call() {
    let edge = RunningEdge::new();
    let mut health = DeviceHealth::new();
    let armed = 5_000;
    health.armed(armed);
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(true)));
    fire(&edge, PROP_IS_RUNNING);
    assert!(
        health.needs_reopen(false, true, armed, armed, &edge, || Some(false)),
        "a witnessed 1 -> 0 the worker did not ask for is a reopen"
    );
}

#[test]
fn a_healthy_running_queue_is_never_reopened_by_the_listener() {
    let edge = RunningEdge::new();
    let mut health = DeviceHealth::new();
    let armed = 5_000;
    health.armed(armed);
    for _ in 0..100 {
        assert!(!health.needs_reopen(false, true, armed, armed, &edge, no_read));
    }
    for i in 0..100 {
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed + i, armed + i, &edge, || Some(true)));
    }
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, false, armed, armed, &edge, no_read));
    assert!(!edge.take(), "our own stop's edge is consumed, not carried");
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, true, armed, armed, &edge, || None));
}

#[test]
fn a_start_still_taking_is_looked_at_again_never_reopened() {
    let edge = RunningEdge::new();
    let mut health = DeviceHealth::new();
    let armed = 5_000;
    health.armed(armed);
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(false)));
    assert!(edge.take(), "NotYetRunning re-raises the edge");
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(true)));
    health.armed(armed + 10);
    fire(&edge, PROP_IS_RUNNING);
    assert!(
        !health.needs_reopen(false, true, armed + 10, armed + 10, &edge, || Some(false)),
        "a fresh start's 0 is 'not yet', not 'stopped'"
    );
    let _ = edge.take();
    fire(&edge, PROP_IS_RUNNING);
    assert!(!health.needs_reopen(false, true, armed + 20, armed + 20, &edge, || Some(false)));
    assert!(!edge.take(), "an abstention does not re-raise");
}

#[test]
fn the_older_detectors_answer_through_the_same_verdict() {
    let edge = RunningEdge::new();
    let mut health = DeviceHealth::new();
    let armed = 5_000;
    health.armed(armed);
    assert!(health.needs_reopen(true, true, armed, armed, &edge, no_read));
    let stalled_at = armed + super::STALL_AFTER_MS * 1_000;
    assert!(health.needs_reopen(false, true, armed, stalled_at, &edge, no_read));
    assert!(!health.needs_reopen(false, true, armed, stalled_at - 1, &edge, no_read));
}

struct RecordingCalls {
    log: Arc<std::sync::Mutex<Vec<&'static str>>>,
    accept_listener: bool,
}

impl RecordingCalls {
    fn new(accept_listener: bool) -> (Self, Arc<std::sync::Mutex<Vec<&'static str>>>) {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                log: Arc::clone(&log),
                accept_listener,
            },
            log,
        )
    }

    fn note(&self, what: &'static str) {
        self.log.lock().unwrap().push(what);
    }
}

impl QueueOwnerCalls for RecordingCalls {
    fn add_running_listener(&mut self) -> bool {
        self.note("add");
        self.accept_listener
    }

    fn remove_running_listener(&mut self) {
        self.note("remove");
    }

    fn dispose(&mut self) {
        self.note("dispose");
    }

    fn release_user(&mut self) {
        self.note("release");
    }
}

#[test]
fn the_queue_owner_removes_the_listener_before_dispose_on_every_path() {
    let (calls, log) = RecordingCalls::new(true);
    let mut owned = OwnedQueue::new(calls);
    assert!(owned.listen());
    drop(owned);
    assert_eq!(
        *log.lock().unwrap(),
        ["add", "remove", "dispose", "release"]
    );

    let (calls, log) = RecordingCalls::new(false);
    let mut owned = OwnedQueue::new(calls);
    assert!(!owned.listen());
    drop(owned);
    assert_eq!(*log.lock().unwrap(), ["add", "dispose", "release"]);

    fn open_then_fail(calls: RecordingCalls) -> Option<OwnedQueue<RecordingCalls>> {
        let mut owned = OwnedQueue::new(calls);
        owned.listen();
        for slot in 0..3 {
            if slot == 1 {
                return None;
            }
        }
        Some(owned)
    }
    let (calls, log) = RecordingCalls::new(true);
    assert!(open_then_fail(calls).is_none());
    assert_eq!(
        *log.lock().unwrap(),
        ["add", "remove", "dispose", "release"],
        "the allocation unwind removes the listener before disposing"
    );

    let (calls, log) = RecordingCalls::new(true);
    let mut owned = OwnedQueue::new(calls);
    owned.listen();
    std::mem::forget(owned);
    assert_eq!(*log.lock().unwrap(), ["add"]);
}

/// A device double for the worker-level proof: the real `DeviceHealth`
/// verdict and the real listener body, over a fake `IsRunning` property,
/// driven through the shipping `worker_loop` and its reopen ladder.
#[derive(Default)]
struct ListenerDevice {
    /// 0 reads stopped, 1 reads running, 2 the property call fails.
    property: AtomicU8,
    reads: AtomicUsize,
    opens: AtomicUsize,
    pushes: AtomicUsize,
    ticks: AtomicUsize,
    drops: AtomicUsize,
    /// The edge of the current device — each open gets a fresh one, as
    /// each real `MacOut` gets a fresh `Shared`.
    edge: std::sync::Mutex<Option<Arc<RunningEdge>>>,
}

impl ListenerDevice {
    fn stop_under_us(&self, running: bool) {
        self.property.store(u8::from(running), Ordering::Release);
        let edge = self.edge.lock().unwrap().clone().expect("a device is open");
        fire(&edge, PROP_IS_RUNNING);
    }
}

struct ListenerOutput {
    dev: Arc<ListenerDevice>,
    edge: Arc<RunningEdge>,
    health: DeviceHealth,
    running: bool,
}

impl ListenerOutput {
    fn needs_reopen(&mut self) -> bool {
        let dev = &self.dev;
        self.health
            .needs_reopen(false, self.running, 1_000, 1_000, &self.edge, || {
                dev.reads.fetch_add(1, Ordering::AcqRel);
                match dev.property.load(Ordering::Acquire) {
                    0 => Some(false),
                    1 => Some(true),
                    _ => None,
                }
            })
    }
}

impl AudioWorkerOutput for ListenerOutput {
    fn push_meta(&mut self, _ev: SoundEvent, _meta: EventMeta) -> Delivery {
        if self.needs_reopen() {
            return Delivery::Reopen;
        }
        if !self.running {
            self.running = true;
            self.health.armed(1_000);
        }
        self.dev.pushes.fetch_add(1, Ordering::AcqRel);
        Delivery::Sounded
    }

    fn on_tick(&mut self) -> Service {
        self.dev.ticks.fetch_add(1, Ordering::AcqRel);
        if self.needs_reopen() {
            Service::Reopen
        } else {
            Service::Running
        }
    }

    fn is_running(&self) -> bool {
        self.running
    }
}

impl Drop for ListenerOutput {
    fn drop(&mut self) {
        self.dev.drops.fetch_add(1, Ordering::AcqRel);
    }
}

fn spawn_listener_worker(
    dev: Arc<ListenerDevice>,
    housekeeping: std::time::Duration,
) -> FakeWorkerHandles {
    let (tx, rx) = cue_channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(AtomicU8::new(STATE_DORMANT));
    let busy = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let dropped_backoff = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let reopens_left = Arc::new(AtomicU8::new(REOPEN_BUDGET));
    let (w_shutdown, w_state, w_busy, w_dropped, w_left) = (
        Arc::clone(&shutdown),
        Arc::clone(&state),
        Arc::clone(&busy),
        Arc::clone(&dropped_backoff),
        Arc::clone(&reopens_left),
    );
    let worker = std::thread::spawn(move || {
        worker_loop(
            rx,
            WorkerFlags {
                shutdown: &w_shutdown,
                state: &w_state,
                busy: &w_busy,
                dropped_backoff: &w_dropped,
                reopens_left: &w_left,
            },
            7,
            housekeeping,
            &FAKE_BACKOFF,
            move |_| {
                dev.opens.fetch_add(1, Ordering::AcqRel);
                let edge = Arc::new(RunningEdge::new());
                *dev.edge.lock().unwrap() = Some(Arc::clone(&edge));
                Some(ListenerOutput {
                    dev: Arc::clone(&dev),
                    edge,
                    health: DeviceHealth::new(),
                    running: false,
                })
            },
        );
    });
    (
        tx,
        shutdown,
        state,
        busy,
        worker,
        dropped_backoff,
        reopens_left,
    )
}

#[test]
fn the_next_key_after_a_stop_under_us_reopens_and_is_heard() {
    let dev = Arc::new(ListenerDevice::default());
    let (tx, shutdown, state, _busy, worker, _dropped, reopens_left) =
        spawn_listener_worker(Arc::clone(&dev), std::time::Duration::from_secs(60));

    tx.send(cue().into()).unwrap();
    wait_until("first open + push", || {
        dev.pushes.load(Ordering::Acquire) == 1
    });

    for n in 2..=4 {
        dev.stop_under_us(true);
        tx.send(cue().into()).unwrap();
        wait_until("healthy push", || dev.pushes.load(Ordering::Acquire) == n);
    }
    tx.send(cue().into()).unwrap();
    wait_until("plain push", || dev.pushes.load(Ordering::Acquire) == 5);
    assert_eq!(dev.reads.load(Ordering::Acquire), 3, "one read per edge");
    assert_eq!(dev.opens.load(Ordering::Acquire), 1, "never reopened");
    assert_eq!(dev.drops.load(Ordering::Acquire), 0);
    assert_eq!(reopens_left.load(Ordering::Acquire), REOPEN_BUDGET);

    dev.stop_under_us(false);
    tx.send(cue().into()).unwrap();
    wait_until("reopened and re-pushed", || {
        dev.opens.load(Ordering::Acquire) == 2 && dev.pushes.load(Ordering::Acquire) == 6
    });
    assert_eq!(dev.drops.load(Ordering::Acquire), 1, "the dead device went");
    assert_eq!(
        reopens_left.load(Ordering::Acquire),
        REOPEN_BUDGET - 1,
        "the same budget the watchdog spends: one ladder"
    );
    wait_until("running again", || {
        state.load(Ordering::Acquire) == STATE_RUNNING
    });

    shutdown.store(true, Ordering::Release);
    drop(tx);
    worker.join().unwrap();
}

#[test]
fn the_next_tick_after_a_stop_under_us_reads_reopening() {
    let dev = Arc::new(ListenerDevice::default());
    let (tx, shutdown, state, _busy, worker, _dropped, reopens_left) =
        spawn_listener_worker(Arc::clone(&dev), std::time::Duration::from_millis(2));

    tx.send(cue().into()).unwrap();
    wait_until("first push", || dev.pushes.load(Ordering::Acquire) == 1);

    dev.stop_under_us(true);
    wait_until("a tick read the edge", || {
        dev.reads.load(Ordering::Acquire) == 1
    });
    let ticks = dev.ticks.load(Ordering::Acquire);
    wait_until("ten more healthy ticks", || {
        dev.ticks.load(Ordering::Acquire) >= ticks + 10
    });
    assert_eq!(dev.drops.load(Ordering::Acquire), 0, "never reopened");
    assert_eq!(state.load(Ordering::Acquire), STATE_RUNNING);

    dev.stop_under_us(false);
    wait_until("the tick reopens", || {
        state.load(Ordering::Acquire) == STATE_REOPENING
    });
    assert_eq!(dev.drops.load(Ordering::Acquire), 1);
    assert_eq!(reopens_left.load(Ordering::Acquire), REOPEN_BUDGET - 1);
    assert_eq!(HostState::Reopening.label(), "reopening");

    tx.send(cue().into()).unwrap();
    wait_until("reopened on the next key", || {
        dev.opens.load(Ordering::Acquire) == 2 && dev.pushes.load(Ordering::Acquire) == 2
    });

    shutdown.store(true, Ordering::Release);
    drop(tx);
    worker.join().unwrap();
}
