// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bounded polling helpers and hang detectors for tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default hang-detector timeout for conditions that must hold.
pub const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// Lifetime in seconds for a parked test stand-in (`2 * MUST_HAPPEN`).
pub const PARKED_SECS: u64 = 2 * MUST_HAPPEN.as_secs();

/// The gap [`until_within`] leaves between observations. Not a synchronisation
/// primitive: the observation is the assertion, and this only keeps the poll
/// from spinning a core.
const UNTIL_GAP: Duration = Duration::from_millis(10);

/// Polls `probe` every `interval` until it returns `Some` or `budget` elapses.
pub fn within<T>(
    budget: Duration,
    gap: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now().checked_add(budget);
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        let now = Instant::now();
        if let Some(deadline) = deadline {
            if now >= deadline {
                return None;
            }
            if gap.is_zero() {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a zero-gap poll a test asked for: its budget is the hang detector, and no deadline waits on it"
                )]
                std::thread::yield_now();
            } else {
                std::thread::sleep(gap.min(deadline.saturating_duration_since(now)));
            }
        } else if gap.is_zero() {
            #[expect(
                clippy::disallowed_methods,
                reason = "a zero-gap poll a test asked for: its budget is the hang detector, and no deadline waits on it"
            )]
            std::thread::yield_now();
        } else {
            std::thread::sleep(gap);
        }
    }
}

/// Polls `probe` every 10 ms until it returns `Some`, or panics after `budget`.
///
/// # Panics
/// Panics if `budget` elapses before `probe` returns `Some`, naming `what`.
#[track_caller]
pub fn until_within<T>(budget: Duration, what: &str, probe: impl FnMut() -> Option<T>) -> T {
    match within(budget, UNTIL_GAP, probe) {
        Some(value) => value,
        None => panic!("timed out after {budget:?} waiting for: {what}"),
    }
}

/// How a wait on a working process ended without its answer ([`within_while_working`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// The process and its descendants used no CPU and did no I/O for a whole window: a hang.
    Stalled,
    /// The process is gone, and the probe still had no answer once it was.
    Ended,
    /// The process kept working for the whole guard and the probe never answered.
    Guard,
}

/// How often [`within_while_working`] reads the process tree: a read of each of its processes (a
/// whole-table sweep off macOS, [`aterm_sysprobe::procs::process_tree`]), so not once a poll.
const WORK_TICK: Duration = Duration::from_millis(500);

/// Polls `probe` every `gap` until it returns `Some`, for as long as process `root` works:
/// a wait for something a process must do, held to that process's progress and not to the
/// wall clock, which a loaded machine stretches without bound. It ends unanswered only
/// when `root` and its descendants used no CPU and did no I/O for one whole `window`
/// ([`aterm_uds::deadline::ProcessProgressProbe`]: a starved process still advances its CPU
/// time within any window it is scheduled in, a hung one does not), when `root` is gone and
/// one more probe had no answer, or when `guard` passed in all (a process that works forever
/// without the answer). A tree that does not read, or whose members changed since the last
/// reading, is no evidence of a stall: the window opens again at the next comparable reading.
///
/// # Errors
/// [`Unanswered`], naming which of the three ended the wait.
pub fn within_while_working<T>(
    root: u32,
    window: Duration,
    guard: Duration,
    gap: Duration,
    probe: impl FnMut() -> Option<T>,
) -> Result<T, Unanswered> {
    use aterm_uds::process::ProcessPresence;
    use aterm_uds::runs::Kernel as _;
    let kernel = aterm_uds::runs::LiveKernel;
    let pid = i32::try_from(root).unwrap_or(0);
    let birth = kernel.birth(pid);
    let mut tree = aterm_uds::deadline::ProcessProgressProbe::default();
    working(
        window,
        guard,
        gap,
        &mut Instant::now,
        // Gone where the kernel says so, or where its pid now names a process born at another
        // time; a reading that fails is no evidence that it ended. One whose exit has begun and
        // not finished (XNU reads it exited from the start) is not gone yet: what its end does,
        // its reaping and its closed descriptors, is not all there for the one more probe.
        &mut || match kernel.presence(pid) {
            ProcessPresence::Exited => aterm_uds::process::exit_unfinished(pid),
            ProcessPresence::Absent => false,
            ProcessPresence::Present | ProcessPresence::Unknown => {
                birth.is_none() || kernel.birth(pid).is_none_or(|now| Some(now) == birth)
            }
        },
        &mut || tree.read(root),
        &mut |gap| std::thread::sleep(gap),
        probe,
    )
}

