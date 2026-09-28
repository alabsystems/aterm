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
//!   would otherwise outrun the join. The park STOPS it — one bounded last
//!   look — before freezing anything, so no chunk read ever contends with the
//!   capture's locks or the layout's. The fork lane, which parks at once,
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
//!   fails any check costs exactly its `take` lines, counted like the rest. A
//!   pane whose scrollback was CLEARED between the adopt and the import (an
//!   ED3 or a reset the adopted shell sent) is not imported into at all: the
//!   history stays cleared, and nothing is counted. The pane's own retention
//!   then decides what it keeps, with one wrinkle the carry does not count:
//!   right after an update the pane's fast tier (its ring) is nearly empty,
//!   and the unified limit reserves the ring's whole share, so a history AT
//!   its limit keeps up to one ring (1,000 lines) fewer than the outgoing
//!   process held until new output refills the ring.
//!
//! A SUCCESSOR'S HANDOFF POLICY that carries no scrollback (`carry =
//! "visible"` or `"repaint"`, `aterm_update_core::handoff_policy`) is followed
//! here too, not only by the screen carry: under such a ceiling nothing is
//! exported ([`HistoryPlan::Withheld`] — the launched lane does not start its
//! export when it already knows the policy, and its park stops one it started
//! before it did, removing every sidecar), no sidecar is named, and the join
//! COUNTS every line the park saw as left behind, said as the policy's
//! ([`Fallback::Withheld`]). A `full` ceiling is the carry above, unchanged.
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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// counted).
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
/// (`seamless::strict_grid_lines`).
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
    format!("{len} {} {take}", hex(sha))
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

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Stream one sidecar: `take` must not exceed what the header names, the
/// bytes must be exactly `len` long and hash to `sha`, and every frame read
/// must decode strictly. Returns the width the lines are wrapped at and the
/// FIRST `take` lines. Frames past `take` are hashed and never decoded.
fn read_sidecar(
    mut reader: impl std::io::Read,
    len: u64,
    sha: &[u8; 32],
    take: u64,
) -> Result<(u16, Vec<Line>), &'static str> {
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
            let decoded = aterm_core::scrollback::deserialize_lines_strict(
                &frame,
                CHUNK_LINES,
                usize::from(cols),
                content_cap,
                record_cap,
            )
            .ok_or("a frame that does not decode")?;
            let room = want - out.len();
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
    Ok((cols, out))
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
}

impl std::fmt::Debug for HistoryExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryExport")
            .field("local_id", &self.local_id)
            .field("facts", &self.facts)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

enum ExportEvent {
    Exported(HistoryExport),
    NotExported {
        local_id: u64,
        why: String,
    },
    /// Every session's first pass is done: the park need not wait for it.
    CaughtUp,
    Done,
}

/// What the exporter produced by the time it was asked for: the finished
/// exports, and why each session that has none has none.
#[derive(Default, Debug)]
pub(crate) struct ExportResults {
    pub(crate) exports: Vec<HistoryExport>,
    pub(crate) refused: Vec<(u64, String)>,
    /// The exporter had not finished when asked: the sessions it had not
    /// reached carry today's bounded history, counted.
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
    /// The worker, to wake from its follow tick when it is stopped.
    worker: std::thread::Thread,
    results: ExportResults,
    caught_up: bool,
    done: bool,
}

