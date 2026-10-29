// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm buffer-primitives kernel.

#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, deny(unsafe_code))]

use std::sync::Arc;

#[cfg(test)]
std::thread_local! {
    /// Comparisons made by the line-id lower bound: a deterministic work counter
    /// for unit tests, thread-local so parallel tests stay independent.
    static LINE_ID_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn take_line_id_comparisons() -> usize {
    LINE_ID_COMPARISONS.with(|count| {
        let observed = count.get();
        count.set(0);
        observed
    })
}

/// A monotonic position on the event-log spine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// A recorded logical instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ticks(pub u64);

/// A handle into the host-owned blob store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(pub u64);

/// A handle into the keyframe store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyframeId(pub u64);

/// A handle into the live-witness store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WitnessId(pub u64);

/// A handle into the host-op store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostOpId(pub u64);

/// A committed-line identity, stable across scroll and eviction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

/// Where a byte or cell came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginTag {
    /// Authoritative output from the surface's owning Source (e.g. the PTY child).
    Source,
    /// An ephemeral overlay written by a non-owner (§3.2).
    Overlay,
    /// Engine-synthesized (banner, status).
    System,
}

/// Effect marker: reading a [`Surface`] (`read_text`, `snapshot`, `subscribe`).
pub enum BufferRead {}
/// Effect marker: mutating a [`Surface`] (`apply`, `transact`).
pub enum BufferWrite {}

/// Capability witnesses, passed by reference.
pub type ReadCap = aterm_cap::Cap<BufferRead>;
/// See [`ReadCap`].
pub type WriteCap = aterm_cap::Cap<BufferWrite>;

/// A half-open line range `start, end)` for reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: LineId,
    pub end: LineId,
}

/// The closed edit algebra: the argument of the single `apply` verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Append a new committed line of text.
    AppendLine(String),
    /// Replace the text of an existing committed line.
    SetLine(LineId, String),
    /// Clear a committed line to empty; the line is kept, so its `LineId` survives.
    ClearLine(LineId),
}

/// The outcome of a `transact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxnOutcome {
    /// All edits applied atomically; carries the resulting head Seq.
    Committed(Seq),
    /// The surface moved past the base snapshot — nothing applied; caller retries.
    Conflict,
}

/// One entry on the event-log spine: a coalesced high-level op with its assigned `Seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: Seq,
    pub op: Op,
    /// The recorded logical instant.
    pub ts: Ticks,
}

/// High-level op recorded on the spine; every mutation is one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Append(LineId),
    Write(LineId),
    Clear(LineId),
    /// Raw PTY input bytes were fed to the engine; payload in the blob store under this [`BlobId`].
    RawIn(BlobId),
    /// The engine emitted reply bytes to the PTY peer (DSR/DA/…).
    Reply(BlobId),
    /// The terminal was resized to `rows`×`cols`.
    Resize {
        rows: u16,
        cols: u16,
    },
    /// The host cell-pixel box was updated to `width`×`height`.
    CellPx {
        width: u16,
        height: u16,
    },
    /// A keyframe (serialized `TerminalCheckpoint`) taken at a parser-ground boundary.
    Keyframe(KeyframeId),
    /// A snapshot of the live screen a replay to this point must equal.
    Witness(WitnessId),
    /// A host-side terminal mutation recorded in the host-op store under this [`HostOpId`].
    Host(HostOpId),
}

/// A subscription cursor, the synchronous read face of the event log.
#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    at: Seq,
}

/// What a `poll` returns: the new events, or a gap signalling a required re-pull.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubUpdate {
    Events(Vec<Event>),
    /// The cursor fell behind the live ring; re-pull state via `read_text`.
    Gap {
        resync_to: Seq,
    },
}

/// Ring bound: older events are evicted; `seq` keeps counting.
pub const MAX_LOG_EVENTS: usize = 1 << 16;

/// The bounded, append-only, sequence-numbered ring: the only change timeline.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    ring: std::collections::VecDeque<Event>,
    /// Total events ever appended (monotone; the spine's logical length).
    total: u64,
}

