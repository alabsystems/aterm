// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-session TURN LEDGER: a bounded, drop-oldest ring of one record per
//! completed [`crate::control_session::cmd_turn`] exchange. It is the durable
//! memory behind two capabilities:
//!
//! * the `turns` control verb — an orchestrator reads back WHAT it drove into a
//!   session (submitted text, whether the submit landed, whether the reply
//!   settled or timed out, how long it took, and a stable hash of the settled
//!   screen), keyed by the same monotonic turn id the `turn` reply prints; and
//! * the `subscribe … events` digest — the push loop scans this ledger by id
//!   watermark and emits one `EVENT <sid> turn <id> …` line per new record, so a
//!   fleet controller watches N sessions on one fd and pulls a full `image`/
//!   `screen` only when a turn actually settled.
//!
//! Shaped like the other bounded recorders on [`crate::SessionCtx`] (drop-oldest,
//! `Mutex`-wrapped, cheap when unused): a session that is never driven by `turn`
//! holds an empty ring.

use std::collections::VecDeque;

/// How many turn records a session retains. Sized so a long agent conversation
/// stays fully readable while the ring never grows unbounded; the screen-hash +
/// bounded text keep each record small (~a few hundred bytes).
pub(crate) const LEDGER_CAP: usize = 512;

/// The submitted-text is bounded in the record so a pathological multi-megabyte
/// `turn` payload cannot bloat the ring; the full text still reached the PTY.
const MAX_TEXT: usize = 512;

/// One completed turn, in the order `cmd_turn` finished it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRecord {
    /// The turn id (the `id=` the `turn` reply prints, and the id the events
    /// digest and `ERR busy turn=<id>` refusals name): unique in the process,
    /// and still rising after a self-update handoff, which carries the counter.
    pub id: u64,
    /// Milliseconds since the process epoch when the turn began (monotonic; for
    /// ordering + aligning against the temporal/cast spines, not wall-clock).
    pub started_ms: u64,
    /// How long the whole type→submit→settle exchange took, in milliseconds.
    pub dur_ms: u64,
    /// Whether a submit keypress VERIFIABLY landed (content advanced). `false`
    /// for `submit=none` and for a swallowed submit that never took.
    pub submitted: bool,
    /// The settle verdict: `settled` (went quiet) or `timeout` (deadline hit).
    pub status: &'static str,
    /// The submitted message, truncated to [`MAX_TEXT`] bytes on a char boundary.
    pub text: String,
    /// FNV-1a/64 of the settled screen text — deterministic across processes, so
    /// a replay/eval harness can diff a re-driven turn's screen against this.
    pub screen_hash: u64,
    /// The engine `content_seq` at settle (matches the `turn` reply's `seq=`).
    pub seq: u64,
    /// Where the session's alt-screen archive stood when the turn STARTED (before
    /// a byte was typed): `offscreen since=<origin>:<last>` then reads exactly the
    /// rows a fullscreen app scrolled off the top during and after this turn.
    /// `history` prints it as `arch=<origin>:<last>`.
    pub arch: ArchMark,
    /// The record came from an EARLIER aterm process, carried to this one by
    /// a self-update handoff: `history` prints `carried=1`, and its
    /// `started_ms` and `seq` are that process's clock and content counter
    /// (neither is comparable with this process's).
    pub carried: bool,
}

/// A position in a session's alt-screen archive (`aterm_core`'s `AltArchive`):
/// the archive's host-assigned `origin` and its newest index `last` (0 = nothing
/// archived yet). Printed `<origin>:<last>` — the exact token `offscreen
/// since=` accepts. A self-update handoff carries the archive with its origin,
/// so a mark minted before it still reads the rows after it; a mark from
/// another origin (a restart, a handoff that could not carry the archive)
/// reads from the start of the new archive instead of from an unrelated index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArchMark {
    /// The archive's origin (unique per aterm process that started an archive,
    /// and kept by a handoff that carries it; 0 when the host never set one).
    pub origin: u64,
    /// The newest archived index at the mark.
    pub last: u64,
}

impl ArchMark {
    /// The mark of `term`'s archive as it stands now. Caller holds the term lock.
    pub(crate) fn of(term: &aterm_core::terminal::Terminal) -> Self {
        let archive = term.alt_archive();
        Self {
            origin: archive.origin(),
            last: archive.last(),
        }
    }
}

impl std::fmt::Display for ArchMark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.origin, self.last)
    }
}

