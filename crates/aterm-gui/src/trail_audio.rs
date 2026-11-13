// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail audio output queue fed from [`aterm_effects::trail_sound::TrailSynth`] (AudioToolbox
//! `AudioQueue` on macOS, inert stub elsewhere).

use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use aterm_effects::trail_sound::{EventMeta, SoundEvent};

/// Whether this build has a real platform audio output.
#[must_use]
pub(crate) const fn output_available() -> bool {
    cfg!(target_os = "macos")
}

/// Output sample rate (48 kHz).
#[cfg(target_os = "macos")]
const SAMPLE_RATE: f64 = 48_000.0;

/// Frames per queue buffer (~10.7 ms at 48 kHz), with 3 buffers in flight.
#[cfg(target_os = "macos")]
const BUFFER_FRAMES: usize = 512;
#[cfg(target_os = "macos")]
const BUFFER_COUNT: usize = 3;

/// Consecutive silent buffers before the queue pauses (~1.5 s of exact silence).
#[cfg(target_os = "macos")]
const PAUSE_AFTER_SILENT: u32 = 140;

/// Housekeeping cadence while the AudioQueue runs: observes the exact-silence
/// counter and pauses the device.
#[cfg(target_os = "macos")]
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(250);

/// UI-side staleness threshold for the worker's platform-busy stamp.
#[cfg(target_os = "macos")]
const WEDGE_AFTER_MS: u64 = 3_000;

/// How long teardown waits for a live worker before detaching it: one
/// housekeeping interval plus a healthy platform call, so an ordinary drop is
/// effectively synchronous and a wedge never blocks the event loop.
#[cfg(target_os = "macos")]
const DETACH_DEADLINE: Duration = Duration::from_millis(600);

/// Poll cadence while waiting out [`DETACH_DEADLINE`].
#[cfg(target_os = "macos")]
const DETACH_POLL: Duration = Duration::from_millis(5);

/// What a [`TrailAudio`] drop did with its worker, recorded (test-only) on the
/// dropping thread so teardown tests assert the join/detach decision itself.
#[cfg(all(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerTeardown {
    /// The worker finished inside the deadline and was joined.
    Joined,
    /// A wedged worker, detached without waiting.
    DetachedWedged,
    /// A worker that outlived the whole deadline, detached after it.
    DetachedAtDeadline,
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static LAST_TEARDOWN: std::cell::Cell<Option<WorkerTeardown>> =
        const { std::cell::Cell::new(None) };
    static DETACH_DEADLINE_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
    static CLOSED_BEFORE_WAIT: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only: the workers (named as in [`PARKED_WORKERS`]) that exited because
/// their idle `recv()` answered `Disconnected`.
#[cfg(all(test, target_os = "macos"))]
static DISCONNECT_EXITS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Record that the worker owning `state` left its idle `recv()` on
/// `Disconnected` ([`DISCONNECT_EXITS`]).
#[cfg(all(test, target_os = "macos"))]
fn note_disconnect_exit(state: &std::sync::atomic::AtomicU8) {
    DISCONNECT_EXITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(std::ptr::from_ref(state) as usize);
}

/// Whether the worker owning `state` left on `Disconnected`, consuming the
/// record ([`DISCONNECT_EXITS`]).
#[cfg(all(test, target_os = "macos"))]
fn took_disconnect_exit(state: &std::sync::atomic::AtomicU8) -> bool {
    let key = std::ptr::from_ref(state) as usize;
    let mut exits = DISCONNECT_EXITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    exits
        .iter()
        .position(|at| *at == key)
        .map(|at| exits.swap_remove(at))
        .is_some()
}

/// Test-only: the workers, named by the address of their state word, that are
/// past their last `shutdown` check and committed to the idle `recv()`, so a
/// teardown test can drop a worker that is really parked.
#[cfg(all(test, target_os = "macos"))]
static PARKED_WORKERS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// One worker's entry in [`PARKED_WORKERS`], held across its idle `recv()`.
#[cfg(all(test, target_os = "macos"))]
struct ParkedMark(usize);

#[cfg(all(test, target_os = "macos"))]
impl ParkedMark {
    fn new(state: &std::sync::atomic::AtomicU8) -> Self {
        let key = std::ptr::from_ref(state) as usize;
        PARKED_WORKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(key);
        Self(key)
    }

    fn holds(state: &std::sync::atomic::AtomicU8) -> bool {
        let key = std::ptr::from_ref(state) as usize;
        PARKED_WORKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&key)
    }
}

#[cfg(all(test, target_os = "macos"))]
impl Drop for ParkedMark {
    fn drop(&mut self) {
        let mut parked = PARKED_WORKERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = parked.iter().position(|key| *key == self.0) {
            parked.swap_remove(at);
        }
    }
}

/// [`DETACH_DEADLINE`], unless a test stretched it on this thread.
#[cfg(target_os = "macos")]
fn detach_deadline() -> Duration {
    #[cfg(test)]
    if let Some(stretched) = DETACH_DEADLINE_OVERRIDE.with(std::cell::Cell::get) {
        return stretched;
    }
    DETACH_DEADLINE
}

/// Lifetime budget of worker revivals after wedge detection.
#[cfg(target_os = "macos")]
const WEDGE_REVIVES: u8 = 3;

/// How long a device open may run before it is judged a wedge: five times
/// [`WEDGE_AFTER_MS`].
#[cfg(target_os = "macos")]
const OPEN_WEDGE_AFTER_MS: u64 = WEDGE_AFTER_MS * 5;

/// How long without a render callback counts as a stall.
#[cfg(target_os = "macos")]
const STALL_AFTER_MS: u64 = 750;

/// Lifetime budget of device reopens.
#[cfg(target_os = "macos")]
const REOPEN_BUDGET: u8 = 6;

