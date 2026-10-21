// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-session TURN LEDGER: a bounded, drop-oldest ring of one record per
//! completed [`crate::control_session::cmd_turn`] exchange (a `turn
//! settle=submitted`, answered at its verified submit, is `status=pressed`),
//! and per text the window harness typed with `send` and submitted with `key
//! enter` ([`crate::control_session::ledger_typed_input`], `status=pressed`). It is
//! the durable memory behind two capabilities:
//!
//! * the `history` control verb — an orchestrator reads back WHAT was driven
//!   into a session and BY WHOM (submitted text, who typed it, whether the
//!   submit landed, whether the reply settled or timed out, how long it took,
//!   and a stable hash of the settled screen), keyed by the same monotonic
//!   turn id the `turn` reply prints; and
//! * the `subscribe … events` digest — the push loop scans this ledger by id
//!   watermark and emits one `EVENT <sid> turn <id> …` line per new record, so a
//!   fleet controller watches N sessions on one fd and pulls a full `image`/
//!   `screen` only when a turn actually settled.
//!
//! Shaped like the other bounded recorders on [`crate::SessionCtx`] (drop-oldest,
//! `Mutex`-wrapped, cheap when unused): a session that is never driven by `turn`
//! holds an empty ring. One refinement: past [`HARNESS_SHARE`] the window
//! harness's own records make room among themselves ([`TurnLedger::push`]),
//! so a long autonomous run of continuations never evicts the manager's
//! turn the readers anchor on.

use std::collections::VecDeque;
use std::sync::Arc;

/// How many turn records a session retains (a few hundred bytes each).
pub(crate) const LEDGER_CAP: usize = 512;

/// While more than this many of a full ledger's records are the window
/// harness's own ([`TurnBy::Harness`]), an eviction takes the OLDEST OF
/// THOSE instead of the oldest record. The harness's continuations, done
/// checks and `/usage` probes come one per act, with no cap by default, so a
/// long autonomous run would otherwise push the manager's turns out of the
/// ring: the report reader would find no turn of the manager's and open at the
/// harness's own `❯ keep going`, and `drive ledger` and the companion would
/// lose them. The harness keeps its newest texts (all a reader of them
/// needs: the survey matches their words in a transcript). At or below it
/// the ring is plain drop-oldest, so a manager's long run sheds the
/// harness's records like any other.
pub(crate) const HARNESS_SHARE: usize = LEDGER_CAP / 2;

/// The submitted-text is bounded in the record so a pathological multi-megabyte
/// `turn` payload cannot bloat the ring; the full text still reached the PTY.
const MAX_TEXT: usize = 512;

/// One completed turn, in the order `cmd_turn` finished it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnRecord {
    /// The turn id (the `turn` reply's `id=`, also named by the events digest and `ERR busy
    /// turn=<id>`): unique in the process, and still rising after a self-update handoff,
    /// which carries the counter.
    pub id: u64,
    /// Milliseconds since the process epoch when the turn began ([`now_ms`]; monotonic).
    pub started_ms: u64,
    /// How long the whole type→submit→settle exchange took, in milliseconds.
    pub dur_ms: u64,
    /// Whether a submit keypress VERIFIABLY landed (content advanced). `false`
    /// for `submit=none` and for a swallowed submit that never took. A
    /// [`STATUS_PRESSED`] record's Enter was written behind the text and
    /// nothing after it was awaited: the harness's, fenced on a read that
    /// showed the text in the composer; a `turn settle=submitted`'s, verified
    /// as any turn's submit is.
    pub submitted: bool,
    /// The settle verdict: `settled` (went quiet), `timeout` (deadline hit) or
    /// `hangup` (the caller hung up during the settle, so it was not awaited);
    /// [`STATUS_PRESSED`] for the harness's `send` and `key enter`, and for a
    /// `turn settle=submitted`, which await no settle.
    pub status: &'static str,
    /// The submitted message, truncated to [`MAX_TEXT`] bytes on a char
    /// boundary. Shared, so a reader copying records out of the ledger lock
    /// bumps a refcount instead of copying the text.
    pub text: Arc<str>,
    /// FNV-1a/64 of the settled screen text — deterministic across processes, so
    /// a replay/eval harness can diff a re-driven turn's screen against this.
    pub screen_hash: u64,
    /// The engine `content_seq` at settle (matches the `turn` reply's `seq=`).
    pub seq: u64,
    /// The alt-screen archive's mark when the turn started, before a byte was typed, so
    /// `offscreen since=<origin>:<last>` reads the rows a fullscreen app scrolled off during
    /// and after the turn. `history` prints it as `arch=`.
    pub arch: ArchMark,
    /// Who typed the turn ([`TurnBy`]): `history` prints it as `by=<word>`.
    pub by: TurnBy,
    /// The record came from an EARLIER aterm process, carried to this one by
    /// a self-update handoff: `history` prints `carried=1`, and its
    /// `started_ms` and `seq` are that process's clock and content counter
    /// (neither is comparable with this process's).
    pub carried: bool,
}

