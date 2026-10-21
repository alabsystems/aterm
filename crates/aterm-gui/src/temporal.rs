// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session **temporal recorder** — the GUI-side capture half of the
//! hydratable temporal buffer (design `HIERARCHICAL_SESSIONS.md` Addendum B, B.9).
//!
//! [`TemporalRecorder`] folds the live session into the `aterm-buffer` event-log
//! spine: a [`Keyframe`](aterm_buffer::Op::Keyframe) (a serialized
//! [`TerminalCheckpoint`]) plus the stream of
//! [`RawIn`](aterm_buffer::Op::RawIn)/[`Reply`](aterm_buffer::Op::Reply)/
//! [`Resize`](aterm_buffer::Op::Resize) events that drive the engine. Replaying
//! `hydrate(keyframe) + process(RawIn…up to t)` reconstructs the engine state at
//! any `t` — the property `recording_model` (B.8.3) proves and
//! `conformance_recording` (B.8.4) binds to the real engine.
//!
//! ## Discipline (mirrors [`CastRecorder`](crate::cast::CastRecorder))
//! - **Handles, not payloads on the spine.** The spine carries `BlobId`/
//!   `KeyframeId` handles; the bulk bytes live here in bounded side stores so the
//!   in-RAM ring stays small.
//! - **Bounded and counted.** Retention is the spine's live ring plus a byte
//!   budget over the payloads (input bytes and keyframes). An event
//!   [`EventLog::append_at`] evicts takes its payload with it, because no replay
//!   folds an evicted event, and is **counted**
//!   ([`dropped_events`](TemporalRecorder::dropped_events): total minus live),
//!   never silently lost. Under the budget, payloads no replay can read go
//!   first, then older keyframes, and the newest keyframe's chain last.
//!   Re-keyframing keeps a recent `[keyframe..latest]` window inside both limits
//!   (R19, [`TemporalRecorder::rekey_due`]), and `temporal status` says
//!   where the replayable range starts (`reachable_from=`). There is no warm or
//!   cold tier, by design (OPEN-B4): recording is opt-in, and archival is `cast`.
//! - **No fs / no lock / no wall-clock-as-state.** Ticks come from one epoch
//!   captured at construction; the GUI feeds bursts off the reader hot path on a
//!   dedicated writer thread, which also mints re-keyframes, so neither the append
//!   nor a replay runs on the reader.
//! - **Replays fold off the lock.** A replay is lifted out of the recorder as a
//!   [`ReplayPlan`] (refcounts on the keyframe and the input) and folded after
//!   the lock is released, by `temporal` and by the writer's own re-keyframe
//!   ([`write()`]), so neither stalls the writer or another reader.
//! - **A dropped input is a break, closed at the source.** Input the reader
//!   could not queue is missing from every replay that would fold it, so the
//!   reader counts it ([`SpineWatermark`]) and queues nothing more until a
//!   keyframe of the live engine closes the break; no replay reaches across it.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_buffer::{BlobId, EventLog, KeyframeId, Op, Seq, Ticks};
use aterm_control::replay::ReplayEngine;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

/// Default byte budget for retained payloads: input blobs plus keyframes. A
/// flood cannot balloon RAM past this; an idle session costs nothing.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// A burst handed from the reader hot path to the temporal writer thread
/// (lock-free, FIFO, bounded: [`queue`]). Recording the
/// tick + spine append happens on the writer thread, never under `term_lock`.
pub(crate) enum TemporalMsg {
    /// Raw PTY input fed to `process()` (the engine-driving bytes): the slice
    /// one reader hold processed, a range of the reader's one copy of its batch
    /// ([`InputSlice`]), which the cast and `bytes` taps carry too.
    ///
    /// Not only PTY bytes: a FOREGROUND HANDBACK (2026-09-25,
    /// `Terminal::foreground_handback`) feeds host-synthesized bytes through the
    /// same `process_at` at the offset where the foreground process group
    /// changed, and the reader records them here as one `RawIn` at that position
    /// (and splices them into the cast and `bytes` taps at the same offset), so a
    /// replay of the spine reaches the live state.
    RawIn(InputSlice),
    /// The engine emitted reply bytes to the PTY peer (`take_response()`). The
    /// spine records the instant; the bytes are not carried, because a replay
    /// never re-emits a reply (B.4.1) and nothing else reads them.
    Reply,
    /// A geometry change the engine applied (a window pass, the cross-session
    /// `resize` verb). The resizing thread does not enqueue it: the READER does,
    /// from the engine's resize journal, inside its next `term_lock` hold and
    /// before that hold's `RawIn` ([`SpineMark::record`]), so the writer thread
    /// APPENDS it to the spine in the order the engine applied it, never ahead of
    /// already-processed input (B.2.3: reflow is path-dependent, so a mid-print
    /// resize must replay in the right place). Until that hold it is PENDING
    /// ([`SpineWatermark::pending`], `temporal status` `pending_resizes=`), and it
    /// is stamped with that hold's tick, not its own.
    Resize { rows: u16, cols: u16 },
    /// Resizes the engine applied that the spine will never hold: its journal
    /// (`RESIZE_JOURNAL_CAP` deep) overran before a reader's hold read it —
    /// between two holds, or while the session had no reader attached. Counted
    /// ([`TemporalRecorder::lost_resizes`], `temporal status` `lost_resizes=`),
    /// never silent: a replay across them lays the screen out without their row
    /// moves.
    ResizesLost(u64),
    /// A keyframe of the live engine, taken by the reader inside a hold to close
    /// the break it opened when it could not queue input ([`SpineMark::record`]).
    /// It is the first message after the dropped one, so the dropped input lies
    /// between the event appended before it and itself; the writer ends the
    /// newest keyframe's chain there and records this one as the new base.
    Keyframe(Arc<TerminalCheckpoint>),
}

/// The bytes of one `RawIn`: a range of the allocation the reader copied its
/// batch into, once, before the batch's first `term_lock` hold. Every slice of
/// the batch shares that allocation, with the others and with the cast and
/// `bytes` taps, so queuing one inside a hold is a refcount, never an
/// allocation or a memcpy ([`SpineMark::record`]). A foreground handback's or
/// a manual reset's synthesized bytes are a small allocation of their own
/// (`From<&[u8]>`).
#[derive(Clone)]
pub(crate) struct InputSlice {
    batch: Arc<[u8]>,
    range: Range<usize>,
}

impl InputSlice {
    /// `batch[range]`, sharing `batch`.
    ///
    /// # Panics
    /// When `range` is not inside `batch`: the caller cut the slice from the
    /// bytes it processed, so the spine would hold bytes the engine never saw.
    pub(crate) fn new(batch: Arc<[u8]>, range: Range<usize>) -> Self {
        assert!(
            range.start <= range.end && range.end <= batch.len(),
            "input slice {range:?} outside its {}-byte batch",
            batch.len()
        );
        Self { batch, range }
    }

    /// The bytes the engine processed.
    fn bytes(&self) -> &[u8] {
        &self.batch[self.range.clone()]
    }

    /// Their length: what the slice is charged against the budget.
    fn len(&self) -> usize {
        self.range.len()
    }
}

impl From<Arc<[u8]>> for InputSlice {
    /// All of `batch`, sharing it.
    fn from(batch: Arc<[u8]>) -> Self {
        let range = 0..batch.len();
        Self { batch, range }
    }
}

impl From<&[u8]> for InputSlice {
    /// A copy of `bytes` in an allocation of its own.
    fn from(bytes: &[u8]) -> Self {
        Self::from(Arc::<[u8]>::from(bytes))
    }
}

/// Open a recording's writer queue, bounded at `cap` messages: the reader's
/// end and the writer thread's.
pub(crate) fn queue(cap: usize) -> (TemporalTx, TemporalRx) {
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    let state = Arc::new(QueueState::default());
    (
        TemporalTx {
            tx,
            state: state.clone(),
            cap,
        },
        TemporalRx { rx, state },
    )
}

/// What the two ends of the writer queue share, so the reader can tell
/// whether a send could succeed without making one ([`TemporalTx::has_room`]).
#[derive(Default)]
struct QueueState {
    /// Messages queued and not yet taken by the writer, counted before each
    /// send and taken back when the send fails, so it never reads below the
    /// queue's depth.
    in_flight: AtomicUsize,
    /// The writer's end was dropped ([`TemporalRx`]'s `Drop`): its thread
    /// ended, a panic unwinding it included (aterm isolates panics, so the
    /// reader outlives one). No send succeeds again, whatever `in_flight`
    /// reads; the messages it held are dropped uncounted with it.
    writer_gone: AtomicBool,
}

/// The reader's end of the writer queue: a bounded FIFO it never blocks on, so
/// a writer that falls behind costs the reader drops (counted,
/// [`SpineWatermark`]), never a stall. It also says whether a send could
/// succeed ([`Self::has_room`]), so the keyframe that closes a break is
/// projected only when it can be queued.
pub(crate) struct TemporalTx {
    tx: SyncSender<TemporalMsg>,
    state: Arc<QueueState>,
    cap: usize,
}

