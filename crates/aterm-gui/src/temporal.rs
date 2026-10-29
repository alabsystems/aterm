// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session temporal recorder.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Instant;

use aterm_aos::Digest;
use aterm_aos_pane::trace::PaneRecording;
use aterm_aos_pane::{PaneClock, PaneEnv, PaneState, screen_digest};
use aterm_buffer::{BlobId, EventLog, HostOpId, KeyframeId, Op, Seq, Ticks, WitnessId};
use aterm_control::replay::ReplayEngine;
use aterm_core::config::TerminalConfig;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{
    ClockReading, HostOp, ReplayCarry, ShellIntegrationNonce, Terminal, TerminalCheckpoint,
};

/// Default byte budget for retained payloads: input blobs plus keyframes.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Size of one packed history byte slab: 256 KiB sits in macOS `libsystem_malloc`'s Large VM
/// region, so evicting a packed chain unmaps whole pages instead of stranding freed slots in
/// Small/Tiny magazine pages.
const BLOB_SLAB_BYTES: usize = 256 * 1024;

/// Freed history bytes accumulated before an idle writer relieves the allocator.
const IDLE_RELIEF_FLOOR_BYTES: usize = 1024 * 1024;

/// Freed history bytes accumulated during multi-pane pool shedding before mid-flood relief.
const POOL_RELIEF_MID_FLOOD_BYTES: usize = 16 * 1024 * 1024;

/// An in-memory replay keyframe: its [`TerminalCheckpoint`] paired with the transient engine state no checkpoint carries ([`ReplayCarry`]).
#[derive(Clone, Debug)]
pub(crate) struct ReplayKeyframe {
    pub(crate) checkpoint: Arc<TerminalCheckpoint>,
    pub(crate) carry: Arc<ReplayCarry>,
    pub(crate) recorded_config: Option<Option<Arc<TerminalConfig>>>,
}

impl std::ops::Deref for ReplayKeyframe {
    type Target = TerminalCheckpoint;

    fn deref(&self) -> &Self::Target {
        &self.checkpoint
    }
}

impl From<TerminalCheckpoint> for ReplayKeyframe {
    fn from(checkpoint: TerminalCheckpoint) -> Self {
        Self::from(Arc::new(checkpoint))
    }
}

impl From<Arc<TerminalCheckpoint>> for ReplayKeyframe {
    fn from(checkpoint: Arc<TerminalCheckpoint>) -> Self {
        Self {
            checkpoint,
            carry: Arc::new(ReplayCarry::default()),
            recorded_config: None,
        }
    }
}

/// A burst handed from the reader hot path to the temporal writer thread (lock-free, FIFO, bounded: [`queue`]).
pub(crate) enum TemporalMsg {
    RawIn(InputSlice),
    Reply,
    Resize {
        rows: u16,
        cols: u16,
    },
    CellPx {
        width: u16,
        height: u16,
    },
    Host(HostOp, Option<ClockReading>),
    ResizesLost(u64),
    Keyframe(ReplayKeyframe),
    /// A live keyframe a diverged witness asked for ([`SpineWatermark::keyframe_wanted`]), with the marks of the slice it stands in for; it closes no break.
    LiveKeyframe(ReplayKeyframe, SliceMarks),
    /// The live screen right after the slice queued before it, for a replay to match ([`Fidelity`]).
    Witness(Arc<TerminalCheckpoint>),
}

/// The bytes of one `RawIn`, the clock reading the live engine parsed them at, what the live screen showed after them, and what the live engine answered them with.
#[derive(Clone)]
pub(crate) struct InputSlice {
    batch: Arc<[u8]>,
    range: Range<usize>,
    clock: ClockReading,
    marks: SliceMarks,
    reply: Option<Arc<[u8]>>,
}

/// What the live engine showed right after one slice: a mode-2026 frame still open, and a command the slice started (an accepted OSC 133/633 `C`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SliceMarks {
    pub(crate) sync_open: bool,
    pub(crate) command: bool,
}

impl InputSlice {
    /// `batch[range]`, sharing `batch`, parsed live at `clock`.
    pub(crate) fn new(batch: Arc<[u8]>, range: Range<usize>, clock: ClockReading) -> Self {
        assert!(
            range.start <= range.end && range.end <= batch.len(),
            "input slice {range:?} outside its {}-byte batch",
            batch.len()
        );
        Self {
            batch,
            range,
            clock,
            marks: SliceMarks::default(),
            reply: None,
        }
    }

    /// This slice with `reply`, what the live engine answered it with: its `take_response()` right after it, reports last ([`TemporalRecorder::pane_chains`] matches a replay's against it).
    #[must_use]
    pub(crate) fn replying(mut self, reply: Option<Arc<[u8]>>) -> Self {
        self.reply = reply.filter(|r| !r.is_empty());
        self
    }

    /// A copy of `bytes`, parsed live at `clock`.
    pub(crate) fn at(bytes: &[u8], clock: ClockReading) -> Self {
        let batch = Arc::<[u8]>::from(bytes);
        Self::new(batch, 0..bytes.len(), clock)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.batch[self.range.clone()]
    }

    /// What the live screen showed after this slice ([`SpineMark::record`]).
    pub(crate) const fn marks(&self) -> SliceMarks {
        self.marks
    }

    fn len(&self) -> usize {
        self.range.len()
    }

    /// The bytes it holds against the budget: the slice and its reply.
    fn charge(&self) -> usize {
        self.len() + self.reply.as_ref().map_or(0, |r| r.len())
    }
}

/// What the live engine answered one slice with, as one pane step's replies run (`take_response()`: responses, then reports): `response`, then `reports`, or `None` for neither.
#[must_use]
pub(crate) fn reply_of(response: Option<&[u8]>, reports: Option<&[u8]>) -> Option<Arc<[u8]>> {
    match (response, reports) {
        (None, None) => None,
        (Some(one), None) | (None, Some(one)) => Some(Arc::from(one)),
        (Some(a), Some(b)) => Some([a, b].concat().into()),
    }
}

/// A replay keyframe of `term` with each grid's history bounded to `max_history` lines, carrying the shell-integration nonce and revision so a replay verifies shell marks as the live engine does, plus the in-process [`ReplayCarry`] no checkpoint holds (it stays in memory).
#[must_use]
pub(crate) fn keyframe_of(term: &Terminal, max_history: usize) -> Option<ReplayKeyframe> {
    let mut checkpoint = term.checkpoint_continuing_bounded(max_history)?;
    term.note_bounded_replay_keyframe(max_history);
    if checkpoint.grid.capacity() > checkpoint.grid.len() + 4096 {
        checkpoint.grid.shrink_to_fit();
    }
    if let Some(alt) = &mut checkpoint.alt_grid
        && alt.capacity() > alt.len() + 4096
    {
        alt.shrink_to_fit();
    }
    checkpoint.shell_integration_nonce = term
        .shell_integration_nonce_in_use()
        .map(ShellIntegrationNonce);
    checkpoint.shell_integration_rev = term.shell_integration_rev().map(str::to_owned);
    Some(ReplayKeyframe {
        checkpoint: Arc::new(checkpoint),
        carry: Arc::new(term.capture_replay_carry()),
        recorded_config: None,
    })
}

/// A message on its way to a recorder, with the recording generation it was sent in ([`TemporalRecorder::forget`]) and the bytes it holds ([`TemporalMsg::weight`]).
struct Queued {
    to: Option<Arc<Mutex<TemporalRecorder>>>,
    state: Arc<QueueState>,
    generation: u64,
    weight: usize,
    msg: TemporalMsg,
}

/// How many messages a reader may have queued that the writer has not written yet.
pub(crate) const QUEUE_CAP: usize = 1024;

/// The fewest bytes a queue may hold unwritten, whatever its recorder's budget.
const MIN_QUEUE_BYTES: usize = 1024 * 1024;

/// The share of its pool's limit every queue together may hold unwritten (a quarter).
const POOL_QUEUE_SHARE: usize = 4;

impl TemporalMsg {
    /// The bytes this message holds while queued: a slice and its reply, a checkpoint's grids.
    fn weight(&self) -> usize {
        match self {
            Self::RawIn(input) => input.charge(),
            Self::Keyframe(k) | Self::LiveKeyframe(k, _) => {
                charge_of(&k.checkpoint).saturating_add(k.carry.retained_image_bytes())
            }
            Self::Witness(c) => charge_of(c),
            Self::Reply
            | Self::Resize { .. }
            | Self::CellPx { .. }
            | Self::Host(..)
            | Self::ResizesLost(_) => 0,
        }
    }

    const fn is_keyframe(&self) -> bool {
        matches!(self, Self::Keyframe(_) | Self::LiveKeyframe(..))
    }
}

/// Open `recorder`'s queue onto the one writer thread every recording shares, holding at most `cap` messages and the recorder's budget in bytes unwritten, and charging them to [`pool`].
/// # Errors
/// The writer thread could not be started.
pub(crate) fn open(
    recorder: &Arc<Mutex<TemporalRecorder>>,
    cap: usize,
) -> Result<TemporalTx, String> {
    let sender = writer()?.clone();
    let (spine, budget) = {
        let rec = recorder.lock().unwrap_or_else(PoisonError::into_inner);
        (rec.spine_watermark(), rec.budget)
    };
    Ok(TemporalTx {
        sender,
        to: Some(recorder.clone()),
        state: Arc::new(QueueState::on(pool().clone())),
        spine,
        cap,
        byte_cap: budget.max(MIN_QUEUE_BYTES),
    })
}

/// Set once the shared writer thread has ended, so no queue takes another message.
static WRITER_GONE: AtomicBool = AtomicBool::new(false);

struct WriterGone;

impl Drop for WriterGone {
    fn drop(&mut self) {
        WRITER_GONE.store(true, Ordering::Release);
    }
}

