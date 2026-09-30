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
pub(crate) struct TurnRecord {
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
    /// The settle verdict: `settled` (went quiet), `timeout` (deadline hit) or
    /// `hangup` (the caller hung up during the settle, so it was not awaited).
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
pub(crate) struct ArchMark {
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
pub(crate) struct TurnLedger {
    records: VecDeque<TurnRecord>,
    /// Turn ids below this may have named records of this session that the
    /// ledger never got: a self-update handoff could not carry the ledger
    /// whole (no sidecar, a bad one, a lock another thread kept, records shed
    /// to fit, a turn still open at the export). [`TurnLedger::low_id`]
    /// reports it while it is above every held record's id, so a resumed
    /// `since-turn=` below it is told, as for evicted records.
    /// 0: none.
    unheld_below: u64,
    /// Turn ids minted for this session whose record has not landed yet
    /// ([`TurnLedger::begin`] … [`TurnLedger::record`] or
    /// [`TurnLedger::abandon`]): a handoff's export carries them as ids the
    /// carried ledger does not vouch for ([`TurnLedger::carry_floor`]), since
    /// their records land in the process that exits (the 2026-09-29 review,
    /// round seven, finding 23). One lease holds a session's turn at a time,
    /// so this holds one id, or a few when `lease release force` preempted a
    /// wedged turn.
    open: Vec<u64>,
}

impl TurnLedger {
    /// Append one record, evicting the oldest past the cap. The caller keeps
    /// the ids rising ([`TurnLedger::record`] does, for a live turn).
    pub(crate) fn push(&mut self, rec: TurnRecord) {
        if self.records.len() == LEDGER_CAP {
            self.records.pop_front();
        }
        self.records.push_back(rec);
    }

    /// A turn with id `id` began on this session: its record is owed
    /// ([`TurnLedger::record`]), or it ends without one
    /// ([`TurnLedger::abandon`]).
    pub(crate) fn begin(&mut self, id: u64) {
        self.open.push(id);
    }

    /// The turn `id` ended without a record (refused, failed, the session
    /// exited): no record is owed for it any more. Idempotent.
    pub(crate) fn abandon(&mut self, id: u64) {
        self.open.retain(|&open| open != id);
    }

    /// Record a finished turn that began as `rec.id`, and return the id it is
    /// recorded under — the one its reply prints. That is `rec.id`, unless a
    /// LATER turn already recorded (a `lease release force` preempted this
    /// one, and the turn that took the slot settled first): then it is
    /// re-numbered with `mint`, the process's turn-id mint, so the ids stay
    /// strictly rising in the order records land. [`TurnLedger::since`] seeks
    /// by id, the `events` digest streams by id watermark, and a handoff
    /// carries only a rising run — a record landing under its old, lower id
    /// was one no subscriber was ever sent (round seven, finding 43).
    pub(crate) fn record(&mut self, mut rec: TurnRecord, mint: impl FnOnce() -> u64) -> u64 {
        self.abandon(rec.id);
        if self.high_id().is_some_and(|high| high >= rec.id) {
            rec.id = mint();
        }
        let id = rec.id;
        self.push(rec);
        id
    }

    /// How many turns this session has retained (for the `who` verb's `turns=`).
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// The ledger a self-update handoff carried from the previous process:
    /// its records, each marked [`TurnRecord::carried`], in id order (as
    /// [`TurnLedger::since`] seeks), and only the newest [`LEDGER_CAP`] of
    /// them. A sender whose ledger was out of order (an older build recorded a
    /// preempted turn after the one that replaced it) is put back in order
    /// rather than losing the record; of two records under one id, the first
    /// is kept. `unheld_below`: turn ids below it may have named records the
    /// carry could not bring (0: none).
    pub(crate) fn carried(mut records: Vec<TurnRecord>, unheld_below: u64) -> Self {
        records.sort_by_key(|rec| rec.id);
        records.dedup_by_key(|rec| rec.id);
        let mut ledger = Self {
            records: VecDeque::new(),
            unheld_below,
            open: Vec::new(),
        };
        for mut rec in records {
            rec.carried = true;
            ledger.push(rec);
        }
        ledger
    }

