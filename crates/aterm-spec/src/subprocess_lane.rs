// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-binary and in-process admission for tests that spawn multi-process trees.
//!
//! Live integration suites (`headless_boot`, `fabric_world`, `aos_workers`) spawn a
//! headless `aterm` instance or a multi-worker coordinator per test, each bringing
//! four to six OS processes with it. `libtest` counts only in-process test threads,
//! and `aterm-verify`'s `testrun` pool runs several test binaries side by side, so
//! uncoordinated binaries can stack hundreds of runnable processes on one host and
//! push live waits past their one-minute hang detectors (`ATERM-KI-1103-3`).
//!
//! When `aterm-verify` runs a test stage, it creates a scratch pool directory sized to
//! the stage's granted CPU budget and passes its path in [`SUBPROCESS_SLOTS_ENV`].
//! Every test thread across all concurrent test binaries takes one [`SubprocessLease`]
//! from that shared directory (via [`aterm_uds::file_lock::LockFile`], which keeps the
//! descriptor out of spawned children and unlocks with `LOCK_UN` before closing) and
//! holds it until the spawned process tree is torn down. Outside `aterm-verify`, a
//! process-local fallback pool bounds concurrency from `RUST_TEST_THREADS`,
//! `CARGO_BUILD_JOBS`, or `std::thread::available_parallelism`.
//!
//! Leases are re-entrant per thread via a thread-local [`Weak`] reference: a test that
//! boots two or more instances sequentially on the same thread reuses its active lease
//! without waiting for a second slot (preventing hold-and-wait deadlock even on a
//! single-slot pool), while dropping its last live instance releases the slot
//! immediately without waiting for the `libtest` worker thread to exit.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Environment variable naming the shared subprocess slot directory created by `aterm-verify`.
pub const SUBPROCESS_SLOTS_ENV: &str = "ATERM_TEST_SUBPROCESS_SLOTS";

/// File inside the pool directory recording the number of subprocess slots.
pub const COUNT_FILE: &str = "count";

/// File inside the pool directory recording the number of concurrent boot permits.
pub const BOOT_COUNT_FILE: &str = "boot_count";

/// Safety ceiling on a slot wait so an unexpected cross-test dependency never deadlocks forever.
pub const LET_IN_AFTER: Duration = Duration::from_secs(300);

/// Poll cap when contending on cross-process slot files.
const POLL_CAP: Duration = Duration::from_millis(20);

/// Compute the number of concurrent multi-process test stacks admitted for `cores` granted CPUs.
///
/// Single-core grants admit one stack; multi-core grants admit `(cores / 2).clamp(2, 8)` stacks so
/// concurrent test binaries share one host-wide process-tree ceiling without starving multi-step
/// suites on loaded hosts.
#[must_use]
pub const fn slots_for_cores(cores: u32) -> usize {
    if cores <= 1 {
        1
    } else {
        let half = (cores as usize) / 2;
        if half < 2 {
            2
        } else if half > 8 {
            8
        } else {
            half
        }
    }
}

/// Compute the number of concurrent process-tree boots admitted for `slots` live stack slots.
#[must_use]
pub const fn boot_slots_for_pool(slots: usize) -> usize {
    let raw = slots / 2;
    if raw < 1 {
        1
    } else if raw > 3 {
        3
    } else {
        raw
    }
}

/// Parse a positive decimal `u32` worker ceiling from an environment value.
#[must_use]
pub fn parse_positive_u32(raw: Option<&OsStr>) -> Option<u32> {
    let text = raw?.to_str()?.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>().ok().filter(|&n| n > 0)
}

/// Resolve the standalone core budget from `rust_test_threads`, `cargo_build_jobs`, or `host_cores`.
///
/// Environment counts act as ceilings on `host_cores.max(1)` so a suite that widens
/// `RUST_TEST_THREADS` for idle-waiting unit tests never inflates its live subprocess pool above
/// the physical core count.
#[must_use]
pub fn default_cores_from(
    rust_test_threads: Option<&OsStr>,
    cargo_build_jobs: Option<&OsStr>,
    host_cores: u32,
) -> u32 {
    let mut budget = host_cores.max(1);
    if let Some(threads) = parse_positive_u32(rust_test_threads) {
        budget = budget.min(threads);
    }
    if let Some(jobs) = parse_positive_u32(cargo_build_jobs) {
        budget = budget.min(jobs);
    }
    budget
}

fn default_cores() -> u32 {
    let host = std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(2);
    default_cores_from(
        std::env::var_os("RUST_TEST_THREADS").as_deref(),
        std::env::var_os("CARGO_BUILD_JOBS").as_deref(),
        host,
    )
}

