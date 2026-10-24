// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session turn ledger: bounded drop-oldest ring of completed turns.

use std::collections::VecDeque;
use std::sync::Arc;

/// How many turn records a session retains (a few hundred bytes each).
pub(crate) const LEDGER_CAP: usize = 512;

/// Max harness records before eviction prefers the oldest harness record ([`TurnBy::Harness`]).
pub(crate) const HARNESS_SHARE: usize = LEDGER_CAP / 2;

const MAX_TEXT: usize = 512;

/// One completed turn, in the order `cmd_turn` finished it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnRecord {
    /// The turn id (`id=` in `turn` replies and `ERR busy turn=<id>`).
    pub id: u64,
    /// Milliseconds since the process epoch when the turn began ([`now_ms`]; monotonic).
    pub started_ms: u64,
    /// How long the whole type→submit→settle exchange took, in milliseconds.
    pub dur_ms: u64,
    /// Whether a submit keypress VERIFIABLY landed (content advanced).
    pub submitted: bool,
    /// The settle verdict: `settled`, `timeout` or `hangup`; [`STATUS_PRESSED`] for `send` +
    /// `key enter` and `turn settle=submitted`.
    pub status: &'static str,
    /// The submitted message, truncated to [`MAX_TEXT`] bytes on a char boundary.
    pub text: Arc<str>,
    /// FNV-1a/64 of the settled screen text: deterministic across processes, so a replay can diff a re-driven turn's screen against it.
    pub screen_hash: u64,
    /// The engine `content_seq` at settle (matches the `turn` reply's `seq=`).
    pub seq: u64,
    /// The alt-screen archive's mark when the turn started, before a byte was typed, so `offscreen since=<origin>:<last>` reads the rows a fullscreen app scrolled off during and after the turn.
    pub arch: ArchMark,
    /// Who typed the turn ([`TurnBy`]).
    pub by: TurnBy,
    /// Carried from an earlier aterm process by a self-update handoff (`history` prints `carried=1`): its `started_ms` and `seq` are that process's clock and counter.
    pub carried: bool,
}

/// WHO TYPED A TURN.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum TurnBy {
    /// This process's window harness ([`crate::harness_host::holder`]).
    Harness,
    /// An owner-token client (`by=ctl`): `aterm ctl … turn`, an in-session agent's `@<sid>` connection.
    Ctl,
    /// The fabric bridge (`by=bridge`): a connection the server pre-resolves, with no sid to write.
    Bridge,
    /// An edge-scoped connection (`by=<sid>`): the session its token was granted to, read from the target's edge table.
    Sid(String),
    /// Not attributable (`by=-`): no caller can be named — a record carried from a build before the field, an edge whose grant is gone.
    #[default]
    Unknown,
}

impl TurnBy {
    /// The stable wire word (`harness|ctl|bridge|<sid>|-`).
    #[must_use]
    pub(crate) const fn as_wire(&self) -> &str {
        match self {
            TurnBy::Harness => "harness",
            TurnBy::Ctl => "ctl",
            TurnBy::Bridge => "bridge",
            TurnBy::Sid(sid) => sid.as_str(),
            TurnBy::Unknown => "-",
        }
    }

    /// A carried record's word as this build names it: one of its own words, a sid of the shape
    /// this build mints, else [`TurnBy::Unknown`].
    #[must_use]
    pub(crate) fn from_wire(word: &str) -> Self {
        match word {
            "harness" => TurnBy::Harness,
            "ctl" => TurnBy::Ctl,
            "bridge" => TurnBy::Bridge,
            sid if crate::keeper_link::valid_sid(sid) => TurnBy::Sid(sid.to_string()),
            _ => TurnBy::Unknown,
        }
    }
}

/// A position in a session's alt-screen archive (`aterm_core`'s `AltArchive`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArchMark {
    /// The archive's origin (unique per aterm process that started an archive, kept by a handoff that carries it; 0 when the host never set one).
    pub origin: u64,
    /// The newest archived index at the mark.
    pub last: u64,
}

