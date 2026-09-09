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

use aterm_effects::trail_sound::{EventMeta, SoundEvent};

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

/// Consecutive silent buffers before the queue pauses: 140 buffers ≈ 1.5 s
/// of exact digital silence (the synth's beds have already snapped to zero
/// by then, so this can never clip a tail).
///
/// 140, NOT THE 48 (≈ 0.5 s) IT WAS (THE PRISM §2.4 item 5, §3.4 d). A
/// stopped queue makes the next cue pay an `AudioQueueStart`, and the first
/// key after every think-pause therefore carried a DIFFERENT offset from its
/// successors — the one part of latency an ear hears as slop rather than as
/// a constant. Half a second is shorter than an ordinary pause between
/// words; a second and a half is longer than nearly all of them and still
/// parks the device hard when the typing has really stopped. It trades a
/// little idle power for alignment and ships behind its own measurement
/// (§7 step 6): revert if the idle census shows a wake cost.
const PAUSE_AFTER_SILENT: u32 = 140;

/// Event-loop housekeeping cadence while the AudioQueue is running. The queue
/// callback already does the sample work; this low-rate one-shot only observes
/// its exact-silence counter and pauses the device. It is armed only while audio
/// is live and self-disarms after a successful pause.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(250);

/// UI-side staleness threshold for the worker's platform-busy stamp. A healthy
/// AudioQueue control call returns in milliseconds; one that has not returned
/// for this long is WEDGED. The 2026-08 field incident — typing sounds silent
/// for hours while `tone status` read `audio=live` — is the case this exists
/// for, but its cause is NOT proven: the sample taken then showed the worker in
/// `dispatch_semaphore_wait`, which is also exactly what a healthy parked
/// `recv()` looks like. The stamp makes the next occurrence decide it: a
/// worker stuck inside a platform call reads `audio=wedged`, a parked one
/// reads `live` with `dropped=0`.
const WEDGE_AFTER_MS: u64 = 3_000;

/// How long teardown waits for a live worker to finish before detaching it.
/// Covers the honest worst case — one housekeeping interval plus a healthy
/// platform call — so an ordinary drop stays effectively synchronous while a
/// wedge can never carry itself onto the event-loop thread.
const DETACH_DEADLINE: Duration = Duration::from_millis(600);

/// Poll cadence while waiting out [`DETACH_DEADLINE`].
const DETACH_POLL: Duration = Duration::from_millis(5);

