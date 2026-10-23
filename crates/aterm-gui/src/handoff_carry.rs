// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session JSON sidecar carrying the turn ledger, alt-screen archive tail, and session log across a self-update handoff.

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

/// The most archived rows one session carries, charged as the live archive charges them (text plus 32 bytes a row).
#[cfg(any(unix, test))]
pub(crate) const CARRY_ARCHIVE_BYTES: usize = 1024 * 1024;
/// Maximum byte size of one session's handoff sidecar.
pub(crate) const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum aggregate sidecar bytes across all sessions in one handoff.
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 16 * 1024 * 1024;
/// Count of newest submitted turns whose archive marks bound the carried tail.
#[cfg(any(unix, test))]
pub(crate) const TAIL_TURNS: usize = 8;
/// Wire format version for the handoff control sidecar.
const WIRE_VERSION: u64 = 1;
/// The highest turn id a handoff carries: what the manifest can hold (a TOML integer is an `i64`).
pub(crate) const MAX_TURN_ID: u64 = i64::MAX as u64;
/// How long the worker waits, over the whole export, for locks other threads hold before it
/// carries less.
#[cfg(any(unix, test))]
const LOCK_PATIENCE: Duration = Duration::from_millis(50);
/// How long the worker waits, over the whole log pass (after every ledger
/// and terminal was taken), for timelines other threads hold.
#[cfg(any(unix, test))]
const LOG_LOCK_PATIENCE: Duration = Duration::from_millis(20);
/// The most session-log DETAIL (past each log's skeleton) one handoff carries across every session.
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
    /// Session local id for this capture.
    #[cfg(unix)]
    pub(crate) const fn local_id(&self) -> u64 {
        self.local_id
    }

    /// Whether the differ's state may travel with this session.
    #[cfg(all(test, unix))]
    pub(crate) fn carries_differ(&self) -> bool {
        self.keep_differ
    }

    /// Drop the differ's state when the session's screen was lowered below an exact rung.
    #[cfg(unix)]
    pub(crate) fn without_differ(&mut self) {
        self.head.differ = None;
        self.keep_differ = false;
    }
}

/// Capture the archive's fence, counters, and optional differ state during the freeze.
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

/// Whether a successor of `target_build` reads the sidecar's session log.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) const fn successor_reads_session_log(target_build: u64, running_build: u64) -> bool {
    target_build >= running_build
}

/// Turn-status vocabulary understood by the receiving build.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusWords {
    /// Vocabulary including `settled`, `timeout`, and `hangup` ([`READS_HANGUP_FROM_BUILD`]).
    Current,
    /// Pre-`hangup` vocabulary (`settled` and `timeout` only).
    PreHangup,
}

/// First build number whose turn ledger reads `hangup`.
#[cfg(any(unix, test))]
pub(crate) const READS_HANGUP_FROM_BUILD: u64 = 1_790_807_622;

#[cfg(any(unix, test))]
impl StatusWords {
    /// Select the status-word vocabulary for `target_build`.
    #[must_use]
    pub(crate) const fn for_target(target_build: u64) -> Self {
        if target_build < READS_HANGUP_FROM_BUILD {
            Self::PreHangup
        } else {
            Self::Current
        }
    }

