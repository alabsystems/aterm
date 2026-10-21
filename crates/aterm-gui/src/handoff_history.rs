// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sidecar scrollback carry across an in-session self-update handoff.
//!
//! Screen checkpoints (`seamless::carry_for_wire`) are taken during the freeze and
//! cap carried history at `seamless::MAX_HANDOFF_HISTORY_LINES` (256 lines). This
//! module exports each session's older scrollback to a `0600` sidecar in the
//! per-user control directory and imports it after Commit:
//!
//! * **Export (`HistoryExporter`)**: walks each handed session's history (or saved
//!   primary under the alternate screen) oldest-first in [`CHUNK_LINES`] chunks
//!   under short terminal locks, guarded by [`HistoryFence`]. On the launched lane,
//!   the exporter starts alongside the launch, follows new history every
//!   [`FOLLOW_EVERY`], and pauses before the park freezes anything. On the fork
//!   lane ([`HistoryPlan::Deferred`]), the worker exports after the capture while
//!   readers are parked. Held panes (`--hold` or kept failed starts) export after
//!   handed sessions and yield budget to handed sessions' follow writes via
//!   `Aggregate::evict_for`.
//! * **Join ([`join`])**: compares the export's `[first, end)` against the park's
//!   [`HistoryHead`] and the checkpoint's `[head.end - carried, head.end)`. When
//!   the fence holds and the ranges meet, links the sidecar to its attempt-bound
//!   name (`seamless-<pid>-<nonce>.s<id>.hist` or `.h<id>.hist`) and stamps
//!   `"<len> <sha256hex> <take>"`. Any uncarried lines are counted on
//!   `history_dropped` / `history_lost`.
//! * **Import ([`incoming`], [`import`])**: the successor opens and unlinks each
//!   named sidecar before publishing its adoption proof, reserves row keys via
//!   `Terminal::reserve_older_history_keys`, and after Commit verifies the sidecar
//!   length and SHA-256, decodes frames strictly (stripping over-cap OSC 8 links),
//!   builds an [`OlderHistory`] off-lock, and attaches it in front of the session's
//!   oldest line unless the scrollback was cleared since adoption.
//! * **Policy withholding ([`HistoryPlan::Withheld`])**: when the successor's
//!   handoff policy specifies `carry = "visible"` or `"repaint"`, nothing is
//!   exported and all history lines are recorded as withheld (`Fallback::Withheld`).

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

const MAGIC: &[u8; 8] = b"ATHIST1\n";
/// 8-byte magic, `u16` little-endian wrap width, and 2 reserved zero bytes.
const HEADER_LEN: usize = 8 + 2 + 2;
/// Lines per frame and per terminal-lock hold while exporting.
pub(crate) const CHUNK_LINES: usize = 1024;
/// Maximum history lines exported per session.
pub(crate) const MAX_EXPORT_LINES: u64 = 1 << 20;
/// Maximum sidecar size per session.
pub(crate) const MAX_SIDECAR_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum aggregate sidecar bytes across all sessions in one handoff.
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum time the launched lane's park gate waits for the export's first pass.
#[cfg(any(target_os = "macos", test))]
pub(crate) const EXPORT_PATIENCE: Duration = Duration::from_secs(5);
/// Time budget for stopping or pausing an active export worker.
pub(crate) const HALT_WAIT: Duration = Duration::from_millis(100);