impl EventLog {
    /// Append a temporal op at a recorded tick.
    #[must_use = "the evicted event is the spill seam — tier or count it (B.8.2)"]
    pub fn append_at(&mut self, op: Op, ts: Ticks) -> (Seq, Option<Event>) {
        self.total = self.total.saturating_add(1);
        let seq = Seq(self.total);
        let evicted = if self.ring.len() >= MAX_LOG_EVENTS {
            self.ring.pop_front()
        } else {
            None
        };
        self.ring.push_back(Event { seq, op, ts });
        (seq, evicted)
    }

    /// Append a Surface text op at `Ticks(0)`. Eviction here just forgets the
    /// oldest entry; temporal callers use [`append_at`](Self::append_at) and
    /// handle the spill seam.
    #[inline]
    fn append(&mut self, op: Op) -> Seq {
        let (seq, _evicted) = self.append_at(op, Ticks(0));
        seq
    }
    /// The logical length of the spine (total events ever appended).
    #[inline]
    pub const fn total(&self) -> u64 {
        self.total
    }
    /// The current Seq (head of the spine), or `Seq(0)` before any event.
    #[inline]
    pub const fn head(&self) -> Seq {
        Seq(self.total)
    }
    /// Live (un-evicted) events, oldest first.
    #[inline]
    pub fn live(&self) -> impl Iterator<Item = &Event> {
        self.ring.iter()
    }

    /// The oldest live event (the ring's low-water), or `None` when empty.
    #[inline]
    pub fn oldest_live(&self) -> Option<&Event> {
        self.ring.front()
    }

    /// The newest live event, or `None` when empty.
    #[inline]
    pub fn newest_live(&self) -> Option<&Event> {
        self.ring.back()
    }

    /// Live events with `seq > after`, oldest first, reached by an O(1) seek.
    #[inline]
    pub fn live_after(&self, after: Seq) -> impl Iterator<Item = &Event> {
        let start = match self.ring.front() {
            None => 0,
            Some(first) => usize::try_from(after.0.saturating_add(1).saturating_sub(first.seq.0))
                .unwrap_or(usize::MAX)
                .min(self.ring.len()),
        };
        self.ring.range(start..)
    }

    /// Drop every live event and the ring's storage; `seq` keeps counting from the head.
    pub fn forget(&mut self) {
        self.ring = std::collections::VecDeque::new();
    }
}

/// A Surface: committed lines plus the event-log spine.
#[derive(Clone, Debug)]
pub struct Surface {
    /// Immutable snapshot spine. [`Arc::make_mut`] clones this ordered vector
    /// only when a writer actually changes a retained line.
    lines: Arc<Vec<(LineId, Arc<String>)>>,
    next_line: u64,
    /// The sequence spine shared by snapshots until the next real event.
    log: Arc<EventLog>,
}

impl Default for Surface {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    #[inline]
    pub fn new() -> Self {
        Surface {
            lines: Arc::new(Vec::new()),
            next_line: 0,
            log: Arc::new(EventLog::default()),
        }
    }

    #[inline]
    pub fn log(&self) -> &EventLog {
        &self.log
    }
    /// The current head of the spine.
    #[inline]
    pub fn seq(&self) -> Seq {
        self.log.head()
    }

    /// First ordered line whose id is not less than `id`, in O(log lines).
    fn line_lower_bound(&self, id: LineId) -> usize {
        let mut low = 0usize;
        let mut high = self.lines.len();
        while low < high {
            let mid = low + (high - low) / 2;
            #[cfg(test)]
            LINE_ID_COMPARISONS.with(|count| count.set(count.get().saturating_add(1)));
            let Some((candidate, _)) = self.lines.get(mid) else {
                return low;
            };
            if *candidate < id {
                low = mid.saturating_add(1);
            } else {
                high = mid;
            }
        }
        low
    }

    #[inline]
    fn line_index(&self, id: LineId) -> Option<usize> {
        let index = self.line_lower_bound(id);
        match self.lines.get(index) {
            Some((found, _)) if *found == id => Some(index),
            Some(_) | None => None,
        }
    }

