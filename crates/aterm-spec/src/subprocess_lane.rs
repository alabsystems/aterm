// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-binary and in-process admission for tests that spawn multi-process trees.
//!
//! Live integration suites (`headless_boot`, `fabric_world`) spawn a headless `aterm`
//! instance per test, four to six OS processes each, and `aos_search` a threaded search.
//! `libtest` counts only in-process test threads,
//! and `aterm-verify`'s `testrun` pool runs several test binaries side by side, so
//! uncoordinated binaries can stack hundreds of runnable processes on one host and
//! push live waits past their one-minute hang detectors (`ATERM-KI-1103-3`).
//!
//! When `aterm-verify` runs a test stage, it creates a scratch pool directory sized to
//! the stage's granted CPU budget and passes its path in [`SUBPROCESS_SLOTS_ENV`].
//! Every test thread across all concurrent test binaries takes one [`SubprocessLease`]
//! from that shared directory (via [`aterm_uds::file_lock::LockFile`], which keeps the
//! descriptor out of spawned children and unlocks with `LOCK_UN` before closing) and
//! holds it until the spawned process tree is torn down. A configured shared pool is
//! authoritative: a missing, malformed, saturated, or unsupported pool refuses the
//! test before it spawns. Only an absent configuration uses the process-local pool,
//! bounded from `RUST_TEST_THREADS`, `CARGO_BUILD_JOBS`, or
//! `std::thread::available_parallelism`.
//! The verifier owns that private layout until its children finish. Admission detects same-path
//! replacement during acquisition and thread-local re-entry, and refuses link/reparse-point
//! members, but this protocol is not a confinement boundary against same-account code deliberately
//! mutating individual members between opens.
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
pub const ADMISSION_TIMEOUT: Duration = Duration::from_secs(300);

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
/// Returns an I/O error if the new `dir` or any fixed-layout member cannot be created or written.
/// An existing destination is refused. After any error following directory creation, the caller
/// still owns the partially initialized new directory and decides how to remove it.
pub fn init_pool_dir(dir: &Path, slots: usize) -> io::Result<()> {
    let slots = slots.clamp(1, 64);
    let boots = boot_slots_for_pool(slots);
    create_private_dir(dir)?;
    (|| {
        for idx in 0..slots {
            create_slot_file(&slot_path(dir, "slot", idx))?;
        }
        for idx in 0..boots {
            create_slot_file(&slot_path(dir, "boot", idx))?;
        }
        create_count_file(&dir.join(COUNT_FILE), slots)?;
        create_count_file(&dir.join(BOOT_COUNT_FILE), boots)?;
        Ok(())
    })()
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?
            .set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_slot_file(path: &Path) -> io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
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

fn create_count_file(path: &Path, count: usize) -> io::Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    let mut file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    writeln!(file, "{count}")
}

fn slot_path(dir: &Path, kind: &str, idx: usize) -> PathBuf {
    dir.join(format!("{kind}-{idx:04}.lock"))
}

fn read_count(dir: &Path, file: &str) -> io::Result<usize> {
    use std::io::Read as _;

    let path = dir.join(file);
    let (count_file, meta) = aterm_uds::regular_file::open(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "subprocess pool count is not a plain regular file at {}: {error}",
                path.display()
            ),
        )
    })?;
    let mut raw = String::with_capacity(
        usize::try_from(meta.len().min(65))
            .unwrap_or(0)
            .saturating_add(1),
    );
    count_file.take(65).read_to_string(&mut raw)?;
    if raw.len() > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "subprocess pool count is unexpectedly large: {}",
                path.display()
            ),
        ));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "subprocess pool count is not a positive decimal: {}",
                path.display()
            ),
        ));
    }
    trimmed
        .parse::<usize>()
        .ok()
        .filter(|&n| (1..=64).contains(&n))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "subprocess pool count is outside 1..=64: {}",
                    path.display()
                ),
            )
        })
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
        let started = Instant::now();
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held >= self.cap {
            let left = timeout.saturating_sub(started.elapsed());
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PoolIdentity {
    Shared {
        path: PathBuf,
        object: SharedPoolIdentity,
    },
    Local,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SharedPoolIdentity {
    #[cfg(unix)]
    Unix(aterm_uds::directory_identity::Identity),
    #[cfg(windows)]
    Windows { volume: u64, file_id: u128 },
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

impl PoolIdentity {
    fn of(dir: Option<&Path>) -> io::Result<Self> {
        let Some(dir) = dir else {
            return Ok(Self::Local);
        };
        let canonical = std::fs::canonicalize(dir).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "configured subprocess pool is unavailable at {}: {error}",
                    dir.display()
                ),
            )
        })?;
        let object = shared_pool_identity(&canonical)?;
        Ok(Self::Shared {
            path: canonical,
            object,
        })
    }
}

