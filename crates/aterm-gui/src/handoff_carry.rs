// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's CONTROL CARRY: what a session's drivers rely on
//! that lived only in the outgoing process — its TURN LEDGER and the TAIL of
//! its alt-screen archive — handed to the process that adopts the session, in
//! one JSON sidecar per session beside the manifest.
//!
//! # Why
//!
//! Measured 2026-09-14, live: after the 0.84 → 0.85 handoff a worker's
//! `history` was empty, `offscreen` restarted at a new origin (`epoch=2`), and
//! the first rows the new archive kept were the composer's rule and footer.
//! A manager's `aterm drive report` could not find the turn it had just
//! driven: the ledger and the archive were memory of a process that had
//! exited. (The composer half is `aterm-core`'s, fixed on the receiving side.)
//!
//! # Shape
//!
//! * The FREEZE pays for [`capture_head`] alone, per session, under the
//!   terminal lock its checkpoint is taken under: the archive's fence and
//!   counters, and the differ's screen-sized state — nothing proportional to
//!   the archive, nothing that can fail the capture.
//! * The handoff WORKER, readers still parked, runs [`export`] just before
//!   `seamless::write_outgoing`: it clones the ledger, picks the archive's
//!   tail — the rows after the earliest mark among the newest [`TAIL_TURNS`]
//!   submitted turns (what `aterm drive report` reads back), and never fewer
//!   than the rows the differ may still point back at (`aterm-core` widens it
//!   to its reach) — and attaches at most [`CARRY_ARCHIVE_BYTES`] of them
//!   while the fence still holds, with the differ's state when the freeze had
//!   no time for it. A lock another thread keeps, or an archive that moved,
//!   carries the counters alone: the rows count as lost, which a reader is
//!   told. A ledger that could not be carried whole says which turn ids it
//!   no longer vouches for, so a resumed `subscribe … since-turn=` is told.
//! * `write_outgoing` writes it `0600`, `O_CREAT|O_EXCL|O_NOFOLLOW`, as
//!   `seamless-<pid>-<nonce>.s<id>.ctl`, and names it in the session's
//!   manifest record as `control = "<len> <sha256hex>"`; the manifest also
//!   carries the turn-id counter. It is in NEITHER adoption-proof digest and
//!   no schema moved: an older receiver skips both keys and adopts exactly as
//!   before, and the sender unlinks the sidecar once the proof checks out — so
//!   a receiver reads its sidecars BEFORE it publishes its proof. A HELD pane
//!   (`--hold`, its command ended) is carried the same way, its sidecar
//!   `seamless-<pid>-<nonce>.h<id>.ctl` named on its `HeldPaneCarry::control`
//!   (round eight of the update audit, finding 730).
//! * `take_incoming` reads and deletes it, checks its length and sha, and
//!   [`decode`]s it; `spawn_session` installs it into the adopted engine and
//!   ledger ([`ControlCarry::ledger`], [`ControlCarry::install`]). Anything
//!   wrong with it — missing, truncated, oversized, a bad sha, bad JSON —
//!   drops the carry and nothing else: the session adopts as it did before.
//!
//! # Privacy
//!
//! The archive was built to live in memory. Carrying it puts scrolled-off text
//! and submitted turn text on disk for the length of one handoff: in the
//! per-user `0700` control dir, `0600`, never through a symlink, deleted by the
//! receiver as it reads it and by the sender once the proof checks out, and
//! swept at the next start when a crash left it behind
//! (`seamless::sweep_dead_handoff_leftovers`). A receiver with
//! an archive switched off drops the carried rows. The ledger's text is carried
//! as the ledger holds it, so a redacted operator turn stays redacted.

use std::sync::Arc;
#[cfg(any(unix, test))]
use std::sync::Mutex;
#[cfg(any(unix, test))]
use std::time::{Duration, Instant};

#[cfg(any(unix, test))]
use aterm_core::terminal::AltArchiveFence;
use aterm_core::terminal::{
    AltArchiveCarry, AltArchiveDiffer, AltArchiveGap, AltArchiveGapKind, AltArchiveImport, Terminal,
};

use crate::turn_ledger::{ArchMark, TurnLedger, TurnRecord};

