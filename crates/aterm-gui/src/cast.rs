// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session asciicast v2 recording of the child's program output.
//!
//! [`CastRecorder`] accumulates coalesced PTY-output bursts as asciicast v2 events (a
//! JSON header line, then `[t, "o", "<data>"]` per burst and `[t, "r", "<cols>x<rows>"]`
//! per resize). A snapshot of them is taken under the recorder lock and, after it is
//! released, lifted into the asciicast codec's values ([`CastSnapshot::into_cast`],
//! `aterm_control::asciicast`), which `cast` writes as text `asciinema play`/`agg`
//! accept and `cast drift` audits.
//!
//! 1. Every chunk is JSON-escaped and UTF-8-lossy decoded; PTY output is binary and
//!    splits multibyte sequences across reads, and raw bytes would invalidate the JSON.
//! 2. Output only: the recorder is fed `buf[..r]` at the reader-thread tap. The
//!    terminal's own query replies (`take_response()`) must never appear as `"o"`
//!    events; keeping them out is the caller's contract.
//! 3. Timestamps are non-decreasing from one epoch captured at construction, in the
//!    order events are written (the engine's, below).
//! 4. Bounded: byte and entry budgets with drop-oldest. The byte budget charges what
//!    holding each event costs, its slot included ([`Event::cost`]).
//!
//! The recorder does no fs/socket work and takes no other lock. The PTY reader records
//! each burst itself, after the `term_lock` holds that processed it. The recorder's lock
//! is never taken under `term_lock`, and every hold of it is bounded (a push, or a
//! snapshot's chunk refcounts, [`CastRecorder::snapshot`]), so the reader waits on no
//! long hold and no burst is dropped.
//!
//! ## The engine's order, not arrival order
//! A burst is recorded by the reader and a resize by the thread that resized
//! (the main thread's window pass, a control worker's `resize`), each after its
//! `term_lock` hold. Stamped on arrival, a resize could land on the wrong side of a
//! burst the engine processed first, and a replay (`cast frames`, `cast drift`) would
//! re-lay the alternate screen at the wrong moment. So each burst and resize carries
//! the engine's resize ordinal (`Terminal::resize_ordinal`, under its lifetime token),
//! read inside the hold that applied it ([`CastCuts`], [`ResizeStamp`]), plus a time on
//! this recorder's timeline (a resize's read in its hold; a burst's at arrival, before
//! the reader's first hold, so no clock is read under the lock). The recorder inserts
//! by that order: a burst processed after resize `k` sorts after it and before resize
//! `k+1`. A burst whose slices straddle a resize is cut there. Unstamped calls
//! (tests) append.
//!
//! ## Holding the screen `cast drift` read
//! Each reader hold that processes a burst counts itself on the recorder's
//! [`CastWatermark`], and the burst brings its last hold's count to the recorder. A
//! [`CastCut`] read in the hold that reads the screen (that count, and the engine's
//! resize ordinal) names exactly the bursts and resizes the screen shows, so
//! [`CastRecorder::snapshot_at`] waits until the recording holds them instead of
//! sleeping and hoping it does.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_control::asciicast::{CastEvent, CastHeader, HandoffHeader};
use aterm_control::replay::ReplayEngine;
use aterm_core::grid::{ResizeLaw, ResizePolicy};
use aterm_core::terminal::Terminal;

/// Default byte budget of a recording and of each byte subscriber's queue: a flood
/// cannot balloon RAM past it, and an idle terminal costs nothing (no events ⇒ no
/// bytes). A recording charges each event what holding it costs ([`Event::cost`]); a
/// subscriber's queue charges payloads.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Cap on retained events as well as bytes: a long interactive session can emit
/// millions of one-byte bursts within 4 MiB, which a payload-only budget (a byte
/// subscriber's) would keep. Evictions are disclosed as for bytes. A recording at the
/// default budget meets its charged budget first; the cap bounds a larger budget's
/// event list, so a snapshot's chunk count stays bounded ([`EventLog`]).
const MAX_RETAINED_EVENTS: usize = 65_536;

/// One event's slot in a recording's event list ([`EventLog`]).
const EVENT_SLOT_BYTES: usize = std::mem::size_of::<Event>();

/// What a retained output burst of `len` bytes costs a recording's budget: its slot,
/// the payload, and the payload allocation's strong and weak counts. The reader's
/// allocation may also be held by the `bytes` taps and the temporal spine; each holder
/// charges its own share.
const fn output_cost(len: usize) -> usize {
    EVENT_SLOT_BYTES + 2 * std::mem::size_of::<usize>() + len
}

/// Events per sealed chunk of a recording's event list ([`EventLog`]). A snapshot
/// takes one refcount per sealed chunk and clones fewer than two chunks of open
/// events: with [`MAX_RETAINED_EVENTS`], at most 256 refcounts and 511 clones.
const CHUNK_EVENTS: usize = 256;

/// One recorded asciicast v2 event: program output (`"o"`) or a resize (`"r"`).
/// Cloning is an `Arc` bump for `Output`, so [`CastRecorder::snapshot`] lifts the list
/// out of the recorder lock cheaply.
#[derive(Clone)]
enum Event {
    /// `[t, "o", "<json-escaped(bytes)>"]`: a coalesced output burst stored as raw
    /// bytes and escaped only at render time. `Arc<[u8]>` so a complete burst keeps the
    /// reader thread's allocation (see [`CastRecorder::record_output_shared`]).
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
/// next. Same-rank bursts keep arrival order. Lifetime tokens only grow, so a replaced
/// engine's events sort after the old one's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    lifetime: u64,
    rank: u64,
}

impl OrderKey {
    fn output(lifetime: u64, ordinal: u64) -> Self {
        Self {
            lifetime,
            rank: ordinal.saturating_mul(2).saturating_add(1),
        }
    }

    fn resize(lifetime: u64, ordinal: u64) -> Self {
        Self {
            lifetime,
            rank: ordinal.saturating_mul(2),
        }
    }
}

/// One piece's place in the engine's order, read inside the `term_lock` hold that
/// processed its first byte: the engine's resize lifetime and ordinal. No clock is
/// read there; the burst carries one time for all pieces ([`CastRecorder::record_burst`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CastStamp {
    lifetime: u64,
    ordinal: u64,
}

/// Where the engine's resize ordinal moved while one burst was processed (a resize can
/// take `term_lock` between the reader's slices), so the recorder can cut the burst
/// there. [`observe`](Self::observe) runs in every hold that processes the burst; the
/// common case keeps one stamp and allocates nothing.
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
    /// Note the hold about to process the burst's bytes from offset `at` (in the burst
    /// as the recorder receives it: a manual reset's bytes first, a foreground
    /// handback's where the reader splices them), and count it on `watermark`. The
    /// caller holds `term`'s lock. No clock read; allocates only when a resize cuts
    /// the burst.
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
            // A hold that processed none of the burst's bytes (an empty manual
            // reset) is superseded by the next one at the same offset.
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

    /// Whether a hold noted the burst: it processed bytes, or ran a manual reset.
    pub(crate) fn held(&self) -> bool {
        self.first.is_some()
    }

    /// How many pieces the burst is cut into (tests).
    #[cfg(test)]
    fn pieces(&self) -> usize {
        usize::from(self.first.is_some()) + self.rest.len()
    }
}

/// The reader's side of the cut `cast drift` takes ([`CastCut`]): a count of the
/// reader's `term_lock` holds that process output, bumped inside each
/// ([`CastCuts::observe`]), and the condvar the recorder's lock signals when a record
/// may have caught up with a cut someone waits on ([`CastRecorder::snapshot_at`]).
/// One per recorder, shared with each reader its session attaches
/// ([`CastRecorder::watermark`]).
#[derive(Default)]
pub(crate) struct CastWatermark {
    holds: AtomicU64,
    caught_up: Condvar,
}

impl CastWatermark {
    /// Count one reader hold and return its count. The caller holds the engine's lock,
    /// which orders this against a [`CastCut::take`], so `Relaxed` suffices.
    fn note(&self) -> u64 {
        self.holds.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// What the engine had applied when `cast drift` read its screen, taken in the same
/// `term_lock` hold ([`take`](Self::take)): the reader holds counted so far, and the
/// engine's resize lifetime and ordinal. A recording holds that screen exactly when it
/// holds every burst those holds processed and every resize up to that ordinal, and
/// nothing later ([`CastRecorder::snapshot_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CastCut {
    holds: u64,
    lifetime: u64,
    ordinal: u64,
}

impl CastCut {
    /// The cut of `term`, whose lock the caller holds, against `watermark`.
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

/// One resize's place in the engine's order, taken inside the `term_lock` hold that
/// applied it ([`ResizeStamp::take`]): the instant (converted onto the recorder's
/// timeline when recorded), the engine's lifetime and the resize's ordinal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResizeStamp {
    at: Instant,
    lifetime: u64,
    ordinal: u64,
}

impl ResizeStamp {
    /// Stamp the resize `term` just applied (the caller holds its lock).
    pub(crate) fn take(term: &Terminal) -> Self {
        Self::take_at(term, Instant::now())
    }

    /// [`take`](Self::take) at a given instant.
    fn take_at(term: &Terminal, at: Instant) -> Self {
        Self {
            at,
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        }
    }
}

impl Event {
    /// The event's time since the recorder's epoch.
    fn t(&self) -> Duration {
        match self {
            Event::Output { t, .. } | Event::Resize { t, .. } => *t,
        }
    }

    fn set_t(&mut self, to: Duration) {
        match self {
            Event::Output { t, .. } | Event::Resize { t, .. } => *t = to,
        }
    }