impl TemporalTx {
    /// Queue `msg` unless the queue is full (or the writer is gone).
    fn send(&self, msg: TemporalMsg) -> bool {
        self.state.in_flight.fetch_add(1, Ordering::AcqRel);
        let sent = self.tx.try_send(msg).is_ok();
        if !sent {
            self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

    /// Whether a send could succeed: a slot is free and the writer is still
    /// there to take it. Only the reader sends, so a slot it sees free stays
    /// free until its own next send; a writer that is gone never comes back,
    /// so a break open on it is never projected for again.
    fn has_room(&self) -> bool {
        !self.state.writer_gone.load(Ordering::Acquire)
            && self.state.in_flight.load(Ordering::Acquire) < self.cap
    }
}

/// The writer thread's end of the queue ([`queue`]).
pub(crate) struct TemporalRx {
    rx: Receiver<TemporalMsg>,
    state: Arc<QueueState>,
}

impl TemporalRx {
    /// The next message in FIFO order, waiting while the queue is empty; `None`
    /// once the reader's end is dropped and the queue is drained.
    pub(crate) fn recv(&self) -> Option<TemporalMsg> {
        let msg = self.rx.recv().ok()?;
        self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
        Some(msg)
    }

    /// The next message if one is queued.
    #[cfg(test)]
    pub(crate) fn try_recv(&self) -> Option<TemporalMsg> {
        let msg = self.rx.try_recv().ok()?;
        self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
        Some(msg)
    }
}

impl Drop for TemporalRx {
    fn drop(&mut self) {
        self.state.writer_gone.store(true, Ordering::Release);
    }
}

/// Where the spine's resizes are up to, SHARED by every reader a session has
/// (2026-09-28): the engine lifetime and resize ordinal through which a reader
/// has handed the spine every resize. The live reader's [`SpineMark`]
/// publishes each advance here, and the session's next reader starts from it
/// ([`SpineMark::attach`]), so a resize that landed while no reader was
/// attached (a park, a deferred attach) is read from the engine's journal and
/// recorded — past the journal's depth, counted in `lost_resizes=` — rather
/// than skipped. `temporal` compares it with the live engine to say how many
/// resizes the spine has not been handed yet ([`Self::pending`]).
///
/// Seeded with the keyframe the recording starts from ([`Self::seed`]). Two
/// atomics that never tear: after the seed (at spawn, before any reader
/// exists) the one writer is the live reader inside its `term_lock` hold, and
/// every read that must agree with the engine is taken under that lock too
/// (`temporal`) or after the writing reader was joined (the next attach). Held by the [`TemporalRecorder`] and handed out once per
/// attach ([`TemporalRecorder::spine_watermark`]), so neither the reader nor
/// `temporal` takes the recorder's lock inside `term_lock`.
///
/// It also counts what the readers could not queue (`temporal status`
/// `dropped_input=`, `dropped_replies=`) and whether a break is open
/// ([`Self::break_open`]), which the next reader takes over at attach. The live
/// reader alone bumps the two drop counts; the writer thread alone bumps
/// `closed`, under the recorder's lock.
#[derive(Debug, Default)]
pub(crate) struct SpineWatermark {
    /// The engine's lifetime token (never 0 for a real engine); 0 before the seed.
    lifetime: AtomicU64,
    /// The resize ordinal every resize through which the spine was handed.
    ordinal: AtomicU64,
    /// Input slices a reader could not queue (a full queue, for the slice's
    /// `RawIn` or a resize that had to precede it). Each opened a break.
    dropped_input: AtomicU64,
    /// Breaks the recorder closed: keyframes of the live engine it recorded
    /// ([`TemporalMsg::Keyframe`]), one per break.
    closed: AtomicU64,
    /// Reply instants a reader could not queue: a full queue, or an open
    /// break, which queues nothing but its keyframe. No replay reads a reply,
    /// so these break nothing.
    dropped_replies: AtomicU64,
}

impl SpineWatermark {
    /// Start the watermark at `pre`: the engine the recording's keyframe was
    /// checkpointed from, read in the same hold, so the first reader records
    /// every resize after the keyframe and none it already holds.
    pub(crate) fn seed(&self, pre: SpinePre) {
        self.store((pre.lifetime, pre.ordinal));
    }

    fn load(&self) -> Option<(u64, u64)> {
        let lifetime = self.lifetime.load(Ordering::Acquire);
        (lifetime != 0).then(|| (lifetime, self.ordinal.load(Ordering::Acquire)))
    }

    fn store(&self, (lifetime, ordinal): (u64, u64)) {
        self.ordinal.store(ordinal, Ordering::Release);
        self.lifetime.store(lifetime, Ordering::Release);
    }

    /// Resizes `term` (the caller holds its lock) applied that no reader has
    /// handed the spine yet. They wait for the reader's next hold — the
    /// program's next output — which records them first; a replay at the
    /// spine's end lacks them until then. Against another engine lifetime than
    /// the watermark's (or none), every resize this engine applied.
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

    /// Whether input was dropped that the recorder holds no keyframe after
    /// yet. Meanwhile the spine ends before input the engine processed, so a
    /// replay past its last event answers `ERR temporal unreachable`
    /// ([`TemporalRecorder::replay_plan`]). Exact at attach, after the
    /// previous reader and its writer were joined.
    #[must_use]
    pub(crate) fn break_open(&self) -> bool {
        self.closed.load(Ordering::Acquire) < self.dropped_input()
    }
}

/// One reader's RESIZE WATERMARK (2026-09-28, P4(c)): the engine lifetime and
/// resize ordinal (`Terminal::resize_ordinal`) up to which the spine holds
/// every resize. The reader builds one per attach from the session's
/// [`SpineWatermark`] ([`attach`](Self::attach)) and records through it
/// ([`SpineMark::record`]) inside the hold that processes each slice.
///
/// It replaced a `(rows, cols)` watermark, which saw a resize only as a
/// geometry DIFFERENT from the last one recorded: a net-zero flap between two
/// reads (64→63→64, the 2026-09-28 incident) left the geometry where it was,
/// so the spine never held it, and a replay kept every alternate-screen row
/// where the live engine had moved it. The engine's own resize journal names
/// each resize, both halves of a flap included. That watermark also started
/// over at every attach; this one starts where the previous reader stopped.
#[derive(Debug, Default)]
pub(crate) struct SpineMark {
    /// `(lifetime, ordinal)` recorded up to; `None` when the session's
    /// watermark was never seeded (the next record then records the live
    /// geometry, as the old `(0, 0)` sentinel did).
    at: Option<(u64, u64)>,
    /// The session's watermark, which every advance of `at` is published to.
    shared: Arc<SpineWatermark>,
    /// A break is open: input the spine needs was not queued, and nothing is
    /// until a keyframe of the live engine is ([`Self::resync`]).
    broken: bool,
}

/// What one reader hold read BEFORE processing its bytes: the engine's resize
/// lifetime and ordinal and its geometry. The spine records the resizes up to
/// here BEFORE the bytes, so a resize a slice's own bytes caused (none today:
/// DECCOLM is flag-only, XTWINOPS-8 a host callback) would replay from the
/// bytes, never twice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpinePre {
    lifetime: u64,
    ordinal: u64,
    rows: u16,
    cols: u16,
}

impl SpinePre {
    /// Read `term` (the caller holds its lock) before processing a slice.
    pub(crate) fn take(term: &Terminal) -> Self {
        Self {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
            rows: term.rows(),
            cols: term.cols(),
        }
    }
}

/// One resize a [`SpineMark`] hands its sink, in the engine's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpineResize {
    /// The geometry after it, `(rows, cols)`.
    pub(crate) to: (u16, u16),
    /// The geometry before it, from the engine's resize journal; `None` for the live
    /// geometry recorded once (a mark never seeded, or another engine lifetime's).
    pub(crate) from: Option<(u16, u16)>,
    /// The engine lifetime and resize ordinal it carries.
    pub(crate) lifetime: u64,
    pub(crate) ordinal: u64,
}

/// WHERE A READER'S SPINE GOES (crash restore, increment 4): the temporal
/// recorder's channel ([`TemporalMsg`]) or the crash-restore journal's tail
/// (`crate::crash_restore`). Both need the same discipline — every resize the
/// engine applied, in engine order and before the input that followed it, net-zero
/// flaps included, read from the engine's own resize journal — so the watermark
/// ([`SpineMark`]) is one piece of code and only where it writes differs. A sink
/// answers `false` when it could not take a record; the mark then stops there and
/// the next hold retries from where it stopped, so input is never recorded at a
/// geometry the sink does not hold yet.
pub(crate) trait SpineSink {
    /// One resize the engine applied.
    fn resize(&mut self, resize: SpineResize) -> bool;
    /// `n` resizes the engine's journal overran before a hold read them.
    fn resizes_lost(&mut self, n: u64) -> bool;
    /// The bytes the hold processed, after every resize before them. Only
    /// `SpineMark::record_to` (unix: the crash-restore journal) sends them.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn bytes(&mut self, bytes: &[u8]) -> bool;
}

/// The temporal recorder's queue as a [`SpineSink`]: bounded sends
/// ([`TemporalTx`]) that never block the reader. A slice's input rides as its
/// [`InputSlice`] ([`SpineMark::record`]); [`SpineSink::bytes`] copies.
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

    fn bytes(&mut self, bytes: &[u8]) -> bool {
        self.0.send(TemporalMsg::RawIn(InputSlice::from(bytes)))
    }
}

impl SpineMark {
    /// A reader's mark, starting where the session's previous reader left the
    /// spine (or where its keyframe seeded it), a break it left open included.
    /// Built at attach, after the previous reader and its writer were joined.
    pub(crate) fn attach(shared: Arc<SpineWatermark>) -> Self {
        Self {
            at: shared.load(),
            broken: shared.break_open(),
            shared,
        }
    }

    /// Put the watermark at `pre`: a keyframe read in the same hold holds every
    /// resize up to it, so none of those may be recorded after it (the journal's
    /// keyframes, `crate::crash_restore`, which a replay would otherwise resize a
    /// second time).
    #[cfg(unix)]
    pub(crate) fn seed(&mut self, pre: SpinePre) {
        self.advance((pre.lifetime, pre.ordinal));
    }

    /// Move the watermark to `at`, and publish it when it moved.
    fn advance(&mut self, at: (u64, u64)) {
        if self.at != Some(at) {
            self.at = Some(at);
            self.shared.store(at);
        }
    }

    /// Record one slice on the spine: every resize the engine applied since
    /// this watermark and up to `pre` (each journalled report's geometry, in
    /// order), then the slice's `input` as `RawIn`. The caller holds `term`'s
    /// lock, the same hold `pre` was read in and the input was processed
    /// under, so the spine's order is the engine's. Queuing the input moves a
    /// refcount on the reader's batch ([`InputSlice`]); nothing here allocates
    /// or copies bytes unless the ordinal moved or a break is open.
    ///
    /// * No watermark (the session's was never seeded), or one of another
    ///   lifetime token (the engine was replaced): the live geometry `pre`
    ///   names, once.
    /// * A journal overrun (more than `RESIZE_JOURNAL_CAP` resizes since the
    ///   watermark, a reader's absence included): the lost count rides the
    ///   spine ([`TemporalMsg::ResizesLost`]), then the kept reports.
    /// * A full queue: the resizes sent so far advance the watermark, and the
    ///   `RawIn` is DROPPED, never queued ahead of a resize it follows. Every
    ///   replay that would fold it would be wrong, so the drop is counted
    ///   (`dropped_input=`) and opens a BREAK: nothing more is queued until a
    ///   later hold queues a keyframe of the live engine in place of its slice
    ///   ([`Self::resync`]), which the writer records as the new base. No
    ///   replay reaches across the break ([`TemporalRecorder::replay_plan`]).
    ///
    /// The journal is read (one small copy) only when the ordinal moved.
    pub(crate) fn record(
        &mut self,
        tx: &TemporalTx,
        pre: SpinePre,
        term: &Terminal,
        input: InputSlice,
    ) {
        if self.broken {
            self.resync(tx, term);
        } else if self.catch_up(&mut TemporalSink(tx), pre, term)
            && tx.send(TemporalMsg::RawIn(input))
        {
            // Past anything the bytes themselves applied (they replay it).
            self.advance((term.resize_lifetime(), term.resize_ordinal()));
        } else {
            self.broken = true;
            self.shared.dropped_input.fetch_add(1, Ordering::Release);
        }
    }