    /// Every retained record, oldest first (the handoff's export).
    #[cfg(any(unix, test))]
    pub(crate) fn records(&self) -> impl Iterator<Item = &TurnRecord> {
        self.records.iter()
    }

    /// Turn ids below this may have named records the ledger never got (see
    /// the field); 0: none. Replies read [`TurnLedger::live_floor`].
    #[cfg(test)]
    pub(crate) const fn unheld_below(&self) -> u64 {
        self.unheld_below
    }

    /// The floor ([`TurnLedger::unheld_below`]) while it still tells a reader
    /// something: the ledger is empty, or the floor lies above its OLDEST
    /// held record, so a turn id inside the window the ledger answers for may
    /// be missing. Once every record under the floor has been evicted (the
    /// oldest held id is at or past it), the floor sits below that window —
    /// the same low-water eviction leaves, and [`TurnLedger::low_id`] already
    /// answers the oldest record there — so it is stale and `None`: a floor
    /// set by one update must not ride every `history` reply and every later
    /// export for the rest of the session (round seven, finding 58's review).
    pub(crate) fn live_floor(&self) -> Option<u64> {
        let floor = self.unheld_below;
        (floor > 0 && self.records.front().is_none_or(|front| floor > front.id)).then_some(floor)
    }

    /// What a handoff's export carries as the ledger's `unheld_below`: the
    /// ledger's own, raised past every turn still OPEN on this session. Such a
    /// turn records into this process's ledger after the export took it (a
    /// parked reader leaves its screen frozen, so an idle settle latches), and
    /// the successor never sees that record — so the carried ledger must not
    /// vouch for its id (round seven, finding 23).
    #[cfg(any(unix, test))]
    /// A floor gone stale ([`TurnLedger::live_floor`]) is not carried on.
    pub(crate) fn carry_floor(&self) -> u64 {
        self.open
            .iter()
            .map(|id| id.saturating_add(1))
            .fold(self.live_floor().unwrap_or(0), u64::max)
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
    /// are just as gone — and so does one whose floor sits ABOVE a held record
    /// (a turn still open when the handoff exported it: the records past the
    /// floor are whole, the ones below it are not), so the low-water is the
    /// higher of the two.
    pub(crate) fn low_id(&self) -> Option<u64> {
        let floor = (self.unheld_below > 0).then_some(self.unheld_below);
        match (self.records.front().map(|r| r.id), floor) {
            (Some(front), Some(floor)) => Some(front.max(floor)),
            (front, floor) => front.or(floor),
        }
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
            Some(a) => self.records.partition_point(|r| {
                #[cfg(test)]
                crate::work_counts::retained_record_touched();
                r.id <= a
            }),
        };
        self.records.range(start..)
    }
}

/// A carried record's `status` word as the one this build prints: `settled`,
/// `timeout` or `hangup` (what `cmd_turn` records); `None` for any other word.
pub(crate) fn status_word(word: &str) -> Option<&'static str> {
    match word {
        "settled" => Some("settled"),
        "timeout" => Some("timeout"),
        "hangup" => Some("hangup"),
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
    let mut h = Fnv1a64::new();
    h.update(bytes);
    h.finish()
}

/// The same FNV-1a/64 over successive byte slices. The agent-status sweep
/// hashes visible rows while retaining only its last forty, so it must use
/// exactly the algorithm that `status hash=` and `turn` use for one buffer.
pub(crate) struct Fnv1a64(u64);

impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv1a64 {
    pub(crate) const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }

    pub(crate) const fn finish(self) -> u64 {
        self.0
    }
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

