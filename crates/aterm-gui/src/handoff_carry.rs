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
//! A manager's `aterm drive report` (since removed) could not find the turn
//! it had just driven: the ledger and the archive were memory of a process
//! that had exited. (The composer half is `aterm-core`'s, fixed on the receiving side.)
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
//!   submitted turns (what a reader of `history` and `offscreen since=` reads
//!   back), and never fewer
//!   than the rows the differ may still point back at (`aterm-core` widens it
//!   to its reach) — and attaches at most [`CARRY_ARCHIVE_BYTES`] of them
//!   while the fence still holds, with the differ's state when the freeze had
//!   no time for it. A lock another thread keeps, or an archive that moved,
//!   carries the counters alone: the rows count as lost, which a reader is
//!   told. A ledger that could not be carried whole says which turn ids it
//!   no longer vouches for, so a resumed `subscribe … since-turn=` is told.
//! * `write_outgoing` writes it `0600`, `O_CREAT|O_EXCL|O_NOFOLLOW`, as
//!   `seamless-<pid>-<nonce>.s<id>.ctl` (a held pane's as `.h<id>.ctl`, named on
//!   `HeldPaneCarry::control`) and names it in the session's manifest record as
//!   `control = "<len> <sha256hex>"`, beside the turn-id counter. It is in neither
//!   adoption-proof digest: an older receiver skips both keys, and the sender unlinks the
//!   sidecar once the proof checks out, so a receiver reads its sidecars before it publishes
//!   its proof.
//! * `take_incoming` reads and deletes it, checks its length and sha, and decodes it
//!   ([`decode_why`]); `spawn_session` installs it ([`ControlCarry::ledger`],
//!   [`ControlCarry::install`]). Anything wrong with it drops the carry and nothing else.
//!
//! # The session log
//!
//! A handed session's log rides its sidecar too (`log`, design §5.2, phase
//! 2A): at most 128 KiB of `aterm_slog` carry lines, cut in the eviction
//! order, so the successor's log goes on as it stood ([`export_for_target`]).
//! The member is written only for a successor at least as new as this build
//! ([`successor_reads_session_log`]): below it the export is exactly what it
//! was. Its timeline is locked in a pass of its own after every ledger and
//! terminal, with its own patience, so a busy log costs only itself. Under
//! the aggregate's pressure the log's skeleton is set aside after the cores
//! and before the archive's rows, and its detail shares only what the rows
//! left (at most [`MAX_AGGREGATE_LOG_DETAIL`]): log detail is shed first,
//! then rows, then the differ's state, then the skeleton, then ledger
//! records. A receiver reads the member leniently and apart: a value that is
//! not a list of lines, or more of them than a carry holds, reads as no log,
//! and the ledger and archive adopt as they would have.
//!
//! # The crash carry
//!
//! An unplanned death of the window leaves no process to export anything, so the
//! ledger half of this sidecar is also written ahead of time, into the control slot of
//! the session's crash-restore region ([`CrashLedger`], `crash_restore`'s increment 7),
//! and a keeper recovery adopts it as an update adopts a sidecar
//! ([`decode_crash_carry`]). It carries the ledger and the dead process's turn-id count,
//! no archive and no log; the region is anonymous shared memory, never on disk.
//!
//! # Privacy
//!
//! Carrying the archive puts scrolled-off and submitted turn text on disk for one handoff: in
//! the per-user `0700` control dir, `0600`, never through a symlink, deleted by the receiver as
//! it reads it and by the sender once the proof checks out, and swept at the next start after
//! a crash (`seamless::sweep_dead_handoff_leftovers`). A receiver with the archive switched off
//! drops the carried rows. The ledger's text is carried as held, so a redacted operator turn
//! stays redacted.

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

use crate::turn_ledger::{ArchMark, TurnBy, TurnLedger, TurnRecord};

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
/// first, then its differ's state; past it, sessions carry fewer rows, alike
/// and their newest kept (then no differ state) — and
/// only a desk whose ledgers alone overflow it sheds ledger records, every
/// session its oldest, the longest ledgers alike ([`export`]).
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 16 * 1024 * 1024;
/// How many of the newest SUBMITTED turns' marks the carried tail reaches back
/// to: a reader of `history 8` that reads on from the newest submitted turn's
/// `arch=` mark (`offscreen since=`) finds its rows.
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
/// How long the worker waits, over the whole export, for locks other threads hold before it
/// carries less. The readers are parked, so the only holders are brief (a control read, a
/// `turn` recording, the scrollback compressor).
#[cfg(any(unix, test))]
const LOCK_PATIENCE: Duration = Duration::from_millis(50);
/// How long the worker waits, over the whole log pass (after every ledger
/// and terminal was taken), for timelines other threads hold. A timeline not
/// taken by then is taken only if free at once: a busy one costs its own
/// session's log, never another's ledger.
#[cfg(any(unix, test))]
const LOG_LOCK_PATIENCE: Duration = Duration::from_millis(20);
/// The most session-log DETAIL (past each log's skeleton) one handoff
/// carries across every session. Both ends of it are inside the freeze: the
/// export encodes it on the worker while every reader is parked, and the
/// receiver decodes it before the reader starts. Set from a one-off measure of
/// this export and its decode at 1-128 sessions and of aterm-slog's carry
/// encode and decode (2026-10-16, owner Mac): a full 128 KiB
/// log's cut and encode take 0.71 ms and its `Log::from_carry` 0.77 ms in
/// release (3.5 and 3.6 ms in debug), so this cap, about sixteen full logs,
/// is on the order of 12 ms of each end at worst in a shipped build. Every
/// other handed session pays only its snapshot under its lock (0.14 ms for
/// a full ring in release) and its skeleton's cut (0.02 ms): its detail is
/// sized from what the ring already knows and never encoded. Measured in a
/// debug build at 128 sessions with full logs, the export took 63 ms against
/// 3.6 ms without logs (shell-only) and 287 ms against 249 ms (Claude-Code-
/// like, whose rows take the aggregate); it was 360 and 685 ms when every
/// log was encoded in full two to four times.
#[cfg(any(unix, test))]
pub(crate) const MAX_AGGREGATE_LOG_DETAIL: u64 = 2 * 1024 * 1024;

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
    /// The session's timeline, whose log a successor that reads one is
    /// handed (`None`: a held pane's, which carries no log).
    timeline: Option<Arc<Mutex<crate::session_timeline::SessionTimeline>>>,
}

