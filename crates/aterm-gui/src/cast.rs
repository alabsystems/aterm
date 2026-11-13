// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session asciicast v2 recording ([`CastRecorder`], [`CastSnapshot`]) and live byte fan-out ([`ByteFanout`]).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_control::asciicast::{CastEvent, CastHeader, HandoffHeader};
use aterm_control::replay::ReplayEngine;
use aterm_core::grid::{ResizeLaw, ResizePolicy};
use aterm_core::terminal::Terminal;

/// Default byte budget (`4 MiB`) for a recording and per-subscriber byte queue.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Cap on retained events as well as bytes: a long interactive session can emit
/// millions of one-byte bursts within 4 MiB, which a payload-only budget (a byte
/// subscriber's) would keep.
const MAX_RETAINED_EVENTS: usize = 65_536;

/// One event's slot in a recording's event list ([`EventLog`]).
const EVENT_SLOT_BYTES: usize = std::mem::size_of::<Event>();

/// What a retained output burst of `len` bytes costs a recording's budget: its slot,
/// the payload, and the payload allocation's strong and weak counts.
const fn output_cost(len: usize) -> usize {
    EVENT_SLOT_BYTES + 2 * std::mem::size_of::<usize>() + len
}

/// Events per sealed chunk of a recording's event list ([`EventLog`]).
const CHUNK_EVENTS: usize = 256;

/// One recorded asciicast v2 event: program output (`"o"`) or a resize (`"r"`).
#[derive(Clone)]
enum Event {
    /// `[t, "o", "<json-escaped(bytes)>"]`: a coalesced output burst stored as raw
    /// bytes and escaped only at render time.
    Output {
        t: Duration,
        data: Arc<[u8]>,
        key: Option<OrderKey>,
    },
    /// `[t, "r", "<cols>x<rows>"]` — a geometry change the program observed.
    Resize {
        t: Duration,
        cols: u16,
        rows: u16,
        key: Option<OrderKey>,
    },
}

/// An event's place in the engine's order: the engine's lifetime token, then a rank on
/// its resize ordinal (`2k` for resize `k`, `2o + 1` for a burst processed with `o`
/// resizes applied), so a burst sorts after every resize it followed and before the
/// next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    lifetime: u64,
    rank: u64,
}

impl OrderKey {
    const fn output(lifetime: u64, ordinal: u64) -> Self {
        Self {
            lifetime,
            rank: ordinal.saturating_mul(2).saturating_add(1),
        }
    }

    const fn resize(lifetime: u64, ordinal: u64) -> Self {
        Self {
            lifetime,
            rank: ordinal.saturating_mul(2),
        }
    }
}

/// Engine-order position of a burst piece captured under `term_lock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CastStamp {
    lifetime: u64,
    ordinal: u64,
}

/// Engine resize cut points observed while processing a single output burst.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CastCuts {
    first: Option<CastStamp>,
    /// `(offset into the burst, stamp)` of each later piece, ascending; empty until a
    /// resize cuts the burst.
    rest: Vec<(usize, CastStamp)>,
    /// The count of the burst's last hold on the recorder's [`CastWatermark`].
    hold: u64,
}

impl CastCuts {
    /// Records the `term_lock` hold about to process burst bytes starting at `at`.
    #[inline]
    pub(crate) fn observe(&mut self, at: usize, term: &Terminal, watermark: &CastWatermark) {
        self.hold = watermark.note();
        let stamp = CastStamp {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        };
        let Some(first) = self.first.as_mut() else {
            self.first = Some(stamp);
            return;
        };
        match self.rest.last_mut() {
            Some((_, last)) if *last == stamp => {}
            Some((offset, last)) if *offset == at => *last = stamp,
            Some(_) => self.push_cut(at, stamp),
            None if *first == stamp => {}
            None if at == 0 => *first = stamp,
            None => self.push_cut(at, stamp),
        }
    }

    /// The rare path: a resize cut the burst at `at`.
    #[cold]
    fn push_cut(&mut self, at: usize, stamp: CastStamp) {
        self.rest.push((at, stamp));
    }

    /// Whether at least one `term_lock` hold processed this burst.
    pub(crate) const fn held(&self) -> bool {
        self.first.is_some()
    }

