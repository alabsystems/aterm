// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm buffer-primitives kernel — the substrate of the 11-verb `BufferApi`
//! (ATERM_DESIGN §4), built bottom-up.
//!
//! This first slice lands the **CONSISTENCY core**: the single, bounded,
//! sequence-numbered event-log spine (§3.4) and the `apply`/`read_text`/
//! `resolve`/`snapshot` verbs over it. It deliberately stops short of the full
//! trait (read_image needs the Rasterizer, process needs world effects, spans &
//! transact are the next slices) so the freeze (§4, M1) lands piece by verified
//! piece rather than as a big bang.
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

use std::num::NonZeroU64;

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

/// The addressing root (§3.3): a Surface is the namespace every Addr lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceId(pub NonZeroU64);

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

/// Capability witnesses — passed BY REFERENCE; a verb with no matching cap is
/// unreachable, not "denied at runtime" (§4.3 clause 6, §5.4). These are
/// placeholder attenuations of a real capability; the sealed mint lands in
/// `aterm-cap` (§5.4).
#[derive(Clone, Copy, Debug)]
pub struct ReadCap;
#[derive(Clone, Copy, Debug)]
pub struct WriteCap;

/// A typed address rooted at the Surface (§3.3), minimal first slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr {
    Surface(SurfaceId),
    Line(SurfaceId, LineId),
    Cell(SurfaceId, LineId, u32),
}

/// `resolve` is TOTAL — every address resolves to a first-class status, never a
/// silently-wrong cell (§4.3 clause 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Resolved(LineId, u32),
    /// Below the scrollback horizon — gone, but never wrong.
    Evicted,
    /// Survived a width reflow; columns remapped.
    Reflowed,
    /// The live region it named was cleared/superseded.
    Invalidated,
}

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

/// A content/structure predicate for `query` (§4.2 READ). search/grep/hit-test
/// all compose over this one fold. First slice: substring match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Predicate {
    /// Lines whose text contains the needle.
    TextContains(String),
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
    /// The recorded logical instant (B.4.2 Clock domain). Surface text/span ops
    /// (the legacy spine) use `Ticks(0)`; temporal ops appended via
    /// [`EventLog::append_at`] carry the real recorded tick.
    pub ts: Ticks,
}

/// High-level op summary recorded on the log (§3.4). Span mutations ride the
/// SAME spine as cell edits — there is no second timeline (§4.3 clause 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Append(LineId),
    Write(LineId),
    Clear(LineId),
    SpanDefine(SpanId),
    SpanRestyle(SpanId),
    SpanDrop(SpanId),
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

/// STRUCTURE axis (§4.2): one anchored typed-span primitive. `mark` = zero-width,
/// `region` = styled, `block` = provenance-typed (§5.7), `media` = pixel-backed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Mark,
    Region,
    Block,
    Media,
}

/// A span's anchored extent over committed lines (half-open). A `Mark` is
/// zero-width (`start == end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub start: LineId,
    pub end: LineId,
}

/// Opaque, kind-specific span payload (a style id, a block label, a media handle).
/// First slice carries a small string; the typed variants land with the renderer.
pub type SpanPayload = String;

/// A first-class span id rooted at its Surface (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Span {
    id: SpanId,
    surface: SurfaceId,
    extent: Extent,
    kind: SpanKind,
    payload: SpanPayload,
}

/// The public view of a resolved span (§4.2 `span_resolve`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpan {
    pub id: SpanId,
    pub extent: Extent,
    pub kind: SpanKind,
    pub payload: SpanPayload,
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

    /// Append a Surface text/span op (the legacy spine). Records at `Ticks(0)`;
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
    id: SurfaceId,
    lines: Vec<(LineId, String)>,
    next_line: u64,
    log: EventLog,
    /// Spans are a SEPARATE decoration stream, not per-cell fields (§4.2).
    spans: Vec<Span>,
    next_span: u64,
}

impl Surface {
    pub fn new(id: SurfaceId) -> Self {
        Surface {
            id,
            lines: Vec::new(),
            next_line: 0,
            log: EventLog::default(),
            spans: Vec::new(),
            next_span: 0,
        }
    }

    pub fn id(&self) -> SurfaceId {
        self.id
    }
    pub fn log(&self) -> &EventLog {
        &self.log
    }
    /// The current head of the spine (§4.3 clause 1).
    pub fn seq(&self) -> Seq {
        self.log.head()
    }

