// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The live install-progress protocol: `progress.json` v1 + the `bump` file, and the promise of a pass that has not begun yet (`pass-owed.json`, [`pass_owed`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The schema version writers stamp.
pub const PROGRESS_VERSION: u32 = 1;

/// Read cap for `progress.json`; a larger file is treated as absent.
pub const PROGRESS_READ_CAP: u64 = 256 * 1024;

/// Read cap for the `bump` file: one program name per line; a file past this is treated as absent.
pub const BUMP_READ_CAP: u64 = 4 * 1024;

/// A heartbeat older than this many seconds means "installer not running", whatever the snapshot claims.
pub const HEARTBEAT_STALE_SECS: u64 = 10;

/// Writer rate cap: write only on change and at least this many milliseconds after the last write (≤ 10 Hz), so a fast extract loop never becomes an fsync storm.
pub const WRITE_MIN_INTERVAL_MS: u64 = 100;

/// Where one program is in its install, in pass order, serialized lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Planned, not yet started.
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
    /// This program failed; `ProgramProgress::error` names why.
    Failed,
    /// Nothing to do — already current (zero bytes moved) or excluded.
    Skipped,
}

/// The whole-pass rollup the overall bar renders.
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

/// One program's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramProgress {
    /// Where this program is now.
    pub phase: Phase,
    /// Bytes landed in the current metered phase (download: `.part` size on disk; extract: bytes written).
    #[serde(default)]
    pub bytes_done: u64,
    /// The metered phase's denominator: the signed `artifact.size` for download, the signed uncompressed size for extract.
    #[serde(default)]
    pub bytes_total: u64,
    /// The pinned build number being installed, once resolved from the signed index.
    #[serde(default)]
    pub build: Option<u64>,
    /// Whether a bump-file entry moved this program forward in the queue.
    #[serde(default)]
    pub bumped: bool,
    /// Why `phase` is `failed`.
    #[serde(default)]
    pub error: Option<String>,
}

impl ProgramProgress {
    /// A download whose every byte is in, waiting for the pass to stage it: the set completion fetches archives ahead of the program it is staging.
    #[must_use]
    pub fn downloaded(&self) -> bool {
        self.phase == Phase::Download && self.bytes_total > 0 && self.bytes_done >= self.bytes_total
    }
}

/// The `progress.json` snapshot: everything a reader needs to render the pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressFile {
    /// Schema version ([`PROGRESS_VERSION`]).
    pub v: u32,
    /// The writing installer's pid while the pass is live; `None` once it ended cleanly.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Which pass is writing: `"net"` (the default-set pass), or a typed pass that plans no default set, `"update"` (the channel apply) or `"install"` (one program).
    #[serde(default)]
    pub pass: String,
    /// Unix seconds when the pass started.
    #[serde(default)]
    pub started_unix: u64,
    /// An opaque per-pass identity, minted once and retained in the terminal snapshot.
    #[serde(default)]
    pub generation: Option<String>,
    /// Unix seconds of the last write, the staleness input for [`HEARTBEAT_STALE_SECS`].
    #[serde(default)]
    pub heartbeat_unix: u64,
    /// The whole-pass rollup.
    #[serde(default)]
    pub overall: Overall,
    /// Remaining install order, front first.
    #[serde(default)]
    pub queue: Vec<String>,
    /// Per-program rows, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramProgress>,
    /// Unix seconds when the pass ended cleanly, written with `pid: None`.
    #[serde(default)]
    pub ended_unix: Option<u64>,
}

/// Current unix seconds, saturating at 0 for a clock before the epoch (backward skew
/// then reads as epoch-stale — the safe, "not running" direction).
fn unix_now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// The clocks a pass's writer reads: the monotonic one its write policy ages the last
/// write by ([`maybe_write`]), and the unix seconds every write stamps.
#[derive(Clone)]
enum PassClock {
    /// [`Instant::now`] and [`unix_now`].
    System,
    /// TESTS ONLY: a clock that moves when its test moves it.
    #[cfg(test)]
    Manual(Arc<ManualPassClock>),
}

