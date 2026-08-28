// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The live install-progress protocol: `progress.json` v1 + the `bump` file.
//!
//! **This module holds the TYPES, constants and format contract only.** The writer (a
//! `ProgressSink` threaded through `install_default_set` / `cmd_seed` /
//! `apply_group_txn`, enabled by `--progress-file`) and the readers (the GUI tailer and
//! `atpkg __pending`) are built on top of it — the schema lives here so every party
//! serializes and deserializes the same shape, pinned by the tests below.
//!
//! # One source of truth, three readers
//!
//! `<prefix>/progress.json` is the LIVE complement to `status.toml` (the per-pass
//! durable outcome record): a snapshot of the pass in flight, written only by the
//! process holding the store flock, read by the GUI progress card, by pending-program
//! stubs, and by anyone tailing it. Writes use the `status.rs` atomic temp+rename
//! discipline, on change only and at most every [`WRITE_MIN_INTERVAL_MS`] ms (≤10 Hz).
//!
//! # Crash truthfulness
//!
//! A dead installer's file must never claim live progress. `heartbeat_unix` refreshes on
//! every write and at minimum every 2 s during long transfers; readers treat *heartbeat
//! older than [`HEARTBEAT_STALE_SECS`] OR dead pid* as "installer not running" and render
//! only not-running states. Pid reuse is covered by the heartbeat window; backward clock
//! skew fails to the safe "not running" state. On clean pass end the writer rewrites a
//! terminal snapshot with `pid` cleared; a pass start truncates and rewrites.
//!
//! # Readers treat BOTH files as untrusted input
//!
//! The files live in the user-owned prefix, so this is defense-in-depth, not a trust
//! boundary — but every reader (and the writer path) holds to it:
//!
//! * open with a `symlink_metadata` regular-file check BEFORE any read or write — never
//!   follow a symlink out of the prefix (the same discipline gc's staging sweep states in
//!   code: "Regular files only: never follow a symlink out of the prefix");
//! * cap reads at [`PROGRESS_READ_CAP`] / [`BUMP_READ_CAP`]; oversize ⇒ treated as
//!   absent, never partially parsed;
//! * ignore unknown fields (serde's default here — nothing derives
//!   `deny_unknown_fields`), so a newer writer never breaks an older reader;
//! * an unknown `v` renders a generic "installing…", never a guess at fields whose
//!   meaning may have changed;
//! * strings that reach a TTY are hostile until proven otherwise: program names must
//!   round-trip through `store::ToolName` + roster intersection before printing, and
//!   error strings are control-character-stripped and length-capped (terminal
//!   escape-sequence injection);
//! * malformed/oversized files are ignored identically — render nothing or the generic
//!   line; never crash, never trust.
//!
//! # The `bump` file: reorder-only by construction
//!
//! `<prefix>/bump` is the priority-queue channel: **plain text, one program name per
//! line, append-only by writers** (stubs append the name of the program the user just
//! ran), consumed by the installer between items. The installer reads it capped at
//! [`BUMP_READ_CAP`] (same symlink discipline), parses each line through the
//! `store::ToolName` gate, and **intersects with the set it already planned** from the
//! signed index. Unknown names, installed names, removed names, garbage and oversized
//! files are ignored. The file can therefore never ADD work, name a version, supply a
//! URL, or touch trust — it can only permute the order of installs the signed index
//! already authorized. It is deleted at clean pass end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The schema version this module defines. Readers render a generic "installing…" for
/// any `v` they do not know; writers always stamp this value.
pub const PROGRESS_VERSION: u32 = 1;

/// Read cap for `progress.json`: a file larger than this is treated as ABSENT (never
/// partially parsed). Generously above any honest snapshot — the roster is a couple
/// dozen programs — while bounding what a hostile or corrupt file can make a reader
/// buffer. The 4 KiB bump-cap's twin.
pub const PROGRESS_READ_CAP: u64 = 256 * 1024;

/// Read cap for the `bump` file: one program name per line; a file past this is treated
/// as absent. A user hammering stubs cannot push an honest file anywhere near it.
pub const BUMP_READ_CAP: u64 = 4 * 1024;

/// A heartbeat older than this many seconds means "installer not running" — readers must
/// then render only not-running states, regardless of what the snapshot claims.
pub const HEARTBEAT_STALE_SECS: u64 = 10;

/// Writer rate cap: write only on change AND at least this many milliseconds since the
/// last write (≤10 Hz), so a fast extract loop never turns the progress file into an
/// fsync storm.
pub const WRITE_MIN_INTERVAL_MS: u64 = 100;

/// Where one program is in its install, in pass order. Serialized lowercase — the wire
/// strings are `queued | download | verify | extract | link | done | failed | skipped`.
///
/// Honesty note: `download` and `extract` are the two phases with real byte meters (the
/// `.part` poller and the `write_capped` loop); `verify` and `link` are label-only —
/// they are not byte streams atpkg can meter, and the schema does not pretend otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Planned, not yet started. The `ProgressFile::queue` array carries the order.
    Queued,
    /// Bytes moving from the network into the staged `.part`.
    Download,
    /// Full-asset sha256 against the signed manifest (label-only; no byte meter).
    Verify,
    /// The scratch-sibling extract, metered from the in-process write loop.
    Extract,
    /// Activation: shims + `current` links (label-only; effectively instant).
    Link,
    /// Installed and live in this pass.
    Done,
    /// This program failed; `ProgramProgress::error` names why. Per-program isolation:
    /// one `Failed` never aborts the set.
    Failed,
    /// Nothing to do — already current (zero bytes moved) or excluded.
    Skipped,
}

/// The whole-pass rollup the overall bar renders. Byte totals are the SIGNED
/// `artifact.size` sums for the pass's planned work, so the denominator is honest and
/// fixed; already-current programs contribute zero.
/// Container-level `serde(default)`: a snapshot carrying only SOME of these counters
/// (an older writer, a mid-change write) degrades the missing ones to zero — never a
/// parse failure that blinds the reader to the whole file (the module's degrade-to-
/// safe rule, which the `__pending` tests exercise with a counters-only `overall`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overall {
    /// Programs finished (`done`/`failed`/`skipped`) so far.
    pub programs_done: u32,
    /// Programs this pass planned.
    pub programs_total: u32,
    /// Bytes landed so far, summed across programs.
    pub bytes_done: u64,
    /// Signed total bytes for the pass.
    pub bytes_total: u64,
}

