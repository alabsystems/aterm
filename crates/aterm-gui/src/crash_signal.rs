// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Async-signal-safe capture of native fatal signals.

/// Arm async-signal-safe fatal-signal capture (`SIGSEGV`/`SIGABRT`/`SIGBUS`/`SIGILL`/`SIGFPE`).
pub(crate) fn install_signal_handlers_as(arming: Arming) {
    #[cfg(unix)]
    imp::install_signal_handlers(arming);
    #[cfg(not(unix))]
    let _ = arming;
    #[cfg(windows)]
    imp_windows::install();
}

/// Whose marker this is, which decides whether its empty corpse means anything.
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "the owner lives in the unix marker name only")
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerOwner {
    /// The installed.
    App,
    /// Everything else.
    Other,
    /// A lab desk's headless instance ([`crate::desk::Desk::LabHeadless`]).
    Lab,
}

impl MarkerOwner {
    /// The token in the marker's file name.
    #[cfg(unix)]
    const fn tag(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Other => "other",
            Self::Lab => "lab",
        }
    }

    /// Whether this owner's dead ([`markers::sweep`]).
    #[cfg(unix)]
    pub(crate) const fn is_news_to(self, scan: MarkerOwner) -> bool {
        match self {
            Self::App => true,
            Self::Lab => matches!(scan, Self::Lab),
            Self::Other => false,
        }
    }
}

/// How a launch arms its marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arming {
    /// As [`MarkerOwner::App`] from the start.
    App,
    /// As [`MarkerOwner::Other`], for good.
    Other,
    /// As [`MarkerOwner::Other`] until [`adopt_app_identity`].
    AppAfterCommit,
    /// As [`MarkerOwner::Lab`], for good ([`crate::desk::Desk::LabHeadless`]).
    Lab,
}

impl Arming {
    /// The arming of a launch ([`Arming::Lab`]).
    #[must_use]
    pub(crate) const fn for_launch(
        installed_bundle: bool,
        headless: bool,
        handoff_candidate: bool,
        lab_desk: bool,
    ) -> Self {
        if lab_desk {
            Self::Lab
        } else if !installed_bundle || headless {
            Self::Other
        } else if handoff_candidate {
            Self::AppAfterCommit
        } else {
            Self::App
        }
    }

    /// The owner the marker is created under.
    #[cfg(unix)]
    const fn initial_owner(self) -> MarkerOwner {
        match self {
            Self::App => MarkerOwner::App,
            Self::Other | Self::AppAfterCommit => MarkerOwner::Other,
            Self::Lab => MarkerOwner::Lab,
        }
    }
}

/// This process is about to end cleanly.
#[cfg(unix)]
pub(crate) fn release_own_marker() {
    imp::release_own_marker();
}

/// The start this process's crash marker is named with.
#[cfg(unix)]
#[must_use]
pub(crate) fn own_marker_nanos() -> Option<u128> {
    imp::own_marker_nanos()
}

/// Where this process's crash marker is.
#[cfg(unix)]
#[must_use]
pub(crate) fn own_marker_place() -> Option<(std::path::PathBuf, u128)> {
    imp::own_marker_place()
}

/// The handoff candidate this process was is now the daily driver ([`MarkerOwner::App`]).
#[cfg(unix)]
pub(crate) fn adopt_app_identity() -> bool {
    imp::adopt_app_identity()
}

/// Leave the process with `code` NOW ([`release_own_marker`]).
#[cfg(unix)]
pub(crate) fn clean_exit_now(code: i32) -> ! {
    let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
    release_own_marker();
    // SAFETY: async-signal-safe immediate exit of the calling process.
    unsafe { libc::_exit(code) }
}

/// Release this launch's crash marker at a clean exit (Windows).
pub(crate) fn remove_marker_on_clean_exit() {
    #[cfg(windows)]
    imp_windows::release_marker();
}

/// Launch-unique marker path.
#[cfg(windows)]
fn marker_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!(
        "crash-signal-{}-{}.log",
        std::process::id(),
        startup_nanos()
    ))
}

/// The one rule shared by the Windows clean-exit release and the next-launch sweep: a marker is a
/// removable clean-run leftover exactly when it is empty; any byte makes it a crash record.
#[cfg(any(windows, test))]
fn marker_is_clean_leftover(len: u64) -> bool {
    len == 0
}

/// Delete the marker at `path` if it is a regular file with nothing in it.
#[cfg(any(windows, test))]
fn remove_marker_if_empty(path: &std::path::Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && marker_is_clean_leftover(meta.len()) => {
            std::fs::remove_file(path).is_ok()
        }
        _ => false,
    }
}

/// Nanoseconds since the epoch.
#[cfg(any(unix, windows))]
fn startup_nanos() -> u128 {
    aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
}