#[cfg(any(unix, test))]
impl CarrySource {
    /// The session this capture belongs to, which the park's capture matches against its
    /// carried screens: a session whose screen it lowered below the exact rungs goes without
    /// its differ's state too ([`Self::without_differ`]).
    #[cfg(unix)]
    pub(crate) fn local_id(&self) -> u64 {
        self.local_id
    }

    /// Whether the differ's state may travel with this session.
    #[cfg(all(test, unix))]
    pub(crate) fn carries_differ(&self) -> bool {
        self.keep_differ
    }

    /// Carry this session without the differ's state: its screen was not
    /// carried as the program drew it (`CarryRung::keeps_differ_state`), so
    /// that state describes nothing the successor shows. The ledger and the
    /// archive's rows still travel.
    #[cfg(unix)]
    pub(crate) fn without_differ(&mut self) {
        self.head.differ = None;
        self.keep_differ = false;
    }
}

/// The freeze's share: the archive's fence and counters and, with `differ`, the differ's
/// screen-sized state, under the terminal lock the caller already holds for this session's
/// checkpoint (so both describe the same screen). It cannot fail.
#[cfg(any(unix, test))]
pub(crate) fn capture_head(
    local_id: u64,
    terminal: &Terminal,
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    timeline: Option<&Arc<Mutex<crate::session_timeline::SessionTimeline>>>,
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
        timeline: timeline.map(Arc::clone),
    }
}

/// Whether a successor of `target_build` reads the session log a handoff
/// carries (the sidecar's `log`): one at least as new as this build, as for
/// the mailbox (`app_update_handoff::successor_reads_fabric_carry`). Below
/// it the export is exactly what it was without the member, so an older
/// receiver adopts as before, its aggregate unspent on a member it skips.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) fn successor_reads_session_log(target_build: u64, running_build: u64) -> bool {
    target_build >= running_build
}

/// Which turn-status words the receiving build reads, so [`export_for`] spells every record in
/// its vocabulary; a receiver leaves out a record whose word it does not know.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusWords {
    /// This build's `settled`, `timeout` and `hangup`, read by every build from
    /// [`READS_HANGUP_FROM_BUILD`] on.
    Current,
    /// A build from before `hangup` (a rollback, an older bundle, a channel switch): v0.98.0
    /// and earlier read `settled` and `timeout` alone, and leave a `hangup` record out while
    /// still vouching for its id.
    PreHangup,
}

/// The first build whose turn ledger reads `hangup`: v0.100.0's in `RELEASES.ledger` (no
/// v0.99.0 was cut). A development build's number is its HEAD's commit time, so one built
/// between the word's addition and this release is handed `timeout`, which it also reads.
#[cfg(any(unix, test))]
pub(crate) const READS_HANGUP_FROM_BUILD: u64 = 1_790_807_622;

#[cfg(any(unix, test))]
impl StatusWords {
    /// The words of the successor at `target_build`, keyed on the first build
    /// that reads `hangup` and not on whether the target is older than this
    /// build, as the screen carry's ceilings (`seamless::WireCaps::for_hop`)
    /// and the history carry (`handoff_history::exports_for_target`) are keyed
    /// on theirs: a rollback to v0.100.0 or later reads `hangup`, carried it
    /// as it is before this export respelled any word, and still does.
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
    /// submitted, its text, its archive mark — is what a reader of `history`
    /// and `offscreen since=` anchors on, and that crosses whole.
    fn spell(self, status: &'static str) -> &'static str {
        match (self, status) {
            (Self::PreHangup, "hangup") => "timeout",
            _ => status,
        }
    }
}

/// What a session's carry left behind in [`export_noting`], for the log line that names it.
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
    /// Another thread kept the session's timeline past the log pass's
    /// patience: it hands off without its log (the successor's starts over
    /// after `gap why=handoff`).
    LogBusy,
    /// The session's log did not fit (its skeleton past its 32 KiB, or past
    /// what the aggregate left): it hands off without its log.
    LogNotCarried,
}

#[cfg(any(unix, test))]
impl CarryLoss {
    /// The WARN line naming this loss for session `local_id`.
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
            Self::LogBusy => format!(
                "update apply: session {local_id}'s timeline was held by another thread past the \
                 log pass's {LOG_LOCK_PATIENCE:?}; it hands off without its session log, which \
                 starts over after a handoff gap"
            ),
            Self::LogNotCarried => format!(
                "update apply: session {local_id}'s session log does not fit its carry; it hands \
                 off without it, and the log starts over after a handoff gap"
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

/// The worker's share, just before the manifest is written: one sidecar per session,
/// `(local_id, JSON)`, its records spelled in `words` (the receiving build's). Never fails the
/// handoff: a session whose sidecar would not fit carries less, and in the end nothing.
/// `turn_id_ceiling` is the turn-id count the manifest carries, the most this process mints
/// before it exits.
///
/// THE AGGREGATE IS SPENT BY PRIORITY, not first come, first served (round
/// seven, finding 37): every session's CORE — its ledger and the archive's
/// counters, what `history`, `offscreen since=` and a resumed `since-turn=`
/// stand on — is set aside first, then each session's differ state (all or
/// none), and the archive's rows share what is left alike, each session's
/// newest first: first every session's rows after its newest turn's mark,
/// then the older ones. Spent greedily, a crowded
/// desk's first sessions carried their optional rows and its last ones lost
/// their whole ledger, the same sessions on every update. When the cores ALONE
/// overflow it, they share it ([`core_shares`]): every session keeps its
/// newest records, the longest ledgers are cut alike, and each carry names
/// the turn ids it shed (round eight, finding 41).
///
/// No session log: what a successor older than the log's carry is handed
/// ([`export_for_target`]).
#[cfg(test)]
pub(crate) fn export_for(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    words: StatusWords,
) -> Vec<(u64, Vec<u8>)> {
    export_noting(sources, turn_id_ceiling, words, false, |local_id, loss| {
        loss.log(local_id);
    })
}

/// [`export_for`] for a successor of `target_build`: its turn-status words
/// ([`StatusWords::for_target`]) and, when it reads one
/// ([`successor_reads_session_log`]), each handed session's log.
#[cfg(any(unix, test))]
pub(crate) fn export_for_target(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    target_build: u64,
) -> Vec<(u64, Vec<u8>)> {
    export_noting(
        sources,
        turn_id_ceiling,
        StatusWords::for_target(target_build),
        successor_reads_session_log(target_build, crate::running_build_number()),
        |local_id, loss| loss.log(local_id),
    )
}

/// What the log pass took of a session: its carry's source, prepared (the
/// records that always cross encoded once), its skeleton cut, and what its
/// whole carry would take, sized from what the ring already knows of its
/// records without encoding them. A session whose share is the skeleton
/// crosses with that cut as it is, and its detail is never encoded; one
/// granted detail is cut once at its share.
#[cfg(any(unix, test))]
struct LogTake {
    source: crate::session_log::PreparedSlogCarry,
    /// The carry with only what always crosses.
    skeleton: aterm_slog::CarryOut,
    /// `skeleton`'s lines as the member's JSON, its key included.
    skeleton_json: u64,
    /// The whole carry's member JSON, at most (the node lines its detail
    /// needs aside): the skeleton's, its detail's line bytes, and two JSON
    /// bytes a line.
    full_json: u64,
    /// The JSON's bytes per carry-line byte (its quotes, commas, escapes).
    json_ratio: f64,
}

/// The bytes the JSON string literal of `text` takes, quotes included:
/// `aterm_json`'s writer's escapes (`"` and `\` two, `\b \f \n \r \t`
/// two, any other C0 control six, every other byte itself), counted rather
/// than written.
#[cfg(any(unix, test))]
fn json_string_len(text: &str) -> u64 {
    let escaped: u64 = text
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | 0x08 | 0x0C | b'\n' | b'\r' | b'\t' => 2,
            0x00..=0x1F => 6,
            _ => 1,
        })
        .sum();
    escaped + 2
}

