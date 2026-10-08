// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session asciicast v2 recording of the child's program output.
//!
//! [`CastRecorder`] accumulates coalesced PTY-output bursts as asciicast v2 events (a
//! JSON header line, then `[t, "o", "<data>"]` per burst and `[t, "r", "<cols>x<rows>"]`
//! per resize) and [`CastRecorder::to_asciicast`] serializes them into text that
//! `asciinema play`/`agg` accept.
//!
//! 1. Every chunk is JSON-escaped and UTF-8-lossy decoded; PTY output is binary and
//!    splits multibyte sequences across reads, and raw bytes would invalidate the JSON.
//! 2. Output only: the recorder is fed `buf[..r]` at the reader-thread tap. The
//!    terminal's own query replies (`take_response()`) must never appear as `"o"`
//!    events; keeping them out is the caller's contract.
//! 3. Timestamps are non-decreasing from one epoch captured at construction, in the
//!    order events are written (the engine's, below).
//! 4. Bounded: payload and entry budgets with drop-oldest.
//!
//! The recorder does no fs/socket/lock work; the GUI feeds it from a dedicated writer
//! thread, so the reader's hot path never serializes under `term_lock`.
//!
//! ## The engine's order, not arrival order
//! A burst arrives via the writer thread and a resize via the thread that resized
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

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

/// Default byte budget for the retained event payloads: a flood cannot balloon
/// RAM past this, and an idle terminal costs nothing (no events ⇒ no bytes).
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Cap on retained events as well as payload: a long interactive session can emit
/// millions of one-byte bursts within 4 MiB. Evictions are disclosed as for bytes.
const MAX_RETAINED_EVENTS: usize = 65_536;

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
/// read there; the burst carries one time for all pieces ([`CastBurst::t`]).
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
    /// `(offset into the burst, stamp)` of each later piece, ascending; `None` until a
    /// resize cuts the burst (never `Some` and empty). Boxed to keep channel slots small.
    #[expect(
        clippy::box_collection,
        reason = "one pointer, not a 24-byte Vec, in each of the cast queue's 1024 \
                  preallocated slots per session; the Vec is built only for a burst a \
                  resize cut (a_stamped_burst_stays_small_in_the_queue pins the size)"
    )]
    rest: Option<Box<Vec<(usize, CastStamp)>>>,
}

impl CastCuts {
    /// Note the hold about to process the burst's bytes from offset `at` (in the burst
    /// as the recorder receives it: a manual reset's bytes first, a foreground
    /// handback's where the reader splices them). The caller holds `term`'s lock. No
    /// clock read; allocates only when a resize cuts the burst.
    #[inline]
    pub(crate) fn observe(&mut self, at: usize, term: &Terminal) {
        let stamp = CastStamp {
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        };
        let Some(first) = self.first.as_mut() else {
            self.first = Some(stamp);
            return;
        };
        match self.rest.as_deref_mut().and_then(|rest| rest.last_mut()) {
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
        self.rest.get_or_insert_with(Box::default).push((at, stamp));
    }

    /// How many pieces the burst is cut into (tests).
    #[cfg(test)]
    fn pieces(&self) -> usize {
        usize::from(self.first.is_some()) + self.rest.as_deref().map_or(0, Vec::len)
    }
}

/// One burst for the cast writer: the bytes the engine processed, where they
/// sit in its order, and when they arrived.
pub(crate) struct CastBurst {
    bytes: Arc<[u8]>,
    cuts: CastCuts,
    /// The burst's time on the recorder's timeline in nanoseconds (`u64` keeps the
    /// queue slot at 56 bytes); see [`new`](Self::new).
    t_ns: u64,
}

impl CastBurst {
    /// `bytes` as the engine processed them, `cuts` as the reader's holds noted them,
    /// at `t` since [`CastRecorder::epoch`]: the arrival, read before the reader's first
    /// `term_lock` hold (for a manual reset's empty batch, after its last). All pieces
    /// share that time; next to a resize the recorder clamps it against the resize's,
    /// keeping the timeline non-decreasing in engine order.
    pub(crate) fn new(bytes: Arc<[u8]>, cuts: CastCuts, t: Duration) -> Self {
        Self {
            bytes,
            cuts,
            t_ns: u64::try_from(t.as_nanos()).unwrap_or(u64::MAX),
        }
    }
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
        Self {
            at: Instant::now(),
            lifetime: term.resize_lifetime(),
            ordinal: term.resize_ordinal(),
        }
    }
}

impl Event {
    /// The event's timestamp (seconds since the recording's t0), for keyframe
    /// expansion ([`CastSnapshot::fold_frames`]).
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

    /// The byte cost charged against the budget: the retained payload length.
    fn cost(&self) -> usize {
        match self {
            Event::Output { data, .. } => data.len(),
            // A resize payload is tiny + fixed-ish; charge its rendered length.
            Event::Resize { .. } => 16,
        }
    }
}

/// Accumulates program-output bursts as asciicast v2 and serializes them.
///
/// Construct with the child's initial grid size; feed coalesced output bursts
/// with [`record_output`](Self::record_output) and geometry changes with
/// [`record_resize`](Self::record_resize); render with
/// [`to_asciicast`](Self::to_asciicast).
pub(crate) struct CastRecorder {
    /// asciicast v2 header width (cols), snapshotted at construction.
    width: u16,
    /// asciicast v2 header height (rows), snapshotted at construction.
    height: u16,
    /// Recorded events, oldest first; drop-oldest under [`budget`](Self::budget).
    events: std::collections::VecDeque<Event>,
    /// Sum of `Event::cost()` over `events` (kept in step with the deque).
    used: usize,
    /// The retained-payload byte budget; drop-oldest when `used` would exceed it.
    budget: usize,
    /// Drop-oldest evictions, disclosed in the asciicast header
    /// ([`to_asciicast`](Self::to_asciicast)) because a truncated recording's leading
    /// ANSI state is incomplete.
    evicted: u64,
    /// The last emitted timestamp, so we clamp deltas to be non-decreasing even
    /// if a caller hands a `t` that went backwards.
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
    /// Bursts the reader could not hand to the writer thread (queue full or writer
    /// gone), counted via [`drop_counter`](Self::drop_counter) without this lock and
    /// disclosed in the header ([`to_asciicast`](Self::to_asciicast)) and by `cast
    /// frames` / `cast drift`, since a missing burst makes every replay wrong.
    drops: Arc<CastDrops>,
    /// Set on an adopted session's recorder: the recording starts at a seamless update
    /// and lacks what the session showed before. Disclosed as `aterm_handoff` in the
    /// header ([`to_asciicast`](Self::to_asciicast)) and by `cast frames`.
    handoff: Option<crate::session_timeline::HandoffGap>,
}