/// A session's bounded turn history, newest-last, drop-oldest at [`LEDGER_CAP`].
#[derive(Default)]
pub struct TurnLedger {
    records: VecDeque<TurnRecord>,
    /// Turn ids below this may have named records of this session that the
    /// ledger never got: a self-update handoff could not carry the ledger
    /// whole (no sidecar, a bad one, a lock another thread kept, records shed
    /// to fit). [`TurnLedger::low_id`] reports it while no record is held,
    /// so a resumed `since-turn=` below it is told, as for evicted records.
    /// 0: none.
    unheld_below: u64,
}

impl TurnLedger {
    /// Append one record, evicting the oldest past the cap.
    pub(crate) fn push(&mut self, rec: TurnRecord) {
        if self.records.len() == LEDGER_CAP {
            self.records.pop_front();
        }
        self.records.push_back(rec);
    }

    /// How many turns this session has retained (for the `who` verb's `turns=`).
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// The ledger a self-update handoff carried from the previous process:
    /// its records, each marked [`TurnRecord::carried`], newest-last. Only a
    /// strictly rising run of ids is kept (a record whose id does not rise past
    /// the one before it is dropped), because [`TurnLedger::since`] seeks by id,
    /// and only the newest [`LEDGER_CAP`] of them. `unheld_below`: turn ids
    /// below it may have named records the carry could not bring (0: none).
    pub(crate) fn carried(records: Vec<TurnRecord>, unheld_below: u64) -> Self {
        let mut ledger = Self {
            records: VecDeque::new(),
            unheld_below,
        };
        for mut rec in records {
            if ledger.records.back().is_some_and(|last| last.id >= rec.id) {
                continue;
            }
            rec.carried = true;
            ledger.push(rec);
        }
        ledger
    }

    /// Every retained record, oldest first (the handoff's export).
    pub(crate) fn records(&self) -> impl Iterator<Item = &TurnRecord> {
        self.records.iter()
    }

    /// Turn ids below this may have named records the ledger never got (see
    /// the field); 0: none. The handoff's export carries it on.
    pub(crate) const fn unheld_below(&self) -> u64 {
        self.unheld_below
    }

    /// The highest recorded turn id, or `None` when empty — the events digest
    /// seeds its watermark to this so it streams only turns that land AFTER
    /// subscription (a live stream, never the historical backlog).
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.records.back().map(|r| r.id)
    }

    /// The LOWEST retained turn id (the drop-oldest low-water), or `None` when empty.
    /// A `since-turn=<n>` resume with `n < low_id - 1` means records were EVICTED
    /// between the anchor and the retained window — the events stream emits a
    /// `GAP … events-resync=` so the resumed subscriber knows it missed some (turn
    /// ids come from a process-global counter, so a client cannot infer the loss from
    /// a per-session id gap the way it can for contiguous block ids). An EMPTY
    /// ledger a self-update handoff could not carry whole reports the first id it
    /// can vouch for ([`TurnLedger::carried`]'s `unheld_below`): the turns below it
    /// are just as gone.
    pub(crate) fn low_id(&self) -> Option<u64> {
        self.records
            .front()
            .map(|r| r.id)
            .or((self.unheld_below > 0).then_some(self.unheld_below))
    }

    /// Records with `id > after`, oldest-first (the events digest's scan, and the
    /// `turns since=<id>` verb). Records only ever append with strictly increasing
    /// ids, so this is a suffix. `None` = all retained records.
    ///
    /// SEEK, DON'T FILTER. The doc above has always said "this is a suffix" and
    /// the body then walked all [`LEDGER_CAP`] retained records and threw away
    /// the prefix — which the subscribe `events` digest paid on EVERY 250 ms
    /// liveness tick per watched target, whether or not a turn had landed.
    /// `id <= after` is a MONOTONE predicate over the ring precisely because the
    /// ids are strictly increasing, so `partition_point` lands on the first
    /// record past the watermark in O(log n) and `range` yields the suffix
    /// itself. Same elements, same order, same borrow — the drain is now
    /// O(log n + matched) instead of O(retained).
    ///
    /// A watermark BELOW the retained low-water still yields EVERY retained
    /// record (`partition_point` returns 0), which is the behaviour the
    /// events-resume `GAP … events-resync=` frame is built on: `low_id()`
    /// reports the drop-oldest eviction, `since` does not silently swallow it.
    pub(crate) fn since(&self, after: Option<u64>) -> impl ExactSizeIterator<Item = &TurnRecord> {
        let start = match after {
            None => 0,
            Some(a) => self.records.partition_point(|r| r.id <= a),
        };
        self.records.range(start..)
    }
}