#[cfg(unix)]
fn shared_pool_identity(path: &Path) -> io::Result<SharedPoolIdentity> {
    aterm_uds::directory_identity::read(path).map(SharedPoolIdentity::Unix)
}

#[cfg(windows)]
fn shared_pool_identity(path: &Path) -> io::Result<SharedPoolIdentity> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    // Read identity from the opened directory object. Path metadata could describe a replacement
    // published between canonicalization and inspection, and following a replacement reparse
    // point would make that race invisible.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let directory = options.open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "configured subprocess pool is a reparse point or not a directory: {}",
                path.display()
            ),
        ));
    }
    let identity = aterm_dirfd::file_id(&directory).map_err(io::Error::from)?;
    Ok(SharedPoolIdentity::Windows {
        volume: identity.dev,
        file_id: identity.ino,
    })
}

#[cfg(not(any(unix, windows)))]
fn shared_pool_identity(path: &Path) -> io::Result<SharedPoolIdentity> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "configured subprocess pool is not a directory: {}",
                path.display()
            ),
        ));
    }
    Ok(SharedPoolIdentity::Unsupported)
}

/// Lease held for the lifetime of a test's live multi-process stack.
#[must_use = "releasing the lease immediately frees the subprocess slot"]
pub struct SubprocessLease {
    _held: HeldKind,
    pool: PoolIdentity,
}

/// Short-lived permit held while launching and awaiting initial readiness of a headless process tree.
#[must_use = "releasing the permit allows another concurrent process-tree boot to start"]
pub struct BootPermit {
    _inner: Arc<SubprocessLease>,
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
) -> io::Result<HeldKind> {
    let count = read_count(dir, count_file)?;
    for idx in 0..count {
        let path = slot_path(dir, kind, idx);
        aterm_uds::regular_file::open(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "subprocess pool slot is not a plain regular file at {}: {error}",
                    path.display()
                ),
            )
        })?;
    }
    let started = Instant::now();
    let start = NEXT_OFFSET.fetch_add(1, Ordering::Relaxed);
    let mut backoff = Duration::from_millis(2);
    loop {
        for step in 0..count {
            let idx = (start.wrapping_add(step)) % count;
            let path = slot_path(dir, kind, idx);
            match aterm_uds::file_lock::LockFile::take_existing(&path, Instant::now(), "busy") {
                Ok(lock) => return Ok(HeldKind::File { _lock: lock }),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "subprocess pool slot is unusable at {}: {error}",
                            path.display()
                        ),
                    ));
                }
            }
        }
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "subprocess {kind} pool remained full for {:.3}s at {}",
                    timeout.as_secs_f64(),
                    dir.display()
                ),
            ));
        }
        std::thread::sleep(backoff.min(left));
        backoff = (backoff * 2).min(POLL_CAP);
    }
}

#[cfg(not(any(unix, windows)))]
fn try_acquire_in_dir(
    dir: &Path,
    _kind: &str,
    _count_file: &str,
    _timeout: Duration,
) -> io::Result<HeldKind> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "configured cross-process subprocess pool is unsupported on this target: {}",
            dir.display()
        ),
    ))
}

fn acquire_raw(
    pool: PoolIdentity,
    kind: &str,
    count_file: &str,
    fallback: &'static LocalSemaphore,
    timeout: Duration,
) -> io::Result<SubprocessLease> {
    let held = match &pool {
        PoolIdentity::Shared { path, .. } => {
            let held = try_acquire_in_dir(path, kind, count_file, timeout)?;
            if PoolIdentity::of(Some(path))? != pool {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "configured subprocess pool changed while admitting {kind}: {}",
                        path.display()
                    ),
                ));
            }
            held
        }
        PoolIdentity::Local => fallback
            .acquire(timeout)
            .map(|_guard| HeldKind::Local { _guard })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "local subprocess {kind} pool remained full for {:.3}s",
                        timeout.as_secs_f64()
                    ),
                )
            })?,
    };
    Ok(SubprocessLease { _held: held, pool })
}

