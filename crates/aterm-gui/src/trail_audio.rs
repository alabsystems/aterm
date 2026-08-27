// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail audio OUTPUT — the host half of the trail sound effects: owns the
//! platform audio queue and feeds it from the pure
//! [`aterm_effects::trail_sound::TrailSynth`].
//!
//! Split of responsibilities (the bell discipline, `bell.rs`-style): the
//! synth owns the *what* (every sample, deterministically); this module owns
//! the *how* — a real-time output device, its callback thread, and the
//! power policy (the queue synchronously STOPS whenever the synth reports exact
//! silence, reclaiming its buffers so an idle terminal runs zero audio work).
//!
//! macOS backend: AudioToolbox's `AudioQueue`, hand-rolled flat-C FFI in the
//! house style (see `mod cg_capture` / `hdr_win.rs`): AudioToolbox is a stock
//! system framework, so the `#[link]` below adds bindings, not a dependency.
//! Three 512-frame float buffers (~10 ms each) survive scheduling hiccups, but
//! they are never pre-enqueued as silence: cold start and idle resume render
//! every available buffer only after the cue has entered the synth, so audible
//! samples begin in the first buffer instead of behind a ~32 ms silent FIFO.
//! The queue's own thread drives the render callback; a dormant audio worker
//! accepts nonblocking UI cues and owns all platform control calls.
//!
//! Other platforms: an inert stub with the same API — the synth itself is
//! pure and portable, so a WASAPI/ALSA twin can land behind this seam
//! without touching any call site.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Duration;

use aterm_effects::trail_sound::SoundEvent;

/// Whether this build has a real platform audio-output host. The synth is
/// portable, but non-macOS [`TrailAudio`] implementations intentionally discard
/// cues; Settings uses this exact capability to describe the saved toggle
/// without claiming that sound is currently playing.
#[must_use]
pub(crate) const fn output_available() -> bool {
    cfg!(target_os = "macos")
}

/// Output sample rate. 48 kHz is the native rate of every modern Mac output
/// path; the hardware resampler handles the rest.
const SAMPLE_RATE: f64 = 48_000.0;

/// Frames per queue buffer (~10.7 ms at 48 kHz) × 3 buffers in flight. Start
/// latency is bounded by ONE such buffer; the other two are post-cue audio,
/// never silent priming ahead of it.
const BUFFER_FRAMES: usize = 512;
const BUFFER_COUNT: usize = 3;

/// Consecutive silent buffers before the queue pauses: ~48 buffers ≈ 0.5 s
/// of exact digital silence (the synth's beds have already snapped to zero
/// by then, so this can never clip a tail).
const PAUSE_AFTER_SILENT: u32 = 48;