/// The JSON a `log` member of `lines` takes (`,"log":[…]`), counted.
#[cfg(any(unix, test))]
fn log_json_len(lines: &[String]) -> u64 {
    let items: u64 = lines.iter().map(|l| json_string_len(l)).sum();
    let commas = lines.len().saturating_sub(1) as u64;
    items + commas + 2 + 7
}

/// `sidecar` (a [`Wire`] as [`encode`] writes it, compact, its `log` member
/// absent) with `log` as its `log` member, spliced in before the object's
/// closing brace: the ledger and archive are not serialized again. `None`
/// when `sidecar` is not such an object.
#[cfg(any(unix, test))]
fn with_log_member(sidecar: &[u8], log: &[String]) -> Option<Vec<u8>> {
    let (last, body) = sidecar.split_last()?;
    if *last != b'}' || body.len() < 2 || log.is_empty() {
        return None;
    }
    let member = aterm_json::to_vec(log).ok()?;
    let mut out = Vec::with_capacity(sidecar.len() + member.len() + 8);
    out.extend_from_slice(body);
    out.extend_from_slice(b",\"log\":");
    out.extend_from_slice(&member);
    out.push(b'}');
    Some(out)
}

/// `sidecar` with a `log_mirror` member naming `prior` (`<ep>:<seq>:<ms>:<head>`),
/// spliced in before the object's closing brace as [`with_log_member`] does.
#[cfg(any(unix, test))]
fn with_mirror_member(sidecar: &[u8], prior: aterm_slog::Prior) -> Option<Vec<u8>> {
    let (last, body) = sidecar.split_last()?;
    if *last != b'}' || body.len() < 2 {
        return None;
    }
    let member = format!(
        ",\"log_mirror\":\"{}:{}\"}}",
        prior.landed.word(),
        prior.ms_head
    );
    let mut out = Vec::with_capacity(sidecar.len() + member.len());
    out.extend_from_slice(body);
    out.extend_from_slice(member.as_bytes());
    Some(out)
}

/// The most bytes [`with_mirror_member`] adds: `,"log_mirror":"` and four
/// numbers (`u32`, then three `u64`) with their colons and closing quote.
#[cfg(any(unix, test))]
const MIRROR_MEMBER_MAX: u64 = 16 + 10 + 3 * 20 + 3 + 1;

/// The `log_mirror` member read back (`<ep>:<seq>:<ms>:<head>`), or none.
fn read_mirror_member(raw: &str) -> Option<aterm_slog::Prior> {
    let (cursor, head) = raw.rsplit_once(':')?;
    Some(aterm_slog::Prior {
        landed: aterm_slog::BusCursor::parse(cursor)?,
        ms_head: head.parse().ok()?,
    })
}