/// The shared writer thread's end of every queue, the thread started on first use.
fn writer() -> Result<&'static Sender<Queued>, String> {
    static WRITER: OnceLock<Result<Sender<Queued>, String>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Queued>();
            crate::qos::spawn_lane_thread(
                "aterm-temporal-writer",
                crate::qos::Role::Responsive,
                move || {
                    let _gone = WriterGone;
                    let mut next = rx.recv().ok();
                    while let Some(first) = next {
                        let mut cur = Some(first);
                        while let Some(queued) = cur {
                            if let Some(to) = &queued.to {
                                write_queued(to, queued.generation, queued.msg);
                            }
                            queued.state.written(queued.weight);
                            queued.state.pool.relieve();
                            cur = rx.try_recv().ok();
                        }
                        if pool().relief_due() {
                            match rx.recv_timeout(std::time::Duration::from_millis(20)) {
                                Ok(more) => {
                                    next = Some(more);
                                    continue;
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                    pool().relieve_allocator_if_due();
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        next = rx.recv().ok();
                    }
                },
            )
            .map(|_| tx)
            .map_err(|error| format!("spawn temporal writer: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// A queue whose messages a test takes from the returned end instead of the shared writer.
#[cfg(test)]
pub(crate) fn queue(cap: usize) -> (TemporalTx, TemporalRx) {
    queue_on(Arc::default(), cap)
}

/// [`queue`], stamping each message with `spine`'s generation.
#[cfg(test)]
pub(crate) fn queue_on(spine: Arc<SpineWatermark>, cap: usize) -> (TemporalTx, TemporalRx) {
    queue_in(spine, cap, usize::MAX, RewindPool::new(usize::MAX))
}

/// [`queue_on`] holding at most `byte_cap` bytes unwritten, charged to `pool`.
#[cfg(test)]
fn queue_in(
    spine: Arc<SpineWatermark>,
    cap: usize,
    byte_cap: usize,
    pool: Arc<RewindPool>,
) -> (TemporalTx, TemporalRx) {
    let (sender, rx) = std::sync::mpsc::channel();
    let state = Arc::new(QueueState::on(pool));
    (
        TemporalTx {
            sender,
            to: None,
            state: state.clone(),
            spine,
            cap,
            byte_cap,
        },
        TemporalRx { rx, state },
    )
}

/// What one queue holds unwritten, and the pool it charges.
struct QueueState {
    in_flight: AtomicUsize,
    bytes: AtomicUsize,
    closed: AtomicBool,
    pool: Arc<RewindPool>,
}

impl QueueState {
    fn on(pool: Arc<RewindPool>) -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            pool,
        }
    }

    fn queue(&self, weight: usize) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.bytes.fetch_add(weight, Ordering::AcqRel);
        self.pool.queued.fetch_add(weight, Ordering::AcqRel);
    }

    /// Uncount one message of `weight` bytes the writer has written (or that was never sent).
    fn written(&self, weight: usize) {
        self.pool.queued.fetch_sub(weight, Ordering::AcqRel);
        self.bytes.fetch_sub(weight, Ordering::AcqRel);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A reader's end of its recording's queue.
#[derive(Clone)]
pub(crate) struct TemporalTx {
    sender: Sender<Queued>,
    to: Option<Arc<Mutex<TemporalRecorder>>>,
    state: Arc<QueueState>,
    spine: Arc<SpineWatermark>,
    cap: usize,
    byte_cap: usize,
}

impl TemporalTx {
    /// Queue `msg`, unless the queue holds `cap` messages, or a message other than a keyframe would take it past its bytes or every queue past the pool's share.
    fn send(&self, msg: TemporalMsg) -> bool {
        let weight = msg.weight();
        if !self.admits(weight, msg.is_keyframe()) {
            return false;
        }
        self.state.queue(weight);
        let queued = Queued {
            to: self.to.clone(),
            state: self.state.clone(),
            generation: self.spine.generation(),
            weight,
            msg,
        };
        if self.sender.send(queued).is_ok() {
            return true;
        }
        self.state.written(weight);
        false
    }

    fn admits(&self, weight: usize, keyframe: bool) -> bool {
        if WRITER_GONE.load(Ordering::Acquire)
            || self.state.closed.load(Ordering::Acquire)
            || self.state.in_flight.load(Ordering::Acquire) >= self.cap
        {
            return false;
        }
        keyframe
            || (self
                .state
                .bytes
                .load(Ordering::Acquire)
                .saturating_add(weight)
                <= self.byte_cap
                && self.state.pool.queued().saturating_add(weight) <= self.pool_share())
    }

    fn pool_share(&self) -> usize {
        (self.state.pool.limit() / POOL_QUEUE_SHARE).max(MIN_QUEUE_BYTES)
    }

    /// Whether a keyframe may be queued now: a free place, and this queue and every queue at most half their bytes, so a recording that broke for want of room keys again only once its writer has caught up.
    fn has_room(&self) -> bool {
        self.admits(0, true)
            && self.state.bytes.load(Ordering::Acquire) <= self.byte_cap / 2
            && self.state.pool.queued() <= self.pool_share() / 2
    }

    /// The recording's spine watermark, where a reader that starts on this queue attaches ([`SpineMark::attach`]).
    pub(crate) fn spine(&self) -> Arc<SpineWatermark> {
        self.spine.clone()
    }

    /// What tells whether every message queued here has been written.
    pub(crate) fn drain(&self) -> TemporalDrain {
        TemporalDrain(self.state.clone())
    }
}

/// Whether every message a reader queued has been written ([`TemporalTx::drain`]).
#[derive(Clone)]
pub(crate) struct TemporalDrain(Arc<QueueState>);

impl TemporalDrain {
    /// Whether nothing queued is still unwritten.
    pub(crate) fn is_finished(&self) -> bool {
        self.0.in_flight.load(Ordering::Acquire) == 0
    }

    /// Wait until every queued message is written, or the writer is gone.
    pub(crate) fn wait(&self) {
        while !self.is_finished() && !WRITER_GONE.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }

    /// A drain held unfinished until the returned guard drops, as a writer still busy holds one.
    #[cfg(test)]
    pub(crate) fn held_for_test() -> (Self, HeldDrain) {
        let state = Arc::new(QueueState::on(RewindPool::new(usize::MAX)));
        state.in_flight.store(1, Ordering::Release);
        (Self(state.clone()), HeldDrain(state))
    }
}

/// Finishes a [`TemporalDrain::held_for_test`] when dropped.
#[cfg(test)]
pub(crate) struct HeldDrain(Arc<QueueState>);

#[cfg(test)]
impl Drop for HeldDrain {
    fn drop(&mut self) {
        self.0.in_flight.store(0, Ordering::Release);
    }
}

/// A test's end of a [`queue`].
#[cfg(test)]
pub(crate) struct TemporalRx {
    rx: Receiver<Queued>,
    state: Arc<QueueState>,
}

#[cfg(test)]
impl TemporalRx {
    /// The next message if one is queued.
    pub(crate) fn try_recv(&self) -> Option<TemporalMsg> {
        self.try_recv_stamped().map(|(_, msg)| msg)
    }

    /// The next message and the generation it was sent in.
    pub(crate) fn try_recv_stamped(&self) -> Option<(u64, TemporalMsg)> {
        let queued = self.rx.try_recv().ok()?;
        self.state.written(queued.weight);
        Some((queued.generation, queued.msg))
    }
}

#[cfg(test)]
impl Drop for TemporalRx {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::Release);
    }
}

/// Pick up a recording armed since the reader started ([`Arm`]); the caller holds the terminal's lock, so the recording's first slice is the next one parsed.
pub(crate) fn pick_up(arm: &Arm, tx: &mut Option<TemporalTx>, mark: &mut SpineMark) {
    if tx.is_some() {
        return;
    }
    if let Some(armed) = arm.take() {
        *mark = SpineMark::attach(armed.spine());
        *tx = Some(armed);
    }
}

/// A recording a running reader picks up at its next slice, armed when a spare is promoted into a tab ([`crate::spare`]).
#[derive(Default)]
pub(crate) struct Arm {
    ready: AtomicBool,
    tx: Mutex<Option<TemporalTx>>,
}

impl Arm {
    /// Hand `tx` to the reader.
    pub(crate) fn set(&self, tx: TemporalTx) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
        self.ready.store(true, Ordering::Release);
    }

    /// The armed queue, once.
    pub(crate) fn take(&self) -> Option<TemporalTx> {
        if !self.ready.load(Ordering::Acquire) || !self.ready.swap(false, Ordering::AcqRel) {
            return None;
        }
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Where the spine's resizes are up to, shared by every reader of a session.
#[derive(Debug, Default)]
pub(crate) struct SpineWatermark {
    lifetime: AtomicU64,
    ordinal: AtomicU64,
    cell_px: AtomicU64,
    host_lifetime: AtomicU64,
    host_ordinal: AtomicU64,
    deep_pullback: AtomicU64,
    dropped_input: AtomicU64,
    closed: AtomicU64,
    dropped_replies: AtomicU64,
    keyframes_queued: AtomicU64,
    keyframe_wanted: AtomicBool,
    generation: AtomicU64,
    replay_config: Mutex<Option<Arc<TerminalConfig>>>,
}

impl SpineWatermark {
    /// Start the watermark at `pre`.
    pub(crate) fn seed(&self, pre: SpinePre) {
        self.store_host((pre.lifetime, pre.host_ordinal));
        self.store((pre.lifetime, pre.ordinal));
        self.store_cell_px(pre.cell_px);
        self.store_deep_pullback(pre.deep_pullback);
    }

    fn load_deep_pullback(&self) -> u64 {
        self.deep_pullback.load(Ordering::Acquire)
    }

    fn store_deep_pullback(&self, ordinal: u64) {
        self.deep_pullback.store(ordinal, Ordering::Release);
    }

    fn load(&self) -> Option<(u64, u64)> {
        let lifetime = self.lifetime.load(Ordering::Acquire);
        (lifetime != 0).then(|| (lifetime, self.ordinal.load(Ordering::Acquire)))
    }

    fn store(&self, (lifetime, ordinal): (u64, u64)) {
        self.ordinal.store(ordinal, Ordering::Release);
        self.lifetime.store(lifetime, Ordering::Release);
    }

    fn load_host(&self) -> Option<(u64, u64)> {
        let lifetime = self.host_lifetime.load(Ordering::Acquire);
        (lifetime != 0).then(|| (lifetime, self.host_ordinal.load(Ordering::Acquire)))
    }

    fn store_host(&self, (lifetime, ordinal): (u64, u64)) {
        self.host_ordinal.store(ordinal, Ordering::Release);
        self.host_lifetime.store(lifetime, Ordering::Release);
    }

    fn load_cell_px(&self) -> Option<(u16, u16)> {
        let packed = self.cell_px.load(Ordering::Acquire);
        ((packed & (1 << 32)) != 0).then_some(((packed >> 16) as u16, packed as u16))
    }

    fn store_cell_px(&self, cell_px: Option<(u16, u16)>) {
        let packed = match cell_px {
            Some((w, h)) => (1 << 32) | (u64::from(w) << 16) | u64::from(h),
            None => 0,
        };
        self.cell_px.store(packed, Ordering::Release);
    }

    fn set_replay_config(&self, config: Option<Arc<TerminalConfig>>) {
        *self
            .replay_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = config;
    }

    fn replay_config(&self) -> Option<Arc<TerminalConfig>> {
        self.replay_config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Resizes `term` (the caller holds its lock) applied that no reader has handed the spine yet.
    #[must_use]
    pub(crate) fn pending(&self, term: &Terminal) -> u64 {
        match self.load() {
            Some((lifetime, ordinal)) if lifetime == term.resize_lifetime() => {
                term.resize_ordinal().saturating_sub(ordinal)
            }
            _ => term.resize_ordinal(),
        }
    }

    /// Input slices the readers could not queue, each the start of a break.
    #[must_use]
    pub(crate) fn dropped_input(&self) -> u64 {
        self.dropped_input.load(Ordering::Acquire)
    }

    /// Reply instants the readers could not queue.
    #[must_use]
    pub(crate) fn dropped_replies(&self) -> u64 {
        self.dropped_replies.load(Ordering::Relaxed)
    }

    /// Whether input was dropped that the recorder holds no keyframe after yet.
    #[must_use]
    pub(crate) fn break_open(&self) -> bool {
        self.closed.load(Ordering::Acquire) < self.dropped_input()
    }

    /// Whether a diverged witness asked for a live keyframe that no reader has queued yet.
    #[must_use]
    pub(crate) fn keyframe_wanted(&self) -> bool {
        self.keyframe_wanted.load(Ordering::Acquire)
    }

    /// How many times the recording has been forgotten ([`TemporalRecorder::forget`]).
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// One reader's resize watermark.
#[derive(Debug, Default)]
pub(crate) struct SpineMark {
    at: Option<(u64, u64)>,
    cell_px: Option<(u16, u16)>,
    host_at: Option<(u64, u64)>,
    deep_pullback_at: u64,
    shared: Arc<SpineWatermark>,
    broken: bool,
    since_witness: usize,
    witnessed_at: Option<aterm_time::Instant>,
    generation: u64,
}

/// What one reader hold read before processing its bytes: the engine's resize lifetime, ordinal and geometry, and its count of started commands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpinePre {
    lifetime: u64,
    ordinal: u64,
    rows: u16,
    cols: u16,
    started: u64,
    cell_px: Option<(u16, u16)>,
    host_ordinal: u64,
    host_io: u64,
    deep_pullback: u64,
}

impl SpinePre {
    /// Read `term` (the caller holds its lock) before processing a slice.
    pub(crate) fn take(term: &Terminal) -> Self {
        Self {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
            rows: term.rows(),
            cols: term.cols(),
            started: term.started_command_seq(),
            cell_px: term.host_cell_pixel_size(),
            host_ordinal: term.host_ordinal(),
            host_io: term.host_io_ordinal(),
            deep_pullback: term.deep_history_pullback_ordinal(),
        }
    }
}

/// One resize a [`SpineMark`] hands its sink, in the engine's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpineResize {
    /// The geometry after it, `(rows, cols)`.
    pub(crate) to: (u16, u16),
    /// The geometry before it, from the engine's resize journal.
    pub(crate) from: Option<(u16, u16)>,
    /// The engine lifetime and resize ordinal it carries.
    pub(crate) lifetime: u64,
    pub(crate) ordinal: u64,
}

/// WHERE A READER'S SPINE GOES (crash restore, increment 4).
pub(crate) trait SpineSink {
    fn resize(&mut self, resize: SpineResize) -> bool;
    fn resizes_lost(&mut self, n: u64) -> bool;
    #[cfg_attr(not(unix), allow(dead_code))]
    fn bytes(&mut self, bytes: &[u8]) -> bool;
}

struct TemporalSink<'a>(&'a TemporalTx);

impl SpineSink for TemporalSink<'_> {
    fn resize(&mut self, resize: SpineResize) -> bool {
        self.0.send(TemporalMsg::Resize {
            rows: resize.to.0,
            cols: resize.to.1,
        })
    }

    fn resizes_lost(&mut self, n: u64) -> bool {
        self.0.send(TemporalMsg::ResizesLost(n))
    }

    /// Refused: the writer queue takes bytes only with the clock they were parsed at ([`SpineMark::record`]).
    fn bytes(&mut self, _bytes: &[u8]) -> bool {
        false
    }
}

enum CatchUpHost {
    Ok,
    Overrun,
    QueueFull,
}

impl SpineMark {
    /// A reader's mark, starting where the session's previous reader left the spine (or where its keyframe seeded it), a break it left open included.
    pub(crate) fn attach(shared: Arc<SpineWatermark>) -> Self {
        Self {
            at: shared.load(),
            cell_px: shared.load_cell_px(),
            host_at: shared.load_host(),
            deep_pullback_at: shared.load_deep_pullback(),
            broken: shared.break_open(),
            generation: shared.generation(),
            shared,
            since_witness: 0,
            witnessed_at: None,
        }
    }

    /// Put the watermark at `pre`.
    #[cfg(unix)]
    pub(crate) fn seed(&mut self, pre: SpinePre) {
        self.advance_host((pre.lifetime, pre.host_ordinal));
        self.advance((pre.lifetime, pre.ordinal));
        self.advance_cell_px(pre.cell_px);
        self.advance_deep_pullback(pre.deep_pullback);
    }

    fn advance_deep_pullback(&mut self, ordinal: u64) {
        if self.deep_pullback_at != ordinal {
            self.deep_pullback_at = ordinal;
            self.shared.store_deep_pullback(ordinal);
        }
    }

    fn advance(&mut self, at: (u64, u64)) {
        if self.at != Some(at) {
            self.at = Some(at);
            self.shared.store(at);
        }
    }

    fn advance_host(&mut self, at: (u64, u64)) {
        if self.host_at != Some(at) {
            self.host_at = Some(at);
            self.shared.store_host(at);
        }
    }

    fn advance_cell_px(&mut self, cell_px: Option<(u16, u16)>) {
        if self.cell_px != cell_px {
            self.cell_px = cell_px;
            self.shared.store_cell_px(cell_px);
        }
    }

    fn catch_up_cell_px(&mut self, tx: &TemporalTx, cell_px: Option<(u16, u16)>) -> bool {
        if self.cell_px == cell_px {
            return true;
        }
        let (width, height) = cell_px.unwrap_or((0, 0));
        if !tx.send(TemporalMsg::CellPx { width, height }) {
            return false;
        }
        self.advance_cell_px(cell_px);
        true
    }

    fn catch_up_host(&mut self, tx: &TemporalTx, term: &Terminal) -> CatchUpHost {
        if let Some(shared_host) = self.shared.load_host()
            && self.host_at.is_none_or(|cur| shared_host > cur)
        {
            self.host_at = Some(shared_host);
        }
        let lifetime = term.resize_lifetime();
        let target = term.host_ordinal();
        let since = match self.host_at {
            Some((l, ord)) if l == lifetime && ord == target => return CatchUpHost::Ok,
            Some((l, ord)) if l == lifetime && ord < target => ord,
            _ if target == 0 => {
                self.advance_host((lifetime, 0));
                return CatchUpHost::Ok;
            }
            _ => 0,
        };
        let (reports, lost) = term.host_journal_since(since);
        if lost > 0 {
            return CatchUpHost::Overrun;
        }
        for report in reports {
            if !tx.send(TemporalMsg::Host(report.op, report.clock)) {
                return CatchUpHost::QueueFull;
            }
            self.advance_host((lifetime, report.ordinal));
        }
        CatchUpHost::Ok
    }

    /// Flush any host-side mutations recorded on `term` (such as a retired drawer mark) onto `tx` when no output slice accompanies them.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn flush_host(&mut self, tx: &TemporalTx, term: &Terminal) {
        let generation = self.shared.generation();
        if generation != self.generation {
            self.generation = generation;
            self.at = self.shared.load();
            self.cell_px = self.shared.load_cell_px();
            self.host_at = self.shared.load_host();
            self.deep_pullback_at = self.shared.load_deep_pullback();
            self.since_witness = 0;
            self.witnessed_at = None;
        }
        if self.broken {
            self.resync(tx, term, None);
            return;
        }
        if self.shared.keyframe_wanted() && self.resync(tx, term, None) {
            return;
        }
        if self.shared.load().is_some()
            && term.deep_history_pullback_ordinal() != self.deep_pullback_at
        {
            let marks = SliceMarks {
                sync_open: term.modes().synchronized_output(),
                command: false,
            };
            if !self.resync(tx, term, Some(marks)) && !tx.has_room() {
                self.broken = true;
                self.shared.dropped_input.fetch_add(1, Ordering::Release);
            }
            return;
        }
        if self.shared.load().is_some()
            && !self.catch_up(&mut TemporalSink(tx), SpinePre::take(term), term)
        {
            self.broken = true;
            self.shared.dropped_input.fetch_add(1, Ordering::Release);
            return;
        }
        match self.catch_up_host(tx, term) {
            CatchUpHost::Ok => {
                if self.shared.load().is_some() {
                    term.note_bounded_replay_step(REKEY_SCROLLBACK_LINES);
                }
            }
            CatchUpHost::Overrun => {
                if !self.resync(tx, term, None) {
                    self.broken = true;
                    self.shared.dropped_input.fetch_add(1, Ordering::Release);
                }
            }
            CatchUpHost::QueueFull => {
                self.broken = true;
                self.shared.dropped_input.fetch_add(1, Ordering::Release);
            }
        }
    }

    /// Record one slice on the spine with what the live screen showed after it ([`SliceMarks`]), then a live witness when one is due ([`WITNESS_BYTES`], [`WITNESS_INTERVAL`]).
    pub(crate) fn record(
        &mut self,
        tx: &TemporalTx,
        pre: SpinePre,
        term: &Terminal,
        mut input: InputSlice,
    ) {
        input.marks = SliceMarks {
            sync_open: term.modes().synchronized_output(),
            command: term.started_command_seq() != pre.started,
        };
        let generation = self.shared.generation();
        if generation != self.generation {
            // A forget cleared the spine: take up where it put the watermark.
            self.generation = generation;
            self.at = self.shared.load();
            self.cell_px = self.shared.load_cell_px();
            self.host_at = self.shared.load_host();
            self.deep_pullback_at = self.shared.load_deep_pullback();
            self.since_witness = 0;
            self.witnessed_at = None;
        }
        if self.broken {
            self.resync(tx, term, None);
            return;
        }
        if self.shared.keyframe_wanted() && self.resync(tx, term, Some(input.marks)) {
            return;
        }
        if term.deep_history_pullback_ordinal() != self.deep_pullback_at {
            if !self.resync(tx, term, Some(input.marks)) {
                self.broken = true;
                self.shared.dropped_input.fetch_add(1, Ordering::Release);
            }
            return;
        }
        let (len, at) = (input.len(), input.clock.monotonic);
        if !self.catch_up(&mut TemporalSink(tx), pre, term)
            || !self.catch_up_cell_px(tx, pre.cell_px)
        {
            self.broken = true;
            self.shared.dropped_input.fetch_add(1, Ordering::Release);
            return;
        }
        match self.catch_up_host(tx, term) {
            CatchUpHost::Ok => {}
            CatchUpHost::Overrun => {
                if !self.resync(tx, term, Some(input.marks)) {
                    self.broken = true;
                    self.shared.dropped_input.fetch_add(1, Ordering::Release);
                }
                return;
            }
            CatchUpHost::QueueFull => {
                self.broken = true;
                self.shared.dropped_input.fetch_add(1, Ordering::Release);
                return;
            }
        }
        if term.host_io_ordinal() != pre.host_io {
            if !self.resync(tx, term, Some(input.marks)) {
                self.broken = true;
                self.shared.dropped_input.fetch_add(1, Ordering::Release);
            }
            return;
        }
        if tx.send(TemporalMsg::RawIn(input)) {
            // Past anything the bytes themselves applied (they replay it).
            self.advance((term.resize_lifetime(), term.resize_ordinal()));
            self.advance_cell_px(term.host_cell_pixel_size());
            self.advance_host((term.resize_lifetime(), term.host_ordinal()));
            self.advance_deep_pullback(term.deep_history_pullback_ordinal());
            term.note_bounded_replay_step(REKEY_SCROLLBACK_LINES);
            self.witness(tx, term, len, at);
        } else {
            self.broken = true;
            self.shared.dropped_input.fetch_add(1, Ordering::Release);
        }
    }

    fn witness(&mut self, tx: &TemporalTx, term: &Terminal, len: usize, at: aterm_time::Instant) {
        let since = *self.witnessed_at.get_or_insert(at);
        self.since_witness = self.since_witness.saturating_add(len);
        let due = self.since_witness >= WITNESS_BYTES
            || at.saturating_duration_since(since) >= WITNESS_INTERVAL;
        if !due || !term.parser_is_ground() {
            return;
        }
        let Some(live) = term.checkpoint_visible() else {
            return;
        };
        if tx.send(TemporalMsg::Witness(Arc::new(live))) {
            self.since_witness = 0;
            self.witnessed_at = Some(at);
        }
    }

    /// Queue a keyframe of the live screen in place of the slice just parsed: one that closes a break (`live: None`), or a live one carrying that slice's marks.
    fn resync(&mut self, tx: &TemporalTx, term: &Terminal, live: Option<SliceMarks>) -> bool {
        if !tx.has_room()
            || term.partial_sequence_swallowed().is_some()
            || term.has_detached_scrollback()
        {
            return false;
        }
        let Some(mut keyframe) = keyframe_of(term, REKEY_SCROLLBACK_LINES) else {
            return false;
        };
        keyframe.recorded_config = Some(self.shared.replay_config());
        #[cfg(test)]
        resync_probe::projected();
        let msg = match live {
            None => TemporalMsg::Keyframe(keyframe),
            Some(marks) => TemporalMsg::LiveKeyframe(keyframe, marks),
        };
        if !tx.send(msg) {
            return false;
        }
        self.broken = false;
        self.shared.keyframes_queued.fetch_add(1, Ordering::Release);
        self.shared.keyframe_wanted.store(false, Ordering::Release);
        self.since_witness = 0;
        self.witnessed_at = None;
        self.advance_host((term.resize_lifetime(), term.host_ordinal()));
        self.advance((term.resize_lifetime(), term.resize_ordinal()));
        self.advance_cell_px(term.host_cell_pixel_size());
        self.advance_deep_pullback(term.deep_history_pullback_ordinal());
        true
    }

    /// Queue the instant of a reply the engine emitted (`take_response()`).
    pub(crate) fn reply(&self, tx: &TemporalTx) {
        if self.broken || !tx.send(TemporalMsg::Reply) {
            self.shared.dropped_replies.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// [`Self::record`] into any [`SpineSink`] (the crash-restore journal's tail, `crate::crash_restore`).
    #[cfg(unix)]
    pub(crate) fn record_to(
        &mut self,
        sink: &mut impl SpineSink,
        pre: SpinePre,
        term: &Terminal,
        bytes: &[u8],
    ) -> bool {
        if !self.catch_up(sink, pre, term) {
            return false;
        }
        if !sink.bytes(bytes) {
            return false;
        }
        self.advance((term.resize_lifetime(), term.resize_ordinal()));
        true
    }

    /// Bring the sink up to `pre`'s resize ordinal.
    pub(crate) fn catch_up(
        &mut self,
        sink: &mut impl SpineSink,
        pre: SpinePre,
        term: &Terminal,
    ) -> bool {
        if let Some(shared_at) = self.shared.load()
            && self.at.is_none_or(|cur| shared_at > cur)
        {
            self.at = Some(shared_at);
        }
        let since = match self.at {
            Some((lifetime, ordinal)) if lifetime == pre.lifetime && ordinal == pre.ordinal => {
                return true;
            }
            Some((lifetime, ordinal)) if lifetime == pre.lifetime && ordinal < pre.ordinal => {
                ordinal
            }
            _ => {
                let resize = SpineResize {
                    to: (pre.rows, pre.cols),
                    from: None,
                    lifetime: pre.lifetime,
                    ordinal: pre.ordinal,
                };
                if !sink.resize(resize) {
                    return false;
                }
                self.advance((pre.lifetime, pre.ordinal));
                return true;
            }
        };
        let (reports, lost) = term.resize_journal_since(since);
        if lost > 0 {
            if !sink.resizes_lost(lost) {
                return false;
            }
            let first_kept = reports
                .first()
                .map_or(pre.ordinal, |r| r.ordinal.saturating_sub(1));
            self.advance((pre.lifetime, first_kept));
        }
        for report in reports.iter().filter(|r| r.ordinal <= pre.ordinal) {
            let resize = SpineResize {
                to: report.to,
                from: Some(report.from),
                lifetime: pre.lifetime,
                ordinal: report.ordinal,
            };
            if !sink.resize(resize) {
                return false;
            }
            self.advance((pre.lifetime, report.ordinal));
        }
        true
    }
}

/// Per-session capture into the `aterm-buffer` temporal spine.
pub(crate) struct TemporalRecorder {
    log: EventLog,
    blobs: VecDeque<Option<InputSlice>>,
    host_ops: VecDeque<(HostOp, Option<ClockReading>)>,
    keyframes: VecDeque<RetainedKeyframe>,
    next_blob: u64,
    next_host_op: u64,
    next_keyframe: u64,
    used: usize,
    budget: usize,
    lost_resizes: u64,
    spine: Arc<SpineWatermark>,
    bytes_since_keyframe: usize,
    ends_mid_sequence: bool,
    #[cfg(test)]
    disabled_trigger: Option<RekeyTrigger>,
    #[cfg(test)]
    frozen_now: Option<Ticks>,
    /// A sleep the test adds to the clock, as a machine asleep would.
    #[cfg(test)]
    slept_ns: u64,
    epoch: Instant,
    /// The sleep-counting clock at `epoch` ([`Self::clock_ns`]), when the platform has one.
    epoch_ns: Option<u64>,
    resize_policy: ResizePolicy,
    host_resyncs_alt_screen: bool,
    replay_config: Option<Arc<TerminalConfig>>,
    handoff: Option<crate::session_timeline::HandoffGap>,
    witnesses: VecDeque<Witnessed>,
    next_witness: u64,
    witnessed: u64,
    diverged: u64,
    chain_diverged: bool,
    generation: u64,
    share: Option<Share>,
}

/// What the live witnesses say of one replayed instant ([`TemporalRecorder::fidelity_at`]); only `Faithful` may be shown as what was on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fidelity {
    /// The first witness after the last slice a replay to the instant folds, and every one before it since the instant's keyframe, matched the replay.
    Faithful,
    /// A witness since the instant's keyframe, up to the first after the last slice the replay folds, did not match the replay.
    Diverged,
    /// No fold has judged every witness that covers the instant yet, or none covers it.
    Unwitnessed,
}

/// One live witness on the spine: its snapshot until a fold judges it, then only its verdict, and the digest a pane replay matches ([`screen_digest`]).
struct Witnessed {
    id: WitnessId,
    seq: Seq,
    ts: Ticks,
    screen: Digest,
    state: WitnessState,
}

enum WitnessState {
    Pending(Arc<TerminalCheckpoint>),
    Matched,
    Diverged,
}

impl Witnessed {
    fn charge(&self) -> usize {
        match &self.state {
            WitnessState::Pending(live) => charge_of(live),
            WitnessState::Matched | WitnessState::Diverged => 0,
        }
    }
}

/// The verdicts a fold reached on the live witnesses it passed, for [`TemporalRecorder::judge`].
#[must_use = "a verdict counts once the recorder is handed it (`TemporalRecorder::judge`)"]
#[derive(Debug, Default)]
pub(crate) struct Judged(Vec<(WitnessId, bool)>);

impl Judged {
    /// Whether the fold judged no witness.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A fold's replayed engine and its verdicts ([`ReplayPlan::fold`]).
pub(crate) struct Fold {
    /// The engine at the plan's instant.
    pub(crate) term: Terminal,
    /// What the fold found of each pending witness it passed.
    pub(crate) judged: Judged,
}

fn charge_of(checkpoint: &TerminalCheckpoint) -> usize {
    checkpoint.grid.len() + checkpoint.alt_grid.as_ref().map_or(0, Vec::len)
}

impl TemporalRecorder {
    /// A recorder with the default budget.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }

    /// A recorder with an explicit retained-byte budget (>= 1).
    #[must_use]
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            log: EventLog::default(),
            blobs: VecDeque::new(),
            host_ops: VecDeque::new(),
            keyframes: VecDeque::new(),
            next_blob: 0,
            next_host_op: 0,
            next_keyframe: 0,
            used: 0,
            budget: budget.max(1),
            lost_resizes: 0,
            spine: Arc::default(),
            bytes_since_keyframe: 0,
            ends_mid_sequence: false,
            #[cfg(test)]
            disabled_trigger: None,
            #[cfg(test)]
            frozen_now: None,
            #[cfg(test)]
            slept_ns: 0,
            epoch: Instant::now(),
            epoch_ns: aterm_uds::process::continuous_ns(),
            resize_policy: ResizePolicy::Native,
            host_resyncs_alt_screen: false,
            replay_config: None,
            handoff: None,
            witnesses: VecDeque::new(),
            next_witness: 0,
            witnessed: 0,
            diverged: 0,
            chain_diverged: false,
            generation: 0,
            share: None,
        }
    }

    /// What this recorder charges its pool: its retained bytes and the slots its log and slices hold.
    fn charge(&self) -> usize {
        self.used
            + self.blobs.len() * std::mem::size_of::<Option<InputSlice>>()
            + self.host_ops.len() * std::mem::size_of::<(HostOp, Option<ClockReading>)>()
            + self.live_events() * std::mem::size_of::<aterm_buffer::Event>()
    }

    /// Bring this recorder's charge in its pool up to date.
    fn settle(&mut self) {
        let now = self.charge();
        if let Some(share) = self.share.as_mut() {
            share.move_to(now);
        }
    }

    fn note_freed(&self, bytes: usize) {
        if let Some(share) = self.share.as_ref() {
            share.pool.note_freed(bytes);
        }
    }

    fn compact_storage(&mut self) {
        fn shrink<T>(q: &mut VecDeque<T>) {
            if q.capacity() > 64 && q.capacity() / 4 > q.len() {
                q.shrink_to_fit();
            }
        }
        shrink(&mut self.blobs);
        shrink(&mut self.host_ops);
        shrink(&mut self.keyframes);
        shrink(&mut self.witnesses);
    }

    /// Drop every kept event, slice, keyframe and witness, so no replay reaches an instant before now; a message queued before it is dropped as it arrives.
    pub(crate) fn forget(&mut self) {
        self.generation += 1;
        self.spine
            .generation
            .store(self.generation, Ordering::Release);
        self.spine.keyframes_queued.store(0, Ordering::Release);
        self.log.forget();
        self.blobs = VecDeque::new();
        self.host_ops = VecDeque::new();
        self.keyframes = VecDeque::new();
        self.witnesses = VecDeque::new();
        self.used = 0;
        self.bytes_since_keyframe = 0;
        self.ends_mid_sequence = false;
        self.chain_diverged = false;
        self.settle();
    }

    /// [`Self::forget`], then keep `term` as the first state; the caller holds `term`'s lock, so no reader is mid-slice, and takes the recorder's lock inside it.
    pub(crate) fn forget_onto(&mut self, term: &Terminal) -> bool {
        self.forget();
        let seed = if term.partial_sequence_swallowed().is_some() || term.has_detached_scrollback()
        {
            None
        } else {
            keyframe_of(term, REKEY_SCROLLBACK_LINES)
        };
        let Some(seed) = seed else {
            self.want_keyframe();
            return false;
        };
        self.record_keyframe(seed);
        self.spine.seed(SpinePre::take(term));
        self.spine.keyframe_wanted.store(false, Ordering::Release);
        true
    }

    /// Ask the reader for a live keyframe on its next slice at parser ground.
    pub(crate) fn want_keyframe(&self) {
        self.spine.keyframe_wanted.store(true, Ordering::Release);
    }

    /// The pool's [`Self::forget`]: the reader keys the recording afresh on its next slice.
    fn forget_for_pool(&mut self) {
        self.forget();
        self.want_keyframe();
    }

    /// Drop the slices before the oldest keyframe, then the oldest chains, while `over` holds and a chain besides the newest is kept.
    fn shed_while(&mut self, over: &dyn Fn() -> bool) {
        if !over() {
            return;
        }
        let first = self
            .keyframes
            .front()
            .map_or(self.next_blob, |kf| kf.first_blob);
        while self.blob_base() < first && self.pop_blob() {}
        let first_host = self
            .keyframes
            .front()
            .map_or(self.next_host_op, |kf| kf.first_host_op);
        while self.host_op_base() < first_host && self.pop_host_op() {}
        self.settle();
        while over() && self.keyframes.len() > 1 {
            self.pop_keyframe();
            let first = self
                .keyframes
                .front()
                .map_or(self.next_blob, |kf| kf.first_blob);
            while self.blob_base() < first && self.pop_blob() {}
            let first_host = self
                .keyframes
                .front()
                .map_or(self.next_host_op, |kf| kf.first_host_op);
            while self.host_op_base() < first_host && self.pop_host_op() {}
            self.settle();
        }
        self.compact_storage();
    }

    /// Mark this spine as one that starts at a seamless update ([`crate::session_timeline::HandoffGap`]).
    pub(crate) const fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// What `temporal status` appends to its line.
    #[must_use]
    pub(crate) fn status_tail(&self) -> String {
        self.handoff
            .map(|gap| format!(" {}", gap.payload()))
            .unwrap_or_default()
    }

    /// Replay under `policy` (every recorded resize, and the replayed engine's console-host posture).
    pub(crate) const fn set_resize_policy(&mut self, policy: ResizePolicy) {
        self.resize_policy = policy;
    }

    /// Replay under the live engine's alternate-screen recovery.
    pub(crate) const fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.host_resyncs_alt_screen = on;
    }

    /// Replay under `config`, the config the live engine was given last ([`ReplayEngine::with_config`]); a change asks the reader for a live keyframe, so each chain replays under the config its keyframe was recorded with.
    pub(crate) fn set_replay_config(&mut self, config: Option<Arc<TerminalConfig>>) {
        if self.replay_config.as_deref() == config.as_deref() {
            return;
        }
        self.spine.set_replay_config(config.clone());
        self.replay_config = config;
        if !self.keyframes.is_empty() || self.spine.keyframes_queued.load(Ordering::Acquire) > 0 {
            self.spine.keyframe_wanted.store(true, Ordering::Release);
        }
    }

    /// The current tick on this recorder's timeline: micros since the epoch on a clock that counts time asleep, so ticks apart are time elapsed.
    #[must_use]
    pub(crate) fn now(&self) -> Ticks {
        #[cfg(test)]
        if let Some(frozen) = self.frozen_now {
            return frozen;
        }
        let micros = match (self.epoch_ns, self.clock_ns()) {
            (Some(epoch), Some(now)) => u128::from(now.saturating_sub(epoch) / 1000),
            _ => self.epoch.elapsed().as_micros(),
        };
        Ticks(u64::try_from(micros).unwrap_or(u64::MAX))
    }

    /// The sleep-counting clock the timeline runs on, in nanoseconds (`std::time::Instant` stops while macOS sleeps).
    fn clock_ns(&self) -> Option<u64> {
        let ns = aterm_uds::process::continuous_ns();
        #[cfg(test)]
        let ns = ns.map(|ns| ns.saturating_add(self.slept_ns));
        ns
    }

    /// Let the clock run on by `slept` with no awake time passing, as a machine asleep would.
    #[cfg(test)]
    pub(crate) fn sleep_for_test(&mut self, slept: std::time::Duration) {
        let ns = u64::try_from(slept.as_nanos()).unwrap_or(u64::MAX);
        self.slept_ns = self.slept_ns.saturating_add(ns);
    }

    fn append(&mut self, op: Op, ts: Ticks) -> Seq {
        let (seq, evicted) = self.log.append_at(op, ts);
        match evicted.map(|ev| (ev.seq, ev.op)) {
            Some((_, Op::RawIn(id) | Op::Reply(id))) => {
                while self.blob_base() <= id.0 && self.pop_blob() {}
            }
            Some((_, Op::Host(id))) => while self.host_op_base() <= id.0 && self.pop_host_op() {},
            Some((gone, Op::Keyframe(_))) => {
                while self.keyframes.front().is_some_and(|kf| kf.seq <= gone) {
                    self.pop_keyframe();
                }
            }
            Some((gone, Op::Witness(_))) => self.drop_witnesses_through(gone),
            _ => {}
        }
        seq
    }

    fn record(&mut self, op: Op) -> Option<Rekey> {
        let ts = self.now();
        self.append(op, ts);
        self.due()
    }

    fn due(&mut self) -> Option<Rekey> {
        let due = self.rekey_due();
        if due.is_none() {
            self.enforce_budget();
        }
        due
    }

    fn witness(&mut self, live: Arc<TerminalCheckpoint>) -> Option<Rekey> {
        if self.keyframes.is_empty() || self.chain_diverged {
            return None;
        }
        let ts = self.now();
        let id = WitnessId(self.next_witness);
        self.next_witness += 1;
        let seq = self.append(Op::Witness(id), ts);
        let charge = charge_of(&live);
        self.used += charge;
        self.bytes_since_keyframe += charge;
        self.witnesses.push_back(Witnessed {
            id,
            seq,
            ts,
            screen: screen_digest(&live),
            state: WitnessState::Pending(live),
        });
        self.due()
    }

    fn drop_witnesses_through(&mut self, gone: Seq) {
        while let Some(w) = self.witnesses.front().filter(|w| w.seq <= gone) {
            self.used = self.used.saturating_sub(w.charge());
            self.witnesses.pop_front();
        }
    }

    fn witness_mut(&mut self, id: WitnessId) -> Option<&mut Witnessed> {
        let base = self.witnesses.front()?.id.0;
        let idx = usize::try_from(id.0.checked_sub(base)?).ok()?;
        self.witnesses.get_mut(idx).filter(|w| w.id == id)
    }

    fn witness_of(&self, id: WitnessId) -> Option<&Witnessed> {
        let base = self.witnesses.front()?.id.0;
        let idx = usize::try_from(id.0.checked_sub(base)?).ok()?;
        self.witnesses.get(idx).filter(|w| w.id == id)
    }

    /// Record the verdicts a fold reached ([`ReplayPlan::fold`]), refunding each judged snapshot to the budget and, in the newest keyframe's chain only, to its rekey count; a mismatch there asks the reader for a live keyframe and keeps the chain from minting one.
    pub(crate) fn judge(&mut self, judged: Judged) {
        let newest = self.keyframes.back().map(|kf| kf.seq);
        for (id, matched) in judged.0 {
            let Some(w) = self.witness_mut(id) else {
                continue;
            };
            let WitnessState::Pending(live) = &w.state else {
                continue;
            };
            let charge = charge_of(live);
            w.state = if matched {
                WitnessState::Matched
            } else {
                WitnessState::Diverged
            };
            let in_newest_chain = newest.is_some_and(|kf| w.seq > kf);
            self.used = self.used.saturating_sub(charge);
            if in_newest_chain {
                self.bytes_since_keyframe = self.bytes_since_keyframe.saturating_sub(charge);
            }
            self.witnessed += 1;
            if !matched {
                self.diverged += 1;
                if in_newest_chain {
                    self.chain_diverged = true;
                    self.spine.keyframe_wanted.store(true, Ordering::Release);
                }
            }
        }
        self.settle();
    }

    /// What the live witnesses say of a replay to `at`, from the verdicts folds have reached so far: the witness that covers it is the first after the last slice the replay folds, whatever its tick.
    #[must_use]
    pub(crate) fn fidelity_at(&self, at: Ticks) -> Fidelity {
        let Some(base) = self.base_keyframe(at) else {
            return Fidelity::Unwitnessed;
        };
        let folded = self.last_folded(base.seq, at);
        let mut verdict = Fidelity::Faithful;
        for w in self.chain_witnesses(base.seq) {
            match w.state {
                WitnessState::Diverged => return Fidelity::Diverged,
                WitnessState::Pending(_) => verdict = Fidelity::Unwitnessed,
                WitnessState::Matched => {}
            }
            if w.seq > folded {
                return verdict;
            }
        }
        Fidelity::Unwitnessed
    }

    /// The instant of the witness that covers `at` ([`Self::fidelity_at`]), so a fold to it passes every witness `fidelity_at` reads for `at`.
    #[must_use]
    fn covering_witness(&self, at: Ticks) -> Option<Ticks> {
        let base = self.base_keyframe(at)?;
        let folded = self.last_folded(base.seq, at);
        self.chain_witnesses(base.seq)
            .find(|w| w.seq > folded)
            .map(|w| w.ts)
    }

    /// The replay that judges the witness covering `at` ([`Self::covering_witness`]), on `at`'s own tick included: `at`'s own chain folded to that witness's tick, so a next keyframe on the same tick does not take the fold to its chain.
    #[must_use]
    pub(crate) fn covering_plan(&self, at: Ticks) -> Option<ReplayPlan> {
        let base = self.base_keyframe(at)?;
        if base.cut.is_some_and(|end| at > end) {
            return None;
        }
        let to = self.covering_witness(at)?;
        self.plan_from(base, to)
    }

    /// The witnesses after keyframe `base` and before the next retained one, in seq order.
    fn chain_witnesses(&self, base: Seq) -> impl Iterator<Item = &Witnessed> {
        let end = self
            .keyframes
            .iter()
            .find(|kf| kf.seq > base)
            .map(|kf| kf.seq);
        self.witnesses
            .iter()
            .skip_while(move |w| w.seq <= base)
            .take_while(move |w| end.is_none_or(|end| w.seq < end))
    }

    /// The seq of the last slice, resize or host step after `base` that a replay to `at` folds ([`Self::plan`]), or `base` when it folds none.
    fn last_folded(&self, base: Seq, at: Ticks) -> Seq {
        self.log
            .live_after(base)
            .take_while(|ev| ev.ts <= at)
            .filter(|ev| {
                matches!(
                    ev.op,
                    Op::RawIn(_) | Op::Resize { .. } | Op::CellPx { .. } | Op::Host(_)
                )
            })
            .last()
            .map_or(base, |ev| ev.seq)
    }

    /// Witnesses a fold has judged (`temporal status` `witnessed=`).
    #[must_use]
    pub(crate) const fn witnessed(&self) -> u64 {
        self.witnessed
    }

    /// Judged witnesses the replay did not match (`temporal status` `diverged=`).
    #[must_use]
    pub(crate) const fn diverged(&self) -> u64 {
        self.diverged
    }

    /// Record a keyframe (a serialized [`TerminalCheckpoint`]), the seed for replays to instants at or after it.
    pub(crate) fn record_keyframe(&mut self, keyframe: impl Into<ReplayKeyframe>) {
        self.record_keyframe_inner(keyframe.into(), false, false);
        self.settle();
    }

    fn record_keyframe_with_marks(&mut self, keyframe: ReplayKeyframe, command: bool) {
        self.record_keyframe_inner(keyframe, command, true);
    }

    fn record_keyframe_inner(
        &mut self,
        mut keyframe: ReplayKeyframe,
        command: bool,
        coalesceable: bool,
    ) {
        let config = keyframe
            .recorded_config
            .take()
            .unwrap_or_else(|| self.replay_config.clone());
        let stale_config = config.as_deref() != self.replay_config.as_deref();
        self.record_keyframe_as(keyframe, false, command, coalesceable, config);
        if stale_config {
            self.spine.keyframe_wanted.store(true, Ordering::Release);
        }
    }

    fn has_chain_steps_after(&self, seq: Seq) -> bool {
        self.log.live_after(seq).any(|ev| {
            matches!(
                ev.op,
                Op::RawIn(_) | Op::Resize { .. } | Op::CellPx { .. } | Op::Host(_) | Op::Witness(_)
            )
        })
    }

    /// [`Self::record_keyframe`], marked as minted by a replay (a rekey) or taken from the live engine, as standing in for a slice that started a command, and its chain replaying under `config`.
    fn record_keyframe_as(
        &mut self,
        keyframe: ReplayKeyframe,
        replayed: bool,
        command: bool,
        coalesceable: bool,
        config: Option<Arc<TerminalConfig>>,
    ) {
        let ts = self.now();
        let id = KeyframeId(self.next_keyframe);
        self.next_keyframe += 1;
        let seq = self.append(Op::Keyframe(id), ts);
        let mut command = command;
        let mut freed_keyframe = 0usize;
        if coalesceable
            && self.keyframes.len() >= 2
            && self.keyframes.back().is_some_and(|prev| {
                prev.coalesceable
                    && prev.cut.is_none()
                    && prev.config.as_deref() == config.as_deref()
                    && prev.checkpoint.modes.synchronized_output()
                        == keyframe.checkpoint.modes.synchronized_output()
                    && !self.has_chain_steps_after(prev.seq)
            })
            && let Some(prev) = self.keyframes.pop_back()
        {
            let prev_charge = prev.charge();
            self.used = self.used.saturating_sub(prev_charge);
            freed_keyframe = prev_charge;
            command |= prev.command;
        }
        let keyframe = RetainedKeyframe {
            #[cfg(test)]
            id,
            seq,
            ts,
            first_blob: self.next_blob,
            first_host_op: self.next_host_op,
            cut: None,
            replayed,
            coalesceable,
            command,
            packed: false,
            checkpoint: keyframe.checkpoint,
            carry: keyframe.carry,
            config,
        };
        self.used += keyframe.charge();
        self.keyframes.push_back(keyframe);
        self.bytes_since_keyframe = 0;
        self.chain_diverged = false;
        while self.keyframes.len() > MAX_KEYFRAMES {
            self.pop_keyframe();
        }
        self.enforce_budget();
        self.pack_older_chains();
        if freed_keyframe > 0 {
            self.note_freed(freed_keyframe);
        }
    }

    /// Pack unshared small slices of completed chains into [`BLOB_SLAB_BYTES`] slabs so evicted
    /// chains drop Large VM allocations whole while slices still shared with `CastRecorder`, a
    /// byte tap, or a caller stay shared.
    fn pack_older_chains(&mut self) {
        if self.keyframes.len() < 2 {
            return;
        }
        let base = self.blob_base();
        let mut freed = 0usize;
        for k in 0..self.keyframes.len() - 1 {
            if self.keyframes[k].packed {
                continue;
            }
            let start_id = self.keyframes[k].first_blob.max(base);
            let end_id = self.keyframes[k + 1].first_blob.min(self.next_blob);
            if end_id <= start_id {
                self.keyframes[k].packed = true;
                continue;
            }
            let lo = usize::try_from(start_id - base).unwrap_or(self.blobs.len());
            let hi = usize::try_from(end_id - base)
                .unwrap_or(self.blobs.len())
                .min(self.blobs.len());
            if hi <= lo {
                self.keyframes[k].packed = true;
                continue;
            }
            // Identify contiguous runs of slices sharing one `Arc<[u8]>` whose `strong_count`
            // equals the run length (no holder outside this chain).
            let mut runs: Vec<Range<usize>> = Vec::new();
            let mut packable_bytes = 0usize;
            let mut all_small_unshared = true;
            let mut idx = lo;
            while idx < hi {
                let Some(first) = self.blobs[idx].as_ref() else {
                    idx += 1;
                    continue;
                };
                if first.len() == 0 || first.batch.len() > BLOB_SLAB_BYTES / 2 {
                    idx += 1;
                    continue;
                }
                let ptr = Arc::as_ptr(&first.batch).cast::<u8>();
                let strong = Arc::strong_count(&first.batch);
                let run_start = idx;
                let mut run_bytes = first.len();
                idx += 1;
                while idx < hi {
                    let Some(next) = self.blobs[idx].as_ref() else {
                        break;
                    };
                    if Arc::as_ptr(&next.batch).cast::<u8>() != ptr
                        || next.len() == 0
                        || next.batch.len() > BLOB_SLAB_BYTES / 2
                    {
                        break;
                    }
                    run_bytes += next.len();
                    idx += 1;
                }
                if strong == idx - run_start {
                    runs.push(run_start..idx);
                    packable_bytes += run_bytes;
                } else {
                    all_small_unshared = false;
                }
            }
            if packable_bytes == 0 {
                if all_small_unshared {
                    self.keyframes[k].packed = true;
                }
                continue;
            }
            if !all_small_unshared && packable_bytes < BLOB_SLAB_BYTES / 2 {
                continue;
            }
            let mut slab_indices: Vec<usize> = Vec::new();
            let mut slab_buf = Vec::with_capacity(packable_bytes.min(BLOB_SLAB_BYTES));
            let flush_slab = |blobs: &mut VecDeque<Option<InputSlice>>,
                              indices: &mut Vec<usize>,
                              buf: &mut Vec<u8>| {
                if indices.is_empty() {
                    return;
                }
                let slab: Arc<[u8]> = std::mem::take(buf).into();
                let mut at = 0usize;
                for &slot_idx in indices.iter() {
                    if let Some(slice) = blobs[slot_idx].as_mut() {
                        let len = slice.len();
                        slice.batch = slab.clone();
                        slice.range = at..at + len;
                        at += len;
                    }
                }
                indices.clear();
            };
            for run in runs {
                for slot_idx in run {
                    let len = self.blobs[slot_idx].as_ref().map_or(0, InputSlice::len);
                    if !slab_indices.is_empty() && slab_buf.len() + len > BLOB_SLAB_BYTES {
                        flush_slab(&mut self.blobs, &mut slab_indices, &mut slab_buf);
                        slab_buf.reserve_exact(
                            packable_bytes.saturating_sub(freed).min(BLOB_SLAB_BYTES),
                        );
                    }
                    if let Some(slice) = self.blobs[slot_idx].as_ref() {
                        slab_buf.extend_from_slice(slice.bytes());
                        slab_indices.push(slot_idx);
                        freed += len;
                    }
                }
            }
            flush_slab(&mut self.blobs, &mut slab_indices, &mut slab_buf);
            if all_small_unshared {
                self.keyframes[k].packed = true;
            }
        }
        if freed > 0 {
            self.note_freed(freed);
        }
    }

    fn close_break(&mut self, keyframe: ReplayKeyframe) {
        let end = self.latest_tick();
        if let Some(newest) = self.keyframes.back_mut() {
            newest.cut = Some(end);
        }
        self.ends_mid_sequence = false;
        self.record_keyframe(keyframe);
        self.spine.closed.fetch_add(1, Ordering::Release);
    }

    fn rekey_due(&self) -> Option<Rekey> {
        if self.ends_mid_sequence || self.chain_diverged {
            return None;
        }
        let newest = self.keyframes.back().filter(|kf| {
            kf.first_blob >= self.blob_base() && kf.first_host_op >= self.host_op_base()
        })?;
        let chain = self.bytes_since_keyframe;
        let interval = (self.budget / 4).max(MIN_REKEY_BYTES);
        let by_bytes = chain >= interval;
        let by_events = self.log.head().0.saturating_sub(newest.seq.0) >= REKEY_EVENTS;
        let by_pressure = chain >= MIN_REKEY_BYTES
            && newest.charge() + chain > self.budget.saturating_sub(interval);
        #[cfg(test)]
        let (by_events, by_pressure) = (
            by_events && self.disabled_trigger != Some(RekeyTrigger::Events),
            by_pressure && self.disabled_trigger != Some(RekeyTrigger::Pressure),
        );
        if !(by_bytes || by_events || by_pressure) {
            return None;
        }
        Some(Rekey {
            plan: self.plan(self.latest_tick())?,
            head: self.log.head(),
            generation: self.generation,
        })
    }

    /// Record the verdicts and the keyframe a due re-keyframe minted ([`Rekey::mint`]), unless a witness in its chain diverged, then hold the budget.
    pub(crate) fn rekeyed(&mut self, minted: Minted) {
        self.note_freed(BLOB_SLAB_BYTES);
        if minted.generation != self.generation {
            return;
        }
        self.judge(minted.judged);
        match minted.keyframe {
            Some(keyframe) if self.log.head() == minted.head && !self.chain_diverged => {
                self.record_keyframe_as(keyframe, true, false, false, minted.config);
            }
            Some(_) => self.enforce_budget(),
            None => {
                self.ends_mid_sequence = true;
                self.enforce_budget();
            }
        }
        self.settle();
    }

    fn store_blob(&mut self, input: Option<InputSlice>) -> BlobId {
        let id = BlobId(self.next_blob);
        self.next_blob += 1;
        self.used += input.as_ref().map_or(0, InputSlice::charge);
        self.blobs.push_back(input);
        id
    }

    fn blob_base(&self) -> u64 {
        self.next_blob - self.blobs.len() as u64
    }

    fn pop_blob(&mut self) -> bool {
        let Some(slot) = self.blobs.pop_front() else {
            return false;
        };
        self.used = self
            .used
            .saturating_sub(slot.as_ref().map_or(0, InputSlice::charge));
        true
    }

    fn store_host_op(&mut self, op: HostOp, clock: Option<ClockReading>) -> HostOpId {
        let id = HostOpId(self.next_host_op);
        self.next_host_op += 1;
        self.host_ops.push_back((op, clock));
        id
    }

    fn host_op_base(&self) -> u64 {
        self.next_host_op - self.host_ops.len() as u64
    }

    fn pop_host_op(&mut self) -> bool {
        self.host_ops.pop_front().is_some()
    }

    fn host_op(&self, id: HostOpId) -> Option<(HostOp, Option<ClockReading>)> {
        let idx = usize::try_from(id.0.checked_sub(self.host_op_base())?).ok()?;
        self.host_ops.get(idx).copied()
    }

    fn pop_keyframe(&mut self) -> bool {
        let Some(keyframe) = self.keyframes.pop_front() else {
            return false;
        };
        self.used = self.used.saturating_sub(keyframe.charge());
        let first_host = self
            .keyframes
            .front()
            .map_or(self.next_host_op, |kf| kf.first_host_op);
        while self.host_op_base() < first_host && self.pop_host_op() {}
        let base = self.keyframes.front().map_or(Seq(u64::MAX), |kf| kf.seq);
        self.drop_witnesses_through(Seq(base.0.saturating_sub(1)));
        true
    }

    fn enforce_budget(&mut self) {
        let mut evicted = false;
        while self.used > self.budget {
            let base = self.blob_base();
            let host_base = self.host_op_base();
            let oldest_chain = self
                .keyframes
                .front()
                .map(|kf| (kf.first_blob, kf.first_host_op));
            if oldest_chain
                .is_some_and(|(first, first_host)| first < base || first_host < host_base)
            {
                evicted |= self.pop_keyframe();
            } else if !self.blobs.is_empty() && oldest_chain.is_none_or(|(first, _)| base < first) {
                evicted |= self.pop_blob();
            } else if self.keyframes.len() > 1 {
                evicted |= self.pop_keyframe();
            } else if self.pop_blob() || self.pop_keyframe() {
                evicted = true;
            } else {
                break;
            }
        }
        if evicted {
            self.compact_storage();
        }
    }

    /// Total events ever appended to the spine (live + evicted).
    #[must_use]
    pub(crate) const fn total_events(&self) -> u64 {
        self.log.total()
    }

    /// Live (un-evicted) event count on the spine.
    #[must_use]
    pub(crate) fn live_events(&self) -> usize {
        match (self.log.oldest_live(), self.log.newest_live()) {
            (Some(oldest), Some(newest)) => {
                usize::try_from(newest.seq.0 - oldest.seq.0 + 1).unwrap_or(usize::MAX)
            }
            _ => 0,
        }
    }

    /// Keyframes currently retained.
    #[must_use]
    pub(crate) fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// Events the spine's ring evicted: `total_events - live_events`, every seq below the ring's low-water.
    #[must_use]
    pub(crate) fn dropped_events(&self) -> u64 {
        self.log
            .oldest_live()
            .map_or(self.log.total(), |oldest| oldest.seq.0 - 1)
    }

    /// The earliest tick from which every later tick replays, the spine's end included.
    #[must_use]
    pub(crate) fn reachable_from(&self) -> Option<Ticks> {
        if self.spine.break_open() {
            return None;
        }
        self.oldest_replayable().map(|kf| kf.ts)
    }

    fn oldest_replayable(&self) -> Option<&RetainedKeyframe> {
        let base = self.blob_base();
        let host_base = self.host_op_base();
        self.keyframes
            .iter()
            .rev()
            .take_while(|kf| {
                kf.first_blob >= base && kf.first_host_op >= host_base && kf.cut.is_none()
            })
            .last()
    }

    /// Fold one message from the reader into the spine, in FIFO order: the writer thread's whole job ([`write_queued`]).
    #[must_use]
    pub(crate) fn apply(&mut self, msg: TemporalMsg) -> Option<Rekey> {
        let due = self.apply_op(msg);
        self.settle();
        due
    }

    /// [`Self::apply`] for a message sent in recording generation `generation`: one sent before a forget is dropped, a break it closes still counted.
    #[must_use]
    fn apply_in(&mut self, generation: u64, msg: TemporalMsg) -> Option<Rekey> {
        if generation == self.generation {
            return self.apply(msg);
        }
        if let TemporalMsg::Keyframe(_) = msg {
            self.spine.closed.fetch_add(1, Ordering::Release);
        }
        None
    }

    fn apply_op(&mut self, msg: TemporalMsg) -> Option<Rekey> {
        let op = match msg {
            TemporalMsg::RawIn(input) => {
                self.bytes_since_keyframe += input.len();
                self.ends_mid_sequence = false;
                Op::RawIn(self.store_blob(Some(input)))
            }
            TemporalMsg::Reply => Op::Reply(self.store_blob(None)),
            TemporalMsg::Resize { rows, cols } => Op::Resize { rows, cols },
            TemporalMsg::CellPx { width, height } => Op::CellPx { width, height },
            TemporalMsg::Host(op, clock) => Op::Host(self.store_host_op(op, clock)),
            TemporalMsg::ResizesLost(n) => {
                self.lost_resizes = self.lost_resizes.saturating_add(n);
                return None;
            }
            TemporalMsg::Keyframe(checkpoint) => {
                let _ = self.spine.keyframes_queued.try_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |n| Some(n.saturating_sub(1)),
                );
                self.close_break(checkpoint);
                return None;
            }
            TemporalMsg::LiveKeyframe(keyframe, marks) => {
                let _ = self.spine.keyframes_queued.try_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |n| Some(n.saturating_sub(1)),
                );
                self.ends_mid_sequence = false;
                self.record_keyframe_with_marks(keyframe, marks.command);
                return None;
            }
            TemporalMsg::Witness(live) => return self.witness(live),
        };
        self.record(op)
    }

    /// [`Self::apply`], minting a due re-keyframe in place.
    #[cfg(test)]
    pub(crate) fn apply_inline(&mut self, msg: TemporalMsg) {
        if let Some(due) = self.apply(msg) {
            let minted = due.mint();
            self.rekeyed(minted);
        }
    }

    /// Record a raw PTY-input burst (one copy into a shared allocation).
    #[cfg(test)]
    pub(crate) fn record_raw_in(&mut self, bytes: &[u8]) {
        self.apply_inline(TemporalMsg::RawIn(InputSlice::at(
            bytes,
            ClockReading::now(),
        )));
    }

    /// Record a raw PTY-input burst, retaining the caller's allocation.
    #[cfg(test)]
    pub(crate) fn record_raw_in_shared(&mut self, bytes: Arc<[u8]>) {
        let range = 0..bytes.len();
        self.apply_inline(TemporalMsg::RawIn(InputSlice::new(
            bytes,
            range,
            ClockReading::now(),
        )));
    }

    /// Record a reply's instant.
    #[cfg(test)]
    pub(crate) fn record_reply(&mut self) {
        self.apply_inline(TemporalMsg::Reply);
    }

    /// Record a geometry change.
    #[cfg(test)]
    pub(crate) fn record_resize(&mut self, rows: u16, cols: u16) {
        self.apply_inline(TemporalMsg::Resize { rows, cols });
    }

    /// Input slices the session's readers could not queue (`temporal status` `dropped_input=`).
    #[must_use]
    pub(crate) fn dropped_input(&self) -> u64 {
        self.spine.dropped_input()
    }

    /// Reply instants the session's readers could not queue (`temporal status` `dropped_replies=`).
    #[must_use]
    pub(crate) fn dropped_replies(&self) -> u64 {
        self.spine.dropped_replies()
    }

    /// Resizes the live engine applied that the spine never held.
    #[must_use]
    pub(crate) const fn lost_resizes(&self) -> u64 {
        self.lost_resizes
    }

    /// The session's spine watermark ([`SpineWatermark`]).
    #[must_use]
    pub(crate) fn spine_watermark(&self) -> Arc<SpineWatermark> {
        self.spine.clone()
    }

    /// The latest recorded instant on the live spine (the default replay target), or `Ticks(0)` before any event.
    #[must_use]
    pub(crate) fn latest_tick(&self) -> Ticks {
        self.log.newest_live().map(|e| e.ts).unwrap_or_default()
    }

    fn blob(&self, id: BlobId) -> Option<&InputSlice> {
        let idx = usize::try_from(id.0.checked_sub(self.blob_base())?).ok()?;
        self.blobs.get(idx)?.as_ref()
    }

    #[cfg(test)]
    fn blob_bytes(&self, id: BlobId) -> Option<&[u8]> {
        self.blob(id).map(InputSlice::bytes)
    }

    /// Every retained `RawIn` slice, oldest first: the batch it shares and its range of it.
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn retained_inputs(&self) -> Vec<(Arc<[u8]>, Range<usize>)> {
        self.blobs
            .iter()
            .flatten()
            .map(|input| (input.batch.clone(), input.range.clone()))
            .collect()
    }

    fn pending_witness(&self, id: WitnessId) -> Option<Arc<TerminalCheckpoint>> {
        let base = self.witnesses.front()?.id.0;
        let w = self
            .witnesses
            .get(usize::try_from(id.0.checked_sub(base)?).ok()?)?;
        match &w.state {
            WitnessState::Pending(live) if w.id == id => Some(live.clone()),
            _ => None,
        }
    }

    fn base_keyframe(&self, at: Ticks) -> Option<&RetainedKeyframe> {
        self.keyframes.iter().rev().find(|kf| kf.ts <= at)
    }

    fn plan(&self, at: Ticks) -> Option<ReplayPlan> {
        let base = self.base_keyframe(at)?;
        if base.cut.is_some_and(|end| at > end) {
            return None;
        }
        self.plan_from(base, at)
    }

    /// The replay of `base`'s chain through every event at or before `at`, each step with its seq and tick.
    fn plan_from(&self, base: &RetainedKeyframe, at: Ticks) -> Option<ReplayPlan> {
        let mut steps = Vec::new();
        let next = self
            .keyframes
            .iter()
            .find(|kf| kf.seq > base.seq)
            .map(|kf| kf.seq);
        for ev in self.log.live_after(base.seq) {
            if ev.ts > at || next.is_some_and(|next| ev.seq >= next) {
                break;
            }
            let step = match ev.op {
                Op::RawIn(id) => Step::RawIn(self.blob(id)?.clone()),
                Op::Resize { rows, cols } => Step::Resize { rows, cols },
                Op::CellPx { width, height } => Step::CellPx { width, height },
                Op::Host(id) => {
                    let (op, clock) = self.host_op(id)?;
                    Step::Host(op, clock)
                }
                Op::Witness(id) => match self.pending_witness(id) {
                    Some(live) => Step::Witness(id, live),
                    None => continue,
                },
                _ => continue,
            };
            steps.push(Placed {
                seq: ev.seq,
                ts: ev.ts,
                step,
            });
        }
        Some(ReplayPlan {
            base: base.checkpoint.clone(),
            carry: base.carry.clone(),
            base_seq: base.seq,
            base_ts: base.ts,
            steps,
            policy: self.resize_policy,
            host_resyncs: self.host_resyncs_alt_screen,
            live_base: !base.replayed,
            config: base.config.clone(),
        })
    }

    /// The replay of each replayable chain to `at`'s, oldest first, the last folding only to `at` ([`App::rewind_changed`](crate::App)).
    #[must_use]
    pub(crate) fn chain_plans(&self, at: Ticks) -> Vec<ReplayPlan> {
        let Some(oldest) = self.oldest_replayable().map(|kf| kf.seq) else {
            return Vec::new();
        };
        let Some(last) = self.base_keyframe(at).map(|kf| kf.seq) else {
            return Vec::new();
        };
        self.keyframes
            .iter()
            .filter(|kf| kf.seq >= oldest && kf.seq <= last)
            .filter_map(|kf| self.plan_from(kf, if kf.seq == last { at } else { Ticks(u64::MAX) }))
            .collect()
    }

    /// The whole replay of the replayable chain before `at`'s, which a view at `at` may step back into.
    #[must_use]
    pub(crate) fn chain_before(&self, at: Ticks) -> Option<ReplayPlan> {
        let oldest = self.oldest_replayable()?.seq;
        let last = self.base_keyframe(at)?.seq;
        let before = self
            .keyframes
            .iter()
            .rev()
            .find(|kf| kf.seq >= oldest && kf.seq < last)?;
        self.plan_from(before, Ticks(u64::MAX))
    }

    /// The whole replays a view at `at` keeps forks along: the replayable chain before `at`'s, then `at`'s ([`crate::rewind_worker::Show::around`]).
    #[must_use]
    pub(crate) fn warm_plans(&self, at: Ticks) -> Vec<ReplayPlan> {
        let mut plans: Vec<ReplayPlan> = self.chain_before(at).into_iter().collect();
        let oldest = self.oldest_replayable().map(|kf| kf.seq);
        plans.extend(
            self.base_keyframe(at)
                .filter(|kf| oldest.is_some_and(|oldest| kf.seq >= oldest))
                .and_then(|kf| self.plan_from(kf, Ticks(u64::MAX))),
        );
        plans
    }

    /// Whether a judged witness contradicts the replay at each of `instants` (ascending), as [`Self::fidelity_at`] reads them: from the first slice that brings a diverged witness into cover to the chain's end.
    #[must_use]
    pub(crate) fn contradicted(&self, instants: &[RewindInstant]) -> Vec<bool> {
        let from: Vec<(Ticks, Option<Ticks>)> = self
            .keyframes
            .iter()
            .map(|kf| (kf.ts, self.diverged_from(kf)))
            .collect();
        instants
            .iter()
            .map(|i| {
                from.iter()
                    .rev()
                    .find(|(ts, _)| *ts <= i.at)
                    .and_then(|(_, f)| *f)
                    .is_some_and(|f| i.at >= f)
            })
            .collect()
    }

    /// The first tick of `base`'s chain whose replay [`Self::fidelity_at`] calls diverged, when a witness in it is.
    fn diverged_from(&self, base: &RetainedKeyframe) -> Option<Ticks> {
        let mut before = None;
        for w in self.chain_witnesses(base.seq) {
            if matches!(w.state, WitnessState::Diverged) {
                let Some(before) = before else {
                    return Some(base.ts);
                };
                return Some(
                    self.log
                        .live_after(before)
                        .find(|ev| {
                            matches!(
                                ev.op,
                                Op::RawIn(_) | Op::Resize { .. } | Op::CellPx { .. } | Op::Host(_)
                            )
                        })
                        .map_or(Ticks(u64::MAX), |ev| ev.ts),
                );
            }
            before = Some(w.seq);
        }
        None
    }

    /// Stamp every event from now on at `tick`, or at the clock again with `None`.
    #[cfg(test)]
    pub(crate) const fn freeze_now_for_test(&mut self, tick: Option<Ticks>) {
        self.frozen_now = tick;
    }

    /// The replay `temporal` folds for instant `at` (default: the spine's end).
    #[must_use]
    pub(crate) fn replay_plan(&self, at: Option<Ticks>) -> Option<ReplayPlan> {
        let latest = self.latest_tick();
        if self.spine.break_open() && at.is_none_or(|tick| tick > latest) {
            return None;
        }
        self.plan(at.unwrap_or(latest))
    }

    /// The distinct instants a Rewind view steps through, oldest first: the oldest keyframe every later instant replays from, then each later tick a slice, resize or live keyframe lands on, less each the live screen showed inside an open mode-2026 frame (its command start goes to the next kept instant) and each empty slice that ended no frame.
    #[must_use]
    pub(crate) fn rewind_instants(&self) -> Vec<RewindInstant> {
        if self.spine.break_open() {
            return Vec::new();
        }
        let Some(base) = self.oldest_replayable() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut open = Some(RewindInstant {
            at: base.ts,
            command: false,
        });
        let mut sync_open = base.checkpoint.modes.synchronized_output();
        let mut carried = false;
        for ev in self.log.live_after(base.seq) {
            let (marks, starts) = match ev.op {
                Op::RawIn(id) => {
                    let slice = self.blob(id);
                    let ends_frame = sync_open && slice.is_some_and(|s| !s.marks().sync_open);
                    let starts = ends_frame || slice.is_none_or(|s| !s.bytes().is_empty());
                    (slice.map(InputSlice::marks), starts)
                }
                Op::Resize { .. } => (None, true),
                Op::Host(id) => match self.host_op(id) {
                    Some((op, _)) if op.is_screen_cut() => (
                        matches!(op, HostOp::Reset | HostOp::LeaveAlternateScreen)
                            .then_some(SliceMarks::default()),
                        true,
                    ),
                    _ => continue,
                },
                Op::Keyframe(_) => {
                    let Some(kf) = self.keyframes.iter().find(|kf| kf.seq == ev.seq) else {
                        continue;
                    };
                    let marks = Some(SliceMarks {
                        sync_open: kf.checkpoint.modes.synchronized_output(),
                        command: kf.command,
                    });
                    (marks, !kf.replayed)
                }
                _ => continue,
            };
            if open.is_none_or(|o| ev.ts > o.at) {
                if let Some(o) = open.take() {
                    if sync_open {
                        carried |= o.command;
                    } else {
                        out.push(RewindInstant {
                            command: o.command || std::mem::take(&mut carried),
                            ..o
                        });
                    }
                }
                open = starts.then_some(RewindInstant {
                    at: ev.ts,
                    command: false,
                });
            }
            if let Some(marks) = marks {
                sync_open = marks.sync_open;
                if let Some(o) = open.as_mut() {
                    o.command |= marks.command;
                }
            }
        }
        out.extend(open.filter(|_| !sync_open).map(|o| RewindInstant {
            command: o.command || carried,
            ..o
        }));
        out
    }

    /// The Rewind track: the ticks the spine still names, and the stretches in them a view cannot rebuild or whose replay a live witness did not match.
    #[must_use]
    pub(crate) fn rewind_track(&self) -> Track {
        let to = self.latest_tick();
        let replayable = self.oldest_replayable().map(|kf| (kf.seq, kf.ts));
        let first = [
            self.log.oldest_live().map(|ev| ev.ts),
            self.keyframes.front().map(|kf| kf.ts),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(to);
        let mut spans = Vec::new();
        match replayable {
            Some((_, from)) if from > first => spans.push((first, from, Span::Lost)),
            Some(_) => {}
            None => spans.push((first, to, Span::Lost)),
        }
        if self.spine.break_open() {
            spans.push((first, to, Span::Lost));
        }
        let kept = self
            .keyframes
            .iter()
            .filter(|kf| replayable.is_some_and(|(seq, _)| kf.seq >= seq));
        for kf in kept {
            let end = self
                .keyframes
                .iter()
                .find(|next| next.seq > kf.seq)
                .map_or(to, |next| next.ts);
            let mut before = kf.ts;
            for w in self.chain_witnesses(kf.seq) {
                if matches!(w.state, WitnessState::Diverged) {
                    spans.push((before, end, Span::Differs));
                    break;
                }
                before = w.ts;
            }
        }
        Track {
            from: first,
            to,
            spans,
        }
    }

    /// What `rewind export` writes: the oldest replayable keyframe and every slice and resize after it.
    /// # Errors
    /// Output was dropped that no keyframe follows yet, no kept keyframe replays, or a slice's bytes are gone.
    pub(crate) fn export(&self) -> Result<Export, NoExport> {
        if self.spine.break_open() {
            return Err(NoExport::Broken);
        }
        let base = self
            .oldest_replayable()
            .ok_or(NoExport::NothingReplayable)?;
        let mut events = Vec::new();
        for ev in self.log.live_after(base.seq) {
            let t = ev.ts.0.saturating_sub(base.ts.0);
            match ev.op {
                Op::RawIn(id) => {
                    let slice = self.blob(id).ok_or(NoExport::BytesGone)?;
                    events.push((t, Exported::Output(slice.clone())));
                }
                Op::Resize { rows, cols } => events.push((t, Exported::Resize { rows, cols })),
                Op::Keyframe(_) => {
                    let live = self.keyframes.iter().find(|kf| kf.seq == ev.seq);
                    if let Some(kf) = live.filter(|kf| !kf.replayed) {
                        events.push((t, Exported::Screen(kf.checkpoint.clone())));
                    }
                }
                _ => {}
            }
        }
        Ok(Export {
            base: base.checkpoint.clone(),
            events,
            dropped: (self.dropped_input(), self.lost_resizes()),
        })
    }

    fn chain_env(&self, kf: &RetainedKeyframe) -> PaneEnv {
        PaneEnv {
            config: kf.config.as_deref().cloned().unwrap_or_default(),
            policy: self.resize_policy,
            host_resyncs_alt_screen: self.host_resyncs_alt_screen,
            host_alternate_scroll: kf.carry.host_alternate_scroll().unwrap_or(false),
            cell_px: kf.carry.host_cell_px(),
            security_policy: kf.carry.security_policy(),
            hyperlink_schemes: kf.carry.hyperlink_schemes(),
            kitty_media_admitted: kf.carry.kitty_media_admitted(),
        }
    }

    /// Each replayable chain as a recording of the pane task ([`aterm_aos_pane`]), oldest first: its keyframe under the config it was recorded with, the session's resize policy and alternate-screen recovery, then every slice with its clock and reply, resize and live witness before the next keyframe. `None` while a break is open or before any keyframe.
    #[must_use]
    pub(crate) fn pane_chains(&self) -> Option<Vec<PaneRecording>> {
        if self.spine.break_open() {
            return None;
        }
        let oldest = self.oldest_replayable()?.seq;
        let kept: Vec<&RetainedKeyframe> = self
            .keyframes
            .iter()
            .filter(|kf| kf.seq >= oldest)
            .collect();
        let mut chains = Vec::with_capacity(kept.len());
        for (i, kf) in kept.iter().enumerate() {
            let next = kept.get(i + 1).map(|kf| kf.seq);
            let events: Vec<_> = self
                .log
                .live_after(kf.seq)
                .take_while(|ev| next.is_none_or(|next| ev.seq < next))
                .collect();
            let earliest_slice = events
                .iter()
                .filter_map(|ev| match ev.op {
                    Op::RawIn(id) => self.blob(id).map(|slice| slice.clock),
                    Op::Host(id) => self.host_op(id).and_then(|(_, clock)| clock),
                    _ => None,
                })
                .min_by_key(|clock| clock.monotonic);
            let origin_mono = match (
                earliest_slice.map(|c| c.monotonic),
                kf.carry.earliest_instant(),
            ) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => ClockReading::now().monotonic,
            };
            let origin = ClockReading {
                monotonic: origin_mono,
                wall_ms: earliest_slice.and_then(|c| c.wall_ms),
            };
            let env = self.chain_env(kf);
            let carry = aterm_core::terminal::PaneStepCarry::from_replay_carry(
                &kf.carry,
                &kf.checkpoint,
                &env.config,
                env.security_policy.as_ref(),
                origin.monotonic,
            );
            let mut chain = PaneRecording::new(
                origin,
                PaneState::with_carry((*kf.checkpoint).clone(), carry, env),
            );
            for ev in events {
                match ev.op {
                    Op::RawIn(id) => {
                        let slice = self.blob(id)?;
                        let offset = slice
                            .clock
                            .monotonic
                            .saturating_duration_since(origin.monotonic);
                        let clock = PaneClock {
                            offset_ns: u64::try_from(offset.as_nanos()).unwrap_or(u64::MAX),
                            wall_ms: slice.clock.wall_ms,
                        };
                        chain.bytes(
                            Arc::from(slice.bytes()),
                            Some(clock),
                            slice.reply.as_deref().unwrap_or_default(),
                        );
                    }
                    Op::Resize { rows, cols } => chain.resize(rows, cols),
                    Op::CellPx { width, height } => chain.cell_px(width, height),
                    Op::Host(id) => {
                        let (op, clock) = self.host_op(id)?;
                        let pane_clock = clock.map(|clock| {
                            let offset =
                                clock.monotonic.saturating_duration_since(origin.monotonic);
                            PaneClock {
                                offset_ns: u64::try_from(offset.as_nanos()).unwrap_or(u64::MAX),
                                wall_ms: clock.wall_ms,
                            }
                        });
                        chain.host(op, pane_clock);
                    }
                    Op::Witness(id) => {
                        if let Some(w) = self.witness_of(id) {
                            chain.witness(w.screen);
                        }
                    }
                    _ => {}
                }
            }
            chains.push(chain);
        }
        Some(chains)
    }

    /// [`Self::replay_plan`], folded in place.
    #[cfg(test)]
    pub(crate) fn replay_at(&self, at: Option<Ticks>) -> Option<Terminal> {
        self.replay_plan(at).map(ReplayPlan::run)
    }
}