    /// How many pieces the burst is cut into (tests).
    #[cfg(test)]
    fn pieces(&self) -> usize {
        usize::from(self.first.is_some()) + self.rest.len()
    }
}

/// Shared reader hold counter used by `cast drift` to cut against [`CastRecorder`].
#[derive(Default)]
pub(crate) struct CastWatermark {
    holds: AtomicU64,
    caught_up: Condvar,
}

impl CastWatermark {
    /// Count one reader hold and return its count.
    fn note(&self) -> u64 {
        self.holds.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Engine state snapshot taken under `term_lock` for `cast drift` ([`CastCut::take`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CastCut {
    holds: u64,
    lifetime: u64,
    ordinal: u64,
}

impl CastCut {
    /// Captures the current cut of `term` (with `term_lock` held) against `watermark`.
    pub(crate) fn take(term: &Terminal, watermark: &CastWatermark) -> Self {
        Self {
            holds: watermark.holds.load(Ordering::Relaxed),
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        }
    }
}

/// How a recording stands against a [`CastCut`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holding {
    /// Something the engine had applied at the cut is not recorded yet.
    Behind,
    /// Exactly what the engine had applied at the cut.
    Exact,
    /// Something the engine applied after the cut is recorded.
    Past,
}

/// Engine-order stamp of a terminal resize captured under `term_lock`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResizeStamp {
    at: Instant,
    lifetime: u64,
    ordinal: u64,
}

impl ResizeStamp {
    /// Captures the resize stamp for `term` (with `term_lock` held).
    pub(crate) fn take(term: &Terminal) -> Self {
        Self::take_at(term, Instant::now())
    }

    /// [`take`](Self::take) at a given instant.
    const fn take_at(term: &Terminal, at: Instant) -> Self {
        Self {
            at,
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        }
    }
}

impl Event {
    /// The event's time since the recorder's epoch.
    const fn t(&self) -> Duration {
        match self {
            Event::Output { t, .. } | Event::Resize { t, .. } => *t,
        }
    }

    const fn set_t(&mut self, to: Duration) {
        match self {
            Event::Output { t, .. } | Event::Resize { t, .. } => *t = to,
        }
    }

    const fn key(&self) -> Option<OrderKey> {
        match self {
            Event::Output { key, .. } | Event::Resize { key, .. } => *key,
        }
    }

    /// The bytes charged against the budget: what holding the event costs, its slot,
    /// plus for output the payload and its allocation's counts ([`output_cost`]).
    fn cost(&self) -> usize {
        match self {
            Event::Output { data, .. } => output_cost(data.len()),
            Event::Resize { .. } => EVENT_SLOT_BYTES,
        }
    }
}

/// What an evicted slot of a sealed chunk holds once its payload is released: no
/// reader visits it (it is before [`EventLog::head`]).
const RELEASED: Event = Event::Resize {
    t: Duration::ZERO,
    cols: 0,
    rows: 0,
    key: None,
};

/// A recording's events, oldest first, kept so a snapshot shares most of them.
#[derive(Default)]
struct EventLog {
    /// Sealed chunks, oldest first.
    sealed: VecDeque<Arc<Vec<Event>>>,
    /// Events at the front of `sealed[0]` already evicted.
    head: usize,
    /// Of those, the leading ones whose payload is released.
    released: usize,
    /// The newest events.
    open: VecDeque<Event>,
    /// Live events: the sealed ones from `head` on, plus `open`.
    len: usize,
}

impl EventLog {
    const fn len(&self) -> usize {
        self.len
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Live events in sealed chunks.
    fn sealed_len(&self) -> usize {
        self.len - self.open.len()
    }

    /// The live events, oldest first.
    fn iter(&self) -> impl DoubleEndedIterator<Item = &Event> {
        live_sealed(self.sealed.iter(), self.head).chain(self.open.iter())
    }

    /// The live event at index `i` (0 the oldest).
    fn get(&self, i: usize) -> Option<&Event> {
        let sealed = self.sealed_len();
        if i >= sealed {
            return self.open.get(i - sealed);
        }
        let mut i = i + self.head;
        for chunk in &self.sealed {
            if i < chunk.len() {
                return chunk.get(i);
            }
            i -= chunk.len();
        }
        None
    }

    /// Insert `ev` so it becomes the live event at index `at` (`at <= len`).
    fn insert(&mut self, at: usize, ev: Event) {
        let unsealing = at < self.sealed_len();
        while at < self.sealed_len() {
            self.unseal_newest();
        }
        let i = at - self.sealed_len();
        self.open.insert(i, ev);
        self.len += 1;
        while self.open.len() >= 2 * CHUNK_EVENTS {
            let chunk: Vec<Event> = self.open.drain(..CHUNK_EVENTS).collect();
            self.sealed.push_back(Arc::new(chunk));
        }
        if unsealing {
            self.open.shrink_to(2 * CHUNK_EVENTS);
        }
    }

    /// Move the newest sealed chunk's live events back to the front of `open`: the
    /// chunk itself when no snapshot shares it, else a clone of its events.
    #[cold]
    fn unseal_newest(&mut self) {
        let Some(chunk) = self.sealed.pop_back() else {
            return;
        };
        let mut events = Arc::try_unwrap(chunk).unwrap_or_else(|shared| (*shared).clone());
        if self.sealed.is_empty() {
            events.drain(..self.head);
            (self.head, self.released) = (0, 0);
        }
        for ev in events.into_iter().rev() {
            self.open.push_front(ev);
        }
    }

    /// Evict the oldest live event and hand it back (its payload freed when the caller
    /// drops it, unless a snapshot still shares its chunk).
    fn pop_front(&mut self) -> Option<Event> {
        let Some(chunk) = self.sealed.front_mut() else {
            let ev = self.open.pop_front()?;
            self.len -= 1;
            return Some(ev);
        };
        let at = self.head;
        let ev = match Arc::get_mut(chunk) {
            Some(slots) => {
                slots[self.released..at].fill(RELEASED);
                self.released = at + 1;
                std::mem::replace(&mut slots[at], RELEASED)
            }
            None => chunk[at].clone(),
        };
        let spent = at + 1 == chunk.len();
        self.head += 1;
        if spent {
            self.sealed.pop_front();
            (self.head, self.released) = (0, 0);
        }
        self.len -= 1;
        Some(ev)
    }

    /// Move every live event `by` earlier, floored at zero (a warm spare's adoption,
    /// [`CastRecorder::adopt_spare`]).
    fn shift_back(&mut self, by: Duration) {
        let head = self.head;
        for (i, chunk) in self.sealed.iter_mut().enumerate() {
            let from = if i == 0 { head } else { 0 };
            for ev in &mut Arc::make_mut(chunk)[from..] {
                ev.set_t(ev.t().saturating_sub(by));
            }
        }
        for ev in &mut self.open {
            ev.set_t(ev.t().saturating_sub(by));
        }
    }

    /// The live events by refcount on the sealed chunks plus a clone of the open ones.
    fn share(&self) -> SharedEvents {
        SharedEvents {
            sealed: self.sealed.iter().cloned().collect(),
            head: self.head,
            open: self.open.iter().cloned().collect(),
        }
    }

    /// Event slots this list holds, live or evicted (tests).
    #[cfg(test)]
    fn slots(&self) -> usize {
        self.sealed.iter().map(|chunk| chunk.len()).sum::<usize>() + self.open.capacity()
    }
}

/// The live events of `sealed`, a run of chunks whose first has `head` events evicted.
fn live_sealed<'a>(
    sealed: impl DoubleEndedIterator<Item = &'a Arc<Vec<Event>>> + ExactSizeIterator,
    head: usize,
) -> impl DoubleEndedIterator<Item = &'a Event> {
    sealed
        .enumerate()
        .flat_map(move |(i, chunk)| chunk[if i == 0 { head } else { 0 }..].iter())
}

/// A recording's events as a snapshot holds them ([`EventLog::share`]): the sealed
/// chunks it shares with the recorder, and its own copy of the open events.
struct SharedEvents {
    sealed: Vec<Arc<Vec<Event>>>,
    head: usize,
    open: Vec<Event>,
}

impl SharedEvents {
    /// The events, oldest first.
    fn iter(&self) -> impl DoubleEndedIterator<Item = &Event> {
        live_sealed(self.sealed.iter(), self.head).chain(self.open.iter())
    }
}

/// Accumulates PTY output bursts and terminal resizes as asciicast v2 events.
pub(crate) struct CastRecorder {
    /// asciicast v2 header width (cols): the geometry in force at the first retained
    /// event, so a replay of the survivors starts at the size they were drawn at.
    width: u16,
    /// asciicast v2 header height (rows), kept with [`width`](Self::width).
    height: u16,
    /// Recorded events, oldest first; drop-oldest under [`budget`](Self::budget).
    events: EventLog,
    /// Sum of `Event::cost()` over `events` (kept in step with the list).
    used: usize,
    /// The byte budget over [`Event::cost`]; drop-oldest when `used` would exceed it.
    budget: usize,
    /// Drop-oldest evictions, disclosed in the asciicast header
    /// ([`CastSnapshot::into_cast`]) because a truncated recording's leading
    /// ANSI state is incomplete.
    evicted: u64,
    /// The last emitted timestamp; an unstamped event's `t` is clamped up to it.
    last_t: Duration,
    /// Monotonic epoch of this recorder's timeline, captured at construction.
    epoch: Instant,
    /// Trailing incomplete UTF-8 lead bytes (at most 3) carried from the previous burst
    /// and prepended to the next, so a character split across reads is reassembled
    /// instead of emitted as U+FFFD.
    pending: Vec<u8>,
    /// Shared with the session's readers and `cast drift` ([`CastWatermark`]).
    watermark: Arc<CastWatermark>,
    /// The hold count of the newest burst recorded.
    recorded: u64,
    /// The resizes recorded from the newest engine lifetime seen: its token, how many,
    /// and the newest ordinal.
    resizes: (u64, u64, u64),
    /// Callers waiting in [`snapshot_at`](Self::snapshot_at), so a record signals the
    /// condvar only when someone waits.
    waiters: usize,
    /// Set on an adopted session's recorder: the recording starts at a seamless update
    /// and lacks what the session showed before.
    handoff: Option<crate::session_timeline::HandoffGap>,
    /// How far an adopted warm spare's timeline was rebased
    /// ([`adopt_spare`](Self::adopt_spare)): subtracted from every event's time,
    /// so the recording starts at the adoption, the moment the person asked
    /// for the tab.
    shift: Duration,
}

impl CastRecorder {
    /// Creates a recorder for a `cols`×`rows` grid with [`DEFAULT_BUDGET_BYTES`].
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self::with_budget(cols, rows, DEFAULT_BUDGET_BYTES)
    }

