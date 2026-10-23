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

/// Output sample rate.
#[cfg(target_os = "macos")]
const SAMPLE_RATE: f64 = 48_000.0;

/// Frames per queue buffer (~10.7 ms at 48 kHz), 3 buffers in flight.
#[cfg(target_os = "macos")]
const BUFFER_FRAMES: usize = 512;
#[cfg(target_os = "macos")]
const BUFFER_COUNT: usize = 3;

/// Consecutive silent buffers before the queue pauses: 140 buffers is about
/// 1.5 s of exact silence (the synth's beds are already at zero, so this cannot
/// clip a tail).
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
    /// The last teardown a drop on this thread performed.
    static LAST_TEARDOWN: std::cell::Cell<Option<WorkerTeardown>> =
        const { std::cell::Cell::new(None) };
    /// A longer [`DETACH_DEADLINE`] for drops on this thread, so a slow
    /// scheduler cannot turn an expected join into a detach.
    static DETACH_DEADLINE_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
    /// Whether the last drop on this thread had already closed the cue channel
    /// when it began waiting for its worker — the wake a parked worker needs.
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
/// [`WEDGE_AFTER_MS`], because an open waits on hardware (a Bluetooth output
/// negotiating, a device switch, coreaudiod restarting after wake).
#[cfg(target_os = "macos")]
const OPEN_WEDGE_AFTER_MS: u64 = WEDGE_AFTER_MS * 5;