/// Reader-side count of bursts the cast tap dropped (see
/// [`CastRecorder::drop_counter`]): relaxed counters bumped only on a failed
/// `try_send`.
#[derive(Default)]
pub(crate) struct CastDrops {
    bursts: AtomicU64,
    bytes: AtomicU64,
}

impl CastDrops {
    /// Count one dropped burst of `len` bytes.
    pub(crate) fn note(&self, len: usize) {
        self.bursts.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(len as u64, Ordering::Relaxed);
    }

    /// `(bursts, bytes)` dropped so far.
    pub(crate) fn load(&self) -> (u64, u64) {
        (
            self.bursts.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

impl CastRecorder {
    /// A recorder for a `cols`×`rows` grid with the default 4 MiB budget.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self::with_budget(cols, rows, DEFAULT_BUDGET_BYTES)
    }

    /// A recorder with an explicit retained-payload byte budget (≥ 1).
    pub(crate) fn with_budget(cols: u16, rows: u16, budget: usize) -> Self {
        Self {
            width: cols,
            height: rows,
            events: std::collections::VecDeque::new(),
            used: 0,
            budget: budget.max(1),
            evicted: 0,
            last_t: Duration::ZERO,
            epoch: Instant::now(),
            pending: Vec::new(),
            drops: Arc::default(),
            handoff: None,
        }
    }

    /// Mark this recording as one that starts at a seamless update
    /// ([`crate::session_timeline::HandoffGap`]).
    pub(crate) fn mark_handoff(&mut self, gap: crate::session_timeline::HandoffGap) {
        self.handoff = Some(gap);
    }

    /// The counter the reader bumps when it cannot hand this recorder a burst, taken
    /// once per attach so counting a drop never takes the recorder lock.
    pub(crate) fn drop_counter(&self) -> Arc<CastDrops> {
        self.drops.clone()
    }

    /// Elapsed time since this recorder's epoch, for callers with no stamp (tests,
    /// unstamped bursts).
    pub(crate) fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// This recorder's epoch, copied by the reader once per attach to place bursts on
    /// this timeline ([`CastBurst::new`]) without the lock.
    pub(crate) fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Clamp `t` to be non-decreasing w.r.t. the last emitted timestamp.
    fn monotonic(&mut self, t: Duration) -> Duration {
        let t = t.max(self.last_t);
        self.last_t = t;
        t
    }

    /// Push `ev` at its place, evicting oldest events first so the deque never exceeds
    /// [`MAX_RETAINED_EVENTS`] or its payload budget. An unstamped event goes last (`t`
    /// clamped non-decreasing). A stamped one goes before every trailing stamped event
    /// the engine applied after it ([`OrderKey`]); the walk back is as long as the race
    /// it repairs. Its `t` is clamped between its neighbours'.
    ///
    /// The pushed event is never dropped, so the most recent activity survives. Every
    /// evicted head event is counted (`evicted`) for [`to_asciicast`](Self::to_asciicast)
    /// to disclose.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "OutputRetention",
            action = "Push",
            project = "cast::tests::tiny_output_retention_conforms"
        )
    )]
    fn push(&mut self, mut ev: Event) {
        let cost = ev.cost();
        while !self.events.is_empty()
            && (self.used.saturating_add(cost) > self.budget
                || self.events.len() >= MAX_RETAINED_EVENTS)
        {
            if let Some(old) = self.events.pop_front() {
                self.used = self.used.saturating_sub(old.cost());
                self.evicted += 1;
            }
        }
        let mut at = self.events.len();
        match ev.key() {
            None => {
                let t = self.monotonic(ev.t());
                ev.set_t(t);
            }
            Some(key) => {
                while at > 0
                    && self
                        .events
                        .get(at - 1)
                        .and_then(Event::key)
                        .is_some_and(|before| before > key)
                {
                    at -= 1;
                }
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
            }
        }
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

    /// Record one reader burst at its place in engine order: one event per piece its
    /// [`CastCuts`] name (a one-piece burst keeps the reader's allocation; a cut burst
    /// copies its pieces). A burst with no stamp is recorded now, last.
    pub(crate) fn record_burst(&mut self, burst: CastBurst) {
        let CastBurst { bytes, cuts, t_ns } = burst;
        let t = Duration::from_nanos(t_ns);
        let Some(first) = cuts.first else {
            let t = self.now();
            self.record_output_shared(t, bytes);
            return;
        };
        // One piece (an empty cut list reads as none): keep the reader's
        // allocation.
        let Some(rest) = cuts.rest.filter(|rest| !rest.is_empty()) else {
            self.record_output_stamped(t, first, bytes);
            return;
        };
        let (mut start, mut stamp) = (0usize, first);
        for &(at, next) in rest.iter() {
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
    }

    /// Number of recorded events (output + resize), for tests.
    #[cfg(test)]
    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Count of drop-oldest evictions so far (0 ⇒ the recording is complete from
    /// t0; > 0 ⇒ the head was truncated and `to_asciicast` discloses it).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Lift the event list and header geometry out of the recorder lock (Arc bumps,
    /// no byte copies) so the O(events) VTE fold in [`CastSnapshot::fold_frames`] runs
    /// with the lock released and never stalls the reader's cast writer thread.
    #[must_use]
    pub(crate) fn snapshot(&self) -> CastSnapshot {
        CastSnapshot {
            width: self.width,
            height: self.height,
            events: self.events.iter().cloned().collect(),
            evicted: self.evicted,
            dropped: self.drops.load(),
            handoff: self.handoff,
        }
    }

    /// Serialize to asciicast v2 text: a header line, then one line per event,
    /// newline-terminated.
    ///
    /// Output reflects the last complete-character boundary: a trailing incomplete lead
    /// stays in `pending`, so this is idempotent and never emits a phantom U+FFFD.
    /// Consumers needing every byte immediately use `subscribe … bytes`.
    pub(crate) fn to_asciicast(&self) -> String {
        // After drop-oldest truncation, rebase survivors to the first retained event
        // (else a player idles through the first survivor's absolute offset) and
        // disclose it in the header (an extra field asciinema/agg ignore), since the
        // evicted prefix carried leading ANSI state. An untruncated recording keeps
        // absolute timestamps and the plain header.
        let truncated = self.evicted > 0;
        let rebase = if truncated {
            self.events.front().map_or(Duration::ZERO, Event::t)
        } else {
            Duration::ZERO
        };
        // Header: a small fixed-shape object; width/height are plain integers so
        // no escaping is needed. The disclosure note is a static ASCII literal.
        // A burst the READER dropped (a full writer queue) is disclosed the same
        // way, `aterm_dropped`, and only when there was one. Every recording
        // says which resize law made it (`aterm_resize_undo`: the engine hands
        // an alternate-screen shrink's rows back to a grow that follows it with
        // nothing output between), so `cast drift` replays it under that law and
        // a key-less recording (an older aterm, a saved file) is matched to its
        // screen instead. Players ignore the key, as they do the others.
        let mut out = format!(
            "{{\"version\": 2, \"width\": {}, \"height\": {}, \"aterm_resize_undo\": 1",
            self.width, self.height
        );
        if truncated {
            out.push_str(&format!(
                ", \"aterm_truncated\": {{\"evicted_events\": {}, \"note\": \
                 \"drop-oldest evicted the head; leading ANSI state incomplete; \
                 timestamps rebased to the first retained event\"}}",
                self.evicted
            ));
        }
        let (dropped_bursts, dropped_bytes) = self.drops.load();
        if dropped_bursts > 0 {
            out.push_str(&format!(
                ", \"aterm_dropped\": {{\"bursts\": {dropped_bursts}, \"bytes\": {dropped_bytes}}}"
            ));
        }
        // An adopted session's recording starts at the update that adopted it; only
        // then is the field present.
        if let Some(gap) = self.handoff {
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
        for ev in &self.events {
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
}

/// An off-lock snapshot of a [`CastRecorder`]'s events and header geometry
/// ([`CastRecorder::snapshot`]), so the O(events) flipbook fold runs outside the
/// recorder lock.
pub(crate) struct CastSnapshot {
    /// asciicast v2 header width (cols), snapshotted at recorder construction.
    width: u16,
    /// asciicast v2 header height (rows), snapshotted at recorder construction.
    height: u16,
    /// Recorded events, oldest first (`Arc` refcount bumps; no byte copy).
    events: Vec<Event>,
    /// Head events evicted before this snapshot, so the `cast frames` flipbook discloses
    /// truncation as `to_asciicast`'s `aterm_truncated` header does.
    evicted: u64,
    /// `(bursts, bytes)` the reader dropped before they reached the recorder
    /// ([`CastDrops`]), as of the snapshot.
    dropped: (u64, u64),
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

    /// `(bursts, bytes)` the reader dropped before they reached the recorder.
    /// `> 0` means the engine processed output this recording does not hold.
    #[must_use]
    pub(crate) fn dropped(&self) -> (u64, u64) {
        self.dropped
    }

    /// The handoff gap this recording starts at, if it does
    /// ([`CastRecorder::mark_handoff`]).
    #[must_use]
    pub(crate) fn handoff(&self) -> Option<crate::session_timeline::HandoffGap> {
        self.handoff
    }

    /// The recording as `cast drift` reads it: header facts and every event on the same
    /// timeline `to_asciicast` prints (rebased after a head eviction), so a run's `t=`
    /// matches a line in the `cast` text. Each burst is an `Arc` bump.
    #[must_use]
    pub(crate) fn drift_input(
        &self,
    ) -> (
        aterm_control::cast_drift::CastHeader,
        Vec<aterm_control::cast_drift::CastEvent<Arc<[u8]>>>,
    ) {
        use aterm_control::cast_drift::{CastDrops as Drops, CastEvent, CastHeader};
        let rebase = if self.evicted > 0 {
            self.events.first().map_or(Duration::ZERO, Event::t)
        } else {
            Duration::ZERO
        };
        let events = self
            .events
            .iter()
            .map(|ev| match ev {
                Event::Output { t, data, .. } => CastEvent::Output {
                    t: t.saturating_sub(rebase).as_secs_f64(),
                    data: data.clone(),
                },
                Event::Resize { t, cols, rows, .. } => CastEvent::Resize {
                    t: t.saturating_sub(rebase).as_secs_f64(),
                    cols: *cols,
                    rows: *rows,
                },
            })
            .collect();
        let header = CastHeader {
            width: self.width,
            height: self.height,
            evicted: self.evicted,
            dropped: Some(Drops {
                bursts: self.dropped.0,
                bytes: self.dropped.1,
            }),
            // This engine keeps the resize undo, and its replay says so.
            resize_undo: Some(true),
        };
        (header, events)
    }
}

impl CastSnapshot {
    /// Expand the recording into `count` evenly spaced keyframe screens (the readable
    /// "video" flipbook; `cast` is the compact form). Folds output and resizes into one
    /// headless engine once, in order, rendering its visible rows at each frame
    /// boundary: O(events + count×rows). Frame instants are rebased to the first
    /// retained event, so an evicted head spreads frames over the surviving span.
    /// Returns `(rebased_elapsed, rows)` oldest first; empty for an empty recording.
    #[must_use]
    pub(crate) fn fold_frames(&self, count: usize) -> Vec<(Duration, Vec<String>)> {
        let count = count.clamp(1, 240);
        let (Some(first), Some(last)) = (self.events.first(), self.events.last()) else {
            return Vec::new();
        };
        let base = first.t();
        let span = last.t().saturating_sub(base);
        let mut term = Terminal::new(self.height, self.width);
        // Single forward fold: `idx` never rewinds, and frame targets are monotone
        // non-decreasing, so each event is processed exactly once across all frames.
        let mut idx = 0usize;
        let mut frames = Vec::with_capacity(count);
        for k in 1..=count {
            // Frame k's absolute instant; the last frame lands on the final state.
            let target = base + span * (k as u32) / (count as u32);
            while idx < self.events.len() && self.events[idx].t() <= target {
                match &self.events[idx] {
                    Event::Output { data, .. } => term.process(data),
                    Event::Resize { cols, rows, .. } => term.resize(*rows, *cols),
                }
                idx += 1;
            }
            let rows = term.rows() as usize;
            let mut screen = Vec::with_capacity(rows);
            for r in 0..rows {
                screen.push(crate::control::visible_row(&term, r));
            }
            frames.push((target.saturating_sub(base), screen));
        }
        frames
    }
}

/// Format a [`Duration`] as the asciicast `f64` seconds field (microsecond
/// precision; always a decimal point so it parses as a JSON number/float).
fn fmt_t(t: Duration) -> String {
    format!("{:.6}", t.as_secs_f64())
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

/// JSON-escape raw bytes as a string body: longest valid UTF-8 runs go through
/// [`json_escape`], and each invalid byte becomes one U+FFFD (incomplete trailing
/// leads are carried by the caller and never reach here).
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
                    // SAFETY: bytes[i..i+valid] is valid UTF-8 by `valid_up_to`.
                    let s = std::str::from_utf8(&bytes[i..i + valid]).unwrap();
                    out.push_str(&crate::control::json_escape(s));
                }
                out.push('\u{fffd}');
                match e.error_len() {
                    Some(len) => i += valid + len,
                    // No `error_len` => an unexpected end; the caller carries
                    // genuine incomplete leads, so treat any residue as consumed.
                    None => break,
                }
            }
        }
    }
    out
}

// ===========================================================================
// ByteFanout: the live, byte-lossless output channel
// ===========================================================================
//
// `CastRecorder` is a pull snapshot (the `cast` verb); `ByteFanout` is its push twin.
// The reader thread tees every output burst (the same `Arc<[u8]>`, one refcount) to
// each live subscriber's queue. Unlike `subscribe screen/cells`, which coalesces, the
// `bytes` stream keeps every nonempty burst between wakes; only a flood past the
// per-subscriber byte or entry budget drops oldest, reported as a counted gap. The
// producer never blocks (push + drop-oldest under a leaf mutex).

/// One subscriber's byte- and entry-bounded, drop-oldest queue of output bursts.
#[derive(Default)]
struct ByteQueue {
    /// Bursts in arrival order; each is the reader thread's shared `Arc<[u8]>`.
    bursts: VecDeque<Arc<[u8]>>,
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

/// The per-session live byte fan-out, held in `SessionCtx` (for `subscribe … bytes`
/// registration) and cloned into the reader thread. With no subscribers `tee` exits
/// after one atomic load without touching the `slots` mutex.
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

    /// Push `burst` (an `Arc` bump) into every live subscriber's queue, dropping each
    /// queue's oldest bursts past its byte or entry budget and counting dropped bytes.
    /// Never blocks: a slow subscriber cannot backpressure the reader. Zero
    /// subscribers costs one atomic load. A burst racing a new subscription may be
    /// skipped; a subscriber is owed only bursts teed after its slot push, which
    /// `live`'s increment-before-push guarantees it sees.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "OutputRetention",
            action = "Push",
            project = "cast::tests::tiny_output_retention_conforms"
        )
    )]
    pub(crate) fn tee(&self, burst: &Arc<[u8]>) {
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
                    q.used = q.used.saturating_sub(old.len());
                    q.dropped += old.len() as u64;
                }
            }
            q.bursts.push_back(burst.clone());
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
        // Publish the count BEFORE the slot so `tee` can never early-out past a
        // fully-registered subscriber (see `live`).
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
        // Decrement AFTER removal (mirror of `subscribe`'s order).
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
/// last call plus the dropped-byte count; dropping it deregisters.
pub(crate) struct ByteSubscription {
    fanout: Arc<ByteFanout>,
    slot: Arc<ByteSlot>,
    id: u64,
}

