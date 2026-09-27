// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm buffer-primitives kernel — the substrate of the 11-verb `BufferApi`
//! (ATERM_DESIGN §4), built bottom-up.
//!
//! This first slice lands the **CONSISTENCY core**: the single, bounded,
//! sequence-numbered event-log spine (§3.4) and the `apply`/`read_text`/
//! `snapshot`/`subscribe`/`transact` verbs over it. It deliberately stops short
//! of the full trait (read_image needs the Rasterizer, process needs world
//! effects; addressing and spans ship with the slice that has a consumer) so
//! the freeze (§4, M1) lands piece by verified piece rather than as a big bang.
//!
//! Invariant proven here AND model-checked by the DERIVED kernel-family twins in
//! `aterm-spec::derive` (Kernel/Subscribe/Snapshot/Transact/Ring — exhaustively
//! `ty check`ed in `aterm-spec/tests/derived_ring_ty.rs`; they cover the spine,
//! poll, snapshot isolation, transact OCC, and ring eviction). The hand-written
//! `.tla` originals are quarantined under `aterm-spec-models/specs/legacy/`
//! (TRUST_NATIVE_TLA Phase 1 — superseded by the derived, drift-free twins):
//! the event log is a **gap-free, strictly-monotonic spine** — every `apply`
//! yields exactly one new `Seq`, and `seq == log.len()` always (§4.3 clause 1).
//!
//! STATUS: per §0.1 — designed-for-verification; the Trust contracts and the
//! `aterm-buffer` TLA+ ledger (§6.2) are not yet green. This is tested, not proven.

#![forbid(unsafe_code)]

use std::sync::Arc;

#[cfg(test)]
std::thread_local! {
    /// Comparisons performed by the ordered line-id lower bound. Unit tests use
    /// this as a deterministic work counter; thread-local keeps parallel tests
    /// independent.
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

/// A monotonic position on the one event-log spine (§4.3 clause 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// A recorded logical instant (the B.4.2 Clock tick domain — see
/// `HIERARCHICAL_SESSIONS.md` Addendum B). Every temporal [`Event`] carries the
/// tick it was recorded at, so replay can re-seed the engine clock deterministically
/// instead of reading wall time. Monotone but NOT required gap-free (unlike `Seq`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ticks(pub u64);

/// A handle into the out-of-band blob store (B.9). Raw input and reply payloads
/// are bulk bytes that must NOT bloat the in-RAM spine, so the spine carries a
/// `BlobId` handle and the bytes live in the tiered blob store the host owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(pub u64);

/// A handle into the keyframe store (B.9 / B.3.2). A keyframe is a serialized
/// `TerminalCheckpoint`; the spine references it by id so a scrub can seek to the
/// nearest keyframe `<= seq(t)` and replay forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyframeId(pub u64);

/// A committed-line identity, stable across scroll/eviction (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

/// Where a byte/cell came from — carried on every read so prompt injection is
/// legible and never silently trusted (§4.3 clause 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginTag {
    /// Authoritative output from the surface's owning Source (e.g. the PTY child).
    Source,
    /// An ephemeral overlay written by a non-owner (§3.2).
    Overlay,
    /// Engine-synthesized (banner, status).
    System,
}

/// Capability witnesses — passed BY REFERENCE (§4.3 clause 6, §5.4). TODAY
/// THEY ARE PLACEHOLDERS WITH NO TEETH: public unit structs any caller can
/// construct, which the verbs ignore, so a missing cap is NOT yet unreachable.
/// `aterm-cap`'s sealed mint has landed (§5.4); wiring these to
/// `aterm_cap::Cap<E>` is the step `docs/design/P1_KERNEL_MIGRATION.md` §1b
/// still owes. The signatures are already cap-by-reference, so that swap is
/// non-breaking.
#[derive(Clone, Copy, Debug)]
pub struct ReadCap;
#[derive(Clone, Copy, Debug)]
pub struct WriteCap;

/// A half-open line range `[start, end)` for reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: LineId,
    pub end: LineId,
}