impl ArchMark {
    /// The mark of `term`'s archive as it stands now.
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

/// Status for `send` + `key enter` turns ([`TurnRecord::status`], [`TurnLedger::typed`]).
pub(crate) const STATUS_PRESSED: &str = "pressed";

/// A TEXT THE WINDOW HARNESS TYPED AND HAS NOT SUBMITTED.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Typed {
    /// The control connection that typed it: only its Enter submits it.
    pub conn: u64,
    /// The text, clamped like a record's ([`clamp_text`]).
    pub text: Arc<str>,
    /// When it was written ([`now_ms`]).
    pub started_ms: u64,
    /// The archive's mark as the text was written.
    pub arch: ArchMark,
}

/// A session's bounded turn history ([`LEDGER_CAP`], [`HARNESS_SHARE`]).
#[derive(Default)]
pub(crate) struct TurnLedger {
    records: VecDeque<TurnRecord>,
    /// How many of `records` are the window harness's ([`TurnBy::Harness`]).
    harness: usize,
    /// Evicted harness-record floor ([`HARNESS_SHARE`], [`TurnLedger::low_id`]).
    shed_below: u64,
    /// Turn ids below this may have named records the ledger never got: a self-update handoff
    /// could not carry it whole (no sidecar, a bad one, a lock held elsewhere, records shed to
    /// fit, a turn open at the export).
    unheld_below: u64,
    /// In-flight turn ids ([`TurnLedger::begin`], [`TurnLedger::record`], [`TurnLedger::abandon`]).
    /// Used by [`TurnLedger::carry_floor`].
    open: Vec<u64>,
    /// The session's watchers ([`crate::SessionCtx::watchers`], [`TurnLedger::record`]).
    watchers: std::sync::Arc<crate::session_watch::Watchers>,
    /// The text the window harness typed and has not submitted yet ([`Typed`]).
    typed: Option<Typed>,
    /// How many records the ledger has taken ([`TurnLedger::push`]).
    #[cfg(any(unix, test))]
    generation: u64,
    /// Told after every record the ledger takes ([`TurnLedger::watch`]).
    watch: Option<LedgerWatch>,
}

/// What a ledger tells after every record it takes ([`TurnLedger::watch`]).
pub(crate) type LedgerWatch = std::sync::Arc<dyn Fn() + Send + Sync>;

impl TurnLedger {
    /// Append one record, evicting one past the cap ([`HARNESS_SHARE`]).
    pub(crate) fn push(&mut self, rec: TurnRecord) {
        if self.records.len() >= LEDGER_CAP {
            let at = if self.harness > HARNESS_SHARE {
                self.records
                    .iter()
                    .position(|r| r.by == TurnBy::Harness)
                    .unwrap_or(0)
            } else {
                0
            };
            if let Some(gone) = self.records.remove(at) {
                if gone.by == TurnBy::Harness {
                    self.harness -= 1;
                }
                if at > 0 {
                    self.shed_below = self.shed_below.max(gone.id.saturating_add(1));
                }
            }
        }
        if rec.by == TurnBy::Harness {
            self.harness += 1;
        }
        self.records.push_back(rec);
        #[cfg(any(unix, test))]
        {
            self.generation = self.generation.wrapping_add(1);
        }
        if let Some(watch) = &self.watch {
            watch();
        }
    }

    /// Notify `watch` on every new record ([`crate::crash_restore::SessionJournal::carry_ledger`]).
    #[cfg(any(unix, test))]
    pub(crate) fn watch(&mut self, watch: LedgerWatch) {
        self.watch = Some(watch);
    }

    /// How many records the ledger has taken.
    #[cfg(any(unix, test))]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a carry of this ledger says anything.
    #[cfg(any(unix, test))]
    pub(crate) fn carries_anything(&self) -> bool {
        !self.records.is_empty() || self.unheld_below > 0 || self.shed_below > 0
    }