/// [`export_for`], each [`CarryLoss`] handed to `noted` with its session;
/// `logs`: whether the successor reads the session log (each source's
/// timeline is then taken in a pass of its own, after every ledger and
/// terminal).
#[cfg(any(unix, test))]
pub(crate) fn export_noting(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    words: StatusWords,
    logs: bool,
    mut noted: impl FnMut(u64, CarryLoss),
) -> Vec<(u64, Vec<u8>)> {
    let patience = Instant::now() + LOCK_PATIENCE;
    // Every id this process may still number a record with.
    let unvouched = turn_id_ceiling
        .max(crate::control::turn_ids_minted())
        .saturating_add(1);
    let mut taken = Vec::with_capacity(sources.len());
    for source in sources {
        // A ledger another thread keeps is carried as nothing it vouches for: every turn id
        // this process minted, or still mints, may name a record of it.
        let (turns, floors) = match patiently(patience, || source.turns.try_lock()) {
            // A turn still open here records into this process after the export, so the
            // carry does not vouch for its id. Each record in the receiver's words.
            Some(ledger) => (
                ledger
                    .records()
                    .cloned()
                    .map(|mut record| {
                        record.status = words.spell(record.status);
                        record
                    })
                    .collect(),
                Floors {
                    unheld_below: ledger.carry_floor(turn_id_ceiling),
                    shed_below: ledger.shed_below(),
                },
            ),
            None => {
                noted(source.local_id, CarryLoss::LedgerBusy { unvouched });
                (
                    Vec::new(),
                    Floors {
                        unheld_below: unvouched,
                        shed_below: 0,
                    },
                )
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
        let core = core_len(&turns, floors, &archive);
        taken.push((source.local_id, turns, floors, archive, core));
    }
    // THE LOG PASS, after every ledger and terminal, with its own patience:
    // each timeline's take (its snapshot and producers' lines, its readers
    // fenced from here on) is the only thing done under its lock.
    let log_patience = Instant::now() + LOG_LOCK_PATIENCE;
    let mut log_takes: Vec<Option<LogTake>> = Vec::with_capacity(sources.len());
    // THE BUS MIRROR's cursor and head under each fence just set (phase 4,
    // ruling Q7): what a successor says of the old space's tail when this
    // session's log does not cross, or crosses and its carry does not read
    // there. `None` for an unmirrored log.
    let mut log_priors: Vec<Option<aterm_slog::Prior>> = Vec::with_capacity(sources.len());
    for source in sources {
        let mut busy = false;
        let mut prior = None;
        let take = logs
            .then_some(source.timeline.as_ref())
            .flatten()
            .and_then(|timeline| {
                let Some(mut tl) = patiently(log_patience, || timeline.try_lock()) else {
                    busy = true;
                    return None;
                };
                let src = tl.slog_carry_source();
                drop(tl);
                // Off the lock: the skeleton cut (its records encoded once),
                // the detail sized without encoding it.
                let source = src.prepare();
                prior = source.mirror_prior();
                let skeleton = source.skeleton()?;
                let skeleton_json = log_json_len(&skeleton.lines);
                let (detail_bytes, detail_records) = source.detail();
                let detail_json = (detail_bytes as u64).saturating_add(detail_records as u64 * 2);
                let json_ratio = skeleton_json as f64 / skeleton.bytes.max(1) as f64;
                // The carry never passes its session budget.
                let cap_json = (aterm_slog::CARRY_TOTAL_BYTES as f64 * json_ratio) as u64;
                let full_json = skeleton_json
                    .saturating_add(detail_json)
                    .min(cap_json.max(skeleton_json));
                Some(LogTake {
                    json_ratio,
                    source,
                    skeleton,
                    skeleton_json,
                    full_json,
                })
            });
        if busy {
            noted(source.local_id, CarryLoss::LogBusy);
        } else if logs && source.timeline.is_some() && take.is_none() {
            noted(source.local_id, CarryLoss::LogNotCarried);
        }
        log_takes.push(take);
        log_priors.push(prior);
    }
    // Every carried log is fenced: this process's journal drains each to its
    // fence and ends its chain `#handed` (phase 2B); the Commit never waits.
    if logs {
        crate::session_log_journal::note_handing();
    }
    // Mirror cursors are mandatory metadata: reserve their bounded members
    // before any core, archive row, log skeleton or detail spends the cap.
    let mirror_reserved = log_priors
        .iter()
        .flatten()
        .fold(0_u64, |sum, _| sum.saturating_add(MIRROR_MEMBER_MAX));
    let control_budget = MAX_AGGREGATE_BYTES.saturating_sub(mirror_reserved);
    let cores: Vec<u64> = taken.iter().map(|(.., core)| *core).collect();
    let shares = core_shares(&cores, control_budget);
    // The log skeletons from what the cores left, before any archive row.
    let after_cores = control_budget.saturating_sub(shares.iter().sum());
    let skeletons: Vec<u64> = log_takes
        .iter()
        .map(|t| t.as_ref().map_or(0, |t| t.skeleton_json))
        .collect();
    let skeleton_grants: Vec<u64> = core_shares(&skeletons, after_cores)
        .into_iter()
        .zip(&skeletons)
        .map(|(granted, want)| if granted >= *want { *want } else { 0 })
        .collect();
    // THE DIFFERS' STATE from what the skeletons left, before any row
    // : a sidecar sheds its rows before it, so it is
    // granted first, or the rows' grants would leave no room for it.
    // Screen-sized; all or none.
    let after_skeletons = after_cores.saturating_sub(skeleton_grants.iter().sum());
    let differs: Vec<u64> = taken
        .iter()
        .map(|(_, _, _, archive, _)| differ_len(archive))
        .collect();
    let differ_grants: Vec<u64> = core_shares(&differs, after_skeletons)
        .into_iter()
        .zip(&differs)
        .map(|(granted, want)| if granted >= *want { *want } else { 0 })
        .collect();
    // THE ROWS share what the differs left, in two tiers, each split alike
    // (`core_shares`) and each session's newest rows first (
    // U30): every session's rows after its newest turn's mark (what `drive
    // report` reads back), then its older rows. Spent in pool order, the first
    // sessions carried their whole tails and the later ones none, the same
    // sessions on every update.
    let spare = after_skeletons.saturating_sub(differ_grants.iter().sum());
    let (newest, older): (Vec<u64>, Vec<u64>) = taken
        .iter()
        .map(|(_, turns, _, archive, _)| row_tiers(turns, archive))
        .unzip();
    let newest_grants = core_shares(&newest, spare);
    let after_newest = spare.saturating_sub(newest_grants.iter().sum());
    let older_grants = core_shares(&older, after_newest);
    // What no grant took, and what each session leaves of its own, goes to
    // the sessions after it.
    let mut unspent = after_newest.saturating_sub(older_grants.iter().sum());
    let mut total = 0_u64;
    let (mut shed_sessions, mut shed_records) = (0_usize, 0_usize);
    // Each chosen sidecar keeps its source's index: its log take is that
    // source's (`log_takes` is in source order).
    let mut chosen: Vec<(usize, u64, Vec<TurnRecord>, Chosen, u64)> =
        Vec::with_capacity(taken.len());
    for (index, ((((local_id, turns, floors, archive, _), share), skeleton), differ)) in taken
        .into_iter()
        .zip(shares)
        .zip(&skeleton_grants)
        .zip(differ_grants)
        .enumerate()
    {
        let member = if log_priors[index].is_some() {
            MIRROR_MEMBER_MAX
        } else {
            0
        };
        let granted = share
            .saturating_add(differ)
            .saturating_add(newest_grants[index])
            .saturating_add(older_grants[index])
            .saturating_add(unspent);
        let room = MAX_SIDECAR_BYTES
            .saturating_sub(member)
            .saturating_sub(*skeleton)
            .min(granted)
            .min(control_budget.saturating_sub(total));
        if let Some((pick, shed)) = encode_within(&turns, floors, archive, room) {
            let len = pick.bytes.len() as u64;
            total = total.saturating_add(len);
            unspent = granted.saturating_sub(len);
            if shed > 0 {
                shed_sessions += 1;
                shed_records += shed;
            }
            chosen.push((index, local_id, turns, pick, *skeleton));
        } else {
            unspent = granted;
            // The session adopts with an empty ledger and a fresh archive.
            aterm_log::warn!(
                "update apply: session {local_id}'s control carry does not fit what is left \
                 of the handoff's budget ({room} bytes); it hands off without its turn ledger \
                 and archive tail"
            );
        }
    }
    // THE LOG DETAIL shares what the rows left, capped (the receiver decodes
    // it inside the freeze), and each session's sidecar is written once.
    let reserved: u64 = chosen.iter().map(|(.., skeleton)| *skeleton).sum();
    let detail_budget = control_budget
        .saturating_sub(total)
        .saturating_sub(reserved)
        .min(MAX_AGGREGATE_LOG_DETAIL);
    let detail_wants: Vec<u64> = chosen
        .iter()
        .map(|(index, .., skeleton)| match log_takes[*index].as_ref() {
            Some(t) if *skeleton > 0 => t.full_json.saturating_sub(t.skeleton_json),
            _ => 0,
        })
        .collect();
    let detail_shares = core_shares(&detail_wants, detail_budget);
    let mut out = Vec::with_capacity(chosen.len());
    for ((index, local_id, turns, pick, skeleton), detail) in chosen.into_iter().zip(detail_shares)
    {
        let take = log_takes[index].as_ref().filter(|_| skeleton > 0);
        let prior = log_priors[index];
        // Its member was reserved before the core and log allocations.
        let member = if prior.is_some() {
            MIRROR_MEMBER_MAX
        } else {
            0
        };
        let bytes = take
            .and_then(|t| {
                let room = MAX_SIDECAR_BYTES
                    .saturating_sub(member)
                    .min(pick.bytes.len() as u64 + skeleton + detail);
                with_log(&turns, &pick, t, skeleton + detail, room)
            })
            .unwrap_or_else(|| {
                if take.is_some() {
                    noted(local_id, CarryLoss::LogNotCarried);
                }
                pick.bytes
            });
        // A MIRRORED log's cursor and head cross whether or not its log does,
        // a few dozen bytes (ruling Q7): a successor whose `Log::from_carry`
        // refuses the carry still says the old space's tail as one exact
        // `gap why=reset`. An unmirrored one adds none.
        let bytes = match prior {
            Some(p) => with_mirror_member(&bytes, p).unwrap_or(bytes),
            None => bytes,
        };
        out.push((local_id, bytes));
    }
    if shed_sessions > 0 {
        // Each of these carries names the ids it shed (`history`'s `unheld_below=`, a resumed
        // `since-turn=`'s GAP).
        aterm_log::warn!(
            "update apply: {shed_sessions} sessions' turn ledgers do not fit the handoff's \
             {MAX_AGGREGATE_BYTES}-byte control budget whole; they hand off their newest \
             records, {shed_records} older ones in all left behind"
        );
    }
    out
}

/// The sidecar `pick` chose, with the session's log cut to `log_json` bytes
/// of member JSON and the whole within `room`. A share of the skeleton alone
/// takes the take's own cut as it is; a larger one is cut once at it (the
/// whole carry when it covers the take's estimate), taken down by what the
/// JSON overran, twice at most, then to the skeleton. The member is spliced
/// into the sidecar `pick` already encoded ([`with_log_member`]); a sidecar
/// that cannot take it is encoded whole.
#[cfg(any(unix, test))]
fn with_log(
    turns: &[TurnRecord],
    pick: &Chosen,
    take: &LogTake,
    log_json: u64,
    room: u64,
) -> Option<Vec<u8>> {
    let sidecar = |lines: &[String]| -> Option<Vec<u8>> {
        with_log_member(&pick.bytes, lines)
            .or_else(|| encode_wire(&turns[pick.cut..], pick.floors, &pick.archive, lines))
    };
    let fits = |b: &Vec<u8>| b.len() as u64 <= room;
    if log_json <= take.skeleton_json {
        return sidecar(&take.skeleton.lines).filter(fits);
    }
    let mut total = if log_json >= take.full_json {
        aterm_slog::CARRY_TOTAL_BYTES
    } else {
        (log_json as f64 / take.json_ratio.max(1.0)) as usize
    };
    for _ in 0..3 {
        let budget = aterm_slog::CarryBudget {
            total: total.min(aterm_slog::CARRY_TOTAL_BYTES),
            ..aterm_slog::CarryBudget::SESSION
        };
        // A cut the budget refuses (one taken below the skeleton's own
        // bytes) still leaves the skeleton, which was granted.
        let Some(bytes) = take
            .source
            .encode(budget)
            .and_then(|cut| sidecar(&cut.lines))
        else {
            break;
        };
        let len = bytes.len() as u64;
        if len <= room {
            return Some(bytes);
        }
        let over = usize::try_from(len - room).unwrap_or(usize::MAX);
        total = total.saturating_sub(over.saturating_mul(2).max(1024));
    }
    sidecar(&take.skeleton.lines).filter(fits)
}

/// Each session's share of `budget` for its core ([`core_len`], `cores` in pool order): the
/// whole core while every core fits; past that, max-min fair (a core under the common share
/// keeps all of it, the longer ones are cut alike). The shares never sum past `budget`, and
/// pool order buys nothing.
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

/// The size of a session's core sidecar (its whole ledger and the archive's counters, no rows
/// or differ state) as [`export_noting`] sets it aside, capped at one sidecar's limit.
#[cfg(any(unix, test))]
fn core_len(turns: &[TurnRecord], floors: Floors, archive: &AltArchiveCarry) -> u64 {
    let mut core = counters_only(archive.clone());
    core.differ = None;
    encode(turns, floors, &core).map_or(MAX_SIDECAR_BYTES, |bytes| {
        (bytes.len() as u64).min(MAX_SIDECAR_BYTES)
    })
}

/// The turn-id counter as the manifest can carry it (a TOML integer is an
/// `i64`; a count past that is not carried rather than failing the write).
#[cfg(any(unix, test))]
pub(crate) fn manifest_turn_id(minted: u64) -> Option<u64> {
    (minted > 0 && minted <= MAX_TURN_ID).then_some(minted)
}

/// The turn ledger an adopted session starts with: the one its carry brought
/// ([`ControlCarry::ledger`]), or, when the handoff could not carry it, an empty one that no
/// longer vouches for any turn id up to the count this process continued from, so a resumed
/// `subscribe … since-turn=` below it is told (`GAP … events-resync=`).
pub(crate) fn adopted_ledger(control: Option<&mut ControlCarry>) -> TurnLedger {
    match control {
        Some(control) => control.ledger(),
        None => TurnLedger::carried(
            Vec::new(),
            crate::control::turn_ids_minted().saturating_add(1),
        ),
    }
}

/// A SESSION'S CRASH CARRY (the crash-restore plan's increment 7): the turn-ledger half of
/// this module's sidecar, for the control slot of the session's crash-restore region
/// (`crash_restore`: the window's journal worker writes it at most every two seconds while
/// the ledger moves). After an unplanned death of this window the next one adopts it as an
/// update's successor adopts a sidecar, so `history` there still finds the turns driven into
/// the session, `carried=1`.
///
/// The alt-screen archive stays out: it changes with every frame and is up to a MiB, and a
/// carry of it would describe a screen other than the one the crash restore brings back. A
/// carried record's `arch=` mark names this process's archive origin, which no later
/// archive takes (`spawn::alt_archive_origin`), so `offscreen since=` with it reads from the
/// start and says why. The session log stays out too.
///
/// Taken under the ledger's lock ([`Self::of`]: a clone of its records, as the update's
/// export takes them), encoded with no lock held ([`Self::encode`]).
#[cfg(any(unix, test))]
pub(crate) struct CrashLedger {
    turns: Vec<TurnRecord>,
    floors: Floors,
    turn_ids: u64,
}

#[cfg(any(unix, test))]
impl CrashLedger {
    /// `ledger` as it stands, with `turn_ids`, this process's turn-id count now
    /// (`control::turn_ids_minted`): the floor carried is the export's own
    /// ([`TurnLedger::carry_floor`]), raised past every turn still open.
    pub(crate) fn of(ledger: &TurnLedger, turn_ids: u64) -> Self {
        Self {
            turns: ledger.records().cloned().collect(),
            floors: Floors {
                unheld_below: ledger.carry_floor(turn_ids),
                shed_below: ledger.shed_below(),
            },
            turn_ids,
        }
    }

    /// The carry's bytes: the sidecar's JSON holding the ledger and the turn-id count
    /// alone, every record in this build's words, and how many records it left out. When
    /// the whole ledger does not `fit` (the slot's record, compressed, past the slot), its
    /// OLDEST records are shed, the fewest that let the rest fit (a binary search: the
    /// records are encoded a handful of times, never once per record shed), and the carry
    /// no longer vouches for their ids (`unheld_below` raised past the newest one shed), as
    /// an update's export sheds them; the count shed is returned beside the bytes, so the
    /// writer can say the slot does not hold them (`status journal_ledger_lag=`). `None`
    /// only when not even the floors alone fit.
    pub(crate) fn encode(&self, fits: impl Fn(&[u8]) -> bool) -> Option<(Vec<u8>, usize)> {
        let at = |cut: usize| -> Option<Vec<u8>> {
            let unheld_below = cut
                .checked_sub(1)
                .and_then(|newest_shed| self.turns.get(newest_shed))
                .map_or(self.floors.unheld_below, |t| {
                    self.floors.unheld_below.max(t.id.saturating_add(1))
                });
            let floors = Floors {
                unheld_below,
                ..self.floors
            };
            encode_ledger(self.turns.get(cut..)?, floors, self.turn_ids)
        };
        let whole = at(0)?;
        if fits(&whole) {
            return Some((whole, 0));
        }
        // `best` is the carry at `hi`, which fits; the one at `lo - 1` does not.
        let mut best = at(self.turns.len()).filter(|bytes| fits(bytes))?;
        let (mut lo, mut hi) = (1, self.turns.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match at(mid) {
                Some(bytes) if fits(&bytes) => {
                    best = bytes;
                    hi = mid;
                }
                _ => lo = mid + 1,
            }
        }
        Some((best, hi))
    }

    /// How many records it holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.turns.len()
    }
}

/// The sidecar of a crash carry: the ledger, its floors and the turn-id count, no archive
/// and no log.
#[cfg(any(unix, test))]
fn encode_ledger(turns: &[TurnRecord], floors: Floors, turn_ids: u64) -> Option<Vec<u8>> {
    let wire = Wire {
        version: WIRE_VERSION,
        turns: turns.iter().map(turn_wire).collect(),
        unheld_below: floors.unheld_below,
        shed_below: floors.shed_below,
        turn_ids,
        ..Wire::default()
    };
    aterm_json::to_vec(&wire).ok()
}

/// Decode a CRASH CARRY ([`CrashLedger`], read out of a dead window's region) exactly as an
/// update's sidecar is decoded ([`decode_why`]: its version, every record's status word and
/// id, the floors), with what no crash carry holds left out: an archive or a session log in
/// one is no writing of this build's, and an archive would describe a screen other than the
/// restored one.
///
/// # Errors
///
/// [`decode_why`]'s.
#[cfg(any(unix, test))]
pub(crate) fn decode_crash_carry(bytes: &[u8]) -> Result<ControlCarry, &'static str> {
    let mut carry = decode_why(bytes)?;
    carry.archive = None;
    carry.log = None;
    carry.log_mirror = None;
    Ok(carry)
}