impl ByteSubscription {
    /// Take ALL queued bursts (in arrival order) and the dropped-byte count since
    /// the previous drain, resetting both. Loss-free between drains up to budget.
    #[must_use]
    pub(crate) fn drain(&self) -> (Vec<Arc<[u8]>>, u64) {
        let mut q = self.slot.queue.lock().unwrap_or_else(|p| p.into_inner());
        let bursts: Vec<Arc<[u8]>> = q.bursts.drain(..).collect();
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

    /// Tier-1: drive both shipping queues through the real entry ceiling and project
    /// every one-byte append onto the derived model; a payload-only rule is the
    /// negative control at the first count eviction.
    #[test]
    fn tiny_output_retention_conforms() {
        use aterm_spec::{derive::output_retention_model, interp};
        let model = interp::with_consts(
            &output_retention_model(),
            &[
                ("Cap", MAX_RETAINED_EVENTS as i64),
                ("ByteBudget", DEFAULT_BUDGET_BYTES as i64),
                ("MaxSeq", (MAX_RETAINED_EVENTS + 9) as i64),
            ],
        );
        let mut state = model.init_state();
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
            fan.tee(&burst);
            assert!(model.fire("Push", &mut state));
            let count = (state["seq"] - state["lo"] + 1) as usize;
            assert_eq!(rec.events.len(), count);
            assert_eq!(rec.evicted, (state["lo"] - 1) as u64);
            let q = sub.slot.queue.lock().unwrap();
            assert_eq!(q.bursts.len(), count);
            assert_eq!(q.dropped, (state["lo"] - 1) as u64);
            assert_eq!(q.used, count);
        }
        drop(first);
        assert!(
            weak.upgrade().is_none(),
            "evicted payload released by both queues"
        );
        assert_eq!(rec.events.capacity(), MAX_RETAINED_EVENTS);
        eprintln!(
            "tiny output retention: {} events, {} deque bytes; payload-only ceiling allowed {} deque bytes",
            rec.events.len(),
            rec.events.capacity() * std::mem::size_of::<Event>(),
            DEFAULT_BUDGET_BYTES * std::mem::size_of::<Event>()
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

        let old = interp::with_buggy(&model, 1);
        let mut before = state.clone();
        before.insert("seq", MAX_RETAINED_EVENTS as i64);
        before.insert("lo", 1);
        assert!(old.fire("Push", &mut before));
        assert!(!model.check_invariant("EntriesBounded", &before));
    }

    #[test]
    fn empty_live_bursts_use_no_queue_space() {
        let fan = Arc::new(ByteFanout::new());
        let sub = fan.subscribe();
        for _ in 0..MAX_RETAINED_EVENTS + 1 {
            fan.tee(&Arc::from(&b""[..]));
        }
        let q = sub.slot.queue.lock().unwrap();
        assert_eq!(q.bursts.capacity(), 0);
        assert_eq!(q.used, 0);
        assert_eq!(q.dropped, 0);
    }

    /// `fold_frames` yields N keyframes showing the screen at ~k/N of the span: none for
    /// an empty recording, and later text only in the final frame.
    #[test]
    fn fold_frames_steps_through_the_recording() {
        let mut rec = CastRecorder::new(40, 6);
        assert!(
            rec.snapshot().fold_frames(4).is_empty(),
            "empty recording ⇒ no frames"
        );

        rec.record_output(Duration::from_millis(0), b"AAA");
        rec.record_output(Duration::from_millis(1000), b"\r\nZZZ");
        let frames = rec.snapshot().fold_frames(2);
        assert_eq!(frames.len(), 2, "asked for 2 keyframes");
        // Frame 1 (~500ms) has the early output but NOT the 1000ms output.
        let row0_f1 = &frames[0].1[0];
        assert!(row0_f1.contains("AAA") && !row0_f1.contains("ZZZ"));
        // Frame 2 (final) has both.
        let text_f2: String = frames[1].1.join("");
        assert!(text_f2.contains("AAA") && text_f2.contains("ZZZ"));
    }

    /// The single forward fold reproduces, frame for frame, a per-frame re-fold (each
    /// frame = the screen after every event with `t <= frame_target`).
    #[test]
    fn fold_frames_single_pass_matches_per_frame_refold() {
        let mut rec = CastRecorder::new(40, 6);
        rec.record_output(Duration::from_millis(0), b"one");
        rec.record_output(Duration::from_millis(100), b"\r\ntwo");
        rec.record_output(Duration::from_millis(200), b"\r\nthree");
        rec.record_output(Duration::from_millis(300), b"\r\nfour");
        let snap = rec.snapshot();
        let count = 5;
        let got = snap.fold_frames(count);
        assert_eq!(got.len(), count);
        // Reference: independently re-fold a FRESH engine per frame target.
        let span = Duration::from_millis(300); // base is 0 here
        for (k, (_, rows)) in got.iter().enumerate() {
            let target = span * ((k + 1) as u32) / (count as u32);
            let mut term = Terminal::new(6, 40);
            for ev in &snap.events {
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
        let frames = rec.snapshot().fold_frames(2);
        // Frame 1 (rebased midpoint = 305s) already shows the first survivor.
        assert!(
            frames[0].1[0].contains("AAA"),
            "frame 1 must not be blank after rebase"
        );
        // Times are rebased so the flipbook starts at 0, not 300s.
        assert_eq!(frames[0].0, Duration::from_secs(5));
        assert_eq!(frames[1].0, Duration::from_secs(10));
    }

    /// A header line that `asciinema` would accept: valid JSON object with
    /// `version`/`width`/`height`.
    #[test]
    fn header_is_valid_v2_json() {
        let rec = CastRecorder::new(120, 40);
        let cast = rec.to_asciicast();
        let header = cast.lines().next().unwrap();
        // Hand-parse the three fields we promise (no serde dep in-tree).
        assert!(header.starts_with('{') && header.ends_with('}'));
        assert!(header.contains("\"version\": 2"));
        assert!(header.contains("\"width\": 120"));
        assert!(header.contains("\"height\": 40"));
        // An empty recording is JUST the header line (one line, idle costs none).
        assert_eq!(cast.lines().count(), 1);
        assert_eq!(rec.event_count(), 0);
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
        // And the RAW line really did escape them (no bare control byte / quote).
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

    /// Timestamps are monotonic non-decreasing even when a caller hands a `t`
    /// that went backwards (deltas clamp to ≥ 0).
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
        // The RAW wire line uses \u00XX for every C0 control — never a bare byte.
        assert!(line.contains("\\u001b") && line.contains("\\u0007") && line.contains("\\u0000"));
        // Strip the framing brackets; the inner JSON carries no bare control byte.
        assert!(!line.as_bytes().iter().any(|&b| b < 0x20));
        // And it decodes back to exactly the original control bytes.
        let payload = parse_event(line).2;
        assert_eq!(payload, "\u{1b}[0m\u{7}\u{0}");
    }

    /// Drop-oldest keeps the recording bounded under a flood; the newest event
    /// always survives and the header is unaffected.
    #[test]
    fn budget_drops_oldest() {
        // Tiny budget: each "o" payload below is ~4 bytes, so only a few fit.
        let mut rec = CastRecorder::with_budget(80, 24, 10);
        for i in 0..100u32 {
            rec.record_output(Duration::from_millis(i as u64), b"abcd");
        }
        // Bounded well under 100 events.
        assert!(rec.event_count() < 10, "unbounded: {}", rec.event_count());
        let cast = rec.to_asciicast();
        // Header still present and valid.
        assert!(cast.lines().next().unwrap().contains("\"version\": 2"));
        // Timestamps of the survivors are still monotonic.
        let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
        for w in ts.windows(2) {
            assert!(w[1] >= w[0]);
        }
        // Every dropped head event is COUNTED (never a silent loss).
        assert!(rec.evicted() > 0, "evictions counted: {}", rec.evicted());
    }

    /// After drop-oldest eviction the asciicast discloses the truncation in the header
    /// and rebases survivor timestamps to the first retained event.
    #[test]
    fn eviction_is_disclosed_and_timestamps_rebased() {
        let mut rec = CastRecorder::with_budget(80, 24, 10);
        // 4-byte bursts one SECOND apart; the tiny budget keeps only the newest
        // few, evicting the head (including the t=0 event).
        for i in 0..100u32 {
            rec.record_output(Duration::from_secs(u64::from(i)), b"abcd");
        }
        assert!(rec.evicted() > 0, "head was evicted");
        let cast = rec.to_asciicast();
        let header = cast.lines().next().unwrap();
        // Header stays valid v2 and now carries the truncation disclosure.
        assert!(header.starts_with('{') && header.ends_with('}'));
        assert!(header.contains("\"version\": 2"));
        assert!(header.contains("aterm_truncated"), "disclosed: {header}");
        assert!(header.contains("\"evicted_events\""));
        // First surviving event is rebased to t=0 (no minutes-long leading idle).
        let first_ts = parse_event(cast.lines().nth(1).unwrap()).0;
        assert_eq!(first_ts, 0.0, "first survivor rebased to 0");
        // Survivor timestamps stay monotonic after the rebase.
        let ts: Vec<f64> = cast.lines().skip(1).map(|l| parse_event(l).0).collect();
        for w in ts.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    /// An untruncated recording keeps the plain 3-field header and absolute timestamps
    /// (no rebase, no disclosure field).
    #[test]
    fn no_eviction_keeps_plain_header_and_absolute_timestamps() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::from_millis(100), b"hi");
        rec.record_output(Duration::from_millis(400), b"there");
        assert_eq!(rec.evicted(), 0);
        let cast = rec.to_asciicast();
        assert!(!cast.contains("aterm_truncated"), "no false disclosure");
        // Original absolute timestamps preserved (not rebased to 0).
        assert_eq!(parse_event(cast.lines().nth(1).unwrap()).0, 0.100);
        assert_eq!(parse_event(cast.lines().nth(2).unwrap()).0, 0.400);
    }

    /// A burst the READER dropped (its `try_send` failed) is disclosed in the
    /// header as `aterm_dropped`, beside `aterm_truncated` when both happened,
    /// and carried by the snapshot `cast drift` reads. With no drop the header
    /// is the plain header (the resize-law key and nothing else), byte for byte.
    #[test]
    fn a_dropped_burst_is_disclosed_in_the_header() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::from_millis(100), b"hi");
        assert_eq!(
            rec.to_asciicast().lines().next().unwrap(),
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_resize_undo\": 1}"
        );
        let drops = rec.drop_counter();
        drops.note(4096);
        drops.note(10);
        let cast = rec.to_asciicast();
        let header = cast.lines().next().unwrap();
        assert_eq!(
            header,
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_resize_undo\": 1, \
             \"aterm_dropped\": {\"bursts\": 2, \"bytes\": 4106}}"
        );
        assert_eq!(rec.snapshot().dropped(), (2, 4106));
        let parsed = aterm_control::cast_drift::parse_asciicast(&cast).expect("parses");
        assert_eq!(
            parsed.header.dropped,
            Some(aterm_control::cast_drift::CastDrops {
                bursts: 2,
                bytes: 4106,
            })
        );
        // Both disclosures at once stay one valid object.
        let mut small = CastRecorder::with_budget(80, 24, 10);
        for i in 0..10u32 {
            small.record_output(Duration::from_secs(u64::from(i)), b"abcd");
        }
        small.drop_counter().note(1);
        let header = small.to_asciicast().lines().next().unwrap().to_string();
        assert!(header.contains("\"aterm_truncated\"") && header.contains("\"aterm_dropped\""));
        let parsed = aterm_control::cast_drift::parse_asciicast(&small.to_asciicast())
            .expect("both disclosures parse");
        assert!(parsed.header.evicted > 0);
    }