    fn line_index(&self, id: LineId) -> Option<usize> {
        self.lines.iter().position(|(l, _)| *l == id)
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
                self.lines.push((id, text));
                self.log.append(Op::Append(id))
            }
            Edit::SetLine(id, text) => {
                if let Some(i) = self.line_index(id) {
                    self.lines[i].1 = text;
                }
                self.log.append(Op::Write(id))
            }
            Edit::ClearLine(id) => {
                if let Some(i) = self.line_index(id) {
                    self.lines[i].1.clear();
                }
                self.log.append(Op::Clear(id))
            }
        }
    }

    /// READ — text projection over a line range, carrying origin + the reflected
    /// Seq (§4.2 READ, §4.3 clause 4). First slice tags all output `Source`.
    pub fn read_text(&self, _c: &ReadCap, r: Range) -> TextWithOrigin {
        let mut out = String::new();
        for (id, text) in &self.lines {
            if *id >= r.start && *id < r.end {
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

    /// READ — content/structure fold (§4.2). Returns the addresses of committed
    /// lines satisfying the predicate, over a line range. search/grep compose
    /// over this single verb (§4.4).
    pub fn query(&self, _c: &ReadCap, r: Range, p: &Predicate) -> Vec<Addr> {
        // Spelled as an explicit loop + push (identical fold, identical order)
        // instead of filter/map/collect: the strict L0 gate cannot derive a
        // bound for `collect`'s bulk allocation and cannot lower the borrowing
        // closure aggregates, while this shape mirrors the proved `read_text`
        // loop (push growth is bounded by `self.lines.len()`).
        let mut out = Vec::new();
        for (id, text) in &self.lines {
            if *id >= r.start && *id < r.end {
                let hit = match p {
                    Predicate::TextContains(needle) => text.contains(needle.as_str()),
                };
                if hit {
                    out.push(Addr::Line(self.id, *id));
                }
            }
        }
        out
    }

    /// ADDRESS — TOTAL resolution: every address maps to a first-class status
    /// (§4.3 clause 3). Never returns a silently-wrong cell.
    pub fn resolve(&self, a: Addr) -> Resolution {
        match a {
            Addr::Surface(s) | Addr::Line(s, _) | Addr::Cell(s, _, _) if s != self.id => {
                Resolution::Invalidated
            }
            Addr::Surface(_) => Resolution::Resolved(LineId(0), 0),
            Addr::Line(_, id) | Addr::Cell(_, id, _) => match self.line_index(id) {
                Some(_) => {
                    let col = if let Addr::Cell(_, _, c) = a { c } else { 0 };
                    Resolution::Resolved(id, col)
                }
                // A LineId below our first live line was evicted; above is not-yet.
                None if id.0 < self.first_line_id().map_or(0, |l| l.0) => Resolution::Evicted,
                None => Resolution::Invalidated,
            },
        }
    }

    fn first_line_id(&self) -> Option<LineId> {
        self.lines.first().map(|(l, _)| *l)
    }

    /// COMPOSE — O(1)-COW snapshot (§4.2). First slice is a cheap clone behind a
    /// SnapshotId; the structural-sharing COW lands with the grid integration.
    pub fn snapshot(&self, _c: &ReadCap) -> Snapshot {
        Snapshot {
            at: self.seq(),
            surface: self.clone(),
        }
    }

    // ===== STRUCTURE ===== one anchored typed-span primitive (§4.2).

    /// Define a span; rides the spine like any mutation. Returns its stable id.
    pub fn span_define(
        &mut self,
        _c: &WriteCap,
        extent: Extent,
        kind: SpanKind,
        payload: SpanPayload,
    ) -> SpanId {
        let id = SpanId(self.next_span);
        // Monotone id counter: 2^64 span definitions are unreachable, so the
        // saturation can never fire on a real path (same L0 idiom as
        // `next_line` in `apply` and `total` in `EventLog::append_at`).
        self.next_span = self.next_span.saturating_add(1);
        self.spans.push(Span {
            id,
            surface: self.id,
            extent,
            kind,
            payload,
        });
        self.log.append(Op::SpanDefine(id));
        id
    }

    /// Resolve a span to its public view, or `None` if dropped/unknown.
    pub fn span_resolve(&self, _c: &ReadCap, id: SpanId) -> Option<ResolvedSpan> {
        self.spans
            .iter()
            .find(|s| s.id == id)
            .map(|s| ResolvedSpan {
                id: s.id,
                extent: s.extent,
                kind: s.kind,
                payload: s.payload.clone(),
            })
    }

    /// Query spans of a kind overlapping a line range (§4.2). The span/query fold
    /// search/hit-test/overlap all compose over this.
    pub fn span_query(&self, _c: &ReadCap, r: Range, kind: SpanKind) -> Vec<SpanId> {
        // Explicit loop + push (identical fold, identical order) for the same
        // strict-gate reasons as `query`: no `collect` bulk-allocation
        // recognizer, no closure aggregates to lower.
        let mut out = Vec::new();
        for s in &self.spans {
            if s.kind == kind && Self::overlaps(s.extent, r) {
                out.push(s.id);
            }
        }
        out
    }

    fn overlaps(e: Extent, r: Range) -> bool {
        if e.start == e.end {
            // zero-width mark: overlaps iff its point falls in [r.start, r.end)
            e.start >= r.start && e.start < r.end
        } else {
            e.start < r.end && e.end > r.start
        }
    }

    /// Restyle a span in place (no id change); rides the spine.
    pub fn span_restyle(&mut self, _c: &WriteCap, id: SpanId, payload: SpanPayload) {
        if let Some(s) = self.spans.iter_mut().find(|s| s.id == id) {
            s.payload = payload;
            self.log.append(Op::SpanRestyle(id));
        }
    }

    /// Drop a span; rides the spine.
    pub fn span_drop(&mut self, _c: &WriteCap, id: SpanId) {
        if let Some(i) = self.spans.iter().position(|s| s.id == id) {
            self.spans.remove(i);
            self.log.append(Op::SpanDrop(id));
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
        // Explicit loop + push (identical filter, identical order) instead of
        // filter/cloned/collect — the strict gate cannot bound `collect`'s
        // bulk allocation (push growth is bounded by the live ring's length).
        let mut events: Vec<Event> = Vec::new();
        for e in self.log.live() {
            if e.seq.0 > cursor.at.0 {
                events.push(e.clone());
            }
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
    /// `branch` — a writable COW fork of the snapshot (§4.2). The fork is an
    /// independent Surface under a fresh id.
    pub fn branch(&self, new_id: SurfaceId) -> Surface {
        let mut s = self.surface.clone();
        s.id = new_id;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SurfaceId {
        SurfaceId(NonZeroU64::new(n).unwrap())
    }

    /// THE kernel invariant (§4.3 clause 1), the executable twin of
    /// `Kernel.tla`'s `SeqIsLen` + `Monotonic`: every apply bumps seq by exactly
    /// one, the spine is gap-free, and seq == total events appended.
    #[test]
    fn event_log_is_gap_free_and_monotonic() {
        let mut s = Surface::new(sid(1));
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
        let mut s = Surface::new(sid(1));
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
    fn resolve_is_total() {
        let mut s = Surface::new(sid(1));
        s.apply(&WriteCap, Edit::AppendLine("x".into()));
        // A live line resolves.
        assert!(matches!(
            s.resolve(Addr::Line(sid(1), LineId(0))),
            Resolution::Resolved(..)
        ));
        // A wrong surface never silently succeeds.
        assert_eq!(
            s.resolve(Addr::Line(sid(2), LineId(0))),
            Resolution::Invalidated
        );
        // A not-yet line is a first-class status, never a wrong cell.
        assert!(matches!(
            s.resolve(Addr::Line(sid(1), LineId(99))),
            Resolution::Invalidated | Resolution::Evicted
        ));
    }

    #[test]
    fn snapshot_is_isolated_from_later_writes() {
        let mut s = Surface::new(sid(1));
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
    fn span_lifecycle_rides_the_one_spine() {
        let mut s = Surface::new(sid(1));
        for i in 0..5 {
            s.apply(&WriteCap, Edit::AppendLine(format!("l{i}")));
        }
        let before = s.seq().0;
        let id = s.span_define(
            &WriteCap,
            Extent {
                start: LineId(1),
                end: LineId(3),
            },
            SpanKind::Region,
            "bold".into(),
        );
        // STRUCTURE mutations ride the SAME spine (§4.3 clause 1).
        assert_eq!(s.seq().0, before + 1);

        let rs = s.span_resolve(&ReadCap, id).unwrap();
        assert_eq!(rs.kind, SpanKind::Region);
        assert_eq!(rs.payload, "bold");

        // query by kind + range overlap
        assert_eq!(
            s.span_query(
                &ReadCap,
                Range {
                    start: LineId(0),
                    end: LineId(2)
                },
                SpanKind::Region
            ),
            vec![id]
        );
        assert!(
            s.span_query(
                &ReadCap,
                Range {
                    start: LineId(0),
                    end: LineId(2)
                },
                SpanKind::Block
            )
            .is_empty(),
            "wrong kind"
        );
        assert!(
            s.span_query(
                &ReadCap,
                Range {
                    start: LineId(3),
                    end: LineId(5)
                },
                SpanKind::Region
            )
            .is_empty(),
            "non-overlapping range"
        );

        s.span_restyle(&WriteCap, id, "italic".into());
        assert_eq!(s.span_resolve(&ReadCap, id).unwrap().payload, "italic");

        s.span_drop(&WriteCap, id);
        assert!(s.span_resolve(&ReadCap, id).is_none());
    }

    #[test]
    fn subscribe_pulls_new_events_then_drains() {
        let mut s = Surface::new(sid(1));
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
        let mut s = Surface::new(sid(1));
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
    fn query_folds_content_to_addresses() {
        let mut s = Surface::new(sid(1));
        s.apply(&WriteCap, Edit::AppendLine("error: boom".into()));
        s.apply(&WriteCap, Edit::AppendLine("all good".into()));
        s.apply(&WriteCap, Edit::AppendLine("error: again".into()));
        let hits = s.query(
            &ReadCap,
            Range {
                start: LineId(0),
                end: LineId(9),
            },
            &Predicate::TextContains("error".into()),
        );
        assert_eq!(
            hits,
            vec![Addr::Line(sid(1), LineId(0)), Addr::Line(sid(1), LineId(2))]
        );
    }

    #[test]
    fn transact_is_atomic_and_cc_guarded() {
        let mut s = Surface::new(sid(1));
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