/// Polls `probe` every 10 ms until it returns `Some`, for as long as the processes this one
/// started work: [`until_while_working`] for a wait on what this process's own children do (a
/// build it runs, a helper it drives), read as its descendants' CPU and I/O without its own
/// ([`aterm_uds::deadline::ProcessProgressProbe::read_descendants`]: the poll here is work
/// too, and would hide a hang). Every descendant counts, so in a test binary that runs tests
/// side by side another test's working child holds the window open; only the guard ends that.
///
/// # Panics
/// Panics, naming `what` and why the wait ended, when it ends unanswered.
#[track_caller]
pub fn until_while_descendants_work<T>(what: &str, probe: impl FnMut() -> Option<T>) -> T {
    let mut tree = aterm_uds::deadline::ProcessProgressProbe::default();
    let own = std::process::id();
    let got = working(
        MUST_HAPPEN,
        WORKING_GUARD,
        UNTIL_GAP,
        &mut Instant::now,
        &mut || true,
        &mut || tree.read_descendants(own),
        &mut |gap| std::thread::sleep(gap),
        probe,
    );
    match got {
        Ok(value) => value,
        Err(Unanswered::Stalled | Unanswered::Ended) => panic!(
            "what this process started made no CPU or I/O progress for {MUST_HAPPEN:?} while \
             waiting for: {what}"
        ),
        Err(Unanswered::Guard) => {
            panic!("what this process started kept working for {WORKING_GUARD:?} without: {what}")
        }
    }
}

/// [`within_while_working`] over its seams: the clock, whether the process still runs, one
/// reading of its tree's progress generation, and the pause between polls.
#[expect(
    clippy::too_many_arguments,
    reason = "the wait's four seams beside its three bounds and its probe; a struct would only rename them"
)]
fn working<T>(
    window: Duration,
    guard: Duration,
    gap: Duration,
    now: &mut dyn FnMut() -> Instant,
    runs: &mut dyn FnMut() -> bool,
    read: &mut dyn FnMut() -> Option<u64>,
    pause: &mut dyn FnMut(Duration),
    mut probe: impl FnMut() -> Option<T>,
) -> Result<T, Unanswered> {
    let began = now();
    let tick = WORK_TICK.min(window / 4).max(Duration::from_millis(1));
    // The window's start: the last reading at which the tree had advanced or did not compare.
    let mut worked_at = began;
    let mut seen = None;
    let mut read_at: Option<Instant> = None;
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        let at = now();
        if at.saturating_duration_since(began) >= guard {
            return Err(Unanswered::Guard);
        }
        if read_at.is_none_or(|last| at.saturating_duration_since(last) >= tick) {
            read_at = Some(at);
            if !runs() {
                // What it did as it ended may be the answer.
                return probe().ok_or(Unanswered::Ended);
            }
            match read() {
                Some(generation) if seen == Some(generation) => {}
                reading => {
                    seen = reading;
                    worked_at = at;
                }
            }
            if at.saturating_duration_since(worked_at) >= window {
                return Err(Unanswered::Stalled);
            }
        }
        pause(gap.max(Duration::from_millis(1)));
    }
}

/// The guard of [`until_while_working`]: how long a process that keeps working may go without
/// the answer. Not a latency budget: a stalled process is named after one [`MUST_HAPPEN`].
pub const WORKING_GUARD: Duration = Duration::from_secs(15 * 60);

/// Polls `probe` every 10 ms until it returns `Some`, for as long as process `root` works
/// ([`within_while_working`], its window [`MUST_HAPPEN`] and its guard [`WORKING_GUARD`]).
///
/// # Panics
/// Panics, naming `what` and why the wait ended, when it ends unanswered.
#[track_caller]
pub fn until_while_working<T>(root: u32, what: &str, probe: impl FnMut() -> Option<T>) -> T {
    match within_while_working(root, MUST_HAPPEN, WORKING_GUARD, UNTIL_GAP, probe) {
        Ok(value) => value,
        Err(Unanswered::Stalled) => panic!(
            "process {root} and its descendants made no CPU or I/O progress for \
             {MUST_HAPPEN:?} while waiting for: {what}"
        ),
        Err(Unanswered::Ended) => panic!("process {root} ended while waiting for: {what}"),
        Err(Unanswered::Guard) => {
            panic!("process {root} kept working for {WORKING_GUARD:?} without: {what}")
        }
    }
}