/// Event-loop housekeeping cadence while the AudioQueue is running. The queue
/// callback already does the sample work; this low-rate one-shot only observes
/// its exact-silence counter and pauses the device. It is armed only while audio
/// is live and self-disarms after a successful pause.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(250);

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use aterm_effects::trail_sound::{CHANNELS, SoundEvent, TrailSynth};

    use super::{BUFFER_COUNT, BUFFER_FRAMES, PAUSE_AFTER_SILENT, SAMPLE_RATE};

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

    // AudioToolbox is a stock system framework, in the dyld shared cache of
    // every macOS install — this adds bindings, not a dependency.
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
        fn AudioQueueDispose(in_aq: AudioQueueRef, in_immediate: u8) -> i32;
    }

    /// `kAudioFormatLinearPCM` ('lpcm').
    const FORMAT_LPCM: u32 = 0x6C70_636D;
    /// `kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked`.
    const FLAGS_FLOAT_PACKED: u32 = 1 | 8;

    fn saturating_increment(counter: &AtomicU32) {
        let mut current = counter.load(Ordering::Relaxed);
        while current != u32::MAX {
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// State shared with the queue's callback thread.
    struct Shared {
        synth: Mutex<TrailSynth>,
        /// Consecutive buffers of exact silence (reset by any audible one).
        silent: AtomicU32,
        /// Whether the queue is currently started (vs stopped). Written by the
        /// audio worker only; read for the idle-stop policy.
        running: AtomicBool,
        /// Callback-side queue failure. The worker observes this and disposes
        /// the device once; a broken callback must not spin or retry per cue.
        faulted: AtomicBool,
        /// Whether returned buffers may be rendered and re-enqueued. The worker
        /// clears this before synchronous stop/reset, because AudioToolbox can
        /// invoke output callbacks while flushing scheduled buffers and forbids
        /// enqueue during that operation.
        /// Even = disabled/stopped, odd = one running generation. Incrementing
        /// at every enable/disable prevents a callback from an old generation
        /// observing a later resume and re-enqueueing the same pointer twice.
        recycle_epoch: AtomicU64,
        /// Serializes the callback's final epoch check + enqueue with the
        /// worker's disable transition. Without this gate, stop could begin in
        /// the few instructions between an atomic check and the FFI enqueue.
        recycle_gate: Mutex<()>,
    }

    fn set_callback_recycling(shared: &Shared, enabled: bool) {
        // Stop calls this before AudioQueueStop. Taking the same gate as the
        // callback means every earlier enqueue has returned before the epoch
        // becomes disabled, and no later enqueue can pass the check below.
        let _gate = match shared.recycle_gate.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let epoch = shared.recycle_epoch.load(Ordering::Relaxed);
        if (epoch & 1 == 1) != enabled {
            shared
                .recycle_epoch
                .store(epoch.wrapping_add(1), Ordering::Release);
        }
    }

    /// Queue-control seam shared by the real AudioToolbox adapter and the
    /// callback-free conformance fake. Buffer indices are available to the
    /// implementation only before `enqueue_buffer`; after enqueue, the queue
    /// owns scheduling access until `stop_immediate` returns synchronously.
    pub(super) trait QueueCycle {
        fn buffer_count(&self) -> usize;
        fn render_post_cue(&mut self, index: usize) -> bool;
        fn enqueue_buffer(&mut self, index: usize) -> bool;
        fn set_callback_recycling(&mut self, enabled: bool);
        fn start_queue(&mut self) -> bool;
        fn stop_immediate(&mut self) -> bool;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct PrimeReport {
        /// One-based queue position of the first buffer containing audible
        /// post-cue samples. `None` is permitted for a deliberately inert cue.
        pub(super) first_audible_buffer: Option<usize>,
    }

    /// Fill every AVAILABLE buffer from the post-cue synth state, enqueue it
    /// exactly once, then enable callback recycling and start. This ordering is
    /// the latency contract: no pre-cue buffer can sit ahead of buffer one.
    pub(super) fn prime_and_start<Q: QueueCycle>(queue: &mut Q) -> Option<PrimeReport> {
        let count = queue.buffer_count();
        debug_assert!(count > 0);
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

    /// Stop synchronously with callback recycling disabled. AudioQueueStop's
    /// immediate path resets the queue and removes every scheduled buffer; only
    /// after this returns may the worker render into the retained pointers.
    pub(super) fn stop_and_reclaim<Q: QueueCycle>(queue: &mut Q) -> bool {
        queue.set_callback_recycling(false);
        queue.stop_immediate()
    }

    /// The queue render callback — runs on AudioToolbox's own thread. Locks
    /// the synth briefly (its only other holder is the worker's cue push),
    /// renders one buffer, re-enqueues it.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "trail_audio_lifecycle",
            action = "RenderAudible",
            project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "trail_audio_lifecycle",
            action = "RenderSilent",
            project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
        )
    )]
    extern "C" fn render_cb(user: *mut c_void, q: AudioQueueRef, buf: *mut AudioQueueBuffer) {
        // SAFETY: `user` is the `Arc<Shared>` raw pointer installed at queue
        // creation and outlives the queue (disposed synchronously first).
        let shared = unsafe { &*(user as *const Shared) };
        // SAFETY: the queue hands us a buffer it allocated with capacity
        // BUFFER_FRAMES × CHANNELS f32s; we fill exactly that.
        unsafe {
            // Stop/reset may return scheduled buffers through this callback.
            // They are deliberately left AVAILABLE for the worker's next
            // post-cue prime, not rendered or re-enqueued during reset.
            let recycle_epoch = shared.recycle_epoch.load(Ordering::Acquire);
            if recycle_epoch & 1 == 0 {
                return;
            }
            let out = std::slice::from_raw_parts_mut(
                (*buf).m_audio_data as *mut f32,
                BUFFER_FRAMES * CHANNELS,
            );
            let quiet = {
                let mut synth = match shared.synth.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                synth.render(out);
                synth.is_quiet()
            };
            if quiet {
                saturating_increment(&shared.silent);
            } else {
                shared.silent.store(0, Ordering::Relaxed);
            }
            (*buf).m_audio_data_byte_size = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            // The gate closes the check-to-enqueue race: stop/reset takes it
            // before disabling recycling and cannot begin its FFI call until
            // this enqueue returns. A callback that was still rendering when
            // disable happened observes the changed generation and retires.
            let _gate = match shared.recycle_gate.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if shared.recycle_epoch.load(Ordering::Acquire) != recycle_epoch {
                return;
            }
            if AudioQueueEnqueueBuffer(q, buf, 0, std::ptr::null()) != 0 {
                shared.faulted.store(true, Ordering::Release);
            }
        }
    }

    pub struct MacOut {
        queue: AudioQueueRef,
        shared: Arc<Shared>,
        /// All buffers are allocated for the queue's lifetime. Pointer values
        /// remain stable, but the worker dereferences them only initially or
        /// after synchronous immediate stop has removed scheduled ownership.
        buffers: [*mut AudioQueueBuffer; BUFFER_COUNT],
        buffers_available: bool,
    }

    // SAFETY: `queue` is only touched from the audio worker (start/stop/
    // dispose); the callback thread reaches state exclusively through the
    // `Shared` (Mutex + atomics). AudioQueue control calls are themselves
    // thread-safe per AudioToolbox's documented contract.
    unsafe impl Send for MacOut {}

    impl MacOut {
        /// Open the output queue and allocate, but DO NOT enqueue, its buffers.
        /// The worker applies the first cue before [`Self::start`] renders and
        /// enqueues them, so no silent FIFO can precede the sound. `None` on any
        /// AudioToolbox error (no audio device, etc.) — trail sound then simply
        /// stays off this session; never fatal.
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerStart",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub fn new(seed: u32) -> Option<Self> {
            let shared = Arc::new(Shared {
                synth: Mutex::new(TrailSynth::new(SAMPLE_RATE as f32, seed)),
                silent: AtomicU32::new(0),
                running: AtomicBool::new(false),
                faulted: AtomicBool::new(false),
                recycle_epoch: AtomicU64::new(0),
                recycle_gate: Mutex::new(()),
            });
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
                // SAFETY: reclaim the refcount handed to the (never-created)
                // queue so it isn't leaked.
                unsafe { drop(Arc::from_raw(user as *const Shared)) };
                return None;
            }
            let bytes = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            let mut buffers = [std::ptr::null_mut(); BUFFER_COUNT];
            for slot in &mut buffers {
                let mut buf: *mut AudioQueueBuffer = std::ptr::null_mut();
                // SAFETY: queue is live; on success the pointer stays allocated
                // until queue disposal. It remains unscheduled/available here.
                unsafe {
                    if AudioQueueAllocateBuffer(queue, bytes, &mut buf) != 0 || buf.is_null() {
                        AudioQueueDispose(queue, 1);
                        drop(Arc::from_raw(user as *const Shared));
                        return None;
                    }
                }
                *slot = buf;
            }
            Some(Self {
                queue,
                shared,
                buffers,
                buffers_available: true,
            })
        }

        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerStartFails",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        fn start(&mut self) -> bool {
            if self.shared.faulted.load(Ordering::Acquire) || !self.buffers_available {
                return false;
            }
            // From this point, even a partial enqueue transfers scheduling
            // ownership away from the worker. Failure is terminal and disposal
            // reclaims it; no retry may touch these pointers.
            self.buffers_available = false;
            let report = {
                let mut cycle = MacQueueCycle {
                    queue: self.queue,
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
                true
            } else {
                self.shared.faulted.store(true, Ordering::Release);
                false
            }
        }

        /// Queue a cue and report whether output is running. The silence reset
        /// happens while holding the same synth lock as the callback: once the
        /// lock is released, `on_tick` can never observe the pre-cue threshold
        /// and pause a newly queued first sound after idle.
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "trail_audio_lifecycle",
                action = "WorkerPushRunning",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub fn push(&mut self, ev: SoundEvent) -> bool {
            {
                let mut synth = match self.shared.synth.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                synth.push(ev);
                self.shared.silent.store(0, Ordering::Release);
            }
            if !self.shared.running.load(Ordering::Relaxed) {
                let _ = self.start();
            }
            self.shared.running.load(Ordering::Acquire)
        }

        /// Pause housekeeping. Returns whether the queue remains live so the
        /// host can re-arm or retract its sole event-loop deadline exactly.
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "trail_audio_lifecycle",
                action = "ServiceRunning",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "trail_audio_lifecycle",
                action = "PauseIdle",
                project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
            )
        )]
        pub fn on_tick(&mut self) -> Result<bool, ()> {
            if self.shared.faulted.load(Ordering::Acquire) {
                return Err(());
            }
            if self.shared.running.load(Ordering::Relaxed)
                && self.shared.silent.load(Ordering::Acquire) >= PAUSE_AFTER_SILENT
            {
                let reclaimed = {
                    let mut cycle = MacQueueCycle {
                        queue: self.queue,
                        shared: &self.shared,
                        buffers: &self.buffers,
                    };
                    stop_and_reclaim(&mut cycle)
                };
                if reclaimed {
                    self.buffers_available = true;
                    self.shared.running.store(false, Ordering::Relaxed);
                } else {
                    return Err(());
                }
            }
            Ok(self.shared.running.load(Ordering::Relaxed))
        }

        pub fn is_running(&self) -> bool {
            self.shared.running.load(Ordering::Acquire)
        }
    }

    impl Drop for MacOut {
        fn drop(&mut self) {
            // SAFETY: synchronous dispose (immediate=1) stops the callback
            // thread before we release the Shared refcount it was using.
            unsafe {
                set_callback_recycling(&self.shared, false);
                AudioQueueDispose(self.queue, 1);
                drop(Arc::from_raw(Arc::as_ptr(&self.shared)));
                // (self.shared's own refcount drops normally after this.)
            }
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
                    let mut synth = match self.shared.synth.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
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
                AudioQueueEnqueueBuffer(self.queue, self.buffers[index], 0, std::ptr::null()) == 0
            }
        }

        fn set_callback_recycling(&mut self, enabled: bool) {
            set_callback_recycling(self.shared, enabled);
        }

        fn start_queue(&mut self) -> bool {
            // SAFETY: queue is live and all three buffers are scheduled.
            unsafe { AudioQueueStart(self.queue, std::ptr::null()) == 0 }
        }

        fn stop_immediate(&mut self) -> bool {
            // SAFETY: immediate=true is synchronous. On success AudioToolbox
            // reset/removal has completed and all retained pointers are again
            // available for the worker to fill.
            unsafe { AudioQueueStop(self.queue, 1) == 0 }
        }
    }
}