/// [`HALT_WAIT`] scaled to the test hang-detector budget under `cfg(test)`.
pub(crate) fn park_halt_wait() -> Duration {
    #[cfg(all(test, unix))]
    let wait = crate::app_update_handoff::park_budget_for_test::deadline_budget(HALT_WAIT);
    #[cfg(not(all(test, unix)))]
    let wait = HALT_WAIT;
    wait
}
/// Interval between follow ticks on the launched lane.
pub(crate) const FOLLOW_EVERY: Duration = Duration::from_millis(25);
/// Maximum chunks followed per session per tick.
const FOLLOW_CHUNKS: usize = 8;
/// Maximum chunks followed per session during the final pre-park look.
const LAST_LOOK_CHUNKS: usize = 2;
/// Time budget for the fork lane's post-capture export worker.
pub(crate) const FORK_EXPORT_WAIT: Duration = Duration::from_secs(2);
/// Share of [`FORK_EXPORT_WAIT`] allocated to held panes after handed sessions finish.
pub(crate) const FORK_HELD_WAIT: Duration = Duration::from_millis(500);
/// Maximum time the successor retries attaching older history while a pane rewraps.
const IMPORT_PATIENCE: Duration = Duration::from_secs(5);
const IMPORT_RETRY: Duration = Duration::from_millis(25);

fn header(cols: u16) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[..8].copy_from_slice(MAGIC);
    out[8..10].copy_from_slice(&cols.to_le_bytes());
    out
}

/// Strict per-line decode bounds for width `cols`: `(content_cap, record_cap)`.
fn line_caps(cols: u16) -> (usize, usize) {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    (content_cap, record_cap)
}

/// Format the manifest `history` stamp: `"<len> <sha256hex> <take>"`.
#[must_use]
pub(crate) fn stamp(len: u64, sha: &[u8; 32], take: u64) -> String {
    format!("{len} {} {take}", aterm_codec::hex::encode_lower(sha))
}

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

/// Clear hyperlinks from any line in `lines` whose serialized record exceeds the
/// per-line record cap at `cols` ([`line_caps`]), returning the count of stripped lines.
pub(crate) fn strip_over_cap_links(lines: &mut [Line], cols: u16) -> u64 {
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

#[derive(Debug)]
struct SidecarLines {
    cols: u16,
    lines: Vec<Line>,
    links_dropped: u64,
}

/// Verify and decode `take` lines (after `skip`) from a sidecar or crash-restore
/// frame stream of exact byte length `len` and optional SHA-256 `sha`.
///
/// Frames beyond `skip + take` are hashed without decoding. Lines exceeding the
/// record cap solely due to OSC 8 hyperlinks are retained with their links cleared.
fn read_sidecar(
    mut reader: impl std::io::Read,
    len: u64,
    sha: Option<&[u8; 32]>,
    skip: u64,
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
    // Lines still to drop before the first one kept.
    let mut skipping = skip;
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
            let drop = usize::try_from(skipping)
                .unwrap_or(usize::MAX)
                .min(decoded.len());
            skipping -= drop as u64;
            let room = want - out.len();
            // Only the lines kept count: one past `take` is the checkpoint's
            // to carry, and the screen carry says its own; one skipped is not
            // carried at all.
            links_dropped += stripped
                .iter()
                .filter(|&&at| at >= drop && at - drop < room)
                .count() as u64;
            out.extend(decoded.into_iter().skip(drop).take(room));
        }
    }
    // Nothing may follow the stamped length.
    let mut tail = [0u8; 1];
    if read != len || reader.read(&mut tail).map_err(|_| "unreadable")? != 0 {
        return Err("a length other than its stamp");
    }
    if sha.is_some_and(|sha| hasher.finalize() != *sha) {
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
    /// Every handed session's first pass is complete.
    CaughtUp,
    /// Every first pass is complete, including held panes.
    Passed,
    /// The worker finished its last look for pause generation `u64` and holds no
    /// terminal lock until resumed or stopped.
    Paused(u64),
    Done,
}