    fn key(&self) -> Option<OrderKey> {
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

/// A recording's events, oldest first, kept so a snapshot shares most of them. The
/// older ones sit in sealed chunks of [`CHUNK_EVENTS`], immutable once sealed, which a
/// snapshot takes by refcount; the newest sit in an open deque, where engine-order
/// insertion lands, which a snapshot clones. The open deque seals its oldest chunk once
/// it holds two, so an insertion's walk back (as long as the race it repairs) stays in
/// it; one that reaches further unseals chunks back into it, which costs O(events
/// walked) as the walk does, and shrinks the deque back to two chunks once they are
/// resealed. The list holds at most `len + 3 * CHUNK_EVENTS` event slots: fewer than
/// a chunk evicted from the oldest, plus the deque's two chunks.
///
/// Eviction moves a head index through the oldest chunk, releasing each evicted slot's
/// payload when no snapshot shares the chunk; a slot a snapshot pinned is released at
/// the next eviction that finds the chunk unshared, and the chunk is freed once its last
/// event is evicted.
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
    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
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
            // The unsealed run grew the deque; resealed, it gives those slots back,
            // or they would outlive it uncharged.
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
                // Unshared: release this slot, and any a snapshot pinned earlier
                // (`released <= head`).
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
    /// [`CastRecorder::adopt_spare`]). A sealed chunk a snapshot shares is copied
    /// first, so the snapshot keeps the times it read.
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

/// Accumulates program-output bursts and resizes as asciicast v2 events.
/// Constructed with the child's initial grid size; fed by
/// [`record_burst`](Self::record_burst) and
/// [`record_resize_stamped`](Self::record_resize_stamped); the events are lifted
/// out with [`snapshot`](Self::snapshot) and handed to the codec with
/// [`CastSnapshot::into_cast`].
pub(crate) struct CastRecorder {
    /// asciicast v2 header width (cols): the geometry in force at the first retained
    /// event, so a replay of the survivors starts at the size they were drawn at.
    /// The construction width until a drop-oldest eviction removes a resize, then
    /// that resize's ([`push`](Self::push)).
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
    /// Monotonic epoch of this recorder's timeline, captured at construction. Reader
    /// bursts are stamped against it ([`epoch`](Self::epoch)) and resize instants are
    /// converted onto it ([`record_resize_stamped`](Self::record_resize_stamped)), so
    /// both taps share one timeline per session.
    epoch: Instant,
    /// Trailing incomplete UTF-8 lead bytes (at most 3) carried from the previous burst
    /// and prepended to the next, so a character split across reads is reassembled
    /// instead of emitted as U+FFFD.
    pending: Vec<u8>,
    /// Shared with the session's readers and `cast drift` ([`CastWatermark`]).
    watermark: Arc<CastWatermark>,
    /// The hold count of the newest burst recorded. Bursts arrive in hold order: one
    /// reader at a time, each recording after its holds.
    recorded: u64,
    /// The resizes recorded from the newest engine lifetime seen: its token, how many,
    /// and the newest ordinal. An engine numbers its resizes 1, 2, … and each is
    /// recorded once, so `count == newest == k` says exactly resizes 1..=k are in.
    resizes: (u64, u64, u64),
    /// Callers waiting in [`snapshot_at`](Self::snapshot_at), so a record signals the
    /// condvar only when someone waits.
    waiters: usize,
    /// Set on an adopted session's recorder: the recording starts at a seamless update
    /// and lacks what the session showed before. Disclosed as `aterm_handoff` in the
    /// header ([`CastSnapshot::into_cast`]) and by `cast frames`.
    handoff: Option<crate::session_timeline::HandoffGap>,
    /// How far an adopted warm spare's timeline was rebased
    /// ([`adopt_spare`](Self::adopt_spare)): subtracted from every event's time,
    /// so the recording starts at the adoption, the moment the person asked
    /// for the tab. Zero for every other session.
    shift: Duration,
}

impl CastRecorder {
    /// A recorder for a `cols`×`rows` grid with the default 4 MiB budget.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self::with_budget(cols, rows, DEFAULT_BUDGET_BYTES)
    }

    /// A recorder with an explicit byte budget over [`Event::cost`] (≥ 1).
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

    /// A warm spare was adopted at `at` (`crate::spare`): what it recorded
    /// before (its startup output) moves to t = 0, and every later event is
    /// offset by the spare's age, so the recording reads as a tab started now.
    /// Order is kept, so `cast drift` replays exactly.
    pub(crate) fn adopt_spare(&mut self, at: Instant) {
        let shift = at.saturating_duration_since(self.epoch);
        self.events.shift_back(shift);
        self.last_t = self.last_t.saturating_sub(shift);
        self.shift = shift;
    }

    /// Mark this recording as one that starts at a seamless update
    /// ([`crate::session_timeline::HandoffGap`]).
    pub(crate) fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// The watermark the reader counts its holds on and `cast drift` cuts against,
    /// taken once per attach so a hold never takes the recorder lock.
    pub(crate) fn watermark(&self) -> Arc<CastWatermark> {
        Arc::clone(&self.watermark)
    }

    /// Elapsed time since this recorder's epoch, for callers with no stamp (tests,
    /// unstamped bursts).
    pub(crate) fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// This recorder's epoch, copied by the reader once per attach to place bursts on
    /// this timeline ([`record_burst`](Self::record_burst)) without the lock.
    pub(crate) fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Clamp `t` to be non-decreasing w.r.t. the last emitted timestamp.
    fn monotonic(&mut self, t: Duration) -> Duration {
        let t = t.max(self.last_t);
        self.last_t = t;
        t
    }

    /// Push `ev` at its place, evicting oldest events first so the list never exceeds
    /// [`MAX_RETAINED_EVENTS`] or its byte budget. An unstamped event goes last (`t`
    /// clamped non-decreasing). A stamped one goes before every trailing stamped event
    /// the engine applied after it ([`OrderKey`]); the walk back is as long as the race
    /// it repairs. Its `t` is clamped between its neighbours'.
    ///
    /// The pushed event is never dropped, so the most recent activity survives. Every
    /// evicted head event is counted (`evicted`) for [`CastSnapshot::into_cast`]
    /// to disclose. An evicted resize hands its geometry to the header: it was the last
    /// change before the new head, so its size is the one the survivors were drawn at.
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

    /// Record a coalesced output burst at relative time `t`. `bytes` is program output,
    /// never the terminal's own query replies (`take_response()`). Borrowing wrapper
    /// over [`record_output_shared`](Self::record_output_shared).
    #[cfg(test)]
    pub(crate) fn record_output(&mut self, t: Duration, bytes: &[u8]) {
        self.record_output_shared(t, Arc::from(bytes));
    }

    /// Record an output burst already in the reader's shared `Arc<[u8]>`. With no carried
    /// lead and a complete final character the Arc is retained directly (zero copy);
    /// only UTF-8 reassembly builds a fresh buffer. Output is byte-identical either way.
    pub(crate) fn record_output_shared(&mut self, t: Duration, bytes: Arc<[u8]>) {
        self.record_output_keyed(t, None, bytes);
    }

    /// Record one reader burst at its place in engine order: `bytes` as the engine
    /// processed them, one event per piece its [`CastCuts`] name (a one-piece burst
    /// keeps the reader's allocation; a cut burst copies its pieces), at `t` since
    /// [`epoch`](Self::epoch): the arrival, read before the reader's first `term_lock`
    /// hold (for a manual reset's empty batch, after its last). All pieces share that
    /// time; next to a resize the recorder clamps it against the resize's, keeping the
    /// timeline non-decreasing in engine order. A burst with no stamp is recorded now,
    /// last. The burst's hold count reaches the watermark even when it pushes no event
    /// (a manual reset with no bytes).
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
        // Peel off a new incomplete trailing lead to carry forward; the rest is complete
        // program output. An empty or all-lead burst pushes no event.
        let data: Arc<[u8]> = if self.pending.is_empty() {
            // Nothing carried: the complete prefix is the burst's own, and a
            // burst ending on a complete char keeps the reader's allocation.
            let split = bytes.len() - incomplete_tail_len(&bytes);
            self.pending.extend_from_slice(&bytes[split..]);
            match split {
                0 => return,
                n if n == bytes.len() => bytes,
                n => Arc::from(&bytes[..n]),
            }
        } else {
            // Reassemble across reads: prepend the lead carried from the
            // previous burst, then peel.
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

    /// Record a geometry change (`[t, "r", "<cols>x<rows>"]`) at relative `t`,
    /// last (unstamped: tests).
    #[cfg(test)]
    pub(crate) fn record_resize(&mut self, t: Duration, cols: u16, rows: u16) {
        self.push(Event::Resize {
            t,
            cols,
            rows,
            key: None,
        });
    }

    /// Record a geometry change at its place in engine order: `stamp` was taken inside
    /// the hold that applied it ([`ResizeStamp::take`]), so bursts sort around it by
    /// what the engine processed, whichever thread reached this lock first.
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

    /// Callers waiting in [`snapshot_at`](Self::snapshot_at) (tests).
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

    /// The recording at `cut`: wait up to `bound` for it to hold what the engine had
    /// applied there (no wait when it does, or once it holds something later), then
    /// snapshot it. `true` when it holds exactly that: every burst the reader's holds
    /// up to the cut processed and every resize up to its ordinal, nothing after. The
    /// reader records a burst right after its last hold, and a resize pass records its
    /// resize right after its hold, so a wait is short; `bound` caps one stalled in
    /// between (a cross-session resize records after its rewrap).
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

    /// Number of recorded events (output + resize), for tests.
    #[cfg(test)]
    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Drop-oldest evictions so far.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Lift the event list and header geometry out of the recorder lock so the
    /// O(events) work on them (the VTE fold in [`CastSnapshot::fold_frames`], the codec
    /// values in [`CastSnapshot::into_cast`], and the `cast` text and `cast drift` folds
    /// over those) runs with the lock released and never stalls the reader, which
    /// records into it. The lift itself is bounded, not O(events): one refcount per
    /// sealed chunk and a clone of the open events ([`EventLog::share`]), no byte
    /// copies. `pending` stays behind: a trailing incomplete lead is not yet output.
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

    /// The recording as the `cast` verb's body: the codec's text for the
    /// [`CastSnapshot::into_cast`] of a [`snapshot`](Self::snapshot).
    #[cfg(test)]
    pub(crate) fn to_asciicast(&self) -> String {
        let (header, events) = self.snapshot().into_cast();
        let mut out = String::new();
        aterm_control::asciicast::write_into(&mut out, &header, &events);
        out
    }
}

/// An off-lock snapshot of a [`CastRecorder`]'s events and header geometry
/// ([`CastRecorder::snapshot`]), so the O(events) flipbook fold and the codec values
/// `cast` and `cast drift` read ([`into_cast`](Self::into_cast)) are built outside the
/// recorder lock.
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
    /// Head events evicted before this snapshot. `> 0` means the leading engine state
    /// is incomplete and early frames may render wrong.
    #[must_use]
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// The handoff gap this recording starts at, if it does
    /// ([`CastRecorder::mark_handoff`]).
    #[must_use]
    pub(crate) fn handoff(&self) -> Option<crate::session_timeline::HandoffGap> {
        self.handoff
    }

    /// The recording as the asciicast codec models it, for `cast` to write and `cast
    /// drift` to audit: the header facts, and every event with its time on the
    /// timeline `cast` prints. That timeline is rebased here, once, to the first
    /// retained event after a drop-oldest truncation (else a player idles through the
    /// first survivor's absolute offset) and is t0 for an untruncated recording. Each
    /// burst is a refcount on its `Arc`. Runs off the recorder lock, on the snapshot.
    ///
    /// The header says the engine keeps the resize undo and lays its rows out under the
    /// current resize law (`ResizeLaw::header_value`), and that this recorder drops no
    /// burst: its reader records every one itself, so the count is always none.
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
    /// Expand the recording into `count` evenly spaced keyframe screens (the readable
    /// "video" flipbook; `cast` is the compact form). Folds output and resizes into one
    /// [`ReplayEngine`] of the session's seam `policy` once, in order, rendering its
    /// visible rows at each frame boundary: O(events + count×rows). Frame instants are
    /// rebased to the first retained event, so an evicted head spreads frames over the
    /// surviving span. Returns `(rebased_elapsed, rows)` oldest first; empty for an
    /// empty recording.
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
        // `events` never rewinds and targets never decrease: each event is processed once.
        let mut events = self.events.iter().peekable();
        let mut frames = Vec::with_capacity(count);
        for k in 1..=count {
            // Frame k's absolute instant; the last frame lands on the final state.
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
/// lead the next read could complete. 0 when the buffer ends on a complete character
/// or an invalid byte (rendered as U+FFFD in place, not carried).
fn incomplete_tail_len(bytes: &[u8]) -> usize {
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
            // A continuation byte; the lead is further back — keep scanning.
            continue;
        } else {
            // ASCII (complete) or an invalid lead — nothing to carry.
            return 0;
        };
        // `back` continuation+lead bytes seen; if fewer than the sequence needs,
        // the tail is incomplete and must be carried.
        return if back < needed { back } else { 0 };
    }
    0
}

/// Test-only hook [`CastSnapshot::into_cast`] calls first, so a test can probe the
/// recorder lock from inside the off-lock path `cast` serializes on.
#[cfg(test)]
mod serialize_hook {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
    }