/// Bounded cue ingress. Eight cues can be emitted by one visual frame; 64 keeps
/// several burst frames without making producer work depend on the callback. A
/// full queue drops newest sound only — visual/input correctness always wins.
const COMMAND_CAPACITY: usize = 64;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnqueueDisposition {
    Queued,
    DroppedFull,
    Disconnected,
}

#[cfg(target_os = "macos")]
fn cue_channel() -> (
    std::sync::mpsc::SyncSender<SoundEvent>,
    std::sync::mpsc::Receiver<SoundEvent>,
) {
    std::sync::mpsc::sync_channel(COMMAND_CAPACITY)
}

/// The complete UI-thread ingress decision. `try_send` is structurally
/// nonblocking; a full channel preserves all queued cues, drops only the newest
/// cue, and records that loss with a saturating counter. Both formal actions
/// refine this one shipping branch point.
#[cfg(target_os = "macos")]
trait AudioWorkerOutput {
    fn push(&mut self, ev: SoundEvent) -> bool;
    fn on_tick(&mut self) -> Result<bool, ()>;
    fn is_running(&self) -> bool;
}

#[cfg(target_os = "macos")]
impl AudioWorkerOutput for mac::MacOut {
    fn push(&mut self, ev: SoundEvent) -> bool {
        mac::MacOut::push(self, ev)
    }

