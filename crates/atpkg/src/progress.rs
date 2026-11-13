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
    /// Bytes moving from the network into a staged partial: the release host's `.part` or a LAN peer's `.peer.part`.
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
    /// Bytes landed in the current metered phase (download: the size on disk of the partial [`watch_download`] shows; extract: bytes written).
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
    /// Which pass is writing: `"net"` (the default-set pass), or a pass that plans no default set, `"update"` (a person's channel apply) or `"install"` (one program, a person's or a worker's launcher's).
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
    static CLOCK_OVERRIDE: std::cell::RefCell<Option<Arc<ManualPassClock>>> =
        const { std::cell::RefCell::new(None) };
    static WRITES_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// TESTS ONLY: while held, this thread's snapshot writes fail before a temporary is made
/// ([`write_now`]), as a full or read-only volume fails them.
#[cfg(test)]
pub(crate) struct FailWrites;

#[cfg(test)]
impl FailWrites {
    pub(crate) fn hold() -> Self {
        WRITES_FAIL.with(|fail| fail.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for FailWrites {
    fn drop(&mut self) {
        WRITES_FAIL.with(|fail| fail.set(false));
    }
}

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

    /// Download meter: `done` of `total` bytes landed in a staged partial ([`watch_download`]).
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
    #[cfg(test)]
    if WRITES_FAIL.with(std::cell::Cell::get) {
        return false;
    }
    let Ok((tmp, mut file)) = crate::platform::create_temp_beside(&s.path, 0o644) else {
        return false;
    };
    use std::io::Write as _;
    let wrote = file.write_all(text.as_bytes());
    drop(file);
    if wrote.is_ok() && std::fs::rename(&tmp, &s.path).is_ok() {
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

struct PassHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

const HEARTBEAT_TICK_MS: u64 = 500;

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
    // SAFETY: `poll` reads and writes only the array handed to it, for its stated length, and a
    // zero timeout never blocks.
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(log)
    else {
        return;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return;
    }
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

static PROCESS_PASS: PassSlot = PassSlot::new();

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
    static LENT: std::cell::RefCell<Option<ProgressSink>> = const { std::cell::RefCell::new(None) };
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

const PART_POLL_TICK_MS: u64 = 100;

/// How long a partial may stand still and still be what a download's meter shows: longer than a
/// transfer that still runs goes without a byte (curl's patient `--speed-time`, 120 s; a LAN peer's
/// 2 s), and far short of what a reader waits on the meter for (aterm-gui's
/// `harness_host::INSTALL_QUIET_LIMIT`).
const PARTIAL_QUIET: Duration = Duration::from_secs(150);

/// One partial's size at the last look, and since when it has had that size.
#[derive(Clone, Copy)]
struct Partial {
    len: u64,
    since: Instant,
}

/// What a download's meter shows of the two partials its bytes land in at once
/// ([`crate::peers::race`]). Both hold the same signed bytes at the same offsets, so the furthest
/// says what has landed. A transfer that failed can leave its partial behind, though (the release
/// host's, for the next call to resume), with a live one still short of it, so the meter leaves a
/// partial that has gone quiet for one that moves: whatever transfer still runs moves the meter
/// within [`PARTIAL_QUIET`].
struct Partials([Partial; 2]);

impl Partials {
    const fn new(now: Instant) -> Self {
        Self([Partial { len: 0, since: now }; 2])
    }

    /// What to show at `now` for each partial's size `lens`: the furthest of those that moved
    /// within [`PARTIAL_QUIET`], or the furthest of all where neither did.
    fn shown(&mut self, lens: [u64; 2], now: Instant) -> u64 {
        for (partial, len) in self.0.iter_mut().zip(lens) {
            if partial.len != len {
                *partial = Partial { len, since: now };
            }
        }
        let moving = self
            .0
            .iter()
            .filter(|partial| now.saturating_duration_since(partial.since) < PARTIAL_QUIET)
            .map(|partial| partial.len)
            .max();
        moving.unwrap_or_else(|| lens.into_iter().max().unwrap_or(0))
    }
}

/// `path`'s size where it is a regular file, else 0: never follow a symlink out of the prefix.
fn regular_len(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .map_or(0, |meta| meta.len())
}

/// Flow hook: watch `asset_path`'s growing partials while a download runs, the release host's `<asset>.part` and a LAN peer's ([`crate::peers::arrival_paths`], [`Partials`]). A transfer's bytes are what moves `progress.json` for a reader judging whether the pass still works, so neither may land unseen.
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
    let [part, peer_part, _] = crate::peers::arrival_paths(asset_path);
    let program = program.to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let tick = pass_tick(PART_POLL_TICK_MS);
    let handle = std::thread::Builder::new()
        .name("atpkg-part-poll".into())
        .spawn(move || {
            let mut partials = Partials::new(Instant::now());
            while !stop2.load(Ordering::Acquire) {
                let lens = [regular_len(&part), regular_len(&peer_part)];
                let done = partials.shown(lens, Instant::now());
                sink.download_bytes(&program, done, total);
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
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bump is not a regular file",
        ));
    }
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
    let _ = crate::metadata_io::write_private_atomic(&path, text.as_bytes());
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
#[path = "progress_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "progress_plan_publication_tests.rs"]
mod plan_publication_tests;

#[cfg(test)]
#[path = "progress_writer_tests.rs"]
mod writer_tests;