/// WHO TYPED A TURN, as `history` prints it (`by=<word>`): the connection's
/// own identity, the one the exits ledger's `by=` names
/// ([`crate::control_session::caller_actor`]), with one refinement only the
/// turn can make — an owner-token turn typed under this process's window
/// harness's hand is the harness's
/// ([`crate::control_session::typed_by`]). So a reader of the ledger (the
/// survey's newest prompt, a manager's report) tells the harness's own turns
/// — the live upgrade's notice and release, a relaunched agent's
/// continuation — from a manager's or another session's, without a list of
/// the harness's words. (A person at the keyboard types no `turn`, so has no
/// record here.) `fleet` is reserved for a turn a fleet peer types; no path
/// types one yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum TurnBy {
    /// This process's window harness (`by=harness`): an owner-token turn
    /// typed under the drive lease its supervisor loops take a tab by
    /// ([`crate::harness_host::holder`]), held over the turn's own
    /// connection — the hand the presence band reads as aterm's own (ruling
    /// 313), and the one every line the upgrade and relaunch lanes type goes
    /// under. The turn-end loop types its texts (the continuation, the done
    /// check, an answer, the worker's own suggestion, the accounts probe's
    /// `/usage`) under the same hand: as a fenced `send` and `key enter`
    /// where the host fences them (`aterm-agent`'s `Session::type_fenced`,
    /// recorded `status=pressed` by
    /// [`crate::control_session::ledger_typed_input`]), else as a `turn`.
    /// A text of its own that it left standing (its Enter refused or
    /// skipped) and sends later with Enter alone is NOT recorded: by then
    /// the hand was given back, and nothing here can tell that the composer
    /// still holds the text as it was typed. Another instance's harness, or
    /// a CLI supervisor, holds a hand under its own pid and stays `ctl`. A
    /// carried record's `harness` is the previous instance's.
    Harness,
    /// An owner-token client (`by=ctl`): `aterm ctl … turn`, an in-session
    /// agent's `@<sid>` connection. The per-instance token names the
    /// instance's owner, not a session, so this is the most the socket knows.
    Ctl,
    /// The fabric bridge (`by=bridge`): a connection the server
    /// pre-resolves, with no sid to write.
    Bridge,
    /// An edge-scoped connection (`by=<sid>`): the session its token was
    /// granted to, read from the target's edge table.
    Sid(String),
    /// Not attributable (`by=-`): no caller can be named — a record carried
    /// from a build before the field, an edge whose grant is gone.
    #[default]
    Unknown,
}

impl TurnBy {
    /// The stable wire word (`harness|ctl|bridge|<sid>|-`).
    #[must_use]
    pub(crate) fn as_wire(&self) -> &str {
        match self {
            TurnBy::Harness => "harness",
            TurnBy::Ctl => "ctl",
            TurnBy::Bridge => "bridge",
            TurnBy::Sid(sid) => sid.as_str(),
            TurnBy::Unknown => "-",
        }
    }

