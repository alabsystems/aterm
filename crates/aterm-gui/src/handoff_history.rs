// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's HISTORY CARRY (2026-09-26): every tab's whole
//! scrollback crosses an in-session update, not the 256 lines the screen
//! carry holds — and a history that cannot cross is COUNTED and SAID
//! (`status` `history_lost=`, a band row), never dropped in silence.
//!
//! # Why
//!
//! The screen carry (`seamless::carry_for_wire`) puts a session's scrollback
//! INSIDE its checkpoint, and the checkpoint is taken in the freeze, so it is
//! bounded by the freeze: at most `seamless::MAX_HANDOFF_HISTORY_LINES` (256)
//! lines per session, and none at all once the capture window is half spent
//! or the pool's cell budget is. A shell tab configured for the default
//! 100,000-line ring came out of every update with a screen and a half of
//! history; a tab running a full-screen program came out with NONE of its
//! shell's history (the saved primary's blob is pinned to its visible rows).
//! The only word about it was one line in aterm.log (30293eb3a).
//!
//! # Shape
//!
//! * EXPORT, WITH EVERY READER LIVE. When an attempt starts, a worker
//!   ([`HistoryExporter`]) walks each handed session's whole history — the
//!   grid that holds it, which is the saved primary under an alternate screen
//!   — oldest first, in chunks of [`CHUNK_LINES`] read under short terminal
//!   locks, each read refused the moment the history's FENCE no longer holds
//!   (`aterm_grid::HistoryFence`: a width reflow, an unscroll, a rows-grow
//!   that revealed history, a scrollback clear or a reset). The lines go to a
//!   `0600`, `O_CREAT|O_EXCL|O_NOFOLLOW` sidecar in the per-user control dir,
//!   hashed as they are written. On the launched lane the export starts
//!   beside the launch, the successor's whole boot runs meanwhile, and the
//!   park gate waits (boundedly, [`EXPORT_PATIENCE`]) for its first pass.
//!   After that pass the export FOLLOWS each history, appending what lands
//!   every [`FOLLOW_EVERY`] under the same fence, because the park is seconds
//!   away (the boot, then the gate's quiet moment) and a tab still printing
//!   would otherwise outrun the join. The park PAUSES it — one bounded last
//!   look — before freezing anything, so no chunk read ever contends with the
//!   capture's locks or the layout's; a park that then misses resumes it with
//!   the readers ([`HistoryExporter::resume`]), and one that lands hands it
//!   over as the pause left it. The fork lane, which parks at once,
//!   exports on its worker after the capture instead
//!   ([`HistoryPlan::Deferred`]): every reader is parked then, so the fence
//!   trivially holds and there is nothing to follow.
//! * THE FREEZE IS UNCHANGED. The capture takes each session's screen exactly
//!   as before — plus the fence of its history as the park saw it
//!   ([`HistoryHead`]), a pure read under the lock the checkpoint is taken
//!   under.
//! * THE JOIN ([`join`]), on the worker before the manifest is written. The
//!   export fenced history `[first, end)`; the checkpoint carries the newest
//!   `carried` lines, `[head.end - carried, head.end)`. When the park's fence
//!   still describes the export's history and the checkpoint reaches back to
//!   the export's end, the two are CONTIGUOUS: the sidecar's first `take`
//!   lines are exactly the history older than what the checkpoint carries.
//!   The sidecar is linked to its attempt-bound name
//!   (`seamless-<pid>-<nonce>.s<id>.hist`) and its record says
//!   `history = "<len> <sha256hex> <take>"`. Anything else — no export, a
//!   broken fence, more output after the export's last look than the
//!   checkpoint carries —
//!   leaves the session with exactly today's bounded carry, and the lines
//!   that do not cross are COUNTED on the record (`history_dropped`, and the
//!   session's running `history_lost`).
//! * IMPORT AFTER COMMIT. The successor opens (and unlinks) each named
//!   sidecar before it publishes its proof — the outgoing process retires
//!   what is left once the proof checks out, and an open descriptor outlives
//!   that — reserves the absolute-row keys the import will take
//!   (`Terminal::reserve_older_history_keys`, whose claim fences the import),
//!   and adopts. Only after Commit, on its own worker, does it read the
//!   sidecar, check its length and sha, decode it strictly, build it into a
//!   tiered store off any lock (`aterm_grid::OlderHistory`) and place it in
//!   front of the session's oldest line under one brief lock. A sidecar that
//!   fails any check costs exactly its `take` lines, counted like the rest.
//!   The one leniency: a line over the line-record cap because of its OSC 8
//!   links crosses with those links dropped and its text exact, as the
//!   screen carry's does ([`read_sidecar`]); the export drops them before it
//!   writes, and the import still admits the frames exports through v0.97.0
//!   wrote without dropping them (2026-09-28). A
//!   pane whose scrollback was CLEARED between the adopt and the import (an
//!   ED3 or a reset the adopted shell sent) is not imported into at all: the
//!   history stays cleared, and nothing is counted. The pane's own retention
//!   then decides what it keeps, with one wrinkle the carry does not count:
//!   right after an update the pane's fast tier (its ring) is nearly empty,
//!   and the unified limit reserves the ring's whole share, so a history AT
//!   its limit keeps up to one ring (1,000 lines) fewer than the outgoing
//!   process held until new output refills the ring.
//!
//! A HELD PANE (a pane kept open after its command ended: `--hold`, a kept
//! failed start) is no handed session — its screen rides the manifest's held
//! list, visible rows only — but its scrollback crosses the same way (round
//! eight of the update audit, finding 39): exported beside the handed
//! sessions' — after them, and only into room they do not need: a handed
//! session that keeps printing evicts held panes' exports when its follow
//! would not fit the aggregate otherwise ([`ExportSessions`]); and only in the
//! time they do not need: its first pass runs a chunk at a time between the
//! handed sessions' follows, which it never holds up, and the park does not
//! wait for it (round eight's #801) — joined with
//! the fence its held capture took
//! ([`name_held_sidecar`], a `.h<id>.hist` sidecar named on its
//! `HeldPaneCarry`), opened before the proof ([`incoming_held`]) and imported
//! after Commit into the read-only pane by the same worker. What does not
//! cross is counted, and the producer writes WHY beside the count
//! ([`HeldLoss`]): a hop that carries a held pane's screen alone (to an older
//! build) is said as a held pane's, a policy's withholding as the new
//! version's request, and a carry that failed as a failure.
//!
//! A SUCCESSOR'S HANDOFF POLICY that carries no scrollback (`carry =
//! "visible"` or `"repaint"`, `aterm_update_core::handoff_policy`) is followed
//! here too, not only by the screen carry: under such a ceiling nothing is
//! exported ([`HistoryPlan::Withheld`] — the launched lane does not start its
//! export when it already knows the policy, and its park stops one it started
//! before it did, removing every sidecar), no sidecar is named, and the join
//! COUNTS every line the park saw as left behind, said as the policy's
//! ([`Fallback::Withheld`]) — in the log, and on the record
//! (`history_withheld`), so the successor's band row says the new version
//! asked for it instead of reading it as a failure that kept "the newest
//! lines" (it kept none; [`LossSummary::withheld`]). A v0.97.0 producer
//! follows such a policy but predates the record's word for it, so the
//! successor also reads its OWN sealed policy: a record that counts lines,
//! names no sidecar and carried none in its screen is the policy's
//! withholding when that policy asked the record's producer for no
//! scrollback ([`incoming`]'s `followed_no_scrollback`; round eight of the
//! update audit, finding 87) — for a v0.97.0 producer alone, and only from a
//! manifest that names no sidecar: an older producer followed no policy, and
//! a newer one says what it withheld, so for either an unexplained loss stays
//! a failure. A `full` ceiling is the carry above, unchanged.
//!
//! The sidecar is in NEITHER adoption-proof digest and no schema moved: an
//! older successor skips the record keys and adopts exactly as before (the
//! outgoing process retires the unread sidecar), and an older outgoing
//! process names none. A successor OLDER than the running build (a rollback)
//! is handed no sidecar at all ([`exports_for_target`]): it could not import
//! one, and would say nothing about the lines it did not get.
//!
//! # Privacy
//!
//! The history was memory of a process; for the length of one handoff it is
//! on disk, in the per-user `0700` control dir, `0600`, never through a
//! symlink. The exporter removes its own files when an attempt stops, the
//! successor unlinks what it opens, the outgoing process retires the rest
//! with the attempt (`seamless::discard_outgoing` on a rollback,
//! `seamless::retire_outgoing_controls` once the proof checks out), and a
//! crash's leftovers go with the next start's sweep
//! (`seamless::sweep_dead_handoff_leftovers`).

// The seamless handoff is unix-only; the successor-side types still ride the
// cross-platform `Adopted`, so the module compiles everywhere.
#![cfg_attr(not(unix), allow(dead_code))]

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::grid::{HistoryFence, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal};
use aterm_core::scrollback::Line;
use aterm_core::terminal::{Terminal, TerminalCheckpoint};
use aterm_update_core::handoff_policy::CarryCeiling;

/// The sidecar's first bytes. The version is in the magic: a successor that
/// does not know it drops the sidecar (and counts its lines).
const MAGIC: &[u8; 8] = b"ATHIST1\n";
/// Magic, then the width the lines are wrapped at (`u16`, little-endian) and
/// two reserved zero bytes. No line count: a FOLLOWING export appends frames
/// until it is stopped, so the count is the stamp's to name (`take`), and the
/// reader proves the frames hold that many.
const HEADER_LEN: usize = 8 + 2 + 2;
/// Lines per frame, and per terminal-lock hold while exporting.
pub(crate) const CHUNK_LINES: usize = 1024;
/// The most history lines one session exports — ten times the default ring.
/// Older lines than that stay behind, counted.
pub(crate) const MAX_EXPORT_LINES: u64 = 1 << 20;
/// The largest sidecar one session writes and a successor opens.
pub(crate) const MAX_SIDECAR_BYTES: u64 = 256 * 1024 * 1024;
/// The most sidecar bytes one handoff writes across every session. A session
/// past it carries today's bounded history, counted.
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 1024 * 1024 * 1024;
/// How long the launched lane's park gate waits, after the successor has
/// dialled, for the export's FIRST PASS to finish before it parks anyway (the
/// sessions the pass had not reached then carry today's bounded history,
/// counted). The launched lane is macOS's.
#[cfg(any(target_os = "macos", test))]
pub(crate) const EXPORT_PATIENCE: Duration = Duration::from_secs(5);
/// How long the park waits for an export it stops: the chunk it is reading,
/// then its last look (at most [`LAST_LOOK_CHUNKS`] per session) — a
/// millisecond or two on a desk that is not flooding.
pub(crate) const HALT_WAIT: Duration = Duration::from_millis(100);
/// How often a FOLLOWING export (the launched lane's) takes the history that
/// landed since its last look. Between the export's first pass and the park
/// the successor boots and the gate waits for a quiet moment — seconds, up to
/// the automatic lane's two-minute hold — and a tab that printed more than the
/// screen carry's 256 lines in that time used to fall back to them (the
/// 2026-09-26 review). Following, what can still outrun the join is only what
/// lands between the last look and the park: 256 lines per tick covers ten
/// thousand lines a second.
pub(crate) const FOLLOW_EVERY: Duration = Duration::from_millis(25);
/// Chunks one session's history is followed by per tick. A flood that outruns
/// it is caught up over the next ticks, or outruns the join and is counted.
const FOLLOW_CHUNKS: usize = 8;
/// Chunks one session's history is followed by in the export's LAST look, as
/// the park stops it — bounded, because the park waits for it on the main
/// thread ([`HALT_WAIT`]).
const LAST_LOOK_CHUNKS: usize = 2;
/// How long the fork lane's worker gives its export, which it runs itself
/// once the capture is done: every reader is parked by then, so nothing
/// contends with it, and the successor this lane forks has its whole boot
/// ahead of it in the freeze anyway.
pub(crate) const FORK_EXPORT_WAIT: Duration = Duration::from_secs(2);
/// Of [`FORK_EXPORT_WAIT`], how long the fork lane's worker gives the held
/// panes' first passes once every handed session's is done (round eight of
/// the update audit, #801). Every reader is parked while it waits, and a held
/// pane's scrollback is a finished command's: it may lengthen a desk's freeze
/// by this much at most, never by the whole wait.
pub(crate) const FORK_HELD_WAIT: Duration = Duration::from_millis(500);
/// How long the successor's import keeps retrying while its pane is being
/// rewrapped (a window settling at a new width right after the update).
const IMPORT_PATIENCE: Duration = Duration::from_secs(5);
const IMPORT_RETRY: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// The sidecar codec
// ---------------------------------------------------------------------------

fn header(cols: u16) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[..8].copy_from_slice(MAGIC);
    out[8..10].copy_from_slice(&cols.to_le_bytes());
    out
}

/// The strict per-line bounds a frame is decoded under, from the width the
/// header names — the ones the screen carry decodes its grids under
/// (`seamless::strict_grid_lines`): at most `cols * 256` content bytes, and a
/// line record of at most `16 KiB + cols * 512` bytes (57,344 at 80 columns).
///
/// The record cap binds what the import KEEPS, not what it reads: a record
/// over it because of its OSC 8 links is kept with those links dropped
/// ([`read_sidecar`]), up to the hard ceiling
/// `aterm_core::scrollback::max_linked_record_bytes` (the cap plus 8,460 bytes
/// a column — one link at the ingestion ceilings per column: 734,144 bytes at
/// 80 columns, 36,765,696 at the widest grid). The export drops those links
/// before it writes ([`strip_over_cap_links`]).
fn line_caps(cols: u16) -> (usize, usize) {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    (content_cap, record_cap)
}

/// The record's `history` value: `"<len> <sha256hex> <take>"`.
#[must_use]
pub(crate) fn stamp(len: u64, sha: &[u8; 32], take: u64) -> String {
    format!("{len} {} {take}", aterm_codec::hex::encode_lower(sha))
}