/// Initialize a cross-process subprocess slot directory at `dir` with `slots` stack slots.
///
/// # Errors
/// Returns an I/O error if `dir` or its slot files cannot be created or written.
pub fn init_pool_dir(dir: &Path, slots: usize) -> io::Result<()> {
    let slots = slots.clamp(1, 64);
    let boots = boot_slots_for_pool(slots);
    create_private_dir(dir)?;
    for idx in 0..slots {
        create_slot_file(&slot_path(dir, "slot", idx))?;
    }
    for idx in 0..boots {
        create_slot_file(&slot_path(dir, "boot", idx))?;
    }
    std::fs::write(dir.join(COUNT_FILE), format!("{slots}\n"))?;
    std::fs::write(dir.join(BOOT_COUNT_FILE), format!("{boots}\n"))?;
    Ok(())
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn create_slot_file(path: &Path) -> io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn slot_path(dir: &Path, kind: &str, idx: usize) -> PathBuf {
    dir.join(format!("{kind}-{idx:04}.lock"))
}

fn read_count(dir: &Path, file: &str) -> Option<usize> {
    let raw = std::fs::read_to_string(dir.join(file)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    trimmed
        .parse::<usize>()
        .ok()
        .filter(|&n| (1..=64).contains(&n))
}

struct LocalSemaphore {
    cap: usize,
    held: Mutex<usize>,
    cv: Condvar,
}

impl LocalSemaphore {
    fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            held: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    fn acquire(&'static self, timeout: Duration) -> Option<LocalGuard> {
        let deadline = Instant::now().checked_add(timeout);
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held >= self.cap {
            let left = deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return None;
            }
            held = self
                .cv
                .wait_timeout(held, left.min(Duration::from_millis(50)))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *held += 1;
        Some(LocalGuard { sem: self })
    }
}

struct LocalGuard {
    sem: &'static LocalSemaphore,
}

impl Drop for LocalGuard {
    fn drop(&mut self) {
        let mut held = self.sem.held.lock().unwrap_or_else(PoisonError::into_inner);
        *held = held.saturating_sub(1);
        self.sem.cv.notify_one();
    }
}

fn local_stack_pool() -> &'static LocalSemaphore {
    static POOL: OnceLock<LocalSemaphore> = OnceLock::new();
    POOL.get_or_init(|| LocalSemaphore::new(slots_for_cores(default_cores())))
}

fn local_boot_pool() -> &'static LocalSemaphore {
    static POOL: OnceLock<LocalSemaphore> = OnceLock::new();
    POOL.get_or_init(|| LocalSemaphore::new(boot_slots_for_pool(slots_for_cores(default_cores()))))
}

enum HeldKind {
    #[cfg(any(unix, windows))]
    File {
        _lock: aterm_uds::file_lock::LockFile,
    },
    Local {
        _guard: LocalGuard,
    },
    TimedOut,
}

/// Lease held for the lifetime of a test's live multi-process stack.
#[must_use = "releasing the lease immediately frees the subprocess slot"]
pub struct SubprocessLease {
    held: HeldKind,
}

impl SubprocessLease {
    /// Whether this lease holds an exclusive slot rather than a post-timeout let-in fallback.
    #[must_use]
    pub const fn is_exclusive(&self) -> bool {
        !matches!(self.held, HeldKind::TimedOut)
    }
}

/// Short-lived permit held while launching and awaiting initial readiness of a headless process tree.
#[must_use = "releasing the permit allows another concurrent process-tree boot to start"]
pub struct BootPermit {
    _inner: Arc<SubprocessLease>,
}

impl BootPermit {
    /// Whether this permit holds an exclusive boot slot rather than a post-timeout let-in fallback.
    #[must_use]
    pub fn is_exclusive(&self) -> bool {
        self._inner.is_exclusive()
    }
}

thread_local! {
    static THREAD_STACK_LEASE: RefCell<Weak<SubprocessLease>> = const { RefCell::new(Weak::new()) };
    static THREAD_BOOT_LEASE: RefCell<Weak<SubprocessLease>> = const { RefCell::new(Weak::new()) };
}

static NEXT_OFFSET: AtomicUsize = AtomicUsize::new(0);