    fn on_tick(&mut self) -> Result<bool, ()> {
        mac::MacOut::on_tick(self)
    }

    fn is_running(&self) -> bool {
        mac::MacOut::is_running(self)
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerStart",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerPushRunning",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "WorkerStartFails",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "ServiceRunning",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "PauseIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "ParkIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
fn worker_loop<Output, Open>(
    rx: std::sync::mpsc::Receiver<SoundEvent>,
    shutdown: &std::sync::atomic::AtomicBool,
    state: &std::sync::atomic::AtomicU8,
    seed: u32,
    housekeeping_interval: Duration,
    mut open: Open,
) where
    Output: AudioWorkerOutput,
    Open: FnMut(u32) -> Option<Output>,
{
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;

    let mut output: Option<Output> = None;
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
                    match out.on_tick() {
                        Ok(true) => state.store(STATE_RUNNING, Ordering::Release),
                        Ok(false) => state.store(STATE_PAUSED, Ordering::Release),
                        Err(()) => {
                            state.store(STATE_FAILED, Ordering::Release);
                            return;
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    state.store(STATE_STOPPED, Ordering::Release);
                    return;
                }
            }
        } else {
            // Before the first cue and after an idle pause, sleep indefinitely:
            // no timer, callback, run-loop wake, or polling CPU remains armed.
            match rx.recv() {
                Ok(cue) => cue,
                Err(_) => {
                    state.store(STATE_STOPPED, Ordering::Release);
                    return;
                }
            }
        };
        if shutdown.load(Ordering::Acquire) {
            state.store(STATE_STOPPED, Ordering::Release);
            return;
        }
        if output.is_none() {
            output = open(seed);
            if output.is_none() {
                state.store(STATE_FAILED, Ordering::Release);
                return;
            }
        }
        if output.as_mut().is_none_or(|out| !out.push(cue)) {
            state.store(STATE_FAILED, Ordering::Release);
            return;
        }
        state.store(STATE_RUNNING, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "PushCueAvailable",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_ingress"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "PushCueFull",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_ingress"
    )
)]
fn enqueue_cue(
    tx: &std::sync::mpsc::SyncSender<SoundEvent>,
    dropped: &std::sync::atomic::AtomicU64,
    ev: SoundEvent,
) -> EnqueueDisposition {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::TrySendError;

    match tx.try_send(ev) {
        Ok(()) => EnqueueDisposition::Queued,
        Err(TrySendError::Full(_)) => {
            let mut current = dropped.load(Ordering::Relaxed);
            while current != u64::MAX {
                match dropped.compare_exchange_weak(
                    current,
                    current + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => current = observed,
                }
            }
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

#[cfg(target_os = "macos")]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "trail_audio_lifecycle",
        action = "ParkIdle",
        project = "aterm_gui::trail_audio::trail_audio_conformance::project_worker"
    )
)]
fn worker_main(
    rx: std::sync::mpsc::Receiver<SoundEvent>,
    shutdown: &std::sync::atomic::AtomicBool,
    state: &std::sync::atomic::AtomicU8,
    seed: u32,
) {
    worker_loop(
        rx,
        shutdown,
        state,
        seed,
        HOUSEKEEPING_INTERVAL,
        mac::MacOut::new,
    );
}

/// Cross-platform host face. On macOS, the UI owns only a bounded `SyncSender`:
/// `push` is a single `try_send` and never opens CoreAudio, waits on the callback's
/// synth mutex, allocates, sleeps, or logs. A dormant worker owns the platform
/// queue and blocks with zero wakeups until the first cue; headless/test apps can
/// construct the inert form and create no thread at all.
pub struct TrailAudio {
    #[cfg(target_os = "macos")]
    tx: Option<std::sync::mpsc::SyncSender<SoundEvent>>,
    #[cfg(target_os = "macos")]
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    #[cfg(target_os = "macos")]
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(target_os = "macos")]
    worker: Option<std::thread::JoinHandle<()>>,
    /// TEST-ONLY cue tap: `Some` makes this host report itself LIVE (so the
    /// policies that gate on a reachable device — `App::tone_infer_active`,
    /// `keystroke_click_audible` — behave exactly as they do on a real Mac)
    /// and records every pushed [`SoundEvent`] instead of queueing it to a
    /// worker. It exists so a test can assert what the render seams actually
    /// HANDED the audio host, rather than re-deriving the event and grading
    /// its own arithmetic. Platform-independent by construction: the seam
    /// under test is host-side policy, not CoreAudio.
    #[cfg(test)]
    capture: Option<Vec<SoundEvent>>,
}