/// Delete zero-length `crash-signal-*.log` files left in `dir` ([`remove_marker_on_clean_exit`]).
#[cfg(windows)]
fn sweep_stale_markers(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("crash-signal-")
            && name.ends_with(".log")
            && entry
                .metadata()
                .map(|m| marker_is_clean_leftover(m.len()))
                .unwrap_or(false)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The unix marker's lifecycle.
#[cfg(unix)]
pub(crate) mod markers {
    use std::path::{Path, PathBuf};

    use super::MarkerOwner;

    /// Prefix of this module's markers.
    pub(crate) const PREFIX: &str = "crash-marker-";
    /// Prefix of the markers builds before this module armed.
    const LEGACY_PREFIX: &str = "crash-signal-";
    /// Suffix of a marker still being armed (see [`create_locked`]).
    const PENDING_SUFFIX: &str = ".pending";

    /// A marker file name, parsed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct MarkerName {
        pub(crate) pid: u32,
        /// The owner's start, nanoseconds since the epoch.
        pub(crate) nanos: u128,
        pub(crate) owner: MarkerOwner,
    }

    /// `crash-marker-<pid>-<nanos>-<owner>.log`.
    #[must_use]
    pub(crate) fn file_name(pid: u32, nanos: u128, owner: MarkerOwner) -> String {
        format!("{PREFIX}{pid}-{nanos}-{}.log", owner.tag())
    }

    /// Parse a [`file_name`].
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<MarkerName> {
        let body = name.strip_prefix(PREFIX)?.strip_suffix(".log")?;
        let mut parts = body.splitn(3, '-');
        let pid = parts.next()?.parse::<u32>().ok()?;
        let nanos = parts.next()?;
        if nanos.is_empty() || !nanos.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let nanos = nanos.parse::<u128>().ok()?;
        let owner = match parts.next()? {
            "app" => MarkerOwner::App,
            "other" => MarkerOwner::Other,
            "lab" => MarkerOwner::Lab,
            _ => return None,
        };
        Some(MarkerName { pid, nanos, owner })
    }

    /// The pid of a legacy `crash-signal-<pid>-<nanos>.log`.
    fn legacy_pid(name: &str) -> Option<u32> {
        let body = name.strip_prefix(LEGACY_PREFIX)?.strip_suffix(".log")?;
        body.split('-').next()?.parse().ok()
    }

    /// What trying a marker's owner lock says about its owner.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Owner {
        /// Someone holds it.
        Live,
        /// The lock was free.
        Dead,
        /// The file could not be opened or locked for another reason.
        Unknown,
    }

    /// Probe `path`'s owner lock without waiting.
    #[must_use]
    pub(crate) fn probe(path: &Path) -> Owner {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Owner::Unknown;
        };
        // SAFETY: `c_path` is a live NUL-terminated string for the call.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Owner::Unknown;
        }
        // SAFETY: `fd` was opened above and is closed below; LOCK_NB never waits.
        #[expect(
            clippy::disallowed_methods,
            reason = "a probe: a lock it takes is let go by LOCK_UN below, before the close"
        )]
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        let err = std::io::Error::last_os_error().raw_os_error();
        if locked {
            // SAFETY: `fd` is the descriptor opened above; LOCK_UN never waits.
            #[expect(
                clippy::disallowed_methods,
                reason = "the release: LOCK_UN before the close"
            )]
            unsafe {
                libc::flock(fd, libc::LOCK_UN)
            };
        }
        // SAFETY: closes the descriptor opened above exactly once.
        unsafe { libc::close(fd) };
        if locked {
            Owner::Dead
        } else if err == Some(libc::EWOULDBLOCK) {
            Owner::Live
        } else {
            Owner::Unknown
        }
    }

    /// An empty regular file (not a symlink, FIFO or directory).
    fn empty_regular_file(entry: &std::fs::DirEntry) -> bool {
        entry
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() == 0)
    }

    /// Remove dead markers in `dir` whose emptiness is not news ([`MarkerOwner::is_news_to`]).
    pub(crate) fn sweep(dir: &Path, own_pid: u32, scan: MarkerOwner) -> Vec<MarkerName> {
        let mut swept = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return swept;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !empty_regular_file(&entry) {
                continue;
            }
            let path = entry.path();
            if let Some(pending) = name
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(PENDING_SUFFIX))
            {
                // An arming that died between its create and its rename.
                if parse(pending).is_some() && probe(&path) == Owner::Dead {
                    let _ = std::fs::remove_file(&path);
                }
            } else if let Some(marker) = parse(name) {
                let news = marker.owner.is_news_to(scan) && marker.pid != own_pid;
                if !news
                    && probe(&path) == Owner::Dead
                    && std::fs::remove_file(&path).is_ok()
                    && marker.pid != own_pid
                {
                    swept.push(marker);
                }
            } else if legacy_pid(name).is_some_and(|pid| !aterm_uds::process::pid_alive(pid)) {
                let _ = std::fs::remove_file(&path);
            }
        }
        swept
    }

    /// What this process's install-time [`sweep`] removed.
    static SWEPT_AT_INSTALL: std::sync::OnceLock<Vec<MarkerName>> = std::sync::OnceLock::new();

    /// Keep the install-time sweep's answer (the first call wins).
    pub(super) fn keep_swept(swept: Vec<MarkerName>) {
        let _ = SWEPT_AT_INSTALL.set(swept);
    }

    /// The dead, empty markers of other processes this launch's install-time sweep removed.
    #[must_use]
    pub(crate) fn swept_at_install() -> &'static [MarkerName] {
        SWEPT_AT_INSTALL.get().map_or(&[], Vec::as_slice)
    }

    /// The dead, empty [`MarkerOwner::App`] markers in `dir` another process left.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn dead_app_markers(dir: &Path, own_pid: u32) -> Vec<PathBuf> {
        dead_markers_of(dir, own_pid, MarkerOwner::App)
    }

    /// Dead, empty markers of `owner` in `dir` ([`dead_app_markers`], [`MarkerOwner::Lab`]).
    #[must_use]
    pub(crate) fn dead_markers_of(dir: &Path, own_pid: u32, owner: MarkerOwner) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                name.to_str()
                    .and_then(parse)
                    .is_some_and(|marker| marker.owner == owner && marker.pid != own_pid)
                    && empty_regular_file(entry)
                    && probe(&entry.path()) == Owner::Dead
            })
            .map(|entry| entry.path())
            .collect()
    }

    /// Create `dir/<name>` `0600`, locked before it is visible under that name.
    pub(crate) fn create_locked(dir: &Path, name: &str) -> Option<(i32, PathBuf)> {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::io::IntoRawFd as _;
        let path = dir.join(name);
        let pending = dir.join(format!(".{name}{PENDING_SUFFIX}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&pending)
            .ok()?;
        let fd = file.into_raw_fd();
        // SAFETY: `fd` is the newly opened descriptor; LOCK_NB never waits.
        #[expect(
            clippy::disallowed_methods,
            reason = "the marker's lock, held for life; a failed arming lets go by LOCK_UN below"
        )]
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !locked || std::fs::rename(&pending, &path).is_err() {
            let _ = std::fs::remove_file(&pending);
            // SAFETY: unlocks and closes the descriptor opened above once.
            #[expect(
                clippy::disallowed_methods,
                reason = "the release: LOCK_UN before the close"
            )]
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
            return None;
        }
        Some((fd, path))
    }

    /// This launch's marker, as [`arm`] made it.
    #[derive(Debug)]
    pub(crate) struct Armed {
        /// The open descriptor the fatal-signal handler writes to.
        pub(crate) fd: i32,
        /// Its name in the log dir.
        pub(crate) path: PathBuf,
        /// Whether the owner lock is held.
        pub(crate) locked: bool,
    }

    /// Arm this launch's marker in `dir` ([`create_locked`]).
    pub(crate) fn arm(dir: &Path, pid: u32, nanos: u128, owner: MarkerOwner) -> Option<Armed> {
        if let Some((fd, path)) = create_locked(dir, &file_name(pid, nanos, owner)) {
            return Some(Armed {
                fd,
                path,
                locked: true,
            });
        }
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::io::IntoRawFd as _;
        let path = dir.join(file_name(pid, nanos, MarkerOwner::Other));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .ok()?;
        Some(Armed {
            fd: file.into_raw_fd(),
            path,
            locked: false,
        })
    }

    /// The same marker, named for `owner` instead.
    #[must_use]
    pub(crate) fn renamed_for(path: &Path, owner: MarkerOwner) -> Option<PathBuf> {
        let name = path.file_name()?.to_str()?;
        let (stem, _) = name.strip_suffix(".log")?.rsplit_once('-')?;
        Some(path.with_file_name(format!("{stem}-{}.log", owner.tag())))
    }
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering};

    /// Fatal signals we trap.
    const FATAL_SIGNALS: [i32; 5] = [
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];

    /// Pre-built banner halves around the signal number.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal signal ";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// fd of the pre-opened crash-marker file.
    static MARKER_FD: AtomicI32 = AtomicI32::new(-1);

    /// Tripped once we have armed the `sigaction` handlers.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Leaked NUL-terminated marker path for clean-exit `unlink` ([`release_own_marker`]).
    static MARKER_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    /// The pid that armed the marker ([`release_own_marker`]).
    static OWNER_PID: AtomicI32 = AtomicI32::new(0);

    /// Set for [`super::Arming::AppAfterCommit`] until [`adopt_app_identity`] renames the marker.
    static PROMOTE_ON_COMMIT: AtomicBool = AtomicBool::new(false);

    /// The `<nanos>` this process's marker is named with, once armed.
    static MARKER_NANOS: AtomicU64 = AtomicU64::new(0);

    /// See [`super::own_marker_nanos`].
    pub(super) fn own_marker_nanos() -> Option<u128> {
        match MARKER_NANOS.load(Ordering::SeqCst) {
            0 => None,
            nanos => Some(u128::from(nanos)),
        }
    }

    /// See [`super::own_marker_place`].
    pub(super) fn own_marker_place() -> Option<(std::path::PathBuf, u128)> {
        let nanos = own_marker_nanos()?;
        let current = MARKER_PATH.load(Ordering::SeqCst);
        if current.is_null() {
            return None;
        }
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: a non-null MARKER_PATH is a leaked, never-freed CString (`publish_marker_path`).
        let current = unsafe { std::ffi::CStr::from_ptr(current) };
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(current.to_bytes()));
        Some((path.parent()?.to_path_buf(), nanos))
    }

    /// Park `path` in [`MARKER_PATH`] (leaking the string: it must outlive every exit path).
    fn publish_marker_path(path: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        MARKER_PATH.store(c_path.into_raw(), Ordering::SeqCst);
        true
    }

    /// See [`super::release_own_marker`].
    pub(super) fn release_own_marker() {
        // SAFETY: `getpid` is async-signal-safe and has no preconditions.
        if unsafe { libc::getpid() } != OWNER_PID.load(Ordering::SeqCst) {
            return;
        }
        let path = MARKER_PATH.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !path.is_null() {
            // SAFETY: a non-null MARKER_PATH is a leaked CString; `unlink` is async-signal-safe.
            unsafe { libc::unlink(path) };
        }
    }

    /// The `atexit` half of [`release_own_marker`].
    extern "C" fn release_at_exit() {
        release_own_marker();
    }

    unsafe extern "C" {
        /// `stdlib.h`: register a handler for `exit(3)` ([`super::clean_exit_now`]).
        fn atexit(cb: extern "C" fn()) -> libc::c_int;
    }

    /// See [`super::adopt_app_identity`].
    pub(super) fn adopt_app_identity() -> bool {
        if !PROMOTE_ON_COMMIT.swap(false, Ordering::SeqCst) {
            return false;
        }
        let current = MARKER_PATH.load(Ordering::SeqCst);
        if current.is_null() {
            return true;
        }
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: a non-null MARKER_PATH is a leaked, never-freed CString.
        let current = unsafe { std::ffi::CStr::from_ptr(current) };
        let current = std::path::Path::new(std::ffi::OsStr::from_bytes(current.to_bytes()));
        let Some(app) = super::markers::renamed_for(current, super::MarkerOwner::App) else {
            return true;
        };
        if std::fs::rename(current, &app).is_ok() {
            publish_marker_path(&app);
        }
        true
    }

    /// Maximum decimal digits of a `u32` (`4294967295`).
    const U32_MAX_DIGITS: usize = 10;

    /// Format `value` as decimal ASCII into the tail of `buf`, returning the filled sub-slice.
    fn fmt_u32_decimal(value: u32, buf: &mut [u8; U32_MAX_DIGITS]) -> &[u8] {
        let mut n = value;
        let mut pos = buf.len();
        loop {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        &buf[pos..]
    }

    /// Async-signal-safe `write(2)` of `bytes` to `fd`.
    fn write_all_signal_safe(fd: i32, bytes: &[u8]) {
        if fd < 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            // SAFETY: `bytes[off..]` is live for the call and `fd` is non-negative.
            let n = unsafe {
                libc::write(
                    fd,
                    bytes[off..].as_ptr().cast::<libc::c_void>(),
                    bytes.len() - off,
                )
            };
            if n > 0 {
                off += n as usize;
                continue;
            }
            if n < 0 {
                // SAFETY: `errno_location()` returns a valid, thread-local `*mut c_int`.
                let err = unsafe { *errno_location() };
                if err == libc::EINTR {
                    continue;
                }
            }
            break;
        }
    }

    /// # Safety The returned pointer is valid for the calling thread only.
    /// # Safety
    /// The returned pointer is valid for the calling thread only.
    #[inline]
    unsafe fn errno_location() -> *mut libc::c_int {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: `__error` returns this thread's errno address.
            unsafe { libc::__error() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // SAFETY: `__errno_location` returns this thread's errno address.
            unsafe { libc::__errno_location() }
        }
    }

    /// The fatal-signal handler, installed without `SA_SIGINFO` (it gets only the signal number).
    extern "C" fn handle_fatal_signal(sig: libc::c_int) {
        let mut digits = [0u8; U32_MAX_DIGITS];
        let num = fmt_u32_decimal(sig.max(0) as u32, &mut digits);

        let marker_fd = MARKER_FD.load(Ordering::Relaxed);
        for fd in [libc::STDERR_FILENO, marker_fd] {
            write_all_signal_safe(fd, BANNER_PREFIX);
            write_all_signal_safe(fd, num);
            write_all_signal_safe(fd, BANNER_SUFFIX);
        }

        // SAFETY: `act` is a zeroed `sigaction` with `SIG_DFL`.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
            #[expect(
                clippy::disallowed_methods,
                reason = "a real crash: its own fatal signal re-raised with the default action, as if never trapped"
            )]
            libc::raise(sig);
        }
    }

    /// Open this launch's crash marker ([`super::markers`]).
    fn open_marker_fd(owner: super::MarkerOwner) -> i32 {
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        let pid = std::process::id();
        super::markers::keep_swept(super::markers::sweep(&dir, pid, owner));
        let nanos = super::startup_nanos();
        match super::markers::arm(&dir, pid, nanos, owner) {
            Some(armed) if publish_marker_path(&armed.path) => {
                MARKER_NANOS.store(u64::try_from(nanos).unwrap_or(0), Ordering::SeqCst);
                if !armed.locked {
                    PROMOTE_ON_COMMIT.store(false, Ordering::SeqCst);
                }
                armed.fd
            }
            Some(armed) => {
                let _ = std::fs::remove_file(&armed.path);
                // SAFETY: unlocks and closes the descriptor `arm` returned once.
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the release: LOCK_UN before the close"
                )]
                unsafe {
                    libc::flock(armed.fd, libc::LOCK_UN);
                    libc::close(armed.fd);
                }
                -1
            }
            None => -1,
        }
    }

    /// Arm async-signal-safe capture of the fatal signals.
    pub(super) fn install_signal_handlers(arming: super::Arming) {
        if ARMED.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: `getpid` has no preconditions.
        OWNER_PID.store(unsafe { libc::getpid() }, Ordering::SeqCst);
        PROMOTE_ON_COMMIT.store(arming == super::Arming::AppAfterCommit, Ordering::SeqCst);
        let fd = open_marker_fd(arming.initial_owner());
        MARKER_FD.store(fd, Ordering::SeqCst);
        if fd >= 0 {
            // SAFETY: registers a plain `extern "C"` function with no captured state.
            unsafe { atexit(release_at_exit) };
        }

        for &sig in &FATAL_SIGNALS {
            // SAFETY: `act` is a valid `sigaction` registered outside signal context.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = handle_fatal_signal as *const () as usize;
                act.sa_flags = libc::SA_RESTART | libc::SA_ONSTACK;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn render(value: u32) -> String {
            let mut buf = [0u8; U32_MAX_DIGITS];
            let bytes = fmt_u32_decimal(value, &mut buf);
            String::from_utf8(bytes.to_vec()).unwrap()
        }

        #[test]
        fn decimal_formatter_matches_std_for_many_values() {
            for v in 0u32..2000 {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                9,
                10,
                11,
                99,
                100,
                255,
                999,
                1000,
                65_535,
                1_000_000,
                4_294_967_294,
                u32::MAX,
            ] {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
        }
    }
}

/// The marker lifecycle's rules, each against real files, real `flock`s and
/// real processes in a scratch dir — never the user's log dir.
#[cfg(all(test, unix))]
mod marker_tests {
    use super::MarkerOwner;
    use super::markers::{self, Owner};
    use crate::crash_journal::tests::{reaped_pid, scratch};
    use std::path::Path;

    fn let_go(fd: i32) {
        // SAFETY: `fd` is an open descriptor owned by the caller; LOCK_UN never waits.
        #[expect(
            clippy::disallowed_methods,
            reason = "the release: LOCK_UN before the close"
        )]
        unsafe {
            libc::flock(fd, libc::LOCK_UN)
        };
        // SAFETY: closes the descriptor once.
        unsafe { libc::close(fd) };
    }

    fn dead_marker(dir: &Path, pid: u32, owner: MarkerOwner) -> std::path::PathBuf {
        let path = dir.join(markers::file_name(pid, 7, owner));
        std::fs::write(&path, b"").unwrap();
        path
    }

    fn sweep_as_before(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("crash-")
                && name.ends_with(".log")
                && entry.metadata().is_ok_and(|m| m.len() == 0)
            {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
    }

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        let name = markers::file_name(4242, 1_790_309_523_499_676_000, MarkerOwner::App);
        assert_eq!(name, "crash-marker-4242-1790309523499676000-app.log");
        assert_eq!(
            markers::parse(&name),
            Some(markers::MarkerName {
                pid: 4242,
                nanos: 1_790_309_523_499_676_000,
                owner: MarkerOwner::App
            })
        );
        let other = markers::file_name(1, 2, MarkerOwner::Other);
        assert_eq!(markers::parse(&other).unwrap().owner, MarkerOwner::Other);
        let lab = markers::file_name(1, 2, MarkerOwner::Lab);
        assert_eq!(lab, "crash-marker-1-2-lab.log");
        assert_eq!(markers::parse(&lab).unwrap().owner, MarkerOwner::Lab);
        for stranger in [
            "crash-signal-4242-7.log",
            "crash-marker-4242-7-app.log.seen",
            ".crash-marker-4242-7-app.log.pending",
            "crash-marker-x-7-app.log",
            "crash-marker-4242--app.log",
            "crash-marker-4242-7-root.log",
            "crash-4242.log",
            "aterm.log",
        ] {
            assert_eq!(markers::parse(stranger), None, "{stranger}");
        }
        let path = Path::new("/l").join(&other);
        assert_eq!(
            markers::renamed_for(&path, MarkerOwner::App).unwrap(),
            Path::new("/l/crash-marker-1-2-app.log")
        );
    }

    #[test]
    fn a_sweep_keeps_a_live_owners_marker_and_removes_a_dead_ones() {
        let dir = scratch("marker-live");
        let own = std::process::id();
        let (fd, live) =
            markers::create_locked(dir.path(), &markers::file_name(own, 1, MarkerOwner::Other))
                .expect("arm a locked marker");
        let dead = dead_marker(dir.path(), reaped_pid(), MarkerOwner::Other);
        assert_eq!(markers::probe(&live), Owner::Live);
        assert_eq!(markers::probe(&dead), Owner::Dead);

        markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
        assert!(
            live.exists(),
            "a live owner's marker must survive the sweep"
        );
        assert!(!dead.exists(), "a dead owner's empty marker is swept");

        sweep_as_before(dir.path());
        assert!(
            !live.exists(),
            "control: the old sweep unlinks the live marker the new one keeps"
        );
        let_go(fd);
    }

    #[test]
    fn a_launch_that_cannot_lock_still_gets_an_unlocked_marker_that_is_never_news() {
        let dir = scratch("marker-arm");
        let own = std::process::id();
        let armed = markers::arm(dir.path(), own + 11, 5, MarkerOwner::App).expect("armed");
        assert!(armed.locked);
        assert_eq!(
            armed.path,
            dir.path()
                .join(markers::file_name(own + 11, 5, MarkerOwner::App))
        );
        assert_eq!(markers::probe(&armed.path), Owner::Live);
        let_go(armed.fd);
        std::fs::remove_file(&armed.path).unwrap();

        let name = markers::file_name(own + 12, 6, MarkerOwner::App);
        std::fs::create_dir(dir.path().join(format!(".{name}.pending"))).unwrap();
        assert!(
            markers::create_locked(dir.path(), &name).is_none(),
            "control: the locked arming alone leaves this launch without a marker"
        );
        let fallback = markers::arm(dir.path(), own + 12, 6, MarkerOwner::App).expect("fallback");
        assert!(!fallback.locked);
        assert_eq!(
            fallback.path,
            dir.path()
                .join(markers::file_name(own + 12, 6, MarkerOwner::Other)),
            "unlocked, so never named for the daily driver"
        );
        // SAFETY: `fallback.fd` is the open descriptor `arm` returned.
        let wrote = unsafe { libc::write(fallback.fd, b"x".as_ptr().cast(), 1) };
        assert_eq!(wrote, 1);
        assert_eq!(std::fs::read(&fallback.path).unwrap(), b"x");
        std::fs::write(&fallback.path, b"").unwrap();
        assert!(
            markers::dead_app_markers(dir.path(), own).is_empty(),
            "an empty unlocked marker is not a killed daily driver"
        );
        // SAFETY: closes the descriptor `arm` returned once.
        unsafe { libc::close(fallback.fd) };
    }

    #[test]
    fn only_a_dead_app_marker_of_another_process_is_evidence() {
        let dir = scratch("marker-news");
        let own = std::process::id();
        let gone = reaped_pid();
        let killed = dead_marker(dir.path(), gone, MarkerOwner::App);
        let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
        let previous_image = dead_marker(dir.path(), own, MarkerOwner::App);
        let (fd, live_app) = markers::create_locked(
            dir.path(),
            &markers::file_name(own + 7, 3, MarkerOwner::App),
        )
        .unwrap();

        assert_eq!(
            markers::dead_app_markers(dir.path(), own),
            vec![killed.clone()]
        );
        markers::sweep(dir.path(), own, MarkerOwner::Other);
        assert!(killed.exists(), "left for the report");
        assert!(!test_start.exists(), "a test start's corpse is not news");
        assert!(
            !previous_image.exists(),
            "this process's own previous image"
        );
        assert!(live_app.exists());
        std::fs::write(&killed, b"aterm: fatal signal 11").unwrap();
        markers::sweep(dir.path(), own, MarkerOwner::Other);
        assert!(killed.exists());
        assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        let_go(fd);
    }

    #[test]
    fn a_lab_marker_is_news_to_a_lab_desks_sweep_alone() {
        for scan in [MarkerOwner::Lab, MarkerOwner::App, MarkerOwner::Other] {
            let dir = scratch("marker-lab");
            let own = std::process::id();
            let gone = reaped_pid();
            let lab = dead_marker(dir.path(), gone, MarkerOwner::Lab);
            let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
            let previous_image = dead_marker(dir.path(), own, MarkerOwner::Lab);
            assert!(markers::dead_app_markers(dir.path(), own).is_empty());
            markers::sweep(dir.path(), own, scan);
            let news = scan == MarkerOwner::Lab;
            assert_eq!(
                lab.exists(),
                news,
                "a dead lab marker under a {scan:?} sweep"
            );
            assert!(!test_start.exists(), "a test start's corpse is never news");
            assert!(
                !previous_image.exists(),
                "this process's own previous image"
            );
            assert_eq!(
                markers::dead_markers_of(dir.path(), own, MarkerOwner::Lab),
                if news { vec![lab.clone()] } else { Vec::new() }
            );
            assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        }
    }

    #[test]
    fn the_sweep_answers_the_dead_starts_it_removed() {
        let dir = scratch("marker-swept");
        let own = std::process::id();
        let gone = reaped_pid();
        let test_start = dead_marker(dir.path(), gone, MarkerOwner::Other);
        let previous_image = dead_marker(dir.path(), own, MarkerOwner::Other);
        let record = dir.path().join(markers::file_name(
            gone.wrapping_add(1),
            7,
            MarkerOwner::Other,
        ));
        std::fs::write(&record, b"aterm: fatal signal 11").unwrap();
        let (fd, live) = markers::create_locked(
            dir.path(),
            &markers::file_name(own + 3, 3, MarkerOwner::Other),
        )
        .unwrap();

        let swept = markers::sweep(dir.path(), own, MarkerOwner::Other);
        assert_eq!(
            swept,
            vec![markers::MarkerName {
                pid: gone,
                nanos: 7,
                owner: MarkerOwner::Other
            }]
        );
        assert!(
            !test_start.exists(),
            "control: the evidence left the directory"
        );
        assert!(!previous_image.exists());
        assert!(record.exists() && live.exists());
        let_go(fd);
    }

    #[test]
    fn legacy_markers_follow_their_pid_and_pending_ones_their_lock() {
        let dir = scratch("marker-legacy");
        let own = std::process::id();
        let alive = dir.path().join(format!("crash-signal-{own}-5.log"));
        let gone = dir
            .path()
            .join(format!("crash-signal-{}-5.log", reaped_pid()));
        let record = dir
            .path()
            .join(format!("crash-signal-{}-6.log", reaped_pid()));
        std::fs::write(&alive, b"").unwrap();
        std::fs::write(&gone, b"").unwrap();
        std::fs::write(&record, b"aterm: fatal signal 6").unwrap();
        let name = markers::file_name(reaped_pid(), 9, MarkerOwner::Other);
        let stale_pending = dir.path().join(format!(".{name}.pending"));
        std::fs::write(&stale_pending, b"").unwrap();

        markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
        assert!(
            alive.exists(),
            "an older build still running keeps its marker"
        );
        assert!(!gone.exists());
        assert!(record.exists(), "a crash record is never swept");
        assert!(!stale_pending.exists(), "an arming that died mid-way");
    }

    #[test]
    fn a_killed_owner_releases_its_lock() {
        use std::io::Read as _;
        let dir = scratch("marker-kill");
        let Ok(perl) = aterm_uds::spawnfd::command("/usr/bin/perl")
            .arg("-e")
            .arg(
                "use Fcntl qw(:flock); open(my $f, '>', $ARGV[0]) or die; \
                 flock($f, LOCK_EX|LOCK_NB) or die; $|=1; print \"locked\\n\"; sleep 60",
            )
            .arg(dir.path().join(markers::file_name(1, 1, MarkerOwner::App)))
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
        else {
            aterm_spec::host::could_not_run("/usr/bin/perl is not available to hold the lock");
        };
        let mut child = perl;
        let mut ready = [0u8; 7];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .expect("the child locked its marker");
        let path = dir.path().join(markers::file_name(1, 1, MarkerOwner::App));
        assert_eq!(markers::probe(&path), Owner::Live);
        assert!(markers::dead_app_markers(dir.path(), 2).is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(markers::probe(&path), Owner::Dead);
        assert_eq!(markers::dead_app_markers(dir.path(), 2), vec![path]);
    }
}