/// The most archived rows one session carries, charged as the live archive
/// charges them (text plus 32 bytes a row). The newest rows win.
#[cfg(any(unix, test))]
pub(crate) const CARRY_ARCHIVE_BYTES: usize = 1024 * 1024;
/// The largest sidecar a receiver reads: the rows above, the ledger (512
/// records of at most 512 bytes of text) and the differ's state, with JSON's
/// escaping on top. A bigger one is dropped unread.
pub(crate) const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;
/// The most sidecar bytes one handoff writes, and one receiver reads, across
/// every session — the receiver decodes them in early `main`, inside the
/// freeze the user sees. Every session's ledger and counters are set aside
/// first; past it, sessions carry fewer rows (then no differ state) — and
/// only a desk whose ledgers alone overflow it sheds ledger records, every
/// session its oldest, the longest ledgers alike ([`export`]).
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 16 * 1024 * 1024;
/// How many of the newest SUBMITTED turns' marks the carried tail reaches back
/// to: `aterm drive report` reads `history 8` and starts from the newest
/// submitted turn among them.
#[cfg(any(unix, test))]
pub(crate) const TAIL_TURNS: usize = 8;
/// The sidecar's layout version. Unknown fields are skipped and missing ones
/// default, so it moves only when a field changes meaning; a receiver drops a
/// version it does not know.
const WIRE_VERSION: u64 = 1;
/// The highest turn id a handoff carries: what the manifest can hold (a TOML
/// integer is an `i64`). A carried record above it is left out, and the count
/// is never continued past it (`control::raise_turn_ids`), so the next `turn`
/// id — minted with a `+ 1` — neither overflows nor wraps below the ids
/// before it.
pub(crate) const MAX_TURN_ID: u64 = i64::MAX as u64;
/// How long the worker waits, over the WHOLE export, for locks other threads
/// hold before it carries less. The readers are parked, so the only holders
/// are brief (a control read, a `turn` recording its record, the scrollback
/// compressor) — and the terminal stays frozen until Commit, so the export
/// never waits long on anyone.
#[cfg(any(unix, test))]
const LOCK_PATIENCE: Duration = Duration::from_millis(50);

/// One session's capture, taken in the freeze and exported on the worker.
#[cfg(any(unix, test))]
pub(crate) struct CarrySource {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    turns: Arc<Mutex<TurnLedger>>,
    fence: AltArchiveFence,
    /// The counters-only carry (plus the differ's state when there was time):
    /// what is carried when the rows cannot be.
    head: AltArchiveCarry,
    /// Whether the differ's state may travel ([`Self::without_differ`]).
    keep_differ: bool,
}

#[cfg(any(unix, test))]
impl CarrySource {
    /// The session this capture belongs to — what the park's capture matches
    /// against its carried screens, so a session whose screen it lowered below
    /// the exact rungs after the fact goes without its differ's state too (the
    /// 2026-09-22/23 update audit, plan P0-1e; [`Self::without_differ`]).
    #[cfg(unix)]
    pub(crate) fn local_id(&self) -> u64 {
        self.local_id
    }

    /// Whether the differ's state may travel with this session.
    #[cfg(test)]
    pub(crate) fn carries_differ(&self) -> bool {
        self.keep_differ
    }

    /// Carry this session without the differ's state: its screen was not
    /// carried as the program drew it (`CarryRung::keeps_differ_state`), so
    /// that state describes nothing the successor shows. The ledger and the
    /// archive's rows still travel.
    pub(crate) fn without_differ(&mut self) {
        self.head.differ = None;
        self.keep_differ = false;
    }
}

/// THE FREEZE'S SHARE: the archive's fence and counters, and — with `differ`
/// — the differ's screen-sized state, under the terminal lock the caller
/// already holds for this session's checkpoint (so both describe the same
/// screen). Two `Arc` clones and a screen of row text; it cannot fail.
#[cfg(any(unix, test))]
pub(crate) fn capture_head(
    local_id: u64,
    terminal: &Terminal,
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    differ: bool,
) -> CarrySource {
    let (fence, head) = terminal.alt_archive_carry_head(differ);
    CarrySource {
        local_id,
        term: Arc::clone(term),
        turns: Arc::clone(turns),
        fence,
        head,
        keep_differ: true,
    }
}