/// The CLOSED edit algebra — the single `apply` verb's argument (§4.2 MUTATE).
/// Small and total so the screen↔logical addressing proof can't regress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Append a new committed line of text.
    AppendLine(String),
    /// Replace the text of an existing committed line.
    SetLine(LineId, String),
    /// Clear a committed line to empty (kept, so its LineId survives — §3.3).
    ClearLine(LineId),
}

/// The outcome of a `transact` (§4.2 COMPOSE): an atomic, isolated apply-group
/// under optimistic concurrency control over a base snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxnOutcome {
    /// All edits applied atomically; carries the resulting head Seq.
    Committed(Seq),
    /// The surface moved past the base snapshot — nothing applied; caller retries.
    Conflict,
}

/// One entry on the event-log spine: a coalesced high-level op, not a per-cell
/// delta (§3.4). Carries the monotone Seq it was assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: Seq,
    pub op: Op,
    /// The recorded logical instant (B.4.2 Clock domain). Surface text ops
    /// (the legacy spine) use `Ticks(0)`; temporal ops appended via
    /// [`EventLog::append_at`] carry the real recorded tick.
    pub ts: Ticks,
}

/// High-level op summary recorded on the log (§3.4) — the one spine every
/// mutation rides (§4.3 clause 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Append(LineId),
    Write(LineId),
    Clear(LineId),
    // --- temporal recording (B.9): handles, not payloads. The bulk bytes live
    // in the host-owned blob/keyframe stores; the spine stays small. ---
    /// Raw PTY input bytes were fed to the engine (`process`); payload in the
    /// blob store under this [`BlobId`].
    RawIn(BlobId),
    /// The engine emitted reply bytes to the PTY peer (DSR/DA/…); payload in the
    /// blob store. Recorded for fork-timeline fidelity; NOT re-emitted on replay.
    Reply(BlobId),
    /// The terminal was resized to `rows`×`cols` (reflow is path-dependent, so
    /// resize is a recorded event, never re-ordered — B.2.3).
    Resize {
        rows: u16,
        cols: u16,
    },
    /// A keyframe (serialized `TerminalCheckpoint`) was taken at a parser-ground
    /// boundary (B.3.3); referenced by [`KeyframeId`] in the keyframe store.
    Keyframe(KeyframeId),
}

/// A subscription cursor — the synchronous read-face of the event log (§3.4).
/// The reader pulls new events since `at`; if it has fallen behind the ring's
/// horizon it gets a `Gap` and must re-pull via `read_text` to resync. A slow
/// subscriber NEVER blocks the writer.
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

/// Hard ceilings (§3.4): the log is bounded; old events behind the horizon are
/// reclaimed. `seq` keeps counting; the ring just forgets the oldest entries.
pub const MAX_LOG_EVENTS: usize = 1 << 16;

/// The bounded, append-only, sequence-numbered ring — THE only change timeline.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    ring: std::collections::VecDeque<Event>,
    /// Total events ever appended (monotone; the spine's logical length).
    total: u64,
}

