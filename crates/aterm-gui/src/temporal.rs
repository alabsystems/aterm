// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session **temporal recorder** — the GUI-side capture half of the
//! hydratable temporal buffer (design `HIERARCHICAL_SESSIONS.md` Addendum B, B.9).
//!
//! [`TemporalRecorder`] folds the live session into the `aterm-buffer` event-log
//! spine: a [`Keyframe`](aterm_buffer::Op::Keyframe) (a serialized
//! [`TerminalCheckpoint`]) plus the stream of
//! [`RawIn`](aterm_buffer::Op::RawIn)/[`Reply`](aterm_buffer::Op::Reply)/
//! [`Resize`](aterm_buffer::Op::Resize) events that drive the engine. Replaying
//! `hydrate(keyframe) + process(RawIn…up to t)` reconstructs the engine state at
//! any `t` — the property `recording_model` (B.8.3) proves and
//! `conformance_recording` (B.8.4) binds to the real engine.
//!
//! ## Discipline (mirrors [`CastRecorder`](crate::cast::CastRecorder))
//! - **Handles, not payloads on the spine.** The spine carries `BlobId`/
//!   `KeyframeId` handles; the bulk bytes live here in bounded side stores so the
//!   in-RAM ring stays small.
//! - **Spill-not-forget.** [`EventLog::append_at`] hands back the evicted oldest
//!   event; we move it to the warm `spilled` tier rather than drop it (the
//!   B.8.2 `tier_residency_model` obligation). The warm tier is itself bounded by
//!   a byte budget; anything dropped past the budget is **counted**
//!   ([`dropped_events`](Self::dropped_events)), never silently lost — the cold
//!   (disk) drain that would make the budget unnecessary is the off-lock
//!   persistence task (a documented follow-up, not this headless unit).
//! - **No fs / no lock / no wall-clock-as-state.** Ticks come from one epoch
//!   captured at construction; the GUI feeds bursts off the reader hot path on a
//!   dedicated writer thread, exactly as the asciicast tap does.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use aterm_buffer::{BlobId, Event, EventLog, KeyframeId, Op, Seq, Ticks};
use aterm_core::terminal::{HostBindings, Terminal, TerminalCheckpoint};

/// Default byte budget for retained blob payloads + warm-tier events. A flood
/// cannot balloon RAM past this; an idle session costs nothing.
pub const DEFAULT_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// A burst handed from the reader hot path to the temporal writer thread
/// (lock-free, FIFO — mirrors the asciicast `Vec<u8>` channel). Recording the
/// tick + spine append happens on the writer thread, never under `term_lock`.
pub enum TemporalMsg {
    /// Raw PTY input fed to `process()` (the engine-driving bytes). `Arc<[u8]>` so
    /// the reader's single per-burst heap copy is shared with the asciicast tap.
    RawIn(Arc<[u8]>),
    /// Engine reply bytes emitted to the PTY peer (`take_response()`). `Arc<[u8]>`
    /// so the reader's single allocation is shared with the sink write.
    Reply(Arc<[u8]>),
    /// A geometry change (DECSET/window resize). Routed through this SAME FIFO —
    /// enqueued UNDER `term_lock` at the engine resize, exactly like the reader's
    /// per-chunk `RawIn` — so the writer thread APPENDS it to the spine in the order
    /// the engine observed it, never ahead of already-processed input (B.2.3: reflow
    /// is path-dependent, so a mid-print resize must replay in the right place).
    Resize { rows: u16, cols: u16 },
}

/// A retained blob payload (the bytes behind a `RawIn`/`Reply` handle). Held in a
/// FIFO `VecDeque` (oldest-first); the owning `BlobId` is the queue position, so it
/// is not stored here — handle→bytes resolution will reintroduce it with its reader.
/// `Arc<[u8]>` retains the reader's shared burst allocation directly (no re-copy);
/// eviction drops this ref, and the heap is reclaimed once sibling taps drop theirs.
struct Blob {
    bytes: Arc<[u8]>,
}

