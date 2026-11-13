// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session temporal recorder.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
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

#[path = "temporal_urgency.rs"]
mod urgency;
#[cfg(test)]
#[path = "temporal_urgency_replay_tests.rs"]
mod urgency_replay_tests;
#[cfg(test)]
#[path = "temporal_urgency_tests.rs"]
mod urgency_tests;
#[cfg(test)]
#[path = "temporal_writer_tests.rs"]
mod writer_tests;
pub(crate) use urgency::Demand as RecordingDemand;

/// Keep recording fresh while an actual rewind consumer owns it.
pub(crate) fn consumer_demand() -> RecordingDemand {
    urgency::consumer()
}

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

/// An in-memory replay keyframe: its [`TerminalCheckpoint`] paired with the transient engine state no checkpoint carries ([`ReplayCarry`]), and the bytes retaining it costs ([`Self::charge`]).
#[derive(Clone, Debug)]
pub(crate) struct ReplayKeyframe {
    pub(crate) checkpoint: Arc<TerminalCheckpoint>,
    pub(crate) carry: Arc<ReplayCarry>,
    pub(crate) recorded_config: Option<Option<Arc<TerminalConfig>>>,
    charge: usize,
    shared_images: usize,
}

impl ReplayKeyframe {
    /// A keyframe of `checkpoint` and `carry`, charged once for all it holds.
    fn new(checkpoint: Arc<TerminalCheckpoint>, carry: Arc<ReplayCarry>) -> Self {
        let charge = std::mem::size_of::<RetainedKeyframe>()
            .saturating_add(charge_of(&checkpoint))
            .saturating_add(ARC_HEADER)
            .saturating_add(carry.memory_used())
            .saturating_add(carry.retained_image_bytes());
        let mut shared_images = 0usize;
        carry.for_each_retained_image(|img| {
            shared_images = shared_images.saturating_add(img.payload_bytes());
        });
        Self {
            checkpoint,
            carry,
            recorded_config: None,
            charge,
            shared_images,
        }
    }

    /// What queuing it holds beyond the engine it was taken of: [`Self::charge`] less the image payloads its carry shares with that engine ([`ReplayCarry::for_each_retained_image`]), which the queue keeps alive but does not copy ([`TemporalMsg::weight`]).
    pub(crate) const fn queue_weight(&self) -> usize {
        self.charge.saturating_sub(self.shared_images)
    }

    /// The bytes retaining this keyframe holds: its slot, its checkpoint and its carry whole, and every image payload the carry keeps alive; the replay config it shares with the recorder is not charged. A queued keyframe weighs it less the image payloads it shares with the engine it was taken of ([`Self::queue_weight`]); a recording charges each image payload its kept keyframes share once ([`SharedImages`]).
    pub(crate) const fn charge(&self) -> usize {
        self.charge
    }
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
        Self::new(checkpoint, Arc::new(ReplayCarry::default()))
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
    /// A keyframe of the live engine right after the slice queued before it, which a reader sends once a rekey is due ([`SpineMark::rekey`]): it follows that slice, so it is no instant, no export writes it and it cuts no chain.
    Rekeyframe(ReplayKeyframe),
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
    let keyframe = capture_keyframe(term, max_history)?;
    term.note_bounded_replay_keyframe(max_history);
    Some(keyframe)
}

/// [`keyframe_of`] without seeding the live grid's carried replay history from it ([`Terminal::note_bounded_replay_keyframe`]), for a reader that may not queue it: once it is queued, the reader seeds it, under the same terminal lock.
#[must_use]
fn capture_keyframe(term: &Terminal, max_history: usize) -> Option<ReplayKeyframe> {
    let mut checkpoint = term.checkpoint_continuing_bounded(max_history)?;
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
    Some(ReplayKeyframe::new(
        Arc::new(checkpoint),
        Arc::new(term.capture_replay_carry()),
    ))
}

/// A message on its way to a recorder, with the recording generation it was sent in ([`TemporalRecorder::forget`]) and the bytes it holds ([`TemporalMsg::weight`]).
struct Queued {
    completion: Completion,
    generation: u64,
    msg: TemporalMsg,
}

/// Own accounting and urgency through processing, failed send, or unwind.
struct Completion {
    route: Arc<QueueRoute>,
    weight: usize,
    pressure: Option<RecordingDemand>,
    written: bool,
    processing: bool,
}

impl Completion {
    fn begin_processing(&mut self) {
        self.processing = true;
    }

    fn written(&mut self) {
        if !self.written {
            self.route.state.written(self.weight);
            self.written = true;
            drop(self.pressure.take());
        }
    }

    fn abandon(&mut self, shared_gone: Option<&AtomicBool>) {
        // A failed temporary queue can lose its last strong reference as this
        // guard unwinds. Publish the shared writer's failure before refunding,
        // too, so a status read of the surviving queues cannot miss it.
        self.route.state.abandoned.store(true, Ordering::Release);
        if let Some(gone) = shared_gone {
            gone.store(true, Ordering::Release);
        }
        self.written();
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if self.processing && !self.written {
            let shared_gone = self.route.to.is_some().then_some(&WRITER_GONE);
            self.abandon(shared_gone);
        }
        self.written();
    }
}

/// The immutable destination and accounting of one queue, shared once per message. A drain
/// holds only the accounting state, so it cannot keep a retired recorder alive.
struct QueueRoute {
    to: Option<Arc<Mutex<TemporalRecorder>>>,
    state: Arc<QueueState>,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    costs: Option<Arc<crate::recording_cost::Costs>>,
}

/// How many messages a reader may have queued that the writer has not written yet.
pub(crate) const QUEUE_CAP: usize = 1024;

/// The fewest bytes a queue may hold unwritten, whatever its recorder's budget.
const MIN_QUEUE_BYTES: usize = 1024 * 1024;

/// The room a rekey's keyframe leaves on its queue for the reader's next slice ([`TemporalTx::fits`]): a whole read, which a reader takes 64 KiB at most of, and a reply to it of up to 16 KiB. A slice whose reply is larger (the engine owes up to 1 MiB) may still find no room.
const SLICE_ROOM: usize = 80 * 1024;

/// The share of its pool's limit every queue together may hold unwritten (a quarter).
const POOL_QUEUE_SHARE: usize = 4;

impl TemporalMsg {
    /// The bytes this message holds while queued: a slice and its reply, a witness's charge, or a keyframe's charge less the image payloads it shares with the live engine ([`ReplayKeyframe::queue_weight`]).
    fn weight(&self) -> usize {
        match self {
            Self::RawIn(input) => input.charge(),
            Self::Keyframe(k) | Self::LiveKeyframe(k, _) | Self::Rekeyframe(k) => k.queue_weight(),
            Self::Witness(c) => charge_of(c),
            Self::Reply
            | Self::Resize { .. }
            | Self::CellPx { .. }
            | Self::Host(..)
            | Self::ResizesLost(_) => 0,
        }
    }

