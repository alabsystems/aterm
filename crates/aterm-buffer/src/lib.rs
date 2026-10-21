// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm buffer-primitives kernel: a text-line `Surface` over one bounded,
//! sequence-numbered event-log spine (`docs/ATERM_DESIGN.md` §3), with the
//! capability-gated verbs `apply`, `read_text`, `snapshot`, `subscribe`/`poll`
//! and `transact`.
//!
//! Invariant: the spine is gap-free and strictly monotonic — every `apply`
//! yields exactly one new `Seq`, and `seq == log.total()` even after eviction
//! bounds the resident ring. The derived
//! kernel-family models in `aterm_spec::derive` (Kernel, Subscribe, Snapshot,
//! Transact, Ring) check it, and `tests/conformance_*.rs` bind them to this
//! code. The code itself carries no Trust source contracts.

// Production code forbids `unsafe`; the unit-test build only denies it so its
// capability helpers can call the `unsafe` launcher mint under an explicit `allow`.
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

/// A recorded logical instant (the clock tick domain of
/// `docs/design/HIERARCHICAL_SESSIONS.md` Addendum B). Temporal events carry it so
/// replay re-seeds the engine clock instead of reading wall time. Monotone, but
/// not gap-free (unlike `Seq`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ticks(pub u64);

/// A handle into the host-owned blob store: raw input and reply payloads live
/// there, and the spine carries only the handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(pub u64);

/// A handle into the keyframe store. A keyframe is a serialized
/// `TerminalCheckpoint`; a scrub seeks to the nearest keyframe `<= seq(t)` and
/// replays forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyframeId(pub u64);

/// A committed-line identity, stable across scroll and eviction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

/// Where a byte or cell came from, carried on every read so injected content is
/// never silently trusted (`docs/ATERM_DESIGN.md` §3.2).
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

/// Capability witnesses, passed by reference. They are `aterm_cap`
/// capabilities: only an [`aterm_cap::Authority`] grants one, and only
/// `Authority::root_authority`, which the `launcher-mint` source guard keeps in
/// the launcher binaries, mints that. A verb called without the matching cap does not compile, and neither
/// does a forged one:
///
/// ```compile_fail,E0423
/// // A cap is not a value anyone can write down.
/// let _forged = aterm_buffer::WriteCap;
/// ```
///
/// The granted tier is not consulted: buffer edits are reversible and
/// buffer-only, so holding the cap is the whole gate.
pub type ReadCap = aterm_cap::Cap<BufferRead>;
/// See [`ReadCap`].
pub type WriteCap = aterm_cap::Cap<BufferWrite>;

/// A half-open line range `[start, end)` for reads.
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

/// The outcome of a `transact`: an atomic, isolated apply-group under
/// optimistic concurrency control over a base snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxnOutcome {
    /// All edits applied atomically; carries the resulting head Seq.
    Committed(Seq),
    /// The surface moved past the base snapshot — nothing applied; caller retries.
    Conflict,
}

/// One entry on the event-log spine: a coalesced high-level op (not a per-cell
/// delta) with its assigned `Seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: Seq,
    pub op: Op,
    /// The recorded logical instant. Surface text ops use `Ticks(0)`; temporal
    /// ops appended via [`EventLog::append_at`] carry the recorded tick.
    pub ts: Ticks,
}

/// High-level op recorded on the spine; every mutation is one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Append(LineId),
    Write(LineId),
    Clear(LineId),
    // Temporal recording: handles, not payloads. The bytes live in the
    // host-owned blob and keyframe stores.
    /// Raw PTY input bytes were fed to the engine (`process`); payload in the
    /// blob store under this [`BlobId`].
    RawIn(BlobId),
    /// The engine emitted reply bytes to the PTY peer (DSR/DA/…). Recorded for
    /// fork-timeline fidelity; not re-emitted on replay, so a host may keep no
    /// payload under the handle.
    Reply(BlobId),
    /// The terminal was resized to `rows`×`cols`. Reflow is path-dependent, so a
    /// resize is a recorded event and never reordered.
    Resize {
        rows: u16,
        cols: u16,
    },
    /// A keyframe (serialized `TerminalCheckpoint`) taken at a parser-ground
    /// boundary; referenced by [`KeyframeId`] in the keyframe store.
    Keyframe(KeyframeId),
}