impl PassClock {
    /// The clock a sink opened NOW runs on: the system's, unless a test set
    /// `CLOCK_OVERRIDE` on this thread.
    fn for_new_sink() -> Self {
        #[cfg(test)]
        {
            if let Some(clock) = CLOCK_OVERRIDE.with(|clock| clock.borrow().clone()) {
                return Self::Manual(clock);
            }
        }
        Self::System
    }

    fn now(&self) -> Instant {
        match self {
            Self::System => Instant::now(),
            #[cfg(test)]
            Self::Manual(clock) => clock.now(),
        }
    }

    fn unix_now(&self) -> u64 {
        match self {
            Self::System => unix_now(),
            #[cfg(test)]
            Self::Manual(clock) => clock.unix_now(),
        }
    }
}

/// TESTS ONLY: a pass clock standing at the moment it was made until [`Self::advance`] moves it — both its instant and its unix seconds, together.
#[cfg(test)]
pub(crate) struct ManualPassClock {
    origin: Instant,
    origin_unix: u64,
    moved: Mutex<Duration>,
}

#[cfg(test)]
impl ManualPassClock {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            origin_unix: unix_now(),
            moved: Mutex::new(Duration::ZERO),
        })
    }

    fn moved(&self) -> Duration {
        *self
            .moved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn advance(&self, by: Duration) {
        *self
            .moved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += by;
    }

    fn now(&self) -> Instant {
        self.origin + self.moved()
    }

    pub(crate) fn unix_now(&self) -> u64 {
        self.origin_unix + self.moved().as_secs()
    }

    /// Every pass this thread opens from now on runs on this clock, until the guard drops.
    pub(crate) fn drive_passes(self: &Arc<Self>) -> ClockOverride {
        ClockOverride(CLOCK_OVERRIDE.with(|clock| clock.replace(Some(Arc::clone(self)))))
    }
}

/// TESTS ONLY: restores the thread's previous pass clock on drop ([`ManualPassClock::drive_passes`]).
#[cfg(test)]
pub(crate) struct ClockOverride(Option<Arc<ManualPassClock>>);

#[cfg(test)]
impl Drop for ClockOverride {
    fn drop(&mut self) {
        let previous = self.0.take();
        CLOCK_OVERRIDE.with(|clock| *clock.borrow_mut() = previous);
    }
}

#[cfg(test)]
thread_local! {
    /// TESTS ONLY: the clock [`PassClock::for_new_sink`] hands the sinks THIS thread
    /// opens, read on it and moved into the sink, so no other test's passes see it.
    static CLOCK_OVERRIDE: std::cell::RefCell<Option<Arc<ManualPassClock>>> =
        const { std::cell::RefCell::new(None) };
}

/// The writer's mutable state, behind the sink's one lock.
struct SinkState {
    /// Where the snapshot lands. The GUI passes `<prefix>/progress.json`.
    path: PathBuf,
    /// What the write policy ages writes by and what each write stamps.
    clock: PassClock,
    /// The snapshot being built — serialized verbatim on every write.
    file: ProgressFile,
    /// Per-program download credit `(done, total)` for the overall rollup, kept apart from
    /// the wire rows: a row's `bytes_done/bytes_total` switch meaning per phase, while the
    /// overall denominator stays the signed asset sums the plan fixed.
    dl: BTreeMap<String, (u64, u64)>,
    /// When the last snapshot hit disk — the ≤10 Hz rate cap's clock.
    last_write: Option<Instant>,
    /// Whether the snapshot changed since the last write.
    dirty: bool,
    /// Set when the destination is not a regular file (a symlink or FIFO planted at the
    /// path): the sink goes silent rather than follow it.
    disabled: bool,
}

/// The live-progress writer: one per pass, held by the process with the store flock.
#[derive(Clone)]
pub struct ProgressSink {
    inner: Arc<Mutex<SinkState>>,
}

