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
#[path = "lib_tests.rs"]
mod tests;