    /// Creates a recorder with an explicit byte `budget`.
    pub(crate) fn with_budget(cols: u16, rows: u16, budget: usize) -> Self {
        Self {
            width: cols,
            height: rows,
            events: EventLog::default(),
            used: 0,
            budget: budget.max(1),
            evicted: 0,
            last_t: Duration::ZERO,
            epoch: Instant::now(),
            pending: Vec::new(),
            watermark: Arc::default(),
            recorded: 0,
            resizes: (0, 0, 0),
            waiters: 0,
            handoff: None,
            shift: Duration::ZERO,
        }
    }

    /// Rebases recorded startup events to `t = 0` when adopting a warm spare at `at`.
    pub(crate) fn adopt_spare(&mut self, at: Instant) {
        let shift = at.saturating_duration_since(self.epoch);
        self.events.shift_back(shift);
        self.last_t = self.last_t.saturating_sub(shift);
        self.shift = shift;
    }

    /// Marks this recording as starting at a seamless-update [`crate::session_timeline::HandoffGap`].
    pub(crate) const fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// Shared [`CastWatermark`] for reader holds and `cast drift` cuts.
    pub(crate) fn watermark(&self) -> Arc<CastWatermark> {
        Arc::clone(&self.watermark)
    }

    /// Elapsed duration since this recorder's epoch.
    pub(crate) fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// Monotonic epoch of this recorder's timeline.
    pub(crate) const fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Clamp `t` to be non-decreasing w.r.t. the last emitted timestamp.
    fn monotonic(&mut self, t: Duration) -> Duration {
        let t = t.max(self.last_t);
        self.last_t = t;
        t
    }