    /// A carried record's word as this build names it: one of its own words,
    /// a sid of the shape this build mints, else [`TurnBy::Unknown`] — an
    /// older sender's missing field, a later build's word (`fleet`), or
    /// anything a sidecar should not have held.
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

/// A position in a session's alt-screen archive (`aterm_core`'s `AltArchive`):
/// the archive's host-assigned `origin` and its newest index `last` (0 = nothing
/// archived yet). Printed `<origin>:<last>` — the exact token `offscreen
/// since=` accepts. A self-update handoff carries the archive with its origin,
/// so a mark minted before it still reads the rows after it; a mark from
/// another origin (a restart, a handoff that could not carry the archive)
/// reads from the start of the new archive instead of from an unrelated index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArchMark {
    /// The archive's origin (unique per aterm process that started an archive, kept by a
    /// handoff that carries it; 0 when the host never set one).
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

/// The [`TurnRecord::status`] of a text the window harness typed with `send`
/// and submitted with `key enter` ([`TurnLedger::typed`]), and of a `turn
/// settle=submitted` whose submit was verified: the Enter was written, and no
/// settle was awaited after it.
pub(crate) const STATUS_PRESSED: &str = "pressed";

/// A TEXT THE WINDOW HARNESS TYPED AND HAS NOT SUBMITTED: its `send` under
/// its hand, kept for the Enter its connection presses next
/// ([`crate::control_session::ledger_typed_input`]).
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

/// A session's bounded turn history, newest-last, drop-oldest at [`LEDGER_CAP`]
/// (the harness's records past [`HARNESS_SHARE`] going first).
#[derive(Default)]
pub(crate) struct TurnLedger {
    records: VecDeque<TurnRecord>,
    /// How many of `records` are the window harness's ([`TurnBy::Harness`]).
    harness: usize,
    /// Turn ids below this may have named a harness record evicted from
    /// INSIDE the ring ([`HARNESS_SHARE`]): every record from here on is
    /// held, so [`TurnLedger::low_id`] reports it and a resumed
    /// `since-turn=` below it is told, as for a record evicted from the
    /// front. A handoff carries it (`handoff_carry`'s sidecar). 0: none.
    shed_below: u64,
    /// Turn ids below this may have named records of this session that the
    /// ledger never got: a self-update handoff could not carry the ledger
    /// whole (no sidecar, a bad one, a lock another thread kept, records shed
    /// to fit, a turn still open at the export). [`TurnLedger::low_id`]
    /// reports it while it is above every held record's id, so a resumed
    /// `since-turn=` below it is told, as for evicted records.
    /// 0: none.
    unheld_below: u64,
    /// Turn ids begun on this session whose record has not landed ([`TurnLedger::begin`] …
    /// [`TurnLedger::record`] / [`TurnLedger::abandon`]); [`TurnLedger::carry_floor`] keeps a
    /// handoff's carried ledger from vouching for them. One lease holds a session's turn at a
    /// time, so this is one id, or a few after `lease release force` preempted a wedged turn.
    open: Vec<u64>,
    /// The session's watchers ([`crate::SessionCtx::watchers`]), rung by
    /// [`TurnLedger::record`] so the `events` digest pushes a landed turn at
    /// once. Never carried: a successor's ledger rings its own session's list
    /// ([`crate::session_watch::Observed::new`]).
    watchers: std::sync::Arc<crate::session_watch::Watchers>,
    /// The text the window harness typed and has not submitted yet
    /// ([`Typed`]): this process's alone, never carried.
    typed: Option<Typed>,
    /// How many records the ledger has taken ([`TurnLedger::push`]): what a
    /// carry of it last saw, so the crash journal's control slot is
    /// rewritten only when it moved (`crash_restore`, increment 7).
    #[cfg(any(unix, test))]
    generation: u64,
    /// Told after every record the ledger takes ([`TurnLedger::watch`]):
    /// the session's crash journal, whose worker then carries the ledger
    /// into the session's region. Called under the ledger's lock, so it
    /// takes no lock a holder of another could be waiting on.
    watch: Option<LedgerWatch>,
}

/// What a ledger tells after every record it takes ([`TurnLedger::watch`]).
pub(crate) type LedgerWatch = std::sync::Arc<dyn Fn() + Send + Sync>;

impl TurnLedger {
    /// Append one record, evicting one past the cap: the oldest, or — while
    /// the window harness's records are more than [`HARNESS_SHARE`] of them
    /// — the oldest of those, wherever it is. One taken from inside the ring
    /// raises [`Self::low_id`] past its id (`shed_below`). The caller keeps
    /// the ids rising ([`TurnLedger::record`] does, for a live turn), and
    /// the ring stays in id order.
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

    /// Tell `watch` after every record this ledger takes from here on (the
    /// session's crash journal: [`crate::crash_restore::SessionJournal::carry_ledger`]).
    #[cfg(any(unix, test))]
    pub(crate) fn watch(&mut self, watch: LedgerWatch) {
        self.watch = Some(watch);
    }