/// Where the carried tail starts: after the earliest mark among the newest [`TAIL_TURNS`]
/// submitted turns (the newest turn when none was submitted) that were minted under this
/// archive's origin, and no later than the rows the screen shows again. With no such turn,
/// from the oldest row (the byte cap then keeps the newest).
#[cfg(any(unix, test))]
fn tail_from(
    turns: &[TurnRecord],
    fence: AltArchiveFence,
    differ: Option<&AltArchiveDiffer>,
) -> u64 {
    rows_after_marks(turns, TAIL_TURNS, fence.origin, fence.first, differ)
}

/// [`tail_from`] over the newest `count` submitted turns, for the archive of
/// `origin` whose oldest row is `first`.
#[cfg(any(unix, test))]
fn rows_after_marks(
    turns: &[TurnRecord],
    count: usize,
    origin: u64,
    first: u64,
    differ: Option<&AltArchiveDiffer>,
) -> u64 {
    let mut marks: Vec<ArchMark> = turns
        .iter()
        .rev()
        .filter(|t| t.submitted)
        .take(count)
        .map(|t| t.arch)
        .collect();
    if marks.is_empty() {
        marks.extend(turns.last().map(|t| t.arch));
    }
    let mut from = marks
        .iter()
        .filter(|m| m.origin == origin)
        .map(|m| m.last.saturating_add(1))
        .min()
        .unwrap_or(first);
    if let Some(d) = differ.filter(|d| d.debt > 0) {
        from = from.min(d.debt_at);
    }
    from
}