/// One instant a Rewind view steps to ([`TemporalRecorder::rewind_instants`]): its tick, and whether a slice on it started a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RewindInstant {
    pub(crate) at: Ticks,
    pub(crate) command: bool,
}

/// The ticks a Rewind track spans and its hatched stretches ([`TemporalRecorder::rewind_track`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Track {
    pub(crate) from: Ticks,
    pub(crate) to: Ticks,
    pub(crate) spans: Vec<(Ticks, Ticks, Span)>,
}

/// Why a stretch of the track is hatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    /// Nothing kept rebuilds it: its keyframe or bytes are gone, or the spine broke.
    Lost,
    /// A live witness in it did not match its replay.
    Differs,
}

/// Why [`TemporalRecorder::export`] has nothing to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoExport {
    /// Output was dropped that no keyframe follows yet; the next slice that keyframes closes it.
    Broken,
    /// No kept keyframe has every slice after it.
    NothingReplayable,
    /// A slice after the oldest replayable keyframe has lost its bytes.
    BytesGone,
}

/// The recording `rewind export` writes ([`TemporalRecorder::export`]): a keyframe and what followed it, each at its microseconds after it.
pub(crate) struct Export {
    pub(crate) base: Arc<TerminalCheckpoint>,
    pub(crate) events: Vec<(u64, Exported)>,
    /// Slices and resizes the spine lost, as `temporal status` counts them.
    pub(crate) dropped: (u64, u64),
}