/// One program's row. Every field except `phase` is `#[serde(default)]` so a reader
/// degrades to zeros/absent — never a parse failure — on a snapshot written mid-change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramProgress {
    /// Where this program is now.
    pub phase: Phase,
    /// Bytes landed in the CURRENT metered phase (download: `.part` size on disk;
    /// extract: bytes written). Zero in label-only phases.
    #[serde(default)]
    pub bytes_done: u64,
    /// The metered phase's honest denominator — the signed `artifact.size` for
    /// download, the signed uncompressed size for extract. Zero when unmetered.
    #[serde(default)]
    pub bytes_total: u64,
    /// The pinned build number being installed, once resolved from the signed index.
    #[serde(default)]
    pub build: Option<u64>,
    /// Whether a bump-file entry moved this program forward in the queue — the GUI
    /// renders it as "bumped — you asked for this".
    #[serde(default)]
    pub bumped: bool,
    /// When the bump pulled this program forward NOT because a user asked for it but
    /// because a program they DID ask for `requires` it (§17.10): that program's name.
    /// The GUI renders it as "bumped with <name>". `None` for a plain bump and for every
    /// other row; absent from the file when `None`, so an older reader sees the row it
    /// always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bumped_with: Option<String>,
    /// Why `phase` is `failed`, when it is. UNTRUSTED for display: control-strip and
    /// length-cap before any TTY (see the module docs).
    #[serde(default)]
    pub error: Option<String>,
}

/// The `progress.json` snapshot: everything a reader needs to render the pass, in one
/// atomically-replaced file.
///
/// `v` is deliberately the only REQUIRED field: a reader that cannot even establish the
/// version treats the file as absent, and every other field is `#[serde(default)]` so
/// missing data degrades toward the SAFE reading (no pid + zero heartbeat ⇒ "installer
/// not running"), never toward a parse failure or a live-looking lie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressFile {
    /// Schema version ([`PROGRESS_VERSION`]). Readers render a generic "installing…"
    /// for versions they do not know.
    pub v: u32,
    /// The writing installer's pid while the pass is live; `None` (serialized `null`)
    /// once the pass ended cleanly. A dead pid means "not running" no matter how fresh
    /// the file looks.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Which lane is writing: `"net"` (the network default-set pass) or `"seed"` (the
    /// sealed-seed pass, which has no download rows). A string, not an enum, so an old
    /// reader renders an unknown future lane generically instead of failing the parse.
    #[serde(default)]
    pub pass: String,
    /// Unix seconds when the pass started.
    #[serde(default)]
    pub started_unix: u64,
    /// Unix seconds of the last write; the staleness input for
    /// [`HEARTBEAT_STALE_SECS`]. Defaults to 0 when absent — epoch-stale, which fails
    /// to the safe "not running" state.
    #[serde(default)]
    pub heartbeat_unix: u64,
    /// The whole-pass rollup.
    #[serde(default)]
    pub overall: Overall,
    /// Remaining install order, front first — the array the GUI animates a bumped row
    /// to the front of.
    #[serde(default)]
    pub queue: Vec<String>,
    /// Per-program rows, keyed by program name. Names are UNTRUSTED until
    /// `ToolName`/roster-intersected (see the module docs).
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramProgress>,
    /// Unix seconds when the pass ended CLEANLY — the terminal snapshot's stamp,
    /// written together with `pid: None`. `None` while the pass is live (and on any
    /// snapshot from a writer that died mid-pass, which is exactly what the heartbeat
    /// staleness rule exists to catch).
    #[serde(default)]
    pub ended_unix: Option<u64>,
}

// ---------------------------------------------------------------------------
// The writer: ProgressSink.
// ---------------------------------------------------------------------------

/// Current unix seconds, saturating at 0 for a clock before the epoch (backward skew
/// then reads as epoch-stale — the safe, "not running" direction).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The writer's mutable state, behind the sink's one lock.
struct SinkState {
    /// Where the snapshot lands. The GUI passes `<prefix>/progress.json`.
    path: PathBuf,
    /// The snapshot being built — serialized verbatim on every write.
    file: ProgressFile,
    /// Per-program DOWNLOAD credit `(done, total)` for the overall rollup — kept beside
    /// the wire rows because a row's `bytes_done/bytes_total` switch meaning per phase
    /// (download → extract), while the overall bar's denominator must stay the signed
    /// asset sums the plan fixed.
    dl: BTreeMap<String, (u64, u64)>,
    /// Which program the in-process extract loop is currently crediting (the loop
    /// itself cannot know — see [`extract_tick`]).
    extract_program: Option<String>,
    /// When the last snapshot hit disk — the ≤10 Hz rate cap's clock.
    last_write: Option<Instant>,
    /// Whether the snapshot changed since the last write.
    dirty: bool,
    /// Set when the destination stops being writable as a REGULAR file (a symlink or
    /// FIFO planted at the path): the sink goes silent rather than follow it. Progress
    /// is diagnostics; the install itself never depends on this file.
    disabled: bool,
}

/// The live-progress writer: one per pass, held by the process with the store flock.
///
/// Cheap to clone (an `Arc` over one mutex). Every mutation funnels through
/// [`SinkState`] and lands on disk via the `status.rs` atomic temp+rename discipline,
/// rate-capped to [`WRITE_MIN_INTERVAL_MS`] and heartbeat-refreshed at least every
/// [`HEARTBEAT_MIN_INTERVAL_SECS`] (the download poller guarantees the cadence during
/// long transfers).
#[derive(Clone)]
pub struct ProgressSink {
    inner: Arc<Mutex<SinkState>>,
}

impl ProgressSink {
    /// The live pass's name ("seed" / "net" / …) — so phase writers can honor
    /// per-pass display rules without threading pass labels through flow.
    pub fn pass_name(&self) -> String {
        self.inner
            .lock()
            .map(|st| st.file.pass.clone())
            .unwrap_or_default()
    }
}

/// The writer must refresh `heartbeat_unix` at least this often, even with nothing to
/// report — a reader's staleness window ([`HEARTBEAT_STALE_SECS`]) is only meaningful
/// against a writer that keeps this promise.
pub const HEARTBEAT_MIN_INTERVAL_SECS: u64 = 2;

impl ProgressSink {
    /// Open a sink writing to `path` for the named pass (`"net"` / `"seed"`), truncating
    /// any previous snapshot (a pass start rewrites — a stale file must not survive into
    /// a new pass looking fresh). `None` when the path exists as anything but a regular
    /// file (never follow a symlink out of the prefix — the same discipline the readers
    /// hold) or the initial write fails.
    #[must_use]
    pub fn create(path: &Path, pass: &str) -> Option<Self> {
        let now = unix_now();
        let state = SinkState {
            path: path.to_path_buf(),
            file: ProgressFile {
                v: PROGRESS_VERSION,
                pid: Some(std::process::id()),
                pass: pass.to_string(),
                started_unix: now,
                heartbeat_unix: now,
                overall: Overall::default(),
                queue: Vec::new(),
                programs: BTreeMap::new(),
                ended_unix: None,
            },
            dl: BTreeMap::new(),
            extract_program: None,
            last_write: None,
            dirty: true,
            disabled: false,
        };
        let sink = Self {
            inner: Arc::new(Mutex::new(state)),
        };
        let mut guard = sink.inner.lock().ok()?;
        write_now(&mut guard);
        if guard.disabled {
            return None;
        }
        drop(guard);
        Some(sink)
    }