/// Acquire a subprocess stack lease from `pool_dir` (or the process-local fallback when `None`),
/// reusing any live lease already held on the calling thread.
///
/// # Errors
/// Returns an I/O error when a configured pool is unavailable or malformed, or `TimedOut` when
/// no exclusive slot becomes available before `timeout`.
pub fn acquire_with_pool(
    pool_dir: Option<&Path>,
    timeout: Duration,
) -> io::Result<Arc<SubprocessLease>> {
    let pool = PoolIdentity::of(pool_dir)?;
    if let Ok(Some(existing)) = THREAD_STACK_LEASE.try_with(|cell| cell.borrow().upgrade()) {
        if existing.pool != pool {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nested subprocess stack admission changed pools",
            ));
        }
        return Ok(existing);
    }
    let lease = Arc::new(acquire_raw(
        pool,
        "slot",
        COUNT_FILE,
        local_stack_pool(),
        timeout,
    )?);
    let _ = THREAD_STACK_LEASE.try_with(|cell| {
        *cell.borrow_mut() = Arc::downgrade(&lease);
    });
    Ok(lease)
}

/// Acquire a subprocess stack lease for the calling test thread.
///
/// Reuses the calling thread's active lease when it already holds a live stack, and releases the
/// underlying slot as soon as the thread's last [`SubprocessLease`] drops.
///
/// # Errors
/// Returns [`acquire_with_pool`]'s error for the configured shared pool or local fallback.
pub fn acquire() -> io::Result<Arc<SubprocessLease>> {
    let dir = std::env::var_os(SUBPROCESS_SLOTS_ENV).map(PathBuf::from);
    acquire_with_pool(dir.as_deref(), ADMISSION_TIMEOUT)
}

/// Acquire a short-lived boot permit from `pool_dir` (or the process-local fallback when `None`),
/// reusing any active boot permit on the calling thread.
///
/// # Errors
/// Returns an I/O error when a configured pool is unavailable or malformed, or `TimedOut` when
/// no exclusive boot slot becomes available before `timeout`.
pub fn acquire_boot_permit_with_pool(
    pool_dir: Option<&Path>,
    timeout: Duration,
) -> io::Result<BootPermit> {
    let pool = PoolIdentity::of(pool_dir)?;
    if let Ok(Some(existing)) = THREAD_BOOT_LEASE.try_with(|cell| cell.borrow().upgrade()) {
        if existing.pool != pool {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nested subprocess boot admission changed pools",
            ));
        }
        return Ok(BootPermit { _inner: existing });
    }
    let lease = Arc::new(acquire_raw(
        pool,
        "boot",
        BOOT_COUNT_FILE,
        local_boot_pool(),
        timeout,
    )?);
    let _ = THREAD_BOOT_LEASE.try_with(|cell| {
        *cell.borrow_mut() = Arc::downgrade(&lease);
    });
    Ok(BootPermit { _inner: lease })
}