/// Wait before the n-th reopen attempt.
#[cfg(target_os = "macos")]
const REOPEN_BACKOFF: [Duration; REOPEN_BUDGET as usize] = [
    Duration::ZERO,
    Duration::from_millis(200),
    Duration::from_millis(800),
    Duration::from_secs(3),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use aterm_effects::trail_sound::{CHANNELS, EventMeta, SoundEvent, TrailSynth};

    use super::{BUFFER_COUNT, BUFFER_FRAMES, PAUSE_AFTER_SILENT, SAMPLE_RATE};

    /// One queue block in seconds.
    pub(super) const BLOCK_S: f32 = BUFFER_FRAMES as f32 / SAMPLE_RATE as f32;

    /// Pre-roll: how far into the in-flight block a cue arrived.
    pub(super) fn block_lead_s(running: bool, block_start_us: u64, now_us: u64) -> f32 {
        if !running || block_start_us == 0 {
            return 0.0;
        }
        (now_us.saturating_sub(block_start_us) as f32 * 1e-6).min(BLOCK_S)
    }

    /// Stall verdict: the queue is running, armed (a successful start stamped `last_cb_us`).
    pub(super) const fn callback_stalled(running: bool, last_cb_us: u64, now_us: u64) -> bool {
        running
            && last_cb_us != 0
            && now_us.saturating_sub(last_cb_us) >= super::STALL_AFTER_MS * 1_000
    }

    /// What servicing the queue decided.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Service {
        Running,
        Paused,
        Reopen,
    }

    /// What delivering a cue decided.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Delivery {
        Sounded,
        Reopen,
    }

    /// Edge raised by [`running_listener`] when AudioToolbox reports a change.
    pub(super) struct RunningEdge(AtomicBool);

    impl RunningEdge {
        pub(super) const fn new() -> Self {
            Self(AtomicBool::new(false))
        }

        /// The listener's whole body.
        pub(super) fn raise(&self) {
            self.0.store(true, Ordering::Release);
        }

        /// Consume a pending edge.
        pub(super) fn take(&self) -> bool {
            self.0.load(Ordering::Relaxed) && self.0.swap(false, Ordering::AcqRel)
        }
    }

    /// The `IsRunning` property listener.
    pub(super) extern "C" fn running_listener(
        user: *mut c_void,
        _q: AudioQueueRef,
        id: AudioQueuePropertyID,
    ) {
        if id != PROP_IS_RUNNING || user.is_null() {
            return;
        }
        // SAFETY: `user` points at a `RunningEdge` that outlives every listener call.
        let edge = unsafe { &*(user as *const RunningEdge) };
        edge.raise();
    }

    /// What one read of `IsRunning` means, given what the worker already knows about this start.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum RunningProbe {
        /// It reads running.
        Running,
        /// Seen running since our start and now stopped, without the worker stopping it.
        StoppedUnderUs,
        /// Reads stopped, but this start has not been seen running and no callback has arrived.
        NotYetRunning,
        /// No reading (the property call failed), or one that contradicts the callbacks seen.
        Abstain,
    }

    /// The `IsRunning` verdict.
    pub(super) const fn running_probe(
        seen_running: bool,
        called_back: bool,
        property: Option<bool>,
    ) -> RunningProbe {
        match property {
            None => RunningProbe::Abstain,
            Some(true) => RunningProbe::Running,
            Some(false) if seen_running => RunningProbe::StoppedUnderUs,
            Some(false) if !called_back => RunningProbe::NotYetRunning,
            Some(false) => RunningProbe::Abstain,
        }
    }

    /// The worker's per-start knowledge of whether the device is alive.
    pub(super) struct DeviceHealth {
        /// `last_cb_us` exactly as the last start armed it.
        armed_cb_us: u64,
        /// The worker has read `IsRunning != 0` since the last start.
        seen_running: bool,
    }

    impl DeviceHealth {
        pub(super) const fn new() -> Self {
            Self {
                armed_cb_us: 0,
                seen_running: false,
            }
        }

        /// A start succeeded and armed the watchdog at `armed_cb_us`.
        pub(super) const fn armed(&mut self, armed_cb_us: u64) {
            self.armed_cb_us = armed_cb_us;
            self.seen_running = false;
        }

        /// Whether the device must be reopened.
        pub(super) fn needs_reopen(
            &mut self,
            faulted: bool,
            running: bool,
            last_cb_us: u64,
            now_us: u64,
            edge: &RunningEdge,
            read_is_running: impl FnOnce() -> Option<bool>,
        ) -> bool {
            if faulted || callback_stalled(running, last_cb_us, now_us) {
                return true;
            }
            if !edge.take() {
                return false;
            }
            if !running {
                return false;
            }
            match running_probe(
                self.seen_running,
                last_cb_us > self.armed_cb_us,
                read_is_running(),
            ) {
                RunningProbe::Running => {
                    self.seen_running = true;
                    false
                }
                RunningProbe::StoppedUnderUs => true,
                RunningProbe::NotYetRunning => {
                    edge.raise();
                    false
                }
                RunningProbe::Abstain => false,
            }
        }
    }

    use aterm_macos_ui::audio::{
        AudioApi, AudioQueueBuffer, AudioQueuePropertyID, AudioQueueRef,
        AudioStreamBasicDescription, api,
    };

    /// `kAudioQueueProperty_IsRunning` ('aqrn').
    pub(super) const PROP_IS_RUNNING: AudioQueuePropertyID = 0x6171_726E;

    const FORMAT_LPCM: u32 = 0x6C70_636D;
    const FLAGS_FLOAT_PACKED: u32 = 1 | 8;

    struct Shared {
        /// Acquired before the queue exists; callbacks only read resolved pointers.
        api: &'static AudioApi,
        synth: Mutex<TrailSynth>,
        /// Consecutive buffers of exact silence (reset by any audible one).
        silent: AtomicU32,
        /// Whether the queue is currently started (vs stopped).
        running: AtomicBool,
        /// Callback-side queue failure.
        faulted: AtomicBool,
        /// Whether returned buffers may be rendered and re-enqueued.
        recycle_epoch: AtomicU64,
        /// Serializes the callback's final epoch check and enqueue with the worker's disable.
        recycle_gate: Mutex<()>,
        /// When the callback began rendering the in-flight block, in [`crate::metrics::now_us`] µs.
        block_start_us: AtomicU64,
        /// When the callback last ran, in [`crate::metrics::now_us`] µs.
        last_cb_us: AtomicU64,
        /// The `IsRunning` listener's edge.
        running_edge: RunningEdge,
    }

    impl Shared {
        fn new(seed: u32, api: &'static AudioApi) -> Self {
            Self {
                api,
                synth: Mutex::new(TrailSynth::new(SAMPLE_RATE as f32, seed)),
                silent: AtomicU32::new(0),
                running: AtomicBool::new(false),
                faulted: AtomicBool::new(false),
                recycle_epoch: AtomicU64::new(0),
                recycle_gate: Mutex::new(()),
                block_start_us: AtomicU64::new(0),
                last_cb_us: AtomicU64::new(0),
                running_edge: RunningEdge::new(),
            }
        }

        fn device_needs_reopen(
            &self,
            health: &mut DeviceHealth,
            now_us: u64,
            read_is_running: impl FnOnce() -> Option<bool>,
        ) -> bool {
            health.needs_reopen(
                self.faulted.load(Ordering::Acquire),
                self.running.load(Ordering::Relaxed),
                self.last_cb_us.load(Ordering::Relaxed),
                now_us,
                &self.running_edge,
                read_is_running,
            )
        }
    }

    mod queue_owner {
        use std::ffi::c_void;
        use std::sync::Arc;

        use super::{AudioQueueRef, PROP_IS_RUNNING, Shared, api, running_listener};

        /// Own the queue `AudioQueueNewOutput` just made.
        pub(super) fn own(
            queue: AudioQueueRef,
            user: *const Shared,
        ) -> OwnedQueue<AudioToolboxQueue> {
            OwnedQueue::new(AudioToolboxQueue { queue, user })
        }

        /// The platform calls that bracket a queue's life.
        pub(in crate::trail_audio) trait QueueOwnerCalls {
            fn add_running_listener(&mut self) -> bool;
            fn remove_running_listener(&mut self);
            fn dispose(&mut self);
            fn release_user(&mut self);
        }

        /// The one owner of a live audio queue, and the only place that disposes one.
        pub(in crate::trail_audio) struct OwnedQueue<P: QueueOwnerCalls> {
            calls: P,
            listening: bool,
        }

        impl<P: QueueOwnerCalls> OwnedQueue<P> {
            pub(in crate::trail_audio) const fn new(calls: P) -> Self {
                Self {
                    calls,
                    listening: false,
                }
            }

            /// Register the `IsRunning` listener.
            pub(in crate::trail_audio) fn listen(&mut self) -> bool {
                self.listening = self.calls.add_running_listener();
                self.listening
            }

            pub(in crate::trail_audio) const fn calls(&self) -> &P {
                &self.calls
            }
        }

        impl<P: QueueOwnerCalls> Drop for OwnedQueue<P> {
            fn drop(&mut self) {
                if std::mem::take(&mut self.listening) {
                    self.calls.remove_running_listener();
                }
                self.calls.dispose();
                self.calls.release_user();
            }
        }

        /// The real [`QueueOwnerCalls`].
        pub(super) struct AudioToolboxQueue {
            queue: AudioQueueRef,
            /// `Arc::into_raw` of the queue's `Shared`.
            user: *const Shared,
        }

        impl AudioToolboxQueue {
            pub(super) const fn queue(&self) -> AudioQueueRef {
                self.queue
            }

            const fn edge_user(&self) -> *mut c_void {
                // SAFETY: `user` is a live `Arc<Shared>` raw pointer until
                // `release_user`, which runs last; taking a field's address does
                // not create a reference.
                unsafe { std::ptr::addr_of!((*self.user).running_edge) as *mut c_void }
            }

            /// Read `kAudioQueueProperty_IsRunning`.
            pub(super) fn read_is_running(&self) -> Option<bool> {
                let mut value: u32 = 0;
                let mut size = std::mem::size_of::<u32>() as u32;
                // SAFETY: the queue is live (owned, not yet disposed); `value`
                // and `size` outlive the call and `size` states the buffer.
                let status = unsafe {
                    (api().get_property)(
                        self.queue,
                        PROP_IS_RUNNING,
                        (&mut value as *mut u32).cast(),
                        &mut size,
                    )
                };
                (status == 0 && size as usize == std::mem::size_of::<u32>()).then_some(value != 0)
            }
        }

        impl QueueOwnerCalls for AudioToolboxQueue {
            fn add_running_listener(&mut self) -> bool {
                // SAFETY: The queue is live; the userdata's lifetime is argued at
                // `running_listener`.
                unsafe {
                    (api().add_listener)(
                        self.queue,
                        PROP_IS_RUNNING,
                        running_listener,
                        self.edge_user(),
                    ) == 0
                }
            }

            fn remove_running_listener(&mut self) {
                // SAFETY: the queue is live (dispose has not run); proc and
                // userdata are the exact pair that was added.
                unsafe {
                    (api().remove_listener)(
                        self.queue,
                        PROP_IS_RUNNING,
                        running_listener,
                        self.edge_user(),
                    );
                }
            }

            fn dispose(&mut self) {
                // SAFETY: synchronous dispose (immediate = 1) stops the callback
                // thread before `release_user` drops the refcount it was using.
                unsafe {
                    (api().dispose)(self.queue, 1);
                }
            }

            fn release_user(&mut self) {
                // SAFETY: `user` came from `Arc::into_raw` in `MacOut::new` and is released once,
                // here, after the queue is gone.
                unsafe { drop(Arc::from_raw(self.user)) };
            }
        }
    }
    use queue_owner::AudioToolboxQueue;
    pub(super) use queue_owner::OwnedQueue;
    #[cfg(test)]
    pub(super) use queue_owner::QueueOwnerCalls;

    fn set_callback_recycling(shared: &Shared, enabled: bool) {
        let _gate = shared
            .recycle_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let epoch = shared.recycle_epoch.load(Ordering::Relaxed);
        if (epoch & 1 == 1) != enabled {
            shared
                .recycle_epoch
                .store(epoch.wrapping_add(1), Ordering::Release);
        }
    }

    /// Queue-control seam shared by the real AudioToolbox adapter and the test stand-in.
    pub(super) trait QueueCycle {
        fn buffer_count(&self) -> usize;
        fn stamp_block_start(&mut self);
        fn render_post_cue(&mut self, index: usize) -> bool;
        fn enqueue_buffer(&mut self, index: usize) -> bool;
        fn set_callback_recycling(&mut self, enabled: bool);
        fn start_queue(&mut self) -> bool;
        fn stop_immediate(&mut self) -> bool;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct PrimeReport {
        /// One-based queue position of the first buffer containing audible post-cue samples.
        pub(super) first_audible_buffer: Option<usize>,
    }

    /// Stamp the block start, fill available buffers from the synth, and start the queue.
    pub(super) fn prime_and_start<Q: QueueCycle>(queue: &mut Q) -> Option<PrimeReport> {
        let count = queue.buffer_count();
        debug_assert!(count > 0);
        queue.stamp_block_start();
        let mut first_audible_buffer = None;
        for index in 0..count {
            if queue.render_post_cue(index) && first_audible_buffer.is_none() {
                first_audible_buffer = Some(index + 1);
            }
            if !queue.enqueue_buffer(index) {
                queue.set_callback_recycling(false);
                return None;
            }
        }
        queue.set_callback_recycling(true);
        if !queue.start_queue() {
            queue.set_callback_recycling(false);
            return None;
        }
        Some(PrimeReport {
            first_audible_buffer,
        })
    }

    /// Stop synchronously with callback recycling disabled.
    pub(super) fn stop_and_reclaim<Q: QueueCycle>(queue: &mut Q) -> bool {
        queue.set_callback_recycling(false);
        queue.stop_immediate()
    }

    /// The queue render callback.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "trail_audio_lifecycle",
            action = "RenderAudible",
            project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "trail_audio_lifecycle",
            action = "RenderSilent",
            project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
        )
    )]
    extern "C" fn render_cb(user: *mut c_void, q: AudioQueueRef, buf: *mut AudioQueueBuffer) {
        // SAFETY: `user` is the `Arc<Shared>` raw pointer installed at queue
        // creation and outlives the queue (disposed synchronously first).
        let shared = unsafe { &*(user as *const Shared) };
        shared
            .last_cb_us
            .store(crate::metrics::now_us(), Ordering::Relaxed);
        // SAFETY: the queue hands us a buffer allocated with `BUFFER_FRAMES * CHANNELS` `f32`s, and
        // we fill exactly that.
        unsafe {
            let recycle_epoch = shared.recycle_epoch.load(Ordering::Acquire);
            if recycle_epoch & 1 == 0 {
                return;
            }
            let out = std::slice::from_raw_parts_mut(
                (*buf).m_audio_data as *mut f32,
                BUFFER_FRAMES * CHANNELS,
            );
            let quiet = {
                let mut synth = shared.synth.lock().unwrap_or_else(PoisonError::into_inner);
                shared
                    .block_start_us
                    .store(crate::metrics::now_us(), Ordering::Relaxed);
                synth.render(out);
                synth.is_quiet()
            };
            if quiet {
                let _ = shared
                    .silent
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
            } else {
                shared.silent.store(0, Ordering::Relaxed);
            }
            (*buf).m_audio_data_byte_size = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            let _gate = shared
                .recycle_gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if shared.recycle_epoch.load(Ordering::Acquire) != recycle_epoch {
                return;
            }
            if (shared.api.enqueue_buffer)(q, buf, 0, std::ptr::null()) != 0 {
                shared.faulted.store(true, Ordering::Release);
            }
        }
    }

    pub(crate) struct MacOut {
        /// The queue, its `IsRunning` listener and the callbacks' userdata refcount.
        owned: OwnedQueue<AudioToolboxQueue>,
        shared: Arc<Shared>,
        /// All buffers are allocated for the queue's lifetime.
        buffers: [*mut AudioQueueBuffer; BUFFER_COUNT],
        buffers_available: bool,
        /// Per-start knowledge the `IsRunning` verdict needs.
        health: DeviceHealth,
    }

    // SAFETY: `queue` is only touched from the audio worker (start/stop/
    // dispose); the callback thread reaches state exclusively through the
    // `Shared` (Mutex + atomics). AudioQueue control calls are themselves
    // thread-safe per AudioToolbox's documented contract.
    unsafe impl Send for MacOut {}

    impl MacOut {
        /// Open the output queue and allocate, but do not enqueue, its buffers.
        ///
        /// # Panics
        /// Panics before acquiring queue state if the system AudioToolbox framework
        /// or one of its required entry points is unavailable.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerStart",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub(crate) fn new(seed: u32) -> Option<Self> {
            let api = api();
            let shared = Arc::new(Shared::new(seed, api));
            let fmt = AudioStreamBasicDescription {
                m_sample_rate: SAMPLE_RATE,
                m_format_id: FORMAT_LPCM,
                m_format_flags: FLAGS_FLOAT_PACKED,
                m_bytes_per_packet: (CHANNELS * 4) as u32,
                m_frames_per_packet: 1,
                m_bytes_per_frame: (CHANNELS * 4) as u32,
                m_channels_per_frame: CHANNELS as u32,
                m_bits_per_channel: 32,
                m_reserved: 0,
            };
            let user = Arc::into_raw(Arc::clone(&shared)) as *mut c_void;
            let mut queue: AudioQueueRef = std::ptr::null_mut();
            // SAFETY: fmt/queue outlive the call; null run loop selects
            // AudioToolbox's internal callback thread.
            let st = unsafe {
                (api.new_output)(
                    &fmt,
                    render_cb,
                    user,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut queue,
                )
            };
            if st != 0 || queue.is_null() {
                // SAFETY: reclaim the refcount handed to the uncreated queue so it is not leaked;
                // no queue exists, so there is no listener and nothing to dispose.
                unsafe { drop(Arc::from_raw(user as *const Shared)) };
                return None;
            }
            let mut owned = queue_owner::own(queue, user as *const Shared);
            owned.listen();
            let bytes = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            let mut buffers = [std::ptr::null_mut(); BUFFER_COUNT];
            for slot in &mut buffers {
                let mut buf: *mut AudioQueueBuffer = std::ptr::null_mut();
                // SAFETY: Queue is live; on success the pointer stays allocated until queue
                // disposal. It remains unscheduled/available here.
                let st = unsafe { (api.allocate_buffer)(queue, bytes, &mut buf) };
                if st != 0 || buf.is_null() {
                    return None;
                }
                *slot = buf;
            }
            Some(Self {
                owned,
                shared,
                buffers,
                buffers_available: true,
                health: DeviceHealth::new(),
            })
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerStartFails",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        fn start(&mut self) -> bool {
            if self.shared.faulted.load(Ordering::Acquire) || !self.buffers_available {
                return false;
            }
            self.buffers_available = false;
            let report = {
                let mut cycle = MacQueueCycle {
                    queue: self.owned.calls().queue(),
                    shared: &self.shared,
                    buffers: &self.buffers,
                };
                prime_and_start(&mut cycle)
            };
            if let Some(report) = report {
                debug_assert!(
                    report.first_audible_buffer.is_none_or(|buffer| buffer <= 1),
                    "an audible cue must begin within the first queue buffer"
                );
                self.shared.running.store(true, Ordering::Relaxed);
                self.shared.silent.store(0, Ordering::Relaxed);
                let armed_cb_us = crate::metrics::now_us();
                self.shared.last_cb_us.store(armed_cb_us, Ordering::Relaxed);
                self.health.armed(armed_cb_us);
                true
            } else {
                self.shared.faulted.store(true, Ordering::Release);
                false
            }
        }

        /// Queue a cue: `Sounded` once output runs, else `Reopen`.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerPushRunning",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub(crate) fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> Delivery {
            // The key path checks the death verdict too, since `on_tick` only
            // runs when nobody is typing. The cue is not consumed: the worker
            // reopens and re-pushes it.
            if self.device_needs_reopen() {
                return Delivery::Reopen;
            }
            {
                let mut synth = self
                    .shared
                    .synth
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let block_lead_s = block_lead_s(
                    self.shared.running.load(Ordering::Relaxed),
                    self.shared.block_start_us.load(Ordering::Relaxed),
                    crate::metrics::now_us(),
                );
                synth.push_meta(
                    ev,
                    EventMeta {
                        block_lead_s,
                        ..meta
                    },
                );
                self.shared.silent.store(0, Ordering::Release);
            }
            if !self.shared.running.load(Ordering::Relaxed) && !self.start() {
                return Delivery::Reopen;
            }
            if self.shared.running.load(Ordering::Acquire) {
                Delivery::Sounded
            } else {
                Delivery::Reopen
            }
        }

        /// Housekeeping: pause a running queue after [`PAUSE_AFTER_SILENT`] silent buffers.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "ServiceRunning",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "PauseIdle",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub(crate) fn on_tick(&mut self) -> Service {
            // A faulted, stalled or stopped-under-us queue must reopen, not
            // pause: the death verdict is asked first.
            if self.device_needs_reopen() {
                return Service::Reopen;
            }
            if self.shared.running.load(Ordering::Relaxed)
                && self.shared.silent.load(Ordering::Acquire) >= PAUSE_AFTER_SILENT
            {
                let reclaimed = {
                    let mut cycle = MacQueueCycle {
                        queue: self.owned.calls().queue(),
                        shared: &self.shared,
                        buffers: &self.buffers,
                    };
                    stop_and_reclaim(&mut cycle)
                };
                if reclaimed {
                    self.buffers_available = true;
                    self.shared.running.store(false, Ordering::Relaxed);
                } else {
                    return Service::Reopen;
                }
            }
            if self.shared.running.load(Ordering::Relaxed) {
                Service::Running
            } else {
                Service::Paused
            }
        }

        pub(crate) fn is_running(&self) -> bool {
            self.shared.running.load(Ordering::Acquire)
        }

        fn device_needs_reopen(&mut self) -> bool {
            let owned = &self.owned;
            self.shared
                .device_needs_reopen(&mut self.health, crate::metrics::now_us(), || {
                    owned.calls().read_is_running()
                })
        }
    }

    impl Drop for MacOut {
        fn drop(&mut self) {
            set_callback_recycling(&self.shared, false);
        }
    }

    struct MacQueueCycle<'a> {
        queue: AudioQueueRef,
        shared: &'a Shared,
        buffers: &'a [*mut AudioQueueBuffer; BUFFER_COUNT],
    }

    impl QueueCycle for MacQueueCycle<'_> {
        fn buffer_count(&self) -> usize {
            self.buffers.len()
        }

        fn stamp_block_start(&mut self) {
            self.shared
                .block_start_us
                .store(crate::metrics::now_us(), Ordering::Relaxed);
        }

        fn render_post_cue(&mut self, index: usize) -> bool {
            let buf = self.buffers[index];
            debug_assert!(!buf.is_null());
            // SAFETY: this adapter is constructed only while every retained
            // pointer is AVAILABLE (initial allocation or after synchronous
            // immediate stop), and capacity is fixed at allocation.
            unsafe {
                let out = std::slice::from_raw_parts_mut(
                    (*buf).m_audio_data as *mut f32,
                    BUFFER_FRAMES * CHANNELS,
                );
                {
                    let mut synth = self
                        .shared
                        .synth
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    synth.render(out);
                }
                (*buf).m_audio_data_byte_size = (BUFFER_FRAMES * CHANNELS * 4) as u32;
                out.iter().any(|sample| *sample != 0.0)
            }
        }

        fn enqueue_buffer(&mut self, index: usize) -> bool {
            // SAFETY: the indexed pointer is still AVAILABLE and becomes
            // scheduled/queue-owned exactly once on success.
            unsafe {
                (api().enqueue_buffer)(self.queue, self.buffers[index], 0, std::ptr::null()) == 0
            }
        }

        fn set_callback_recycling(&mut self, enabled: bool) {
            set_callback_recycling(self.shared, enabled);
        }

        fn start_queue(&mut self) -> bool {
            // SAFETY: Queue is live and all three buffers are scheduled.
            unsafe { (api().start)(self.queue, std::ptr::null()) == 0 }
        }

        fn stop_immediate(&mut self) -> bool {
            // SAFETY: immediate=true is synchronous. On success AudioToolbox
            // reset/removal has completed and all retained pointers are again
            // available for the worker to fill.
            unsafe { (api().stop)(self.queue, 1) == 0 }
        }
    }
}