    /// Ordered half-open bounds for `r`; inverted/empty ranges stay empty.
    #[inline]
    fn line_range_bounds(&self, r: Range) -> (usize, usize) {
        let start = self.line_lower_bound(r.start);
        if r.end <= r.start {
            return (start, start);
        }
        (start, self.line_lower_bound(r.end))
    }

    /// MUTATE: the one buffer-edit verb.
    pub fn apply(&mut self, _c: &WriteCap, e: Edit) -> Seq {
        match e {
            Edit::AppendLine(text) => {
                let id = LineId(self.next_line);
                self.next_line = self.next_line.saturating_add(1);
                Arc::make_mut(&mut self.lines).push((id, Arc::new(text)));
                Arc::make_mut(&mut self.log).append(Op::Append(id))
            }
            Edit::SetLine(id, text) => {
                // Resolve before make_mut: an absent id still rides the event
                // spine, but must not copy a shared line spine it cannot change.
                if let Some(i) = self.line_index(id) {
                    let changed = match self.lines.get(i) {
                        Some((_, current)) => current.as_str() != text.as_str(),
                        None => false,
                    };
                    if changed {
                        Arc::make_mut(&mut self.lines)[i].1 = Arc::new(text);
                    }
                }
                Arc::make_mut(&mut self.log).append(Op::Write(id))
            }
            Edit::ClearLine(id) => {
                if let Some(i) = self.line_index(id) {
                    let nonempty = match self.lines.get(i) {
                        Some((_, current)) => !current.is_empty(),
                        None => false,
                    };
                    if nonempty {
                        Arc::make_mut(&mut self.lines)[i].1 = Arc::new(String::new());
                    }
                }
                Arc::make_mut(&mut self.log).append(Op::Clear(id))
            }
        }
    }

    /// READ: text over a line range, with origin and the reflected `Seq`.
    pub fn read_text(&self, _c: &ReadCap, r: Range) -> TextWithOrigin {
        let mut out = String::new();
        let (start, end) = self.line_range_bounds(r);
        if let Some(lines) = self.lines.get(start..end) {
            for (_, text) in lines {
                out.push_str(text);
                out.push('\n');
            }
        }
        TextWithOrigin {
            text: out,
            origin: OriginTag::Source,
            seq: self.seq(),
        }
    }

    /// COMPOSE: O(1) copy-on-write snapshot.
    #[inline]
    pub fn snapshot(&self, _c: &ReadCap) -> Snapshot {
        Snapshot {
            at: self.seq(),
            surface: self.clone(),
        }
    }

    /// Open a subscription cursor at the current head; it sees events appended after this point.
    #[inline]
    pub fn subscribe(&self, _c: &ReadCap) -> Cursor {
        Cursor { at: self.seq() }
    }

    /// Pull what's new since the cursor.
    #[allow(
        clippy::collapsible_if,
        reason = "the nested form keeps poll's MIR lowerable for the Trust lane (`ship trust run`); the `&&` let-chain is not"
    )]
    pub fn poll(&self, cursor: Cursor) -> (SubUpdate, Cursor) {
        let head = self.seq();
        if let Some(oldest) = self.log.live().next() {
            if oldest.seq.0 > cursor.at.0.saturating_add(1) {
                return (SubUpdate::Gap { resync_to: head }, Cursor { at: head });
            }
        }
        let mut events: Vec<Event> = Vec::new();
        for e in self.log.live_after(cursor.at) {
            events.push(e.clone());
        }
        (SubUpdate::Events(events), Cursor { at: head })
    }

    /// COMPOSE: an atomic, isolated apply-group under optimistic concurrency over a base seq.
    pub fn transact(&mut self, c: &WriteCap, base: Seq, body: Vec<Edit>) -> TxnOutcome {
        if self.seq() != base {
            return TxnOutcome::Conflict;
        }
        for e in body {
            self.apply(c, e);
        }
        TxnOutcome::Committed(self.seq())
    }
}

/// A read result carrying provenance and the reflected `Seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextWithOrigin {
    pub text: String,
    pub origin: OriginTag,
    pub seq: Seq,
}