    /// Close the open break: queue a keyframe of `term`, which has processed
    /// every slice up to here (the dropped one included), in place of this
    /// slice's input. Its history is bounded as a re-keyframe's, since it is
    /// projected under `term`'s lock: `O((rows + REKEY_SCROLLBACK_LINES) ×
    /// cols)`, paid only after a drop, and only once a send can succeed
    /// ([`TemporalTx::has_room`]) and a keyframe carries the parser without
    /// swallowing a sequence. None holds a hooked DCS query, whose reply lives
    /// in its handler (`checkpoint_continuing_bounded` refuses). A sequence
    /// whose consumer started — a kitty-graphics APC, a hooked sixel string,
    /// an OSC over the carry's cap — rides one only as a string to swallow
    /// (`Terminal::partial_sequence_swallowed`): the live engine still delivers
    /// it (an image it places, moving the cursor past it), while a replay from
    /// the keyframe drops the string's tail, so every later replay would lay
    /// text where the session never showed it. A hooked string whose handler keeps
    /// nothing loses nothing and rides. Otherwise a later hold tries again; the
    /// slices meanwhile ride in its keyframe.
    fn resync(&mut self, tx: &TemporalTx, term: &Terminal) {
        if !tx.has_room() || term.partial_sequence_swallowed().is_some() {
            return;
        }
        let Some(checkpoint) = term.checkpoint_continuing_bounded(REKEY_SCROLLBACK_LINES) else {
            return;
        };
        #[cfg(test)]
        resync_probe::projected();
        if tx.send(TemporalMsg::Keyframe(Arc::new(checkpoint))) {
            self.broken = false;
            // The keyframe holds every resize the engine applied.
            self.advance((term.resize_lifetime(), term.resize_ordinal()));
        }
    }

    /// Queue the instant of a reply the engine emitted (`take_response()`). A
    /// reply not queued — on a full queue, or inside a break, which queues
    /// nothing but its keyframe — is counted (`dropped_replies=`) and breaks
    /// nothing: no replay reads a reply.
    pub(crate) fn reply(&self, tx: &TemporalTx) {
        if self.broken || !tx.send(TemporalMsg::Reply) {
            self.shared.dropped_replies.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// [`Self::record`] into any [`SpineSink`] (the crash-restore journal's tail,
    /// `crate::crash_restore`): every resize up to `pre`, then `bytes`. `false`
    /// when the sink refused a record (the watermark stays where the sink
    /// stopped, and the bytes were not recorded).
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
        // Past anything the bytes themselves applied (they replay it).
        self.advance((term.resize_lifetime(), term.resize_ordinal()));
        true
    }

    /// Bring the sink up to `pre`'s resize ordinal. `false`: a resize could not
    /// be recorded, so the caller must not record input after it.
    pub(crate) fn catch_up(
        &mut self,
        sink: &mut impl SpineSink,
        pre: SpinePre,
        term: &Terminal,
    ) -> bool {
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
            // Counted once: the watermark moves past what the journal lost.
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
    /// The event-log spine (the one timeline). A bounded ring: an event it
    /// evicts takes its payload with it ([`append`](Self::append)) and is
    /// counted ([`dropped_events`](Self::dropped_events)).
    log: EventLog,
    /// One slot per `RawIn`/`Reply` handle, oldest first: a contiguous window
    /// of blob ids `[next_blob - len, next_blob)`, so a handle resolves by
    /// offset ([`blob`](Self::blob)). A `RawIn` slot holds its slice of the
    /// reader's batch (a refcount on the batch, no re-copy), charged the
    /// slice's length; eviction pops the front and drops that ref. A `Reply`
    /// slot holds nothing: no replay reads a reply, so its bytes are never
    /// retained or charged.
    ///
    /// A batch whose slices are all retained is charged its length once, since
    /// its slices partition it. It outlives its charge only while part of it
    /// is gone: its oldest slices evicted (one batch, at the front), or some
    /// slices never queued (the batch a break opened in, at the end of one
    /// keyframe's chain, and the batch its keyframe closed it in, at the start
    /// of the next: at most two per retained keyframe). The cast recorder
    /// retains the same batches.
    blobs: VecDeque<Option<InputSlice>>,
    /// Keyframes (serialized checkpoints), oldest first, at most
    /// [`MAX_KEYFRAMES`] retained, each live on the spine (an evicted
    /// `Op::Keyframe` event takes its checkpoint with it).
    ///
    /// Each entry carries the SPINE COORDINATES its `Op::Keyframe` event was
    /// appended at — `(seq, ts)` — not just the checkpoint. That is what lets
    /// `plan` pick its base by looking at this four-entry deque instead of
    /// re-deriving it by scanning up to `MAX_LOG_EVENTS` = 65,536 spine events
    /// for the last `Op::Keyframe`. The coordinates cannot drift from the spine
    /// because they are the exact values `append` returned when that very event
    /// was recorded.
    keyframes: VecDeque<RetainedKeyframe>,
    /// Monotone blob-id source.
    next_blob: u64,
    /// Monotone keyframe-id source.
    next_keyframe: u64,
    /// Retained payload bytes: the `RawIn` blobs plus the keyframe charges.
    used: usize,
    /// The retained byte budget ([`enforce_budget`](Self::enforce_budget)).
    budget: usize,
    /// Resizes the live engine applied that the spine never held: its resize
    /// journal overran between two reader holds ([`TemporalMsg::ResizesLost`]).
    /// Disclosed by `temporal status` as `lost_resizes=`.
    lost_resizes: u64,
    /// Where the spine's resizes are up to, shared with the session's readers
    /// ([`SpineWatermark`]): seeded with the first keyframe, advanced by the
    /// live reader, the start of the next one. It also counts what the readers
    /// dropped and the breaks this recorder closed.
    spine: Arc<SpineWatermark>,
    /// Bytes of `RawIn` recorded since the newest keyframe: its chain's
    /// retained bytes while the chain is whole. Two of the re-keyframe triggers
    /// read it ([`rekey_due`](Self::rekey_due)).
    bytes_since_keyframe: usize,
    /// A due re-keyframe found the replay at the spine's end inside an escape
    /// sequence. Only a `RawIn` can end it, so none is retried before the next.
    ends_mid_sequence: bool,
    /// The trigger a test turned off, to show what it alone keeps reachable.
    #[cfg(test)]
    disabled_trigger: Option<RekeyTrigger>,
    /// The monotonic epoch this recorder's tick timeline is relative to.
    epoch: Instant,
    /// The seam policy the live engine resizes with (`pty_resize_policy` of
    /// the session's master), which every replay is built for
    /// ([`ReplayEngine`]): each recorded `Resize` replays under it and the
    /// replayed engine takes its console-host posture. The spine's
    /// `Op::Resize` carries only the geometry, and the policy is a fact of the
    /// session's backend rather than of any one resize, so it is held once
    /// here. Replaying a ConPTY session's resizes as Native would
    /// reveal history that the recorded conhost repaint then paints over: the
    /// replayed engine would lose lines the live one kept.
    resize_policy: ResizePolicy,
    /// Whether the session's console host leaves a dead app's alternate screen
    /// itself (OpenConsole, `crate::alt_screen_resync`), which every replay
    /// folds under: the live engine leaves such a screen only at the host's
    /// forwarded `?1049l`, recorded later in the spine, so a replay that left
    /// it locally at the prompt would draw that prompt where the live one never
    /// had it. A fact of the session's host, held once like `resize_policy`.
    host_resyncs_alt_screen: bool,
    /// Set on an ADOPTED session's recorder (round five, item 17): the spine
    /// starts at a seamless update — its t0 keyframe is the screen the update
    /// restored, and nothing before it was recorded here. Reported by
    /// [`status_tail`](Self::status_tail) beside `dropped_events=`, the
    /// recorder's other gap.
    handoff: Option<crate::session_timeline::HandoffGap>,
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
            keyframes: VecDeque::new(),
            next_blob: 0,
            next_keyframe: 0,
            used: 0,
            budget: budget.max(1),
            lost_resizes: 0,
            spine: Arc::default(),
            bytes_since_keyframe: 0,
            ends_mid_sequence: false,
            #[cfg(test)]
            disabled_trigger: None,
            epoch: Instant::now(), // recorder timeline epoch, not engine state
            resize_policy: ResizePolicy::Native,
            host_resyncs_alt_screen: false,
            handoff: None,
        }
    }

    /// Mark this spine as one that starts at a seamless update
    /// ([`crate::session_timeline::HandoffGap`]).
    pub(crate) fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// What `temporal status` appends after `dropped_events=`: ` carried=0
    /// from_build=<n|->` for a spine that starts at a seamless update, and
    /// nothing otherwise — a fresh session's status line is unchanged.
    #[must_use]
    pub(crate) fn status_tail(&self) -> String {
        self.handoff
            .map(|gap| format!(" {}", gap.payload()))
            .unwrap_or_default()
    }

    /// Replay under `policy` (every recorded resize, and the replayed engine's
    /// console-host posture) — the one the live engine resizes with. Set by the
    /// reader attach from the session's PTY backend.
    pub(crate) fn set_resize_policy(&mut self, policy: ResizePolicy) {
        self.resize_policy = policy;
    }

