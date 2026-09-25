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
//!   a receiver reads its sidecars BEFORE it publishes its proof.
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
//! `ATERM_ALT_ARCHIVE=0` drops the carried rows. The ledger's text is carried
//! as the ledger holds it, so a redacted operator turn stays redacted.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::{
    AltArchiveCarry, AltArchiveDiffer, AltArchiveFence, AltArchiveGap, AltArchiveGapKind,
    AltArchiveImport, Terminal,
};

use crate::turn_ledger::{ArchMark, TurnLedger, TurnRecord};

/// The most archived rows one session carries, charged as the live archive
/// charges them (text plus 32 bytes a row). The newest rows win.
pub(crate) const CARRY_ARCHIVE_BYTES: usize = 1024 * 1024;
/// The largest sidecar a receiver reads: the rows above, the ledger (512
/// records of at most 512 bytes of text) and the differ's state, with JSON's
/// escaping on top. A bigger one is dropped unread.
pub(crate) const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;
/// The most sidecar bytes one handoff writes, and one receiver reads, across
/// every session — the receiver decodes them in early `main`, inside the
/// freeze the user sees. A session past it carries less (counters, then
/// nothing).
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 16 * 1024 * 1024;
/// How many of the newest SUBMITTED turns' marks the carried tail reaches back
/// to: `aterm drive report` reads `history 8` and starts from the newest
/// submitted turn among them.
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
const LOCK_PATIENCE: Duration = Duration::from_millis(50);

/// One session's capture, taken in the freeze and exported on the worker.
pub(crate) struct CarrySource {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    turns: Arc<Mutex<TurnLedger>>,
    fence: AltArchiveFence,
    /// The counters-only carry (plus the differ's state when there was time):
    /// what is carried when the rows cannot be.
    head: AltArchiveCarry,
}

impl CarrySource {
    /// The session this capture belongs to — what the park's capture matches
    /// against its carried screens, so a session whose screen it lowered below
    /// VisibleOnly after the fact goes without its control carry too (the
    /// 2026-09-22/23 update audit, plan P0-1e).
    pub(crate) fn local_id(&self) -> u64 {
        self.local_id
    }
}

/// THE FREEZE'S SHARE: the archive's fence and counters, and — with `differ`
/// — the differ's screen-sized state, under the terminal lock the caller
/// already holds for this session's checkpoint (so both describe the same
/// screen). Two `Arc` clones and a screen of row text; it cannot fail.
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
    }
}

/// THE WORKER'S SHARE, just before the manifest is written: one sidecar per
/// session, `(local_id, JSON)`. Never fails the handoff — a session whose
/// sidecar would not fit carries less, and in the end nothing.
pub(crate) fn export(sources: &[CarrySource]) -> Vec<(u64, Vec<u8>)> {
    let mut out = Vec::with_capacity(sources.len());
    let mut total = 0_u64;
    let patience = Instant::now() + LOCK_PATIENCE;
    for source in sources {
        // A ledger another thread keeps is carried as NOTHING it vouches for:
        // every turn id this process minted may have named a record of it.
        let (turns, unheld_below) = patiently(patience, || source.turns.try_lock()).map_or_else(
            || {
                (
                    Vec::new(),
                    crate::control::turn_ids_minted().saturating_add(1),
                )
            },
            |ledger| (ledger.records().cloned().collect(), ledger.unheld_below()),
        );
        let mut archive = source.head.clone();
        if let Some(terminal) = patiently(patience, || source.term.try_lock()) {
            // The differ's state, when the freeze had no time for it — only
            // while nothing committed since (then it is the state the freeze
            // would have taken). Without it the adopting engine starts a new
            // baseline after a `restore` gap, and a report says `archive-gap`.
            let _ = terminal.alt_archive_carry_differ(&mut archive, source.fence);
            let from = tail_from(&turns, source.fence, archive.differ.as_ref());
            // `false` (the archive moved since the freeze) leaves the head:
            // counters only, the rows counted lost.
            let _ = terminal.alt_archive_carry_rows(
                &mut archive,
                source.fence,
                from,
                CARRY_ARCHIVE_BYTES,
            );
        }
        let room = MAX_SIDECAR_BYTES.min(MAX_AGGREGATE_BYTES.saturating_sub(total));
        if let Some(bytes) = encode_within(turns, unheld_below, archive, room) {
            total += bytes.len() as u64;
            out.push((source.local_id, bytes));
        }
    }
    out
}