#[derive(Default, Debug)]
pub(crate) struct ExportResults {
    pub(crate) exports: Vec<HistoryExport>,
    pub(crate) refused: Vec<(u64, String)>,
    /// True when not all handed sessions finished their first pass before stopping.
    pub(crate) unfinished: bool,
    /// True when nothing was exported because the successor's policy withholds scrollback.
    pub(crate) withheld: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseAnswer {
    Paused,
    Asked,
    Unanswered,
}

/// Outgoing scrollback export worker; dropping cancels the worker and removes unclaimed sidecars.
pub(crate) struct HistoryExporter {
    events: std::sync::mpsc::Receiver<ExportEvent>,
    cancel: Arc<AtomicBool>,
    #[cfg(any(target_os = "macos", test))]
    pause: Arc<AtomicU64>,
    pause_generation: u64,
    #[cfg(any(target_os = "macos", test))]
    pause_asked_at: Option<std::time::Instant>,
    paused: u64,
    worker: std::thread::Thread,
    #[cfg(test)]
    exited: Arc<AtomicBool>,
    results: ExportResults,
    follow: bool,
    caught_up: bool,
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

/// Handed sessions and held panes to export.
///
/// Handed sessions run first; held panes use remaining aggregate budget and can be
/// evicted by handed sessions' follow writes via [`Aggregate::evict_for`].
#[derive(Default)]
pub(crate) struct ExportSessions {
    pub(crate) handed: Vec<ExportSession>,
    pub(crate) held: Vec<ExportSession>,
    #[cfg(test)]
    pub(crate) stalled: Option<Arc<AtomicU64>>,
    #[cfg(test)]
    pub(crate) followed: Arc<AtomicU64>,
}

pub(crate) type ExportSession = (u64, Arc<Mutex<Terminal>>);

impl ExportSessions {
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.handed.len() + self.held.len()
    }

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

/// Per-lane scrollback export plan.
pub(crate) enum HistoryPlan {
    /// No sidecars exported (target lacks an importer or no private control dir exists).
    Unexported,
    /// Launched-lane background export that ran while the successor booted.
    #[cfg(any(target_os = "macos", test))]
    Exported(HistoryExporter),
    /// Fork-lane export run on the worker after the screen capture completes.
    Deferred(ExportSessions),
    /// Scrollback withheld by the successor's signed handoff policy (`carry = "visible"` or `"repaint"`).
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
    /// Build the fork lane's plan under `ceiling`.
    pub(crate) fn deferred_under(
        ceiling: CarryCeiling,
        sessions: impl FnOnce() -> Option<ExportSessions>,
    ) -> Self {
        if !ceiling.carries_scrollback() {
            return Self::Withheld;
        }
        sessions().map_or(Self::Unexported, Self::Deferred)
    }

    /// Build the launched lane's plan under `ceiling`, dropping `export` if scrollback is withheld.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn exported_under(ceiling: CarryCeiling, export: Option<HistoryExporter>) -> Self {
        if !ceiling.carries_scrollback() {
            drop(export);
            return Self::Withheld;
        }
        export.map_or(Self::Unexported, Self::Exported)
    }

    pub(crate) fn results(self, dir: &Path) -> ExportResults {
        self.results_within(dir, FORK_EXPORT_WAIT, FORK_HELD_WAIT)
    }

    #[cfg(test)]
    pub(crate) fn results_for_test(self, dir: &Path) -> ExportResults {
        const HANG: Duration = Duration::from_secs(60);
        self.results_within(dir, HANG, HANG)
    }