/// Bounded cue ingress capacity.
#[cfg(target_os = "macos")]
const COMMAND_CAPACITY: usize = 64;

/// One queued cue: the gesture and its side-car (host input-clock stamp, glyph
/// class, origin pan).
#[derive(Clone, Copy, Debug)]
#[cfg(target_os = "macos")]
struct Cue {
    ev: SoundEvent,
    meta: EventMeta,
}

#[cfg(target_os = "macos")]
impl From<SoundEvent> for Cue {
    fn from(ev: SoundEvent) -> Self {
        Self {
            ev,
            meta: EventMeta::default(),
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnqueueDisposition {
    Queued,
    DroppedFull,
    Disconnected,
}

#[cfg(target_os = "macos")]
fn cue_channel() -> (
    std::sync::mpsc::SyncSender<Cue>,
    std::sync::mpsc::Receiver<Cue>,
) {
    std::sync::mpsc::sync_channel(COMMAND_CAPACITY)
}

/// Milliseconds on a process-monotonic clock, for the worker's platform-busy
/// stamp.
#[cfg(target_os = "macos")]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    let elapsed =
        u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX);
    elapsed.saturating_add(WEDGE_AFTER_MS + 1)
}

/// Set on the busy stamp while the section it covers is a device open.
#[cfg(target_os = "macos")]
const BUSY_OPENING: u64 = 1 << 63;

/// RAII stamp around every worker platform section (device open, cue apply,
/// pause housekeeping).
#[cfg(target_os = "macos")]
struct PlatformBusy<'a>(&'a std::sync::atomic::AtomicU64);

