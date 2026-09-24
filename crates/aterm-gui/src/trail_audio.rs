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
#[cfg(target_os = "macos")]
use std::time::Instant;

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

/// How long a DEVICE OPEN may run before it is judged a wedge.
///
/// Five times [`WEDGE_AFTER_MS`], because an open is not a steady-state
/// control call: it is the one section whose honest duration is set by
/// hardware the worker is waiting on (a Bluetooth output negotiating, a
/// device switched out from under us, a coreaudiod restarting after wake),
/// and the reopen ladder already bounds how many of them a session may run.
/// An open that has not returned in fifteen seconds is genuinely stuck.
#[cfg(target_os = "macos")]
const OPEN_WEDGE_AFTER_MS: u64 = WEDGE_AFTER_MS * 5;

/// HOW LONG WITHOUT A CALLBACK IS A STALL (D4). The queue says it is running
/// and it was armed by a successful start, yet AudioToolbox has not invoked
/// `render_cb` for this long: a coreaudiod restart, a bound device that
/// disappeared, or a post-sleep invalidation. Before this existed, the
/// `silent` counter — incremented ONLY inside the callback — simply froze,
/// the idle pause never fired, `running` never cleared, and every later
/// keystroke was pushed into a synth nobody read, with `dropped=0` and
/// `audio=live` throughout.
///
/// THE ARITHMETIC, so the number is not a vibe: one buffer period is
/// `BUFFER_FRAMES / SAMPLE_RATE` = 10.667 ms and the whole 3-buffer FIFO
/// drains in 32 ms, so 750 ms is 70 buffer periods, 23 complete FIFO drains
/// and 3 housekeeping intervals — no scheduling hiccup a 3-buffer queue can
/// survive reaches it. Yet a real invalidation costs under a second of
/// typing on the tick path, and nothing at all on the key path, which checks
/// it on every push.
///
/// IT CANNOT FIRE ON A HEALTHY PARKED QUEUE, by construction rather than by
/// margin: after the idle stop `running` is false, so `worker_loop` takes the
/// blocking `recv` arm and `on_tick` is not called at all; and throughout the
/// whole [`PAUSE_AFTER_SILENT`] window (1493 ms) the queue IS calling back —
/// it renders digital silence every 10.667 ms — so the stamp is never older
/// than one block. If a future change ever polls `on_tick` while parked, this
/// watchdog starts reopening a perfectly healthy idle queue.
#[cfg(target_os = "macos")]
const STALL_AFTER_MS: u64 = 750;

/// Lifetime budget of DEVICE REOPENS (D5). One transient AudioToolbox failure
/// used to be terminal for the process: a failed lazy open, a failed
/// `AudioQueueStart` or one failed enqueue stored `STATE_FAILED` and returned,
/// dropping the receiver, so the next keystroke sealed ingress permanently.
/// Budget exhaustion is now the ONLY terminal state — exhaustion stays
/// explicit and observable, which was the half of the old law worth keeping.
#[cfg(target_os = "macos")]
const REOPEN_BUDGET: u8 = 6;