/// How long without a render callback counts as a stall: the queue is running
/// and was armed by a successful start, yet `render_cb` has not run (a
/// coreaudiod restart, a vanished device, a post-sleep invalidation).
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
    pub(super) fn callback_stalled(running: bool, last_cb_us: u64, now_us: u64) -> bool {
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
    pub(super) fn running_probe(
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
        pub(super) fn armed(&mut self, armed_cb_us: u64) {
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

    /// Opaque AudioToolbox handles (never dereferenced in Rust).
    type AudioQueueRef = *mut c_void;

    /// `AudioStreamBasicDescription` as CoreAudioTypes lays it out.
    #[repr(C)]
    struct AudioStreamBasicDescription {
        m_sample_rate: f64,
        m_format_id: u32,
        m_format_flags: u32,
        m_bytes_per_packet: u32,
        m_frames_per_packet: u32,
        m_bytes_per_frame: u32,
        m_channels_per_frame: u32,
        m_bits_per_channel: u32,
        m_reserved: u32,
    }

    /// `AudioQueueBuffer` header; `mAudioData` points at the sample storage
    /// the queue allocated alongside it.
    #[repr(C)]
    struct AudioQueueBuffer {
        m_audio_data_bytes_capacity: u32,
        m_audio_data: *mut c_void,
        m_audio_data_byte_size: u32,
        m_user_data: *mut c_void,
        m_packet_description_capacity: u32,
        m_packet_descriptions: *mut c_void,
        m_packet_description_count: u32,
    }

    type AudioQueueOutputCallback =
        extern "C" fn(*mut c_void, AudioQueueRef, *mut AudioQueueBuffer);

    /// `AudioQueuePropertyID` (a four-char code).
    type AudioQueuePropertyID = u32;

    /// `AudioQueuePropertyListenerProc`.
    type AudioQueuePropertyListenerProc =
        extern "C" fn(*mut c_void, AudioQueueRef, AudioQueuePropertyID);

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        fn AudioQueueNewOutput(
            in_format: *const AudioStreamBasicDescription,
            in_callback_proc: AudioQueueOutputCallback,
            in_user_data: *mut c_void,
            in_callback_run_loop: *const c_void,
            in_run_loop_mode: *const c_void,
            in_flags: u32,
            out_aq: *mut AudioQueueRef,
        ) -> i32;
        fn AudioQueueAllocateBuffer(
            in_aq: AudioQueueRef,
            in_buffer_byte_size: u32,
            out_buffer: *mut *mut AudioQueueBuffer,
        ) -> i32;
        fn AudioQueueEnqueueBuffer(
            in_aq: AudioQueueRef,
            in_buffer: *mut AudioQueueBuffer,
            in_num_packet_descs: u32,
            in_packet_descs: *const c_void,
        ) -> i32;
        fn AudioQueueStart(in_aq: AudioQueueRef, in_start_time: *const c_void) -> i32;
        fn AudioQueueStop(in_aq: AudioQueueRef, in_immediate: u8) -> i32;
        fn AudioQueueAddPropertyListener(
            in_aq: AudioQueueRef,
            in_id: AudioQueuePropertyID,
            in_proc: AudioQueuePropertyListenerProc,
            in_user_data: *mut c_void,
        ) -> i32;
        fn AudioQueueGetProperty(
            in_aq: AudioQueueRef,
            in_id: AudioQueuePropertyID,
            out_data: *mut c_void,
            io_data_size: *mut u32,
        ) -> i32;
    }

    /// `kAudioQueueProperty_IsRunning` ('aqrn').
    pub(super) const PROP_IS_RUNNING: AudioQueuePropertyID = 0x6171_726E;

    /// `kAudioFormatLinearPCM` ('lpcm').
    const FORMAT_LPCM: u32 = 0x6C70_636D;
    /// `kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked`.
    const FLAGS_FLOAT_PACKED: u32 = 1 | 8;

    /// State shared with the queue's callback thread.
    struct Shared {
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
        fn new(seed: u32) -> Self {
            Self {
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

        /// The one death verdict, read from this queue's shared words; both
        /// [`MacOut::push_meta`] and [`MacOut::on_tick`] call it.
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

    /// THE ONE DOOR TO A QUEUE'S TEARDOWN, by construction:
    /// `AudioQueueDispose` and `AudioQueueRemovePropertyListener` are declared
    /// in this module and nowhere else, so nothing outside it can name them; a
    /// queue `AudioQueueNewOutput` made is owned the instant [`own`] wraps it,
    /// [`AudioToolboxQueue`]'s fields are this module's, and the owner hands
    /// out only `&` access to its calls, so [`OwnedQueue`]'s `Drop` is the one
    /// caller of `dispose` and of the listener's removal.
    mod queue_owner {
        use std::ffi::c_void;
        use std::sync::Arc;

        use super::{
            AudioQueueAddPropertyListener, AudioQueueGetProperty, AudioQueuePropertyID,
            AudioQueuePropertyListenerProc, AudioQueueRef, PROP_IS_RUNNING, Shared,
            running_listener,
        };

        #[link(name = "AudioToolbox", kind = "framework")]
        unsafe extern "C" {
            fn AudioQueueDispose(in_aq: AudioQueueRef, in_immediate: u8) -> i32;
            fn AudioQueueRemovePropertyListener(
                in_aq: AudioQueueRef,
                in_id: AudioQueuePropertyID,
                in_proc: AudioQueuePropertyListenerProc,
                in_user_data: *mut c_void,
            ) -> i32;
        }

        /// Own the queue `AudioQueueNewOutput` just made.
        pub(super) fn own(
            queue: AudioQueueRef,
            user: *const Shared,
        ) -> OwnedQueue<AudioToolboxQueue> {
            OwnedQueue::new(AudioToolboxQueue { queue, user })
        }

        /// The platform calls that bracket a queue's life.
        pub(in crate::trail_audio) trait QueueOwnerCalls {
            /// Register the `IsRunning` listener.
            fn add_running_listener(&mut self) -> bool;
            fn remove_running_listener(&mut self);
            /// `AudioQueueDispose(queue, immediate = true)` — synchronous.
            fn dispose(&mut self);
            /// Release the `Arc<Shared>` refcount handed to AudioToolbox as the
            /// callbacks' userdata.
            fn release_user(&mut self);
        }

        /// The one owner of a live audio queue, and the only place that disposes one.
        pub(in crate::trail_audio) struct OwnedQueue<P: QueueOwnerCalls> {
            calls: P,
            listening: bool,
        }

        impl<P: QueueOwnerCalls> OwnedQueue<P> {
            pub(in crate::trail_audio) fn new(calls: P) -> Self {
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

            pub(in crate::trail_audio) fn calls(&self) -> &P {
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
            pub(super) fn queue(&self) -> AudioQueueRef {
                self.queue
            }

            /// The listener's userdata (the edge field inside the pinned
            /// allocation).
            fn edge_user(&self) -> *mut c_void {
                // SAFETY: `user` is a live `Arc<Shared>` raw pointer until `release_user`.
                unsafe { std::ptr::addr_of!((*self.user).running_edge) as *mut c_void }
            }

            /// Read `kAudioQueueProperty_IsRunning`.
            pub(super) fn read_is_running(&self) -> Option<bool> {
                let mut value: u32 = 0;
                let mut size = std::mem::size_of::<u32>() as u32;
                // SAFETY: The queue is live (owned, not yet disposed).
                let status = unsafe {
                    AudioQueueGetProperty(
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
                // SAFETY: The queue is live.
                unsafe {
                    AudioQueueAddPropertyListener(
                        self.queue,
                        PROP_IS_RUNNING,
                        running_listener,
                        self.edge_user(),
                    ) == 0
                }
            }

            fn remove_running_listener(&mut self) {
                // SAFETY: The queue is live (dispose has not run).
                unsafe {
                    AudioQueueRemovePropertyListener(
                        self.queue,
                        PROP_IS_RUNNING,
                        running_listener,
                        self.edge_user(),
                    );
                }
            }

            fn dispose(&mut self) {
                // SAFETY: Synchronous dispose (immediate = 1) stops the callback thread.
                unsafe {
                    AudioQueueDispose(self.queue, 1);
                }
            }

            fn release_user(&mut self) {
                // SAFETY: `user` came from `Arc::into_raw` in `MacOut::new` and is released once.
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
        /// Stamp the block admission boundary for the buffers about to be
        /// primed — the same stamp the render callback takes before each
        /// block, taken once here because the prime renders every buffer in
        /// one breath.
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
        // SAFETY: `user` is the `Arc<Shared>` raw pointer installed at queue creation.
        let shared = unsafe { &*(user as *const Shared) };
        shared
            .last_cb_us
            .store(crate::metrics::now_us(), Ordering::Relaxed);
        // SAFETY: The queue hands us a buffer allocated with `BUFFER_FRAMES * CHANNELS` `f32`s.
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
            if AudioQueueEnqueueBuffer(q, buf, 0, std::ptr::null()) != 0 {
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

    // SAFETY: `queue` is only touched from the audio worker (start/stop/dispose).
    unsafe impl Send for MacOut {}

    impl MacOut {
        /// Open the output queue and allocate, but do not enqueue, its buffers.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerStart",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub(crate) fn new(seed: u32) -> Option<Self> {
            let shared = Arc::new(Shared::new(seed));
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
            // SAFETY: Fmt/queue outlive the call.
            let st = unsafe {
                AudioQueueNewOutput(
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
                // SAFETY: Reclaim the refcount handed to the uncreated queue so it is not leaked.
                unsafe { drop(Arc::from_raw(user as *const Shared)) };
                return None;
            }
            let mut owned = queue_owner::own(queue, user as *const Shared);
            owned.listen();
            let bytes = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            let mut buffers = [std::ptr::null_mut(); BUFFER_COUNT];
            for slot in &mut buffers {
                let mut buf: *mut AudioQueueBuffer = std::ptr::null_mut();
                // SAFETY: Queue is live.
                let st = unsafe { AudioQueueAllocateBuffer(queue, bytes, &mut buf) };
                if st != 0 || buf.is_null() {
                    // `owned` drops here: listener, dispose, refcount.
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
                // A failed lazy open. The cue is in the discarded synth, which
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

        /// The death verdict for this device (see
        /// [`Shared::device_needs_reopen`]).
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
            // Disable callback recycling first. `owned`'s `Drop` then removes
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
            // SAFETY: This adapter is constructed only while every retained pointer is available.
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
            // SAFETY: The indexed pointer is still available and becomes scheduled/queue-owned.
            unsafe {
                AudioQueueEnqueueBuffer(self.queue, self.buffers[index], 0, std::ptr::null()) == 0
            }
        }

        fn set_callback_recycling(&mut self, enabled: bool) {
            set_callback_recycling(self.shared, enabled);
        }

        fn start_queue(&mut self) -> bool {
            // SAFETY: Queue is live and all three buffers are scheduled.
            unsafe { AudioQueueStart(self.queue, std::ptr::null()) == 0 }
        }

        fn stop_immediate(&mut self) -> bool {
            // SAFETY: Immediate=true is synchronous.
            unsafe { AudioQueueStop(self.queue, 1) == 0 }
        }
    }
}

/// Bounded cue ingress.
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

    /// Mark a device open: the same stamp, judged at [`OPEN_WEDGE_AFTER_MS`].
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
    fn new(backoff: &'a [Duration]) -> Self {
        Self {
            attempts: 0,
            earliest: None,
            played_since: None,
            backoff,
        }
    }

    /// How long a device must play after its last fault before the ladder
    /// resets: the schedule's longest step, so the compressed test schedule
    /// scales with it.
    fn healthy_for(&self) -> Duration {
        self.backoff.iter().copied().max().unwrap_or_default()
    }

    /// Reopens remaining in the lifetime budget.
    fn left(&self) -> u8 {
        (self.backoff.len() as u8).saturating_sub(self.attempts)
    }

    /// Whether a cue arriving now may spend an attempt.
    fn ready(&self, now: Instant) -> bool {
        self.earliest.is_none_or(|t| now >= t)
    }

    /// A cue sounded.
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
    pub(crate) fn label(self) -> &'static str {
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
    /// Test-only cue tap: `Some` makes this host report itself live.
    #[cfg(test)]
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
                #[cfg(test)]
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
                #[cfg(test)]
                capture: None,
            }
        }
    }

    /// A test host that reports live and records every cue (the `capture` field).
    #[cfg(test)]
    pub(crate) fn capturing_for_test() -> Self {
        let mut host = Self::new(false);
        host.capture = Some(Vec::new());
        host
    }

    /// Take the cues recorded since the last call (test-only).
    #[cfg(test)]
    pub(crate) fn take_captured_for_test(&mut self) -> Vec<SoundEvent> {
        self.take_captured_with_meta_for_test()
            .into_iter()
            .map(|(ev, _)| ev)
            .collect()
    }

    /// [`Self::take_captured_for_test`] with each cue's side-car.
    #[cfg(test)]
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
        #[cfg(test)]
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
                    // A push at an already-sealed host; counted only for a
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
    pub fn is_live(&self) -> bool {
        #[cfg(test)]
        if self.capture.is_some() {
            return true;
        }
        #[cfg(target_os = "macos")]
        {
            self.tx.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
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
        #[cfg(test)]
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
        #[cfg(test)]
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

    /// A wedge was observed at the ingress.
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
        if self.capture.is_some() {
            return false;
        }
        #[cfg(target_os = "macos")]
        {
            self.tx.is_none() && self.worker.is_none()
        }
        #[cfg(not(target_os = "macos"))]
        {
            true
        }
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
pub(crate) mod trail_audio_conformance {
    use aterm_spec::interp::State;

    /// Project the bounded lifecycle fields shared by the worker/device actions.
    #[allow(
        clippy::too_many_arguments,
        reason = "named scalar projection mirrors the derived model variables exactly"
    )]
    pub(crate) fn project_worker(
        queued: usize,
        dropped: u64,
        last_full: bool,
        running: bool,
        silent: u32,
        service_deadline: bool,
        cue_applied: bool,
        failed: bool,
        paused: bool,
    ) -> State {
        [
            ("queued", queued.min(2) as i64),
            ("dropped", dropped.min(2) as i64),
            ("last_full", i64::from(last_full)),
            ("running", i64::from(running)),
            ("silent", i64::from(silent.min(2))),
            ("service_deadline", i64::from(service_deadline)),
            ("cue_applied", i64::from(cue_applied)),
            ("failed", i64::from(failed)),
            ("paused", i64::from(paused)),
            ("ui_blocked", 0),
            ("ui_platform_calls", 0),
        ]
        .into_iter()
        .collect()
    }

    pub(crate) fn project_ingress(queued: usize, dropped: u64, last_full: bool) -> State {
        project_worker(
            queued, dropped, last_full, false, 0, false, false, false, false,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "exact scalar projection of the queue ownership/latency model"
    )]
    pub(crate) fn project_start_latency(
        phase: i64,
        available: usize,
        queued: usize,
        recycling: bool,
        running: bool,
        audible_buffer: usize,
        unsafe_writes: usize,
        generation: u64,
        callback_generation: u64,
        stale_enqueues: usize,
        enqueue_in_flight: bool,
        stop_overlaps: usize,
    ) -> State {
        [
            ("phase", phase),
            ("available", available as i64),
            ("queued", queued as i64),
            ("recycling", i64::from(recycling)),
            ("running", i64::from(running)),
            ("audible_buffer", audible_buffer as i64),
            ("unsafe_writes", unsafe_writes.min(1) as i64),
            ("generation", generation as i64),
            ("callback_generation", callback_generation as i64),
            ("stale_enqueue", stale_enqueues.min(1) as i64),
            ("enqueue_in_flight", i64::from(enqueue_in_flight)),
            ("stop_overlap", stop_overlaps.min(1) as i64),
        ]
        .into_iter()
        .collect()
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
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
        HostState, LAST_TEARDOWN, ParkedMark, REOPEN_BUDGET, STATE_DORMANT, STATE_FAILED,
        STATE_PAUSED, STATE_REOPENING, STATE_RUNNING, STATE_STOPPED, TrailAudio, WEDGE_REVIVES,
        WorkerFlags, WorkerTeardown, cue_channel, monotonic_ms, worker_loop,
    };

    /// The state word reaches the status verb: a process that never opened a
    /// device does not print `live`.
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

    /// The word a `HostState` reaches `tone` as, the same spelling the row
    /// prints.
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

    /// Wait for the worker thread to reach a state: a one-minute hang detector
    /// polled every millisecond, so a correct run leaves as soon as it holds.
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

    /// Deterministic AudioQueue seam with no callback thread.
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

        /// The render callback's half of the stamp: a block begins now.
        fn callback_renders_block(&mut self) {
            assert!(self.running && self.recycling);
            self.block_start_us = self.clock_us;
        }

        /// What `MacOut::push_meta` would hand the synth for a cue at the
        /// fake's current instant.
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

    /// Tier-1 binding for `TrailAudioStartLatency`: drive MacOut's generic
    /// prime/stop helpers with a callback-free queue that checks buffer
    /// ownership on every write.
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

    /// Cues in the first block after a resume prime measure their pre-roll
    /// against the prime, not the last block rendered before the park.
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

    /// What [`spawn_fake_worker`] hands back: cue ingress, the shutdown flag,
    /// the state and busy cells, and the worker's join handle.
    type FakeWorkerHandles = (
        std::sync::mpsc::SyncSender<Cue>,
        Arc<AtomicBool>,
        Arc<AtomicU8>,
        Arc<std::sync::atomic::AtomicU64>,
        std::thread::JoinHandle<()>,
        Arc<std::sync::atomic::AtomicU64>,
        Arc<AtomicU8>,
    );

    /// The shipping `REOPEN_BACKOFF` compressed to milliseconds, so the retry
    /// schedule is exercised at test speed; same length, so the budget under
    /// test is the shipping budget.
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

    /// Tier-1: drive the shipping `TrailAudio::push` and check its
    /// available/full decisions against the derived model's transitions.
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

    /// Tier-1 worker lifecycle with an injected deterministic device: the
    /// shipping worker loop does no open/timer work before the first cue,
    /// starts once, pauses onto a blocking receive, and resumes the same
    /// backend on the next cue.
    #[test]
    fn worker_lifecycle_conforms_and_parks_without_polling() {
        use super::trail_audio_conformance::project_worker;

        let shared = Arc::new(FakeShared::default());
        let (tx, shutdown, state, _busy, worker, _dropped_backoff, _reopens_left) =
            spawn_fake_worker(Arc::clone(&shared));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(shared.opens.load(Ordering::Relaxed), 0);
        assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
        assert_eq!(shared.ticks.load(Ordering::Relaxed), 0);

        tx.send(cue().into()).unwrap();
        wait_until("worker start", || {
            state.load(Ordering::Acquire) == STATE_RUNNING
                && shared.pushes.load(Ordering::Relaxed) == 1
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
        std::thread::sleep(std::time::Duration::from_millis(12));
        let paused_deadline = shared.ticks.load(Ordering::Relaxed) != ticks_at_pause;
        assert!(
            !paused_deadline,
            "paused worker blocks on recv instead of polling a timeout"
        );
        let before_pause =
            project_worker(0, 0, false, true, 2, running_deadline, false, false, false);
        let after_pause =
            project_worker(0, 0, false, false, 2, paused_deadline, false, false, true);
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
        let polling_pause =
            project_worker(0, 0, false, false, 2, running_deadline, false, false, true);
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
            state.load(Ordering::Acquire) == STATE_RUNNING
                && shared.pushes.load(Ordering::Relaxed) == 2
        });
        assert_eq!(shared.opens.load(Ordering::Relaxed), 1);

        shutdown.store(true, Ordering::Release);
        let _ = tx.send(cue().into());
        worker.join().unwrap();
        assert_eq!(state.load(Ordering::Acquire), STATE_STOPPED);
    }

    /// A failed device open, start or enqueue is retried on the reopen ladder
    /// instead of sealing ingress; budget exhaustion is still explicit,
    /// terminal and observable.
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
        while state.load(Ordering::Acquire) != STATE_FAILED && std::time::Instant::now() < deadline
        {
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

    /// A key discovers a faulted or stalled queue on the push path (not only
    /// on the idle tick), and that key is still heard.
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

    /// Negative control for the two above: a fake that never stalls reopens
    /// zero times.
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

    /// The budget is spendable by a device that opens and never plays
    /// (`AudioQueueNewOutput` succeeds, start or enqueue does not): the ladder
    /// resets only on a delivery past a healthy window, so this reaches
    /// `Reopen::Exhausted` instead of reopening per keystroke forever.
    #[test]
    fn a_device_that_opens_but_never_plays_spends_the_budget_and_stops() {
        let shared = Arc::new(FakeShared::default());
        shared.fail_push.store(true, Ordering::Relaxed);
        let (tx, _shutdown, state, _busy, worker, dropped_backoff, reopens_left) =
            spawn_fake_worker(Arc::clone(&shared));

        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while state.load(Ordering::Acquire) != STATE_FAILED && std::time::Instant::now() < deadline
        {
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

    /// A slow device open is not a wedge.
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

    /// The stall verdict against a fake clock: a stopped queue and a
    /// never-armed one are not stalls.
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

    /// The reopen schedule is cue-driven and respects its window, so it never
    /// busy-loops or wakes an idle worker.
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

    /// A host born inert (`new(false)`, ingress `None` from birth) counts no
    /// drops.
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

    /// The busy stamp is nonzero and stable while the worker sits inside one
    /// call, and zero once it parks.
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

    /// A full channel alone is a burst, not a wedge: with the busy stamp
    /// young (or clear), the drop is accounted and nothing is revived.
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

    /// Channel full and the worker stuck inside one platform call past the
    /// threshold: one push reports the wedge and spends a revival (fresh
    /// channel, fresh worker, old ingress dead).
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

    /// An exhausted revival budget seals ingress: `is_live` goes false, so
    /// every status verb reads inert instead of eternally-healthy silence.
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

    /// Dropping a wedged host never blocks the calling (event-loop) thread: the
    /// stuck worker is detached, not joined.
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

    /// A parked shipping worker is woken by the closing channel and joined.
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

    /// `push` is `push_meta` with the identity side-car, and a stamped cue
    /// reaches the host with the stamp the seam gave it.
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

    /// The worker hands the synth exactly the side-car the host queued.
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

    /// Fire the real listener body at `edge`, exactly as AudioToolbox would:
    /// the userdata is the edge's address and the id the one registered.
    fn fire(edge: &RunningEdge, id: u32) {
        running_listener(
            std::ptr::from_ref(edge).cast_mut().cast::<c_void>(),
            std::ptr::null_mut(),
            id,
        );
    }

    /// A read that must never happen: the healthy key path makes no platform
    /// call, and neither does an edge the worker can prove is its own stop.
    fn no_read() -> Option<bool> {
        panic!("IsRunning was read on a path that must make no platform call")
    }

    /// The listener's body is one store, and it answers only for the property
    /// it was registered on: a stray id or null userdata does nothing.
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

    /// Every cell of the `IsRunning` verdict.
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

    /// A queue AudioToolbox stops under us is seen on the next service call
    /// (push or tick), before the callback watchdog.
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

    /// Negative control: a healthy running queue is never reopened by the
    /// listener, and paths that must make no platform call make none.
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

    /// A start still taking reads stopped too: the verdict re-raises the edge
    /// and looks again instead of reopening, and a new start forgets the last.
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

    /// The callback's `faulted` latch and the callback watchdog answer through
    /// the same verdict, ahead of the listener and without a platform read.
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

    /// The teardown order observed through a test double: what the owner's
    /// `Drop` does, in order.
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

    /// Listener removal before dispose, dispose before release, on every way a
    /// queue ends.
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

    /// The key path through the shipping ladder: a healthy queue whose
    /// listener fires is not reopened; one stopped under us is reopened by
    /// the next key, and that key is heard.
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

    /// The tick path: with nobody typing, a stop under us is seen by the next
    /// housekeeping tick and the host reads `reopening`; ticks over a healthy
    /// queue reopen nothing.
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
}