#[cfg(target_os = "macos")]
impl<'a> PlatformBusy<'a> {
    fn mark(cell: &'a std::sync::atomic::AtomicU64) -> Self {
        cell.store(monotonic_ms().max(1), std::sync::atomic::Ordering::Release);
        Self(cell)
    }

    fn mark_open(cell: &'a std::sync::atomic::AtomicU64) -> Self {
        cell.store(
            monotonic_ms().max(1) | BUSY_OPENING,
            std::sync::atomic::Ordering::Release,
        );
        Self(cell)
    }
}

#[cfg(target_os = "macos")]
impl Drop for PlatformBusy<'_> {
    fn drop(&mut self) {
        self.0.store(0, std::sync::atomic::Ordering::Release);
    }
}

/// The wedge verdict: `Some(age_ms)` when the worker has been inside one
/// platform call too long, `None` for a parked worker (stamp 0) or a call
/// still young.
#[cfg(target_os = "macos")]
fn busy_stale(mark: u64, now_ms: u64) -> Option<u64> {
    if mark == 0 {
        return None;
    }
    let threshold = if mark & BUSY_OPENING != 0 {
        OPEN_WEDGE_AFTER_MS
    } else {
        WEDGE_AFTER_MS
    };
    let elapsed = now_ms.saturating_sub(mark & !BUSY_OPENING);
    (elapsed >= threshold).then_some(elapsed)
}