/// Lifetime budget of worker revivals after wedge detection. Each revival
/// abandons one blocked thread and its queue by design (a thread stuck inside
/// a platform call cannot be cancelled; it disposes its own queue if the call
/// ever returns); the budget bounds that leak, and an exhausted budget seals
/// ingress so the host reads honestly inert instead of wedging forever.
const WEDGE_REVIVES: u8 = 3;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use aterm_effects::trail_sound::{CHANNELS, EventMeta, SoundEvent, TrailSynth};

    use super::{BUFFER_COUNT, BUFFER_FRAMES, PAUSE_AFTER_SILENT, SAMPLE_RATE};

    /// One queue block in seconds — 512 frames at 48 kHz = 10.667 ms — the
    /// ceiling on a cue's pre-roll ([`EventMeta::block_lead_s`]).
    pub(super) const BLOCK_S: f32 = BUFFER_FRAMES as f32 / SAMPLE_RATE as f32;

    /// THE PRE-ROLL LAW (THE PRISM §3.4 c): how far into the in-flight block
    /// a cue arrived, from the block's admission stamp and the cue's own
    /// instant. `0` while the queue is not running (the first cue after idle
    /// primes fresh buffers that render at once) and before any block has
    /// been stamped; otherwise `now - start`, clamped to one block so a late
    /// callback can never become a late note. Pure, so the callback-free
    /// fake can drive it against a fake clock and pin the one scenario the
    /// real device hides: the cues that arrive in the first block after a
    /// resume prime.
    pub(super) fn block_lead_s(running: bool, block_start_us: u64, now_us: u64) -> f32 {
        if !running || block_start_us == 0 {
            return 0.0;
        }
        (now_us.saturating_sub(block_start_us) as f32 * 1e-6).min(BLOCK_S)
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
        /// THE INSTANT THE CALLBACK BEGAN RENDERING THE IN-FLIGHT BLOCK, in
        /// [`crate::metrics::now_us`] µs — one relaxed store per block, taken
        /// under the synth lock immediately before `render` (the cue
        /// admission boundary: a cue pushed before this instant is in that
        /// block, one pushed after it waits for the next). [`MacOut::push_meta`]
        /// reads it to hand the synth the cue's PRE-ROLL
        /// ([`EventMeta::block_lead_s`]), which is what turns the render
        /// grid's 0–10.7 ms of onset jitter into a constant. ALSO stamped by
        /// every prime ([`QueueCycle::stamp_block_start`], first thing in
        /// [`prime_and_start`]): the primed buffers are the in-flight blocks
        /// until the first callback, and without that stamp every cue in the
        /// ~10.7 ms after a resume would measure against a start seconds old
        /// and clamp to a full block — the one-per-restart asymmetry this
        /// stamp exists to remove, moved from the first key to the second.
        /// `0` until the first prime.
        block_start_us: AtomicU64,
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
        /// Stamp the block admission boundary for the buffers about to be
        /// primed — the same stamp the render callback takes before each
        /// block, taken once here because the prime renders every buffer in
        /// one breath. Called by [`prime_and_start`] BEFORE its first
        /// `render_post_cue`, on every prime, cold and resume alike.
        fn stamp_block_start(&mut self);
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

    /// Stamp the block start, fill every AVAILABLE buffer from the post-cue
    /// synth state, enqueue it exactly once, then enable callback recycling
    /// and start. This ordering is the latency contract: no pre-cue buffer
    /// can sit ahead of buffer one, and no cue admitted after this prime can
    /// measure its pre-roll against a block from before the park.
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
                // The block's admission boundary, stamped under the lock so
                // a push can never read a start that is about to move under
                // it. One relaxed store: no allocation, no lock, RT-safe.
                shared
                    .block_start_us
                    .store(crate::metrics::now_us(), Ordering::Relaxed);
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
                block_start_us: AtomicU64::new(0),
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
        pub fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> bool {
            {
                let mut synth = match self.shared.synth.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                // THE PRE-ROLL (THE PRISM §3.4 c, [`block_lead_s`]): how far
                // into the in-flight block this cue arrived. Measured under
                // the same lock the callback stamps under, so the two cannot
                // interleave: if the callback is rendering, we wait and then
                // measure against the block it just started. Only while the
                // queue is RUNNING — the first cue after idle primes fresh
                // buffers that render at once, so its lead is exactly 0 —
                // and the prime itself re-stamps, so the cues in the first
                // block after a resume measure against the prime, not the
                // block before the park.
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

        fn stamp_block_start(&mut self) {
            // The worker is the only pusher and it is the thread priming, so
            // no lock is needed for the stamp to be ordered before the
            // renders below; the callback cannot run (recycling is off).
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

/// ONE QUEUED CUE: the gesture and its side-car (`RAINBOW-KITTY-V2.md` §16
/// rows 6-8 — the host input-clock stamp, the glyph class, the meteor's
/// origin pan). The worker hands both to `TrailSynth::push_meta`; a host
/// with nothing to stamp pushes the identity side-car ([`EventMeta::default`])
/// through [`TrailAudio::push`], which is byte-for-byte the pre-v2 path.
#[derive(Clone, Copy, Debug)]
struct Cue {
    ev: SoundEvent,
    meta: EventMeta,
}

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
/// stamp. Zero is reserved as the "not inside a platform call" sentinel, so
/// live stamps are floored to 1. The origin sits [`WEDGE_AFTER_MS`] + 1 in
/// the past — meaningless to a relative clock, but it means a stamp of `1`
/// is stale from the process's first instant, so a test can fabricate a
/// wedge deterministically instead of sleeping out the real threshold.
#[cfg(target_os = "macos")]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    let elapsed =
        u64::try_from(BASE.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX);
    elapsed.saturating_add(WEDGE_AFTER_MS + 1)
}

/// RAII stamp around every worker platform section (device open, cue apply,
/// pause housekeeping). While held, the cell carries the section's entry
/// instant; every exit path — including the failure returns — clears it. The
/// UI reads the cell to tell a parked worker (0: healthy by definition, the
/// next send wakes it) from one stuck inside a single platform call (stale
/// nonzero: the wedge a bare channel-liveness bit can never see).
#[cfg(target_os = "macos")]
struct PlatformBusy<'a>(&'a std::sync::atomic::AtomicU64);

#[cfg(target_os = "macos")]
impl<'a> PlatformBusy<'a> {
    fn mark(cell: &'a std::sync::atomic::AtomicU64) -> Self {
        cell.store(monotonic_ms().max(1), std::sync::atomic::Ordering::Release);
        Self(cell)
    }
}

#[cfg(target_os = "macos")]
impl Drop for PlatformBusy<'_> {
    fn drop(&mut self) {
        self.0.store(0, std::sync::atomic::Ordering::Release);
    }
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
}

/// The complete UI-thread ingress decision. `try_send` is structurally
/// nonblocking; a full channel preserves all queued cues, drops only the newest
/// cue, and records that loss with a saturating counter. Both formal actions
/// refine this one shipping branch point.
#[cfg(target_os = "macos")]
trait AudioWorkerOutput {
    fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> bool;
    fn on_tick(&mut self) -> Result<bool, ()>;
    fn is_running(&self) -> bool;
}

#[cfg(target_os = "macos")]
impl AudioWorkerOutput for mac::MacOut {
    fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> bool {
        mac::MacOut::push_meta(self, ev, meta)
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
    rx: std::sync::mpsc::Receiver<Cue>,
    flags: WorkerFlags<'_>,
    seed: u32,
    housekeeping_interval: Duration,
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
    } = flags;
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
                    let tick = {
                        let _busy = PlatformBusy::mark(busy);
                        out.on_tick()
                    };
                    match tick {
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
        // Everything from here to the loop bottom may enter AudioToolbox
        // (device open, start, enqueue) — the sections a wedge parks in.
        let _busy = PlatformBusy::mark(busy);
        if output.is_none() {
            output = open(seed);
            if output.is_none() {
                state.store(STATE_FAILED, Ordering::Release);
                return;
            }
        }
        if output
            .as_mut()
            .is_none_or(|out| !out.push_meta(cue.ev, cue.meta))
        {
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
    tx: &std::sync::mpsc::SyncSender<Cue>,
    dropped: &std::sync::atomic::AtomicU64,
    cue: Cue,
) -> EnqueueDisposition {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::TrySendError;

    match tx.try_send(cue) {
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
fn worker_main(rx: std::sync::mpsc::Receiver<Cue>, flags: WorkerFlags<'_>, seed: u32) {
    worker_loop(rx, flags, seed, HOUSEKEEPING_INTERVAL, mac::MacOut::new);
}

/// Cross-platform host face. On macOS, the UI owns only a bounded `SyncSender`:
/// `push` is a single `try_send` and never opens CoreAudio, waits on the callback's
/// synth mutex, allocates, sleeps, or logs. A dormant worker owns the platform
/// queue and blocks with zero wakeups until the first cue; headless/test apps can
/// construct the inert form and create no thread at all.
pub struct TrailAudio {
    #[cfg(target_os = "macos")]
    tx: Option<std::sync::mpsc::SyncSender<Cue>>,
    #[cfg(target_os = "macos")]
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    #[cfg(target_os = "macos")]
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The worker's platform-busy stamp (see [`PlatformBusy`]): 0 while the
    /// worker is parked or between platform calls, else the entry instant of
    /// the call it is currently inside. Staleness past [`WEDGE_AFTER_MS`] is
    /// the wedge verdict [`Self::wedged_for`] reports.
    #[cfg(target_os = "macos")]
    busy: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Remaining worker revivals ([`WEDGE_REVIVES`] at birth). Decremented by
    /// each wedge-triggered revival; at zero the next wedge seals ingress.
    #[cfg(target_os = "macos")]
    revives_left: u8,
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
    capture: Option<Vec<(SoundEvent, EventMeta)>>,
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
            let busy = Arc::new(AtomicU64::new(0));
            if !active {
                return Self {
                    tx: None,
                    shutdown,
                    state,
                    dropped,
                    busy,
                    revives_left: WEDGE_REVIVES,
                    worker: None,
                    #[cfg(test)]
                    capture: None,
                };
            }
            let (tx, rx) = cue_channel();
            let worker_shutdown = Arc::clone(&shutdown);
            let worker_state = Arc::clone(&state);
            let worker_busy = Arc::clone(&busy);
            let worker = std::thread::Builder::new()
                .name("aterm-trail-audio".into())
                .spawn(move || {
                    // THE ROLE, DECLARED FIRST (THE PRISM §3.4 d). This
                    // worker holds the synth mutex the AudioQueue callback
                    // takes on the one thread that must never miss; left at
                    // the inherited default it was the priority inversion
                    // `qos.rs` warns of — a descheduled lock holder with the
                    // RT callback queued behind it under load. `Responsive`,
                    // not `Interactive`: `qos.rs` names Responsive as the
                    // floor for a lock holder the UI thread contends, and a
                    // class at or above the UI thread can cost a frame.
                    crate::qos::set_self(crate::qos::Role::Responsive);
                    worker_main(
                        rx,
                        WorkerFlags {
                            shutdown: &worker_shutdown,
                            state: &worker_state,
                            busy: &worker_busy,
                        },
                        0x5EED_50FD,
                    );
                })
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
                busy,
                revives_left: WEDGE_REVIVES,
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

    /// Replace the complete audio host with a freshly resolved active/inert one.
    /// Dropping the previous value closes cue ingress, wakes and joins the worker
    /// (bounded — a wedged or deadline-overrunning worker is detached and
    /// self-terminates instead of freezing this thread), and lets the platform
    /// output dispose its queue. This is the serious-mode edge seam; no
    /// already-playing decorative tail survives a healthy teardown returning.
    pub fn replace(&mut self, active: bool) {
        // Nothing to carry across (§17.3 phase 7): the music box is named on
        // each event's voice, so a fresh worker's synth needs no latch.
        *self = Self::new(active);
    }

    /// Queue one cue without blocking the input/present thread. Full means this
    /// nonessential sound is dropped; disconnected permanently disables ingress.
    /// A full channel whose worker is WEDGED (stuck inside one platform call
    /// past [`WEDGE_AFTER_MS`]) additionally spends one revival — see
    /// [`Self::revive_or_seal`] — so a stuck device costs seconds of silence,
    /// not the rest of the session.
    pub fn push(&mut self, ev: SoundEvent) {
        self.push_meta(ev, EventMeta::default());
    }

    /// [`Self::push`] with the v2 side-car (`RAINBOW-KITTY-V2.md` §16 rows
    /// 6-8) — the host input-clock stamp and the glyph class, stamped on BOTH
    /// delivery paths (the keyed seam and the frame drain). Same nonblocking
    /// ingress, same drop policy; `push` is exactly this with the identity
    /// side-car.
    pub fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) {
        #[cfg(test)]
        if let Some(captured) = self.capture.as_mut() {
            captured.push((ev, meta));
            return;
        }
        #[cfg(target_os = "macos")]
        match self
            .tx
            .as_ref()
            .map(|tx| enqueue_cue(tx, &self.dropped, Cue { ev, meta }))
        {
            Some(EnqueueDisposition::Disconnected) => {
                self.tx = None;
                self.state
                    .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
            }
            Some(EnqueueDisposition::DroppedFull) if self.busy_stale_ms().is_some() => {
                self.revive_or_seal();
            }
            _ => {}
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (ev, meta);
    }

    /// Whether cues pushed here can ever reach a device: `false` for the
    /// sealed headless/test form, on platforms without an audio backend, and
    /// after a permanent ingress failure. The tone-of-typing classifier
    /// gates on this — inference must never spend a microsecond in a build
    /// whose sound can only be silence (the "never runs headless-muted"
    /// policy).
    ///
    /// This is CHANNEL liveness only; a wedged worker keeps it `true`. Pair
    /// with [`Self::wedged_for`] wherever "live" is reported to a human.
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

    /// The worker's current platform call has outlived [`WEDGE_AFTER_MS`]:
    /// `Some(elapsed)`. `None` means parked/idle, between calls, or a call
    /// still young enough to be presumed healthy.
    #[cfg(target_os = "macos")]
    fn busy_stale_ms(&self) -> Option<u64> {
        let mark = self.busy.load(std::sync::atomic::Ordering::Acquire);
        if mark == 0 {
            return None;
        }
        let elapsed = monotonic_ms().saturating_sub(mark);
        (elapsed >= WEDGE_AFTER_MS).then_some(elapsed)
    }

    /// WEDGED: ingress is open (so [`Self::is_live`] reads `true`) but the
    /// worker has been stuck inside ONE platform call past its threshold —
    /// every new cue is being dropped while a bare liveness bit would keep
    /// reading healthy. Whether the 2026-08 field incident was this state is
    /// unproven (see [`WEDGE_AFTER_MS`]); what is certain is that `tone
    /// status` printed `audio=live` over a dead-silent synth for hours, and
    /// this verdict is what would have told the two apart. `None` on the
    /// healthy, sealed, and non-macOS forms.
    pub fn wedged_for(&self) -> Option<Duration> {
        #[cfg(test)]
        if self.capture.is_some() {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            // A sealed host (no ingress) outranks wedged.
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

    /// Saturating count of cues dropped on a full ingress channel — the loss
    /// the wedge (or a plain burst) actually cost, surfaced so status verbs
    /// can print it instead of keeping it a private counter.
    pub fn dropped_cues(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            self.dropped.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// A wedge was just observed at the ingress. With budget remaining, spend
    /// one revival: abandon the stuck worker (its thread cannot be cancelled —
    /// drop detaches it, and it disposes its own queue if the call ever
    /// returns) and stand up a fresh channel + worker so sound returns on the
    /// next cue. With the budget exhausted, seal ingress instead: `is_live`
    /// goes `false` and every status verb reads inert, rather than a fourth
    /// hour of healthy-looking silence.
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
    fn test_ingress() -> (Self, std::sync::mpsc::Receiver<Cue>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

        let (tx, rx) = cue_channel();
        (
            Self {
                tx: Some(tx),
                shutdown: Arc::new(AtomicBool::new(false)),
                state: Arc::new(AtomicU8::new(STATE_DORMANT)),
                dropped: Arc::new(AtomicU64::new(0)),
                busy: Arc::new(AtomicU64::new(0)),
                revives_left: WEDGE_REVIVES,
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
            let Some(worker) = self.worker.take() else {
                return;
            };
            // A WEDGED worker (stuck inside one platform call past the
            // threshold) cannot observe the shutdown flag; joining it would
            // carry the wedge onto this — the event-loop — thread. Detach it:
            // the thread keeps blocking harmlessly, and if its call ever
            // returns it sees `shutdown` at the loop top, exits, and its
            // `MacOut` disposes the queue.
            if self.busy_stale_ms().is_some() {
                drop(worker);
                return;
            }
            // Healthy teardown stays effectively synchronous: a parked worker
            // wakes from the closed channel immediately, a running one returns
            // within a housekeeping interval — both land well inside the
            // deadline. Only a platform call slower than the deadline is
            // detached, and it still self-terminates as above.
            let deadline = std::time::Instant::now() + DETACH_DEADLINE;
            while !worker.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(DETACH_POLL);
            }
            if worker.is_finished() {
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
        CHANNELS, EventMeta, SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth,
        WordGesture,
    };

    use super::mac::{BLOCK_S, QueueCycle, block_lead_s, prime_and_start, stop_and_reclaim};
    use super::{
        AudioWorkerOutput, COMMAND_CAPACITY, Cue, DETACH_DEADLINE, STATE_DORMANT, STATE_FAILED,
        STATE_PAUSED, STATE_RUNNING, STATE_STOPPED, TrailAudio, WEDGE_REVIVES, WorkerFlags,
        cue_channel, monotonic_ms, worker_loop,
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
            shifted: false,
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
                    shifted: false,
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
            // Retired boolean gate: disabling/re-enabling carried no generation
            // identity, so an old callback could alias the later run.
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

    /// THE PRE-ROLL ACROSS A PARK (THE PRISM §3.4 c, §2.4 item 5): the cues
    /// that arrive in the first block after a resume prime measure their
    /// pre-roll against the PRIME, not against the last block the callback
    /// rendered before the park. Driven through the exact generic helpers
    /// `MacOut` uses, on the callback-free fake with a fake clock, and the
    /// lead read through the same pure law `MacOut::push_meta` applies.
    /// The negative control is the retired code: a prime that did not stamp
    /// left the parked block's start in place, and a cue 3 ms after the
    /// resume read `now - <a stamp seconds old>`, clamped to the full block
    /// — a once-per-restart onset error of (10.667 - d) ms on the second key
    /// of every burst that began after a think-pause.
    #[test]
    fn a_resume_prime_restamps_the_block_so_the_next_cues_pre_roll_is_its_true_offset() {
        const MS: u64 = 1_000;
        let mut queue = CallbackFreeQueue::new();

        // Before any prime nothing is stamped and nothing is running: 0.
        assert_eq!(queue.lead_now_s(), 0.0);

        // Cold start at t = 1 s. The first cue is pushed while the queue is
        // not running (lead 0 by law), then primed.
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

        // Steady state: the callback stamps each block as it begins.
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

        // Silence: the queue parks for five seconds. Parked, the law reads 0
        // whatever the stamp says.
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

        // The think-pause ends with a burst. Key 1 wakes the queue (lead 0),
        // the prime runs, and key 2 arrives 3 ms later, inside the first
        // block after the resume.
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

        // The retired code, for the record: no stamp at the prime, so the
        // same key 2 measured against the block before the park and clamped
        // to the whole block — an error of BLOCK_S - 3 ms on that one key.
        let retired = block_lead_s(true, last_callback_us, queue.clock_us);
        assert_eq!(
            retired, BLOCK_S,
            "the retired prime clamped key 2 to a full block"
        );
        assert!(
            retired - key2 > 0.007,
            "the asymmetry this pin keeps out: {retired} vs {key2}"
        );

        // And once the first callback lands, prime and callback stamps are
        // one continuous clock.
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
    }

    struct FakeOutput {
        shared: Arc<FakeShared>,
        running: bool,
    }

    impl AudioWorkerOutput for FakeOutput {
        fn push_meta(&mut self, _ev: SoundEvent, meta: EventMeta) -> bool {
            // The stamp lands BEFORE the count a test waits on.
            self.shared.last_at_ms.store(meta.at_ms, Ordering::Release);
            self.shared.pushes.fetch_add(1, Ordering::Release);
            while self.shared.block_push.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
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

    /// What [`spawn_fake_worker`] hands back: cue ingress, the shutdown flag,
    /// the state and busy cells, and the worker's join handle.
    type FakeWorkerHandles = (
        std::sync::mpsc::SyncSender<Cue>,
        Arc<AtomicBool>,
        Arc<AtomicU8>,
        Arc<std::sync::atomic::AtomicU64>,
        std::thread::JoinHandle<()>,
    );

    fn spawn_fake_worker(shared: Arc<FakeShared>) -> FakeWorkerHandles {
        let (tx, rx) = cue_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(STATE_DORMANT));
        let busy = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_state = Arc::clone(&state);
        let worker_busy = Arc::clone(&busy);
        let worker = std::thread::spawn(move || {
            let factory_shared = Arc::clone(&shared);
            worker_loop(
                rx,
                WorkerFlags {
                    shutdown: &worker_shutdown,
                    state: &worker_state,
                    busy: &worker_busy,
                },
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
        (tx, shutdown, state, busy, worker)
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
        let (tx, shutdown, state, _busy, worker) = spawn_fake_worker(Arc::clone(&shared));
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

    #[test]
    fn worker_device_failure_is_terminal_and_never_retried() {
        let shared = Arc::new(FakeShared::default());
        shared.fail_open.store(true, Ordering::Relaxed);
        let (tx, _shutdown, state, _busy, worker) = spawn_fake_worker(Arc::clone(&shared));
        tx.send(cue().into()).unwrap();
        wait_until("explicit open failure", || {
            state.load(Ordering::Acquire) == STATE_FAILED
        });
        worker.join().unwrap();
        assert_eq!(shared.opens.load(Ordering::Relaxed), 1);
        assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
        assert!(tx.send(cue().into()).is_err(), "failed worker is terminal");
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

    /// The busy stamp brackets exactly the platform sections: nonzero and
    /// stable while the worker sits inside one call, zero once it parks. This
    /// is the observable the wedge verdict is built from.
    #[test]
    fn platform_busy_stamps_calls_and_clears_at_park() {
        let shared = Arc::new(FakeShared::default());
        shared.block_push.store(true, Ordering::Release);
        let (tx, shutdown, state, busy, worker) = spawn_fake_worker(Arc::clone(&shared));

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

    /// The field incident's shape: channel full AND the worker stuck inside
    /// one platform call past the threshold. One push both reports the wedge
    /// and spends a revival — fresh channel, fresh worker, old ingress dead.
    #[test]
    fn wedged_full_ingress_revives_within_budget() {
        let (mut audio, rx) = TrailAudio::test_ingress();
        audio.revives_left = 1;
        for _ in 0..COMMAND_CAPACITY {
            audio.push(cue());
        }
        // Stamp 1 = the clock's origin, which sits past the threshold by
        // construction (see `monotonic_ms`) — a wedge, deterministically.
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
        // The fresh worker never receives a cue, so it parks without touching
        // the device and this drop joins it synchronously.
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

    /// Dropping a WEDGED host must not carry the wedge onto the calling
    /// (event-loop) thread: the stuck worker is detached, not joined. The
    /// regression this pins: the former unconditional `join()` would have
    /// frozen the UI forever on the serious-mode toggle and on quit.
    #[test]
    fn dropping_a_wedged_host_detaches_instead_of_freezing() {
        let (mut audio, _rx) = TrailAudio::test_ingress();
        // Stand-in for a thread stuck in a platform call: parked until the
        // keeper is dropped, which this test does only AFTER the drop returns.
        let (keeper_tx, keeper_rx) = std::sync::mpsc::channel::<()>();
        audio.worker = Some(std::thread::spawn(move || {
            let _ = keeper_rx.recv();
        }));
        audio.busy.store(1, std::sync::atomic::Ordering::Release);

        let begun = std::time::Instant::now();
        drop(audio);
        assert!(
            begun.elapsed() < DETACH_DEADLINE,
            "a wedged worker is detached immediately, never joined"
        );
        drop(keeper_tx);
    }

    /// The healthy teardown contract survives the bounded join: a parked
    /// shipping worker is woken by the closing channel and joined well inside
    /// the deadline.
    #[test]
    fn dropping_a_healthy_host_stays_synchronous() {
        let audio = TrailAudio::new(true);
        let begun = std::time::Instant::now();
        drop(audio);
        assert!(
            begun.elapsed() < DETACH_DEADLINE,
            "a parked worker joins promptly on teardown"
        );
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
                shifted: false,
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
            shifted: false,
        });
        std::thread::sleep(std::time::Duration::from_millis(1200));
        // Worker-owned housekeeping eventually pauses without any render tick.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_ne!(audio.state(), STATE_FAILED);
    }

    /// `push` IS `push_meta` with the identity side-car (`RAINBOW-KITTY-V2.md`
    /// §16 rows 6-8): every pre-v2 caller keeps its behaviour by
    /// construction, and a stamped cue reaches the host with the stamp the
    /// seam gave it — both delivery paths hand the same two numbers over.
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
        let (tx, shutdown, _state, _busy, worker) = spawn_fake_worker(Arc::clone(&shared));
        tx.send(Cue {
            ev: cue(),
            meta: EventMeta {
                at_ms: 31_337,
                glyph_class: 1,
                rank: 40,
                pan_from: 0.0,
                block_lead_s: 0.0,
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
}