/// A stamp's three fields, or `None` for a malformed one.
#[must_use]
pub(crate) fn parse_stamp(stamp: &str) -> Option<(u64, [u8; 32], u64)> {
    let mut fields = stamp.split(' ');
    let len = fields.next()?.parse().ok()?;
    let digest = fields.next()?;
    let take = fields.next()?.parse().ok()?;
    if fields.next().is_some() || digest.len() != 64 {
        return None;
    }
    let mut sha = [0u8; 32];
    let (pairs, _) = digest.as_bytes().as_chunks::<2>();
    for (byte, pair) in sha.iter_mut().zip(pairs) {
        let pair = std::str::from_utf8(pair).ok()?;
        if pair.bytes().any(|b| b.is_ascii_uppercase()) {
            return None;
        }
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some((len, sha, take))
}

/// Drop the hyperlinks of every line in `lines` whose record would be over the
/// frame decode's record cap at `cols` ([`line_caps`]), and nothing else, so
/// no frame the export writes holds a record the import has to repair. The
/// screen carry's rule (`seamless::strip_over_cap_links`, plan P2-4 of the
/// 2026-09-22/23 update audit): the line keeps its text, attributes,
/// underline colours and wrap flag; only its link destinations are lost.
/// Returns how many lines lost their links.
///
/// Through v0.97.0 the export wrote such a line as it was, and the import
/// refused the whole sidecar for it: one row of a dozen 8 KiB links cost a tab
/// its entire carried history (2026-09-28).
fn strip_over_cap_links(lines: &mut [Line], cols: u16) -> u64 {
    let (_, record_cap) = line_caps(cols);
    let mut stripped = 0;
    for line in lines.iter_mut().filter(|line| line.has_hyperlinks()) {
        if line.serialize().len() > record_cap {
            line.clear_hyperlinks();
            stripped += 1;
        }
    }
    stripped
}

/// A sidecar's lines, as the import reads them.
#[derive(Debug)]
struct SidecarLines {
    /// The width the lines are wrapped at.
    cols: u16,
    /// The FIRST `take` lines.
    lines: Vec<Line>,
    /// Of `lines`, how many were over the record cap because of their links
    /// and are kept with those links dropped.
    links_dropped: u64,
}

/// Stream one sidecar: `take` must not exceed what the header names, the
/// bytes must be exactly `len` long and hash to `sha`, and every frame read
/// must decode strictly. Returns the width the lines are wrapped at and the
/// FIRST `take` lines. Frames past `take` are hashed and never decoded.
///
/// ONE LENIENCY (2026-09-28, law L3 of the fixture README: a consumer only
/// ever becomes more lenient). A line record over the record cap
/// ([`line_caps`]) BECAUSE OF ITS OSC 8 LINKS — which every export through
/// v0.97.0 wrote as it was — is kept with its links dropped and its text,
/// attributes, underline colours and wrap flag exact, and every other line of
/// the frame is kept as it is. Such a record is decoded only up to the hard
/// ceiling `aterm_core::scrollback::max_linked_record_bytes`, and only if it
/// decodes under every other bound and is within the cap without its links
/// (`deserialize_lines_strict_dropping_over_cap_links`). A frame corrupt in any
/// other way still refuses the whole sidecar, as before; nothing is repaired
/// by dropping a line or a frame.
fn read_sidecar(
    mut reader: impl std::io::Read,
    len: u64,
    sha: &[u8; 32],
    take: u64,
) -> Result<SidecarLines, &'static str> {
    let mut hasher = aterm_digest::Sha256::new();
    let mut head = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut head)
        .map_err(|_| "shorter than its header")?;
    hasher.update(head);
    if &head[..8] != MAGIC {
        return Err("not a history sidecar this build reads");
    }
    let cols = u16::from_le_bytes([head[8], head[9]]);
    if head[10..] != [0, 0] {
        return Err("not a history sidecar this build reads");
    }
    if cols == 0 || cols > aterm_core::grid::MAX_GRID_COLS {
        return Err("a width no grid has");
    }
    if take > MAX_EXPORT_LINES {
        return Err("more lines than a sidecar may hold");
    }
    let (content_cap, record_cap) = line_caps(cols);
    let mut read = HEADER_LEN as u64;
    let mut out: Vec<Line> = Vec::new();
    let mut links_dropped = 0_u64;
    let want = usize::try_from(take).map_err(|_| "more lines than this process can hold")?;
    out.try_reserve_exact(want)
        .map_err(|_| "more lines than this process can hold")?;
    let mut frame = Vec::new();
    while read < len {
        let mut size = [0u8; 4];
        reader
            .read_exact(&mut size)
            .map_err(|_| "truncated inside a frame")?;
        hasher.update(size);
        let size = u64::from(u32::from_le_bytes(size));
        read += 4;
        if read > len || size > len - read {
            return Err("a frame longer than the sidecar");
        }
        frame.clear();
        (&mut reader)
            .take(size)
            .read_to_end(&mut frame)
            .map_err(|_| "truncated inside a frame")?;
        if frame.len() as u64 != size {
            return Err("truncated inside a frame");
        }
        hasher.update(&frame);
        read += size;
        if out.len() < want {
            let (decoded, stripped) =
                aterm_core::scrollback::deserialize_lines_strict_dropping_over_cap_links(
                    &frame,
                    CHUNK_LINES,
                    usize::from(cols),
                    content_cap,
                    record_cap,
                )
                .ok_or("a frame that does not decode")?;
            let room = want - out.len();
            // Only the lines kept count: one past `take` is the checkpoint's
            // to carry, and the screen carry says its own.
            links_dropped += stripped.iter().filter(|&&at| at < room).count() as u64;
            out.extend(decoded.into_iter().take(room));
        }
    }
    // Nothing may follow the stamped length.
    let mut tail = [0u8; 1];
    if read != len || reader.read(&mut tail).map_err(|_| "unreadable")? != 0 {
        return Err("a length other than its stamp");
    }
    if hasher.finalize() != *sha {
        return Err("a sha other than its stamp");
    }
    if out.len() != want {
        return Err("fewer lines than its stamp names");
    }
    Ok(SidecarLines {
        cols,
        lines: out,
        links_dropped,
    })
}

/// Create `path` afresh, owner-only, never through a symlink, never over
/// anything already there.
fn create_private_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// A file this module wrote and still owns: removed when dropped, unless it
/// was handed on ([`Self::keep`]).
struct OwnedFile(Option<PathBuf>);

impl OwnedFile {
    fn keep(mut self) -> PathBuf {
        self.0.take().unwrap_or_default()
    }
}

impl Drop for OwnedFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ---------------------------------------------------------------------------
// The outgoing side: export, head, join
// ---------------------------------------------------------------------------

/// What one finished export fenced: the history `[first, fence.end)`, as
/// `lines` lines. `first` is above `fence.oldest` only when the history was
/// deeper than [`MAX_EXPORT_LINES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExportFacts {
    pub(crate) fence: HistoryFence,
    pub(crate) first: u64,
    pub(crate) lines: u64,
}

/// One session's finished export: its facts and the sidecar that holds it,
/// removed when this is dropped unless the join links it on.
pub(crate) struct HistoryExport {
    pub(crate) local_id: u64,
    pub(crate) facts: ExportFacts,
    file: OwnedFile,
    len: u64,
    sha: [u8; 32],
    /// Lines written with their links dropped ([`strip_over_cap_links`]).
    links_dropped: u64,
}

impl std::fmt::Debug for HistoryExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryExport")
            .field("local_id", &self.local_id)
            .field("facts", &self.facts)
            .field("len", &self.len)
            .field("links_dropped", &self.links_dropped)
            .finish_non_exhaustive()
    }
}

enum ExportEvent {
    Exported(HistoryExport),
    NotExported {
        local_id: u64,
        why: String,
    },
    /// Every HANDED session's first pass is done. A held pane's first pass
    /// runs between the handed sessions' follows (round eight of the update
    /// audit, #801); the park gate waits for it too, boundedly
    /// ([`ExportEvent::Passed`], [`HistoryExporter::passed`]).
    CaughtUp,
    /// Every first pass is done, the held panes' too ([`ExportEvent::CaughtUp`]
    /// came first).
    Passed,
    /// The worker took its last look for the pause request of this
    /// generation ([`HistoryExporter::pause`]) and waits, taking no terminal
    /// lock, to be resumed or stopped.
    Paused(u64),
    Done,
}

/// What the exporter produced by the time it was asked for: the finished
/// exports, and why each session that has none has none.
#[derive(Default, Debug)]
pub(crate) struct ExportResults {
    pub(crate) exports: Vec<HistoryExport>,
    pub(crate) refused: Vec<(u64, String)>,
    /// The exporter had not handed over every handed session's result when
    /// asked: the sessions it had not reached carry today's bounded history,
    /// counted. A held pane's pass the export did not finish is no part of it:
    /// that pane is told as not exported ([`STOPPED_BEFORE_ITS_PASS`]).
    pub(crate) unfinished: bool,
    /// Nothing was exported because the successor's handoff policy carries no
    /// scrollback ([`HistoryPlan::Withheld`]): the join's "not exported" is
    /// the policy's, and says so ([`Fallback::Withheld`]).
    pub(crate) withheld: bool,
}

/// The outgoing process's export worker — see the module doc. Dropping it
/// stops the worker, and every sidecar it wrote that nobody took is removed.
pub(crate) struct HistoryExporter {
    events: std::sync::mpsc::Receiver<ExportEvent>,
    cancel: Arc<AtomicBool>,
    /// The generation of the pause in force, `0` for none
    /// ([`Self::pause`], [`Self::resume`]): only the launched lane, macOS's,
    /// pauses an export.
    #[cfg(any(target_os = "macos", test))]
    pause: Arc<AtomicU64>,
    /// The last pause generation handed out.
    pause_generation: u64,
    /// The last pause generation the worker answered.
    paused: u64,
    /// The worker, to wake from its follow tick when it is stopped.
    worker: std::thread::Thread,
    /// Set when the worker's last act is done (its sidecars removed or handed
    /// over): a dropped exporter is not joined, so a test reads this.
    #[cfg(test)]
    exited: Arc<AtomicBool>,
    results: ExportResults,
    /// Whether the worker follows each history after its first pass (the
    /// launched lane): it then hands its exports over only when it stops.
    follow: bool,
    caught_up: bool,
    /// Every first pass is done, the held panes' too ([`ExportEvent::Passed`]).
    passed: bool,
    done: bool,
}

impl std::fmt::Debug for HistoryExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryExporter")
            .field("caught_up", &self.caught_up)
            .field("passed", &self.passed)
            .field(
                "paused",
                &(self.pause_generation != 0 && self.paused == self.pause_generation),
            )
            .field("done", &self.done)
            .field("exported", &self.results.exports.len())
            .finish_non_exhaustive()
    }
}

/// The sessions one export takes, in the order it spends the handoff's
/// aggregate ([`MAX_AGGREGATE_BYTES`]) and the successor reads it back
/// (`take_sidecar`'s `remaining`): every handed session, then the held panes
/// (`App::history_sessions`). A held pane's export only ever takes room the
/// handed sessions do not need: its first pass comes after theirs, and a
/// handed session's follow that would not fit evicts held panes' exports to
/// make room ([`Aggregate::evict_for`]; the review of round eight's finding
/// 39). Nor the time they need (round eight's #801): on the launched lane a
/// held pane's first pass runs a chunk at a time between the handed
/// sessions' follows, which go on every [`FOLLOW_EVERY`] however deep it is,
/// so the park gate can wait for every first pass, the held panes' too
/// ([`HistoryExporter::passed`], bounded by [`EXPORT_PATIENCE`], readers
/// live) at no cost to a handed session; on the fork lane the held passes get
/// at most [`FORK_HELD_WAIT`] of the freeze. So the handed sessions carry
/// exactly what they would with no held pane exported, and a deep held pane's
/// scrollback crosses whenever its pass fits the patience.
#[derive(Default)]
pub(crate) struct ExportSessions {
    pub(crate) handed: Vec<ExportSession>,
    pub(crate) held: Vec<ExportSession>,
    /// The local id whose first pass the tests hold where it is (`u64::MAX`
    /// for none): a pass that does not finish, as a deep one does not
    /// before a park.
    #[cfg(test)]
    pub(crate) stalled: Option<Arc<AtomicU64>>,
    /// How far every handed session's export has followed: the least of their
    /// cursors (`fence.end`) after each follow, for the tests to wait on
    /// instead of sleeping.
    #[cfg(test)]
    pub(crate) followed: Arc<AtomicU64>,
}

/// One session an export takes: its local id and its engine.
pub(crate) type ExportSession = (u64, Arc<Mutex<Terminal>>);

impl ExportSessions {
    /// How many sessions the export takes.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.handed.len() + self.held.len()
    }

    /// Whether the tests hold `local_id`'s first pass where it is
    /// ([`Self::stalled`]); never outside them.
    fn stalls(&self, local_id: u64) -> bool {
        #[cfg(test)]
        {
            self.stalled
                .as_ref()
                .is_some_and(|stalled| stalled.load(Ordering::Acquire) == local_id)
        }
        #[cfg(not(test))]
        {
            let _ = local_id;
            false
        }
    }
}

impl From<Vec<ExportSession>> for ExportSessions {
    /// Handed sessions only, no held pane.
    fn from(handed: Vec<ExportSession>) -> Self {
        Self {
            handed,
            held: Vec::new(),
            #[cfg(test)]
            stalled: None,
            #[cfg(test)]
            followed: Arc::default(),
        }
    }
}