    /// Round seven, finding 43: a turn a `lease release force` preempted
    /// finishes after the turn that took its slot. Recorded under its own,
    /// lower id it sat behind the newer record — the `events` digest, whose
    /// watermark had passed it, never sent it, and a handoff's carry dropped
    /// it. It is re-numbered instead, so the ids rise in the order records
    /// land, and a watcher past the newer turn is sent it.
    #[test]
    fn a_preempted_turn_that_records_late_is_renumbered_and_streamed() {
        let mut l = TurnLedger::default();
        l.push(rec(5));
        l.begin(10);
        l.begin(11);
        assert_eq!(l.record(rec(11), || unreachable!("in order")), 11);
        let watermark = l.high_id();
        let mut minted = 11;
        let late = l.record(rec(10), || {
            minted += 1;
            minted
        });
        assert_eq!(late, 12, "re-numbered past the newer record");
        let ids: Vec<u64> = l.since(None).map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 11, 12], "rising in landing order");
        let streamed: Vec<u64> = l.since(watermark).map(|r| r.id).collect();
        assert_eq!(streamed, vec![12], "the watcher past 11 is sent it");
        assert_eq!(l.carry_floor(), 0, "nothing is open any more");

        // A sender that predates this (its ledger out of order) is put back
        // in order by the receiver, not left a record short.
        let carried = TurnLedger::carried(vec![rec(5), rec(11), rec(10), rec(11)], 0);
        let ids: Vec<u64> = carried.since(None).map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 10, 11]);
        assert_eq!(carried.since(Some(9)).count(), 2, "turn 10 is found");
    }

    /// Round seven, finding 23: a turn still OPEN when the handoff exports
    /// the ledger records into the exiting process. The export carries a
    /// floor past it, and the adopted ledger's low-water is that floor even
    /// though older records are held, so a subscriber resuming from before
    /// it is told (`GAP … events-resync=`).
    #[test]
    fn an_open_turn_is_carried_as_an_id_the_ledger_does_not_vouch_for() {
        let mut l = TurnLedger::default();
        for id in 1..=3 {
            l.push(rec(id));
        }
        l.begin(4);
        assert_eq!(l.carry_floor(), 5);
        let adopted = TurnLedger::carried(l.since(None).cloned().collect(), l.carry_floor());
        assert_eq!(adopted.low_id(), Some(5), "the floor, over the held 1");
        // An abandoned turn owes no record.
        l.abandon(4);
        assert_eq!(l.carry_floor(), 0);
    }

    /// A floor stays live while it lies inside the ledger's window, and goes
    /// stale — neither reported nor carried on — once every record under it
    /// is evicted (round seven, finding 58's review: it rode every `history`
    /// reply and every later export for the life of the session).
    #[test]
    fn a_floor_goes_stale_once_the_records_under_it_are_evicted() {
        let empty = TurnLedger::carried(Vec::new(), 10);
        assert_eq!(empty.live_floor(), Some(10), "an empty ledger's floor");
        assert_eq!(empty.carry_floor(), 10);
        let mut l = TurnLedger::carried((1..=3).map(rec).collect(), 10);
        assert_eq!(l.live_floor(), Some(10), "above the oldest held record");
        let at = TurnLedger::carried((10..=12).map(rec).collect(), 10);
        assert_eq!(at.live_floor(), None, "at the oldest held record");
        assert_eq!(at.low_id(), Some(10), "the low-water still says it");
        for id in 10..10 + LEDGER_CAP as u64 {
            l.push(rec(id));
        }
        assert_eq!(l.low_id(), Some(10));
        assert_eq!(l.live_floor(), None, "every record under 10 is evicted");
        assert_eq!(l.carry_floor(), 0, "a stale floor is not carried on");
        // An open turn still raises the carry over a stale floor.
        l.begin(10 + LEDGER_CAP as u64);
        assert_eq!(l.carry_floor(), 11 + LEDGER_CAP as u64);
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