    /// Push `ev` at its place, evicting oldest events first so the list never exceeds
    /// [`MAX_RETAINED_EVENTS`] or its byte budget.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OutputRetention",
            action = "Push",
            project = "cast::tests::tiny_output_retention_conforms"
        )
    )]
    fn push(&mut self, mut ev: Event) {
        ev.set_t(ev.t().saturating_sub(self.shift));
        let cost = ev.cost();
        while !self.events.is_empty()
            && (self.used.saturating_add(cost) > self.budget
                || self.events.len() >= MAX_RETAINED_EVENTS)
        {
            if let Some(old) = self.events.pop_front() {
                self.used = self.used.saturating_sub(old.cost());
                self.evicted += 1;
                if let Event::Resize { cols, rows, .. } = old {
                    (self.width, self.height) = (cols, rows);
                }
            }
        }
        let at = match ev.key() {
            None => {
                let t = self.monotonic(ev.t());
                ev.set_t(t);
                self.events.len()
            }
            Some(key) => {
                let later = self
                    .events
                    .iter()
                    .rev()
                    .take_while(|before| before.key().is_some_and(|before| before > key))
                    .count();
                let at = self.events.len() - later;
                let lo = at
                    .checked_sub(1)
                    .and_then(|i| self.events.get(i))
                    .map_or(Duration::ZERO, Event::t);
                let mut t = ev.t().max(lo);
                if let Some(next) = self.events.get(at) {
                    t = t.min(next.t()).max(lo);
                }
                ev.set_t(t);
                self.last_t = self.last_t.max(t);
                at
            }
        };
        self.used += cost;
        self.events.insert(at, ev);
    }

    /// Records a coalesced output burst at relative timestamp `t`.
    #[cfg(test)]
    pub(crate) fn record_output(&mut self, t: Duration, bytes: &[u8]) {
        self.record_output_shared(t, Arc::from(bytes));
    }

    /// Records a shared `Arc<[u8]>` output burst at relative timestamp `t`.
    pub(crate) fn record_output_shared(&mut self, t: Duration, bytes: Arc<[u8]>) {
        self.record_output_keyed(t, None, bytes);
    }

    /// Records a reader burst split across engine-order resize `cuts`.
    pub(crate) fn record_burst(&mut self, bytes: Arc<[u8]>, cuts: CastCuts, t: Duration) {
        let CastCuts { first, rest, hold } = cuts;
        match first {
            None => {
                let t = self.now();
                self.record_output_shared(t, bytes);
            }
            Some(first) if rest.is_empty() => self.record_output_stamped(t, first, bytes),
            Some(first) => {
                let (mut start, mut stamp) = (0usize, first);
                for (at, next) in rest {
                    let at = at.clamp(start, bytes.len());
                    if at > start {
                        self.record_output_stamped(t, stamp, Arc::from(&bytes[start..at]));
                    }
                    (start, stamp) = (at, next);
                }
                if start < bytes.len() {
                    self.record_output_stamped(t, stamp, Arc::from(&bytes[start..]));
                }
            }
        }
        if hold > self.recorded {
            self.recorded = hold;
            self.signal();
        }
    }

    fn record_output_stamped(&mut self, t: Duration, stamp: CastStamp, bytes: Arc<[u8]>) {
        let key = OrderKey::output(stamp.lifetime, stamp.ordinal);
        self.record_output_keyed(t, Some(key), bytes);
    }

    fn record_output_keyed(&mut self, t: Duration, key: Option<OrderKey>, bytes: Arc<[u8]>) {
        let data: Arc<[u8]> = if self.pending.is_empty() {
            let split = bytes.len() - incomplete_tail_len(&bytes);
            self.pending.extend_from_slice(&bytes[split..]);
            match split {
                0 => return,
                n if n == bytes.len() => bytes,
                n => Arc::from(&bytes[..n]),
            }
        } else {
            let mut buf = std::mem::take(&mut self.pending);
            buf.extend_from_slice(&bytes);
            let split = buf.len() - incomplete_tail_len(&buf);
            self.pending = buf[split..].to_vec();
            buf.truncate(split);
            if buf.is_empty() {
                return;
            }
            Arc::from(buf)
        };
        self.push(Event::Output { t, data, key });
    }

    /// Records a terminal resize (`cols`×`rows`) at relative timestamp `t`.
    #[cfg(test)]
    pub(crate) fn record_resize(&mut self, t: Duration, cols: u16, rows: u16) {
        self.push(Event::Resize {
            t,
            cols,
            rows,
            key: None,
        });
    }

    /// Records a stamped terminal resize in engine order.
    pub(crate) fn record_resize_stamped(&mut self, stamp: ResizeStamp, cols: u16, rows: u16) {
        self.push(Event::Resize {
            t: stamp.at.saturating_duration_since(self.epoch),
            cols,
            rows,
            key: Some(OrderKey::resize(stamp.lifetime, stamp.ordinal)),
        });
        let resizes = &mut self.resizes;
        match stamp.lifetime.cmp(&resizes.0) {
            std::cmp::Ordering::Greater => *resizes = (stamp.lifetime, 1, stamp.ordinal),
            std::cmp::Ordering::Equal => {
                resizes.1 += 1;
                resizes.2 = resizes.2.max(stamp.ordinal);
            }
            std::cmp::Ordering::Less => {}
        }
        self.signal();
    }

    /// Number of callers currently waiting in [`Self::snapshot_at`].
    #[cfg(test)]
    pub(crate) fn waiters(&self) -> usize {
        self.waiters
    }

    /// Wake the callers waiting for this recording to catch up with a cut, if any.
    fn signal(&self) {
        if self.waiters > 0 {
            self.watermark.caught_up.notify_all();
        }
    }

    /// How this recording stands against `cut`: bursts by the hold count of the
    /// newest recorded one, resizes by how many of the cut's engine lifetime are in.
    fn holding(&self, cut: &CastCut) -> Holding {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let bursts = self.recorded.cmp(&cut.holds);
        let (lifetime, count, newest) = self.resizes;
        let resizes = match lifetime.cmp(&cut.lifetime) {
            Equal if newest > cut.ordinal => Greater,
            Equal => count.cmp(&cut.ordinal),
            Greater => Greater,
            Less => 0.cmp(&cut.ordinal),
        };
        match (bursts, resizes) {
            (Greater, _) | (_, Greater) => Holding::Past,
            (Equal, Equal) => Holding::Exact,
            (Less, _) | (_, Less) => Holding::Behind,
        }
    }

    /// Waits up to `bound` for the recording to reach `cut` and returns its snapshot.
    pub(crate) fn snapshot_at(
        cast: &Mutex<Self>,
        cut: &CastCut,
        bound: Duration,
    ) -> (CastSnapshot, bool) {
        let mut rec = cast.lock().unwrap_or_else(PoisonError::into_inner);
        if rec.holding(cut) == Holding::Behind {
            #[cfg(test)]
            cut_probe::waited();
            let watermark = Arc::clone(&rec.watermark);
            rec.waiters += 1;
            rec = watermark
                .caught_up
                .wait_timeout_while(rec, bound, |rec| rec.holding(cut) == Holding::Behind)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            rec.waiters -= 1;
        }
        let exact = rec.holding(cut) == Holding::Exact;
        (rec.snapshot(), exact)
    }

    /// Total number of retained events (output and resize).
    #[cfg(test)]
    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Number of oldest events evicted under budget pressure.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Takes an off-lock [`CastSnapshot`] of the retained events and header geometry.
    #[must_use]
    pub(crate) fn snapshot(&self) -> CastSnapshot {
        CastSnapshot {
            width: self.width,
            height: self.height,
            events: self.events.share(),
            evicted: self.evicted,
            handoff: self.handoff,
        }
    }

    /// Serializes the current recording as an asciicast v2 string.
    #[cfg(test)]
    pub(crate) fn to_asciicast(&self) -> String {
        let (header, events) = self.snapshot().into_cast();
        let mut out = String::new();
        aterm_control::asciicast::write_into(&mut out, &header, &events);
        out
    }
}