impl EventLog {
    /// Append a temporal op at a recorded tick (B.9). Returns the assigned `Seq`
    /// and, when the bounded ring was already full, the **evicted oldest event**.
    ///
    /// That returned event is the **spill seam** (B.8.2 `tier_residency_model`'s
    /// `NoSilentLoss` obligation): the caller MUST tier it (warm/cold) rather than
    /// drop it, so an evicted recorded event stays recoverable. `aterm-buffer` is
    /// headless and does no fs — it hands the event back; the host (gui /
    /// sessiond) owns the tiered persistence. Keeping the callback OUT of the log
    /// preserves `EventLog: Clone + Debug + Default`.
    #[must_use = "the evicted event is the spill seam — tier it (B.8.2), do not drop it"]
    pub fn append_at(&mut self, op: Op, ts: Ticks) -> (Seq, Option<Event>) {
        self.total = self.total.saturating_add(1);
        let seq = Seq(self.total);
        // Ring steady-state fits in `MAX_LOG_EVENTS + 1` slots (push, then evict
        // one). A `VecDeque` that grew by doubling would land at `2 * MAX_LOG_EVENTS`
        // and stay there (pop_front never shrinks), wasting ~2 MiB per full ring.
        // Reserve the single extra slot exactly once, right at the boundary, so the
        // backing store settles at `MAX_LOG_EVENTS + 1` — no eager allocation for
        // small/idle rings, no doubling for busy ones.
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

    /// Append a Surface text op (the legacy spine). Records at `Ticks(0)`;
    /// eviction follows the existing ring contract (the oldest entry is forgotten
    /// — the Surface/Snapshot text arm has its own scrollback). Temporal recording
    /// callers use [`append_at`](Self::append_at) and handle the spill seam.
    fn append(&mut self, op: Op) -> Seq {
        // Legacy ops deliberately drop on eviction (existing behavior); the
        // evicted return is for temporal callers via append_at.
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

    /// The OLDEST live event (the ring's low-water), or `None` when nothing is
    /// retained. O(1).
    ///
    /// It is the reachability oracle for a handle held OUTSIDE the ring. The
    /// ring only ever pushes to the back and pops from the front, so the live
    /// seqs are the contiguous range `[oldest.seq, head]` — which means
    /// `seq >= oldest_live().seq` is EXACTLY "this event is still on the spine".
    /// A keyframe store that outlives the ring window (temporal recording keeps
    /// its checkpoints in a separate bounded deque) has to answer that question
    /// before it may seed a replay from one, or it would fold a forward chain
    /// whose head has already been evicted and hand back a silently wrong
    /// engine.
    pub fn oldest_live(&self) -> Option<&Event> {
        self.ring.front()
    }

    /// The NEWEST live event, or `None` when nothing is retained. O(1).
    ///
    /// Callers that record with [`append_at`](Self::append_at) from one
    /// monotone clock get a nondecreasing `ts` down the ring, so this is also
    /// the LATEST RECORDED INSTANT — a `max` fold over `live()` is the same
    /// answer at `MAX_LOG_EVENTS` times the cost.
    pub fn newest_live(&self) -> Option<&Event> {
        self.ring.back()
    }

    /// Live events with `seq > after`, oldest-first — the same suffix
    /// `live().filter(|e| e.seq > after)` yields, reached by an O(1) SEEK
    /// instead of a walk over the whole retained ring.
    ///
    /// Sound because the live seqs are contiguous and ascending (push-back /
    /// pop-front only, one seq per append): the first event past `after` sits at
    /// offset `after + 1 - oldest.seq`, clamped into `[0, len]` so a watermark
    /// below the low-water yields EVERYTHING (not a panic) and one at or above
    /// the head yields nothing.
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

/// A Surface: the addressing root holding committed lines + the event-log spine
/// (§3.1). First slice models a line as text; the cell/style model integrates
/// `aterm-grid` in the next slice.
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
    /// The current head of the spine (§4.3 clause 1).
    pub fn seq(&self) -> Seq {
        self.log.head()
    }

    /// First ordered line whose id is not less than `id`.
    ///
    /// Line ids are appended monotonically and never reordered. Keeping the
    /// lower bound explicit avoids a borrowing closure in the Trust lane while
    /// reducing lookup from a prefix scan to O(log lines).
    fn line_lower_bound(&self, id: LineId) -> usize {
        let mut low = 0usize;
        let mut high = self.lines.len();
        while low < high {
            let mid = low + (high - low) / 2;
            #[cfg(test)]
            LINE_ID_COMPARISONS.with(|count| count.set(count.get().saturating_add(1)));
            // `mid < high <= len` on every reachable iteration. The total
            // access spelling carries that bound for Trust; returning the
            // current insertion point only handles the impossible miss.
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

    /// MUTATE — the one buffer-edit verb. Returns the monotone Seq it was
    /// assigned. Reversible (buffer-only). (§4.2)
    pub fn apply(&mut self, _c: &WriteCap, e: Edit) -> Seq {
        match e {
            Edit::AppendLine(text) => {
                let id = LineId(self.next_line);
                // Monotone id counter: 2^64 appends are unreachable, so the
                // saturation can never fire on a real path — it just spells the
                // increment in the provably non-overflowing form the strict L0
                // gate accepts (same idiom as the `total` counter in
                // `EventLog::append_at`).
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

    /// READ — text projection over a line range, carrying origin + the reflected
    /// Seq (§4.2 READ, §4.3 clause 4). First slice tags all output `Source`.
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

    /// COMPOSE — O(1)-COW snapshot (§4.2). The immutable line and event-log
    /// spines are shared; the first corresponding mutation detaches only that
    /// spine via [`Arc::make_mut`].
    pub fn snapshot(&self, _c: &ReadCap) -> Snapshot {
        Snapshot {
            at: self.seq(),
            surface: self.clone(),
        }
    }

    // ===== READ (subscribe) ===== the event log's read-face (§3.4).

    /// Open a subscription cursor positioned at the current head: it will see
    /// events appended AFTER this point.
    pub fn subscribe(&self, _c: &ReadCap) -> Cursor {
        Cursor { at: self.seq() }
    }

    /// Pull what's new since the cursor. Returns the update and the advanced
    /// cursor. A cursor that fell behind the live ring's horizon gets a `Gap`
    /// (it must re-pull via `read_text`); the writer is never blocked.
    #[allow(
        clippy::collapsible_if,
        reason = "the nested `if let` + inner `if` keeps poll's MIR fully lowerable for the strict Trust gate; the collapsed `&&` form is not (see comment inside)"
    )]
    pub fn poll(&self, cursor: Cursor) -> (SubUpdate, Cursor) {
        let head = self.seq();
        // Did we fall behind the ring? The oldest still-live event has a seq; if
        // it is newer than cursor.at + 1, events in between were evicted.
        // Spelled `if let` + saturating_add for the strict L0 gate: cursor.at
        // is a Seq off the spine, whose `total` counter itself saturates at
        // u64::MAX, so the `+ 1` here can never actually overflow (and no live
        // seq can exceed u64::MAX to out-compare the saturated sum anyway);
        // the closure-free shape also keeps poll's MIR fully lowerable.
        if let Some(oldest) = self.log.live().next() {
            if oldest.seq.0 > cursor.at.0.saturating_add(1) {
                return (SubUpdate::Gap { resync_to: head }, Cursor { at: head });
            }
        }
        // Seek directly to the first event after the cursor. A caught-up
        // subscriber is the common case; starting from `live()` would scan the
        // entire retained ring (up to `MAX_LOG_EVENTS`) just to return empty.
        // Keep the explicit loop + push instead of cloned/collect: the strict
        // gate can bound this growth by the live ring's length.
        let mut events: Vec<Event> = Vec::new();
        for e in self.log.live_after(cursor.at) {
            events.push(e.clone());
        }
        (SubUpdate::Events(events), Cursor { at: head })
    }

    /// COMPOSE — atomic, isolated apply-group under optimistic CC over a base
    /// snapshot seq (§4.2). Subsumes single-op CAS and frozen-world act: the
    /// editor undo unit, the multi-cursor atomic edit, and the harness scripted
    /// step are all `transact`. Commits iff the surface has not advanced past
    /// `base`; otherwise nothing is applied and the caller retries.
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

/// A read result carrying provenance + the reflected Seq (§4.3 clause 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextWithOrigin {
    pub text: String,
    pub origin: OriginTag,
    pub seq: Seq,
}

/// A seq-anchored COW prefix of a Surface (§4.2 COMPOSE).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// THE kernel invariant (§4.3 clause 1), the executable twin of
    /// `Kernel.tla`'s `SeqIsLen` + `Monotonic`: every apply bumps seq by exactly
    /// one, the spine is gap-free, and seq == total events appended.
    #[test]
    fn event_log_is_gap_free_and_monotonic() {
        let mut s = Surface::new();
        assert_eq!(s.seq(), Seq(0));
        let mut prev = 0u64;
        for i in 0..1000 {
            let before = s.seq().0;
            s.apply(&WriteCap, Edit::AppendLine(format!("line {i}")));
            let now = s.seq().0;
            assert_eq!(now, before + 1, "each apply yields exactly one Seq");
            assert!(now > prev, "seq is strictly monotonic");
            prev = now;
            // Monotonic + gap-free: seq equals total events ever appended.
            assert_eq!(s.seq().0, s.log().total());
        }
        assert_eq!(s.seq(), Seq(1000));
    }

    #[test]
    fn apply_read_round_trips() {
        let mut s = Surface::new();
        s.apply(&WriteCap, Edit::AppendLine("hello".into()));
        s.apply(&WriteCap, Edit::AppendLine("world".into()));
        let got = s.read_text(
            &ReadCap,
            Range {
                start: LineId(0),
                end: LineId(2),
            },
        );
        assert_eq!(got.text, "hello\nworld\n");
        assert_eq!(got.origin, OriginTag::Source);
        assert_eq!(got.seq, Seq(2));
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let mut s = Surface::new();
        s.apply(&WriteCap, Edit::AppendLine("frozen".into()));
        let snap = s.snapshot(&ReadCap);
        s.apply(&WriteCap, Edit::AppendLine("after".into()));
        // The snapshot sees the frozen world; the live surface moved on.
        let snap_text = snap
            .read_text(
                &ReadCap,
                Range {
                    start: LineId(0),
                    end: LineId(9),
                },
            )
            .text;
        let live_text = s
            .read_text(
                &ReadCap,
                Range {
                    start: LineId(0),
                    end: LineId(9),
                },
            )
            .text;
        assert_eq!(snap_text, "frozen\n");
        assert_eq!(live_text, "frozen\nafter\n");
        assert_eq!(snap.at, Seq(1));
    }

    #[test]
    fn snapshot_shares_line_allocation_until_replaced() {
        let mut s = Surface::new();
        let original = String::from("frozen allocation");
        let original_ptr = original.as_ptr();
        let original_capacity = original.capacity();
        s.apply(&WriteCap, Edit::AppendLine(original));
        assert_eq!(s.lines[0].1.as_ptr(), original_ptr);
        assert_eq!(s.lines[0].1.capacity(), original_capacity);

        let snap = s.snapshot(&ReadCap);
        assert!(Arc::ptr_eq(&s.lines[0].1, &snap.surface.lines[0].1));

        let replacement = String::from("live replacement allocation");
        let replacement_ptr = replacement.as_ptr();
        let replacement_capacity = replacement.capacity();
        s.apply(&WriteCap, Edit::SetLine(LineId(0), replacement));
        assert_eq!(s.lines[0].1.as_ptr(), replacement_ptr);
        assert_eq!(s.lines[0].1.capacity(), replacement_capacity);
        assert!(!Arc::ptr_eq(&s.lines[0].1, &snap.surface.lines[0].1));
        assert_eq!(snap.surface.lines[0].1.as_str(), "frozen allocation");
    }

    /// COW scope: a snapshot — and a writable copy taken from it — share the
    /// line and log spines with the live surface until a line write, which
    /// detaches exactly those two on the writer and nothing on the readers.
    #[test]
    fn snapshot_shares_the_line_and_log_spines_until_a_line_write() {
        let mut live = Surface::new();
        live.apply(&WriteCap, Edit::AppendLine("frozen".into()));
        let snap = live.snapshot(&ReadCap);
        let fork = snap.surface.clone();
        for candidate in [&snap.surface, &fork] {
            assert!(Arc::ptr_eq(&live.lines, &candidate.lines));
            assert!(Arc::ptr_eq(&live.log, &candidate.log));
        }

        live.apply(&WriteCap, Edit::SetLine(LineId(0), "live".into()));
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
        source.apply(&WriteCap, Edit::AppendLine("line".into()));
        let snap = source.snapshot(&ReadCap);

        for edit in [
            Edit::SetLine(LineId(99), "absent".into()),
            Edit::ClearLine(LineId(99)),
        ] {
            let mut fork = snap.surface.clone();
            fork.apply(&WriteCap, edit);
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
        source.apply(&WriteCap, Edit::AppendLine("same".into()));
        source.apply(&WriteCap, Edit::AppendLine(String::new()));
        let snap = source.snapshot(&ReadCap);

        for edit in [
            Edit::SetLine(LineId(0), "same".into()),
            Edit::ClearLine(LineId(1)),
        ] {
            let mut fork = snap.surface.clone();
            fork.apply(&WriteCap, edit);
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
            s.apply(&WriteCap, Edit::AppendLine(text.into()));
        }
        let tail = Range {
            start: LineId((LINES - 1) as u64),
            end: LineId(LINES as u64),
        };

        let _ = take_line_id_comparisons();
        assert_eq!(s.read_text(&ReadCap, tail).text, "tail needle\n");
        let read_comparisons = take_line_id_comparisons();
        assert!(read_comparisons > 0, "read must reach the lower bound");
        assert!(
            read_comparisons <= 2 * usize::BITS as usize,
            "tail read made {read_comparisons} id comparisons for {LINES} lines"
        );

        s.apply(
            &WriteCap,
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
                &ReadCap,
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
        let cur = s.subscribe(&ReadCap); // positioned at head (seq 0)
        s.apply(&WriteCap, Edit::AppendLine("a".into()));
        s.apply(&WriteCap, Edit::AppendLine("b".into()));
        let (upd, cur) = s.poll(cur);
        match upd {
            SubUpdate::Events(ev) => {
                assert_eq!(ev.len(), 2);
                assert_eq!(ev[0].seq, Seq(1));
                assert_eq!(ev[1].seq, Seq(2));
            }
            other => panic!("expected events, got {other:?}"),
        }
        // caught up: empty, NOT a gap
        let (upd, _cur) = s.poll(cur);
        assert_eq!(upd, SubUpdate::Events(vec![]));
    }

    #[test]
    fn slow_subscriber_gets_a_gap_and_never_blocks() {
        let mut s = Surface::new();
        let cur = s.subscribe(&ReadCap); // at seq 0
        // overflow the bounded ring so the oldest live event is past the cursor
        for i in 0..(MAX_LOG_EVENTS + 8) {
            s.apply(&WriteCap, Edit::AppendLine(format!("{i}")));
        }
        let (upd, _cur) = s.poll(cur);
        assert!(
            matches!(upd, SubUpdate::Gap { .. }),
            "fell behind horizon -> gap, not block"
        );
    }

    #[test]
    fn event_log_ring_does_not_double_past_max() {
        // Fill well past MAX_LOG_EVENTS: the ring must settle at cap MAX_LOG_EVENTS+1
        // (targeted reserve at the eviction boundary) rather than doubling to ~2x.
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

    /// DIFFERENTIAL: the O(1) seek in `live_after` yields exactly what the
    /// linear `live().filter(|e| e.seq > after)` yields, at every watermark
    /// around a ring that HAS evicted — below the low-water (everything), on
    /// the low-water, in the middle, at the head, and past it (nothing). Plus
    /// the two O(1) ends: `oldest_live`/`newest_live` are the ring's low/high,
    /// and `newest_live().ts` is the `max` fold over a monotone-`ts` recording.
    #[test]
    fn live_after_seek_matches_the_filter_and_the_ends_are_the_extremes() {
        let mut log = EventLog::default();
        // Past the cap so the front has been evicted, with a nondecreasing ts —
        // the shape a temporal recorder produces from one monotone clock.
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
        s.apply(&WriteCap, Edit::AppendLine("base".into()));
        let snap = s.snapshot(&ReadCap); // base = Seq(1)

        // up-to-date base: the whole group lands atomically
        let out = s.transact(
            &WriteCap,
            snap.at,
            vec![Edit::AppendLine("a".into()), Edit::AppendLine("b".into())],
        );
        assert_eq!(out, TxnOutcome::Committed(Seq(3)));
        assert_eq!(
            s.read_text(
                &ReadCap,
                Range {
                    start: LineId(0),
                    end: LineId(9)
                }
            )
            .text,
            "base\na\nb\n"
        );

        // stale base: optimistic CC conflicts and applies NOTHING
        let out2 = s.transact(&WriteCap, snap.at, vec![Edit::AppendLine("z".into())]);
        assert_eq!(out2, TxnOutcome::Conflict);
        assert_eq!(s.seq(), Seq(3), "conflict left the surface untouched");
    }
}