#[cfg(any(unix, windows))]
fn try_acquire_in_dir(
    dir: &Path,
    kind: &str,
    count_file: &str,
    timeout: Duration,
) -> Option<HeldKind> {
    let count = read_count(dir, count_file)?;
    let deadline = Instant::now().checked_add(timeout);
    let start = NEXT_OFFSET.fetch_add(1, Ordering::Relaxed);
    let mut backoff = Duration::from_millis(2);
    loop {
        let mut saw_regular = false;
        for step in 0..count {
            let idx = (start.wrapping_add(step)) % count;
            let path = slot_path(dir, kind, idx);
            match aterm_uds::file_lock::LockFile::take(&path, Instant::now(), "busy") {
                Ok(lock) => return Some(HeldKind::File { _lock: lock }),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    saw_regular = true;
                }
                Err(_) => {}
            }
        }
        if !saw_regular {
            // The directory disappeared or its slot files are unreadable; fall back in-process.
            return None;
        }
        let left = deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
        if left.is_zero() {
            return Some(HeldKind::TimedOut);
        }
        std::thread::sleep(backoff.min(left));
        backoff = (backoff * 2).min(POLL_CAP);
    }
}

#[cfg(not(any(unix, windows)))]
fn try_acquire_in_dir(
    _dir: &Path,
    _kind: &str,
    _count_file: &str,
    _timeout: Duration,
) -> Option<HeldKind> {
    None
}

fn acquire_raw(
    pool_dir: Option<&Path>,
    kind: &str,
    count_file: &str,
    fallback: &'static LocalSemaphore,
    timeout: Duration,
) -> SubprocessLease {
    if let Some(dir) = pool_dir
        && let Some(held) = try_acquire_in_dir(dir, kind, count_file, timeout)
    {
        return SubprocessLease { held };
    }
    let held = fallback
        .acquire(timeout)
        .map_or(HeldKind::TimedOut, |_guard| HeldKind::Local { _guard });
    SubprocessLease { held }
}

/// Acquire a subprocess stack lease from `pool_dir` (or the process-local fallback when `None`),
/// reusing any live lease already held on the calling thread.
#[must_use]
pub fn acquire_with_pool(pool_dir: Option<&Path>, timeout: Duration) -> Arc<SubprocessLease> {
    if let Ok(Some(existing)) = THREAD_STACK_LEASE.try_with(|cell| cell.borrow().upgrade()) {
        return existing;
    }
    let lease = Arc::new(acquire_raw(
        pool_dir,
        "slot",
        COUNT_FILE,
        local_stack_pool(),
        timeout,
    ));
    let _ = THREAD_STACK_LEASE.try_with(|cell| {
        *cell.borrow_mut() = Arc::downgrade(&lease);
    });
    lease
}

/// Acquire a subprocess stack lease for the calling test thread.
///
/// Reuses the calling thread's active lease when it already holds a live stack, and releases the
/// underlying slot as soon as the thread's last [`SubprocessLease`] drops.
#[must_use]
pub fn acquire() -> Arc<SubprocessLease> {
    let dir = std::env::var_os(SUBPROCESS_SLOTS_ENV).map(PathBuf::from);
    acquire_with_pool(dir.as_deref(), LET_IN_AFTER)
}

/// Acquire a short-lived boot permit from `pool_dir` (or the process-local fallback when `None`),
/// reusing any active boot permit on the calling thread.
pub fn acquire_boot_permit_with_pool(pool_dir: Option<&Path>, timeout: Duration) -> BootPermit {
    if let Ok(Some(existing)) = THREAD_BOOT_LEASE.try_with(|cell| cell.borrow().upgrade()) {
        return BootPermit { _inner: existing };
    }
    let lease = Arc::new(acquire_raw(
        pool_dir,
        "boot",
        BOOT_COUNT_FILE,
        local_boot_pool(),
        timeout,
    ));
    let _ = THREAD_BOOT_LEASE.try_with(|cell| {
        *cell.borrow_mut() = Arc::downgrade(&lease);
    });
    BootPermit { _inner: lease }
}