impl ProgressSink {
    /// The pass as it stands now (the snapshot the next write would land, its download credit summed as [`write_now`] sums it), for the terminal meter ([`crate::meter`]), which must not wait out the write cap or read the file back.
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

/// The writer must refresh `heartbeat_unix` at least this often, even with nothing to report — a reader's staleness window ([`HEARTBEAT_STALE_SECS`]) is only meaningful against a writer that keeps this promise.
pub const HEARTBEAT_MIN_INTERVAL_SECS: u64 = 2;

impl ProgressSink {
    /// Open a sink writing to `path` for the named pass, truncating any previous snapshot.
    #[must_use]
    pub fn create(path: &Path, pass: &str) -> Option<Self> {
        let clock = PassClock::for_new_sink();
        let now = clock.unix_now();
        let state = SinkState {
            path: path.to_path_buf(),
            clock,
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
        if guard.disabled || guard.last_write.is_none() {
            return None;
        }
        drop(guard);
        Some(sink)
    }

    /// Fix the pass's plan: the queue order and each planned program's signed asset size.
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

    /// Transfer a default-set pass's promise to its live planned rows.
    pub(crate) fn plan_promised_pass(
        &self,
        layout: &crate::store::Layout,
        planned: &[(String, u64)],
    ) {
        if self.plan(planned) {
            retire_promise(layout);
        }
    }

    /// Move `program` to `phase`, resetting the row's byte meter so a download count never renders under an extract label.
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

    /// Download meter: `done` of `total` bytes landed in the staged `.part`.
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

    /// Size `program`'s extract meter (denominator: the signed `disk_installed` size, 0 ⇒ unmetered) as its extract begins ([`extract_scope`]).
    pub fn extract_begin(&self, program: &str, total: u64) {
        self.with(|s| {
            if let Some(row) = s.file.programs.get_mut(program) {
                row.bytes_total = total;
            }
        });
    }

    /// Credit `n` freshly-extracted bytes to `program`, flipping its row to [`Phase::Extract`] on the first tick (the verify→extract boundary lives inside `verify_and_stage`, which this module deliberately does not modify — the first written byte IS that boundary).
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

    /// Mark `program` bumped (a user asked for it via the bump file) and re-order the visible queue to match the installer's re-sorted remainder.
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

    /// Replace the remaining-order queue (front first).
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

    /// A program reached a terminal phase: `Done`, `Failed` (with its error) or `Skipped`.
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

    /// The clean-pass terminal snapshot: `pid` cleared, `ended_unix` stamped, written unconditionally, so a reader can tell an ended pass from a dead writer.
    pub fn finish(&self) {
        if let Ok(mut s) = self.inner.lock() {
            s.file.pid = None;
            s.file.ended_unix = Some(s.clock.unix_now());
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

    /// A content-free tick: lets the write policy refresh `heartbeat_unix` if it is due.
    pub fn heartbeat(&self) {
        self.with(|_| {});
    }
}

/// The write policy: on change and ≥ [`WRITE_MIN_INTERVAL_MS`] since the last write, plus
/// an unconditional heartbeat refresh every [`HEARTBEAT_MIN_INTERVAL_SECS`] so a quiet
/// transfer never lets a live writer's file go stale.
fn maybe_write(s: &mut SinkState) {
    if s.disabled {
        return;
    }
    let age = s.last_write.map_or(Duration::MAX, |t| {
        s.clock.now().saturating_duration_since(t)
    });
    let change_due = s.dirty && age >= Duration::from_millis(WRITE_MIN_INTERVAL_MS);
    let heartbeat_due = age >= Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS);
    if change_due || heartbeat_due {
        write_now(s);
    }
}

/// Serialize and land the snapshot atomically (temp + rename), refreshing the heartbeat
/// and the overall download rollup.
fn write_now(s: &mut SinkState) -> bool {
    if s.disabled {
        return false;
    }
    let mut done = 0u64;
    for (d, _) in s.dl.values() {
        done = done.saturating_add(*d);
    }
    s.file.overall.bytes_done = done;
    s.file.heartbeat_unix = s.clock.unix_now();
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
        s.last_write = Some(s.clock.now());
        s.dirty = false;
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// The wait both pass-owned threads use between ticks: hold for `tick`, but return as soon as [`stop_and_join`] raises `stop`.
pub(crate) fn tick_or_stop(stop: &AtomicBool, tick: Duration) {
    let deadline = Instant::now() + tick;
    while !stop.load(Ordering::Acquire) {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return;
        };
        std::thread::park_timeout(left);
    }
}

/// Stop a thread parked in [`tick_or_stop`] and join it: raise the flag, unpark, then join.
pub(crate) fn stop_and_join(stop: &AtomicBool, handle: Option<std::thread::JoinHandle<()>>) {
    stop.store(true, Ordering::Release);
    if let Some(h) = handle {
        h.thread().unpark();
        let _ = h.join();
    }
}

/// The pass heartbeat: a thread that ticks the live sink every [`HEARTBEAT_TICK_MS`] for the
/// whole pass, independent of flow calls, so a phase with no sink calls (a multi-GB Verify,
/// an Extract) never ages the file past [`HEARTBEAT_STALE_SECS`].
struct PassHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// How often the pass heartbeat ticks: well under [`HEARTBEAT_MIN_INTERVAL_SECS`] (the write
/// policy decides whether a tick lands) and the readers' [`HEARTBEAT_STALE_SECS`].
const HEARTBEAT_TICK_MS: u64 = 500;

/// The tick a pass-owned thread spawned now parks for: `ms`, unless a test set
/// `TICK_OVERRIDE` on the spawning thread (read there and moved into the new thread, so no
/// other test's threads see it).
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
    /// Tests only: the tick [`pass_tick`] hands the pass-owned threads this thread spawns.
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

/// Keep an orphaned pass working.
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

/// Whether fd 1 or fd 2 is a pipe with no reader, which a parent that `exec`s in place
/// produces without changing its pid.
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
    // SAFETY: `poll` reads and writes only the array handed to it, for its stated length, and a zero timeout never blocks.
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

/// How often the orphan watch polls: each poll is two cheap syscalls, so the residual
/// EPIPE window, not the cost, sets it.
const ORPHAN_POLL_MS: u64 = 100;

/// Point stdout and stderr at `log` (appended, `0600`) and say so there. A log that cannot
/// be opened leaves the fds alone.
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
    // SAFETY: `dup2` re-points fds 1 and 2 at an open file this process owns; `file` is
    // leaked right after, so its descriptor stays valid for the rest of the process. No
    // other thread relies on what fds 1 and 2 are.
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

/// A process's live pass: its sink, the thread that owns it and its heartbeat — at most
/// one at a time ([`begin_pass`]) — and whether one is live, the one atomic load the
/// flow hooks' idle path costs.
struct PassSlot {
    live: Mutex<Option<(ProgressSink, std::thread::ThreadId, PassHeartbeat)>>,
    enabled: AtomicBool,
}

impl PassSlot {
    const fn new() -> Self {
        Self {
            live: Mutex::new(None),
            enabled: AtomicBool::new(false),
        }
    }
}

/// THE process's pass slot.
static PROCESS_PASS: PassSlot = PassSlot::new();

/// The pass slot this thread's calls answer to: [`PROCESS_PASS`], in every build. A test
/// thread may hold one of its own (`own_pass_slot`), and lends it with its pass
/// ([`lend`]), so tests that begin passes run side by side, each its own process's one
/// pass, and none sees another's — the production rule, one slot per process, unbent.
fn slot() -> &'static PassSlot {
    #[cfg(test)]
    {
        if let Some(own) = TEST_SLOT.get() {
            return own;
        }
    }
    &PROCESS_PASS
}

#[cfg(test)]
thread_local! {
    /// TESTS ONLY: the pass slot [`slot`] answers on this thread, when not the process's.
    static TEST_SLOT: std::cell::Cell<Option<&'static PassSlot>> =
        const { std::cell::Cell::new(None) };
}

/// TESTS ONLY: give this thread a pass slot of its own until the guard drops — a test that begins a pass sees no other test's, and none sees its.
#[cfg(test)]
pub(crate) fn own_pass_slot() -> OwnPassSlot {
    let own: &'static PassSlot = Box::leak(Box::new(PassSlot::new()));
    OwnPassSlot {
        own,
        previous: TEST_SLOT.replace(Some(own)),
    }
}

/// TESTS ONLY: the guard [`own_pass_slot`] returns.
#[cfg(test)]
pub(crate) struct OwnPassSlot {
    own: &'static PassSlot,
    previous: Option<&'static PassSlot>,
}

#[cfg(test)]
impl Drop for OwnPassSlot {
    fn drop(&mut self) {
        TEST_SLOT.set(self.previous);
        let left = self
            .own
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.own.enabled.store(false, Ordering::Release);
        if let Some((_, _, heartbeat)) = left {
            heartbeat.stop();
        }
    }
}

/// Install the process-global sink for one pass, owned by the calling thread.
pub fn begin_pass(path: &Path, pass: &str) -> bool {
    let slot = slot();
    let Ok(mut g) = slot.live.lock() else {
        return false;
    };
    if g.is_some() {
        return false;
    }
    let Some(sink) = ProgressSink::create(path, pass) else {
        return false;
    };
    let heartbeat = PassHeartbeat::spawn(sink.clone());
    *g = Some((sink, std::thread::current().id(), heartbeat));
    slot.enabled.store(true, Ordering::Release);
    true
}

/// Write the terminal snapshot and retire the global sink.
pub fn end_pass() {
    let slot = slot();
    let Ok(mut g) = slot.live.lock() else {
        return;
    };
    if g.as_ref()
        .is_some_and(|(_, owner, _)| *owner == std::thread::current().id())
        && let Some((sink, _, heartbeat)) = g.take()
    {
        slot.enabled.store(false, Ordering::Release);
        heartbeat.stop();
        sink.finish();
    }
}

/// The live pass's sink — for the OWNING thread (see [`begin_pass`]), and for a lane thread that owner started and LENT it to ([`lend`]).
#[must_use]
pub fn active() -> Option<ProgressSink> {
    let slot = slot();
    if !slot.enabled.load(Ordering::Acquire) {
        return None;
    }
    slot.live
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

/// The live pass, as its owning thread hands it to a lane thread it starts — one that lands programs beside the owner's own, inside the pass and under its store lock.
#[derive(Clone)]
pub struct Lent {
    sink: Option<ProgressSink>,
    /// TESTS ONLY: the lender's own pass slot (`own_pass_slot`), which the lane answers
    /// to while it holds the pass — [`active`]'s first load is that slot's.
    #[cfg(test)]
    slot: Option<&'static PassSlot>,
}

/// On the pass-owning thread: the live pass, to lend a lane thread ([`Lent::adopt`]).
#[must_use]
pub fn lend() -> Lent {
    Lent {
        sink: active(),
        #[cfg(test)]
        slot: TEST_SLOT.get(),
    }
}

impl Lent {
    /// Credit this thread's flow hooks to the lent pass until the guard drops.
    #[must_use]
    pub fn adopt(self) -> Adopted {
        LENT.with(|lent| *lent.borrow_mut() = self.sink);
        Adopted {
            _adopted: (),
            #[cfg(test)]
            slot_before: TEST_SLOT.replace(self.slot),
        }
    }
}

/// Ends a lane thread's adoption of a lent pass on drop ([`Lent::adopt`]).
pub struct Adopted {
    /// Made only by [`Lent::adopt`].
    _adopted: (),
    /// TESTS ONLY: the pass slot this thread answered to before it adopted the pass.
    #[cfg(test)]
    slot_before: Option<&'static PassSlot>,
}

impl Drop for Adopted {
    fn drop(&mut self) {
        LENT.with(|lent| *lent.borrow_mut() = None);
        #[cfg(test)]
        TEST_SLOT.set(self.slot_before);
    }
}

/// The live pass's snapshot, from any thread: [`active`] answers only the pass's own thread, and the terminal meter ([`crate::meter`]) draws from a thread of its own.
#[must_use]
pub fn live_snapshot() -> Option<ProgressFile> {
    let slot = slot();
    if !slot.enabled.load(Ordering::Acquire) {
        return None;
    }
    let sink = slot
        .live
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

/// Flow hook: `program`'s download of `total` signed bytes is complete, and its row waits for the stage at that whole size ([`ProgramProgress::downloaded`]) — the set completion's download-ahead lane finishes archives the pass reaches later.
pub fn note_downloaded(program: &str, total: u64) {
    if let Some(sink) = active() {
        sink.download_bytes(program, total, total);
    }
}

/// Flow hook: credit `n` extracted bytes to this thread's extract program (the loop itself cannot name it).
pub fn extract_tick(n: u64) {
    if !slot().enabled.load(Ordering::Relaxed) {
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

/// Ends the extract crediting scope on drop (panic-safe, so a failing stage can never leave a later program's extract bytes credited to this one).
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
                // Regular files only: never follow a symlink out of the prefix.
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

/// Read `<prefix>/progress.json` under the untrusted-reader rules: symlink-refusing bounded open ([`crate::metadata_io`]), [`PROGRESS_READ_CAP`], malformed or oversize ⇒ `None`.
#[must_use]
pub fn read_progress(layout: &crate::store::Layout) -> Option<ProgressFile> {
    let cap = usize::try_from(PROGRESS_READ_CAP).unwrap_or(usize::MAX);
    let text = crate::metadata_io::read_bounded_regular_utf8(&layout.progress_file(), cap).ok()?;
    aterm_json::from_str(&text).ok()
}

/// Whether `file` was written by an installer running now: a live pid and a heartbeat within [`HEARTBEAT_STALE_SECS`] of `now_unix` in either direction (a far-future stamp from backward clock skew is stale too).
#[must_use]
pub fn snapshot_running(file: &ProgressFile, now_unix: u64) -> bool {
    let Some(pid) = file.pid else {
        return false;
    };
    let hb = file.heartbeat_unix;
    let fresh = hb <= now_unix.saturating_add(HEARTBEAT_STALE_SECS)
        && now_unix.saturating_sub(hb) <= HEARTBEAT_STALE_SECS;
    fresh && pid_alive(pid)
}

/// Whether a pass is installing on `layout`'s store now ([`snapshot_running`]): what a scheduler reads beside `status.toml` so it never queues a pass behind one in flight.
#[must_use]
pub fn pass_running(layout: &crate::store::Layout, now_unix: u64) -> bool {
    read_progress(layout).is_some_and(|file| snapshot_running(&file, now_unix))
}

/// Shared kernel pid liveness: Unix signal-zero and Windows process-handle probes both distinguish a dead owner from an existing process we may not query.
#[must_use]
pub(crate) fn pid_alive(pid: u32) -> bool {
    aterm_uds::process::pid_alive(pid)
}

/// Strip control characters (terminal escape-sequence injection) and cap length before a progress/status string reaches a TTY.
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
    /// A person at a terminal: the stub's stdin and stderr are both terminals.
    Person,
    /// A script, a pipe or an agent's tool call: the name and [`UNATTENDED_BUMP`] on one line.
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

/// Append `program` to `<prefix>/bump` — the stub's whole write privilege — as `asker`'s line.
pub fn append_bump(
    layout: &crate::store::Layout,
    program: &crate::store::ToolName,
    asker: Asker,
) -> std::io::Result<()> {
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

/// Read the bump file's admitted names, in file order, first mention wins, each with whether a person asked for it on any line: size-capped ([`BUMP_READ_CAP`], oversize ⇒ absent), symlink-refusing, each name gated through [`crate::store::ToolName`].
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

/// The bump file's admitted names, whoever asked ([`read_bumps`]): what reorders a pass's remaining work and wakes a window's pass.
#[must_use]
pub fn read_bump(layout: &crate::store::Layout) -> Vec<String> {
    read_bumps(layout).into_iter().map(|b| b.program).collect()
}

/// Delete the bump file — the clean-pass-end consumption step.
pub fn clear_bump(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(layout.bump_file());
}

/// Read cap for `pass-owed.json`; a larger file reads as no promise.
pub const PROMISE_READ_CAP: usize = 1024;

/// How long the window's seed promises the update it spawns next (the login shell's PATH is that update's only wait, five seconds at most).
pub const SEED_PROMISE_SECS: u64 = 60;

/// How long a whole update promises its own plan: its index resolve, vendor lane and manifest prefetch come first.
pub const PASS_PROMISE_SECS: u64 = 10 * 60;

/// `pass-owed.json`: the process the promise rides on and when it lapses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassPromise {
    /// Schema version ([`PROGRESS_VERSION`]); any other reads as no promise.
    pub v: u32,
    /// The pid whose death breaks the promise: the window that will spawn the pass, or the pass itself.
    pub owner: u32,
    /// Unix seconds after which the promise is broken.
    pub until_unix: u64,
    /// The owner's start time as the writer read it ([`crate::platform::process_birth`]): a process that took over the pid of an owner that died does not keep the promise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub born: Option<u64>,
}

/// Promise a default-set pass on `layout`'s store until `owner` dies or `secs` pass.
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

/// Retire the promise only when one of `holders` owns it: an update ending retires the promise it wrote, or the seed's of the window that spawned it — never one another window's seed wrote for the update that window spawns next.
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

/// Whether a promised pass is still coming at `now_unix`: a readable promise (bounded, symlink-refusing, [`PROMISE_READ_CAP`]) whose deadline has not passed — nor lies further ahead than any writer promises, a clock that stepped back — and whose owner is alive ([`pid_alive`]) and, when the writer read its start time, the same process that wrote it ([`same_owner`]): a pid a crash freed and another process took promises nothing.
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

/// Whether the process under a promise's owner pid, whose start time reads `now`, is the one the writer read as `born`: no recorded start time ⇒ the pid alone answers; another start time, or nothing under the pid (a zombie), ⇒ another process, or none; a start time the reader cannot read — a stub run in a sandbox that denies process-info, where `kill(pid, 0)` still answers ([`pid_alive`]) — says nothing, and the pid alone answers, as it does for `EPERM` there.
#[must_use]
pub const fn same_owner(born: Option<u64>, now: crate::platform::Birth) -> bool {
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

    /// The v1 example snapshot round-trips field for field.
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

    /// A newer writer's extra fields never break an older reader, and a minimal snapshot
    /// degrades to the safe reading (no pid, epoch-stale heartbeat), not a parse failure.
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

    /// The writer too refuses a symlink planted at the progress path: the sink is disabled.
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

    /// A first write that cannot land (the prefix does not exist yet, or is not writable)
    /// yields no sink, so [`begin_pass`] reports no pass rather than an owned one whose file
    /// never appears.
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
        let _slot = own_pass_slot();
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
        file.pid = Some(std::process::id());
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

    /// A pid we may not signal (`EPERM`; pid 1 stands in for another user's installer) still
    /// exists, so its fresh heartbeat reads as running.
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
        assert!(!pid_alive(u32::MAX), "an impossible pid is not alive");
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let dead = child.id();
        child.wait().expect("reap it");
        assert!(!pid_alive(dead), "a reaped pid must not read as alive");
        file.pid = Some(dead);
        assert!(!snapshot_running(&file, 1_000_000));
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
        append_bump(&l, &trust, Asker::Person).unwrap();
        assert_eq!(read_bump(&l), vec!["trust".to_string(), "ty".to_string()]);
        std::fs::write(l.bump_file(), "sudo\n../evil\na/b\ntrust\n\n").unwrap();
        assert_eq!(
            read_bump(&l),
            vec!["trust".to_string()],
            "shim-refused and malformed names never come back"
        );
        std::fs::write(l.bump_file(), "x".repeat(BUMP_READ_CAP as usize + 1)).unwrap();
        assert!(read_bump(&l).is_empty());
        clear_bump(&l);
        assert!(!l.bump_file().exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A script's bump names its program as a person's does, for every reader that orders
    /// work by it, and says nobody at a terminal asked: a person's line for the same name,
    /// before or after it, makes it a person's.
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
    /// terminal snapshot the GUI retires its row on.
    #[test]
    fn stopping_a_pass_poller_does_not_wait_out_its_tick() {
        let l = layout("prompt-stop");
        let progress = l.progress_file();
        let asset = l.prefix.join("trust-1.tar.zst");
        std::fs::write(l.prefix.join("trust-1.tar.zst.part"), [0u8; 7]).unwrap();
        let reported =
            || live_snapshot().and_then(|f| f.programs.get("trust").map(|r| r.bytes_done));
        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        let pass = std::thread::spawn(move || {
            let _slot = own_pass_slot();
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

    /// A pass that makes no sink calls for longer than the writer's minimum interval (a
    /// multi-GB Verify, an Extract) still reads as running: the heartbeat thread ticks the
    /// file on its own.
    #[test]
    fn a_silent_phase_keeps_the_pass_heartbeat_live() {
        let _slot = own_pass_slot();
        let clock = ManualPassClock::new();
        let _clock = clock.drive_passes();
        let l = layout("silent-heartbeat");
        TICK_OVERRIDE.set(Some(Duration::from_millis(1)));
        assert!(begin_pass(&l.progress_file(), "net"));
        TICK_OVERRIDE.set(None);
        let h0 = read_file(&l.progress_file()).heartbeat_unix;
        clock.advance(Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS));
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let file = loop {
            let file = read_file(&l.progress_file());
            if file.heartbeat_unix >= h0 + HEARTBEAT_MIN_INTERVAL_SECS {
                break file;
            }
            assert!(
                Instant::now() < deadline,
                "the pass heartbeat must advance with no flow calls ({h0} → {})",
                file.heartbeat_unix
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(
            snapshot_running(&file, clock.unix_now()),
            "a silent-but-live pass reads as running"
        );
        end_pass();
        let ended = read_file(&l.progress_file());
        assert_eq!(ended.pid, None);
        assert!(!snapshot_running(&ended, clock.unix_now()));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE HEARTBEAT INTERVAL IS THE WRITE POLICY'S, TO THE MILLISECOND: a sink with
    /// nothing new to say rewrites its file once [`HEARTBEAT_MIN_INTERVAL_SECS`] have
    /// passed on its clock since its last write, and a tick a millisecond before writes
    /// nothing — the stamp a reader's staleness window ([`HEARTBEAT_STALE_SECS`]) is
    /// sized against.
    #[test]
    fn a_quiet_sink_refreshes_its_heartbeat_at_the_interval_and_not_before() {
        let clock = ManualPassClock::new();
        let _clock = clock.drive_passes();
        let l = layout("heartbeat-interval");
        let path = l.progress_file();
        let sink = ProgressSink::create(&path, "net").expect("sink creates");
        let h0 = read_file(&path).heartbeat_unix;
        let interval = Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS);
        clock.advance(interval - Duration::from_millis(1));
        sink.heartbeat();
        assert_eq!(
            read_file(&path).heartbeat_unix,
            h0,
            "a tick inside the interval writes nothing"
        );
        clock.advance(Duration::from_millis(1));
        sink.heartbeat();
        assert_eq!(
            read_file(&path).heartbeat_unix,
            h0 + HEARTBEAT_MIN_INTERVAL_SECS,
            "the tick at the interval refreshes the stamp"
        );
        drop(sink);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A LANE THREAD CREDITS THE PASS IT WAS LENT, and no other thread does: `active`
    /// answers the owner and the thread that adopted the pass, never a bystander.
    #[test]
    fn a_lent_pass_credits_its_lane_and_each_lane_extracts_into_its_own_row() {
        let _slot = own_pass_slot();
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
    /// it promises nothing.
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
    /// not coming.
    #[test]
    fn a_promise_ends_when_its_owners_pid_is_another_process() {
        let l = layout("promise-born");
        let now = unix_now();
        let me = std::process::id();
        promise_pass(&l, me, SEED_PROMISE_SECS);
        let written: PassPromise =
            aterm_json::from_str(&std::fs::read_to_string(l.pass_owed_file()).unwrap()).unwrap();
        if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
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

    /// The platform's start-time reader as [`same_owner`] judges it, on every platform that
    /// has one: this process, read twice, is the owner it recorded; a child that has exited
    /// is no owner — reaped on Unix (`ESRCH`; no `/proc/<pid>`), and on Windows still held by
    /// its `Child`'s handle, so the pid names that exited process and no other
    /// (`GetExitCodeProcess` no longer `STILL_ACTIVE`).
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn the_platform_reader_tells_a_live_owner_from_one_that_exited() {
        use crate::platform::{Birth, process_birth};
        let me = std::process::id();
        let born = process_birth(me);
        assert!(
            matches!(born, Birth::At(_)),
            "this process's start time: {born:?}"
        );
        assert!(
            same_owner(born.at(), process_birth(me)),
            "read again: the same owner"
        );
        #[cfg(unix)]
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("spawn cmd /C exit 0");
        let exited = child.id();
        child.wait().expect("it exits");
        assert_eq!(
            process_birth(exited),
            Birth::Gone,
            "an exited child is no owner"
        );
        drop(child);
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