impl std::fmt::Debug for HistoryExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryExporter")
            .field("caught_up", &self.caught_up)
            .field("done", &self.done)
            .field("exported", &self.results.exports.len())
            .finish_non_exhaustive()
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
    /// successor booted and FOLLOWING the history until the park stopped it
    /// ([`HistoryExporter::halt`]) before freezing anything.
    Exported(HistoryExporter),
    /// The fork lane's sessions, exported by the worker once the capture is
    /// done — never beside the park, where its chunk reads would contend with
    /// the capture's and the layout's terminal locks.
    Deferred(Vec<(u64, Arc<Mutex<Terminal>>)>),
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
        sessions: impl FnOnce() -> Option<Vec<(u64, Arc<Mutex<Terminal>>)>>,
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
            // Stopped at the park already; this collects what it finished.
            Self::Exported(export) => export.finish(Duration::ZERO),
            Self::Deferred(sessions) => {
                match HistoryExporter::start(dir.to_path_buf(), sessions, false) {
                    Ok(export) => export.finish(FORK_EXPORT_WAIT),
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
    /// which exports under the park), it stops after the first pass.
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
        sessions: Vec<(u64, Arc<Mutex<Terminal>>)>,
        follow: bool,
    ) -> std::io::Result<Self> {
        let (tx, events) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
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
                run_export(&dir, &tag, &sessions, follow, &worker_cancel, &tx);
            })?
            .thread()
            .clone();
        Ok(Self {
            events,
            cancel,
            worker,
            results: ExportResults::default(),
            caught_up: false,
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
            ExportEvent::Done => self.done = true,
        }
    }

    /// Take every event until `deadline` or the worker's `Done`.
    fn drain_until(&mut self, deadline: std::time::Instant) {
        while !self.done {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => self.absorb(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => self.done = true,
            }
        }
    }

    /// Whether the worker's FIRST PASS is over for every session
    /// (non-blocking) — what the park gate waits for. A following export
    /// keeps running after it; a stopped one is over too.
    pub(crate) fn caught_up(&mut self) -> bool {
        while !self.done {
            match self.events.try_recv() {
                Ok(event) => self.absorb(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.done = true,
            }
        }
        self.caught_up || self.done
    }

    /// Wait up to `wait` for the first pass to finish ([`Self::caught_up`]).
    #[cfg(test)]
    pub(crate) fn await_caught_up(&mut self, wait: Duration) -> bool {
        let deadline = std::time::Instant::now() + wait;
        while !self.caught_up() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => self.absorb(event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return false,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => self.done = true,
            }
        }
        true
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

    /// Wait up to `wait` for the worker to finish (a following export never
    /// does by itself: it is stopped here), then stop it — allowing it
    /// [`HALT_WAIT`] to hand over what it finished — and hand that over. A
    /// session it had not finished is not in the result.
    pub(crate) fn finish(mut self, wait: Duration) -> ExportResults {
        self.drain_until(std::time::Instant::now() + wait);
        if !self.done {
            self.halt(HALT_WAIT);
        }
        let mut results = std::mem::take(&mut self.results);
        results.unfinished = !self.done;
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

fn run_export(
    dir: &Path,
    tag: &str,
    sessions: &[(u64, Arc<Mutex<Terminal>>)],
    follow: bool,
    cancel: &AtomicBool,
    events: &std::sync::mpsc::Sender<ExportEvent>,
) {
    // A receiver that went away drops the event, and with it the file.
    let send = |event| events.send(event).is_ok();
    let mut written = 0_u64;
    let mut open: Vec<OpenExport> = Vec::new();
    for (local_id, term) in sessions {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let path = dir.join(format!("{tag}.s{local_id}.hist"));
        let room = MAX_AGGREGATE_BYTES.saturating_sub(written);
        let sent = match begin_export(&path, *local_id, term, cancel, room) {
            Ok(Some(export)) => {
                written = written.saturating_add(export.len);
                if follow {
                    open.push(export);
                    true
                } else {
                    send(export.close())
                }
            }
            // An empty history crosses by itself: nothing to write.
            Ok(None) => true,
            Err(why) => send(ExportEvent::NotExported {
                local_id: *local_id,
                why,
            }),
        };
        if !sent {
            return;
        }
    }
    if !cancel.load(Ordering::Acquire) && !send(ExportEvent::CaughtUp) {
        return;
    }
    if follow {
        // FOLLOW until stopped: the readers are live, so the history keeps
        // growing, and the park joins whatever this has taken by then.
        while !cancel.load(Ordering::Acquire) {
            std::thread::park_timeout(FOLLOW_EVERY);
            for export in &mut open {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                export.follow(&mut written, FOLLOW_CHUNKS);
            }
        }
        // The last look, as the park stops it: what landed since the last
        // tick, bounded (the park waits for it on the main thread).
        for export in &mut open {
            export.follow(&mut written, LAST_LOOK_CHUNKS);
        }
        for export in open {
            if !send(export.close()) {
                return;
            }
        }
    }
    let _ = send(ExportEvent::Done);
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
}

impl OpenExport {
    /// Whether one more frame of `body` bytes fits this sidecar's and the
    /// handoff's byte budgets.
    fn fits(&self, body: usize, written: u64, room: u64) -> bool {
        let frame = 4 + body as u64;
        self.len.saturating_add(frame) <= MAX_SIDECAR_BYTES && written.saturating_add(frame) <= room
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
    /// does not, retention passed the cursor, or a bound was reached.
    fn follow(&mut self, written: &mut u64, chunks: usize) {
        for _ in 0..chunks {
            if !self.following || self.broken.is_some() {
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
            let lines = match read {
                Ok(lines) if lines.is_empty() => return,
                Ok(lines) => lines,
                Err(()) => {
                    self.following = false;
                    return;
                }
            };
            let body = aterm_core::scrollback::serialize_lines(&lines);
            if self.facts.lines + lines.len() as u64 > MAX_EXPORT_LINES
                || !self.fits(body.len(), *written, MAX_AGGREGATE_BYTES)
            {
                self.following = false;
                return;
            }
            match self.append(&lines, &body) {
                Ok(()) => *written = written.saturating_add(4 + body.len() as u64),
                Err(error) => {
                    self.broken = Some(format!("could not write ({})", error.kind()));
                    return;
                }
            }
        }
    }

    /// Close the sidecar: its event for the exporter.
    fn close(mut self) -> ExportEvent {
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
        })
    }
}

/// Export one session's fenced history to `path` — its FIRST PASS: every
/// line the fence covers, oldest first, in chunks under short locks.
fn begin_export(
    path: &Path,
    local_id: u64,
    term: &Arc<Mutex<Terminal>>,
    cancel: &AtomicBool,
    room: u64,
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
    let Some((fence, first, mut lines)) = first_read else {
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
    };
    let head = header(fence.cols);
    export
        .writer
        .write_all(&head)
        .map_err(|e| format!("could not write ({})", e.kind()))?;
    export.hasher.update(head);
    loop {
        if lines.is_empty() {
            return Err("the history ended before its fence".to_string());
        }
        let body = aterm_core::scrollback::serialize_lines(&lines);
        if !export.fits(body.len(), export.len, room) {
            return Err(format!(
                "the history is over the {}-byte sidecar budget",
                room.min(MAX_SIDECAR_BYTES)
            ));
        }
        export
            .append(&lines, &body)
            .map_err(|e| format!("could not write ({})", e.kind()))?;
        let cursor = export.facts.fence.end;
        if cursor >= fence.end {
            return Ok(Some(export));
        }
        if cancel.load(Ordering::Acquire) {
            return Err("the attempt stopped".to_string());
        }
        lines = locked(term, |t| {
            t.history_lines_since_fence(&fence, cursor, CHUNK_LINES)
        })
        .map_err(|broken| broken.to_string())?;
    }
}

/// One handed session's history as the park saw it — its fence, taken under
/// the lock the session's checkpoint is taken under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryHead {
    pub(crate) local_id: u64,
    pub(crate) fence: HistoryFence,
}

/// The park's share: a pure read of the fence.
#[must_use]
pub(crate) fn capture_head(local_id: u64, terminal: &Terminal) -> HistoryHead {
    HistoryHead {
        local_id,
        fence: terminal.history_fence(),
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
            let source = export.file.keep();
            match std::fs::hard_link(&source, &named) {
                Ok(()) => {
                    record.history = Some(stamp(export.len, &export.sha, joined.take));
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
        record.history_lost = record.history_lost.saturating_add(joined.dropped);
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
    fn read_verified(self) -> Result<(u16, Vec<Line>), &'static str> {
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
    pub(crate) lost: u64,
    pub(crate) claim: Option<OlderHistoryClaim>,
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
}

/// The successor's reading of one record's history carry, before the proof
/// (`seamless::take_incoming`): the record's counts, and its sidecar opened
/// at `path` when it names one. A sidecar that cannot be opened, or one whose
/// session's screen this build refused (`screen_degraded`: the carried lines
/// the sidecar's newest line meets went with that screen, so importing it
/// would put a hole in the history), is removed and its lines counted.
pub(crate) fn incoming(
    record: &crate::session_store::SessionRecord,
    screen_degraded: bool,
    path: &Path,
    dir: &Path,
    remaining: &mut u64,
) -> AdoptedHistory {
    let mut history = AdoptedHistory {
        carry: None,
        dropped: record.history_dropped,
        lost: record.history_lost,
        claim: None,
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
    /// The named carry's lines, when it failed HERE (not yet counted).
    pub(crate) failed_lines: u64,
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

    /// The report for a job whose import never ran (its worker could not
    /// start): everything it named is lost.
    #[must_use]
    pub(crate) fn not_run(job: &ImportJob, why: &str) -> Self {
        Self {
            session: job.session,
            imported: 0,
            dropped: job.history.dropped,
            failed_lines: job.history.reserve(),
            failed: job.history.carry.as_ref().map(|_| why.to_string()),
            cleared: false,
        }
    }
}

/// How one import settled when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Imported {
    /// Placed in front of the history; how many lines the pane's own
    /// retention kept.
    Retained(u64),
    /// The pane's scrollback was cleared since the adopt: not put back.
    Cleared,
}

/// Import one carry into `term` under `claim` — see the module doc.
/// Blocking; the import worker's. A pane whose scrollback was cleared since
/// the adopt is not imported into at all, before the sidecar is read and
/// again at the attach ([`Imported::Cleared`]).
pub(crate) fn import(
    term: &Mutex<Terminal>,
    carry: HistoryCarry,
    claim: OlderHistoryClaim,
) -> Result<Imported, String> {
    if !locked(term, |t| t.older_history_claim_holds(claim)) {
        return Ok(Imported::Cleared);
    }
    let (from_cols, lines) = carry
        .read_verified()
        .map_err(|why| format!("the sidecar arrived with {why}"))?;
    let deadline = std::time::Instant::now() + IMPORT_PATIENCE;
    loop {
        let cols = locked(term, |t| t.history_cols());
        let older = OlderHistory::build(&lines, from_cols, cols);
        match locked(term, |t| t.attach_older_history(older, claim)) {
            Ok(retained) => return Ok(Imported::Retained(retained as u64)),
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
    }
}

/// This update's loss across every adopted session: the lines left behind,
/// and in how many tabs.
#[must_use]
pub(crate) fn loss_summary(reports: &[ImportReport]) -> (u64, usize) {
    reports
        .iter()
        .filter(|report| report.lost() > 0)
        .fold((0, 0), |(lines, tabs), report| {
            (lines.saturating_add(report.lost()), tabs + 1)
        })
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
                claim,
                ..
            } = history;
            let (imported, failed_lines, failed, cleared) = match (carry, claim) {
                (None, _) => (0, 0, None, false),
                // The adopt reserves a claim for every carry it keeps; one
                // without is a carry nothing was restored in front of.
                (Some(carry), None) => (
                    0,
                    carry.take(),
                    Some("no screen was restored for it to meet".to_string()),
                    false,
                ),
                (Some(carry), Some(claim)) => {
                    let take = carry.take();
                    match import(&term, carry, claim) {
                        Ok(Imported::Retained(imported)) => (imported, 0, None, false),
                        Ok(Imported::Cleared) => (0, 0, None, true),
                        Err(why) => (0, take, Some(why), false),
                    }
                }
            };
            ImportReport {
                session,
                imported,
                dropped,
                failed_lines,
                failed,
                cleared,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "handoff_history_tests.rs"]
mod tests;

/// Tier-1 bind for `NativeUpdateHistoryCarry`: every path of the machine on
/// the real export, park, join, sidecar and import.
#[cfg(all(test, unix))]
#[path = "handoff_history_conformance.rs"]
mod conformance;