    /// Translate `status` into this vocabulary.
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
    /// Another thread kept the session's turn ledger past the export's patience: it hands off without its ledger, every id below `unvouched` said as unheld.
    LedgerBusy { unvouched: u64 },
    /// Another thread kept the session's engine past the export's patience: its archive crosses as counters alone.
    TerminalBusy,
    /// The archive moved after the freeze took its fence: its counters alone.
    ArchiveMoved,
    /// Another thread kept the session's timeline past the log pass's patience: it hands off without its log (the successor's starts over after `gap why=handoff`).
    LogBusy,
    /// The session's log did not fit (its skeleton past its 32 KiB, or past what the aggregate left): it hands off without its log.
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

/// [`export_for`] in this build's own words: what a successor at least as new as this build reads.
#[cfg(test)]
pub(crate) fn export(sources: &[CarrySource], turn_id_ceiling: u64) -> Vec<(u64, Vec<u8>)> {
    export_for(sources, turn_id_ceiling, StatusWords::Current)
}

/// The worker's share, just before the manifest is written: one sidecar per session, `(local_id, JSON)`, its records spelled in `words` (the receiving build's).
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

/// `export_for` for a successor of `target_build`: its turn-status words ([`StatusWords::for_target`]) and, when it reads one ([`successor_reads_session_log`]), each handed session's log.
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

/// Prepared session-log carry state and size estimates for one session.
#[cfg(any(unix, test))]
struct LogTake {
    source: crate::session_log::PreparedSlogCarry,
    /// The carry with only what always crosses.
    skeleton: aterm_slog::CarryOut,
    /// `skeleton`'s lines as the member's JSON, its key included.
    skeleton_json: u64,
    /// Estimated upper bound on the full carry's member JSON size.
    full_json: u64,
    /// The JSON's bytes per carry-line byte (its quotes, commas, escapes).
    json_ratio: f64,
}

/// Byte length of `text` when serialized as a JSON string literal, quotes included.
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

/// Splice `log` as the `"log"` member into a compact `sidecar` JSON object.
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

/// `export_for`, each [`CarryLoss`] handed to `noted` with its session; `logs`: whether the successor reads the session log (each source's timeline is then taken in a pass of its own, after every ledger and terminal).
#[cfg(any(unix, test))]
pub(crate) fn export_noting(
    sources: &[CarrySource],
    turn_id_ceiling: u64,
    words: StatusWords,
    logs: bool,
    mut noted: impl FnMut(u64, CarryLoss),
) -> Vec<(u64, Vec<u8>)> {
    let patience = Instant::now() + LOCK_PATIENCE;
    let unvouched = turn_id_ceiling
        .max(crate::control::turn_ids_minted())
        .saturating_add(1);
    let mut taken = Vec::with_capacity(sources.len());
    for source in sources {
        let (turns, floors) = match patiently(patience, || source.turns.try_lock()) {
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
    let log_patience = Instant::now() + LOG_LOCK_PATIENCE;
    let mut log_takes: Vec<Option<LogTake>> = Vec::with_capacity(sources.len());
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
                let source = src.prepare();
                prior = source.mirror_prior();
                let skeleton = source.skeleton()?;
                let skeleton_json = log_json_len(&skeleton.lines);
                let (detail_bytes, detail_records) = source.detail();
                let detail_json = (detail_bytes as u64).saturating_add(detail_records as u64 * 2);
                let json_ratio = skeleton_json as f64 / skeleton.bytes.max(1) as f64;
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
    if logs {
        crate::session_log_journal::note_handing();
    }
    let mirror_reserved = log_priors
        .iter()
        .flatten()
        .fold(0_u64, |sum, _| sum.saturating_add(MIRROR_MEMBER_MAX));
    let control_budget = MAX_AGGREGATE_BYTES.saturating_sub(mirror_reserved);
    let cores: Vec<u64> = taken.iter().map(|(.., core)| *core).collect();
    let shares = core_shares(&cores, control_budget);
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
    let spare = after_skeletons.saturating_sub(differ_grants.iter().sum());
    let (newest, older): (Vec<u64>, Vec<u64>) = taken
        .iter()
        .map(|(_, turns, _, archive, _)| row_tiers(turns, archive))
        .unzip();
    let newest_grants = core_shares(&newest, spare);
    let after_newest = spare.saturating_sub(newest_grants.iter().sum());
    let older_grants = core_shares(&older, after_newest);
    let mut unspent = after_newest.saturating_sub(older_grants.iter().sum());
    let mut total = 0_u64;
    let (mut shed_sessions, mut shed_records) = (0_usize, 0_usize);
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
            aterm_log::warn!(
                "update apply: session {local_id}'s control carry does not fit what is left \
                 of the handoff's budget ({room} bytes); it hands off without its turn ledger \
                 and archive tail"
            );
        }
    }
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
        let bytes = match prior {
            Some(p) => with_mirror_member(&bytes, p).unwrap_or(bytes),
            None => bytes,
        };
        out.push((local_id, bytes));
    }
    if shed_sessions > 0 {
        aterm_log::warn!(
            "update apply: {shed_sessions} sessions' turn ledgers do not fit the handoff's \
             {MAX_AGGREGATE_BYTES}-byte control budget whole; they hand off their newest \
             records, {shed_records} older ones in all left behind"
        );
    }
    out
}

/// The sidecar `pick` chose, with the session's log cut to `log_json` bytes
/// of member JSON and the whole within `room`.
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

/// Allocate `budget` across `cores` using max-min fair sharing.
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

/// The turn-id counter as the manifest can carry it (a TOML integer is an `i64`; a count past that is not carried rather than failing the write).
#[cfg(any(unix, test))]
pub(crate) fn manifest_turn_id(minted: u64) -> Option<u64> {
    (minted > 0 && minted <= MAX_TURN_ID).then_some(minted)
}

/// Build the initial [`TurnLedger`] for an adopted session from `control`.
pub(crate) fn adopted_ledger(control: Option<&mut ControlCarry>) -> TurnLedger {
    match control {
        Some(control) => control.ledger(),
        None => TurnLedger::carried(
            Vec::new(),
            crate::control::turn_ids_minted().saturating_add(1),
        ),
    }
}

/// Turn-ledger snapshot for a session's crash-restore control slot.
#[cfg(any(unix, test))]
pub(crate) struct CrashLedger {
    turns: Vec<TurnRecord>,
    floors: Floors,
    turn_ids: u64,
}

#[cfg(any(unix, test))]
impl CrashLedger {
    /// Snapshot `ledger` at process turn-id count `turn_ids`.
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