/// The bytes a carry's rows add to its sidecar (each row's JSON string and a
/// comma), in two tiers: the rows after the newest turn's mark, and the rows
/// the screen shows again ([`rows_after_marks`] over one turn: what `drive
/// report` reads back), then the older ones.
#[cfg(any(unix, test))]
fn row_tiers(turns: &[TurnRecord], archive: &AltArchiveCarry) -> (u64, u64) {
    let from = rows_after_marks(
        turns,
        1,
        archive.origin,
        archive.first,
        archive.differ.as_ref(),
    );
    let older = usize::try_from(from.saturating_sub(archive.first))
        .unwrap_or(usize::MAX)
        .min(archive.rows.len());
    let len = |rows: &[Arc<str>]| -> u64 { rows.iter().map(|row| json_string_len(row) + 1).sum() };
    (len(&archive.rows[older..]), len(&archive.rows[..older]))
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

/// What [`encode_within`] chose: the ledger's cut (its first record carried
/// and the floors below which the carry vouches for no id), the archive as it
/// crosses, and the sidecar those encode to.
#[cfg(any(unix, test))]
struct Chosen {
    cut: usize,
    floors: Floors,
    archive: AltArchiveCarry,
    bytes: Vec<u8>,
}

/// Encode, shedding what does not fit `room`: the archive's OLDEST rows
/// first, no more of them than the room needs, then the differ's state, then
/// the ledger's OLDEST records, likewise (the ids shed are ones the carried
/// ledger no longer vouches for). With the records shed, if any.
///
/// Each row's and record's bytes are measured once, and the cut moves past as
/// many as the last encode ran over (at least one): an encode or two, where
/// shedding half the ledger at a time left up to half the room unspent.
///
/// THE ROWS ARE SHED A FEW AT A TIME TOO. They went
/// all at once: a tail a few bytes past its room crossed as counters alone,
/// so on a crowded desk the first session whose whole tail did not fit what
/// the aggregate had left lost every row — the few KB after its newest turn's
/// mark that `drive report` reads included — on every update, and what it
/// left went unspent.
#[cfg(any(unix, test))]
fn encode_within(
    turns: &[TurnRecord],
    floors: Floors,
    archive: AltArchiveCarry,
    room: u64,
) -> Option<(Chosen, usize)> {
    let mut archive = archive;
    // A row's bytes in the array: its JSON string and a comma.
    let row_lens: Vec<u64> = archive
        .rows
        .iter()
        .map(|row| json_string_len(row) + 1)
        .collect();
    let mut left_out = 0_usize;
    loop {
        let bytes = encode(turns, floors, &archive)?;
        let over = (bytes.len() as u64).saturating_sub(room);
        if over == 0 {
            return Some((
                Chosen {
                    cut: 0,
                    floors,
                    archive,
                    bytes,
                },
                0,
            ));
        }
        if archive.rows.is_empty() {
            break;
        }
        let mut freed = 0_u64;
        let carried = archive.rows.len();
        let mut shed = 0_usize;
        while shed < carried && (freed == 0 || freed < over) {
            freed = freed.saturating_add(row_lens[left_out + shed]);
            shed += 1;
        }
        archive = newest_rows(archive, carried - shed);
        left_out += shed;
    }
    archive.differ = None;
    let mut lens: Option<Vec<u64>> = None;
    let mut cut = 0_usize;
    loop {
        let unheld_below = cut
            .checked_sub(1)
            .and_then(|newest_shed| turns.get(newest_shed))
            .map_or(floors.unheld_below, |t| {
                floors.unheld_below.max(t.id.saturating_add(1))
            });
        let floors = Floors {
            unheld_below,
            ..floors
        };
        let bytes = encode(&turns[cut..], floors, &archive)?;
        let over = (bytes.len() as u64).saturating_sub(room);
        if over == 0 {
            return Some((
                Chosen {
                    cut,
                    floors,
                    archive,
                    bytes,
                },
                cut,
            ));
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
fn counters_only(c: AltArchiveCarry) -> AltArchiveCarry {
    newest_rows(c, 0)
}

/// The carry with only its `keep` newest rows: the older ones left out and
/// counted lost, with the gaps only they needed (as `AltArchive::carry_rows`
/// leaves out the gaps before its first row); the indices unchanged.
#[cfg(any(unix, test))]
fn newest_rows(mut c: AltArchiveCarry, keep: usize) -> AltArchiveCarry {
    let left_out = c.rows.len().saturating_sub(keep);
    c.rows.drain(..left_out);
    c.first = c.first.saturating_add(left_out as u64);
    c.lost = c.lost.saturating_add(left_out as u64);
    let first = c.first;
    c.gaps.retain(|g| g.after.saturating_add(1) >= first);
    c
}

// ------------------------------------------------------------------ wire

/// The sidecar. Every field defaults when absent and unknown ones are
/// skipped, so a newer sender's additions cost an older receiver nothing.
/// Numbers are `u64` on the wire whatever they are in memory, so a value out
/// of its in-memory range spoils only its own part (the differ's state is
/// then dropped whole by `AltArchive::import`), never the parse. The
/// cross-version fixtures' field cover reads a desk's sidecars as this
/// (`seamless_fixture_cover.rs`).
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct Wire {
    version: u64,
    turns: Vec<TurnWire>,
    /// Turn ids below this may have named records `turns` does not hold (a
    /// ledger that could not be carried whole); 0: none.
    unheld_below: u64,
    /// Turn ids below this may have named a harness record the sender's
    /// ring evicted from inside (`TurnLedger::shed_below`); 0: none, and
    /// then absent, so a ledger that shed nothing carries the bytes it did
    /// before the field. An older receiver ignores it: its `since-turn=`
    /// resume below it is not told of the harness records it misses.
    #[serde(skip_serializing_if = "is_zero")]
    shed_below: u64,
    /// The turn-id count of the process that wrote it
    /// (`control::turn_ids_minted`), as of the write; 0: none, and then
    /// absent. Only a CRASH carry writes it ([`CrashLedger`]): its process
    /// left no manifest to carry the count, and the restoring window
    /// continues its own count above every carry's
    /// (`crash_restore::continue_carried_turns`). An update's sidecar never
    /// does, so its bytes are what they were.
    #[serde(skip_serializing_if = "is_zero")]
    turn_ids: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<ArchiveWire>,
    /// THE SESSION LOG (phase 2A): its carry lines, oldest part first
    /// (`aterm_slog::Log::carry`). Absent (an older sender, or a successor
    /// older than the carry) reads as none; a value that is not a list of
    /// strings reads as none too and costs nothing else ([`lenient_log`]).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_log"
    )]
    log: Vec<String>,
    /// THE BUS MIRROR's cursor and head in the old seq space, for a mirrored
    /// log that did not cross (phase 4, ruling Q7): `<ep>:<seq>:<ms>:<head>`.
    /// Absent for every other sidecar, so an unmirrored instance's sidecars
    /// are the bytes they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    log_mirror: Option<String>,
}

/// The sidecar's `log` member, read so that nothing it holds can cost the
/// rest: a list of strings is the log; anything else is none.
fn lenient_log<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Member {
        Lines(Vec<String>),
        Other(serde::de::IgnoredAny),
    }
    Ok(match <Member as serde::Deserialize>::deserialize(d)? {
        Member::Lines(lines) => lines,
        Member::Other(_) => Vec::new(),
    })
}

/// The two floors a carried ledger names ([`Wire::unheld_below`],
/// [`Wire::shed_below`]).
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Floors {
    unheld_below: u64,
    shed_below: u64,
}

/// `skip_serializing_if` for a floor of none.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct TurnWire {
    id: u64,
    started_ms: u64,
    dur_ms: u64,
    submitted: bool,
    /// `settled`, `timeout` or `hangup` — a word, never a code. A
    /// [`crate::turn_ledger::STATUS_PRESSED`] record says `timeout` here, a
    /// word every build reads, and [`Self::pressed`].
    status: String,
    text: String,
    screen_hash: u64,
    seq: u64,
    arch_origin: u64,
    arch_last: u64,
    /// Who typed it (`history`'s `by=` word); an older sender's record has
    /// none and reads as `-` ([`TurnBy::from_wire`]).
    by: String,
    /// The window harness's `send` and `key enter`, or a `turn
    /// settle=submitted` ([`crate::turn_ledger::STATUS_PRESSED`]), whose `status` says
    /// `timeout`. The word is newer than every release: one a successor did
    /// not know would leave the record out and raise its floor past it
    /// ([`decode_why`]), and no build number yet names the first that reads
    /// it, so it never crosses as the word. An older successor ignores this
    /// field and holds the record as a settle nobody saw end (`timeout`:
    /// nothing was awaited after the Enter); this build and every later one
    /// read it back as `pressed`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pressed: bool,
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
    let pressed = t.status == crate::turn_ledger::STATUS_PRESSED;
    TurnWire {
        id: t.id,
        started_ms: t.started_ms,
        dur_ms: t.dur_ms,
        submitted: t.submitted,
        status: if pressed { "timeout" } else { t.status }.to_string(),
        text: t.text.to_string(),
        screen_hash: t.screen_hash,
        seq: t.seq,
        arch_origin: t.arch.origin,
        arch_last: t.arch.last,
        by: t.by.as_wire().to_string(),
        pressed,
    }
}

