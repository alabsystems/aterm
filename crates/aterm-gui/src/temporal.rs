// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session temporal recorder.

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

/// Default byte budget for retained payloads: input blobs plus keyframes.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// A burst handed from the reader hot path to the temporal writer thread (lock-free, FIFO, bounded: [`queue`]).
pub(crate) enum TemporalMsg {
    RawIn(InputSlice),
    Reply,
    Resize { rows: u16, cols: u16 },
    ResizesLost(u64),
    Keyframe(Arc<TerminalCheckpoint>),
}

/// The bytes of one `RawIn`.
#[derive(Clone)]
pub(crate) struct InputSlice {
    batch: Arc<[u8]>,
    range: Range<usize>,
}

impl InputSlice {
    /// `batch[range]`, sharing `batch`.
    pub(crate) fn new(batch: Arc<[u8]>, range: Range<usize>) -> Self {
        assert!(
            range.start <= range.end && range.end <= batch.len(),
            "input slice {range:?} outside its {}-byte batch",
            batch.len()
        );
        Self { batch, range }
    }

    fn bytes(&self) -> &[u8] {
        &self.batch[self.range.clone()]
    }

    fn len(&self) -> usize {
        self.range.len()
    }
}

impl From<Arc<[u8]>> for InputSlice {
    fn from(batch: Arc<[u8]>) -> Self {
        let range = 0..batch.len();
        Self { batch, range }
    }
}

impl From<&[u8]> for InputSlice {
    fn from(bytes: &[u8]) -> Self {
        Self::from(Arc::<[u8]>::from(bytes))
    }
}

/// Open a recording's writer queue, bounded at `cap` messages: the reader's end and the writer thread's.
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

#[derive(Default)]
struct QueueState {
    in_flight: AtomicUsize,
    writer_gone: AtomicBool,
}

/// The reader's end of the writer queue.
pub(crate) struct TemporalTx {
    tx: SyncSender<TemporalMsg>,
    state: Arc<QueueState>,
    cap: usize,
}

