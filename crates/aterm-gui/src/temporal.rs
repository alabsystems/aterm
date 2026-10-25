// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session temporal recorder.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_buffer::{BlobId, EventLog, KeyframeId, Op, Seq, Ticks, WitnessId};
use aterm_control::replay::ReplayEngine;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{ClockReading, ShellIntegrationNonce, Terminal, TerminalCheckpoint};

/// Default byte budget for retained payloads: input blobs plus keyframes.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// A burst handed from the reader hot path to the temporal writer thread (lock-free, FIFO, bounded: [`queue`]).
pub(crate) enum TemporalMsg {
    RawIn(InputSlice),
    Reply,
    Resize {
        rows: u16,
        cols: u16,
    },
    ResizesLost(u64),
    Keyframe(Arc<TerminalCheckpoint>),
    /// A live keyframe a diverged witness asked for ([`SpineWatermark::keyframe_wanted`]); it closes no break.
    LiveKeyframe(Arc<TerminalCheckpoint>),
    /// The live screen right after the slice queued before it, for a replay to match ([`Fidelity`]).
    Witness(Arc<TerminalCheckpoint>),
}

/// The bytes of one `RawIn` and the clock reading the live engine parsed them at.
#[derive(Clone)]
pub(crate) struct InputSlice {
    batch: Arc<[u8]>,
    range: Range<usize>,
    clock: ClockReading,
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
        }
    }

    /// A copy of `bytes`, parsed live at `clock`.
    pub(crate) fn at(bytes: &[u8], clock: ClockReading) -> Self {
        let batch = Arc::<[u8]>::from(bytes);
        Self::new(batch, 0..bytes.len(), clock)
    }

    fn bytes(&self) -> &[u8] {
        &self.batch[self.range.clone()]
    }

    fn len(&self) -> usize {
        self.range.len()
    }
}

/// A replay keyframe of `term` with each grid's history bounded to `max_history` lines, carrying the shell-integration nonce and revision so a replay verifies shell marks as the live engine does (it stays in memory).
#[must_use]
pub(crate) fn keyframe_of(term: &Terminal, max_history: usize) -> Option<TerminalCheckpoint> {
    let mut checkpoint = term.checkpoint_continuing_bounded(max_history)?;
    checkpoint.shell_integration_nonce = term
        .shell_integration_nonce_in_use()
        .map(ShellIntegrationNonce);
    checkpoint.shell_integration_rev = term.shell_integration_rev().map(str::to_owned);
    Some(checkpoint)
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
    keyframe_wanted: AtomicBool,
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

    /// Whether a diverged witness asked for a live keyframe that no reader has queued yet.
    #[must_use]
    pub(crate) fn keyframe_wanted(&self) -> bool {
        self.keyframe_wanted.load(Ordering::Acquire)
    }
}