    /// Fix the pass's plan: the queue order and each planned program's signed asset
    /// size. This is what makes the overall denominator honest and FIXED — already-
    /// current programs are simply not in the plan and contribute zero.
    pub fn plan(&self, planned: &[(String, u64)]) {
        self.with(|s| {
            s.file.queue = planned.iter().map(|(n, _)| n.clone()).collect();
            s.file.overall.programs_total = u32::try_from(planned.len()).unwrap_or(u32::MAX);
            let mut total = 0u64;
            for (name, size) in planned {
                total = total.saturating_add(*size);
                s.dl.insert(name.clone(), (0, *size));
                s.file.programs.insert(
                    name.clone(),
                    ProgramProgress {
                        phase: Phase::Queued,
                        bytes_done: 0,
                        bytes_total: *size,
                        build: None,
                        bumped: false,
                        bumped_with: None,
                        error: None,
                    },
                );
            }
            s.file.overall.bytes_total = total;
            s.dirty = true;
        });
    }

    /// Move `program` to `phase` (label-only phases included — verify and link are
    /// honest labels, not byte meters). Resets the row's byte meter so a stale download
    /// count never renders under an extract label.
    pub fn phase(&self, program: &str, phase: Phase) {
        self.with(|s| {
            let row = s
                .file
                .programs
                .entry(program.to_string())
                .or_insert_with(|| ProgramProgress {
                    phase,
                    bytes_done: 0,
                    bytes_total: 0,
                    build: None,
                    bumped: false,
                    bumped_with: None,
                    error: None,
                });
            if row.phase != phase {
                row.phase = phase;
                row.bytes_done = 0;
                row.bytes_total = 0;
                s.dirty = true;
            }
        });
    }

    /// Record the pinned build being installed, once resolved from the signed index.
    pub fn build(&self, program: &str, build: u64) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program)
                && row.build != Some(build)
            {
                row.build = Some(build);
                s.dirty = true;
            }
        });
    }

    /// Download meter: `done` of `total` bytes landed in the staged `.part`. Flips the
    /// row to [`Phase::Download`] (the poller can observe bytes before the phase call
    /// lands) and feeds the overall rollup's download credit.
    pub fn download_bytes(&self, program: &str, done: u64, total: u64) {
        self.with(|s| {
            let done = if total > 0 { done.min(total) } else { done };
            let credit = s.dl.entry(program.to_string()).or_insert((0, total));
            if credit.1 == 0 {
                credit.1 = total;
            }
            let changed = credit.0 != done;
            credit.0 = done;
            if let Some(row) = s.file.programs.get_mut(program) {
                if row.phase == Phase::Queued {
                    row.phase = Phase::Download;
                }
                if row.phase == Phase::Download {
                    row.bytes_done = done;
                    row.bytes_total = total;
                }
            }
            if changed {
                s.dirty = true;
            }
        });
    }

    /// Begin crediting the in-process extract loop's bytes to `program` (denominator:
    /// the signed `disk_installed` size, 0 ⇒ unmetered). See [`extract_tick`] for why
    /// the loop cannot name the program itself.
    pub fn extract_begin(&self, program: &str, total: u64) {
        self.with(|s| {
            s.extract_program = Some(program.to_string());
            if let Some(row) = s.file.programs.get_mut(program) {
                row.bytes_total = total;
            }
        });
    }

    /// Credit `n` freshly-extracted bytes to the current extract program, flipping its
    /// row to [`Phase::Extract`] on the first tick (the verify→extract boundary lives
    /// inside `verify_and_stage`, which this module deliberately does not modify — the
    /// first written byte IS that boundary).
    pub fn extract_add(&self, n: u64) {
        self.with(|s| {
            let Some(program) = s.extract_program.clone() else {
                return;
            };
            // A download credit for this program is complete the moment extraction
            // starts (the asset fully landed before its sha256 could pass).
            if let Some(credit) = s.dl.get_mut(&program)
                && credit.1 > 0
            {
                credit.0 = credit.1;
            }
            if let Some(row) = s.file.programs.get_mut(&program) {
                if row.phase != Phase::Extract {
                    row.phase = Phase::Extract;
                    row.bytes_done = 0;
                }
                row.bytes_done = row.bytes_done.saturating_add(n);
                s.dirty = true;
            }
        });
    }

    /// Stop crediting the extract loop (the scope guard's other half).
    pub fn extract_end(&self) {
        self.with(|s| {
            s.extract_program = None;
        });
    }

    /// Mark `program` bumped (a user asked for it via the bump file) and re-order the
    /// visible queue to match the installer's re-sorted remainder.
    pub fn bumped(&self, program: &str) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program)
                && !row.bumped
            {
                row.bumped = true;
                s.dirty = true;
            }
        });
    }

    /// Mark `program` bumped ALONG WITH `with` — the bumped program that requires it — so
    /// the row can say why it moved ("bumped with brew"). A row already bumped in its
    /// own right keeps that: the user's own ask outranks the pull.
    pub fn bumped_with(&self, program: &str, with: &str) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program)
                && !row.bumped
            {
                row.bumped = true;
                row.bumped_with = Some(with.to_string());
                s.dirty = true;
            }
        });
    }

    /// Replace the remaining-order queue (front first).
    pub fn queue(&self, order: &[String]) {
        self.with(|s| {
            if s.file.queue != order {
                s.file.queue = order.to_vec();
                s.dirty = true;
            }
        });
    }

    /// A program reached a terminal phase: `Done`, `Failed` (with its honest error) or
    /// `Skipped`. Advances the overall counter and retires the program from the queue;
    /// `Done` completes its download credit so the overall bar never regresses.
    pub fn finished(&self, program: &str, phase: Phase, error: Option<String>) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program) {
                row.phase = phase;
                row.error = error;
                if phase == Phase::Done
                    && let Some(credit) = s.dl.get_mut(program)
                {
                    credit.0 = credit.1;
                }
            }
            s.file.queue.retain(|q| q != program);
            s.file.overall.programs_done = s.file.overall.programs_done.saturating_add(1);
            s.dirty = true;
        });
    }

    /// The clean-pass terminal snapshot: `pid` cleared, `ended_unix` stamped, written
    /// unconditionally. A reader finding this file knows the pass ENDED — as opposed to
    /// a dead-writer file, which only the heartbeat window can call.
    pub fn finish(&self) {
        if let Ok(mut s) = self.inner.lock() {
            s.file.pid = None;
            s.file.ended_unix = Some(unix_now());
            s.dirty = true;
            write_now(&mut s);
        }
    }

    /// Run `f` under the lock, then apply the rate-capped write policy.
    fn with(&self, f: impl FnOnce(&mut SinkState)) {
        if let Ok(mut s) = self.inner.lock() {
            f(&mut s);
            maybe_write(&mut s);
        }
    }

    /// A content-free tick: nothing to report, but the writer is ALIVE — let the
    /// rate-cap policy refresh `heartbeat_unix` if it is due. This is what the
    /// pass heartbeat thread calls (see [`begin_pass`]); every other mutator
    /// gets the same refresh for free through [`Self::with`].
    pub fn heartbeat(&self) {
        self.with(|_| {});
    }
}