/// The worker's shared control words, borrowed for the loop's lifetime: the
/// shutdown request, the lifecycle state it reports and its platform-busy
/// stamp.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct WorkerFlags<'a> {
    shutdown: &'a std::sync::atomic::AtomicBool,
    state: &'a std::sync::atomic::AtomicU8,
    busy: &'a std::sync::atomic::AtomicU64,
    /// Cues the worker consumed inside a reopen backoff window.
    dropped_backoff: &'a std::sync::atomic::AtomicU64,
    /// Device reopens the worker has left, surfaced so `tone` can print it.
    reopens_left: &'a std::sync::atomic::AtomicU8,
}

/// The device the worker drives: [`mac::MacOut`], or a test double.
#[cfg(target_os = "macos")]
trait AudioWorkerOutput {
    fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> mac::Delivery;
    fn on_tick(&mut self) -> mac::Service;
    fn is_running(&self) -> bool;
}

#[cfg(target_os = "macos")]
impl AudioWorkerOutput for mac::MacOut {
    fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> mac::Delivery {
        mac::MacOut::push_meta(self, ev, meta)
    }

    fn on_tick(&mut self) -> mac::Service {
        mac::MacOut::on_tick(self)
    }

    fn is_running(&self) -> bool {
        mac::MacOut::is_running(self)
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerStart",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerPushRunning",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerStartFails",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "ServiceRunning",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "PauseIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "ParkIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
fn worker_loop<Output, Open>(
    rx: std::sync::mpsc::Receiver<Cue>,
    flags: WorkerFlags<'_>,
    seed: u32,
    housekeeping_interval: Duration,
    backoff: &[Duration],
    mut open: Open,
) where
    Output: AudioWorkerOutput,
    Open: FnMut(u32) -> Option<Output>,
{
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;

    let WorkerFlags {
        shutdown,
        state,
        busy,
        dropped_backoff,
        reopens_left,
    } = flags;
    let mut output: Option<Output> = None;
    let mut retry = RetryState::new(backoff);
    reopens_left.store(retry.left(), Ordering::Release);
    macro_rules! reopen_or_die {
        ($now:expr) => {
            match {
                let _busy = PlatformBusy::mark_open(busy);
                reopen_after_fault(&mut output, &mut retry, $now)
            } {
                Reopen::Now => {
                    reopens_left.store(retry.left(), Ordering::Release);
                    state.store(STATE_REOPENING, Ordering::Release);
                }
                Reopen::Wait => {
                    reopens_left.store(retry.left(), Ordering::Release);
                    state.store(STATE_REOPENING, Ordering::Release);
                }
                Reopen::Exhausted => {
                    reopens_left.store(0, Ordering::Release);
                    state.store(STATE_FAILED, Ordering::Release);
                    return;
                }
            }
        };
    }
    loop {
        if shutdown.load(Ordering::Acquire) {
            state.store(STATE_STOPPED, Ordering::Release);
            return;
        }
        let cue = if output.as_ref().is_some_and(AudioWorkerOutput::is_running) {
            match rx.recv_timeout(housekeeping_interval) {
                Ok(cue) => cue,
                Err(RecvTimeoutError::Timeout) => {
                    let Some(out) = output.as_mut() else {
                        continue;
                    };
                    let tick = {
                        let _busy = PlatformBusy::mark(busy);
                        out.on_tick()
                    };
                    match tick {
                        mac::Service::Running => state.store(STATE_RUNNING, Ordering::Release),
                        mac::Service::Paused => state.store(STATE_PAUSED, Ordering::Release),
                        mac::Service::Reopen => reopen_or_die!(Instant::now()),
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    state.store(STATE_STOPPED, Ordering::Release);
                    return;
                }
            }
        } else {
            #[cfg(test)]
            let _parked = ParkedMark::new(state);
            match rx.recv() {
                Ok(cue) => cue,
                Err(_) => {
                    #[cfg(test)]
                    note_disconnect_exit(state);
                    state.store(STATE_STOPPED, Ordering::Release);
                    return;
                }
            }
        };
        if shutdown.load(Ordering::Acquire) {
            state.store(STATE_STOPPED, Ordering::Release);
            return;
        }
        macro_rules! open_device {
            () => {{
                let _busy = PlatformBusy::mark_open(busy);
                open(seed)
            }};
        }
        macro_rules! push_cue {
            ($out:expr) => {{
                let _busy = PlatformBusy::mark(busy);
                $out
            }};
        }
        let now = Instant::now();
        if output.is_none() && !retry.ready(now) {
            saturating_increment_u64(dropped_backoff);
            continue;
        }
        if output.is_none() {
            output = open_device!();
            if output.is_none() {
                reopen_or_die!(now);
                saturating_increment_u64(dropped_backoff);
                continue;
            }
        }
        let delivery = push_cue!(
            output
                .as_mut()
                .map_or(mac::Delivery::Reopen, |out| out.push_meta(cue.ev, cue.meta))
        );
        if delivery == mac::Delivery::Reopen {
            reopen_or_die!(now);
            if output.is_none() && retry.ready(Instant::now()) {
                output = open_device!();
            }
            match push_cue!(output.as_mut().map(|out| out.push_meta(cue.ev, cue.meta))) {
                Some(mac::Delivery::Sounded) => {
                    retry.delivered(Instant::now());
                    reopens_left.store(retry.left(), Ordering::Release);
                    state.store(STATE_RUNNING, Ordering::Release);
                }
                _ => {
                    saturating_increment_u64(dropped_backoff);
                }
            }
            continue;
        }
        retry.delivered(Instant::now());
        reopens_left.store(retry.left(), Ordering::Release);
        state.store(STATE_RUNNING, Ordering::Release);
    }
}

/// How the cue-driven retry schedule stands right now.
#[cfg(target_os = "macos")]
struct RetryState<'a> {
    attempts: u8,
    earliest: Option<Instant>,
    /// When the device first played after the last fault.
    played_since: Option<Instant>,
    backoff: &'a [Duration],
}

/// What [`reopen_after_fault`] decided.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reopen {
    /// Reopen on this cue.
    Now,
    /// Inside the backoff window.
    Wait,
    /// The budget is spent.
    Exhausted,
}

#[cfg(target_os = "macos")]
impl<'a> RetryState<'a> {
    const fn new(backoff: &'a [Duration]) -> Self {
        Self {
            attempts: 0,
            earliest: None,
            played_since: None,
            backoff,
        }
    }