/// A subscription cursor, the synchronous read face of the event log. `poll`
/// returns events after `at`; a cursor behind the ring's horizon gets a `Gap`
/// and resyncs via `read_text`. A slow subscriber never blocks the writer.
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
    /// Append a temporal op at a recorded tick. Returns the assigned `Seq` and,
    /// when the ring was full, the evicted oldest event.
    ///
    /// The evicted event is the spill seam: the caller tiers it (the
    /// `NoSilentLoss` obligation of `tier_residency_model`) or, keeping no
    /// tier, counts it and releases what it referenced; it never drops it
    /// silently. This crate does no fs; the host owns tiered persistence, which
    /// also keeps `EventLog: Clone + Debug + Default`.
    #[must_use = "the evicted event is the spill seam — tier or count it (B.8.2)"]
    pub fn append_at(&mut self, op: Op, ts: Ticks) -> (Seq, Option<Event>) {
        self.total = self.total.saturating_add(1);
        let seq = Seq(self.total);
        // A full ring needs `MAX_LOG_EVENTS + 1` slots (push, then evict).
        // Reserve that one extra slot at the boundary so the deque does not
        // double to `2 * MAX_LOG_EVENTS` (pop_front never shrinks it).
        if self.ring.len() == MAX_LOG_EVENTS && self.ring.capacity() == MAX_LOG_EVENTS {
            self.ring.reserve_exact(1);
        }
        self.ring.push_back(Event { seq, op, ts });
        let evicted = if self.ring.len() > MAX_LOG_EVENTS {
            self.ring.pop_front()
        } else {
            None
        };
        (seq, evicted)
    }

    /// Append a Surface text op at `Ticks(0)`. Eviction here just forgets the
    /// oldest entry; temporal callers use [`append_at`](Self::append_at) and
    /// handle the spill seam.
    fn append(&mut self, op: Op) -> Seq {
        let (seq, _evicted) = self.append_at(op, Ticks(0));
        seq
    }
    /// The logical length of the spine (total events ever appended).
    pub fn total(&self) -> u64 {
        self.total
    }
    /// The current Seq (head of the spine), or `Seq(0)` before any event.
    pub fn head(&self) -> Seq {
        Seq(self.total)
    }
    /// Live (un-evicted) events, oldest first.
    pub fn live(&self) -> impl Iterator<Item = &Event> {
        self.ring.iter()
    }

    /// The oldest live event (the ring's low-water), or `None` when empty. O(1).
    ///
    /// Live seqs are the contiguous range `[oldest.seq, head]`, so
    /// `seq >= oldest_live().seq` means "still on the spine". A keyframe store
    /// that outlives the ring must check this before seeding a replay, or it
    /// would fold a chain whose head was evicted into a wrong engine.
    pub fn oldest_live(&self) -> Option<&Event> {
        self.ring.front()
    }

    /// The newest live event, or `None` when empty. O(1).
    ///
    /// With [`append_at`](Self::append_at) fed from one monotone clock, `ts` is
    /// nondecreasing down the ring, so this is also the latest recorded instant.
    pub fn newest_live(&self) -> Option<&Event> {
        self.ring.back()
    }

    /// Live events with `seq > after`, oldest first, reached by an O(1) seek.
    ///
    /// Live seqs are contiguous and ascending, so the first event past `after`
    /// sits at offset `after + 1 - oldest.seq`, clamped to `[0, len]`: a
    /// watermark below the low-water yields everything, one at or past the head
    /// yields nothing.
    pub fn live_after(&self, after: Seq) -> impl Iterator<Item = &Event> {
        let start = match self.ring.front() {
            None => 0,
            Some(first) => usize::try_from(after.0.saturating_add(1).saturating_sub(first.seq.0))
                .unwrap_or(usize::MAX)
                .min(self.ring.len()),
        };
        self.ring.range(start..)
    }
}