    /// The header says which resize law made the recording (`aterm_resize_undo`),
    /// the parser reads it back on the text path and the snapshot carries it on
    /// the server's, and a player that knows nothing of it sees a valid v2 header.
    #[test]
    fn the_resize_law_key_round_trips_and_a_player_ignores_it() {
        let rec = CastRecorder::new(80, 24);
        let cast = rec.to_asciicast();
        let header = cast.lines().next().unwrap();
        assert!(header.contains("\"aterm_resize_undo\": 1"), "{header}");
        assert!(header.starts_with("{\"version\": 2, \"width\": 80, \"height\": 24"));
        let parsed = aterm_control::cast_drift::parse_asciicast(&cast).expect("parses");
        assert_eq!(parsed.header.resize_undo, Some(true));
        assert_eq!(rec.snapshot().drift_input().0.resize_undo, Some(true));
    }

    /// `cast drift` reads the snapshot on the SAME timeline the `cast` text
    /// prints (rebased after an eviction), so the server's analysis and a
    /// client's analysis of the fetched text see the same events — and a run's
    /// `t=` is findable in the asciicast.
    #[test]
    fn drift_input_matches_the_asciicast_text() {
        let mut rec = CastRecorder::with_budget(80, 24, 40);
        for i in 0..20u32 {
            rec.record_output(
                Duration::from_millis(u64::from(i) * 1500 + 3),
                b"abcd\x1b[2J",
            );
            if i % 5 == 0 {
                rec.record_resize(Duration::from_millis(u64::from(i) * 1500 + 700), 80, 23);
            }
        }
        let snap = rec.snapshot();
        assert!(snap.evicted() > 0, "exercise the rebase");
        let (header, events) = snap.drift_input();
        let parsed =
            aterm_control::cast_drift::parse_asciicast(&rec.to_asciicast()).expect("parses");
        assert_eq!(parsed.header.width, header.width);
        assert_eq!(parsed.header.height, header.height);
        assert_eq!(parsed.header.evicted, header.evicted);
        assert_eq!(parsed.events.len(), events.len());
        for (a, b) in parsed.events.iter().zip(&events) {
            use aterm_control::cast_drift::{CastEvent, fmt_t};
            assert_eq!(fmt_t(a.t()), fmt_t(b.t()));
            match (a, b) {
                (CastEvent::Output { data: x, .. }, CastEvent::Output { data: y, .. }) => {
                    assert_eq!(&x[..], &y[..]);
                }
                (
                    CastEvent::Resize {
                        cols: c1, rows: r1, ..
                    },
                    CastEvent::Resize {
                        cols: c2, rows: r2, ..
                    },
                ) => assert_eq!((c1, r1), (c2, r2)),
                _ => panic!("event kinds diverge"),
            }
        }
    }