/// How one attempt's history is exported — decided per LANE, because the two
/// lanes park at different moments.
pub(crate) enum HistoryPlan {
    /// Nothing is exported: a successor older than this build (it has no
    /// importer), or no private control dir. Every session keeps the screen
    /// carry's bounded history, and the join counts what that leaves behind.
    Unexported,
    /// The launched lane's export, run with every reader live while the
    /// successor booted and FOLLOWING the history until the park paused it
    /// ([`HistoryExporter::pause`]) before freezing anything. macOS's lane.
    #[cfg(any(target_os = "macos", test))]
    Exported(HistoryExporter),
    /// The fork lane's sessions, exported by the worker once the capture is
    /// done — never beside the park, where its chunk reads would contend with
    /// the capture's and the layout's terminal locks.
    Deferred(ExportSessions),
    /// The successor's signed handoff policy carries NO scrollback (a `carry`
    /// ceiling of `"visible"` or `"repaint"`,
    /// [`CarryCeiling::carries_scrollback`]): nothing is exported — an export
    /// the launched lane started before it had read the policy is stopped,
    /// and every sidecar it wrote removed — and no sidecar is named. The join
    /// still runs over the park's heads, so each session's lines that do not
    /// cross (all of them, the screen carrying none under such a ceiling) are
    /// COUNTED on its record like any other loss, said as the policy's
    /// ([`Fallback::Withheld`]).
    Withheld,
}

impl std::fmt::Debug for HistoryPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unexported => formatter.write_str("Unexported"),
            #[cfg(any(target_os = "macos", test))]
            Self::Exported(export) => formatter.debug_tuple("Exported").field(export).finish(),
            Self::Deferred(sessions) => write!(formatter, "Deferred({} sessions)", sessions.len()),
            Self::Withheld => formatter.write_str("Withheld"),
        }
    }
}

impl HistoryPlan {
    /// THE FORK LANE'S PLAN, under the ceiling its park was taken under:
    /// [`Self::Withheld`] when that ceiling carries no scrollback (and
    /// `sessions` is never asked), else the handed sessions to export on the
    /// worker once the capture is done ([`Self::Deferred`]), or
    /// [`Self::Unexported`] when `sessions` has none to give (a rollback, no
    /// control dir). The fork lane's ceiling is the one its worker holds the
    /// policy to: a policy read after the park that asks for less refuses the
    /// whole capture before this plan runs.
    pub(crate) fn deferred_under(
        ceiling: CarryCeiling,
        sessions: impl FnOnce() -> Option<ExportSessions>,
    ) -> Self {
        if !ceiling.carries_scrollback() {
            return Self::Withheld;
        }
        sessions().map_or(Self::Unexported, Self::Deferred)
    }

    /// THE LAUNCHED LANE'S PLAN, under the ceiling its park read the policy
    /// at: the export that ran beside the launch ([`Self::Exported`]), or
    /// [`Self::Unexported`] without one — and [`Self::Withheld`] when that
    /// ceiling carries no scrollback, `export` dropped here, which stops its
    /// worker and removes every sidecar it wrote. (The park gate already
    /// stops such an export before it waits on it; this is the last word, so
    /// no export can reach the worker past a ceiling that forbids it.)
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn exported_under(ceiling: CarryCeiling, export: Option<HistoryExporter>) -> Self {
        if !ceiling.carries_scrollback() {
            drop(export);
            return Self::Withheld;
        }
        export.map_or(Self::Unexported, Self::Exported)
    }

    /// What the export produced, as the worker's join takes it (see the
    /// variants for when each runs).
    pub(crate) fn results(self, dir: &Path) -> ExportResults {
        match self {
            Self::Unexported => ExportResults::default(),
            Self::Withheld => ExportResults {
                withheld: true,
                ..ExportResults::default()
            },
            // Paused at the park already; this stops it and collects what it
            // finished.
            #[cfg(any(target_os = "macos", test))]
            Self::Exported(export) => export.finish(Duration::ZERO),
            Self::Deferred(sessions) => {
                match HistoryExporter::start(dir.to_path_buf(), sessions, false) {
                    Ok(export) => export.finish_deferred(FORK_EXPORT_WAIT, FORK_HELD_WAIT),
                    Err(error) => {
                        aterm_log::warn!(
                            "update apply: the scrollback export could not start ({error}); each \
                             tab carries the screen carry's bounded history"
                        );
                        ExportResults::default()
                    }
                }
            }
        }
    }
}

/// Whether an attempt handing to `target_build` exports history at all: only
/// to a successor at least as new as this build. An OLDER one (a rollback)
/// has no importer, would retire the sidecar unread, and could not say what
/// it did not get.
#[must_use]
pub(crate) fn exports_for_target(running_build: u64, target_build: u64) -> bool {
    target_build >= running_build
}

impl HistoryExporter {
    /// Start exporting `sessions` into `dir`, on a worker of its own. With
    /// `follow` (the launched lane, whose readers stay live until the park),
    /// the worker keeps each session's sidecar current after its first pass,
    /// every [`FOLLOW_EVERY`], until it is stopped; without (the fork lane,
    /// which exports under the park), it stops after the first pass. The
    /// sidecars together stay within [`MAX_AGGREGATE_BYTES`].
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "NativeUpdateHistoryCarry",
            action = "Export",
            project = "aterm_gui::handoff_history::conformance::project"
        )
    )]
    pub(crate) fn start(
        dir: PathBuf,
        sessions: impl Into<ExportSessions>,
        follow: bool,
    ) -> std::io::Result<Self> {
        Self::start_within(dir, sessions.into(), follow, MAX_AGGREGATE_BYTES)
    }

    /// [`Self::start`] with the sidecars' aggregate bounded by `cap` — the
    /// tests' way to reach the bound without writing a gigabyte.
    fn start_within(
        dir: PathBuf,
        sessions: ExportSessions,
        follow: bool,
        cap: u64,
    ) -> std::io::Result<Self> {
        let (tx, events) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let pause = Arc::new(AtomicU64::new(0));
        let worker_pause = Arc::clone(&pause);
        #[cfg(test)]
        let exited = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let worker_exited = Arc::clone(&exited);
        // A name nobody else's attempt can take: `seamless-<pid>-…` so the
        // start-up sweep retires it if this process dies holding it, and a
        // fresh token so it never meets an attempt's `<nonce>` prefix.
        // The sequence number keeps two attempts of this process apart even if
        // the random source failed and both drew the fallback token.
        static ATTEMPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tag = format!(
            "seamless-{}-hist{}x{}",
            std::process::id(),
            aterm_uds::rand::hex_token::<16>().unwrap_or_else(|_| "0".repeat(32)),
            ATTEMPTS.fetch_add(1, Ordering::Relaxed)
        );
        let worker = std::thread::Builder::new()
            .name("aterm-history-export".to_string())
            .spawn(move || {
                // It takes each session's terminal lock, which the UI thread
                // contends: Responsive is the floor for such a holder (`qos`).
                crate::qos::set_self(crate::qos::Role::Responsive);
                let stop = Stop {
                    cancel: &worker_cancel,
                    pause: &worker_pause,
                };
                run_export(&dir, &tag, &sessions, follow, cap, stop, &tx);
                #[cfg(test)]
                worker_exited.store(true, Ordering::Release);
            })?
            .thread()
            .clone();
        Ok(Self {
            events,
            cancel,
            #[cfg(any(target_os = "macos", test))]
            pause,
            pause_generation: 0,
            paused: 0,
            worker,
            #[cfg(test)]
            exited,
            results: ExportResults::default(),
            follow,
            caught_up: false,
            passed: false,
            done: false,
        })
    }

    fn absorb(&mut self, event: ExportEvent) {
        match event {
            ExportEvent::Exported(export) => self.results.exports.push(export),
            ExportEvent::NotExported { local_id, why } => {
                self.results.refused.push((local_id, why));
            }
            ExportEvent::CaughtUp => self.caught_up = true,
            ExportEvent::Passed => self.passed = true,
            ExportEvent::Paused(generation) => self.paused = generation,
            ExportEvent::Done => self.done = true,
        }
    }

    /// Take every event until `deadline` or the worker's `Done`.
    fn drain_until(&mut self, deadline: std::time::Instant) {
        self.drain_while(deadline, |_| true);
    }

    /// Take every event until `deadline`, the worker's `Done`, or `waiting`
    /// says what it waits for has arrived.
    fn drain_while(&mut self, deadline: std::time::Instant, waiting: impl Fn(&Self) -> bool) {
        while !self.done && waiting(self) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => self.absorb(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => self.done = true,
            }
        }
    }

    /// Take every event the worker has sent so far, without waiting.
    #[cfg(any(target_os = "macos", test))]
    fn poll(&mut self) {
        while !self.done {
            match self.events.try_recv() {
                Ok(event) => self.absorb(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.done = true,
            }
        }
    }

    /// Whether the worker's FIRST PASS is over for every HANDED session
    /// (non-blocking) — what the park names as the tabs it had not reached. A
    /// following export keeps running after it; a stopped one is over too.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn caught_up(&mut self) -> bool {
        self.poll();
        self.caught_up || self.done
    }

    /// Whether EVERY first pass is over, the held panes' too (non-blocking,
    /// [`ExportEvent::Passed`]) — what the launched lane's park gate waits for,
    /// bounded by [`EXPORT_PATIENCE`]. A held pane's pass runs between the
    /// handed sessions' follows (round eight of the update audit, #801), so the
    /// wait costs a handed session nothing, and every reader is live while it
    /// lasts; a held pass the patience runs out on is paused, kept and told as
    /// [`STOPPED_BEFORE_ITS_PASS`]. Keyed on [`Self::caught_up`] instead, the
    /// park landed at the dial and left a deep held pane's scrollback behind —
    /// the loss #39 was fixed to stop (round eight's last review).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn passed(&mut self) -> bool {
        self.poll();
        self.passed || self.done
    }

    /// Wait up to `wait` for the first pass to finish ([`Self::caught_up`]).
    #[cfg(test)]
    pub(crate) fn await_caught_up(&mut self, wait: Duration) -> bool {
        self.drain_while(std::time::Instant::now() + wait, |export| !export.caught_up);
        self.caught_up || self.done
    }

    /// The flag the worker sets when it has finished — read after a drop,
    /// which does not join it.
    #[cfg(test)]
    pub(crate) fn exited(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.exited)
    }

    /// Wait up to `wait` for every first pass to finish, the held panes' too
    /// ([`ExportEvent::Passed`]).
    #[cfg(test)]
    pub(crate) fn await_passed(&mut self, wait: Duration) -> bool {
        self.drain_while(std::time::Instant::now() + wait, |export| !export.passed);
        self.passed || self.done
    }

    /// Stop the worker and wait up to `wait` for it to say it has stopped —
    /// it finishes the chunk it is reading, takes one bounded last look at
    /// what landed since its last tick, and takes no terminal lock after.
    /// What the park calls before it freezes anything, so no chunk read
    /// contends with the capture's locks or the layout's. `false` when the
    /// worker did not answer in time.
    pub(crate) fn halt(&mut self, wait: Duration) -> bool {
        self.cancel.store(true, Ordering::Release);
        self.worker.unpark();
        self.drain_until(std::time::Instant::now() + wait);
        self.done
    }

    /// PAUSE the worker and wait up to `wait` for it to say it has — what
    /// the launched lane's park runs before it freezes anything. Like
    /// [`Self::halt`] the worker finishes (or drops, to redo) the chunk it is
    /// reading, takes one bounded last look, and then takes no terminal lock;
    /// unlike it, the export is not over. A park that lands hands it over as
    /// it stands ([`Self::finish`] stops it then, taking no further look). A
    /// park that misses — or returns before the capture for any reason —
    /// RESUMES it ([`Self::resume`]) with the readers, so the re-park joins
    /// an export that followed the history through the retry delay instead of
    /// one stopped where the first attempt stopped it (round six of the update
    /// audit, finding 14: a tab printing more than the screen carry's lines
    /// in that delay lost its whole exported history to `Outrun`). `true`
    /// when the worker answered (or had already stopped).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn pause(&mut self, wait: Duration) -> bool {
        self.pause_generation = self.pause_generation.wrapping_add(1).max(1);
        let generation = self.pause_generation;
        self.pause.store(generation, Ordering::Release);
        self.worker.unpark();
        self.drain_while(std::time::Instant::now() + wait, |export| {
            export.paused != generation
        });
        self.done || self.paused == generation
    }

    /// Undo [`Self::pause`]: the worker goes on following each history (and
    /// finishes a first pass the pause interrupted). Nothing when no pause is
    /// in force; a worker that never answered the pause simply never waits.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn resume(&mut self) {
        if self.pause.swap(0, Ordering::AcqRel) != 0 {
            self.worker.unpark();
        }
    }

    /// Wait up to `wait` for the worker to finish (a following export never
    /// does by itself: it is stopped here), then stop it — allowing it
    /// [`HALT_WAIT`] to hand over what it finished — and hand that over. A
    /// session it had not finished is not in the result.
    pub(crate) fn finish(self, wait: Duration) -> ExportResults {
        self.finish_by(std::time::Instant::now() + wait)
    }

    /// THE FORK LANE'S FINISH ([`HistoryPlan::Deferred`]): up to `wait` for the
    /// export, but past the handed sessions' first passes at most `held_wait`
    /// for the held panes' (round eight of the update audit, #801). Every reader
    /// is parked while this runs, and the held passes come last: run to the
    /// whole wait, a desk of deep held panes held every live tab frozen for it.
    fn finish_deferred(mut self, wait: Duration, held_wait: Duration) -> ExportResults {
        let deadline = std::time::Instant::now() + wait;
        self.drain_while(deadline, |export| !export.caught_up);
        let held_deadline = deadline.min(std::time::Instant::now() + held_wait);
        self.finish_by(held_deadline)
    }

    /// [`Self::finish`] by `deadline`.
    fn finish_by(mut self, deadline: std::time::Instant) -> ExportResults {
        self.drain_until(deadline);
        if !self.done {
            self.halt(HALT_WAIT);
        }
        // Without following, each handed session's export was handed over as
        // its pass finished, every one of them by `CaughtUp`.
        let handed_over = self.done || (self.caught_up && !self.follow);
        let mut results = std::mem::take(&mut self.results);
        results.unfinished = !handed_over;
        results
    }
}

impl Drop for HistoryExporter {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

/// The launched lane's park gate, for the export: wait while its first pass
/// is still running and the successor has held its claim for less than
/// [`EXPORT_PATIENCE`]. Waiting freezes nothing (every reader is live), so it
/// applies in every mode, an explicit apply's included.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn park_wait(exporting: bool, held_for: Duration) -> Option<&'static str> {
    (exporting && held_for < EXPORT_PATIENCE).then_some("the scrollback export is still running")
}