/// A Surface: committed lines plus the event-log spine. It is the native
/// editor's document model (`amacs`'s `store.rs`: one `Document` owns one
/// `Surface`); terminal cells live in `aterm-grid`.
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
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    pub fn new() -> Self {
        Surface {
            lines: Arc::new(Vec::new()),
            next_line: 0,
            log: Arc::new(EventLog::default()),
        }
    }

    pub fn log(&self) -> &EventLog {
        &self.log
    }
    /// The current head of the spine.
    pub fn seq(&self) -> Seq {
        self.log.head()
    }

    /// First ordered line whose id is not less than `id`, in O(log lines).
    ///
    /// Line ids are appended monotonically and never reordered. The explicit
    /// loop avoids a borrowing closure in the Trust lane.
    fn line_lower_bound(&self, id: LineId) -> usize {
        let mut low = 0usize;
        let mut high = self.lines.len();
        while low < high {
            let mid = low + (high - low) / 2;
            #[cfg(test)]
            LINE_ID_COMPARISONS.with(|count| count.set(count.get().saturating_add(1)));
            // `mid < high <= len` on every reachable iteration; the checked
            // access spells that bound for Trust, and the miss arm is unreachable.
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

    fn line_index(&self, id: LineId) -> Option<usize> {
        let index = self.line_lower_bound(id);
        match self.lines.get(index) {
            Some((found, _)) if *found == id => Some(index),
            Some(_) | None => None,
        }
    }

    /// Ordered half-open bounds for `r`; inverted/empty ranges stay empty.
    fn line_range_bounds(&self, r: Range) -> (usize, usize) {
        let start = self.line_lower_bound(r.start);
        if r.end <= r.start {
            return (start, start);
        }
        (start, self.line_lower_bound(r.end))
    }

    /// MUTATE: the one buffer-edit verb. Returns the assigned `Seq`. Reversible.
    pub fn apply(&mut self, _c: &WriteCap, e: Edit) -> Seq {
        match e {
            Edit::AppendLine(text) => {
                let id = LineId(self.next_line);
                // 2^64 appends are unreachable; saturating spells the increment
                // in the non-overflowing form the strict L0 gate accepts.
                self.next_line = self.next_line.saturating_add(1);
                // `Arc<String>` moves the public String header without copying
                // its text allocation, and makes a later spine detach shallow.
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

    /// READ: text over a line range, with origin and the reflected `Seq`. All
    /// output is currently tagged `Source`.
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

    /// COMPOSE: O(1) copy-on-write snapshot. The line and event-log spines are
    /// shared; the first mutation of either detaches only that spine.
    pub fn snapshot(&self, _c: &ReadCap) -> Snapshot {
        Snapshot {
            at: self.seq(),
            surface: self.clone(),
        }
    }

    // READ (subscribe): the event log's read face.

    /// Open a subscription cursor at the current head; it sees events appended
    /// after this point.
    pub fn subscribe(&self, _c: &ReadCap) -> Cursor {
        Cursor { at: self.seq() }
    }

    /// Pull what's new since the cursor. Returns the update and the advanced
    /// cursor. A cursor that fell behind the live ring's horizon gets a `Gap`
    /// (it must re-pull via `read_text`); the writer is never blocked.
    #[allow(
        clippy::collapsible_if,
        reason = "the nested form keeps poll's MIR lowerable for the Trust lane (`ship trust run`); the `&&` let-chain is not"
    )]
    pub fn poll(&self, cursor: Cursor) -> (SubUpdate, Cursor) {
        let head = self.seq();
        // Behind the ring if the oldest live seq is newer than `cursor.at + 1`.
        // `if let` + `saturating_add` keep poll closure-free and provably
        // non-overflowing for the strict L0 gate.
        if let Some(oldest) = self.log.live().next() {
            if oldest.seq.0 > cursor.at.0.saturating_add(1) {
                return (SubUpdate::Gap { resync_to: head }, Cursor { at: head });
            }
        }
        // Seek past the cursor rather than scan the ring. The explicit push
        // loop lets the Trust lane (`ship trust run`) bound growth by the ring length.
        let mut events: Vec<Event> = Vec::new();
        for e in self.log.live_after(cursor.at) {
            events.push(e.clone());
        }
        (SubUpdate::Events(events), Cursor { at: head })
    }

    /// COMPOSE: an atomic, isolated apply-group under optimistic concurrency
    /// over a base seq. Commits iff the surface has not advanced past `base`;
    /// otherwise nothing is applied and the caller retries.
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
    pub fn read_text(&self, c: &ReadCap, r: Range) -> TextWithOrigin {
        self.surface.read_text(c, r)
    }
}

/// Wire size in bytes of an encoded spine event (`"SPNE"`).
pub const SPINE_EVENT_WIRE_BYTES: usize = 24;

/// Wire size in bytes of an encoded spine cursor state (`"SPNC"`).
pub const SPINE_CURSOR_WIRE_BYTES: usize = 20;

/// Encode a 24-byte spine [`Event`] wire frame (`"SPNE"`).
#[inline]
#[must_use]
pub const fn encode_spine_event_wire(event: &Event) -> [u8; SPINE_EVENT_WIRE_BYTES] {
    let (op_tag, payload) = match event.op {
        Op::Append(id) => (1u8, id.0),
        Op::Write(id) => (2u8, id.0),
        Op::Clear(id) => (3u8, id.0),
        Op::RawIn(id) => (4u8, id.0),
        Op::Reply(id) => (5u8, id.0),
        Op::Resize { rows, cols } => (6u8, ((rows as u64) << 16) | (cols as u64)),
        Op::Keyframe(id) => (7u8, id.0),
    };
    let [s0, s1, s2, s3, s4, s5, s6, s7] = event.seq.0.to_le_bytes();
    let pb = payload.to_le_bytes();
    let tb = (event.ts.0 as u32).to_le_bytes();
    [
        b'S', b'P', b'N', op_tag, s0, s1, s2, s3, s4, s5, s6, s7, pb[0], pb[1], pb[2], pb[3],
        pb[4], pb[5], pb[6], pb[7], tb[0], tb[1], tb[2], tb[3],
    ]
}

/// Decode a 24-byte spine [`Event`] wire frame produced by
/// [`encode_spine_event_wire`].
#[inline]
#[must_use]
pub const fn decode_spine_event_wire(bytes: &[u8]) -> Option<Event> {
    if bytes.len() < SPINE_EVENT_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'S' || bytes[1] != b'P' || bytes[2] != b'N' {
        return None;
    }
    let seq = Seq(u64::from_le_bytes([
        bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
    ]));
    let payload = u64::from_le_bytes([
        bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
    ]);
    let ts = Ticks(u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as u64);
    let op = match bytes[3] {
        1 => Op::Append(LineId(payload)),
        2 => Op::Write(LineId(payload)),
        3 => Op::Clear(LineId(payload)),
        4 => Op::RawIn(BlobId(payload)),
        5 => Op::Reply(BlobId(payload)),
        6 => {
            if payload > 0xFFFF_FFFF {
                return None;
            }
            Op::Resize {
                rows: (payload >> 16) as u16,
                cols: (payload & 0xFFFF) as u16,
            }
        }
        7 => Op::Keyframe(KeyframeId(payload)),
        _ => return None,
    };
    Some(Event { seq, op, ts })
}

/// Encode a 20-byte spine cursor & origin wire frame (`"SPNC"`).
#[inline]
#[must_use]
pub const fn encode_spine_cursor_wire(
    origin: OriginTag,
    cursor: Cursor,
    head: Seq,
) -> [u8; SPINE_CURSOR_WIRE_BYTES] {
    let origin_byte = match origin {
        OriginTag::Source => 1u8,
        OriginTag::Overlay => 2u8,
        OriginTag::System => 3u8,
    };
    let cb = cursor.at.0.to_le_bytes();
    let hb = head.0.to_le_bytes();
    [
        b'S',
        b'P',
        b'C',
        origin_byte,
        cb[0],
        cb[1],
        cb[2],
        cb[3],
        cb[4],
        cb[5],
        cb[6],
        cb[7],
        hb[0],
        hb[1],
        hb[2],
        hb[3],
        hb[4],
        hb[5],
        hb[6],
        hb[7],
    ]
}

/// Decode a 20-byte spine cursor & origin wire frame produced by
/// [`encode_spine_cursor_wire`].
#[inline]
#[must_use]
pub const fn decode_spine_cursor_wire(bytes: &[u8]) -> Option<(OriginTag, Cursor, Seq)> {
    if bytes.len() < SPINE_CURSOR_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'S' || bytes[1] != b'P' || bytes[2] != b'C' {
        return None;
    }
    let origin = match bytes[3] {
        1 => OriginTag::Source,
        2 => OriginTag::Overlay,
        3 => OriginTag::System,
        _ => return None,
    };
    let at = Seq(u64::from_le_bytes([
        bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
    ]));
    let head = Seq(u64::from_le_bytes([
        bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
    ]));
    if at.0 > head.0 {
        return None;
    }
    Some((origin, Cursor { at }, head))
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
    /// rewriting the tail of a long surface costs O(log lines) id comparisons.
    #[test]
    fn narrow_tail_line_operations_do_logarithmic_work() {
        const LINES: usize = 4096;
        let mut s = Surface::new();
        for i in 0..LINES {
            let text = if i + 1 == LINES {
                "tail needle"
            } else {
                "prefix"
            };
            s.apply(&write_cap(), Edit::AppendLine(text.into()));
        }
        let tail = Range {
            start: LineId((LINES - 1) as u64),
            end: LineId(LINES as u64),
        };

        let _ = take_line_id_comparisons();
        assert_eq!(s.read_text(&read_cap(), tail).text, "tail needle\n");
        let read_comparisons = take_line_id_comparisons();
        assert!(read_comparisons > 0, "read must reach the lower bound");
        assert!(
            read_comparisons <= 2 * usize::BITS as usize,
            "tail read made {read_comparisons} id comparisons for {LINES} lines"
        );

        s.apply(
            &write_cap(),
            Edit::SetLine(LineId((LINES - 1) as u64), "updated".into()),
        );
        let lookup_comparisons = take_line_id_comparisons();
        assert!(lookup_comparisons > 0, "SetLine must reach ordered lookup");
        assert!(
            lookup_comparisons <= usize::BITS as usize,
            "tail SetLine made {lookup_comparisons} id comparisons for {LINES} lines"
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

    #[test]
    fn subscribe_pulls_new_events_then_drains() {
        let mut s = Surface::new();
        let cur = s.subscribe(&read_cap()); // positioned at head (seq 0)
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
        // caught up: empty, not a gap
        let (upd, _cur) = s.poll(cur);
        assert_eq!(upd, SubUpdate::Events(vec![]));
    }

    #[test]
    fn event_log_ring_does_not_double_past_max() {
        // The ring must settle at capacity MAX_LOG_EVENTS + 1, not double.
        let mut log = EventLog::default();
        let mut max_cap_seen = 0usize;
        for _ in 0..(MAX_LOG_EVENTS + 4096) {
            let (_seq, _evicted) = log.append_at(Op::Resize { rows: 0, cols: 0 }, Ticks(0));
            max_cap_seen = max_cap_seen.max(log.ring.capacity());
        }
        assert_eq!(log.ring.len(), MAX_LOG_EVENTS, "ring stays bounded");
        assert!(
            log.ring.capacity() <= MAX_LOG_EVENTS + 1,
            "no doubling: settled cap {} must be <= {}",
            log.ring.capacity(),
            MAX_LOG_EVENTS + 1
        );
        assert!(
            max_cap_seen <= MAX_LOG_EVENTS + 1,
            "never doubled at any point: peak cap {} must be <= {}",
            max_cap_seen,
            MAX_LOG_EVENTS + 1
        );
    }

    /// `live_after`'s seek matches `live().filter(|e| e.seq > after)` at every
    /// watermark around an evicted ring, and `oldest_live`/`newest_live` are the
    /// ring's ends (`newest_live().ts` is the max over a monotone recording).
    #[test]
    fn live_after_seek_matches_the_filter_and_the_ends_are_the_extremes() {
        let mut log = EventLog::default();
        // Past the cap, with nondecreasing ts as one monotone clock produces.
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
    fn transact_is_atomic_and_cc_guarded() {
        let mut s = Surface::new();
        s.apply(&write_cap(), Edit::AppendLine("base".into()));
        let snap = s.snapshot(&read_cap()); // base = Seq(1)

        // up-to-date base: the whole group lands atomically
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

        // stale base: optimistic CC conflicts and applies nothing
        let out2 = s.transact(&write_cap(), snap.at, vec![Edit::AppendLine("z".into())]);
        assert_eq!(out2, TxnOutcome::Conflict);
        assert_eq!(s.seq(), Seq(3), "conflict left the surface untouched");
    }

    #[test]
    fn spine_event_and_cursor_wire_roundtrip() {
        for op in [
            Op::Append(LineId(1)),
            Op::Write(LineId(2)),
            Op::Clear(LineId(3)),
            Op::RawIn(BlobId(4)),
            Op::Reply(BlobId(5)),
            Op::Resize { rows: 24, cols: 80 },
            Op::Keyframe(KeyframeId(7)),
        ] {
            let ev = Event {
                seq: Seq(42),
                op,
                ts: Ticks(99),
            };
            let w = encode_spine_event_wire(&ev);
            assert_eq!(decode_spine_event_wire(&w), Some(ev));
        }

        let cw = encode_spine_cursor_wire(OriginTag::Source, Cursor { at: Seq(5) }, Seq(10));
        let (origin, cur, head) = decode_spine_cursor_wire(&cw).expect("decode cursor");
        assert_eq!(origin, OriginTag::Source);
        assert_eq!(cur.at, Seq(5));
        assert_eq!(head, Seq(10));
        let invalid_cw =
            encode_spine_cursor_wire(OriginTag::Overlay, Cursor { at: Seq(20) }, Seq(10));
        assert!(decode_spine_cursor_wire(&invalid_cw).is_none());
    }
}