/// One reader's resize watermark.
#[derive(Debug, Default)]
pub(crate) struct SpineMark {
    at: Option<(u64, u64)>,
    shared: Arc<SpineWatermark>,
    broken: bool,
    since_witness: usize,
    witnessed_at: Option<aterm_time::Instant>,
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
    pub(crate) const fn take(term: &Terminal) -> Self {
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

    /// Refused: the writer queue takes bytes only with the clock they were parsed at ([`SpineMark::record`]).
    fn bytes(&mut self, _bytes: &[u8]) -> bool {
        false
    }
}

impl SpineMark {
    /// A reader's mark, starting where the session's previous reader left the spine (or where its keyframe seeded it), a break it left open included.
    pub(crate) fn attach(shared: Arc<SpineWatermark>) -> Self {
        Self {
            at: shared.load(),
            broken: shared.break_open(),
            shared,
            since_witness: 0,
            witnessed_at: None,
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

    /// Record one slice on the spine, then a live witness when one is due ([`WITNESS_BYTES`], [`WITNESS_INTERVAL`]).
    pub(crate) fn record(
        &mut self,
        tx: &TemporalTx,
        pre: SpinePre,
        term: &Terminal,
        input: InputSlice,
    ) {
        if self.broken {
            self.resync(tx, term, true);
            return;
        }
        if self.shared.keyframe_wanted() && self.resync(tx, term, false) {
            return;
        }
        let (len, at) = (input.len(), input.clock.monotonic);
        if self.catch_up(&mut TemporalSink(tx), pre, term) && tx.send(TemporalMsg::RawIn(input)) {
            // Past anything the bytes themselves applied (they replay it).
            self.advance((term.resize_lifetime(), term.resize_ordinal()));
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

    fn resync(&mut self, tx: &TemporalTx, term: &Terminal, closes_break: bool) -> bool {
        if !tx.has_room() || term.partial_sequence_swallowed().is_some() {
            return false;
        }
        let Some(checkpoint) = keyframe_of(term, REKEY_SCROLLBACK_LINES) else {
            return false;
        };
        #[cfg(test)]
        resync_probe::projected();
        let checkpoint = Arc::new(checkpoint);
        let msg = if closes_break {
            TemporalMsg::Keyframe(checkpoint)
        } else {
            TemporalMsg::LiveKeyframe(checkpoint)
        };
        if !tx.send(msg) {
            return false;
        }
        self.broken = false;
        self.shared.keyframe_wanted.store(false, Ordering::Release);
        self.since_witness = 0;
        self.witnessed_at = None;
        self.advance((term.resize_lifetime(), term.resize_ordinal()));
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
    #[cfg(test)]
    frozen_now: Option<Ticks>,
    epoch: Instant,
    resize_policy: ResizePolicy,
    host_resyncs_alt_screen: bool,
    handoff: Option<crate::session_timeline::HandoffGap>,
    witnesses: VecDeque<Witnessed>,
    next_witness: u64,
    witnessed: u64,
    diverged: u64,
    chain_diverged: bool,
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

/// One live witness on the spine: its snapshot until a fold judges it, then only its verdict.
struct Witnessed {
    id: WitnessId,
    seq: Seq,
    ts: Ticks,
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
            #[cfg(test)]
            frozen_now: None,
            epoch: Instant::now(),
            resize_policy: ResizePolicy::Native,
            host_resyncs_alt_screen: false,
            handoff: None,
            witnesses: VecDeque::new(),
            next_witness: 0,
            witnessed: 0,
            diverged: 0,
            chain_diverged: false,
        }
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

    /// The current tick on this recorder's timeline (micros since the epoch).
    #[must_use]
    pub(crate) fn now(&self) -> Ticks {
        #[cfg(test)]
        if let Some(frozen) = self.frozen_now {
            return frozen;
        }
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
    pub(crate) fn covering_witness(&self, at: Ticks) -> Option<Ticks> {
        let base = self.base_keyframe(at)?;
        let folded = self.last_folded(base.seq, at);
        self.chain_witnesses(base.seq)
            .find(|w| w.seq > folded)
            .map(|w| w.ts)
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

    /// The seq of the last slice or resize after `base` that a replay to `at` folds ([`Self::plan`]), or `base` when it folds none.
    fn last_folded(&self, base: Seq, at: Ticks) -> Seq {
        self.log
            .live_after(base)
            .take_while(|ev| ev.ts <= at)
            .filter(|ev| matches!(ev.op, Op::RawIn(_) | Op::Resize { .. }))
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
        self.chain_diverged = false;
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
        if self.ends_mid_sequence || self.chain_diverged {
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

    /// Record the verdicts and the keyframe a due re-keyframe minted ([`Rekey::mint`]), unless a witness in its chain diverged, then hold the budget.
    pub(crate) fn rekeyed(&mut self, minted: Minted) {
        self.judge(minted.judged);
        match minted.checkpoint {
            Some(checkpoint) if self.log.head() == minted.head && !self.chain_diverged => {
                self.record_keyframe(checkpoint);
            }
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
        let base = self.keyframes.front().map_or(Seq(u64::MAX), |kf| kf.seq);
        self.drop_witnesses_through(Seq(base.0.saturating_sub(1)));
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
            TemporalMsg::LiveKeyframe(checkpoint) => {
                self.ends_mid_sequence = false;
                self.record_keyframe(checkpoint);
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
        let mut steps = Vec::new();
        for ev in self.log.live_after(base.seq) {
            if ev.ts > at {
                break;
            }
            match ev.op {
                Op::RawIn(id) => steps.push(Step::RawIn(self.blob(id)?.clone())),
                Op::Resize { rows, cols } => steps.push(Step::Resize { rows, cols }),
                Op::Witness(id) => {
                    if let Some(live) = self.pending_witness(id) {
                        steps.push(Step::Witness(id, live));
                    }
                }
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

    /// The distinct instants a Rewind view steps through, oldest first: the oldest keyframe every later instant replays from, then each later slice or resize.
    #[must_use]
    pub(crate) fn rewind_instants(&self) -> Vec<Ticks> {
        if self.spine.break_open() {
            return Vec::new();
        }
        let Some(base) = self.oldest_replayable() else {
            return Vec::new();
        };
        let mut out = vec![base.ts];
        for ev in self.log.live_after(base.seq) {
            if matches!(ev.op, Op::RawIn(_) | Op::Resize { .. })
                && out.last().is_some_and(|last| ev.ts > *last)
            {
                out.push(ev.ts);
            }
        }
        out
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
        charge_of(&self.checkpoint)
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
    Witness(WitnessId, Arc<TerminalCheckpoint>),
}

impl ReplayPlan {
    /// Fold the plan from its base keyframe through a fresh [`ReplayEngine`] configured with [`TemporalRecorder::set_resize_policy`] and [`TemporalRecorder::set_host_resyncs_alt_screen`], each slice at its recorded clock, judging each pending witness it passes by full equality with the replay's `checkpoint_visible()`.
    #[must_use]
    pub(crate) fn fold(self) -> Fold {
        #[cfg(test)]
        replay_hook::before_replay();
        let mut replay = ReplayEngine::from_checkpoint(&self.base, self.policy)
            .with_host_resyncs_alt_screen(self.host_resyncs);
        let mut judged = Vec::new();
        for step in self.steps {
            match step {
                #[cfg(test)]
                Step::RawIn(input) if replay_clock::live() => replay.process(input.bytes()),
                Step::RawIn(input) => replay.process_at(input.bytes(), input.clock),
                Step::Resize { rows, cols } => replay.resize(rows, cols),
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
}

/// A due re-keyframe ([`TemporalRecorder::apply`]).
#[must_use = "the budget holds once the re-keyframe is recorded (`TemporalRecorder::rekeyed`)"]
pub(crate) struct Rekey {
    plan: ReplayPlan,
    head: Seq,
}

impl Rekey {
    /// Replay to the spine's end, judging the witnesses on the way, and checkpoint the result, with the recorder unlocked.
    pub(crate) fn mint(self) -> Minted {
        let Fold { term, judged } = self.plan.fold();
        let checkpoint = if term.parser_is_ground() {
            keyframe_of(&term, REKEY_SCROLLBACK_LINES)
        } else {
            None
        };
        Minted {
            head: self.head,
            checkpoint,
            judged,
        }
    }
}

/// What [`Rekey::mint`] produced, for [`TemporalRecorder::rekeyed`].
pub(crate) struct Minted {
    head: Seq,
    checkpoint: Option<TerminalCheckpoint>,
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
            TemporalMsg::LiveKeyframe(_) => "live-keyframe".to_string(),
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
            steps: vec![Step::RawIn(InputSlice::at(&tail[..], clock(0)))],
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
            let next = Arc::new(keyframe_of(&live, usize::MAX).expect("at ground"));
            r.apply_inline(if live_keyframe {
                TemporalMsg::LiveKeyframe(next)
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
        for (keyframe, accepted) in [(carried, true), (bare, false)] {
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