/// Which turn-status words the receiving build reads, so [`export_for`]
/// spells every record in its vocabulary (round eight of the update audit,
/// finding 714). A receiver leaves out a record whose word it does not know.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusWords {
    /// This build's: `settled`, `timeout` and `hangup`. Every build from
    /// [`READS_HANGUP_FROM_BUILD`] on reads all three.
    Current,
    /// A build from before `hangup` — a rollback, an older bundle put back, a
    /// channel switch. 53bc0f2f7 added the word after v0.98.0 was cut: v0.98.0
    /// and every build before it read `settled` and `timeout` alone, and leave
    /// a `hangup` record out while still vouching for its id.
    PreHangup,
}

/// The first build whose turn ledger reads `hangup`: v0.100.0's claim in
/// `RELEASES.ledger`. No v0.99.0 was cut, and v0.98.0 (1790607299) predates
/// 53bc0f2f7, which added the word. A development build's number is its
/// HEAD's commit time, so one built between 53bc0f2f7 and this claim is
/// handed `timeout`, a word it also reads.
#[cfg(any(unix, test))]
pub(crate) const READS_HANGUP_FROM_BUILD: u64 = 1_790_807_622;

#[cfg(any(unix, test))]
impl StatusWords {
    /// The words of the successor at `target_build`, keyed on the first build
    /// that reads `hangup` and not on whether the target is older than this
    /// build, as the screen carry's ceilings (`seamless::WireCaps::for_hop`)
    /// and the history carry (`handoff_history::exports_for_target`) are: a
    /// rollback to v0.100.0 or later reads `hangup`, carried it as it is
    /// before this export respelled any word, and still does.
    #[must_use]
    pub(crate) fn for_target(target_build: u64) -> Self {
        if target_build < READS_HANGUP_FROM_BUILD {
            Self::PreHangup
        } else {
            Self::Current
        }
    }

    /// `status` in these words. A build from before `hangup` has no word for a
    /// turn whose caller hung up during its settle (that build never recorded
    /// one); its nearest is `timeout`, a settle nobody saw end. The record —
    /// submitted, its text, its archive mark — is what `aterm drive report`
    /// anchors on, and that crosses whole.
    fn spell(self, status: &'static str) -> &'static str {
        match (self, status) {
            (Self::PreHangup, "hangup") => "timeout",
            _ => status,
        }
    }
}

/// What a session's carry left behind on its way out of [`export_noting`],
/// for the log line that names it (round eight of the update audit, finding
/// 731): every other export, write and adoption loss is logged by session and
/// cause, and these two lock arms were not.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarryLoss {
    /// Another thread kept the session's turn ledger past the export's
    /// patience: it hands off without its ledger, every id below `unvouched`
    /// said as unheld.
    LedgerBusy { unvouched: u64 },
    /// Another thread kept the session's engine past the export's patience:
    /// its archive crosses as counters alone.
    TerminalBusy,
    /// The archive moved after the freeze took its fence: its counters alone.
    ArchiveMoved,
}

#[cfg(any(unix, test))]
impl CarryLoss {
    /// The WARN line naming this loss for session `local_id`, one sentence
    /// read in aterm.log.
    fn line(self, local_id: u64) -> String {
        match self {
            Self::LedgerBusy { unvouched } => format!(
                "update apply: session {local_id}'s turn ledger was held by another thread past \
                 the export's {LOCK_PATIENCE:?}; it hands off without its ledger, and says every \
                 turn id below {unvouched} as one it cannot vouch for"
            ),
            Self::TerminalBusy => format!(
                "update apply: session {local_id}'s terminal was held by another thread past the \
                 export's {LOCK_PATIENCE:?}; its archive crosses as counters alone, its rows \
                 counted lost"
            ),
            Self::ArchiveMoved => format!(
                "update apply: session {local_id}'s archive moved after the freeze took it; it \
                 crosses as counters alone, its rows counted lost"
            ),
        }
    }

    fn log(self, local_id: u64) {
        aterm_log::warn!("{}", self.line(local_id));
    }
}

/// [`export_for`] in this build's own words: what a successor at least as new
/// as this build reads.
#[cfg(test)]
pub(crate) fn export(sources: &[CarrySource], turn_id_ceiling: u64) -> Vec<(u64, Vec<u8>)> {
    export_for(sources, turn_id_ceiling, StatusWords::Current)
}