/// Wait before the n-th reopen ATTEMPT. The first is IMMEDIATE because the
/// overwhelmingly likely fault is transient (a device switch, a coreaudiod
/// restart, a wake) and an immediate reopen makes the NEXT key sound; the
/// schedule then quadruples, so a genuinely absent device costs ~44 s of
/// attempts across the whole session and then stops burning platform calls.
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

    /// THE STALL VERDICT (D4), pure so a fake clock proves it with no device.
    ///
    /// STALLED means: the queue says it is running, it has been ARMED (a
    /// successful start stamped `last_cb_us`), and no callback has arrived
    /// for [`super::STALL_AFTER_MS`]. `false` while stopped and `false` while
    /// never armed — a queue that has not started cannot have stopped calling
    /// back, and reporting one that had not is the shape this whole audit is
    /// about: a bound on our own patience returned as a fact about the
    /// device. Both conjuncts are load-bearing; dropping either turns the
    /// healthy 1.5 s idle window, or a cold `MacOut`, into a false stall.
    pub(super) fn callback_stalled(running: bool, last_cb_us: u64, now_us: u64) -> bool {
        running
            && last_cb_us != 0
            && now_us.saturating_sub(last_cb_us) >= super::STALL_AFTER_MS * 1_000
    }

    /// What servicing the queue decided. There is deliberately NO error
    /// variant: every fault is a `Reopen`, and the BUDGET — not the verdict —
    /// is what can eventually make silence permanent.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Service {
        Running,
        Paused,
        Reopen,
    }

    /// What delivering a cue decided. `Reopen` means the cue was NOT consumed:
    /// the worker tears the device down, opens a fresh one under the budget,
    /// and re-pushes this same cue — so the key that discovered the fault is
    /// still heard. That is the difference between a watchdog and a bug
    /// report.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Delivery {
        Sounded,
        Reopen,
    }

    /// THE SECOND STALL DETECTOR'S EDGE (D4): raised by [`running_listener`]
    /// whenever AudioToolbox reports that `kAudioQueueProperty_IsRunning`
    /// changed, consumed by the worker in [`DeviceHealth::needs_reopen`].
    ///
    /// It carries NO value on purpose. The notification arrives on an
    /// AudioToolbox thread at a time of AudioToolbox's choosing — after our
    /// own starts and stops as well as after a stop we never asked for — so
    /// the only thing it can honestly say is "the property moved; go and
    /// read it". The READ, and the verdict, happen on the worker thread,
    /// inside the worker's platform-busy stamp, like every other control call
    /// in this file.
    pub(super) struct RunningEdge(AtomicBool);

    impl RunningEdge {
        pub(super) const fn new() -> Self {
            Self(AtomicBool::new(false))
        }

        /// The listener's whole body: ONE atomic store. Wait-free — no RMW
        /// loop, no lock, no allocation, no logging — so the AudioToolbox
        /// thread that delivers it can never wait on us.
        pub(super) fn raise(&self) {
            self.0.store(true, Ordering::Release);
        }

        /// Consume a pending edge. The plain load first keeps the healthy
        /// per-key path to one relaxed read — no read-modify-write on a cache
        /// line the listener may be writing.
        pub(super) fn take(&self) -> bool {
            self.0.load(Ordering::Relaxed) && self.0.swap(false, Ordering::AcqRel)
        }
    }

    /// THE `IsRunning` PROPERTY LISTENER — runs on an AudioToolbox thread.
    ///
    /// Matches [`render_cb`]'s real-time discipline and goes further: it
    /// takes no lock at all. It never reads the property (that is a platform
    /// call, and may block on the queue's own internal lock), never calls back
    /// into the control path and never pushes a cue — a listener thread must
    /// not become a second driver of the device, which is this file's founding
    /// rule ("a dormant audio worker owns all platform control calls").
    ///
    /// `user` is the address of the [`Shared::running_edge`] field inside the
    /// queue's `Arc<Shared>` allocation. That allocation is kept alive by the
    /// raw refcount [`AudioToolboxQueue::user`] owns, which [`OwnedQueue`]'s
    /// `Drop` releases only AFTER it has removed this listener and disposed
    /// the queue synchronously — so every invocation AudioToolbox can make
    /// finds the edge alive. A queue whose worker was ABANDONED mid-call is
    /// never disposed, and so never releases that refcount: the allocation
    /// leaks with it, it is never freed under a live listener.
    ///
    /// Removal is the FIRST line of defence, not the only one. AudioToolbox
    /// does not promise that `AudioQueueRemovePropertyListener` waits out an
    /// invocation already in flight on its thread, so the refcount is held
    /// past the synchronous `AudioQueueDispose(queue, true)` as well — the
    /// same guarantee [`render_cb`]'s userdata has always rested on. This
    /// detector adds no lifetime assumption the file did not already make.
    pub(super) extern "C" fn running_listener(
        user: *mut c_void,
        _q: AudioQueueRef,
        id: AudioQueuePropertyID,
    ) {
        if id != PROP_IS_RUNNING || user.is_null() {
            return;
        }
        // SAFETY: see the ownership argument above — `user` points at a
        // `RunningEdge` that outlives every listener invocation.
        let edge = unsafe { &*(user as *const RunningEdge) };
        edge.raise();
    }

    /// What one read of `IsRunning` means, given what the worker already
    /// knows about this start. Pure, so every cell is provable headless.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum RunningProbe {
        /// It reads running. Nothing to claim, and from now on a stop IS a
        /// stop: the worker has seen this start actually take.
        Running,
        /// We started it, we SAW it running, and it now reads stopped — and
        /// the worker (the only thread that stops it) did not. AudioToolbox
        /// stopped the queue under us: a device that vanished, a coreaudiod
        /// restart, a post-sleep invalidation.
        StoppedUnderUs,
        /// It reads stopped, but this start has not yet been seen running
        /// and no callback has arrived since it: AudioToolbox says the
        /// running notification may land AFTER `AudioQueueStart` returns, so
        /// this may be a start still taking. Look again on the next push or
        /// tick — the edge is re-raised.
        NotYetRunning,
        /// No reading (the property call failed), or a reading that
        /// contradicts the callbacks we HAVE seen. Claim nothing; the 750 ms
        /// callback watchdog still stands behind this detector.
        Abstain,
    }

    /// THE `IsRunning` VERDICT (D4, second detector).
    ///
    /// `seen_running`: the worker has read `IsRunning != 0` since its last
    /// start. `called_back`: a render callback has arrived since that start.
    /// `property`: this read, `None` if the property call failed.
    ///
    /// The one claim it can make is `StoppedUnderUs`, and it demands a
    /// WITNESSED 1 → 0 within one start: a reading of 0 alone is also what a
    /// start still taking looks like, and reopening a healthy queue for it
    /// would be the listener manufacturing the fault it exists to find. The
    /// cost of that strictness is bounded, not open: a queue AudioToolbox
    /// stops before the worker ever read it running falls to the 750 ms
    /// callback watchdog, exactly as it did before this detector existed.
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

    /// The worker's per-start knowledge about whether the device is alive,
    /// and THE ONE DEATH VERDICT both service paths read.
    ///
    /// [`MacOut::push_meta`] and [`MacOut::on_tick`] each used to carry their
    /// own copy of "faulted, or stalled"; a third detector added to two
    /// copies is how the key path and the tick path come to disagree. Now
    /// both ask this, and all three detectors — the callback's `faulted`
    /// latch, the 750 ms callback watchdog and the `IsRunning` listener —
    /// feed the SAME answer, which the worker turns into the same `Reopen`
    /// on the same budgeted ladder. There is no second ladder.
    pub(super) struct DeviceHealth {
        /// `last_cb_us` exactly as the last start armed it; a later value is
        /// a real callback.
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

        /// A start succeeded and armed the watchdog at `armed_cb_us`. What was
        /// seen of the previous run says nothing about this one.
        pub(super) fn armed(&mut self, armed_cb_us: u64) {
            self.armed_cb_us = armed_cb_us;
            self.seen_running = false;
        }

        /// Whether the device must be REOPENED. `read_is_running` is the
        /// `IsRunning` property read, called at most once and ONLY when the
        /// listener raised an edge while the worker believes the queue is
        /// running — the healthy per-key path makes no platform call here.
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
            // Not running by the worker's own account: the edge is our own
            // stop (or a start not yet made). Only the worker stops the queue,
            // and it is the thread asking, so this cannot hide a stop under
            // us — and it spends no platform call.
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
        fn AudioQueueAddPropertyListener(
            in_aq: AudioQueueRef,
            in_id: AudioQueuePropertyID,
            in_proc: AudioQueuePropertyListenerProc,
            in_user_data: *mut c_void,
        ) -> i32;
        fn AudioQueueRemovePropertyListener(
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

    /// `kAudioQueueProperty_IsRunning` ('aqrn'): a read-only `UInt32`,
    /// nonzero while the queue runs. AudioToolbox notifies its listeners when
    /// the queue starts or stops — "which may occur sometime after the
    /// AudioQueueStart or AudioQueueStop function is called", so the
    /// notification is an EDGE to go and look, never a value to trust.
    pub(super) const PROP_IS_RUNNING: AudioQueuePropertyID = 0x6171_726E;

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
        /// THE INSTANT THE CALLBACK LAST RAN, in [`crate::metrics::now_us`]
        /// µs — the D4 watchdog's heartbeat. Stamped as the FIRST statement
        /// of [`render_cb`], before the epoch check, so the flush callbacks
        /// AudioToolbox delivers during our own stop count as progress and
        /// our stop can never look like a stall. `0` until a start arms it.
        ///
        /// DELIBERATELY A SEPARATE WORD from [`Self::block_start_us`], which
        /// is ALSO written by the worker at every prime
        /// ([`QueueCycle::stamp_block_start`]) — so a queue that opened,
        /// primed and then never called back would read fresh at exactly the
        /// moment D4 describes — and which is a semantic value in the
        /// pre-roll latency contract, where a future change would disarm this
        /// watchdog invisibly. Two meanings, two words.
        last_cb_us: AtomicU64,
        /// The `IsRunning` listener's edge (D4, second detector). The
        /// listener's userdata is THIS FIELD's address, so it lives exactly
        /// as long as the allocation the queue's raw refcount pins.
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

        /// THE ONE DEATH VERDICT, read from this queue's shared words. Both
        /// service paths — [`MacOut::push_meta`] on every key and
        /// [`MacOut::on_tick`] on the housekeeping tick — call exactly this,
        /// so the callback's `faulted` latch, the 750 ms callback watchdog
        /// and the `IsRunning` listener's edge reach the worker as ONE answer
        /// and are turned into the SAME `Reopen` on the SAME budgeted ladder.
        /// Worker thread only: `read_is_running` is a platform call.
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

    /// The platform calls that bracket a queue's life, as a seam: the ORDER
    /// of the teardown half is the listener's memory-safety contract, and a
    /// seam is what lets a headless test observe that order with no device.
    pub(super) trait QueueOwnerCalls {
        /// Register the `IsRunning` listener. `false` when AudioToolbox
        /// refused it; the queue is still usable, only without the second
        /// detector.
        fn add_running_listener(&mut self) -> bool;
        fn remove_running_listener(&mut self);
        /// `AudioQueueDispose(queue, immediate = true)` — synchronous.
        fn dispose(&mut self);
        /// Release the `Arc<Shared>` refcount handed to AudioToolbox as the
        /// callbacks' userdata.
        fn release_user(&mut self);
    }

    /// THE ONE OWNER OF A LIVE AUDIO QUEUE, and the only place in this file
    /// that disposes one.
    ///
    /// A property listener that fires against a disposed queue whose `Shared`
    /// has been released is a use-after-free, so the listener must come off
    /// before `AudioQueueDispose` on EVERY path that ends a queue: ordinary
    /// teardown, the reopen after a fault (the worker drops the whole
    /// `MacOut`), the late exit of a worker that was abandoned mid-call, and
    /// `MacOut::new`'s buffer-allocation unwind. Those paths used to each
    /// spell their own dispose; a new one could forget. Here they are all
    /// the same `Drop`:
    ///
    /// 1. remove the listener, if it was registered;
    /// 2. dispose the queue synchronously, which also retires `render_cb`;
    /// 3. only then release the callbacks' userdata refcount.
    ///
    /// Constructed the instant `AudioQueueNewOutput` succeeds, so no early
    /// return after that point can skip it. The test
    /// `the_queue_is_disposed_in_exactly_one_place` pins the "only place"
    /// half lexically, and
    /// `the_queue_owner_removes_the_listener_before_dispose_on_every_path`
    /// pins the order through a recording double.
    pub(super) struct OwnedQueue<P: QueueOwnerCalls> {
        calls: P,
        listening: bool,
    }

    impl<P: QueueOwnerCalls> OwnedQueue<P> {
        pub(super) fn new(calls: P) -> Self {
            Self {
                calls,
                listening: false,
            }
        }

        /// Register the `IsRunning` listener; remembered so teardown removes
        /// exactly what was added.
        pub(super) fn listen(&mut self) -> bool {
            self.listening = self.calls.add_running_listener();
            self.listening
        }

        pub(super) fn calls(&self) -> &P {
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

    /// The real [`QueueOwnerCalls`]: one AudioToolbox queue plus the raw
    /// `Arc<Shared>` refcount its callbacks were handed.
    struct AudioToolboxQueue {
        queue: AudioQueueRef,
        /// `Arc::into_raw` of the queue's `Shared` — the `render_cb`
        /// userdata, and the refcount that keeps [`Shared::running_edge`]
        /// alive for the listener. Released by [`OwnedQueue`]'s `Drop` only.
        user: *const Shared,
    }

    impl AudioToolboxQueue {
        fn queue(&self) -> AudioQueueRef {
            self.queue
        }

        /// The listener's userdata: the edge field inside the pinned
        /// allocation. The SAME value must reach add and remove, because a
        /// listener is identified by (proc, userdata).
        fn edge_user(&self) -> *mut c_void {
            // SAFETY: `user` is a live `Arc<Shared>` raw pointer until
            // `release_user`, which runs last; taking a field's address does
            // not create a reference.
            unsafe { std::ptr::addr_of!((*self.user).running_edge) as *mut c_void }
        }

        /// Read `kAudioQueueProperty_IsRunning`. Worker thread only.
        fn read_is_running(&self) -> Option<bool> {
            let mut value: u32 = 0;
            let mut size = std::mem::size_of::<u32>() as u32;
            // SAFETY: the queue is live (owned, not yet disposed); `value`
            // and `size` outlive the call and `size` states the buffer.
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
            // SAFETY: the queue is live; the userdata's lifetime is argued
            // at `running_listener`.
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
            // The status is deliberately ignored: there is nothing to do with
            // a refusal here, and the synchronous dispose that follows retires
            // every listener the queue still holds before the userdata's
            // refcount is released (see `running_listener`).
            // SAFETY: the queue is live (dispose has not run); proc and
            // userdata are the exact pair that was added.
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
            // SAFETY: synchronous dispose (immediate = 1) stops the callback
            // thread before `release_user` drops the refcount it was using.
            unsafe {
                AudioQueueDispose(self.queue, 1);
            }
        }

        fn release_user(&mut self) {
            // SAFETY: `user` came from `Arc::into_raw` in `MacOut::new` and
            // is released exactly once, here, after the queue is gone.
            unsafe { drop(Arc::from_raw(self.user)) };
        }
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
        // THE WATCHDOG HEARTBEAT (D4), first thing and BEFORE the epoch check
        // so the flush callbacks of our own stop count as progress. One
        // relaxed store: no lock, no allocation, RT-safe.
        shared
            .last_cb_us
            .store(crate::metrics::now_us(), Ordering::Relaxed);
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
        /// The queue, its `IsRunning` listener and the callbacks' userdata
        /// refcount, owned as ONE value whose `Drop` removes the listener,
        /// disposes the queue and only then releases the refcount — the
        /// single teardown every path that ends a queue goes through
        /// ([`OwnedQueue`]).
        owned: OwnedQueue<AudioToolboxQueue>,
        shared: Arc<Shared>,
        /// All buffers are allocated for the queue's lifetime. Pointer values
        /// remain stable, but the worker dereferences them only initially or
        /// after synchronous immediate stop has removed scheduled ownership.
        buffers: [*mut AudioQueueBuffer; BUFFER_COUNT],
        buffers_available: bool,
        /// Per-start knowledge the `IsRunning` verdict needs (D4, second
        /// detector). Worker-thread only, like every field here.
        health: DeviceHealth,
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
                // queue so it isn't leaked. No queue exists, so there is no
                // listener and nothing to dispose.
                unsafe { drop(Arc::from_raw(user as *const Shared)) };
                return None;
            }
            // OWNED FROM THE INSTANT IT EXISTS. Every `return None` below this
            // line drops `owned`, and its `Drop` is the one teardown: listener
            // off, queue disposed, refcount released — in that order. The
            // buffer-allocation unwind used to spell its own dispose; it now
            // cannot forget the listener because it no longer spells anything.
            let mut owned = OwnedQueue::new(AudioToolboxQueue {
                queue,
                user: user as *const Shared,
            });
            // THE SECOND STALL DETECTOR (D4). Registered before the buffers are
            // allocated, so the unwind below is a path that really does have
            // a listener to remove. A refusal is not fatal: the queue plays,
            // and the 750 ms callback watchdog still stands.
            owned.listen();
            let bytes = (BUFFER_FRAMES * CHANNELS * 4) as u32;
            let mut buffers = [std::ptr::null_mut(); BUFFER_COUNT];
            for slot in &mut buffers {
                let mut buf: *mut AudioQueueBuffer = std::ptr::null_mut();
                // SAFETY: queue is live; on success the pointer stays allocated
                // until queue disposal. It remains unscheduled/available here.
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
                // ARM the stall watchdog — not FEED it. The worker writes
                // this exactly once per start transition, so the threshold
                // measures "no callback since the queue actually began
                // running"; from here on only the callback writes it.
                let armed_cb_us = crate::metrics::now_us();
                self.shared.last_cb_us.store(armed_cb_us, Ordering::Relaxed);
                // …and forget what was seen of the previous run: the
                // `IsRunning` verdict demands a 1 → 0 witnessed WITHIN this
                // start.
                self.health.armed(armed_cb_us);
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
        pub fn push_meta(&mut self, ev: SoundEvent, meta: EventMeta) -> Delivery {
            // THE KEY PATH DISCOVERS THE FAULT (D7). The death verdict was
            // previously read only by `on_tick`, which runs only after the
            // worker's 250 ms receive timeout expires — i.e. only while
            // NOBODY is typing. While cues arrive closer together than that, a
            // dead queue was never noticed at all. The cue is NOT consumed:
            // the worker reopens and re-pushes it. The SAME verdict `on_tick`
            // asks — all three detectors, one answer.
            if self.device_needs_reopen() {
                return Delivery::Reopen;
            }
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
            if !self.shared.running.load(Ordering::Relaxed) && !self.start() {
                // A failed lazy open. The cue is already in the dying synth,
                // so re-pushing it into the fresh device plays it ONCE — the
                // discarded synth was never heard — not twice.
                return Delivery::Reopen;
            }
            if self.shared.running.load(Ordering::Acquire) {
                Delivery::Sounded
            } else {
                Delivery::Reopen
            }
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
        pub fn on_tick(&mut self) -> Service {
            // A faulted, stalled or stopped-under-us queue must REOPEN, never
            // "pause": asking the death verdict FIRST is what keeps a dead
            // device from being mistaken for an idle one.
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

        pub fn is_running(&self) -> bool {
            self.shared.running.load(Ordering::Acquire)
        }

        /// THE ONE DEATH VERDICT for this device (see
        /// [`Shared::device_needs_reopen`]). The `IsRunning` read is handed
        /// in as a closure so it happens only when the listener raised an
        /// edge while the queue is running — never on the healthy key path.
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
            // Retire callback recycling first, so no callback re-enqueues
            // while the queue goes down. The teardown itself is `owned`'s
            // `Drop`, which runs right after this body: listener removed,
            // queue disposed synchronously, and only then the callbacks'
            // userdata refcount released. (`self.shared`'s own refcount drops
            // normally after that.)
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

/// Set on the busy stamp while the section it covers is a DEVICE OPEN.
///
/// The stamp is a millisecond clock, so its top bit is free for the whole
/// life of any process; carrying the phase IN the same word is what keeps
/// the reader's verdict atomic — a separate `opening` flag beside the stamp
/// could be read torn, and the torn reading is exactly the one that costs a
/// revival.
#[cfg(target_os = "macos")]
const BUSY_OPENING: u64 = 1 << 63;

/// RAII stamp around every worker platform section (device open, cue apply,
/// pause housekeeping). While held, the cell carries the section's entry
/// instant; every exit path — including the failure returns — clears it. The
/// UI reads the cell to tell a parked worker (0: healthy by definition, the
/// next send wakes it) from one stuck inside a single platform call (stale
/// nonzero: the wedge a bare channel-liveness bit can never see).
///
/// A DEVICE OPEN IS A DIFFERENT OPERATION and is marked as one
/// ([`Self::mark_open`]): `AudioQueueNewOutput` + three
/// `AudioQueueAllocateBuffer`s against a Bluetooth device, a device just
/// switched, or a coreaudiod still coming back after wake can legitimately
/// run for seconds. Judging it at [`WEDGE_AFTER_MS`] reads "this is opening"
/// as "this is stuck", and the cure — [`TrailAudio::revive_or_seal`] —
/// abandons the thread that was about to succeed, three of which seal audio
/// for the process. Guard the OPERATION, not the call site.
#[cfg(target_os = "macos")]
struct PlatformBusy<'a>(&'a std::sync::atomic::AtomicU64);

#[cfg(target_os = "macos")]
impl<'a> PlatformBusy<'a> {
    fn mark(cell: &'a std::sync::atomic::AtomicU64) -> Self {
        cell.store(monotonic_ms().max(1), std::sync::atomic::Ordering::Release);
        Self(cell)
    }

    /// Mark a DEVICE OPEN: the same stamp, judged at [`OPEN_WEDGE_AFTER_MS`].
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

/// THE WEDGE VERDICT, as a pure function of the stamp and the clock —
/// `Some(age_ms)` when the worker has been inside one platform call too long,
/// `None` for a parked worker (stamp 0, healthy by definition) or a call
/// still young enough to be presumed healthy.
///
/// Pure and separately tested for the same reason [`mac::callback_stalled`]
/// is: it is a bound on OUR OWN PATIENCE being reported as a fact about the
/// device, so the exact thresholds must be provable on a fabricated clock
/// rather than raced against a real one. It also cannot be proved live at
/// both thresholds — [`monotonic_ms`]'s origin sits only `WEDGE_AFTER_MS + 1`
/// in the past, so no stamp a young process can fabricate is
/// [`OPEN_WEDGE_AFTER_MS`] old.
///
/// The threshold depends on WHICH operation the stamp covers: a device open
/// carries [`BUSY_OPENING`] and is judged at [`OPEN_WEDGE_AFTER_MS`]. The
/// phase rides IN the stamp word, so a caller reads both with one atomic load
/// and can never see them out of step.
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
    /// Cues the worker consumed while INSIDE a reopen backoff window (D8) —
    /// a real loss with its own name, so it is never confused with the
    /// ingress-full drops.
    dropped_backoff: &'a std::sync::atomic::AtomicU64,
    /// Device reopens the worker has left, surfaced so `tone` can print it.
    reopens_left: &'a std::sync::atomic::AtomicU8,
}

/// The complete UI-thread ingress decision. `try_send` is structurally
/// nonblocking; a full channel preserves all queued cues, drops only the newest
/// cue, and records that loss with a saturating counter. Both formal actions
/// refine this one shipping branch point.
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
    // ONE fault handler for every way the device can die — a failed open, a
    // failed start or enqueue, a faulted callback, a stalled callback, a
    // queue AudioToolbox stopped under us (the `IsRunning` listener). It
    // drops the whole `Output` (whose `Drop` disposes the queue
    // synchronously) and opens a FRESH one, deliberately at the DEVICE OBJECT
    // level: nothing retries `start()` on a `MacOut` whose buffers are
    // already scheduled, so the ownership contract at `MacOut::start` and the
    // `trail_audio_start_latency_model` it backs are untouched.
    macro_rules! reopen_or_die {
        ($now:expr) => {
            // Dropping the old `Output` disposes the queue SYNCHRONOUSLY, and
            // disposing a device that has already stopped answering is the
            // slowest platform call in this file. It is device teardown, so it
            // is stamped as an open ([`PlatformBusy::mark_open`]) and judged at
            // [`OPEN_WEDGE_AFTER_MS`] — otherwise clearing a wedged device
            // reads as a wedge and costs a revival.
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
        // (device open, start, enqueue) — the sections a wedge parks in. Each
        // is stamped for the DURATION IT IS ALLOWED, not as one undifferentiated
        // "busy": `open_device!` marks the device open, which is honestly slow
        // and judged at [`OPEN_WEDGE_AFTER_MS`], and `push_cue!` marks the
        // steady-state enqueue, judged at [`WEDGE_AFTER_MS`]. One stamp over
        // both would have priced a Bluetooth open as a wedge, and the cure for
        // a wedge abandons the worker.
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
        // REOPENS ARE CUE-DRIVEN, not timer-driven: sound only matters when a
        // key happens, and a timer would re-arm the event loop the idle park
        // deliberately disarmed. A cue that arrives inside the wait window is
        // dropped and COUNTED — silently swallowing it is what made the old
        // `dropped=0` a lie.
        if output.is_none() && !retry.ready(now) {
            saturating_increment_u64(dropped_backoff);
            continue;
        }
        if output.is_none() {
            output = open_device!();
            if output.is_none() {
                // A failed device open is no longer terminal (D5). It spends
                // one reopen; only budget exhaustion seals ingress.
                reopen_or_die!(now);
                saturating_increment_u64(dropped_backoff);
                continue;
            }
            // NO `retry.delivered()` HERE. An open that SUCCEEDS has proved
            // nothing about a device that plays: `AudioQueueStart` runs later,
            // inside `push_meta`. Resetting the ladder here made the budget
            // unspendable for the one fault it exists for — a device that
            // constructs fine and never sounds — and the worker then ran a
            // full dispose/open cycle per keystroke forever while `tone`
            // printed `reopens_left=6`.
        }
        let delivery = push_cue!(
            output
                .as_mut()
                .map_or(mac::Delivery::Reopen, |out| out.push_meta(cue.ev, cue.meta))
        );
        if delivery == mac::Delivery::Reopen {
            reopen_or_die!(now);
            // …and RE-PUSH THE SAME CUE into the fresh device, so the key that
            // discovered the fault is still heard. One attempt: if the fresh
            // device fails too, that is another fault against the budget and
            // the next cue carries it.
            if output.is_none() && retry.ready(Instant::now()) {
                output = open_device!();
            }
            match push_cue!(output.as_mut().map(|out| out.push_meta(cue.ev, cue.meta))) {
                Some(mac::Delivery::Sounded) => {
                    // THE DEVICE PLAYED. That — and only that — may reset the
                    // ladder; see `RetryState::delivered`.
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
        // THE DEVICE PLAYED — the only evidence that may reset the ladder.
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
    /// When the device FIRST PLAYED after the last fault — the start of the
    /// healthy window. `None` from every fault until a delivery lands. A
    /// device must keep playing for [`RetryState::healthy_for`] past THIS,
    /// not past the fault, before the ladder resets (see
    /// [`RetryState::delivered`]).
    played_since: Option<Instant>,
    backoff: &'a [Duration],
}

/// What [`reopen_after_fault`] decided.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reopen {
    /// Reopen on this cue.
    Now,
    /// Inside the backoff window; wait for a later cue.
    Wait,
    /// The budget is spent. This — and only this — is terminal.
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

    /// How long a device must PLAY, after its last fault, before the ladder
    /// resets. Derived from the schedule (its longest step: 30 s in the ship
    /// build) rather than being a constant of its own, so the compressed test
    /// schedule scales with it and the law is exercised at test speed.
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

    /// A cue SOUNDED. The schedule resets — but only once the device has
    /// PLAYED for [`Self::healthy_for`]: measured from its first delivery
    /// after the last fault, so the budget bounds CONSECUTIVE failure
    /// without being punished for a lifetime of healthy use.
    ///
    /// THE DELAY IS THE WHOLE POINT, and it is why this is not
    /// `succeeded()`-on-open. Two shapes make a naive reset unspendable, and
    /// both are shapes this lane exists to survive:
    ///
    /// * a device that CONSTRUCTS and never plays — `AudioQueueNewOutput`
    ///   succeeds, `AudioQueueStart` does not. Resetting on the open put
    ///   `attempts` back to 0 before the failure that would have spent it.
    /// * a device that plays ONE block and stalls — it opens, starts, sounds
    ///   the cue that reopened it, and its callback dies again 750 ms later.
    ///   Resetting on that delivery is just as unspendable: the budget is
    ///   restored by the very cue whose successor spends it.
    ///
    /// In both, the fault recurs far inside `healthy_for`, so `attempts`
    /// climbs to the budget and exhaustion — the honestly permanent reading —
    /// is reached. A real transient (a device switch, a wake, a coreaudiod
    /// restart) is followed by a device that plays for minutes, so it costs
    /// one reopen and hands the budget back.
    ///
    /// THE WINDOW STARTS AT THE FIRST DELIVERY, NOT AT THE FAULT. Measured
    /// from the fault it included the backoff WAIT, during which nothing
    /// played — and the schedule's last wait IS `healthy_for` (both are its
    /// longest step), so the first delivery after the final reopen was
    /// "healthy" by construction and handed the whole budget back. A device
    /// that plays one block and stalls then cycled through the ladder
    /// forever, ~6 reopens a minute, and never reached exhaustion — the
    /// second shape above, defeated one step later. The derived
    /// `TrailAudioReopenLadder` machine's two-way conformance found it (the
    /// shipping schedule could not reach a state the model allowed), and
    /// `plays-once-then-stalls` in its worker traces pins it.
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

/// THE ONE FAULT HANDLER. Throws the whole output away — its `Drop` disables
/// callback recycling and disposes the queue synchronously — and arms the next
/// attempt, or reports exhaustion.
/// DELIBERATELY UNANCHORED to `trail_audio_lifecycle`'s `WorkerStartFails`.
/// This function does not latch `failed`; it decides whether the worker may
/// try again. `worker_loop` — which DOES latch it, on `Reopen::Exhausted` —
/// carries that anchor already. An anchor on a fn that does not perform the
/// action passes the spec-link gate while proving nothing (the recorded
/// `is_tmux_mode_active` hole), so this one is left off on purpose.
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
    // Whatever the device played before this fault is not evidence about
    // the one the next cue opens.
    retry.played_since = None;
    if wait.is_zero() {
        Reopen::Now
    } else {
        Reopen::Wait
    }
}

/// Tier-1 conformance of the derived `TrailAudioReopenLadder` machine to the
/// ladder above and to `worker_loop`'s call sites. A child module so it
/// drives the private ladder itself; it lives in its own file.
#[cfg(all(test, target_os = "macos"))]
#[path = "trail_audio_reopen_conformance.rs"]
mod reopen_conformance;

/// Saturating `+1` on a `u64` counter, the `saturating_increment` shape the
/// callback already uses for its `u32` silence counter.
#[cfg(target_os = "macos")]
fn saturating_increment_u64(counter: &std::sync::atomic::AtomicU64) {
    use std::sync::atomic::Ordering;
    let mut current = counter.load(Ordering::Relaxed);
    while current != u64::MAX {
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
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
/// A fault was seen and the worker is inside its bounded reopen schedule.
/// APPEND-ONLY discriminants, mirroring `metrics::DeadlineOwner`.
#[cfg(target_os = "macos")]
const STATE_REOPENING: u8 = 5;

/// The three distinct cue losses, counted apart (D8). One counter used to
/// carry only the ingress-full case, so a cue lost to a sealed host or to a
/// reopen wait was invisible and `dropped=0` read as "nothing was lost".
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct DroppedCues {
    /// The ingress FIFO was full.
    pub(crate) full: u64,
    /// Pushed at a host whose ingress is gone.
    pub(crate) sealed: u64,
    /// Consumed by the worker inside a reopen backoff window.
    pub(crate) backoff: u64,
}

/// The audio host's honest state, as a wire-facing enum rather than a raw
/// atomic word. Until 2026-09-22 the `state` atomic had exactly ONE reader —
/// a `#[cfg(test)]` helper — so the one word that records whether
/// `AudioQueueStart` actually SUCCEEDED could not reach a status verb, and
/// `audio=live` meant only "an ingress channel exists". A process that had
/// never opened a device, one whose open was in flight, and one whose worker
/// had just died all printed the same word.
///
/// `Inert` and `Wedged` keep their exact prior meanings and spellings; the
/// four new words (`opening`, `paused`, `failed`, `stopped`) name states that
/// were previously all spelled `live`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HostState {
    /// Ingress is sealed: a headless/test form, a non-macOS build, or a
    /// permanent failure (including an exhausted wedge-revival budget). It
    /// can never sound.
    Inert,
    /// Ingress is open and no device has been opened yet — the queue opens
    /// LAZILY on the first cue. NOT a fault, and the state every process is
    /// in before it makes its first sound.
    Opening,
    /// `AudioQueueStart` returned OK and the callback is being served.
    Running,
    /// The queue is open and started but parked at idle; the next cue resumes
    /// it.
    Paused,
    /// A platform call failed; ingress is not sealed yet.
    Failed,
    /// A fault was seen and the worker is inside its bounded reopen schedule
    /// — the next cue (or the next one past the backoff window) opens a fresh
    /// device. Recoverable, and the state a transient CoreAudio hiccup used
    /// to skip straight past into permanent silence.
    Reopening,
    /// The queue was stopped.
    Stopped,
    /// Ingress is open but the worker has been stuck inside ONE platform call
    /// past [`WEDGE_AFTER_MS`], so cues are being dropped. Outranks the state
    /// word: a RUNNING worker stuck in a call is wedged, not running.
    Wedged,
}

impl HostState {
    /// Every variant, for the exhaustiveness proof that a seventh state word
    /// cannot reach the wire as a silent fallback to `live`.
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
}

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
    worker_loop(
        rx,
        flags,
        seed,
        HOUSEKEEPING_INTERVAL,
        &REOPEN_BACKOFF,
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
    tx: Option<std::sync::mpsc::SyncSender<Cue>>,
    #[cfg(target_os = "macos")]
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Cues dropped because INGRESS WAS FULL — the original `dropped`
    /// counter, kept under its own name now that three distinct losses are
    /// counted apart (D8).
    #[cfg(target_os = "macos")]
    dropped_full: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Cues pushed at a host whose ingress is GONE, including the one that
    /// discovers the disconnect. It used to return uncounted, so the cue that
    /// proved the worker had died was invisible.
    #[cfg(target_os = "macos")]
    dropped_sealed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Cues the worker consumed while inside a reopen backoff window.
    #[cfg(target_os = "macos")]
    dropped_backoff: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Device reopens the worker has left ([`REOPEN_BUDGET`] at birth).
    #[cfg(target_os = "macos")]
    reopens_left: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Whether this host was born ACTIVE. `TrailAudio::new(false)` is the
    /// headless/test form whose `tx` is `None` from birth: counting sealed
    /// drops there would make `dropped=` a large meaningless number in every
    /// headless run, which is new noise in place of the old lie.
    #[cfg(target_os = "macos")]
    born_active: bool,
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
            let dropped_full = Arc::new(AtomicU64::new(0));
            let dropped_sealed = Arc::new(AtomicU64::new(0));
            let dropped_backoff = Arc::new(AtomicU64::new(0));
            let reopens_left = Arc::new(AtomicU8::new(REOPEN_BUDGET));
            let busy = Arc::new(AtomicU64::new(0));
            if !active {
                return Self {
                    tx: None,
                    shutdown,
                    state,
                    dropped_full,
                    dropped_sealed,
                    dropped_backoff,
                    reopens_left,
                    born_active: false,
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
            let worker_dropped_backoff = Arc::clone(&dropped_backoff);
            let worker_reopens_left = Arc::clone(&reopens_left);
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
                            dropped_backoff: &worker_dropped_backoff,
                            reopens_left: &worker_reopens_left,
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
                dropped_full,
                dropped_sealed,
                dropped_backoff,
                reopens_left,
                born_active: true,
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
        // Nothing AUDIBLE to carry across (§17.3 phase 7): the music box is
        // named on each event's voice, so a fresh worker's synth needs no
        // latch. THE COUNTERS ARE A DIFFERENT MATTER (D8): they are
        // documented as lifetime totals, and a lifetime total that silently
        // resets under a revival is the lie in its purest form — a driver
        // polling `tone` twice must never see `dropped=` go BACKWARDS,
        // least of all at the exact moment the revival it justified happened.
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
        {
            let disposition = self
                .tx
                .as_ref()
                .map(|tx| enqueue_cue(tx, &self.dropped_full, Cue { ev, meta }));
            match disposition {
                Some(EnqueueDisposition::Disconnected) => {
                    // THE DISCOVERING CUE IS A REAL LOSS (D8). It used to
                    // return uncounted, so the one push that PROVED the
                    // worker had died left no trace at all.
                    saturating_increment_u64(&self.dropped_sealed);
                    self.tx = None;
                    self.state
                        .store(STATE_FAILED, std::sync::atomic::Ordering::Release);
                }
                None if self.born_active => {
                    // A push at an already-sealed host. Counted only for a
                    // host that was born active: `new(false)` is the
                    // headless/test form, where counting would manufacture
                    // noise rather than report a loss.
                    saturating_increment_u64(&self.dropped_sealed);
                }
                _ => {}
            }
            // THE WEDGE IS JUDGED ON EVERY PUSH, not only on a full FIFO
            // (D8). A wedged worker with a half-empty channel used to be
            // invisible until 64 cues had accumulated, so the recovery time
            // was bounded by FIFO DEPTH instead of by `WEDGE_AFTER_MS`. The
            // stale-stamp gate stays — it is what stops `revive_or_seal`
            // (which spawns a thread) from firing per keystroke; it can fire
            // at most once per `WEDGE_AFTER_MS`.
            if self.tx.is_some() && self.busy_stale_ms().is_some() {
                self.revive_or_seal();
            }
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
    ///
    /// IT MUST KEEP THIS MEANING. It is the `worker_live` term of
    /// `keystroke_click_audible` (via [`Self::host_state`]'s `Inert` arm),
    /// and the device opens LAZILY on the first cue — so a
    /// device-STARTED predicate here would refuse the very first keystroke
    /// of every session and read as "the sound fix broke the sound". The
    /// honest device state is [`Self::host_state`], which is for the status
    /// ROW and must never become a gate.
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
        busy_stale(
            self.busy.load(std::sync::atomic::Ordering::Acquire),
            monotonic_ms(),
        )
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

    /// THE HOST'S HONEST STATE for a status row — the first production
    /// reader of the worker's `state` atomic, which until 2026-09-22 had none
    /// outside `#[cfg(test)]`.
    ///
    /// Precedence, documented because it is not the obvious one: a sealed
    /// ingress is `Inert` and outranks everything (there is nothing left to
    /// be running); a wedge outranks the state word (a RUNNING worker stuck
    /// inside one platform call is wedged, and the cues it is dropping are
    /// the fact that matters); otherwise the word itself answers.
    ///
    /// It honours the `#[cfg(test)]` capture short-circuit exactly as
    /// [`Self::is_live`] and [`Self::wedged_for`] do, so the predicates
    /// cannot disagree about the same host.
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
                // STATE_DORMANT, and any word a future worker adds: ingress
                // is open and no device is started. Falling back to `Opening`
                // rather than to `Running` is the fail-honest direction — an
                // unknown state must never be reported as a started device.
                _ => HostState::Opening,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            HostState::Inert
        }
    }

    /// The three losses, named apart. `dropped_cues()` is their sum, so the
    /// `dropped=` field keeps its exact prior meaning while a driver that
    /// wants to know WHICH loss happened can now ask.
    pub(crate) fn dropped_breakdown(&self) -> DroppedCues {
        #[cfg(target_os = "macos")]
        {
            use std::sync::atomic::Ordering;
            DroppedCues {
                full: self.dropped_full.load(Ordering::Relaxed),
                sealed: self.dropped_sealed.load(Ordering::Relaxed),
                backoff: self.dropped_backoff.load(Ordering::Relaxed),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            DroppedCues::default()
        }
    }

    /// Device reopens the worker has left. `0` beside `audio=failed` is the
    /// one honestly terminal reading.
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

    /// How many wedge revivals this host has SPENT. A revival stands up a
    /// fresh worker, which is exactly the event that makes `dropped=0`
    /// misleading — so the two are printed side by side.
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

    /// Saturating count of cues dropped on a full ingress channel — the loss
    /// the wedge (or a plain burst) actually cost, surfaced so status verbs
    /// can print it instead of keeping it a private counter.
    pub fn dropped_cues(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            let b = self.dropped_breakdown();
            b.full.saturating_add(b.sealed).saturating_add(b.backoff)
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
                dropped_full: Arc::new(AtomicU64::new(0)),
                dropped_sealed: Arc::new(AtomicU64::new(0)),
                dropped_backoff: Arc::new(AtomicU64::new(0)),
                reopens_left: Arc::new(AtomicU8::new(REOPEN_BUDGET)),
                born_active: true,
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

    use super::mac::{
        BLOCK_S, Delivery, QueueCycle, Service, block_lead_s, callback_stalled, prime_and_start,
        stop_and_reclaim,
    };
    use super::{
        AudioWorkerOutput, COMMAND_CAPACITY, Cue, DETACH_DEADLINE, HostState, REOPEN_BUDGET,
        STATE_DORMANT, STATE_FAILED, STATE_PAUSED, STATE_REOPENING, STATE_RUNNING, STATE_STOPPED,
        TrailAudio, WEDGE_REVIVES, WorkerFlags, cue_channel, monotonic_ms, worker_loop,
    };

    /// D6 — THE STATE WORD REACHES THE WIRE. Until 2026-09-22 the one atomic
    /// that records whether `AudioQueueStart` actually succeeded had no reader
    /// outside `#[cfg(test)]`, so `audio=live` meant only "an ingress channel
    /// exists": a process that had never opened a device printed the same word
    /// as one serving callbacks.
    #[test]
    fn a_dormant_host_is_not_live_on_the_wire() {
        let (mut audio, _rx) = TrailAudio::test_ingress();
        // Ingress open, no device opened: `live` would be a lie.
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

        // A WEDGE OUTRANKS THE STATE WORD: a RUNNING worker stuck inside one
        // platform call is wedged, and the cues it drops are the fact that
        // matters.
        audio.state.store(STATE_RUNNING, Ordering::Release);
        audio.busy.store(1, Ordering::Release);
        assert_eq!(audio.host_state(), HostState::Wedged);

        // …and a SEALED ingress outranks everything: there is nothing left to
        // be running.
        audio.tx = None;
        assert_eq!(audio.host_state(), HostState::Inert);
        assert_eq!(wire(audio.host_state()), "inert");
    }

    /// Every state has its own wire word, so a seventh one a future worker
    /// adds cannot reach a status row as a silent fallback to `live`.
    /// The wire word a `HostState` reaches `tone` as — through the SAME
    /// conversion the row uses, so this test cannot pass over a `From` arm
    /// the row would get wrong.
    fn wire(s: HostState) -> &'static str {
        crate::tone_infer::AudioHost::from(s).label()
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
        // And `live` is reserved for the ONE state in which a platform call
        // really succeeded — the D6 lie was every other state borrowing it.
        assert_eq!(wire(HostState::Running), "live");
        for s in [HostState::Inert, HostState::Opening, HostState::Failed] {
            assert_ne!(wire(s), "live", "{s:?}");
        }
    }

    /// A revival allocates a fresh drop counter, which is exactly what makes
    /// `dropped=0` misleading — so the row prints the revivals beside it.
    #[test]
    fn revives_spent_counts_up_from_zero() {
        let (mut audio, _rx) = TrailAudio::test_ingress();
        assert_eq!(audio.revives_spent(), 0);
        audio.revives_left = WEDGE_REVIVES - 2;
        assert_eq!(audio.revives_spent(), 2);
        audio.revives_left = 0;
        assert_eq!(audio.revives_spent(), u64::from(WEDGE_REVIVES));
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
        /// One-shot: the next `push_meta` reports `Reopen` without consuming
        /// the cue — the deterministic stand-in for a callback that stopped
        /// arriving (D4) or a queue that faulted under us (D7).
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
            // The stamp lands BEFORE the count a test waits on.
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
        // `dropped_backoff`, `reopens_left` — the two counters the reopen
        // lane reports through.
        Arc<std::sync::atomic::AtomicU64>,
        Arc<AtomicU8>,
    );

    /// The shipping `REOPEN_BACKOFF` compressed to milliseconds, so the retry
    /// schedule is EXERCISED rather than slept through. Same length, so the
    /// budget under test is the shipping budget.
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
    fn bounded_ingress_drops_instead_of_blocking_when_full() {
        use std::sync::atomic::Ordering;

        let (mut audio, rx) = TrailAudio::test_ingress();
        for _ in 0..COMMAND_CAPACITY {
            audio.push(cue());
        }
        assert_eq!(audio.dropped_full.load(Ordering::Relaxed), 0);
        audio.push(cue());
        assert_eq!(
            audio.dropped_full.load(Ordering::Relaxed),
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

    /// THE LAW CHANGED, DELIBERATELY (D5, 2026-09-22). This test was
    /// `worker_device_failure_is_terminal_and_never_retried`, and it pinned
    /// exactly one open attempt followed by permanent silence for the process
    /// lifetime: a failed lazy device open, a failed `AudioQueueStart` or one
    /// failed enqueue stored `STATE_FAILED` and returned, dropping the
    /// receiver, so the next keystroke sealed ingress for good.
    ///
    /// That turned ONE transient CoreAudio hiccup — a device switch, a
    /// coreaudiod restart, a wake from sleep — into the user-visible
    /// complaint this whole campaign exists for. The half of the old law
    /// worth keeping is kept, and is what the second half of this test pins:
    /// exhaustion is still EXPLICIT, still terminal, still observable.
    #[test]
    fn worker_device_failure_is_retried_with_backoff_then_terminal_at_budget() {
        let shared = Arc::new(FakeShared::default());
        shared.fail_open.store(true, Ordering::Relaxed);
        let (tx, _shutdown, state, _busy, worker, dropped_backoff, reopens_left) =
            spawn_fake_worker(Arc::clone(&shared));

        // ONE fault is not terminal any more: the worker reports REOPENING and
        // ingress stays OPEN, which is the whole behaviour change.
        tx.send(cue().into()).unwrap();
        wait_until("first fault reopens", || {
            state.load(Ordering::Acquire) == STATE_REOPENING
        });
        assert!(
            tx.send(cue().into()).is_ok(),
            "ingress stays open while budget remains"
        );

        // Every cue past a backoff window spends one more attempt. The
        // compressed schedule totals 31 ms, so feeding cues steadily walks
        // the budget to its end.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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

        // RECOVERY WAS REAL: the device was reopened, not abandoned after one
        // try — and never more times than the budget allows.
        assert_eq!(
            shared.opens.load(Ordering::Relaxed),
            usize::from(REOPEN_BUDGET) + 1,
            "the first open plus exactly the budget's worth of REopens"
        );
        assert_eq!(shared.pushes.load(Ordering::Relaxed), 0);
        // EXHAUSTION IS STILL TERMINAL, and still observable.
        assert_eq!(reopens_left.load(Ordering::Acquire), 0);
        assert!(
            tx.send(cue().into()).is_err(),
            "an exhausted budget still seals ingress"
        );
        // …and the cues lost while the device was down are COUNTED (D8),
        // instead of vanishing with `dropped=0` on the row.
        assert!(
            dropped_backoff.load(Ordering::Relaxed) > 0,
            "cues lost to a down device must be counted"
        );
    }

    /// D7 — THE KEY DISCOVERS THE FAULT, AND IS STILL HEARD. Before this,
    /// the callback's `faulted` latch and a stalled callback were read ONLY
    /// by `on_tick`, which runs only after the worker's 250 ms receive
    /// timeout expires — i.e. only while nobody is typing. A queue that died
    /// mid-burst was invisible for as long as the burst lasted.
    #[test]
    fn a_stalled_device_is_discovered_by_the_next_key_and_that_key_is_still_heard() {
        let shared = Arc::new(FakeShared::default());
        let (tx, shutdown, state, _busy, worker, _dropped_backoff, reopens_left) =
            spawn_fake_worker(Arc::clone(&shared));

        // One healthy cue opens the device.
        tx.send(cue().into()).unwrap();
        wait_until("first open", || {
            shared.opens.load(Ordering::Relaxed) == 1 && shared.pushes.load(Ordering::Acquire) == 1
        });

        // The next push reports a stall. The worker must reopen AND re-push
        // the same cue into the fresh device, so the key that found the fault
        // still sounds.
        shared.stall_push_once.store(true, Ordering::Release);
        tx.send(cue().into()).unwrap();
        wait_until("reopen after stall", || {
            shared.opens.load(Ordering::Relaxed) == 2 && shared.pushes.load(Ordering::Acquire) == 3
        });
        wait_until("running again", || {
            state.load(Ordering::Acquire) == STATE_RUNNING
        });
        // A SUCCESSFUL reopen does NOT restore the budget on the spot. It is
        // spent, and it comes back only once the device has PLAYED for
        // `RetryState::healthy_for` past its first delivery — the compressed
        // schedule's 16 ms here. Resetting on the reopen itself is what made
        // the budget unspendable against a device that plays one block and
        // stalls again.
        assert_eq!(
            reopens_left.load(Ordering::Acquire),
            REOPEN_BUDGET - 1,
            "the reopen it just spent stays spent"
        );

        // …and past that window, the next delivered cue hands it back: the
        // bound is on CONSECUTIVE failure, not on a lifetime of healthy use.
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

    /// NEGATIVE CONTROL for the two above: a fake that never stalls reopens
    /// ZERO times, so neither test can pass by reopening unconditionally.
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

    /// THE BUDGET MUST BE SPENDABLE BY THE FAULT IT EXISTS FOR.
    ///
    /// A device that CONSTRUCTS and never PLAYS — `AudioQueueNewOutput`
    /// succeeds, `AudioQueueStart` or the enqueue does not; a present but
    /// unusable output, a coreaudiod in a bad state — is the shape that
    /// defeated the first draft of this ladder. It reset `attempts` on a
    /// successful `open()`, which happens BEFORE the push that fails, so
    /// every cue ran open → reset → push-fails → immediate reopen → reset,
    /// `attempts` never exceeded 1, `Reopen::Exhausted` was unreachable, and
    /// the worker ran a full dispose/open cycle per keystroke for the life of
    /// the process — while `tone` printed `reopens_left=6` throughout, the
    /// same structurally-unable-to-refute shape this whole lane exists to
    /// delete. The ladder now resets on a DELIVERY past a healthy window, so
    /// this terminates.
    #[test]
    fn a_device_that_opens_but_never_plays_spends_the_budget_and_stops() {
        let shared = Arc::new(FakeShared::default());
        // The open succeeds; every push reports Reopen. `fail_push` was
        // declared by the first draft and set by no test — which is exactly
        // why the defect shipped.
        shared.fail_push.store(true, Ordering::Relaxed);
        let (tx, _shutdown, state, _busy, worker, dropped_backoff, reopens_left) =
            spawn_fake_worker(Arc::clone(&shared));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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

        // BOUNDED, and bounded by the BUDGET rather than by how long the
        // person keeps typing.
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

    /// A SLOW DEVICE OPEN IS NOT A WEDGE.
    ///
    /// The wedge is now judged on EVERY push rather than only on a full
    /// ingress FIFO (D8) — which is right, because a wedged worker with a
    /// half-empty channel used to be invisible until 64 cues had piled up.
    /// But the worker also holds the busy stamp across `AudioQueueNewOutput`
    /// and its three `AudioQueueAllocateBuffer`s, and that section can
    /// honestly run for seconds on a Bluetooth output, a just-switched device,
    /// or a
    /// coreaudiod still coming back after wake. Judging it at
    /// [`WEDGE_AFTER_MS`] made one slow open cost a revival — and
    /// `revive_or_seal` ABANDONS the worker that was about to succeed, three
    /// of which seal audio for the process. So the phase rides in the stamp
    /// and an open is judged at [`OPEN_WEDGE_AFTER_MS`] instead.
    ///
    /// The two arms are one pair: the SAME stamp age is a wedge for a
    /// steady-state call and is not one for an open.
    #[test]
    fn a_slow_device_open_is_not_a_wedge() {
        use super::{BUSY_OPENING, OPEN_WEDGE_AFTER_MS, WEDGE_AFTER_MS, busy_stale};

        // THE VERDICT, exact on a fabricated clock. A stamp of `t0`, read at
        // `t0 + WEDGE_AFTER_MS`, is the SAME AGE in both arms — only the
        // operation differs.
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

        // …AND THE COST THE MISREADING CARRIED, on the live host: a push
        // while the worker is inside an open of wedge age must not abandon
        // it. `WEDGE_AFTER_MS` back from now is stale for a steady-state call
        // at any process age and short of the open threshold at every one,
        // which the pure arms above just proved.
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

    /// The stall VERDICT itself (D4), proved against a fake clock with no
    /// device — exactly as `block_lead_s` proves the pre-roll one.
    ///
    /// The two conjuncts are the point: a stopped queue and a never-armed one
    /// are NOT stalls. Reporting either would be a bound on our own patience
    /// returned as a fact about the device, which is the shape of every other
    /// finding in this audit.
    #[test]
    fn callback_stall_verdict_is_exact_on_a_fake_clock() {
        const MS: u64 = 1_000;
        let armed = 10 * MS;
        // Not running: a parked queue is not calling back and must not be.
        assert!(!callback_stalled(false, armed, armed + 10_000 * MS));
        // Never armed: a cold `MacOut` has not started, so it cannot have
        // stopped calling back.
        assert!(!callback_stalled(true, 0, 10_000 * MS));
        // One microsecond under the threshold, and exactly on it.
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
        // A clock that appears to run backwards saturates to zero elapsed,
        // never to a spurious stall.
        assert!(!callback_stalled(true, 10_000 * MS, 1));
    }

    /// The reopen schedule is CUE-DRIVEN and respects its window, so it can
    /// never become a busy loop nor re-arm the event loop the idle park
    /// deliberately disarmed.
    #[test]
    fn reopen_attempts_respect_their_backoff_window() {
        let mut output: Option<()> = Some(());
        let backoff = [
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(50),
        ];
        let mut retry = super::RetryState::new(&backoff);
        let t0 = std::time::Instant::now();

        // First fault: immediate, because the overwhelmingly likely cause is
        // transient and an immediate reopen makes the NEXT key sound.
        assert_eq!(
            super::reopen_after_fault(&mut output, &mut retry, t0),
            super::Reopen::Now
        );
        assert!(output.is_none(), "the device object is thrown away");
        assert!(retry.ready(t0));
        assert_eq!(retry.left(), 1);

        // Second fault: inside its window a cue may not spend an attempt.
        output = Some(());
        assert_eq!(
            super::reopen_after_fault(&mut output, &mut retry, t0),
            super::Reopen::Wait
        );
        assert!(!retry.ready(t0 + std::time::Duration::from_millis(49)));
        assert!(retry.ready(t0 + std::time::Duration::from_millis(50)));
        assert_eq!(retry.left(), 0);

        // And the budget is the only terminal state.
        assert_eq!(
            super::reopen_after_fault(&mut output, &mut retry, t0),
            super::Reopen::Exhausted
        );
    }

    /// D8 — a host born INERT must not manufacture drops. `new(false)` is the
    /// headless/test form whose ingress is `None` from birth; counting there
    /// would replace the old lie (`dropped=0` over real losses) with new
    /// noise (a large number in every headless run).
    #[test]
    fn an_inert_host_counts_no_sealed_drops() {
        let mut audio = TrailAudio::new(false);
        for _ in 0..100 {
            audio.push(cue());
        }
        assert_eq!(audio.dropped_cues(), 0);
        assert_eq!(audio.dropped_breakdown(), super::DroppedCues::default());
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

    // ── D4, SECOND DETECTOR: the `kAudioQueueProperty_IsRunning` listener ──

    use std::ffi::c_void;

    use super::mac::{
        DeviceHealth, OwnedQueue, PROP_IS_RUNNING, QueueOwnerCalls, RunningEdge, RunningProbe,
        running_listener, running_probe,
    };

    /// Fire the REAL listener body at `edge`, exactly as AudioToolbox would:
    /// the userdata is the edge's address and the id the one registered.
    fn fire(edge: &RunningEdge, id: u32) {
        running_listener(
            std::ptr::from_ref(edge).cast_mut().cast::<c_void>(),
            std::ptr::null_mut(),
            id,
        );
    }

    /// A read that must never happen: the healthy key path makes NO platform
    /// call, and neither does an edge the worker can prove is its own stop.
    fn no_read() -> Option<bool> {
        panic!("IsRunning was read on a path that must make no platform call")
    }

    /// The listener's whole body is one store, and it answers only for the
    /// property it was registered on — a stray id or a null userdata raises
    /// nothing and dereferences nothing.
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

    /// Every cell of the `IsRunning` verdict. The only claim it may make is a
    /// WITNESSED 1 → 0 within one start.
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

    /// THE POSITIVE CASE: a queue AudioToolbox stops under us is seen on the
    /// very next service call — push or tick, they ask the same verdict —
    /// rather than after the 750 ms callback watchdog.
    #[test]
    fn a_queue_stopped_under_us_is_seen_on_the_next_service_call() {
        let edge = RunningEdge::new();
        let mut health = DeviceHealth::new();
        let armed = 5_000;
        health.armed(armed);
        // The start's own notification: it reads running.
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(true)));
        // AudioToolbox stops it. The clock has NOT moved — the watchdog
        // could not have fired — and the verdict is already Reopen.
        fire(&edge, PROP_IS_RUNNING);
        assert!(
            health.needs_reopen(false, true, armed, armed, &edge, || Some(false)),
            "a witnessed 1 -> 0 the worker did not ask for is a reopen"
        );
    }

    /// THE NEGATIVE CONTROL: a healthy running queue is NEVER reopened by the
    /// listener, however often it fires, and the paths that must make no
    /// platform call make none.
    #[test]
    fn a_healthy_running_queue_is_never_reopened_by_the_listener() {
        let edge = RunningEdge::new();
        let mut health = DeviceHealth::new();
        let armed = 5_000;
        health.armed(armed);
        // No edge: the healthy per-key path reads nothing.
        for _ in 0..100 {
            assert!(!health.needs_reopen(false, true, armed, armed, &edge, no_read));
        }
        // Edges that read running — however many — are never a reopen.
        for i in 0..100 {
            fire(&edge, PROP_IS_RUNNING);
            assert!(!health.needs_reopen(false, true, armed + i, armed + i, &edge, || Some(true)));
        }
        // An edge while the WORKER says stopped is the worker's own stop:
        // consumed, no read, no reopen.
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, false, armed, armed, &edge, no_read));
        assert!(!edge.take(), "our own stop's edge is consumed, not carried");
        // A failed read claims nothing.
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed, armed, &edge, || None));
    }

    /// A start still taking reads stopped too. The verdict re-raises the edge
    /// and looks again next time instead of reopening a queue that is about
    /// to run — and a new start forgets what was seen of the last one.
    #[test]
    fn a_start_still_taking_is_looked_at_again_never_reopened() {
        let edge = RunningEdge::new();
        let mut health = DeviceHealth::new();
        let armed = 5_000;
        health.armed(armed);
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(false)));
        assert!(edge.take(), "NotYetRunning re-raises the edge");
        // …it reads running on the next look, and only THEN is a stop a stop.
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed, armed, &edge, || Some(true)));
        // A NEW start: what was seen of the previous run says nothing.
        health.armed(armed + 10);
        fire(&edge, PROP_IS_RUNNING);
        assert!(
            !health.needs_reopen(false, true, armed + 10, armed + 10, &edge, || Some(false)),
            "a fresh start's 0 is 'not yet', not 'stopped'"
        );
        // Callbacks arrived since this start, yet it never read running: the
        // reading contradicts what we saw, so the listener abstains and the
        // watchdog still stands behind it.
        let _ = edge.take();
        fire(&edge, PROP_IS_RUNNING);
        assert!(!health.needs_reopen(false, true, armed + 20, armed + 20, &edge, || Some(false)));
        assert!(!edge.take(), "an abstention does not re-raise");
    }

    /// ONE VERDICT, THREE DETECTORS: the callback's `faulted` latch and the
    /// 750 ms watchdog answer through the same function, ahead of the
    /// listener and without a platform read.
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

    /// REMOVAL BEFORE DISPOSE, DISPOSE BEFORE RELEASE, on every way a queue
    /// ends. A listener firing against a disposed queue whose `Shared` has
    /// been released is a use-after-free; the owner's `Drop` is the one
    /// teardown, so this is the order every path gets.
    #[test]
    fn the_queue_owner_removes_the_listener_before_dispose_on_every_path() {
        // Ordinary teardown (and the reopen after a fault, which drops the
        // whole output): listener registered.
        let (calls, log) = RecordingCalls::new(true);
        let mut owned = OwnedQueue::new(calls);
        assert!(owned.listen());
        drop(owned);
        assert_eq!(
            *log.lock().unwrap(),
            ["add", "remove", "dispose", "release"]
        );

        // AudioToolbox refused the listener: nothing to remove, and nothing
        // is removed that was never added.
        let (calls, log) = RecordingCalls::new(false);
        let mut owned = OwnedQueue::new(calls);
        assert!(!owned.listen());
        drop(owned);
        assert_eq!(*log.lock().unwrap(), ["add", "dispose", "release"]);

        // `MacOut::new`'s buffer-allocation unwind, in its real shape: the
        // owner exists, the listener is on, and an early `return None` is
        // the whole unwind.
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

        // A queue whose worker is ABANDONED inside a platform call is never
        // dropped: nothing is removed, disposed OR released, so the listener
        // keeps firing into a live allocation (it leaks with the thread).
        let (calls, log) = RecordingCalls::new(true);
        let mut owned = OwnedQueue::new(calls);
        owned.listen();
        std::mem::forget(owned);
        assert_eq!(*log.lock().unwrap(), ["add"]);
    }

    /// The "only place" half, lexically: the file contains exactly ONE
    /// `AudioQueueDispose` call, and it is `AudioToolboxQueue::dispose`,
    /// which only `OwnedQueue`'s `Drop` calls. A new dispose site spelled
    /// anywhere else fails here before it can forget the listener.
    #[test]
    fn the_queue_is_disposed_in_exactly_one_place() {
        let source = include_str!("trail_audio.rs");
        let code_lines = || {
            source
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim_start().starts_with("//"))
        };
        let calls_of = |needle: &str, decl: &str| -> Vec<usize> {
            code_lines()
                .filter(|(_, line)| line.contains(needle) && !line.contains(decl))
                .map(|(n, _)| n)
                .collect()
        };
        let lines: Vec<&str> = source.lines().collect();
        let enclosing_fn = |at: usize| -> &str {
            lines[..at]
                .iter()
                .rev()
                .find(|line| line.trim_start().starts_with("fn "))
                .map_or("", |line| line.trim())
        };

        let dispose = calls_of(
            concat!("AudioQueue", "Dispose("),
            concat!("fn AudioQueue", "Dispose("),
        );
        assert_eq!(dispose.len(), 1, "dispose call sites: {dispose:?}");
        assert!(
            enclosing_fn(dispose[0]).starts_with("fn dispose(&mut self)"),
            "the one dispose lives in AudioToolboxQueue::dispose, found in {:?}",
            enclosing_fn(dispose[0])
        );
        let remove = calls_of(
            concat!("AudioQueue", "RemovePropertyListener("),
            concat!("fn AudioQueue", "RemovePropertyListener("),
        );
        assert_eq!(remove.len(), 1, "listener removal sites: {remove:?}");
        assert!(enclosing_fn(remove[0]).starts_with("fn remove_running_listener(&mut self)"));
        // …and `.dispose()` is reached only from the owner's `Drop`.
        let owner_calls = calls_of(concat!(".dispose", "()"), concat!("fn ", "dispose("));
        assert_eq!(owner_calls.len(), 1, "dispose() callers: {owner_calls:?}");
        assert!(enclosing_fn(owner_calls[0]).starts_with("fn drop(&mut self)"));
    }

    /// A device double for the WORKER-level proof: the real `DeviceHealth`
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
        /// The edge of the CURRENT device — each open gets a fresh one, as
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
            // The clock never moves past the arm, so the watchdog cannot
            // fire: whatever reopens here, the listener found.
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

    /// THE KEY PATH, end to end through the shipping ladder: a healthy queue
    /// whose listener fires is not reopened (negative control); one stopped
    /// under us is reopened by the very next key — and that key is heard.
    #[test]
    fn the_next_key_after_a_stop_under_us_reopens_and_is_heard() {
        let dev = Arc::new(ListenerDevice::default());
        // Housekeeping far beyond the test, so ONLY pushes consume the edge.
        let (tx, shutdown, state, _busy, worker, _dropped, reopens_left) =
            spawn_listener_worker(Arc::clone(&dev), std::time::Duration::from_secs(60));

        tx.send(cue().into()).unwrap();
        wait_until("first open + push", || {
            dev.pushes.load(Ordering::Acquire) == 1
        });

        // NEGATIVE CONTROL: the listener fires and the queue reads running.
        for n in 2..=4 {
            dev.stop_under_us(true);
            tx.send(cue().into()).unwrap();
            wait_until("healthy push", || dev.pushes.load(Ordering::Acquire) == n);
        }
        // No edge: the healthy key reads nothing.
        tx.send(cue().into()).unwrap();
        wait_until("plain push", || dev.pushes.load(Ordering::Acquire) == 5);
        assert_eq!(dev.reads.load(Ordering::Acquire), 3, "one read per edge");
        assert_eq!(dev.opens.load(Ordering::Acquire), 1, "never reopened");
        assert_eq!(dev.drops.load(Ordering::Acquire), 0);
        assert_eq!(reopens_left.load(Ordering::Acquire), REOPEN_BUDGET);

        // AudioToolbox stops it under us.
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

    /// THE TICK PATH: with nobody typing, a stop under us is seen by the next
    /// housekeeping tick and the host reads `reopening` — never `live` over a
    /// dead queue — while ticks over a healthy queue reopen nothing.
    #[test]
    fn the_next_tick_after_a_stop_under_us_reads_reopening() {
        let dev = Arc::new(ListenerDevice::default());
        let (tx, shutdown, state, _busy, worker, _dropped, reopens_left) =
            spawn_listener_worker(Arc::clone(&dev), std::time::Duration::from_millis(2));

        tx.send(cue().into()).unwrap();
        wait_until("first push", || dev.pushes.load(Ordering::Acquire) == 1);

        // NEGATIVE CONTROL on the tick path.
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
        assert_eq!(
            crate::tone_infer::AudioHost::from(HostState::Reopening).label(),
            "reopening"
        );

        // Reopens are cue-driven: the next key opens a fresh device.
        tx.send(cue().into()).unwrap();
        wait_until("reopened on the next key", || {
            dev.opens.load(Ordering::Acquire) == 2 && dev.pushes.load(Ordering::Acquire) == 2
        });

        shutdown.store(true, Ordering::Release);
        drop(tx);
        worker.join().unwrap();
    }
}