    fn results_within(self, dir: &Path, wait: Duration, held_wait: Duration) -> ExportResults {
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
                    Ok(export) => export.finish_deferred(wait, held_wait),
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

/// Whether `target_build` supports importing handed-session scrollback sidecars.
#[must_use]
pub(crate) fn exports_for_target(running_build: u64, target_build: u64) -> bool {
    target_build >= running_build || target_build >= IMPORTS_HISTORY_FROM_BUILD
}

/// Whether `target_build` supports importing held-pane scrollback sidecars.
#[must_use]
pub(crate) fn exports_held_for_target(running_build: u64, target_build: u64) -> bool {
    target_build >= running_build || target_build >= IMPORTS_HELD_HISTORY_FROM_BUILD
}

/// First build that imports handed-session sidecars (v0.94.0 in `RELEASES.ledger`).
const IMPORTS_HISTORY_FROM_BUILD: u64 = 1_790_526_323;

/// First build that imports held-pane sidecars (v0.101.0 in `RELEASES.ledger`).
const IMPORTS_HELD_HISTORY_FROM_BUILD: u64 = 1_791_501_389;

impl HistoryExporter {
    /// Start exporting `sessions` into `dir`, on a worker of its own. With
    /// `follow` (the launched lane, whose readers stay live until the park),
    /// the worker keeps each session's sidecar current after its first pass,
    /// every [`FOLLOW_EVERY`], until it is stopped; without (the fork lane,
    /// which exports under the park), it stops after the first pass. The
    /// sidecars together stay within [`MAX_AGGREGATE_BYTES`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        let worker = crate::qos::spawn_lane_thread(
            "aterm-history-export".to_string(),
            crate::qos::Role::Responsive,
            move || {
                // It takes each session's terminal lock, which the UI thread
                // contends: Responsive is the floor for such a holder (`qos`).
                let stop = Stop {
                    cancel: &worker_cancel,
                    pause: &worker_pause,
                };
                run_export(&dir, &tag, &sessions, follow, cap, stop, &tx);
                #[cfg(test)]
                worker_exited.store(true, Ordering::Release);
            },
        )?
        .thread()
        .clone();
        Ok(Self {
            events,
            cancel,
            #[cfg(any(target_os = "macos", test))]
            pause,
            pause_generation: 0,
            #[cfg(any(target_os = "macos", test))]
            pause_asked_at: None,
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

    /// Whether the worker's first pass is complete for every handed session (non-blocking).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn caught_up(&mut self) -> bool {
        self.poll();
        self.caught_up || self.done
    }

    /// Whether every first pass is complete, including held panes (non-blocking).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn passed(&mut self) -> bool {
        self.poll();
        self.passed || self.done
    }

    #[cfg(test)]
    pub(crate) fn await_caught_up(&mut self, wait: Duration) -> bool {
        self.drain_while(std::time::Instant::now() + wait, |export| !export.caught_up);
        self.caught_up || self.done
    }

    #[cfg(test)]
    pub(crate) fn exited(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.exited)
    }

    #[cfg(test)]
    pub(crate) fn await_passed(&mut self, wait: Duration) -> bool {
        self.drain_while(std::time::Instant::now() + wait, |export| !export.passed);
        self.passed || self.done
    }

    /// Cancel the worker and wait up to `wait` for it to finish its last look and stop.
    pub(crate) fn halt(&mut self, wait: Duration) -> bool {
        self.cancel.store(true, Ordering::Release);
        self.worker.unpark();
        self.drain_until(std::time::Instant::now() + wait);
        self.done
    }