    /// A turn with id `id` began on this session ([`TurnLedger::record`], [`TurnLedger::abandon`]).
    pub(crate) fn begin(&mut self, id: u64) {
        self.open.push(id);
    }

    /// The turn `id` ended without a record (refused, failed, the session exited).
    pub(crate) fn abandon(&mut self, id: u64) {
        self.open.retain(|&open| open != id);
    }

    /// Record a finished turn that began as `rec.id`.
    pub(crate) fn record(&mut self, mut rec: TurnRecord, mint: impl FnOnce() -> u64) -> u64 {
        self.abandon(rec.id);
        if self.high_id().is_some_and(|high| high >= rec.id) {
            rec.id = mint();
        }
        let id = rec.id;
        self.push(rec);
        self.watchers.ring_record();
        id
    }

    /// This ledger, ringing `watchers` on every record.
    #[must_use]
    pub(crate) fn watched_by(
        self,
        watchers: std::sync::Arc<crate::session_watch::Watchers>,
    ) -> Self {
        TurnLedger { watchers, ..self }
    }

    /// The window harness typed `typed` and has not submitted it ([`Self::take_typed`]).
    pub(crate) fn typed(&mut self, typed: Typed) {
        self.typed = Some(typed);
    }

    /// The text the window harness typed and has not submitted, taken.
    pub(crate) const fn take_typed(&mut self) -> Option<Typed> {
        self.typed.take()
    }

    /// How many turns this session has retained (for the `who` verb's `turns=`).
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// Ledger carried across handoff ([`TurnRecord::carried`], [`TurnLedger::since`]).
    /// Bounded by [`LEDGER_CAP`] and [`Self::push`].
    pub(crate) fn carried(mut records: Vec<TurnRecord>, unheld_below: u64) -> Self {
        records.sort_by_key(|rec| rec.id);
        records.dedup_by_key(|rec| rec.id);
        let mut ledger = Self {
            records: VecDeque::new(),
            harness: 0,
            shed_below: 0,
            unheld_below,
            open: Vec::new(),
            watchers: std::sync::Arc::default(),
            typed: None,
            #[cfg(any(unix, test))]
            generation: 0,
            watch: None,
        };
        for mut rec in records {
            rec.carried = true;
            ledger.push(rec);
        }
        ledger
    }

    /// [`Self::carried`] with the sender's `shed_below`.
    pub(crate) fn with_shed_below(mut self, below: u64) -> Self {
        self.shed_below = self.shed_below.max(below);
        self
    }

    /// Every retained record, oldest first (the handoff's export).
    #[cfg(any(unix, test))]
    pub(crate) fn records(&self) -> impl Iterator<Item = &TurnRecord> {
        self.records.iter()
    }

    /// Turn ids below this may have named a harness record evicted from inside the ring.
    #[cfg(any(unix, test))]
    pub(crate) const fn shed_below(&self) -> u64 {
        self.shed_below
    }

    /// Turn ids below this may have named records the ledger never got (see the field).
    #[cfg(all(test, unix))]
    pub(crate) const fn unheld_below(&self) -> u64 {
        self.unheld_below
    }

    /// The live [`TurnLedger::unheld_below`]: `None` when 0, or once the records it lost would
    /// have aged out of a ledger carried whole.
    pub(crate) fn live_floor(&self) -> Option<u64> {
        let floor = self.unheld_below;
        let aged_out = self.records.len() >= LEDGER_CAP
            && self.records.front().is_some_and(|front| front.id >= floor);
        (floor > 0 && !aged_out).then_some(floor)
    }