    /// Clears the installed hook when dropped.
    pub(super) struct Installed;

    impl Drop for Installed {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    /// Run `hook` at the start of every later serialization on this thread, until
    /// the returned guard drops.
    pub(super) fn install(hook: impl FnMut() + 'static) -> Installed {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        Installed
    }

    pub(super) fn before_serialize() {
        HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

/// Test-only count of the waits [`CastRecorder::snapshot_at`] made on this thread.
#[cfg(test)]
pub(crate) mod cut_probe {
    use std::cell::Cell;

    thread_local! {
        static WAITS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn waited() {
        WAITS.with(|w| w.set(w.get() + 1));
    }

    /// Waits on this thread so far.
    pub(crate) fn waits() -> usize {
        WAITS.with(Cell::get)
    }
}

/// Test-only drivers that record into a session's engine and recorder the way the
/// PTY reader and a resize pass do, hold first and record after.
#[cfg(test)]
pub(crate) mod rig {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use aterm_core::terminal::Terminal;

    use super::{CastCuts, CastRecorder, ResizeStamp};

    /// A reader hold: count it on the recorder's watermark and process `bytes`. The
    /// screen then shows a burst the recording does not hold yet.
    pub(crate) fn hold(
        term: &Mutex<Terminal>,
        cast: &Mutex<CastRecorder>,
        bytes: &[u8],
    ) -> CastCuts {
        let watermark = cast.lock().unwrap().watermark();
        let mut cuts = CastCuts::default();
        let mut t = crate::term_lock(term);
        cuts.observe(0, &t, &watermark);
        t.process(bytes);
        cuts
    }

    /// A reader burst: the [`hold`], then the record at `t` since the epoch.
    pub(crate) fn burst(
        term: &Mutex<Terminal>,
        cast: &Mutex<CastRecorder>,
        t: Duration,
        bytes: &[u8],
    ) {
        let cuts = hold(term, cast, bytes);
        cast.lock().unwrap().record_burst(Arc::from(bytes), cuts, t);
    }

    /// A resize pass's hold: the resize and its stamp, at `t` since the epoch. The
    /// screen then shows a resize the recording does not hold yet.
    pub(crate) fn resize_hold(
        term: &Mutex<Terminal>,
        cast: &Mutex<CastRecorder>,
        t: Duration,
        rows: u16,
        cols: u16,
    ) -> ResizeStamp {
        let at = cast.lock().unwrap().epoch() + t;
        let mut term = crate::term_lock(term);
        term.resize(rows, cols);
        ResizeStamp::take_at(&term, at)
    }

    /// A resize pass: the [`resize_hold`], then the record.
    pub(crate) fn resize(
        term: &Mutex<Terminal>,
        cast: &Mutex<CastRecorder>,
        t: Duration,
        rows: u16,
        cols: u16,
    ) {
        let stamp = resize_hold(term, cast, t, rows, cols);
        cast.lock()
            .unwrap()
            .record_resize_stamped(stamp, cols, rows);
    }
}

/// One output burst as a `bytes` subscriber's queue holds it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QueuedBurst {
    /// The reader thread's shared allocation, one refcount per queue.
    pub(crate) bytes: Arc<[u8]>,
    /// The instant the reader took the burst in, on `metrics::now_us`'s clock.
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

/// The per-session live byte fan-out behind `subscribe … bytes`, the push twin of the
/// `cast` snapshot: the reader tees every output burst (the same `Arc<[u8]>`) to each
/// subscriber's queue. Unlike `subscribe screen/cells`, which coalesce, it keeps every
/// nonempty burst between drains; only a flood past a subscriber's byte or entry budget
/// drops oldest, counted as a gap. Held in `SessionCtx`, cloned into the reader thread.
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
    /// A fan-out with the default 4 MiB per-subscriber budget.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A fan-out with an explicit per-subscriber retained-byte budget (≥ 1).
    #[must_use]
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            live: AtomicUsize::new(0),
            budget: budget.max(1),
        }
    }

    /// Push `burst` (an `Arc` bump) into every live subscriber's queue, with `at_us`,
    /// the instant the reader took it in on `metrics::now_us`'s clock, which a
    /// stamped `BYTES` frame reports. Each queue's oldest bursts past its byte or
    /// entry budget are dropped and their bytes counted. Never waits on a
    /// subscriber: `slots` and the queues are leaf locks, each held briefly, so a
    /// slow subscriber cannot backpressure the reader. Zero subscribers costs one
    /// atomic load. A burst racing a new subscription may be skipped; a subscriber
    /// is owed only bursts teed after its slot push, which `live`'s
    /// increment-before-push guarantees it sees.
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

    /// Register a new live subscriber, returning an RAII [`ByteSubscription`] that
    /// drains its queue and deregisters on drop.
    #[must_use]
    pub(crate) fn subscribe(self: &Arc<Self>) -> ByteSubscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(ByteSlot {
            id,
            queue: Mutex::new(ByteQueue::default()),
        });
        // Publish the count before the slot so `tee` never early-outs past a
        // registered subscriber (see `live`).
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
        // Decrement after removal, mirroring `subscribe`.
        self.live.fetch_sub(1, Ordering::Release);
    }

    /// Number of live subscribers (tests).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// Consumer end of a byte subscription: `drain` returns every burst queued since the
/// last call, each with its arrival, plus the dropped-byte count; dropping it
/// deregisters.
pub(crate) struct ByteSubscription {
    fanout: Arc<ByteFanout>,
    slot: Arc<ByteSlot>,
    id: u64,
}

impl ByteSubscription {
    /// Take ALL queued bursts (in arrival order, each with the `at_us` it was teed
    /// with) and the dropped-byte count since the previous drain, resetting both.
    /// Loss-free between drains up to budget.
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
mod tests {
    use super::*;