/// Per-session capture into the `aterm-buffer` temporal spine.
pub struct TemporalRecorder {
    /// The event-log spine (the one timeline). Bounded ring; eviction spills.
    log: EventLog,
    /// Bulk payloads for `RawIn`/`Reply`, keyed by `BlobId`, oldest first.
    blobs: VecDeque<Blob>,
    /// Keyframes (serialized checkpoints), oldest first, at most
    /// [`MAX_KEYFRAMES`] retained.
    ///
    /// Each entry carries the SPINE COORDINATES its `Op::Keyframe` event was
    /// appended at — `(seq, ts)` — not just the checkpoint. That is what lets
    /// `replay_at` pick its base by looking at this four-entry deque instead of
    /// re-deriving it by scanning up to `MAX_LOG_EVENTS` = 65,536 spine events
    /// for the last `Op::Keyframe`. The coordinates cannot drift from the spine
    /// because they are the exact values `append` returned when that very event
    /// was recorded.
    keyframes: VecDeque<RetainedKeyframe>,
    /// Warm tier: events evicted from the live ring (spill-not-forget). In a full
    /// deployment an off-lock task drains these to the cold/disk tier.
    spilled: VecDeque<Event>,
    /// Monotone blob-id source.
    next_blob: u64,
    /// Monotone keyframe-id source.
    next_keyframe: u64,
    /// Retained payload bytes (blobs + a fixed charge per spilled event).
    used: usize,
    /// The retained byte budget; drop-oldest (counted) when exceeded.
    budget: usize,
    /// Count of warm-tier events dropped past the budget (NEVER silent — the
    /// design's "no silent caps" rule). Zero once the cold drain is wired.
    dropped_events: u64,
    /// Bytes of `RawIn` recorded since the last keyframe. When it crosses the
    /// re-keyframe interval the recorder mints a FRESH keyframe (by replaying to the
    /// current instant), so a recent `[keyframe..latest]` window always survives
    /// budget eviction — without it a single t0 keyframe is orphaned the moment
    /// cumulative I/O exceeds the budget and replay dies permanently (B.8.2 R19).
    bytes_since_keyframe: usize,
    /// The monotonic epoch this recorder's tick timeline is relative to.
    epoch: Instant,
}

impl TemporalRecorder {
    /// A recorder with the default budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }

    /// A recorder with an explicit retained-byte budget (>= 1).
    #[must_use]
    pub fn with_budget(budget: usize) -> Self {
        Self {
            log: EventLog::default(),
            blobs: VecDeque::new(),
            keyframes: VecDeque::new(),
            spilled: VecDeque::new(),
            next_blob: 0,
            next_keyframe: 0,
            used: 0,
            budget: budget.max(1),
            dropped_events: 0,
            bytes_since_keyframe: 0,
            epoch: Instant::now(), // CLOCK-EXEMPT: recorder timeline epoch, not engine state
        }
    }