    const fn is_keyframe(&self) -> bool {
        matches!(
            self,
            Self::Keyframe(_) | Self::LiveKeyframe(..) | Self::Rekeyframe(_)
        )
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
    let state = Arc::new(QueueState::on(pool().clone()));
    Ok(open_to(recorder, sender, state, cap))
}

fn open_to(
    recorder: &Arc<Mutex<TemporalRecorder>>,
    sender: Sender<Queued>,
    state: Arc<QueueState>,
    cap: usize,
) -> TemporalTx {
    let (spine, budget) = {
        let mut rec = recorder.lock().unwrap_or_else(PoisonError::into_inner);
        rec.queues.retain(|queue| queue.strong_count() != 0);
        rec.queues.push(Arc::downgrade(&state));
        (rec.spine_watermark(), rec.budget)
    };
    TemporalTx {
        sender,
        route: Arc::new(QueueRoute {
            to: Some(recorder.clone()),
            state: state.clone(),
            #[cfg(any(debug_assertions, feature = "dev-seams"))]
            costs: spine.costs.handle(),
        }),
        state,
        spine,
        cap,
        byte_cap: budget.max(MIN_QUEUE_BYTES),
    }
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
                if cfg!(target_os = "macos") {
                    crate::qos::Role::Background
                } else {
                    crate::qos::Role::Responsive
                },
                move || {
                    let _gone = WriterGone;
                    let demands = urgency::global().clone();
                    demands.adopt_writer(std::thread::current());
                    let role = std::rc::Rc::new(std::cell::RefCell::new(urgency::WriterRole::new(
                        demands.clone(),
                    )));
                    role.borrow_mut().poll();
                    let mut channel = WriterChannel::new(rx, pool().clone(), demands.clone());
                    channel.role = Some(role.clone());
                    serve(
                        &mut channel,
                        || demands.is_urgent(),
                        |queued| {
                            role.borrow_mut().poll();
                            let Queued {
                                mut completion,
                                generation,
                                msg,
                            } = queued;
                            if completion.route.to.is_some() {
                                completion.begin_processing();
                            }
                            if let Some(to) = &completion.route.to {
                                write_queued(
                                    to,
                                    generation,
                                    msg,
                                    #[cfg(any(debug_assertions, feature = "dev-seams"))]
                                    completion.route.costs.as_deref(),
                                );
                            }
                            completion.written();
                            completion.route.state.pool.relieve();
                            drop(completion);
                            role.borrow_mut().poll();
                        },
                    );
                },
            )
            .map(|worker| {
                urgency::global().adopt_worker(Arc::new(worker));
                tx
            })
            .map_err(|error| format!("spawn temporal writer: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// How long the shared writer naps once it has emptied a backlog ([`serve`]). A writer waiting on
/// its channel is woken by every send, so a reader streaming output wakes it once per slice, each
/// wake a context switch on the cores the reader and its PTY drain run on; napping off the channel
/// instead, it takes the slices that queued meanwhile as one batch. A new demand ends a nap at once
/// ([`urgency::Demands::acquire`]), so a drain, a view or a queue that reaches its pressure mark
/// never waits on one.
const WRITER_BATCH_NAP: std::time::Duration = std::time::Duration::from_millis(1);

/// Where the shared writer takes its messages from ([`serve`]).
trait Inbox<Q> {
    /// The next message, waiting on the channel, where a send wakes the writer; `None` once every sender is gone.
    fn wait(&mut self) -> Option<Q>;
    /// A message already queued, without waiting.
    fn take(&mut self) -> Option<Q>;
    /// Wait up to [`WRITER_BATCH_NAP`] off the channel, where a send does not wake the writer.
    fn nap(&mut self);
}

/// Write every message `inbox` brings, in the order queued. A backlog starts batching: once its
/// queue is empty the writer naps and takes what queued meanwhile, unless `urgent()`. It waits
/// on the channel after an isolated message or once a nap brought nothing, ending that batch.
fn serve<Q>(inbox: &mut impl Inbox<Q>, urgent: impl Fn() -> bool, mut write: impl FnMut(Q)) {
    while let Some(first) = inbox.wait() {
        let mut batching = false;
        let mut next = Some(first);
        while let Some(queued) = next {
            write(queued);
            next = inbox.take();
            batching |= next.is_some();
            if next.is_none() && batching && !urgent() {
                inbox.nap();
                next = inbox.take();
            }
        }
    }
}

/// How long a writer that owes work to its quiet periods waits on its channel before it does that work ([`WriterChannel`]).
const QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(20);

/// The receiving end of the shared writer's channel ([`WriterChannel`]).
trait Mailbox {
    type Msg;
    /// The next message, waiting as long as it takes; `None` once every sender is gone.
    fn recv(&mut self) -> Option<Self::Msg>;
    /// The next message, waiting at most `timeout`.
    fn recv_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Self::Msg, std::sync::mpsc::RecvTimeoutError>;
    /// A message already queued, without waiting.
    fn try_recv(&mut self) -> Option<Self::Msg>;
}

impl<Q> Mailbox for Receiver<Q> {
    type Msg = Q;

    fn recv(&mut self) -> Option<Q> {
        Receiver::recv(self).ok()
    }

    fn recv_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Q, std::sync::mpsc::RecvTimeoutError> {
        Receiver::recv_timeout(self, timeout)
    }

    fn try_recv(&mut self) -> Option<Q> {
        Receiver::try_recv(self).ok()
    }
}

/// The shared writer's channel, whose wait spends the writer's quiet periods on `pool`: a wait that owes the allocator relief once enough was freed ([`RewindPool::relieve_allocator_if_due`]), or owes a judge of closed chains ([`RewindPool::judge_due`]) while no demand stands, first waits [`QUIET_WINDOW`] for a message, and only when none came does that work ([`IdleJudge`]), stopping it for the next message or demand.
struct WriterChannel<R: Mailbox = Receiver<Queued>> {
    rx: R,
    /// The message the idle judge stopped for, the next one written.
    held: Option<R::Msg>,
    judge: IdleJudge,
    demands: Arc<urgency::Demands>,
    pool: Arc<RewindPool>,
    /// The writer thread's role, which it shares with the code that writes each message: polled before the judge runs, so the judge runs in the role no demand raises ([`urgency::WriterRole::poll`]). A test's channel has none.
    role: Option<std::rc::Rc<std::cell::RefCell<urgency::WriterRole>>>,
}

impl<R: Mailbox> WriterChannel<R> {
    fn new(rx: R, pool: Arc<RewindPool>, demands: Arc<urgency::Demands>) -> Self {
        Self {
            rx,
            held: None,
            judge: IdleJudge::default(),
            demands,
            pool,
            role: None,
        }
    }
}

impl<R: Mailbox> Inbox<R::Msg> for WriterChannel<R> {
    /// A held message first. Otherwise, while relief is due, or a judge is due and no demand stands, one wait of at most [`QUIET_WINDOW`]; on its timeout the relief, then the judge until a message or a demand comes. Then a wait as long as it takes, so a wait times out at most once, and with nothing left due waits with no timeout at all.
    fn wait(&mut self) -> Option<R::Msg> {
        if let Some(held) = self.held.take() {
            return Some(held);
        }
        let judging = self.pool.judge_due() && !self.demands.is_urgent();
        if self.pool.relief_due() || judging {
            match self.rx.recv_timeout(QUIET_WINDOW) {
                Ok(more) => return Some(more),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    self.pool.relieve_allocator_if_due();
                    if judging {
                        if let Some(role) = &self.role {
                            role.borrow_mut().poll();
                        }
                        let Self {
                            rx,
                            held,
                            judge,
                            demands,
                            pool,
                            ..
                        } = self;
                        judge.run(pool, || {
                            if held.is_none() {
                                *held = rx.try_recv();
                            }
                            held.is_some() || demands.is_urgent()
                        });
                        if let Some(held) = self.held.take() {
                            return Some(held);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        self.rx.recv()
    }

    fn take(&mut self) -> Option<R::Msg> {
        self.held.take().or_else(|| self.rx.try_recv())
    }

    fn nap(&mut self) {
        std::thread::park_timeout(WRITER_BATCH_NAP);
    }
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
    queue_in_with_demands(spine, cap, byte_cap, pool, urgency::Demands::new())
}

#[cfg(test)]
fn queue_in_with_demands(
    spine: Arc<SpineWatermark>,
    cap: usize,
    byte_cap: usize,
    pool: Arc<RewindPool>,
    demands: Arc<urgency::Demands>,
) -> (TemporalTx, TemporalRx) {
    let (sender, rx) = std::sync::mpsc::channel();
    let state = Arc::new(QueueState::with_demands(pool, demands));
    (
        TemporalTx {
            sender,
            route: Arc::new(QueueRoute {
                to: None,
                state: state.clone(),
                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                costs: spine.costs.handle(),
            }),
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
    /// The bytes ever counted on this queue, wrapping: what a reader queued within one terminal-lock hold is the difference across it ([`TemporalTx::sent`]).
    sent: AtomicUsize,
    closed: AtomicBool,
    abandoned: AtomicBool,
    pool: Arc<RewindPool>,
    demands: Arc<urgency::Demands>,
    drain_armed: AtomicBool,
    drain_demand: Mutex<Option<RecordingDemand>>,
    /// Rung each time `in_flight` falls to zero, for a park waiting out the queue
    /// (`TemporalDrain::written_by`). Its own lock, never `drain_demand`: a writer that empties
    /// the queue while a drain request holds that lock rings without waiting for it.
    emptied: (Mutex<()>, Condvar),
    /// How many times a waiter went to sleep on `emptied`.
    #[cfg(test)]
    sleeps: AtomicUsize,
    /// How many of those sleeps lasted to the waiter's deadline instead of ending at a ring.
    #[cfg(test)]
    slept_out: AtomicUsize,
}

struct QueuedCounts {
    messages: usize,
    bytes: usize,
    pool_bytes: usize,
}

impl QueueState {
    fn on(pool: Arc<RewindPool>) -> Self {
        Self::with_demands(pool, urgency::global().clone())
    }

    fn with_demands(pool: Arc<RewindPool>, demands: Arc<urgency::Demands>) -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            pool,
            demands,
            drain_armed: AtomicBool::new(false),
            drain_demand: Mutex::new(None),
            emptied: (Mutex::new(()), Condvar::new()),
            #[cfg(test)]
            sleeps: AtomicUsize::new(0),
            #[cfg(test)]
            slept_out: AtomicUsize::new(0),
        }
    }

    fn queue(&self, weight: usize) -> QueuedCounts {
        self.sent.fetch_add(weight, Ordering::AcqRel);
        QueuedCounts {
            messages: self.in_flight.fetch_add(1, Ordering::AcqRel) + 1,
            bytes: self.bytes.fetch_add(weight, Ordering::AcqRel) + weight,
            pool_bytes: self.pool.queued.fetch_add(weight, Ordering::AcqRel) + weight,
        }
    }

    fn writer_gone(&self) -> bool {
        self.abandoned.load(Ordering::Acquire) || WRITER_GONE.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn completion_snapshot(&self) -> (usize, usize, bool) {
        self.completion_snapshot_with(|| {})
    }

    #[cfg(test)]
    fn completion_snapshot_with(&self, after_counts: impl FnOnce()) -> (usize, usize, bool) {
        // An abandoned apply publishes its failure before refunding these
        // counters. Read failure last so zero cannot hide that publication.
        let messages = self.in_flight.load(Ordering::Acquire);
        let bytes = self.bytes.load(Ordering::Acquire);
        after_counts();
        (messages, bytes, self.writer_gone())
    }

    /// Release one message of `weight` bytes after processing, a failed send or abandonment.
    fn written(&self, weight: usize) {
        self.pool.queued.fetch_sub(weight, Ordering::AcqRel);
        self.bytes.fetch_sub(weight, Ordering::AcqRel);
        let last = self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1;
        if last && self.drain_armed.load(Ordering::Acquire) {
            let mut demand = self
                .drain_demand
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.in_flight.load(Ordering::Acquire) == 0 {
                drop(demand.take());
                self.drain_armed.store(false, Ordering::Release);
            }
        }
        if last {
            // Under the lock, so a waiter between its look and its sleep cannot miss the ring.
            let (lock, emptied) = &self.emptied;
            let _held = lock.lock().unwrap_or_else(PoisonError::into_inner);
            emptied.notify_all();
        }
    }

    /// The queue owns this demand until its actual completion, even if a
    /// deadline returns its caller to asynchronous resume polling.
    fn request_drain_urgency(&self) {
        self.request_drain_with(|| {});
    }

    fn request_drain_with(&self, before_arm: impl FnOnce()) {
        if self.in_flight.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut demand = self
            .drain_demand
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.in_flight.load(Ordering::Acquire) == 0 {
            return;
        }
        before_arm();
        demand.get_or_insert_with(|| self.demands.acquire(urgency::Reason::Drain));
        self.drain_armed.store(true, Ordering::Release);
        // The writer may have completed before it saw drain_armed. Reconcile
        // under this same lock so that ordering cannot leave a demand behind.
        if self.in_flight.load(Ordering::Acquire) == 0 {
            drop(demand.take());
            self.drain_armed.store(false, Ordering::Release);
        }
    }
}

/// A reader's end of its recording's queue.
#[derive(Clone)]
pub(crate) struct TemporalTx {
    sender: Sender<Queued>,
    route: Arc<QueueRoute>,
    state: Arc<QueueState>,
    spine: Arc<SpineWatermark>,
    cap: usize,
    byte_cap: usize,
}

impl TemporalTx {
    /// Queue `msg`, unless the queue holds `cap` messages, or a message other than a keyframe would take it past its bytes or every queue past the pool's share.
    ///
    /// A message that brings the queue to three quarters of `cap` messages, or the queue or the pool to half its bytes, each rounded up, holds a Pressure demand until it is written; [`Self::has_room`] stops a keyframe only past half. Every message still unwritten was counted when the newest one was, so while a live queue has no room under an unchanged pool limit, the newest message on it or on its pool holds that demand.
    fn send(&self, msg: TemporalMsg) -> bool {
        let weight = msg.weight();
        if !self.admits(weight, msg.is_keyframe()) {
            return false;
        }
        let counts = self.state.queue(weight);
        let share = self.pool_share();
        let reaches = |used: usize, cap: usize, spare: usize| used >= cap.saturating_sub(spare);
        let pressure = (reaches(counts.messages, self.cap, self.cap / 4)
            || reaches(counts.bytes, self.byte_cap, self.byte_cap / 2)
            || reaches(counts.pool_bytes, share, share / 2))
        .then(|| self.state.demands.acquire(urgency::Reason::Pressure));
        let queued = Queued {
            completion: Completion {
                route: self.route.clone(),
                weight,
                pressure,
                written: false,
                processing: false,
            },
            generation: self.spine.generation(),
            msg,
        };
        self.sender.send(queued).is_ok()
    }

    fn admits(&self, weight: usize, keyframe: bool) -> bool {
        if self.state.writer_gone()
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

    /// Whether a keyframe may be queued now: a free place, and this queue and every queue at most half their bytes, so a recording that broke for want of room keys again only once its writer has caught up. Each line it stops at lies at or past one of [`Self::send`]'s Pressure lines, so while a live queue has no room a Pressure demand stands.
    fn has_room(&self) -> bool {
        self.admits(0, true)
            && self.state.bytes.load(Ordering::Acquire) <= self.byte_cap / 2
            && self.state.pool.queued() <= self.pool_share() / 2
    }

    /// Whether a keyframe weighing `weight` may be queued now: queued, it would still leave this queue and its pool room for [`SLICE_ROOM`] more, so the slice a reader sends after it is not refused for it; or it could not even with nothing queued but `own`, what the asking reader queued in its current terminal-lock hold, so waiting for the writer would not make room for both and the keyframe goes ahead of the slice. A keyframe passes the byte caps ([`Self::admits`]), so a rekey asks this, after [`Self::has_room`], before it queues one ([`SpineMark::rekey`]).
    fn fits(&self, weight: usize, own: usize) -> bool {
        let after = |used: usize| used.saturating_add(weight).saturating_add(SLICE_ROOM);
        let (bytes, pool) = (self.byte_cap, self.pool_share());
        after(own) > bytes.min(pool)
            || (after(self.state.bytes.load(Ordering::Acquire)) <= bytes
                && after(self.state.pool.queued()) <= pool)
    }

    /// The bytes ever counted on this queue, wrapping: across a terminal-lock hold, what the holder queued ([`SpineMark::sent_from`]).
    fn sent(&self) -> usize {
        self.state.sent.load(Ordering::Acquire)
    }

    /// The recording's spine watermark, where a reader that starts on this queue attaches ([`SpineMark::attach`]).
    pub(crate) fn spine(&self) -> Arc<SpineWatermark> {
        self.spine.clone()
    }

    /// What lets lifecycle work wait until this queue is quiescent.
    pub(crate) fn drain(&self) -> TemporalDrain {
        TemporalDrain(self.state.clone())
    }
}

/// Whether the writer still holds a message from a reader's queue ([`TemporalTx::drain`]).
#[derive(Clone)]
pub(crate) struct TemporalDrain(Arc<QueueState>);

impl TemporalDrain {
    /// Whether the writer no longer holds a queued message for this session.
    /// This permits lifecycle recovery after an abandoned write; successful
    /// capture additionally requires a live writer in `writer_status`.
    pub(crate) fn is_finished(&self) -> bool {
        self.0.in_flight.load(Ordering::Acquire) == 0
    }

    /// Sleep until nothing queued is unwritten or `deadline` passes: whether the queue emptied.
    /// The writer's last write wakes the sleeper (`QueueState::written`).
    #[cfg(any(unix, test))]
    pub(crate) fn written_by(&self, deadline: Instant) -> bool {
        let (lock, emptied) = &self.0.emptied;
        let mut held = lock.lock().unwrap_or_else(PoisonError::into_inner);
        while !self.is_finished() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            #[cfg(test)]
            self.0.sleeps.fetch_add(1, Ordering::AcqRel);
            let woken = emptied
                .wait_timeout(held, left)
                .unwrap_or_else(PoisonError::into_inner);
            #[cfg(test)]
            if woken.1.timed_out() {
                self.0.slept_out.fetch_add(1, Ordering::AcqRel);
            }
            held = woken.0;
        }
        true
    }

    /// Wait until every queued message is written, or the writer is gone.
    pub(crate) fn wait(&self) {
        let _waiting = self.waiting();
        while !self.is_finished() && !self.0.writer_gone() {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }

    /// Preserve real pending drain work across deadline-based resume polling.
    pub(crate) fn request_urgency(&self) {
        self.0.request_drain_urgency();
    }

    /// A synchronous wait keeps its demand even across a transient empty queue.
    pub(crate) fn waiting(&self) -> RecordingDemand {
        self.request_urgency();
        self.0.demands.acquire(urgency::Reason::Drain)
    }

    /// A drain held unfinished until the returned guard drops, as a writer still busy holds one.
    #[cfg(test)]
    pub(crate) fn held_for_test() -> (Self, HeldDrain) {
        let state = Arc::new(QueueState::with_demands(
            RewindPool::new(usize::MAX),
            urgency::Demands::new(),
        ));
        state.in_flight.store(1, Ordering::Release);
        (Self(state.clone()), HeldDrain(state))
    }

    #[cfg(test)]
    pub(crate) fn demands_for_test(&self) -> [usize; 3] {
        self.0.demands.snapshot().counts
    }

    /// How many of this queue's waiters slept to their deadline instead of being rung awake.
    #[cfg(test)]
    pub(crate) fn slept_out_for_test(&self) -> usize {
        self.0.slept_out.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn capture_complete_for_test(&self) -> bool {
        self.0.completion_snapshot() == (0, 0, false)
    }
}

/// Finishes a [`TemporalDrain::held_for_test`] when dropped.
#[cfg(test)]
pub(crate) struct HeldDrain(Arc<QueueState>);

#[cfg(test)]
impl HeldDrain {
    pub(crate) fn abandon(self) {
        self.0.abandoned.store(true, Ordering::Release);
    }

    /// How many times a waiter went to sleep on the held queue's emptying.
    pub(crate) fn sleeps(&self) -> usize {
        self.0.sleeps.load(Ordering::Acquire)
    }
}

#[cfg(test)]
impl Drop for HeldDrain {
    fn drop(&mut self) {
        self.0.written(0);
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
        let Queued {
            mut completion,
            generation,
            msg,
        } = self.rx.try_recv().ok()?;
        completion.written();
        Some((generation, msg))
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
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    pub(crate) costs: crate::recording_cost::Owner,
    lifetime: AtomicU64,
    ordinal: AtomicU64,
    cell_px: AtomicU64,
    host_lifetime: AtomicU64,
    host_ordinal: AtomicU64,
    deep_pullback: AtomicU64,
    dropped_input: AtomicU64,
    closed: AtomicU64,
    dropped_replies: AtomicU64,
    /// Keyframes a reader has counted to send that the writer has not applied. The reader counts one before its send, so the writer never applies a keyframe ahead of its count, and uncounts it if the send is refused; the writer uncounts each one it applies. A forget stores its generation, then zeroes the count, and the writer drops a keyframe sent before it without uncounting it.
    keyframes_queued: AtomicU64,
    keyframe_wanted: AtomicBool,
    generation: AtomicU64,
    replay_config: Mutex<Option<Arc<TerminalConfig>>>,
    /// The output a chain runs before the readers key it afresh ([`SpineMark::rekey`]): a quarter of the recorder's budget, at least [`MIN_REKEY_BYTES`]. Zero on a watermark no recorder made, which keys only when the writer asks ([`Self::rekey_wanted`]).
    rekey_interval: AtomicUsize,
    /// The recorder's budget, which the pressure trigger keys under ([`Self::rekey_due`]).
    budget: AtomicUsize,
    /// The slice bytes and witness charges the readers sent since the last keyframe they sent or the recorder seeded: what the writer charges the newest chain, counted as it is sent. Written only under the terminal's lock.
    rekey_bytes: AtomicUsize,
    /// What that keyframe charges ([`ReplayKeyframe::charge`]). Written only under the terminal's lock, and by [`TemporalRecorder::seed_spine`].
    base_charge: AtomicUsize,
    /// The queue weight the next keyframe a rekey takes is expected to bring ([`ReplayKeyframe::queue_weight`]): the last one's, sent or refused, zero before the first ([`SpineMark::rekey`]). Only a prediction: [`TemporalTx::fits`] passes any weight once the queue holds no more than the asking hold queued, so a stale one never stops a rekey once the writer catches up. Written only under the terminal's lock.
    keyframe_weight: AtomicUsize,
    /// Whether a reader sent a step that changed the screen since the last witness or keyframe it sent, so no witness covers the chain's tail yet. Written only under the terminal's lock.
    tail_unwitnessed: AtomicBool,
    /// Whether the writer asks for a rekey: the newest chain ran [`REKEY_EVENTS`] events, lost its first slice or host step, or no keyframe is kept while the spine holds events ([`TemporalRecorder::after_op`]). The writer sets it only while no keyframe is in flight; a reader's keyframe clears it.
    rekey_wanted: AtomicBool,
    /// A rekey trigger a test switches off, the negative control of the trigger it tests.
    #[cfg(test)]
    disabled_trigger: Mutex<Option<RekeyTrigger>>,
}

impl SpineWatermark {
    /// The watermark of a recorder whose budget is `budget`, keying its chains every quarter of it ([`Self::rekey_interval`]).
    fn keyed(budget: usize) -> Self {
        let watermark = Self::default();
        watermark
            .rekey_interval
            .store((budget / 4).max(MIN_REKEY_BYTES), Ordering::Relaxed);
        watermark.budget.store(budget, Ordering::Relaxed);
        watermark
    }

    /// Start the watermark at `pre`, with no output counted toward a rekey and none asked for.
    fn seed(&self, pre: SpinePre) {
        self.store_host((pre.lifetime, pre.host_ordinal));
        self.store((pre.lifetime, pre.ordinal));
        self.store_cell_px(pre.cell_px);
        self.store_deep_pullback(pre.deep_pullback);
        self.keyed_afresh(0);
        self.rekey_wanted.store(false, Ordering::SeqCst);
    }

    /// Count a sent step that changed the screen, carrying `bytes` of output toward a rekey, and leave the chain's tail uncovered until the next witness.
    fn note_step(&self, bytes: usize) {
        if bytes > 0 {
            self.rekey_bytes.fetch_add(bytes, Ordering::AcqRel);
        }
        self.tail_unwitnessed.store(true, Ordering::Release);
    }

    /// Count a sent witness of `charge` bytes toward a rekey, as the writer charges it to the chain; the chain's tail is covered.
    fn note_witness(&self, charge: usize) {
        self.rekey_bytes.fetch_add(charge, Ordering::AcqRel);
        self.tail_unwitnessed.store(false, Ordering::Release);
    }

    /// A keyframe charging `charge` was sent: a fresh chain, with nothing counted toward a rekey and its tail covered.
    fn keyed_afresh(&self, charge: usize) {
        self.rekey_bytes.store(0, Ordering::Release);
        self.base_charge.store(charge, Ordering::Release);
        self.tail_unwitnessed.store(false, Ordering::Release);
        self.rekey_wanted.store(false, Ordering::SeqCst);
    }

    /// Whether the newest chain is due a rekey: the writer asked for one, or the output and witnesses sent since its keyframe reach the interval, or, under pressure, half the room its keyframe leaves it under the budget, at least [`MIN_REKEY_BYTES`], so it is keyed well before the budget takes its first slice. Pressure keys sooner the heavier the keyframe, and never once a keyframe leaves less than [`MIN_REKEY_BYTES`], which a fresh keyframe of the same charge could not keep a chain behind either.
    fn rekey_due(&self) -> bool {
        #[cfg(test)]
        let disabled = *self
            .disabled_trigger
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let wanted = self.rekey_wanted.load(Ordering::Acquire);
        #[cfg(test)]
        let wanted = wanted && disabled != Some(RekeyTrigger::Events);
        let interval = self.rekey_interval.load(Ordering::Relaxed);
        if wanted || interval == 0 {
            return wanted;
        }
        let chain = self.rekey_bytes.load(Ordering::Acquire);
        let by_bytes = chain >= interval;
        let room = self
            .budget
            .load(Ordering::Relaxed)
            .saturating_sub(self.base_charge.load(Ordering::Acquire));
        let by_pressure = room >= MIN_REKEY_BYTES && chain >= (room / 2).max(MIN_REKEY_BYTES);
        #[cfg(test)]
        let by_pressure = by_pressure && disabled != Some(RekeyTrigger::Pressure);
        by_bytes || by_pressure
    }

    /// Switch `trigger` off, the negative control of a test of it.
    #[cfg(test)]
    fn disable_trigger(&self, trigger: RekeyTrigger) {
        *self
            .disabled_trigger
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(trigger);
    }

    fn load_deep_pullback(&self) -> u64 {
        self.deep_pullback.load(Ordering::Acquire)
    }

    fn store_deep_pullback(&self, ordinal: u64) {
        self.deep_pullback.store(ordinal, Ordering::Release);
    }

    /// Uncount one queued keyframe, never below zero: a forget may have zeroed the count already.
    fn unqueue_keyframe(&self) {
        let _ = self
            .keyframes_queued
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
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
    /// The queue's [`TemporalTx::sent`] as the current terminal-lock hold began ([`Self::record`], [`Self::flush_host`]), so what the hold queued is the difference.
    sent_from: usize,
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

/// A reader's queue as a [`SpineSink`], noting whether it sent a resize, which the chain's next witness must cover ([`SpineWatermark::note_step`]).
struct TemporalSink<'a> {
    tx: &'a TemporalTx,
    resized: bool,
}

impl<'a> TemporalSink<'a> {
    const fn new(tx: &'a TemporalTx) -> Self {
        Self { tx, resized: false }
    }
}

impl SpineSink for TemporalSink<'_> {
    fn resize(&mut self, resize: SpineResize) -> bool {
        let sent = self.tx.send(TemporalMsg::Resize {
            rows: resize.to.0,
            cols: resize.to.1,
        });
        self.resized |= sent;
        sent
    }

    fn resizes_lost(&mut self, n: u64) -> bool {
        self.tx.send(TemporalMsg::ResizesLost(n))
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
            sent_from: 0,
        }
    }

    /// Whether the queue has room for the keyframe a rekey expects ([`SpineWatermark::keyframe_weight`]), asked before it captures one, so while the writer stays behind a refused rekey does not capture again each slice.
    fn room_expected(&self, tx: &TemporalTx) -> bool {
        tx.fits(
            self.shared.keyframe_weight.load(Ordering::Acquire),
            self.queued_in_hold(tx),
        )
    }

    /// What this hold queued on `tx` since it began ([`Self::sent_from`]).
    fn queued_in_hold(&self, tx: &TemporalTx) -> usize {
        tx.sent().wrapping_sub(self.sent_from)
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
        self.shared.note_step(0);
        self.advance_cell_px(cell_px);
        true
    }

    /// [`Self::catch_up`] onto `tx`, noting a resize it sent as a step the chain's next witness must cover.
    fn catch_up_queue(&mut self, tx: &TemporalTx, pre: SpinePre, term: &Terminal) -> bool {
        let mut sink = TemporalSink::new(tx);
        let caught_up = self.catch_up(&mut sink, pre, term);
        if sink.resized {
            self.shared.note_step(0);
        }
        caught_up
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
            self.shared.note_step(0);
            self.advance_host((lifetime, report.ordinal));
        }
        CatchUpHost::Ok
    }

    /// Take up where a forget put the shared watermark, when one ran since this mark last looked.
    fn follow_generation(&mut self) {
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
    }

    /// Hand `tx` the resizes, then the host mutations, `term` applied that no reader has handed the spine yet. A full queue or an overrun stops it and leaves the rest in the engine's journals: they hold no output, so nothing is lost, and the reader's next slice owes the same catch-up (or, past an overrun, a resync) before its bytes.
    fn send_pending(&mut self, tx: &TemporalTx, term: &Terminal) -> CatchUpHost {
        let seeded = self.shared.load().is_some();
        if seeded && !self.catch_up_queue(tx, SpinePre::take(term), term) {
            return CatchUpHost::QueueFull;
        }
        let sent = self.catch_up_host(tx, term);
        if seeded && matches!(sent, CatchUpHost::Ok) {
            term.note_bounded_replay_step(REKEY_SCROLLBACK_LINES);
        }
        sent
    }

    /// Hand `tx` the host mutations `term` applied (a config install among them) at the change, with no keyframe: only on a seeded spine with no open break and no deep pullback owed, each of which needs a keyframe the reader's next slice projects. It never projects a keyframe, opens a break or counts a dropped input ([`Self::send_pending`]).
    pub(crate) fn send_host_ops(&mut self, tx: &TemporalTx, term: &Terminal) {
        self.follow_generation();
        if self.broken
            || self.shared.load().is_none()
            || term.deep_history_pullback_ordinal() != self.deep_pullback_at
        {
            return;
        }
        let _ = self.send_pending(tx, term);
    }

    /// Flush any host-side mutations recorded on `term` (such as a retired drawer mark) onto `tx` when no output slice accompanies them: [`Self::send_host_ops`] then a rekey when one is due ([`Self::rekey`]), or the keyframe a break, a diverged witness, a deep pullback or an overrun owes.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn flush_host(&mut self, tx: &TemporalTx, term: &Terminal) {
        self.sent_from = tx.sent();
        self.follow_generation();
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
        match self.send_pending(tx, term) {
            CatchUpHost::Ok => {
                if self.shared.load().is_some() {
                    self.rekey(tx, term);
                }
            }
            CatchUpHost::Overrun => {
                if !self.resync(tx, term, None) {
                    self.broken = true;
                    self.shared.dropped_input.fetch_add(1, Ordering::Release);
                }
            }
            CatchUpHost::QueueFull => {}
        }
    }

    /// Whether retaining input can advance this recording. A broken recording first needs
    /// room for its recovery keyframe; until then `record` would discard its input without
    /// changing the spine, so the reader need not allocate a copy of those bytes.
    pub(crate) fn needs_input(&self, tx: &TemporalTx) -> bool {
        !self.broken || tx.has_room()
    }

    /// Record one slice on the spine with what the live screen showed after it ([`SliceMarks`]), then a live witness when one is due ([`WITNESS_BYTES`], [`WITNESS_INTERVAL`]), then a rekey when one is due ([`Self::rekey`]).
    pub(crate) fn record(
        &mut self,
        tx: &TemporalTx,
        pre: SpinePre,
        term: &Terminal,
        mut input: InputSlice,
    ) {
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        let _cost = crate::recording_cost::begin(
            tx.spine.costs.get().map(|cost| &cost.record),
            u64::try_from(input.len()).ok(),
        );
        input.marks = SliceMarks {
            sync_open: term.modes().synchronized_output(),
            command: term.started_command_seq() != pre.started,
        };
        self.sent_from = tx.sent();
        self.follow_generation();
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
        if !self.catch_up_queue(tx, pre, term) || !self.catch_up_cell_px(tx, pre.cell_px) {
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
            self.shared.note_step(len);
            self.witness(tx, term, len, at);
            self.rekey(tx, term);
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
        if self.send_witness(tx, term) {
            self.witnessed_at = Some(at);
        }
    }

    /// Queue a witness of the live screen, counting its charge toward a rekey as the writer charges it to the chain: whether it was queued.
    fn send_witness(&mut self, tx: &TemporalTx, term: &Terminal) -> bool {
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        let _cost =
            crate::recording_cost::begin(tx.spine.costs.get().map(|cost| &cost.witness), Some(1));
        let Some(live) = term.checkpoint_visible() else {
            return false;
        };
        let charge = charge_of(&live);
        if !tx.send(TemporalMsg::Witness(Arc::new(live))) {
            return false;
        }
        self.shared.note_witness(charge);
        self.since_witness = 0;
        true
    }

    /// Key the recording afresh from the live screen, under the terminal's lock the caller holds, once a rekey is due ([`SpineWatermark::rekey_due`]): first a witness of the screen when a step since the last one left the chain's tail uncovered, so every chain a rekey closes ends at a witness, then a [`TemporalMsg::Rekeyframe`], which follows the steps already queued. Without room on the queue ([`TemporalTx::has_room`], or [`TemporalTx::fits`] for the keyframe and the slice after it, asked first of the weight [`SpineWatermark::keyframe_weight`] expects ([`Self::room_expected`]), then of the keyframe taken), inside a sequence a keyframe would swallow or cut, or with history detached, it sends nothing more and the rekey stays due; a refusal opens no break. The live grid's carried replay history is seeded from the keyframe only once it is queued.
    fn rekey(&mut self, tx: &TemporalTx, term: &Terminal) {
        if !self.shared.rekey_due()
            || !tx.has_room()
            || !self.room_expected(tx)
            || term.partial_sequence_swallowed().is_some()
            || term.has_detached_scrollback()
            || !term.parser_is_ground()
        {
            return;
        }
        if self.shared.tail_unwitnessed.load(Ordering::Acquire) {
            if !self.send_witness(tx, term) {
                return;
            }
            self.witnessed_at = None;
        }
        let keyframe = {
            #[cfg(any(debug_assertions, feature = "dev-seams"))]
            let _cost =
                crate::recording_cost::begin(tx.spine.costs.get().map(|cost| &cost.rekey), Some(1));
            capture_keyframe(term, REKEY_SCROLLBACK_LINES)
        };
        let Some(mut keyframe) = keyframe else {
            return;
        };
        #[cfg(test)]
        capture_probe::captured();
        let weight = keyframe.queue_weight();
        let fits = tx.fits(weight, self.queued_in_hold(tx));
        self.shared.keyframe_weight.store(weight, Ordering::Release);
        if !fits {
            return;
        }
        let charge = keyframe.charge();
        keyframe.recorded_config = Some(self.shared.replay_config());
        self.shared.keyframes_queued.fetch_add(1, Ordering::SeqCst);
        if !tx.send(TemporalMsg::Rekeyframe(keyframe)) {
            self.shared.unqueue_keyframe();
            return;
        }
        term.note_bounded_replay_keyframe(REKEY_SCROLLBACK_LINES);
        self.shared.keyed_afresh(charge);
        self.since_witness = 0;
        self.witnessed_at = None;
    }

    /// Queue a keyframe of the live screen in place of the slice just parsed: one that closes a break (`live: None`), or a live one carrying that slice's marks. It sends whenever the queue has room for it ([`TemporalTx::has_room`]), since the recording needs it: unlike a rekey's, its keyframe may leave the next slice no room, which then breaks the recording for the next resync to close. It seeds the live grid's carried replay history from the keyframe once it is queued.
    fn resync(&mut self, tx: &TemporalTx, term: &Terminal, live: Option<SliceMarks>) -> bool {
        if !tx.has_room()
            || term.partial_sequence_swallowed().is_some()
            || term.has_detached_scrollback()
        {
            return false;
        }
        let Some(mut keyframe) = capture_keyframe(term, REKEY_SCROLLBACK_LINES) else {
            return false;
        };
        keyframe.recorded_config = Some(self.shared.replay_config());
        #[cfg(test)]
        resync_probe::projected();
        let charge = keyframe.charge();
        let msg = match live {
            None => TemporalMsg::Keyframe(keyframe),
            Some(marks) => TemporalMsg::LiveKeyframe(keyframe, marks),
        };
        self.shared.keyframes_queued.fetch_add(1, Ordering::SeqCst);
        if !tx.send(msg) {
            self.shared.unqueue_keyframe();
            return false;
        }
        term.note_bounded_replay_keyframe(REKEY_SCROLLBACK_LINES);
        self.broken = false;
        self.shared.keyed_afresh(charge);
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
    /// The image payloads the kept keyframes hold, each charged to `used` once.
    images: SharedImages,
    next_blob: u64,
    next_host_op: u64,
    next_keyframe: u64,
    used: usize,
    budget: usize,
    lost_resizes: u64,
    spine: Arc<SpineWatermark>,
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
    /// Kept witnesses no fold has judged yet ([`Self::pending`]).
    pending: u64,
    /// Witnesses an eviction dropped before any fold judged them ([`Self::unjudged_evicted`]).
    unjudged_evicted: u64,
    /// Whether the writer asked for a rekey because the newest chain no longer replays and no keyframe recorded since was kept with its chain whole ([`Self::after_op`]): a keyframe the budget evicts as it lands asks for no next one.
    lost_asked: bool,
    chain_diverged: bool,
    generation: u64,
    share: Option<Share>,
    queues: Vec<Weak<QueueState>>,
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
    /// What its snapshot was charged when stored ([`charge_of`]), refunded once a fold judges it.
    snapshot_charge: usize,
}

enum WitnessState {
    Pending(Arc<TerminalCheckpoint>),
    Matched,
    Diverged,
}

impl Witnessed {
    const fn charge(&self) -> usize {
        match &self.state {
            WitnessState::Pending(_) => self.snapshot_charge,
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

/// The bytes an `Arc`'s counts add to the value it holds.
const ARC_HEADER: usize = 2 * std::mem::size_of::<usize>();

/// The bytes a checkpoint held in an `Arc` costs: the whole of it ([`TerminalCheckpoint::memory_used`]) and the `Arc`'s counts.
fn charge_of(checkpoint: &TerminalCheckpoint) -> usize {
    checkpoint.memory_used().saturating_add(ARC_HEADER)
}

/// What a kept host op costs beyond its slot in the store: a config install holds a whole config ([`HostOp::Config`]).
const fn host_op_charge(op: &HostOp) -> usize {
    if matches!(op, HostOp::Config { .. }) {
        std::mem::size_of::<TerminalConfig>()
    } else {
        0
    }
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
        let budget = budget.max(1);
        Self {
            log: EventLog::default(),
            blobs: VecDeque::new(),
            host_ops: VecDeque::new(),
            keyframes: VecDeque::new(),
            images: SharedImages::default(),
            next_blob: 0,
            next_host_op: 0,
            next_keyframe: 0,
            used: 0,
            budget,
            lost_resizes: 0,
            spine: Arc::new(SpineWatermark::keyed(budget)),
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
            pending: 0,
            unjudged_evicted: 0,
            lost_asked: false,
            chain_diverged: false,
            generation: 0,
            share: None,
            queues: Vec::new(),
        }
    }

    /// What this recorder charges its pool: its retained bytes, the slots its log and slices hold, and its image ledger's table.
    fn charge(&self) -> usize {
        self.used
            + self.images.table_bytes()
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
        const fn wasteful(capacity: usize, len: usize) -> bool {
            capacity > 64 && capacity / 4 > len
        }
        fn shrink<T>(q: &mut VecDeque<T>) {
            if wasteful(q.capacity(), q.len()) {
                q.shrink_to_fit();
            }
        }
        shrink(&mut self.blobs);
        shrink(&mut self.host_ops);
        shrink(&mut self.keyframes);
        shrink(&mut self.witnesses);
        let images = &mut self.images.0;
        if wasteful(images.capacity(), images.len()) {
            images.shrink_to_fit();
        }
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
        self.images = SharedImages::default();
        self.witnesses = VecDeque::new();
        self.pending = 0;
        self.lost_asked = false;
        self.used = 0;
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
        self.seed_spine(SpinePre::take(term));
        self.spine.keyframe_wanted.store(false, Ordering::Release);
        true
    }

    /// Start the spine's watermark at `at` ([`SpineWatermark::seed`]) behind the newest keyframe, which the caller just recorded of the engine `at` was taken of, its charge the base the readers' pressure trigger counts from ([`SpineWatermark::rekey_due`]). The caller holds that engine's lock, or no reader runs on it yet.
    pub(crate) fn seed_spine(&self, at: SpinePre) {
        self.spine.seed(at);
        let charge = self.keyframes.back().map_or(0, RetainedKeyframe::charge);
        self.spine.base_charge.store(charge, Ordering::Release);
    }

    /// Ask the reader for a live keyframe on its next slice at parser ground.
    pub(crate) fn want_keyframe(&self) {
        self.spine.keyframe_wanted.store(true, Ordering::Release);
    }

    /// The pool's [`Self::forget`], which counts the witnesses it drops unjudged ([`Self::unjudged_evicted`]): the reader keys the recording afresh on its next slice.
    fn forget_for_pool(&mut self) {
        self.unjudged_evicted += self.pending;
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
        self.settle();
    }

    /// Mark this spine as one that starts at a seamless update ([`crate::session_timeline::HandoffGap`]).
    pub(crate) const fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// What `temporal status` appends to its line.
    #[must_use]
    pub(crate) fn status_tail(&self) -> String {
        let tail = self
            .handoff
            .map(|gap| format!(" {}", gap.payload()))
            .unwrap_or_default();
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        let tail = tail + &self.spine.costs.fields();
        tail
    }

    /// Snapshot actual queued/in-progress work, not a barrier for a live producer.
    pub(crate) fn writer_status(&self) -> String {
        let queues: Vec<_> = self.queues.iter().filter_map(Weak::upgrade).collect();
        if queues.is_empty() {
            let gone = WRITER_GONE.load(Ordering::Acquire);
            format!(
                " writer_queue=absent writer_in_flight=- writer_queued_bytes=- writer_gone={gone}{}",
                urgency::global().snapshot().fields(),
            )
        } else {
            // A reset may overlap its temporary queue with the live reader's
            // queue. Read all counts before any failure flags, preserving the
            // failure-before-refund ordering across the complete queue set.
            let (messages, bytes) = queues.iter().fold((0usize, 0usize), |(m, b), queue| {
                (
                    m.saturating_add(queue.in_flight.load(Ordering::Acquire)),
                    b.saturating_add(queue.bytes.load(Ordering::Acquire)),
                )
            });
            let gone = queues.iter().any(|queue| queue.writer_gone());
            format!(
                " writer_queue=present writer_in_flight={messages} writer_queued_bytes={bytes} writer_gone={gone}{}",
                urgency::global().snapshot().fields(),
            )
        }
    }

    /// Replay under `policy` (every recorded resize, and the replayed engine's console-host posture).
    pub(crate) const fn set_resize_policy(&mut self, policy: ResizePolicy) {
        self.resize_policy = policy;
    }

    /// Replay under the live engine's alternate-screen recovery.
    pub(crate) const fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.host_resyncs_alt_screen = on;
    }

    /// Replay under `config`, the config the live engine was given last ([`ReplayEngine::with_config`]): the config a keyframe projected from now on is recorded with. A chain recorded before it moves to it by the [`HostOp::Config`] the live engine journaled with the install, so a change asks for no keyframe.
    pub(crate) fn set_replay_config(&mut self, config: Option<Arc<TerminalConfig>>) {
        if self.replay_config.as_deref() == config.as_deref() {
            return;
        }
        self.spine.set_replay_config(config.clone());
        self.replay_config = config;
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
            // No sleep-counting boot clock here (`continuous_ns` reads none on Windows): the
            // recorder's own `Instant` is its clock, and a test's sleep runs that one on.
            _ => {
                let elapsed = self.epoch.elapsed();
                #[cfg(test)]
                let elapsed =
                    elapsed.saturating_add(std::time::Duration::from_nanos(self.slept_ns));
                elapsed.as_micros()
            }
        };
        Ticks(u64::try_from(micros).unwrap_or(u64::MAX))
    }

    /// The tick a new event takes: [`Self::now`], or one past the newest event's when the clock
    /// reads no later than it, so no two events share a tick. A writer recording a batch, as it does
    /// after a nap ([`WRITER_BATCH_NAP`]) or when it drains a backlog, can record several slices
    /// in one microsecond; on one tick they would be one Rewind state ([`Self::rewind_instants`]),
    /// so the states between them would be lost to a view. A frozen test clock stamps every event
    /// at its tick (`freeze_now_for_test`).
    fn stamp(&self) -> Ticks {
        let now = self.now();
        #[cfg(test)]
        if self.frozen_now.is_some() {
            return now;
        }
        match self.log.newest_live() {
            Some(newest) if newest.ts >= now => Ticks(newest.ts.0.saturating_add(1)),
            _ => now,
        }
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

    fn record(&mut self, op: Op) {
        let ts = self.stamp();
        self.append(op, ts);
        self.after_op();
    }

    /// After each op the writer appends: hold the budget, then ask the readers for a rekey ([`SpineWatermark::rekey_wanted`]) while no keyframe is in flight and the newest chain ran [`REKEY_EVENTS`] events, or, once until a keyframe is kept with its chain whole ([`Self::lost_asked`]), lost its first slice or host step, or no keyframe is kept while the spine holds events. A keyframe the budget cannot keep is evicted as it lands, so asking again for each would key every slice.
    fn after_op(&mut self) {
        self.enforce_budget();
        let in_flight = || self.spine.keyframes_queued.load(Ordering::SeqCst) != 0;
        if in_flight() {
            return;
        }
        let lost = match self.wants_rekey() {
            None => return,
            Some(true) if self.lost_asked => return,
            Some(lost) => lost,
        };
        self.spine.rekey_wanted.store(true, Ordering::SeqCst);
        #[cfg(test)]
        if ask_probe::races() {
            self.spine.keyframes_queued.fetch_add(1, Ordering::SeqCst);
        }
        // A reader that counted a keyframe since keys the chain afresh, and may already have cleared the request: take it back, so it never outlives a keyframe already sent. A loss is asked for again after the next op should that keyframe not be sent after all.
        if in_flight() {
            self.spine.rekey_wanted.store(false, Ordering::SeqCst);
            return;
        }
        self.lost_asked |= lost;
    }

    /// Whether the newest chain needs a reader's keyframe the readers do not count toward, and whether for a loss: `Some(true)` when it lost its first slice or host step, so it no longer replays, or no keyframe is kept while the spine holds events; `Some(false)` when it ran [`REKEY_EVENTS`] events, which the spine's ring would evict it after.
    fn wants_rekey(&self) -> Option<bool> {
        match self.keyframes.back() {
            Some(_) if !self.newest_replays() => Some(true),
            Some(newest) => {
                (self.log.head().0.saturating_sub(newest.seq.0) >= REKEY_EVENTS).then_some(false)
            }
            None => self.log.newest_live().is_some().then_some(true),
        }
    }

    /// Whether the newest keyframe is kept with its chain's first slice and host step.
    fn newest_replays(&self) -> bool {
        self.keyframes.back().is_some_and(|newest| {
            newest.first_blob >= self.blob_base() && newest.first_host_op >= self.host_op_base()
        })
    }

    fn witness(&mut self, live: Arc<TerminalCheckpoint>) {
        if self.keyframes.is_empty() || self.chain_diverged {
            return;
        }
        let ts = self.stamp();
        let id = WitnessId(self.next_witness);
        self.next_witness += 1;
        let seq = self.append(Op::Witness(id), ts);
        let charge = charge_of(&live);
        self.used += charge;
        self.witnesses.push_back(Witnessed {
            id,
            seq,
            ts,
            screen: screen_digest(&live),
            state: WitnessState::Pending(live),
            snapshot_charge: charge,
        });
        self.pending += 1;
        self.after_op();
    }

    /// Evict the witnesses at or before `gone`, counting each one no fold judged ([`Self::unjudged_evicted`]).
    fn drop_witnesses_through(&mut self, gone: Seq) {
        while let Some(w) = self.witnesses.front().filter(|w| w.seq <= gone) {
            self.used = self.used.saturating_sub(w.charge());
            if matches!(w.state, WitnessState::Pending(_)) {
                self.pending = self.pending.saturating_sub(1);
                self.unjudged_evicted += 1;
            }
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

    /// Record the verdicts a fold reached ([`ReplayPlan::fold`]) on witnesses still pending, taking each out of [`Self::pending`] and refunding its snapshot to the budget; a mismatch in the newest keyframe's chain asks the reader for a live keyframe and keeps that chain from taking another witness.
    pub(crate) fn judge(&mut self, judged: Judged) {
        let newest = self.keyframes.back().map(|kf| kf.seq);
        for (id, matched) in judged.0 {
            let Some(w) = self.witness_mut(id) else {
                continue;
            };
            let charge = w.charge();
            if !matches!(w.state, WitnessState::Pending(_)) {
                continue;
            }
            w.state = if matched {
                WitnessState::Matched
            } else {
                WitnessState::Diverged
            };
            let in_newest_chain = newest.is_some_and(|kf| w.seq > kf);
            self.pending = self.pending.saturating_sub(1);
            self.used = self.used.saturating_sub(charge);
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
            .get(self.keyframe_after(base))
            .map(|kf| kf.seq);
        self.witnesses
            .range(self.witnesses_after(base)..)
            .take_while(move |w| end.is_none_or(|end| w.seq < end))
    }

    /// The index of the first retained keyframe after spine event `seq`, or `keyframes.len()` when none is: the keyframes are in seq order.
    fn keyframe_after(&self, seq: Seq) -> usize {
        #[cfg(test)]
        if lookup_scan::linear() {
            return self
                .keyframes
                .iter()
                .position(|kf| kf.seq > seq)
                .unwrap_or(self.keyframes.len());
        }
        self.keyframes.partition_point(|kf| kf.seq <= seq)
    }

    /// The retained keyframe spine event `seq` records, if it is retained.
    fn keyframe_at(&self, seq: Seq) -> Option<&RetainedKeyframe> {
        #[cfg(test)]
        if lookup_scan::linear() {
            return self.keyframes.iter().find(|kf| kf.seq == seq);
        }
        let idx = self
            .keyframes
            .binary_search_by_key(&seq, |kf| kf.seq)
            .ok()?;
        self.keyframes.get(idx)
    }

    /// The index of the first witness after spine event `seq`, or `witnesses.len()` when none is: the witnesses are in seq order.
    fn witnesses_after(&self, seq: Seq) -> usize {
        #[cfg(test)]
        if lookup_scan::linear() {
            return self
                .witnesses
                .iter()
                .position(|w| w.seq > seq)
                .unwrap_or(self.witnesses.len());
        }
        self.witnesses.partition_point(|w| w.seq <= seq)
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

    /// Kept witnesses no fold has judged yet (`temporal status` `pending=`): each is charged to the budget until a fold judges it or an eviction drops it.
    #[must_use]
    pub(crate) const fn pending(&self) -> u64 {
        self.pending
    }

    /// Witnesses the spine's ring, the budget, the pool's shedding or the pool's forget dropped before any fold judged them (`temporal status` `unjudged_evicted=`); a forget the person asked for drops its witnesses uncounted.
    #[must_use]
    pub(crate) const fn unjudged_evicted(&self) -> u64 {
        self.unjudged_evicted
    }

    /// Record a keyframe (a serialized [`TerminalCheckpoint`]), the seed for replays to instants at or after it.
    pub(crate) fn record_keyframe(&mut self, keyframe: impl Into<ReplayKeyframe>) {
        self.record_keyframe_inner(keyframe.into(), false, false, false);
        self.settle();
    }

    fn record_keyframe_with_marks(&mut self, keyframe: ReplayKeyframe, command: bool) {
        self.record_keyframe_inner(keyframe, false, command, true);
    }

    /// [`Self::record_keyframe_as`] under the config the reader projected `keyframe` under, or the recorder's when it names none; a config that has moved since asks the reader for a live keyframe under the new one.
    fn record_keyframe_inner(
        &mut self,
        mut keyframe: ReplayKeyframe,
        follows: bool,
        command: bool,
        coalesceable: bool,
    ) {
        let config = keyframe
            .recorded_config
            .take()
            .unwrap_or_else(|| self.replay_config.clone());
        self.record_keyframe_as(keyframe, follows, command, coalesceable, config);
    }

    fn has_chain_steps_after(&self, seq: Seq) -> bool {
        self.log.live_after(seq).any(|ev| {
            matches!(
                ev.op,
                Op::RawIn(_) | Op::Resize { .. } | Op::CellPx { .. } | Op::Host(_) | Op::Witness(_)
            )
        })
    }

    /// [`Self::record_keyframe`], marked as following the slice before it (a reader's rekey, [`TemporalMsg::Rekeyframe`]) or standing in for it, as standing in for a slice that started a command, and its chain replaying under `config`. When it closes a chain that still holds a pending witness, it tells the pool the recorder is charged to that a judge is due ([`RewindPool::judge_due`]).
    fn record_keyframe_as(
        &mut self,
        keyframe: ReplayKeyframe,
        follows: bool,
        command: bool,
        coalesceable: bool,
        config: Option<Arc<TerminalConfig>>,
    ) {
        let closes_pending = self
            .keyframes
            .back()
            .is_some_and(|prev| self.holds_pending(prev.seq));
        let ts = self.stamp();
        let id = KeyframeId(self.next_keyframe);
        self.next_keyframe += 1;
        let seq = self.append(Op::Keyframe(id), ts);
        let mut command = command;
        let mut coalesced = None;
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
            self.used = self.used.saturating_sub(prev.own_charge());
            command |= prev.command;
            coalesced = Some(prev);
        }
        let retained_bytes = keyframe.charge;
        let (image_bytes, charged_images) = self.images.hold(&keyframe.carry);
        let keyframe = RetainedKeyframe {
            #[cfg(test)]
            id,
            seq,
            ts,
            first_blob: self.next_blob,
            first_host_op: self.next_host_op,
            cut: None,
            follows,
            coalesceable,
            command,
            packed: false,
            retained_bytes,
            image_bytes,
            checkpoint: keyframe.checkpoint,
            carry: keyframe.carry,
            config,
        };
        self.used += keyframe.own_charge() + charged_images;
        self.keyframes.push_back(keyframe);
        // The keyframe it replaces lets go of its images only after this one holds them, so an image both hold is neither refunded and charged again nor counted as freed.
        let freed_keyframe = coalesced.map_or(0, |prev| {
            let refunded = self.images.release(&prev.carry);
            self.used = self.used.saturating_sub(refunded);
            prev.own_charge() + refunded
        });
        self.chain_diverged = false;
        if closes_pending && let Some(share) = &self.share {
            share.pool.judge_due.store(true, Ordering::Release);
        }
        self.enforce_budget();
        self.lost_asked &= !self.newest_replays();
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
        self.record_keyframe(keyframe);
        self.spine.closed.fetch_add(1, Ordering::Release);
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
        self.used += host_op_charge(&op);
        self.host_ops.push_back((op, clock));
        id
    }

    fn host_op_base(&self) -> u64 {
        self.next_host_op - self.host_ops.len() as u64
    }

    fn pop_host_op(&mut self) -> bool {
        let Some((op, _)) = self.host_ops.pop_front() else {
            return false;
        };
        self.used = self.used.saturating_sub(host_op_charge(&op));
        true
    }

    fn host_op(&self, id: HostOpId) -> Option<&(HostOp, Option<ClockReading>)> {
        let idx = usize::try_from(id.0.checked_sub(self.host_op_base())?).ok()?;
        self.host_ops.get(idx)
    }

    fn pop_keyframe(&mut self) -> bool {
        let Some(keyframe) = self.keyframes.pop_front() else {
            return false;
        };
        let refunded = self.images.release(&keyframe.carry);
        self.used = self.used.saturating_sub(keyframe.own_charge() + refunded);
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
    pub(crate) fn apply(&mut self, msg: TemporalMsg) {
        self.apply_op(msg);
        self.settle();
    }

    /// [`Self::apply`] for a message sent in recording generation `generation`: one sent before a forget is dropped, a break it closes still counted.
    fn apply_in(&mut self, generation: u64, msg: TemporalMsg) {
        if generation == self.generation {
            self.apply(msg);
        } else if let TemporalMsg::Keyframe(_) = msg {
            self.spine.closed.fetch_add(1, Ordering::Release);
        }
    }

    fn apply_op(&mut self, msg: TemporalMsg) {
        let op = match msg {
            TemporalMsg::RawIn(input) => Op::RawIn(self.store_blob(Some(input))),
            TemporalMsg::Reply => Op::Reply(self.store_blob(None)),
            TemporalMsg::Resize { rows, cols } => Op::Resize { rows, cols },
            TemporalMsg::CellPx { width, height } => Op::CellPx { width, height },
            TemporalMsg::Host(op, clock) => Op::Host(self.store_host_op(op, clock)),
            TemporalMsg::ResizesLost(n) => {
                self.lost_resizes = self.lost_resizes.saturating_add(n);
                return;
            }
            TemporalMsg::Keyframe(checkpoint) => {
                self.spine.unqueue_keyframe();
                self.close_break(checkpoint);
                return;
            }
            TemporalMsg::LiveKeyframe(keyframe, marks) => {
                self.spine.unqueue_keyframe();
                self.record_keyframe_with_marks(keyframe, marks.command);
                return;
            }
            TemporalMsg::Rekeyframe(keyframe) => {
                self.spine.unqueue_keyframe();
                self.record_keyframe_inner(keyframe, true, false, false);
                return;
            }
            TemporalMsg::Witness(live) => {
                self.witness(live);
                return;
            }
        };
        self.record(op);
    }

    /// Record a raw PTY-input burst (one copy into a shared allocation).
    #[cfg(test)]
    pub(crate) fn record_raw_in(&mut self, bytes: &[u8]) {
        self.apply(TemporalMsg::RawIn(InputSlice::at(
            bytes,
            ClockReading::now(),
        )));
    }

    /// Record a raw PTY-input burst, retaining the caller's allocation.
    #[cfg(test)]
    pub(crate) fn record_raw_in_shared(&mut self, bytes: Arc<[u8]>) {
        let range = 0..bytes.len();
        self.apply(TemporalMsg::RawIn(InputSlice::new(
            bytes,
            range,
            ClockReading::now(),
        )));
    }

    /// Record a reply's instant.
    #[cfg(test)]
    pub(crate) fn record_reply(&mut self) {
        self.apply(TemporalMsg::Reply);
    }

    /// Record a geometry change.
    #[cfg(test)]
    pub(crate) fn record_resize(&mut self, rows: u16, cols: u16) {
        self.apply(TemporalMsg::Resize { rows, cols });
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

    /// The retained keyframe of greatest seq whose tick is at or before `at`: the keyframes' ticks never fall in seq order (temporal-spine-monotone).
    fn base_keyframe(&self, at: Ticks) -> Option<&RetainedKeyframe> {
        #[cfg(test)]
        if lookup_scan::linear() {
            return self.keyframes.iter().rev().find(|kf| kf.ts <= at);
        }
        let after = self.keyframes.partition_point(|kf| kf.ts <= at);
        self.keyframes.get(after.checked_sub(1)?)
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
            .get(self.keyframe_after(base.seq))
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
                    let (op, clock) = self.host_op(id)?.clone();
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
            stands_in: !base.follows,
            config: base.config.clone(),
        })
    }

    /// The replay of each replayable chain to `at`'s, oldest first, the last folding to `at`, or on to the witness that covers `at` ([`Self::covering_witness`]) where one does, so a search that reads instants only to `at` still passes the witness its replay is judged by ([`App::rewind_changed`](crate::App)).
    #[must_use]
    pub(crate) fn chain_plans(&self, at: Ticks) -> Vec<ReplayPlan> {
        let Some(oldest) = self.oldest_replayable().map(|kf| kf.seq) else {
            return Vec::new();
        };
        let Some(last) = self.base_keyframe(at).map(|kf| kf.seq) else {
            return Vec::new();
        };
        let to = self.covering_witness(at).map_or(at, |w| w.max(at));
        self.keyframes
            .iter()
            .filter(|kf| kf.seq >= oldest && kf.seq <= last)
            .filter_map(|kf| self.plan_from(kf, if kf.seq == last { to } else { Ticks(u64::MAX) }))
            .collect()
    }

    /// The whole replay of the replayable chain before `at`'s, which a view at `at` may step back into.
    #[must_use]
    pub(crate) fn chain_before(&self, at: Ticks) -> Option<ReplayPlan> {
        let oldest = self.oldest_replayable()?.seq;
        let last = self.base_keyframe(at)?.seq;
        #[cfg(test)]
        if lookup_scan::linear() {
            let before = self
                .keyframes
                .iter()
                .rev()
                .find(|kf| kf.seq >= oldest && kf.seq < last)?;
            return self.plan_from(before, Ticks(u64::MAX));
        }
        // `last` is retained, so the keyframe before it sits two below the first index after it.
        let before = self
            .keyframes
            .get(self.keyframe_after(last).checked_sub(2)?)
            .filter(|kf| kf.seq >= oldest)?;
        self.plan_from(before, Ticks(u64::MAX))
    }

    /// The whole replay of every kept chain that holds a pending witness, newest first, the newest chain included: what [`judge_all`] folds.
    #[must_use]
    pub(crate) fn judge_plans(&self) -> Vec<ReplayPlan> {
        self.keyframes
            .iter()
            .rev()
            .filter(|kf| self.holds_pending(kf.seq))
            .filter_map(|kf| self.plan_from(kf, Ticks(u64::MAX)))
            .collect()
    }

    /// The whole replay of the newest closed chain (one a later keyframe ends) that holds a pending witness: what the shared writer judges next when idle ([`IdleJudge`]). The newest chain still grows, so only a fold on demand judges it ([`Self::judge_plans`]).
    #[must_use]
    pub(crate) fn judge_plan(&self) -> Option<ReplayPlan> {
        let closed = self.keyframes.len().checked_sub(1)?;
        self.keyframes
            .iter()
            .take(closed)
            .rev()
            .filter(|kf| self.holds_pending(kf.seq))
            .find_map(|kf| self.plan_from(kf, Ticks(u64::MAX)))
    }

    /// Whether keyframe `base`'s chain holds a witness no fold has judged.
    fn holds_pending(&self, base: Seq) -> bool {
        self.chain_witnesses(base)
            .any(|w| matches!(w.state, WitnessState::Pending(_)))
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
        #[cfg(test)]
        if lookup_scan::linear() {
            return instants
                .iter()
                .map(|i| {
                    self.base_keyframe(i.at)
                        .and_then(|kf| self.diverged_from(kf))
                        .is_some_and(|f| i.at >= f)
                })
                .collect();
        }
        // One cursor over the keyframes, as the instants ascend: each chain's verdict is read once.
        let mut after = 0;
        let mut from = None;
        instants
            .iter()
            .map(|i| {
                let was = after;
                while self.keyframes.get(after).is_some_and(|kf| kf.ts <= i.at) {
                    after += 1;
                }
                if after != was {
                    from = self
                        .keyframes
                        .get(after - 1)
                        .and_then(|kf| self.diverged_from(kf));
                }
                from.is_some_and(|f| i.at >= f)
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

    /// The distinct instants a Rewind view steps through, oldest first: the oldest keyframe every later instant replays from, then each later tick a slice, resize, screen-cutting host op or live keyframe lands on, less each the live screen showed inside an open mode-2026 frame (its command start goes to the next kept instant) and each empty slice that ended no frame. A run of recolouring config installs with nothing else starting a state between them (a palette browse, a preview) is one state, at the last of them.
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
        // Whether a config install started the open state and nothing else joined it.
        let mut open_by_config = false;
        for ev in self.log.live_after(base.seq) {
            let mut config_cut = false;
            let (marks, starts) = match ev.op {
                Op::RawIn(id) => {
                    let slice = self.blob(id);
                    let ends_frame = sync_open && slice.is_some_and(|s| !s.marks().sync_open);
                    let starts = ends_frame || slice.is_none_or(|s| !s.bytes().is_empty());
                    (slice.map(InputSlice::marks), starts)
                }
                Op::Resize { .. } => (None, true),
                Op::Host(id) => match self.host_op(id) {
                    Some((op, _)) if op.is_screen_cut() => {
                        config_cut = matches!(op, HostOp::Config { .. });
                        (
                            matches!(op, HostOp::Reset | HostOp::LeaveAlternateScreen)
                                .then_some(SliceMarks::default()),
                            true,
                        )
                    }
                    _ => continue,
                },
                Op::Keyframe(_) => {
                    let Some(kf) = self.keyframe_at(ev.seq) else {
                        continue;
                    };
                    let marks = Some(SliceMarks {
                        sync_open: kf.checkpoint.modes.synchronized_output(),
                        command: kf.command,
                    });
                    (marks, !kf.follows)
                }
                _ => continue,
            };
            let later = open.is_none_or(|o| ev.ts > o.at);
            if later && config_cut && open_by_config {
                if let Some(o) = open.as_mut() {
                    o.at = ev.ts;
                }
            } else if later {
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
            open_by_config = open.is_some()
                && match (later, starts) {
                    (true, _) => config_cut,
                    (false, true) => open_by_config && config_cut,
                    (false, false) => open_by_config,
                };
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
                .get(self.keyframe_after(kf.seq))
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
                    if let Some(kf) = self.keyframe_at(ev.seq).filter(|kf| !kf.follows) {
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
                    Op::Host(id) => self.host_op(id).and_then(|(_, clock)| *clock),
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
                        let (op, clock) = self.host_op(id)?.clone();
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
    /// Whether it is the live engine just after the slice before it (a reader's rekey, [`TemporalMsg::Rekeyframe`]) rather than standing in for that slice: every slice before it is on the spine, so it is no instant, no export writes it and it cuts no chain.
    follows: bool,
    /// Whether this keyframe came from a live in-band resync ([`TemporalMsg::LiveKeyframe`]) and may coalesce with an immediately following one when no chain steps fell between them.
    coalesceable: bool,
    /// Whether the slice a live keyframe stands in for started a command ([`SliceMarks::command`]).
    command: bool,
    /// Whether unshared small slices in this completed chain were packed into slabs.
    packed: bool,
    /// What the keyframe pins whole ([`ReplayKeyframe::charge`]), its image payloads included.
    retained_bytes: usize,
    /// The payload bytes of the distinct images its carry keeps alive, which the recording charges through [`SharedImages`], once however many keyframes hold one.
    image_bytes: usize,
    checkpoint: Arc<TerminalCheckpoint>,
    /// The in-process transient state no checkpoint carries ([`ReplayCarry`]).
    carry: Arc<ReplayCarry>,
    /// The config its chain replays under ([`TemporalRecorder::set_replay_config`]).
    config: Option<Arc<TerminalConfig>>,
}

impl RetainedKeyframe {
    /// What it pins whole, its images included: what its chain holds once every older chain is gone, the base the readers' pressure trigger counts from ([`TemporalRecorder::seed_spine`]).
    const fn charge(&self) -> usize {
        self.retained_bytes
    }

    /// What `used` charges it alone: [`Self::charge`] less its images, refunded when it goes.
    const fn own_charge(&self) -> usize {
        self.retained_bytes.saturating_sub(self.image_bytes)
    }
}

/// The image payloads the kept keyframes hold, each charged to the recording once however many of them share it: how many kept keyframes hold each, by the address of its `Arc` ([`ReplayCarry::for_each_retained_image`]). An address names one image while its entry lives, because an entry lives only while a kept keyframe holds that `Arc`, so its allocation cannot be reused.
#[derive(Default)]
struct SharedImages(HashMap<usize, usize>);

impl SharedImages {
    /// Count one more holder of each image `carry` keeps alive. Returns their payload bytes, and of those the bytes of the images no kept keyframe held before, which the recording charges now.
    fn hold(&mut self, carry: &ReplayCarry) -> (usize, usize) {
        let (mut held, mut charged) = (0usize, 0usize);
        carry.for_each_retained_image(|img| {
            let bytes = img.payload_bytes();
            held = held.saturating_add(bytes);
            let holders = self.0.entry(Arc::as_ptr(img).addr()).or_insert(0);
            if *holders == 0 {
                charged = charged.saturating_add(bytes);
            }
            *holders += 1;
        });
        (held, charged)
    }

    /// Count one holder fewer of each image `carry` keeps alive, as a keyframe [`Self::hold`] counted goes. Returns the payload bytes of the images no kept keyframe holds now, which the recording refunds.
    fn release(&mut self, carry: &ReplayCarry) -> usize {
        let mut refunded = 0usize;
        carry.for_each_retained_image(|img| {
            let key = Arc::as_ptr(img).addr();
            let holders = self.0.get_mut(&key);
            debug_assert!(
                holders.is_some(),
                "a keyframe let go of an image the ledger never counted it holding"
            );
            let Some(holders) = holders else {
                return;
            };
            *holders -= 1;
            if *holders == 0 {
                self.0.remove(&key);
                refunded = refunded.saturating_add(img.payload_bytes());
            }
        });
        refunded
    }

    /// The bytes its table allocates, which the recorder charges its pool apart from `used` ([`TemporalRecorder::charge`]).
    fn table_bytes(&self) -> usize {
        aterm_alloc::heap::table_bytes::<(usize, usize)>(self.0.capacity())
    }
}

impl Default for TemporalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// The writer thread's step for one message a reader sent in recording generation `generation`: apply it under the recorder's lock ([`TemporalRecorder::apply_in`]).
fn write_queued(
    recorder: &Mutex<TemporalRecorder>,
    generation: u64,
    msg: TemporalMsg,
    #[cfg(any(debug_assertions, feature = "dev-seams"))] costs: Option<
        &crate::recording_cost::Costs,
    >,
) {
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    let _cost = crate::recording_cost::begin(costs.map(|cost| &cost.apply), Some(1));
    recorder
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .apply_in(generation, msg);
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
    /// Set when a member records a keyframe that closes a chain still holding a pending witness ([`TemporalRecorder::record_keyframe_as`]); cleared by the shared writer's [`IdleJudge`] once no member has a closed chain left to judge.
    judge_due: AtomicBool,
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
            judge_due: AtomicBool::new(false),
        })
    }

    /// Whether a member's closed chain may hold a pending witness the shared writer has yet to judge ([`IdleJudge`]).
    fn judge_due(&self) -> bool {
        self.judge_due.load(Ordering::Acquire)
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
    /// Whether the base keyframe stands in for the slice before it, so past the oldest chain the spine keeps no bytes of that slice.
    stands_in: bool,
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
                    replay.apply_host_op(op.clone(), *clock);
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

    /// Fold every step onto `replay` (the base restored), calling `before` ahead of each slice or resize with the engine as the steps before it left it, while it says to go on, and `witness` at each pending witness with its tick and whether it differs from the replay in more than a bounded history renumbers ([`unnumbered`], as [`Self::fold_onto`] reads it); whether every step was folded.
    pub(crate) fn fold_before(
        &self,
        replay: &mut ReplayEngine,
        mut before: impl FnMut(Passed<'_>, &ReplayEngine) -> bool,
        mut witness: impl FnMut(Ticks, bool),
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
                    replay.apply_host_op(op.clone(), *clock);
                    continue;
                }
                Step::Host(..) => (None, None),
                Step::Witness(_, live) => {
                    let differs = replay
                        .terminal()
                        .checkpoint_visible()
                        .is_none_or(|seen| unnumbered(seen) != unnumbered((**live).clone()));
                    witness(placed.ts, differs);
                    continue;
                }
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
                Step::Host(op, clock) => replay.apply_host_op(op.clone(), *clock),
                Step::CellPx { .. } | Step::Witness(..) => {}
            }
        }
        true
    }

    /// The base restored onto an engine that keeps its whole history, as [`Self::fold`] restores it, for a fold whose verdicts count.
    pub(crate) fn restore(&self) -> ReplayEngine {
        self.configured(ReplayEngine::from_checkpoint(&self.base, self.policy))
    }

    /// Fold the steps after `after` onto `replay` (from [`Self::restore`], or as an earlier call left it) until `budget` bytes of work are done, judging each pending witness it passes as [`Self::fold`] does, up to the next step that is not a witness: the verdicts, the seq of the last step applied, the bytes of slices applied, and whether steps are left. A slice works its bytes, a resize, which reflows the whole history, [`RESIZE_WORK`], and any other step [`STEP_WORK`], so a chain of resizes or host steps yields as a chain of slices does.
    pub(crate) fn judge_onto(
        &self,
        replay: &mut ReplayEngine,
        after: Seq,
        budget: usize,
    ) -> (Judged, Seq, usize, bool) {
        let mut judged = Vec::new();
        let (mut last, mut folded, mut work) = (after, 0usize, 0usize);
        for placed in self.steps.iter().filter(|p| p.seq > after) {
            if work >= budget && !matches!(placed.step, Step::Witness(..)) {
                return (Judged(judged), last, folded, true);
            }
            match &placed.step {
                #[cfg(test)]
                Step::RawIn(input) if replay_clock::live() => {
                    replay.process(input.bytes());
                    folded += input.len();
                    work += input.len();
                }
                Step::RawIn(input) => {
                    replay.process_at(input.bytes(), input.clock);
                    folded += input.len();
                    work += input.len();
                }
                Step::Resize { rows, cols } => {
                    replay.resize(*rows, *cols);
                    work += RESIZE_WORK;
                }
                Step::CellPx { width, height } => {
                    replay.set_cell_pixel_size(*width, *height);
                    work += STEP_WORK;
                }
                Step::Host(op, clock) => {
                    replay.apply_host_op(op.clone(), *clock);
                    work += STEP_WORK;
                }
                Step::Witness(id, live) => {
                    let replayed = replay.terminal().checkpoint_visible();
                    judged.push((*id, replayed.as_ref() == Some(&**live)));
                    work += STEP_WORK;
                }
            }
            last = placed.seq;
        }
        (Judged(judged), last, folded, false)
    }

    /// Whether the base keyframe stands in for the slice before it, so past the oldest chain the spine keeps no bytes of that slice.
    pub(crate) const fn stands_in(&self) -> bool {
        self.stands_in
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

/// Output folded between the forks a view's cache keeps, and the most a judging pass folds before its caller looks for other work.
pub(crate) const FORK_EVERY: usize = 64 * 1024;

/// The work a judging fold books for a resize ([`ReplayPlan::judge_onto`]): a pass's whole budget, since a resize reflows the whole history the judging engine keeps.
const RESIZE_WORK: usize = FORK_EVERY;

/// The work a judging fold books for a step that is neither a slice nor a resize ([`ReplayPlan::judge_onto`]).
const STEP_WORK: usize = 1024;

/// A judging fold of one chain on an engine that keeps its whole history, as [`ReplayPlan::fold`] restores it, so its verdicts count: [`FORK_EVERY`] bytes of slices a pass ([`ReplayPlan::judge_onto`]). It holds the plan, not the recorder, so each pass runs with the recorder unlocked.
pub(crate) struct ChainJudge {
    plan: ReplayPlan,
    engine: ReplayEngine,
    /// The seq of the last step a pass applied, or the base's before the first.
    at: Seq,
}

impl ChainJudge {
    /// A judge of `plan`, its base restored ([`ReplayPlan::restore`]).
    pub(crate) fn new(plan: ReplayPlan) -> Self {
        #[cfg(test)]
        replay_hook::before_replay();
        Self {
            engine: plan.restore(),
            at: plan.base_seq(),
            plan,
        }
    }

    /// Judge `plan` too: on `judge`'s chain, carry its fold on to the further witness; on another, or with none, start over. Whether it started over.
    pub(crate) fn extend(judge: &mut Option<Self>, plan: ReplayPlan) -> bool {
        match judge.as_mut() {
            Some(j) if j.plan.base_seq() == plan.base_seq() => {
                if plan.end_seq() > j.plan.end_seq() {
                    j.plan = plan;
                }
                false
            }
            _ => {
                *judge = Some(Self::new(plan));
                true
            }
        }
    }

    /// One pass: the verdicts it reached, the bytes of slices it folded and whether steps are left.
    pub(crate) fn step(&mut self) -> (Judged, usize, bool) {
        let (judged, last, folded, more) =
            self.plan.judge_onto(&mut self.engine, self.at, FORK_EVERY);
        self.at = last;
        (judged, folded, more)
    }

    /// The memory its engine holds ([`engine_bytes`]), which no pool is charged.
    pub(crate) fn engine_bytes(&self) -> usize {
        engine_bytes(&self.engine)
    }
}

/// The memory `engine` holds: both grids and their stores.
/// Unbooked Kitty store and visible-cell image payloads are added on top.
pub(crate) fn engine_bytes(engine: &ReplayEngine) -> usize {
    let term = engine.terminal();
    term.grid()
        .memory_used()
        .saturating_add(
            term.inactive_grid()
                .map_or(0, aterm_core::grid::Grid::memory_used),
        )
        .saturating_add(term.unbooked_image_bytes())
}

/// What [`judge_all`] did: the witnesses it judged and how many of them diverged, then the recorder's [`TemporalRecorder::pending`] and [`TemporalRecorder::unjudged_evicted`] after it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct JudgeCounts {
    pub(crate) judged: u64,
    pub(crate) diverged: u64,
    pub(crate) pending: u64,
    pub(crate) unjudged_evicted: u64,
}

/// Judge every kept chain that holds a pending witness, newest first ([`TemporalRecorder::judge_plans`], taken under one brief lock): each chain is folded to its end by a [`ChainJudge`] with the recorder unlocked, and its verdicts are handed to [`TemporalRecorder::judge`] under the lock. A witness evicted or forgotten meanwhile takes no verdict.
pub(crate) fn judge_all(recorder: &Mutex<TemporalRecorder>) -> JudgeCounts {
    let lock = || recorder.lock().unwrap_or_else(PoisonError::into_inner);
    let plans = lock().judge_plans();
    let mut counts = JudgeCounts::default();
    for plan in plans {
        let mut judge = ChainJudge::new(plan);
        let mut verdicts = Judged::default();
        loop {
            let (judged, _, more) = judge.step();
            verdicts.0.extend(judged.0);
            if !more {
                break;
            }
        }
        drop(judge);
        let mut rec = lock();
        let (witnessed, diverged) = (rec.witnessed(), rec.diverged());
        rec.judge(verdicts);
        counts.judged += rec.witnessed() - witnessed;
        counts.diverged += rec.diverged() - diverged;
    }
    let rec = lock();
    counts.pending = rec.pending();
    counts.unjudged_evicted = rec.unjudged_evicted();
    counts
}

/// The shared writer's judge of closed chains in its quiet periods ([`WriterChannel`]): at most one [`ChainJudge`] in the process, parked between them.
#[derive(Default)]
struct IdleJudge {
    job: Option<IdleJob>,
}

/// One closed chain the idle writer is judging, and the recording it was planned on.
struct IdleJob {
    recorder: Weak<Mutex<TemporalRecorder>>,
    /// The seq of the chain's base keyframe, which must stay kept for a verdict to post. A forget ([`TemporalRecorder::forget`]) drops every keyframe and no seq recurs, so a job planned before one finds its base gone.
    base: Seq,
    judge: ChainJudge,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    costs: Option<Arc<crate::recording_cost::Costs>>,
}

impl IdleJudge {
    /// Judge `pool`'s closed chains that hold pending witnesses: the parked job first, then each member's newest such chain ([`TemporalRecorder::judge_plan`]), the pane focused last first. Before each [`FORK_EVERY`] pass it asks `stop`, and once that holds it returns with the job parked; each pass runs with the recorder unlocked and posts its verdicts under the lock before the next. A job is dropped when its chain is folded, when its recorder is gone, or, unposted, when the recording was forgotten or the chain's base evicted since it was planned. With no member's closed chain left to judge, it clears [`RewindPool::judge_due`].
    fn run(&mut self, pool: &RewindPool, mut stop: impl FnMut() -> bool) {
        loop {
            if stop() {
                return;
            }
            if self.job.is_none() {
                // Cleared before the members are read, so a chain a member closes meanwhile keeps it set.
                pool.judge_due.store(false, Ordering::Release);
                let Some(job) = IdleJob::pick(pool) else {
                    return;
                };
                pool.judge_due.store(true, Ordering::Release);
                self.job = Some(job);
            }
            if !self.job.as_mut().is_some_and(IdleJob::pass) {
                self.drop_job(pool);
            }
        }
    }

    /// Drop the job, reporting its engine freed to `pool` ([`RewindPool::note_freed`]), and relieve `pool`.
    fn drop_job(&mut self, pool: &RewindPool) {
        let Some(job) = self.job.take() else {
            return;
        };
        let freed = job.judge.engine_bytes();
        drop(job);
        pool.note_freed(freed);
        pool.relieve();
    }
}

impl IdleJob {
    /// A judge of the newest closed chain holding a pending witness in the first of `pool`'s members, the pane focused last first, that has one; each recorder locked only to take its plan, the base restored with none locked.
    fn pick(pool: &RewindPool) -> Option<Self> {
        let mut members: Vec<(u64, Weak<Mutex<TemporalRecorder>>)> = pool
            .members
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|m| (m.focused, m.recorder.clone()))
            .collect();
        members.sort_by_key(|&(focused, _)| std::cmp::Reverse(focused));
        members.into_iter().find_map(|(_, recorder)| {
            let strong = recorder.upgrade()?;
            let rec = strong.lock().unwrap_or_else(PoisonError::into_inner);
            let plan = rec.judge_plan()?;
            #[cfg(any(debug_assertions, feature = "dev-seams"))]
            let costs = rec.spine.costs.handle();
            drop(rec);
            drop(strong);
            Some(Self {
                base: plan.base_seq(),
                judge: ChainJudge::new(plan),
                recorder,
                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                costs,
            })
        })
    }

    /// One pass, booked under the recorder's `judge` cost, its verdicts then posted: whether the job goes on, which it does while steps are left and its chain's base is still kept (neither forgotten nor evicted). A job whose recorder is gone runs no pass.
    fn pass(&mut self) -> bool {
        if self.recorder.strong_count() == 0 {
            return false;
        }
        let (judged, more) = {
            #[cfg(any(debug_assertions, feature = "dev-seams"))]
            let mut cost =
                crate::recording_cost::begin(self.costs.as_deref().map(|c| &c.judge), None);
            let (judged, folded, more) = self.judge.step();
            #[cfg(any(debug_assertions, feature = "dev-seams"))]
            if let Some(cost) = cost.as_mut() {
                cost.set_work(u64::try_from(folded).ok());
            }
            #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
            let _ = folded;
            (judged, more)
        };
        let Some(recorder) = self.recorder.upgrade() else {
            return false;
        };
        let mut rec = recorder.lock().unwrap_or_else(PoisonError::into_inner);
        if rec.keyframe_at(self.base).is_none() {
            return false;
        }
        rec.judge(judged);
        more
    }
}

/// Test-only hook a replay calls first ([`ReplayPlan::fold`], and [`ChainJudge::new`] before it restores its base), so a test can probe the recorder lock from inside the fold.
#[cfg(test)]
#[path = "temporal_replay_hook_tests.rs"]
pub(crate) mod replay_hook;

/// Test-only switch [`ReplayPlan::fold`] reads per slice: a replay on the live clock, the negative control of the recorded one.
#[cfg(test)]
#[path = "temporal_replay_clock_tests.rs"]
pub(crate) mod replay_clock;

/// Test-only switch the keyframe and witness lookups read: the linear scans they replaced, the reference a test compares the sorted lookups with.
#[cfg(test)]
#[path = "temporal_lookup_scan_tests.rs"]
pub(crate) mod lookup_scan;

/// The keyframes rekeys on this thread captured ([`SpineMark::rekey`]), sent or refused.
#[cfg(test)]
#[path = "temporal_capture_probe_tests.rs"]
mod capture_probe;

/// A reader that counts a keyframe between a writer's ask and its recheck ([`TemporalRecorder::after_op`]), which no test thread can time.
#[cfg(test)]
#[path = "temporal_ask_probe_tests.rs"]
mod ask_probe;

#[cfg(test)]
#[path = "temporal_resync_probe_tests.rs"]
mod resync_probe;

/// Events since the newest keyframe after which the recorder asks the readers for a rekey ([`SpineWatermark::rekey_wanted`]), well inside the event ring so that keyframe stays replayable.
const REKEY_EVENTS: u64 = (aterm_buffer::MAX_LOG_EVENTS / 4) as u64;

/// The least rekey interval, and the least output since the newest keyframe for the pressure trigger to rekey ([`SpineWatermark::rekey_due`]).
const MIN_REKEY_BYTES: usize = 64 * 1024;

/// A trigger a test turns off on a recording's readers ([`SpineWatermark::disable_trigger`]): `Events` ignores the recorder's ask ([`SpineWatermark::rekey_wanted`]), `Pressure` the budget-pressure arm of [`SpineWatermark::rekey_due`].
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RekeyTrigger {
    Events,
    Pressure,
}

/// The history, in lines, a reader's keyframes carry ([`keyframe_of`]) and its primary grid's carried replay-history suffix is clamped to after each recorded step ([`aterm_core::Terminal::note_bounded_replay_step`]).
const REKEY_SCROLLBACK_LINES: usize = 2048;

/// Output since the last witness that makes the next slice at parser ground take one.
const WITNESS_BYTES: usize = 256 * 1024;

/// Time since the last witness after which the next slice at parser ground takes one.
const WITNESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