    /// The `unheld_below` a handoff's export carries.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_floor(&self, ceiling: u64) -> u64 {
        let live = self.live_floor().unwrap_or(0);
        let Some(&newest) = self.open.iter().max() else {
            return live;
        };
        let own_id = self.open.len() == 1 && self.high_id().is_none_or(|high| high < newest);
        let past = if own_id { newest } else { newest.max(ceiling) };
        live.max(past.saturating_add(1))
    }

    /// The highest recorded turn id, or `None` when empty.
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.records.back().map(|r| r.id)
    }

    /// The newest record the window harness did not type ([`TurnBy::Harness`]).
    pub(crate) fn newest_not_harness(&self) -> Option<&TurnRecord> {
        self.records.iter().rev().find(|r| r.by != TurnBy::Harness)
    }

    /// The LOWEST retained turn id (the drop-oldest low-water), or `None` when empty.
    pub(crate) fn low_id(&self) -> Option<u64> {
        let floor = self.unheld_below.max(self.shed_below);
        let floor = (floor > 0).then_some(floor);
        match (self.records.front().map(|r| r.id), floor) {
            (Some(front), Some(floor)) => Some(front.max(floor)),
            (front, floor) => front.or(floor),
        }
    }

    /// Records with `id > after`, oldest first.
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

/// A carried record's `status` word as the one this build prints: `settled`, `timeout`,
/// `hangup` or [`STATUS_PRESSED`]; `None` for any other word.
pub(crate) fn status_word(word: &str) -> Option<&'static str> {
    match word {
        "settled" => Some("settled"),
        "timeout" => Some("timeout"),
        "hangup" => Some("hangup"),
        STATUS_PRESSED => Some(STATUS_PRESSED),
        _ => None,
    }
}

/// Truncate `s` to at most [`MAX_TEXT`] bytes on a UTF-8 char boundary.
pub(crate) fn clamp_text(s: &str) -> Arc<str> {
    let mut end = s.len().min(MAX_TEXT);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Arc::from(&s[..end])
}

/// Milliseconds since a lazily pinned process epoch (monotonic, not wall-clock).
#[cfg(not(test))]
pub(crate) use aterm_gui_timing::now_ms;

/// [`aterm_gui_timing::now_ms`], moved on by what this test thread [`advance_for_test`]ed.
#[cfg(test)]
pub(crate) fn now_ms() -> u64 {
    aterm_gui_timing::now_ms().saturating_add(ADVANCED.with(std::cell::Cell::get))
}

