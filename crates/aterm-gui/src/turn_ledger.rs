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
    /// Another Mac's fleet drive (`by=fleet:<head>:<sid>`, [`TurnBy::fleet`]): the proven node's
    /// head, and the session its requester line names, `-` where it names none.
    Fleet(String),
    /// Not attributable (`by=-`): no caller can be named — a record carried from a build before the field, an edge whose grant is gone.
    #[default]
    Unknown,
}

impl TurnBy {
    /// The drive of the proven `node` for its requester's session `sid`, none where the drive
    /// names none ([`aterm_slog::Actor::fleet_drive`]): [`TurnBy::Unknown`] for a node with
    /// nothing to name.
    #[must_use]
    pub(crate) fn fleet(node: &str, sid: Option<&str>) -> Self {
        match aterm_slog::Actor::fleet_drive(node, sid) {
            aterm_slog::Actor::Fleet(word) => TurnBy::Fleet(word.as_str().to_owned()),
            _ => TurnBy::Unknown,
        }
    }

    /// The stable wire word (`harness|ctl|bridge|<sid>|fleet:<node>:<sid>|-`).
    #[must_use]
    pub(crate) const fn as_wire(&self) -> &str {
        match self {
            TurnBy::Harness => "harness",
            TurnBy::Ctl => "ctl",
            TurnBy::Bridge => "bridge",
            TurnBy::Sid(sid) | TurnBy::Fleet(sid) => sid.as_str(),
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
            fleet if is_fleet_word(fleet) => TurnBy::Fleet(fleet.to_owned()),
            sid if crate::keeper_link::valid_sid(sid) => TurnBy::Sid(sid.to_string()),
            _ => TurnBy::Unknown,
        }
    }
}

/// Whether `word` is a fleet drive's word as [`TurnBy::fleet`] writes it: `fleet:<node>:<sid>`,
/// both parts named, in the session log's actor alphabet.
fn is_fleet_word(word: &str) -> bool {
    word.strip_prefix(aterm_slog::FLEET_ACTOR_PREFIX)
        .and_then(|rest| rest.rsplit_once(':'))
        .is_some_and(|(node, sid)| !node.is_empty() && !sid.is_empty())
        && aterm_slog::ShortId::new(word).is_some()
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
#[path = "turn_ledger_tests.rs"]
mod tests;