    /// The current tick on this recorder's timeline (micros since the epoch).
    /// Both the reader-thread (RawIn/Reply) and main-thread (Resize/Keyframe)
    /// taps call this so one session shares a single monotone tick timeline.
    #[must_use]
    pub fn now(&self) -> Ticks {
        // CLOCK-EXEMPT: derives the recorded tick from the recorder epoch; this
        // is the value we RECORD, not engine state read during process().
        Ticks(u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    /// Append `op` at `ts`, moving any evicted event to the warm tier (spill-not-
    /// forget), then enforce the byte budget (drop-oldest warm-tier events, counted).
    /// Returns the spine `Seq` the event was assigned — `record_keyframe` retains
    /// it beside the checkpoint so the base-keyframe lookup is a four-entry scan
    /// rather than a walk of the whole spine.
    fn append(&mut self, op: Op, ts: Ticks) -> Seq {
        let (seq, evicted) = self.log.append_at(op, ts);
        if let Some(ev) = evicted {
            // SPILL: tier the evicted event instead of dropping it (B.8.2).
            self.spilled.push_back(ev);
            self.used += SPILLED_EVENT_CHARGE;
        }
        self.enforce_budget();
        seq
    }

    /// Record a raw PTY-input burst fed to `process()` (the `RawIn` event). The
    /// bytes are the genuine engine-driving input — replay re-feeds exactly these.
    /// Borrowing convenience over [`record_raw_in_shared`](Self::record_raw_in_shared)
    /// for callers without a shared allocation in hand (one copy).
    pub fn record_raw_in(&mut self, bytes: &[u8]) {
        self.record_raw_in_shared(Arc::from(bytes));
    }

    /// [`record_raw_in`](Self::record_raw_in), retaining the caller's SHARED burst
    /// allocation directly (a refcount bump — no re-copy of the reader's bytes).
    pub fn record_raw_in_shared(&mut self, bytes: Arc<[u8]>) {
        let ts = self.now();
        self.bytes_since_keyframe += bytes.len();
        let id = self.store_blob(bytes);
        self.append(Op::RawIn(id), ts);
        // Mint a fresh keyframe once enough input has accrued, BEFORE cumulative I/O
        // can orphan the current keyframe's chain — the R19 reachable-window fix.
        self.maybe_rekeyframe();
    }

    /// Record an engine reply burst (`take_response()` -> PTY peer). Recorded for
    /// forked-timeline fidelity; NOT re-emitted on replay (the design's contract).
    /// Borrowing convenience over [`record_reply_shared`](Self::record_reply_shared).
    pub fn record_reply(&mut self, bytes: &[u8]) {
        self.record_reply_shared(Arc::from(bytes));
    }

    /// [`record_reply`](Self::record_reply), retaining the caller's shared
    /// allocation directly. Empty replies are a no-op (no spurious event).
    pub fn record_reply_shared(&mut self, bytes: Arc<[u8]>) {
        if bytes.is_empty() {
            return;
        }
        let ts = self.now();
        let id = self.store_blob(bytes);
        self.append(Op::Reply(id), ts);
    }

    /// Record a geometry change (reflow is path-dependent, so resize is a
    /// first-class recorded event, never re-ordered — B.2.3).
    pub fn record_resize(&mut self, rows: u16, cols: u16) {
        let ts = self.now();
        self.append(Op::Resize { rows, cols }, ts);
    }

    /// Record a keyframe (a serialized [`TerminalCheckpoint`] taken at a
    /// parser-ground boundary, B.3.3). Replay seeds from the nearest keyframe
    /// `<= seq(t)` and folds `RawIn` forward.
    pub fn record_keyframe(&mut self, checkpoint: TerminalCheckpoint) {
        let ts = self.now();
        let id = KeyframeId(self.next_keyframe);
        self.next_keyframe += 1;
        // A keyframe is large; charge its grid bytes against the budget.
        self.used += checkpoint.grid.len() + checkpoint.alt_grid.as_ref().map_or(0, Vec::len);
        // APPEND FIRST, then retain — the spine assigns the `Seq` and it is the
        // coordinate `replay_at` seeks by, so the retained entry is built from
        // the append's own return value and cannot disagree with the spine.
        let seq = self.append(Op::Keyframe(id), ts);
        self.keyframes.push_back(RetainedKeyframe {
            id,
            seq,
            ts,
            checkpoint,
        });
        // This keyframe is the fresh base: the forward chain restarts from here.
        self.bytes_since_keyframe = 0;
        // Cap the retained keyframes: without this, periodic re-keyframing lets
        // keyframes ACCUMULATE until they fill the budget, forcing `enforce_budget`
        // to evict the newest keyframe's forward-chain blobs — which breaks replay
        // for the very window the re-keyframe exists to keep. Keeping only the newest
        // few bounds keyframe budget use, leaving room for the chain; older instants
        // age out honestly (bounded retention). The newest keyframe is always kept.
        while self.keyframes.len() > MAX_KEYFRAMES {
            if let Some(kf) = self.keyframes.pop_front() {
                self.used = self.used.saturating_sub(kf.charge());
            }
        }
        self.enforce_budget();
    }

    /// Mint a FRESH keyframe by replaying to the CURRENT instant when enough input
    /// has accrued since the last one, so a recent `[keyframe..latest]` window
    /// survives budget eviction (R19). Replaying to the LATEST tick yields the
    /// current state (NOT a past state stamped `now()`), seeded from the newest
    /// keyframe + its still-live forward chain — so the new keyframe is faithful.
    /// `HostBindings::none()` reconstructs a fully-inspectable buffer whose grid
    /// matches the source (host callbacks are not checkpointed anyway). Interval
    /// floors at 64 KiB so a pathologically tiny budget never thrashes (and cannot
    /// retain a keyframe, so it honestly still degrades to `None`).
    fn maybe_rekeyframe(&mut self) {
        let interval = (self.budget / 4).max(64 * 1024);
        if self.bytes_since_keyframe < interval {
            return;
        }
        match self.replay_at(HostBindings::none(), None) {
            Some(mut live) => {
                // GUARD: only checkpoint a parser-GROUND engine. A recorded RawIn burst
                // is an arbitrary PTY read, so the burst that crossed the re-keyframe
                // interval can end MID-ESCAPE (a split CSI) — replaying it leaves the
                // parser non-ground. `checkpoint()` REQUIRES ground (it captures no
                // partial-sequence state): mid-sequence it `debug_assert`-panics the
                // recorder thread (killing recording for the session), and in release
                // records a keyframe whose parser is reset to Ground, so the pending
                // intermediate bytes are LOST and replay diverges. If not ground, DEFER:
                // leave `bytes_since_keyframe` untouched so the NEXT (ground-terminated)
                // burst re-keyframes one interval later — the window still survives.
                if !live.parser_is_ground() {
                    return;
                }
                // BOUND the keyframe's scrollback. `restore_grid` restores replayed
                // scrollback UNLIMITED (it must not silently drop history), so a
                // re-keyframe seeded from the prior keyframe would ACCUMULATE all
                // scrollback since t0 and bloat past the budget — defeating the fix.
                // Trim to a recent window so keyframes stay bounded; older scrollback
                // ages out honestly (the bounded-retention contract). The recent
                // `[keyframe..latest]` window an AI replays stays faithful.
                live.set_scrollback_line_limit(Some(REKEY_SCROLLBACK_LINES));
                self.record_keyframe(live.checkpoint());
            }
            // The chain was already broken (tiny budget): reset so we do not retry
            // the failing replay on every subsequent burst.
            None => self.bytes_since_keyframe = 0,
        }
    }

    /// Store `bytes` under a fresh `BlobId`, charging the budget (charge and
    /// refund are both the payload `len`, whether or not the alloc is shared).
    fn store_blob(&mut self, bytes: Arc<[u8]>) -> BlobId {
        let id = BlobId(self.next_blob);
        self.next_blob += 1;
        self.used += bytes.len();
        self.blobs.push_back(Blob { bytes });
        id
    }

    /// Drop oldest retained payloads (blobs, then warm-tier events, then oldest
    /// keyframes) until the budget holds. Every dropped warm-tier event is
    /// COUNTED in `dropped_events` — the recording is bounded but never silently
    /// loses without saying so.
    ///
    // PERIODIC RE-KEYFRAMING (R19, SHIPPED — IN THIS FILE): `maybe_rekeyframe`
    // (above, wired live from `record_raw_in_shared`) self-replays the LIVE engine to
    // the CURRENT instant and mints a fresh keyframe via `record_keyframe`, trimming
    // the replayed scrollback to a bounded window and capping retained keyframes. So
    // evicting oldest blobs here no longer kills replay: a reachable `[keyframe..
    // latest]` window always survives, and the newest keyframe's spine event stays
    // inside the live ring. (The naive t0-only design a prior comment here described —
    // where evicting the sole keyframe's forward chain made `replay_at` return None
    // after one build log — no longer holds; the self-replayed keyframe seeds the
    // `nearest keyframe <= at` fold correctly because it is stamped at its replay
    // instant, not `now()`.) The remaining follow-up is the COLD/on-disk tier for
    // deep history beyond the RAM budget, not the re-keyframing itself.
    fn enforce_budget(&mut self) {
        while self.used > self.budget {
            // Prefer dropping the oldest blob (largest, most reclaimable) first.
            if let Some(b) = self.blobs.pop_front() {
                self.used = self.used.saturating_sub(b.bytes.len());
                continue;
            }
            if let Some(_ev) = self.spilled.pop_front() {
                self.used = self.used.saturating_sub(SPILLED_EVENT_CHARGE);
                self.dropped_events += 1;
                continue;
            }
            if let Some(kf) = self.keyframes.pop_front() {
                self.used = self.used.saturating_sub(kf.charge());
                continue;
            }
            break; // nothing left to reclaim
        }
    }

    /// Total events ever appended to the spine (live + spilled + dropped).
    #[must_use]
    pub fn total_events(&self) -> u64 {
        self.log.total()
    }

    /// Live (un-evicted) event count on the spine.
    #[must_use]
    pub fn live_events(&self) -> usize {
        self.log.live().count()
    }

    /// Warm-tier (spilled-but-retained) event count.
    #[must_use]
    pub fn spilled_events(&self) -> usize {
        self.spilled.len()
    }

    /// Keyframes currently retained.
    #[must_use]
    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// Warm-tier events dropped past the budget (cold drain not yet wired).
    #[must_use]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// The latest recorded instant on the live spine (the default replay target),
    /// or `Ticks(0)` before any event.
    ///
    /// O(1): the newest live event's tick, not a `max` fold over the ring.
    /// SOUND because this recorder's spine is monotone in `ts` BY
    /// CONSTRUCTION — every append (`RawIn`, `Reply`, `Resize`, `Keyframe`)
    /// stamps `self.now()`, which is `epoch.elapsed()` off ONE `Instant`, and
    /// every append is serialized on the single temporal writer thread (the
    /// reader hands bursts over a FIFO, and `Resize` is enqueued on that SAME
    /// FIFO under `term_lock` precisely so the ordering holds). A monotone
    /// clock read in append order gives a nondecreasing `ts` down the ring, so
    /// `back()` IS the max — and it was being paid for with a walk over up to
    /// `MAX_LOG_EVENTS` = 65,536 events, on the default replay target, from
    /// inside the re-keyframe path that runs every 2 MiB of recorded input.
    #[must_use]
    pub fn latest_tick(&self) -> Ticks {
        self.log.newest_live().map(|e| e.ts).unwrap_or_default()
    }

    /// Resolve a `RawIn`/`Reply` handle to its retained bytes, or `None` if the
    /// blob has aged out of the bounded retention window. The blob deque is a
    /// contiguous FIFO window `[base, next_blob)` — eviction only pops the front —
    /// so the front blob's id is `next_blob - blobs.len()` and the handle resolves
    /// by offset from that base. Bounds-checked: an evicted (or not-yet-issued) id
    /// yields `None`, never a mis-indexed read after `pop_front` shifted positions.
    fn blob_bytes(&self, id: BlobId) -> Option<&[u8]> {
        let base = self.next_blob - self.blobs.len() as u64;
        let idx = usize::try_from(id.0.checked_sub(base)?).ok()?;
        self.blobs.get(idx).map(|b| &*b.bytes)
    }

    /// The base keyframe for a replay to instant `at`: the greatest-`seq`
    /// keyframe that is BOTH retained here AND still live on the spine, with
    /// `ts <= at`. `None` when no such keyframe exists (bounded retention won —
    /// the honest, lossy answer `replay_at` already contracts for).
    ///
    /// WHY THIS IS THE SAME ANSWER the old spine scan produced, in O(1) instead
    /// of O(`MAX_LOG_EVENTS`). The old code walked every live event, kept the
    /// last `Op::Keyframe` with `ts <= at`, and then REQUIRED that one to still
    /// be retained here (`?` on the checkpoint lookup) — it never fell back to
    /// an older one. Take `k0` = that event. Two cases, and they agree:
    ///
    ///  * `k0` is retained ⇒ it is in this deque, live, and `ts <= at`; and no
    ///    retained keyframe newer than `k0` can qualify, because a newer
    ///    keyframe is also live (the ring evicts oldest-first, so anything newer
    ///    than a live event is live) and `k0` was the greatest live qualifier —
    ///    so the newer ones all have `ts > at`. Same pick.
    ///  * `k0` is NOT retained ⇒ `k0` is older than every entry here (this deque
    ///    keeps the newest [`MAX_KEYFRAMES`]), so every entry here is live and,
    ///    by the same maximality argument, has `ts > at`. This search finds
    ///    nothing and returns `None` — which is exactly what the `?` on the old
    ///    checkpoint lookup did.
    ///
    /// LIVENESS IS NOT OPTIONAL. A keyframe can outlive its own spine event: the
    /// deque holds four, the ring holds 65,536 events, so a long enough run
    /// evicts the `Op::Keyframe` while the checkpoint is still here. Seeding a
    /// replay from such a keyframe would fold a forward chain missing everything
    /// between it and the ring's low-water and hand back a SILENTLY WRONG
    /// engine, so the liveness test (`seq >= oldest_live().seq`, sound because
    /// live seqs are contiguous) is a correctness guard, not an optimization.
    fn base_keyframe(&self, at: Ticks) -> Option<&RetainedKeyframe> {
        let floor = self.log.oldest_live()?.seq;
        self.keyframes
            .iter()
            .rev()
            .find(|kf| kf.ts <= at && kf.seq >= floor)
    }

    /// Reconstruct the engine state at logical instant `at` (default: the latest
    /// recorded instant) — the read half of the hydratable spine (B.9). Seeds a
    /// fresh headless [`Terminal`] from the nearest retained keyframe with
    /// `ts <= at`, then folds the recorded `RawIn`/`Resize` events forward in seq
    /// order through `process`/`resize`; `Reply` is NOT re-emitted (the design's
    /// contract) and intermediate keyframes are skipped (already seeded from base).
    ///
    /// Returns `None` when the target is unreachable under bounded retention: the
    /// base keyframe aged out, or a needed input blob was evicted. Degrades to
    /// `None` — never a panic or an out-of-bounds read (the B.8.2 lossy-but-honest
    /// contract). `host` rebinds host effects (B.3.2); this increment's
    /// [`HostBindings`] is empty, so a null set reconstructs a fully-inspectable
    /// buffer whose grid matches the source.
    #[must_use]
    pub fn replay_at(&self, host: HostBindings, at: Option<Ticks>) -> Option<Terminal> {
        let at = at.unwrap_or_else(|| self.latest_tick());
        // Base keyframe: O(MAX_KEYFRAMES) over the retained deque (which carries
        // each keyframe's own spine coordinates) instead of a full walk of the
        // spine looking for the last `Op::Keyframe` — see `base_keyframe` for
        // why the two pick the same entry.
        let base = self.base_keyframe(at)?;
        let mut term = Terminal::from_checkpoint(&base.checkpoint, host);
        // Fold every live event AFTER the seed with ts <= at, in seq order.
        //
        // TWO SCANS DELETED. The seek (`live_after`) starts at the base instead
        // of re-walking the ~65k events BEFORE it — and the base is by design a
        // RECENT keyframe, so that prefix was almost the whole ring. The tail
        // then BREAKS rather than `continue`s on `ts > at`: identical set of
        // folded events, because `ts` is nondecreasing down this recorder's
        // spine (one monotone clock, one serialized writer — see `latest_tick`),
        // so the first event past `at` proves every later one is too.
        for ev in self.log.live_after(base.seq) {
            if ev.ts > at {
                break;
            }
            match ev.op {
                Op::RawIn(id) => term.process(self.blob_bytes(id)?),
                Op::Resize { rows, cols } => term.resize(rows, cols),
                _ => {}
            }
        }
        Some(term)
    }
}

/// One retained keyframe: the serialized checkpoint PLUS the spine coordinates
/// of the `Op::Keyframe` event that announced it.
///
/// Keeping `(seq, ts)` here is what turns the base-keyframe lookup from a walk
/// of the whole 65k-event spine into a four-entry scan, and it costs 16 bytes
/// against a checkpoint that carries a whole serialized grid.
struct RetainedKeyframe {
    /// The spine handle this checkpoint is referenced by.
    #[allow(dead_code)] // the spine's own `Op::Keyframe(id)` is the wire form
    id: KeyframeId,
    /// The `Seq` its `Op::Keyframe` event was appended at — the fold's start
    /// cursor AND the liveness coordinate (`seq >= oldest_live().seq`).
    seq: Seq,
    /// The tick it was recorded at — compared against the replay target.
    ts: Ticks,
    /// The serialized engine state.
    checkpoint: TerminalCheckpoint,
}

impl RetainedKeyframe {
    /// The retained-byte charge this keyframe carries against the budget: its
    /// grid plus any alt grid. One definition, used by BOTH eviction paths (the
    /// `MAX_KEYFRAMES` trim and `enforce_budget`), which previously each
    /// open-coded the same sum and could have drifted.
    fn charge(&self) -> usize {
        self.checkpoint.grid.len() + self.checkpoint.alt_grid.as_ref().map_or(0, Vec::len)
    }
}

impl Default for TemporalRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed budget charge per warm-tier event (the `Event` struct itself; its
/// payload, if any, is charged separately as a blob). Keeps `enforce_budget`
/// O(1) per step without measuring each enum variant.
const SPILLED_EVENT_CHARGE: usize = 64;

/// The scrollback a re-keyframe retains (a recent window). Bounds keyframe size so
/// periodic re-keyframing cannot accumulate all scrollback since t0 (replayed
/// scrollback is restored unlimited); older history ages out under the byte budget,
/// while the recent `[keyframe..latest]` replay window stays faithful.
const REKEY_SCROLLBACK_LINES: usize = 2048;

/// The number of keyframes retained. The newest seeds a `[keyframe..latest]` replay;
/// the rest let recent PAST instants replay too. Bounded so accumulating keyframes
/// cannot fill the budget and starve the newest keyframe's forward-chain blobs.
const MAX_KEYFRAMES: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_raw_in_reply_resize_on_one_spine() {
        let mut r = TemporalRecorder::new();
        r.record_raw_in(b"ls -la\n");
        r.record_reply(b"\x1b[0n"); // a DSR reply
        r.record_resize(30, 100);
        // Empty reply is a no-op (no spurious event).
        r.record_reply(b"");

        assert_eq!(
            r.total_events(),
            3,
            "raw_in + reply + resize (empty reply skipped)"
        );
        assert_eq!(r.live_events(), 3);
        assert_eq!(r.spilled_events(), 0, "nothing evicted under capacity");
        assert_eq!(r.dropped_events(), 0);
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

    /// DIFFERENTIAL for the whole `replay_at` seek. The reference here is the
    /// EXACT pre-change body — full spine scan for the base, full spine scan for
    /// the fold, `continue` (not `break`) on `ts > at` — run against the same
    /// recorder, at every interesting instant: before the first keyframe, on and
    /// between recorded ticks, at the latest tick, and past it. The two must
    /// agree on reachability AND on the reconstructed screen text.
    ///
    /// It also pins the two invariants the seek rests on, so a future change
    /// that broke either would fail HERE rather than silently hand back a
    /// wrong-state engine: the spine's ticks are nondecreasing, and the retained
    /// keyframes' `(seq, ts)` coordinates agree with the spine events that
    /// announced them.
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

        // INVARIANT 1: nondecreasing ticks down the live spine (what turns the
        // `max` fold into `back()` and the `continue` into a `break`).
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

        // INVARIANT 2: retained keyframe coordinates agree with the spine.
        for kf in &r.keyframes {
            if let Some(ev) = r.log.live().find(|e| e.seq == kf.seq) {
                assert_eq!(
                    ev.op,
                    Op::Keyframe(kf.id),
                    "seq {:?} is not this keyframe",
                    kf.seq
                );
                assert_eq!(ev.ts, kf.ts, "retained ts disagrees with the spine");
            }
        }

        // The pre-change body, verbatim, as the oracle.
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
            let mut term = Terminal::from_checkpoint(cp, HostBindings::none());
            for ev in r.log.live() {
                if ev.seq <= base_seq || ev.ts > at {
                    continue;
                }
                match ev.op {
                    Op::RawIn(id) => term.process(r.blob_bytes(id)?),
                    Op::Resize { rows, cols } => term.resize(rows, cols),
                    _ => {}
                }
            }
            Some(screen_of(&term))
        };
        let observed = |at: Ticks| -> Option<Vec<String>> {
            r.replay_at(HostBindings::none(), Some(at))
                .map(|t| screen_of(&t))
        };

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
        // And the default target (`None`) still lands on the latest instant.
        assert_eq!(
            r.replay_at(HostBindings::none(), None)
                .map(|t| screen_of(&t)),
            reference(latest),
            "the default replay target must still be the latest recorded instant"
        );
    }

    /// The visible screen text of a reconstructed engine, for differential
    /// comparison (a `Terminal` is not `PartialEq`). Same row reader the
    /// control-plane `screen` verb and the subscribe push loop use.
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
        // The recorder retains the SAME allocation (refcount bump, no re-copy)...
        assert_eq!(Arc::strong_count(&burst), 2, "blob shares the caller's Arc");
        // ...charging exactly the payload len against the budget.
        assert_eq!(r.used, 64);
        // Flood past the budget: evicting the oldest blob drops the shared ref
        // and refunds the identical len.
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
        // Tiny budget: each blob is ~64 bytes, so blobs get reclaimed, but the
        // SPINE (total_events) keeps counting every append — bounding payload RAM
        // never rewrites history's length.
        let mut r = TemporalRecorder::with_budget(256);
        for _ in 0..1000 {
            r.record_raw_in(&[b'x'; 64]);
        }
        // The spine counted every event...
        assert_eq!(r.total_events(), 1000);
        // ...while retained payload bytes stayed bounded.
        assert!(
            r.used <= r.budget + 64,
            "payload bytes bounded: used={}",
            r.used
        );
        // Blobs were reclaimed under budget (far fewer than 1000 retained).
        assert!(r.blobs.len() < 1000);
    }