/// A carried record's `status` word as the one this build prints: `settled`
/// or `timeout` (what `cmd_turn` records); `None` for any other word.
pub(crate) fn status_word(word: &str) -> Option<&'static str> {
    match word {
        "settled" => Some("settled"),
        "timeout" => Some("timeout"),
        _ => None,
    }
}

/// Truncate `s` to at most [`MAX_TEXT`] bytes on a UTF-8 char boundary.
pub(crate) fn clamp_text(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_string();
    }
    let mut end = MAX_TEXT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// FNV-1a/64 — deterministic across processes (unlike `DefaultHasher`), so the
/// screen hash is stable enough to diff a replayed turn against a recorded one.
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Milliseconds since the process epoch (a lazily-pinned monotonic `Instant`).
/// Monotonic and cheap; used for turn `started_ms`/`dur_ms`, not wall-clock.
pub(crate) fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    Instant::now().saturating_duration_since(epoch).as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64) -> TurnRecord {
        TurnRecord {
            id,
            started_ms: id,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: format!("msg{id}"),
            screen_hash: id,
            seq: id,
            arch: ArchMark::default(),
            carried: false,
        }
    }

    #[test]
    fn ring_drops_oldest_and_since_is_a_suffix() {
        let mut l = TurnLedger::default();
        for i in 1..=(LEDGER_CAP as u64 + 3) {
            l.push(rec(i));
        }
        assert_eq!(l.high_id(), Some(LEDGER_CAP as u64 + 3));
        assert_eq!(l.since(None).count(), LEDGER_CAP, "capped");
        // The three oldest were evicted; `since` past the new floor is a suffix.
        let hi = LEDGER_CAP as u64 + 3;
        let got: Vec<u64> = l.since(Some(hi - 2)).map(|r| r.id).collect();
        assert_eq!(
            got,
            vec![hi - 1, hi],
            "only ids strictly greater than `after`"
        );
    }

    /// DIFFERENTIAL: the `partition_point` seek must agree with the linear
    /// filter it replaced, for EVERY watermark — including the two that the
    /// events digest and its GAP frame actually depend on (a watermark below
    /// the retained low-water must still yield everything; a watermark at or
    /// above the high must yield nothing). Ids are deliberately GAPPY here:
    /// turn ids come from a process-global counter, so a single session's
    /// ledger is sparse, and a seek that assumed contiguity would pass a dense
    /// fixture and be wrong in production.
    #[test]
    fn since_seek_matches_the_linear_filter_for_every_watermark() {
        let mut l = TurnLedger::default();
        // Sparse ids (7, 14, 21, …) past the cap, so the ring has evicted and the
        // retained low-water is well above 0.
        for i in 1..=(LEDGER_CAP as u64 + 5) {
            l.push(rec(i * 7));
        }
        let low = l.low_id().expect("non-empty");
        let high = l.high_id().expect("non-empty");
        assert!(
            low > 1,
            "the fixture must have evicted, or the below-low arm is vacuous"
        );

        let reference = |after: Option<u64>| -> Vec<u64> {
            l.records
                .iter()
                .filter(|r| after.is_none_or(|a| r.id > a))
                .map(|r| r.id)
                .collect()
        };
        let observed = |after: Option<u64>| -> Vec<u64> { l.since(after).map(|r| r.id).collect() };

        // The whole neighbourhood: below the low-water, exactly on retained ids,
        // in the GAPS between them, at the high, and past it.
        let mut probes: Vec<Option<u64>> = vec![None, Some(0), Some(1), Some(low - 1)];
        for id in [
            low,
            low + 1,
            low + 3,
            high - 7,
            high - 1,
            high,
            high + 1,
            high + 100,
        ] {
            probes.push(Some(id));
        }
        for after in probes {
            assert_eq!(
                observed(after),
                reference(after),
                "since({after:?}) diverged"
            );
        }
        // The two arms the digest and the GAP frame stand on, named explicitly.
        assert_eq!(
            observed(Some(low - 1)).len(),
            LEDGER_CAP,
            "below low-water = all retained"
        );
        assert!(
            observed(Some(high)).is_empty(),
            "at the high-water = nothing new"
        );
    }

    #[test]
    fn fnv_is_deterministic_and_text_clamps_on_boundary() {
        assert_eq!(fnv1a_64(b"kitty"), fnv1a_64(b"kitty"));
        assert_ne!(fnv1a_64(b"cat"), fnv1a_64(b"dog"));
        let long = "é".repeat(400); // 800 bytes, 2 per char
        let c = clamp_text(&long);
        assert!(c.len() <= MAX_TEXT && c.chars().all(|ch| ch == 'é'));
    }
}