/// Acquire a short-lived permit pacing concurrent process-tree boots across test binaries.
///
/// # Errors
/// Returns [`acquire_boot_permit_with_pool`]'s error for the configured shared pool or local
/// fallback.
pub fn acquire_boot_permit() -> io::Result<BootPermit> {
    let dir = std::env::var_os(SUBPROCESS_SLOTS_ENV).map(PathBuf::from);
    acquire_boot_permit_with_pool(dir.as_deref(), ADMISSION_TIMEOUT)
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
        let (held_tx, held_rx) = mpsc::channel::<()>();

        std::thread::scope(|scope| {
            let pool_a = &pool;
            let held_tx1 = held_tx.clone();
            let h1 = scope.spawn(move || {
                let _lease = acquire_with_pool(Some(pool_a), Duration::from_secs(60))
                    .expect("first exclusive lease");
                held_tx1.send(()).expect("report h1");
                wait_first
                    .recv_timeout(Duration::from_secs(60))
                    .expect("release h1");
            });
            let held_tx2 = held_tx;
            let h2 = scope.spawn(move || {
                let _lease = acquire_with_pool(Some(pool_a), Duration::from_secs(60))
                    .expect("second exclusive lease");
                held_tx2.send(()).expect("report h2");
                wait_second
                    .recv_timeout(Duration::from_secs(60))
                    .expect("release h2");
            });
            held_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("h1 acquired");
            held_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("h2 acquired");

            // Negative control: a third thread cannot receive any usable lease while 2/2 slots
            // remain held.
            let third = scope
                .spawn(move || {
                    acquire_with_pool(Some(pool_a), Duration::ZERO)
                        .err()
                        .map(|error| error.kind())
                })
                .join()
                .expect("third joined");
            assert_eq!(
                third,
                Some(io::ErrorKind::TimedOut),
                "third caller must receive no lease while 2/2 are held"
            );

            // Dropping the first holder frees its slot immediately for the next waiter.
            release_first.send(()).expect("release first holder");
            h1.join().expect("h1 finished");

            let fourth = scope
                .spawn(move || acquire_with_pool(Some(pool_a), Duration::from_secs(60)).is_ok())
                .join()
                .expect("fourth joined");
            assert!(
                fourth,
                "fourth caller must acquire the released exclusive slot"
            );

            release_second.send(()).expect("release second holder");
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

        let first =
            acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("first exclusive lease");
        let second =
            acquire_with_pool(Some(&pool), Duration::ZERO).expect("reentrant exclusive lease");
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
                    acquire_with_pool(Some(pool_ref), Duration::ZERO)
                        .err()
                        .map(|error| error.kind())
                })
                .join()
                .expect("contender joined")
        });
        assert_eq!(blocked, Some(io::ErrorKind::TimedOut));

        // Dropping `second` on this still-running thread must unlock the slot immediately.
        drop(second);
        let freed = std::thread::scope(|scope| {
            let pool_ref = &pool;
            scope
                .spawn(move || acquire_with_pool(Some(pool_ref), Duration::from_secs(60)).is_ok())
                .join()
                .expect("post-drop contender joined")
        });
        assert!(
            freed,
            "slot must unlock as soon as the last lease on the thread drops"
        );
    }

    /// A configured shared pool is an admission authority, not a hint. Missing or malformed
    /// structure refuses acquisition and never falls back to this process's private semaphore.
    /// NEGATIVE CONTROL: with no shared pool configured, the local semaphore still admits a
    /// caller normally.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_broken_configured_pool_refuses_instead_of_falling_back_locally() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pools");
        let first_pool = tmp.path().join("first");
        let broken_pool = tmp.path().join("broken");
        init_pool_dir(&first_pool, 1).expect("first pool");
        init_pool_dir(&broken_pool, 1).expect("second pool");
        let duplicate = init_pool_dir(&broken_pool, 4).expect_err("pool init is exclusive");
        assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read_count(&broken_pool, COUNT_FILE).unwrap(), 1);

        let first = acquire_with_pool(Some(&first_pool), Duration::from_secs(60))
            .expect("first pool lease");
        let changed_pool = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
            .err()
            .expect("nested admission cannot change pools");
        assert_eq!(changed_pool.kind(), io::ErrorKind::InvalidInput);
        drop(first);

        std::fs::remove_file(slot_path(&broken_pool, "slot", 0)).expect("break stack pool");
        let broken = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
            .err()
            .expect("a broken configured pool must refuse");
        assert_eq!(broken.kind(), io::ErrorKind::NotFound);

        std::fs::write(broken_pool.join(COUNT_FILE), b"not-a-count\n").expect("malformed count");
        let malformed = acquire_with_pool(Some(&broken_pool), Duration::ZERO)
            .err()
            .expect("a malformed configured pool must refuse");
        assert_eq!(malformed.kind(), io::ErrorKind::InvalidData);

        std::fs::remove_file(slot_path(&broken_pool, "boot", 0)).expect("break boot pool");
        let broken_boot = acquire_boot_permit_with_pool(Some(&broken_pool), Duration::ZERO)
            .err()
            .expect("a broken configured boot pool must refuse");
        assert_eq!(broken_boot.kind(), io::ErrorKind::NotFound);

        let local = acquire_with_pool(None, Duration::from_secs(60))
            .expect("an absent shared configuration uses the local bound");
        drop(local);
        let local_boot = acquire_boot_permit_with_pool(None, Duration::from_secs(60))
            .expect("an absent shared configuration uses the local boot bound");
        drop(local_boot);
    }

    /// Every bounded decimal count is checked independently for stack and boot admission.
    /// NEGATIVE CONTROL: a freshly initialized pool admits both kinds after the malformed
    /// fixtures refuse without publishing replacement slots.
    #[cfg(any(unix, windows))]
    #[test]
    fn configured_count_bounds_refuse_stack_and_boot_admission() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pools");
        for (idx, raw) in [
            None,
            Some("0"),
            Some("65"),
            Some("bad"),
            Some("999999999999999999999999999999999999999999999999999999999999999999999"),
        ]
        .into_iter()
        .enumerate()
        {
            let pool = tmp.path().join(format!("broken-{idx}"));
            std::fs::create_dir(&pool).expect("broken pool dir");
            for name in [COUNT_FILE, BOOT_COUNT_FILE] {
                if let Some(raw) = raw {
                    std::fs::write(pool.join(name), raw).expect("broken count");
                }
            }
            assert!(acquire_with_pool(Some(&pool), Duration::ZERO).is_err());
            assert!(acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO).is_err());
        }

        let valid = tmp.path().join("valid");
        init_pool_dir(&valid, 1).expect("valid pool");
        assert!(acquire_with_pool(Some(&valid), Duration::from_secs(60)).is_ok());
        assert!(acquire_boot_permit_with_pool(Some(&valid), Duration::from_secs(60)).is_ok());
    }

    /// Stack and boot pools fail closed when saturated, and neither re-entrant lane can switch
    /// from a configured shared pool to the local fallback while its lease remains live.
    #[cfg(any(unix, windows))]
    #[test]
    fn boot_saturation_refuses_and_nested_admission_cannot_change_pools() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pools");
        let pool = tmp.path().join("pool");
        init_pool_dir(&pool, 1).expect("pool");
        let held = acquire_boot_permit_with_pool(Some(&pool), Duration::from_secs(60))
            .expect("boot permit");
        let nested = acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO)
            .expect("same pool boot permit");
        assert!(Arc::ptr_eq(&held._inner, &nested._inner));
        assert!(acquire_boot_permit_with_pool(None, Duration::ZERO).is_err());
        let error = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    acquire_boot_permit_with_pool(Some(&pool), Duration::ZERO)
                        .err()
                        .map(|error| error.kind())
                })
                .join()
                .expect("boot contender")
        });
        assert_eq!(error, Some(io::ErrorKind::TimedOut));

        let stack =
            acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("stack admission");
        assert!(acquire_with_pool(None, Duration::ZERO).is_err());
        assert!(acquire_with_pool(Some(&tmp.path().join("missing")), Duration::ZERO).is_err());
        drop((stack, nested, held));
        assert!(acquire_boot_permit_with_pool(Some(&pool), Duration::from_secs(60)).is_ok());
    }

    /// The standalone semaphore returns no lease when full, then admits immediately after drop.
    #[test]
    fn standalone_saturation_returns_no_lease() {
        fn pool() -> &'static LocalSemaphore {
            static POOL: OnceLock<LocalSemaphore> = OnceLock::new();
            POOL.get_or_init(|| LocalSemaphore::new(1))
        }
        let held = acquire_raw(
            PoolIdentity::Local,
            "slot",
            COUNT_FILE,
            pool(),
            Duration::ZERO,
        )
        .expect("first local lease");
        assert_eq!(
            acquire_raw(
                PoolIdentity::Local,
                "slot",
                COUNT_FILE,
                pool(),
                Duration::ZERO,
            )
            .err()
            .map(|error| error.kind()),
            Some(io::ErrorKind::TimedOut)
        );
        drop(held);
        assert!(
            acquire_raw(
                PoolIdentity::Local,
                "slot",
                COUNT_FILE,
                pool(),
                Duration::ZERO,
            )
            .is_ok()
        );
    }

    /// Re-entrant admission belongs to the opened directory object, not merely its pathname.
    /// NEGATIVE CONTROL: replacing a pool directory at the same path refuses the old thread-local
    /// lease; an alias of the unchanged directory still resolves to the same pool.
    #[cfg(any(unix, windows))]
    #[test]
    fn reentrant_admission_tracks_the_pool_object_across_path_replacement() {
        let tmp = aterm_tempfile::tempdir().expect("scratch pools");
        let pool = tmp.path().join("pool");
        let displaced = tmp.path().join("displaced");
        init_pool_dir(&pool, 1).expect("first pool");
        let first =
            acquire_with_pool(Some(&pool), Duration::from_secs(60)).expect("first pool lease");
        std::fs::rename(&pool, &displaced).expect("displace first pool");
        init_pool_dir(&pool, 1).expect("replacement pool");
        let replacement = acquire_with_pool(Some(&pool), Duration::ZERO)
            .err()
            .expect("replacement cannot reuse old lease");
        assert_eq!(replacement.kind(), io::ErrorKind::InvalidInput);
        drop(first);

        let second = acquire_with_pool(Some(&pool), Duration::from_secs(60))
            .expect("replacement pool lease after old release");
        #[cfg(unix)]
        {
            let alias = tmp.path().join("alias");
            std::os::unix::fs::symlink(&pool, &alias).expect("pool alias");
            let through_alias = acquire_with_pool(Some(&alias), Duration::ZERO)
                .expect("unchanged pool alias reuses the lease");
            assert!(Arc::ptr_eq(&second, &through_alias));
        }
        drop(second);
    }
}