/// A terminal's lock, taken on the export and import workers. A poisoned
/// lock's data is still the data.
fn locked<T>(term: &Mutex<Terminal>, read: impl FnOnce(&mut Terminal) -> T) -> T {
    let mut guard = term
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read(&mut guard)
}

/// What the export worker is asked to stop for: `cancel` ends it, a non-zero
/// `pause` generation parks it after one last look until it is resumed
/// ([`HistoryExporter::pause`]).
#[derive(Clone, Copy)]
struct Stop<'a> {
    cancel: &'a AtomicBool,
    pause: &'a AtomicU64,
}

impl Stop<'_> {
    fn cancelled(self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// The pause generation in force, `0` for none.
    fn paused(self) -> u64 {
        self.pause.load(Ordering::Acquire)
    }

    /// Whether the worker must stop reading now, for either reason.
    fn requested(self) -> bool {
        self.cancelled() || self.paused() != 0
    }
}

/// Why a held pane's export was given up while the export followed
/// ([`Aggregate::evict_for`]).
const EVICTED: &str =
    "its room in the handoff's scrollback budget went to a live tab's newer lines";

/// Why a held pane has no export when the export stopped before its first
/// pass finished, or began: the park came first, or the fork lane's time for
/// held panes ran out ([`FORK_HELD_WAIT`]).
const STOPPED_BEFORE_ITS_PASS: &str =
    "the export stopped before it had read the pane's whole scrollback";

/// THE HANDOFF'S SIDECAR BYTES AS ONE EXPORT SPENDS THEM: every sidecar's
/// bytes against `cap` ([`MAX_AGGREGATE_BYTES`]), with the room a handed
/// session's newer lines may take back from the held panes.
struct Aggregate {
    cap: u64,
    written: u64,
    /// The held panes whose exports a handed session's follow evicted since
    /// the worker last told the exporter.
    evicted: Vec<u64>,
}

impl Aggregate {
    /// The room one more session's first pass may take.
    fn room(&self) -> u64 {
        self.cap.saturating_sub(self.written)
    }

    /// Whether `frame` more bytes fit.
    fn fits(&self, frame: u64) -> bool {
        self.written.saturating_add(frame) <= self.cap
    }

    /// Give back the bytes `export` was charged, as it is dropped.
    fn release(&mut self, export: &OpenExport) {
        self.written = self.written.saturating_sub(export.len);
    }

    /// A HANDED SESSION'S NEWER LINES OUTRANK A HELD PANE'S SCROLLBACK (the
    /// review of round eight's finding 39). A held pane's export takes the
    /// room the handed sessions' first passes left, and a live tab that keeps
    /// printing between the dial and the park needs room to FOLLOW: without
    /// it, its follow stops, and once more lines land than the screen carry
    /// holds, the join leaves its whole older scrollback behind ([`join`]'s
    /// `Outrun`) — a tab the person is using paying for panes that no longer
    /// run. So a handed session's `frame` that does not fit evicts held
    /// panes' exports from `held` — the largest first, which gives up the
    /// fewest panes for the room — and only when that makes it fit: no held
    /// pane is given up for a frame the handed sessions could not take
    /// anyway. An evicted export's sidecar is removed as it drops, and the
    /// pane is told as not exported ([`Self::evicted`]): its lines stay
    /// behind, counted and said as a failed carry ([`HeldLoss::Failed`]).
    /// Whether `frame` fits now.
    fn evict_for(&mut self, frame: u64, held: &mut Vec<OpenExport>) -> bool {
        let over = self.written.saturating_add(frame).saturating_sub(self.cap);
        if over == 0 {
            return true;
        }
        let evictable = held
            .iter()
            .fold(0_u64, |sum, export| sum.saturating_add(export.len));
        if evictable < over {
            return false;
        }
        let mut freed = 0_u64;
        while freed < over {
            let Some(at) = held
                .iter()
                .enumerate()
                .max_by_key(|(_, export)| (export.len, export.local_id))
                .map(|(at, _)| at)
            else {
                break;
            };
            let export = held.swap_remove(at);
            freed = freed.saturating_add(export.len);
            self.written = self.written.saturating_sub(export.len);
            self.evicted.push(export.local_id);
        }
        self.fits(frame)
    }
}

fn run_export(
    dir: &Path,
    tag: &str,
    sessions: &ExportSessions,
    follow: bool,
    cap: u64,
    stop: Stop<'_>,
    events: &std::sync::mpsc::Sender<ExportEvent>,
) {
    let worker = Worker {
        dir,
        tag,
        sessions,
        follow,
        stop,
        events,
        aggregate: Aggregate {
            cap,
            written: 0,
            evicted: Vec::new(),
        },
        open: Vec::new(),
        open_held: Vec::new(),
        next: 0,
        next_held: 0,
    };
    // `Gone`: the exporter went away, and the worker's files with its state.
    let _ = worker.run();
}

/// The exporter went away: a send failed, so nothing more is told, and the
/// worker's open sidecars are removed as its state drops.
struct Gone;

/// The export worker's state ([`run_export`]).
struct Worker<'a> {
    dir: &'a Path,
    tag: &'a str,
    sessions: &'a ExportSessions,
    /// The launched lane's: follow each history until the park stops it.
    follow: bool,
    stop: Stop<'a>,
    events: &'a std::sync::mpsc::Sender<ExportEvent>,
    aggregate: Aggregate,
    /// A following export's open sidecars: the handed sessions'.
    open: Vec<OpenExport>,
    /// And apart from them the held panes', which a handed session's follow
    /// may evict — the one whose first pass is still running among them
    /// ([`OpenExport::pass`]).
    open_held: Vec<OpenExport>,
    /// The next handed session whose first pass is to run: a pause may
    /// interrupt one, which then runs again from its start on resume (only the
    /// launched lane, which follows, pauses).
    next: usize,
    /// The next held pane whose first pass is to begin.
    next_held: usize,
}

impl Worker<'_> {
    fn send(&self, event: ExportEvent) -> Result<(), Gone> {
        self.events.send(event).map_err(|_| Gone)
    }

    /// Where `local_id`'s sidecar is written.
    fn path(&self, local_id: u64) -> PathBuf {
        self.dir.join(format!("{}.s{local_id}.hist", self.tag))
    }

    /// The export: the handed sessions' first passes, then — on the launched
    /// lane — following every history until the park stops it, the held
    /// panes' first passes run in the time between two follows.
    fn run(mut self) -> Result<(), Gone> {
        let mut caught_up = false;
        let mut passed = false;
        'export: loop {
            let handed = self.handed_passes()?;
            if !caught_up && handed && !self.stop.requested() {
                caught_up = true;
                self.send(ExportEvent::CaughtUp)?;
            }
            if !self.follow {
                // The fork lane: every reader is parked, so nothing follows.
                // The held panes' passes come after the handed sessions',
                // each to its end, until the worker is stopped
                // ([`HistoryExporter::finish_deferred`] bounds them).
                if caught_up {
                    while !self.stop.requested() && self.held_step()? {}
                }
                break;
            }
            // FOLLOW until stopped or paused: the readers are live, so the
            // history keeps growing, and the park joins whatever this has
            // taken by then.
            //
            // A HELD PANE'S FIRST PASS NEVER HOLDS THE FOLLOWING UP (round
            // eight of the update audit, #801). Every first pass, the held
            // panes' included, used to run before the first follow: on a desk
            // of deep `--hold` panes the handed sessions went unfollowed for
            // as long as those passes took, the park paused the export in the
            // middle of one, and a tab printing meanwhile outran the join's
            // last look (2,048 lines) plus the screen carry and lost its
            // whole older scrollback — which, with no held pane exported, it
            // kept. A held pass now runs a chunk at a time until the next
            // follow is due, and the follow runs every [`FOLLOW_EVERY`] as
            // with no held pane at all; a pause keeps a held pass where it
            // stood, and a resume goes on from there.
            let mut due = std::time::Instant::now() + FOLLOW_EVERY;
            while !self.stop.requested() {
                // A resume that overtook the pause which interrupted a handed
                // session's first pass (or came before `CaughtUp` was told):
                // take the passes up again.
                if self.next < self.sessions.handed.len() || !caught_up {
                    continue 'export;
                }
                if std::time::Instant::now() < due {
                    let held = !passed && self.held_step()?;
                    if !held {
                        if !passed {
                            passed = true;
                            self.send(ExportEvent::Passed)?;
                        }
                        std::thread::park_timeout(
                            due.saturating_duration_since(std::time::Instant::now()),
                        );
                    }
                    continue;
                }
                self.follow_tick(FOLLOW_CHUNKS, true)?;
                due = std::time::Instant::now() + FOLLOW_EVERY;
            }
            // The last look, as the park stops or pauses it: what landed since
            // the last tick, bounded (the park waits for it on the main
            // thread). A held pass still running is not taken further.
            self.follow_tick(LAST_LOOK_CHUNKS, false)?;
            if self.stop.cancelled() {
                break;
            }
            // PAUSED: every sidecar written through, the park told, and no
            // terminal lock taken until it resumes or stops the export.
            for export in self.open.iter_mut().chain(&mut self.open_held) {
                export.flush();
            }
            let generation = self.stop.paused();
            // A resume that overtook this last look (the pause timed out, the
            // park missed and resumed) leaves no pause to answer: waiting on
            // generation 0 would stop following until the next park (round
            // eight, finding 16).
            if generation == 0 {
                continue;
            }
            self.send(ExportEvent::Paused(generation))?;
            while !self.stop.cancelled() && self.stop.paused() == generation {
                std::thread::park_timeout(FOLLOW_EVERY);
            }
            // Stopped from the pause: the park landed and the export is handed
            // over as the pause left it — no look past the park's own.
            if self.stop.cancelled() {
                break;
            }
        }
        self.close()
    }

    /// THE HANDED SESSIONS' FIRST PASSES, from the first not done, each whole
    /// in turn. Interrupted by a PAUSE, a pass is dropped (its file with it)
    /// and taken again from its start on resume. Whether every one is done.
    fn handed_passes(&mut self) -> Result<bool, Gone> {
        let sessions = self.sessions;
        while let Some((local_id, term)) = sessions.handed.get(self.next) {
            if self.stop.cancelled() || (self.follow && self.stop.paused() != 0) {
                return Ok(false);
            }
            let path = self.path(*local_id);
            let stalls = sessions.stalls(*local_id);
            match begin_export(
                &path,
                *local_id,
                term,
                self.stop,
                &mut self.aggregate,
                stalls,
            ) {
                Ok(Some(export)) if !self.follow => self.send(export.close())?,
                Ok(Some(export)) => self.open.push(export),
                // An empty history crosses by itself: nothing to write.
                Ok(None) => {}
                Err(_) if self.follow && !self.stop.cancelled() && self.stop.paused() != 0 => {
                    return Ok(false);
                }
                Err(why) => self.send(ExportEvent::NotExported {
                    local_id: *local_id,
                    why,
                })?,
            }
            self.next += 1;
        }
        Ok(true)
    }

    /// One step of the HELD PANES' FIRST PASSES (round eight of the update
    /// audit, #801): the next chunk of the pass that is running, else the
    /// first chunk of the next held pane's. Without following, a pass that is
    /// done is handed over at once. Whether a held pass is left to run.
    fn held_step(&mut self) -> Result<bool, Gone> {
        let sessions = self.sessions;
        if let Some(at) = self
            .open_held
            .iter()
            .position(|export| export.pass.is_some())
        {
            if sessions.stalls(self.open_held[at].local_id) {
                std::thread::sleep(Duration::from_millis(1));
                return Ok(true);
            }
            match self.open_held[at].pass_step(&mut self.aggregate) {
                Ok(()) if !self.follow && self.open_held[at].pass.is_none() => {
                    let export = self.open_held.swap_remove(at);
                    self.send(export.close())?;
                }
                Ok(()) => {}
                Err(why) => {
                    let export = self.open_held.swap_remove(at);
                    self.aggregate.release(&export);
                    self.send(ExportEvent::NotExported {
                        local_id: export.local_id,
                        why,
                    })?;
                }
            }
            return Ok(true);
        }
        let Some((local_id, term)) = sessions.held.get(self.next_held) else {
            return Ok(false);
        };
        self.next_held += 1;
        match open_export(&self.path(*local_id), *local_id, term, &mut self.aggregate) {
            Ok(Some(export)) if !self.follow && export.pass.is_none() => {
                self.send(export.close())?;
            }
            Ok(Some(export)) => self.open_held.push(export),
            Ok(None) => {}
            Err(why) => self.send(ExportEvent::NotExported {
                local_id: *local_id,
                why,
            })?,
        }
        Ok(true)
    }

    /// One look at what landed past each cursor, `chunks` at most: the handed
    /// sessions first, each able to take room back from the held panes, then
    /// the held panes whose first pass is done; then the evicted are told. A
    /// tick (`interruptible`) stops at once for a stop or a pause; the last
    /// look does not.
    fn follow_tick(&mut self, chunks: usize, interruptible: bool) -> Result<(), Gone> {
        for export in &mut self.open {
            if interruptible && self.stop.requested() {
                break;
            }
            export.follow(&mut self.aggregate, Some(&mut self.open_held), chunks);
        }
        for export in &mut self.open_held {
            if interruptible && self.stop.requested() {
                break;
            }
            export.follow(&mut self.aggregate, None, chunks);
        }
        #[cfg(test)]
        if let Some(cursor) = self.open.iter().map(|export| export.facts.fence.end).min() {
            self.sessions.followed.store(cursor, Ordering::Release);
        }
        // The held panes' exports a handed session's follow evicted: each one
        // told as not exported, before anything the park waits on.
        for local_id in std::mem::take(&mut self.aggregate.evicted) {
            self.send(ExportEvent::NotExported {
                local_id,
                why: EVICTED.to_string(),
            })?;
        }
        Ok(())
    }

    /// Hand every open sidecar over, and tell each held pane whose first pass
    /// did not finish (or never began) why it has none; then `Done`.
    fn close(mut self) -> Result<(), Gone> {
        let open = std::mem::take(&mut self.open);
        let held = std::mem::take(&mut self.open_held);
        for export in open.into_iter().chain(held) {
            self.send(export.close())?;
        }
        let sessions = self.sessions;
        for (local_id, _) in sessions.held.get(self.next_held..).unwrap_or_default() {
            self.send(ExportEvent::NotExported {
                local_id: *local_id,
                why: STOPPED_BEFORE_ITS_PASS.to_string(),
            })?;
        }
        self.send(ExportEvent::Done)
    }
}