    /// Request a non-blocking pause on the launched lane before freezing sessions,
    /// returning [`PauseAnswer`].
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn ask_pause(&mut self, wait: Duration) -> PauseAnswer {
        let asked_at = if let Some(asked_at) = self.pause_asked_at {
            asked_at
        } else {
            self.pause_generation = self.pause_generation.wrapping_add(1).max(1);
            self.pause.store(self.pause_generation, Ordering::Release);
            self.worker.unpark();
            *self.pause_asked_at.insert(std::time::Instant::now())
        };
        self.poll();
        if self.done || self.paused == self.pause_generation {
            PauseAnswer::Paused
        } else if asked_at.elapsed() >= wait {
            PauseAnswer::Unanswered
        } else {
            PauseAnswer::Asked
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn pause_asked(&self) -> bool {
        self.pause_asked_at.is_some()
    }

    #[cfg(test)]
    pub(crate) fn pause(&mut self, wait: Duration) -> bool {
        let deadline = std::time::Instant::now() + wait;
        self.ask_pause(wait);
        self.drain_while(deadline, |export| export.paused != export.pause_generation);
        self.done || self.paused == self.pause_generation
    }

    /// Resume a paused export worker when a park attempt misses.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn resume(&mut self) {
        self.pause_asked_at = None;
        if self.pause.swap(0, Ordering::AcqRel) != 0 {
            self.worker.unpark();
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn finish(self, wait: Duration) -> ExportResults {
        self.finish_by(std::time::Instant::now() + wait)
    }

    /// Wait up to `wait` for handed sessions and at most `held_wait` more for held panes.
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
            self.halt(park_halt_wait());
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
/// lock's data is still the data. It is a [`crate::TermGuard`] hold, so an
/// import's attach, which moves the screen generation, rings the session's
/// screen readers as the hold ends.
fn locked<T>(term: &Mutex<Terminal>, read: impl FnOnce(&mut Terminal) -> T) -> T {
    read(&mut crate::term_lock(term))
}

/// What the export worker is asked to stop for: `cancel` ends it, a non-zero
/// `pause` generation parks it after one last look until it is resumed
/// ([`HistoryExporter::ask_pause`]).
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

const EVICTED: &str =
    "its room in the handoff's scrollback budget went to a live tab's newer lines";

const STOPPED_BEFORE_ITS_PASS: &str =
    "the export stopped before it had read the pane's whole scrollback";

/// Aggregate sidecar byte tracker across a handoff (`cap` = [`MAX_AGGREGATE_BYTES`]).
struct Aggregate {
    cap: u64,
    written: u64,
    evicted: Vec<u64>,
}

impl Aggregate {
    fn room(&self) -> u64 {
        self.cap.saturating_sub(self.written)
    }

    fn fits(&self, frame: u64) -> bool {
        self.written.saturating_add(frame) <= self.cap
    }

    fn release(&mut self, export: &OpenExport) {
        self.written = self.written.saturating_sub(export.len);
    }

    /// Evict the largest held-pane exports from `held` when doing so makes `frame` fit
    /// for a handed session's follow write.
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

struct Gone;

struct Worker<'a> {
    dir: &'a Path,
    tag: &'a str,
    sessions: &'a ExportSessions,
    follow: bool,
    stop: Stop<'a>,
    events: &'a std::sync::mpsc::Sender<ExportEvent>,
    aggregate: Aggregate,
    open: Vec<OpenExport>,
    open_held: Vec<OpenExport>,
    next: usize,
    next_held: usize,
}

impl Worker<'_> {
    fn send(&self, event: ExportEvent) -> Result<(), Gone> {
        self.events.send(event).map_err(|_| Gone)
    }

    fn path(&self, local_id: u64) -> PathBuf {
        self.dir.join(format!("{}.s{local_id}.hist", self.tag))
    }

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
                if caught_up {
                    while !self.stop.requested() && self.held_step()? {}
                }
                break;
            }
            // Interleave single-chunk held-pane passes between handed-session follow ticks.
            let mut due = std::time::Instant::now() + FOLLOW_EVERY;
            while !self.stop.requested() {
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
            self.follow_tick(LAST_LOOK_CHUNKS, false)?;
            if self.stop.cancelled() {
                break;
            }
            for export in self.open.iter_mut().chain(&mut self.open_held) {
                export.flush();
            }
            let generation = self.stop.paused();
            if generation == 0 {
                continue;
            }
            self.send(ExportEvent::Paused(generation))?;
            while !self.stop.cancelled() && self.stop.paused() == generation {
                std::thread::park_timeout(FOLLOW_EVERY);
            }
            if self.stop.cancelled() {
                break;
            }
        }
        self.close()
    }

    /// Run first passes for all remaining handed sessions; if paused mid-pass, drop
    /// the partial file so it restarts cleanly on resume.
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

    /// Advance the held panes' first passes by one chunk, returning whether work remains.
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

fn open_export(
    path: &Path,
    local_id: u64,
    term: &Arc<Mutex<Terminal>>,
    aggregate: &mut Aggregate,
) -> Result<Option<OpenExport>, String> {
    // Read the fence and the first chunk under a single lock so concurrent
    // retention eviction cannot invalidate `fence.oldest` before the first read.
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

/// Snapshot of a session's history fence and scroll offset taken during the park.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryHead {
    pub(crate) local_id: u64,
    pub(crate) fence: HistoryFence,
    /// Lines scrolled above the live bottom (`None` at the bottom or on the alternate screen).
    pub(crate) viewport_from_bottom: Option<u32>,
}

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

/// Restore the pre-handoff scroll offset `lines` above `bottom_at_adopt` after import,
/// unless the user has interacted (`gestures_at_adopt`), scrolled, or entered the
/// alternate screen since adoption.
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

/// Tracks user/controller input sent to an adopted session via [`OutputEchoTracker::input_sent`],
/// ignoring terminal focus/DA/DSR replies and avoiding holding the PTY master descriptor.
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
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
        record.history_withheld = joined.fallback == Some(Fallback::Withheld);
        record.history_lost = record.history_lost.saturating_add(joined.dropped);
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
    /// Partition held-pane exports out of `self` before [`stamp_manifest`] runs,
    /// recording why any unexported held pane has no sidecar.
    pub(crate) fn take_held(&mut self, held: &[u64], older_successor: bool) -> HeldExports {
        let (exports, kept) = std::mem::take(&mut self.exports)
            .into_iter()
            .partition(|export| held.contains(&export.local_id));
        self.exports = kept;
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

/// Reason a held pane's scrollback lines did not cross the handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldLoss {
    /// Target build predates held-pane scrollback import ([`exports_held_for_target`]).
    ScreenOnly,
    /// Successor's signed handoff policy withholds scrollback ([`HistoryPlan::Withheld`]).
    Withheld,
    /// Export or sidecar carry failed.
    Failed,
}

impl HeldLoss {
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

#[derive(Debug)]
pub(crate) struct HeldExports {
    pub(crate) exports: Vec<HistoryExport>,
    pub(crate) unexported: HeldLoss,
}

impl Default for HeldExports {
    fn default() -> Self {
        Self {
            exports: Vec::new(),
            unexported: HeldLoss::Failed,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeldHistory {
    pub(crate) stamp: Option<String>,
    pub(crate) dropped: u64,
    pub(crate) why: HeldLoss,
    pub(crate) path: Option<PathBuf>,
}

/// Join a held pane's exported history with `head` and hard-link its sidecar to `named`.
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

/// One session's carried history, read only after Commit (an update) or after the
/// recovery's restore drain (a crash restore): where its lines come from, and how many.
pub(crate) struct HistoryCarry {
    source: CarrySource,
    take: u64,
}

/// Source of a [`HistoryCarry`]'s lines, decoded by [`read_sidecar`].
enum CarrySource {
    /// Update sidecar opened and unlinked before publishing the adoption proof.
    Sidecar {
        file: std::fs::File,
        len: u64,
        sha: [u8; 32],
    },
    /// Crash-restore history ring chunks (`crash_restore`), decompressed on demand via
    /// [`JournalFrames`] after `skip` leading lines.
    #[cfg(unix)]
    Journal {
        chunks: Vec<aterm_restore::HistoryChunk>,
        cols: u16,
        skip: u64,
    },
}

impl std::fmt::Debug for HistoryCarry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = formatter.debug_struct("HistoryCarry");
        match &self.source {
            CarrySource::Sidecar { len, .. } => f.field("len", len),
            #[cfg(unix)]
            CarrySource::Journal { chunks, skip, .. } => {
                f.field("chunks", &chunks.len()).field("skip", skip)
            }
        };
        f.field("take", &self.take).finish_non_exhaustive()
    }
}

impl HistoryCarry {
    /// A crash restore's carry: `take` lines of the history ring's `chunks` (wrapped at
    /// `cols`) after the first `skip` ([`CarrySource::Journal`]).
    #[cfg(unix)]
    pub(crate) fn journal(
        chunks: Vec<aterm_restore::HistoryChunk>,
        cols: u16,
        skip: u64,
        take: u64,
    ) -> Self {
        Self {
            source: CarrySource::Journal { chunks, cols, skip },
            take,
        }
    }

    /// How many lines the carry names — what is lost if it fails.
    #[must_use]
    pub(crate) fn take(&self) -> u64 {
        self.take
    }

    /// Read, check and decode the carry — see [`read_sidecar`].
    fn read_verified(self) -> Result<SidecarLines, &'static str> {
        match self.source {
            CarrySource::Sidecar { file, len, sha } => {
                read_sidecar(std::io::BufReader::new(file), len, Some(&sha), 0, self.take)
            }
            #[cfg(unix)]
            CarrySource::Journal { chunks, cols, skip } => {
                let frames = JournalFrames::new(chunks, cols)?;
                let len = frames.len;
                read_sidecar(frames, len, None, skip, self.take)
            }
        }
    }
}

/// A crash restore's history ring chunks as one sidecar's bytes, for [`read_sidecar`]:
/// the sidecar header at the ring's width, then each chunk's frame decompressed in turn,
/// one at a time (only the frame being read is held decompressed). The length is known
/// before anything is decompressed: the header's, plus what each chunk's LZ4 prefix says
/// within the bound its line count and width allow
/// (`aterm_restore::HistoryChunk::frame_size`). A frame that does not decompress to
/// exactly that is a read error, which the decoder reports as a truncated frame.
#[cfg(unix)]
struct JournalFrames {
    chunks: std::collections::VecDeque<aterm_restore::HistoryChunk>,
    /// What is being read: the header, then the current frame.
    current: Vec<u8>,
    at: usize,
    len: u64,
}

#[cfg(unix)]
impl JournalFrames {
    fn new(chunks: Vec<aterm_restore::HistoryChunk>, cols: u16) -> Result<Self, &'static str> {
        let mut len = HEADER_LEN as u64;
        for chunk in &chunks {
            if chunk.identity.cols != cols {
                return Err("a frame wrapped at another width");
            }
            let size = chunk
                .frame_size()
                .map_err(|_| "a frame that does not decode")?;
            len = len.saturating_add(size as u64);
        }
        Ok(Self {
            chunks: chunks.into(),
            current: header(cols).to_vec(),
            at: 0,
            len,
        })
    }
}

#[cfg(unix)]
impl std::io::Read for JournalFrames {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at == self.current.len() {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let frame = chunk
                .frame()
                .map_err(|_| std::io::Error::other("a history chunk that does not decompress"))?;
            self.current = frame;
            self.at = 0;
        }
        let n = out.len().min(self.current.len() - self.at);
        out[..n].copy_from_slice(&self.current[self.at..self.at + n]);
        self.at += n;
        Ok(n)
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
            source: CarrySource::Sidecar { file, len, sha },
            take,
        })
    })();
    let _ = std::fs::remove_file(path);
    result
}