/// Off-lock snapshot of a [`CastRecorder`]'s events and header geometry.
pub(crate) struct CastSnapshot {
    /// asciicast v2 header width (cols): the geometry in force at the first retained
    /// event ([`CastRecorder::width`]).
    width: u16,
    /// asciicast v2 header height (rows), kept with [`width`](Self::width).
    height: u16,
    /// Recorded events, oldest first, sharing the recorder's sealed chunks.
    events: SharedEvents,
    /// Head events evicted before this snapshot, so the `cast frames` flipbook discloses
    /// truncation as the asciicast header's `aterm_truncated` does.
    evicted: u64,
    /// The recorder's handoff gap, when it starts at a seamless update.
    handoff: Option<crate::session_timeline::HandoffGap>,
}

impl CastSnapshot {
    /// Number of leading events evicted before this snapshot.
    #[must_use]
    pub(crate) const fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Seamless-update handoff gap at the start of this recording, if any.
    #[must_use]
    pub(crate) const fn handoff(&self) -> Option<crate::session_timeline::HandoffGap> {
        self.handoff
    }

    /// Converts this snapshot into asciicast header and event values.
    #[must_use]
    pub(crate) fn into_cast(self) -> (CastHeader, Vec<CastEvent<Arc<[u8]>>>) {
        #[cfg(test)]
        serialize_hook::before_serialize();
        let rebase = if self.evicted > 0 {
            self.events.iter().next().map_or(Duration::ZERO, Event::t)
        } else {
            Duration::ZERO
        };
        let header = CastHeader {
            width: self.width,
            height: self.height,
            evicted: self.evicted,
            dropped: Some(aterm_control::asciicast::CastDrops::default()),
            resize_undo: Some(true),
            resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
            handoff: self.handoff.map(|gap| HandoffHeader {
                from_build: gap.from_build,
            }),
        };
        let events = self
            .events
            .iter()
            .map(|ev| match ev {
                Event::Output { t, data, .. } => CastEvent::Output {
                    t: t.saturating_sub(rebase).as_secs_f64(),
                    data: Arc::clone(data),
                },
                Event::Resize { t, cols, rows, .. } => CastEvent::Resize {
                    t: t.saturating_sub(rebase).as_secs_f64(),
                    cols: *cols,
                    rows: *rows,
                },
            })
            .collect();
        (header, events)
    }
}