/// One session's sidecar while it is being written: its fence (whose `end`
/// is how far it has read), the open file, and the running length and hash.
struct OpenExport {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    /// `fence.end` is the export's CURSOR: one past the newest line written.
    facts: ExportFacts,
    writer: std::io::BufWriter<std::fs::File>,
    file: OwnedFile,
    len: u64,
    hasher: aterm_digest::Sha256,
    /// Whether ticks still extend it: not once its fence moved, retention
    /// passed it, or a bound was reached (its lines so far still cross, and
    /// the join decides whether they still meet the park's).
    following: bool,
    /// A write that failed after the first pass: the file is no longer what
    /// the hash says, so it cannot be named at all.
    broken: Option<String>,
    /// Lines written with their links dropped ([`strip_over_cap_links`]).
    links_dropped: u64,
    /// While its FIRST PASS runs: the fence the pass reads under, to its
    /// `end`. `None` once the pass is done. A handed session's pass runs to its
    /// end at once ([`begin_export`]); a held pane's a chunk at a time between
    /// the follows ([`Worker::held_step`]), so until then it is no whole
    /// history: not followed, and not handed over ([`Self::close`]).
    pass: Option<HistoryFence>,
}

impl OpenExport {
    /// Append the next `lines` of the FIRST PASS as one frame, within the
    /// sidecar's and the handoff's byte budgets, and charge it to `aggregate`;
    /// the pass is done once the cursor reaches its fence's end.
    fn pass_append(
        &mut self,
        mut lines: Vec<Line>,
        aggregate: &mut Aggregate,
    ) -> Result<(), String> {
        let Some(fence) = self.pass else {
            return Ok(());
        };
        if lines.is_empty() {
            return Err("the history ended before its fence".to_string());
        }
        self.links_dropped += strip_over_cap_links(&mut lines, fence.cols);
        let body = aterm_core::scrollback::serialize_lines(&lines);
        let frame = 4 + body.len() as u64;
        if self.len.saturating_add(frame) > MAX_SIDECAR_BYTES || !aggregate.fits(frame) {
            return Err(format!(
                "the history is over the {}-byte sidecar budget",
                self.len
                    .saturating_add(aggregate.room())
                    .min(MAX_SIDECAR_BYTES)
            ));
        }
        self.append(&lines, &body)
            .map_err(|e| format!("could not write ({})", e.kind()))?;
        aggregate.written = aggregate.written.saturating_add(frame);
        if self.facts.fence.end >= fence.end {
            self.pass = None;
        }
        Ok(())
    }

    /// The next chunk of the FIRST PASS, read under the fence it began on.
    fn pass_step(&mut self, aggregate: &mut Aggregate) -> Result<(), String> {
        let Some(fence) = self.pass else {
            return Ok(());
        };
        let cursor = self.facts.fence.end;
        let lines = locked(&self.term, |t| {
            t.history_lines_since_fence(&fence, cursor, CHUNK_LINES)
        })
        .map_err(|broken| broken.to_string())?;
        self.pass_append(lines, aggregate)
    }

    /// Append `lines` (the next ones after the cursor) as one frame.
    fn append(&mut self, lines: &[Line], body: &[u8]) -> std::io::Result<()> {
        let size = u32::try_from(body.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        self.writer.write_all(&size.to_le_bytes())?;
        self.writer.write_all(body)?;
        self.hasher.update(size.to_le_bytes());
        self.hasher.update(body);
        self.len = self.len.saturating_add(4 + u64::from(size));
        self.facts.fence.end += lines.len() as u64;
        self.facts.lines += lines.len() as u64;
        Ok(())
    }

    /// One tick of FOLLOWING: take up to `chunks` chunks of the history that
    /// landed past the cursor, while the fence the first pass was taken on
    /// still holds. Stops following (and keeps what it has) the moment it
    /// does not, retention passed the cursor, or a bound was reached. A
    /// handed session passes the held panes' exports as `held`, which it may
    /// evict to fit the aggregate ([`Aggregate::evict_for`]); a held pane
    /// passes `None`.
    fn follow(
        &mut self,
        aggregate: &mut Aggregate,
        mut held: Option<&mut Vec<OpenExport>>,
        chunks: usize,
    ) {
        for _ in 0..chunks {
            if !self.following || self.broken.is_some() || self.pass.is_some() {
                return;
            }
            let fence = self.facts.fence;
            let read = locked(&self.term, |t| {
                let now = t.history_fence();
                if !fence.holds_at(&now) {
                    return Err(());
                }
                // The history past the cursor, under the fence it was
                // exported on: same epoch, clear and reveal generations,
                // width.
                let ahead = HistoryFence {
                    end: now.end,
                    ..fence
                };
                t.history_lines_since_fence(&ahead, fence.end, CHUNK_LINES)
                    .map_err(|_| ())
            });
            let mut lines = match read {
                Ok(lines) if lines.is_empty() => return,
                Ok(lines) => lines,
                Err(()) => {
                    self.following = false;
                    return;
                }
            };
            let stripped = strip_over_cap_links(&mut lines, fence.cols);
            let body = aterm_core::scrollback::serialize_lines(&lines);
            let frame = 4 + body.len() as u64;
            if self.facts.lines + lines.len() as u64 > MAX_EXPORT_LINES
                || self.len.saturating_add(frame) > MAX_SIDECAR_BYTES
                || !(aggregate.fits(frame)
                    || held
                        .as_deref_mut()
                        .is_some_and(|held| aggregate.evict_for(frame, held)))
            {
                self.following = false;
                return;
            }
            match self.append(&lines, &body) {
                Ok(()) => {
                    aggregate.written = aggregate.written.saturating_add(frame);
                    self.links_dropped += stripped;
                }
                Err(error) => {
                    self.broken = Some(format!("could not write ({})", error.kind()));
                    return;
                }
            }
        }
    }

    /// Write everything buffered through to the file, so a paused export's
    /// [`Self::close`] has nothing left to do. A failed write breaks it, as
    /// in [`Self::close`].
    fn flush(&mut self) {
        if self.broken.is_none()
            && let Err(error) = self.writer.flush()
        {
            self.broken = Some(format!("could not write ({})", error.kind()));
        }
    }

    /// Close the sidecar: its event for the exporter. One whose first pass
    /// had not finished holds no whole history: it is not exported.
    fn close(mut self) -> ExportEvent {
        if self.pass.is_some() {
            return ExportEvent::NotExported {
                local_id: self.local_id,
                why: STOPPED_BEFORE_ITS_PASS.to_string(),
            };
        }
        if let Some(why) = self.broken.take() {
            return ExportEvent::NotExported {
                local_id: self.local_id,
                why,
            };
        }
        if let Err(error) = self.writer.flush() {
            return ExportEvent::NotExported {
                local_id: self.local_id,
                why: format!("could not write ({})", error.kind()),
            };
        }
        ExportEvent::Exported(HistoryExport {
            local_id: self.local_id,
            facts: self.facts,
            file: self.file,
            len: self.len,
            sha: self.hasher.finalize(),
            links_dropped: self.links_dropped,
        })
    }
}

/// Open one session's sidecar at `path` and write the first chunk of its
/// FIRST PASS — every line the fence covers, oldest first, in chunks under
/// short locks ([`OpenExport::pass_step`] reads the rest). The bytes are
/// charged to `aggregate` as they are written, and given back if it fails.
fn open_export(
    path: &Path,
    local_id: u64,
    term: &Arc<Mutex<Terminal>>,
    aggregate: &mut Aggregate,
) -> Result<Option<OpenExport>, String> {
    // THE FENCE AND THE FIRST CHUNK UNDER ONE LOCK (2026-09-26 review): the
    // export starts AT the oldest retained row, so on a tab at its retention
    // limit a drain between the two (it evicts up to a thousand lines at a
    // time) refused the very first read, and the whole session fell back to
    // the screen carry's 256 lines.
    let first_read = locked(term, |t| {
        let fence = t.history_fence();
        if fence.detached {
            return Err("a scrollback rewrap held the history".to_string());
        }
        if fence.lines() == 0 {
            return Ok(None);
        }
        let first = fence.oldest.max(fence.end.saturating_sub(MAX_EXPORT_LINES));
        let lines = t
            .history_lines_since_fence(&fence, first, CHUNK_LINES)
            .map_err(|broken| broken.to_string())?;
        Ok(Some((fence, first, lines)))
    })?;
    let Some((fence, first, lines)) = first_read else {
        return Ok(None);
    };
    let file = create_private_new(path).map_err(|e| format!("could not create ({})", e.kind()))?;
    let mut export = OpenExport {
        local_id,
        term: Arc::clone(term),
        facts: ExportFacts {
            fence: HistoryFence {
                end: first,
                ..fence
            },
            first,
            lines: 0,
        },
        writer: std::io::BufWriter::new(file),
        file: OwnedFile(Some(path.to_path_buf())),
        len: HEADER_LEN as u64,
        hasher: aterm_digest::Sha256::new(),
        following: true,
        broken: None,
        links_dropped: 0,
        pass: Some(fence),
    };
    let head = header(fence.cols);
    export
        .writer
        .write_all(&head)
        .map_err(|e| format!("could not write ({})", e.kind()))?;
    export.hasher.update(head);
    aggregate.written = aggregate.written.saturating_add(export.len);
    match export.pass_append(lines, aggregate) {
        Ok(()) => Ok(Some(export)),
        Err(why) => {
            aggregate.release(&export);
            Err(why)
        }
    }
}

/// Export one HANDED session's fenced history to `path`: its whole FIRST
/// PASS, given up the moment the attempt asks the worker to stop (`stalls`:
/// the tests hold it where it is, [`ExportSessions::stalled`]).
fn begin_export(
    path: &Path,
    local_id: u64,
    term: &Arc<Mutex<Terminal>>,
    stop: Stop<'_>,
    aggregate: &mut Aggregate,
    stalls: bool,
) -> Result<Option<OpenExport>, String> {
    let Some(mut export) = open_export(path, local_id, term, aggregate)? else {
        return Ok(None);
    };
    while export.pass.is_some() {
        let step = if stop.requested() {
            Err("the attempt stopped".to_string())
        } else if stalls {
            std::thread::sleep(Duration::from_millis(1));
            Ok(())
        } else {
            export.pass_step(aggregate)
        };
        if let Err(why) = step {
            aggregate.release(&export);
            return Err(why);
        }
    }
    Ok(Some(export))
}

/// One handed session's history as the park saw it — its fence, taken under
/// the lock the session's checkpoint is taken under — and where in it the
/// person was reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryHead {
    pub(crate) local_id: u64,
    pub(crate) fence: HistoryFence,
    /// Lines above the live bottom the view was scrolled
    /// (`SessionRecord::viewport_from_bottom`, round five, item 18); `None` at
    /// the bottom, and on the alternate screen, which has no history to read.
    pub(crate) viewport_from_bottom: Option<u32>,
}

/// The park's share: a pure read of the fence and the viewport.
#[must_use]
pub(crate) fn capture_head(local_id: u64, terminal: &Terminal) -> HistoryHead {
    let offset = if terminal.is_alternate_screen() {
        0
    } else {
        terminal.grid().display_offset()
    };
    HistoryHead {
        local_id,
        fence: terminal.history_fence(),
        viewport_from_bottom: (offset > 0).then(|| u32::try_from(offset).unwrap_or(u32::MAX)),
    }
}

/// Put back where the person was reading (round five, item 18): scroll the
/// view `lines` above the bottom the ADOPT restored, clamped to the scrollback
/// `terminal` holds — AFTER the import, so the lines the offset reaches are
/// there. A view that is no longer at the bottom was moved here by the person
/// since the adopt (or the alternate screen is up), and is left as it is:
/// their newer gesture wins. Returns whether the view moved.
///
/// ANCHORED ABSOLUTELY. The restore runs on the import worker, after Commit,
/// with the adopted readers already appending output; `lines` was measured
/// from the bottom AT THE PARK. `bottom_at_adopt` is the history grid's
/// absolute row counter right after the restore
/// ([`AdoptedHistory::anchor_in`]) — the park's bottom, in this engine's
/// numbering — so every line pushed into history since then is added to the
/// offset, and the view lands on the lines the person was reading rather than
/// that many lines below them. The caller moves that bottom up by whatever
/// the import's attach renumbered ([`Imported::Retained`]): a raise of the
/// counter that pushed nothing. `None` (no screen was restored) keeps the
/// offset as the park measured it.
///
/// Only a session nobody has used since the adopt is moved.
/// `gestures_at_adopt` is its [`Terminal::reader_gesture_count`] sampled at
/// the adopt ([`AdoptedHistory::anchor_in`]); a count that has moved since
/// means the person typed, pasted, scrolled, selected or went back to live in
/// it — the keys typed before Commit replay through the same seam — and their
/// newer gesture wins (round six, finding 24). A view at the bottom says
/// nothing about that: typing at the prompt leaves it there, and pulling it up
/// the screen under a half-typed command is the defect. `None` (no screen was
/// restored) has no sample to compare, and the view check alone decides.
pub(crate) fn restore_viewport(
    terminal: &mut Terminal,
    lines: u32,
    bottom_at_adopt: Option<u64>,
    gestures_at_adopt: Option<u64>,
) -> bool {
    if terminal.is_alternate_screen()
        || terminal.grid().display_offset() != 0
        || gestures_at_adopt.is_some_and(|at| terminal.reader_gesture_count() != at)
    {
        return false;
    }
    let pushed = bottom_at_adopt.map_or(0, |bottom| {
        terminal
            .main_grid()
            .absolute_row_counter()
            .saturating_sub(bottom)
    });
    let held = terminal.grid().scrollback_lines();
    let lines = usize::try_from(u64::from(lines).saturating_add(pushed))
        .unwrap_or(usize::MAX)
        .min(held);
    terminal.scroll_display(i32::try_from(lines).unwrap_or(i32::MAX));
    terminal.grid().display_offset() != 0
}