    fn healthy_for(&self) -> Duration {
        self.backoff.iter().copied().max().unwrap_or_default()
    }

    const fn left(&self) -> u8 {
        (self.backoff.len() as u8).saturating_sub(self.attempts)
    }

    fn ready(&self, now: Instant) -> bool {
        self.earliest.is_none_or(|t| now >= t)
    }

    fn delivered(&mut self, now: Instant) {
        if self.attempts == 0 {
            return;
        }
        let since = *self.played_since.get_or_insert(now);
        if now.saturating_duration_since(since) < self.healthy_for() {
            return;
        }
        self.attempts = 0;
        self.earliest = None;
        self.played_since = None;
    }
}

/// The one fault handler: drops the whole output (its `Drop` disables callback
/// recycling and disposes the queue synchronously) and arms the next attempt,
/// or reports exhaustion.
#[cfg(target_os = "macos")]
fn reopen_after_fault<Output>(
    output: &mut Option<Output>,
    retry: &mut RetryState<'_>,
    now: Instant,
) -> Reopen {
    *output = None;
    if usize::from(retry.attempts) >= retry.backoff.len() {
        return Reopen::Exhausted;
    }
    let wait = retry.backoff[usize::from(retry.attempts)];
    retry.attempts = retry.attempts.saturating_add(1);
    retry.earliest = Some(now + wait);
    retry.played_since = None;
    if wait.is_zero() {
        Reopen::Now
    } else {
        Reopen::Wait
    }
}

/// Tier-1 conformance of the derived `TrailAudioReopenLadder` machine to the
/// ladder above and to `worker_loop`'s call sites (a child module in its own
/// file, so it can drive the private ladder).
#[cfg(all(test, target_os = "macos"))]
#[path = "trail_audio_reopen_conformance.rs"]
mod reopen_conformance;

/// Saturating `+1` on a `u64` counter: `u64::MAX` stays put rather than
/// wrapping (the callback's `u32` silence counter takes the same step).
#[cfg(target_os = "macos")]
fn saturating_increment_u64(counter: &std::sync::atomic::AtomicU64) {
    use std::sync::atomic::Ordering;
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
}

/// The device the worker drives.
#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "PushCueAvailable",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_ingress"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "PushCueFull",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_ingress"
    )
)]
fn enqueue_cue(
    tx: &std::sync::mpsc::SyncSender<Cue>,
    dropped: &std::sync::atomic::AtomicU64,
    cue: Cue,
) -> EnqueueDisposition {
    use std::sync::mpsc::TrySendError;

    match tx.try_send(cue) {
        Ok(()) => EnqueueDisposition::Queued,
        Err(TrySendError::Full(_)) => {
            saturating_increment_u64(dropped);
            EnqueueDisposition::DroppedFull
        }
        Err(TrySendError::Disconnected(_)) => EnqueueDisposition::Disconnected,
    }
}