/// Adopted session's scrollback carry state for post-Commit import.
#[derive(Debug, Default)]
pub(crate) struct AdoptedHistory {
    pub(crate) carry: Option<HistoryCarry>,
    pub(crate) dropped: u64,
    /// Lines withheld by the successor's handoff policy (`SessionRecord::history_withheld`).
    pub(crate) withheld: u64,
    /// Lines left behind because the target build carries only a held pane's visible screen.
    pub(crate) held: u64,
    pub(crate) held_pane: bool,
    pub(crate) lost: u64,
    pub(crate) claim: Option<OlderHistoryClaim>,
    pub(crate) viewport_from_bottom: Option<u32>,
    pub(crate) bottom_at_adopt: Option<u64>,
    pub(crate) input_at_adopt: Option<InputWatch>,
    pub(crate) gestures_at_adopt: Option<u64>,
    pub(crate) history_at_adopt: Option<u64>,
}

impl AdoptedHistory {
    #[must_use]
    pub(crate) fn reserve(&self) -> u64 {
        self.carry.as_ref().map_or(0, HistoryCarry::take)
    }

    /// Reserve absolute-row keys in `engine` for the pending import.
    pub(crate) fn reserve_in(&mut self, engine: &mut Terminal) {
        if self.carry.is_some() {
            self.claim = Some(engine.reserve_older_history_keys(self.reserve()));
        }
    }