    /// How many records the ledger has taken: it moves exactly when its
    /// records change (a record lands, or one is evicted with it). A carry
    /// also says the turns still open and the process's turn-id count
    /// ([`Self::carry_floor`]), which move without it: the crash journal does
    /// not carry them again for that, since the region's own count word holds
    /// every id the process mints and the recovery raises every carried floor
    /// past it (`crash_restore::note_turn_ids`, `continue_carried_turns`).
    #[cfg(any(unix, test))]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a carry of this ledger says anything: a record, or a floor
    /// below which it no longer vouches for the turn ids.
    #[cfg(any(unix, test))]
    pub(crate) fn carries_anything(&self) -> bool {
        !self.records.is_empty() || self.unheld_below > 0 || self.shed_below > 0
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
    ///
    /// It wakes the session's watchers for a record
    /// ([`crate::session_watch::Watchers::ring_record`]): the record is the
    /// scannable state.
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

    /// The window harness typed `typed` and has not submitted it: kept, in
    /// place of any text before it, for its connection's Enter
    /// ([`Self::take_typed`]).
    pub(crate) fn typed(&mut self, typed: Typed) {
        self.typed = Some(typed);
    }

    /// The text the window harness typed and has not submitted, taken: by
    /// the next write that reaches the session, which submits it or leaves
    /// it to no record, and by the end of the hand it was typed under.
    pub(crate) fn take_typed(&mut self) -> Option<Typed> {
        self.typed.take()
    }

    /// How many turns this session has retained (for the `who` verb's `turns=`).
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// The ledger a self-update handoff carried from the previous process:
    /// its records, each marked [`TurnRecord::carried`], in id order (as
    /// [`TurnLedger::since`] seeks), and no more than [`LEDGER_CAP`] of
    /// them, evicted as [`Self::push`] evicts. A sender whose ledger was out
    /// of order (an older build recorded a preempted turn after the one that
    /// replaced it) is put back in order rather than losing the record; of
    /// two records under one id, the first is kept. `unheld_below`: turn ids
    /// below it may have named records the carry could not bring (0: none).
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

    /// [`Self::carried`] with the sender's `shed_below` (0: none): the ids
    /// below it may have named a harness record its ring evicted from
    /// inside, and still may here.
    pub(crate) fn with_shed_below(mut self, below: u64) -> Self {
        self.shed_below = self.shed_below.max(below);
        self
    }

    /// Every retained record, oldest first (the handoff's export).
    #[cfg(any(unix, test))]
    pub(crate) fn records(&self) -> impl Iterator<Item = &TurnRecord> {
        self.records.iter()
    }

    /// Turn ids below this may have named a harness record evicted from
    /// inside the ring (see the field); 0: none. The handoff's export
    /// carries it.
    #[cfg(any(unix, test))]
    pub(crate) const fn shed_below(&self) -> u64 {
        self.shed_below
    }

    /// Turn ids below this may have named records the ledger never got (see
    /// the field); 0: none. Replies read [`TurnLedger::live_floor`].
    #[cfg(all(test, unix))]
    pub(crate) const fn unheld_below(&self) -> u64 {
        self.unheld_below
    }

    /// The floor ([`TurnLedger::unheld_below`]) until the records it lost would have aged out
    /// of a ledger carried whole: once this ledger is full and its oldest record is at or past
    /// the floor, it is stale and `None`, so one update's floor does not ride every later
    /// `history` reply and export. Records at or past the floor alone do not make it stale:
    /// every turn after a dropped ledger, and every record of a halved carry, is at or past it.
    pub(crate) fn live_floor(&self) -> Option<u64> {
        let floor = self.unheld_below;
        let aged_out = self.records.len() >= LEDGER_CAP
            && self.records.front().is_some_and(|front| front.id >= floor);
        (floor > 0 && !aged_out).then_some(floor)
    }

    /// The `unheld_below` a handoff's export carries: the live floor, raised past every id a
    /// turn still open on this session can record under. Such a turn records into this
    /// process's ledger after the export (a parked reader freezes its screen, so an idle settle
    /// latches), and the successor never sees that record.
    ///
    /// One open turn past every record records under its own id. A preempted turn (at or
    /// below a record) or one of several open can land behind a newer record and be
    /// re-numbered by [`TurnLedger::record`] with an id minted after the export, up to
    /// `ceiling` (the turn-id count the handoff's manifest carries, which the Commit holds the
    /// mint to); the carry then vouches for no id up to `ceiling`.
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

    /// The highest recorded turn id, or `None` when empty: the events digest seeds its
    /// watermark here so it streams only turns that land after subscription.
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.records.back().map(|r| r.id)
    }