#[cfg(any(unix, test))]
fn encode(turns: &[TurnRecord], floors: Floors, archive: &AltArchiveCarry) -> Option<Vec<u8>> {
    encode_wire(turns, floors, archive, &[])
}

/// [`encode`] with the session log's carry `log` as its `log` member.
#[cfg(any(unix, test))]
fn encode_wire(
    turns: &[TurnRecord],
    floors: Floors,
    archive: &AltArchiveCarry,
    log: &[String],
) -> Option<Vec<u8>> {
    let wire = Wire {
        log: log.to_vec(),
        log_mirror: None,
        version: WIRE_VERSION,
        unheld_below: floors.unheld_below,
        shed_below: floors.shed_below,
        turn_ids: 0,
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
            differ: archive.differ.as_ref().map(differ_wire),
        }),
    };
    aterm_json::to_vec(&wire).ok()
}

/// The differ's state as the sidecar carries it.
#[cfg(any(unix, test))]
fn differ_wire(d: &AltArchiveDiffer) -> DifferWire {
    DifferWire {
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
    }
}

/// The bytes the differ's state adds to a sidecar (`,"differ":` and its
/// JSON); 0 when it carries none. Screen-sized, so encoded alone: what
/// [`export_noting`] sets aside for it before any session's rows.
#[cfg(any(unix, test))]
fn differ_len(archive: &AltArchiveCarry) -> u64 {
    archive.differ.as_ref().map_or(0, |d| {
        aterm_json::to_vec(&differ_wire(d)).map_or(0, |json| json.len() as u64 + 10)
    })
}