    #[test]
    fn replay_reconstructs_screen_from_keyframe_and_rawin() {
        let mut r = TemporalRecorder::new();
        // Seed a t0 keyframe of a blank, parser-ground engine, then feed input.
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        r.record_raw_in(b"seed\r\n");

        let term = r
            .replay_at(HostBindings::none(), None)
            .expect("base keyframe retained -> replay reconstructs");
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert!(row0.starts_with("seed"), "replayed row 0 = {row0:?}");
    }

    /// B.2.3: a resize is a first-class spine event and replay applies it in SPINE
    /// ORDER, so a cursor-addressed write BEFORE a resize replays at its PRE-resize
    /// column — never re-laid-out at the post-resize width. This is the invariant the
    /// under-`term_lock` FIFO enqueue (reader per-chunk `RawIn` + main-thread `Resize`)
    /// upholds: if a resize could jump ahead of already-processed input on the spine,
    /// the write would land at the wrong column on replay.
    #[test]
    fn replay_applies_resize_in_spine_order_after_cursor_addressed_write() {
        let mut r = TemporalRecorder::new();
        // Seed a 4x10 keyframe (parser-ground, blank).
        let blank = aterm_core::terminal::Terminal::new(4, 10);
        r.record_keyframe(blank.checkpoint());
        // Move to column 16 (1-based) and write X. At width 10 the cursor CLAMPS to
        // the last column (col 9, 0-based).
        r.record_raw_in(b"\x1b[16GX");
        // THEN widen to 20 columns (recorded AFTER the write — the spine order).
        r.record_resize(4, 20);

        let term = r.replay_at(HostBindings::none(), None).expect("replay");
        assert_eq!(term.cols(), 20, "the resize is applied");
        // X sits at col 9 (clamped at the OLD 10-col width), proving the write replayed
        // BEFORE the resize. If the resize had jumped ahead (the bug), the width would
        // be 20 when the write ran and X would land at col 15.
        let row0 = term.get_line_text(0, None).unwrap_or_default();
        assert_eq!(
            row0.find('X'),
            Some(9),
            "X at the pre-resize column: row0={row0:?}"
        );
    }