    /// The newest record the window harness did not type ([`TurnBy::Harness`]
    /// aside, carried or not): the turn the window's companion reacts to.
    /// aterm's own turns — a `keep going`, a done check — are no news in the
    /// room, as presence reads them (`TurnFact::aterm`).
    pub(crate) fn newest_not_harness(&self) -> Option<&TurnRecord> {
        self.records.iter().rev().find(|r| r.by != TurnBy::Harness)
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
    /// higher of the two. A harness record evicted from inside the ring
    /// (`shed_below`, [`HARNESS_SHARE`]) raises it the same way: the
    /// records past it are whole.
    pub(crate) fn low_id(&self) -> Option<u64> {
        let floor = self.unheld_below.max(self.shed_below);
        let floor = (floor > 0).then_some(floor);
        match (self.records.front().map(|r| r.id), floor) {
            (Some(front), Some(floor)) => Some(front.max(floor)),
            (front, floor) => front.or(floor),
        }
    }

    /// Records with `id > after`, oldest first (the events digest's scan, and `history
    /// since=<id>`); `None`: every retained record. Ids strictly rise, so this is a suffix
    /// that `partition_point` finds in O(log n): the digest runs it every 250 ms per watched
    /// target. A watermark below the retained low-water yields every record, and
    /// [`TurnLedger::low_id`] reports the eviction.
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
/// `timeout` or `hangup` (what `cmd_turn` records) or [`STATUS_PRESSED`] (the
/// harness's `send` and `key enter`, and a `turn settle=submitted`); `None` for
/// any other word.
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

/// Milliseconds since the process epoch (a lazily-pinned monotonic `Instant`).
/// Monotonic and cheap; used for turn `started_ms`/`dur_ms`, not wall-clock.
/// The one process clock, shared with the fabric's mail rows.
#[cfg(not(test))]
pub(crate) use aterm_gui_timing::now_ms;

/// [`aterm_gui_timing::now_ms`], moved on by what this test thread
/// [`advance_for_test`]ed: a test that needs an agent's 45 s of busy time
/// on the A/B book (`harness_ab`, design §11.2.5) moves the clock on rather
/// than waiting for it. Still monotonic: it only ever moves forward.
#[cfg(test)]
pub(crate) fn now_ms() -> u64 {
    aterm_gui_timing::now_ms().saturating_add(ADVANCED.with(std::cell::Cell::get))
}

#[cfg(test)]
thread_local! {
    /// How far this test thread moved the ledger clock on.
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

    /// The turn-id ceiling the unit tests export under: past every id here.
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

    /// Every word `by=` prints reads back as the same caller, the shape a
    /// carried record is held to: the reserved `fleet`, an older sender's
    /// missing field and anything not a word or a sid this build mints read
    /// as `-`, never as a sid.
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

    /// THE CRASH JOURNAL'S WATCH (`crash_restore`'s increment 7): every
    /// record the ledger takes moves its generation and tells the watch once,
    /// an eviction past the cap included; nothing else does (a turn begun or
    /// abandoned, a typed text). A carried ledger counts its records, and says
    /// whether it carries anything: a record or a floor.
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

    /// The window's companion reacts to the newest turn the window harness
    /// did not type ([`TurnLedger::newest_not_harness`]): a `keep going`
    /// (`status=pressed`, carried or not) after a manager's turn leaves the
    /// manager's as the room's. Negative control: a manager's turn after the
    /// harness's is the newest again.
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

    /// A LONG RUN OF THE HARNESS'S OWN TURNS NEVER EVICTS THE MANAGER'S:
    /// past [`HARNESS_SHARE`] the harness's records make room among
    /// themselves, so a manager's turn followed by twice [`LEDGER_CAP`]
    /// continuations is still held, beside the harness's newest. The ring
    /// stays in id order, and [`TurnLedger::low_id`] rises past the records
    /// shed from inside it, so a resume anchored at the manager's turn is
    /// told it missed some. Once the manager's turns come again, they push
    /// the harness's out down to the share, and then the ring is plain
    /// drop-oldest. NEGATIVE CONTROLS: a run of everyone else's turns, or of
    /// the harness's alone, sheds nothing from inside (`low_id` is the
    /// front).
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
        // The manager's turns come again: the harness's give way to the share,
        // then the manager's oldest goes like any record.
        let resumed = last + 1;
        for id in resumed..resumed + cap - 1 - HARNESS_SHARE as u64 {
            l.push(rec(id));
        }
        assert_eq!(l.since(None).next().map(|r| r.id), Some(1), "still held");
        l.push(rec(resumed + cap));
        assert_ne!(l.since(None).next().map(|r| r.id), Some(1), "drop-oldest");
        let held = ids(&l);
        assert!(held.windows(2).all(|w| w[0] < w[1]), "in id order");
        // NEGATIVE CONTROLS: plain drop-oldest, nothing shed from inside.
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
        // in the gaps between them, at the high, and past it.
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

    /// A turn a `lease release force` preempted finishes after the turn that took its slot:
    /// it is re-numbered past the newer record, so ids rise in landing order and a watcher
    /// past the newer turn is sent it.
    #[test]
    fn a_preempted_turn_that_records_late_is_renumbered_and_streamed() {
        let mut l = TurnLedger::default();
        l.push(rec(5));
        l.begin(10);
        l.begin(11);
        assert_eq!(l.record(rec(11), || unreachable!("in order")), 11);
        // Exported now, turn 10 can only record behind 11, under an id still to be minted:
        // the carry vouches for none up to the ceiling.
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

        // An out-of-order sender's ledger is put back in order, not left a record short.
        let carried = TurnLedger::carried(vec![rec(5), rec(11), rec(10), rec(11)], 0);
        let ids: Vec<u64> = carried.since(None).map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 10, 11]);
        assert_eq!(carried.since(Some(9)).count(), 2, "turn 10 is found");
    }