#[cfg(target_os = "macos")]
const STATE_DORMANT: u8 = 0;
#[cfg(target_os = "macos")]
const STATE_RUNNING: u8 = 1;
#[cfg(target_os = "macos")]
const STATE_PAUSED: u8 = 2;
#[cfg(target_os = "macos")]
const STATE_FAILED: u8 = 3;
#[cfg(target_os = "macos")]
const STATE_STOPPED: u8 = 4;
/// A fault was seen and the worker is inside its bounded reopen schedule.
#[cfg(target_os = "macos")]
const STATE_REOPENING: u8 = 5;

/// The audio host's state as reported by status verbs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HostState {
    /// Ingress is sealed: headless/test form, non-macOS build, or a permanent failure.
    Inert,
    /// Ingress is open and no device has been opened.
    #[cfg(any(target_os = "macos", test))]
    Opening,
    /// `AudioQueueStart` returned OK and the callback is being served.
    Running,
    /// The queue is open and started but parked at idle.
    #[cfg(any(target_os = "macos", test))]
    Paused,
    /// The worker spent its reopen budget and exited.
    #[cfg(any(target_os = "macos", test))]
    Failed,
    /// A fault was seen and the worker is inside its bounded reopen schedule.
    #[cfg(any(target_os = "macos", test))]
    Reopening,
    /// The queue was stopped.
    #[cfg(any(target_os = "macos", test))]
    Stopped,
    /// Ingress is open but the worker has been stuck inside one platform call.
    #[cfg(any(target_os = "macos", test))]
    Wedged,
}

impl HostState {
    /// Every variant, for the test that each has its own wire word (none falls back to `live`).
    #[cfg(test)]
    pub(crate) const ALL: [Self; 8] = [
        Self::Inert,
        Self::Opening,
        Self::Running,
        Self::Paused,
        Self::Failed,
        Self::Reopening,
        Self::Stopped,
        Self::Wedged,
    ];

    /// The word `tone` and `trail status` print.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Inert => "inert",
            Self::Running => "live",
            #[cfg(any(target_os = "macos", test))]
            Self::Opening => "opening",
            #[cfg(any(target_os = "macos", test))]
            Self::Paused => "paused",
            #[cfg(any(target_os = "macos", test))]
            Self::Failed => "failed",
            #[cfg(any(target_os = "macos", test))]
            Self::Reopening => "reopening",
            #[cfg(any(target_os = "macos", test))]
            Self::Stopped => "stopped",
            #[cfg(any(target_os = "macos", test))]
            Self::Wedged => "wedged",
        }
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "trail_audio_lifecycle",
        action = "ParkIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
fn worker_main(rx: std::sync::mpsc::Receiver<Cue>, flags: WorkerFlags<'_>, seed: u32) {
    worker_loop(
        rx,
        flags,
        seed,
        HOUSEKEEPING_INTERVAL,
        &REOPEN_BACKOFF,
        mac::MacOut::new,
    );
}

/// Cross-platform host face.
pub(crate) struct TrailAudio {
    #[cfg(target_os = "macos")]
    tx: Option<std::sync::mpsc::SyncSender<Cue>>,
    #[cfg(target_os = "macos")]
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Cues dropped because ingress was full.
    #[cfg(target_os = "macos")]
    dropped_full: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Cues pushed at a host whose ingress is gone.
    #[cfg(target_os = "macos")]
    dropped_sealed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Cues the worker consumed while inside a reopen backoff window.
    #[cfg(target_os = "macos")]
    dropped_backoff: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Device reopens the worker has left ([`REOPEN_BUDGET`] at birth).
    #[cfg(target_os = "macos")]
    reopens_left: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Whether this host was born active.
    #[cfg(target_os = "macos")]
    born_active: bool,
    /// The worker's platform-busy stamp (see [`PlatformBusy`]).
    #[cfg(target_os = "macos")]
    busy: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Remaining worker revivals ([`WEDGE_REVIVES`] at birth).
    #[cfg(target_os = "macos")]
    revives_left: u8,
    #[cfg(target_os = "macos")]
    worker: Option<std::thread::JoinHandle<()>>,
    /// The tests' and `crate::trail_rig`'s cue tap: `Some` makes this host report itself live.
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    capture: Option<Vec<(SoundEvent, EventMeta)>>,
}

impl TrailAudio {
    /// `active=false` is the sealed headless/test path.
    pub(crate) fn new(active: bool) -> Self {
        #[cfg(target_os = "macos")]
        {
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

            let mut host = Self {
                tx: None,
                shutdown: Arc::new(AtomicBool::new(false)),
                state: Arc::new(AtomicU8::new(STATE_DORMANT)),
                dropped_full: Arc::new(AtomicU64::new(0)),
                dropped_sealed: Arc::new(AtomicU64::new(0)),
                dropped_backoff: Arc::new(AtomicU64::new(0)),
                reopens_left: Arc::new(AtomicU8::new(REOPEN_BUDGET)),
                born_active: active,
                busy: Arc::new(AtomicU64::new(0)),
                revives_left: WEDGE_REVIVES,
                worker: None,
                #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
                capture: None,
            };
            if !active {
                return host;
            }
            let (tx, rx) = cue_channel();
            let worker_shutdown = Arc::clone(&host.shutdown);
            let worker_state = Arc::clone(&host.state);
            let worker_busy = Arc::clone(&host.busy);
            let worker_dropped_backoff = Arc::clone(&host.dropped_backoff);
            let worker_reopens_left = Arc::clone(&host.reopens_left);
            host.worker = crate::qos::spawn_lane_thread(
                "aterm-trail-audio",
                crate::qos::Role::Responsive,
                move || {
                    worker_main(
                        rx,
                        WorkerFlags {
                            shutdown: &worker_shutdown,
                            state: &worker_state,
                            busy: &worker_busy,
                            dropped_backoff: &worker_dropped_backoff,
                            reopens_left: &worker_reopens_left,
                        },
                        0x5EED_50FD,
                    );
                },
            )
            .ok();
            if host.worker.is_some() {
                host.tx = Some(tx);
            } else {
                host.state
                    .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
            }
            host
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = active;
            Self {
                #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
                capture: None,
            }
        }
    }

    /// A test host that reports live and records every cue (the `capture` field).
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    pub(crate) fn capturing_for_test() -> Self {
        let mut host = Self::new(false);
        host.capture = Some(Vec::new());
        host
    }

    /// Take the cues recorded since the last call (the tests and `crate::trail_rig`).
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    pub(crate) fn take_captured_for_test(&mut self) -> Vec<SoundEvent> {
        self.take_captured_with_meta_for_test()
            .into_iter()
            .map(|(ev, _)| ev)
            .collect()
    }