impl TemporalTx {
    fn send(&self, msg: TemporalMsg) -> bool {
        self.state.in_flight.fetch_add(1, Ordering::AcqRel);
        let sent = self.tx.try_send(msg).is_ok();
        if !sent {
            self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

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
    /// The next message in FIFO order, waiting while the queue is empty.
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

/// Where the spine's resizes are up to, shared by every reader of a session.
#[derive(Debug, Default)]
pub(crate) struct SpineWatermark {
    lifetime: AtomicU64,
    ordinal: AtomicU64,
    dropped_input: AtomicU64,
    closed: AtomicU64,
    dropped_replies: AtomicU64,
}

impl SpineWatermark {
    /// Start the watermark at `pre`.
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
}

/// One reader's resize watermark.
#[derive(Debug, Default)]
pub(crate) struct SpineMark {
    at: Option<(u64, u64)>,
    shared: Arc<SpineWatermark>,
    broken: bool,
}

/// What one reader hold read before processing its bytes: the engine's resize lifetime, ordinal and geometry.
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

    fn bytes(&mut self, bytes: &[u8]) -> bool {
        self.0.send(TemporalMsg::RawIn(InputSlice::from(bytes)))
    }
}

impl SpineMark {
    /// A reader's mark, starting where the session's previous reader left the spine (or where its keyframe seeded it), a break it left open included.
    pub(crate) fn attach(shared: Arc<SpineWatermark>) -> Self {
        Self {
            at: shared.load(),
            broken: shared.break_open(),
            shared,
        }
    }

    /// Put the watermark at `pre`.
    #[cfg(unix)]
    pub(crate) fn seed(&mut self, pre: SpinePre) {
        self.advance((pre.lifetime, pre.ordinal));
    }

    fn advance(&mut self, at: (u64, u64)) {
        if self.at != Some(at) {
            self.at = Some(at);
            self.shared.store(at);
        }
    }

    /// Record one slice on the spine.
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
            self.advance((term.resize_lifetime(), term.resize_ordinal()));
        }
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
    keyframes: VecDeque<RetainedKeyframe>,
    next_blob: u64,
    next_keyframe: u64,
    used: usize,
    budget: usize,
    lost_resizes: u64,
    spine: Arc<SpineWatermark>,
    bytes_since_keyframe: usize,
    ends_mid_sequence: bool,
    #[cfg(test)]
    disabled_trigger: Option<RekeyTrigger>,
    epoch: Instant,
    resize_policy: ResizePolicy,
    host_resyncs_alt_screen: bool,
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
            epoch: Instant::now(),
            resize_policy: ResizePolicy::Native,
            host_resyncs_alt_screen: false,
            handoff: None,
        }
    }

    /// Mark this spine as one that starts at a seamless update ([`crate::session_timeline::HandoffGap`]).
    pub(crate) fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
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
    pub(crate) fn set_resize_policy(&mut self, policy: ResizePolicy) {
        self.resize_policy = policy;
    }

    /// Replay under the live engine's alternate-screen recovery.
    pub(crate) fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.host_resyncs_alt_screen = on;
    }

    /// The current tick on this recorder's timeline (micros since the epoch).
    #[must_use]
    pub(crate) fn now(&self) -> Ticks {
        Ticks(u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

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

    fn record(&mut self, op: Op) -> Option<Rekey> {
        let ts = self.now();
        self.append(op, ts);
        let due = self.rekey_due();
        if due.is_none() {
            self.enforce_budget();
        }
        due
    }

    /// Record a keyframe (a serialized [`TerminalCheckpoint`]), the seed for replays to instants at or after it.
    pub(crate) fn record_keyframe(&mut self, checkpoint: impl Into<Arc<TerminalCheckpoint>>) {
        let ts = self.now();
        let id = KeyframeId(self.next_keyframe);
        self.next_keyframe += 1;
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
        self.used += keyframe.charge();
        self.keyframes.push_back(keyframe);
        self.bytes_since_keyframe = 0;
        while self.keyframes.len() > MAX_KEYFRAMES {
            self.pop_keyframe();
        }
        self.enforce_budget();
    }

    fn close_break(&mut self, checkpoint: Arc<TerminalCheckpoint>) {
        let end = self.latest_tick();
        if let Some(newest) = self.keyframes.back_mut() {
            newest.cut = Some(end);
        }
        self.ends_mid_sequence = false;
        self.record_keyframe(checkpoint);
        self.spine.closed.fetch_add(1, Ordering::Release);
    }

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

    /// Record the keyframe a due re-keyframe minted ([`Rekey::mint`]), then hold the budget.
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

    fn store_blob(&mut self, input: Option<InputSlice>) -> BlobId {
        let id = BlobId(self.next_blob);
        self.next_blob += 1;
        self.used += input.as_ref().map_or(0, InputSlice::len);
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
            .saturating_sub(slot.as_ref().map_or(0, InputSlice::len));
        true
    }

    fn pop_keyframe(&mut self) -> bool {
        let Some(keyframe) = self.keyframes.pop_front() else {
            return false;
        };
        self.used = self.used.saturating_sub(keyframe.charge());
        true
    }

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
        self.keyframes
            .iter()
            .rev()
            .take_while(|kf| kf.first_blob >= base && kf.cut.is_none())
            .last()
    }

    /// Fold one message from the reader into the spine, in FIFO order: the writer thread's whole job ([`write()`]).
    #[must_use]
    pub(crate) fn apply(&mut self, msg: TemporalMsg) -> Option<Rekey> {
        let op = match msg {
            TemporalMsg::RawIn(input) => {
                self.bytes_since_keyframe += input.len();
                self.ends_mid_sequence = false;
                Op::RawIn(self.store_blob(Some(input)))
            }
            TemporalMsg::Reply => Op::Reply(self.store_blob(None)),
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
    pub(crate) fn lost_resizes(&self) -> u64 {
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

    fn base_keyframe(&self, at: Ticks) -> Option<&RetainedKeyframe> {
        self.keyframes.iter().rev().find(|kf| kf.ts <= at)
    }

    fn plan(&self, at: Ticks) -> Option<ReplayPlan> {
        let base = self.base_keyframe(at)?;
        if base.cut.is_some_and(|end| at > end) {
            return None;
        }
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

    /// The replay `temporal` folds for instant `at` (default: the spine's end).
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

struct RetainedKeyframe {
    #[cfg(test)]
    id: KeyframeId,
    seq: Seq,
    ts: Ticks,
    first_blob: u64,
    cut: Option<Ticks>,
    checkpoint: Arc<TerminalCheckpoint>,
}

impl RetainedKeyframe {
    fn charge(&self) -> usize {
        self.checkpoint.grid.len() + self.checkpoint.alt_grid.as_ref().map_or(0, Vec::len)
    }
}

impl Default for TemporalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// The writer thread's step for one message from the reader.
pub(crate) fn write(recorder: &Mutex<TemporalRecorder>, msg: TemporalMsg) {
    let lock = || recorder.lock().unwrap_or_else(PoisonError::into_inner);
    let due = lock().apply(msg);
    if let Some(due) = due {
        let minted = due.mint();
        lock().rekeyed(minted);
    }
}

/// A replay lifted out of the recorder ([`TemporalRecorder::replay_plan`]).
pub(crate) struct ReplayPlan {
    base: Arc<TerminalCheckpoint>,
    steps: Vec<Step>,
    policy: ResizePolicy,
    host_resyncs: bool,
}

enum Step {
    RawIn(InputSlice),
    Resize { rows: u16, cols: u16 },
}

impl ReplayPlan {
    /// Fold the plan from its base keyframe through a fresh [`ReplayEngine`] configured with [`TemporalRecorder::set_resize_policy`] and [`TemporalRecorder::set_host_resyncs_alt_screen`].
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

/// A due re-keyframe ([`TemporalRecorder::apply`]).
#[must_use = "the budget holds once the re-keyframe is recorded (`TemporalRecorder::rekeyed`)"]
pub(crate) struct Rekey {
    plan: ReplayPlan,
    head: Seq,
}

impl Rekey {
    /// Replay to the spine's end and checkpoint the result, with the recorder unlocked.
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

/// What [`Rekey::mint`] produced, for [`TemporalRecorder::rekeyed`].
pub(crate) struct Minted {
    head: Seq,
    checkpoint: Option<TerminalCheckpoint>,
}

/// Test-only hook [`ReplayPlan::run`] calls first, so a test can probe the recorder lock from inside the fold.
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
    fn replay_reconstructs_screen_from_keyframe_and_rawin() {
        let mut r = TemporalRecorder::new();
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        r.record_raw_in(b"seed\r\n");

        let term = r
            .replay_at(None)
            .expect("base keyframe retained -> replay reconstructs");
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert!(row0.starts_with("seed"), "replayed row 0 = {row0:?}");
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
            TemporalMsg::ResizesLost(n) => format!("lost:{n}"),
            TemporalMsg::Keyframe(_) => "keyframe".to_string(),
        }
    }

    fn slice(live: &mut Terminal, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
        let pre = SpinePre::take(live);
        live.process(bytes);
        mark.record(tx, pre, live, InputSlice::from(bytes));
    }

    fn seeded(live: &Terminal) -> TemporalRecorder {
        let mut r = TemporalRecorder::new();
        r.record_keyframe(live.checkpoint());
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