    /// Snapshot `engine`'s row counter, gesture count, and scrollback depth at adoption.
    pub(crate) fn anchor_in(&mut self, engine: &Terminal) {
        self.bottom_at_adopt = Some(engine.main_grid().absolute_row_counter());
        self.gestures_at_adopt = Some(engine.reader_gesture_count());
        self.history_at_adopt =
            Some(u64::try_from(engine.main_grid().scrollback_lines()).unwrap_or(u64::MAX));
    }

    /// Snapshot `input`'s sent-input counter at adoption so later input suppresses viewport restore.
    pub(crate) fn watch_input(&mut self, input: &Arc<crate::app_input::OutputEchoTracker>) {
        self.input_at_adopt = Some(InputWatch {
            input: Arc::clone(input),
            at: input.input_sent(),
        });
    }
}

/// Open and unlink `record`'s scrollback sidecar before publishing the adoption proof.
///
/// If `screen_degraded` is true or the sidecar cannot be taken, removes `path` and
/// adds the named lines to `dropped` and `lost`.
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

/// Open and unlink a held pane's scrollback sidecar before publishing the adoption proof.
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

/// Import `carry` into `term` under `claim`.
///
/// Clears hyperlinks on held panes (whose carry is outside the adoption proof)
/// and returns [`Imported::Cleared`] if the pane's scrollback was cleared since adoption.
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