    #[test]
    fn blob_handle_resolves_after_eviction_shifts_the_deque() {
        // Budget 150 retains two 64-byte blobs; the third evicts the oldest, so the
        // deque's front is no longer BlobId(0) — the handle-resolution base must
        // track that shift or the reader reads the wrong bytes.
        let mut r = TemporalRecorder::with_budget(150);
        r.record_raw_in(&[b'a'; 64]); // BlobId(0)
        r.record_raw_in(&[b'b'; 64]); // BlobId(1)
        r.record_raw_in(&[b'c'; 64]); // BlobId(2) -> evicts BlobId(0)

        assert!(r.blob_bytes(BlobId(0)).is_none(), "evicted handle => None");
        assert_eq!(r.blob_bytes(BlobId(1)), Some(&[b'b'; 64][..]));
        assert_eq!(r.blob_bytes(BlobId(2)), Some(&[b'c'; 64][..]));
        assert!(r.blob_bytes(BlobId(3)).is_none(), "unissued handle => None");
    }

    /// R19: periodic re-keyframing keeps a recent `[keyframe..latest]` window
    /// reachable even after cumulative I/O exceeds the budget MANY times over —
    /// where a single t0 keyframe would be orphaned and replay would die forever.
    /// The keyframes stay BOUNDED (their scrollback is trimmed), so they do not
    /// accumulate and blow the budget.
    #[test]
    fn rekeyframing_keeps_the_latest_reachable_past_the_budget() {
        // A realistic-shaped budget with a modest screen. `interval` floors at
        // 64 KiB, so re-keyframing engages here.
        let mut r = TemporalRecorder::with_budget(1024 * 1024);
        let blank = aterm_core::terminal::Terminal::new(6, 24);
        r.record_keyframe(blank.checkpoint());
        // Flood ~4 MiB (4x the budget) in newline-terminated bursts (grows
        // scrollback), then a recognizable final line.
        for i in 0..4096u32 {
            r.record_raw_in(format!("row {i:06}\r\n").as_bytes());
            r.record_raw_in(&[b'.'; 1000]);
            r.record_raw_in(b"\r\n");
        }
        r.record_raw_in(b"FINAL-SENTINEL-LINE\r\n");

        // Replay to the LATEST instant reconstructs the recent screen (the window
        // survived eviction because re-keyframing minted fresh, bounded bases).
        let term = r
            .replay_at(HostBindings::none(), None)
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
        // Re-keyframing happened, and the keyframes did NOT accumulate unboundedly:
        // the recorder stays within its byte budget.
        assert!(
            r.keyframe_count() >= 2,
            "re-keyframing minted fresh keyframes: {}",
            r.keyframe_count()
        );
    }

    #[test]
    fn replay_degrades_to_none_when_needed_blobs_were_evicted() {
        // A flood past a tiny budget reclaims early input blobs (the keyframe is
        // evicted LAST). A full replay to the latest instant cannot re-feed the
        // aged-out bytes, so it degrades to None — never a panic or a mis-resolved
        // blob (the bounded-retention honest-partial-reach contract).
        let mut r = TemporalRecorder::with_budget(128);
        let blank = aterm_core::terminal::Terminal::new(6, 20);
        r.record_keyframe(blank.checkpoint());
        for _ in 0..64 {
            r.record_raw_in(&[b'x'; 64]);
        }
        assert!(r.replay_at(HostBindings::none(), None).is_none());
    }
}