impl CastSnapshot {
    /// Replays this snapshot into `count` evenly spaced keyframe screens (`cast frames`).
    #[must_use]
    pub(crate) fn fold_frames(
        &self,
        count: usize,
        policy: ResizePolicy,
    ) -> Vec<(Duration, Vec<String>)> {
        let count = count.clamp(1, 240);
        let (Some(first), Some(last)) = (self.events.iter().next(), self.events.iter().next_back())
        else {
            return Vec::new();
        };
        let base = first.t();
        let span = last.t().saturating_sub(base);
        let mut replay = ReplayEngine::new(self.height, self.width, policy);
        let mut events = self.events.iter().peekable();
        let mut frames = Vec::with_capacity(count);
        for k in 1..=count {
            let target = base + span * (k as u32) / (count as u32);
            while let Some(ev) = events.next_if(|ev| ev.t() <= target) {
                match ev {
                    Event::Output { data, .. } => replay.process(data),
                    Event::Resize { cols, rows, .. } => replay.resize(*rows, *cols),
                }
            }
            let term = replay.terminal();
            let rows = term.rows() as usize;
            let mut screen = Vec::with_capacity(rows);
            for r in 0..rows {
                screen.push(crate::control::visible_row(term, r));
            }
            frames.push((target.saturating_sub(base), screen));
        }
        frames
    }
}