    /// The carry's bytes: the sidecar's JSON holding the ledger and the turn-id count alone, every record in this build's words, and how many records it left out.
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

/// Decode a [`CrashLedger`] payload, discarding any archive or session-log fields.
/// # Errors
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
/// archive's origin, and no later than the rows the screen shows again.
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

/// JSON byte sizes of `archive`'s rows split into newest-turn and older tiers.
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

/// Retry `attempt` until `until`, returning `None` if it remains contended.
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

/// Encode within `room` bytes, shedding oldest archive rows, differ state, and oldest turns as needed.
#[cfg(any(unix, test))]
fn encode_within(
    turns: &[TurnRecord],
    floors: Floors,
    archive: AltArchiveCarry,
    room: u64,
) -> Option<(Chosen, usize)> {
    let mut archive = archive;
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

/// The sidecar.
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct Wire {
    version: u64,
    turns: Vec<TurnWire>,
    /// Turn id floor below which records may have been omitted.
    unheld_below: u64,
    /// Turn id floor below which harness records were evicted (`TurnLedger::shed_below`).
    #[serde(skip_serializing_if = "is_zero")]
    shed_below: u64,
    /// Process turn-id count at the time of writing (`control::turn_ids_minted`).
    #[serde(skip_serializing_if = "is_zero")]
    turn_ids: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<ArchiveWire>,
    /// Carried session-log lines, oldest first (`aterm_slog::Log::carry`).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_log"
    )]
    log: Vec<String>,
    /// Bus mirror cursor and head in the old sequence space (`<ep>:<seq>:<ms>:<head>`).
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
const fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct TurnWire {
    id: u64,
    started_ms: u64,
    dur_ms: u64,
    submitted: bool,
    /// Turn status word (`settled`, `timeout`, or `hangup`).
    status: String,
    text: String,
    screen_hash: u64,
    seq: u64,
    arch_origin: u64,
    arch_last: u64,
    /// Origin actor token ([`TurnBy::from_wire`]).
    by: String,
    /// Whether the turn's status was [`crate::turn_ledger::STATUS_PRESSED`].
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
/// JSON); 0 when it carries none.
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
    /// Turn ids below this may have named records `turns` does not hold (the ledger could not be carried whole); 0: none.
    pub unheld_below: u64,
    /// Turn ids below this may have named a harness record the sender's ring evicted from inside ([`TurnLedger::with_shed_below`]); 0: none.
    pub shed_below: u64,
    /// A crash carry's turn-id count, as its process had minted it when the carry was written ([`Wire::turn_ids`]); 0 for an update's sidecar.
    #[cfg_attr(
        not(any(unix, test)),
        expect(dead_code, reason = "only a unix keeper recovery reads a crash carry")
    )]
    pub turn_ids: u64,
    /// The carried archive.
    pub archive: Option<AltArchiveCarry>,
    /// The carried session log's lines, when the sender wrote them and they read as a list within a carry's bounds.
    pub log: Option<Vec<String>>,
    /// The bus mirror's old-space cursor and head, for a log that did not cross (`Wire::log_mirror`).
    pub log_mirror: Option<aterm_slog::Prior>,
}

impl ControlCarry {
    /// The carried ledger, every record marked carried (`history` prints `carried=1`), for the adopted session's `SessionCtx`.
    pub(crate) fn ledger(&mut self) -> TurnLedger {
        TurnLedger::carried(std::mem::take(&mut self.turns), self.unheld_below)
            .with_shed_below(self.shed_below)
    }

    /// Install the carried archive into the adopted engine, which has just restored the checkpoint taken with it.
    pub(crate) fn install(self, terminal: &mut Terminal) -> Option<AltArchiveImport> {
        self.archive
            .map(|archive| terminal.alt_archive_import(archive))
    }

    /// Adopt this carry without the differ's state: the successor repaints the session, so the screen that state describes is not the one it shows.
    pub(crate) fn without_differ(&mut self) {
        if let Some(archive) = self.archive.as_mut() {
            archive.differ = None;
        }
    }

    /// The carried session log, taken (the adopting log reads it, `session_log::SessionLog::adopt`).
    pub(crate) const fn take_log(&mut self) -> Option<Vec<String>> {
        self.log.take()
    }

    /// The bus mirror's old-space cursor, taken (`LogAdopt::mirror`).
    pub(crate) const fn take_log_mirror(&mut self) -> Option<aterm_slog::Prior> {
        self.log_mirror.take()
    }

    /// The highest carried turn id.
    pub(crate) fn high_turn_id(&self) -> Option<u64> {
        self.turns.iter().map(|t| t.id).max()
    }
}

/// Decode a sidecar; `None` for anything that is not one this build reads ([`decode_why`] says which).
#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Option<ControlCarry> {
    decode_why(bytes).ok()
}

/// Decode a sidecar, or say why it is not one this build reads.
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