    /// Replay under the live engine's alternate-screen recovery: `on` for a
    /// session whose console host leaves a dead app's screen itself
    /// (`crate::alt_screen_resync::host_resyncs`). Set by the reader attach.
    pub(crate) fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.host_resyncs_alt_screen = on;
    }

    /// The current tick on this recorder's timeline (micros since the epoch).
    /// Both the reader-thread (RawIn/Reply) and main-thread (Resize/Keyframe)
    /// taps call this so one session shares a single monotone tick timeline.
    #[must_use]
    pub(crate) fn now(&self) -> Ticks {
        // Derives the recorded tick from the recorder epoch; this
        // is the value we RECORD, not engine state read during process().
        Ticks(u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    /// Append `op` at `ts` and return the spine `Seq` it was assigned
    /// (`record_keyframe` retains it beside the checkpoint, so the base-keyframe
    /// lookup is a four-entry scan rather than a walk of the whole spine).
    ///
    /// An event the ring evicts takes its payload with it: no replay folds an
    /// evicted event or seeds from an evicted keyframe event, so its blob or
    /// checkpoint is released here, and the eviction is counted
    /// ([`dropped_events`](Self::dropped_events)). This is also what keeps
    /// every retained keyframe live on the spine. Payload ids follow spine
    /// order, so what an evicted event held is at the front of its store.
    fn append(&mut self, op: Op, ts: Ticks) -> Seq {
        let (seq, evicted) = self.log.append_at(op, ts);
        match evicted.map(|ev| (ev.seq, ev.op)) {
            Some((_, Op::RawIn(id) | Op::Reply(id))) => {
                while self.blob_base() <= id.0 && self.pop_blob() {}
            }
            Some((gone, Op::Keyframe(_))) => {
                while self.keyframes.front().is_some_and(|kf| kf.seq <= gone) {
                    self.pop_keyframe();
                }
            }
            _ => {}
        }
        seq
    }

    /// Append an event, then hold the budget unless a re-keyframe is due. A due
    /// one is handed back to mint with the lock released ([`Rekey`]), and the
    /// budget is held once it is recorded ([`Self::rekeyed`]), so the event that
    /// makes one due never first evicts the chain it replays from: retained
    /// state then moves exactly as if the replay had run here.
    fn record(&mut self, op: Op) -> Option<Rekey> {
        let ts = self.now();
        self.append(op, ts);
        let due = self.rekey_due();
        if due.is_none() {
            self.enforce_budget();
        }
        due
    }

    /// Record a keyframe (a serialized [`TerminalCheckpoint`] taken at a
    /// parser-ground boundary, B.3.3). Replay seeds from the nearest keyframe
    /// `<= seq(t)` and folds `RawIn` forward.
    pub(crate) fn record_keyframe(&mut self, checkpoint: impl Into<Arc<TerminalCheckpoint>>) {
        let ts = self.now();
        let id = KeyframeId(self.next_keyframe);
        self.next_keyframe += 1;
        // APPEND FIRST, then retain — the spine assigns the `Seq` and it is the
        // coordinate `plan` seeks by, so the retained entry is built from
        // the append's own return value and cannot disagree with the spine.
        let seq = self.append(Op::Keyframe(id), ts);
        let keyframe = RetainedKeyframe {
            #[cfg(test)]
            id,
            seq,
            ts,
            first_blob: self.next_blob,
            cut: None,
            checkpoint: checkpoint.into(),
        };
        // A keyframe is large; its grid bytes are charged against the budget.
        self.used += keyframe.charge();
        self.keyframes.push_back(keyframe);
        // This keyframe is the fresh base: the forward chain restarts from here.
        self.bytes_since_keyframe = 0;
        // Cap the retained keyframes, so periodic re-keyframing cannot let them
        // accumulate until they fill the budget; older instants age out honestly
        // (bounded retention). The newest keyframe is always kept.
        while self.keyframes.len() > MAX_KEYFRAMES {
            self.pop_keyframe();
        }
        self.enforce_budget();
    }

    /// Close the break the reader opened when it dropped input, with the
    /// keyframe of the live engine it queued next ([`TemporalMsg::Keyframe`]).
    /// The dropped input lies after the last event appended, so the newest
    /// keyframe's chain ends at that event's tick ([`RetainedKeyframe::cut`]):
    /// no replay from it reaches a later tick. The new keyframe starts a whole
    /// chain, and a re-keyframe the old chain deferred is moot.
    fn close_break(&mut self, checkpoint: Arc<TerminalCheckpoint>) {
        let end = self.latest_tick();
        if let Some(newest) = self.keyframes.back_mut() {
            newest.cut = Some(end);
        }
        self.ends_mid_sequence = false;
        self.record_keyframe(checkpoint);
        self.spine.closed.fetch_add(1, Ordering::Release);
    }

    /// A re-keyframe, when one is due: a FRESH keyframe minted by replaying to
    /// the CURRENT instant, so a recent `[keyframe..latest]` window stays inside
    /// both retention limits, the spine's ring and the byte budget (R19). Due
    /// on whichever comes first:
    /// - **bytes**: the newest keyframe's chain reaches the interval,
    ///   `max(budget / 4, MIN_REKEY_BYTES)`;
    /// - **events**: [`REKEY_EVENTS`] events follow the newest keyframe, so its
    ///   own event never leaves the ring however small the events are;
    /// - **pressure**: the newest keyframe's charge plus its chain comes within
    ///   one interval of the budget. Past the budget only that chain is left
    ///   to evict ([`enforce_budget`](Self::enforce_budget) takes everything
    ///   else first), and the interval of headroom covers a re-keyframe
    ///   deferred to a later burst.
    ///
    /// The pressure trigger, like the byte one, waits for [`MIN_REKEY_BYTES`]
    /// of chain, so a keyframe that alone nearly fills the budget cannot
    /// replay on every burst; its chain then degrades to `ERR temporal
    /// unreachable`, honestly.
    ///
    /// Only a keyframe whose chain is whole is replayed from (the newest is
    /// never cut, [`Self::close_break`]), so the replay to the LATEST tick
    /// cannot fail and yields the current state (NOT a past state stamped
    /// `now()`).
    fn rekey_due(&self) -> Option<Rekey> {
        if self.ends_mid_sequence {
            return None;
        }
        let newest = self
            .keyframes
            .back()
            .filter(|kf| kf.first_blob >= self.blob_base())?;
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
        })
    }

    /// Record the keyframe a due re-keyframe minted ([`Rekey::mint`]), then
    /// hold the budget. Only the writer that called [`Self::apply`] appends
    /// (seeds and attaches run while no writer does), so the spine still ends
    /// where the replay ended; one that moved anyway discards the keyframe,
    /// which would stand for an earlier instant than its place. A replay that
    /// ended inside an escape sequence minted none, and only a `RawIn` can end
    /// it, so none is tried again before the next.
    pub(crate) fn rekeyed(&mut self, minted: Minted) {
        match minted.checkpoint {
            Some(checkpoint) if self.log.head() == minted.head => self.record_keyframe(checkpoint),
            Some(_) => self.enforce_budget(),
            None => {
                self.ends_mid_sequence = true;
                self.enforce_budget();
            }
        }
    }

    /// Give the next `BlobId` a slot holding `input`, charging the budget its
    /// `len` (the refund on eviction is the same `len`, however many slices
    /// share its batch). `None` is a reply's empty slot.
    fn store_blob(&mut self, input: Option<InputSlice>) -> BlobId {
        let id = BlobId(self.next_blob);
        self.next_blob += 1;
        self.used += input.as_ref().map_or(0, InputSlice::len);
        self.blobs.push_back(input);
        id
    }

    /// The id of the oldest retained blob slot, `next_blob` when none is. A
    /// keyframe's chain is whole while `first_blob >= blob_base()`: blobs leave
    /// only from the front, and every blob of the chain has an id at or past
    /// its `first_blob`.
    fn blob_base(&self) -> u64 {
        self.next_blob - self.blobs.len() as u64
    }

    /// Evict the oldest blob slot, refunding its bytes. `false` when none is left.
    fn pop_blob(&mut self) -> bool {
        let Some(slot) = self.blobs.pop_front() else {
            return false;
        };
        self.used = self
            .used
            .saturating_sub(slot.as_ref().map_or(0, InputSlice::len));
        true
    }

    /// Evict the oldest keyframe, refunding its charge. `false` when none is left.
    fn pop_keyframe(&mut self) -> bool {
        let Some(keyframe) = self.keyframes.pop_front() else {
            return false;
        };
        self.used = self.used.saturating_sub(keyframe.charge());
        true
    }

    /// Evict until the budget holds, losing as little replayable history as
    /// it can. In order:
    /// 1. what no replay can read: a keyframe whose chain lost a blob, and the
    ///    blobs older than the oldest retained keyframe's chain (every blob,
    ///    when no keyframe is retained);
    /// 2. the oldest keyframe, while a newer one remains: the instants before
    ///    the next keyframe become unreachable, and its chain falls to step 1;
    /// 3. the newest keyframe's chain, oldest blob first, and then the keyframe
    ///    itself: the last resort, reached only when the newest keyframe and its
    ///    chain alone exceed the budget. Replay then answers `ERR temporal
    ///    unreachable` until a new keyframe arrives.
    ///
    /// The re-keyframe triggers keep step 3 out of reach whenever the newest
    /// keyframe leaves room for its chain ([`rekey_due`](Self::rekey_due)).
    /// Deep history beyond the RAM budget (a COLD/on-disk tier) is not built,
    /// by design (OPEN-B4); archival is the `cast` export.
    fn enforce_budget(&mut self) {
        while self.used > self.budget {
            let base = self.blob_base();
            let oldest_chain = self.keyframes.front().map(|kf| kf.first_blob);
            if oldest_chain.is_some_and(|first| first < base) {
                self.pop_keyframe();
            } else if !self.blobs.is_empty() && oldest_chain.is_none_or(|first| base < first) {
                self.pop_blob();
            } else if self.keyframes.len() > 1 {
                self.pop_keyframe();
            } else if !self.pop_blob() && !self.pop_keyframe() {
                break;
            }
        }
    }

    /// Total events ever appended to the spine (live + evicted).
    #[must_use]
    pub(crate) fn total_events(&self) -> u64 {
        self.log.total()
    }

    /// Live (un-evicted) event count on the spine. O(1): live seqs are the
    /// contiguous range from the ring's oldest event to its newest.
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

    /// Events the spine's ring evicted: `total_events - live_events`, every
    /// seq below the ring's low-water. Each took its payload with it, so no
    /// replay reaches an instant before the oldest live event.
    #[must_use]
    pub(crate) fn dropped_events(&self) -> u64 {
        self.log
            .oldest_live()
            .map_or(self.log.total(), |oldest| oldest.seq.0 - 1)
    }

    /// The earliest tick from which every later tick replays, the spine's end
    /// included: the tick of the oldest retained keyframe whose chain is whole
    /// and uncut, with every newer keyframe's chain too (chains lose only their
    /// oldest blobs, and only an older keyframe's chain is ever cut). `None`
    /// when the spine's end does not replay — no keyframe's chain is whole, or
    /// a break is open ([`SpineWatermark::break_open`]) — so `temporal` answers
    /// `ERR temporal unreachable` there (`temporal status` `reachable_from=-`).
    #[must_use]
    pub(crate) fn reachable_from(&self) -> Option<Ticks> {
        if self.spine.break_open() {
            return None;
        }
        self.oldest_replayable().map(|kf| kf.ts)
    }

    /// The keyframe [`reachable_from`](Self::reachable_from) names, open break
    /// or not.
    fn oldest_replayable(&self) -> Option<&RetainedKeyframe> {
        let base = self.blob_base();
        self.keyframes
            .iter()
            .rev()
            .take_while(|kf| kf.first_blob >= base && kf.cut.is_none())
            .last()
    }

    /// Fold one message from the reader into the spine, in FIFO order: the
    /// writer thread's whole job ([`write()`]). A re-keyframe it makes due comes
    /// back to mint with the lock released; the budget holds once it is
    /// recorded ([`Self::rekeyed`]).
    #[must_use]
    pub(crate) fn apply(&mut self, msg: TemporalMsg) -> Option<Rekey> {
        let op = match msg {
            // Raw PTY input fed to `process()`, which replay re-feeds exactly.
            // The slice of the reader's SHARED batch is retained as is (a
            // refcount move, no re-copy into the blob store).
            TemporalMsg::RawIn(input) => {
                self.bytes_since_keyframe += input.len();
                // These bytes may end the sequence a deferred re-keyframe waits on.
                self.ends_mid_sequence = false;
                Op::RawIn(self.store_blob(Some(input)))
            }
            // The event keeps its place and tick on the spine; its handle names
            // a slot that holds no bytes, since a replay never re-emits a reply
            // (the design's contract) and nothing else reads one.
            TemporalMsg::Reply => Op::Reply(self.store_blob(None)),
            // Appended HERE, in FIFO order — the enqueue was ordered under
            // `term_lock` relative to the reader's RawIn chunks (reflow is
            // path-dependent, so a resize is never re-ordered — B.2.3).
            TemporalMsg::Resize { rows, cols } => Op::Resize { rows, cols },
            TemporalMsg::ResizesLost(n) => {
                self.lost_resizes = self.lost_resizes.saturating_add(n);
                return None;
            }
            TemporalMsg::Keyframe(checkpoint) => {
                self.close_break(checkpoint);
                return None;
            }
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
        self.apply_inline(TemporalMsg::RawIn(InputSlice::from(bytes)));
    }

    /// Record a raw PTY-input burst, retaining the caller's allocation.
    #[cfg(test)]
    pub(crate) fn record_raw_in_shared(&mut self, bytes: Arc<[u8]>) {
        self.apply_inline(TemporalMsg::RawIn(InputSlice::from(bytes)));
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

    /// Input slices the session's readers could not queue (`temporal status`
    /// `dropped_input=`).
    #[must_use]
    pub(crate) fn dropped_input(&self) -> u64 {
        self.spine.dropped_input()
    }

    /// Reply instants the session's readers could not queue (`temporal status`
    /// `dropped_replies=`).
    #[must_use]
    pub(crate) fn dropped_replies(&self) -> u64 {
        self.spine.dropped_replies()
    }

    /// Resizes the live engine applied that the spine never held.
    #[must_use]
    pub(crate) fn lost_resizes(&self) -> u64 {
        self.lost_resizes
    }

    /// The session's spine watermark ([`SpineWatermark`]): taken once per
    /// reader attach (and by `temporal`), so the reader publishes its progress
    /// without this recorder's lock, which the writer thread contends.
    #[must_use]
    pub(crate) fn spine_watermark(&self) -> Arc<SpineWatermark> {
        self.spine.clone()
    }

    /// The latest recorded instant on the live spine (the default replay target),
    /// or `Ticks(0)` before any event.
    ///
    /// O(1): the newest live event's tick, not a `max` fold over the ring.
    /// SOUND because this recorder's spine is monotone in `ts` BY
    /// CONSTRUCTION — every append (`RawIn`, `Reply`, `Resize`, `Keyframe`)
    /// stamps `self.now()`, which is `epoch.elapsed()` off ONE `Instant`, and
    /// every append is serialized on the single temporal writer thread (the
    /// reader hands bursts over a FIFO, and `Resize` is enqueued on that SAME
    /// FIFO under `term_lock` precisely so the ordering holds). A monotone
    /// clock read in append order gives a nondecreasing `ts` down the ring, so
    /// `back()` IS the max — and it was being paid for with a walk over up to
    /// `MAX_LOG_EVENTS` = 65,536 events, on the default replay target, from
    /// inside the re-keyframe path that runs every 2 MiB of recorded input.
    #[must_use]
    pub(crate) fn latest_tick(&self) -> Ticks {
        self.log.newest_live().map(|e| e.ts).unwrap_or_default()
    }

    /// Resolve a `RawIn` handle to its retained slice, or `None` if the blob
    /// has aged out of the bounded retention window (or the handle is a
    /// reply's, whose slot holds none). The slot deque is a contiguous FIFO
    /// window `[base, next_blob)` — eviction only pops the front — so the handle
    /// resolves by offset from [`blob_base`](Self::blob_base). Bounds-checked:
    /// an evicted (or not-yet-issued) id yields `None`, never a mis-indexed read
    /// after `pop_front` shifted positions.
    fn blob(&self, id: BlobId) -> Option<&InputSlice> {
        let idx = usize::try_from(id.0.checked_sub(self.blob_base())?).ok()?;
        self.blobs.get(idx)?.as_ref()
    }

    /// [`Self::blob`]'s bytes.
    #[cfg(test)]
    fn blob_bytes(&self, id: BlobId) -> Option<&[u8]> {
        self.blob(id).map(InputSlice::bytes)
    }

    /// Every retained `RawIn` slice, oldest first: the batch it shares and its
    /// range of it.
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
    pub(crate) fn retained_inputs(&self) -> Vec<(Arc<[u8]>, Range<usize>)> {
        self.blobs
            .iter()
            .flatten()
            .map(|input| (input.batch.clone(), input.range.clone()))
            .collect()
    }

    /// The base keyframe for a replay to instant `at`: the greatest-`seq`
    /// retained keyframe with `ts <= at` (every retained keyframe is live on
    /// the spine). `None` when no such keyframe exists (bounded retention won —
    /// the honest, lossy answer `replay_plan` already contracts for).
    ///
    /// WHY THIS IS THE SAME ANSWER the old spine scan produced, in O(1) instead
    /// of O(`MAX_LOG_EVENTS`). The old code walked every live event, kept the
    /// last `Op::Keyframe` with `ts <= at`, and then REQUIRED that one to still
    /// be retained here (`?` on the checkpoint lookup) — it never fell back to
    /// an older one. Take `k0` = that event. Two cases, and they agree:
    ///
    ///  * `k0` is retained ⇒ it is in this deque and `ts <= at`; and no
    ///    retained keyframe newer than `k0` can qualify, because every retained
    ///    keyframe is live and `k0` was the greatest live qualifier — so the
    ///    newer ones all have `ts > at`. Same pick.
    ///  * `k0` is NOT retained ⇒ `k0` is older than every entry here (this deque
    ///    keeps the newest keyframes and drops only from its front), so every
    ///    entry here is live and, by the same maximality argument, has
    ///    `ts > at`. This search finds nothing and returns `None` — which is
    ///    exactly what the `?` on the old checkpoint lookup did.
    ///
    /// LIVENESS IS NOT OPTIONAL, and it holds by construction: an evicted
    /// `Op::Keyframe` event takes its checkpoint with it ([`append`](Self::append)).
    /// Seeding a replay from a keyframe whose event left the ring would fold a
    /// forward chain missing everything between it and the ring's low-water and
    /// hand back a SILENTLY WRONG engine.
    fn base_keyframe(&self, at: Ticks) -> Option<&RetainedKeyframe> {
        self.keyframes.iter().rev().find(|kf| kf.ts <= at)
    }

    /// What a replay to instant `at` folds, lifted out of the recorder: the
    /// nearest retained keyframe with `ts <= at` and the recorded `RawIn`/
    /// `Resize` events after it up to `at`, in seq order, each payload a
    /// refcount. `Reply` is NOT re-emitted (the design's contract) and an
    /// intermediate keyframe never sits in the range (the base is the newest one
    /// at or before `at`).
    ///
    /// `None` when `at` is unreachable: the base keyframe aged out, a needed
    /// input blob was evicted, or `at` lies past the end of the base's chain,
    /// where input the reader dropped is missing ([`RetainedKeyframe::cut`]).
    /// Degrades to `None` — never a panic or an out-of-bounds read (the B.8.2
    /// lossy-but-honest contract).
    fn plan(&self, at: Ticks) -> Option<ReplayPlan> {
        // Base keyframe: O(MAX_KEYFRAMES) over the retained deque (which carries
        // each keyframe's own spine coordinates) instead of a full walk of the
        // spine looking for the last `Op::Keyframe` — see `base_keyframe` for
        // why the two pick the same entry.
        let base = self.base_keyframe(at)?;
        if base.cut.is_some_and(|end| at > end) {
            return None;
        }
        // Every live event AFTER the seed with ts <= at, in seq order.
        //
        // TWO SCANS DELETED. The seek (`live_after`) starts at the base instead
        // of re-walking the ~65k events BEFORE it — and the base is by design a
        // RECENT keyframe, so that prefix was almost the whole ring. The tail
        // then BREAKS rather than `continue`s on `ts > at`: identical set of
        // folded events, because `ts` is nondecreasing down this recorder's
        // spine (one monotone clock, one serialized writer — see `latest_tick`),
        // so the first event past `at` proves every later one is too.
        let mut steps = Vec::new();
        for ev in self.log.live_after(base.seq) {
            if ev.ts > at {
                break;
            }
            match ev.op {
                Op::RawIn(id) => steps.push(Step::RawIn(self.blob(id)?.clone())),
                Op::Resize { rows, cols } => steps.push(Step::Resize { rows, cols }),
                _ => {}
            }
        }
        Some(ReplayPlan {
            base: base.checkpoint.clone(),
            steps,
            policy: self.resize_policy,
            host_resyncs: self.host_resyncs_alt_screen,
        })
    }

    /// The replay `temporal` folds for instant `at` (default: the spine's end)
    /// — the read half of the hydratable spine (B.9) — or `None` when `at` is
    /// unreachable ([`Self::plan`]). Built under the recorder's lock, folded
    /// after it is released ([`ReplayPlan::run`]).
    ///
    /// While a break is open ([`SpineWatermark::break_open`]) the spine ends
    /// before input the engine processed, so a target past its last event —
    /// the default among them — is unreachable too. A target at or before that
    /// event replays: every input queued before the drop is on the spine by
    /// then, or reaches it later with a later tick.
    #[must_use]
    pub(crate) fn replay_plan(&self, at: Option<Ticks>) -> Option<ReplayPlan> {
        let latest = self.latest_tick();
        if self.spine.break_open() && at.is_none_or(|tick| tick > latest) {
            return None;
        }
        self.plan(at.unwrap_or(latest))
    }

    /// [`Self::replay_plan`], folded in place.
    #[cfg(test)]
    pub(crate) fn replay_at(&self, at: Option<Ticks>) -> Option<Terminal> {
        self.replay_plan(at).map(ReplayPlan::run)
    }
}

/// One retained keyframe: the serialized checkpoint PLUS the spine coordinates
/// of the `Op::Keyframe` event that announced it.
///
/// Keeping `(seq, ts)` here is what turns the base-keyframe lookup from a walk
/// of the whole 65k-event spine into a four-entry scan, and `first_blob` makes
/// "is its chain whole" one comparison; together they cost 24 bytes against a
/// checkpoint that carries a whole serialized grid.
struct RetainedKeyframe {
    /// The spine handle this checkpoint is referenced by. Only the tests read
    /// it, to hold the `(seq, ts)` index to the spine's own `Op::Keyframe(id)`.
    #[cfg(test)]
    id: KeyframeId,
    /// The `Seq` its `Op::Keyframe` event was appended at — the fold's start
    /// cursor, and the event whose eviction takes this keyframe with it.
    seq: Seq,
    /// The tick it was recorded at — compared against the replay target.
    ts: Ticks,
    /// `next_blob` when it was recorded: the first blob id of its chain (every
    /// `RawIn` after it). The chain is whole while no blob at or past it was
    /// evicted, `first_blob >= blob_base()`.
    first_blob: u64,
    /// Where a break ended its chain: the tick of the last event before input
    /// the reader dropped ([`TemporalRecorder::close_break`]). No replay from
    /// this keyframe reaches a later tick; the next keyframe, of the live
    /// engine, is where the spine is whole again. Only a keyframe with a newer
    /// one after it is ever cut.
    cut: Option<Ticks>,
    /// The serialized engine state, shared with the replays lifted out of the
    /// recorder ([`ReplayPlan`]).
    checkpoint: Arc<TerminalCheckpoint>,
}

impl RetainedKeyframe {
    /// The retained-byte charge this keyframe carries against the budget: its
    /// grid plus any alt grid. One definition for the charge, its refund
    /// ([`TemporalRecorder::pop_keyframe`]) and the pressure trigger.
    fn charge(&self) -> usize {
        self.checkpoint.grid.len() + self.checkpoint.alt_grid.as_ref().map_or(0, Vec::len)
    }
}

impl Default for TemporalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// The writer thread's step for one message from the reader: fold it into the
/// spine under the recorder's lock ([`TemporalRecorder::apply`]), then mint any
/// re-keyframe it made due with the lock released ([`Rekey::mint`]) and re-lock
/// only to record it. Neither `temporal` nor `temporal status` waits on a
/// replay.
pub(crate) fn write(recorder: &Mutex<TemporalRecorder>, msg: TemporalMsg) {
    let lock = || recorder.lock().unwrap_or_else(PoisonError::into_inner);
    let due = lock().apply(msg);
    if let Some(due) = due {
        let minted = due.mint();
        lock().rekeyed(minted);
    }
}

/// A replay lifted out of the recorder ([`TemporalRecorder::replay_plan`]): the
/// base keyframe and the input and resizes after it, each a refcount on what
/// the recorder retains, with the session's seam policy. It is folded after the
/// recorder's lock is released ([`Self::run`]).
pub(crate) struct ReplayPlan {
    base: Arc<TerminalCheckpoint>,
    steps: Vec<Step>,
    policy: ResizePolicy,
    /// The session's host leaves a dead app's alternate screen itself
    /// ([`TemporalRecorder::set_host_resyncs_alt_screen`]).
    host_resyncs: bool,
}

/// One recorded event a replay folds.
enum Step {
    RawIn(InputSlice),
    Resize { rows: u16, cols: u16 },
}

impl ReplayPlan {
    /// Fold the plan: a fresh [`ReplayEngine`] of the session's seam policy
    /// ([`TemporalRecorder::set_resize_policy`]) and its host's alternate-screen
    /// recovery ([`TemporalRecorder::set_host_resyncs_alt_screen`]) seeded from
    /// the base keyframe, fed every step through its `process`/`resize`. The replayed terminal
    /// carries no host effects (no callbacks, default auth), which is what a
    /// scrub needs: a fully-inspectable buffer whose grid matches the source.
    #[must_use]
    pub(crate) fn run(self) -> Terminal {
        #[cfg(test)]
        replay_hook::before_replay();
        let mut replay = ReplayEngine::from_checkpoint(&self.base, self.policy)
            .with_host_resyncs_alt_screen(self.host_resyncs);
        for step in self.steps {
            match step {
                Step::RawIn(input) => replay.process(input.bytes()),
                Step::Resize { rows, cols } => replay.resize(rows, cols),
            }
        }
        replay.into_terminal()
    }
}

/// A due re-keyframe ([`TemporalRecorder::apply`]): the replay to the spine's end
/// from the newest keyframe, and the spine head it was planned at.
#[must_use = "the budget holds once the re-keyframe is recorded (`TemporalRecorder::rekeyed`)"]
pub(crate) struct Rekey {
    plan: ReplayPlan,
    head: Seq,
}

impl Rekey {
    /// Replay to the spine's end and checkpoint the result, with the recorder
    /// unlocked.
    ///
    /// GUARD: only a parser-GROUND engine is checkpointed. A recorded RawIn
    /// burst is an arbitrary PTY read, so the burst that made the re-keyframe
    /// due can end MID-ESCAPE (a split CSI), and replaying it leaves the parser
    /// inside that sequence. Such a replay mints nothing and the re-keyframe
    /// waits for the next `RawIn`, which may end it ([`TemporalRecorder::rekeyed`]).
    ///
    /// BOUND: the keyframe carries the newest [`REKEY_SCROLLBACK_LINES`] of
    /// history. A replay restores its keyframe's scrollback unlimited (a
    /// restore must not silently drop history), so a re-keyframe of the whole
    /// replay would ACCUMULATE all scrollback since t0 and bloat past the
    /// budget. Older scrollback ages out honestly (the bounded-retention
    /// contract); the recent `[keyframe..latest]` window stays faithful.
    pub(crate) fn mint(self) -> Minted {
        let live = self.plan.run();
        let checkpoint = if live.parser_is_ground() {
            live.checkpoint_continuing_bounded(REKEY_SCROLLBACK_LINES)
        } else {
            None
        };
        Minted {
            head: self.head,
            checkpoint,
        }
    }
}

/// What [`Rekey::mint`] produced, for [`TemporalRecorder::rekeyed`]: the
/// keyframe, or `None` for a replay that ended inside an escape sequence.
pub(crate) struct Minted {
    head: Seq,
    checkpoint: Option<TerminalCheckpoint>,
}

/// Test-only hook [`ReplayPlan::run`] calls first, so a test can probe the
/// recorder lock from inside the fold.
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

    /// Run `hook` at the start of every later replay on this thread, until the
    /// returned guard drops.
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

/// Test-only count of the keyframes of the live engine [`SpineMark::resync`]
/// projected on this thread, so a test tells a hold that paid for one from a
/// hold that did not.
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

/// Events after the newest keyframe that make a re-keyframe due: a quarter of
/// the spine's ring, so the newest keyframe's own event stays live while the
/// spine grows by small events that never reach the byte interval.
const REKEY_EVENTS: u64 = (aterm_buffer::MAX_LOG_EVENTS / 4) as u64;

/// The fewest chain bytes the byte and pressure triggers re-keyframe at (the
/// interval's floor): at most one replay per 64 KiB of input, however small
/// the budget or large the keyframe.
const MIN_REKEY_BYTES: usize = 64 * 1024;

/// A re-keyframe trigger a test turns off ([`TemporalRecorder::disabled_trigger`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RekeyTrigger {
    Events,
    Pressure,
}