/// Number of trailing bytes of `bytes` that form an incomplete but so-far valid UTF-8
/// lead the next read could complete.
pub(crate) fn incomplete_tail_len(bytes: &[u8]) -> usize {
    let n = bytes.len();
    let max_back = 3.min(n);
    for back in 1..=max_back {
        let b = bytes[n - back];
        let needed = if b >> 5 == 0b110 {
            2
        } else if b >> 4 == 0b1110 {
            3
        } else if b >> 3 == 0b11110 {
            4
        } else if b >> 6 == 0b10 {
            continue;
        } else {
            return 0;
        };
        return if back < needed { back } else { 0 };
    }
    0
}

/// Test-only hook [`CastSnapshot::into_cast`] calls first, so a test can probe the
/// recorder lock from inside the off-lock path `cast` serializes on.
#[cfg(test)]
#[path = "cast_serialize_hook_tests.rs"]
mod serialize_hook;

/// Test-only counter for [`CastRecorder::snapshot_at`] waits on the current thread.
#[cfg(test)]
#[path = "cast_cut_probe_tests.rs"]
pub(crate) mod cut_probe;

/// Test-only helpers that drive `Terminal` and [`CastRecorder`] in lockstep.
#[cfg(test)]
#[path = "cast_rig_tests.rs"]
pub(crate) mod rig;