    /// Raw program-output bytes round-trip exactly (ESC/CSI/SGR, tab, quote, backslash,
    /// CR/LF escaped on the wire and decoded back identically).
    #[test]
    fn raw_bytes_round_trip_exactly() {
        let mut rec = CastRecorder::new(80, 24);
        let raw: &[u8] = b"\x1b[31mred\x1b[0m\tx\"y\\z\r\n";
        rec.record_output(Duration::from_millis(5), raw);
        let cast = rec.to_asciicast();
        let payload = parse_event(cast.lines().nth(1).unwrap()).2;
        assert_eq!(payload.as_bytes(), raw, "raw bytes must round-trip exactly");
    }

    /// A genuinely invalid byte (a lone continuation mid-stream, not a trailing
    /// lead) is still rendered as exactly one U+FFFD in place.
    #[test]
    fn genuinely_invalid_byte_still_fffd() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output(Duration::ZERO, &[b'A', 0x80, b'B']);
        let cast = rec.to_asciicast();
        let payload = parse_event(cast.lines().nth(1).unwrap()).2;
        assert_eq!(payload, "A\u{fffd}B");
    }

    /// A trailing incomplete lead is CARRIED (not replaced); the next burst that
    /// completes it produces the whole character.
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
        match rec.events.front().expect("one event") {
            Event::Output { data, .. } => {
                assert!(Arc::ptr_eq(data, &burst), "fast path must not copy");
            }
            Event::Resize { .. } => panic!("expected an output event"),
        }
        // Budget accounting charges the identical length either path.
        assert_eq!(rec.used, burst.len());
    }

    /// The zero-copy path is not taken when a carry is pending or the tail is
    /// incomplete; the shared-Arc entry point reassembles like the borrowing one.
    #[test]
    fn shared_burst_with_split_multibyte_still_reassembles() {
        let mut rec = CastRecorder::new(80, 24);
        // First 2 bytes of '€': carried, nothing emitted.
        rec.record_output_shared(Duration::from_millis(1), Arc::from(&[0xE2u8, 0x82][..]));
        assert_eq!(rec.event_count(), 0, "incomplete lead must be carried");
        // Final byte + a NEW dangling lead: emits '€', carries the lead.
        rec.record_output_shared(Duration::from_millis(2), Arc::from(&[0xACu8, 0xC3][..]));
        assert_eq!(rec.event_count(), 1);
        let payload = parse_event(rec.to_asciicast().lines().nth(1).unwrap()).2;
        assert_eq!(payload, "€");
        // The carried lead completes on the next (fast-path-eligible) burst.
        rec.record_output_shared(Duration::from_millis(3), Arc::from(&[0xA9u8][..]));
        let last = parse_event(rec.to_asciicast().lines().last().unwrap()).2;
        assert_eq!(last, "é");
    }

    /// An empty shared burst records nothing (parity with the borrowing path,
    /// which never pushed zero-length events).
    #[test]
    fn shared_empty_burst_records_nothing() {
        let mut rec = CastRecorder::new(80, 24);
        rec.record_output_shared(Duration::from_millis(1), Arc::from(&b""[..]));
        assert_eq!(rec.event_count(), 0);
    }

    /// ITEM 2: `tee` delivers EVERY burst (incl. non-UTF-8) to ALL subscribers,
    /// byte-exact and every-frame (no coalescing).
    #[test]
    fn byte_fanout_tees_every_burst_to_all_subscribers() {
        let fan = Arc::new(ByteFanout::new());
        let a = fan.subscribe();
        let b = fan.subscribe();
        assert_eq!(fan.subscriber_count(), 2);
        fan.tee(&Arc::from(&b"\x1b[31m"[..]));
        fan.tee(&Arc::from(&[0x80u8, 0xff, 0x00][..])); // non-UTF-8 + NUL
        for sub in [&a, &b] {
            let (bursts, dropped) = sub.drain();
            assert_eq!(dropped, 0);
            assert_eq!(bursts.len(), 2, "every burst delivered, no coalesce");
            assert_eq!(&bursts[0][..], b"\x1b[31m");
            assert_eq!(&bursts[1][..], &[0x80, 0xff, 0x00]);
            // A second drain is empty (queue consumed).
            assert_eq!(sub.drain().0.len(), 0);
        }
    }

    /// A full queue drops OLDEST and counts the dropped bytes; the producer never
    /// blocks. The newest burst always survives.
    #[test]
    fn byte_fanout_full_queue_drops_oldest_and_counts() {
        let fan = Arc::new(ByteFanout::with_budget(10));
        let sub = fan.subscribe();
        for _ in 0..100 {
            fan.tee(&Arc::from(&b"abcd"[..])); // 4 bytes each, budget 10
        }
        let (bursts, dropped) = sub.drain();
        // Bounded well under 100 retained; the rest counted as dropped.
        assert!(bursts.len() <= 3, "queue bounded: {}", bursts.len());
        assert!(
            dropped >= 4 * (100 - bursts.len() as u64),
            "dropped counted: {dropped}"
        );
        // The newest burst is retained.
        assert_eq!(&bursts.last().unwrap()[..], b"abcd");
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
        fan.tee(&Arc::from(&b"x"[..])); // safe no-op
    }

    /// The zero-subscriber early-out never desynchronizes delivery: bursts teed before
    /// a subscription are unseen, every burst after `subscribe` returns is delivered,
    /// and the fast path re-arms at the subscribe/drop edges (including a second
    /// subscription after the count returns to zero).
    #[test]
    fn byte_fanout_zero_subscriber_fast_path_rearms_across_lifecycle() {
        let fan = Arc::new(ByteFanout::new());
        fan.tee(&Arc::from(&b"before"[..])); // fast-path no-op, nothing retained
        let sub = fan.subscribe();
        fan.tee(&Arc::from(&b"during"[..]));
        let (bursts, dropped) = sub.drain();
        assert_eq!(dropped, 0);
        assert_eq!(bursts.len(), 1, "only the post-subscribe burst is owed");
        assert_eq!(&bursts[0][..], b"during");
        drop(sub);
        fan.tee(&Arc::from(&b"between"[..])); // fast-path again at zero
        let sub2 = fan.subscribe();
        fan.tee(&Arc::from(&b"again"[..]));
        let (bursts, _) = sub2.drain();
        assert_eq!(
            bursts.len(),
            1,
            "second lifecycle delivers post-subscribe only"
        );
        assert_eq!(&bursts[0][..], b"again");
    }

    // ---- Engine-ordered cast -----------------------------------------------

    /// The event kinds and payloads as written, oldest first: `o:<text>` and
    /// `r:<cols>x<rows>`.
    fn kinds(rec: &CastRecorder) -> Vec<String> {
        rec.events
            .iter()
            .map(|ev| match ev {
                Event::Output { data, .. } => format!("o:{}", String::from_utf8_lossy(data)),
                Event::Resize { cols, rows, .. } => format!("r:{cols}x{rows}"),
            })
            .collect()
    }

    /// One reader hold: stamp the burst the way the reader does (inside the
    /// hold, before its bytes), then process them.
    fn hold(term: &mut Terminal, cuts: &mut CastCuts, at: usize, bytes: &[u8]) {
        cuts.observe(at, term);
        term.process(bytes);
    }

    /// One burst for the writer, as the reader hands it over.
    fn burst(bytes: &[u8], cuts: CastCuts, t: Duration) -> CastBurst {
        CastBurst::new(Arc::from(bytes), cuts, t)
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
                    0 => rec.record_burst(burst(b"A", a.take().expect("once"), ta)),
                    1 => rec.record_resize_stamped(r, 20, 7),
                    _ => rec.record_burst(burst(b"B", b.take().expect("once"), tb)),
                }
            }
            assert_eq!(kinds(&rec), ["o:A", "r:20x7", "o:B"], "arrival {order:?}");
            let ts: Vec<Duration> = rec.events.iter().map(Event::t).collect();
            assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{order:?}: {ts:?}");
        }

        // The negative control: the same arrivals on the old taps (each
        // stamped `now()` on arrival, appended) misplace the resize.
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
            // The reader's one reading, before its first hold.
            let arrived = rec.now();
            hold(&mut term, &mut cuts, 0, b"head");
            hold(&mut term, &mut cuts, 4, b"+more");
            assert_eq!(cuts.pieces(), 1, "no resize yet: one piece");
            assert!(cuts.rest.is_none(), "an uncut burst allocates nothing");
            term.resize(7, 20);
            let r = ResizeStamp::take(&term);
            hold(&mut term, &mut cuts, 9, b"tail");
            assert_eq!(cuts.pieces(), 2, "cut where the resize took the lock");
            if resize_first {
                rec.record_resize_stamped(r, 20, 7);
            }
            rec.record_burst(burst(b"head+moretail", cuts, arrived));
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
        cuts.observe(0, &term);
        term.resize(7, 20);
        cuts.observe(0, &term);
        assert_eq!(cuts.pieces(), 1);
        assert_eq!(cuts.first.map(|s| s.ordinal), Some(1));
        assert!(
            cuts.rest.is_none(),
            "superseding the first stamp allocates nothing"
        );
    }

    /// A one-piece burst keeps the reader's allocation even with an empty cut list,
    /// which reads as no cut.
    #[test]
    fn a_one_piece_burst_keeps_the_readers_allocation() {
        let term = Terminal::new(8, 20);
        let mut rec = CastRecorder::new(20, 8);
        for rest in [None, Some(Box::default())] {
            let mut cuts = CastCuts::default();
            cuts.observe(0, &term);
            cuts.rest = rest;
            let bytes: Arc<[u8]> = Arc::from(&b"shared"[..]);
            rec.record_burst(CastBurst::new(bytes.clone(), cuts, rec.now()));
            let Some(Event::Output { data, .. }) = rec.events.back() else {
                panic!("an output event");
            };
            assert!(Arc::ptr_eq(data, &bytes), "no copy of a one-piece burst");
        }
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
        // The writer records both bursts before the main thread records
        // either resize: the race.
        rec.record_burst(burst(&frame, c0, t0));
        rec.record_burst(burst(&diff, c1, t1));
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
                .fold_frames(1)
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
        let parsed =
            aterm_control::cast_drift::parse_asciicast(&rec.to_asciicast()).expect("parses");
        let codes: Vec<&str> = parsed
            .events
            .iter()
            .map(|ev| match ev {
                aterm_control::cast_drift::CastEvent::Output { .. } => "o",
                aterm_control::cast_drift::CastEvent::Resize { .. } => "r",
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
        rec.record_burst(burst(b"new", cuts, rec.now()));
        rec.record_resize_stamped(old_resize, 20, 7);
        assert_eq!(kinds(&rec), ["r:20x7", "o:new"]);
    }

    /// The cast queue preallocates `CAST_QUEUE_CAP` (1024) slots per session, so the
    /// stamped burst's size is per-session memory: 16 bytes of shared bytes, a 24-byte
    /// first stamp, one pointer for cuts and an 8-byte time. Pinned so growth is a
    /// decision.
    #[test]
    fn a_stamped_burst_stays_small_in_the_queue() {
        assert_eq!(std::mem::size_of::<Arc<[u8]>>(), 16);
        assert_eq!(std::mem::size_of::<CastCuts>(), 32);
        assert_eq!(std::mem::size_of::<CastBurst>(), 56);
    }

    // ---- test helper: a minimal asciicast-event parser ----

    /// Parse one event line `[<f64>, "<code>", "<json-string>"]` into
    /// `(t, code, unescaped_payload)`, strictly enough to stand in for an asciinema-style
    /// reader.
    fn parse_event(line: &str) -> (f64, String, String) {
        let inner = line
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or_else(|| panic!("not a JSON array: {line}"));
        // t: up to the first comma.
        let (t_str, after_t) = inner.split_once(',').expect("missing comma after t");
        let t: f64 = t_str.trim().parse().expect("t is not an f64");
        // code: the next quoted string.
        let after_t = after_t.trim_start();
        let (code, after_code) = parse_json_string(after_t);
        let after_code = after_code.trim_start();
        let after_code = after_code
            .strip_prefix(',')
            .expect("missing comma after code");
        // data: the final quoted string (rest of the array).
        let (data, tail) = parse_json_string(after_code.trim_start());
        assert!(tail.trim().is_empty(), "trailing junk after data: {tail:?}");
        (t, code, data)
    }

    /// Parse a leading JSON string literal, returning (unescaped, remainder).
    /// Recognizes the exact escapes [`json_escape`] produces.
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
                        // control.rs json_escape emits short forms for 0x08/0x0C.
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