    /// A turn still open when the handoff exports the ledger records into the exiting
    /// process: the export carries a floor past it, and the adopted ledger's low-water is that
    /// floor even with older records held.
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
        // An abandoned turn owes no record.
        l.abandon(4);
        assert_eq!(l.carry_floor(CEILING), 0);
    }

    /// A floor stays live until [`LEDGER_CAP`] records at or past it have landed, then is
    /// neither reported nor carried on. Records at or past it alone are not enough: every turn
    /// after a dropped ledger, and every record of a halved carry, is at or past it.
    #[test]
    fn a_floor_goes_stale_once_the_records_under_it_would_have_aged_out() {
        let cap = LEDGER_CAP as u64;
        let empty = TurnLedger::carried(Vec::new(), 10);
        assert_eq!(empty.live_floor(), Some(10), "an empty ledger's floor");
        assert_eq!(empty.carry_floor(CEILING), 10);
        // A dropped ledger, then one new turn: its id is the floor itself.
        let mut dropped = TurnLedger::carried(Vec::new(), 10);
        dropped.push(rec(10));
        assert_eq!(dropped.live_floor(), Some(10), "one turn past a drop");
        assert_eq!(
            dropped.carry_floor(CEILING),
            10,
            "carried on by a second update"
        );
        // A halved carry: every held record is past the floor.
        let at = TurnLedger::carried((10..=12).map(rec).collect(), 10);
        assert_eq!(at.live_floor(), Some(10), "at the oldest held record");
        assert_eq!(at.low_id(), Some(10), "the low-water says it too");
        // An open turn at the export: records held under the floor.
        let mut l = TurnLedger::carried((1..=3).map(rec).collect(), 10);
        assert_eq!(l.live_floor(), Some(10), "above the oldest held record");
        // One record short of LEDGER_CAP at or past the floor: a ledger
        // carried whole would still hold turn 9.
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
        // An open turn still raises the carry over a stale floor.
        l.begin(10 + cap);
        assert_eq!(l.carry_floor(CEILING), 11 + cap);
    }

    #[test]
    fn text_clamps_on_boundary() {
        // 801 bytes whose MAX_TEXT-th byte is inside an 'é': the clamp backs
        // off to the boundary before it, one byte short of the cap.
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