/// A decoded sidecar, ready to install into the adopted session.
#[derive(Debug, Default)]
pub(crate) struct ControlCarry {
    /// The carried ledger's records, oldest first.
    pub turns: Vec<TurnRecord>,
    /// Turn ids below this may have named records `turns` does not hold (the
    /// ledger could not be carried whole); 0: none.
    pub unheld_below: u64,
    /// Turn ids below this may have named a harness record the sender's
    /// ring evicted from inside ([`TurnLedger::with_shed_below`]); 0: none.
    pub shed_below: u64,
    /// A crash carry's turn-id count, as its process had minted it when the
    /// carry was written ([`Wire::turn_ids`]); 0 for an update's sidecar.
    #[cfg_attr(
        not(any(unix, test)),
        expect(dead_code, reason = "only a unix keeper recovery reads a crash carry")
    )]
    pub turn_ids: u64,
    /// The carried archive.
    pub archive: Option<AltArchiveCarry>,
    /// The carried session log's lines, when the sender wrote them and they
    /// read as a list within a carry's bounds.
    pub log: Option<Vec<String>>,
    /// The bus mirror's old-space cursor and head, for a log that did not
    /// cross (`Wire::log_mirror`).
    pub log_mirror: Option<aterm_slog::Prior>,
}

impl ControlCarry {
    /// The carried ledger, every record marked carried (`history` prints
    /// `carried=1`), for the adopted session's `SessionCtx`.
    pub(crate) fn ledger(&mut self) -> TurnLedger {
        TurnLedger::carried(std::mem::take(&mut self.turns), self.unheld_below)
            .with_shed_below(self.shed_below)
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

    /// The carried session log, taken (the adopting log reads it,
    /// `session_log::SessionLog::adopt`).
    pub(crate) fn take_log(&mut self) -> Option<Vec<String>> {
        self.log.take()
    }

    /// The bus mirror's old-space cursor, taken (`LogAdopt::mirror`).
    pub(crate) fn take_log_mirror(&mut self) -> Option<aterm_slog::Prior> {
        self.log_mirror.take()
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

/// Decode a sidecar, or say why it is not one this build reads. A record with a status word
/// this build does not print, or an id past [`MAX_TURN_ID`], is left out, and the carry no
/// longer vouches for its id (`unheld_below` rises past it, as for a record the sender shed),
/// so a resumed `since-turn=` below it is told and `history` says it. A gap of an unknown kind
/// is left out.
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
            let status = if t.pressed {
                crate::turn_ledger::STATUS_PRESSED
            } else {
                status
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
                by: TurnBy::from_wire(&t.by),
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
    // The log, bounded before anything reads it: more lines or bytes than
    // a carry holds is no log (the log's own reader checks the rest).
    let log = Some(wire.log).filter(|lines| {
        !lines.is_empty()
            && lines.len() <= aterm_slog::CARRY_MAX_LINES
            && lines.iter().map(|l| l.len() + 1).sum::<usize>() <= aterm_slog::CARRY_MAX_BYTES
    });
    let log_mirror = wire.log_mirror.as_deref().and_then(read_mirror_member);
    Ok(ControlCarry {
        turns,
        unheld_below: unheld_below.min(MAX_TURN_ID.saturating_add(1)),
        shed_below: wire.shed_below.min(MAX_TURN_ID.saturating_add(1)),
        turn_ids: wire.turn_ids.min(MAX_TURN_ID),
        archive,
        log,
        log_mirror,
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