/// THE WORKER'S SHARE, just before the manifest is written: one sidecar per
/// session, `(local_id, JSON)`, its records spelled in `words`, the receiving
/// build's. Never fails the handoff — a session whose sidecar would not fit
/// carries less, and in the end nothing.
/// `turn_id_ceiling` is the turn-id count the manifest carries: the most
/// this process mints before it exits, so the most a record that lands here
/// after the export can be numbered.
///
/// THE AGGREGATE IS SPENT BY PRIORITY, not first come, first served (round
/// seven, finding 37): every session's CORE — its ledger and the archive's
/// counters, what `aterm drive report`, `history` and a resumed `since-turn=`
/// stand on — is set aside first, and the differ's state and the archive's
/// rows share what is left, in pool order. Spent greedily, a crowded desk's
/// first sessions carried their optional rows and its last ones lost their
/// whole ledger, the same sessions on every update. When the cores ALONE
/// overflow it, they share it ([`core_shares`]): every session keeps its
/// newest records, the longest ledgers are cut alike, and each carry names
/// the turn ids it shed (round eight, finding 41).
#[cfg(any(unix, test))]
pub(crate) fn export_for(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    words: StatusWords,
) -> Vec<(u64, Vec<u8>)> {
    export_noting(sources, turn_id_ceiling, words, |local_id, loss| {
        loss.log(local_id);
    })
}

/// [`export_for`], each [`CarryLoss`] handed to `noted` with its session.
#[cfg(any(unix, test))]
pub(crate) fn export_noting(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    words: StatusWords,
    mut noted: impl FnMut(u64, CarryLoss),
) -> Vec<(u64, Vec<u8>)> {
    let patience = Instant::now() + LOCK_PATIENCE;
    // Every id this process may still number a record with.
    let unvouched = turn_id_ceiling
        .max(crate::control::turn_ids_minted())
        .saturating_add(1);
    let mut taken = Vec::with_capacity(sources.len());
    for source in sources {
        // A ledger another thread keeps is carried as NOTHING it vouches for:
        // every turn id this process minted, or still mints, may name a
        // record of it. SAID, as every other carry loss is.
        let (turns, unheld_below) = match patiently(patience, || source.turns.try_lock()) {
            // A turn still open here records into THIS process after the
            // export: the carry does not vouch for the id it lands under.
            // Each record in the receiver's words.
            Some(ledger) => (
                ledger
                    .records()
                    .cloned()
                    .map(|mut record| {
                        record.status = words.spell(record.status);
                        record
                    })
                    .collect(),
                ledger.carry_floor(turn_id_ceiling),
            ),
            None => {
                noted(source.local_id, CarryLoss::LedgerBusy { unvouched });
                (Vec::new(), unvouched)
            }
        };
        let mut archive = source.head.clone();
        if let Some(terminal) = patiently(patience, || source.term.try_lock()) {
            // The differ's state, when the freeze had no time for it — only
            // while nothing committed since (then it is the state the freeze
            // would have taken). Without it the adopting engine starts a new
            // baseline after a `restore` gap, and a report says `archive-gap`.
            if source.keep_differ {
                let _ = terminal.alt_archive_carry_differ(&mut archive, source.fence);
            }
            let from = tail_from(&turns, source.fence, archive.differ.as_ref());
            // `false` (the archive moved since the freeze) leaves the head:
            // counters only, the rows counted lost.
            if !terminal.alt_archive_carry_rows(
                &mut archive,
                source.fence,
                from,
                CARRY_ARCHIVE_BYTES,
            ) {
                noted(source.local_id, CarryLoss::ArchiveMoved);
            }
        } else {
            noted(source.local_id, CarryLoss::TerminalBusy);
        }
        let core = core_len(&turns, unheld_below, &archive);
        taken.push((source.local_id, turns, unheld_below, archive, core));
    }
    let cores: Vec<u64> = taken.iter().map(|(.., core)| *core).collect();
    let shares = core_shares(&cores, MAX_AGGREGATE_BYTES);
    // What the differs and the rows may spend, over every session's share.
    let mut spare = MAX_AGGREGATE_BYTES.saturating_sub(shares.iter().sum());
    let mut total = 0_u64;
    let (mut shed_sessions, mut shed_records) = (0_usize, 0_usize);
    let mut out = Vec::with_capacity(taken.len());
    for ((local_id, turns, unheld_below, archive, _), share) in taken.into_iter().zip(shares) {
        let room = MAX_SIDECAR_BYTES
            .min(share.saturating_add(spare))
            .min(MAX_AGGREGATE_BYTES.saturating_sub(total));
        if let Some((bytes, shed)) = encode_within(turns, unheld_below, archive, room) {
            let len = bytes.len() as u64;
            total = total.saturating_add(len);
            spare = spare.saturating_sub(len.saturating_sub(share));
            if shed > 0 {
                shed_sessions += 1;
                shed_records += shed;
            }
            out.push((local_id, bytes));
        } else {
            // SAID, not silent (round seven, finding 44): the session adopts
            // with an empty ledger and a fresh archive.
            aterm_log::warn!(
                "update apply: session {local_id}'s control carry does not fit what is left \
                 of the handoff's budget ({room} bytes); it hands off without its turn ledger \
                 and archive tail"
            );
        }
    }
    if shed_sessions > 0 {
        // COUNTED, and said: each of these carries names the ids it shed
        // (`history`'s `unheld_below=`, a resumed `since-turn=`'s GAP).
        aterm_log::warn!(
            "update apply: {shed_sessions} sessions' turn ledgers do not fit the handoff's \
             {MAX_AGGREGATE_BYTES}-byte control budget whole; they hand off their newest \
             records, {shed_records} older ones in all left behind"
        );
    }
    out
}