/// The turn-id counter as the manifest can carry it (a TOML integer is an
/// `i64`; a count past that is not carried rather than failing the write).
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
/// only), then the differ's state, then the ledger's older half at a time
/// (the ids shed are ones the carried ledger no longer vouches for).
fn encode_within(
    mut turns: Vec<TurnRecord>,
    mut unheld_below: u64,
    archive: AltArchiveCarry,
    room: u64,
) -> Option<Vec<u8>> {
    let fits = |b: &Vec<u8>| b.len() as u64 <= room;
    let mut archive = archive;
    if let Some(bytes) = encode(&turns, unheld_below, &archive).filter(fits) {
        return Some(bytes);
    }
    archive = counters_only(archive);
    if let Some(bytes) = encode(&turns, unheld_below, &archive).filter(fits) {
        return Some(bytes);
    }
    archive.differ = None;
    loop {
        if let Some(bytes) = encode(&turns, unheld_below, &archive).filter(fits) {
            return Some(bytes);
        }
        if turns.is_empty() {
            return None;
        }
        let shed = turns.len().div_ceil(2);
        if let Some(newest_shed) = turns.get(shed - 1) {
            unheld_below = unheld_below.max(newest_shed.id.saturating_add(1));
        }
        turns.drain(..shed);
    }
}

/// The carry with its rows left out: counted lost, the indices unchanged.
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
    /// `settled` or `timeout` — a word, never a code.
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

fn encode(turns: &[TurnRecord], unheld_below: u64, archive: &AltArchiveCarry) -> Option<Vec<u8>> {
    let wire = Wire {
        version: WIRE_VERSION,
        unheld_below,
        turns: turns
            .iter()
            .map(|t| TurnWire {
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
            })
            .collect(),
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

    /// The highest carried turn id.
    pub(crate) fn high_turn_id(&self) -> Option<u64> {
        self.turns.iter().map(|t| t.id).max()
    }
}

/// Decode a sidecar; `None` for anything that is not one this build reads.
/// A record with a status word this build does not print, or an id past
/// [`MAX_TURN_ID`], is left out; a gap of a kind it does not know is left out.
pub(crate) fn decode(bytes: &[u8]) -> Option<ControlCarry> {
    let wire: Wire = aterm_json::from_slice(bytes).ok()?;
    if wire.version != WIRE_VERSION {
        return None;
    }
    let turns = wire
        .turns
        .into_iter()
        .filter_map(|t| {
            if t.id > MAX_TURN_ID {
                return None;
            }
            Some(TurnRecord {
                id: t.id,
                started_ms: t.started_ms,
                dur_ms: t.dur_ms,
                submitted: t.submitted,
                status: crate::turn_ledger::status_word(&t.status)?,
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
    Some(ControlCarry {
        turns,
        unheld_below: wire.unheld_below.min(MAX_TURN_ID.saturating_add(1)),
        archive,
    })
}

/// `"<len> <sha256hex>"`: the manifest's name for a sidecar's exact bytes.
pub(crate) fn stamp(bytes: &[u8]) -> String {
    format!("{} {}", bytes.len(), hex(&sha256(bytes)))
}

/// Whether `bytes` are exactly what `stamp` names.
pub(crate) fn stamp_matches(stamp: &str, bytes: &[u8]) -> bool {
    stamp_len(stamp).is_some_and(|len| len == bytes.len() as u64)
        && stamp
            .split_once(' ')
            .is_some_and(|(_, h)| h == hex(&sha256(bytes)))
}

/// The length a stamp names (`None` for a malformed stamp).
pub(crate) fn stamp_len(stamp: &str) -> Option<u64> {
    let (len, h) = stamp.split_once(' ')?;
    (h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then_some(())?;
    len.parse().ok()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
#[path = "handoff_carry_tests.rs"]
mod tests;