    /// Drives both shipping queues through the real entry ceiling, projecting every
    /// one-byte append onto the derived model; a payload-only rule is the negative
    /// control at the first count eviction. A byte subscriber's queue charges
    /// payloads, so it is the model at the default budget and meets the entry cap. The
    /// recorder charges each one-byte burst `output_cost(1)`, so against the same
    /// budget it is the model whose budget counts those charges, `⌊B / output_cost(1)⌋`
    /// (n bursts overrun B exactly when n overruns that), and its charge alone keeps
    /// it under the cap: that model's payload-only variant never breaks the bound.
    #[test]
    fn tiny_output_retention_conforms() {
        use aterm_models_gui::output_retention_model;
        use aterm_spec::interp;
        let projection = |budget: usize| {
            interp::with_consts(
                &output_retention_model(),
                &[
                    ("Cap", MAX_RETAINED_EVENTS as i64),
                    ("ByteBudget", budget as i64),
                    ("MaxSeq", (MAX_RETAINED_EVENTS + 9) as i64),
                ],
            )
        };
        let fan_model = projection(DEFAULT_BUDGET_BYTES);
        let rec_model = projection(DEFAULT_BUDGET_BYTES / output_cost(1));
        let rec_payload_only = interp::with_buggy(&rec_model, 1);
        let mut fan_state = fan_model.init_state();
        let mut rec_state = rec_model.init_state();
        let mut payload_only_state = rec_payload_only.init_state();
        let mut rec = CastRecorder::new(80, 24);
        let fan = Arc::new(ByteFanout::new());
        let sub = fan.subscribe();
        let first: Arc<[u8]> = Arc::from(&b"x"[..]);
        let weak = Arc::downgrade(&first);
        for i in 0..MAX_RETAINED_EVENTS + 9 {
            let burst = if i == 0 {
                first.clone()
            } else {
                Arc::from(&b"x"[..])
            };
            rec.record_output_shared(Duration::from_millis(i as u64), burst.clone());
            fan.tee(&burst, i as u64);
            assert!(rec_model.fire("Push", &mut rec_state));
            let count = (rec_state["seq"] - rec_state["lo"] + 1) as usize;
            assert_eq!(rec.events.len(), count);
            assert_eq!(rec.evicted, (rec_state["lo"] - 1) as u64);
            assert_eq!(rec.used, count * output_cost(1));
            assert!(fan_model.fire("Push", &mut fan_state));
            let count = (fan_state["seq"] - fan_state["lo"] + 1) as usize;
            let q = sub.slot.queue.lock().unwrap();
            assert_eq!(q.bursts.len(), count);
            assert_eq!(q.dropped, (fan_state["lo"] - 1) as u64);
            assert_eq!(q.used, count);
            assert!(rec_payload_only.fire("Push", &mut payload_only_state));
            assert!(rec_model.check_invariant("EntriesBounded", &payload_only_state));
        }
        drop(first);
        assert!(
            weak.upgrade().is_none(),
            "evicted payload released by both queues"
        );
        // The recorder's slots stay with its live events: an evicted slot is held
        // only until its sealed chunk is spent, and the open events' deque never
        // outgrows two chunks.
        assert!(rec.used <= DEFAULT_BUDGET_BYTES);
        assert!(
            rec.events.slots() <= rec.events.len() + 3 * CHUNK_EVENTS,
            "{} slots for {} events",
            rec.events.slots(),
            rec.events.len()
        );
        eprintln!(
            "tiny output retention: {} events in {} slot bytes, {} bytes charged of {}",
            rec.events.len(),
            rec.events.slots() * EVENT_SLOT_BYTES,
            rec.used,
            DEFAULT_BUDGET_BYTES
        );
        assert_eq!(
            sub.slot.queue.lock().unwrap().bursts.capacity(),
            MAX_RETAINED_EVENTS
        );
        assert!(
            rec.to_asciicast()
                .lines()
                .next()
                .unwrap()
                .contains("aterm_truncated")
        );
        let (bursts, dropped) = sub.drain();
        assert_eq!(bursts.len(), MAX_RETAINED_EVENTS);
        assert_eq!(dropped, 9);
        assert_eq!(sub.drain(), (Vec::new(), 0));

        let old = interp::with_buggy(&fan_model, 1);
        let mut before = fan_state.clone();
        before.insert("seq", MAX_RETAINED_EVENTS as i64);
        before.insert("lo", 1);
        assert!(old.fire("Push", &mut before));
        assert!(!fan_model.check_invariant("EntriesBounded", &before));
    }

    #[test]
    fn empty_live_bursts_use_no_queue_space() {
        let fan = Arc::new(ByteFanout::new());
        let sub = fan.subscribe();
        for i in 0..MAX_RETAINED_EVENTS + 1 {
            fan.tee(&Arc::from(&b""[..]), i as u64);
        }
        let q = sub.slot.queue.lock().unwrap();
        assert_eq!(q.bursts.capacity(), 0);
        assert_eq!(q.used, 0);
        assert_eq!(q.dropped, 0);
    }

    /// The single forward fold reproduces, frame for frame, a per-frame re-fold (each
    /// frame = the screen after every event with `t <= frame_target`).
    #[test]
    fn fold_frames_single_pass_matches_per_frame_refold() {
        let mut rec = CastRecorder::new(40, 6);
        assert!(
            rec.snapshot()
                .fold_frames(4, ResizePolicy::Native)
                .is_empty(),
            "empty recording ⇒ no frames"
        );
        rec.record_output(Duration::from_millis(0), b"one");
        rec.record_output(Duration::from_millis(100), b"\r\ntwo");
        rec.record_output(Duration::from_millis(200), b"\r\nthree");
        rec.record_output(Duration::from_millis(300), b"\r\nfour");
        let snap = rec.snapshot();
        let count = 5;
        let got = snap.fold_frames(count, ResizePolicy::Native);
        assert_eq!(got.len(), count);
        // Reference: re-fold a fresh engine per frame target.
        let span = Duration::from_millis(300); // base is 0 here
        for (k, (_, rows)) in got.iter().enumerate() {
            let target = span * ((k + 1) as u32) / (count as u32);
            let mut term = Terminal::new(6, 40);
            for ev in snap.events.iter() {
                if ev.t() > target {
                    break;
                }
                if let Event::Output { data, .. } = ev {
                    term.process(data);
                }
            }
            let want: Vec<String> = (0..term.rows() as usize)
                .map(|r| crate::control::visible_row(&term, r))
                .collect();
            assert_eq!(rows, &want, "frame {k} diverged from per-frame re-fold");
        }
    }

    /// `fold_frames` rebases to the first retained event, so survivors far past t0
    /// spread frames over the surviving span rather than a blank leading gap.
    #[test]
    fn fold_frames_rebases_to_first_event() {
        let mut rec = CastRecorder::new(40, 6);
        rec.record_output(Duration::from_secs(300), b"AAA");
        rec.record_output(Duration::from_secs(310), b"\r\nZZZ");
        let frames = rec.snapshot().fold_frames(2, ResizePolicy::Native);
        // Frame 1 (rebased midpoint = 305s) already shows the first survivor.
        assert!(
            frames[0].1[0].contains("AAA"),
            "frame 1 must not be blank after rebase"
        );
        // Times are rebased so the flipbook starts at 0, not 300s.
        assert_eq!(frames[0].0, Duration::from_secs(5));
        assert_eq!(frames[1].0, Duration::from_secs(10));
    }

    /// `cast frames` replays a session under its seam policy, as `cast drift` does
    /// (`aterm_control::replay::ReplayEngine`). A ConPTY tab's recording, in which
    /// conhost's `?9001h` decides how a killed pager's alternate screen is left at
    /// the shell's next `133;A` and a grow lands over history, folds to the live
    /// ConPTY engine's screen, and `cast drift` holds the same recording faithful to
    /// it. Negative control: folded under Native it is not that screen.
    #[test]
    fn fold_frames_replays_under_the_sessions_seam_policy() {
        use aterm_control::cast_drift::{DriftArgs, LiveScreen, analyze};
        enum Step {
            Out(Vec<u8>),
            Grow(u16),
        }
        let policy = ResizePolicy::ConPty;
        let (cols, rows) = (40u16, 10u16);
        let mut steps = vec![
            Step::Out(b"\x1b[?9001h\x1b[?1004h".to_vec()),
            Step::Out(
                b"history 1\r\nhistory 2\r\nC:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J".to_vec(),
            ),
        ];
        for i in 1..rows {
            steps.push(Step::Out(format!("pager {i}\r\n").into_bytes()));
        }
        steps.push(Step::Out(b"\x1b]133;D;-1\x07\x1b]133;A\x07C:\\>".to_vec()));
        steps.push(Step::Out(
            (0..12)
                .flat_map(|i| format!("\r\nL{i:02}").into_bytes())
                .collect(),
        ));
        steps.push(Step::Grow(rows + 4));
        let mut live = Terminal::new(rows, cols);
        live.set_console_host_posture(policy);
        let mut rec = CastRecorder::new(cols, rows);
        for (i, step) in steps.iter().enumerate() {
            let t = Duration::from_millis(10 * i as u64);
            match step {
                Step::Out(bytes) => {
                    live.process(bytes);
                    rec.record_output(t, bytes);
                }
                Step::Grow(to) => {
                    live.resize_with_policy(*to, cols, policy);
                    rec.record_resize(t, cols, *to);
                }
            }
        }
        let screen: Vec<String> = (0..live.rows() as usize)
            .map(|r| crate::control::visible_row(&live, r))
            .collect();
        let folded = |policy| {
            rec.snapshot()
                .fold_frames(1, policy)
                .pop()
                .map(|(_, rows)| rows)
                .expect("a frame")
        };
        assert_eq!(folded(policy), screen, "the live ConPTY screen");
        let (header, events) = rec.snapshot().into_cast();
        let c = live.cursor();
        let held = LiveScreen {
            rows: screen.clone(),
            cursor: Some((c.row, c.col)),
            alt: Some(live.is_alternate_screen()),
        };
        let report = analyze(&header, &events, Some(&held), &DriftArgs::default(), policy);
        assert_eq!(report.fidelity, Some(true), "{report:?}");
        assert_ne!(
            folded(ResizePolicy::Native),
            screen,
            "the recording must tell the policies apart"
        );
    }

    /// Each event line is a `[f64, "o"|"r", string]` JSON array, output-only for
    /// `record_output`, with a correctly JSON-escaped payload.
    #[test]
    fn events_are_well_formed_arrays() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::from_millis(100), b"hello\n");
        rec.record_output(Duration::from_millis(250), b"a\tb\"c\\d\r");
        rec.record_resize(Duration::from_millis(300), 100, 30);

        let cast = rec.to_asciicast();
        let mut lines = cast.lines();
        let _header = lines.next().unwrap();

        // `parse_event` unescapes the payload as a real reader would, so expected values
        // are the original bytes: the wire escaping round-trips.
        let e0 = lines.next().unwrap();
        assert_eq!(
            parse_event(e0),
            (0.100, "o".to_string(), "hello\n".to_string())
        );