/// Each session's share of `budget` for its CORE ([`core_len`], `cores` in
/// pool order): the whole core while every core fits; past that, the most
/// every session can have alike — a core under that keeps all of it, and
/// what it leaves goes to the longer ones, cut alike (max-min fair). The
/// shares never sum past `budget`, and pool order buys nothing.
#[cfg(any(unix, test))]
fn core_shares(cores: &[u64], budget: u64) -> Vec<u64> {
    if cores.iter().fold(0_u64, |sum, c| sum.saturating_add(*c)) <= budget {
        return cores.to_vec();
    }
    let mut order: Vec<usize> = (0..cores.len()).collect();
    order.sort_by_key(|&i| cores[i]);
    let mut shares = vec![0; cores.len()];
    let mut left = budget;
    for (k, &i) in order.iter().enumerate() {
        let alike = left / (cores.len() - k) as u64;
        shares[i] = cores[i].min(alike);
        left -= shares[i];
    }
    shares
}

/// The size of a session's CORE sidecar — its whole ledger and the archive's
/// counters, without rows or the differ's state — as [`export`] sets it aside
/// (never more than one sidecar may hold: a ledger past that is shed anyway).
#[cfg(any(unix, test))]
fn core_len(turns: &[TurnRecord], unheld_below: u64, archive: &AltArchiveCarry) -> u64 {
    let mut core = counters_only(archive.clone());
    core.differ = None;
    encode(turns, unheld_below, &core).map_or(MAX_SIDECAR_BYTES, |bytes| {
        (bytes.len() as u64).min(MAX_SIDECAR_BYTES)
    })
}

/// The turn-id counter as the manifest can carry it (a TOML integer is an
/// `i64`; a count past that is not carried rather than failing the write).
#[cfg(any(unix, test))]
pub(crate) fn manifest_turn_id(minted: u64) -> Option<u64> {
    (minted > 0 && minted <= MAX_TURN_ID).then_some(minted)
}

/// The turn ledger an ADOPTED session starts with: the one its carry brought
/// ([`ControlCarry::ledger`]), or — when the handoff could not carry it (no
/// sidecar, a bad one) — an empty one that no longer vouches for any turn id
/// up to the count this process continued from, so a resumed `subscribe …
/// since-turn=` below it is told (`GAP … events-resync=`) rather than left to
/// believe no turn landed.
pub(crate) fn adopted_ledger(control: Option<&mut ControlCarry>) -> TurnLedger {
    match control {
        Some(control) => control.ledger(),
        None => TurnLedger::carried(
            Vec::new(),
            crate::control::turn_ids_minted().saturating_add(1),
        ),
    }
}