/// Single output burst queued for a live `bytes` subscriber.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QueuedBurst {
    /// Shared output byte slice.
    pub(crate) bytes: Arc<[u8]>,
    /// Reader timestamp in microseconds (`metrics::now_us`).
    pub(crate) at_us: u64,
}

/// One subscriber's byte- and entry-bounded, drop-oldest queue of output bursts.
#[derive(Default)]
struct ByteQueue {
    /// Bursts in arrival order.
    bursts: VecDeque<QueuedBurst>,
    /// Sum of `bursts` byte lengths, kept in step for the budget check.
    used: usize,
    /// Bytes dropped (oldest first) since the last `drain`, reported as a gap.
    dropped: u64,
}

/// One registered byte subscriber: a stable id + its queue.
struct ByteSlot {
    id: u64,
    queue: Mutex<ByteQueue>,
}

/// Per-session live byte fan-out backing `subscribe … bytes`.
pub(crate) struct ByteFanout {
    slots: Mutex<Vec<Arc<ByteSlot>>>,
    next_id: AtomicU64,
    /// Lock-free mirror of `slots.len()`, incremented before a slot is pushed and
    /// decremented after it is removed, so it is nonzero whenever a registered slot
    /// exists (a positive count with no slot yet is a harmless no-op pass).
    live: AtomicUsize,
    /// Per-subscriber retained-byte budget; drop-oldest beyond it.
    budget: usize,
}

impl Default for ByteFanout {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }
}

impl ByteFanout {
    /// Creates a fan-out with [`DEFAULT_BUDGET_BYTES`] per subscriber.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Creates a fan-out with an explicit per-subscriber byte `budget`.
    #[must_use]
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            live: AtomicUsize::new(0),
            budget: budget.max(1),
        }
    }

    /// Pushes `burst` at `at_us` to all active subscribers.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OutputRetention",
            action = "Push",
            project = "cast::tests::tiny_output_retention_conforms"
        )
    )]
    pub(crate) fn tee(&self, burst: &Arc<[u8]>, at_us: u64) {
        if burst.is_empty() || self.live.load(Ordering::Acquire) == 0 {
            return;
        }
        let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        for slot in slots.iter() {
            let mut q = slot.queue.lock().unwrap_or_else(|p| p.into_inner());
            while !q.bursts.is_empty()
                && (q.used.saturating_add(burst.len()) > self.budget
                    || q.bursts.len() >= MAX_RETAINED_EVENTS)
            {
                if let Some(old) = q.bursts.pop_front() {
                    q.used = q.used.saturating_sub(old.bytes.len());
                    q.dropped += old.bytes.len() as u64;
                }
            }
            q.bursts.push_back(QueuedBurst {
                bytes: burst.clone(),
                at_us,
            });
            q.used += burst.len();
        }
    }

    /// Registers a new live subscriber and returns its RAII [`ByteSubscription`].
    #[must_use]
    pub(crate) fn subscribe(self: &Arc<Self>) -> ByteSubscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(ByteSlot {
            id,
            queue: Mutex::new(ByteQueue::default()),
        });
        self.live.fetch_add(1, Ordering::Release);
        self.slots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(slot.clone());
        ByteSubscription {
            fanout: self.clone(),
            slot,
            id,
        }
    }

    fn deregister(&self, id: u64) {
        self.slots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|s| s.id != id);
        self.live.fetch_sub(1, Ordering::Release);
    }

    /// Number of active subscribers.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// Consumer handle for a live byte subscription; deregisters on drop.
pub(crate) struct ByteSubscription {
    fanout: Arc<ByteFanout>,
    slot: Arc<ByteSlot>,
    id: u64,
}

impl ByteSubscription {
    /// Drains all queued bursts and the dropped-byte count since the last drain.
    #[must_use]
    pub(crate) fn drain(&self) -> (Vec<QueuedBurst>, u64) {
        let mut q = self.slot.queue.lock().unwrap_or_else(|p| p.into_inner());
        let bursts: Vec<QueuedBurst> = q.bursts.drain(..).collect();
        q.used = 0;
        let dropped = std::mem::take(&mut q.dropped);
        (bursts, dropped)
    }
}

impl Drop for ByteSubscription {
    fn drop(&mut self) {
        self.fanout.deregister(self.id);
    }
}

#[cfg(test)]
#[path = "cast_tests.rs"]
mod tests;