#[cfg(test)]
thread_local! {
    static ADVANCED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Move this test thread's ledger clock on by `ms` ([`now_ms`]).
#[cfg(test)]
pub(crate) fn advance_for_test(ms: u64) {
    ADVANCED.with(|advanced| advanced.set(advanced.get().saturating_add(ms)));
}

#[cfg(test)]
mod tests {
    use super::*;

    const CEILING: u64 = 1 << 20;

    fn rec(id: u64) -> TurnRecord {
        TurnRecord {
            id,
            started_ms: id,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: format!("msg{id}").into(),
            screen_hash: id,
            seq: id,
            arch: ArchMark::default(),
            by: TurnBy::Ctl,
            carried: false,
        }
    }

    #[test]
    fn by_words_round_trip_and_anything_else_is_unknown() {
        let sid = aterm_session::SessionId::generate();
        for by in [
            TurnBy::Harness,
            TurnBy::Ctl,
            TurnBy::Bridge,
            TurnBy::Sid(sid.as_str().to_string()),
            TurnBy::Unknown,
        ] {
            assert_eq!(TurnBy::from_wire(by.as_wire()), by, "{by:?}");
        }
        for word in ["", "fleet", "human", "Harness", "s-", "s-xyz", "s-1 text=x"] {
            assert_eq!(TurnBy::from_wire(word), TurnBy::Unknown, "{word:?}");
        }
        assert_eq!(TurnBy::default(), TurnBy::Unknown);
    }

    #[test]
    fn every_record_moves_the_generation_and_tells_the_watch() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        let told = Arc::new(AtomicU64::new(0));
        let mut l = TurnLedger::default();
        assert_eq!(l.generation(), 0);
        assert!(!l.carries_anything());
        l.push(rec(1));
        assert_eq!(l.generation(), 1, "a record before any watch still counts");
        let seen = Arc::clone(&told);
        l.watch(Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
        }));
        for id in 2..=(LEDGER_CAP as u64 + 3) {
            l.push(rec(id));
        }
        assert_eq!(l.generation(), LEDGER_CAP as u64 + 3);
        assert_eq!(told.load(Ordering::Relaxed), LEDGER_CAP as u64 + 2);
        l.begin(9_000);
        l.abandon(9_000);
        assert_eq!(l.record(rec(9_001), || 9_002), 9_001);
        assert_eq!(
            (l.generation(), told.load(Ordering::Relaxed)),
            (LEDGER_CAP as u64 + 4, LEDGER_CAP as u64 + 3),
            "a turn's record counts once; its begin and abandon do not"
        );
        let carried = TurnLedger::carried(vec![rec(4), rec(5)], 0);
        assert_eq!(carried.generation(), 2);
        assert!(carried.carries_anything());
        assert!(TurnLedger::carried(Vec::new(), 3).carries_anything());
        assert!(!TurnLedger::carried(Vec::new(), 0).carries_anything());
    }

    #[test]
    fn the_room_turn_is_the_newest_the_harness_did_not_type() {
        let harness = |id: u64, carried: bool| TurnRecord {
            status: STATUS_PRESSED,
            by: TurnBy::Harness,
            carried,
            ..rec(id)
        };
        let mut l = TurnLedger::default();
        assert!(l.newest_not_harness().is_none());
        l.push(harness(1, true));
        assert!(l.newest_not_harness().is_none(), "the harness's alone");
        l.push(rec(2));
        l.push(harness(3, false));
        l.push(harness(4, true));
        assert_eq!(l.high_id(), Some(4));
        assert_eq!(l.newest_not_harness().map(|r| r.id), Some(2));
        l.push(rec(5));
        assert_eq!(l.newest_not_harness().map(|r| r.id), Some(5));
    }

    #[test]
    fn the_harness_makes_room_among_its_own_and_the_managers_turn_stays() {
        let harness = |id: u64| TurnRecord {
            status: STATUS_PRESSED,
            by: TurnBy::Harness,
            ..rec(id)
        };
        let cap = LEDGER_CAP as u64;
        let ids = |l: &TurnLedger| l.since(None).map(|r| r.id).collect::<Vec<u64>>();
        let mut l = TurnLedger::default();
        l.push(rec(1));
        let last = 1 + 2 * cap;
        for id in 2..=last {
            l.push(harness(id));
        }
        let held = ids(&l);
        assert_eq!(held.len(), LEDGER_CAP);
        assert_eq!(l.newest_not_harness().map(|r| r.id), Some(1), "kept");
        assert_eq!(held[1..], (last - cap + 2..=last).collect::<Vec<u64>>()[..]);
        assert_eq!(l.low_id(), Some(held[1]), "a resume from 1 is told");
        assert_eq!(l.shed_below(), held[1]);
        let resumed = last + 1;
        for id in resumed..resumed + cap - 1 - HARNESS_SHARE as u64 {
            l.push(rec(id));
        }
        assert_eq!(l.since(None).next().map(|r| r.id), Some(1), "still held");
        l.push(rec(resumed + cap));
        assert_ne!(l.since(None).next().map(|r| r.id), Some(1), "drop-oldest");
        let held = ids(&l);
        assert!(held.windows(2).all(|w| w[0] < w[1]), "in id order");
        for run in [rec as fn(u64) -> TurnRecord, |id| TurnRecord {
            by: TurnBy::Harness,
            ..rec(id)
        }] {
            let mut l = TurnLedger::default();
            for id in 1..=cap + 5 {
                l.push(run(id));
            }
            assert_eq!(ids(&l), (6..=cap + 5).collect::<Vec<u64>>());
            assert_eq!((l.shed_below(), l.low_id()), (0, Some(6)));
        }
    }

    #[test]
    fn since_seek_matches_the_linear_filter_for_every_watermark() {
        let mut l = TurnLedger::default();
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
    fn a_preempted_turn_that_records_late_is_renumbered_and_streamed() {
        let mut l = TurnLedger::default();
        l.push(rec(5));
        l.begin(10);
        l.begin(11);
        assert_eq!(l.record(rec(11), || unreachable!("in order")), 11);
        assert_eq!(l.carry_floor(CEILING), CEILING + 1);
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
        assert_eq!(l.carry_floor(CEILING), 0, "nothing is open any more");

        let carried = TurnLedger::carried(vec![rec(5), rec(11), rec(10), rec(11)], 0);
        let ids: Vec<u64> = carried.since(None).map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 10, 11]);
        assert_eq!(carried.since(Some(9)).count(), 2, "turn 10 is found");
    }

    #[test]
    fn an_open_turn_is_carried_as_an_id_the_ledger_does_not_vouch_for() {
        let mut l = TurnLedger::default();
        for id in 1..=3 {
            l.push(rec(id));
        }
        l.begin(4);
        assert_eq!(l.carry_floor(CEILING), 5);
        let adopted = TurnLedger::carried(l.since(None).cloned().collect(), l.carry_floor(CEILING));
        assert_eq!(adopted.low_id(), Some(5), "the floor, over the held 1");
        l.abandon(4);
        assert_eq!(l.carry_floor(CEILING), 0);
    }

    #[test]
    fn a_floor_goes_stale_once_the_records_under_it_would_have_aged_out() {
        let cap = LEDGER_CAP as u64;
        let empty = TurnLedger::carried(Vec::new(), 10);
        assert_eq!(empty.live_floor(), Some(10), "an empty ledger's floor");
        assert_eq!(empty.carry_floor(CEILING), 10);
        let mut dropped = TurnLedger::carried(Vec::new(), 10);
        dropped.push(rec(10));
        assert_eq!(dropped.live_floor(), Some(10), "one turn past a drop");
        assert_eq!(
            dropped.carry_floor(CEILING),
            10,
            "carried on by a second update"
        );
        let at = TurnLedger::carried((10..=12).map(rec).collect(), 10);
        assert_eq!(at.live_floor(), Some(10), "at the oldest held record");
        assert_eq!(at.low_id(), Some(10), "the low-water says it too");
        let mut l = TurnLedger::carried((1..=3).map(rec).collect(), 10);
        assert_eq!(l.live_floor(), Some(10), "above the oldest held record");
        for id in 11..10 + cap - 1 {
            dropped.push(rec(id));
        }
        for id in 10..10 + cap - 1 {
            l.push(rec(id));
        }
        assert_eq!(dropped.len(), LEDGER_CAP - 1);
        assert_eq!(dropped.live_floor(), Some(10), "not yet LEDGER_CAP turns");
        assert_eq!(l.live_floor(), Some(10), "records under 10 still held");
        dropped.push(rec(10 + cap - 1));
        l.push(rec(10 + cap - 1));
        assert_eq!(l.low_id(), Some(10));
        assert_eq!(l.live_floor(), None, "LEDGER_CAP turns past the floor");
        assert_eq!(dropped.live_floor(), None, "the lost turns aged out");
        assert_eq!(l.carry_floor(CEILING), 0, "a stale floor is not carried on");
        l.begin(10 + cap);
        assert_eq!(l.carry_floor(CEILING), 11 + cap);
    }

    #[test]
    fn text_clamps_on_boundary() {
        let long = format!("a{}", "é".repeat(400));
        assert_eq!(&*clamp_text(&long), format!("a{}", "é".repeat(255)));
        let on_boundary = "é".repeat(400);
        assert_eq!(
            &*clamp_text(&on_boundary),
            "é".repeat(256),
            "cut at the cap"
        );
        let short = "é".repeat(256);
        assert_eq!(&*clamp_text(&short), short, "at the cap it is kept whole");
    }
}