        let e1 = lines.next().unwrap();
        assert_eq!(
            parse_event(e1),
            (0.250, "o".to_string(), "a\tb\"c\\d\r".to_string())
        );
        // The raw line escapes them: no bare tab or quote.
        assert!(
            e1.contains("\\t") && e1.contains("\\\"") && e1.contains("\\\\") && e1.contains("\\r")
        );
        assert!(!e1[1..e1.len() - 1].contains('\t'));

        let e2 = lines.next().unwrap();
        assert_eq!(
            parse_event(e2),
            (0.300, "r".to_string(), "100x30".to_string())
        );

        assert!(lines.next().is_none());
    }

    /// Timestamps stay non-decreasing when a caller's `t` goes backwards.
    #[test]
    fn timestamps_are_monotonic() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::from_millis(500), b"a");
        rec.record_output(Duration::from_millis(200), b"b"); // backwards!
        rec.record_output(Duration::from_millis(700), b"c");

        let cast = rec.to_asciicast();
        let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
        assert_eq!(ts.len(), 3);
        for w in ts.windows(2) {
            assert!(w[1] >= w[0], "ts not monotonic: {ts:?}");
        }
        // The backwards burst is clamped up to the previous timestamp, not down.
        assert_eq!(ts[0], 0.500);
        assert_eq!(ts[1], 0.500);
        assert_eq!(ts[2], 0.700);
    }

    /// Control bytes below 0x20 (other than \n\r\t) use the \u00XX form, so the
    /// payload is a legal JSON string body.
    #[test]
    fn control_bytes_use_unicode_escape() {
        let mut rec = CastRecorder::new(80, 24);
        // ESC (0x1b), BEL (0x07), NUL (0x00) — all illegal raw in a JSON string.
        rec.record_output(Duration::ZERO, b"\x1b[0m\x07\x00");
        let cast = rec.to_asciicast();
        let line = cast.lines().nth(1).unwrap();
        // The wire line spells every C0 control as \u00XX, never a bare byte.
        assert!(line.contains("\\u001b") && line.contains("\\u0007") && line.contains("\\u0000"));
        assert!(!line.as_bytes().iter().any(|&b| b < 0x20));
        // And it decodes back to exactly the original control bytes.
        let payload = parse_event(line).2;
        assert_eq!(payload, "\u{1b}[0m\u{7}\u{0}");
    }

    /// After drop-oldest eviction the asciicast discloses the truncation in the header
    /// and rebases survivor timestamps to the first retained event.
    #[test]
    fn eviction_is_disclosed_and_timestamps_rebased() {
        let mut rec = CastRecorder::with_budget(80, 24, 10);
        // 4-byte bursts one second apart; the tiny budget evicts the head, t=0 included.
        for i in 0..100u32 {
            rec.record_output(Duration::from_secs(u64::from(i)), b"abcd");
        }
        assert!(rec.evicted() > 0, "head was evicted");
        assert!(rec.event_count() < 10, "bounded: {}", rec.event_count());
        let cast = rec.to_asciicast();
        let header = cast.lines().next().unwrap();
        assert!(header.starts_with('{') && header.ends_with('}'));
        assert!(header.contains("\"version\": 2"));
        assert!(header.contains("aterm_truncated"), "disclosed: {header}");
        assert!(header.contains("\"evicted_events\""));
        let first_ts = parse_event(cast.lines().nth(1).unwrap()).0;
        assert_eq!(first_ts, 0.0, "first survivor rebased to 0");
        let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
        for w in ts.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    /// An untruncated recording keeps absolute timestamps and no `aterm_truncated` field.
    #[test]
    fn no_eviction_keeps_plain_header_and_absolute_timestamps() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::from_millis(100), b"hi");
        rec.record_output(Duration::from_millis(400), b"there");
        assert_eq!(rec.evicted(), 0);
        let cast = rec.to_asciicast();
        assert!(!cast.contains("aterm_truncated"), "no false disclosure");
        assert_eq!(parse_event(cast.lines().nth(1).unwrap()).0, 0.100);
        assert_eq!(parse_event(cast.lines().nth(2).unwrap()).0, 0.400);
    }

    /// The header names the geometry in force at the first retained event. While the
    /// resize is retained (even with the output before it evicted) the header keeps
    /// the construction size and the replay applies the resize; once drop-oldest
    /// evicts it, the asciicast (and `cast drift`, which audits the same values) and
    /// the flipbook all start at its size, so a 100-column line replays unwrapped on
    /// 30 rows.
    #[test]
    fn evicted_resize_moves_the_header_geometry() {
        let line: Vec<u8> = [&b"\r\n"[..], &[b'X'; 100]].concat();
        let mut rec = CastRecorder::with_budget(80, 24, 4 * output_cost(line.len()));
        rec.record_output(Duration::ZERO, &line);
        rec.record_resize(Duration::from_millis(1), 100, 30);
        let geometry = |rec: &CastRecorder| {
            let (header, _) = rec.snapshot().into_cast();
            (header.width, header.height)
        };
        assert_eq!(geometry(&rec), (80, 24), "untruncated: construction size");
        let mut truncated_with_resize = false;
        let mut ms = 2;
        while rec
            .events
            .iter()
            .any(|ev| matches!(ev, Event::Resize { .. }))
        {
            assert_eq!(geometry(&rec), (80, 24), "a retained resize still applies");
            truncated_with_resize |= rec.evicted() > 0;
            rec.record_output(Duration::from_millis(ms), &line);
            ms += 1;
        }
        assert!(
            truncated_with_resize,
            "the output before the resize was evicted while the resize stayed"
        );
        assert_eq!(geometry(&rec), (100, 30));
        let cast = rec.to_asciicast();
        assert!(
            cast.starts_with("{\"version\": 2, \"width\": 100, \"height\": 30,"),
            "{cast}"
        );
        let parsed = aterm_control::asciicast::parse(&cast).expect("parses");
        assert_eq!((parsed.header.width, parsed.header.height), (100, 30));
        let frames = rec.snapshot().fold_frames(1, ResizePolicy::Native);
        let screen = &frames.last().expect("one frame").1;
        assert_eq!(screen.len(), 30);
        assert!(
            screen.iter().any(|row| *row == "X".repeat(100)),
            "{screen:?}"
        );
    }

    /// A seeded recording: random bursts and pieces from every escape class (quotes,
    /// backslashes, C0, DEL, C1, bidi controls, multibyte text, invalid bytes and a
    /// lead completed by the next burst) and resizes, with timestamps that sometimes
    /// go backwards, under `budget`.
    fn seeded_recording(seed: u64, budget: usize, events: usize) -> CastRecorder {
        const PIECES: [&[u8]; 9] = [
            b"plain text ",
            b"\"q\\",
            b"\x1b[31m\x07\x00\x7f\r\n\t",
            "\u{200e}\u{2066}x\u{2069}\u{2028}".as_bytes(),
            "\u{85}\u{9b}".as_bytes(),
            "é漢🙂".as_bytes(),
            &[0x80, 0xff, b'a', 0xc3],
            &[0xa9],
            &[0xf0, 0x9f],
        ];
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut rec = CastRecorder::with_budget(80, 24, budget);
        let mut ms = 0u64;
        for _ in 0..events {
            ms = match next() % 8 {
                0 => ms.saturating_sub(next() % 50),
                _ => ms + next() % 900,
            };
            let t = Duration::from_millis(ms);
            match next() % 10 {
                0 => rec.record_resize(t, 1 + (next() % 300) as u16, 1 + (next() % 100) as u16),
                1..=4 => rec.record_output(t, PIECES[(next() % 9) as usize]),
                _ => {
                    let bytes: Vec<u8> = (0..next() % 64).map(|_| next() as u8).collect();
                    rec.record_output(t, &bytes);
                }
            }
        }
        rec
    }

    /// The asciicast text as the recorder serialized it under its own lock, before
    /// `cast` moved serialization onto the snapshot: the reference the off-lock
    /// writer must match byte for byte.
    fn locked_reference(rec: &CastRecorder) -> String {
        fn fmt_t(t: Duration) -> String {
            format!("{:.6}", t.as_secs_f64())
        }
        fn json_escape_bytes(bytes: &[u8]) -> String {
            let mut out = String::with_capacity(bytes.len() + 2);
            let mut i = 0;
            while i < bytes.len() {
                match std::str::from_utf8(&bytes[i..]) {
                    Ok(s) => {
                        out.push_str(&crate::control::json_escape(s));
                        break;
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        if valid > 0 {
                            let s = std::str::from_utf8(&bytes[i..i + valid]).unwrap();
                            out.push_str(&crate::control::json_escape(s));
                        }
                        out.push('\u{fffd}');
                        match e.error_len() {
                            Some(len) => i += valid + len,
                            None => break,
                        }
                    }
                }
            }
            out
        }
        let truncated = rec.evicted > 0;
        let rebase = if truncated {
            rec.events.iter().next().map_or(Duration::ZERO, Event::t)
        } else {
            Duration::ZERO
        };
        let mut out = format!(
            "{{\"version\": 2, \"width\": {}, \"height\": {}, \"aterm_resize_undo\": {}",
            rec.width,
            rec.height,
            ResizeLaw::default().header_value()
        );
        if truncated {
            out.push_str(&format!(
                ", \"aterm_truncated\": {{\"evicted_events\": {}, \"note\": \
                 \"drop-oldest evicted the head; leading ANSI state incomplete; \
                 timestamps rebased to the first retained event\"}}",
                rec.evicted
            ));
        }
        if let Some(gap) = rec.handoff {
            let from = gap
                .from_build
                .map_or_else(|| "null".to_string(), |build| build.to_string());
            out.push_str(&format!(
                ", \"aterm_handoff\": {{\"carried\": 0, \"from_build\": {from}, \"note\": \
                 \"recording restarted at a seamless update; the output and screen before it \
                 are not in this recording\"}}"
            ));
        }
        out.push_str("}\n");
        for ev in rec.events.iter() {
            match ev {
                Event::Output { t, data, .. } => {
                    out.push_str(&format!(
                        "[{}, \"o\", \"{}\"]\n",
                        fmt_t(t.saturating_sub(rebase)),
                        json_escape_bytes(data)
                    ));
                }
                Event::Resize { t, cols, rows, .. } => {
                    out.push_str(&format!(
                        "[{}, \"r\", \"{cols}x{rows}\"]\n",
                        fmt_t(t.saturating_sub(rebase))
                    ));
                }
            }
        }
        out
    }

    /// The `cast` reply, serialized off the recorder lock into one buffer, is byte for
    /// byte `OK <n>\n` and the text the recorder serialized under its lock: for an
    /// empty recording, an untruncated one, truncated ones with each handoff form, and
    /// one whose body length needs six digits.
    #[test]
    fn cast_reply_is_byte_identical_to_the_locked_serializer() {
        use crate::session_timeline::HandoffGap;
        let untruncated = seeded_recording(1, DEFAULT_BUDGET_BYTES, 400);
        let mut built = seeded_recording(2, 2048 + 96 * output_cost(0), 400);
        built.mark_handoff(HandoffGap {
            from_build: Some(42),
        });
        let mut unknown_build = seeded_recording(3, 512 + 24 * output_cost(0), 400);
        unknown_build.mark_handoff(HandoffGap { from_build: None });
        let large = seeded_recording(4, DEFAULT_BUDGET_BYTES, 20_000);
        let cases = [
            ("empty", CastRecorder::new(80, 24)),
            ("untruncated", untruncated),
            ("built", built),
            ("unknown build", unknown_build),
            ("large", large),
        ];
        for (name, rec) in cases {
            let want = locked_reference(&rec);
            match name {
                "untruncated" => {
                    assert!(!want.contains("aterm_truncated") && want.contains('\u{fffd}'))
                }
                "built" => assert!(
                    want.contains("aterm_truncated")
                        && want.contains("\"from_build\": 42")
                        && want.contains("\\u200e")
                ),
                "unknown build" => assert!(want.contains("\"from_build\": null")),
                "large" => assert!(want.len() >= 100_000, "{}", want.len()),
                _ => {}
            }
            let reply = crate::control::cmd_cast(&Mutex::new(rec));
            assert!(
                reply == format!("OK {}\n{want}", want.len()),
                "{name}: the reply diverged from the locked serializer"
            );
        }
    }

    /// `cast` lifts the snapshot under the recorder lock and serializes after
    /// releasing it: a probe from the off-lock path it serializes on takes the lock.
    /// Negative control: the same probe inside a hold (serializing under the lock, as
    /// `cast` did) finds it taken.
    #[test]
    fn cmd_cast_releases_the_recorder_before_serializing() {
        use std::cell::Cell;
        use std::rc::Rc;
        let cast = Arc::new(Mutex::new(seeded_recording(5, DEFAULT_BUDGET_BYTES, 64)));
        let held = Rc::new(Cell::new(None));
        let _hook = serialize_hook::install({
            let (cast, held) = (cast.clone(), held.clone());
            move || held.set(Some(cast.try_lock().is_err()))
        });
        assert!(crate::control::cmd_cast(&cast).starts_with("OK "));
        assert_eq!(
            held.take(),
            Some(false),
            "serialized with the recorder lock held"
        );
        let rec = cast.lock().unwrap();
        let _ = rec.snapshot().into_cast();
        drop(rec);
        assert_eq!(held.take(), Some(true), "the probe sees a held recorder");
    }

    /// A lone continuation byte mid-stream renders as one U+FFFD in place.
    #[test]
    fn genuinely_invalid_byte_still_fffd() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::ZERO, &[b'A', 0x80, b'B']);
        let cast = rec.to_asciicast();
        let payload = parse_event(cast.lines().nth(1).unwrap()).2;
        assert_eq!(payload, "A\u{fffd}B");
    }

    /// A trailing incomplete lead is carried, not replaced: the burst that completes it
    /// emits the whole character.
    #[test]
    fn trailing_incomplete_lead_is_carried_not_replaced() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::ZERO, &[b'X', 0xC3]); // 'X' + lead of 'é'
        let payload = parse_event(rec.to_asciicast().lines().nth(1).unwrap()).2;
        assert_eq!(payload, "X");
        assert!(!payload.contains('\u{fffd}'));
        rec.record_output(Duration::from_millis(1), &[0xA9]); // 0xC3 0xA9 = 'é'
        let last = parse_event(rec.to_asciicast().lines().last().unwrap()).2;
        assert_eq!(last, "é");
    }

    /// A complete shared burst is retained zero-copy: the stored event holds the
    /// caller's allocation.
    #[test]
    fn shared_complete_burst_is_stored_without_copy() {
        let mut rec = CastRecorder::new(80, 24);
        let burst: Arc<[u8]> = Arc::from(&b"hello \xe2\x82\xac world\r\n"[..]);
        rec.record_output_shared(Duration::from_millis(1), burst.clone());
        match rec.events.iter().next().expect("one event") {
            Event::Output { data, .. } => {
                assert!(Arc::ptr_eq(data, &burst), "fast path must not copy");
            }
            Event::Resize { .. } => panic!("expected an output event"),
        }
        // Budget accounting charges the identical length either path.
        assert_eq!(rec.used, output_cost(burst.len()));
    }

    /// The zero-copy path is not taken when a carry is pending or the tail is
    /// incomplete; the shared-Arc entry point reassembles like the borrowing one.
    #[test]
    fn shared_burst_with_split_multibyte_still_reassembles() {
        let mut rec = CastRecorder::new(80, 24);
        // First 2 bytes of '€': carried, nothing emitted.
        rec.record_output_shared(Duration::from_millis(1), Arc::from(&[0xE2u8, 0x82][..]));
        assert_eq!(rec.event_count(), 0, "incomplete lead must be carried");
        // Final byte + a new dangling lead: emits '€', carries the lead.
        rec.record_output_shared(Duration::from_millis(2), Arc::from(&[0xACu8, 0xC3][..]));
        assert_eq!(rec.event_count(), 1);
        let payload = parse_event(rec.to_asciicast().lines().nth(1).unwrap()).2;
        assert_eq!(payload, "€");
        // The carried lead completes on the next (fast-path-eligible) burst.
        rec.record_output_shared(Duration::from_millis(3), Arc::from(&[0xA9u8][..]));
        let last = parse_event(rec.to_asciicast().lines().last().unwrap()).2;
        assert_eq!(last, "é");
    }

    /// An empty shared burst records nothing.
    #[test]
    fn shared_empty_burst_records_nothing() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output_shared(Duration::from_millis(1), Arc::from(&b""[..]));
        assert_eq!(rec.event_count(), 0);
    }

    /// `n` complete one-burst payloads, `0;`, `1;`, …: each is retained as is.
    fn numbered(n: usize) -> Vec<Arc<[u8]>> {
        (0..n)
            .map(|i| Arc::from(format!("{i};").as_bytes()))
            .collect()
    }

    /// A snapshot shares the sealed chunks and clones only the open events: after
    /// eight chunks of bursts, lifting one moves the refcounts of fewer than two
    /// chunks of payloads, and it reads the recording's events in order. Negative
    /// control: lifting the list by cloning every event, as the snapshot did, moves
    /// every payload's count.
    #[test]
    fn a_snapshot_shares_the_sealed_chunks() {
        let mut rec = CastRecorder::new(80, 24);
        let bursts = numbered(8 * CHUNK_EVENTS);
        for (i, burst) in bursts.iter().enumerate() {
            rec.record_output_shared(Duration::from_millis(i as u64), burst.clone());
        }
        let counts = || bursts.iter().map(Arc::strong_count).collect::<Vec<_>>();
        let moved = |before: &[usize]| counts().iter().zip(before).filter(|(a, b)| a != b).count();
        let before = counts();
        let snap = rec.snapshot();
        assert!(
            moved(&before) < 2 * CHUNK_EVENTS,
            "{} cloned",
            moved(&before)
        );
        assert_eq!(snap.events.sealed.len(), rec.events.sealed.len());
        assert!(
            snap.events
                .sealed
                .iter()
                .zip(&rec.events.sealed)
                .all(|(a, b)| Arc::ptr_eq(a, b))
        );
        assert_eq!(listed(snap.events.iter()), kinds(&rec));
        drop(snap);
        let before = counts();
        let cloned: Vec<Event> = rec.events.iter().cloned().collect();
        assert_eq!(
            moved(&before),
            bursts.len(),
            "a full clone moves every count"
        );
        drop(cloned);
    }

    /// A resize recorded after the bursts the engine processed after it goes before
    /// them even where that is inside sealed chunks: the chunks are unsealed, a snapshot
    /// lifted before keeps the list it lifted, the resealed list gives back the slots
    /// the unsealing took, and eviction then yields the events in the recorded order.
    #[test]
    fn a_late_resize_lands_inside_a_sealed_chunk() {
        let mut rec = CastRecorder::new(20, 8);
        let mut term = Terminal::new(8, 20);
        term.resize(7, 20);
        let late = ResizeStamp::take(&term);
        let n = 9 * CHUNK_EVENTS + 5;
        for burst_bytes in numbered(n) {
            let mut cuts = CastCuts::default();
            cuts.observe(0, &term, &CastWatermark::default());
            rec.record_burst(burst_bytes, cuts, rec.now());
        }
        assert!(rec.events.sealed.len() >= 8, "the bursts sealed chunks");
        let before = rec.snapshot();
        rec.record_resize_stamped(late, 20, 7);
        let now = kinds(&rec);
        assert_eq!(now.len(), n + 1);
        assert_eq!(now[0], "r:20x7", "before every burst it preceded");
        assert_eq!(
            listed(before.events.iter()),
            now[1..],
            "the snapshot is unmoved"
        );
        assert!(rec.events.sealed.len() >= 8, "sealed again");
        assert!(
            rec.events.slots() <= rec.events.len() + 3 * CHUNK_EVENTS,
            "{} slots for {} events",
            rec.events.slots(),
            rec.events.len()
        );
        assert_eq!(rec.used, rec.events.iter().map(Event::cost).sum::<usize>());
        let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]));
        let evicted: Vec<Event> = std::iter::from_fn(|| rec.events.pop_front()).collect();
        assert_eq!(listed(evicted.iter()), now);
        assert!(rec.events.is_empty() && rec.events.sealed.is_empty());
    }

    /// An evicted event's payload is freed at its eviction when no snapshot shares its
    /// chunk. One a snapshot pinned (it still reads it) is freed at the first eviction
    /// after the snapshot is gone, before its chunk is spent.
    #[test]
    fn eviction_releases_what_a_snapshot_pinned() {
        let mut log = EventLog::default();
        let bursts = numbered(3 * CHUNK_EVENTS);
        let weak: Vec<std::sync::Weak<[u8]>> = bursts.iter().map(Arc::downgrade).collect();
        for (i, data) in bursts.into_iter().enumerate() {
            let ev = Event::Output {
                t: Duration::from_millis(i as u64),
                data,
                key: None,
            };
            log.insert(log.len(), ev);
        }
        let freed =
            |range: std::ops::Range<usize>| weak[range].iter().all(|w| w.upgrade().is_none());
        let snap = log.share();
        for _ in 0..10 {
            drop(log.pop_front());
        }
        assert!(
            weak[..10].iter().all(|w| w.upgrade().is_some()),
            "the snapshot still reads them"
        );
        drop(snap);
        drop(log.pop_front());
        assert!(freed(0..11), "released before the chunk is spent");
        assert!(weak[11..].iter().all(|w| w.upgrade().is_some()), "live");
        drop(log.pop_front());
        assert!(freed(11..12), "unshared: freed at its eviction");
    }

    /// `tee` delivers every burst (non-UTF-8 included) to every subscriber, byte-exact
    /// and uncoalesced.
    #[test]
    fn byte_fanout_tees_every_burst_to_all_subscribers() {
        let fan = Arc::new(ByteFanout::new());
        let a = fan.subscribe();
        let b = fan.subscribe();
        assert_eq!(fan.subscriber_count(), 2);
        fan.tee(&Arc::from(&b"\x1b[31m"[..]), 5);
        fan.tee(&Arc::from(&[0x80u8, 0xff, 0x00][..]), 9); // non-UTF-8 + NUL
        for sub in [&a, &b] {
            let (bursts, dropped) = sub.drain();
            assert_eq!(dropped, 0);
            assert_eq!(bursts.len(), 2, "every burst delivered, no coalesce");
            assert_eq!(&bursts[0].bytes[..], b"\x1b[31m");
            assert_eq!(&bursts[1].bytes[..], &[0x80, 0xff, 0x00]);
            assert_eq!(
                (bursts[0].at_us, bursts[1].at_us),
                (5, 9),
                "each burst carries the arrival it was teed with"
            );
            assert_eq!(sub.drain().0.len(), 0);
        }
    }

    /// A full queue drops oldest and counts the dropped bytes; the newest burst
    /// survives.
    #[test]
    fn byte_fanout_full_queue_drops_oldest_and_counts() {
        let fan = Arc::new(ByteFanout::with_budget(10));
        let sub = fan.subscribe();
        for i in 0..100 {
            fan.tee(&Arc::from(&b"abcd"[..]), i); // 4 bytes each, budget 10
        }
        let (bursts, dropped) = sub.drain();
        assert!(bursts.len() <= 3, "queue bounded: {}", bursts.len());
        assert!(
            dropped >= 4 * (100 - bursts.len() as u64),
            "dropped counted: {dropped}"
        );
        assert_eq!(&bursts.last().unwrap().bytes[..], b"abcd");
    }

    /// Dropping a subscription deregisters it; `tee` then pays nothing for it.
    #[test]
    fn byte_subscription_drop_deregisters() {
        let fan = Arc::new(ByteFanout::new());
        {
            let _s = fan.subscribe();
            assert_eq!(fan.subscriber_count(), 1);
        }
        assert_eq!(fan.subscriber_count(), 0, "deregistered on drop");
        fan.tee(&Arc::from(&b"x"[..]), 0); // safe no-op
    }

    /// The zero-subscriber early-out re-arms at each subscribe/drop edge: a subscriber
    /// gets every burst teed after `subscribe` returns and none before.
    #[test]
    fn byte_fanout_zero_subscriber_fast_path_rearms_across_lifecycle() {
        let fan = Arc::new(ByteFanout::new());
        fan.tee(&Arc::from(&b"before"[..]), 0); // fast-path no-op, nothing retained
        let sub = fan.subscribe();
        fan.tee(&Arc::from(&b"during"[..]), 0);
        let (bursts, dropped) = sub.drain();
        assert_eq!(dropped, 0);
        assert_eq!(bursts.len(), 1, "only the post-subscribe burst is owed");
        assert_eq!(&bursts[0].bytes[..], b"during");
        drop(sub);
        fan.tee(&Arc::from(&b"between"[..]), 0); // fast-path again at zero
        let sub2 = fan.subscribe();
        fan.tee(&Arc::from(&b"again"[..]), 0);
        let (bursts, _) = sub2.drain();
        assert_eq!(
            bursts.len(),
            1,
            "second lifecycle delivers post-subscribe only"
        );
        assert_eq!(&bursts[0].bytes[..], b"again");
    }

    // ---- Engine-ordered cast -----------------------------------------------

    /// The event kinds and payloads as written, oldest first: `o:<text>` and
    /// `r:<cols>x<rows>`.
    fn kinds(rec: &CastRecorder) -> Vec<String> {
        listed(rec.events.iter())
    }

    /// [`kinds`] of any run of events.
    fn listed<'a>(events: impl Iterator<Item = &'a Event>) -> Vec<String> {
        events
            .map(|ev| match ev {
                Event::Output { data, .. } => format!("o:{}", String::from_utf8_lossy(data)),
                Event::Resize { cols, rows, .. } => format!("r:{cols}x{rows}"),
            })
            .collect()
    }

    /// One reader hold: stamp the burst the way the reader does (inside the
    /// hold, before its bytes), then process them.
    fn hold(term: &mut Terminal, cuts: &mut CastCuts, at: usize, bytes: &[u8]) {
        cuts.observe(at, term, &CastWatermark::default());
        term.process(bytes);
    }

    /// One reader burst's record, as the reader makes it after its holds.
    fn burst(rec: &mut CastRecorder, bytes: &[u8], cuts: CastCuts, t: Duration) {
        rec.record_burst(Arc::from(bytes), cuts, t);
    }

    /// Burst A, a resize, then burst B in the engine: in every order the two threads
    /// can reach the recorder's lock, it writes `A, r, B` with non-decreasing times.
    #[test]
    fn a_resize_processed_between_two_bursts_serializes_between_them() {
        let arrivals: [[u8; 3]; 3] = [[0, 1, 2], [1, 0, 2], [0, 2, 1]];
        for order in arrivals {
            let mut rec = CastRecorder::new(20, 8);
            let mut term = Terminal::new(8, 20);
            let mut a = CastCuts::default();
            let ta = rec.now();
            hold(&mut term, &mut a, 0, b"A");
            term.resize(7, 20);
            let r = ResizeStamp::take(&term);
            let mut b = CastCuts::default();
            let tb = rec.now();
            hold(&mut term, &mut b, 0, b"B");
            let (mut a, mut b) = (Some(a), Some(b));
            for which in order {
                match which {
                    0 => burst(&mut rec, b"A", a.take().expect("once"), ta),
                    1 => rec.record_resize_stamped(r, 20, 7),
                    _ => burst(&mut rec, b"B", b.take().expect("once"), tb),
                }
            }
            assert_eq!(kinds(&rec), ["o:A", "r:20x7", "o:B"], "arrival {order:?}");
            let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
            assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{order:?}: {ts:?}");
        }

        // Negative control: stamping each arrival `now()` and appending misplaces
        // the resize.
        let mut old = CastRecorder::new(20, 8);
        old.record_output(old.now(), b"A");
        old.record_output(old.now(), b"B");
        old.record_resize(old.now(), 20, 7);
        assert_eq!(kinds(&old), ["o:A", "o:B", "r:20x7"]);
    }

    /// A burst split by a resize between the reader's slices is cut there: head
    /// before the resize, tail after, in either arrival order.
    #[test]
    fn a_burst_straddling_a_resize_is_cut_at_the_slice() {
        for resize_first in [false, true] {
            let mut rec = CastRecorder::new(20, 8);
            let mut term = Terminal::new(8, 20);
            let mut cuts = CastCuts::default();
            // Arrival time, read once before the reader's first hold.
            let arrived = rec.now();
            hold(&mut term, &mut cuts, 0, b"head");
            hold(&mut term, &mut cuts, 4, b"+more");
            assert_eq!(cuts.pieces(), 1, "no resize yet: one piece");
            assert_eq!(cuts.rest.capacity(), 0, "an uncut burst allocates nothing");
            term.resize(7, 20);
            let r = ResizeStamp::take(&term);
            hold(&mut term, &mut cuts, 9, b"tail");
            assert_eq!(cuts.pieces(), 2, "cut where the resize took the lock");
            if resize_first {
                rec.record_resize_stamped(r, 20, 7);
            }
            burst(&mut rec, b"head+moretail", cuts, arrived);
            if !resize_first {
                rec.record_resize_stamped(r, 20, 7);
            }
            assert_eq!(
                kinds(&rec),
                ["o:head+more", "r:20x7", "o:tail"],
                "resize first: {resize_first}"
            );
            // One time for both pieces, clamped against the resize's: the
            // timeline stays non-decreasing in the engine's order.
            let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
            assert!(
                ts.windows(2).all(|w| w[0] <= w[1]),
                "{resize_first}: {ts:?}"
            );
        }

        // A hold that processed nothing (an empty manual reset) is superseded by the
        // next hold at the same offset: no empty piece and no cut list allocated.
        let mut term = Terminal::new(8, 20);
        let mut cuts = CastCuts::default();
        let watermark = CastWatermark::default();
        cuts.observe(0, &term, &watermark);
        term.resize(7, 20);
        cuts.observe(0, &term, &watermark);
        assert_eq!(cuts.pieces(), 1);
        assert_eq!(cuts.first.map(|s| s.ordinal), Some(1));
        assert_eq!(
            cuts.rest.capacity(),
            0,
            "superseding the first stamp allocates nothing"
        );
    }

    /// A one-piece burst keeps the reader's allocation.
    #[test]
    fn a_one_piece_burst_keeps_the_readers_allocation() {
        let term = Terminal::new(8, 20);
        let mut rec = CastRecorder::new(20, 8);
        let mut cuts = CastCuts::default();
        cuts.observe(0, &term, &CastWatermark::default());
        let bytes: Arc<[u8]> = Arc::from(&b"shared"[..]);
        rec.record_burst(bytes.clone(), cuts, rec.now());
        let Some(Event::Output { data, .. }) = rec.events.iter().next_back() else {
            panic!("an output event");
        };
        assert!(Arc::ptr_eq(data, &bytes), "no copy of a one-piece burst");
    }

    /// On the alternate screen a shrink demotes the top row, so order matters: the
    /// engine-ordered recording replays to the live screen, while writing the resize
    /// after a burst the engine processed first replays to a screen never shown.
    #[test]
    fn the_engine_ordered_recording_replays_to_the_live_screen() {
        const ROWS: u16 = 6;
        let frame: Vec<u8> = {
            let mut f = String::from("\x1b[?1049h\x1b[2J");
            for r in 0..ROWS {
                f.push_str(&format!("\x1b[{};1HL{r}", r + 1));
            }
            f.push_str("\x1b[4;3H");
            f.into_bytes()
        };
        // A diff frame at an absolute row: where it lands depends on whether
        // the shrink moved the rows first.
        let diff = b"\x1b[1;1HT1\x1b[K\x1b[4;3H".to_vec();
        let mut live = Terminal::new(ROWS, 20);
        let mut rec = CastRecorder::new(20, ROWS);
        let mut old = CastRecorder::new(20, ROWS);
        let mut c0 = CastCuts::default();
        let t0 = rec.now();
        hold(&mut live, &mut c0, 0, &frame);
        live.resize(ROWS - 1, 20);
        let shrink = ResizeStamp::take(&live);
        let mut c1 = CastCuts::default();
        let t1 = rec.now();
        hold(&mut live, &mut c1, 0, &diff);
        live.resize(ROWS, 20);
        let grow = ResizeStamp::take(&live);
        // The reader records both bursts before the main thread records
        // either resize: the race.
        burst(&mut rec, &frame, c0, t0);
        burst(&mut rec, &diff, c1, t1);
        rec.record_resize_stamped(shrink, 20, ROWS - 1);
        rec.record_resize_stamped(grow, 20, ROWS);
        old.record_output(Duration::from_millis(1), &frame);
        old.record_output(Duration::from_millis(2), &diff);
        old.record_resize(Duration::from_millis(3), 20, ROWS - 1);
        old.record_resize(Duration::from_millis(4), 20, ROWS);

        let screen = |t: &Terminal| -> Vec<String> {
            (0..t.rows() as usize)
                .map(|r| crate::control::visible_row(t, r))
                .collect()
        };
        let last = |rec: &CastRecorder| {
            rec.snapshot()
                .fold_frames(1, ResizePolicy::Native)
                .pop()
                .map(|(_, rows)| rows)
                .expect("a frame")
        };
        assert_eq!(
            last(&rec),
            screen(&live),
            "engine order replays the live screen"
        );
        assert_ne!(last(&old), screen(&live), "arrival order does not");
        // And the parsed text keeps that order (`cast drift` reads it).
        let parsed = aterm_control::asciicast::parse(&rec.to_asciicast()).expect("parses");
        let codes: Vec<&str> = parsed
            .events
            .iter()
            .map(|ev| match ev {
                CastEvent::Output { .. } => "o",
                CastEvent::Resize { .. } => "r",
            })
            .collect();
        assert_eq!(codes, ["o", "r", "o", "r"]);
    }

    /// Two engines in one session (a restored engine, new lifetime token): the newer
    /// engine's events sort after the older's, and each engine's ordinals order its own.
    #[test]
    fn a_newer_engine_sorts_after_the_older_one() {
        let mut rec = CastRecorder::new(20, 8);
        let mut old = Terminal::new(8, 20);
        old.resize(7, 20);
        let old_resize = ResizeStamp::take(&old);
        let mut fresh = Terminal::new(8, 20);
        assert!(fresh.resize_lifetime() > old.resize_lifetime());
        let mut cuts = CastCuts::default();
        hold(&mut fresh, &mut cuts, 0, b"new");
        let now = rec.now();
        burst(&mut rec, b"new", cuts, now);
        rec.record_resize_stamped(old_resize, 20, 7);
        assert_eq!(kinds(&rec), ["r:20x7", "o:new"]);
    }

    /// A recording stands against a cut as the engine and the recorder say: a hold or
    /// a resize the screen shows that is not recorded leaves it behind, its record
    /// makes it exact, and anything recorded after the cut puts it past. Resizes
    /// recorded out of order are held only once every one up to the cut is in, and a
    /// newer engine's cut holds none of an older engine's resizes.
    #[test]
    fn a_recording_holds_a_cut_behind_exactly_or_past() {
        let term = Mutex::new(Terminal::new(8, 20));
        let cast = Mutex::new(CastRecorder::new(20, 8));
        let watermark = cast.lock().unwrap().watermark();
        let cut = || CastCut::take(&crate::term_lock(&term), &watermark);
        let holding = |cut: &CastCut| cast.lock().unwrap().holding(cut);
        assert_eq!(holding(&cut()), Holding::Exact, "a fresh session");

        let cuts = rig::hold(&term, &cast, b"a");
        let shows_a = cut();
        assert_eq!(
            holding(&shows_a),
            Holding::Behind,
            "the burst is not recorded yet"
        );
        cast.lock()
            .unwrap()
            .record_burst(Arc::from(&b"a"[..]), cuts, Duration::ZERO);
        assert_eq!(holding(&shows_a), Holding::Exact);
        rig::burst(&term, &cast, Duration::from_millis(1), b"b");
        assert_eq!(holding(&shows_a), Holding::Past, "a later burst is in");

        let first = rig::resize_hold(&term, &cast, Duration::from_millis(2), 7, 20);
        let second = rig::resize_hold(&term, &cast, Duration::from_millis(3), 8, 20);
        let shows_both = cut();
        assert_eq!(holding(&shows_both), Holding::Behind);
        cast.lock().unwrap().record_resize_stamped(second, 20, 8);
        assert_eq!(
            holding(&shows_both),
            Holding::Behind,
            "the first resize is still missing"
        );
        cast.lock().unwrap().record_resize_stamped(first, 20, 7);
        assert_eq!(holding(&shows_both), Holding::Exact);

        let fresh = Mutex::new(Terminal::new(8, 20));
        let fresh_cut = CastCut::take(&crate::term_lock(&fresh), &watermark);
        assert_eq!(
            holding(&fresh_cut),
            Holding::Exact,
            "no resize of the newer engine"
        );
        rig::resize(&fresh, &cast, Duration::from_millis(4), 6, 20);
        assert_eq!(holding(&fresh_cut), Holding::Past);
        assert_eq!(
            holding(&shows_both),
            Holding::Past,
            "a newer engine's resize is in"
        );
    }

    /// Parse one event line `[<f64>, "<code>", "<json-string>"]` into
    /// `(t, code, unescaped_payload)`, as strictly as an asciinema-style reader.
    fn parse_event(line: &str) -> (f64, String, String) {
        let inner = line
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or_else(|| panic!("not a JSON array: {line}"));
        let (t_str, after_t) = inner.split_once(',').expect("missing comma after t");
        let t: f64 = t_str.trim().parse().expect("t is not an f64");
        let after_t = after_t.trim_start();
        let (code, after_code) = parse_json_string(after_t);
        let after_code = after_code.trim_start();
        let after_code = after_code
            .strip_prefix(',')
            .expect("missing comma after code");
        let (data, tail) = parse_json_string(after_code.trim_start());
        assert!(tail.trim().is_empty(), "trailing junk after data: {tail:?}");
        (t, code, data)
    }

    /// Parse a leading JSON string literal, returning (unescaped, remainder).
    /// Recognizes the escapes `json_escape` produces.
    fn parse_json_string(s: &str) -> (String, &str) {
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(
            chars.first().copied(),
            Some('"'),
            "expected a JSON string at {s:?}"
        );
        let mut out = String::new();
        let mut ci = 1; // skip the opening quote
        while ci < chars.len() {
            match chars[ci] {
                '"' => {
                    // Closing quote; compute the byte remainder past it.
                    let consumed: usize = chars[..=ci].iter().map(|c| c.len_utf8()).sum();
                    return (out, &s[consumed..]);
                }
                '\\' => {
                    ci += 1;
                    match chars[ci] {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        // `json_escape` emits the short forms for 0x08/0x0C.
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let hex: String = chars[ci + 1..ci + 5].iter().collect();
                            let cp = u32::from_str_radix(&hex, 16).expect("bad \\u");
                            out.push(char::from_u32(cp).expect("bad codepoint"));
                            ci += 4;
                        }
                        other => panic!("unknown escape \\{other}"),
                    }
                }
                other => out.push(other),
            }
            ci += 1;
        }
        panic!("unterminated JSON string: {s:?}");
    }
}