/// An adopted session's count of input SENT to it
/// ([`crate::app_input::OutputEchoTracker::input_sent`]) and its value at the
/// adopt — how the viewport restore tells that someone typed or sent
/// something since ([`AdoptedHistory::watch_input`]).
///
/// NOT the sink's input epoch: that also moves for every report the session
/// is written — a DEC 1004 focus report above all, which a focus-reporting
/// program (Claude Code) is sent the moment the successor's window takes
/// focus at Commit — and for the engine's DA/DSR replies, so the restore
/// would stand aside in exactly the session it is for. And NOT the sink
/// itself: for an owned sink the last `Arc` closes the PTY master, and a tab
/// closed while the imports run must hang up its child then, not when the
/// import worker reaches its job. The tracker holds no descriptor.
pub(crate) struct InputWatch {
    input: Arc<crate::app_input::OutputEchoTracker>,
    at: u64,
}

impl InputWatch {
    /// Whether any input was sent to the session since the watch was taken:
    /// a keystroke, a paste, a controller's `send` — never a report or an
    /// engine reply.
    #[must_use]
    fn moved(&self) -> bool {
        self.input.input_sent() != self.at
    }
}

impl std::fmt::Debug for InputWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputWatch")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

/// How many lines of the history-holding grid `checkpoint` carries: the
/// active grid's carried history, and none while the alternate screen is up
/// (the saved primary is carried at its visible rows).
#[must_use]
pub(crate) fn carried_history(checkpoint: &TerminalCheckpoint) -> u64 {
    if checkpoint.modes.alternate_screen {
        0
    } else {
        u64::from(checkpoint.history_lines)
    }
}

/// Why a session's history carry fell back to the checkpoint's bounded one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// Nothing was exported for it (the export was not asked for, had not
    /// reached it, or refused it — the refusal is logged by the join).
    NotExported,
    /// The history changed after it was exported: a rewrap, an unscroll, a
    /// clear, a reset, or a rewrap in flight at the park.
    FenceMoved,
    /// More output arrived between the export and the park than the
    /// checkpoint carries, so the two do not meet.
    Outrun,
    /// The successor's signed handoff policy carries no scrollback
    /// ([`HistoryPlan::Withheld`]): nothing was exported, by request.
    Withheld,
}

impl Fallback {
    /// The words the log and the successor's row use.
    #[must_use]
    pub(crate) fn words(self) -> &'static str {
        match self {
            Self::NotExported => "its history was not exported before the park",
            Self::FenceMoved => "its history was rewrapped, cleared or reset after the export",
            Self::Outrun => "more output arrived after the export than the screen carry holds",
            Self::Withheld => "the successor's handoff policy carries no scrollback",
        }
    }
}

/// THE JOIN's verdict for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Joined {
    /// The sidecar's first `take` lines cross (0: no sidecar is named).
    pub(crate) take: u64,
    /// History lines the park saw that the successor will not hold, even if
    /// every carry arrives intact.
    pub(crate) dropped: u64,
    /// Why the sidecar is not used, when it is not.
    pub(crate) fallback: Option<Fallback>,
}

/// THE JOIN — see the module doc. Pure, so the Tier-1 bind drives exactly
/// what ships: `export` is what the export fenced (if anything), `head` the
/// history as the park saw it, `carried` how many of its newest lines the
/// checkpoint carries.
///
/// Carried, the successor holds `[first, head.end)`: the sidecar's `take`
/// lines up to where the checkpoint's begin, then the checkpoint's. Dropped
/// are the park's lines older than `first` (a history deeper than
/// [`MAX_EXPORT_LINES`]). Not carried, it holds the checkpoint's lines alone
/// and every other line the park saw is dropped.
#[must_use]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateHistoryCarry",
        action = "Park",
        project = "aterm_gui::handoff_history::conformance::project"
    )
)]
pub(crate) fn join(export: Option<ExportFacts>, head: HistoryFence, carried: u64) -> Joined {
    let carried = carried.min(head.lines());
    let fallback = |why| Joined {
        take: 0,
        dropped: head.lines() - carried,
        fallback: Some(why),
    };
    let Some(export) = export else {
        return if head.lines() > carried {
            fallback(Fallback::NotExported)
        } else {
            // Everything the park saw is in the checkpoint: nothing to join.
            Joined {
                take: 0,
                dropped: 0,
                fallback: None,
            }
        };
    };
    if !export.fence.holds_at(&head) {
        return fallback(Fallback::FenceMoved);
    }
    // Where the checkpoint's lines begin. The export covers `[first, end)`;
    // the two meet only when the checkpoint reaches back to the export's end.
    let carried_from = head.end - carried;
    if carried_from > export.fence.end {
        return fallback(Fallback::Outrun);
    }
    let take = carried_from.saturating_sub(export.first).min(export.lines);
    let dropped = export.first.saturating_sub(head.oldest);
    Joined {
        take,
        dropped,
        fallback: None,
    }
}

/// Stamp every record of `manifest` with its session's history carry (and
/// what does not cross), linking each used sidecar to its attempt-bound name
/// `<dir>/seamless-<pid>-<nonce>.s<id>.hist`. Sidecars not used are removed
/// when `results` drops. Returns each session's verdict, for the log.
pub(crate) fn stamp_manifest(
    manifest: &mut crate::session_store::SessionHandoff,
    screens: &[(u64, TerminalCheckpoint)],
    heads: &[HistoryHead],
    mut results: ExportResults,
    dir: &Path,
    nonce: &str,
) -> Vec<(u64, Joined)> {
    let mut verdicts = Vec::with_capacity(manifest.sessions.len());
    for record in &mut manifest.sessions {
        let (Some((_, checkpoint)), Some(head)) = (
            screens.iter().find(|(id, _)| *id == record.local_id),
            heads.iter().find(|head| head.local_id == record.local_id),
        ) else {
            continue;
        };
        let export = results
            .exports
            .iter()
            .position(|export| export.local_id == record.local_id)
            .map(|at| results.exports.swap_remove(at));
        let mut joined = join(
            export.as_ref().map(|export| export.facts),
            head.fence,
            carried_history(checkpoint),
        );
        // A withheld plan exported nothing, so every session it left lines
        // behind for is "not exported" — at the policy's request, and said so.
        if results.withheld && joined.fallback == Some(Fallback::NotExported) {
            joined.fallback = Some(Fallback::Withheld);
        }
        if joined.take > 0
            && let Some(export) = export
        {
            let named = dir.join(format!(
                "seamless-{}-{nonce}.s{}.hist",
                std::process::id(),
                record.local_id
            ));
            // A hard link is the O_EXCL of a rename: it never replaces what
            // is already at the name.
            let links_dropped = export.links_dropped;
            let source = export.file.keep();
            match std::fs::hard_link(&source, &named) {
                Ok(()) => {
                    record.history = Some(stamp(export.len, &export.sha, joined.take));
                    if links_dropped > 0 {
                        aterm_log::warn!(
                            "update apply: session {}'s scrollback crosses with the links of \
                             {links_dropped} line(s) dropped: each was over the {}-byte \
                             line-record cap because of its links; their text is exact",
                            record.local_id,
                            line_caps(export.facts.fence.cols).1
                        );
                    }
                }
                Err(_) => {
                    joined.dropped += joined.take;
                    joined.take = 0;
                    joined.fallback = Some(Fallback::NotExported);
                }
            }
            let _ = std::fs::remove_file(source);
        }
        record.history_dropped = joined.dropped;
        // Why they stayed behind, when the policy chose it: the successor
        // says a withheld carry as the new build's request, never as a
        // failure that kept "the newest lines" (it kept none).
        record.history_withheld = joined.fallback == Some(Fallback::Withheld);
        record.history_lost = record.history_lost.saturating_add(joined.dropped);
        // Where the person was reading, as the park saw it (round five, item 18).
        record.viewport_from_bottom = head.viewport_from_bottom;
        verdicts.push((record.local_id, joined));
    }
    for (local_id, why) in &results.refused {
        aterm_log::warn!(
            "update apply: session {local_id}'s scrollback was not exported ({why}); it carries \
             the screen carry's bounded history"
        );
    }
    if results.unfinished {
        aterm_log::warn!(
            "update apply: the scrollback export had not finished at the park; the sessions it \
             had not reached carry the screen carry's bounded history"
        );
    }
    verdicts
}

impl ExportResults {
    /// The exports of the panes `held` names, taken out of these results for
    /// the held carry ([`name_held_sidecar`]; round eight of the update audit,
    /// finding 39), so [`stamp_manifest`] — which joins the handed sessions'
    /// records — never sees them; and why a held pane with none has none
    /// ([`HeldExports::unexported`]): the policy's withholding, as the handed
    /// records say it; else `older_successor` when the attempt hands to a
    /// build older than this one ([`exports_for_target`] false); else a
    /// failure.
    pub(crate) fn take_held(&mut self, held: &[u64], older_successor: bool) -> HeldExports {
        let (exports, kept) = std::mem::take(&mut self.exports)
            .into_iter()
            .partition(|export| held.contains(&export.local_id));
        self.exports = kept;
        // A held pane's refusal is said as a held pane's — its screen is its
        // visible rows, so no bounded history stands in for what did not
        // cross — and kept out of the handed sessions' log.
        let (refused, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.refused)
            .into_iter()
            .partition(|(local_id, _)| held.contains(local_id));
        self.refused = kept;
        for (local_id, why) in refused {
            aterm_log::warn!(
                "update apply: held pane {local_id}'s scrollback was not exported ({why}); its \
                 lines stay behind, counted"
            );
        }
        HeldExports {
            exports,
            unexported: if self.withheld {
                HeldLoss::Withheld
            } else if older_successor {
                HeldLoss::ScreenOnly
            } else {
                HeldLoss::Failed
            },
        }
    }
}

/// WHY A HELD PANE'S SCROLLBACK LINES DID NOT CROSS (the review of round
/// eight's finding 39), which the producer writes beside its count
/// (`HeldPaneCarry::history_withheld` / `history_failed`) and the successor
/// says on the band: the words true of a choice are false of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldLoss {
    /// BY DESIGN: the hop carries a held pane's screen alone — a successor
    /// older than this build has no held importer ([`exports_for_target`]),
    /// so nothing was exported for it. Said as a held pane's
    /// ([`AdoptedHistory::held`]).
    ScreenOnly,
    /// BY REQUEST: the successor's signed handoff policy carries no
    /// scrollback ([`HistoryPlan::Withheld`]). Said as the new version's
    /// request ([`AdoptedHistory::withheld`]).
    Withheld,
    /// A FAILURE: the carry was offered and did not cross — the export had
    /// not reached the pane, refused it or never started, a resize moved its
    /// fence, the sidecar could not be named, or the history was deeper than
    /// one export carries. Said as a failure, and never with the failure
    /// row's "the newest lines came across": a held screen carries none
    /// ([`LossSummary::held_failed`]).
    Failed,
}

impl HeldLoss {
    /// The reason a successor reads off a held pane's two wire keys: a record
    /// that says both is read as failed, so a malformed one never makes a
    /// failure read as a choice.
    #[must_use]
    pub(crate) const fn from_wire(withheld: bool, failed: bool) -> Self {
        if failed {
            Self::Failed
        } else if withheld {
            Self::Withheld
        } else {
            Self::ScreenOnly
        }
    }
}

/// The held panes' share of an attempt's export ([`ExportResults::take_held`]):
/// each one's export, and why a pane with none has none.
#[derive(Debug)]
pub(crate) struct HeldExports {
    pub(crate) exports: Vec<HistoryExport>,
    pub(crate) unexported: HeldLoss,
}

impl Default for HeldExports {
    /// No export and no plan to say why: a failure, never a choice.
    fn default() -> Self {
        Self {
            exports: Vec::new(),
            unexported: HeldLoss::Failed,
        }
    }
}

/// What a held pane's scrollback carry came to ([`name_held_sidecar`]): the
/// stamp of the sidecar it names (`HeldPaneCarry::history`), the lines that do
/// not cross and why ([`HeldLoss`], meaningful when `dropped` is not 0), and
/// the named file, which the manifest writer retires with the rest of the
/// attempt if it fails.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeldHistory {
    pub(crate) stamp: Option<String>,
    pub(crate) dropped: u64,
    pub(crate) why: HeldLoss,
    pub(crate) path: Option<PathBuf>,
}

/// A HELD PANE'S SCROLLBACK CROSSES AS A HANDED SESSION'S DOES (round eight of
/// the update audit, finding 39). A pane kept open after its command exited
/// (`--hold`, a kept failed start) is no handed session: its screen rides the
/// manifest's held list, visible rows only, and its scrollback used to be left
/// behind on every update. Its history is exported beside the handed
/// sessions' (`App::history_sessions`), and here joined with `head`, the fence
/// its held capture took, exactly as [`join`] joins a session's — the screen
/// carrying no line of it — and linked to `named`, its attempt-bound name
/// `<dir>/seamless-<pid>-<nonce>.h<id>.hist` (the `h` keeps it apart from a
/// session's `.s<id>.hist`). No export (a successor older than this build,
/// a policy that carries no scrollback, an export that had not reached it), a
/// fence that moved (a resize rewrapped it) or a link that cannot be made
/// leaves the lines behind, counted, with WHY ([`HeldLoss`]): with no export,
/// `unexported` — the attempt's reason ([`HeldExports::unexported`]) — and
/// with one, a failure.
pub(crate) fn name_held_sidecar(
    export: Option<HistoryExport>,
    head: HistoryFence,
    named: PathBuf,
    unexported: HeldLoss,
) -> HeldHistory {
    let joined = join(export.as_ref().map(|export| export.facts), head, 0);
    let why = if export.is_some() {
        HeldLoss::Failed
    } else {
        unexported
    };
    let Some(export) = export.filter(|_| joined.take > 0) else {
        return HeldHistory {
            stamp: None,
            dropped: joined.dropped,
            why,
            path: None,
        };
    };
    let source = export.file.keep();
    let named = match std::fs::hard_link(&source, &named) {
        Ok(()) => HeldHistory {
            stamp: Some(stamp(export.len, &export.sha, joined.take)),
            dropped: joined.dropped,
            why,
            path: Some(named),
        },
        Err(_) => HeldHistory {
            stamp: None,
            dropped: joined.dropped.saturating_add(joined.take),
            why,
            path: None,
        },
    };
    let _ = std::fs::remove_file(source);
    named
}