/// One event of an [`Export`].
pub(crate) enum Exported {
    Output(InputSlice),
    Resize {
        rows: u16,
        cols: u16,
    },
    /// A keyframe of the live engine, which stands in for the slice it followed (the spine keeps no bytes for that slice).
    Screen(Arc<TerminalCheckpoint>),
}

struct RetainedKeyframe {
    #[cfg(test)]
    id: KeyframeId,
    seq: Seq,
    ts: Ticks,
    first_blob: u64,
    first_host_op: u64,
    cut: Option<Ticks>,
    /// Whether a replay minted it (a rekey), so the slices before it are all on the spine.
    replayed: bool,
    /// Whether this keyframe came from a live in-band resync ([`TemporalMsg::LiveKeyframe`]) and may coalesce with an immediately following one when no chain steps fell between them.
    coalesceable: bool,
    /// Whether the slice a live keyframe stands in for started a command ([`SliceMarks::command`]).
    command: bool,
    /// Whether unshared small slices in this completed chain were packed into slabs.
    packed: bool,
    checkpoint: Arc<TerminalCheckpoint>,
    /// The in-process transient state no checkpoint carries ([`ReplayCarry`]).
    carry: Arc<ReplayCarry>,
    /// The config its chain replays under ([`TemporalRecorder::set_replay_config`]).
    config: Option<Arc<TerminalConfig>>,
}

impl RetainedKeyframe {
    fn charge(&self) -> usize {
        charge_of(&self.checkpoint).saturating_add(self.carry.retained_image_bytes())
    }
}

impl Default for TemporalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// The writer thread's step for one message a reader sent in recording generation `generation`.
fn write_queued(recorder: &Mutex<TemporalRecorder>, generation: u64, msg: TemporalMsg) {
    let lock = || recorder.lock().unwrap_or_else(PoisonError::into_inner);
    let due = lock().apply_in(generation, msg);
    if let Some(due) = due {
        let minted = due.mint();
        lock().rekeyed(minted);
    }
}

impl Drop for TemporalRecorder {
    fn drop(&mut self) {
        if let Some(share) = self.share.as_mut() {
            share.move_to(0);
        }
    }
}

/// The default [`RewindPool`] limit (`rewind_total_mb`).
pub(crate) const DEFAULT_TOTAL_BYTES: usize = 128 * 1024 * 1024;

/// Every recording's charge against one limit, its queued output included, shed from the panes focused longest ago first.
pub(crate) struct RewindPool {
    charged: AtomicUsize,
    queued: AtomicUsize,
    unrelieved: AtomicUsize,
    limit: AtomicUsize,
    focus: AtomicU64,
    front: AtomicU64,
    members: Mutex<Vec<Member>>,
}

/// One recording in a pool, and when its pane was last focused.
struct Member {
    session: u64,
    recorder: Weak<Mutex<TemporalRecorder>>,
    focused: u64,
}

/// What a recorder has charged its pool.
struct Share {
    pool: Arc<RewindPool>,
    charged: usize,
}

impl Share {
    fn move_to(&mut self, now: usize) {
        if now >= self.charged {
            self.pool
                .charged
                .fetch_add(now - self.charged, Ordering::AcqRel);
        } else {
            let freed = self.charged - now;
            self.pool.charged.fetch_sub(freed, Ordering::AcqRel);
            self.pool.note_freed(freed);
        }
        self.charged = now;
    }
}

/// [`RewindPool::focus`]'s front pane before any is focused, or after a pane joins.
const NO_FRONT: u64 = u64::MAX;

/// The process's pool, which the shared writer relieves after each message.
pub(crate) fn pool() -> &'static Arc<RewindPool> {
    static POOL: OnceLock<Arc<RewindPool>> = OnceLock::new();
    POOL.get_or_init(|| RewindPool::new(DEFAULT_TOTAL_BYTES))
}

impl RewindPool {
    /// An empty pool holding at most `limit` bytes.
    #[must_use]
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            charged: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            unrelieved: AtomicUsize::new(0),
            limit: AtomicUsize::new(limit.max(1)),
            focus: AtomicU64::new(0),
            front: AtomicU64::new(NO_FRONT),
            members: Mutex::new(Vec::new()),
        })
    }

    fn note_freed(&self, bytes: usize) {
        if bytes > 0 {
            self.unrelieved.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    fn relief_due(&self) -> bool {
        self.unrelieved.load(Ordering::Relaxed) >= IDLE_RELIEF_FLOOR_BYTES
    }

    fn relieve_allocator_if_due(&self) {
        if self.relief_due() {
            self.unrelieved.store(0, Ordering::Relaxed);
            aterm_uds::process::release_free_memory();
        }
    }

    fn relieve_allocator_mid_flood(&self) {
        if self.unrelieved.load(Ordering::Relaxed) >= POOL_RELIEF_MID_FLOOD_BYTES {
            self.unrelieved.store(0, Ordering::Relaxed);
            aterm_uds::process::release_free_memory();
        }
    }

    /// Hold at most `limit` bytes from the next message on.
    pub(crate) fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Release);
    }

    /// The bytes every member charges and every queue onto it holds unwritten.
    #[must_use]
    pub(crate) fn used(&self) -> usize {
        self.charged
            .load(Ordering::Acquire)
            .saturating_add(self.queued())
    }

    /// The bytes every queue onto this pool holds unwritten ([`open`]).
    #[must_use]
    fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    /// The most bytes the pool holds once relieved.
    #[must_use]
    pub(crate) fn limit(&self) -> usize {
        self.limit.load(Ordering::Acquire)
    }

    /// Charge `recorder`, session `session`'s, to this pool as the pane focused most recently.
    pub(crate) fn join(self: &Arc<Self>, session: u64, recorder: &Arc<Mutex<TemporalRecorder>>) {
        {
            let mut rec = recorder.lock().unwrap_or_else(PoisonError::into_inner);
            if rec.share.is_some() {
                return;
            }
            rec.share = Some(Share {
                pool: self.clone(),
                charged: 0,
            });
            rec.settle();
        }
        let focused = self.focus.fetch_add(1, Ordering::AcqRel) + 1;
        self.front.store(NO_FRONT, Ordering::Release);
        let mut members = self.members.lock().unwrap_or_else(PoisonError::into_inner);
        members.retain(|m| m.recorder.strong_count() > 0);
        members.push(Member {
            session,
            recorder: Arc::downgrade(recorder),
            focused,
        });
    }

    /// Mark `session`'s pane as the one focused most recently (a no-op while it stays the front pane).
    pub(crate) fn focus(&self, session: u64) {
        if self.front.swap(session, Ordering::AcqRel) == session {
            return;
        }
        let focused = self.focus.fetch_add(1, Ordering::AcqRel) + 1;
        let mut members = self.members.lock().unwrap_or_else(PoisonError::into_inner);
        for m in members.iter_mut().filter(|m| m.session == session) {
            m.focused = focused;
        }
    }

    /// Once over the limit, shed to fifteen sixteenths of it: first the oldest chains of the panes focused longest ago, then whole histories of every pane but the one focused last.
    pub(crate) fn relieve(&self) {
        let limit = self.limit();
        if self.used() <= limit {
            return;
        }
        let target = limit - limit / 16;
        let recorders: Vec<Arc<Mutex<TemporalRecorder>>> = {
            let mut members = self.members.lock().unwrap_or_else(PoisonError::into_inner);
            members.retain(|m| m.recorder.strong_count() > 0);
            members.sort_by_key(|m| m.focused);
            members
                .iter()
                .filter_map(|m| m.recorder.upgrade())
                .collect()
        };
        let over = || self.used() > target;
        for rec in &recorders {
            if !over() {
                break;
            }
            rec.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .shed_while(&over);
        }
        if over()
            && let Some((_, older)) = recorders.split_last()
        {
            for rec in older {
                if !over() {
                    break;
                }
                rec.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .forget_for_pool();
            }
        }
        self.relieve_allocator_mid_flood();
    }
}

/// A replay lifted out of the recorder ([`TemporalRecorder::replay_plan`]).
pub(crate) struct ReplayPlan {
    base: Arc<TerminalCheckpoint>,
    carry: Arc<ReplayCarry>,
    base_seq: Seq,
    base_ts: Ticks,
    steps: Vec<Placed>,
    policy: ResizePolicy,
    host_resyncs: bool,
    /// Whether the base keyframe was taken from the live engine, so past the oldest chain it stands in for a slice the spine keeps no bytes of.
    live_base: bool,
    config: Option<Arc<TerminalConfig>>,
}

/// One step of a plan where it sits on the spine.
struct Placed {
    seq: Seq,
    ts: Ticks,
    step: Step,
}

enum Step {
    RawIn(InputSlice),
    Resize { rows: u16, cols: u16 },
    CellPx { width: u16, height: u16 },
    Host(HostOp, Option<ClockReading>),
    Witness(WitnessId, Arc<TerminalCheckpoint>),
}

/// One slice or resize a view fold applied ([`ReplayPlan::fold_onto`]).
pub(crate) struct Passed<'a> {
    pub(crate) seq: Seq,
    pub(crate) ts: Ticks,
    /// The slice's bytes, or `None` for a resize.
    pub(crate) bytes: Option<&'a [u8]>,
    pub(crate) resize: Option<(u16, u16)>,
}

impl ReplayPlan {
    /// Fold the plan from its base keyframe through a fresh [`ReplayEngine`] configured with [`TemporalRecorder::set_resize_policy`], [`TemporalRecorder::set_host_resyncs_alt_screen`] and the base's [`TemporalRecorder::set_replay_config`], each slice at its recorded clock, judging each pending witness it passes by full equality with the replay's `checkpoint_visible()`.
    #[must_use]
    pub(crate) fn fold(self) -> Fold {
        #[cfg(test)]
        replay_hook::before_replay();
        let mut replay = self.configured(ReplayEngine::from_checkpoint(&self.base, self.policy));
        let mut judged = Vec::new();
        for Placed { step, .. } in self.steps {
            match step {
                #[cfg(test)]
                Step::RawIn(input) if replay_clock::live() => replay.process(input.bytes()),
                Step::RawIn(input) => replay.process_at(input.bytes(), input.clock),
                Step::Resize { rows, cols } => replay.resize(rows, cols),
                Step::CellPx { width, height } => replay.set_cell_pixel_size(width, height),
                Step::Host(op, clock) => replay.apply_host_op(op, clock),
                Step::Witness(id, live) => {
                    let replayed = replay.terminal().checkpoint_visible();
                    judged.push((id, replayed.as_ref() == Some(&*live)));
                }
            }
        }
        Fold {
            term: replay.into_terminal(),
            judged: Judged(judged),
        }
    }