    /// [`Self::take_captured_for_test`] with each cue's side-car.
    #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
    pub(crate) fn take_captured_with_meta_for_test(&mut self) -> Vec<(SoundEvent, EventMeta)> {
        self.capture
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Replace the audio host with a freshly resolved active/inert one.
    pub(crate) fn replace(&mut self, active: bool) {
        #[cfg(target_os = "macos")]
        {
            use std::sync::atomic::Ordering;
            let full = self.dropped_full.load(Ordering::Relaxed);
            let sealed = self.dropped_sealed.load(Ordering::Relaxed);
            let backoff = self.dropped_backoff.load(Ordering::Relaxed);
            let revives_left = self.revives_left;
            *self = Self::new(active);
            self.dropped_full.store(full, Ordering::Relaxed);
            self.dropped_sealed.store(sealed, Ordering::Relaxed);
            self.dropped_backoff.store(backoff, Ordering::Relaxed);
            self.revives_left = revives_left;
        }
        #[cfg(not(target_os = "macos"))]
        {
            *self = Self::new(active);
        }
    }

    /// Queue one cue without blocking the input/present thread.
    pub(crate) fn push(&mut self, ev: SoundEvent) {
        self.push_meta(ev, EventMeta::default());
    }

    /// [`Self::push`] with the side-car (host input-clock stamp and glyph class).
    pub(crate) fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) {
        #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
        if let Some(captured) = self.capture.as_mut() {
            captured.push((ev, meta));
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let disposition = self
                .tx
                .as_ref()
                .map(|tx| enqueue_cue(tx, &self.dropped_full, Cue { ev, meta }));
            match disposition {
                Some(EnqueueDisposition::Disconnected) => {
                    saturating_increment_u64(&self.dropped_sealed);
                    self.tx = None;
                    self.state
                        .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
                }
                None if self.born_active => {
                    saturating_increment_u64(&self.dropped_sealed);
                }
                _ => {}
            }
            if self.tx.is_some() && self.busy_stale_ms().is_some() {
                self.revive_or_seal();
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (ev, meta);
    }

    /// Whether cues pushed here can ever reach a device.
    pub const fn is_live(&self) -> bool {
        #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
        let capturing = self.capture.is_some();
        #[cfg(not(any(test, all(unix, feature = "bench-support", debug_assertions))))]
        let capturing = false;
        #[cfg(target_os = "macos")]
        let bound = self.tx.is_some();
        #[cfg(not(target_os = "macos"))]
        let bound = false;
        capturing || bound
    }

    /// `Some(elapsed)` when the worker's current platform call has outlived its
    /// threshold ([`busy_stale`]); `None` when parked, between calls, or still young.
    #[cfg(target_os = "macos")]
    fn busy_stale_ms(&self) -> Option<u64> {
        busy_stale(
            self.busy.load(std::sync::atomic::Ordering::Acquire),
            monotonic_ms(),
        )
    }

    /// Wedged: ingress is open ([`Self::is_live`] is `true`) but the worker has been stuck.
    pub(crate) fn wedged_for(&self) -> Option<Duration> {
        #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
        if self.capture.is_some() {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            self.tx
                .as_ref()
                .and_then(|_| self.busy_stale_ms())
                .map(Duration::from_millis)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// The host's state for a status row.
    pub(crate) fn host_state(&self) -> HostState {
        #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
        if self.capture.is_some() {
            return HostState::Running;
        }
        #[cfg(target_os = "macos")]
        {
            if self.tx.is_none() {
                return HostState::Inert;
            }
            if self.busy_stale_ms().is_some() {
                return HostState::Wedged;
            }
            match self.state.load(std::sync::atomic::Ordering::Acquire) {
                STATE_RUNNING => HostState::Running,
                STATE_PAUSED => HostState::Paused,
                STATE_FAILED => HostState::Failed,
                STATE_REOPENING => HostState::Reopening,
                STATE_STOPPED => HostState::Stopped,
                _ => HostState::Opening,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            HostState::Inert
        }
    }

    /// Device reopens the worker has left.
    pub(crate) fn reopens_left(&self) -> u8 {
        #[cfg(target_os = "macos")]
        {
            self.reopens_left.load(std::sync::atomic::Ordering::Acquire)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// How many wedge revivals this host has spent.
    pub(crate) fn revives_spent(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            u64::from(WEDGE_REVIVES.saturating_sub(self.revives_left))
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Saturating count of dropped cues.
    pub(crate) fn dropped_cues(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            use std::sync::atomic::Ordering;
            self.dropped_full
                .load(Ordering::Relaxed)
                .saturating_add(self.dropped_sealed.load(Ordering::Relaxed))
                .saturating_add(self.dropped_backoff.load(Ordering::Relaxed))
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Abandon a wedged worker and spawn a replacement if revival budget remains, or seal ingress.
    #[cfg(target_os = "macos")]
    fn revive_or_seal(&mut self) {
        if self.revives_left == 0 {
            self.tx = None;
            self.state
                .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
            return;
        }
        let revives_left = self.revives_left - 1;
        self.replace(true);
        self.revives_left = revives_left;
    }

    #[cfg(test)]
    pub(crate) fn is_inert_for_test(&self) -> bool {
        // A capturing host accepts cues, so it is not inert; this stays the
        // complement of `is_live`'s capture arm.
        #[cfg(target_os = "macos")]
        let unbound = self.tx.is_none() && self.worker.is_none();
        #[cfg(not(target_os = "macos"))]
        let unbound = true;
        self.capture.is_none() && unbound
    }

    #[cfg(all(test, target_os = "macos"))]
    fn state(&self) -> u8 {
        self.state.load(std::sync::atomic::Ordering::Acquire)
    }

    /// An active host with no worker whose ingress is a real channel the test
    /// holds; it exercises the enqueue path, not the capture tap.
    #[cfg(all(test, target_os = "macos"))]
    fn test_ingress() -> (Self, std::sync::mpsc::Receiver<Cue>) {
        let (tx, rx) = cue_channel();
        let mut host = Self::new(false);
        host.tx = Some(tx);
        host.born_active = true;
        (host, rx)
    }
}

impl Drop for TrailAudio {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Release);
            self.tx = None;
            let Some(worker) = self.worker.take() else {
                return;
            };
            if self.busy_stale_ms().is_some() {
                #[cfg(test)]
                LAST_TEARDOWN.with(|last| last.set(Some(WorkerTeardown::DetachedWedged)));
                drop(worker);
                return;
            }
            #[cfg(test)]
            CLOSED_BEFORE_WAIT.with(|closed| closed.set(Some(self.tx.is_none())));
            let deadline = std::time::Instant::now() + detach_deadline();
            while !worker.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(DETACH_POLL);
            }
            let joined = worker.is_finished();
            if joined {
                let _ = worker.join();
            }
            #[cfg(test)]
            LAST_TEARDOWN.with(|last| {
                last.set(Some(if joined {
                    WorkerTeardown::Joined
                } else {
                    WorkerTeardown::DetachedAtDeadline
                }));
            });
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "trail_audio_trail_audio_conformance_tests.rs"]
pub(crate) mod trail_audio_conformance;

#[cfg(all(test, target_os = "macos"))]
#[path = "trail_audio_tests.rs"]
mod tests;