/// Say what the join decided: one line for the sessions whose history
/// crosses whole, one WARN per session that falls back, with its count.
pub(crate) fn log_verdicts(verdicts: &[(u64, Joined)]) {
    let carried: Vec<String> = verdicts
        .iter()
        .filter(|(_, joined)| joined.take > 0)
        .map(|(id, joined)| format!("{id}:{}", joined.take))
        .collect();
    if !carried.is_empty() {
        aterm_log::info!(
            "update apply: scrollback sidecars named for {} session(s) (session:lines {})",
            carried.len(),
            carried.join(" ")
        );
    }
    // The policy's withholding is one decision, said once: which tabs, and how
    // many lines stay behind in each.
    let withheld: Vec<String> = verdicts
        .iter()
        .filter(|(_, joined)| joined.fallback == Some(Fallback::Withheld) && joined.dropped > 0)
        .map(|(id, joined)| format!("{id}:{}", joined.dropped))
        .collect();
    if !withheld.is_empty() {
        aterm_log::info!(
            "update apply: {}, so no tab's scrollback crosses; {} tab(s) leave theirs behind, \
             counted (status history_lost=; session:lines {})",
            Fallback::Withheld.words(),
            withheld.len(),
            withheld.join(" ")
        );
    }
    for (local_id, joined) in verdicts {
        match joined.fallback {
            Some(Fallback::Withheld) => {}
            Some(why) if joined.dropped > 0 => aterm_log::warn!(
                "update apply: session {local_id} carries the screen carry's bounded history — \
                 {}; {} line(s) of its scrollback stay behind, counted (status history_lost=)",
                why.words(),
                joined.dropped
            ),
            None if joined.dropped > 0 => aterm_log::warn!(
                "update apply: session {local_id}'s history is deeper than the {MAX_EXPORT_LINES} \
                 lines one export carries; its {} oldest line(s) stay behind, counted",
                joined.dropped
            ),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The incoming side: take before the proof, import after Commit
// ---------------------------------------------------------------------------

/// One session's history sidecar, opened (and unlinked) before the proof and
/// read only after Commit.
pub(crate) struct HistoryCarry {
    file: std::fs::File,
    len: u64,
    sha: [u8; 32],
    take: u64,
}

impl std::fmt::Debug for HistoryCarry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryCarry")
            .field("len", &self.len)
            .field("take", &self.take)
            .finish_non_exhaustive()
    }
}

impl HistoryCarry {
    /// How many lines the carry names — what is lost if it fails.
    #[must_use]
    pub(crate) fn take(&self) -> u64 {
        self.take
    }

    /// Read, check and decode the sidecar — see [`read_sidecar`].
    fn read_verified(self) -> Result<SidecarLines, &'static str> {
        read_sidecar(
            std::io::BufReader::new(self.file),
            self.len,
            &self.sha,
            self.take,
        )
    }
}

/// Open the sidecar `stamp` names at its exact `path` inside `dir` — a
/// regular file, never through a symlink, exactly the stamped length — and
/// unlink it on every outcome. `remaining` is the handoff's sidecar budget.
/// `Err` names why; the caller counts the stamp's lines as lost.
pub(crate) fn take_sidecar(
    path: &Path,
    dir: &Path,
    stamp: &str,
    remaining: &mut u64,
) -> Result<HistoryCarry, &'static str> {
    let result = (|| {
        let (len, sha, take) = parse_stamp(stamp).ok_or("a malformed stamp")?;
        if !path.starts_with(dir) {
            return Err("a sidecar outside the control dir");
        }
        if len > MAX_SIDECAR_BYTES || len > *remaining || take > MAX_EXPORT_LINES {
            return Err("a sidecar over its byte budget");
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // O_NONBLOCK for the reason `seamless::take_regular_capped` gives:
            // a FIFO at the path must not block the proof.
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| "a sidecar that is not there")?;
        let metadata = file.metadata().map_err(|_| "an unreadable sidecar")?;
        if !metadata.is_file() || metadata.len() != len {
            return Err("a sidecar of another length");
        }
        *remaining -= len;
        Ok(HistoryCarry {
            file,
            len,
            sha,
            take,
        })
    })();
    let _ = std::fs::remove_file(path);
    result
}

/// What an adopted session brings to the import after Commit: its sidecar
/// (if one was named and opened), the lines this handoff already could not
/// carry for it (`dropped`), its running total of lines lost across every
/// handoff it crossed (`lost`, what `status` shows), and the claim its keys
/// were reserved under (`spawn::hydrate_adopted_engine`) — the successor's
/// fence, which a scrollback clear before the import breaks.
#[derive(Debug, Default)]
pub(crate) struct AdoptedHistory {
    pub(crate) carry: Option<HistoryCarry>,
    pub(crate) dropped: u64,
    /// Of `dropped`, the lines the outgoing process left behind because the
    /// successor's handoff policy carries no scrollback
    /// (`SessionRecord::history_withheld`) — a choice, said as one — rather
    /// than for want of a way to carry them.
    pub(crate) withheld: u64,
    /// Of `dropped`, the lines a HELD pane's outgoing process left behind BY
    /// DESIGN (an exited pane kept open, `--hold` or a kept failed start;
    /// round eight of the update audit, finding 39): the hop carried its
    /// screen alone ([`HeldLoss::ScreenOnly`], [`incoming_held`]).
    pub(crate) held: u64,
    /// This is a held pane's history ([`incoming_held`]): its screen carried
    /// no line of its scrollback, so none of its newest lines crossed. Its
    /// lines that are neither `withheld` nor `held` are a FAILED carry, said
    /// as one but never with "the newest lines came across"
    /// ([`LossSummary::held_failed`]).
    pub(crate) held_pane: bool,
    pub(crate) lost: u64,
    pub(crate) claim: Option<OlderHistoryClaim>,
    /// Where the person was reading at the park
    /// (`SessionRecord::viewport_from_bottom`), put back after the import
    /// ([`restore_viewport`]). `None` from an older producer: the bottom.
    pub(crate) viewport_from_bottom: Option<u32>,
    /// The history grid's absolute row counter right after the adopt restored
    /// the screen ([`Self::anchor_in`]): the bottom `viewport_from_bottom` was
    /// measured from, in this engine's numbering. `None` when nothing was
    /// restored.
    pub(crate) bottom_at_adopt: Option<u64>,
    /// The session's input count at the adopt ([`Self::watch_input`]). Input
    /// sent since — the person typing at the prompt, which the viewport
    /// restore would otherwise yank away from them — skips the restore.
    pub(crate) input_at_adopt: Option<InputWatch>,
    /// The engine's [`Terminal::reader_gesture_count`] at the adopt
    /// ([`Self::anchor_in`]): the witness that nobody typed, pasted, scrolled,
    /// selected or went back to live in the session before the import puts the
    /// view back. `None` when nothing was restored.
    pub(crate) gestures_at_adopt: Option<u64>,
    /// The scrollback the adopt restored ([`Self::anchor_in`]): with the lines
    /// the import puts back under it, the only history the park's offset can
    /// point into here. Lines output pushes into history after the adopt are
    /// NOT the lines the person was reading, so the restore never reaches past
    /// this depth (round seven, item 21). `None` when nothing was restored.
    pub(crate) history_at_adopt: Option<u64>,
}

impl AdoptedHistory {
    /// The absolute-row keys to reserve for the import.
    #[must_use]
    pub(crate) fn reserve(&self) -> u64 {
        self.carry.as_ref().map_or(0, HistoryCarry::take)
    }

    /// Reserve the import's keys in the engine just restored from the
    /// checkpoint, and keep the claim for the import — the adopt's share,
    /// under the lock the restore took (`spawn::hydrate_adopted_engine`).
    pub(crate) fn reserve_in(&mut self, engine: &mut Terminal) {
        if self.carry.is_some() {
            self.claim = Some(engine.reserve_older_history_keys(self.reserve()));
        }
    }

    /// Note the bottom the restored screen sits at, for the viewport restore
    /// ([`restore_viewport`]): the history grid's absolute row counter, after
    /// the reserve (which may raise it) and before the session's reader runs.
    /// The adopt's share, under the lock the restore took.
    pub(crate) fn anchor_in(&mut self, engine: &Terminal) {
        self.bottom_at_adopt = Some(engine.main_grid().absolute_row_counter());
        self.gestures_at_adopt = Some(engine.reader_gesture_count());
        self.history_at_adopt =
            Some(u64::try_from(engine.main_grid().scrollback_lines()).unwrap_or(u64::MAX));
    }

    /// Note how much input the session had been sent at the adopt
    /// ([`InputWatch`]), before its reader starts or anything typed into the
    /// revealed window is replayed: input after it means the person has taken
    /// the session back, and the viewport restore stands aside.
    pub(crate) fn watch_input(&mut self, input: &Arc<crate::app_input::OutputEchoTracker>) {
        self.input_at_adopt = Some(InputWatch {
            input: Arc::clone(input),
            at: input.input_sent(),
        });
    }
}

/// The successor's reading of one record's history carry, before the proof
/// (`seamless::take_incoming`): the record's counts, and its sidecar opened
/// at `path` when it names one. A sidecar that cannot be opened, or one whose
/// session's screen this build refused (`screen_degraded`: the carried lines
/// the sidecar's newest line meets went with that screen, so importing it
/// would put a hole in the history), is removed and its lines counted.
///
/// The record's lines are the policy's withholding when it says so
/// (`history_withheld`, v0.98.0 on) or when `followed_no_scrollback`: the
/// record's producer is v0.97.0, this build's own sealed policy asked it for
/// no scrollback, and the record carries none — what v0.97.0 writes when it
/// follows one, with no way to say why (`seamless::take_incoming_from`; round
/// eight of the update audit, finding 87).
pub(crate) fn incoming(
    record: &crate::session_store::SessionRecord,
    screen_degraded: bool,
    followed_no_scrollback: bool,
    path: &Path,
    dir: &Path,
    remaining: &mut u64,
) -> AdoptedHistory {
    let mut history = AdoptedHistory {
        carry: None,
        dropped: record.history_dropped,
        withheld: if record.history_withheld || followed_no_scrollback {
            record.history_dropped
        } else {
            0
        },
        held: 0,
        held_pane: false,
        lost: record.history_lost,
        claim: None,
        viewport_from_bottom: record.viewport_from_bottom,
        bottom_at_adopt: None,
        input_at_adopt: None,
        gestures_at_adopt: None,
        history_at_adopt: None,
    };
    let Some(stamp) = record.history.as_deref() else {
        return history;
    };
    let named = parse_stamp(stamp).map_or(0, |(_, _, take)| take);
    let refused = if screen_degraded {
        let _ = std::fs::remove_file(path);
        Err("its screen was refused here")
    } else {
        take_sidecar(path, dir, stamp, remaining)
    };
    match refused {
        Ok(carry) => history.carry = Some(carry),
        Err(why) => {
            aterm_log::warn!(
                "overlap handoff: session {}'s scrollback sidecar was not taken ({why}); {named} \
                 line(s) of its history stay behind, counted (status history_lost=)",
                record.local_id
            );
            history.dropped = history.dropped.saturating_add(named);
            history.lost = history.lost.saturating_add(named);
        }
    }
    history
}

/// The successor's reading of one HELD pane's scrollback carry, before the
/// proof (`seamless::take_incoming`; round eight of the update audit, finding
/// 39): `dropped` lines its outgoing process could not carry
/// (`HeldPaneCarry::history_dropped`), said by `why` — as a held pane's
/// ([`AdoptedHistory::held`]) when the hop carried its screen alone, as the
/// new version's request ([`AdoptedHistory::withheld`]) under a policy that
/// carries no scrollback, and as a failure when the carry failed — and the
/// sidecar `stamp` names opened at `path` as [`incoming`] opens a session's.
/// A sidecar that cannot be taken costs its lines, counted as a failure.
pub(crate) fn incoming_held(
    stamp: Option<&str>,
    dropped: u64,
    why: HeldLoss,
    local_id: u64,
    path: &Path,
    dir: &Path,
    remaining: &mut u64,
) -> AdoptedHistory {
    let mut history = AdoptedHistory {
        dropped,
        held: if why == HeldLoss::ScreenOnly {
            dropped
        } else {
            0
        },
        withheld: if why == HeldLoss::Withheld {
            dropped
        } else {
            0
        },
        held_pane: true,
        lost: dropped,
        ..AdoptedHistory::default()
    };
    let Some(stamp) = stamp else {
        return history;
    };
    let named = parse_stamp(stamp).map_or(0, |(_, _, take)| take);
    match take_sidecar(path, dir, stamp, remaining) {
        Ok(carry) => history.carry = Some(carry),
        Err(why) => {
            aterm_log::warn!(
                "overlap handoff: held pane {local_id}'s scrollback sidecar was not taken \
                 ({why}); {named} line(s) of its history stay behind, counted"
            );
            history.dropped = history.dropped.saturating_add(named);
            history.lost = history.lost.saturating_add(named);
        }
    }
    history
}