/// The Windows crash lane: an unhandled-exception filter (`SetUnhandledExceptionFilter`) for
/// native fatal exceptions (access violation `0xC0000005`, illegal instruction, stack overflow),
/// which bypass the panic hook as POSIX fatal signals do.
#[cfg(windows)]
mod imp_windows {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(
            filter: extern "system" fn(*mut ExceptionPointers) -> i32,
        ) -> *mut core::ffi::c_void;
        fn GetStdHandle(which: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn WriteFile(
            handle: isize,
            data: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }

    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    /// Minimal `EXCEPTION_POINTERS`: only `ExceptionRecord` is followed.
    #[repr(C)]
    struct ExceptionPointers {
        exception_record: *mut ExceptionRecord,
        context_record: *mut core::ffi::c_void,
    }

    /// `EXCEPTION_RECORD` with its documented fixed layout: `#[repr(C)]` with the native field
    /// types reproduces the OS struct, including the padding before `exception_information` on
    /// 64-bit.
    #[repr(C)]
    struct ExceptionRecord {
        exception_code: u32,
        exception_flags: u32,
        exception_record: *mut ExceptionRecord,
        exception_address: *mut core::ffi::c_void,
        number_parameters: u32,
        exception_information: [usize; 15],
    }

    const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;

    const BANNER_PREFIX: &[u8] = b"aterm: fatal exception 0x";
    const BANNER_AT: &[u8] = b" at 0x";
    const BANNER_AV_OP: &[u8] = " \u{2014} access violation ".as_bytes();
    const BANNER_AV_AT: &[u8] = b" @ 0x";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// Raw HANDLE of the pre-opened crash-marker file, or `-1` when none was opened. `AtomicIsize`
    /// so the filter reads it without a lock; stored once by `install`, swapped back to `-1` by
    /// `release_marker` at a clean exit.
    pub(super) static MARKER_HANDLE: AtomicIsize = AtomicIsize::new(-1);

    /// This launch's marker path, parked by `open_marker_handle` when the open succeeded, so
    /// [`release_marker`] can find the file (the nanos salt in its name cannot be recomputed).
    pub(super) static MARKER_PATH: std::sync::OnceLock<std::path::PathBuf> =
        std::sync::OnceLock::new();

    static ARMED: AtomicBool = AtomicBool::new(false);

    fn fmt_u32_hex(value: u32, buf: &mut [u8; 8]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((7 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    fn fmt_u64_hex(value: u64, buf: &mut [u8; 16]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((15 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    fn write_all_handle(handle: isize, bytes: &[u8]) {
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            let mut written = 0u32;
            let len = u32::try_from(bytes.len() - off).unwrap_or(u32::MAX);
            // SAFETY: `bytes[off..]` is live and `written` is a valid out-param.
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes[off..].as_ptr(),
                    len,
                    &mut written,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                break;
            }
            off += written as usize;
        }
    }

    extern "system" fn handle_fatal_exception(info: *mut ExceptionPointers) -> i32 {
        let mut code = 0u32;
        let mut address = 0u64;
        let mut av: Option<(&[u8], u64)> = None;
        // SAFETY: `info` is a valid `EXCEPTION_POINTERS` from the OS with guarded pointers.
        unsafe {
            if !info.is_null() {
                let rec = (*info).exception_record;
                if !rec.is_null() {
                    code = (*rec).exception_code;
                    address = (*rec).exception_address as usize as u64;
                    if code == EXCEPTION_ACCESS_VIOLATION && (*rec).number_parameters >= 2 {
                        let op: &[u8] = match (*rec).exception_information[0] {
                            0 => b"reading",
                            1 => b"writing",
                            8 => b"executing",
                            _ => b"accessing",
                        };
                        av = Some((op, (*rec).exception_information[1] as u64));
                    }
                }
            }
        }
        let mut code_digits = [0u8; 8];
        let code_hex = fmt_u32_hex(code, &mut code_digits);
        let mut pc_digits = [0u8; 16];
        let pc_hex = fmt_u64_hex(address, &mut pc_digits);
        let mut av_digits = [0u8; 16];
        let av_hex = av.map(|(_, addr)| fmt_u64_hex(addr, &mut av_digits));

        // SAFETY: `GetStdHandle` is a plain handle query, callable anywhere.
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let marker = MARKER_HANDLE.load(Ordering::Relaxed);
        for h in [stderr, marker] {
            write_all_handle(h, BANNER_PREFIX);
            write_all_handle(h, code_hex);
            write_all_handle(h, BANNER_AT);
            write_all_handle(h, pc_hex);
            if let (Some((op, _)), Some(av_hex)) = (av, av_hex) {
                write_all_handle(h, BANNER_AV_OP);
                write_all_handle(h, op);
                write_all_handle(h, BANNER_AV_AT);
                write_all_handle(h, av_hex);
            }
            write_all_handle(h, BANNER_SUFFIX);
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    fn open_marker_handle() -> isize {
        use std::os::windows::io::IntoRawHandle;
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        super::sweep_stale_markers(&dir);
        let path = super::marker_path(&dir);
        match open_marker_file(&path) {
            Ok(f) => {
                let _ = MARKER_PATH.set(path);
                f.into_raw_handle() as isize
            }
            Err(_) => -1,
        }
    }

    const MARKER_SHARE_MODE: u32 = 0x1 | 0x2;

    /// Create (truncating) the marker at `path`, held against deletion while the returned handle
    /// is open.
    pub(super) fn open_marker_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .share_mode(MARKER_SHARE_MODE)
            .open(path)
    }

    /// Clean-exit release ([`super::remove_marker_on_clean_exit`]): swap the handle out first, so
    /// a racing fault writes to stderr only; close it; then remove the file if still empty.
    pub(super) fn release_marker() {
        let handle = MARKER_HANDLE.swap(-1, Ordering::SeqCst);
        if handle != INVALID_HANDLE_VALUE && handle != 0 {
            // SAFETY: `handle` was leaked from `File` and swapped out once.
            unsafe {
                CloseHandle(handle);
            }
        }
        if let Some(path) = MARKER_PATH.get() {
            super::remove_marker_if_empty(path);
        }
    }

    /// Arm the unhandled-exception filter. Idempotent: the marker handle is
    /// opened (and the filter registered) only on the first call.
    pub(crate) fn install() {
        if ARMED.swap(true, Ordering::SeqCst) {
            return;
        }
        MARKER_HANDLE.store(open_marker_handle(), Ordering::SeqCst);
        // SAFETY: registers a plain `extern "system"` filter function.
        unsafe {
            SetUnhandledExceptionFilter(handle_fatal_exception);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn render(value: u32) -> String {
            let mut buf = [0u8; 8];
            String::from_utf8(fmt_u32_hex(value, &mut buf).to_vec()).unwrap()
        }

        #[test]
        fn hex_formatter_matches_std_for_many_values() {
            for v in 0u32..2000 {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                0x8000_0003, // breakpoint
                0xC000_0005,
                0xC000_00FD,
                u32::MAX,
            ] {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v:#010X}");
            }
        }

        #[test]
        fn hex64_formatter_matches_std() {
            for v in [0u64, 1, 0xDEAD_BEEF, 0x7FF6_1234_5678_ABCD, u64::MAX] {
                let mut buf = [0u8; 16];
                assert_eq!(
                    String::from_utf8(fmt_u64_hex(v, &mut buf).to_vec()).unwrap(),
                    format!("{v:016X}"),
                    "mismatch at {v:#018X}"
                );
            }
        }

        #[cfg(target_pointer_width = "64")]
        #[test]
        fn exception_record_layout_matches_native_offsets() {
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_code), 0);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_flags), 4);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_record), 8);
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_address),
                16
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, number_parameters),
                24
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_information),
                32
            );
        }

        #[test]
        fn install_is_idempotent() {
            let _one_at_a_time = super::super::PROCESS_MARKER_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            install();
            install();
            assert!(ARMED.load(Ordering::SeqCst));
        }

        #[test]
        fn a_held_marker_survives_the_sweep_and_goes_once_released() {
            let dir =
                std::env::temp_dir().join(format!("aterm-marker-lock-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            let held = dir.join("crash-signal-1-1.log");
            let file = open_marker_file(&held).expect("marker opened");
            super::super::sweep_stale_markers(&dir);
            assert!(held.exists(), "a live marker survives a sibling's sweep");
            let refused = std::fs::remove_file(&held).expect_err("no delete while held");
            assert_eq!(refused.raw_os_error(), Some(32), "a sharing violation");
            let readable = std::fs::read(&held).expect("the crash scan can still read it");
            assert!(readable.is_empty());
            drop(file);
            super::super::sweep_stale_markers(&dir);
            assert!(!held.exists(), "released, it is an ordinary empty leftover");

            let unheld = dir.join("crash-signal-2-2.log");
            let default_open = std::fs::File::create(&unheld).expect("default open");
            super::super::sweep_stale_markers(&dir);
            assert!(
                !unheld.exists(),
                "control: the default share mode lets a sweep unlink a live marker"
            );
            drop(default_open);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// This test process has ONE marker (`imp_windows`' statics) and two tests
/// drive it: `install_is_idempotent` arms it, the clean-exit test releases it.
#[cfg(all(test, windows))]
static PROCESS_MARKER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The Windows clean-exit release, tested at the seam both platforms share:
/// the rule, the removal (plain `std`, so every host runs it), and the harmless
/// no-op when nothing was armed.
#[cfg(all(test, any(unix, windows)))]
mod clean_exit_tests {
    use super::*;

    #[test]
    fn an_empty_marker_is_removed_at_once_and_a_crash_record_is_kept() {
        let dir = std::env::temp_dir().join(format!("aterm-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("crash-signal-1-1.log");
        let crashed = dir.join("crash-signal-2-2.log");
        std::fs::write(&clean, b"").unwrap();
        std::fs::write(&crashed, "aterm: fatal signal 11\n").unwrap();

        assert!(remove_marker_if_empty(&clean), "an empty marker is removed");
        assert!(!clean.exists(), "gone at once, not merely delete-pending");
        assert!(!remove_marker_if_empty(&crashed), "a crash record is kept");
        assert!(crashed.exists());
        assert!(!remove_marker_if_empty(&dir.join("crash-signal-3-3.log")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_without_an_armed_marker_is_a_harmless_no_op() {
        #[cfg(windows)]
        let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        remove_marker_on_clean_exit();
        remove_marker_on_clean_exit();
        #[cfg(windows)]
        assert_eq!(
            imp_windows::MARKER_HANDLE.load(std::sync::atomic::Ordering::SeqCst),
            -1
        );
    }

    /// The clean exit end to end, armed for real in the real log dir: while armed the marker is
    /// held against deletion, and the release closes the handle and removes the file.
    #[cfg(windows)]
    #[test]
    fn the_clean_exit_takes_the_armed_marker_with_it() {
        use std::sync::atomic::Ordering;
        let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        imp_windows::install();
        let path = imp_windows::MARKER_PATH.get().cloned();
        if imp_windows::MARKER_HANDLE.load(Ordering::SeqCst) != -1 {
            let path = path.as_ref().expect("an open marker has its path parked");
            assert!(path.exists(), "armed: {}", path.display());
            assert!(
                std::fs::remove_file(path).is_err(),
                "held against a sibling's sweep"
            );
        }
        remove_marker_on_clean_exit();
        assert_eq!(imp_windows::MARKER_HANDLE.load(Ordering::SeqCst), -1);
        if let Some(path) = path {
            assert!(
                !path.exists(),
                "the clean exit took its own marker: {}",
                path.display()
            );
        }
    }
}