/// The scrollback a re-keyframe retains (a recent window). Bounds keyframe size so
/// periodic re-keyframing cannot accumulate all scrollback since t0 (replayed
/// scrollback is restored unlimited); older history ages out under the byte budget,
/// while the recent `[keyframe..latest]` replay window stays faithful.
const REKEY_SCROLLBACK_LINES: usize = 2048;

/// The number of keyframes retained. The newest seeds a `[keyframe..latest]` replay;
/// the rest let recent PAST instants replay too. Bounded so accumulating keyframes
/// cannot fill the budget, and spaced by the re-keyframe triggers so the four
/// span about one ring of events.
const MAX_KEYFRAMES: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply keeps its place on the spine, but no replay reads its bytes, so
    /// none are retained or charged: its handle resolves to nothing.
    #[test]
    fn records_raw_in_reply_resize_on_one_spine() {
        let mut r = TemporalRecorder::new();
        r.record_raw_in(b"ls -la\n");
        r.record_reply(); // a DSR reply
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

    /// DIFFERENTIAL for the whole `replay_at` seek. The reference here is the
    /// EXACT pre-change body — full spine scan for the base, full spine scan for
    /// the fold, `continue` (not `break`) on `ts > at` — run against the same
    /// recorder, at every interesting instant: before the first keyframe, on and
    /// between recorded ticks, at the latest tick, and past it. The two must
    /// agree on reachability AND on the reconstructed screen text.
    ///
    /// It also pins the two invariants the seek rests on, so a future change
    /// that broke either would fail HERE rather than silently hand back a
    /// wrong-state engine: the spine's ticks are nondecreasing, and the retained
    /// keyframes' `(seq, ts)` coordinates agree with the spine events that
    /// announced them.
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

        // INVARIANT 1: nondecreasing ticks down the live spine (what turns the
        // `max` fold into `back()` and the `continue` into a `break`).
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

        // INVARIANT 2: every retained keyframe is live, and its coordinates
        // agree with the spine.
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

        // The pre-change body, verbatim, as the oracle.
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
        // And the default target (`None`) still lands on the latest instant.
        assert_eq!(
            r.replay_at(None).map(|t| screen_of(&t)),
            reference(latest),
            "the default replay target must still be the latest recorded instant"
        );
    }

    /// The visible screen text of a reconstructed engine, for differential
    /// comparison (a `Terminal` is not `PartialEq`). Same row reader the
    /// control-plane `screen` verb and the subscribe push loop use.
    fn screen_of(t: &Terminal) -> Vec<String> {
        (0..t.rows() as usize)
            .map(|r| crate::control::visible_row(t, r))
            .collect()
    }

    #[test]
    fn ticks_are_monotone_nondecreasing() {
        let r = TemporalRecorder::new();
        let a = r.now();
        let b = r.now();
        assert!(b >= a, "recorder ticks must be monotone non-decreasing");
    }

    #[test]
    fn shared_burst_is_retained_not_copied_and_refunded_on_eviction() {
        let burst: Arc<[u8]> = Arc::from(&[b'y'; 64][..]);
        let mut r = TemporalRecorder::with_budget(1024);
        r.record_raw_in_shared(burst.clone());
        // The recorder retains the SAME allocation (refcount bump, no re-copy)...
        assert_eq!(Arc::strong_count(&burst), 2, "blob shares the caller's Arc");
        // ...charging exactly the payload len against the budget.
        assert_eq!(r.used, 64);
        // Flood past the budget: evicting the oldest blob drops the shared ref
        // and refunds the identical len.
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
        // Tiny budget: four 64-byte blobs fit, so older blobs get reclaimed,
        // but the SPINE keeps every event — bounding payload RAM never rewrites
        // history's length.
        let mut r = TemporalRecorder::with_budget(256);
        for _ in 0..1000 {
            r.record_raw_in(&[b'x'; 64]);
            assert!(r.used <= r.budget, "used={} past the budget", r.used);
        }
        assert_eq!(r.total_events(), 1000);
        assert_eq!(r.live_events(), 1000, "the ring holds every event");
        assert_eq!(r.dropped_events(), 0);
        // With no keyframe no replay can read a blob, so the budget keeps
        // exactly the newest four that fit.
        assert_eq!(r.used, 256);
        assert_eq!((r.blob_base(), r.blobs.len()), (996, 4));
    }

    #[test]
    fn replay_reconstructs_screen_from_keyframe_and_rawin() {
        let mut r = TemporalRecorder::new();
        // Seed a t0 keyframe of a blank, parser-ground engine, then feed input.
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        r.record_raw_in(b"seed\r\n");

        let term = r
            .replay_at(None)
            .expect("base keyframe retained -> replay reconstructs");
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert!(row0.starts_with("seed"), "replayed row 0 = {row0:?}");
    }

    /// B.2.3: a resize is a first-class spine event and replay applies it in SPINE
    /// ORDER, so a cursor-addressed write BEFORE a resize replays at its PRE-resize
    /// column — never re-laid-out at the post-resize width. This is the invariant the
    /// under-`term_lock` FIFO enqueue (reader per-chunk `RawIn` + main-thread `Resize`)
    /// upholds: if a resize could jump ahead of already-processed input on the spine,
    /// the write would land at the wrong column on replay.
    #[test]
    fn replay_applies_resize_in_spine_order_after_cursor_addressed_write() {
        let mut r = TemporalRecorder::new();
        // Seed a 4x10 keyframe (parser-ground, blank).
        let blank = aterm_core::terminal::Terminal::new(4, 10);
        r.record_keyframe(blank.checkpoint());
        // Move to column 16 (1-based) and write X. At width 10 the cursor CLAMPS to
        // the last column (col 9, 0-based).
        r.record_raw_in(b"\x1b[16GX");
        // THEN widen to 20 columns (recorded AFTER the write — the spine order).
        r.record_resize(4, 20);

        let term = r.replay_at(None).expect("replay");
        assert_eq!(term.cols(), 20, "the resize is applied");
        // X sits at col 9 (clamped at the OLD 10-col width), proving the write replayed
        // BEFORE the resize. If the resize had jumped ahead (the bug), the width would
        // be 20 when the write ran and X would land at col 15.
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert_eq!(
            row0.find('X'),
            Some(9),
            "X at the pre-resize column: row0={row0:?}"
        );
    }

    /// A ConPTY session's recorded resizes replay under ITS seam policy. The
    /// live engine keeps all 7 history lines across a 6→10 grow and the
    /// conhost repaint recorded after it; the replay must rebuild that engine,
    /// not the one a Native grow leaves once the same repaint lands (the 4
    /// revealed lines painted over, 3 left in history).
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
        // conhost's repaint: the pre-grow viewport from row 0, the grown rows
        // blank, the cursor back on its row.
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

        // Teeth: the same spine replayed as Native is the engine the live one
        // is not.
        r.set_resize_policy(ResizePolicy::Native);
        let native = r.replay_at(None).expect("replay");
        assert_eq!(native.grid().scrollback_lines(), 3);
    }

    /// A replay is the engine of the session's seam policy whatever its keyframe
    /// carried ([`ReplayEngine`]): a ConPTY session's replay from a keyframe taken
    /// with the Native posture honors conhost's `?9001h`, and its archive is off.
    /// Negative control: the keyframe restored bare keeps the posture it was taken
    /// with, so the `?9001h` changes nothing, and it archives.
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

    /// An OpenConsole session's killed pager replays under ITS recovery: the
    /// live engine stays on the dead screen at the prompt and leaves at the
    /// host's forwarded `?1049l`, so the replay must too, or it draws the
    /// prompt on the primary screen the live one never put it on.
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
        // The pager is killed; the shell prompts on the host's dead screen.
        feed(
            &mut live,
            b"\r\n\x1b]133;A\x07C:\\Users\\me> \x1b]133;B\x07",
        );
        assert!(live.take_alt_screen_resync_request(), "precondition");
        assert!(live.is_alternate_screen(), "precondition");
        // The host's leave, forwarded, then the next command.
        feed(&mut live, b"\x1b[?1049l");
        feed(&mut live, b"dir\r\nx\r\n\x1b]133;A\x07C:\\Users\\me> ");
        assert!(!live.is_alternate_screen(), "precondition");

        let replayed = r.replay_at(None).expect("replay");
        assert_eq!(screen_of(&replayed), screen_of(&live));
        assert_eq!(
            (replayed.cursor().row, replayed.cursor().col),
            (live.cursor().row, live.cursor().col)
        );

        // Teeth: the same spine replayed with the local leave is not the
        // live engine.
        r.set_host_resyncs_alt_screen(false);
        let local = r.replay_at(None).expect("replay");
        assert_ne!(screen_of(&local), screen_of(&live));
    }

    #[test]
    fn blob_handle_resolves_after_eviction_shifts_the_deque() {
        // Budget 150 retains two 64-byte blobs; the third evicts the oldest, so the
        // deque's front is no longer BlobId(0) — the handle-resolution base must
        // track that shift or the reader reads the wrong bytes.
        let mut r = TemporalRecorder::with_budget(150);
        r.record_raw_in(&[b'a'; 64]); // BlobId(0)
        r.record_raw_in(&[b'b'; 64]); // BlobId(1)
        r.record_raw_in(&[b'c'; 64]); // BlobId(2) -> evicts BlobId(0)

        assert!(r.blob_bytes(BlobId(0)).is_none(), "evicted handle => None");
        assert_eq!(r.blob_bytes(BlobId(1)), Some(&[b'b'; 64][..]));
        assert_eq!(r.blob_bytes(BlobId(2)), Some(&[b'c'; 64][..]));
        assert!(r.blob_bytes(BlobId(3)).is_none(), "unissued handle => None");
    }

    /// R19: periodic re-keyframing keeps a recent `[keyframe..latest]` window
    /// reachable even after cumulative I/O exceeds the budget MANY times over —
    /// where a single t0 keyframe would be orphaned and replay would die forever.
    /// The keyframes stay BOUNDED (their scrollback is trimmed), so they do not
    /// accumulate and blow the budget.
    #[test]
    fn rekeyframing_keeps_the_latest_reachable_past_the_budget() {
        // A realistic-shaped budget with a modest screen. `interval` floors at
        // 64 KiB, so re-keyframing engages here.
        let mut r = TemporalRecorder::with_budget(1024 * 1024);
        let blank = aterm_core::terminal::Terminal::new(6, 24);
        r.record_keyframe(blank.checkpoint());
        // Flood ~4 MiB (4x the budget) in newline-terminated bursts (grows
        // scrollback), then a recognizable final line.
        for i in 0..4096u32 {
            r.record_raw_in(format!("row {i:06}\r\n").as_bytes());
            r.record_raw_in(&[b'.'; 1000]);
            r.record_raw_in(b"\r\n");
        }
        r.record_raw_in(b"FINAL-SENTINEL-LINE\r\n");

        // Replay to the LATEST instant reconstructs the recent screen (the window
        // survived eviction because re-keyframing minted fresh, bounded bases).
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
        // Re-keyframing happened, and the keyframes did NOT accumulate unboundedly:
        // the recorder stays within its byte budget.
        assert!(
            (2..=MAX_KEYFRAMES).contains(&r.keyframe_count()),
            "re-keyframing minted fresh keyframes, at most {MAX_KEYFRAMES} kept: {}",
            r.keyframe_count()
        );
        assert!(r.used <= r.budget, "used={} past the budget", r.used);
    }

    // ---- RETENTION: THE RING, THE BUDGET AND THE RE-KEYFRAME TRIGGERS -------

    /// What a long run must leave in any regime: the budget holds, every
    /// eviction is counted, an evicted event took its payload with it (no more
    /// blob slots than live events), and every retained keyframe is live.
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

    /// Events far below the byte interval (8 bytes each): three rings of them
    /// stay under the 2 MiB byte trigger, so only the event trigger mints a
    /// keyframe before the base keyframe's own event leaves the ring. Replay
    /// stays reachable, and equal to the live screen, throughout. Negative
    /// control: with the event trigger off, the seed keyframe leaves the ring
    /// with its event and nothing replays from then on.
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
                // Mid-way between event-trigger keyframes, so a probe folds a chain.
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

    /// Under a 1 MiB budget, events of 40 bytes: the budget binds long before
    /// the ring does (about 26k blobs fill it), and more than twice the ring
    /// passes. A recorder that charged every evicted event against the budget
    /// lost its newest chain here at about 78k events. Now the byte trigger
    /// keeps a window whose chain fits, and eviction takes older chains first,
    /// so replay stays reachable and equal to the live screen throughout.
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

    /// A keyframe that alone fills most of the budget (a 200-column screen
    /// over 2048 lines of history): the newest chain has less room than the
    /// byte interval, so the byte trigger would come after eviction had taken
    /// the chain. Budget pressure re-keyframes first, and replay stays
    /// reachable and equal to the live screen. Negative control: with the
    /// pressure trigger off, the chain is evicted and nothing replays again.
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

    /// The budget loses the oldest window first: while an older keyframe
    /// remains, a newer keyframe's chain is never touched, and
    /// `reachable_from` names the oldest keyframe whose chain is whole. Only
    /// when the newest keyframe and its chain alone exceed the budget does the
    /// chain go, after everything else, and then nothing replays.
    #[test]
    fn the_budget_loses_the_oldest_window_first_and_says_where_replay_starts() {
        let blank = Terminal::new(4, 10).checkpoint();
        let c = blank.grid.len() + blank.alt_grid.as_ref().map_or(0, Vec::len);
        assert!(6 * c < MIN_REKEY_BYTES, "REACH: no re-keyframe is due");
        let oldest = |r: &TemporalRecorder| r.oldest_replayable().map(|kf| kf.id);
        // Room for three keyframes and two and a half bursts, all c bytes.
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

    /// A due re-keyframe whose replay ends inside an escape sequence cannot be
    /// checkpointed, so it waits: a reply or a resize cannot end the sequence,
    /// and the input that does mints the keyframe.
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

    // ---- THE RESIZE WATERMARK (P4(c)) --------------------------------------

    /// A Claude-shaped alternate-screen frame: `L<r>` on every row and `E` in
    /// its last column (`cols`), the cursor parked below the top, so a one-row
    /// shrink DEMOTES the top row and a one-column shrink cuts every `E`.
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

    /// The writer, naming what it folds: every queued message, in order.
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

    /// A queued message as the tests name it, `RawIn` with its bytes.
    fn shape(msg: &TemporalMsg) -> String {
        match msg {
            TemporalMsg::RawIn(input) => format!("in:{}", String::from_utf8_lossy(input.bytes())),
            TemporalMsg::Reply => "reply".to_string(),
            TemporalMsg::Resize { rows, cols } => format!("r:{rows}x{cols}"),
            TemporalMsg::ResizesLost(n) => format!("lost:{n}"),
            TemporalMsg::Keyframe(_) => "keyframe".to_string(),
        }
    }

    /// One reader slice: read the engine before, process, record — the
    /// reader's order inside one hold.
    fn slice(live: &mut Terminal, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
        let pre = SpinePre::take(live);
        live.process(bytes);
        mark.record(tx, pre, live, InputSlice::from(bytes));
    }

    /// A recording started the way a session starts one: the keyframe and
    /// the session's watermark seeded from the same engine reading.
    fn seeded(live: &Terminal) -> TemporalRecorder {
        let mut r = TemporalRecorder::new();
        r.record_keyframe(live.checkpoint());
        r.spine_watermark().seed(SpinePre::take(live));
        r
    }

    /// The writer: every queued message, in order.
    fn drain(rx: &TemporalRx, r: &mut TemporalRecorder) -> usize {
        let mut n = 0;
        while let Some(msg) = rx.try_recv() {
            r.apply_inline(msg);
            n += 1;
        }
        n
    }

    /// A NET-ZERO FLAP BETWEEN TWO READS that the engine cannot undo: 20 ->
    /// 19 -> 20 columns on the alternate screen cuts the last column for good
    /// (the grid's resize undo serves a rows-only shrink, never a width
    /// change) and leaves the geometry where it was. The spine holds both
    /// halves (the engine's resize journal names them), so a replay reaches the
    /// live, CUT screen. The old `(rows, cols)` watermark saw the same geometry
    /// at both reads and recorded no resize: that spine replays to a screen
    /// the session never showed.
    ///
    /// The incident's own flap (6 -> 5 -> 6 rows, nothing between the halves)
    /// is now an identity — the engine hands the demoted row back — and the
    /// spine still holds both halves, so the replay folds the same undo.
    #[test]
    fn a_net_zero_flap_between_two_reads_reaches_the_spine_and_the_replay() {
        const ROWS: u16 = 6;
        const COLS: u16 = 20;
        let mut live = Terminal::new(ROWS, COLS);
        let mut r = seeded(&live);
        let (tx, rx) = queue(64);
        let mut mark = SpineMark::attach(r.spine_watermark());
        let frame = alt_frame(ROWS, COLS);
        // A diff frame at an absolute row (no clear), after the flap.
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

        // The old watermark's spine: the same bytes, no resize (the geometry
        // at both reads was 6x20).
        let mut old = TemporalRecorder::new();
        old.record_keyframe(Terminal::new(ROWS, COLS).checkpoint());
        old.record_raw_in(&frame);
        old.record_raw_in(diff);
        let uncut = old.replay_at(None).expect("replay");
        assert_ne!(screen_of(&uncut), screen_of(&live), "the flap it missed");

        // The incident's flap: undone by the engine, and still on the spine.
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

    /// The slices of ONE batch, each processed in its own hold with a width
    /// flap between them (the reader's slice loop while the window resizes
    /// between two holds): the spine holds each slice after the resizes the
    /// engine applied before it, and the replay reaches the live screen. Every
    /// queued and retained slice is a range of the reader's batch, never a
    /// copy of it, and the batch is charged its length once.
    ///
    /// NEGATIVE CONTROL: the same bytes with the resizes after all of them (a
    /// tap that queued the batch once, after its holds) replay a screen the
    /// session never showed: the mark the last slice put in the regrown column
    /// is cut.
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
            live.process(&batch[range.clone()]);
            mark.record(
                &tx,
                pre,
                &live,
                InputSlice::new(batch.clone(), range.clone()),
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

    /// More resizes between two reads than the engine's journal keeps: the
    /// overrun is COUNTED (`lost_resizes`, once), the kept ones reach the
    /// spine in order, and the replay ends at the live geometry.
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

    /// A full queue: the resizes that fit advance the watermark, and the
    /// slice's input is DROPPED, never queued ahead of the rest. The drop opens
    /// a break, so the next slice queues a keyframe of the live engine in place
    /// of its input; it holds the rest of the flap and both slices, and the
    /// replay reaches the live screen. (An unseeded mark: its first record is
    /// the live geometry.)
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

    /// A FLAP WHILE NO READER WAS ATTACHED (a park, a deferred attach): the
    /// next reader starts where the session's last one left the spine, reads
    /// both halves from the engine's journal, and the replay reaches the live,
    /// CUT screen (a width flap, which the engine's resize undo does not
    /// serve). The negative control is a reader that starts over, as each
    /// reader's own watermark used to: it records only the live geometry,
    /// which the flap left where it was, so its spine replays uncut.
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
            drop(first); // the reader parks
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

    /// More resizes while no reader was attached than the engine's journal
    /// keeps: the next reader COUNTS the overrun (`lost_resizes`) instead of
    /// starting over silently, and its replay ends at the live geometry.
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

    /// `pending`: the resizes the live engine applied that no reader has
    /// handed the spine yet, which `temporal` discloses while a flap waits for
    /// the program's next output. That output's hold hands both halves over.
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
        // Never seeded (no recording): every resize this engine applied.
        assert_eq!(SpineWatermark::default().pending(&live), 2);
    }

    #[test]
    fn replay_degrades_to_none_when_needed_blobs_were_evicted() {
        // A flood past a tiny budget: the newest keyframe and its chain go last,
        // but together they exceed this budget, so input blobs are reclaimed. A
        // full replay to the latest instant cannot re-feed the aged-out bytes, so
        // it degrades to None — never a panic or a mis-resolved blob (the
        // bounded-retention honest-partial-reach contract).
        let mut r = TemporalRecorder::with_budget(128);
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        for _ in 0..64 {
            r.record_raw_in(&[b'x'; 64]);
        }
        assert!(r.replay_at(None).is_none());
        assert_eq!(r.reachable_from(), None, "status says nothing replays");
    }

    // ---- BREAKS: INPUT THE READER COULD NOT QUEUE ----------------------------

    /// Wait until `r`'s clock is more than one microsecond past `tick`, so the
    /// next event lands strictly after `tick + 1`. The clock must advance; the
    /// deadline only detects a hang.
    fn tick_past(r: &TemporalRecorder, tick: Ticks) {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while r.now().0 <= tick.0 + 1 {
            assert!(Instant::now() < deadline, "the recorder clock stopped");
            std::hint::spin_loop();
        }
    }

    /// A `RawIn` the reader could not queue is counted (`dropped_input=`) and
    /// breaks the chain. While the break is open nothing more is queued and the
    /// spine's end does not replay (`reachable_from=-`), though its last event
    /// still does. The reader parks with the break open; the next one queues a
    /// keyframe of the live engine at its first hold, and the writer ends the
    /// old chain at the last event before the drop: a tick past it replays from
    /// neither keyframe, and the spine's end is the live screen again.
    /// Negative control: the same slices through a queue with room drop
    /// nothing, and every tick replays.
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

    /// A reply's instant the reader could not queue is counted
    /// (`dropped_replies=`) and breaks nothing, since no replay reads a reply:
    /// the next slice queues its input and the spine's end replays. Inside a
    /// break a reply is not queued either (a break queues nothing but its
    /// keyframe), and is counted the same way.
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

    /// The keyframe that closes a break needs a parser it can carry: while the
    /// engine sits inside a DCS query (DECRQSS, whose reply lives in its
    /// handler) none is queued and the break stays open. The hold after the
    /// string ends queues it, and it holds every slice since the drop.
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

    /// Nor while the engine sits inside a sequence a keyframe holds only as
    /// one to swallow: a kitty-graphics APC whose consumer started. The live
    /// engine places the image at its end and moves the cursor past it, so the
    /// keyframe waits for the hold after the string ends, and the replay is
    /// the live screen, cursor included. Negative control: a replay from the
    /// keyframe that hold would have queued drops the string's tail and lays
    /// the text after it where the session never showed it.
    #[test]
    fn a_break_waits_out_a_sequence_a_keyframe_would_swallow() {
        // A 1x1 RGBA pixel stretched over 4 columns and 3 rows; the cursor
        // continues on its last row, right after it.
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
            steps: vec![Step::RawIn(InputSlice::from(&tail[..]))],
            policy: ResizePolicy::Native,
            host_resyncs: false,
        };

        slice(&mut live, &mut mark, &tx, tail);
        assert_eq!(drain_named(&rx, &mut r), ["keyframe"]);
        let replayed = r.replay_at(None).expect("replay");
        let view = |t: &Terminal| (screen_of(t), t.cursor());
        assert_eq!(view(&replayed), view(&live));
        assert_ne!(view(&swallowing.run()), view(&live), "the image moved it");
    }

    /// A writer that is gone (its thread ended, as a panic unwinding it does)
    /// takes nothing again, so the holds after a break open on it project no
    /// keyframe under `term`'s lock. Negative control: with the writer there,
    /// the first hold with a free slot projects one and queues it, and the
    /// holds after it, the queue full again, project none.
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

    /// The writer folds a due re-keyframe with the recorder unlocked: a probe
    /// from inside the fold takes the lock, and the keyframe is recorded after.
    /// Negative control: a replay folded inside a hold (as the writer and
    /// `temporal` both did) finds it taken. A spine that moved between the plan
    /// and the record discards the keyframe, which would stand for an earlier
    /// instant than its place.
    #[test]
    fn temporal_replay_runs_with_the_recorder_unlocked() {
        use std::cell::Cell;
        use std::rc::Rc;
        let burst = || TemporalMsg::RawIn(InputSlice::from(&[b'x'; MIN_REKEY_BYTES][..]));
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
        write(&recorder, burst());
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
}