impl TrailAudio {
    /// `active=false` is the sealed headless/test path: no channel or worker.
    pub fn new(active: bool) -> Self {
        #[cfg(target_os = "macos")]
        {
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

            let shutdown = Arc::new(AtomicBool::new(false));
            let state = Arc::new(AtomicU8::new(STATE_DORMANT));
            let dropped = Arc::new(AtomicU64::new(0));
            if !active {
                return Self {
                    tx: None,
                    shutdown,
                    state,
                    dropped,
                    worker: None,
                    #[cfg(test)]
                    capture: None,
                };
            }
            let (tx, rx) = cue_channel();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_state = Arc::clone(&state);
            let worker = std::thread::Builder::new()
                .name("aterm-trail-audio".into())
                .spawn(move || worker_main(rx, &worker_shutdown, &worker_state, 0x5EED_50FD))
                .ok();
            let tx = worker.as_ref().map(|_| tx);
            if worker.is_none() {
                state.store(STATE_FAILED, std::sync::atomic::Ordering::Release);
            }
            Self {
                tx,
                shutdown,
                state,
                dropped,
                worker,
                #[cfg(test)]
                capture: None,
            }
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

    /// A TEST host that reports LIVE and records every cue (see the `capture`
    /// field). Never reachable from a shipping build.
    #[cfg(test)]
    pub(crate) fn capturing_for_test() -> Self {
        let mut host = Self::new(false);
        host.capture = Some(Vec::new());
        host
    }

    /// Take the cues recorded since the last call (test-only).
    #[cfg(test)]
    pub(crate) fn take_captured_for_test(&mut self) -> Vec<SoundEvent> {
        self.capture
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Replace the complete audio host with a freshly resolved active/inert one.
    /// Dropping the previous value is synchronous: it closes cue ingress, wakes and
    /// joins the worker, and lets the platform output dispose its queue immediately.
    /// This is the serious-mode edge seam; no already-playing decorative tail can
    /// survive the call returning.
    pub fn replace(&mut self, active: bool) {
        *self = Self::new(active);
    }

    /// Queue one cue without blocking the input/present thread. Full means this
    /// nonessential sound is dropped; disconnected permanently disables ingress.
    pub fn push(&mut self, ev: SoundEvent) {
        #[cfg(test)]
        if let Some(captured) = self.capture.as_mut() {
            captured.push(ev);
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(disposition) = self
            .tx
            .as_ref()
            .map(|tx| enqueue_cue(tx, &self.dropped, ev))
            && disposition == EnqueueDisposition::Disconnected
        {
            self.tx = None;
            self.state
                .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = ev;
    }

    /// Whether cues pushed here can ever reach a device: `false` for the
    /// sealed headless/test form, on platforms without an audio backend, and
    /// after a permanent ingress failure. The tone-of-typing classifier
    /// gates on this — inference must never spend a microsecond in a build
    /// whose sound can only be silence (the "never runs headless-muted"
    /// policy).
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

    #[cfg(test)]
    pub(crate) fn is_inert_for_test(&self) -> bool {
        // A capturing host accepts cues, so it is NOT inert — keeping this the
        // exact complement of `is_live` stops the two test predicates from
        // disagreeing about the same host.
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

    #[cfg(all(test, target_os = "macos"))]
    fn test_ingress() -> (Self, std::sync::mpsc::Receiver<SoundEvent>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

        let (tx, rx) = cue_channel();
        (
            Self {
                tx: Some(tx),
                shutdown: Arc::new(AtomicBool::new(false)),
                state: Arc::new(AtomicU8::new(STATE_DORMANT)),
                dropped: Arc::new(AtomicU64::new(0)),
                worker: None,
                // A REAL channel under test: this fixture proves the enqueue
                // path itself, so it must not divert into the capture tap.
                capture: None,
            },
            rx,
        )
    }
}

impl Drop for TrailAudio {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Release);
            self.tx = None;
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod trail_audio_conformance {
    use aterm_spec::interp::State;

    /// Project the bounded lifecycle fields shared by the worker/device actions.
    /// The shipping mailbox/drop counters are collapsed to the model's Cap=2
    /// abstraction; all safety-observable UI violations remain exact zeros.
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
        idle_wakes: usize,
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
            ("idle_wakes", idle_wakes.min(1) as i64),
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
    use aterm_effects::trail_sound::{
        CHANNELS, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth, WordGesture,
    };

    use super::mac::{QueueCycle, prime_and_start, stop_and_reclaim};
    use super::{
        AudioWorkerOutput, COMMAND_CAPACITY, STATE_DORMANT, STATE_FAILED, STATE_PAUSED,
        STATE_RUNNING, STATE_STOPPED, TrailAudio, cue_channel, worker_loop,
    };

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
            // Bed ON in the queue tests: the idle-pause policy proofs cover
            // the WORST-case (bed-breathing) exhale window.
            bed: true,
        }
    }

    /// Bind the queue-position latency premise to the shipping synth: every
    /// supported audible cue produces a nonzero sample in the exact first
    /// 512-frame buffer that `MacQueueCycle` renders after accepting it.
    #[test]
    fn every_audible_cue_begins_in_first_synth_buffer() {
        let styles = [
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
            GlowStyle::Sparkle,
            GlowStyle::Fire,
            GlowStyle::Laser,
            GlowStyle::Beam,
            GlowStyle::Water,
            GlowStyle::Comet,
        ];
        // Every trail gesture PLUS the sparkle-words bonk: the first-buffer
        // latency contract is per-gesture, not per-source.
        let kinds = [
            SoundGesture::Trail(SoundKind::Typed),
            SoundGesture::Trail(SoundKind::Backspace),
            SoundGesture::Trail(SoundKind::Navigation),
            SoundGesture::Trail(SoundKind::Kill),
            SoundGesture::Trail(SoundKind::Jump),
            // The cursor-movement gestures — a Glide is one immediate tone; a
            // Sweep's FIRST run-note has delay 0, so both speak in the first
            // post-cue buffer exactly like every other audible cue.
            SoundGesture::Trail(SoundKind::Glide { dir: 1 }),
            SoundGesture::Trail(SoundKind::Sweep { dir: -1 }),
            // The comma and the lift are delay-0 voices; the deletion's felt
            // damp is delay-0 too (its breath trails, but Backspace above
            // already pins the gesture's first buffer).
            SoundGesture::Trail(SoundKind::Space),
            SoundGesture::Trail(SoundKind::Shift),
            SoundGesture::Words(WordGesture::Bonk),
        ];

        for style in styles {
            for kind in kinds {
                let mut synth = TrailSynth::new(super::SAMPLE_RATE as f32, 0x5EED_50FD);
                synth.push(SoundEvent {
                    style,
                    voice: SoundVoice::Style,
                    kind,
                    pan: 0.0,
                    heat: 0.4,
                    hue: 0.0,
                    gain: 0.4,
                    tone: aterm_effects::tone::Tone::Technical,
                    bed: true,
                });
                let mut samples = [0.0; super::BUFFER_FRAMES * CHANNELS];
                synth.render(&mut samples);
                assert!(
                    samples.iter().any(|sample| *sample != 0.0),
                    "{style:?}/{kind:?} missed the first post-cue buffer"
                );
            }
        }
    }

    /// WHY THE HOST MUST NEVER FAN ONE COALESCED ECHO OUT INTO N CUES.
    ///
    /// The tempting fix for "three glyphs echoed in one frame click once" is
    /// to push one cue per crossed cell. It does not work, and it is worse
    /// than doing nothing — this pins both halves so nobody re-lands it:
    ///
    /// 1. NO EXTRA CLICKS. Discrete voices are admitted by a wall-clock gap
    ///    (`MIN_GAP`, ~45 ms) measured between `push` calls. A fan-out arrives
    ///    in ONE drain, i.e. at one instant, so the first event is admitted and
    ///    every sibling is thinned. Same voice count as a single cue.
    /// 2. IT DUCKS THE REAL ONES. Thinned events still pay into the synth's
    ///    rate estimate before they are dropped, and the rate drives the
    ///    loudness duck — so the fan-out makes the NEXT genuine keystroke
    ///    audibly quieter (measured below) while adding nothing of its own.
    ///
    /// Density can only come from spacing cues in REAL TIME, which is what
    /// cueing at the physical keypress does (`CursorGlow::cue_keystroke`).
    #[test]
    fn batched_cues_add_no_voices_and_duck_the_next_keystroke() {
        // Render ~10 buffers (~107 ms) so the batch's own voices have decayed
        // and the min-gap can no longer thin the follow-up keystroke: whatever
        // difference remains is the rate/duck inflation, nothing else.
        fn batch_then_next_peak(n: usize) -> (usize, f32) {
            let mut synth = TrailSynth::new(super::SAMPLE_RATE as f32, 0x5EED_50FD);
            for _ in 0..n {
                synth.push(cue());
            }
            let voices = synth.live_voices();
            let mut buf = [0.0; super::BUFFER_FRAMES * CHANNELS];
            for _ in 0..10 {
                synth.render(&mut buf);
            }
            synth.push(cue());
            let mut next = [0.0; super::BUFFER_FRAMES * CHANNELS];
            synth.render(&mut next);
            (voices, next.iter().fold(0.0f32, |a, s| a.max(s.abs())))
        }

        let (one_voices, one_peak) = batch_then_next_peak(1);
        for n in [2usize, 3, 8] {
            let (voices, _) = batch_then_next_peak(n);
            assert_eq!(
                voices, one_voices,
                "{n} same-instant cues must yield the SAME voices as one — \
                 the min-gap thins every sibling, so a fan-out is inaudible"
            );
        }
        let (_, eight_peak) = batch_then_next_peak(8);
        assert!(
            eight_peak < one_peak * 0.95,
            "a fan-out must be shown to DUCK the next keystroke \
             (one: {one_peak:.6}, eight: {eight_peak:.6}) — that is the \
             regression this test exists to keep out"
        );
    }

    fn wait_until(label: &str, pred: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !pred() {
            assert!(std::time::Instant::now() < deadline, "timed out: {label}");
            std::thread::yield_now();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeBuffer {
        Available,
        Rendered { audible: bool },
        Queued { audible: bool },
    }

    /// Deterministic AudioQueue seam with no callback thread. Every write
    /// checks that synchronous stop has returned the buffer to AVAILABLE; a
    /// test-only retained-pause mutation records unsafe writes instead.
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
            }
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
            // Retired boolean gate: disabling/re-enabling carried no generation
            // identity, so an old callback could alias the later run.
            self.recycling = false;
            self.enqueue_in_flight = false;
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

        fn render_post_cue(&mut self, index: usize) -> bool {
            self.operations += 1;
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
            true
        }
    }

    /// Tier-1 binding for `TrailAudioStartLatency`: drive the exact generic
    /// prime/stop helpers used by MacOut with a callback-free queue that checks
    /// buffer ownership on every write. Both cold start and resume put audible
    /// post-cue samples in queue position one; synchronous stop returns every
    /// pointer and the parked interval performs no operation at all.
    #[test]
    fn audio_queue_post_cue_prime_conforms_with_callback_free_fake() {
        use super::trail_audio_conformance::project_start_latency;

        let model = aterm_spec::derive::trail_audio_start_latency_model();
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
                0,
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
            "IdleIsCallbackAndWakeFree",
        ] {
            assert!(model.check_invariant(invariant, &state), "{invariant}");
        }

        // Negative control A: the retired cold path queued three silent
        // buffers before applying the cue. FIFO order puts the first possible
        // audible buffer at position four, matching the Buggy model witness.
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

        // Negative control B: the retired boolean check lets stop begin after a
        // callback's check but before its enqueue. Merely pausing then retains
        // queue ownership, so a naive low-latency refill also writes a scheduled
        // pointer. The fake and Buggy model record both violations.
        assert!(buggy.fire("StartCold", &mut bad));
        retained.callback_enqueue_begins();
        assert!(buggy.fire("CallbackEnqueueBegins", &mut bad));
        retained.stop_without_gate_for_negative_control();
        assert!(buggy.fire("StopIdle", &mut bad));
        assert_eq!(retained.stop_overlaps, 1);
        assert!(!buggy.check_invariant("StopNeverOverlapsEnqueue", &bad));
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

    #[derive(Default)]
    struct FakeShared {
        opens: AtomicUsize,
        pushes: AtomicUsize,
        ticks: AtomicUsize,
        pause_on_tick: AtomicBool,
        fail_open: AtomicBool,
        fail_push: AtomicBool,
    }

    struct FakeOutput {
        shared: Arc<FakeShared>,
        running: bool,
    }

    impl AudioWorkerOutput for FakeOutput {
        fn push(&mut self, _ev: SoundEvent) -> bool {
            self.shared.pushes.fetch_add(1, Ordering::Relaxed);
            if self.shared.fail_push.load(Ordering::Relaxed) {
                return false;
            }
            self.running = true;
            true
        }

        fn on_tick(&mut self) -> Result<bool, ()> {
            self.shared.ticks.fetch_add(1, Ordering::Relaxed);
            if self.shared.pause_on_tick.load(Ordering::Relaxed) {
                self.running = false;
            }
            Ok(self.running)
        }

        fn is_running(&self) -> bool {
            self.running
        }
    }

    fn spawn_fake_worker(
        shared: Arc<FakeShared>,
    ) -> (
        std::sync::mpsc::SyncSender<SoundEvent>,
        Arc<AtomicBool>,
        Arc<AtomicU8>,
        std::thread::JoinHandle<()>,
    ) {
        let (tx, rx) = cue_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(STATE_DORMANT));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            let factory_shared = Arc::clone(&shared);
            worker_loop(
                rx,
                &worker_shutdown,
                &worker_state,
                7,
                std::time::Duration::from_millis(2),
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
        (tx, shutdown, state, worker)
    }

    #[test]
    fn bounded_ingress_drops_instead_of_blocking_when_full() {
        use std::sync::atomic::Ordering;

        let (mut audio, rx) = TrailAudio::test_ingress();
        for _ in 0..COMMAND_CAPACITY {
            audio.push(cue());
        }
        assert_eq!(audio.dropped.load(Ordering::Relaxed), 0);
        audio.push(cue());
        assert_eq!(
            audio.dropped.load(Ordering::Relaxed),
            1,
            "the real TrailAudio::push full branch records one explicit drop"
        );
        assert_eq!(
            rx.try_iter().count(),
            COMMAND_CAPACITY,
            "drop-newest preserves every already-queued cue"
        );
        assert_eq!(audio.state(), STATE_DORMANT);
    }

    /// Tier-1: drive the genuine shipping `TrailAudio::push` and validate its
    /// available/full decisions against the derived model's own transitions.
    /// Filling all 64 slots also binds the abstraction to the exact production
    /// capacity rather than a standalone toy channel.
    #[test]
    fn shipping_ingress_conforms_to_bounded_drop_newest_model() {
        use std::sync::atomic::Ordering;

        use super::trail_audio_conformance::project_ingress;

        let model = aterm_spec::derive::trail_audio_lifecycle_model();
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
        assert_eq!(audio.dropped.load(Ordering::Relaxed), 0);
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

        // Negative control: the former silent-loss decision (full queue, but no
        // drop accounting) is not a healthy model transition.
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
        audio.dropped.store(u64::MAX, Ordering::Relaxed);
        audio.push(cue());
        assert_eq!(audio.dropped.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn disconnected_ingress_fails_closed_once() {
        let (mut audio, rx) = TrailAudio::test_ingress();
        drop(rx);
        audio.push(cue());
        assert!(audio.tx.is_none());
        assert_eq!(audio.state(), STATE_FAILED);
        // Permanently disabled: subsequent pushes cannot retry or block.
        audio.push(cue());
        assert!(audio.tx.is_none());
    }

    /// Tier-1 worker lifecycle with an injected deterministic device: the REAL
    /// shipping worker loop performs no open/timer work before the first cue,
    /// starts once, pauses onto an indefinite blocking receive, then resumes the
    /// same backend on the next cue. Model transitions and a negative control
    /// bind the observed states to the derived lifecycle.
    #[test]
    fn worker_lifecycle_conforms_and_parks_without_polling() {
        use super::trail_audio_conformance::project_worker;

        let shared = Arc::new(FakeShared::default());
        let (tx, shutdown, state, worker) = spawn_fake_worker(Arc::clone(&shared));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(shared.opens.load(Ordering::Relaxed), 0);
        assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
        assert_eq!(shared.ticks.load(Ordering::Relaxed), 0);

        tx.send(cue()).unwrap();
        wait_until("worker start", || {
            state.load(Ordering::Acquire) == STATE_RUNNING
                && shared.pushes.load(Ordering::Relaxed) == 1
        });
        assert_eq!(shared.opens.load(Ordering::Relaxed), 1);

        let model = aterm_spec::derive::trail_audio_lifecycle_model();
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
        std::thread::sleep(std::time::Duration::from_millis(12));
        assert_eq!(
            shared.ticks.load(Ordering::Relaxed),
            ticks_at_pause,
            "paused worker blocks on recv instead of polling a timeout"
        );
        let before_pause = project_worker(0, 0, false, true, 2, true, false, false, false);
        let after_pause = project_worker(0, 0, false, false, 2, false, false, false, true);
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

        shared.pause_on_tick.store(false, Ordering::Release);
        tx.send(cue()).unwrap();
        wait_until("worker resume", || {
            state.load(Ordering::Acquire) == STATE_RUNNING
                && shared.pushes.load(Ordering::Relaxed) == 2
        });
        assert_eq!(shared.opens.load(Ordering::Relaxed), 1);

        shutdown.store(true, Ordering::Release);
        let _ = tx.send(cue());
        worker.join().unwrap();
        assert_eq!(state.load(Ordering::Acquire), STATE_STOPPED);
    }

    #[test]
    fn worker_device_failure_is_terminal_and_never_retried() {
        let shared = Arc::new(FakeShared::default());
        shared.fail_open.store(true, Ordering::Relaxed);
        let (tx, _shutdown, state, worker) = spawn_fake_worker(Arc::clone(&shared));
        tx.send(cue()).unwrap();
        wait_until("explicit open failure", || {
            state.load(Ordering::Acquire) == STATE_FAILED
        });
        worker.join().unwrap();
        assert_eq!(shared.opens.load(Ordering::Relaxed), 1);
        assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
        assert!(tx.send(cue()).is_err(), "failed worker is terminal");
    }

    #[test]
    fn inert_audio_constructs_no_worker() {
        let audio = TrailAudio::new(false);
        assert_eq!(audio.state(), STATE_DORMANT);
        assert!(audio.worker.is_none());
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

    /// IGNORED by default: opens the REAL output device and audibly plays a
    /// short water-trail phrase (a droplet run + a splash) at the default
    /// volume. Run it by hand when validating the device path or tuning:
    ///
    ///   cargo test -p aterm-gui trail_audio_smoke -- --ignored --nocapture
    #[test]
    #[ignore = "plays ~3 s of audio on the local output device"]
    fn trail_audio_smoke() {
        let mut audio = TrailAudio::new(true);
        for i in 0..10 {
            audio.push(SoundEvent {
                style: GlowStyle::Water,
                voice: SoundVoice::Style,
                kind: SoundGesture::Trail(SoundKind::Typed),
                pan: -0.8 + i as f32 * 0.16,
                heat: 0.4,
                hue: 0.0,
                gain: 0.4,
                tone: aterm_effects::tone::Tone::Technical,
                bed: true, // the hand-run smoke audits the full palette
            });
            assert_ne!(audio.state(), STATE_FAILED, "AudioQueue failed to open");
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        audio.push(SoundEvent {
            style: GlowStyle::Water,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(SoundKind::Jump),
            pan: -0.9,
            heat: 0.6,
            hue: 0.0,
            gain: 0.4,
            tone: aterm_effects::tone::Tone::Technical,
            bed: true,
        });
        std::thread::sleep(std::time::Duration::from_millis(1200));
        // Worker-owned housekeeping eventually pauses without any render tick.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_ne!(audio.state(), STATE_FAILED);
    }
}