/// One session's import, as the successor settles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportReport {
    pub(crate) session: u64,
    /// Lines placed in front of the session's history (after the grid's own
    /// retention).
    pub(crate) imported: u64,
    /// This handoff's lines the outgoing side could not carry (already on
    /// the session's running count since its registration).
    pub(crate) dropped: u64,
    /// Of `dropped`, the lines a successor's handoff policy asked the
    /// outgoing side to leave behind ([`AdoptedHistory::withheld`]).
    pub(crate) withheld: u64,
    /// Of `dropped`, the lines a held pane's outgoing process left behind
    /// by design ([`AdoptedHistory::held`]).
    pub(crate) held: u64,
    /// The session is a held pane ([`AdoptedHistory::held_pane`]).
    pub(crate) held_pane: bool,
    /// The named carry's lines, when it failed HERE (not yet counted).
    pub(crate) failed_lines: u64,
    /// Of `imported`, the lines whose records were over the line-record cap
    /// because of their OSC 8 links and crossed with those links dropped (a
    /// producer through v0.97.0 wrote them so; [`read_sidecar`]). Their text
    /// crossed, so they are not lost lines; the log names them.
    pub(crate) links_dropped: u64,
    /// Why a named carry failed here, when one did.
    pub(crate) failed: Option<String>,
    /// The pane's scrollback was CLEARED between the adopt and the import
    /// (an ED3 or a reset the adopted shell sent): the carry was not put back
    /// and nothing is counted — the user erased what it held.
    pub(crate) cleared: bool,
}

impl ImportReport {
    /// Every line this handoff could not carry for the session.
    #[must_use]
    pub(crate) fn lost(&self) -> u64 {
        self.dropped.saturating_add(self.failed_lines)
    }

    /// Of [`Self::lost`], the lines a handoff policy withheld — never more
    /// than the outgoing side's own count, so a malformed record cannot make
    /// a failure here read as the policy's.
    #[must_use]
    pub(crate) fn withheld(&self) -> u64 {
        self.withheld.min(self.dropped)
    }

    /// Of [`Self::lost`], the lines a held pane's carry left behind by
    /// design, bounded the same way.
    #[must_use]
    pub(crate) fn held(&self) -> u64 {
        self.held.min(self.dropped)
    }

    /// Of [`Self::lost`], a HELD pane's lines whose carry failed — on the
    /// outgoing side or here ([`held_failed`]).
    #[must_use]
    pub(crate) fn held_failed(&self) -> u64 {
        held_failed(self.held_pane, self.lost(), self.withheld(), self.held())
    }

    /// The report for a job whose import never ran (its worker could not
    /// start): everything it named is lost.
    #[must_use]
    pub(crate) fn not_run(job: &ImportJob, why: &str) -> Self {
        Self {
            session: job.session,
            imported: 0,
            dropped: job.history.dropped,
            withheld: job.history.withheld,
            held: job.history.held,
            held_pane: job.history.held_pane,
            failed_lines: job.history.reserve(),
            links_dropped: 0,
            failed: job.history.carry.as_ref().map(|_| why.to_string()),
            cleared: false,
        }
    }
}

/// How one import settled when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Imported {
    /// Placed in front of the history: how many lines the pane's own
    /// retention kept, and how many of the sidecar's lines crossed with
    /// their links dropped ([`SidecarLines::links_dropped`]).
    ///
    /// `renumbered` is how far the attach itself raised the history grid's
    /// absolute row counter (a rewrap that made the history longer than the
    /// adopt reserved, or a reserve that fell short): every key moved up by
    /// that much, the adopt's bottom ([`AdoptedHistory::bottom_at_adopt`])
    /// with them, and none of it is output pushed into history.
    Retained {
        kept: u64,
        links_dropped: u64,
        renumbered: u64,
    },
    /// The pane's scrollback was cleared since the adopt: not put back.
    Cleared,
}

/// Import one carry into `term` under `claim` — see the module doc.
/// Blocking; the import worker's. A pane whose scrollback was cleared since
/// the adopt is not imported into at all, before the sidecar is read and
/// again at the attach ([`Imported::Cleared`]).
///
/// A HELD PANE'S LINES CROSS WITHOUT A LINK (`held_pane`): its screen is shown
/// with every hyperlink dropped (`seamless::take_held_panes`), because nothing
/// runs behind it and its carry is outside the adoption proof, so a link there
/// could only be a click that opens a URL; its scrollback, imported in front of
/// that screen, keeps every glyph and drops every link the same way. A handed
/// session's lines keep theirs.
pub(crate) fn import(
    term: &Mutex<Terminal>,
    carry: HistoryCarry,
    claim: OlderHistoryClaim,
    held_pane: bool,
) -> Result<Imported, String> {
    if !locked(term, |t| t.older_history_claim_holds(claim)) {
        return Ok(Imported::Cleared);
    }
    let SidecarLines {
        cols: from_cols,
        mut lines,
        links_dropped,
    } = carry
        .read_verified()
        .map_err(|why| format!("the sidecar arrived with {why}"))?;
    if held_pane {
        for line in &mut lines {
            line.clear_hyperlinks();
        }
    }
    let deadline = std::time::Instant::now() + IMPORT_PATIENCE;
    loop {
        let cols = locked(term, |t| t.history_cols());
        let older = OlderHistory::build(&lines, from_cols, cols);
        // The counter is read on either side of the attach under the one
        // lock, so no output the reader appends can be mistaken for the
        // attach's own renumbering.
        let attached = locked(term, |t| {
            let before = t.main_grid().absolute_row_counter();
            t.attach_older_history(older, claim).map(|retained| {
                let raised = t.main_grid().absolute_row_counter().saturating_sub(before);
                (retained, raised)
            })
        });
        match attached {
            Ok((retained, renumbered)) => {
                return Ok(Imported::Retained {
                    kept: retained as u64,
                    links_dropped,
                    renumbered,
                });
            }
            Err(OlderHistoryRefusal::Cleared(_)) => return Ok(Imported::Cleared),
            Err(OlderHistoryRefusal::NoTieredStore(_)) => {
                return Err("the pane keeps no history store to import into".to_string());
            }
            Err(refusal) if std::time::Instant::now() >= deadline => {
                return Err(format!("{refusal} for {} s", IMPORT_PATIENCE.as_secs()));
            }
            Err(_) => std::thread::sleep(IMPORT_RETRY),
        }
    }
}

/// Count each failed carry's lines onto its session's running
/// `history_lost` (what `status` says), and log each session's import. The
/// import worker's last step, so the count is on the registry whether or
/// not its wake reaches the event loop. Takes the registry lock with no
/// terminal lock held (the store's one rule).
pub(crate) fn record_reports(store: &crate::session_store::Store, reports: &[ImportReport]) {
    {
        let mut store = store.write().unwrap_or_else(|p| p.into_inner());
        for report in reports {
            store.add_history_lost(report.session, report.failed_lines);
        }
    }
    for report in reports {
        match &report.failed {
            Some(why) => aterm_log::warn!(
                "overlap handoff: session {} could not import its scrollback ({why}); {} \
                 line(s) of its history stay behind, counted (status history_lost=)",
                report.session,
                report.failed_lines
            ),
            None if report.cleared => aterm_log::info!(
                "overlap handoff: session {}'s scrollback was cleared before its carried \
                 history landed; that history stays cleared",
                report.session
            ),
            None if report.imported > 0 => aterm_log::info!(
                "overlap handoff: session {} imported {} line(s) of scrollback from its \
                 predecessor",
                report.session,
                report.imported
            ),
            None => {}
        }
        if report.failed.is_none() && report.links_dropped > 0 {
            aterm_log::warn!(
                "overlap handoff: session {}'s scrollback crossed with the links of {} line(s) \
                 dropped: each was over the line-record cap because of its links; their text \
                 is exact",
                report.session,
                report.links_dropped
            );
        }
    }
}

/// This update's loss across every adopted session, as the band says it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LossSummary {
    /// Every line left behind…
    pub(crate) lines: u64,
    /// …in how many tabs…
    pub(crate) tabs: usize,
    /// …and how many of those lines a successor's handoff policy asked the
    /// update to leave. The row says a withheld carry as the new build's
    /// request and a failed one as a failure; the words that are true of the
    /// one are false of the other (`update_words::scrollback_left_behind`).
    pub(crate) withheld: u64,
    /// …and how many held panes' outgoing process left behind by design
    /// ([`AdoptedHistory::held`])…
    pub(crate) held: u64,
    /// …and how many of a HELD pane's lines a carry FAILED to bring
    /// ([`AdoptedHistory::held_pane`]): a failure, but one under which no
    /// newest line crossed, so the failure row's comfort is false of them
    /// (the review of round eight's finding 39).
    pub(crate) held_failed: u64,
}

/// This update's loss across every adopted session: the lines left behind,
/// in how many tabs, and how many of them a policy withheld.
#[must_use]
pub(crate) fn loss_summary(reports: &[ImportReport]) -> LossSummary {
    summarize(reports.iter().map(|report| {
        (
            report.lost(),
            report.withheld(),
            report.held(),
            report.held_failed(),
        )
    }))
}

/// Of a session's `lost` lines, the ones a HELD pane's carry failed to bring:
/// all of them that are neither withheld nor held by design, for a held pane;
/// none for a handed session, whose failures keep the failure row.
#[must_use]
const fn held_failed(held_pane: bool, lost: u64, withheld: u64, held: u64) -> u64 {
    if held_pane {
        lost.saturating_sub(withheld).saturating_sub(held)
    } else {
        0
    }
}

/// The part of [`loss_summary`] the successor already holds AT COMMIT, before
/// any import has run: each adopted session's `dropped` — the lines the
/// outgoing process counted as left behind at its park (the record's
/// `history_dropped`), plus a named sidecar this build could not take before
/// its proof ([`incoming`]) — and the share of them a handoff policy withheld.
/// The same fold over the same counts [`ImportReport`] carries on, so an
/// import that fails nothing settles to exactly this.
#[must_use]
pub(crate) fn carried_loss<'a>(
    histories: impl IntoIterator<Item = &'a AdoptedHistory>,
) -> LossSummary {
    summarize(histories.into_iter().map(|history| {
        let withheld = history.withheld.min(history.dropped);
        let held = history.held.min(history.dropped);
        (
            history.dropped,
            withheld,
            held,
            held_failed(history.held_pane, history.dropped, withheld, held),
        )
    }))
}

/// `(lost, withheld, held, held_failed)` per session, folded into the band's
/// one summary.
fn summarize(losses: impl IntoIterator<Item = (u64, u64, u64, u64)>) -> LossSummary {
    losses.into_iter().filter(|&(lost, ..)| lost > 0).fold(
        LossSummary::default(),
        |sum, (lost, withheld, held, held_failed)| LossSummary {
            lines: sum.lines.saturating_add(lost),
            tabs: sum.tabs + 1,
            withheld: sum.withheld.saturating_add(withheld),
            held: sum.held.saturating_add(held),
            held_failed: sum.held_failed.saturating_add(held_failed),
        },
    )
}

/// One adopted session's import job.
pub(crate) struct ImportJob {
    pub(crate) session: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) history: AdoptedHistory,
}

/// Run every job, in order, and report each.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateHistoryCarry",
        action = "Settle",
        project = "aterm_gui::handoff_history::conformance::project"
    )
)]
pub(crate) fn run_imports(jobs: Vec<ImportJob>) -> Vec<ImportReport> {
    jobs.into_iter()
        .map(|job| {
            let ImportJob {
                session,
                term,
                history,
            } = job;
            let AdoptedHistory {
                carry,
                dropped,
                withheld,
                held,
                held_pane,
                claim,
                viewport_from_bottom,
                bottom_at_adopt,
                input_at_adopt,
                gestures_at_adopt,
                history_at_adopt,
                ..
            } = history;
            // What the attach renumbered: added to the adopt's bottom, so the
            // restore counts only the lines output pushed into history.
            let mut renumbered = 0;
            let (imported, links_dropped, failed_lines, failed, cleared) = match (carry, claim) {
                (None, _) => (0, 0, 0, None, false),
                // The adopt reserves a claim for every carry it keeps; one
                // without is a carry nothing was restored in front of.
                (Some(carry), None) => (
                    0,
                    0,
                    carry.take(),
                    Some("no screen was restored for it to meet".to_string()),
                    false,
                ),
                (Some(carry), Some(claim)) => {
                    let take = carry.take();
                    match import(&term, carry, claim, held_pane) {
                        Ok(Imported::Retained {
                            kept,
                            links_dropped,
                            renumbered: raised,
                        }) => {
                            renumbered = raised;
                            (kept, links_dropped, 0, None, false)
                        }
                        Ok(Imported::Cleared) => (0, 0, 0, None, true),
                        Err(why) => (0, 0, take, Some(why), false),
                    }
                }
            };
            // WHERE THE PERSON WAS READING (round five, item 18), once the
            // history is back under it — or, with no carry to import, over the
            // history the screen carry brought. Not once input has been sent
            // to the session since the adopt: a person typing at the prompt
            // has taken the view back, and the restore would yank it from them.
            // AND ONLY INTO HISTORY THAT CROSSED (round seven, item 21): the
            // offset is clamped to what the adopt restored plus what the
            // import put back. A carry that brought none (a successor policy
            // of `carry = "visible"`/`"repaint"` withholds every line) leaves
            // nothing the person was reading here, and the scrollback the
            // adopted program has written since is not it: the view stays at
            // the live bottom rather than climbing into post-update output. A
            // screen that did not cross at all restored no history either
            // (round eight, finding 38: the clamp was skipped there).
            let reachable = history_at_adopt.unwrap_or(0).saturating_add(imported);
            let viewport_from_bottom = viewport_from_bottom
                .map(|lines| u32::try_from(u64::from(lines).min(reachable)).unwrap_or(lines))
                .filter(|lines| *lines > 0);
            if let Some(lines) = viewport_from_bottom
                && !input_at_adopt.as_ref().is_some_and(InputWatch::moved)
            {
                let bottom = bottom_at_adopt.map(|bottom| bottom.saturating_add(renumbered));
                locked(&term, |t| {
                    restore_viewport(t, lines, bottom, gestures_at_adopt)
                });
            }
            ImportReport {
                session,
                imported,
                dropped,
                withheld,
                held,
                held_pane,
                failed_lines,
                links_dropped,
                failed,
                cleared,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "handoff_history_tests.rs"]
pub(crate) mod tests;

/// Tier-1 bind for `NativeUpdateHistoryCarry`: every path of the machine on
/// the real export, park, join, sidecar and import.
#[cfg(all(test, unix))]
#[path = "handoff_history_conformance.rs"]
mod conformance;