/// Waits within [`MUST_HAPPEN`] for `stream`'s peer to close both halves
/// ([`aterm_uds::hangup::peer_closed`]) once a test dropped the peer's end. The socket closes
/// with the last descriptor of that end, and a child spawned while the end could still be
/// copied holds one until its `exec` or its exit lets go: one created and marked apart rather
/// than through [`aterm_uds::kept_out`], or forked off Apple, where no mark keeps a descriptor
/// out of a child before its `exec`.
///
/// # Panics
/// Panics if the peer has not closed within [`MUST_HAPPEN`].
#[track_caller]
pub fn until_peer_closed(stream: &aterm_uds::CtlStream) {
    until_within(MUST_HAPPEN, "the peer's last close", || {
        aterm_uds::hangup::peer_closed(stream).then_some(())
    });
}

/// Retries `spawn` while the kernel returns `ETXTBSY`, up to [`MUST_HAPPEN`].
///
/// # Errors
/// Returns the last error produced by `spawn`.
pub fn past_text_busy<T>(mut spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let deadline = Instant::now().checked_add(MUST_HAPPEN);
    loop {
        match spawn() {
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && deadline.is_none_or(|d| Instant::now() < d) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            done => return done,
        }
    }
}

/// Timeout for [`first_exec`]'s unclocked warm-up run.
pub const FIRST_EXEC: Duration = Duration::from_secs(10 * 60);

/// Runs `path` once outside timed test budgets to warm macOS first-exec policy checks.
pub fn first_exec(path: &Path, args: &[&str]) {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    static RUN: Mutex<BTreeMap<FileId, Arc<OnceLock<()>>>> = Mutex::new(BTreeMap::new());

    let Some(id) = file_id(path) else {
        return;
    };
    let once = Arc::clone(
        RUN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_default(),
    );
    once.get_or_init(|| run_once(path, args, FIRST_EXEC));
}

/// What [`first_exec`] keys a file by: the identity macOS caches its verdict
/// under.
#[cfg(unix)]
type FileId = (u64, u64);
#[cfg(not(unix))]
type FileId = (PathBuf, u64, Option<std::time::SystemTime>);

/// The file at `path` as [`first_exec`] keys it; `None` when there is no file.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(path: &Path) -> Option<FileId> {
    let meta = std::fs::metadata(path).ok()?;
    let at = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    meta.is_file()
        .then(|| (at, meta.len(), meta.modified().ok()))
}

/// [`first_exec`]'s one run: `path` with `args` in a scratch environment,
/// killed once `bound` has passed, and one line on stderr when macOS's
/// program check still held it then.
#[cfg_attr(trust_verify, trust::skip)]
fn run_once(path: &Path, args: &[&str], bound: Duration) {
    run_once_judged(
        path,
        args,
        bound,
        &|pid| aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0)),
        &mut |line| eprintln!("{line}"),
    );
}

/// [`run_once`], with `held` asked whether the run is held at macOS's
/// program check before it is killed at `bound`, and `say` given the one
/// line that says so. It never panics: a program held before its `main`
/// cannot be the code's hang.
#[cfg_attr(trust_verify, trust::skip)]
fn run_once_judged(
    path: &Path,
    args: &[&str],
    bound: Duration,
    held: &dyn Fn(u32) -> Option<bool>,
    say: &mut dyn FnMut(String),
) {
    use aterm_uds::spawnfd::Stdio;
    let scratch = scratch_dir();
    let home = scratch.clone().unwrap_or_else(std::env::temp_dir);
    let mut cmd = aterm_uds::spawnfd::command(path);
    cmd.args(args).env_clear();
    if let Some(search) = std::env::var_os("PATH") {
        cmd.env("PATH", search);
    }
    cmd.env("HOME", &home)
        .env("TMPDIR", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        let ended = within(bound, UNTIL_GAP, || match child.try_wait() {
            Ok(None) => None,
            Ok(Some(_)) | Err(_) => Some(()),
        });
        if ended.is_none() {
            let pid = child.id();
            let was_held = held(pid) == Some(true);
            aterm_uds::deadline::kill_within(&mut child, MUST_HAPPEN);
            if was_held {
                say(format!(
                    "first_exec: macOS's program check held {} (pid {pid}) for the whole \
                     {bound:?} warm-up; it was ended unrun",
                    path.display()
                ));
            }
        }
    }
    if let Some(dir) = scratch {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A new directory under the temp dir for one [`first_exec`] run, or `None`
/// when it cannot be made.
fn scratch_dir() -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-first-exec-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).ok().map(|()| dir)
}

#[cfg(all(test, unix))]
#[path = "wait_tests.rs"]
mod tests;