/// A seq-anchored copy-on-write view of a Surface.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub at: Seq,
    surface: Surface,
}

impl Snapshot {
    /// Read the frozen world at the snapshot's seq.
    #[inline]
    pub fn read_text(&self, c: &ReadCap, r: Range) -> TextWithOrigin {
        self.surface.read_text(c, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(unsafe_code, reason = "the test build is its own trusted launcher")]
    fn authority() -> aterm_cap::Authority {
        // SAFETY: a test process that handles no untrusted input is its own
        // trusted launcher, which is `root_authority`'s whole contract.
        unsafe { aterm_cap::Authority::root_authority() }
    }

    fn read_cap() -> ReadCap {
        authority().grant(aterm_cap::Tier::Trusted)
    }

    fn write_cap() -> WriteCap {
        authority().grant(aterm_cap::Tier::Trusted)
    }

    #[test]
    fn apply_read_round_trips() {
        let mut s = Surface::new();
        s.apply(&write_cap(), Edit::AppendLine("hello".into()));
        s.apply(&write_cap(), Edit::AppendLine("world".into()));
        let got = s.read_text(
            &read_cap(),
            Range {
                start: LineId(0),
                end: LineId(2),
            },
        );
        assert_eq!(got.text, "hello\nworld\n");
        assert_eq!(got.origin, OriginTag::Source);
        assert_eq!(got.seq, Seq(2));
    }

    /// COW scope: a snapshot — and a writable copy taken from it — share the
    /// line and log spines with the live surface until a line write, which
    /// detaches exactly those two on the writer and nothing on the readers.
    #[test]
    fn snapshot_shares_the_line_and_log_spines_until_a_line_write() {
        let mut live = Surface::new();
        live.apply(&write_cap(), Edit::AppendLine("frozen".into()));
        let snap = live.snapshot(&read_cap());
        let fork = snap.surface.clone();
        for candidate in [&snap.surface, &fork] {
            assert!(Arc::ptr_eq(&live.lines, &candidate.lines));
            assert!(Arc::ptr_eq(&live.log, &candidate.log));
        }

        live.apply(&write_cap(), Edit::SetLine(LineId(0), "live".into()));
        assert!(!Arc::ptr_eq(&live.lines, &snap.surface.lines));
        assert!(!Arc::ptr_eq(&live.log, &snap.surface.log));
        assert!(Arc::ptr_eq(&snap.surface.lines, &fork.lines));
        assert!(Arc::ptr_eq(&snap.surface.log, &fork.log));
        assert_eq!(snap.surface.lines[0].1.as_ref(), "frozen");
        assert_eq!(live.lines[0].1.as_ref(), "live");
    }

    /// An edit naming an absent line still rides the one event spine, but must
    /// not copy a shared line spine it cannot change (`apply` resolves the id
    /// before `make_mut`).
    #[test]
    fn absent_line_edits_append_an_event_without_detaching_the_line_spine() {
        let mut source = Surface::new();
        source.apply(&write_cap(), Edit::AppendLine("line".into()));
        let snap = source.snapshot(&read_cap());

        for edit in [
            Edit::SetLine(LineId(99), "absent".into()),
            Edit::ClearLine(LineId(99)),
        ] {
            let mut fork = snap.surface.clone();
            fork.apply(&write_cap(), edit);
            assert!(
                Arc::ptr_eq(&fork.lines, &snap.surface.lines),
                "an absent line edit must not copy the shared line spine"
            );
            assert!(
                !Arc::ptr_eq(&fork.log, &snap.surface.log),
                "Set/Clear still append their one-spine event"
            );
            assert_eq!(fork.seq().0, snap.at.0 + 1);
        }
    }

    /// A write that changes nothing — the same text, or clearing an already
    /// empty line — advances the log by one and leaves the line spine shared.
    #[test]
    fn equal_line_edits_advance_the_log_without_detaching_the_line_spine() {
        let mut source = Surface::new();
        source.apply(&write_cap(), Edit::AppendLine("same".into()));
        source.apply(&write_cap(), Edit::AppendLine(String::new()));
        let snap = source.snapshot(&read_cap());

        for edit in [
            Edit::SetLine(LineId(0), "same".into()),
            Edit::ClearLine(LineId(1)),
        ] {
            let mut fork = snap.surface.clone();
            fork.apply(&write_cap(), edit);
            assert!(Arc::ptr_eq(&fork.lines, &snap.surface.lines));
            assert!(!Arc::ptr_eq(&fork.log, &snap.surface.log));
            assert_eq!(fork.seq().0, snap.at.0 + 1);
        }
    }

    /// Line lookup is an ordered lower bound, not a prefix scan: reading or
    /// rewriting the tail of a long surface costs at most `ceil(log2(lines + 1))`
    /// id comparisons per lookup.
    #[test]
    fn narrow_tail_line_operations_do_logarithmic_work() {
        for lines in [1usize, 2, 3, 7, 8, 15, 16, 1023, 1024, 4096] {
            let max_per_lookup = (lines + 1).next_power_of_two().ilog2() as usize;
            let mut s = Surface::new();
            for i in 0..lines {
                let text = if i + 1 == lines {
                    "tail needle"
                } else {
                    "prefix"
                };
                s.apply(&write_cap(), Edit::AppendLine(text.into()));
            }
            let tail = Range {
                start: LineId((lines - 1) as u64),
                end: LineId(lines as u64),
            };

            let _ = take_line_id_comparisons();
            assert_eq!(s.read_text(&read_cap(), tail).text, "tail needle\n");
            let read_comparisons = take_line_id_comparisons();
            assert!(read_comparisons > 0, "read must reach the lower bound");
            assert!(
                read_comparisons <= 2 * max_per_lookup,
                "tail read made {read_comparisons} id comparisons for {lines} lines (bound {})",
                2 * max_per_lookup
            );

            s.apply(
                &write_cap(),
                Edit::SetLine(LineId((lines - 1) as u64), "updated".into()),
            );
            let lookup_comparisons = take_line_id_comparisons();
            assert!(lookup_comparisons > 0, "SetLine must reach ordered lookup");
            assert!(
                lookup_comparisons <= max_per_lookup,
                "tail SetLine made {lookup_comparisons} id comparisons for {lines} lines (bound {max_per_lookup})"
            );

            assert!(
                s.read_text(
                    &read_cap(),
                    Range {
                        start: LineId(12),
                        end: LineId(3),
                    },
                )
                .text
                .is_empty(),
                "an inverted range stays empty"
            );
        }
    }

    #[test]
    fn subscribe_pulls_new_events_then_drains() {
        let mut s = Surface::new();
        let cur = s.subscribe(&read_cap());
        s.apply(&write_cap(), Edit::AppendLine("a".into()));
        s.apply(&write_cap(), Edit::AppendLine("b".into()));
        let (upd, cur) = s.poll(cur);
        match upd {
            SubUpdate::Events(ev) => {
                assert_eq!(ev.len(), 2);
                assert_eq!(ev[0].seq, Seq(1));
                assert_eq!(ev[1].seq, Seq(2));
            }
            other => panic!("expected events, got {other:?}"),
        }
        let (upd, _cur) = s.poll(cur);
        assert_eq!(upd, SubUpdate::Events(vec![]));
    }

    #[test]
    fn event_log_ring_does_not_double_past_max() {
        let mut log = EventLog::default();
        let mut max_cap_seen = 0usize;
        for _ in 0..(MAX_LOG_EVENTS + 4096) {
            let (_seq, _evicted) = log.append_at(Op::Resize { rows: 0, cols: 0 }, Ticks(0));
            max_cap_seen = max_cap_seen.max(log.ring.capacity());
        }
        assert_eq!(log.ring.len(), MAX_LOG_EVENTS, "ring stays bounded");
        assert!(
            log.ring.capacity() <= MAX_LOG_EVENTS,
            "no doubling past MAX_LOG_EVENTS: settled cap {} must be <= {}",
            log.ring.capacity(),
            MAX_LOG_EVENTS
        );
        assert!(
            max_cap_seen <= MAX_LOG_EVENTS,
            "never doubled at any point: peak cap {} must be <= {}",
            max_cap_seen,
            MAX_LOG_EVENTS
        );
    }

    /// `live_after`'s seek matches `live().filter(|e| e.seq > after)` at every
    /// watermark around an evicted ring, and `oldest_live`/`newest_live` are the
    /// ring's ends (`newest_live().ts` is the max over a monotone recording).
    #[test]
    fn live_after_seek_matches_the_filter_and_the_ends_are_the_extremes() {
        let mut log = EventLog::default();
        for i in 0..(MAX_LOG_EVENTS + 32) {
            let (_seq, _evicted) =
                log.append_at(Op::Resize { rows: 0, cols: 0 }, Ticks((i as u64) / 3));
        }
        let low = log.oldest_live().expect("non-empty").seq;
        let high = log.newest_live().expect("non-empty").seq;
        assert!(
            low.0 > 1,
            "the fixture must have evicted, or the below-low arm is vacuous"
        );
        assert_eq!(high, log.head(), "newest live seq is the spine head");
        assert_eq!(
            log.newest_live().map(|e| e.ts),
            log.live().map(|e| e.ts).max(),
            "back() is the max tick when ts is monotone nondecreasing"
        );

        let reference = |after: Seq| -> Vec<u64> {
            log.live()
                .filter(|e| e.seq > after)
                .map(|e| e.seq.0)
                .collect()
        };
        let observed =
            |after: Seq| -> Vec<u64> { log.live_after(after).map(|e| e.seq.0).collect() };
        for after in [
            Seq(0),
            Seq(1),
            Seq(low.0 - 1),
            low,
            Seq(low.0 + 1),
            Seq(low.0 + 977),
            Seq(high.0 - 1),
            high,
            Seq(high.0 + 1),
            Seq(u64::MAX),
        ] {
            assert_eq!(
                observed(after),
                reference(after),
                "live_after({after:?}) diverged"
            );
        }
        assert_eq!(
            observed(Seq(low.0 - 1)).len(),
            MAX_LOG_EVENTS,
            "below low-water = all live"
        );
        assert!(observed(high).is_empty(), "at the head = nothing after");
    }

    #[test]
    fn a_forgotten_log_keeps_no_event_and_counts_on_from_its_head() {
        let mut log = EventLog::default();
        for i in 0..5 {
            let _ = log.append_at(Op::Resize { rows: 1, cols: 1 }, Ticks(i));
        }
        log.forget();
        assert_eq!(log.live().count(), 0);
        assert!(log.oldest_live().is_none() && log.newest_live().is_none());
        assert_eq!(log.total(), 5, "the spine's length is kept");
        let (seq, evicted) = log.append_at(Op::Resize { rows: 2, cols: 2 }, Ticks(9));
        assert_eq!(seq, Seq(6), "seq counts on from the head");
        assert!(evicted.is_none());
        assert_eq!(log.live_after(Seq(0)).count(), 1);
    }

    #[test]
    fn transact_is_atomic_and_cc_guarded() {
        let mut s = Surface::new();
        s.apply(&write_cap(), Edit::AppendLine("base".into()));
        let snap = s.snapshot(&read_cap());

        let out = s.transact(
            &write_cap(),
            snap.at,
            vec![Edit::AppendLine("a".into()), Edit::AppendLine("b".into())],
        );
        assert_eq!(out, TxnOutcome::Committed(Seq(3)));
        assert_eq!(
            s.read_text(
                &read_cap(),
                Range {
                    start: LineId(0),
                    end: LineId(9)
                }
            )
            .text,
            "base\na\nb\n"
        );

        let out2 = s.transact(&write_cap(), snap.at, vec![Edit::AppendLine("z".into())]);
        assert_eq!(out2, TxnOutcome::Conflict);
        assert_eq!(s.seq(), Seq(3), "conflict left the surface untouched");
    }

    #[test]
    fn op_and_event_keep_their_slot_sizes() {
        assert_eq!(std::mem::size_of::<Op>(), 16);
        assert_eq!(std::mem::size_of::<Event>(), 32);
    }
}