    /// [`Self::fold`]'s engine, its verdicts dropped.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn run(self) -> Terminal {
        self.fold().term
    }

    /// The base keyframe's seq, which names its chain.
    pub(crate) const fn base_seq(&self) -> Seq {
        self.base_seq
    }

    /// The base keyframe's tick.
    pub(crate) const fn base_ts(&self) -> Ticks {
        self.base_ts
    }

    /// The seq of the last step the plan folds, or its base's.
    pub(crate) fn end_seq(&self) -> Seq {
        self.steps.last().map_or(self.base_seq, |p| p.seq)
    }

    /// The base restored onto an engine whose history keeps `store_lines` lines past its ring ([`ReplayEngine::from_checkpoint_bounded`]), so a fork of it copies in bounded time.
    pub(crate) fn restore_bounded(&self, store_lines: usize) -> ReplayEngine {
        self.configured(ReplayEngine::from_checkpoint_bounded(
            &self.base,
            self.policy,
            store_lines,
        ))
    }

    /// `replay` under the session's alternate-screen recovery, config and keyframe carry.
    fn configured(&self, replay: ReplayEngine) -> ReplayEngine {
        let replay = replay.with_host_resyncs_alt_screen(self.host_resyncs);
        let replay = match &self.config {
            Some(config) => replay.with_config(config.clone()),
            None => replay,
        };
        replay.with_replay_carry(&self.carry)
    }

    /// Fold the steps after `after` onto `replay` (the base restored, or a fork of it taken at `after`) while `passed`, called after each slice or resize, says to go on. Returns whether a pending witness it passed differed from the replay in more than a bounded history renumbers ([`unnumbered`]), and the seq of the last step it applied.
    pub(crate) fn fold_onto(
        &self,
        replay: &mut ReplayEngine,
        after: Seq,
        mut passed: impl FnMut(Passed<'_>, &ReplayEngine) -> bool,
    ) -> (bool, Seq) {
        let mut differs = false;
        let mut last = after;
        for placed in self.steps.iter().filter(|p| p.seq > after) {
            let (bytes, resize) = match &placed.step {
                Step::RawIn(input) => {
                    replay.process_at(input.bytes(), input.clock);
                    (Some(input.bytes()), None)
                }
                Step::Resize { rows, cols } => {
                    replay.resize(*rows, *cols);
                    (None, Some((*rows, *cols)))
                }
                Step::CellPx { width, height } => {
                    replay.set_cell_pixel_size(*width, *height);
                    last = placed.seq;
                    continue;
                }
                Step::Host(op, clock) => {
                    replay.apply_host_op(*op, *clock);
                    if !op.is_screen_cut() {
                        last = placed.seq;
                        continue;
                    }
                    (None, None)
                }
                Step::Witness(_, live) => {
                    differs |= replay
                        .terminal()
                        .checkpoint_visible()
                        .is_none_or(|seen| unnumbered(seen) != unnumbered((**live).clone()));
                    last = placed.seq;
                    continue;
                }
            };
            last = placed.seq;
            let go_on = passed(
                Passed {
                    seq: placed.seq,
                    ts: placed.ts,
                    bytes,
                    resize,
                },
                replay,
            );
            if !go_on {
                break;
            }
        }
        (differs, last)
    }

    /// Fold every step onto `replay` (the base restored), calling `before` ahead of each slice or resize with the engine as the steps before it left it, while it says to go on; whether every step was folded.
    pub(crate) fn fold_before(
        &self,
        replay: &mut ReplayEngine,
        mut before: impl FnMut(Passed<'_>, &ReplayEngine) -> bool,
    ) -> bool {
        for placed in &self.steps {
            let (bytes, resize) = match &placed.step {
                Step::RawIn(input) => (Some(input.bytes()), None),
                Step::Resize { rows, cols } => (None, Some((*rows, *cols))),
                Step::CellPx { width, height } => {
                    replay.set_cell_pixel_size(*width, *height);
                    continue;
                }
                Step::Host(op, clock) if !op.is_screen_cut() => {
                    replay.apply_host_op(*op, *clock);
                    continue;
                }
                Step::Host(..) => (None, None),
                Step::Witness(..) => continue,
            };
            let go_on = before(
                Passed {
                    seq: placed.seq,
                    ts: placed.ts,
                    bytes,
                    resize,
                },
                replay,
            );
            if !go_on {
                return false;
            }
            match &placed.step {
                Step::RawIn(input) => replay.process_at(input.bytes(), input.clock),
                Step::Resize { rows, cols } => replay.resize(*rows, *cols),
                Step::Host(op, clock) => replay.apply_host_op(*op, *clock),
                Step::CellPx { .. } | Step::Witness(..) => {}
            }
        }
        true
    }

    /// The base restored onto an engine that keeps its whole history, as [`Self::fold`] restores it, for a fold whose verdicts count.
    pub(crate) fn restore(&self) -> ReplayEngine {
        self.configured(ReplayEngine::from_checkpoint(&self.base, self.policy))
    }

    /// Fold the steps after `after` onto `replay` (from [`Self::restore`], or as an earlier call left it) until `budget` bytes of slices are applied, judging each pending witness it passes as [`Self::fold`] does, up to the next slice: the verdicts, the seq of the last step applied, and whether steps are left.
    pub(crate) fn judge_onto(
        &self,
        replay: &mut ReplayEngine,
        after: Seq,
        budget: usize,
    ) -> (Judged, Seq, bool) {
        let mut judged = Vec::new();
        let (mut last, mut folded) = (after, 0usize);
        for placed in self.steps.iter().filter(|p| p.seq > after) {
            if folded >= budget && matches!(placed.step, Step::RawIn(_)) {
                return (Judged(judged), last, true);
            }
            match &placed.step {
                #[cfg(test)]
                Step::RawIn(input) if replay_clock::live() => {
                    replay.process(input.bytes());
                    folded += input.len();
                }
                Step::RawIn(input) => {
                    replay.process_at(input.bytes(), input.clock);
                    folded += input.len();
                }
                Step::Resize { rows, cols } => replay.resize(*rows, *cols),
                Step::CellPx { width, height } => replay.set_cell_pixel_size(*width, *height),
                Step::Host(op, clock) => replay.apply_host_op(*op, *clock),
                Step::Witness(id, live) => {
                    let replayed = replay.terminal().checkpoint_visible();
                    judged.push((*id, replayed.as_ref() == Some(&**live)));
                }
            }
            last = placed.seq;
        }
        (Judged(judged), last, false)
    }

    /// Whether the base keyframe was taken from the live engine, so past the oldest chain it stands in for a slice the spine keeps no bytes of.
    pub(crate) const fn live_base(&self) -> bool {
        self.live_base
    }

    /// The seqs after which the plan's slices from its base first reach each multiple of `every` bytes: where a cache of its forks keeps them.
    pub(crate) fn fork_marks(&self, every: usize) -> Vec<Seq> {
        let (mut marks, mut folded, mut next) = (Vec::new(), 0usize, every.max(1));
        for placed in &self.steps {
            if let Step::RawIn(input) = &placed.step {
                folded = folded.saturating_add(input.len());
                if folded >= next {
                    marks.push(placed.seq);
                    next = (folded / every.max(1) + 1).saturating_mul(every.max(1));
                }
            }
        }
        marks
    }
}

/// `c` less what a bounded history numbers otherwise than the live engine (aterm-core spec `replay-bounded-restore`): both row counters, the shell's marks and blocks, which sit at absolute rows, and the absolute rows of the holder's anchors and seals.
fn unnumbered(mut c: TerminalCheckpoint) -> TerminalCheckpoint {
    c.absolute_row_counter = 0;
    c.alt_absolute_row_counter = 0;
    c.shell.current_mark = None;
    c.shell.command_marks.clear();
    c.shell.current_block = None;
    c.shell.output_blocks.clear();
    for anchor in &mut c.holder.anchors {
        anchor.row = 0;
    }
    for seal in c.holder.marks.iter_mut().filter_map(|m| m.seal.as_mut()) {
        seal.base = 0;
        for (row, _) in &mut seal.rows {
            *row = 0;
        }
    }
    c
}

/// A due re-keyframe ([`TemporalRecorder::apply`]).
#[must_use = "the budget holds once the re-keyframe is recorded (`TemporalRecorder::rekeyed`)"]
pub(crate) struct Rekey {
    plan: ReplayPlan,
    head: Seq,
    generation: u64,
}

impl Rekey {
    /// Replay to the spine's end, judging the witnesses on the way, and checkpoint the result, with the recorder unlocked.
    pub(crate) fn mint(self) -> Minted {
        let config = self.plan.config.clone();
        let Fold { term, judged } = self.plan.fold();
        let keyframe = if term.parser_is_ground() {
            keyframe_of(&term, REKEY_SCROLLBACK_LINES)
        } else {
            None
        };
        Minted {
            head: self.head,
            generation: self.generation,
            keyframe,
            config,
            judged,
        }
    }
}

/// What [`Rekey::mint`] produced, for [`TemporalRecorder::rekeyed`].
pub(crate) struct Minted {
    head: Seq,
    generation: u64,
    keyframe: Option<ReplayKeyframe>,
    /// The config the replay ran under, which the keyframe's chain keeps.
    config: Option<Arc<TerminalConfig>>,
    judged: Judged,
}

/// Test-only hook [`ReplayPlan::fold`] calls first, so a test can probe the recorder lock from inside the fold.
#[cfg(test)]
pub(crate) mod replay_hook {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
    }

    /// Clears the installed hook when dropped.
    pub(crate) struct Installed;

    impl Drop for Installed {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    /// Run `hook` at the start of every later replay on this thread, until the returned guard drops.
    pub(crate) fn install(hook: impl FnMut() + 'static) -> Installed {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        Installed
    }

    pub(super) fn before_replay() {
        HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

/// Test-only switch [`ReplayPlan::fold`] reads per slice: a replay on the live clock, the negative control of the recorded one.
#[cfg(test)]
pub(crate) mod replay_clock {
    use std::cell::Cell;

    thread_local! {
        static LIVE: Cell<bool> = const { Cell::new(false) };
    }

    /// Puts replays back on the recorded clock when dropped.
    pub(crate) struct Live;

    impl Drop for Live {
        fn drop(&mut self) {
            LIVE.with(|l| l.set(false));
        }
    }

    /// Fold every later replay on this thread with plain `process` (the live clock), until the returned guard drops.
    pub(crate) fn on_live_clock() -> Live {
        LIVE.with(|l| l.set(true));
        Live
    }

    pub(super) fn live() -> bool {
        LIVE.with(Cell::get)
    }
}

#[cfg(test)]
mod resync_probe {
    use std::cell::Cell;

    thread_local! {
        static PROJECTED: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn projected() {
        PROJECTED.with(|n| n.set(n.get() + 1));
    }

    /// Keyframes projected on this thread so far.
    pub(super) fn count() -> usize {
        PROJECTED.with(Cell::get)
    }
}

const REKEY_EVENTS: u64 = (aterm_buffer::MAX_LOG_EVENTS / 4) as u64;

const MIN_REKEY_BYTES: usize = 64 * 1024;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RekeyTrigger {
    Events,
    Pressure,
}

const REKEY_SCROLLBACK_LINES: usize = 2048;

/// Output since the last witness that makes the next slice at parser ground take one.
const WITNESS_BYTES: usize = 256 * 1024;

/// Time since the last witness after which the next slice at parser ground takes one.
const WITNESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

const MAX_KEYFRAMES: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_raw_in_reply_resize_on_one_spine() {
        let mut r = TemporalRecorder::new();
        r.record_raw_in(b"ls -la\n");
        r.record_reply();
        r.record_resize(30, 100);

        assert_eq!(r.total_events(), 3, "raw_in + reply + resize");
        assert_eq!(r.live_events(), 3);
        assert_eq!(r.dropped_events(), 0, "nothing evicted under capacity");
        let ops: Vec<Op> = r.log.live().map(|e| e.op.clone()).collect();
        assert_eq!(
            ops,
            [
                Op::RawIn(BlobId(0)),
                Op::Reply(BlobId(1)),
                Op::Resize {
                    rows: 30,
                    cols: 100
                }
            ]
        );
        assert_eq!(r.blob_bytes(BlobId(0)), Some(&b"ls -la\n"[..]));
        assert_eq!(
            r.blob_bytes(BlobId(1)),
            None,
            "a reply's bytes are not kept"
        );
        assert_eq!(r.used, b"ls -la\n".len(), "only the input is charged");
    }

    #[test]
    fn keyframe_is_recorded_and_counted() {
        let mut t = aterm_core::terminal::Terminal::new(6, 20);
        t.process(b"seed content\r\n");
        assert!(t.parser_is_ground());
        let cp = t.checkpoint();

        let mut r = TemporalRecorder::new();
        r.record_keyframe(cp);
        r.record_raw_in(b"more");

        assert_eq!(r.keyframe_count(), 1);
        assert_eq!(r.total_events(), 2, "Keyframe event + RawIn event");
    }

    #[test]
    fn replay_at_seek_matches_the_full_spine_scan() {
        let mut t = aterm_core::terminal::Terminal::new(6, 20);
        t.process(b"seed\r\n");
        let mut r = TemporalRecorder::new();
        r.record_keyframe(t.checkpoint());
        for i in 0..40u32 {
            r.record_raw_in(format!("line{i}\r\n").as_bytes());
            if i % 9 == 0 {
                r.record_resize(6, 20 + (i % 3) as u16);
                let mut t2 = aterm_core::terminal::Terminal::new(6, 20);
                t2.process(format!("kf{i}\r\n").as_bytes());
                r.record_keyframe(t2.checkpoint());
            }
        }
        assert!(
            r.keyframe_count() > 1,
            "REACH: the fixture must mint several keyframes"
        );

        let ticks: Vec<u64> = r.log.live().map(|e| e.ts.0).collect();
        assert!(
            ticks.windows(2).all(|w| w[1] >= w[0]),
            "spine ticks must be nondecreasing"
        );
        assert_eq!(
            r.latest_tick(),
            r.log.live().map(|e| e.ts).max().unwrap_or_default(),
            "latest_tick must equal the max fold it replaced"
        );

        for kf in &r.keyframes {
            let ev = r
                .log
                .live()
                .find(|e| e.seq == kf.seq)
                .expect("a retained keyframe is live on the spine");
            assert_eq!(
                ev.op,
                Op::Keyframe(kf.id),
                "seq {:?} is not this keyframe",
                kf.seq
            );
            assert_eq!(ev.ts, kf.ts, "retained ts disagrees with the spine");
        }

        let reference = |at: Ticks| -> Option<Vec<String>> {
            let mut base: Option<(Seq, KeyframeId)> = None;
            for ev in r.log.live() {
                if ev.ts > at {
                    continue;
                }
                if let Op::Keyframe(kid) = ev.op {
                    base = Some((ev.seq, kid));
                }
            }
            let (base_seq, base_kid) = base?;
            let cp = r
                .keyframes
                .iter()
                .find(|kf| kf.id == base_kid)
                .map(|kf| &kf.checkpoint)?;
            let mut term = Terminal::from_checkpoint(cp);
            for ev in r.log.live() {
                if ev.seq <= base_seq || ev.ts > at {
                    continue;
                }
                match ev.op {
                    Op::RawIn(id) => term.process(r.blob_bytes(id)?),
                    Op::Resize { rows, cols } => {
                        term.resize_with_policy(rows, cols, r.resize_policy);
                    }
                    _ => {}
                }
            }
            Some(screen_of(&term))
        };
        let observed =
            |at: Ticks| -> Option<Vec<String>> { r.replay_at(Some(at)).map(|t| screen_of(&t)) };

        let latest = r.latest_tick();
        let mut probes: Vec<Ticks> =
            vec![Ticks(0), Ticks(latest.0 / 2), latest, Ticks(latest.0 + 1)];
        probes.extend(r.log.live().map(|e| e.ts));
        probes.extend(r.log.live().map(|e| Ticks(e.ts.0.saturating_sub(1))));
        let mut reached = 0usize;
        for at in probes {
            let (a, b) = (observed(at), reference(at));
            assert_eq!(a, b, "replay_at({at:?}) diverged from the full-spine scan");
            if a.is_some() {
                reached += 1;
            }
        }
        assert!(
            reached > 0,
            "REACH: every probe was unreachable — the differential proved nothing"
        );
        assert_eq!(
            r.replay_at(None).map(|t| screen_of(&t)),
            reference(latest),
            "the default replay target must still be the latest recorded instant"
        );
    }

    fn screen_of(t: &Terminal) -> Vec<String> {
        (0..t.rows() as usize)
            .map(|r| crate::control::visible_row(t, r))
            .collect()
    }

    #[test]
    fn shared_burst_is_retained_not_copied_and_refunded_on_eviction() {
        let burst: Arc<[u8]> = Arc::from(&[b'y'; 64][..]);
        let mut r = TemporalRecorder::with_budget(1024);
        r.record_raw_in_shared(burst.clone());
        assert_eq!(Arc::strong_count(&burst), 2, "blob shares the caller's Arc");
        assert_eq!(r.used, 64);
        for _ in 0..64 {
            r.record_raw_in(&[b'x'; 64]);
        }
        assert_eq!(
            Arc::strong_count(&burst),
            1,
            "evicted blob released its ref"
        );
        assert!(
            r.used <= r.budget,
            "eviction refunded blob bytes: used={}",
            r.used
        );
    }

    #[test]
    fn budget_bounds_blobs_without_silent_event_loss_on_spine() {
        let mut r = TemporalRecorder::with_budget(256);
        for _ in 0..1000 {
            r.record_raw_in(&[b'x'; 64]);
            assert!(r.used <= r.budget, "used={} past the budget", r.used);
        }
        assert_eq!(r.total_events(), 1000);
        assert_eq!(r.live_events(), 1000, "the ring holds every event");
        assert_eq!(r.dropped_events(), 0);
        assert_eq!(r.used, 256);
        assert_eq!((r.blob_base(), r.blobs.len()), (996, 4));
    }

    #[test]
    fn replay_applies_resize_in_spine_order_after_cursor_addressed_write() {
        let mut r = TemporalRecorder::new();
        let blank = aterm_core::terminal::Terminal::new(4, 10);
        r.record_keyframe(blank.checkpoint());
        r.record_raw_in(b"\x1b[16GX");
        r.record_resize(4, 20);

        let term = r.replay_at(None).expect("replay");
        assert_eq!(term.cols(), 20, "the resize is applied");
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert_eq!(
            row0.find('X'),
            Some(9),
            "X at the pre-resize column: row0={row0:?}"
        );
    }

    #[test]
    fn replay_resizes_under_the_sessions_seam_policy() {
        let mut live = Terminal::new(6, 20);
        let mut r = TemporalRecorder::new();
        r.set_resize_policy(ResizePolicy::ConPty);
        r.record_keyframe(live.checkpoint());
        let lines: Vec<u8> = (0..12)
            .flat_map(|i| format!("L{i:02}\r\n").into_bytes())
            .collect();
        live.process(&lines);
        r.record_raw_in(&lines);

        let screen = screen_of(&live);
        live.resize_with_policy(10, 20, ResizePolicy::ConPty);
        r.record_resize(10, 20);
        let mut repaint = b"\x1b[H".to_vec();
        for (i, row) in screen.iter().enumerate() {
            if i > 0 {
                repaint.extend_from_slice(b"\r\n");
            }
            repaint.extend_from_slice(row.trim_end().as_bytes());
            repaint.extend_from_slice(b"\x1b[K");
        }
        repaint.extend_from_slice(&b"\r\n\x1b[K".repeat(4));
        repaint.extend_from_slice(b"\x1b[6;1H");
        live.process(&repaint);
        r.record_raw_in(&repaint);

        let history = |t: &Terminal| -> Vec<String> {
            (0..t.grid().scrollback_lines())
                .map(|i| {
                    t.grid()
                        .get_history_line(i)
                        .map(|l| l.to_string())
                        .unwrap_or_default()
                })
                .collect()
        };
        assert_eq!(live.grid().scrollback_lines(), 7, "precondition");
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(
            history(&replayed),
            history(&live),
            "the history the live engine kept"
        );
        assert_eq!(screen_of(&replayed), screen_of(&live));

        r.set_resize_policy(ResizePolicy::Native);
        let native = r.replay_at(None).expect("replay");
        assert_eq!(native.grid().scrollback_lines(), 3);
    }

    #[test]
    fn a_replay_takes_the_sessions_posture_over_its_keyframes() {
        let mut native = Terminal::new(6, 20);
        native.set_console_host_posture(ResizePolicy::Native);
        let keyframe = native.checkpoint();
        let mut r = TemporalRecorder::new();
        r.set_resize_policy(ResizePolicy::ConPty);
        r.record_keyframe(keyframe.clone());
        r.record_raw_in(b"\x1b[?9001h");
        let replayed = r.replay_at(None).expect("replay");
        let archiving = |t: &Terminal| {
            t.alt_archive_read(aterm_core::terminal::AltArchiveQuery::oldest(0, 0))
                .enabled
        };
        assert!(replayed.modes().win32_input_mode, "the ConPTY posture");
        assert!(!archiving(&replayed));

        let mut bare = Terminal::from_checkpoint(&keyframe);
        bare.process(b"\x1b[?9001h");
        assert!(!bare.modes().win32_input_mode);
        assert!(archiving(&bare));
    }

    #[test]
    fn replay_leaves_a_dead_alt_screen_under_the_sessions_host_recovery() {
        let mut live = Terminal::new(6, 20);
        live.set_host_resyncs_alt_screen(true);
        let mut r = TemporalRecorder::new();
        r.set_host_resyncs_alt_screen(true);
        r.record_keyframe(live.checkpoint());
        let mut feed = |live: &mut Terminal, bytes: &[u8]| {
            live.process(bytes);
            r.record_raw_in(bytes);
        };
        feed(
            &mut live,
            b"C:\\> less\r\n\x1b[?1049h\x1b[H\x1b[2Jpager 1\r\n:",
        );
        feed(
            &mut live,
            b"\r\n\x1b]133;A\x07C:\\Users\\me> \x1b]133;B\x07",
        );
        assert!(live.take_alt_screen_resync_request(), "precondition");
        assert!(live.is_alternate_screen(), "precondition");
        feed(&mut live, b"\x1b[?1049l");
        feed(&mut live, b"dir\r\nx\r\n\x1b]133;A\x07C:\\Users\\me> ");
        assert!(!live.is_alternate_screen(), "precondition");

        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));
        assert_eq!(
            (replayed.cursor().row, replayed.cursor().col),
            (live.cursor().row, live.cursor().col)
        );

        r.set_host_resyncs_alt_screen(false);
        let local = r.replay_at(None).expect("replay");
        assert_ne!(screen_of(&local), screen_of(&live));
    }

    #[test]
    fn blob_handle_resolves_after_eviction_shifts_the_deque() {
        let mut r = TemporalRecorder::with_budget(150);
        r.record_raw_in(&[b'a'; 64]);
        r.record_raw_in(&[b'b'; 64]);
        r.record_raw_in(&[b'c'; 64]);

        assert!(r.blob_bytes(BlobId(0)).is_none(), "evicted handle => None");
        assert_eq!(r.blob_bytes(BlobId(1)), Some(&[b'b'; 64][..]));
        assert_eq!(r.blob_bytes(BlobId(2)), Some(&[b'c'; 64][..]));
        assert!(r.blob_bytes(BlobId(3)).is_none(), "unissued handle => None");
    }

    #[test]
    fn rekeyframing_keeps_the_latest_reachable_past_the_budget() {
        let mut r = TemporalRecorder::with_budget(1024 * 1024);
        let blank = aterm_core::terminal::Terminal::new(6, 24);
        r.record_keyframe(blank.checkpoint());
        for i in 0..4096u32 {
            r.record_raw_in(format!("row {i:06}\r\n").as_bytes());
            r.record_raw_in(&[b'.'; 1000]);
            r.record_raw_in(b"\r\n");
        }
        r.record_raw_in(b"FINAL-SENTINEL-LINE\r\n");

        let term = r
            .replay_at(None)
            .expect("re-keyframing keeps the latest instant reachable past the budget");
        let mut found = false;
        for row in 0..i32::from(term.rows()) {
            if term
                .get_line_text(row, None)
                .unwrap_or_default()
                .contains("FINAL-SENTINEL-LINE")
            {
                found = true;
            }
        }
        assert!(
            found,
            "the recent sentinel is reconstructable after a 4x-budget flood"
        );
        assert!(
            (2..=MAX_KEYFRAMES).contains(&r.keyframe_count()),
            "re-keyframing minted fresh keyframes, at most {MAX_KEYFRAMES} kept: {}",
            r.keyframe_count()
        );
        assert!(r.used <= r.budget, "used={} past the budget", r.used);
    }

    fn assert_retention_holds(r: &TemporalRecorder) {
        assert!(
            r.used <= r.budget,
            "used={} past the budget {}",
            r.used,
            r.budget
        );
        assert_eq!(
            r.dropped_events(),
            r.total_events() - r.live_events() as u64
        );
        assert!(
            r.blobs.len() <= r.live_events(),
            "a payload outlived its event"
        );
        let floor = r.log.oldest_live().map_or(Seq(0), |e| e.seq);
        assert!(
            r.keyframes.iter().all(|kf| kf.seq >= floor),
            "a retained keyframe outlived its event"
        );
    }

    #[test]
    fn replay_survives_more_than_twice_the_ring_of_small_events() {
        const RING: usize = aterm_buffer::MAX_LOG_EVENTS;
        const PROBE: usize = 16_384;
        const {
            assert!(
                3 * RING * 8 < DEFAULT_BUDGET_BYTES / 4,
                "REACH: the byte trigger never fires"
            );
        }
        for events_trigger in [true, false] {
            let mut live = Terminal::new(6, 20);
            let mut r = TemporalRecorder::new();
            if !events_trigger {
                r.disabled_trigger = Some(RekeyTrigger::Events);
            }
            r.record_keyframe(live.checkpoint());
            let mut lost_at = None;
            for i in 0..3 * RING {
                let line = format!("{i:06}\r\n");
                live.process(line.as_bytes());
                r.record_raw_in(line.as_bytes());
                if i % PROBE == PROBE / 2 {
                    let Some(replayed) = r.replay_at(None) else {
                        lost_at = Some(i);
                        break;
                    };
                    assert_eq!(screen_of(&replayed), screen_of(&live), "after {i}");
                }
            }
            assert_retention_holds(&r);
            assert!(r.dropped_events() > 0, "REACH: the ring wrapped");
            if events_trigger {
                assert_eq!(lost_at, None, "replay lost");
                assert!(r.reachable_from().is_some());
            } else {
                assert!(
                    lost_at.is_some_and(|i| (RING - 1..RING - 1 + PROBE).contains(&i)),
                    "lost at the first probe past the seed's eviction: {lost_at:?}"
                );
                assert_eq!(r.keyframe_count(), 0, "the seed went with its event");
                assert_eq!(r.reachable_from(), None);
            }
        }
    }

    #[test]
    fn replay_survives_more_than_twice_the_ring_under_a_small_budget() {
        const RING: usize = aterm_buffer::MAX_LOG_EVENTS;
        let mut live = Terminal::new(6, 40);
        let mut r = TemporalRecorder::with_budget(1024 * 1024);
        r.record_keyframe(live.checkpoint());
        let pad = "x".repeat(31);
        for i in 0..2 * RING + 8192 {
            let line = format!("{i:06} {pad}\r\n");
            assert_eq!(line.len(), 40);
            live.process(line.as_bytes());
            r.record_raw_in(line.as_bytes());
            if i % 16_384 == 8191 {
                let replayed = r
                    .replay_at(None)
                    .unwrap_or_else(|| panic!("replay lost after event {i}"));
                assert_eq!(screen_of(&replayed), screen_of(&live), "after {i}");
                assert_retention_holds(&r);
            }
        }
        assert_retention_holds(&r);
        assert!(r.dropped_events() > 0, "REACH: the ring wrapped");
        assert!(
            r.blobs.len() < r.live_events(),
            "REACH: the budget evicted blobs of live events"
        );
        assert!(r.reachable_from().is_some());
    }

    #[test]
    fn budget_pressure_rekeys_before_the_newest_chain_is_evicted() {
        const COLS: u16 = 200;
        let line = |i: usize| format!("{i:06} {}\r\n", "y".repeat(usize::from(COLS) - 8));
        let seeded = || {
            let mut t = Terminal::new(24, COLS);
            t.set_scrollback_line_limit(Some(REKEY_SCROLLBACK_LINES));
            for i in 0..REKEY_SCROLLBACK_LINES + 24 {
                t.process(line(i).as_bytes());
            }
            t
        };
        let keyframe = seeded().checkpoint();
        let charge = keyframe.grid.len() + keyframe.alt_grid.as_ref().map_or(0, Vec::len);
        let budget = charge * 5 / 4;
        let interval = (budget / 4).max(MIN_REKEY_BYTES);
        assert!(
            budget - charge < interval,
            "REACH: the chain has less room than the byte interval (charge {charge})"
        );
        assert!(
            charge + MIN_REKEY_BYTES + 2 * line(0).len() < budget,
            "REACH: under pressure the chain still fits (charge {charge})"
        );
        for pressure in [true, false] {
            let mut live = seeded();
            let mut r = TemporalRecorder::with_budget(budget);
            if !pressure {
                r.disabled_trigger = Some(RekeyTrigger::Pressure);
            }
            r.record_keyframe(keyframe.clone());
            let mut lost_at = None;
            for i in 0..3 * budget / line(0).len() {
                let bytes = line(REKEY_SCROLLBACK_LINES + 24 + i);
                live.process(bytes.as_bytes());
                r.record_raw_in(bytes.as_bytes());
                if r.reachable_from().is_none() {
                    lost_at = Some(i);
                    break;
                }
            }
            assert!(r.used <= r.budget);
            if pressure {
                assert_eq!(lost_at, None, "the newest chain was evicted");
                assert!(r.next_keyframe > 4, "REACH: pressure re-keyframed");
                let replayed = r.replay_at(None).expect("replay");
                assert_eq!(screen_of(&replayed), screen_of(&live));
            } else {
                assert!(lost_at.is_some(), "the chain survived without pressure");
                assert!(r.replay_at(None).is_none());
            }
        }
    }

    #[test]
    fn the_budget_loses_the_oldest_window_first_and_says_where_replay_starts() {
        let blank = Terminal::new(4, 10).checkpoint();
        let c = blank.grid.len() + blank.alt_grid.as_ref().map_or(0, Vec::len);
        assert!(6 * c < MIN_REKEY_BYTES, "REACH: no re-keyframe is due");
        let oldest = |r: &TemporalRecorder| r.oldest_replayable().map(|kf| kf.id);
        let mut r = TemporalRecorder::with_budget(5 * c + c / 2);
        r.record_keyframe(blank.clone());
        r.record_raw_in(&vec![b'a'; c]);
        r.record_keyframe(blank.clone());
        r.record_raw_in(&vec![b'b'; c]);
        r.record_keyframe(blank.clone());
        assert_eq!(oldest(&r), Some(KeyframeId(0)), "everything fits");
        assert_eq!(r.reachable_from(), Some(r.keyframes[0].ts));

        r.record_raw_in(&vec![b'c'; c]);
        assert_eq!(r.keyframe_count(), 2, "the oldest keyframe went");
        assert_eq!(oldest(&r), Some(KeyframeId(1)), "and no chain lost a blob");
        assert_eq!(r.reachable_from(), Some(r.keyframes[0].ts));
        assert!(r.replay_at(None).is_some());
        assert!(r.used <= r.budget);

        r.record_raw_in(&vec![b'd'; 5 * c]);
        assert_eq!(
            r.keyframe_count(),
            0,
            "the last resort took the newest window"
        );
        assert_eq!(r.reachable_from(), None);
        assert!(r.replay_at(None).is_none());
        assert!(r.used <= r.budget);
    }

    /// Review fix #50 (R1): once the oldest keyframe is evicted, the track is Lost from the oldest tick the log still names up to the keyframe a view now replays from.
    #[test]
    fn the_track_is_lost_up_to_the_oldest_replayable_keyframe_once_the_first_is_evicted() {
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(Terminal::new(6, 20).checkpoint());
        let line = vec![b'x'; MIN_REKEY_BYTES / 8];
        let lost = (0..10_000).find_map(|_| {
            r.record_raw_in(&line);
            let track = r.rewind_track();
            track
                .spans
                .first()
                .copied()
                .filter(|(.., kind)| *kind == Span::Lost)
                .map(|span| (track, span))
        });
        let (track, (from, to, _)) = lost.expect("the first keyframe is evicted");
        assert_eq!(from, track.from);
        assert_eq!(Some(to), r.oldest_replayable().map(|kf| kf.ts));
        assert!(to > from, "a stretch, not a point");
        assert!(
            r.rewind_instants().first().is_some_and(|i| i.at >= to),
            "no instant before the stretch's end replays"
        );
    }

    #[test]
    fn a_rekeyframe_due_mid_sequence_waits_for_the_input_that_ends_it() {
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(Terminal::new(6, 20).checkpoint());
        let mut burst = vec![b'x'; MIN_REKEY_BYTES];
        burst.extend_from_slice(b"\x1b[1;3");
        r.record_raw_in(&burst);
        assert_eq!(r.keyframe_count(), 1, "deferred inside the CSI");
        r.record_reply();
        r.record_resize(6, 21);
        assert_eq!(r.keyframe_count(), 1, "still inside it");
        r.record_raw_in(b"1m");
        assert_eq!(r.keyframe_count(), 2, "minted once the CSI ended");
        assert_eq!(r.bytes_since_keyframe, 0);
    }

    fn alt_frame(rows: u16, cols: u16) -> Vec<u8> {
        let mut f = String::from("\x1b[?1049h\x1b[2J");
        for r in 0..rows {
            f.push_str(&format!(
                "\x1b[{row};1HL{r}\x1b[{row};{cols}HE",
                row = r + 1
            ));
        }
        f.push_str(&format!("\x1b[{};3H", rows - 2));
        f.into_bytes()
    }

    fn drain_named(rx: &TemporalRx, r: &mut TemporalRecorder) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(msg) = rx.try_recv() {
            out.push(match &msg {
                TemporalMsg::RawIn(_) => "in".to_string(),
                other => shape(other),
            });
            r.apply_inline(msg);
        }
        out
    }

    fn shape(msg: &TemporalMsg) -> String {
        match msg {
            TemporalMsg::RawIn(input) => format!("in:{}", String::from_utf8_lossy(input.bytes())),
            TemporalMsg::Reply => "reply".to_string(),
            TemporalMsg::Resize { rows, cols } => format!("r:{rows}x{cols}"),
            TemporalMsg::CellPx { width, height } => format!("px:{width}x{height}"),
            TemporalMsg::Host(op, _) => format!("host:{op:?}"),
            TemporalMsg::ResizesLost(n) => format!("lost:{n}"),
            TemporalMsg::Keyframe(_) => "keyframe".to_string(),
            TemporalMsg::LiveKeyframe(..) => "live-keyframe".to_string(),
            TemporalMsg::Witness(_) => "witness".to_string(),
        }
    }

    /// A clock `ms` past one epoch every test shares, so a slice's time decides no witness unless a test means it to.
    fn clock(ms: u64) -> ClockReading {
        static EPOCH: std::sync::OnceLock<ClockReading> = std::sync::OnceLock::new();
        let epoch = *EPOCH.get_or_init(ClockReading::now);
        ClockReading {
            monotonic: epoch.monotonic + std::time::Duration::from_millis(ms),
            wall_ms: Some(1_800_000_000_000 + ms),
        }
    }

    fn slice(live: &mut Terminal, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
        slice_at(live, mark, tx, bytes, clock(0));
    }

    fn slice_at(
        live: &mut Terminal,
        mark: &mut SpineMark,
        tx: &TemporalTx,
        bytes: &[u8],
        at: ClockReading,
    ) {
        let pre = SpinePre::take(live);
        live.process_at(bytes, at);
        mark.record(tx, pre, live, InputSlice::at(bytes, at));
    }

    /// A bounded view fold past a narrowing that rewraps every line reads an honest witness as the live screen, though the history it dropped renumbered the shell's marks after it, and still differs from a screen that was never shown; the whole fold judges the same.
    #[test]
    fn a_bounded_fold_reads_faithful_over_renumbered_shell_marks() {
        const ROWS: u16 = 24;
        for poisoned in [false, true] {
            let mut live = aterm_core::terminal::TerminalBuilder::new()
                .size(ROWS, 80)
                .tiered_scrollback_defaults()
                .build();
            live.set_scrollback_line_limit(None);
            let mut r = seeded(&live);
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            for chunk in 0..80 {
                let mut out = String::from("\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07");
                for n in 0..60 {
                    out.push_str(&format!(
                        "chunk {chunk:02} line {n:02} {}\r\n",
                        "x".repeat(60)
                    ));
                }
                out.push_str("\x1b]133;D;0\x07");
                slice(&mut live, &mut mark, &tx, out.as_bytes());
                drain(&rx, &mut r);
            }
            live.resize(ROWS, 60);
            slice(&mut live, &mut mark, &tx, b"\x1b]133;A\x07$ \x1b]133;B\x07");
            drain(&rx, &mut r);
            let seen = if poisoned {
                let mut stranger = Terminal::new(ROWS, 60);
                stranger.process(b"not what was on screen");
                stranger.checkpoint_visible()
            } else {
                live.checkpoint_visible()
            };
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen.expect("visible"))));
            let plan = r.replay_plan(None).expect("plan");
            let mut bounded = plan.restore_bounded(crate::rewind_worker::STORE_LINES);
            let (differs, _) = plan.fold_onto(&mut bounded, plan.base_seq(), |_, _| true);
            assert_eq!(differs, poisoned, "the bounded fold, poisoned={poisoned}");
            let matched: Vec<bool> = plan.fold().judged.0.iter().map(|&(_, m)| m).collect();
            assert_eq!(
                matched.split_last(),
                Some((&!poisoned, &vec![true; matched.len().saturating_sub(1)][..])),
                "the whole fold's verdicts, the reader's own witnesses first"
            );
        }
    }

    fn seeded(live: &Terminal) -> TemporalRecorder {
        let mut r = TemporalRecorder::new();
        r.record_keyframe(keyframe_of(live, usize::MAX).expect("seedable"));
        r.spine_watermark().seed(SpinePre::take(live));
        r
    }

    fn drain(rx: &TemporalRx, r: &mut TemporalRecorder) -> usize {
        let mut n = 0;
        while let Some(msg) = rx.try_recv() {
            r.apply_inline(msg);
            n += 1;
        }
        n
    }

    #[test]
    fn a_net_zero_flap_between_two_reads_reaches_the_spine_and_the_replay() {
        const ROWS: u16 = 6;
        const COLS: u16 = 20;
        let mut live = Terminal::new(ROWS, COLS);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let frame = alt_frame(ROWS, COLS);
        let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H";
        slice(&mut live, &mut mark, &tx, &frame);
        live.resize(ROWS, COLS - 1);
        live.resize(ROWS, COLS);
        slice(&mut live, &mut mark, &tx, diff);
        drain(&rx, &mut r);
        assert_eq!((live.rows(), live.cols()), (ROWS, COLS), "net zero");
        assert_eq!(
            live.row_text(1).unwrap_or_default().trim_end(),
            "L1",
            "the live screen lost its last column"
        );
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));
        assert_eq!(r.lost_resizes(), 0);

        let mut old = TemporalRecorder::new();
        old.record_keyframe(Terminal::new(ROWS, COLS).checkpoint());
        old.record_raw_in(&frame);
        old.record_raw_in(diff);
        let uncut = old.replay_at(None).expect("replay");
        assert_ne!(screen_of(&uncut), screen_of(&live), "the flap it missed");

        let mut live = Terminal::new(ROWS, COLS);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, &frame);
        let before = screen_of(&live);
        live.resize(ROWS - 1, COLS);
        live.resize(ROWS, COLS);
        assert_eq!(screen_of(&live), before, "the engine undid the quiet flap");
        slice(&mut live, &mut mark, &tx, diff);
        assert_eq!(
            drain_named(&rx, &mut r),
            ["in", "r:5x20", "r:6x20", "in"],
            "both halves reach the spine, before the output after them"
        );
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));
    }

    #[test]
    fn the_slices_of_one_batch_share_its_copy_in_engine_order() {
        const ROWS: u16 = 6;
        const COLS: u16 = 20;
        let mut live = Terminal::new(ROWS, COLS);
        let mut r = seeded(&live);
        let keyframe_charge = r.used;
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut bytes = alt_frame(ROWS, COLS);
        let a = bytes.len();
        bytes.extend_from_slice(b"\x1b[1;1HT1\x1b[K\x1b[4;3H");
        let b = bytes.len();
        bytes.extend_from_slice(b"\x1b[2;20HZ");
        let batch: Arc<[u8]> = Arc::from(bytes);
        let holds = [0..a, a..b, b..batch.len()];
        for (i, range) in holds.iter().enumerate() {
            match i {
                1 => live.resize(ROWS, COLS - 1),
                2 => live.resize(ROWS, COLS),
                _ => {}
            }
            let pre = SpinePre::take(&live);
            live.process_at(&batch[range.clone()], clock(0));
            mark.record(
                &tx,
                pre,
                &live,
                InputSlice::new(batch.clone(), range.clone(), clock(0)),
            );
        }
        assert_eq!(
            Arc::strong_count(&batch),
            1 + holds.len(),
            "each queued slice is a refcount on the batch"
        );
        let mut order = Vec::new();
        while let Some(msg) = rx.try_recv() {
            order.push(match &msg {
                TemporalMsg::RawIn(input) => {
                    assert!(Arc::ptr_eq(&input.batch, &batch), "a range of the batch");
                    format!("in:{:?}", input.range)
                }
                other => shape(other),
            });
            r.apply_inline(msg);
        }
        assert_eq!(
            order,
            [
                format!("in:{:?}", holds[0]),
                format!("r:{ROWS}x{}", COLS - 1),
                format!("in:{:?}", holds[1]),
                format!("r:{ROWS}x{COLS}"),
                format!("in:{:?}", holds[2]),
            ],
            "each slice after the resizes before it"
        );
        assert_eq!(
            Arc::strong_count(&batch),
            1 + holds.len(),
            "the recorder retains the slices, not copies"
        );
        assert_eq!(r.used - keyframe_charge, batch.len(), "charged once");
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));

        let mut late = TemporalRecorder::new();
        late.record_keyframe(Terminal::new(ROWS, COLS).checkpoint());
        late.record_raw_in_shared(batch.clone());
        late.record_resize(ROWS, COLS - 1);
        late.record_resize(ROWS, COLS);
        let uncut = late.replay_at(None).expect("replay");
        assert_ne!(
            screen_of(&uncut),
            screen_of(&live),
            "the resizes out of order"
        );
    }

    #[test]
    fn a_journal_overrun_is_counted_once_and_the_replay_ends_at_the_live_geometry() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(256);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"a\r\n");
        let n = aterm_core::terminal::RESIZE_JOURNAL_CAP as u64 + 5;
        for i in 0..n {
            live.resize(6, 20 + u16::try_from(i % 7).unwrap_or(0) + 1);
        }
        slice(&mut live, &mut mark, &tx, b"b\r\n");
        slice(&mut live, &mut mark, &tx, b"c\r\n");
        drain(&rx, &mut r);
        assert_eq!(r.lost_resizes(), 5, "counted once, not per slice");
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(
            (replayed.rows(), replayed.cols()),
            (live.rows(), live.cols())
        );
    }

    #[test]
    fn a_full_queue_never_records_input_ahead_of_its_resizes() {
        let mut live = Terminal::new(6, 20);
        let mut r = TemporalRecorder::new();
        let (tx, rx) = queue(3);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut seen = Vec::new();
        slice(&mut live, &mut mark, &tx, b"1");
        live.resize(5, 20);
        live.resize(6, 20);
        slice(&mut live, &mut mark, &tx, b"2");
        while let Some(msg) = rx.try_recv() {
            seen.push(shape(&msg));
            r.apply_inline(msg);
        }
        assert_eq!(r.dropped_input(), 1, "slice 2's input");
        slice(&mut live, &mut mark, &tx, b"3");
        while let Some(msg) = rx.try_recv() {
            seen.push(shape(&msg));
            r.apply_inline(msg);
        }
        assert_eq!(
            seen,
            ["r:6x20", "in:1", "r:5x20", "keyframe"],
            "slice 2's input was dropped, never queued before the grow"
        );
        let replayed = r.replay_at(None).expect("replay from the keyframe");
        assert_eq!(screen_of(&replayed), screen_of(&live));
        assert!(!r.spine_watermark().break_open());
    }

    #[test]
    fn a_flap_while_no_reader_was_attached_reaches_the_next_reader() {
        const ROWS: u16 = 6;
        const COLS: u16 = 20;
        let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H";
        for starts_over in [false, true] {
            let mut live = Terminal::new(ROWS, COLS);
            let mut r = seeded(&live);
            let (tx, rx) = queue(64);
            let mut first = SpineMark::attach(r.spine_watermark());
            slice(&mut live, &mut first, &tx, &alt_frame(ROWS, COLS));
            drop(first);
            live.resize(ROWS, COLS - 1);
            live.resize(ROWS, COLS);
            let mut next = if starts_over {
                SpineMark::default()
            } else {
                SpineMark::attach(r.spine_watermark())
            };
            slice(&mut live, &mut next, &tx, diff);
            drain(&rx, &mut r);
            let replayed = r.replay_at(None).expect("replay");
            assert_eq!(
                screen_of(&replayed) == screen_of(&live),
                !starts_over,
                "a reader that starts over misses the flap (starts over: {starts_over})"
            );
            assert_eq!(r.lost_resizes(), 0);
        }
    }

    #[test]
    fn an_overrun_while_no_reader_was_attached_is_counted() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(256);
        let mut first = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut first, &tx, b"a\r\n");
        drop(first);
        let n = aterm_core::terminal::RESIZE_JOURNAL_CAP as u64 + 3;
        for i in 0..n {
            live.resize(6, 21 + u16::try_from(i % 5).unwrap_or(0));
        }
        let mut next = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut next, &tx, b"b\r\n");
        drain(&rx, &mut r);
        assert_eq!(r.lost_resizes(), 3);
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(
            (replayed.rows(), replayed.cols()),
            (live.rows(), live.cols())
        );
    }

    #[test]
    fn pending_counts_the_resizes_the_spine_has_not_been_handed() {
        let mut live = Terminal::new(6, 20);
        let r = seeded(&live);
        let watermark = r.spine_watermark();
        let (tx, _rx) = queue(64);
        let mut mark = SpineMark::attach(watermark.clone());
        slice(&mut live, &mut mark, &tx, b"a");
        assert_eq!(watermark.pending(&live), 0);
        live.resize(5, 20);
        live.resize(6, 20);
        assert_eq!(
            watermark.pending(&live),
            2,
            "a net-zero flap, no output since"
        );
        slice(&mut live, &mut mark, &tx, b"b");
        assert_eq!(
            watermark.pending(&live),
            0,
            "the next hold handed both over"
        );
        assert_eq!(SpineWatermark::default().pending(&live), 2);
    }

    #[test]
    fn replay_degrades_to_none_when_needed_blobs_were_evicted() {
        let mut r = TemporalRecorder::with_budget(128);
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        for _ in 0..64 {
            r.record_raw_in(&[b'x'; 64]);
        }
        assert!(r.replay_at(None).is_none());
        assert_eq!(r.reachable_from(), None, "status says nothing replays");
    }

    fn tick_past(r: &TemporalRecorder, tick: Ticks) {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while r.now().0 <= tick.0 + 1 {
            assert!(Instant::now() < deadline, "the recorder clock stopped");
            std::hint::spin_loop();
        }
    }

    #[test]
    fn a_dropped_rawin_is_counted_and_unreachable_after_it() {
        for cap in [2, 64] {
            let dropped = cap == 2;
            let mut live = Terminal::new(6, 20);
            let mut r = seeded(&live);
            let (tx, rx) = queue(cap);
            let mut mark = SpineMark::attach(r.spine_watermark());
            slice(&mut live, &mut mark, &tx, b"one\r\n");
            slice(&mut live, &mut mark, &tx, b"two\r\n");
            let before = screen_of(&live);
            slice(&mut live, &mut mark, &tx, b"three\r\n");
            slice(&mut live, &mut mark, &tx, b"four\r\n");
            assert_eq!(drain(&rx, &mut r), if dropped { 2 } else { 4 });
            let last = r.latest_tick();
            assert_eq!(r.dropped_input(), u64::from(dropped));
            assert_eq!(r.spine_watermark().break_open(), dropped);
            assert_eq!(r.replay_at(None).is_none(), dropped, "the spine's end");
            assert_eq!(r.reachable_from().is_none(), dropped);
            let at_last = r.replay_at(Some(last)).expect("the last event replays");
            let expected = if dropped { before } else { screen_of(&live) };
            assert_eq!(screen_of(&at_last), expected);

            drop(mark);
            let mut mark = SpineMark::attach(r.spine_watermark());
            tick_past(&r, last);
            slice(&mut live, &mut mark, &tx, b"five\r\n");
            let queued = drain_named(&rx, &mut r);
            assert_eq!(queued, [if dropped { "keyframe" } else { "in" }]);
            assert!(!r.spine_watermark().break_open());
            let end = r.replay_at(None).expect("the spine's end replays again");
            assert_eq!(screen_of(&end), screen_of(&live));
            assert_eq!(
                r.replay_at(Some(Ticks(last.0 + 1))).is_none(),
                dropped,
                "a tick past the drop"
            );
            assert_eq!(
                r.reachable_from().is_some_and(|tick| tick > last),
                dropped,
                "replay starts past the break"
            );
        }
    }

    #[test]
    fn a_dropped_reply_is_counted_and_breaks_nothing() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(1);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"one\r\n");
        mark.reply(&tx);
        assert_eq!((r.dropped_replies(), r.dropped_input()), (1, 0));
        drain(&rx, &mut r);
        slice(&mut live, &mut mark, &tx, b"two\r\n");
        assert_eq!(drain_named(&rx, &mut r), ["in"]);
        let replayed = r.replay_at(None).expect("no break");
        assert_eq!(screen_of(&replayed), screen_of(&live));

        slice(&mut live, &mut mark, &tx, b"three\r\n");
        slice(&mut live, &mut mark, &tx, b"four\r\n");
        drain(&rx, &mut r);
        mark.reply(&tx);
        assert!(
            rx.try_recv().is_none(),
            "the break queues only its keyframe"
        );
        assert_eq!((r.dropped_replies(), r.dropped_input()), (2, 1));
    }

    #[test]
    fn a_break_waits_for_a_parser_a_keyframe_can_carry() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(1);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"one\r\n");
        slice(&mut live, &mut mark, &tx, b"two\x1bP$qm");
        drain(&rx, &mut r);
        slice(&mut live, &mut mark, &tx, b"x");
        assert_eq!(live.partial_sequence_uncarried(), Some("DcsPassthrough"));
        assert!(rx.try_recv().is_none(), "no keyframe carries the query");
        assert!(r.spine_watermark().break_open());
        slice(&mut live, &mut mark, &tx, b"\x1b\\three");
        assert_eq!(drain_named(&rx, &mut r), ["keyframe"]);
        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));
    }

    #[test]
    fn a_break_waits_out_a_sequence_a_keyframe_would_swallow() {
        let tail = b"/w==\x1b\\after";
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue(1);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"one\r\n");
        slice(
            &mut live,
            &mut mark,
            &tx,
            b"two\x1b_Ga=T,f=32,s=1,v=1,c=4,r=3,q=2;",
        );
        drain(&rx, &mut r);
        slice(&mut live, &mut mark, &tx, b"/wAA");
        assert_eq!(live.partial_sequence_swallowed(), Some("SosPmApcString"));
        assert!(rx.try_recv().is_none(), "no keyframe swallows the image");
        assert!(r.spine_watermark().break_open());
        let swallowing = ReplayPlan {
            base: Arc::new(
                live.checkpoint_continuing_bounded(REKEY_SCROLLBACK_LINES)
                    .expect("carried, as a string to swallow"),
            ),
            carry: Arc::new(ReplayCarry::default()),
            base_seq: Seq(0),
            base_ts: Ticks(0),
            steps: vec![Placed {
                seq: Seq(1),
                ts: Ticks(0),
                step: Step::RawIn(InputSlice::at(&tail[..], clock(0))),
            }],
            policy: ResizePolicy::Native,
            host_resyncs: false,
            live_base: true,
            config: None,
        };

        slice(&mut live, &mut mark, &tx, tail);
        assert_eq!(drain_named(&rx, &mut r), ["keyframe"]);
        let replayed = r.replay_at(None).expect("replay");
        let view = |t: &Terminal| (screen_of(t), t.cursor());
        assert_eq!(view(&replayed), view(&live));
        assert_ne!(view(&swallowing.run()), view(&live), "the image moved it");
    }

    #[test]
    fn a_break_projects_no_keyframe_for_a_writer_that_is_gone() {
        for gone in [false, true] {
            let mut live = Terminal::new(6, 20);
            let r = seeded(&live);
            let (tx, rx) = queue(1);
            let mut mark = SpineMark::attach(r.spine_watermark());
            slice(&mut live, &mut mark, &tx, b"one\r\n");
            slice(&mut live, &mut mark, &tx, b"two\r\n");
            assert!(r.spine_watermark().break_open());
            assert!(rx.try_recv().is_some(), "the writer takes the first slice");
            let rx = (!gone).then_some(rx);
            let before = resync_probe::count();
            for _ in 0..4 {
                slice(&mut live, &mut mark, &tx, b"x");
            }
            let projected = resync_probe::count() - before;
            assert_eq!(projected, usize::from(!gone), "gone: {gone}");
            if let Some(rx) = rx {
                assert_eq!(
                    rx.try_recv().as_ref().map(shape).as_deref(),
                    Some("keyframe")
                );
            }
        }
    }

    fn mark_times(t: &Terminal) -> Vec<[Option<u64>; 4]> {
        t.command_marks()
            .iter()
            .map(|m| {
                [
                    m.prompt_time_ms,
                    m.command_input_start_time_ms,
                    m.command_exec_start_time_ms,
                    m.command_end_time_ms,
                ]
            })
            .collect()
    }

    /// A mode-2026 frame left open past its timeout, bells 50 ms apart and OSC 133 marks replay at the recorded clock exactly as the live engine parsed them; on the live clock the frame is still open.
    #[test]
    fn a_replay_at_the_recorded_clock_is_the_live_engine() {
        const TIMEOUT_MS: u64 = 1000;
        let fixture: [(u64, &[u8]); 8] = [
            (0, b"\x1b]133;A\x07$ \x1b]133;B\x07"),
            (10, b"ls\r\n\x1b]133;C\x07"),
            (20, b"\x1b[?2026hframe"),
            (50, b"\x07"),
            (100, b"\x07"),
            (150, b"\x07"),
            (200, b"\x07"),
            (20 + TIMEOUT_MS + 500, b"after\r\n\x1b]133;D;0\x07"),
        ];
        let mut live = Terminal::new(6, 30);
        let mut r = TemporalRecorder::new();
        r.record_keyframe(live.checkpoint());
        for (ms, bytes) in fixture {
            live.process_at(bytes, clock(ms));
            r.apply_inline(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
        }
        assert!(
            !live.modes().synchronized_output,
            "precondition: the timeout ended the frame live"
        );
        assert_eq!(
            live.bell_total(),
            2,
            "precondition: the 100 ms bell floor held two"
        );
        let marks = mark_times(&live);
        assert_eq!(
            marks.last().and_then(|m| m[3]),
            clock(20 + TIMEOUT_MS + 500).wall_ms,
            "precondition: the mark took the slice's wall clock"
        );

        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
        assert_eq!(replayed.bell_total(), live.bell_total());
        assert_eq!(mark_times(&replayed), marks);

        let _live = replay_clock::on_live_clock();
        let plain = r.replay_at(None).expect("replay");
        assert!(
            plain.modes().synchronized_output,
            "on the live clock the frame never timed out"
        );
        assert_ne!(plain.checkpoint_visible(), live.checkpoint_visible());
    }

    /// Two live witnesses of a recorded session match its replay, and the rekey that judged them mints its keyframe; replayed on the live clock both diverge, the rekey mints none, and the next slice queues a live keyframe the replay then matches.
    #[test]
    fn live_witnesses_judge_the_replay_and_a_mismatch_takes_a_live_keyframe() {
        for live_clock in [false, true] {
            let mut live = Terminal::new(6, 30);
            let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
            r.record_keyframe(live.checkpoint());
            r.spine_watermark().seed(SpinePre::take(&live));
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            let _clock = live_clock.then(replay_clock::on_live_clock);
            let burst = vec![b'.'; MIN_REKEY_BYTES];
            let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
                tick_past(r, r.latest_tick());
                slice_at(live, &mut mark, &tx, bytes, clock(ms));
                drain_named(&rx, r)
            };
            assert_eq!(feed(&mut live, &mut r, b"\x1b[?2026hframe", 0), ["in"]);
            assert_eq!(feed(&mut live, &mut r, b"\r\none", 1500), ["in", "witness"]);
            assert!(!live.modes().synchronized_output, "precondition");
            assert_eq!(feed(&mut live, &mut r, b"\r\ntwo", 3000), ["in", "witness"]);
            assert_eq!(r.witnessed(), 0, "no fold has judged them yet");
            assert_eq!(
                feed(&mut live, &mut r, &burst, 3100),
                ["in"],
                "the burst is due a rekey"
            );
            assert_eq!(
                (r.witnessed(), r.diverged()),
                (2, if live_clock { 2 } else { 0 })
            );
            assert_eq!(
                r.keyframe_count(),
                if live_clock { 1 } else { 2 },
                "a chain that diverged mints no keyframe"
            );
            assert_eq!(r.spine_watermark().keyframe_wanted(), live_clock);
            let at_witness = r.witnesses[0].ts;
            assert_eq!(
                r.fidelity_at(at_witness),
                if live_clock {
                    Fidelity::Diverged
                } else {
                    Fidelity::Faithful
                }
            );
            let at_burst = r
                .log
                .live()
                .filter(|e| matches!(e.op, Op::RawIn(_)))
                .last()
                .map(|e| e.ts)
                .expect("the burst");
            assert_eq!(
                r.fidelity_at(at_burst),
                if live_clock {
                    Fidelity::Diverged
                } else {
                    Fidelity::Unwitnessed
                },
                "no witness covers the burst after the last one"
            );
            assert_eq!(
                feed(&mut live, &mut r, b"\r\nthree", 3200),
                [if live_clock { "live-keyframe" } else { "in" }]
            );
            assert!(!r.spine_watermark().keyframe_wanted());
            assert_eq!(r.keyframe_count(), 2);
            let replayed = r.replay_at(None).expect("replay");
            assert_eq!(screen_of(&replayed), screen_of(&live));
        }
    }

    /// An instant is faithful once the first witness after the last slice its replay folds, and every one before it in the chain, matched; diverged from a mismatch on; unwitnessed while that witness is pending, and with no keyframe.
    #[test]
    fn the_fidelity_of_an_instant_is_the_verdict_of_the_witnesses_that_cover_it() {
        assert_eq!(
            TemporalRecorder::new().fidelity_at(Ticks(0)),
            Fidelity::Unwitnessed,
            "no keyframe"
        );
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
            tick_past(r, r.latest_tick());
            slice_at(live, &mut mark, &tx, bytes, clock(ms));
            drain(&rx, r)
        };
        feed(&mut live, &mut r, b"a", 0);
        let before = r.latest_tick();
        feed(&mut live, &mut r, b"b", 1000);
        feed(&mut live, &mut r, b"c", 2000);
        assert_eq!(r.witnesses.len(), 2, "REACH: one witness a second");
        let [first, second] = [r.witnesses[0].ts, r.witnesses[1].ts];
        let at_c = r
            .log
            .live()
            .filter(|e| matches!(e.op, Op::RawIn(_)))
            .last()
            .map(|e| e.ts)
            .expect("slice c");
        assert!(first < at_c, "precondition: c follows the first witness");
        assert_eq!(
            r.fidelity_at(before),
            Fidelity::Unwitnessed,
            "nothing judged"
        );

        let fold = r.replay_plan(Some(first)).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!(
            (r.witnessed(), r.diverged()),
            (1, 0),
            "a fold judges what it passes"
        );
        assert_eq!(r.fidelity_at(before), Fidelity::Faithful);
        assert_eq!(r.fidelity_at(first), Fidelity::Faithful);
        assert_eq!(
            r.fidelity_at(Ticks(first.0 + 1)),
            Fidelity::Faithful,
            "a replay past the witness that folds no more slices is the screen it saw"
        );
        assert_eq!(
            r.fidelity_at(at_c),
            Fidelity::Unwitnessed,
            "its next witness is pending"
        );

        let poisoned = r.witnesses.back_mut().expect("the second witness");
        let WitnessState::Pending(live_cp) = &poisoned.state else {
            panic!("pending");
        };
        let mut wrong = (**live_cp).clone();
        wrong.cols += 1;
        poisoned.state = WitnessState::Pending(Arc::new(wrong));
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (2, 1));
        assert_eq!(
            r.fidelity_at(first),
            Fidelity::Faithful,
            "before the mismatch"
        );
        assert_eq!(r.fidelity_at(at_c), Fidelity::Diverged);
        assert_eq!(r.fidelity_at(second), Fidelity::Diverged);
        assert_eq!(r.fidelity_at(Ticks(second.0 + 1)), Fidelity::Diverged);
        assert!(r.spine_watermark().keyframe_wanted());
    }

    /// A witness and the slice after it on one tick, as when the writer drains a backlog: a replay to that tick folds the slice, so only the next witness covers it.
    #[test]
    fn a_slice_on_its_witness_tick_is_covered_by_the_next_witness() {
        for poisoned in [false, true] {
            let mut live = Terminal::new(6, 30);
            let mut r = seeded(&live);
            let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
                live.process_at(bytes, clock(ms));
                r.apply_inline(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
            };
            tick_past(&r, r.latest_tick());
            put(&mut live, &mut r, b"one", 0);
            let one = r.latest_tick();
            tick_past(&r, one);
            let tie = r.now();
            r.frozen_now = Some(tie);
            let seen = live.checkpoint_visible().expect("at ground");
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
            put(&mut live, &mut r, b"\r\ntwo", 10);
            r.frozen_now = None;
            assert_eq!(
                (r.witnesses[0].ts, r.latest_tick()),
                (tie, tie),
                "precondition: the witness and the slice after it share a tick"
            );
            let fold = r.replay_plan(Some(tie)).expect("replay").fold();
            r.judge(fold.judged);
            assert_eq!((r.witnessed(), r.diverged()), (1, 0));
            assert_eq!(r.fidelity_at(one), Fidelity::Faithful);
            assert_eq!(r.covering_witness(one), Some(tie));
            assert_eq!(
                r.fidelity_at(tie),
                Fidelity::Unwitnessed,
                "the witness on the tick saw the screen before its slice"
            );
            assert_eq!(
                r.covering_witness(tie),
                None,
                "no witness covers the tick yet"
            );

            tick_past(&r, tie);
            let mut seen = live.checkpoint_visible().expect("at ground");
            if poisoned {
                seen.cols += 1;
            }
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
            assert_eq!(
                r.fidelity_at(tie),
                Fidelity::Unwitnessed,
                "the covering witness is pending"
            );
            let to = r.covering_witness(tie);
            assert_eq!(
                to,
                Some(r.witnesses[1].ts),
                "a view at the tick folds to the next witness for its verdict"
            );
            let fold = r.replay_plan(to).expect("replay").fold();
            r.judge(fold.judged);
            assert_eq!((r.witnessed(), r.diverged()), (2, u64::from(poisoned)));
            assert_eq!(
                r.fidelity_at(tie),
                if poisoned {
                    Fidelity::Diverged
                } else {
                    Fidelity::Faithful
                },
                "poisoned: {poisoned}"
            );
        }
    }

    /// A witness covering a view's instant on the tick of the next keyframe is judged in the instant's own chain, not the keyframe's, so the view reaches a verdict instead of staying unchecked.
    #[test]
    fn a_covering_witness_on_the_next_keyframes_tick_is_judged_in_its_own_chain() {
        for poisoned in [false, true] {
            let mut live = Terminal::new(6, 30);
            let mut r = seeded(&live);
            tick_past(&r, r.latest_tick());
            live.process_at(b"one", clock(0));
            r.apply_inline(TemporalMsg::RawIn(InputSlice::at(b"one", clock(0))));
            let at = r.latest_tick();
            tick_past(&r, at);
            let tie = r.now();
            r.frozen_now = Some(tie);
            let mut seen = live.checkpoint_visible().expect("at ground");
            if poisoned {
                seen.cols += 1;
            }
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
            let next = keyframe_of(&live, usize::MAX).expect("at ground");
            r.apply_inline(TemporalMsg::LiveKeyframe(next, SliceMarks::default()));
            r.frozen_now = None;
            assert_eq!(
                (r.witnesses[0].ts, r.keyframes[1].ts),
                (tie, tie),
                "precondition: the covering witness shares the next keyframe's tick"
            );
            assert_eq!(r.fidelity_at(at), Fidelity::Unwitnessed);
            assert!(
                r.replay_plan(Some(tie))
                    .expect("replay")
                    .fold()
                    .judged
                    .0
                    .is_empty(),
                "negative control: a fold to the tick replays the next keyframe's chain"
            );
            let judge = r
                .covering_plan(at)
                .expect("the instant's chain folds to its witness");
            r.judge(judge.fold().judged);
            assert_eq!((r.witnessed(), r.diverged()), (1, u64::from(poisoned)));
            assert_eq!(
                r.fidelity_at(at),
                if poisoned {
                    Fidelity::Diverged
                } else {
                    Fidelity::Faithful
                },
                "poisoned: {poisoned}"
            );
            assert!(
                r.covering_plan(tie).is_none(),
                "no witness after the tie covers it yet"
            );
        }
    }

    /// A chain recorded under the session's config replays a soft reset (DECSTR) to that config's cursor, so its witness matches in a fold and in the pane task; with no config recorded the same chain diverges there.
    #[test]
    fn a_chain_replays_under_the_config_its_keyframe_was_recorded_with() {
        let config = Arc::new(TerminalConfig {
            cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
            ..TerminalConfig::default()
        });
        for recorded in [false, true] {
            let mut live = Terminal::new(6, 30);
            let _ = live.apply_config(&config);
            let mut r = TemporalRecorder::new();
            if recorded {
                r.set_replay_config(Some(config.clone()));
            }
            r.record_keyframe(live.checkpoint());
            r.spine_watermark().seed(SpinePre::take(&live));
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            slice_at(&mut live, &mut mark, &tx, b"\x1b[2 qa", clock(0));
            slice_at(&mut live, &mut mark, &tx, b"\x1b[!pb", clock(1000));
            drain(&rx, &mut r);
            assert_eq!(r.witnesses.len(), 1, "REACH: the soft reset is witnessed");
            let chains = r.pane_chains().expect("a chain");
            let check = chains[0].check(aterm_aos_pane::PaneCode::ThisBuild, "test");
            let fold = r.replay_plan(None).expect("replay").fold();
            r.judge(fold.judged);
            assert_eq!(r.diverged(), u64::from(!recorded), "recorded: {recorded}");
            assert_eq!(
                check.verdict == aterm_aos::contract::Verdict::Pass,
                recorded,
                "recorded: {recorded}: {:?}",
                check.verdict
            );
        }
    }

    /// Every chain the recorder keeps replays as the pane task, each slice's reply and each witness matched; a reply the reader did not record fails `RepliesMatch` at its slice, in each chain.
    #[test]
    fn every_kept_chain_replays_as_the_pane_task_with_its_replies() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::{PaneCode, PaneCounts, REPLIES_MATCH};

        for replies_recorded in [true, false] {
            let mut live = Terminal::new(6, 30);
            let mut r = seeded(&live);
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
                let pre = SpinePre::take(live);
                live.process_at(bytes, clock(ms));
                let (reports, response) = (live.take_reports(), live.take_response());
                let reply =
                    reply_of(response.as_deref(), reports.as_deref()).filter(|_| replies_recorded);
                mark.record(
                    &tx,
                    pre,
                    live,
                    InputSlice::at(bytes, clock(ms)).replying(reply),
                );
                drain(&rx, r);
            };
            feed(&mut live, &mut r, b"$ ", 0);
            feed(&mut live, &mut r, b"\x1b[c", 10);
            feed(&mut live, &mut r, b"ls\r\n", 1000);
            r.spine.keyframe_wanted.store(true, Ordering::Release);
            feed(&mut live, &mut r, b"a b c\r\n", 1100);
            feed(&mut live, &mut r, b"\x1b[6n$ ", 2200);
            feed(&mut live, &mut r, b"x", 3300);
            assert_eq!(
                (r.keyframe_count(), r.witnesses.len()),
                (2, 2),
                "REACH: a live keyframe starts a second chain, each witnessed once"
            );
            let checks: Vec<_> = r
                .pane_chains()
                .expect("chains")
                .iter()
                .map(|chain| chain.check(PaneCode::ThisBuild, "test"))
                .collect();
            let counts: Vec<_> = checks.iter().map(|c| c.counts).collect();
            assert_eq!(
                counts,
                [
                    PaneCounts {
                        bytes: 3,
                        resizes: 0,
                        witnesses: 1,
                        matched: 1
                    },
                    PaneCounts {
                        bytes: 2,
                        resizes: 0,
                        witnesses: 1,
                        matched: 1
                    }
                ]
            );
            let verdicts: Vec<_> = checks
                .iter()
                .map(|c| match &c.verdict {
                    Verdict::Pass => None,
                    Verdict::Fail { law, cursor, .. } => Some((*law, *cursor)),
                })
                .collect();
            if replies_recorded {
                assert_eq!(verdicts, [None, None]);
                assert_eq!(
                    checks.iter().map(|c| c.replies_matched).collect::<Vec<_>>(),
                    [3, 2]
                );
            } else {
                assert_eq!(
                    verdicts,
                    [Some((REPLIES_MATCH, 1)), Some((REPLIES_MATCH, 0))],
                    "the DA and CPR slices"
                );
            }
        }
    }

    /// A fold that judges a witness of an older chain refunds its grids from the budget only: the newest chain's byte count, which a rekey is due by, never held them.
    #[test]
    fn judging_an_older_chains_witness_leaves_the_newest_chains_bytes() {
        for live_keyframe in [false, true] {
            let mut live = Terminal::new(6, 30);
            let mut r = seeded(&live);
            let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8]| {
                live.process_at(bytes, clock(0));
                r.apply_inline(TemporalMsg::RawIn(InputSlice::at(bytes, clock(0))));
            };
            put(&mut live, &mut r, b"old chain\r\n");
            let seen = live.checkpoint_visible().expect("at ground");
            let charge = charge_of(&seen);
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
            let at = r.witnesses[0].ts;
            tick_past(&r, at);
            let next = keyframe_of(&live, usize::MAX).expect("at ground");
            r.apply_inline(if live_keyframe {
                TemporalMsg::LiveKeyframe(next, SliceMarks::default())
            } else {
                TemporalMsg::Keyframe(next)
            });
            let tail = vec![b'.'; 4 * charge];
            put(&mut live, &mut r, &tail);
            assert_eq!(r.keyframe_count(), 2, "precondition: no rekey");
            assert_eq!(r.bytes_since_keyframe, tail.len());
            let used = r.used;

            let fold = r.replay_plan(Some(at)).expect("replay").fold();
            r.judge(fold.judged);
            assert_eq!((r.witnessed(), r.diverged()), (1, 0));
            assert_eq!(r.used, used - charge, "the witness's grids are refunded");
            assert_eq!(
                r.bytes_since_keyframe,
                tail.len(),
                "live keyframe: {live_keyframe}"
            );
        }
    }

    /// The first slice at parser ground that brings the output since the last witness to 256 KiB takes one, inside the second; a slice off ground leaves it due.
    #[test]
    fn a_witness_is_due_after_256_kib_of_output() {
        let mut live = Terminal::new(6, 30);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms| {
            slice_at(live, &mut mark, &tx, bytes, clock(ms));
            drain_named(&rx, r)
        };
        assert_eq!(feed(&mut live, &mut r, b"a", 0), ["in"]);
        let short = vec![b'.'; WITNESS_BYTES - 2];
        assert_eq!(
            feed(&mut live, &mut r, &short, 10),
            ["in"],
            "one byte short"
        );
        assert_eq!(feed(&mut live, &mut r, b".", 20), ["in", "witness"]);

        let open = [&b"\x1b]2;"[..], &vec![b'.'; WITNESS_BYTES]].concat();
        assert_eq!(feed(&mut live, &mut r, &open, 30), ["in"], "off ground");
        assert_eq!(feed(&mut live, &mut r, b"\x07", 40), ["in", "witness"]);
    }

    /// A rekey mints its keyframe with the session's shell nonce, so the signed marks after it replay and the next witness matches.
    #[test]
    fn a_rekeyed_keyframe_carries_the_shell_nonce() {
        let nonce = [0x5Au8; 32];
        let mut live = Terminal::new(6, 40);
        live.authorize_shell_integration(nonce);
        live.set_require_shell_integration_nonce(true);
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        let put = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8]| {
            live.process_at(bytes, clock(0));
            r.apply_inline(TemporalMsg::RawIn(InputSlice::at(bytes, clock(0))));
        };
        put(&mut live, &mut r, &vec![b'.'; MIN_REKEY_BYTES]);
        assert_eq!(r.keyframe_count(), 2, "precondition: the burst rekeyed");
        assert!(
            r.keyframes
                .back()
                .is_some_and(|kf| kf.checkpoint.shell_integration_nonce.is_some())
        );
        put(&mut live, &mut r, signed_marks(nonce).as_bytes());
        assert_eq!(live.command_marks().len(), 1, "precondition");
        let seen = live.checkpoint_visible().expect("at ground");
        r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (1, 0));
    }

    /// The live keyframe a divergence asks for carries the shell nonce, so the signed marks after it replay and the next witness matches.
    #[test]
    fn a_live_keyframe_carries_the_shell_nonce() {
        let nonce = [0x5Au8; 32];
        let mut live = Terminal::new(6, 40);
        live.authorize_shell_integration(nonce);
        live.set_require_shell_integration_nonce(true);
        let mut r = TemporalRecorder::new();
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        r.spine.keyframe_wanted.store(true, Ordering::Release);
        slice_at(&mut live, &mut mark, &tx, b"x", clock(0));
        assert_eq!(drain_named(&rx, &mut r), ["live-keyframe"]);
        let signed = signed_marks(nonce);
        slice_at(&mut live, &mut mark, &tx, signed.as_bytes(), clock(10));
        slice_at(&mut live, &mut mark, &tx, b"more", clock(1100));
        assert_eq!(drain_named(&rx, &mut r), ["in", "in", "witness"]);
        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (1, 0));
    }

    fn signed_marks(nonce: [u8; 32]) -> String {
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        format!(
            "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07\
             out\r\n\x1b]133;D;0;id={hex}\x07"
        )
    }

    /// A replay keyframe carries the session's shell-integration nonce, so its replay accepts the shell's signed marks as the live engine did; a keyframe without it drops them.
    #[test]
    fn a_replay_keyframe_carries_the_nonce_its_shell_signs_marks_with() {
        let nonce = [0x5Au8; 32];
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let mut live = Terminal::new(6, 40);
        live.authorize_shell_integration(nonce);
        live.set_require_shell_integration_nonce(true);
        let signed = format!(
            "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07ls\r\n\x1b]133;C;id={hex}\x07\
             out\r\n\x1b]133;D;0;id={hex}\x07"
        );
        let carried = keyframe_of(&live, usize::MAX).expect("at ground");
        assert!(carried.shell_integration_nonce.is_some());
        let bare = live.checkpoint();
        live.process_at(signed.as_bytes(), clock(0));
        assert_eq!(
            live.command_marks().len(),
            1,
            "precondition: the mark was accepted"
        );
        for (keyframe, accepted) in [(carried, true), (bare.into(), false)] {
            let mut r = TemporalRecorder::new();
            r.record_keyframe(keyframe);
            r.apply_inline(TemporalMsg::RawIn(InputSlice::at(
                signed.as_bytes(),
                clock(0),
            )));
            let replayed = r.replay_at(None).expect("replay");
            assert_eq!(
                replayed.command_marks().len() == 1,
                accepted,
                "nonce carried: {accepted}"
            );
            assert_eq!(
                replayed.checkpoint_visible() == live.checkpoint_visible(),
                accepted
            );
        }
    }

    #[test]
    fn temporal_replay_runs_with_the_recorder_unlocked() {
        use std::cell::Cell;
        use std::rc::Rc;
        let burst = || TemporalMsg::RawIn(InputSlice::at(&[b'x'; MIN_REKEY_BYTES][..], clock(0)));
        let recorder = Arc::new(Mutex::new(TemporalRecorder::with_budget(
            4 * MIN_REKEY_BYTES,
        )));
        recorder
            .lock()
            .unwrap()
            .record_keyframe(Terminal::new(6, 20).checkpoint());
        let held = Rc::new(Cell::new(None));
        let _hook = replay_hook::install({
            let (recorder, held) = (recorder.clone(), held.clone());
            move || held.set(Some(recorder.try_lock().is_err()))
        });
        write_queued(&recorder, 0, burst());
        assert_eq!(held.take(), Some(false), "folded with the recorder held");
        let mut rec = recorder.lock().unwrap();
        assert_eq!(rec.keyframe_count(), 2, "the re-keyframe was recorded");

        let _ = rec.replay_plan(None).map(ReplayPlan::run);
        assert_eq!(held.take(), Some(true), "the probe sees a held recorder");

        let due = rec.apply(burst()).expect("due by bytes");
        let minted = due.mint();
        let _ = rec.apply(TemporalMsg::Reply);
        rec.rekeyed(minted);
        assert_eq!(rec.keyframe_count(), 2, "the spine moved: discarded");
    }

    /// A replay keyframe carries the live engine's open mode-2026 frame start, drained response rate-limit buckets, alternate-screen leave notes, Kitty ids, host alternate-scroll default and pending restore lineage across initial keyframes, live resyncs and rekeys; a bare checkpoint without the carry diverges on each.
    #[test]
    fn a_replay_keyframe_carries_the_live_transient_state_across_chains_and_rekeys() {
        let judge_after = |keyframe: ReplayKeyframe,
                           live: &mut Terminal,
                           steps: &[(&[u8], u64)],
                           rekey_before: bool|
         -> (u64, u64) {
            let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
            r.record_keyframe(keyframe);
            if rekey_before {
                let burst = vec![b'.'; MIN_REKEY_BYTES];
                live.process_at(&burst, clock(0));
                r.apply_inline(TemporalMsg::RawIn(InputSlice::at(&burst, clock(0))));
                assert_eq!(r.keyframe_count(), 2, "precondition: burst rekeyed");
            }
            for &(bytes, ms) in steps {
                live.process_at(bytes, clock(ms));
                r.apply_inline(TemporalMsg::RawIn(InputSlice::at(bytes, clock(ms))));
            }
            let seen = live.checkpoint_visible().expect("at ground");
            r.apply_inline(TemporalMsg::Witness(Arc::new(seen)));
            let fold = r.replay_plan(None).expect("replay").fold();
            r.judge(fold.judged);
            (r.witnessed(), r.diverged())
        };

        // 1. Open mode-2026 synchronized-output frame whose 1 s window stays open at +500 ms.
        for carried in [true, false] {
            let mut live = Terminal::new(6, 30);
            live.process_at(b"\x1b[?2026hframe", clock(3_600_000));
            let kf = if carried {
                keyframe_of(&live, usize::MAX).expect("at ground")
            } else {
                live.checkpoint().into()
            };
            assert_eq!(
                judge_after(kf, &mut live, &[(b" still-open", 3_600_500)], false),
                (1, u64::from(!carried)),
                "open sync frame, carried: {carried}"
            );
        }

        // Also across a SpineMark live-keyframe resync while mode 2026 is open (both open and expired).
        {
            let mut live = Terminal::new(6, 30);
            let mut r = TemporalRecorder::new();
            r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
            r.spine_watermark().seed(SpinePre::take(&live));
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());
            live.process_at(b"\x1b[?2026hopen", clock(3_600_000));
            r.spine.keyframe_wanted.store(true, Ordering::Release);
            slice_at(&mut live, &mut mark, &tx, b"!", clock(3_600_100));
            assert_eq!(drain_named(&rx, &mut r), ["live-keyframe"]);
            slice_at(&mut live, &mut mark, &tx, b"still", clock(3_600_500));
            assert_eq!(drain_named(&rx, &mut r), ["in"]);
            assert!(live.modes().synchronized_output(), "still open at +500 ms");
            r.apply_inline(TemporalMsg::Witness(Arc::new(
                live.checkpoint_visible().expect("at ground"),
            )));
            slice_at(&mut live, &mut mark, &tx, b"expired", clock(3_601_050));
            assert_eq!(drain_named(&rx, &mut r), ["in"]);
            assert!(
                !live.modes().synchronized_output(),
                "expired 1050 ms past open (950 ms past live-keyframe)"
            );
            r.apply_inline(TemporalMsg::Witness(Arc::new(
                live.checkpoint_visible().expect("at ground"),
            )));
            let fold = r.replay_plan(None).expect("replay").fold();
            r.judge(fold.judged);
            assert_eq!(
                (r.witnessed(), r.diverged()),
                (2, 0),
                "live keyframe inside mode 2026 preserves sync_start"
            );
        }

        // 2. Drained response rate-limit bucket (both PolicyEngine standard profile and fallback limiter), including across a Rekey.
        for (with_policy, rekey, carried) in [
            (true, false, true),
            (true, false, false),
            (true, true, true),
            (false, false, true),
            (false, false, false),
        ] {
            let mut live = Terminal::new(6, 30);
            if with_policy {
                live.apply_policy_engine(aterm_policy::engine::PolicyEngine::new(
                    aterm_policy::profiles::standard(),
                ));
            }
            let mut drain_seq = String::from("\x1b]11;#000000\x07");
            drain_seq.push_str(&"\x1b[5n".repeat(24_592));
            live.process_at(drain_seq.as_bytes(), clock(0));
            let _ = live.take_response();
            let mut probe = "\x1b[5n".repeat(30);
            probe.push_str("\x1b[?2031h\x1b]11;#ffffff\x07");
            let kf = if carried {
                keyframe_of(&live, usize::MAX).expect("at ground")
            } else {
                live.checkpoint().into()
            };
            assert_eq!(
                judge_after(kf, &mut live, &[(probe.as_bytes(), 1)], rekey),
                (1, u64::from(!carried)),
                "drained response bucket (policy={with_policy}, rekey={rekey}, carried={carried})"
            );
        }

        // 3. Alternate screen entered without cursor save (?1047h).
        for carried in [true, false] {
            let mut live = Terminal::new(6, 30);
            live.process_at(b"main\x1b7\x1b[4;10H\x1b[?1047halt", clock(0));
            let kf = if carried {
                keyframe_of(&live, usize::MAX).expect("at ground")
            } else {
                live.checkpoint().into()
            };
            assert_eq!(
                judge_after(kf, &mut live, &[(b"\x1b]133;A\x07$ ls\r\n", 10)], false),
                (1, u64::from(!carried)),
                "1047 enter + orphaned leave without cursor restore, carried: {carried}"
            );
        }

        // 4. Host alternate-scroll default across DECSTR, and pending restore lineage before output.
        for carried in [true, false] {
            let mut live = Terminal::new(6, 30);
            live.set_host_alternate_scroll(true);
            live.process_at(b"\x1b[?1007h", clock(0));
            let kf = if carried {
                keyframe_of(&live, usize::MAX).expect("at ground")
            } else {
                live.checkpoint().into()
            };
            assert_eq!(
                judge_after(kf, &mut live, &[(b"\x1b[!pafter", 10)], false),
                (1, u64::from(!carried)),
                "host_alternate_scroll across DECSTR, carried: {carried}"
            );
        }

        for carried in [true, false] {
            let mut prior = Terminal::new(6, 30);
            prior.process_at(b"prior\r\n", clock(0));
            let seed = prior.checkpoint_continuing().expect("at ground");
            let mut live = Terminal::new(6, 30);
            live.restore_checkpoint(&seed);
            let kf = if carried {
                keyframe_of(&live, usize::MAX).expect("at ground")
            } else {
                live.checkpoint().into()
            };
            assert_eq!(
                judge_after(kf, &mut live, &[(b"more\r\n", 10)], false),
                (1, u64::from(!carried)),
                "pending restore lineage before output, carried: {carried}"
            );
        }
    }

    fn drain_stamped(rx: &TemporalRx, r: &mut TemporalRecorder) {
        while let Some((generation, msg)) = rx.try_recv_stamped() {
            if let Some(due) = r.apply_in(generation, msg) {
                let minted = due.mint();
                r.rekeyed(minted);
            }
        }
    }

    fn holds(r: &TemporalRecorder, needle: &[u8]) -> bool {
        r.blobs
            .iter()
            .flatten()
            .any(|s| s.bytes().windows(needle.len()).any(|w| w == needle))
    }

    #[test]
    fn a_forget_keeps_the_screen_and_none_of_what_was_queued_before_it() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue_on(r.spine_watermark(), 64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"before\r\n");
        assert!(
            r.forget_onto(&live),
            "a ground screen keys the forgotten recording"
        );
        slice(&mut live, &mut mark, &tx, b"after\r\n");
        drain_stamped(&rx, &mut r);
        assert_eq!(r.keyframe_count(), 1);
        assert!(
            !holds(&r, b"before"),
            "the slice queued before the forget is dropped"
        );
        assert!(holds(&r, b"after"));
        let replayed = r
            .replay_at(None)
            .expect("replay from the forget's keyframe");
        assert_eq!(screen_of(&replayed), screen_of(&live));
        assert_eq!(r.spine_watermark().generation(), 1);
    }

    #[test]
    fn a_forgotten_recording_keeps_nothing_and_counts_on() {
        let mut live = Terminal::new(6, 20);
        let mut r = seeded(&live);
        let (tx, rx) = queue_on(r.spine_watermark(), 64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        slice(&mut live, &mut mark, &tx, b"kept\r\n");
        drain_stamped(&rx, &mut r);
        let total = r.total_events();
        r.forget();
        assert_eq!(
            (r.keyframe_count(), r.live_events(), r.used, r.blobs.len()),
            (0, 0, 0, 0)
        );
        assert!(
            r.replay_at(None).is_none(),
            "nothing before the forget replays"
        );
        assert!(matches!(r.export(), Err(NoExport::NothingReplayable)));
        assert_eq!(r.total_events(), total, "the spine's count goes on");
        slice(&mut live, &mut mark, &tx, b"next\r\n");
        drain_stamped(&rx, &mut r);
        assert_eq!(r.total_events(), total + 1);
    }

    #[test]
    fn a_rekey_minted_before_a_forget_is_discarded() {
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        let live = Terminal::new(6, 20);
        r.record_keyframe(live.checkpoint());
        let burst = || TemporalMsg::RawIn(InputSlice::at(&[b'x'; MIN_REKEY_BYTES][..], clock(0)));
        let minted = r.apply(burst()).expect("due by bytes").mint();
        r.forget();
        r.rekeyed(minted);
        assert_eq!(
            r.keyframe_count(),
            0,
            "the pool's forget appends nothing, so only the generation refuses it"
        );
        assert!(r.replay_at(None).is_none());
        r.record_keyframe(live.checkpoint());
        let minted = r.apply(burst()).expect("due by bytes").mint();
        assert!(r.forget_onto(&live));
        r.rekeyed(minted);
        assert_eq!(r.keyframe_count(), 1, "only the forget's keyframe");
    }

    /// A recorder of `chains` chains, each a keyframe and `bytes` of output.
    fn chained(chains: usize, bytes: usize) -> Arc<Mutex<TemporalRecorder>> {
        let mut r = TemporalRecorder::with_budget(usize::MAX / 4);
        let live = Terminal::new(6, 20);
        for _ in 0..chains {
            r.record_keyframe(live.checkpoint());
            r.record_raw_in(&vec![b'x'; bytes]);
        }
        Arc::new(Mutex::new(r))
    }

    fn kept(r: &Arc<Mutex<TemporalRecorder>>) -> usize {
        r.lock().unwrap().keyframe_count()
    }

    #[test]
    fn the_pool_sheds_the_pane_focused_longest_ago_first() {
        const CHAIN: usize = 64 * 1024;
        let pool = RewindPool::new(usize::MAX);
        let (a, b) = (chained(3, CHAIN), chained(3, CHAIN));
        pool.join(1, &a);
        pool.join(2, &b);
        let charge = |r: &Arc<Mutex<TemporalRecorder>>| r.lock().unwrap().charge();
        assert_eq!(
            pool.used(),
            charge(&a) + charge(&b),
            "each member's charge counts"
        );
        pool.focus(1);
        pool.set_limit(pool.used() - CHAIN);
        pool.relieve();
        assert_eq!(
            (kept(&a), kept(&b)),
            (3, 1),
            "b, focused longest ago, sheds first"
        );
        assert!(pool.used() <= pool.limit());
        pool.set_limit(CHAIN * 3 / 2);
        pool.relieve();
        assert!(pool.used() <= pool.limit());
        assert_eq!(
            kept(&b),
            0,
            "past every older chain, b's whole history goes"
        );
        assert!(b.lock().unwrap().spine_watermark().keyframe_wanted());
        assert_eq!(kept(&a), 1, "the front pane keeps its newest chain");
        drop(b);
        assert_eq!(
            pool.used(),
            charge(&a),
            "a dropped recorder refunds its charge"
        );
    }

    #[test]
    fn focusing_the_front_pane_again_moves_nothing_until_a_pane_joins() {
        let pool = RewindPool::new(usize::MAX);
        let (a, b) = (chained(1, 16), chained(1, 16));
        pool.join(1, &a);
        pool.focus(1);
        let stamp = |pool: &RewindPool, s: u64| {
            pool.members
                .lock()
                .unwrap()
                .iter()
                .find(|m| m.session == s)
                .map(|m| m.focused)
        };
        let first = stamp(&pool, 1);
        pool.focus(1);
        assert_eq!(
            stamp(&pool, 1),
            first,
            "the same front pane is stamped once"
        );
        pool.join(2, &b);
        pool.focus(1);
        assert!(
            stamp(&pool, 1) > stamp(&pool, 2),
            "a pane that joined behind it is older"
        );
    }

    #[test]
    fn the_shared_writer_writes_every_queue_and_its_drain_says_when() {
        let recorders: Vec<Arc<Mutex<TemporalRecorder>>> = (0..3)
            .map(|_| {
                let r = Arc::new(Mutex::new(TemporalRecorder::new()));
                r.lock()
                    .unwrap()
                    .record_keyframe(Terminal::new(6, 20).checkpoint());
                r
            })
            .collect();
        let queues: Vec<TemporalTx> = recorders
            .iter()
            .map(|r| open(r, QUEUE_CAP).expect("the shared writer starts"))
            .collect();
        for (n, tx) in queues.iter().enumerate() {
            for _ in 0..=n {
                assert!(tx.send(TemporalMsg::RawIn(InputSlice::at(b"x", clock(0)))));
            }
        }
        for (n, (tx, r)) in queues.iter().zip(&recorders).enumerate() {
            tx.drain().wait();
            assert!(tx.drain().is_finished());
            assert_eq!(
                r.lock().unwrap().live_events(),
                n + 2,
                "keyframe and {} slices",
                n + 1
            );
        }
    }

    #[test]
    fn a_full_queue_refuses_until_the_writer_catches_up() {
        let (tx, rx) = queue(2);
        let msg = || TemporalMsg::RawIn(InputSlice::at(b"x", clock(0)));
        assert!(tx.send(msg()) && tx.send(msg()));
        assert!(!tx.send(msg()), "the third message is past the cap");
        assert!(!tx.drain().is_finished());
        let _ = rx.try_recv();
        assert!(tx.send(msg()), "one written frees one place");
        drop(rx);
        assert!(!tx.send(msg()), "a closed queue takes nothing");
    }

    #[test]
    fn an_armed_recording_is_picked_up_once_at_its_spine() {
        let live = Terminal::new(6, 20);
        let r = seeded(&live);
        let arm = Arm::default();
        let (tx, _rx) = queue_on(r.spine_watermark(), 8);
        let mut held = None;
        let mut mark = SpineMark::default();
        pick_up(&arm, &mut held, &mut mark);
        assert!(held.is_none(), "nothing armed, nothing picked up");
        arm.set(tx);
        pick_up(&arm, &mut held, &mut mark);
        assert!(held.is_some());
        assert_eq!(
            mark.at,
            r.spine_watermark().load(),
            "the mark starts at the seed"
        );
        assert!(arm.take().is_none(), "an arm is taken once");
    }

    #[test]
    fn a_queue_holds_at_most_its_bytes_and_its_pools_share() {
        let slice = |n: usize| TemporalMsg::RawIn(InputSlice::at(&vec![b'x'; n][..], clock(0)));
        let (tx, rx) = queue_in(Arc::default(), 64, 100, RewindPool::new(usize::MAX));
        assert!(tx.send(slice(60)));
        assert!(!tx.send(slice(60)), "past the queue's bytes");
        assert!(tx.send(slice(40)), "up to them");
        assert!(
            !tx.has_room(),
            "a break keys again only once half its bytes are written"
        );
        let key = Arc::new(Terminal::new(6, 20).checkpoint()).into();
        assert!(
            tx.send(TemporalMsg::Keyframe(key)),
            "a keyframe passes the bytes"
        );
        while rx.try_recv().is_some() {}
        assert!(tx.has_room());
        let pool = RewindPool::new(4 * MIN_QUEUE_BYTES);
        let (a, _a) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
        let (b, _b) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
        assert!(a.send(slice(MIN_QUEUE_BYTES - 10)));
        assert!(
            !b.send(slice(20)),
            "past the pool's share across its queues"
        );
        assert_eq!(
            pool.used(),
            MIN_QUEUE_BYTES - 10,
            "what is queued counts against the pool"
        );
    }

    #[test]
    fn output_queued_onto_a_full_pool_sheds_history_to_make_room() {
        const CHAIN: usize = 64 * 1024;
        let a = chained(3, CHAIN);
        let pool = RewindPool::new(usize::MAX);
        pool.join(1, &a);
        pool.set_limit(pool.used() + CHAIN / 2);
        let (tx, rx) = queue_in(Arc::default(), 64, usize::MAX, pool.clone());
        assert!(tx.send(TemporalMsg::RawIn(InputSlice::at(
            &vec![b'x'; CHAIN][..],
            clock(0)
        ))));
        pool.relieve();
        assert!(kept(&a) < 3, "the queued output made room for itself");
        assert!(pool.used() <= pool.limit());
        let _ = rx.try_recv();
        assert_eq!(
            pool.used(),
            a.lock().unwrap().charge(),
            "a written message is uncounted"
        );
    }

    #[test]
    fn pane_chains_carry_transient_engine_state_and_cell_px_across_keyframes_and_slices() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;
        use aterm_aos_pane::trace::PaneTrace;

        let mut policy = aterm_core::policy::profiles::permissive();
        for rl in &mut policy.rate_limits {
            if rl.id == "response" {
                rl.capacity_bytes = 24;
                rl.refill_per_second = 100;
                rl.per_sequence_max = 0;
            }
        }
        let mut live = Terminal::new(6, 30);
        live.apply_policy_engine(aterm_core::policy::engine::PolicyEngine::new(policy));
        let _ = live.authorize_hyperlink_scheme("aterm-test");
        live.set_host_alternate_scroll_default(false);
        live.set_cell_pixel_size(9, 19);

        // Drain the 24-byte response rate-limit bucket (6 DSRs = 24 B) and open a mode-2026 frame
        // before the seed keyframe.
        live.process_at(
            b"\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[5n\x1b[?2026h",
            clock(100),
        );
        let _ = (live.take_response(), live.take_reports());

        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut feed = |live: &mut Terminal, r: &mut TemporalRecorder, bytes: &[u8], ms: u64| {
            let pre = SpinePre::take(live);
            live.process_at(bytes, clock(ms));
            let (reports, response) = (live.take_reports(), live.take_response());
            let reply = reply_of(response.as_deref(), reports.as_deref());
            mark.record(
                &tx,
                pre,
                live,
                InputSlice::at(bytes, clock(ms)).replying(reply),
            );
            drain(&rx, r);
        };

        // At 110ms (+1 B refilled < 4 B needed), the 7th DSR is dropped by the carried response
        // bucket and the custom hyperlink scheme is accepted.
        feed(
            &mut live,
            &mut r,
            b"\x1b[5n\x1b]8;;aterm-test://doc\x07link\x1b]8;;\x07",
            110,
        );
        // At 400ms (bucket refilled), CSI 16 t reports 19;9 and mode 2048 enables in-band reports;
        // then the host updates cell pixel size and drains its immediate report as the UI does.
        feed(&mut live, &mut r, b"\x1b[16t\x1b[?2048h", 400);
        live.set_cell_pixel_size(12, 24);
        let _ = live.take_reports();
        // Past the 1s mode-2026 window (100ms + 1150ms > 1s): synchronized output auto-expires,
        // CSI 16 t reports 24;12, and a witness is taken past WITNESS_INTERVAL.
        feed(&mut live, &mut r, b"\x1b[16t!", 1250);

        // Project a LiveKeyframe mid-session and verify the second chain also carries everything.
        r.want_keyframe();
        feed(&mut live, &mut r, b"resync", 1300);
        feed(&mut live, &mut r, b"\x1b[!p\x1b[16t.", 2400);
        feed(&mut live, &mut r, b"done", 3500);

        assert_eq!(r.keyframe_count(), 2);
        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(
            r.diverged(),
            0,
            "in-memory ReplayPlan folds match every witness"
        );
        assert!(r.witnessed() >= 2, "REACH: both chains witnessed");

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 2);
        for (idx, chain) in chains.iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(
                check.verdict,
                Verdict::Pass,
                "chain {idx}: {:?}",
                check.verdict
            );
            let bytes = chain
                .trace(PaneCode::ThisBuild, "test")
                .expect("trace builds")
                .encode();
            let decoded = PaneTrace::decode(&bytes).expect("trace decodes");
            let recheck = decoded
                .check(ClockReading::now(), "test")
                .expect("trace checks");
            assert_eq!(
                recheck.verdict,
                Verdict::Pass,
                "decoded chain {idx}: {:?}",
                recheck.verdict
            );
        }
    }

    #[test]
    fn an_in_flight_live_keyframe_keeps_the_config_it_was_projected_under_when_replay_config_moves()
    {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let c1 = Arc::new(TerminalConfig {
            cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
            ..TerminalConfig::default()
        });
        let c2 = Arc::new(TerminalConfig {
            cursor_style: aterm_core::terminal::CursorStyle::SteadyUnderline,
            ..TerminalConfig::default()
        });

        let mut live = Terminal::new(6, 30);
        let _ = live.apply_config(&c1);
        let mut r = TemporalRecorder::new();
        r.set_replay_config(Some(c1.clone()));
        // Start from a pool-forgotten state (`keyframes` empty, `keyframe_wanted` true) so we
        // exercise both an in-flight LiveKeyframe AND `set_replay_config` while `keyframes` is empty.
        r.forget_for_pool();

        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());

        // Reader projects LiveKeyframe under c1 onto `tx`, NOT yet drained by the writer thread.
        slice_at(&mut live, &mut mark, &tx, b"under-c1", clock(0));
        assert!(!r.spine_watermark().keyframe_wanted());

        // Main thread switches `live` and `r` to c2 before the writer drains `rx`.
        let _ = live.apply_config(&c2);
        r.set_replay_config(Some(c2.clone()));
        assert!(
            r.spine_watermark().keyframe_wanted(),
            "config move while a keyframe is in flight still asks the reader for a c2 keyframe"
        );

        // Next reader slice resyncs a LiveKeyframe under c2, then soft-resets and witnesses under c2.
        slice_at(&mut live, &mut mark, &tx, b"resync-c2", clock(100));
        slice_at(&mut live, &mut mark, &tx, b"\x1b[2 qa", clock(200));
        slice_at(&mut live, &mut mark, &tx, b"\x1b[!pb", clock(1300));

        drain(&rx, &mut r);
        assert_eq!(r.keyframe_count(), 2);
        assert_eq!(r.keyframes[0].config.as_deref(), Some(&*c1));
        assert_eq!(r.keyframes[1].config.as_deref(), Some(&*c2));

        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (1, 0));

        for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(
                check.verdict,
                Verdict::Pass,
                "chain {idx}: {:?}",
                check.verdict
            );
        }
    }

    #[test]
    fn foreground_drawer_holder_cuts_and_retires_replay_without_divergence() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;
        use aterm_aos_pane::trace::PaneTrace;
        use aterm_core::terminal::{CutKind, HolderCut};

        for record_cuts in [true, false] {
            let mut live = Terminal::new(8, 40);
            live.process_at(b"shell prompt\r\n", clock(0));
            let mut r = seeded(&live);
            let (tx, rx) = queue(64);
            let mut mark = SpineMark::attach(r.spine_watermark());

            // Drawer token 42 takes foreground, draws on main screen, suspends (`from_alive: true`),
            // resumes, suspends again, and then its group exits (`retire_holder_mark` + `flush_host`).
            live.note_holder_cut(HolderCut {
                to: Some(42),
                kind: CutKind::Group,
                from_alive: true,
            });
            live.set_foreground_job(true);
            if !record_cuts {
                // Negative control: advance the spine mark past the cut so the replay never hears it.
                mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
            }
            slice_at(
                &mut live,
                &mut mark,
                &tx,
                b"drawer line 1\r\ndrawer line 2\r\n",
                clock(100),
            );

            live.note_holder_cut(HolderCut {
                to: None,
                kind: CutKind::Group,
                from_alive: true,
            });
            live.set_foreground_job(false);
            if !record_cuts {
                mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
            }
            slice_at(&mut live, &mut mark, &tx, b"$ fg\r\n", clock(1200));

            live.note_holder_cut(HolderCut {
                to: Some(42),
                kind: CutKind::Group,
                from_alive: true,
            });
            live.set_foreground_job(true);
            if !record_cuts {
                mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
            }
            slice_at(&mut live, &mut mark, &tx, b"drawer line 3\r\n", clock(2400));

            live.note_holder_cut(HolderCut {
                to: None,
                kind: CutKind::Group,
                from_alive: true,
            });
            live.set_foreground_job(false);
            if !record_cuts {
                mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
            }
            slice_at(&mut live, &mut mark, &tx, b"$ kill %1\r\n", clock(3600));

            live.retire_holder_mark(42);
            if record_cuts {
                mark.flush_host(&tx, &live);
            } else {
                mark.advance_host((live.resize_lifetime(), live.host_ordinal()));
            }
            slice_at(&mut live, &mut mark, &tx, b"[1] + killed\r\n", clock(4800));
            drain(&rx, &mut r);

            let fold = r.replay_plan(None).expect("replay").fold();
            r.judge(fold.judged);
            if record_cuts {
                assert_eq!((r.witnessed(), r.diverged()), (4, 0));
                assert_eq!(
                    fold.term.checkpoint_visible(),
                    live.checkpoint_visible(),
                    "holder fence anchors, seals and retirements match live"
                );
                for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
                    let check = chain.check(PaneCode::ThisBuild, "test");
                    assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
                    let encoded = chain
                        .trace(PaneCode::ThisBuild, "test")
                        .expect("trace")
                        .encode();
                    let rechecked = PaneTrace::decode(&encoded)
                        .expect("decode")
                        .check(ClockReading::now(), "test")
                        .expect("check");
                    assert_eq!(rechecked.verdict, Verdict::Pass);
                }
            } else {
                assert!(
                    r.diverged() >= 1,
                    "negative control: omitting HolderCut diverges on holder state"
                );
            }
        }
    }

    #[test]
    fn foreground_job_and_program_probe_cwd_restoration_replay_with_zero_extra_probes() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let nonce = [0x44u8; 32];
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let child_running = Arc::new(AtomicBool::new(false));

        let mut live = Terminal::new(6, 40);
        live.authorize_shell_integration(nonce);
        live.set_require_shell_integration_nonce(true);
        {
            let calls = probe_calls.clone();
            let running = child_running.clone();
            live.set_program_probe(Some(Box::new(move |_since_ms| {
                calls.fetch_add(1, Ordering::Relaxed);
                running.load(Ordering::Relaxed)
            })));
        }
        let boot = format!(
            "\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07:\r\n\x1b]133;C;id={hex}\x07\x1b]133;D;0;id={hex}\x07"
        );
        let prompt = format!(
            "\x1b]133;A;id={hex}\x07\x1b]633;P;Cwd=/work;id={hex}\x07$ \x1b]133;B;aterm_input=start;id={hex}\x07"
        );
        live.process_at(boot.as_bytes(), clock(0));
        live.process_at(prompt.as_bytes(), clock(10));
        assert_eq!(live.current_working_directory(), Some("/work"));

        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());

        // 1. Explicit unix foreground job (`set_foreground_job(true)` -> unsigned OSC 7 -> `false`).
        live.set_foreground_job(true);
        slice_at(
            &mut live,
            &mut mark,
            &tx,
            b"\x1b]7;file:///work/sub1\x07",
            clock(100),
        );
        assert_eq!(live.current_working_directory(), Some("/work/sub1"));
        assert_eq!(
            probe_calls.load(Ordering::Relaxed),
            0,
            "explicit foreground_job never calls program_probe"
        );
        live.set_foreground_job(false);
        slice_at(&mut live, &mut mark, &tx, b"back\r\n", clock(1200));
        assert_eq!(live.current_working_directory(), Some("/work"));
        // `set_foreground_job(false)` with `cwd_before_job.is_some()` checks `program_probe` once.
        assert_eq!(probe_calls.load(Ordering::Relaxed), 1);

        // Re-enter fresh signed prompt for the in-group child (`program_probe`) path.
        slice_at(&mut live, &mut mark, &tx, prompt.as_bytes(), clock(1300));
        child_running.store(true, Ordering::Relaxed);
        slice_at(
            &mut live,
            &mut mark,
            &tx,
            b"\x1b]7;file:///work/sub2\x07",
            clock(2400),
        );
        assert_eq!(live.current_working_directory(), Some("/work/sub2"));
        assert!(
            live.input_region_start().is_some(),
            "in-group probe does not set foreground_job on input_region_start"
        );
        // Only the single unsigned OSC 7 called `program_probe` — zero extra probe calls for spine recording.
        assert_eq!(probe_calls.load(Ordering::Relaxed), 2);

        // Child exits; next reader burst calls `set_foreground_job(false)` which probes once and restores `/work`.
        child_running.store(false, Ordering::Relaxed);
        live.set_foreground_job(false);
        assert_eq!(live.current_working_directory(), Some("/work"));
        assert_eq!(probe_calls.load(Ordering::Relaxed), 3);
        slice_at(&mut live, &mut mark, &tx, b"done\r\n", clock(3600));
        drain(&rx, &mut r);

        let fold = r.replay_plan(None).expect("replay").fold();
        r.judge(fold.judged);
        assert_eq!((r.witnessed(), r.diverged()), (3, 0));
        assert_eq!(fold.term.current_working_directory(), Some("/work"));

        for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
        }
    }

    #[test]
    fn off_reader_screen_cuts_alt_leave_and_shell_rekey_replay_and_step_rewind_instants() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let n1 = [0x11u8; 32];
        let n2 = [0x22u8; 32];
        let n3 = [0x33u8; 32];
        let hex3: String = n3.iter().map(|b| format!("{b:02x}")).collect();

        let mut live = Terminal::new(5, 30);
        live.authorize_shell_integration(n1);
        live.set_require_shell_integration_nonce(true);
        for i in 0..8 {
            live.process_at(format!("hist {i}\r\n").as_bytes(), clock(i * 10));
        }
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());

        // Enter alt screen, then force-leave off-reader at t = 200ms.
        r.freeze_now_for_test(Some(Ticks(1_000)));
        slice_at(
            &mut live,
            &mut mark,
            &tx,
            b"\x1b[?1049h\x1b[1;1HALT1",
            clock(100),
        );
        drain(&rx, &mut r);

        r.freeze_now_for_test(Some(Ticks(2_000)));
        assert!(live.leave_alternate_screen_at(clock(200)));
        mark.flush_host(&tx, &live);
        drain(&rx, &mut r);

        // Enter alt screen again, arm `expect_host_alt_screen_leave`, and forward `?1049l`.
        r.freeze_now_for_test(Some(Ticks(3_000)));
        slice_at(
            &mut live,
            &mut mark,
            &tx,
            b"\x1b[?1049h\x1b[1;1HALT2",
            clock(300),
        );
        drain(&rx, &mut r);
        assert!(live.expect_host_alt_screen_leave());
        r.freeze_now_for_test(Some(Ticks(4_000)));
        slice_at(&mut live, &mut mark, &tx, b"\x1b[?1049lMAIN", clock(1500));
        drain(&rx, &mut r);

        // Off-reader `clear_scrollback` at tick 5_000, then typed shell rekey withdraw + settle across a live keyframe.
        r.freeze_now_for_test(Some(Ticks(5_000)));
        live.clear_scrollback();
        mark.flush_host(&tx, &live);
        drain(&rx, &mut r);

        live.authorize_shell_integration_rekey(n2);
        assert!(live.withdraw_shell_integration_rekey(&n2));
        live.authorize_shell_integration_rekey(n3);
        r.want_keyframe();
        r.freeze_now_for_test(Some(Ticks(6_000)));
        slice_at(&mut live, &mut mark, &tx, b"rekey-kf", clock(1600));
        drain(&rx, &mut r);

        assert!(live.settle_shell_integration_rekey(&n3));
        r.freeze_now_for_test(Some(Ticks(7_000)));
        let signed_c = format!(
            "\x1b]133;A;id={hex3}\x07$ \x1b]133;B;id={hex3}\x07ls\r\n\x1b]133;C;id={hex3}\x07"
        );
        slice_at(&mut live, &mut mark, &tx, signed_c.as_bytes(), clock(2800));
        drain(&rx, &mut r);

        // Off-reader `reset` at tick 8_000 creates a distinct steppable RewindInstant and clears screen.
        r.freeze_now_for_test(Some(Ticks(8_000)));
        live.reset();
        mark.flush_host(&tx, &live);
        drain(&rx, &mut r);

        r.freeze_now_for_test(Some(Ticks(9_000)));
        slice_at(&mut live, &mut mark, &tx, b"after-reset", clock(4000));
        drain(&rx, &mut r);
        r.freeze_now_for_test(None);

        let instants: Vec<u64> = r.rewind_instants().into_iter().map(|i| i.at.0).collect();
        for expected_cut_tick in [2_000, 5_000, 8_000] {
            assert!(
                instants.contains(&expected_cut_tick),
                "screen-cut HostOp at tick {expected_cut_tick} starts a RewindInstant in {instants:?}"
            );
        }
        let at_reset = r.replay_at(Some(Ticks(8_000))).expect("replay at reset");
        assert!(at_reset.render_row(0).is_empty());

        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(r.diverged(), 0);
        assert!(r.witnessed() >= 2);

        for (idx, chain) in r.pane_chains().expect("chains").iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
        }
    }

    #[test]
    fn split_decrqss_and_xtgettcap_across_slices_and_deferred_rekeys_pass_pane_chains() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let mut live = Terminal::new(6, 40);
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
            let at = clock(ms);
            let pre = SpinePre::take(live);
            live.process_at(bytes, at);
            let reply = live.take_response().map(Arc::<[u8]>::from);
            mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
        };

        // Push up to the rekey byte threshold and leave the slice mid-DECRQSS (`ends_mid_sequence`).
        let mut near_rekey = vec![b'a'; MIN_REKEY_BYTES];
        near_rekey.extend_from_slice(b"\x1b[32m\x1bP$q");
        step(&mut live, &mut mark, &near_rekey, 100);
        drain(&rx, &mut r);
        assert_eq!(
            r.keyframe_count(),
            1,
            "mid-DECRQSS slice defers rekey until Ground"
        );

        // Advance to ST-pending (`ESC`), still mid-query.
        step(&mut live, &mut mark, b"m\x1b", 200);
        drain(&rx, &mut r);
        assert_eq!(r.keyframe_count(), 1);

        // Complete DECRQSS (`\`), landing at Ground so the deferred rekey mints, then split XTGETTCAP.
        step(&mut live, &mut mark, b"\\ok\r\n", 1300);
        drain(&rx, &mut r);
        assert_eq!(r.keyframe_count(), 2, "rekey minted once Ground returned");

        step(&mut live, &mut mark, b"\x1bP+q54", 1400);
        step(&mut live, &mut mark, b"4E\x1b", 1500);
        step(&mut live, &mut mark, b"\\done\r\n", 2600);
        drain(&rx, &mut r);

        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(r.diverged(), 0);
        assert!(r.witnessed() >= 2);

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 2);
        for (idx, chain) in chains.iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
        }
    }

    #[test]
    fn completed_unshared_chains_pack_into_slabs_while_shared_bursts_stay_shared() {
        let mut live = Terminal::new(8, 32);
        let mut r = TemporalRecorder::with_budget(8 * 1024 * 1024);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));

        let shared: Arc<[u8]> = Arc::from(&b"shared-burst\r\n"[..]);
        live.process(&shared);
        r.record_raw_in_shared(shared.clone());

        // Push 100 small unshared slices (4 KiB each = 400 KiB > BLOB_SLAB_BYTES) in chain 0.
        for i in 0..100u32 {
            let mut chunk = format!("line-{i:04}: ").into_bytes();
            chunk.resize(4096, b'x');
            chunk[4094] = b'\r';
            chunk[4095] = b'\n';
            live.process(&chunk);
            r.record_raw_in(&chunk);
        }
        let chain0_end = r.latest_tick();
        let expected_chain0 = screen_of(&live);

        // Record a second keyframe so chain 0 is completed and packed.
        r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("rekey"));
        assert_eq!(
            Arc::strong_count(&shared),
            2,
            "externally held burst was not copied"
        );
        let retained = r.retained_inputs();
        assert_eq!(retained.len(), 101);
        assert!(
            Arc::ptr_eq(&retained[0].0, &shared),
            "shared slice keeps caller allocation"
        );
        let mut unique_slabs: Vec<*const u8> = retained[1..]
            .iter()
            .map(|(arc, _)| Arc::as_ptr(arc).cast::<u8>())
            .collect();
        unique_slabs.sort_unstable();
        unique_slabs.dedup();
        assert_eq!(
            unique_slabs.len(),
            2,
            "400 KiB of 4 KiB slices packed into two <=256 KiB slabs"
        );
        assert!(
            !r.keyframes[0].packed,
            "chain stays eligible until externally shared burst is dropped"
        );
        let replayed = r.replay_at(Some(chain0_end)).expect("replay packed chain");
        assert_eq!(screen_of(&replayed), expected_chain0);

        drop(retained);
        drop(shared);
        r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("rekey 2"));
        assert!(
            r.keyframes[0].packed,
            "once shared holder drops, remaining small slice packs and marks chain packed"
        );
        let replayed_again = r
            .replay_at(Some(chain0_end))
            .expect("replay fully packed chain");
        assert_eq!(screen_of(&replayed_again), expected_chain0);
    }

    #[test]
    fn kitty_store_numbers_virtuals_and_chunked_transfers_pass_pane_chains_and_charge_budget() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let mut live = Terminal::new(6, 40);
        live.set_cell_pixel_size(10, 20);
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
            let at = clock(ms);
            let pre = SpinePre::take(live);
            live.process_at(bytes, at);
            let reply = live.take_response().map(Arc::<[u8]>::from);
            mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
        };

        // Chain 0: store RGBA image 1 (via number 42) + frame 2, virtual placement U=1 + placeholder,
        // open m=1 chunked transfer for image 8, and cross MIN_REKEY_BYTES so a rekey mints.
        let mut pre_rekey = b"\x1b_Ga=t,f=32,s=3,v=1,I=42;////AAAA////AAAA\x1b\\\
                              \x1b_Ga=f,f=32,s=3,v=1,i=1,z=50;AAAA////AAAA////\x1b\\\
                              \x1b_Ga=p,U=1,i=1,p=3,c=2,r=1\x1b\\"
            .to_vec();
        pre_rekey.extend_from_slice(
            "\x1b[2;1H\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes(),
        );
        pre_rekey.extend_from_slice(b"\x1b_Ga=t,f=24,s=1,v=1,i=8,m=1;/w\x1b\\");
        pre_rekey.extend(std::iter::repeat_n(b'x', MIN_REKEY_BYTES));
        pre_rekey.extend_from_slice(b"\r\n");
        step(&mut live, &mut mark, &pre_rekey, 100);
        drain(&rx, &mut r);
        if let Some(rekey) = r.rekey_due() {
            r.rekeyed(rekey.mint());
        }
        assert_eq!(r.keyframe_count(), 2, "rekey minted carrying Kitty store");
        assert!(
            r.keyframes[1].carry.retained_image_bytes() >= 24,
            "minted keyframe retains Kitty image and chunk bytes"
        );
        assert!(
            r.keyframes[1].charge() > charge_of(&r.keyframes[1].checkpoint),
            "keyframe charge includes retained Kitty payload bytes"
        );

        // Chain 1 (after the rekey): complete m=0 chunk, place by I=42 and i=8, animate, delete.
        step(
            &mut live,
            &mut mark,
            b"\x1b_Gm=0;AA\x1b\\\
              \x1b[3;1H\x1b_Ga=p,I=42,c=2,r=1\x1b\\\
              \x1b[4;1H\x1b_Ga=p,i=8,c=1,r=1\x1b\\\
              \x1b_Ga=a,i=1,c=2\x1b\\",
            1200,
        );
        step(&mut live, &mut mark, b"\x1b_Ga=d,d=A\x1b\\", 2400);
        drain(&rx, &mut r);

        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(r.diverged(), 0);

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 2);
        for (idx, chain) in chains.iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
        }

        // Budget shedding: a keyframe holding Kitty payload bytes counts against the recorder
        // budget and is evicted when a newer keyframe pushes `used` past `budget`.
        let mut small = TemporalRecorder::with_budget(16 * 1024);
        let mut img_term = Terminal::new(4, 20);
        let raw = vec![0xABu8; 80 * 30 * 4];
        let b64 = aterm_codec::base64::encode(&raw).expect("base64");
        let cmd = format!("\x1b_Ga=t,f=32,s=80,v=30,i=1;{b64}\x1b\\");
        img_term.process(cmd.as_bytes());
        let kf_heavy = keyframe_of(&img_term, usize::MAX).expect("heavy keyframe");
        let heavy_img_bytes = kf_heavy.carry.retained_image_bytes();
        assert_eq!(heavy_img_bytes, raw.len());
        small.record_keyframe(kf_heavy);
        assert!(small.used > raw.len());
        assert_eq!(small.keyframe_count(), 1);

        // Replace image 1 with a second 9,600-byte Kitty image on a new keyframe so each keyframe
        // fits under 16 KiB alone, while both together (> 19.2 KiB) exceed 16 KiB.
        let raw2 = vec![0xCDu8; 80 * 30 * 4];
        let b64_2 = aterm_codec::base64::encode(&raw2).expect("base64 2");
        img_term.process(
            format!("\x1b_Ga=d,d=A\x1b\\\x1b_Ga=t,f=32,s=80,v=30,i=2;{b64_2}\x1b\\").as_bytes(),
        );
        let kf_heavy_2 = keyframe_of(&img_term, usize::MAX).expect("second keyframe");
        assert_eq!(kf_heavy_2.carry.retained_image_bytes(), raw2.len());
        small.record_keyframe(kf_heavy_2);
        assert!(
            small.used <= small.budget,
            "budget shed stale image-bearing keyframe ({} <= {})",
            small.used,
            small.budget
        );
        assert_eq!(small.keyframe_count(), 1);
    }

    #[test]
    fn kitty_file_queries_and_missing_file_transmits_pass_witnesses_and_pane_chains_across_rekeys()
    {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let mut live = Terminal::new(6, 40);
        live.set_kitty_file_resolver(|_, _| None);
        let mut r = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("at ground"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
            let at = clock(ms);
            let pre = SpinePre::take(live);
            live.process_at(bytes, at);
            let reply = live.take_response().map(Arc::<[u8]>::from);
            mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
        };

        // Chain 0: query `t=f` (`OK`), cross `MIN_REKEY_BYTES`, and mint a second keyframe.
        let mut pre_rekey = b"\x1b_Gi=31,a=q,t=f\x1b\\".to_vec();
        pre_rekey.extend(std::iter::repeat_n(b'x', MIN_REKEY_BYTES));
        pre_rekey.extend_from_slice(b"\r\n");
        step(&mut live, &mut mark, &pre_rekey, 100);
        drain(&rx, &mut r);
        if let Some(rekey) = r.rekey_due() {
            r.rekeyed(rekey.mint());
        }
        assert_eq!(r.keyframe_count(), 2);
        assert!(
            r.keyframes[1].carry.kitty_media_admitted(),
            "minted keyframe preserves kitty_media_admitted across rekeys"
        );

        // Chain 1 (after the rekey): query `t=t`/`t=s` (`OK`) and refuse a missing `t=f` (`EINVAL`).
        step(
            &mut live,
            &mut mark,
            b"\x1b_Gi=32,a=q,t=t\x1b\\\
              \x1b_Gi=33,a=q,t=s\x1b\\\
              \x1b_Gi=34,a=T,t=f,f=32,s=1,v=1;L21pc3Npbmc=\x1b\\",
            1200,
        );
        drain(&rx, &mut r);

        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(r.diverged(), 0);

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 2);
        for (idx, chain) in chains.iter().enumerate() {
            let check = chain.check(PaneCode::ThisBuild, "test");
            assert_eq!(check.verdict, Verdict::Pass, "chain {idx}: {check:?}");
        }
    }

    #[test]
    fn successful_kitty_file_loads_and_osc52_queries_project_live_keyframes_with_zero_divergence_and_pass_pane_chains()
     {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::{PaneCode, trace::PaneTrace};
        use aterm_core::terminal::{ClipboardAccess, ClipboardOperation};

        let secret_path = "/secret/host/photo.raw";
        let secret_path_b64 = aterm_codec::base64::encode(secret_path.as_bytes()).expect("b64");
        let secret_clip = "secret-clipboard-token";
        let secret_clip_b64 = aterm_codec::base64::encode(secret_clip.as_bytes()).expect("b64");

        let mut live = Terminal::new(6, 40);
        live.set_kitty_file_resolver(|_, path| {
            (path == "/secret/host/photo.raw").then(|| vec![0x11, 0x22, 0x33, 0xff])
        });
        live.authorize_clipboard_access(ClipboardAccess::Query);
        live.set_clipboard_callback(|op| match op {
            ClipboardOperation::Query { .. } => Some("secret-clipboard-token".to_string()),
            _ => None,
        });

        let mut r = TemporalRecorder::with_budget(8 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
            let at = clock(ms);
            let pre = SpinePre::take(live);
            live.process_at(bytes, at);
            let reply = live.take_response().map(Arc::<[u8]>::from);
            mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
        };

        step(&mut live, &mut mark, b"before\r\n", 100);
        let load = format!("\x1b_Gi=7,a=T,t=f,f=32,s=1,v=1;{secret_path_b64}\x1b\\");
        step(&mut live, &mut mark, load.as_bytes(), 200);
        step(&mut live, &mut mark, b"\x1b[3;1H\x1b_Gi=7,a=p\x1b\\", 300);
        step(&mut live, &mut mark, b"placed\r\n", 1_400);
        step(&mut live, &mut mark, b"\x1b]52;c;?\x07", 2_000);
        step(&mut live, &mut mark, b"after\r\n", 3_500);
        step(&mut live, &mut mark, b"done\r\n", 4_600);
        drain(&rx, &mut r);

        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!((r.witnessed(), r.diverged()), (2, 0));
        let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
        assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 3);
        let lacks = |hay: &[u8], needle: &[u8]| !hay.windows(needle.len()).any(|w| w == needle);
        for (idx, chain) in chains.iter().enumerate() {
            assert_eq!(
                chain.check(PaneCode::ThisBuild, "test").verdict,
                Verdict::Pass,
                "{idx}"
            );
            let bytes = chain
                .trace(PaneCode::ThisBuild, "test")
                .expect("trace")
                .encode();
            for s in [
                secret_path.as_bytes(),
                secret_path_b64.as_bytes(),
                secret_clip.as_bytes(),
                secret_clip_b64.as_bytes(),
            ] {
                assert!(lacks(&bytes, s));
            }
            let round = PaneTrace::decode(&bytes)
                .expect("decode")
                .check(clock(0), "test")
                .expect("check");
            assert_eq!(round.verdict, Verdict::Pass);
        }

        // Negative control: recording `load` as a bare `RawIn` without the LiveKeyframe cut fails `REPLIES_MATCH`.
        let mut unresynced = TemporalRecorder::with_budget(4 * MIN_REKEY_BYTES);
        let mut bare_seed = Terminal::new(6, 40);
        bare_seed.set_kitty_file_resolver(|_, _| None);
        unresynced.record_keyframe(keyframe_of(&bare_seed, usize::MAX).expect("seed"));
        unresynced.apply_inline(TemporalMsg::RawIn(
            InputSlice::at(load.as_bytes(), clock(200))
                .replying(Some(Arc::from(&b"\x1b_Gi=7;OK\x1b\\"[..]))),
        ));
        assert_ne!(
            unresynced.pane_chains().expect("chains")[0]
                .check(PaneCode::ThisBuild, "test")
                .verdict,
            Verdict::Pass
        );
    }

    #[test]
    fn consecutive_kitty_file_loads_coalesce_live_keyframes_and_preserve_earlier_chains() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::{PaneCode, trace::PaneTrace};

        let mut live = Terminal::new(8, 40);
        live.set_kitty_file_resolver(|_, path| {
            path.strip_prefix("/secret/gallery/thumb-")
                .and_then(|n| n.strip_suffix(".raw"))
                .and_then(|n| n.parse::<u8>().ok())
                .map(|k| vec![k, k.wrapping_add(1), k.wrapping_add(2), 0xff])
        });
        let mut r = TemporalRecorder::with_budget(16 * MIN_REKEY_BYTES);
        r.freeze_now_for_test(Some(Ticks(10)));
        r.record_keyframe(keyframe_of(&live, usize::MAX).expect("seed"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(128);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let mut step = |bytes: &[u8], ms: u64| {
            let pre = SpinePre::take(&live);
            live.process_at(bytes, clock(ms));
            let reply = live.take_response().map(Arc::<[u8]>::from);
            let replied = reply.is_some();
            mark.record(
                &tx,
                pre,
                &live,
                InputSlice::at(bytes, clock(ms)).replying(reply),
            );
            if replied {
                mark.reply(&tx);
            }
            r.freeze_now_for_test(Some(Ticks(ms)));
            drain(&rx, &mut r);
        };

        step(b"pre-gallery shell output\r\n", 100);
        step(b"ready\r\n", 1_200);
        let mut secrets = Vec::new();
        for idx in 1u8..=10 {
            let path = format!("/secret/gallery/thumb-{idx}.raw");
            let path_b64 = aterm_codec::base64::encode(path.as_bytes()).expect("b64");
            let prefix = if idx == 1 {
                "\x1b]133;A\x07$ \x1b]133;B\x07gallery\r\n\x1b]133;C\x07"
            } else {
                ""
            };
            let cmd = format!("{prefix}\x1b_Gi={idx},a=T,t=f,f=32,s=1,v=1;{path_b64}\x1b\\");
            secrets.extend([path.into_bytes(), path_b64.into_bytes()]);
            step(cmd.as_bytes(), 1_200 + u64::from(idx) * 50);
        }
        step(b"gallery complete\r\n", 2_000);
        step(b"done\r\n", 3_100);

        assert_eq!(
            (r.keyframe_count(), r.reachable_from()),
            (2, Some(Ticks(10)))
        );
        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!((r.witnessed(), r.diverged()), (2, 0));
        let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
        assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
        let pre_gallery = r.plan(Ticks(1_200)).expect("pre-gallery").fold().term;
        assert_eq!(
            pre_gallery.row_text(0).as_deref().map(str::trim_end),
            Some("pre-gallery shell output")
        );
        let inst = |at, command| RewindInstant {
            at: Ticks(at),
            command,
        };
        assert_eq!(
            r.rewind_instants(),
            vec![
                inst(10, false),
                inst(100, false),
                inst(1_200, false),
                inst(1_700, true),
                inst(2_000, false),
                inst(3_100, false),
            ]
        );
        assert!(r.export().is_ok());

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 2);
        for (idx, chain) in chains.iter().enumerate() {
            assert_eq!(
                chain.check(PaneCode::ThisBuild, "test").verdict,
                Verdict::Pass,
                "{idx}"
            );
            let bytes = chain
                .trace(PaneCode::ThisBuild, "test")
                .expect("trace")
                .encode();
            assert!(
                secrets
                    .iter()
                    .all(|s| !bytes.windows(s.len()).any(|w| w == s.as_slice()))
            );
            let round = PaneTrace::decode(&bytes)
                .expect("decode")
                .check(clock(0), "test")
                .expect("check");
            assert_eq!(round.verdict, Verdict::Pass);
        }
    }

    #[test]
    fn deep_unscroll_past_carried_scrollback_projects_live_keyframe_and_hot_tier_images_survive() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::{PaneCode, trace::PaneTrace};

        let mut live = Terminal::new(20, 40);
        live.set_cell_pixel_size(10, 20);
        let mut r = TemporalRecorder::with_budget(80 * MIN_REKEY_BYTES);
        r.record_keyframe(keyframe_of(&live, REKEY_SCROLLBACK_LINES).expect("seed"));
        r.spine_watermark().seed(SpinePre::take(&live));
        let (tx, rx) = queue(256);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let step = |live: &mut Terminal, mark: &mut SpineMark, bytes: &[u8], ms: u64| {
            let at = clock(ms);
            let pre = SpinePre::take(live);
            live.process_at(bytes, at);
            let reply = live.take_response().map(Arc::<[u8]>::from);
            mark.record(&tx, pre, live, InputSlice::at(bytes, at).replying(reply));
        };

        let mut bulk = Vec::with_capacity(21 * MIN_REKEY_BYTES);
        for idx in 0..2_219 {
            if idx == 700 {
                bulk.extend_from_slice(b"\x1bP0;0;0q\"1;1;10;20#1;2;100;0;0~\x1b\\");
            }
            bulk.extend_from_slice(format!("line-{idx:04}\r\n").as_bytes());
        }
        bulk.extend_from_slice(b"line-2219");
        bulk.extend(std::iter::repeat_n(0u8, 20 * MIN_REKEY_BYTES));
        step(&mut live, &mut mark, &bulk, 100);
        drain(&rx, &mut r);
        assert_eq!(r.keyframe_count(), 2);
        assert!(
            r.keyframes[1].replayed,
            "bulk crossed the byte interval so apply_inline minted a trimmed rekey"
        );

        // 102 slices * 20 lines = 2,040 lines (<= 2,048 carried): zero extra keyframes, and the
        // hot-tier Sixel image at line-0700 (rev ~ 1,500) unscrolls cleanly across the rekey.
        for s in 1..=102u64 {
            step(&mut live, &mut mark, b"\x1b[20+T", 100 + s * 20);
            drain(&rx, &mut r);
        }
        assert_eq!(r.keyframe_count(), 2);

        // Slice 103 reaches 2,060 > 2,048 lines, projecting one LiveKeyframe and re-seeding the
        // remaining 141 lines (2,200 text lines + 1 Sixel row) so slices 104..=111 replay without
        // further keyframes.
        for s in 103..=111u64 {
            step(&mut live, &mut mark, b"\x1b[20+T", 100 + s * 20);
            drain(&rx, &mut r);
        }
        assert_eq!(r.keyframe_count(), 3);
        for plan in r.chain_plans(r.latest_tick()) {
            r.judge(plan.fold().judged);
        }
        assert_eq!(r.diverged(), 0);
        let replayed = r.plan(r.latest_tick()).expect("plan").fold().term;
        assert_eq!(replayed.checkpoint_visible(), live.checkpoint_visible());
        assert_eq!(
            replayed.row_text(0).as_deref().map(str::trim_end),
            Some("line-0000")
        );

        let chains = r.pane_chains().expect("chains");
        assert_eq!(chains.len(), 3);
        for (idx, chain) in chains.iter().enumerate() {
            assert_eq!(
                chain.check(PaneCode::ThisBuild, "test").verdict,
                Verdict::Pass,
                "{idx}"
            );
            let bytes = chain
                .trace(PaneCode::ThisBuild, "test")
                .expect("trace")
                .encode();
            let round = PaneTrace::decode(&bytes)
                .expect("decode")
                .check(clock(0), "test")
                .expect("check");
            assert_eq!(round.verdict, Verdict::Pass, "{idx}");
        }
    }

    #[test]
    fn deep_unscroll_without_live_keyframe_cut_diverges_negative_control() {
        use aterm_aos::contract::Verdict;
        use aterm_aos_pane::PaneCode;

        let mut bare_live = Terminal::new(20, 40);
        let mut bulk = Vec::with_capacity(64 * 1024);
        for idx in 0..2_219 {
            bulk.extend_from_slice(format!("line-{idx:04}\r\n").as_bytes());
        }
        bulk.extend_from_slice(b"line-2219");
        bare_live.process(&bulk);
        let mut unresynced = TemporalRecorder::with_budget(8 * MIN_REKEY_BYTES);
        unresynced.record_keyframe(keyframe_of(&bare_live, REKEY_SCROLLBACK_LINES).expect("seed"));
        let deep = b"\x1b[20+T".repeat(103);
        bare_live.process_at(&deep, clock(5_000));
        unresynced.apply_inline(TemporalMsg::RawIn(InputSlice::at(&deep, clock(5_000))));
        unresynced.apply_inline(TemporalMsg::Witness(Arc::new(
            bare_live.checkpoint_visible().expect("witness"),
        )));
        let folded = unresynced
            .plan(unresynced.latest_tick())
            .expect("plan")
            .fold();
        unresynced.judge(folded.judged);
        assert_eq!(unresynced.diverged(), 1);
        assert_ne!(
            unresynced.pane_chains().expect("chains")[0]
                .check(PaneCode::ThisBuild, "test")
                .verdict,
            Verdict::Pass
        );
    }
}