/// Acquire a short-lived permit pacing concurrent process-tree boots across test binaries.
pub fn acquire_boot_permit() -> BootPermit {
    let dir = std::env::var_os(SUBPROCESS_SLOTS_ENV).map(PathBuf::from);
    acquire_boot_permit_with_pool(dir.as_deref(), LET_IN_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn slots_for_cores_scales_with_budget_and_clamps_bounds() {
        assert_eq!(slots_for_cores(0), 1);
        assert_eq!(slots_for_cores(1), 1);
        assert_eq!(slots_for_cores(2), 2);
        assert_eq!(slots_for_cores(5), 2);
        assert_eq!(slots_for_cores(6), 3);
        assert_eq!(slots_for_cores(9), 4);
        assert_eq!(slots_for_cores(14), 7);
        assert_eq!(slots_for_cores(64), 8);

        assert_eq!(boot_slots_for_pool(1), 1);
        assert_eq!(boot_slots_for_pool(3), 1);
        assert_eq!(boot_slots_for_pool(4), 2);
        assert_eq!(boot_slots_for_pool(7), 3);

        assert_eq!(
            default_cores_from(Some(OsStr::new("4")), Some(OsStr::new("10")), 14),
            4
        );
        assert_eq!(
            default_cores_from(Some(OsStr::new("12")), Some(OsStr::new("6")), 14),
            6
        );
        // Negative control: widened `RUST_TEST_THREADS` (e.g. `aterm_suite` setting 4x cores)
        // never inflates the live subprocess budget above `host_cores`.
        assert_eq!(default_cores_from(Some(OsStr::new("56")), None, 14), 14);
        assert_eq!(default_cores_from(None, Some(OsStr::new("0")), 14), 14);
    }

    /// Pool directory admits at most `slots` concurrent exclusive holders and frees on drop.
    /// NEGATIVE CONTROL: a third thread contending on a two-slot pool cannot acquire an
    /// exclusive slot before its timeout while both holders keep their leases.
    #[cfg(any(unix, windows))]
    #[test]
    fn pool_caps_concurrent_holders_and_releases_on_drop() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pool dir");
        let pool = tmp.path().join("slots");
        init_pool_dir(&pool, 2).expect("init 2-slot pool");

        let (release_first, wait_first) = mpsc::channel::<()>();
        let (release_second, wait_second) = mpsc::channel::<()>();
        let (held_tx, held_rx) = mpsc::channel::<bool>();

        std::thread::scope(|scope| {
            let pool_a = &pool;
            let held_tx1 = held_tx.clone();
            let h1 = scope.spawn(move || {
                let lease = acquire_with_pool(Some(pool_a), Duration::from_secs(5));
                held_tx1.send(lease.is_exclusive()).expect("report h1");
                let _ = wait_first.recv();
            });
            let held_tx2 = held_tx;
            let h2 = scope.spawn(move || {
                let lease = acquire_with_pool(Some(pool_a), Duration::from_secs(5));
                held_tx2.send(lease.is_exclusive()).expect("report h2");
                let _ = wait_second.recv();
            });
            assert!(held_rx.recv().expect("h1 acquired"));
            assert!(held_rx.recv().expect("h2 acquired"));

            // Negative control: third thread finds both slots locked and times out non-exclusive.
            let third = scope
                .spawn(move || {
                    acquire_with_pool(Some(pool_a), Duration::from_millis(30)).is_exclusive()
                })
                .join()
                .expect("third joined");
            assert!(
                !third,
                "third caller must not take an exclusive slot while 2/2 are held"
            );

            // Dropping the first holder frees its slot immediately for the next waiter.
            drop(release_first);
            h1.join().expect("h1 finished");

            let fourth = scope
                .spawn(move || {
                    acquire_with_pool(Some(pool_a), Duration::from_secs(5)).is_exclusive()
                })
                .join()
                .expect("fourth joined");
            assert!(
                fourth,
                "fourth caller must acquire the released exclusive slot"
            );

            drop(release_second);
            h2.join().expect("h2 finished");
        });
    }

    /// Re-entrant calls on the same thread reuse the live lease without blocking, and dropping
    /// the last clone on that thread releases the slot before the thread exits.
    /// NEGATIVE CONTROL: while at least one clone remains alive on the first thread, another
    /// thread still observes the single slot as exclusively held.
    #[cfg(any(unix, windows))]
    #[test]
    fn reentrant_acquire_reuses_thread_lease_and_releases_when_last_clone_drops() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pool dir");
        let pool = tmp.path().join("one-slot");
        init_pool_dir(&pool, 1).expect("init 1-slot pool");

        let first = acquire_with_pool(Some(&pool), Duration::from_millis(50));
        assert!(first.is_exclusive());
        let second = acquire_with_pool(Some(&pool), Duration::from_millis(1));
        assert!(second.is_exclusive());
        assert!(
            Arc::ptr_eq(&first, &second),
            "second boot on the same thread must share the first lease without re-locking"
        );

        drop(first);
        // `second` is still alive, so the single slot must remain locked.
        let blocked = std::thread::scope(|scope| {
            let pool_ref = &pool;
            scope
                .spawn(move || {
                    acquire_with_pool(Some(pool_ref), Duration::from_millis(25)).is_exclusive()
                })
                .join()
                .expect("contender joined")
        });
        assert!(
            !blocked,
            "slot must remain held until the thread's last clone drops"
        );

        // Dropping `second` on this still-running thread must unlock the slot immediately.
        drop(second);
        let freed = std::thread::scope(|scope| {
            let pool_ref = &pool;
            scope
                .spawn(move || {
                    acquire_with_pool(Some(pool_ref), Duration::from_secs(5)).is_exclusive()
                })
                .join()
                .expect("post-drop contender joined")
        });
        assert!(
            freed,
            "slot must unlock as soon as the last lease on the thread drops"
        );
    }
}
