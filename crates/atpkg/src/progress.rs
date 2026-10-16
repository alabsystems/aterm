// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The live install-progress protocol: `progress.json` v1 + the `bump` file, and the
//! promise of a pass that has not begun yet (`pass-owed.json`, [`pass_owed`]).
//!
//! **This module holds the TYPES, constants and format contract only.** The writer (a
//! `ProgressSink` threaded through `install_default_set_with_path` / `vendor_pass` /
//! `apply_group_txn`, enabled by `--progress-file` or by a pass a person typed on a
//! terminal) and the readers (the GUI tailer, `atpkg __pending` and the terminal meter,
//! [`crate::meter`]) are built on top of it — the schema lives here so every party
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
//! ran — followed by [`UNATTENDED_BUMP`] when a script ran it, not a person:
//! [`Asker`]), consumed by the installer between items. The installer reads it capped at
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
    /// Why `phase` is `failed`, when it is. UNTRUSTED for display: control-strip and
    /// length-cap before any TTY (see the module docs).
    #[serde(default)]
    pub error: Option<String>,
}

impl ProgramProgress {
    /// A download whose every byte is in, waiting for the pass to stage it: the set
    /// completion fetches archives ahead of the program it is staging. Nothing moves for it,
    /// so a reader naming what the pass is doing now names a row that is moving first.
    #[must_use]
    pub fn downloaded(&self) -> bool {
        self.phase == Phase::Download && self.bytes_total > 0 && self.bytes_done >= self.bytes_total
    }
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
    /// Which lane is writing: `"net"` (the network default-set pass — since Phase 5 the
    /// only default-set pass; older builds also wrote `"seed"`, the deleted sealed-payload
    /// pass), or a pass a person typed on a terminal that plans no default set —
    /// `"update"` (the channel apply) and `"install"` (one program). A string, not an
    /// enum, so an old reader renders an unknown future lane generically instead of
    /// failing the parse.
    #[serde(default)]
    pub pass: String,
    /// Unix seconds when the pass started.
    #[serde(default)]
    pub started_unix: u64,
    /// An opaque per-pass identity, minted once and retained in the terminal
    /// snapshot. Unix seconds alone cannot distinguish consecutive short passes.
    /// Older writers (or unavailable OS entropy) leave this absent; readers then
    /// require a live-pass observation instead of guessing from an ended timestamp.
    #[serde(default)]
    pub generation: Option<String>,
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
    aterm_time::unix_secs().unwrap_or(0)
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
/// [`SinkState`] and lands on disk via the `status.rs` atomic temp+rename discipline.
/// Ordinary meters are rate-capped to [`WRITE_MIN_INTERVAL_MS`] and heartbeat-refreshed
/// at least every [`HEARTBEAT_MIN_INTERVAL_SECS`] (the download poller guarantees the
/// cadence during long transfers); the plan and terminal snapshot land immediately.
#[derive(Clone)]
pub struct ProgressSink {
    inner: Arc<Mutex<SinkState>>,
}

impl ProgressSink {
    /// The pass as it stands NOW — the snapshot the next write would land, its overall
    /// download credit summed as [`write_now`] sums it — for the terminal meter
    /// ([`crate::meter`]), which must not wait out the write cap or read the file back.
    #[must_use]
    pub fn snapshot(&self) -> Option<ProgressFile> {
        self.inner.lock().ok().map(|st| {
            let mut file = st.file.clone();
            file.overall.bytes_done = st
                .dl
                .values()
                .fold(0u64, |sum, (done, _)| sum.saturating_add(*done));
            file
        })
    }
}

/// The writer must refresh `heartbeat_unix` at least this often, even with nothing to
/// report — a reader's staleness window ([`HEARTBEAT_STALE_SECS`]) is only meaningful
/// against a writer that keeps this promise.
pub const HEARTBEAT_MIN_INTERVAL_SECS: u64 = 2;

impl ProgressSink {
    /// Open a sink writing to `path` for the named pass (`"net"`), truncating
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
                generation: aterm_uds::rand::hex_token::<16>().ok(),
                heartbeat_unix: now,
                overall: Overall::default(),
                queue: Vec::new(),
                programs: BTreeMap::new(),
                ended_unix: None,
            },
            dl: BTreeMap::new(),
            last_write: None,
            dirty: true,
            disabled: false,
        };
        let sink = Self {
            inner: Arc::new(Mutex::new(state)),
        };
        let mut guard = sink.inner.lock().ok()?;
        write_now(&mut guard);
        // Both halves of the contract above, because `disabled` is only the
        // non-regular-destination half: a write that simply FAILED (the prefix does not
        // exist yet, or is not writable by this user) leaves `disabled` false and
        // `last_write` None, and a successful first write always stamps `last_write`.
        // A live sink over a file that can never appear is not free — `begin_pass`
        // would report an owned pass, spin the heartbeat thread every tick and run
        // every `.part` poller against a write that cannot succeed.
        if guard.disabled || guard.last_write.is_none() {
            return None;
        }
        drop(guard);
        Some(sink)
    }

    /// Fix the pass's plan: the queue order and each planned program's signed asset
    /// size. This is what makes the overall denominator honest and FIXED — already-
    /// current programs are simply not in the plan and contribute zero. Publish
    /// it before returning, bypassing the ordinary meter's write cap: a caller
    /// may stop promising the pass only once its actual rows are readable.
    /// Returns whether this publication landed.
    pub fn plan(&self, planned: &[(String, u64)]) -> bool {
        let Ok(mut s) = self.inner.lock() else {
            return false;
        };
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
                    error: None,
                },
            );
        }
        s.file.overall.bytes_total = total;
        s.dirty = true;
        write_now(&mut s)
    }

    /// Transfer a default-set pass's promise to its live planned rows. A stub reads
    /// those rows from here on, including when the plan leaves its program out.
    pub(crate) fn plan_promised_pass(
        &self,
        layout: &crate::store::Layout,
        planned: &[(String, u64)],
    ) {
        if self.plan(planned) {
            retire_promise(layout);
        }
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

    /// Size `program`'s extract meter (denominator: the signed `disk_installed` size,
    /// 0 ⇒ unmetered) as its extract begins ([`extract_scope`]).
    pub fn extract_begin(&self, program: &str, total: u64) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program) {
                row.bytes_total = total;
            }
        });
    }

    /// Credit `n` freshly-extracted bytes to `program`, flipping its row to
    /// [`Phase::Extract`] on the first tick (the verify→extract boundary lives inside
    /// `verify_and_stage`, which this module deliberately does not modify — the first
    /// written byte IS that boundary).
    pub fn extract_add(&self, program: &str, n: u64) {
        self.with(|s| {
            // A download credit for this program is complete the moment extraction
            // starts (the asset fully landed before its sha256 could pass).
            if let Some(credit) = s.dl.get_mut(program)
                && credit.1 > 0
            {
                credit.0 = credit.1;
            }
            if let Some(row) = s.file.programs.get_mut(program) {
                if row.phase != Phase::Extract {
                    row.phase = Phase::Extract;
                    row.bytes_done = 0;
                }
                row.bytes_done = row.bytes_done.saturating_add(n);
                s.dirty = true;
            }
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

    /// Replace the remaining-order queue (front first). A second lane may have finished
    /// a row since the caller assembled its order: retire terminal rows under this same
    /// lock, so reordering cannot put one back in the queue.
    pub fn queue(&self, order: &[String]) {
        self.with(|s| {
            let order: Vec<String> = order
                .iter()
                .filter(|name| {
                    !s.file.programs.get(name.as_str()).is_some_and(|row| {
                        matches!(row.phase, Phase::Done | Phase::Failed | Phase::Skipped)
                    })
                })
                .cloned()
                .collect();
            if s.file.queue != order {
                s.file.queue = order;
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
fn write_now(s: &mut SinkState) -> bool {
    if s.disabled {
        return false;
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
            return false;
        }
        _ => {}
    }
    let Ok(text) = aterm_json::to_string(&s.file) else {
        return false;
    };
    let tmp = s
        .path
        .with_file_name(format!("progress.json.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &s.path).is_ok() {
        s.last_write = Some(Instant::now());
        s.dirty = false;
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
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
// extract loop's per-chunk tick costs nothing when no pass is live (a pipe with no
// `--progress-file`, every existing test).
// ---------------------------------------------------------------------------

/// The wait BOTH pass-owned threads use between ticks: hold for `tick`, but
/// return the INSTANT [`stop_and_join`] raises `stop`.
///
/// `std::thread::sleep` cannot be interrupted, so the stop flag alone bought
/// nothing at the join: the thread that stops these pollers is the PASS thread —
/// the one holding the store flock — and it sat out whatever remained of the
/// current tick before `join` returned. That was up to [`PART_POLL_TICK_MS`] of
/// pure idle between download-complete and the sha256, once per downloaded
/// artifact (a default set pays it a dozen times), and up to
/// [`HEARTBEAT_TICK_MS`] at [`end_pass`] before the terminal snapshot the GUI
/// retires its row on. Parking is the same wait with a doorbell, and the unpark
/// token is STICKY: a stop landing in the window between the flag check and the
/// park itself is not lost, it makes the next park return at once.
///
/// The deadline is recomputed rather than trusted, because `park_timeout` may
/// return spuriously early and a stale token spends itself on the first park:
/// re-parking for the REMAINDER keeps the tick cadence exactly what `sleep`
/// gave it, so nothing downstream of these ticks changes.
pub(crate) fn tick_or_stop(stop: &AtomicBool, tick: Duration) {
    let deadline = Instant::now() + tick;
    while !stop.load(Ordering::Acquire) {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return;
        };
        std::thread::park_timeout(left);
    }
}

/// Stop a thread parked in [`tick_or_stop`] and join it: raise the flag, ring the
/// doorbell, THEN wait. The order is the contract — a thread woken by the unpark
/// must be able to see the flag it was woken for.
pub(crate) fn stop_and_join(stop: &AtomicBool, handle: Option<std::thread::JoinHandle<()>>) {
    stop.store(true, Ordering::Release);
    if let Some(h) = handle {
        h.thread().unpark();
        let _ = h.join();
    }
}

/// The pass heartbeat: a thread that ticks the live sink every
/// [`HEARTBEAT_TICK_MS`] for the pass's whole lifetime, INDEPENDENT of flow
/// calls. The heartbeat promise used to rest on the `.part` download poller
/// alone, which covers exactly one phase — so a multi-GB Verify (one sha256
/// over the archive, zero sink calls) or an Extract let the file age past
/// [`HEARTBEAT_STALE_SECS`] under a LIVE writer: minutes of red "stopped"
/// telling the user to run a command the running pass would refuse (audit-2
/// item 8). Owned by the pass exactly like the sink: spawned in [`begin_pass`],
/// stopped and joined in [`end_pass`].
struct PassHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// How often the pass heartbeat ticks. Well under
/// [`HEARTBEAT_MIN_INTERVAL_SECS`] (the write policy decides whether a tick
/// actually lands; most are free no-ops) and far under the readers'
/// [`HEARTBEAT_STALE_SECS`] window.
const HEARTBEAT_TICK_MS: u64 = 500;

/// The tick a pass-owned thread spawned NOW parks for: `ms`, unless a test set
/// `TICK_OVERRIDE` on the spawning thread. Read there and moved into the new
/// thread, so no other test's threads ever see it. (Plain backticks: the
/// override is `cfg(test)`, so an intra-doc link would resolve to nothing in
/// the build this doc is rendered for.)
fn pass_tick(ms: u64) -> Duration {
    #[cfg(test)]
    {
        if let Some(tick) = TICK_OVERRIDE.get() {
            return tick;
        }
    }
    Duration::from_millis(ms)
}

#[cfg(test)]
thread_local! {
    /// TESTS ONLY: the tick [`pass_tick`] hands the pass-owned threads THIS
    /// thread spawns. An hour makes a stop that waits out its tick unable to
    /// return inside a test at all, so the return is the proof — not a
    /// stopwatch. The doc sits INSIDE the macro: on the invocation it would be
    /// `unused_doc_comments`, an error under this crate's gate.
    static TICK_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

impl PassHeartbeat {
    fn spawn(sink: ProgressSink) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let tick = pass_tick(HEARTBEAT_TICK_MS);
        let handle = std::thread::Builder::new()
            .name("atpkg-pass-heartbeat".into())
            .spawn(move || {
                while !stop2.load(Ordering::Acquire) {
                    sink.heartbeat();
                    tick_or_stop(&stop2, tick);
                }
            })
            .ok();
        Self { stop, handle }
    }

    fn stop(mut self) {
        stop_and_join(&self.stop, self.handle.take());
    }
}

/// Where an orphaned pass's output goes, beside `progress.json`: `<prefix>/orphan-pass.log`.
pub const ORPHAN_LOG_NAME: &str = "orphan-pass.log";

/// THE ORPHANED PASS KEEPS WORKING (2026-09-14). A window's pass child is spawned with
/// its stdout and stderr piped to the window and told the window's pid
/// ([`crate::cli::SPAWNER_PID_ENV`]). When that window exits under it — a self-update
/// exec'ing into its successor (the incident: 0.84 → 0.85 applied at 15:06:29, the
/// successor's own child queued behind this one's flock at 15:06:35), the app quit and
/// reopened for a Full Disk Access grant, a window closed by hand — nobody reads the
/// pipes any more, Rust leaves SIGPIPE ignored, and the child's next `println!` got EPIPE
/// and PANICKED (exit 101): minutes of download thrown away, the flock held until that
/// print, and the successor's window showing a "waiting for another aterm's toolchain
/// install" row for a pass that was doomed the moment it next spoke. So a watcher
/// thread, armed at the dispatch edge for the WHOLE process (`crate::cli::main_entry`,
/// only when the spawner passed `--wait-lock`, which the window always does and a typed
/// `aterm pkg update` never — the audit of 2026-09-14 caught the first draft riding the
/// pass heartbeat, which exists only inside a progress pass, so the channel-apply lane
/// that moves the multi-gigabyte rustc group had no watcher at all), polls every
/// [`ORPHAN_POLL_MS`] for the parent to CHANGE ([`crate::cli::parent_is_gone`] — the same
/// rule the lock waiter stands down on) and, once, points fds 1 and 2 at
/// [`ORPHAN_LOG_NAME`] under the store prefix: every later print lands there, the pass
/// runs to completion, the store it leaves is what the successor's `--wait-lock` child
/// finds, and the successor's window shows the REAL progress meanwhile through its
/// child-scoped tailer of `progress.json`. The window's marker contract
/// (`lock-waiting:`, `net-installed:` …) is not blinded: its reader was the parent, and
/// the parent is gone. `curl` is untouched: its stdout and stderr are piped to atpkg,
/// never to the window. A print that lands between the parent's exit and the next poll
/// still dies the old way — a ≤ 100 ms window against passes that print at program
/// boundaries minutes apart — and costs what every death always cost, one program's
/// redo on a crash-consistent store (`crate::lock`), nothing more. A parent that exec's
/// in place keeps its pid (the cold apply lane) but closes the pipes it read, which the
/// watch sees as a pipe with no reader ([`stdio_lost_its_reader`]). One shape it does
/// not cover, on purpose: an orphan that outlives a self-update resolves its launchd
/// helper by path, so the NEW binary serves its hidden verbs — a spec of another
/// version is refused through the result file, and the policy's fallback then records
/// what it wrote.
pub fn watch_for_orphaning(log: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("atpkg-orphan-watch".into())
        .spawn(move || {
            loop {
                if crate::cli::parent_is_gone() || stdio_lost_its_reader() {
                    quiet_stdio(&log);
                    return;
                }
                std::thread::sleep(Duration::from_millis(ORPHAN_POLL_MS));
            }
        });
}

/// Whether the pipe on fd 1 or fd 2 has NO READER any more — the hazard itself, which
/// a parent that `exec`s in place produces WITHOUT changing its pid (the cold apply
/// lane: the same process image is replaced, the pipes it read are closed). Measured
/// 2026-09-15 on macOS: `poll(2)` on a pipe's write end with `POLLOUT` requested reports
/// `POLLHUP` once the read end is closed (and nothing with no events requested); a tty
/// or a regular file reports `POLLOUT` alone. `POLLERR`/`POLLNVAL` count as gone too.
#[cfg(unix)]
fn stdio_lost_its_reader() -> bool {
    let mut fds = [
        libc::pollfd {
            fd: 1,
            events: libc::POLLOUT,
            revents: 0,
        },
        libc::pollfd {
            fd: 2,
            events: libc::POLLOUT,
            revents: 0,
        },
    ];
    // SAFETY: `poll` reads and writes only the array handed to it, for its stated
    // length, and a zero timeout never blocks.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 0) };
    if rc < 0 {
        return false;
    }
    fds.iter()
        .any(|p| p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
}

#[cfg(not(unix))]
fn stdio_lost_its_reader() -> bool {
    false
}

/// How often the orphan watch asks for its parent pid — one `getppid(2)`, so cheap
/// that the residual EPIPE window is what sets it, not the cost.
const ORPHAN_POLL_MS: u64 = 100;

/// Point this process's stdout and stderr at `log` (appended, `0600`), then say so —
/// into the log, which is where that line belongs once the pipes are dead. A log that
/// cannot be opened leaves the fds alone: the pass then dies at its next print exactly
/// as it always did, and nothing is worse.
#[cfg(unix)]
fn quiet_stdio(log: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::io::AsRawFd as _;
    let Ok(file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(log)
    else {
        return;
    };
    let fd = file.as_raw_fd();
    // SAFETY: `dup2` re-points the two standard descriptors at an open file this
    // process owns; `file` is leaked on purpose right after, so the descriptor it
    // wraps stays valid for the rest of the process and neither `dup2` ever names a
    // closed one. No other thread expects fds 1 and 2 to be anything in particular.
    unsafe {
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
    }
    std::mem::forget(file);
    eprintln!(
        "atpkg: the window that spawned this pass is gone; the pass (pid {}) continues \
         and its output lands here",
        std::process::id()
    );
}

#[cfg(not(unix))]
fn quiet_stdio(_log: &Path) {}

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
/// install pipeline on the thread that holds the store flock (the `.part` pollers carry
/// their own sink clone and never come back through here), and a lane thread it starts
/// inside the pass is handed the pass explicitly ([`lend`]), so [`active`] answering
/// only those threads costs production nothing — and keeps a concurrently-running test's
/// installs from crediting a pass they are not part of.
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

/// The live pass's sink — for the OWNING thread (see [`begin_pass`]), and for a lane
/// thread that owner started and LENT it to ([`lend`]). One relaxed load answers "no pass
/// anywhere" for free.
#[must_use]
pub fn active() -> Option<ProgressSink> {
    if !GLOBAL_ENABLED.load(Ordering::Acquire) {
        return None;
    }
    GLOBAL_SINK
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref().and_then(|(sink, owner, _)| {
                (*owner == std::thread::current().id()).then(|| sink.clone())
            })
        })
        .or_else(|| LENT.with(|lent| lent.borrow().clone()))
}

thread_local! {
    /// The pass a lane thread was lent ([`Lent::adopt`]): what [`active`] answers on it.
    static LENT: std::cell::RefCell<Option<ProgressSink>> = const { std::cell::RefCell::new(None) };
    /// The program THIS thread's extract loop credits ([`extract_scope`]): two lanes of one
    /// pass extract at once, each into its own row.
    static EXTRACTING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The live pass, as its owning thread hands it to a lane thread it starts — one that lands
/// programs beside the owner's own, inside the pass and under its store lock. Empty when no
/// pass is live, or off the owning thread.
#[derive(Clone)]
pub struct Lent(Option<ProgressSink>);

/// On the pass-owning thread: the live pass, to lend a lane thread ([`Lent::adopt`]).
#[must_use]
pub fn lend() -> Lent {
    Lent(active())
}

impl Lent {
    /// Credit this thread's flow hooks to the lent pass until the guard drops. The owner
    /// joins the thread before it ends the pass, and an ended pass is credited by no one
    /// ([`active`]'s first load).
    #[must_use]
    pub fn adopt(self) -> Adopted {
        LENT.with(|lent| *lent.borrow_mut() = self.0);
        Adopted(())
    }
}

/// Ends a lane thread's adoption of a lent pass on drop ([`Lent::adopt`]).
pub struct Adopted(());

impl Drop for Adopted {
    fn drop(&mut self) {
        LENT.with(|lent| *lent.borrow_mut() = None);
    }
}

/// The live pass's snapshot, from ANY thread — [`active`] answers only the pass's own
/// thread, and the terminal meter ([`crate::meter`]) draws from a thread of its own. Read
/// only: nothing reached through this can move the pass.
#[must_use]
pub fn live_snapshot() -> Option<ProgressFile> {
    if !GLOBAL_ENABLED.load(Ordering::Acquire) {
        return None;
    }
    let sink = GLOBAL_SINK
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(sink, _, _)| sink.clone()))?;
    sink.snapshot()
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

/// Flow hook: `program`'s download of `total` signed bytes is complete, and its row waits
/// for the stage at that whole size ([`ProgramProgress::downloaded`]) — the set completion's
/// download-ahead lane finishes archives the pass reaches later.
pub fn note_downloaded(program: &str, total: u64) {
    if let Some(sink) = active() {
        sink.download_bytes(program, total, total);
    }
}

/// Flow hook: credit `n` extracted bytes to this thread's extract program (the loop
/// itself cannot name it). Called from `write_capped`'s 64 KiB loop — the one in-process
/// byte loop — so the disabled path must stay one atomic load.
pub fn extract_tick(n: u64) {
    if !GLOBAL_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(sink) = active() {
        EXTRACTING.with(|program| {
            if let Some(program) = program.borrow().as_deref() {
                sink.extract_add(program, n);
            }
        });
    }
}

/// Flow hook: scope guard crediting this thread's extract loop to `program` while alive.
#[must_use]
pub fn extract_scope(program: &str, total: u64) -> ExtractScope {
    if let Some(sink) = active() {
        sink.extract_begin(program, total);
        EXTRACTING.with(|current| *current.borrow_mut() = Some(program.to_string()));
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
        if self.live {
            EXTRACTING.with(|current| *current.borrow_mut() = None);
        }
    }
}

/// How often the `.part` poller stats the growing asset: the 10 Hz the sink's
/// own write cap is sized for.
const PART_POLL_TICK_MS: u64 = 100;

/// Flow hook: watch a growing `<asset>.part` while a download runs.
///
/// Downloads happen in a curl child, so atpkg never sees the bytes in-process; a
/// sibling poller thread stats the `.part` at 10 Hz against the SIGNED
/// `artifact.size` and feeds [`ProgressSink::download_bytes`] — which also keeps the
/// heartbeat fresh through a long quiet transfer. The guard stops, WAKES and joins
/// the thread on drop — never waiting out a tick, see [`tick_or_stop`] — and with no
/// live pass it spawns nothing.
#[must_use]
pub fn watch_download(program: &str, asset_path: &Path, total: u64) -> DownloadWatch {
    let Some(sink) = active() else {
        return DownloadWatch {
            stop: Arc::new(AtomicBool::new(true)),
            handle: None,
        };
    };
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
    let tick = pass_tick(PART_POLL_TICK_MS);
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
                tick_or_stop(&stop2, tick);
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
        stop_and_join(&self.stop, self.handle.take());
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
    aterm_json::from_str(&text).ok()
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

/// Whether a pass is installing on `layout`'s store now: its progress file names a
/// RUNNING writer ([`snapshot_running`]). What a scheduler reads beside `status.toml` so
/// it never queues a pass of its own behind one already in flight.
#[must_use]
pub fn pass_running(layout: &crate::store::Layout, now_unix: u64) -> bool {
    read_progress(layout).is_some_and(|file| snapshot_running(&file, now_unix))
}

/// Shared kernel pid liveness: Unix signal-zero and Windows process-handle probes
/// both distinguish a dead owner from an existing process we may not query. A
/// promise ends with its owner on either platform, without spawning a child.
#[must_use]
pub(crate) fn pid_alive(pid: u32) -> bool {
    aterm_uds::process::pid_alive(pid)
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

/// Who asked for a program the bump file names ([`append_bump`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asker {
    /// A person at a terminal: the stub's stdin and stderr are both terminals. A plain name
    /// line, the only line the file ever held, so an older reader and an older writer agree.
    Person,
    /// A script, a pipe or an agent's tool call: the name and [`UNATTENDED_BUMP`] on one
    /// line. An older reader takes the line for a name no pass plans, and ignores it.
    Script,
}

/// What follows a name on a bump line written for a script ([`Asker::Script`]).
pub const UNATTENDED_BUMP: &str = " unattended";

/// One program the bump file asks for, in first-mention order ([`read_bumps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bump {
    /// The admitted program name.
    pub program: String,
    /// Whether a person asked for it on any of its lines ([`Asker::Person`]).
    pub person: bool,
}

/// Append `program` to `<prefix>/bump` — the stub's whole write privilege — as `asker`'s
/// line. The path is refused unless absent or a regular non-link file (`symlink_metadata`
/// before open, `O_NOFOLLOW` on the open itself where the OS has it), and the name arrives
/// as an already-admitted [`crate::store::ToolName`] so nothing shim-refused can be
/// written. Append-only: the installer is the only consumer and deleter.
pub fn append_bump(
    layout: &crate::store::Layout,
    program: &crate::store::ToolName,
    asker: Asker,
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
    if asker == Asker::Script {
        line.push_str(UNATTENDED_BUMP);
    }
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// Read the bump file's admitted names, in file order, first mention wins, each with
/// whether a person asked for it on any line: size-capped ([`BUMP_READ_CAP`], oversize ⇒
/// absent), symlink-refusing, each name gated through [`crate::store::ToolName`]. Garbage
/// lines are ignored. The caller STILL intersects with its own planned work — this
/// returns candidate names, never authority.
#[must_use]
pub fn read_bumps(layout: &crate::store::Layout) -> Vec<Bump> {
    let cap = usize::try_from(BUMP_READ_CAP).unwrap_or(usize::MAX);
    let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&layout.bump_file(), cap) else {
        return Vec::new();
    };
    let mut out: Vec<Bump> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let (name, person) = match line.strip_suffix(UNATTENDED_BUMP) {
            Some(name) => (name.trim_end(), false),
            None => (line, true),
        };
        if name.is_empty() {
            continue;
        }
        let Some(tool) = crate::store::ToolName::new(name) else {
            continue;
        };
        match out.iter_mut().find(|b| b.program == tool.as_str()) {
            Some(seen) => seen.person |= person,
            None => out.push(Bump {
                program: tool.as_str().to_string(),
                person,
            }),
        }
    }
    out
}

/// The bump file's admitted names, whoever asked ([`read_bumps`]): what reorders a pass's
/// remaining work and wakes a window's pass.
#[must_use]
pub fn read_bump(layout: &crate::store::Layout) -> Vec<String> {
    read_bumps(layout).into_iter().map(|b| b.program).collect()
}

/// Delete the bump file — the clean-pass-end consumption step.
pub fn clear_bump(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(layout.bump_file());
}

// ---------------------------------------------------------------------------
// The promise: `pass-owed.json`.
//
// `progress.json` appears only once a pass begins, seconds into a first launch: the
// window's seed lays the pending stubs and the shell hooks within milliseconds, and the
// update it spawns next resolves the index and plans before it writes a plan. A stub run
// in between used to find nothing installing and exit 127. The promise says a default-set
// pass is coming and has not planned yet: written under the store lock by whoever knows
// it is owed — the window's seed on a first run, its process the owner; a whole update
// that completes the set on a first install or over a promise it finds, itself the owner —
// and retired by that update once its plan lands or it ends (an update retires only its own
// promise or its spawner's, [`retire_promise_of`]). A promise names a deadline and an owner
// — its pid and, where it can be read, its start time — and lapses at either. No
// other reader looks at it: to every `progress.json` reader nothing is running until the
// pass begins.
// ---------------------------------------------------------------------------

/// Read cap for `pass-owed.json`; a larger file reads as no promise.
pub const PROMISE_READ_CAP: usize = 1024;

/// How long the window's seed promises the update it spawns next (the login shell's PATH
/// is that update's only wait, five seconds at most).
pub const SEED_PROMISE_SECS: u64 = 60;

/// How long a whole update promises its own plan: its index resolve, vendor lane and
/// manifest prefetch come first. It retires the promise itself, and its death breaks it —
/// a pid a crash freed for another process included ([`PassPromise::born`]) — so this bounds
/// only an owner that hangs before it plans, or one whose start time no reader could read.
pub const PASS_PROMISE_SECS: u64 = 10 * 60;

/// `pass-owed.json`: the process the promise rides on and when it lapses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassPromise {
    /// Schema version ([`PROGRESS_VERSION`]); any other reads as no promise.
    pub v: u32,
    /// The pid whose death breaks the promise: the window that will spawn the pass, or
    /// the pass itself.
    pub owner: u32,
    /// Unix seconds after which the promise is broken.
    pub until_unix: u64,
    /// The owner's start time as the writer read it ([`crate::platform::process_birth`]): a
    /// process that took over the pid of an owner that died does not keep the promise. A
    /// field an older reader ignores, so it keeps reading the promise by the pid alone; a
    /// promise without it (an older writer's, or one whose writer could not read it) is read
    /// that way too, and so is one whose READER cannot ([`same_owner`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub born: Option<u64>,
}

/// Promise a default-set pass on `layout`'s store until `owner` dies or `secs` pass. The
/// caller holds the store lock. Lands atomically (temp + rename, `0600`), never through a
/// symlink or anything else that is not a regular file. Best-effort: a promise that
/// cannot be written leaves a stub where it was before (exit 127).
pub fn promise_pass(layout: &crate::store::Layout, owner: u32, secs: u64) {
    let path = layout.pass_owed_file();
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        return;
    }
    let promise = PassPromise {
        v: PROGRESS_VERSION,
        owner,
        until_unix: unix_now().saturating_add(secs),
        born: crate::platform::process_birth(owner).at(),
    };
    let Ok(text) = aterm_json::to_string(&promise) else {
        return;
    };
    let tmp = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
    // Only a newly created temp belongs to this call. A colliding entry may be
    // a FIFO or a hardlink to unrelated metadata; never open or remove it.
    let Ok(mut file) = crate::platform::create_private_new(&tmp) else {
        return;
    };
    use std::io::Write as _;
    let wrote = file.write_all(text.as_bytes());
    drop(file);
    if wrote.is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Retire the promise: the pass it was for has planned, or ended.
pub fn retire_promise(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(layout.pass_owed_file());
}

/// Retire the promise only when one of `holders` owns it: an update ending retires the
/// promise it wrote, or the seed's of the window that spawned it — never one another
/// window's seed wrote for the update that window spawns next. The caller holds the store
/// lock, as every writer does, so nothing replaces the file between the read and the
/// removal. An unreadable promise is left as it is (it promises nothing).
pub fn retire_promise_of(layout: &crate::store::Layout, holders: &[u32]) {
    if read_promise(layout).is_some_and(|promise| holders.contains(&promise.owner)) {
        retire_promise(layout);
    }
}

/// `pass-owed.json` as written, bounded and symlink-refusing ([`PROMISE_READ_CAP`]).
fn read_promise(layout: &crate::store::Layout) -> Option<PassPromise> {
    let text =
        crate::metadata_io::read_bounded_regular_utf8(&layout.pass_owed_file(), PROMISE_READ_CAP)
            .ok()?;
    aterm_json::from_str::<PassPromise>(&text).ok()
}

/// Whether a promised pass is still coming at `now_unix`: a readable promise (bounded,
/// symlink-refusing, [`PROMISE_READ_CAP`]) whose deadline has not passed — nor lies
/// further ahead than any writer promises, a clock that stepped back — and whose owner is
/// alive ([`pid_alive`]) and, when the writer read its start time, the same process that
/// wrote it ([`same_owner`]): a pid a crash freed and another process took promises nothing.
#[must_use]
pub fn pass_owed(layout: &crate::store::Layout, now_unix: u64) -> bool {
    let Some(promise) = read_promise(layout) else {
        return false;
    };
    promise.v == PROGRESS_VERSION
        && now_unix <= promise.until_unix
        && promise.until_unix <= now_unix.saturating_add(PASS_PROMISE_SECS)
        && pid_alive(promise.owner)
        && same_owner(promise.born, crate::platform::process_birth(promise.owner))
}

/// Whether the process under a promise's owner pid, whose start time reads `now`, is the one
/// the writer read as `born`: no recorded start time ⇒ the pid alone answers; another start
/// time, or nothing under the pid (a zombie), ⇒ another process, or none; a start time the
/// reader cannot read — a stub run in a sandbox that denies process-info, where `kill(pid, 0)`
/// still answers ([`pid_alive`]) — says nothing, and the pid alone answers, as it does for
/// `EPERM` there. Pure for the test.
#[must_use]
pub fn same_owner(born: Option<u64>, now: crate::platform::Birth) -> bool {
    use crate::platform::Birth;
    match (born, now) {
        (None, _) | (Some(_), Birth::Unreadable) => true,
        (Some(born), Birth::At(now)) => born == now,
        (Some(_), Birth::Gone) => false,
    }
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
        let parsed: ProgressFile = aterm_json::from_str(json).unwrap();
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
            aterm_json::from_str(&aterm_json::to_string(&parsed).unwrap()).unwrap();
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
            assert_eq!(aterm_json::to_string(&phase).unwrap(), wire);
            assert_eq!(aterm_json::from_str::<Phase>(wire).unwrap(), phase);
        }
    }

    /// Forward compatibility is load-bearing: a NEWER writer's extra fields must never
    /// break an older reader, and a minimal snapshot must degrade to the SAFE reading
    /// (no pid, epoch-stale heartbeat ⇒ "installer not running"), not a parse failure.
    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_fail_safe() {
        let future = r#"{"v":1, "hologram":true,
             "programs":{"trust":{"phase":"done","shiny_new_meter":9000}}}"#;
        let parsed: ProgressFile = aterm_json::from_str(future).unwrap();
        assert_eq!(parsed.pid, None, "no pid claim without a pid field");
        assert_eq!(
            parsed.heartbeat_unix, 0,
            "a missing heartbeat is epoch-stale — the safe direction"
        );
        assert_eq!(parsed.programs["trust"].phase, Phase::Done);
        // An unknown VERSION still parses the envelope — the reader's job is then to
        // render the generic line, not to guess at v2 field meanings.
        let v2: ProgressFile = aterm_json::from_str(r#"{"v":2}"#).unwrap();
        assert_ne!(v2.v, PROGRESS_VERSION);
    }
}

#[cfg(test)]
#[path = "progress_plan_publication_tests.rs"]
mod plan_publication_tests;

#[cfg(test)]
mod writer_tests {
    use super::*;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-progress-{label}-"))
    }

    fn read_file(path: &std::path::Path) -> ProgressFile {
        aterm_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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

    /// The terminal snapshot: `finish` clears the pid, stamps `ended_unix`, and writes
    /// unconditionally (no rate cap can swallow the last word).
    #[test]
    fn finish_writes_the_terminal_snapshot() {
        let l = layout("finish");
        let path = l.progress_file();
        let sink = ProgressSink::create(&path, "net").unwrap();
        let generation = read_file(&path).generation.expect("this pass is tagged");
        sink.plan(&[("ty".into(), 7)]);
        sink.finished("ty", Phase::Done, None);
        sink.finish();
        let file = read_file(&path);
        assert_eq!(file.pid, None, "a clean end clears the pid");
        assert!(file.ended_unix.is_some());
        assert_eq!(file.generation.as_deref(), Some(generation.as_str()));
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

    /// `create`'s OTHER refusal, the one `disabled` cannot see: the destination is
    /// nothing suspicious, the first write simply cannot land — the GUI passed
    /// `--progress-file <prefix>/progress.json` before the prefix exists (or the
    /// prefix is not writable by this user). The write fails, nothing in the state
    /// changes, and a `Some` here would hand [`begin_pass`] a pass it reports as OWNED
    /// (`true`) whose file never appears: the heartbeat thread ticks every
    /// [`HEARTBEAT_TICK_MS`] and every `.part` poller runs, each retrying a write that
    /// cannot succeed, for the whole pass.
    #[test]
    fn writer_refuses_a_destination_the_first_write_cannot_reach() {
        let l = layout("unreachable");
        let unreachable = l
            .prefix
            .join("prefix-not-created-yet")
            .join("progress.json");
        assert!(
            ProgressSink::create(&unreachable, "net").is_none(),
            "a first write that cannot land must not yield a live sink"
        );
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let began = begin_pass(&unreachable, "net");
        if began {
            // Never leak a live pass into the next test when this assertion fails.
            end_pass();
        }
        assert!(
            !began,
            "an unwritable progress file must leave the lane with NO pass"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Staleness is the reader's law: a fresh-heartbeat live-pid file is running; a
    /// cleared pid, an old heartbeat, or a far-future heartbeat (backward skew) is not.
    #[test]
    fn snapshot_running_applies_the_heartbeat_window() {
        let now = 1_000_000u64;
        let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
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

    /// A scheduler's in-flight read: no file, a file whose writer ended or went stale,
    /// and a live writer's — only the last is a pass running now.
    #[test]
    fn pass_running_reads_a_live_writer_off_the_progress_file() {
        let l = layout("pass-running");
        let now = 1_000_000u64;
        assert!(!pass_running(&l, now), "no progress file");
        let write = |pid: Option<u32>, heartbeat: u64| {
            let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
            file.pid = pid;
            file.heartbeat_unix = heartbeat;
            std::fs::write(l.progress_file(), aterm_json::to_string(&file).unwrap()).unwrap();
        };
        write(Some(std::process::id()), now);
        assert!(pass_running(&l, now), "a live writer");
        write(None, now);
        assert!(!pass_running(&l, now), "the pass ended");
        write(Some(std::process::id()), now - HEARTBEAT_STALE_SECS - 1);
        assert!(!pass_running(&l, now), "a stale heartbeat");
        let _ = std::fs::remove_dir_all(&l.prefix);
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
        let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
        file.pid = Some(dead_pid);
        file.heartbeat_unix = 1_000_000;
        assert!(
            !snapshot_running(&file, 1_000_000),
            "a reaped pid must not read as a live installer"
        );
    }

    /// A pid we may not SIGNAL is still a pid that EXISTS. The probe used to fork
    /// `/bin/kill -0 <pid>`, which exits non-zero on `EPERM`, so an installer owned by
    /// another user (pid 1 — launchd/init — stands in for one here, and is the same
    /// witness the heal job's own probe uses) read as DEAD under a fresh heartbeat.
    /// The syscall spelling answers "exists", which is what the staleness gate asks.
    #[cfg(unix)]
    #[test]
    fn pid_alive_reads_an_unsignalable_foreign_pid_as_alive() {
        assert!(pid_alive(std::process::id()), "this process is alive");
        assert!(
            pid_alive(1),
            "pid 1 exists — EPERM is not ESRCH, and a foreign pass is not a dead one"
        );
        let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
        file.pid = Some(1);
        file.heartbeat_unix = 1_000_000;
        assert!(
            snapshot_running(&file, 1_000_000),
            "a fresh heartbeat from a live pid we cannot signal is a RUNNING installer"
        );
        // Wider than `pid_t`: cannot name a process, and read dead before too.
        assert!(!pid_alive(u32::MAX), "an impossible pid is not alive");
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
        append_bump(&l, &trust, Asker::Person).unwrap();
        append_bump(&l, &ty, Asker::Person).unwrap();
        append_bump(&l, &trust, Asker::Person).unwrap(); // duplicate: first mention wins
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

    /// A script's bump names its program as a person's does, for every reader that orders
    /// work by it, and says nobody at a terminal asked: a person's line for the same name,
    /// before or after it, makes it a person's. A line an older writer wrote is a person's.
    #[test]
    fn a_bump_says_whether_a_person_asked() {
        let l = layout("bump-asker");
        let codex = crate::store::ToolName::new("codex").unwrap();
        let claude = crate::store::ToolName::new("claude").unwrap();
        append_bump(&l, &codex, Asker::Script).unwrap();
        append_bump(&l, &claude, Asker::Person).unwrap();
        assert_eq!(bump_contents(&l), "codex unattended\nclaude\n");
        assert_eq!(read_bump(&l), ["codex", "claude"]);
        let bump = |program: &str, person| Bump {
            program: program.to_string(),
            person,
        };
        assert_eq!(read_bumps(&l), [bump("codex", false), bump("claude", true)]);
        append_bump(&l, &codex, Asker::Person).unwrap();
        append_bump(&l, &claude, Asker::Script).unwrap();
        assert_eq!(
            read_bumps(&l),
            [bump("codex", true), bump("claude", true)],
            "a person's line makes it a person's, in first-mention order"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    fn bump_contents(l: &crate::store::Layout) -> String {
        std::fs::read_to_string(l.bump_file()).unwrap_or_default()
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
            append_bump(&l, &trust, Asker::Person).is_err(),
            "append must not follow a link"
        );
        assert!(read_bump(&l).is_empty(), "read must not follow a link");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "trust\n");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// STOPPING A PASS POLLER MUST NOT COST ITS TICK. Both pass-owned threads
    /// are joined by the PASS thread — the one holding the store flock — and an
    /// uninterruptible `sleep` made each join wait out whatever remained of the
    /// current tick: up to 100 ms between download-complete and the sha256 on
    /// EVERY downloaded artifact, and up to 500 ms at `end_pass` before the
    /// terminal snapshot the GUI retires its row on. Asserted on the STOP itself,
    /// from inside a tick, never on the tick cadence — which production keeps
    /// deliberately unchanged, and is what the heartbeat tests above pin.
    ///
    /// PROVEN BY RETURNING, NOT BY A STOPWATCH (2026-09-25). Both threads here
    /// park for an HOUR ([`TICK_OVERRIDE`]): a stop that waited out its tick
    /// cannot come back inside this test at all, and one that wakes the thread
    /// comes back however loaded the machine is. The stopwatch this replaced —
    /// four drops under 80 ms, `end_pass` under 150 ms — timed two thread wakes
    /// per drop and an APFS write+rename as much as the stop, and a loaded machine
    /// failed a correct `end_pass` at 220 ms against a regression
    /// worth ~250 ms: no clock separates the two. The 60 s below is only the
    /// fail-safe for that hour: a regression fails there, a slow machine never
    /// does. That no tick lands after the terminal snapshot is structural:
    /// `end_pass` stops and joins the heartbeat before it writes that snapshot.
    #[test]
    fn stopping_a_pass_poller_does_not_wait_out_its_tick() {
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let l = layout("prompt-stop");
        let progress = l.progress_file();
        let asset = l.prefix.join("trust-1.tar.zst");
        // Seven bytes of `.part`: a poller's first tick reports 7, which the
        // synchronous start of `watch_download` (0) never does — the witness
        // that the poller ran and is on its way into its hour-long park.
        std::fs::write(l.prefix.join("trust-1.tar.zst.part"), [0u8; 7]).unwrap();
        let reported =
            || live_snapshot().and_then(|f| f.programs.get("trust").map(|r| r.bytes_done));
        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        // The pass runs on a thread of its own (`end_pass` answers only its
        // owner), so this one can bound the whole of it.
        let pass = std::thread::spawn(move || {
            TICK_OVERRIDE.set(Some(Duration::from_secs(3600)));
            assert!(begin_pass(&progress, "net"));
            for _ in 0..4 {
                let watch = watch_download("trust", &asset, 1000);
                assert!(watch.handle.is_some(), "a net pass gets the live poller");
                while reported() != Some(7) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drop(watch);
            }
            // `end_pass` joins the heartbeat BEFORE `finish()` writes the
            // terminal snapshot, so a heartbeat that slept out its tick would
            // hold the GUI's "ended" back for all of it.
            end_pass();
            let _ = ended_tx.send(read_file(&progress));
        });
        let ended = match ended_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(ended) => ended,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::panic::resume_unwind(pass.join().expect_err("the pass sent nothing"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "the pass did not end within 60 s of hour-long ticks: a stop waited out \
                 its tick (a poller or the heartbeat sleeps instead of parking)"
            ),
        };
        pass.join().unwrap();
        assert_eq!(ended.pid, None, "the terminal snapshot landed");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The heartbeat promise, kept INDEPENDENTLY of flow calls (audit-2 item
    /// 8): a pass that makes zero sink calls for longer than the writer's
    /// minimum interval — a multi-GB Verify sha256, an Extract — must still
    /// read as RUNNING, because the pass heartbeat thread ticks the file on its
    /// own. Before it, the promise rested on the download poller alone (one
    /// phase), so those phases went red "stopped" under a live writer. Asserted
    /// on the heartbeat ADVANCING while the pass thread is deliberately asleep
    /// — a flow call from this thread would defeat the point of the test.
    #[test]
    fn a_silent_phase_keeps_the_pass_heartbeat_live() {
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let l = layout("silent-heartbeat");
        assert!(begin_pass(&l.progress_file(), "net"));
        let h0 = read_file(&l.progress_file()).heartbeat_unix;
        // With NO sink call from the pass thread, only the heartbeat thread can
        // move the stamp, so wait for THAT: a thread that never ticks still
        // fails at the deadline. It was one read at the minimum interval +
        // 700 ms, and the due write lands 0-500 ms into that by tick phase, so
        // five oversleeping 500 ms parks on a loaded run queue made a correct
        // heartbeat read as dead (the load-sensitive test audit of 2026-09-27).
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let file = loop {
            let file = read_file(&l.progress_file());
            if file.heartbeat_unix >= h0 + HEARTBEAT_MIN_INTERVAL_SECS {
                break file;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pass heartbeat must advance with no flow calls ({h0} → {})",
                file.heartbeat_unix
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            snapshot_running(&file, unix_now()),
            "a silent-but-live pass reads as running"
        );
        end_pass();
        // The terminal snapshot is the LAST word: `end_pass` stops and joins the
        // heartbeat thread before it, so no tick can restamp a pid-cleared file live.
        let ended = read_file(&l.progress_file());
        assert_eq!(ended.pid, None);
        assert!(!snapshot_running(&ended, unix_now()));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LANE THREAD CREDITS THE PASS IT WAS LENT, and no other thread does: `active`
    /// answers the owner and the thread that adopted the pass, never a bystander. Two lanes
    /// extracting at once each credit their own row — one pass-wide extract slot handed
    /// both lanes' bytes to whichever began last.
    #[test]
    fn a_lent_pass_credits_its_lane_and_each_lane_extracts_into_its_own_row() {
        let _gate = PASS_TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let l = layout("lent");
        assert!(begin_pass(&l.progress_file(), "net"));
        active()
            .expect("the owner")
            .plan(&[("codex".to_string(), 10), ("trust".to_string(), 10)]);
        let both_open = std::sync::Barrier::new(2);
        let both_ticked = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            assert!(
                s.spawn(|| active().is_none()).join().unwrap(),
                "a thread nobody lent the pass credits nothing"
            );
            let lent = lend();
            let (both_open, both_ticked) = (&both_open, &both_ticked);
            let lane = s.spawn(move || {
                let _adopted = lent.adopt();
                let _codex = extract_scope("codex", 100);
                both_open.wait();
                extract_tick(7);
                both_ticked.wait();
            });
            let _trust = extract_scope("trust", 100);
            both_open.wait();
            extract_tick(3);
            both_ticked.wait();
            lane.join().unwrap();
        });
        let file = live_snapshot().expect("the pass is live");
        end_pass();
        let credited = |p: &str| (file.programs[p].phase, file.programs[p].bytes_done);
        assert_eq!(credited("codex"), (Phase::Extract, 7));
        assert_eq!(credited("trust"), (Phase::Extract, 3));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The promise holds while its owner lives and its deadline is ahead, and not a moment
    /// longer: retired, past its deadline, from a clock that stepped back (a deadline
    /// further ahead than any writer promises), of another version, oversized or malformed,
    /// it promises nothing. It lands `0600`, and never through a symlink.
    #[test]
    fn a_promise_holds_only_while_its_owner_lives_and_its_deadline_is_ahead() {
        let l = layout("promise");
        let now = unix_now();
        let me = std::process::id();
        assert!(!pass_owed(&l, now), "no promise");
        promise_pass(&l, me, SEED_PROMISE_SECS);
        let written: PassPromise =
            aterm_json::from_str(&std::fs::read_to_string(l.pass_owed_file()).unwrap()).unwrap();
        assert_eq!(written.owner, me);
        assert!(
            (now + SEED_PROMISE_SECS..=unix_now() + SEED_PROMISE_SECS)
                .contains(&written.until_unix),
            "{written:?}"
        );
        assert!(pass_owed(&l, now), "a live owner, a deadline ahead");
        assert!(pass_owed(&l, written.until_unix), "up to the deadline");
        assert!(!pass_owed(&l, written.until_unix + 1), "not past it");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(l.pass_owed_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        retire_promise(&l);
        assert!(!pass_owed(&l, now), "retired");
        let write = |body: String| std::fs::write(l.pass_owed_file(), body).unwrap();
        write(format!(
            "{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}",
            now + PASS_PROMISE_SECS + 60
        ));
        assert!(!pass_owed(&l, now), "a deadline no writer promises");
        write(format!(
            "{{\"v\":2,\"owner\":{me},\"until_unix\":{}}}",
            now + 5
        ));
        assert!(!pass_owed(&l, now), "another version");
        write("{not json".to_string());
        assert!(!pass_owed(&l, now), "malformed");
        write(" ".repeat(PROMISE_READ_CAP + 1));
        assert!(!pass_owed(&l, now), "oversized");
        #[cfg(unix)]
        {
            let victim = l.prefix.join("victim.json");
            let honest = format!("{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}", now + 5);
            std::fs::write(&victim, &honest).unwrap();
            let _ = std::fs::remove_file(l.pass_owed_file());
            std::os::unix::fs::symlink(&victim, l.pass_owed_file()).unwrap();
            assert!(!pass_owed(&l, now), "a symlink is never followed");
            promise_pass(&l, me, SEED_PROMISE_SECS);
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                honest,
                "nor written through"
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A promise records its owner's start time, and a process that took over the owner's
    /// pid — another start time — keeps nothing: the stub stops waiting for a pass that is
    /// not coming. A promise with no start time (an older writer's) is read by the pid alone.
    #[test]
    fn a_promise_ends_when_its_owners_pid_is_another_process() {
        let l = layout("promise-born");
        let now = unix_now();
        let me = std::process::id();
        promise_pass(&l, me, SEED_PROMISE_SECS);
        let written: PassPromise =
            aterm_json::from_str(&std::fs::read_to_string(l.pass_owed_file()).unwrap()).unwrap();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(written.born, crate::platform::process_birth(me).at());
            assert!(written.born.is_some(), "{written:?}");
        }
        assert!(pass_owed(&l, now), "its writer's own process");
        if let Some(born) = written.born {
            let reused = PassPromise {
                born: Some(born + 1),
                ..written.clone()
            };
            std::fs::write(l.pass_owed_file(), aterm_json::to_string(&reused).unwrap()).unwrap();
            assert!(!pass_owed(&l, now), "the same pid, another process");
        }
        std::fs::write(
            l.pass_owed_file(),
            format!("{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}", now + 5),
        )
        .unwrap();
        assert!(pass_owed(&l, now), "no start time: the pid alone");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The owner's start time decides only when the reader can read it: another start time,
    /// or nothing under the pid, breaks the promise; a start time the kernel will not give
    /// this reader (a sandbox denying process-info) leaves the pid to answer, as a promise
    /// recorded without one does.
    #[test]
    fn an_owner_start_time_the_reader_cannot_read_leaves_the_pid_to_answer() {
        use crate::platform::Birth;
        assert!(same_owner(Some(7), Birth::At(7)), "the owner it recorded");
        assert!(!same_owner(Some(7), Birth::At(8)), "another process");
        assert!(!same_owner(Some(7), Birth::Gone), "none: a zombie");
        assert!(
            same_owner(Some(7), Birth::Unreadable),
            "the reader cannot say"
        );
        for now in [Birth::At(8), Birth::Gone, Birth::Unreadable] {
            assert!(same_owner(None, now), "no start time recorded: {now:?}");
        }
    }

    /// An update ending retires only a promise its own process or its spawner holds: a
    /// promise another window's seed wrote stays for the update that window spawns.
    #[test]
    fn a_promise_is_retired_only_by_its_holders() {
        let l = layout("promise-holders");
        let me = std::process::id();
        promise_pass(&l, me, SEED_PROMISE_SECS);
        retire_promise_of(&l, &[me.wrapping_add(1)]);
        assert!(
            pass_owed(&l, unix_now()),
            "another holder's ending leaves it"
        );
        retire_promise_of(&l, &[me.wrapping_add(1), me]);
        assert!(
            !l.pass_owed_file().exists(),
            "its holder's ending retires it"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn a_promise_does_not_publish_a_preexisting_fifo_temp() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

        let l = layout("promise-fifo-temp");
        promise_pass(&l, std::process::id(), SEED_PROMISE_SECS);
        let path = l.pass_owed_file();
        let before = std::fs::read(&path).unwrap();
        let tmp = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
        let fifo = std::ffi::CString::new(tmp.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is NUL-terminated and belongs to this private fixture.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        // Keep both ends open so even the defective writer returns without waiting.
        let _fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&tmp)
            .unwrap();

        promise_pass(&l, std::process::id(), PASS_PROMISE_SECS);

        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_file(),
            "a colliding FIFO must not replace the regular promise"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            pass_owed(&l, unix_now()),
            "the original promise remains live"
        );
        assert!(
            std::fs::symlink_metadata(&tmp)
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_promise_does_not_truncate_a_preexisting_hardlinked_temp() {
        use std::os::unix::fs::PermissionsExt as _;

        let l = layout("promise-hardlink-temp");
        promise_pass(&l, std::process::id(), SEED_PROMISE_SECS);
        let path = l.pass_owed_file();
        let before = std::fs::read(&path).unwrap();
        let victim = l.prefix.join("other-metadata");
        std::fs::write(&victim, b"preserve this metadata").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let tmp = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
        std::fs::hard_link(&victim, &tmp).unwrap();

        promise_pass(&l, std::process::id(), PASS_PROMISE_SECS);

        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve this metadata");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(tmp.exists(), "an unowned colliding entry is left untouched");

        // With no collision, an ordinary refresh still replaces the promise.
        std::fs::remove_file(&tmp).unwrap();
        promise_pass(&l, std::process::id(), PASS_PROMISE_SECS);
        let refreshed: PassPromise =
            aterm_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            refreshed.until_unix
                > aterm_json::from_slice::<PassPromise>(&before)
                    .unwrap()
                    .until_unix
        );
        assert!(pass_owed(&l, unix_now()));
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