/// Where the carried tail starts: after the EARLIEST mark among the newest
/// [`TAIL_TURNS`] submitted turns (the newest turn when none was submitted)
/// that were minted under this archive's origin, and no later than the rows
/// the screen shows again. With no such turn, from the oldest row (the byte
/// cap then keeps the newest).
#[cfg(any(unix, test))]
fn tail_from(
    turns: &[TurnRecord],
    fence: AltArchiveFence,
    differ: Option<&AltArchiveDiffer>,
) -> u64 {
    let mut marks: Vec<ArchMark> = turns
        .iter()
        .rev()
        .filter(|t| t.submitted)
        .take(TAIL_TURNS)
        .map(|t| t.arch)
        .collect();
    if marks.is_empty() {
        marks.extend(turns.last().map(|t| t.arch));
    }
    let mut from = marks
        .iter()
        .filter(|m| m.origin == fence.origin)
        .map(|m| m.last.saturating_add(1))
        .min()
        .unwrap_or(fence.first);
    if let Some(d) = differ.filter(|d| d.debt > 0) {
        from = from.min(d.debt_at);
    }
    from
}

/// A `try_lock` (`attempt`, which names the lock so the lock-order census
/// sees whose it is) retried until `until`: `None` when another thread keeps
/// the lock past then. A poisoned lock's data is still the data.
#[cfg(any(unix, test))]
fn patiently<G>(
    until: Instant,
    mut attempt: impl FnMut() -> Result<G, std::sync::TryLockError<G>>,
) -> Option<G> {
    loop {
        match attempt() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    }
}

/// Encode, shedding what does not fit `room`: the rows first (counters
/// only), then the differ's state, then the ledger's OLDEST records, no more
/// of them than the room needs (the ids shed are ones the carried ledger no
/// longer vouches for). With the records shed, if any.
///
/// Each record's bytes are measured once, and the cut moves past as many as
/// the last encode ran over (at least one): an encode or two, where shedding
/// half the ledger at a time left up to half the room unspent.
#[cfg(any(unix, test))]
fn encode_within(
    turns: Vec<TurnRecord>,
    unheld_below: u64,
    archive: AltArchiveCarry,
    room: u64,
) -> Option<(Vec<u8>, usize)> {
    let fits = |b: &Vec<u8>| b.len() as u64 <= room;
    let mut archive = archive;
    if let Some(bytes) = encode(&turns, unheld_below, &archive).filter(fits) {
        return Some((bytes, 0));
    }
    archive = counters_only(archive);
    if let Some(bytes) = encode(&turns, unheld_below, &archive).filter(fits) {
        return Some((bytes, 0));
    }
    archive.differ = None;
    let mut lens: Option<Vec<u64>> = None;
    let mut cut = 0_usize;
    loop {
        let floor = cut
            .checked_sub(1)
            .and_then(|newest_shed| turns.get(newest_shed))
            .map_or(unheld_below, |t| unheld_below.max(t.id.saturating_add(1)));
        let bytes = encode(&turns[cut..], floor, &archive)?;
        let over = (bytes.len() as u64).saturating_sub(room);
        if over == 0 {
            return Some((bytes, cut));
        }
        if cut == turns.len() {
            return None;
        }
        // A record's bytes in the array: its own JSON and a comma.
        let lens = lens.get_or_insert_with(|| {
            turns
                .iter()
                .map(|t| aterm_json::to_vec(&turn_wire(t)).map_or(1, |b| b.len() as u64 + 1))
                .collect()
        });
        let mut freed = 0_u64;
        while cut < turns.len() && (freed == 0 || freed < over) {
            freed = freed.saturating_add(lens[cut]);
            cut += 1;
        }
    }
}

/// The carry with its rows left out: counted lost, the indices unchanged.
#[cfg(any(unix, test))]
fn counters_only(mut c: AltArchiveCarry) -> AltArchiveCarry {
    let last = c.last();
    c.lost = c.lost.saturating_add(c.rows.len() as u64);
    c.first = last + 1;
    c.rows = Vec::new();
    c.gaps.retain(|g| g.after >= last);
    c
}

// ------------------------------------------------------------------ wire