/// The write policy: on change AND ≥[`WRITE_MIN_INTERVAL_MS`] since the last write
/// (≤10 Hz), plus an unconditional heartbeat refresh at
/// [`HEARTBEAT_MIN_INTERVAL_SECS`] so a long quiet transfer never lets the file go
/// stale under a LIVE writer.
fn maybe_write(s: &mut SinkState) {
    if s.disabled {
        return;
    }
    let age = s.last_write.map_or(Duration::MAX, |t| t.elapsed());
    let change_due = s.dirty && age >= Duration::from_millis(WRITE_MIN_INTERVAL_MS);
    let heartbeat_due = age >= Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS);
    if change_due || heartbeat_due {
        write_now(s);
    }
}

/// Serialize + land the snapshot atomically (temp + rename, the `status.rs`
/// discipline), refreshing the heartbeat and the overall download rollup. The
/// destination is checked with `symlink_metadata` FIRST — a non-regular file at the
/// path (symlink, FIFO) disables the sink rather than being followed or written
/// through. Best-effort: progress is diagnostics, never load-bearing.
fn write_now(s: &mut SinkState) {
    if s.disabled {
        return;
    }
    let mut done = 0u64;
    for (d, _) in s.dl.values() {
        done = done.saturating_add(*d);
    }
    s.file.overall.bytes_done = done;
    s.file.heartbeat_unix = unix_now();
    match std::fs::symlink_metadata(&s.path) {
        Ok(m) if !m.file_type().is_file() => {
            s.disabled = true;
            return;
        }
        _ => {}
    }
    let Ok(text) = serde_json::to_string(&s.file) else {
        return;
    };
    // Manual concat, mirroring status.rs's temp naming (same Trust-gate rationale).
    let mut tmp_name = String::from("progress.json.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = s.path.with_file_name(tmp_name);
    if crate::call2(std::fs::write, &tmp, text).is_ok() && std::fs::rename(&tmp, &s.path).is_ok() {
        s.last_write = Some(Instant::now());
        s.dirty = false;
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ---------------------------------------------------------------------------
// The process-global sink and its flow hooks.
//
// The download/extract meters live deep inside `flow`/`extract`, behind API
// signatures shared with every caller and test in this workspace. The sink is
// therefore process-global — which is also simply TRUE to the domain: at most one
// process holds the store flock, and that process runs at most one pass at a time.
// `GLOBAL_ENABLED` keeps the disabled path to one relaxed atomic load, so the
// extract loop's per-chunk tick costs nothing when no `--progress-file` was given
// (the terminal lanes, every existing test).
// ---------------------------------------------------------------------------

/// The pass heartbeat: a thread that ticks the live sink every
/// [`HEARTBEAT_TICK_MS`] for the pass's whole lifetime, INDEPENDENT of flow
/// calls. The heartbeat promise used to rest on the `.part` download poller
/// alone, which covers exactly one phase — so a multi-GB Verify (one sha256
/// over the archive, zero sink calls), an Extract, and the seed pass's
/// cross-filesystem copy (the DMG-mounted install; 700c0d21 silenced the
/// seed pass's poller on purpose, taking its cadence with it) all let the
/// file age past [`HEARTBEAT_STALE_SECS`] under a LIVE writer: minutes of
/// red "stopped" telling the user to run a command the running pass would
/// refuse (audit-2 item 8). Owned by the pass exactly like the sink: spawned
/// in [`begin_pass`], stopped and joined in [`end_pass`].
struct PassHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// How often the pass heartbeat ticks. Well under
/// [`HEARTBEAT_MIN_INTERVAL_SECS`] (the write policy decides whether a tick
/// actually lands; most are free no-ops) and far under the readers'
/// [`HEARTBEAT_STALE_SECS`] window.
const HEARTBEAT_TICK_MS: u64 = 500;

impl PassHeartbeat {
    fn spawn(sink: ProgressSink) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("atpkg-pass-heartbeat".into())
            .spawn(move || {
                while !stop2.load(Ordering::Acquire) {
                    sink.heartbeat();
                    std::thread::sleep(Duration::from_millis(HEARTBEAT_TICK_MS));
                }
            })
            .ok();
        Self { stop, handle }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

static GLOBAL_SINK: Mutex<Option<(ProgressSink, std::thread::ThreadId, PassHeartbeat)>> =
    Mutex::new(None);

/// TESTS ONLY: the global sink is process-wide state with a thread owner, so any
/// two tests that `begin_pass` concurrently see each other's refusal. Every such
/// test takes this gate first (a poisoned gate is fine — the state it guards is
/// reset by `begin_pass` itself).
#[cfg(test)]
pub(crate) static PASS_TEST_GATE: Mutex<()> = Mutex::new(());
static GLOBAL_ENABLED: AtomicBool = AtomicBool::new(false);

/// Install the process-global sink for one pass, OWNED BY THE CALLING THREAD.
/// `false` (and no change) when a pass is already live or the file cannot be
/// created — callers proceed without progress, never fail the install over its
/// diagnostics.
///
/// Thread affinity is truthful, not defensive convenience: a real pass runs the
/// whole install pipeline on the one thread that holds the store flock (the `.part`
/// pollers carry their own sink clone and never come back through here), so
/// [`active`] answering only that thread costs production nothing — and keeps a
/// concurrently-running test's installs from crediting a pass they are not part of.
pub fn begin_pass(path: &Path, pass: &str) -> bool {
    let Ok(mut g) = GLOBAL_SINK.lock() else {
        return false;
    };
    if g.is_some() {
        return false;
    }
    let Some(sink) = ProgressSink::create(path, pass) else {
        return false;
    };
    // The heartbeat carries its OWN clone, so it never comes back through
    // `active()`'s owner check — it is not the pass thread and must not be.
    let heartbeat = PassHeartbeat::spawn(sink.clone());
    *g = Some((sink, std::thread::current().id(), heartbeat));
    GLOBAL_ENABLED.store(true, Ordering::Release);
    true
}

/// Write the terminal snapshot and retire the global sink. Safe to call without a
/// live pass (no-op). Only the pass-owning thread ends it.
pub fn end_pass() {
    let Ok(mut g) = GLOBAL_SINK.lock() else {
        return;
    };
    if g.as_ref()
        .is_some_and(|(_, owner, _)| *owner == std::thread::current().id())
        && let Some((sink, _, heartbeat)) = g.take()
    {
        GLOBAL_ENABLED.store(false, Ordering::Release);
        // Stop the heartbeat FIRST: a tick landing after the terminal
        // snapshot would restamp a live heartbeat onto a pid-cleared file.
        heartbeat.stop();
        sink.finish();
    }
}

/// The live pass's sink — for the OWNING thread only (see [`begin_pass`]). One
/// relaxed load answers "no pass anywhere" for free.
#[must_use]
pub fn active() -> Option<ProgressSink> {
    if !GLOBAL_ENABLED.load(Ordering::Acquire) {
        return None;
    }
    GLOBAL_SINK.lock().ok().and_then(|g| {
        g.as_ref().and_then(|(sink, owner, _)| {
            (*owner == std::thread::current().id()).then(|| sink.clone())
        })
    })
}

/// Flow hook: move `program` to `phase` on the live pass, if any.
pub fn note_phase(program: &str, phase: Phase) {
    if let Some(sink) = active() {
        sink.phase(program, phase);
    }
}

/// Flow hook: record the pinned build `program` is installing on the live pass.
pub fn note_build(program: &str, build: u64) {
    if let Some(sink) = active() {
        sink.build(program, build);
    }
}

/// Flow hook: credit `n` extracted bytes to the current extract program. Called from
/// `write_capped`'s 64 KiB loop — the one in-process byte loop — so the disabled
/// path must stay one atomic load.
pub fn extract_tick(n: u64) {
    if !GLOBAL_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(sink) = active() {
        sink.extract_add(n);
    }
}

/// Flow hook: scope guard crediting the extract loop to `program` while alive.
#[must_use]
pub fn extract_scope(program: &str, total: u64) -> ExtractScope {
    if let Some(sink) = active() {
        sink.extract_begin(program, total);
        ExtractScope { live: true }
    } else {
        ExtractScope { live: false }
    }
}

/// Ends the extract crediting scope on drop (panic-safe, so a failing stage can
/// never leave a later program's extract bytes credited to this one).
pub struct ExtractScope {
    live: bool,
}

impl Drop for ExtractScope {
    fn drop(&mut self) {
        if self.live
            && let Some(sink) = active()
        {
            sink.extract_end();
        }
    }
}

/// Flow hook: watch a growing `<asset>.part` while a download runs.
///
/// Downloads happen in a curl child, so atpkg never sees the bytes in-process; a
/// sibling poller thread stats the `.part` at 10 Hz against the SIGNED
/// `artifact.size` and feeds [`ProgressSink::download_bytes`] — which also keeps the
/// heartbeat fresh through a long quiet transfer. The guard stops and joins the
/// thread on drop; with no live pass it spawns nothing.
#[must_use]
pub fn watch_download(program: &str, asset_path: &Path, total: u64) -> DownloadWatch {
    let Some(sink) = active() else {
        return DownloadWatch {
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        };
    };
    // THE SEED PASS SHOWS NO DOWNLOAD ROWS (the design's §1.C promise). The
    // sealed seed is zero-network by construction — its DirFetcher is a local
    // hardlink that beats the write cap, so a download row was invisible by
    // TIMING alone. On the cross-filesystem fallback (a DMG-mounted install:
    // the copy replaces the hardlink) that luck runs out and a row would sit
    // at "download 0 bytes" for a multi-gigabyte copy — a lying phase on the
    // one pass whose whole point is that nothing downloads. The verify/
    // extract/link phases still report; only the download phase is inert.
    if sink.pass_name() == "seed" {
        return DownloadWatch {
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        };
    }
    sink.phase(program, Phase::Download);
    sink.download_bytes(program, 0, total);
    // `<asset>.part` — APPENDED, matching `aterm_update_core`'s `part_path` (never
    // `with_extension`, which would collide across builds).
    let Some(name) = asset_path.file_name() else {
        return DownloadWatch {
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        };
    };
    let mut part = name.to_os_string();
    part.push(".part");
    let part = asset_path.with_file_name(part);
    let program = program.to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("atpkg-part-poll".into())
        .spawn(move || {
            while !stop2.load(Ordering::Acquire) {
                // Regular files only — never follow a symlink out of the prefix
                // (the same discipline as every reader of these files).
                if let Ok(m) = std::fs::symlink_metadata(&part)
                    && m.file_type().is_file()
                {
                    sink.download_bytes(&program, m.len(), total);
                } else {
                    // No .part (yet, or a dir-registry copy): still let the sink
                    // refresh its heartbeat through the rate-cap policy.
                    sink.download_bytes(&program, 0, total);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .ok();
    DownloadWatch { stop, handle }
}

/// Stops the `.part` poller on drop.
pub struct DownloadWatch {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DownloadWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// The readers: untrusted progress/bump access shared by `__pending` and `run`.
// ---------------------------------------------------------------------------

/// Read `<prefix>/progress.json` under the untrusted-reader rules: symlink-refusing
/// bounded open ([`crate::metadata_io`]), [`PROGRESS_READ_CAP`], malformed/oversize ⇒
/// `None` (treated as absent, never partially parsed). The returned snapshot is still
/// DATA, not truth — callers must apply [`snapshot_running`] before rendering any
/// live-looking state, and must sanitize every string that reaches a TTY.
#[must_use]
pub fn read_progress(layout: &crate::store::Layout) -> Option<ProgressFile> {
    let cap = usize::try_from(PROGRESS_READ_CAP).unwrap_or(usize::MAX);
    let text = crate::metadata_io::read_bounded_regular_utf8(&layout.progress_file(), cap).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether `file` was written by an installer that is RUNNING NOW: a fresh heartbeat
/// (within [`HEARTBEAT_STALE_SECS`]) and a live pid. A dead installer's file can never
/// claim live progress — heartbeat older than the window, a heartbeat from the future
/// beyond the window (backward clock skew), a cleared pid, or a dead pid all read as
/// "not running".
#[must_use]
pub fn snapshot_running(file: &ProgressFile, now_unix: u64) -> bool {
    let Some(pid) = file.pid else {
        return false;
    };
    let hb = file.heartbeat_unix;
    // A heartbeat "ahead" of now (clock skew) is fresh only within the same window —
    // an absurd future stamp must not pin the file live forever.
    let fresh = hb <= now_unix.saturating_add(HEARTBEAT_STALE_SECS)
        && now_unix.saturating_sub(hb) <= HEARTBEAT_STALE_SECS;
    fresh && pid_alive(pid)
}

/// Best-effort pid liveness. Probed via `/bin/kill -0` (a safe subprocess, no
/// signal delivered — "does this process exist and may I signal it") rather than a
/// raw `libc::kill`, keeping atpkg's non-test code `unsafe`-free. When the probe
/// itself cannot run, the answer falls back to `true` — the heartbeat window is the
/// authoritative staleness gate and covers that case within 10 s.
#[must_use]
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("/bin/kill")
            .arg("-0")
            .arg(crate::dec_u64(u64::from(pid)))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_or(true, |s| s.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Strip control characters (terminal escape-sequence injection) and cap length
/// before a progress/status string reaches a TTY. The cap is in characters, applied
/// after the strip; an elided tail is marked so truncation is visible.
#[must_use]
pub fn sanitize_for_tty(s: &str, cap: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().filter(|c| !c.is_control()).enumerate() {
        if i >= cap {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// Append `program` to `<prefix>/bump` — the stub's whole write privilege. The path
/// is refused unless absent or a regular non-link file (`symlink_metadata` before
/// open, `O_NOFOLLOW` on the open itself where the OS has it), and the name arrives
/// as an already-admitted [`crate::store::ToolName`] so nothing shim-refused can be
/// written. Append-only: the installer is the only consumer and deleter.
pub fn append_bump(
    layout: &crate::store::Layout,
    program: &crate::store::ToolName,
) -> std::io::Result<()> {
    // A machine whose prefix does not exist yet still gets its wish recorded — the
    // very first provisioning pass is exactly the one a fresh install's bump should
    // front-load. Through the layout so a system prefix keeps its shared mode.
    layout.ensure_dir(&layout.prefix)?;
    let path = layout.bump_file();
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bump is not a regular file",
            ));
        }
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    use std::io::Write as _;
    let mut line = String::from(program.as_str());
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// Read the bump file's admitted names, in file order, first mention wins:
/// size-capped ([`BUMP_READ_CAP`], oversize ⇒ absent), symlink-refusing, each line
/// gated through [`crate::store::ToolName`]. Garbage lines are ignored. The caller
/// STILL intersects with its own planned work — this returns candidate names, never
/// authority.
#[must_use]
pub fn read_bump(layout: &crate::store::Layout) -> Vec<String> {
    let cap = usize::try_from(BUMP_READ_CAP).unwrap_or(usize::MAX);
    let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&layout.bump_file(), cap) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(tool) = crate::store::ToolName::new(line)
            && !out.iter().any(|n| n == tool.as_str())
        {
            out.push(tool.as_str().to_string());
        }
    }
    out
}

/// Delete the bump file — the clean-pass-end consumption step.
pub fn clear_bump(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(layout.bump_file());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design document's own example is the wire contract: it must round-trip
    /// field-for-field, and the phase strings must be exactly the lowercase vocabulary
    /// the readers grep for.
    #[test]
    fn the_v1_example_snapshot_round_trips() {
        let json = r#"{"v":1, "pid":41234, "pass":"net", "started_unix":0, "heartbeat_unix":0,
             "overall":{"programs_done":2,"programs_total":9,"bytes_done":18022400,"bytes_total":96411648},
             "queue":["trust","robi"],
             "programs":{"trust":{"phase":"download","bytes_done":4300800,"bytes_total":30104576,
                                  "build":210,"bumped":true,"error":null}}}"#;
        let parsed: ProgressFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.v, PROGRESS_VERSION);
        assert_eq!(parsed.pid, Some(41234));
        assert_eq!(parsed.pass, "net");
        assert_eq!(parsed.overall.programs_total, 9);
        assert_eq!(parsed.queue, vec!["trust".to_string(), "robi".to_string()]);
        let trust = &parsed.programs["trust"];
        assert_eq!(trust.phase, Phase::Download);
        assert_eq!(trust.bytes_done, 4_300_800);
        assert_eq!(trust.build, Some(210));
        assert!(trust.bumped && trust.error.is_none());
        // Round-trip: serialize → parse → identical.
        let reparsed: ProgressFile =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(reparsed, parsed);
    }

    /// The wire vocabulary, pinned string by string: a renamed variant is a protocol
    /// break for every reader, and this is the test that makes it loud.
    #[test]
    fn phase_strings_are_the_documented_lowercase_vocabulary() {
        for (phase, wire) in [
            (Phase::Queued, "\"queued\""),
            (Phase::Download, "\"download\""),
            (Phase::Verify, "\"verify\""),
            (Phase::Extract, "\"extract\""),
            (Phase::Link, "\"link\""),
            (Phase::Done, "\"done\""),
            (Phase::Failed, "\"failed\""),
            (Phase::Skipped, "\"skipped\""),
        ] {
            assert_eq!(serde_json::to_string(&phase).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Phase>(wire).unwrap(), phase);
        }
    }

    /// Forward compatibility is load-bearing: a NEWER writer's extra fields must never
    /// break an older reader, and a minimal snapshot must degrade to the SAFE reading
    /// (no pid, epoch-stale heartbeat ⇒ "installer not running"), not a parse failure.
    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_fail_safe() {
        let future = r#"{"v":1, "hologram":true,
             "programs":{"trust":{"phase":"done","shiny_new_meter":9000}}}"#;
        let parsed: ProgressFile = serde_json::from_str(future).unwrap();
        assert_eq!(parsed.pid, None, "no pid claim without a pid field");
        assert_eq!(
            parsed.heartbeat_unix, 0,
            "a missing heartbeat is epoch-stale — the safe direction"
        );
        assert_eq!(parsed.programs["trust"].phase, Phase::Done);
        // An unknown VERSION still parses the envelope — the reader's job is then to
        // render the generic line, not to guess at v2 field meanings.
        let v2: ProgressFile = serde_json::from_str(r#"{"v":2}"#).unwrap();
        assert_ne!(v2.v, PROGRESS_VERSION);
    }

    /// The caps and staleness window are shared constants, not per-reader folklore —
    /// pinned so a drive-by "tune" shows up as a failing test with the rationale
    /// attached.
    #[test]
    fn the_shared_limits_hold_their_documented_values() {
        assert_eq!(PROGRESS_READ_CAP, 256 * 1024);
        assert_eq!(BUMP_READ_CAP, 4 * 1024);
        assert_eq!(HEARTBEAT_STALE_SECS, 10);
        assert_eq!(WRITE_MIN_INTERVAL_MS, 100, "≤10 Hz");
    }
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn layout(label: &str) -> crate::store::Layout {
        let p = std::env::temp_dir().join(format!("atpkg-progress-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::store::Layout { prefix: p }
    }

    fn read_file(path: &std::path::Path) -> ProgressFile {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The writer's landing is atomic (temp+rename — no temp survives) and the pass
    /// start truncates: a stale snapshot from a previous run never bleeds through.
    #[test]
    fn create_truncates_and_leaves_no_temp() {
        let l = layout("create");
        let path = l.progress_file();
        std::fs::write(&path, r#"{"v":1,"pass":"stale","pid":1}"#).unwrap();
        let sink = ProgressSink::create(&path, "net").expect("sink creates");
        let file = read_file(&path);
        assert_eq!(file.pass, "net", "pass start rewrites the stale snapshot");
        assert_eq!(file.pid, Some(std::process::id()));
        assert!(file.ended_unix.is_none());
        let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp+rename leaves no temp behind");
        drop(sink);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The rate cap: a burst of changes inside [`WRITE_MIN_INTERVAL_MS`] lands at most
    /// one extra write — the on-disk file does NOT track every mutation.
    #[test]
    fn writes_are_rate_capped_to_ten_hz() {
        let l = layout("ratecap");
        let path = l.progress_file();
        let sink = ProgressSink::create(&path, "net").unwrap();
        sink.plan(&[("trust".into(), 100)]);
        // Burst immediately after create: within the 100 ms window nothing may land.
        for n in 1..50u64 {
            sink.download_bytes("trust", n, 100);
        }
        let file = read_file(&path);
        assert!(
            file.programs.is_empty() || file.programs["trust"].bytes_done < 49,
            "a same-instant burst must not land write-per-mutation: {file:?}"
        );
        // After the window, the next change lands.
        std::thread::sleep(Duration::from_millis(WRITE_MIN_INTERVAL_MS + 20));
        sink.download_bytes("trust", 50, 100);
        assert_eq!(read_file(&path).programs["trust"].bytes_done, 50);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The heartbeat promise: with NOTHING changing, a poller-driven no-op call after
    /// [`HEARTBEAT_MIN_INTERVAL_SECS`] still refreshes `heartbeat_unix` — a live
    /// writer's file can never go stale under the readers' 10 s window.
    #[test]
    fn heartbeat_refreshes_with_no_change() {
        let l = layout("heartbeat");
        let path = l.progress_file();
        let sink = ProgressSink::create(&path, "net").unwrap();
        let h0 = read_file(&path).heartbeat_unix;
        std::thread::sleep(Duration::from_millis(
            HEARTBEAT_MIN_INTERVAL_SECS * 1000 + 100,
        ));
        // A no-change poll (same bytes as at create): not dirty, but heartbeat-due.
        sink.download_bytes("trust", 0, 0);
        let h1 = read_file(&path).heartbeat_unix;
        assert!(
            h1 >= h0 + HEARTBEAT_MIN_INTERVAL_SECS,
            "no-change writes must still refresh the heartbeat ({h0} → {h1})"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The terminal snapshot: `finish` clears the pid, stamps `ended_unix`, and writes
    /// unconditionally (no rate cap can swallow the last word).
    #[test]
    fn finish_writes_the_terminal_snapshot() {
        let l = layout("finish");
        let path = l.progress_file();
        let sink = ProgressSink::create(&path, "seed").unwrap();
        sink.plan(&[("ty".into(), 7)]);
        sink.finished("ty", Phase::Done, None);
        sink.finish();
        let file = read_file(&path);
        assert_eq!(file.pid, None, "a clean end clears the pid");
        assert!(file.ended_unix.is_some());
        assert_eq!(file.programs["ty"].phase, Phase::Done);
        assert_eq!(file.overall.programs_done, 1);
        assert!(file.queue.is_empty(), "a finished program leaves the queue");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The symlink discipline binds the WRITER too: a symlink planted at the progress
    /// path disables the sink instead of being followed out of the prefix.
    #[cfg(unix)]
    #[test]
    fn writer_refuses_a_symlinked_destination() {
        let l = layout("symlink");
        let victim = l.prefix.join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let path = l.progress_file();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        assert!(
            ProgressSink::create(&path, "net").is_none(),
            "a non-regular destination must disable the sink"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "precious",
            "the symlink target was never written through"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Staleness is the reader's law: a fresh-heartbeat live-pid file is running; a
    /// cleared pid, an old heartbeat, or a far-future heartbeat (backward skew) is not.
    #[test]
    fn snapshot_running_applies_the_heartbeat_window() {
        let now = 1_000_000u64;
        let mut file: ProgressFile = serde_json::from_str(r#"{"v":1}"#).unwrap();
        file.pid = Some(std::process::id()); // this test process: provably alive
        file.heartbeat_unix = now;
        assert!(snapshot_running(&file, now));
        file.heartbeat_unix = now - HEARTBEAT_STALE_SECS - 1;
        assert!(
            !snapshot_running(&file, now),
            "stale heartbeat = not running"
        );
        file.heartbeat_unix = now + HEARTBEAT_STALE_SECS + 1;
        assert!(
            !snapshot_running(&file, now),
            "future heartbeat = not running"
        );
        file.heartbeat_unix = now;
        file.pid = None;
        assert!(!snapshot_running(&file, now), "no pid = not running");
    }

    /// A dead pid renders not-running even under a fresh heartbeat — the 10 s lie
    /// window a crashed installer would otherwise get.
    #[cfg(unix)]
    #[test]
    fn snapshot_running_rejects_a_dead_pid() {
        // Spawn-and-reap a child so its pid is known-dead (not merely unlikely).
        let child = std::process::Command::new("/usr/bin/true")
            .status()
            .map(|_| ())
            .ok();
        let dead = std::process::Command::new("/usr/bin/true")
            .spawn()
            .map(|mut c| {
                let pid = c.id();
                let _ = c.wait();
                pid
            });
        let _ = child;
        let Ok(dead_pid) = dead else {
            return; // cannot build the fixture on this host — skip, don't lie
        };
        let mut file: ProgressFile = serde_json::from_str(r#"{"v":1}"#).unwrap();
        file.pid = Some(dead_pid);
        file.heartbeat_unix = 1_000_000;
        assert!(
            !snapshot_running(&file, 1_000_000),
            "a reaped pid must not read as a live installer"
        );
    }

    /// TTY sanitization: control characters (escape injection) stripped, length capped
    /// with a visible elision.
    #[test]
    fn sanitize_for_tty_strips_and_caps() {
        assert_eq!(sanitize_for_tty("plain error", 64), "plain error");
        assert_eq!(
            sanitize_for_tty("evil\u{1b}[2Jwipe\r\nmore", 64),
            "evil[2Jwipemore",
            "ESC/CR/LF are control characters and must not reach the TTY"
        );
        assert_eq!(sanitize_for_tty("abcdef", 3), "abc…");
    }

    /// The bump channel: appended names round-trip; garbage and shim-refused names
    /// are ignored; an oversized file reads as absent; a symlinked bump is refused on
    /// both the append and the read.
    #[test]
    fn bump_round_trips_and_refuses_garbage() {
        let l = layout("bump");
        let trust = crate::store::ToolName::new("trust").unwrap();
        let ty = crate::store::ToolName::new("ty").unwrap();
        append_bump(&l, &trust).unwrap();
        append_bump(&l, &ty).unwrap();
        append_bump(&l, &trust).unwrap(); // duplicate: first mention wins
        assert_eq!(read_bump(&l), vec!["trust".to_string(), "ty".to_string()]);
        // Garbage lines: ignored, never a crash, never a name.
        std::fs::write(l.bump_file(), "sudo\n../evil\na/b\ntrust\n\n").unwrap();
        assert_eq!(
            read_bump(&l),
            vec!["trust".to_string()],
            "shim-refused and malformed names never come back"
        );
        // Oversize ⇒ absent.
        std::fs::write(l.bump_file(), "x".repeat(BUMP_READ_CAP as usize + 1)).unwrap();
        assert!(read_bump(&l).is_empty());
        clear_bump(&l);
        assert!(!l.bump_file().exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Symlink refusal on the bump paths (unix): append refuses, read refuses.
    #[cfg(unix)]
    #[test]
    fn bump_refuses_symlinks() {
        let l = layout("bump-sym");
        let victim = l.prefix.join("victim");
        std::fs::write(&victim, "trust\n").unwrap();
        std::os::unix::fs::symlink(&victim, l.bump_file()).unwrap();
        let trust = crate::store::ToolName::new("trust").unwrap();
        assert!(
            append_bump(&l, &trust).is_err(),
            "append must not follow a link"
        );
        assert!(read_bump(&l).is_empty(), "read must not follow a link");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "trust\n");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The untrusted progress reader: absent, oversized, and symlinked files all read
    /// as `None`; a well-formed file comes back parsed.
    /// §1.C's "no download rows on the seed pass" is CODE, not luck. The sealed
    /// seed's DirFetcher hardlink used to beat the 100 ms write cap, so a
    /// download row was merely invisible by timing — and on the cross-filesystem
    /// copy fallback (a DMG-mounted install) it surfaced as a lying
    /// "download 0 bytes" for a multi-gigabyte local copy.
    ///
    /// Asserted on the WATCH's own shape (inert vs live poller), not on file
    /// rows: snapshot writes ride a 100 ms cap, so file-based assertions here
    /// are timing, which is the exact mistake this rule exists to end.
    #[test]
    fn the_seed_pass_never_shows_a_download_phase() {
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let l = layout("seed-no-dl");
        assert!(begin_pass(&l.progress_file(), "seed"));
        let watch = watch_download("trust", &l.prefix.join("trust-1.tar.zst"), 1000);
        assert!(
            watch.handle.is_none(),
            "the seed pass must get the INERT watch — no download phase, no poller"
        );
        drop(watch);
        end_pass();
        // The same call on a NET pass spawns the live poller — the suppression
        // is seed-scoped, never a silencing of the phase itself.
        assert!(begin_pass(&l.progress_file(), "net"));
        let watch = watch_download("trust", &l.prefix.join("trust-1.tar.zst"), 1000);
        assert!(
            watch.handle.is_some(),
            "the net pass keeps its live download watch"
        );
        drop(watch);
        end_pass();
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The heartbeat promise, kept INDEPENDENTLY of flow calls (audit-2 item 8):
    /// a pass that makes zero sink calls for longer than the writer's minimum
    /// interval — a multi-GB Verify sha256, an Extract, the seed pass's
    /// cross-filesystem copy — must still read as RUNNING, because the pass
    /// heartbeat thread ticks the file on its own. Before it, the promise
    /// rested on the download poller alone (one phase; silenced on the seed
    /// pass by 700c0d21), so those phases went red "stopped" under a live
    /// writer. Asserted on the heartbeat ADVANCING while the pass thread is
    /// deliberately asleep — a flow call from this thread would defeat the
    /// point of the test.
    #[test]
    fn a_silent_phase_keeps_the_pass_heartbeat_live() {
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let l = layout("silent-heartbeat");
        assert!(begin_pass(&l.progress_file(), "seed"));
        let h0 = read_file(&l.progress_file()).heartbeat_unix;
        // Longer than the writer's minimum interval, with NO sink call from
        // the pass thread: only the heartbeat thread can move the stamp.
        std::thread::sleep(Duration::from_millis(
            HEARTBEAT_MIN_INTERVAL_SECS * 1000 + 700,
        ));
        let file = read_file(&l.progress_file());
        assert!(
            file.heartbeat_unix >= h0 + HEARTBEAT_MIN_INTERVAL_SECS,
            "the pass heartbeat must advance with no flow calls ({h0} → {})",
            file.heartbeat_unix
        );
        assert!(
            snapshot_running(&file, unix_now()),
            "a silent-but-live pass reads as running"
        );
        end_pass();
        // The terminal snapshot is the LAST word: the heartbeat thread is
        // stopped before it, so no tick can restamp a pid-cleared file live.
        let ended = read_file(&l.progress_file());
        assert_eq!(ended.pid, None);
        std::thread::sleep(Duration::from_millis(HEARTBEAT_TICK_MS * 2 + 200));
        let later = read_file(&l.progress_file());
        assert_eq!(
            later.heartbeat_unix, ended.heartbeat_unix,
            "no heartbeat tick may land after end_pass"
        );
        assert!(!snapshot_running(&later, unix_now()));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn read_progress_is_bounded_and_symlink_refusing() {
        let l = layout("read");
        assert!(read_progress(&l).is_none(), "absent reads as None");
        std::fs::write(l.progress_file(), r#"{"v":1,"pass":"net"}"#).unwrap();
        assert_eq!(read_progress(&l).unwrap().pass, "net");
        std::fs::write(l.progress_file(), "not json at all {{{").unwrap();
        assert!(read_progress(&l).is_none(), "malformed reads as None");
        let mut big = String::from(r#"{"v":1,"pass":""#);
        big.push_str(&"x".repeat(PROGRESS_READ_CAP as usize));
        big.push_str("\"}");
        std::fs::write(l.progress_file(), big).unwrap();
        assert!(read_progress(&l).is_none(), "oversize reads as None");
        #[cfg(unix)]
        {
            let victim = l.prefix.join("victim.json");
            std::fs::write(&victim, r#"{"v":1}"#).unwrap();
            let _ = std::fs::remove_file(l.progress_file());
            std::os::unix::fs::symlink(&victim, l.progress_file()).unwrap();
            assert!(read_progress(&l).is_none(), "a symlink is never followed");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