/// Add any failed imports to `store`'s `history_lost` counters and log each outcome.
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

/// Aggregated scrollback loss across adopted sessions for the update band.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LossSummary {
    pub(crate) lines: u64,
    pub(crate) tabs: usize,
    pub(crate) withheld: u64,
    pub(crate) held: u64,
    pub(crate) held_failed: u64,
}

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

#[must_use]
const fn held_failed(held_pane: bool, lost: u64, withheld: u64, held: u64) -> u64 {
    if held_pane {
        lost.saturating_sub(withheld).saturating_sub(held)
    } else {
        0
    }
}

/// Pre-import loss summary known at Commit from each session's `dropped` count.
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

pub(crate) struct ImportJob {
    pub(crate) session: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) history: AdoptedHistory,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
            let mut renumbered = 0;
            let (imported, links_dropped, failed_lines, failed, cleared) = match (carry, claim) {
                (None, _) => (0, 0, 0, None, false),
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
            // Clamp restored scroll offset to history that crossed (`history_at_adopt + imported`)
            // so post-adoption output does not pull the viewport off the live bottom when
            // no history crossed.
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

/// `NativeUpdateHistoryCarry` actions no shipping function performs, each waived where
/// its Tier-1 (`conformance::the_real_history_carry_is_the_derived_machine`) binds it.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Output",
    reason = "The environment: program output reaching the outgoing grid between the export \
              and the park. Tier-1 drives it as real `Terminal::process` bytes; the carry's \
              own seams (`join`, the export, the import) are what it is checked against."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Rewrap",
    reason = "The environment: a width reflow (or a clear or reset) moving the history under \
              the export's fence. Tier-1 drives a real `Terminal::resize`; the join's fence \
              check is what refuses it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Corrupt",
    reason = "The environment: a sidecar that reaches the successor with other bytes than its \
              stamp. Tier-1 flips a byte of the real named file; the import's length and sha \
              check is what refuses it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Clear",
    reason = "The environment: the adopted shell clearing its scrollback (ED3, a reset) after \
              Commit, before the import lands. Tier-1 feeds a real `ESC [3J` to the adopted \
              engine; the import's claim check is what refuses to put the history back."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Settled",
    reason = "Stutter: the settled session stays settled (the machine's deadlock guard); no \
              shipping code runs for it."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn history_carry_waivers() {}