/// The sidecar. Every field defaults when absent and unknown ones are
/// skipped, so a newer sender's additions cost an older receiver nothing.
/// Numbers are `u64` on the wire whatever they are in memory, so a value out
/// of its in-memory range spoils only its own part (the differ's state is
/// then dropped whole by `AltArchive::import`), never the parse.
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Wire {
    version: u64,
    turns: Vec<TurnWire>,
    /// Turn ids below this may have named records `turns` does not hold (a
    /// ledger that could not be carried whole); 0: none.
    unheld_below: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<ArchiveWire>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct TurnWire {
    id: u64,
    started_ms: u64,
    dur_ms: u64,
    submitted: bool,
    /// `settled`, `timeout` or `hangup` — a word, never a code.
    status: String,
    text: String,
    screen_hash: u64,
    seq: u64,
    arch_origin: u64,
    arch_last: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct ArchiveWire {
    origin: u64,
    first: u64,
    lost: u64,
    floor: u64,
    epoch: u64,
    enabled: bool,
    gaps: Vec<GapWire>,
    rows: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    differ: Option<DifferWire>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct GapWire {
    after: u64,
    kind: String,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct DifferWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    prev: Option<FrameWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chrome: Option<u64>,
    pin: u64,
    debt: u64,
    debt_at: u64,
    below: Vec<String>,
    esu_seen: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct FrameWire {
    cols: u64,
    rows: Vec<String>,
}

/// One record as the sidecar carries it.
#[cfg(any(unix, test))]
fn turn_wire(t: &TurnRecord) -> TurnWire {
    TurnWire {
        id: t.id,
        started_ms: t.started_ms,
        dur_ms: t.dur_ms,
        submitted: t.submitted,
        status: t.status.to_string(),
        text: t.text.clone(),
        screen_hash: t.screen_hash,
        seq: t.seq,
        arch_origin: t.arch.origin,
        arch_last: t.arch.last,
    }
}

#[cfg(any(unix, test))]
fn encode(turns: &[TurnRecord], unheld_below: u64, archive: &AltArchiveCarry) -> Option<Vec<u8>> {
    let wire = Wire {
        version: WIRE_VERSION,
        unheld_below,
        turns: turns.iter().map(turn_wire).collect(),
        archive: Some(ArchiveWire {
            origin: archive.origin,
            first: archive.first,
            lost: archive.lost,
            floor: archive.floor,
            epoch: u64::from(archive.epoch),
            enabled: archive.enabled,
            gaps: archive
                .gaps
                .iter()
                .map(|g| GapWire {
                    after: g.after,
                    kind: g.kind.as_str().to_string(),
                })
                .collect(),
            rows: archive.rows.iter().map(|r| r.to_string()).collect(),
            differ: archive.differ.as_ref().map(|d| DifferWire {
                prev: d.prev.as_ref().map(|(cols, rows)| FrameWire {
                    cols: u64::from(*cols),
                    rows: rows.clone(),
                }),
                chrome: d.chrome.map(|c| c as u64),
                pin: d.pin as u64,
                debt: d.debt as u64,
                debt_at: d.debt_at,
                below: d.below.clone(),
                esu_seen: d.esu_seen,
            }),
        }),
    };
    aterm_json::to_vec(&wire).ok()
}

/// A decoded sidecar, ready to install into the adopted session.
#[derive(Debug, Default)]
pub(crate) struct ControlCarry {
    /// The carried ledger's records, oldest first.
    pub turns: Vec<TurnRecord>,
    /// Turn ids below this may have named records `turns` does not hold (the
    /// ledger could not be carried whole); 0: none.
    pub unheld_below: u64,
    /// The carried archive.
    pub archive: Option<AltArchiveCarry>,
}

impl ControlCarry {
    /// The carried ledger, every record marked carried (`history` prints
    /// `carried=1`), for the adopted session's `SessionCtx`.
    pub(crate) fn ledger(&mut self) -> TurnLedger {
        TurnLedger::carried(std::mem::take(&mut self.turns), self.unheld_below)
    }

    /// Install the carried archive into the adopted engine, which has just
    /// restored the checkpoint taken with it. Never panics
    /// (`AltArchive::import`).
    pub(crate) fn install(self, terminal: &mut Terminal) -> Option<AltArchiveImport> {
        self.archive
            .map(|archive| terminal.alt_archive_import(archive))
    }

    /// Adopt this carry without the differ's state: the successor repaints
    /// the session, so the screen that state describes is not the one it
    /// shows. The ledger and the archive's rows are kept.
    pub(crate) fn without_differ(&mut self) {
        if let Some(archive) = self.archive.as_mut() {
            archive.differ = None;
        }
    }

    /// The highest carried turn id.
    pub(crate) fn high_turn_id(&self) -> Option<u64> {
        self.turns.iter().map(|t| t.id).max()
    }
}

/// Decode a sidecar; `None` for anything that is not one this build reads
/// ([`decode_why`] says which).
#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Option<ControlCarry> {
    decode_why(bytes).ok()
}

/// Decode a sidecar, or say why it is not one this build reads. A record
/// with a status word this build does not print, or an id past
/// [`MAX_TURN_ID`], is left out — and the carry no longer vouches for its id
/// (`unheld_below` rises past it, as for a record the sender shed), so a
/// resumed `since-turn=` below it is told and `history` says it (round eight,
/// finding 714). A gap of a kind it does not know is left out.
pub(crate) fn decode_why(bytes: &[u8]) -> Result<ControlCarry, &'static str> {
    let wire: Wire =
        aterm_json::from_slice(bytes).map_err(|_| "its sidecar is not the carry's JSON")?;
    if wire.version != WIRE_VERSION {
        return Err("its sidecar is a layout version this build does not read");
    }
    let mut unheld_below = wire.unheld_below;
    let turns = wire
        .turns
        .into_iter()
        .filter_map(|t| {
            let status = (t.id <= MAX_TURN_ID)
                .then(|| crate::turn_ledger::status_word(&t.status))
                .flatten();
            let Some(status) = status else {
                // Held to the range below, with the sender's own floor.
                unheld_below = unheld_below.max(t.id.saturating_add(1));
                return None;
            };
            Some(TurnRecord {
                id: t.id,
                started_ms: t.started_ms,
                dur_ms: t.dur_ms,
                submitted: t.submitted,
                status,
                text: crate::turn_ledger::clamp_text(&t.text),
                screen_hash: t.screen_hash,
                seq: t.seq,
                arch: ArchMark {
                    origin: t.arch_origin,
                    last: t.arch_last,
                },
                carried: true,
            })
        })
        .collect();
    let archive = wire.archive.map(|a| AltArchiveCarry {
        origin: a.origin,
        first: a.first,
        lost: a.lost,
        floor: a.floor,
        // Informational (a baseline generation that wraps): truncation is
        // the same wrap.
        epoch: a.epoch as u32,
        enabled: a.enabled,
        gaps: a
            .gaps
            .into_iter()
            .filter_map(|g| {
                Some(AltArchiveGap {
                    after: g.after,
                    kind: AltArchiveGapKind::parse(&g.kind)?,
                })
            })
            .collect(),
        rows: a.rows.into_iter().map(Arc::from).collect(),
        differ: a.differ.map(|d| {
            let wide = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
            AltArchiveDiffer {
                // An out-of-range width becomes 0, which the import refuses.
                prev: d.prev.map(|f| (u16::try_from(f.cols).unwrap_or(0), f.rows)),
                chrome: d.chrome.map(wide),
                pin: wide(d.pin),
                debt: wide(d.debt),
                debt_at: d.debt_at,
                below: d.below,
                esu_seen: d.esu_seen,
            }
        }),
    });
    Ok(ControlCarry {
        turns,
        unheld_below: unheld_below.min(MAX_TURN_ID.saturating_add(1)),
        archive,
    })
}

/// `"<len> <sha256hex>"`: the manifest's name for a sidecar's exact bytes.
#[cfg(any(unix, test))]
pub(crate) fn stamp(bytes: &[u8]) -> String {
    format!(
        "{} {}",
        bytes.len(),
        aterm_codec::hex::encode_lower(&sha256(bytes))
    )
}

/// Whether `bytes` are exactly what `stamp` names.
pub(crate) fn stamp_matches(stamp: &str, bytes: &[u8]) -> bool {
    stamp_len(stamp).is_some_and(|len| len == bytes.len() as u64)
        && stamp
            .split_once(' ')
            .is_some_and(|(_, h)| h == aterm_codec::hex::encode_lower(&sha256(bytes)))
}

/// The length a stamp names (`None` for a malformed stamp).
pub(crate) fn stamp_len(stamp: &str) -> Option<u64> {
    let (len, h) = stamp.split_once(' ')?;
    if !aterm_codec::hex::is_lower(h, 64) {
        return None;
    }
    len.parse().ok()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
#[path = "handoff_carry_tests.rs"]
mod tests;
