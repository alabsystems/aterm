// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX PTY backend: [`open_pty_pair_cloexec`], `fork`, `login_tty`, `execve`, and tty ioctls.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{ShellHangup, ShellIdentity, SpawnedShell, build_child_env};
use aterm_containment::sbpl::{SANDBOX_EXEC_PATH, build_sandbox_wrap};

/// [`spawn_shell_with_pid`] minus the pid, with the hardened default limits.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_shell(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
) -> io::Result<i32> {
    spawn_shell_with_pid_cell_px(
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        aterm_sandbox::Limits::shell_default(),
        None,
        None,
    )
    .map(|s| s.master)
}

const SPAWN_UTF8_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

fn is_utf8_locale(loc: &str) -> bool {
    let Some(dot) = loc.rfind('.') else {
        return false;
    };
    let codeset = loc
        .get(dot.saturating_add(1)..)
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let norm: String = codeset
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    norm == "utf8"
}

/// Returns locale overrides ensuring the spawned child runs under a UTF-8 `LC_CTYPE`.
#[must_use]
pub fn resolve_spawn_locale(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> Vec<(String, String)> {
    fn set(o: Option<&str>) -> Option<&str> {
        o.filter(|s| !s.is_empty())
    }
    let effective = set(lc_all).or(set(lc_ctype)).or(set(lang));
    if effective.is_some_and(is_utf8_locale) {
        return Vec::new();
    }
    let mut overrides = vec![("LC_CTYPE".to_string(), SPAWN_UTF8_LOCALE.to_string())];
    if set(lc_all).is_some() {
        overrides.push(("LC_ALL".to_string(), String::new()));
    }
    overrides
}

const PTS_NAME_LEN: usize = 128;

fn pts_name(master: libc::c_int) -> io::Result<CString> {
    let mut buf = [0 as libc::c_char; PTS_NAME_LEN];
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    // SAFETY: `TIOCPTYGNAME` is an `_IOC_OUT` ioctl whose payload size is exactly
    // `PTS_NAME_LEN`; `buf` is a live, owned buffer of precisely that size, and the kernel
    // NUL-terminates the name it writes into it.
    let rc = unsafe {
        libc::ioctl(
            master,
            libc::TIOCPTYGNAME as libc::c_ulong,
            buf.as_mut_ptr(),
        )
    };
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    // SAFETY: `buf` is a live owned buffer and we pass its exact length; `ptsname_r` either
    // NUL-terminates within that length or fails (ERANGE).
    let rc = unsafe { libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    if rc != 0 {
        return Err(ptsname_r_error(rc));
    }
    // SAFETY: `rc == 0` guarantees `buf` holds a NUL-terminated string live for this borrow.
    Ok(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
fn ptsname_r_error(rc: libc::c_int) -> io::Error {
    if rc > 0 {
        io::Error::from_raw_os_error(rc)
    } else {
        io::Error::last_os_error()
    }
}

fn open_pts_slave(master: libc::c_int) -> io::Result<libc::c_int> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `master` is a live ptmx fd and `TIOCGPTPEER` takes its flag word by value.
        let peer = unsafe {
            libc::ioctl(
                master,
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if peer >= 0 {
            return Ok(peer);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const NAME_STEP: &str = "ioctl(TIOCPTYGNAME)";
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    const NAME_STEP: &str = "ptsname_r";
    let name = pts_name(master).map_err(|err| pty_step_failed(NAME_STEP, err))?;
    #[cfg(all(test, target_os = "macos"))]
    let name = pty_race::opening_slave(name);
    // SAFETY: `name` is a NUL-terminated pts path; `open` reads it and returns a fresh fd or -1.
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(pty_step_failed(SLAVE_OPEN, io::Error::last_os_error()));
    }
    Ok(slave)
}

/// The step [`open_pts_slave`] names when the open of the slave by its name fails.
const SLAVE_OPEN: &str = "open of the slave";

/// Whether `err` is [`open_pts_slave`]'s open of the slave by name finding no node there
/// (ENOENT), as under ATERM-KI-1028-4's pty churn ([`open_pty_pair_cloexec`]).
fn slave_node_gone(err: &io::Error) -> bool {
    err.get_ref()
        .and_then(|e| e.downcast_ref::<PtyOpenError>())
        .is_some_and(|e| e.step == SLAVE_OPEN && e.os.raw_os_error() == Some(libc::ENOENT))
}

fn fd_is_cloexec_or_make_it(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is an open owned descriptor; `F_GETFD`/`F_SETFD` only access descriptor flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 {
            return false;
        }
        if flags & libc::FD_CLOEXEC != 0 {
            return true;
        }
        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) != -1
    }
}

#[derive(Debug)]
struct PtyOpenError {
    step: &'static str,
    os: io::Error,
}

impl std::fmt::Display for PtyOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.step, self.os)
    }
}

impl std::error::Error for PtyOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.os)
    }
}

fn pty_step_failed(step: &'static str, os: io::Error) -> io::Error {
    io::Error::new(os.kind(), PtyOpenError { step, os })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
const XNU_EREDRIVEOPEN: libc::c_int = -6;

#[cfg(any(target_os = "macos", target_os = "ios"))]
const PTMX_REDRIVES: u32 = 8;

fn open_ptmx(mut open: impl FnMut(libc::c_int) -> libc::c_int) -> io::Result<libc::c_int> {
    let mut flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let mut redrives = 0;
    loop {
        let fd = open(flags);
        if fd >= 0 {
            return Ok(fd);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINVAL) if flags & libc::O_CLOEXEC != 0 => flags &= !libc::O_CLOEXEC,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Some(XNU_EREDRIVEOPEN) if redrives < PTMX_REDRIVES => redrives += 1,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Some(XNU_EREDRIVEOPEN) => {
                return Err(pty_step_failed(
                    "posix_openpt lost the kernel's pty allocation race on every redrive \
                     (XNU EREDRIVEOPEN; no limit was reached)",
                    err,
                ));
            }
            _ => return Err(pty_step_failed("posix_openpt", err)),
        }
    }
}

/// How many ptys one pty open replaces (one whose slave's node is gone as it opens, or on Apple
/// one whose `grantpt` was let go) before it is an error ([`open_pty_pair_cloexec`]).
const GONE_PTY_REPLACEMENTS: usize = 1;

/// Opens a `(master, slave)` pty pair, both ends close-on-exec, `termp` and `winp` applied to
/// the slave.
///
/// On Apple the slave opens by its name, and under ATERM-KI-1028-4's pty churn that open can
/// find no node there (ENOENT): a new pty left with no `/dev` node, as macOS 27.0.1 was seen to
/// leave one while its master was held, or an open racing the revoke of a terminal on the same
/// device, which `vn_open_auth` gives up on with ENOENT. Such a pty is held open while one fresh
/// pair opens, then closed: XNU hands out the lowest free number, so closed first, its number
/// would go straight to the fresh pair. A second is an error naming both. (Linux opens the slave
/// through `TIOCGPTPEER`, by name only where a kernel predates it.)
///
/// On Apple a pty whose `grantpt` has not returned after [`GRANT_BOUND`] and goes on burning its
/// thread's CPU ([`GRANT_SPIN`]) is let go ([`grant_watch`]): XNU's spins forever on a pty whose
/// node could not be made because its name was still another pty's. It is replaced the same
/// way, held open meanwhile. A grant that is only late (its thread off its core) is waited for.
///
/// # Errors
/// The failed step's own error (`posix_openpt`, `FD_CLOEXEC`, `grantpt`, `unlockpt`, the slave's
/// name or its open), with nothing left open; for a second pty whose slave's node was gone or
/// whose grant was let go (`TimedOut`), or the replacement's own failure, that error naming the
/// gone one, the gone ptys closed.
fn open_pty_pair_cloexec(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
) -> io::Result<(libc::c_int, libc::c_int)> {
    open_pty_pair_replacing(termp, winp, GONE_PTY_REPLACEMENTS)
}

/// [`open_pty_pair_cloexec`], replacing up to `replacements` ptys whose slave's node is gone or,
/// on Apple, whose `grantpt` was let go ([`grant_watch`]), from one budget.
fn open_pty_pair_replacing(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
    replacements: usize,
) -> io::Result<(libc::c_int, libc::c_int)> {
    // SAFETY: `posix_openpt` takes only a flag word and returns a fresh fd or -1.
    let master = open_ptmx(|flags| unsafe { libc::posix_openpt(flags) })?;
    if !fd_is_cloexec_or_make_it(master) {
        let err = pty_step_failed("FD_CLOEXEC on the master", io::Error::last_os_error());
        // SAFETY: closing the master we just opened; nothing else exists yet.
        unsafe {
            libc::close(master);
        }
        return Err(err);
    }

    // A test's grant that spins as XNU's does (`pty_race::grants_spin`), or `grantpt`.
    let grant = |m: libc::c_int| {
        #[cfg(all(test, target_os = "macos"))]
        if pty_race::grant_spins(m) {
            return pty_race::spin_grant(m);
        }
        #[cfg(all(test, target_os = "macos"))]
        if let Some(nap) = pty_race::grant_naps(m) {
            std::thread::sleep(nap);
        }
        // SAFETY: `m` is a live ptmx fd; `grantpt` takes no pointer argument.
        unsafe { libc::grantpt(m) }
    };
    #[cfg(target_vendor = "apple")]
    let granted = match grant_watch::grant(master, grant) {
        grant_watch::Granted::Returned(granted) => granted,
        grant_watch::Granted::LetGo { held } => {
            #[cfg(all(test, target_os = "macos"))]
            pty_race::let_go_slot(master);
            // SAFETY: `master` holds the watchdog's pipe now, a descriptor this call owns; the
            // pty is `held`.
            unsafe {
                libc::close(master);
            }
            let gone = format!(
                "{}, whose grantpt ran past {GRANT_BOUND:?} and then burnt {GRANT_SPIN:?} more of \
                 its thread's CPU, as XNU's spins on a pty left with no node of its own \
                 (ATERM-KI-1028-4)",
                held.map_or_else(|| "a pty".to_owned(), pty_label)
            );
            let err = io::Error::new(io::ErrorKind::TimedOut, "grantpt did not return");
            return open_in_place_of(held, &gone, err, termp, winp, replacements);
        }
    };
    #[cfg(not(target_vendor = "apple"))]
    let granted = if grant(master) == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    let refused = match granted {
        Err(err) => Some(("grantpt", err)),
        // SAFETY: `master` is a live ptmx fd; `unlockpt` takes no pointer argument.
        Ok(()) if unsafe { libc::unlockpt(master) } != 0 => {
            Some(("unlockpt", io::Error::last_os_error()))
        }
        Ok(()) => None,
    };
    if let Some((step, err)) = refused {
        let err = pty_step_failed(step, err);
        // SAFETY: closing the master we opened above.
        unsafe {
            libc::close(master);
        }
        return Err(err);
    }

    let slave = match open_pts_slave(master) {
        Ok(s) => s,
        Err(err) if slave_node_gone(&err) => {
            let gone = format!(
                "{}, whose slave's open found no node (ENOENT), as under heavy pty churn \
                 (ATERM-KI-1028-4)",
                pty_label(master)
            );
            return open_in_place_of(Some(master), &gone, err, termp, winp, replacements);
        }
        Err(err) => {
            // SAFETY: closing the master we opened above; no slave exists yet.
            unsafe {
                libc::close(master);
            }
            return Err(err);
        }
    };
    if !fd_is_cloexec_or_make_it(slave) {
        let err = pty_step_failed("FD_CLOEXEC on the slave", io::Error::last_os_error());
        // SAFETY: closing both ends we opened above.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
        return Err(err);
    }

    if let Some(t) = termp {
        // SAFETY: `t` is an initialized `termios` and `slave` is a live pts fd.
        unsafe {
            libc::tcsetattr(slave, libc::TCSAFLUSH, ptr::from_ref(t));
        }
    }
    if let Some(w) = winp {
        // SAFETY: `w` is an initialized `winsize` read by `TIOCSWINSZ` on `slave`.
        unsafe {
            libc::ioctl(slave, libc::TIOCSWINSZ, ptr::from_ref(w));
        }
    }
    Ok((master, slave))
}

/// A fresh pair opened in place of `gone`, the pty `what` describes, which is held open until
/// that pair is (XNU hands out the lowest free number, so closed first, its number would go
/// straight to the fresh pair) and then closed: up to `replacements` more such replacements
/// within, and with none left `err` naming `what`.
fn open_in_place_of(
    gone: Option<libc::c_int>,
    what: &str,
    err: io::Error,
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
    replacements: usize,
) -> io::Result<(libc::c_int, libc::c_int)> {
    let fresh = replacements
        .checked_sub(1)
        .map(|left| open_pty_pair_replacing(termp, winp, left));
    #[cfg(all(test, target_os = "macos"))]
    pty_race::replaced(gone);
    if let Some(gone) = gone {
        // SAFETY: `gone` is a master the caller hands over to close, once the fresh pair (if
        // any) is open; no slave of it is open here.
        unsafe {
            libc::close(gone);
        }
    }
    match fresh {
        Some(Ok(pair)) => Ok(pair),
        Some(Err(err)) => Err(io::Error::new(
            err.kind(),
            format!("{err} (opening a pty in place of {what})"),
        )),
        None => Err(io::Error::new(err.kind(), format!("{err}: {what}"))),
    }
}

/// How long a pty's `grantpt` runs on Apple before [`grant_watch`] first looks at it: a grant
/// returns in microseconds of its thread's time, so one still running past this is either in
/// ATERM-KI-1028-4's livelock or on a thread the scheduler has kept off its core.
#[cfg(target_vendor = "apple")]
pub(crate) const GRANT_BOUND: std::time::Duration = std::time::Duration::from_secs(1);

/// How much of its thread's CPU time a grant past [`GRANT_BOUND`] must go on to burn, still not
/// returned, before [`grant_watch`] lets it go. XNU's spin burns its core for as long as it
/// runs; a healthy grant uses microseconds, and one that is only late (its thread descheduled
/// under load, paging in, or its process stopped) burns none while it waits.
#[cfg(target_vendor = "apple")]
pub(crate) const GRANT_SPIN: std::time::Duration = std::time::Duration::from_millis(50);

/// A bound on a pty's `grantpt` on Apple. XNU's `TIOCPTYGRANT` on a pty whose `/dev` node could
/// not be made (`devfs_make_node` found its name still another pty's, ATERM-KI-1028-4) calls
/// `_devfs_setattr` on a NULL handle, which answers `ERESTART` at once, and the kernel backs the
/// thread's pc up to the `svc` before it returns to user mode: the thread re-enters that one
/// call forever, burning its core (published xnu-12377; the same code in macOS 27.0.1's kernel,
/// read in its binary 2026-11-04). A signal handler returns into the same `svc`, `SA_RESTART`
/// or not; no timer, thread call or `thread_abort` ends it short of the process's death; a
/// `close` of the descriptor ends it but frees the number for another open to take.
///
/// So one watchdog thread looks at each grant still running past [`GRANT_BOUND`] and lets it go
/// once it has burnt [`GRANT_SPIN`] more of its thread's CPU time (a grant that is only late
/// burns none, and is never let go, however long it takes). It keeps the pty open on a
/// descriptor of its own and puts a pipe in the master's slot (`dup2`, which keeps the slot
/// reserved throughout): each pass re-enters the call through the slot, so the next finds the
/// pipe (`ENOTTY`) or the slot mid-swap (`EBADF`), the grant returns, and the number was never
/// free. Where no descriptor can be had to keep the pty (`EMFILE`), the swap still ends the
/// spin, closing the pty and so freeing its number.
///
/// Every grant's watch lives under the watchdog's one lock, and the watchdog swaps a slot only
/// while it holds it, so an opener that takes the lock once its grant returned finds its fate
/// settled. A grant wakes the watchdog only when it sleeps with no deadline (or one past this
/// grant's bound), and once a grant has begun the watchdog keeps a deadline one bound away, so
/// a burst of opens wakes it about once a bound, not once an open. A child of `fork`,
/// which has no watchdog thread (and whose copy of the lock another thread may have held), runs
/// its grants unbounded, as `grantpt` alone does; so does a process whose watchdog could not
/// start.
#[cfg(target_vendor = "apple")]
mod grant_watch {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    /// How often the watchdog looks again at a grant past [`super::GRANT_BOUND`].
    const LOOK_EVERY: Duration = Duration::from_millis(100);

    type MachPort = u32;

    unsafe extern "C" {
        fn pthread_self() -> libc::pthread_t;
        fn pthread_mach_thread_np(thread: libc::pthread_t) -> MachPort;
        fn thread_info(
            thread: MachPort,
            flavor: u32,
            info: *mut i32,
            count: *mut u32,
        ) -> libc::c_int;
        fn pthread_atfork(
            prepare: Option<extern "C" fn()>,
            parent: Option<extern "C" fn()>,
            child: Option<extern "C" fn()>,
        ) -> libc::c_int;
    }

    const THREAD_BASIC_INFO: u32 = 3;
    const THREAD_BASIC_INFO_COUNT: u32 = 10;

    /// What became of one grant.
    pub(super) enum Granted {
        /// It returned by itself (or the swap failed): `grantpt`'s result, `master` as it was.
        Returned(std::io::Result<()>),
        /// The watchdog let it go: `master` now holds a pipe, and the pty is `held`, a
        /// descriptor of its own, or closed where none could be had.
        LetGo { held: Option<libc::c_int> },
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Fate {
        /// Under way; the watchdog may still let it go.
        Armed,
        /// The master's slot holds the watchdog's pipe; the pty is `held` (-1: not kept).
        Swapped { held: libc::c_int },
        /// The swap failed: the master's slot is as it was.
        Unswapped,
    }

    /// One grant under way, read and written only under [`Dog::watched`]'s lock.
    struct Watch {
        /// The opener's master: a key no other watch shares, since the opener holds it open
        /// until it has taken its watch back.
        master: libc::c_int,
        /// The opener's thread, whose CPU time tells a spin from a wait.
        thread: MachPort,
        /// When the watchdog looks at it next.
        look: Instant,
        /// The opener's CPU time at the watchdog's first look, past the bound.
        cpu_then: Option<Duration>,
        fate: Fate,
    }

    struct Watched {
        watches: Vec<Watch>,
        /// The deadline the watchdog sleeps toward, or `None` when it sleeps with none (or has
        /// yet to sleep): a grant whose first look comes before it wakes the watchdog.
        asleep_until: Option<Instant>,
        /// Whether a grant has begun since the watchdog's last scan: if so, a scan that finds
        /// nothing to watch keeps a deadline one bound away, so the opens of a burst find it on a
        /// timer and wake it not at all, whichever of them its scans happen to see.
        begun: bool,
    }

    struct Dog {
        watched: Mutex<Watched>,
        wake: Condvar,
        /// A pipe's read end, close-on-exec, open for the process's life.
        sentinel: libc::c_int,
    }

    static DOG: OnceLock<Option<&'static Dog>> = OnceLock::new();

    /// Set in a child of `fork` by the handler the watchdog's start registers.
    static FORKED: AtomicBool = AtomicBool::new(false);

    /// The `pthread_atfork` child handler: async-signal-safe, one store.
    extern "C" fn forked() {
        FORKED.store(true, Ordering::Relaxed);
    }

    /// The watchdog, started on first use; `None` in a child of `fork`, or where its fork
    /// handler, pipe or thread could not be had (each grant then runs unbounded, as `grantpt`
    /// alone does).
    fn dog() -> Option<&'static Dog> {
        if FORKED.load(Ordering::Relaxed) {
            return None;
        }
        *DOG.get_or_init(|| {
            // SAFETY: registers one `extern "C"` function that lives as long as the process and
            // cannot unwind.
            if unsafe { pthread_atfork(None, None, Some(forked)) } != 0 {
                return None;
            }
            let sentinel = {
                use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd};
                aterm_uds::kept_out(|| {
                    let mut ends = [0 as libc::c_int; 2];
                    // SAFETY: `ends` is two writable descriptor slots, which `pipe` fills on success.
                    if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // SAFETY: both ends were just opened by this call; the write end is closed at once,
                    // the read end kept for the process's life and made close-on-exec.
                    let sentinel = unsafe {
                        libc::close(ends[1]);
                        let sentinel = OwnedFd::from_raw_fd(ends[0]);
                        if libc::fcntl(sentinel.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        sentinel
                    };
                    Ok(sentinel)
                })
                .ok()?
                .into_raw_fd()
            };
            let dog: &'static Dog = Box::leak(Box::new(Dog {
                watched: Mutex::new(Watched {
                    watches: Vec::new(),
                    asleep_until: None,
                    begun: false,
                }),
                wake: Condvar::new(),
                sentinel,
            }));
            std::thread::Builder::new()
                .name("aterm-pty-grant-watch".to_owned())
                .spawn(move || dog.run())
                .ok()?;
            Some(dog)
        })
    }

    /// The calling thread's port name, which its pthread holds for the thread's life.
    fn this_thread() -> MachPort {
        // SAFETY: the calling thread's own pthread handle, and its port name in this task.
        unsafe { pthread_mach_thread_np(pthread_self()) }
    }

    /// `thread`'s CPU time, user and system, or `None` where the kernel will not say.
    fn cpu_time(thread: MachPort) -> Option<Duration> {
        let mut info = [0i32; THREAD_BASIC_INFO_COUNT as usize];
        let mut count = THREAD_BASIC_INFO_COUNT;
        // SAFETY: `info` holds `count` integers, the size the flavor fills.
        let kr =
            unsafe { thread_info(thread, THREAD_BASIC_INFO, info.as_mut_ptr(), &raw mut count) };
        if kr != 0 {
            return None;
        }
        // `thread_basic_info` opens with `user_time` and `system_time`, each a `time_value_t`
        // of seconds and microseconds.
        let time = |secs: i32, micros: i32| {
            Some(
                Duration::from_secs(u64::try_from(secs).ok()?)
                    + Duration::from_micros(u64::try_from(micros).ok()?),
            )
        };
        Some(time(info[0], info[1])? + time(info[2], info[3])?)
    }

    /// Runs `f` while this thread holds the watchdog's lock (tests): a fork in `f`'s time, on
    /// another thread, gives its child that lock held by a thread the child does not have.
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn holding_lock(f: impl FnOnce()) {
        let dog = dog().expect("the watchdog");
        let _held = dog.lock();
        f();
    }

    /// `grant(master)` (`grantpt`, or a test's stand-in), let go once it spins past
    /// [`super::GRANT_BOUND`].
    pub(super) fn grant(
        master: libc::c_int,
        grant: impl FnOnce(libc::c_int) -> libc::c_int,
    ) -> Granted {
        let returned = |rc: libc::c_int, err: Option<std::io::Error>| {
            Granted::Returned(match (rc, err) {
                (0, _) => Ok(()),
                (_, Some(err)) => Err(err),
                (_, None) => Err(std::io::Error::other("grantpt failed")),
            })
        };
        let Some(dog) = dog() else {
            let rc = grant(master);
            return returned(rc, (rc != 0).then(std::io::Error::last_os_error));
        };
        let thread = this_thread();
        let wake = {
            let mut watched = dog.lock();
            let look = Instant::now() + super::GRANT_BOUND;
            watched.watches.push(Watch {
                master,
                thread,
                look,
                cpu_then: None,
                fate: Fate::Armed,
            });
            watched.begun = true;
            let wake = watched.asleep_until.is_none_or(|until| until > look);
            if wake {
                watched.asleep_until = Some(look);
            }
            wake
        };
        if wake {
            dog.wake.notify_one();
        }
        let rc = grant(master);
        let err = (rc != 0).then(std::io::Error::last_os_error);
        // The watchdog swaps a slot only under the lock, so its fate is settled once we hold it.
        let fate = {
            let mut watched = dog.lock();
            watched
                .watches
                .iter()
                .position(|watch| watch.master == master)
                .map_or(Fate::Armed, |at| watched.watches.swap_remove(at).fate)
        };
        match fate {
            Fate::Swapped { held } => Granted::LetGo {
                held: (held >= 0).then_some(held),
            },
            Fate::Armed | Fate::Unswapped => returned(rc, err),
        }
    }

    impl Dog {
        fn lock(&self) -> MutexGuard<'_, Watched> {
            self.watched.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn run(&self) {
            let mut watched = self.lock();
            loop {
                let now = Instant::now();
                for watch in &mut watched.watches {
                    if watch.fate == Fate::Armed && watch.look <= now {
                        self.look_at(watch, now);
                    }
                }
                let begun = std::mem::take(&mut watched.begun);
                let next = watched
                    .watches
                    .iter()
                    .filter(|watch| watch.fate == Fate::Armed)
                    .map(|watch| watch.look)
                    .min()
                    .or_else(|| begun.then(|| now + super::GRANT_BOUND));
                watched.asleep_until = next;
                watched = match next {
                    None => self
                        .wake
                        .wait(watched)
                        .unwrap_or_else(PoisonError::into_inner),
                    Some(until) => {
                        self.wake
                            .wait_timeout(watched, until.saturating_duration_since(Instant::now()))
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
            }
        }

        /// A look at a grant past its bound: its opener's CPU time taken at the first, and the
        /// grant let go at a later one that finds [`super::GRANT_SPIN`] more of it burnt (or at
        /// once, where the kernel will not say).
        fn look_at(&self, watch: &mut Watch, now: Instant) {
            match (watch.cpu_then, cpu_time(watch.thread)) {
                (Some(then), Some(cpu)) if cpu.saturating_sub(then) < super::GRANT_SPIN => {
                    watch.look = now + LOOK_EVERY;
                }
                (None, Some(cpu)) => {
                    watch.cpu_then = Some(cpu);
                    watch.look = now + LOOK_EVERY;
                }
                (Some(_), Some(_)) | (_, None) => watch.fate = self.let_go(watch.master),
            }
        }

        /// Takes the pty out of `master`'s slot, which its opener holds open while its grant
        /// runs.
        fn let_go(&self, master: libc::c_int) -> Fate {
            // SAFETY: `master` is the opener's live master: the opener closes it only after it
            // has taken its watch back, under the lock this thread holds throughout. `F_DUPFD_CLOEXEC`
            // and `dup2` take descriptors by value; `dup2` closes the slot's old file (the pty,
            // kept open by `held` where `F_DUPFD_CLOEXEC` gave one) and keeps the slot reserved
            // until it holds the pipe.
            unsafe {
                let held = libc::fcntl(master, libc::F_DUPFD_CLOEXEC, 3);
                loop {
                    if libc::dup2(self.sentinel, master) >= 0 {
                        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
                        return Fate::Swapped { held };
                    }
                    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                        break;
                    }
                }
                if held >= 0 {
                    libc::close(held);
                }
                Fate::Unswapped
            }
        }
    }
}

/// Opens a bare `(master, slave)` PTY pair with `FD_CLOEXEC` set on both ends.
pub fn open_pty_pair() -> io::Result<(i32, i32)> {
    open_pty_pair_cloexec(None, None)
}

fn kernel_default_termios() -> Option<libc::termios> {
    static DEFAULTS: std::sync::OnceLock<Option<libc::termios>> = std::sync::OnceLock::new();
    *DEFAULTS.get_or_init(|| {
        let (m, s) = open_pty_pair_cloexec(None, None).ok()?;
        // SAFETY: `s` and `m` are live owned fds closed before return; `t` is a valid out-param.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            let ok = libc::tcgetattr(s, &mut t) == 0;
            libc::close(s);
            libc::close(m);
            ok.then_some(t)
        }
    })
}

fn build_spawn_termios(mut t: libc::termios) -> libc::termios {
    t.c_iflag |= libc::IUTF8;
    // SAFETY: `cfsetspeed` only writes the speed fields of the valid stack-owned `t`.
    unsafe {
        libc::cfsetspeed(&mut t, libc::B230400);
    }
    t
}

fn spawn_termios() -> Option<libc::termios> {
    kernel_default_termios().map(build_spawn_termios)
}

const EXEC_START_HANG: std::time::Duration = crate::START_HANG;

const EXEC_STATUS_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecStatusCarrier {
    #[cfg(target_os = "linux")]
    Pipe2,
    #[cfg(target_os = "macos")]
    Fifo,
    RacyPipe,
}

static RACY_EXEC_STATUS_CARRIERS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn note_racy_exec_status_carrier(why: &io::Error) {
    use std::sync::atomic::Ordering;
    let prior = RACY_EXEC_STATUS_CARRIERS.fetch_add(1, Ordering::Relaxed);
    if prior == 0 {
        {
            use std::io::Write as _;
            #[cfg(target_os = "macos")]
            let place = if matches!(why.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) {
                String::new()
            } else {
                format!(" in {}", String::from_utf8_lossy(exec_status_fifo_dir()))
            };
            #[cfg(not(target_os = "macos"))]
            let place = "";
            let _ = writeln!(
                std::io::stderr(),
                "aterm: cannot create a private pipe{place} ({why}); shells still start"
            );
        }
    }
}

fn fd_is_cloexec(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is an open owned descriptor; `F_GETFD` only reads the descriptor-flag word.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags != -1 && flags & libc::FD_CLOEXEC != 0
}

fn fd_set_nonblocking(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is an open owned descriptor; `F_GETFL`/`F_SETFL` only access status flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags != -1 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    }
}

#[cfg(target_os = "macos")]
fn exec_status_fifo_dir() -> &'static [u8] {
    static DIR: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| fifo_dir_in(std::env::var_os("TMPDIR")))
}

#[cfg(target_os = "macos")]
fn fifo_dir_in(tmpdir: Option<std::ffi::OsString>) -> Vec<u8> {
    let mut dir = tmpdir
        .filter(|d| !d.is_empty())
        .map_or_else(|| b"/tmp".to_vec(), |d| d.as_bytes().to_vec());
    while dir.len() > 1 && dir.last() == Some(&b'/') {
        dir.pop();
    }
    if dir.last() != Some(&b'/') {
        dir.push(b'/');
    }
    dir
}

#[cfg(target_os = "macos")]
fn exec_status_fifo_path() -> io::Result<CString> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let mut path = exec_status_fifo_dir().to_vec();
    let mut rnd = [0u8; 8];
    // SAFETY: `getentropy` fills the 8-byte `rnd` buffer (within its 256-byte limit).
    let entropy_ok =
        unsafe { libc::getentropy(rnd.as_mut_ptr().cast::<libc::c_void>(), rnd.len()) } == 0;
    let unpredictable = if entropy_ok {
        u64::from_ne_bytes(rnd)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
    };
    // SAFETY: `getpid` reads process state and takes no arguments.
    let pid = unsafe { libc::getpid() };
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    path.extend_from_slice(
        format!("aterm-xstatus-{pid:x}-{seq:x}-{unpredictable:016x}").as_bytes(),
    );
    CString::new(path).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "per-user temp directory path contains an interior NUL",
        )
    })
}

#[cfg(target_os = "macos")]
fn fifo_ends_are_one_private_object(rd: libc::c_int, wr: libc::c_int) -> bool {
    // SAFETY: `rd` and `wr` are live fds; `fstat` fills valid `stat` structs and `geteuid` is safe.
    unsafe {
        let mut a: libc::stat = std::mem::zeroed();
        let mut b: libc::stat = std::mem::zeroed();
        if libc::fstat(rd, &mut a) != 0 || libc::fstat(wr, &mut b) != 0 {
            return false;
        }
        a.st_mode & libc::S_IFMT == libc::S_IFIFO
            && b.st_mode & libc::S_IFMT == libc::S_IFIFO
            && a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && a.st_uid == libc::geteuid()
            && a.st_mode & 0o777 == 0o600
    }
}

#[cfg(target_os = "macos")]
fn open_exec_status_fifo() -> io::Result<(libc::c_int, libc::c_int)> {
    let mut last = io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exec-status fifo path collision",
    );
    for _attempt in 0..4 {
        let path = exec_status_fifo_path()?;
        // SAFETY: `path` is a valid NUL-terminated path read by `mkfifo`.
        if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
            last = io::Error::last_os_error();
            if last.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(last);
        }
        // SAFETY: `path` is the fifo we just created; `fchmodat` with `AT_SYMLINK_NOFOLLOW`
        // only sets its mode without following a symlink.
        unsafe {
            libc::fchmodat(
                libc::AT_FDCWD,
                path.as_ptr(),
                0o600,
                libc::AT_SYMLINK_NOFOLLOW,
            );
        }
        // SAFETY: `path` is a valid NUL-terminated fifo path read by `open`.
        let rd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if rd < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: removing the name we created above; no fd exists yet.
            unsafe {
                libc::unlink(path.as_ptr());
            }
            return Err(err);
        }
        // SAFETY: as (3).
        let wr = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if wr < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: closing `rd` opened in (3) and unlinking `path` created in (1).
            unsafe {
                libc::close(rd);
                libc::unlink(path.as_ptr());
            }
            return Err(err);
        }
        // SAFETY: removing the name we created above; both fds stay valid.
        unsafe {
            libc::unlink(path.as_ptr());
        }
        if !fifo_ends_are_one_private_object(rd, wr) || !fd_is_cloexec(rd) || !fd_is_cloexec(wr) {
            // SAFETY: closing the two descriptors opened in (3) and (4); the name is already gone.
            unsafe {
                libc::close(rd);
                libc::close(wr);
            }
            return Err(io::Error::other(
                "exec-status fifo is not the private close-on-exec object we created",
            ));
        }
        return Ok((rd, wr));
    }
    Err(last)
}

fn open_exec_status_channel() -> io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> {
    #[cfg(target_os = "linux")]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid 2-int buffer filled by `pipe2` with `O_CLOEXEC`.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == 0 {
            let (rd, wr) = (fds[0], fds[1]);
            if fd_is_cloexec(rd) && fd_is_cloexec(wr) {
                Ok((rd, wr, ExecStatusCarrier::Pipe2))
            } else {
                // SAFETY: closing the pair we just created; nothing else exists.
                unsafe {
                    libc::close(rd);
                    libc::close(wr);
                }
                Err(io::Error::other(
                    "pipe2(O_CLOEXEC) produced a descriptor without FD_CLOEXEC",
                ))
            }
        } else {
            Err(io::Error::last_os_error())
        }
    };
    #[cfg(target_os = "macos")]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> =
        open_exec_status_fifo().map(|(rd, wr)| (rd, wr, ExecStatusCarrier::Fifo));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no atomic close-on-exec exec-status carrier is implemented for this platform",
    ));

    let (rd, wr, carrier) = match atomic {
        Ok(chan) => chan,
        Err(why) => {
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` is a valid 2-element buffer that `pipe` fills.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (rd, wr) = (fds[0], fds[1]);
            // SAFETY: both fds are valid; `F_SETFD` only sets a flag word.
            let flagged = unsafe {
                libc::fcntl(rd, libc::F_SETFD, libc::FD_CLOEXEC) != -1
                    && libc::fcntl(wr, libc::F_SETFD, libc::FD_CLOEXEC) != -1
            };
            if !flagged {
                let err = io::Error::last_os_error();
                // SAFETY: closing the pair we just created.
                unsafe {
                    libc::close(rd);
                    libc::close(wr);
                }
                return Err(err);
            }
            note_racy_exec_status_carrier(&why);
            (rd, wr, ExecStatusCarrier::RacyPipe)
        }
    };

    if !fd_set_nonblocking(rd) {
        let err = io::Error::last_os_error();
        // SAFETY: closing the pair created above; no child exists yet.
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
        return Err(err);
    }
    Ok((rd, wr, carrier))
}

#[derive(Debug)]
enum ExecStatus {
    Execed,
    FailedBeforeExec(u8),
    NoVerdict,
    ChannelBroke(io::Error),
}

fn read_exec_status_now(rd: libc::c_int) -> Option<ExecStatus> {
    let mut byte = [0u8; 1];
    loop {
        // SAFETY: `rd` is a live read fd and `byte` is a 1-byte buffer.
        let n = unsafe { libc::read(rd, byte.as_mut_ptr().cast::<libc::c_void>(), 1) };
        if n > 0 {
            return Some(ExecStatus::FailedBeforeExec(byte[0]));
        }
        if n == 0 {
            return Some(ExecStatus::Execed);
        }
        let err = io::Error::last_os_error();
        return match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => None,
            _ => Some(ExecStatus::ChannelBroke(err)),
        };
    }
}

fn wait_readable_briefly(fd: libc::c_int, slice: std::time::Duration) {
    if fd >= 0 && (fd as usize) < libc::FD_SETSIZE {
        // SAFETY: `fd` is in `0..FD_SETSIZE`, and `set` and `tv` are valid initialized locals.
        unsafe {
            let mut set: libc::fd_set = std::mem::zeroed();
            libc::FD_SET(fd, &mut set);
            let mut tv = libc::timeval {
                tv_sec: slice.as_secs() as libc::time_t,
                tv_usec: libc::suseconds_t::from(slice.subsec_micros() as i32),
            };
            libc::select(fd + 1, &mut set, ptr::null_mut(), ptr::null_mut(), &mut tv);
        }
        return;
    }
    // SAFETY: `ts` is an initialized `timespec` and the remainder pointer is null.
    unsafe {
        let ts = libc::timespec {
            tv_sec: slice.as_secs() as libc::time_t,
            tv_nsec: libc::c_long::from(slice.subsec_nanos() as i32),
        };
        libc::nanosleep(&ts, ptr::null_mut());
    }
}

/// A test's stall of its own spawns' children before their exec: a child
/// starved between fork and exec, as a loaded machine starves one. It applies
/// only to spawns from the thread that set it, so every other test's spawn
/// runs as it is.
#[cfg(test)]
pub(crate) mod pre_exec_stall {
    use std::cell::Cell;

    std::thread_local! {
        static MS: Cell<u64> = const { Cell::new(0) };
    }

    /// Stall the child of each spawn this thread makes by `ms` before its exec (0: none).
    pub(crate) fn set(ms: u64) {
        MS.with(|m| m.set(ms));
    }

    /// In the child, between fork and exec: a const thread-local read (the
    /// child's one thread is the copy of the thread that forked it, so it
    /// reads that thread's value, with no allocation or lock) and `nanosleep`.
    pub(super) fn in_child() {
        let ms = MS.with(Cell::get);
        if ms == 0 {
            return;
        }
        let ts = libc::timespec {
            tv_sec: libc::time_t::try_from(ms / 1000).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::try_from((ms % 1000) * 1_000_000).unwrap_or(0),
        };
        // SAFETY: `ts` is an initialized `timespec` and the remainder pointer is null.
        unsafe {
            libc::nanosleep(&ts, std::ptr::null_mut());
        }
    }
}

/// The wait a spawn on this thread last gave its exec verdict (tests): what
/// the spawn passed [`wait_for_exec_status`], whatever its constant says.
#[cfg(test)]
pub(crate) mod exec_wait_seen {
    use std::cell::Cell;
    use std::time::Duration;

    std::thread_local! {
        static LAST: Cell<Option<Duration>> = const { Cell::new(None) };
    }

    pub(super) fn note(wait: Duration) {
        LAST.with(|l| l.set(Some(wait)));
    }

    /// The last wait noted on this thread.
    pub(crate) fn last() -> Option<Duration> {
        LAST.with(Cell::get)
    }
}

fn wait_for_exec_status(rd: libc::c_int, budget: std::time::Duration) -> ExecStatus {
    #[cfg(test)]
    exec_wait_seen::note(budget);
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Some(status) = read_exec_status_now(rd) {
            return status;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        wait_readable_briefly(rd, left.min(EXEC_STATUS_SLICE));
    }
    read_exec_status_now(rd).unwrap_or(ExecStatus::NoVerdict)
}

#[derive(Debug)]
struct SpawnKit {
    master: libc::c_int,
    slave: libc::c_int,
    status_rd: libc::c_int,
    status_wr: libc::c_int,
}

impl SpawnKit {
    fn open(winp: Option<&libc::winsize>) -> io::Result<Self> {
        let (status_rd, status_wr, _carrier) = open_exec_status_channel()?;
        let termp = spawn_termios();
        match open_pty_pair_cloexec(termp.as_ref(), winp) {
            Ok((master, slave)) => Ok(Self {
                master,
                slave,
                status_rd,
                status_wr,
            }),
            Err(err) => {
                // SAFETY: closing the two channel ends opened above after pty open failed.
                unsafe {
                    libc::close(status_rd);
                    libc::close(status_wr);
                }
                Err(err)
            }
        }
    }

    fn size(&self, w: &libc::winsize) {
        // SAFETY: `w` is an initialized `winsize` for `TIOCSWINSZ` on the owned `self.slave`.
        unsafe {
            libc::ioctl(self.slave, libc::TIOCSWINSZ, ptr::from_ref(w));
        }
    }

    fn into_raw(self) -> (libc::c_int, libc::c_int, libc::c_int, libc::c_int) {
        let kit = std::mem::ManuallyDrop::new(self);
        (kit.master, kit.slave, kit.status_rd, kit.status_wr)
    }
}

impl Drop for SpawnKit {
    fn drop(&mut self) {
        // SAFETY: closing the four descriptors exclusively owned by this unused kit.
        unsafe {
            libc::close(self.master);
            libc::close(self.slave);
            libc::close(self.status_rd);
            libc::close(self.status_wr);
        }
    }
}

/// The pty's name for a message (`/dev/ttys004`), or "the pty" when its master cannot say.
fn pty_label(master: libc::c_int) -> String {
    pts_name(master).map_or_else(
        |_| "the pty".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Why the pty of `master` and `slave` cannot carry a shell, read in the parent before a fork:
/// its slave descriptor is dead (`fstat` or `tcgetattr` fails) or, on macOS, its node is gone by
/// name. Under heavy pty churn a new pty can be left with no `/dev/ttys…` node of its own (XNU
/// frees a number before it removes the old pty's node, and `devfs_make_node` finds the name
/// taken), and on macOS 27.0.1 such a slave's descriptors were seen to die under a spawn
/// (ATERM-KI-1028-4); a pooled pty, opened well before its spawn, is the widest window.
/// `None`: the pty answered all of it.
fn dead_pty(master: libc::c_int, slave: libc::c_int) -> Option<String> {
    // SAFETY: `st` is a zeroed plain-integer out-param that `fstat` fills from `slave`.
    let fstat_ok = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        libc::fstat(slave, &mut st) == 0
    };
    if !fstat_ok {
        let err = io::Error::last_os_error();
        return Some(format!(
            "{}: its slave failed fstat ({err})",
            pty_label(master)
        ));
    }
    // SAFETY: `t` is a zeroed plain-integer out-param that `tcgetattr` fills from `slave`.
    let tcgetattr_ok = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(slave, &mut t) == 0
    };
    if !tcgetattr_ok {
        let err = io::Error::last_os_error();
        return Some(format!(
            "{}: its slave failed tcgetattr ({err})",
            pty_label(master)
        ));
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let name = match pts_name(master) {
            Ok(name) => name,
            Err(err) => return Some(format!("its master cannot name it ({err})")),
        };
        #[cfg(all(test, target_os = "macos"))]
        let name = pty_race::by_name(name);
        // SAFETY: `name` is a NUL-terminated path and `node` a zeroed out-param `stat` fills.
        let named = unsafe {
            let mut node: libc::stat = std::mem::zeroed();
            libc::stat(name.as_ptr(), &mut node) == 0
        };
        if !named {
            let err = io::Error::last_os_error();
            return Some(format!(
                "{}: its node is gone ({err})",
                name.to_string_lossy()
            ));
        }
    }
    None
}

/// How many dead ptys one spawn replaces with a fresh one before it is an error
/// ([`replace_dead_pty`]).
const DEAD_PTY_REPLACEMENTS: usize = 1;

/// The fresh pty a spawn takes in place of a dead one, whose cause (`died_of`) joins `deaths`:
/// one its parent found dead before the fork ([`dead_pty`]), one whose child could not make it
/// its controlling terminal (status byte `T`), or one that died in the child just after it did
/// (status byte `D`: fd 0 then failed `fstat` or `tcgetattr`). The caller still holds the dead pty while this
/// opens the fresh one, so the two are distinct ptys (never the same minor reopened). A spawn
/// replaces [`DEAD_PTY_REPLACEMENTS`] dead ptys; past that the spawn is an error naming every
/// cause and ATERM-KI-1028-4's, and nothing loops. A fresh pty that cannot open (likeliest under
/// the same churn: EAGAIN or EMFILE) is an error of its kind that still names the dead ones.
fn replace_dead_pty(
    deaths: &mut Vec<String>,
    died_of: String,
    ws: &libc::winsize,
) -> io::Result<SpawnKit> {
    deaths.push(died_of);
    if deaths.len() > DEAD_PTY_REPLACEMENTS {
        return Err(io::Error::other(format!(
            "every pty opened for this shell died before the shell could use it ({}): under \
             heavy pty churn macOS can leave a new pty with no /dev node of its own, or kill its \
             slave, as another pty on the same number finishes tearing down (ATERM-KI-1028-4)",
            deaths.join("; then ")
        )));
    }
    fresh_kit(ws).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "{err} (opening a pty in place of a dead one: {}; ATERM-KI-1028-4)",
                deaths.join("; then ")
            ),
        )
    })
}

/// A kit opened for a spawn that the pool could not lend one, or in place of a dead one.
fn fresh_kit(ws: &libc::winsize) -> io::Result<SpawnKit> {
    #[cfg(all(test, target_os = "macos"))]
    if pty_race::open_fails() {
        return Err(io::Error::from_raw_os_error(libc::EAGAIN));
    }
    let kit = SpawnKit::open(Some(ws))?;
    #[cfg(all(test, target_os = "macos"))]
    pty_race::opened_fresh(kit.master);
    Ok(kit)
}

fn ensure_above_stdio(fd: &mut libc::c_int) -> io::Result<()> {
    if *fd > libc::STDERR_FILENO {
        return Ok(());
    }
    // SAFETY: `*fd` is an open descriptor owned by the caller's `SpawnKit`; `F_DUPFD_CLOEXEC`
    // duplicates it onto the lowest free descriptor `> STDERR_FILENO` with `FD_CLOEXEC`.
    let moved = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, libc::STDERR_FILENO + 1) };
    if moved < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: closing the old descriptor number now that `moved` owns the duplicate.
    unsafe {
        libc::close(*fd);
    }
    *fd = moved;
    Ok(())
}

/// Spawns a session-leading child on a fresh close-on-exec PTY `(master, slave)` whose
/// slave is the child's controlling terminal (`setsid` + `TIOCSCTTY` on stdin, with all
/// descriptors above stdio stripped via [`aterm_uds::spawnfd::InheritNothing`]).
///
/// Guards against XNU's pty minor-teardown race (`ATERM-KI-1028-4`) up to `exec` the same way
/// [`spawn_shell_with_pid_cell_px`] does: checks [`dead_pty`] before each fork, checks
/// `TIOCSCTTY` (`T`) and post-`TIOCSCTTY` `fstat`/`tcgetattr` on stdin (`D`) in the child, and
/// replaces at most [`DEAD_PTY_REPLACEMENTS`] dead pty while still holding the dead one open so
/// the replacement gets a distinct minor. An `exec` refused with `ETXTBSY` (a program some
/// process holds open for writing, such as the child another thread forked while the program
/// was being written, until that child's own `exec`) is tried again off Apple, each time on a
/// fresh pty, as [`aterm_uds::spawnfd::spawn_past_text_busy`] tries one. Any other non-pty
/// spawn error (such as `setsid` or `exec` failing) returns `Err` immediately with no
/// replacement.
///
/// # Errors
/// Returns an `io::Error` if opening or replacing a pty fails, if `build` fails on a live pty,
/// if two consecutive ptys die before `exec`, or if `Command::spawn` fails for a non-pty reason
/// (`ETXTBSY` once `spawn_past_text_busy` has stopped trying).
#[cfg_attr(trust_verify, trust::skip)]
pub fn spawn_session_leader_on_pty(
    mut build: impl FnMut(&std::os::fd::OwnedFd) -> io::Result<aterm_uds::spawnfd::StdCommand>,
) -> io::Result<(
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
    std::process::Child,
)> {
    aterm_uds::spawnfd::spawn_past_text_busy(|| spawn_session_leader_on_fresh_pty(&mut build))
}

/// One try of [`spawn_session_leader_on_pty`], on a pty of its own.
#[cfg_attr(trust_verify, trust::skip)]
fn spawn_session_leader_on_fresh_pty(
    build: &mut impl FnMut(&std::os::fd::OwnedFd) -> io::Result<aterm_uds::spawnfd::StdCommand>,
) -> io::Result<(
    std::os::fd::OwnedFd,
    std::os::fd::OwnedFd,
    std::process::Child,
)> {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;

    let ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let mut kit = fresh_kit(&ws)?;
    let mut deaths: Vec<String> = Vec::new();
    loop {
        if let Some(died_of) = dead_pty(kit.master, kit.slave) {
            let dead = std::mem::replace(&mut kit, replace_dead_pty(&mut deaths, died_of, &ws)?);
            drop(dead);
            continue;
        }
        ensure_above_stdio(&mut kit.status_wr)?;
        // SAFETY: `kit.slave` is owned by `kit` across `slave_fd`'s whole lifetime, and
        // `ManuallyDrop` ensures `slave_fd` never closes `kit.slave`.
        let slave_fd = std::mem::ManuallyDrop::new(unsafe { OwnedFd::from_raw_fd(kit.slave) });
        let prep = build(&slave_fd).and_then(|mut cmd| {
            cmd.stdin(std::process::Stdio::from(slave_fd.try_clone()?));
            Ok(cmd)
        });
        let mut cmd = match prep {
            Ok(cmd) => cmd,
            Err(err) => match dead_pty(kit.master, kit.slave) {
                Some(died_of) => {
                    let dead =
                        std::mem::replace(&mut kit, replace_dead_pty(&mut deaths, died_of, &ws)?);
                    drop(dead);
                    continue;
                }
                None => return Err(err),
            },
        };
        cmd.inherit_nothing();
        #[cfg(test)]
        spawn_trace::forking_onto(kit.master);
        #[cfg(all(test, target_os = "macos"))]
        let strikes = pty_race::child_strikes(kit.master);
        let status_wr = kit.status_wr;
        // SAFETY: runs in the forked child before exec (after `std` placed `slave` onto fd 0
        // and after `inherit_nothing` marked descriptors >= 3 `FD_CLOEXEC`, leaving `status_wr`
        // open until exec). Calls only async-signal-safe `setsid`, `ioctl`, `fstat`, `tcgetattr`,
        // and `write` on stack locals; allocates nothing and takes no locks.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(all(test, target_os = "macos"))]
                pty_race::in_child(strikes.0.as_ref());
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                    let err = io::Error::last_os_error();
                    let b: u8 = b'T';
                    libc::write(status_wr, ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                    return Err(err);
                }
                #[cfg(all(test, target_os = "macos"))]
                pty_race::in_child(strikes.1.as_ref());
                let mut st: libc::stat = std::mem::zeroed();
                let mut t: libc::termios = std::mem::zeroed();
                if libc::fstat(libc::STDIN_FILENO, &mut st) != 0
                    || libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0
                {
                    let err = io::Error::last_os_error();
                    let b: u8 = b'D';
                    libc::write(status_wr, ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                    return Err(err);
                }
                Ok(())
            });
        }
        let spawning = aterm_uds::spawn_lock::spawning();
        let spawned = cmd.spawn();
        drop(spawning);
        drop(cmd);
        match spawned {
            Ok(child) => {
                let (master, slave, status_rd, status_wr) = kit.into_raw();
                // SAFETY: `status_rd` and `status_wr` are owned exclusively by `kit`, which
                // `into_raw` consumed without dropping; `master` and `slave` transfer to
                // `OwnedFd`.
                unsafe {
                    libc::close(status_rd);
                    libc::close(status_wr);
                    return Ok((
                        OwnedFd::from_raw_fd(master),
                        OwnedFd::from_raw_fd(slave),
                        child,
                    ));
                }
            }
            Err(spawn_err) => {
                let how = match read_exec_status_now(kit.status_rd) {
                    Some(ExecStatus::FailedBeforeExec(b'T')) => {
                        "the child could not make it its controlling terminal (TIOCSCTTY failed)"
                    }
                    Some(ExecStatus::FailedBeforeExec(b'D')) => {
                        "it died once the child had made it its controlling terminal (fstat or \
                         tcgetattr on it failed in the child)"
                    }
                    _ => return Err(spawn_err),
                };
                let died_of = format!("{}: {how} ({spawn_err})", pty_label(kit.master));
                let dead =
                    std::mem::replace(&mut kit, replace_dead_pty(&mut deaths, died_of, &ws)?);
                drop(dead);
            }
        }
    }
}

/// The foreground group of the pty whose master is `master`, as [`end_session_on_pty`] reads
/// it: `tcgetpgrp`, or a test's stand-in ([`front_read`]) where it set one.
#[cfg(any(test, debug_assertions))]
fn foreground_of(master: libc::c_int) -> libc::pid_t {
    #[cfg(test)]
    if let Some(group) = front_read::stand_in() {
        return group;
    }
    // SAFETY: `tcgetpgrp` is a read-only ioctl on a master the caller owns.
    unsafe { libc::tcgetpgrp(master) }
}

/// Whether the process whose pid is `group` leads that group and is in the session `session`
/// leads: then `group` names a group of that session, not one an id read stale, or reused since,
/// gave another session (a group keeps its id while its leader lives, so the two reads and the
/// signal after them all name it, short of a pid wrap within that span).
#[cfg(any(test, debug_assertions))]
fn leads_in_session(group: libc::pid_t, session: libc::pid_t) -> bool {
    // SAFETY: plain syscalls taking no pointer.
    unsafe { libc::getpgid(group) == group && libc::getsid(group) == session }
}

/// The foreground read [`end_session_on_pty`] makes, stood in for by a test, as an id read
/// stale, or reused since, would read: a group of another session.
#[cfg(test)]
pub(crate) mod front_read {
    use std::cell::Cell;

    thread_local! {
        static STAND_IN: Cell<Option<libc::pid_t>> = const { Cell::new(None) };
    }

    /// This thread's next [`super::end_session_on_pty`] of a running leader reads its pty's
    /// foreground group as `group`.
    pub(crate) fn reads_as(group: libc::pid_t) {
        STAND_IN.with(|stand_in| stand_in.set(Some(group)));
    }

    /// The stand-in [`reads_as`] set, taken.
    pub(super) fn stand_in() -> Option<libc::pid_t> {
        STAND_IN.with(Cell::take)
    }
}

/// Ends the session that `leader`, spawned by [`spawn_session_leader_on_pty`] onto the pty of
/// `master`, leads, and closes that pty, `master` last: one `SIGKILL` to the pty's foreground
/// group (a shell's job) where that group's own leader runs in this session ([`leads_in_session`]:
/// an id read stale, or reused since, may name another session's group; a job whose group
/// leader has exited is the caller's to end, as background jobs are); a `SIGKILL` to the
/// leader's own group (on Apple again, paced, until
/// the kernel finds no member of it left to signal or [`END_KILL_CAP`] has passed); the leader
/// reaped; all while `said` takes what the master gives (on Apple a session leader's exit first
/// waits for its terminal's output to drain, which reading the master lets it finish); then the
/// caller's `slave` closed and the master read to its end (no process holds the slave any
/// more); then `master` closed. The session's other groups (a shell's background jobs) are the
/// caller's to end: on Linux one that holds the slave keeps the master from its end. A pty torn
/// down by the exit of a process nobody waits on (its session leader, left running when its
/// master closed) frees its number at a moment no one chooses, and XNU frees a number before it
/// removes that pty's `/dev` node: a pty given the number in between is left with no node of its
/// own (`devfs_make_node` finds the name taken), as ATERM-KI-1028-4 records. The call looks at
/// the leader without reaping it ([`child_exit_unreaped`]). A leader the caller has already
/// reaped is not signalled, nor is its group or its terminal's foreground: its pid, and the
/// group it led, may name other processes now. One that has exited and is not reaped (a zombie)
/// still holds its pid, and so its group's id, so its group is signalled as a running leader's
/// is (a member left holding the slave, as a job that ignored the exit's hangup does, is
/// ended); its terminal's foreground is not, since that group may have emptied and its id
/// passed to a process of another session by then.
///
/// # Errors
/// `fcntl`'s, making the master nonblocking; `Other` for a leader pid `pid_t` cannot hold; the
/// first master read or `waitpid` that failed; `TimedOut` when the leader is not reaped, or the
/// master gives no end, `within` from the call. On every error `slave` and `master` close as the
/// call returns, the leader reaped or not.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(trust_verify, trust::skip)]
pub fn end_session_on_pty(
    master: std::os::fd::OwnedFd,
    slave: Option<std::os::fd::OwnedFd>,
    leader: &mut std::process::Child,
    within: std::time::Duration,
    said: &mut dyn FnMut(&[u8]),
) -> io::Result<std::process::ExitStatus> {
    use std::os::fd::AsRawFd as _;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let until = start + within;
    let fd = master.as_raw_fd();
    set_nonblocking(fd, true)?;
    let pid = libc::pid_t::try_from(leader.id()).map_err(io::Error::other)?;
    // `None`: reaped (by the caller), so its pid and group may name others; nothing is signalled.
    if let Some(exited) = child_exit_unreaped(pid) {
        let front = if exited { 0 } else { foreground_of(fd) };
        if front > 1 && front != pid && leads_in_session(front, pid) {
            // SAFETY: a plain syscall taking no pointer; `front` > 1 names one group, the one the
            // pty named its foreground just now, whose own leader runs in this session.
            unsafe {
                libc::kill(-front, libc::SIGKILL);
            }
        }
        // One signal reaches every member on Linux, whose fork restarts when a group signal
        // lands during it. XNU's fork does not: a child forked as the signal lands joins the
        // group unsignalled, so on Apple the group is signalled again until the kernel finds no
        // member to signal (`EPERM` once only zombies are left, the leader, unreaped until this
        // loop ends, among them; `ESRCH` once none are), as
        // `aterm_uds::process::kill_group_until_empty` does; the master is read between rounds.
        let cap = (start + END_KILL_CAP).min(until);
        let mut pause = Duration::from_millis(1);
        loop {
            drain_master(fd, said)?;
            // SAFETY: a plain syscall taking no pointer; `pid` > 1 leads its own group
            // (`setsid`), and, unreaped, keeps that group's id from naming any other.
            let signalled = unsafe { libc::kill(-pid, libc::SIGKILL) } == 0;
            if !cfg!(target_vendor = "apple") || !signalled || Instant::now() >= cap {
                break;
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(END_KILL_PAUSE);
        }
    }
    let status = loop {
        drain_master(fd, said)?;
        if let Some(status) = leader.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the session leader {pid} was not reaped within {within:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(slave);
    while !drain_master(fd, said)? {
        if Instant::now() >= until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{}: its slave was still open {within:?} after its end began",
                    pty_label(fd)
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(master);
    Ok(status)
}

/// How long [`end_session_on_pty`] signals a session leader's group that has not emptied: a
/// member `SIGKILL` cannot end (one stuck in the kernel) is left to the kernel after this.
#[cfg(any(test, debug_assertions))]
const END_KILL_CAP: std::time::Duration = std::time::Duration::from_secs(5);

/// The longest pause between two of [`end_session_on_pty`]'s signals to a group.
#[cfg(any(test, debug_assertions))]
const END_KILL_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// Read the nonblocking master `fd`, each read's bytes to `said`, until it has nothing more now
/// or sixteen reads have: `true` at its end (no process holds the slave: `read` answers 0 on
/// Apple, `EIO` on Linux), `false` while its slave is open.
#[cfg_attr(trust_verify, trust::skip)]
fn drain_master(fd: libc::c_int, said: &mut dyn FnMut(&[u8])) -> io::Result<bool> {
    let mut buf = [0_u8; 4096];
    for _ in 0..16 {
        match usize::try_from(read(fd, &mut buf)) {
            Ok(0) => return Ok(true),
            Ok(n) => said(&buf[..n]),
            Err(_) => {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => {}
                    Some(libc::EAGAIN) => return Ok(false),
                    Some(libc::EIO) => return Ok(true),
                    _ => return Err(err),
                }
            }
        }
    }
    Ok(false)
}

/// What this thread's spawns forked onto (tests): the name of each pty a spawn forked a child
/// onto, in order, read in the parent before the fork.
#[cfg(test)]
pub(crate) mod spawn_trace {
    use std::cell::RefCell;
    use std::ffi::CString;

    std::thread_local! {
        static FORKED_ONTO: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
    }

    /// In the parent, before the fork onto the pty of `master`.
    pub(super) fn forking_onto(master: libc::c_int) {
        let name = super::pts_name(master).unwrap_or_default();
        FORKED_ONTO.with(|f| f.borrow_mut().push(name));
    }

    /// The ptys this thread's spawns forked onto since the last take.
    pub(crate) fn take() -> Vec<CString> {
        FORKED_ONTO.with(|f| std::mem::take(&mut *f.borrow_mut()))
    }
}

/// A test's ATERM-KI-1028-4, on demand: a pty's slave killed under a spawn. `revoke(2)` on the
/// slave's name kills every descriptor of it as XNU's race does (`fstat` fails with EBADF,
/// `tcgetattr` fails, `login_tty`'s `TIOCSCTTY` says ENOENT; measured on macOS 27.0), though it
/// leaves the node in place; the node's removal itself, which no test can do to `/dev`, is
/// shown to the parent's check, and to a new pty's slave as it opens, by a name that names
/// nothing ([`gone_by_name`], [`slave_opens_gone`]). It applies only to spawns and opens from
/// the thread that set it, so every other test's run as they are.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod pty_race {
    use std::cell::{Cell, RefCell};
    use std::ffi::{CStr, CString};

    std::thread_local! {
        static IN_CHILD: Cell<u32> = const { Cell::new(0) };
        static AFTER_LOGIN_TTY: Cell<u32> = const { Cell::new(0) };
        static FRESH: Cell<u32> = const { Cell::new(0) };
        static GONE_BY_NAME: Cell<u32> = const { Cell::new(0) };
        static OPEN_FAILS: Cell<u32> = const { Cell::new(0) };
        static SLAVE_OPENS_GONE: Cell<u32> = const { Cell::new(0) };
        static STRUCK_FRESH: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
        static STRUCK_OPENS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
        static GRANTS_SPIN: Cell<u32> = const { Cell::new(0) };
        static STRUCK_GRANTS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
        static REPLACED: RefCell<Vec<Option<CString>>> = const { RefCell::new(Vec::new()) };
        static GRANTS_NAP: Cell<(u32, std::time::Duration)> =
            const { Cell::new((0, std::time::Duration::ZERO)) };
        static NAPPED_GRANTS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
        static LET_GO_SLOTS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" {
        fn revoke(path: *const libc::c_char) -> libc::c_int;
    }

    /// Kill the slave in the child, just before its `login_tty`, on each of this thread's next
    /// `n` forks.
    pub(crate) fn in_children(n: u32) {
        IN_CHILD.with(|c| c.set(n));
    }

    /// Kill the slave in the child just after its `login_tty` made it the controlling terminal,
    /// on each of this thread's next `n` forks.
    pub(crate) fn after_login_tty(n: u32) {
        AFTER_LOGIN_TTY.with(|c| c.set(n));
    }

    /// On each of this thread's next `n` parent checks that reach the slave's name, `stat` a
    /// name that names nothing: the pty's node gone while its descriptors still answer.
    pub(crate) fn gone_by_name(n: u32) {
        GONE_BY_NAME.with(|c| c.set(n));
    }

    /// Fail each of this thread's next `n` fresh pty opens with EAGAIN, as a pty open can fail
    /// under the churn that kills ptys.
    pub(crate) fn fresh_opens_fail(n: u32) {
        OPEN_FAILS.with(|c| c.set(n));
    }

    /// In the parent, before a fresh kit opens: whether this open is one that fails.
    pub(super) fn open_fails() -> bool {
        take_one(&OPEN_FAILS)
    }

    /// On each of this thread's next `n` opens of a new pty's slave by its name, open a name
    /// that names nothing: the pty's node gone as its slave opens (ENOENT).
    pub(crate) fn slave_opens_gone(n: u32) {
        SLAVE_OPENS_GONE.with(|c| c.set(n));
    }

    /// The names of the ptys whose slave's open [`slave_opens_gone`] struck since the last take.
    pub(crate) fn take_struck_opens() -> Vec<CString> {
        STRUCK_OPENS.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    /// In the open of a new pty's slave, the name it opens: `name`, or on a struck open `name`
    /// with `.gone` after it, which names nothing.
    pub(super) fn opening_slave(name: CString) -> CString {
        if !take_one(&SLAVE_OPENS_GONE) {
            return name;
        }
        STRUCK_OPENS.with(|s| s.borrow_mut().push(name.clone()));
        let mut gone = name.into_bytes();
        gone.extend_from_slice(b".gone");
        CString::new(gone).unwrap_or_default()
    }

    /// On each of this thread's next `n` pty opens, a grant that never returns by itself, as
    /// XNU's `grantpt` spins on a pty whose node could not be made (ATERM-KI-1028-4): it asks the
    /// master's slot for its name again and again, the same call on the same number, until the
    /// slot holds something else.
    pub(crate) fn grants_spin(n: u32) {
        GRANTS_SPIN.with(|c| c.set(n));
    }

    /// In a replacement, once the fresh pair is open and before the replaced pty closes: that
    /// pty's name as its master answers it then (`None` where no pty was kept, or it answers
    /// none), the proof it was held while the fresh pair opened.
    pub(super) fn replaced(gone: Option<libc::c_int>) {
        let name = gone.and_then(|fd| super::pts_name(fd).ok());
        REPLACED.with(|r| r.borrow_mut().push(name));
    }

    /// What [`replaced`] recorded since the last take, one entry a replacement.
    pub(crate) fn take_replaced() -> Vec<Option<CString>> {
        REPLACED.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// The names of the ptys whose grant [`grants_spin`] struck since the last take.
    pub(crate) fn take_struck_grants() -> Vec<CString> {
        STRUCK_GRANTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    /// In a pty's open, before its grant: whether this grant is one that spins (its pty's name
    /// recorded).
    pub(super) fn grant_spins(master: libc::c_int) -> bool {
        if !take_one(&GRANTS_SPIN) {
            return false;
        }
        let name = super::pts_name(master).unwrap_or_default();
        STRUCK_GRANTS.with(|s| s.borrow_mut().push(name));
        true
    }

    /// The spinning grant: `TIOCPTYGNAME` on `master`'s slot until it fails, then -1.
    pub(super) fn spin_grant(master: libc::c_int) -> libc::c_int {
        let mut name = [0 as libc::c_char; super::PTS_NAME_LEN];
        // SAFETY: `name` is `PTS_NAME_LEN` writable bytes, the size `TIOCPTYGNAME` writes; it takes
        // whatever descriptor the slot holds, and fails once it holds no pty master.
        while unsafe {
            libc::ioctl(
                master,
                libc::TIOCPTYGNAME as libc::c_ulong,
                name.as_mut_ptr(),
            )
        } == 0
        {}
        -1
    }

    /// On each of this thread's next `n` pty opens, a grant that sleeps `nap` before it runs, as a
    /// healthy grant on an opener the scheduler leaves off its core (heavy load, a page-in, a
    /// stop): late, burning no CPU, and then granted.
    pub(crate) fn grants_nap(n: u32, nap: std::time::Duration) {
        GRANTS_NAP.with(|c| c.set((n, nap)));
    }

    /// In a pty's open, before its grant: how long this grant sleeps first, if it is one that
    /// does (its pty's name recorded).
    pub(super) fn grant_naps(master: libc::c_int) -> Option<std::time::Duration> {
        let (left, nap) = GRANTS_NAP.with(Cell::get);
        if left == 0 {
            return None;
        }
        GRANTS_NAP.with(|c| c.set((left - 1, nap)));
        let name = super::pts_name(master).unwrap_or_default();
        NAPPED_GRANTS.with(|s| s.borrow_mut().push(name));
        Some(nap)
    }

    /// The names of the ptys whose grant [`grants_nap`] put to sleep since the last take.
    pub(crate) fn take_napped_grants() -> Vec<CString> {
        NAPPED_GRANTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    /// In a let-go grant's open, before the master's slot closes: whether that slot holds a pipe
    /// (the watchdog's), so the number stayed reserved through the swap and the close is of a
    /// descriptor the open owns.
    pub(super) fn let_go_slot(master: libc::c_int) {
        // SAFETY: every field of `stat` is an integer, so all-zero is valid.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `st` is a live, writable `stat`; a closed `master` answers EBADF.
        let pipe = unsafe { libc::fstat(master, &raw mut st) } == 0
            && st.st_mode & libc::S_IFMT == libc::S_IFIFO;
        LET_GO_SLOTS.with(|s| s.borrow_mut().push(pipe));
    }

    /// What [`let_go_slot`] recorded since the last take, one entry a let-go grant.
    pub(crate) fn take_let_go_slots() -> Vec<bool> {
        LET_GO_SLOTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    /// This thread's seam, all of it off, and its record of struck fresh ptys cleared.
    pub(crate) fn reset() {
        in_children(0);
        after_login_tty(0);
        in_fresh(0);
        gone_by_name(0);
        fresh_opens_fail(0);
        slave_opens_gone(0);
        let _ = take_struck_fresh();
        let _ = take_struck_opens();
        grants_spin(0);
        let _ = take_struck_grants();
        let _ = take_replaced();
        grants_nap(0, std::time::Duration::ZERO);
        let _ = take_napped_grants();
        let _ = take_let_go_slots();
    }

    /// The names of the fresh ptys [`in_fresh`] killed since the last take.
    pub(crate) fn take_struck_fresh() -> Vec<CString> {
        STRUCK_FRESH.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    /// Kill the slave of each of the next `n` fresh ptys this thread's spawns open, as it opens.
    pub(crate) fn in_fresh(n: u32) {
        FRESH.with(|c| c.set(n));
    }

    fn take_one(left: &'static std::thread::LocalKey<Cell<u32>>) -> bool {
        left.with(|c| {
            let n = c.get();
            c.set(n.saturating_sub(1));
            n > 0
        })
    }

    /// `revoke(2)` the slave named `name`; `false` when it failed.
    pub(crate) fn revoke_slave(name: &CStr) -> bool {
        // SAFETY: `name` is a NUL-terminated path `revoke` only reads.
        unsafe { revoke(name.as_ptr()) == 0 }
    }

    /// In the parent, before the fork onto `master`'s pty: the slave name this fork's child
    /// revokes before its `login_tty` and the one it revokes after, each when this fork is one
    /// of the struck ones.
    pub(super) fn child_strikes(master: libc::c_int) -> (Option<CString>, Option<CString>) {
        let before = take_one(&IN_CHILD);
        let after = take_one(&AFTER_LOGIN_TTY);
        let name = || super::pts_name(master).ok();
        (before.then(name).flatten(), after.then(name).flatten())
    }

    /// In the child, before or after its `login_tty`: `revoke(2)` by the name its parent made;
    /// no allocation, no lock (one syscall on a live C string).
    pub(super) fn in_child(name: Option<&CString>) {
        if let Some(name) = name {
            revoke_slave(name);
        }
    }

    /// In the parent, as a fresh kit of `master`'s pty opens.
    pub(super) fn opened_fresh(master: libc::c_int) {
        if take_one(&FRESH)
            && let Ok(name) = super::pts_name(master)
        {
            revoke_slave(&name);
            STRUCK_FRESH.with(|s| s.borrow_mut().push(name));
        }
    }

    /// In the parent's check, the slave name it `stat`s: `name`, or on a struck check `name`
    /// with `.gone` after it, which names nothing.
    pub(super) fn by_name(name: CString) -> CString {
        if !take_one(&GONE_BY_NAME) {
            return name;
        }
        let mut gone = name.into_bytes();
        gone.extend_from_slice(b".gone");
        CString::new(gone).unwrap_or_default()
    }
}

const SPENT_MAX: usize = 16;

#[derive(Debug)]
struct KitPool {
    on: bool,
    ready: Vec<SpawnKit>,
    spent: Vec<libc::c_int>,
}

impl KitPool {
    const fn new() -> Self {
        Self {
            on: false,
            ready: Vec::new(),
            spent: Vec::new(),
        }
    }

    fn take(&mut self) -> Option<SpawnKit> {
        if self.on { self.ready.pop() } else { None }
    }

    fn keep_spent(&mut self, fd: libc::c_int) -> Option<libc::c_int> {
        if self.on && self.spent.len() < SPENT_MAX {
            self.spent.push(fd);
            None
        } else {
            Some(fd)
        }
    }
}

impl Drop for KitPool {
    fn drop(&mut self) {
        for fd in self.spent.drain(..) {
            // SAFETY: a spent channel end the pool owned and nothing else uses.
            unsafe {
                libc::close(fd);
            }
        }
    }
}

static KITS: std::sync::Mutex<KitPool> = std::sync::Mutex::new(KitPool::new());

#[cfg(test)]
thread_local! {
    static TEST_KITS: std::cell::Cell<Option<&'static std::sync::Mutex<KitPool>>> =
        const { std::cell::Cell::new(None) };
}

fn kit_pool() -> &'static std::sync::Mutex<KitPool> {
    #[cfg(test)]
    if let Some(pool) = TEST_KITS.with(std::cell::Cell::get) {
        return pool;
    }
    &KITS
}

fn retire_spent(fd: libc::c_int) {
    let kits = kit_pool();
    let refused = kits
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keep_spent(fd);
    if let Some(fd) = refused {
        // SAFETY: the spent channel end this spawn owns; nothing else uses it.
        unsafe {
            libc::close(fd);
        }
    }
}

/// Returns `$HOME` when the current working directory is `/`, or `None` otherwise.
pub fn default_start_folder() -> Option<std::ffi::OsString> {
    if std::env::current_dir().ok().as_deref() == Some(std::path::Path::new("/")) {
        std::env::var_os("HOME")
    } else {
        None
    }
}

/// Opens spawn kits until `depth` are ready and closes spent status descriptors.
pub fn fill_spawn_kits(depth: usize) -> io::Result<usize> {
    fill_kits(kit_pool(), depth)
}

/// How many kits wait in the pool for a spawn.
#[must_use]
pub fn ready_spawn_kits() -> usize {
    kit_pool()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ready
        .len()
}

fn fill_kits(kits: &std::sync::Mutex<KitPool>, depth: usize) -> io::Result<usize> {
    let spent = {
        let mut pool = kits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.on = true;
        std::mem::take(&mut pool.spent)
    };
    for fd in spent {
        // SAFETY: a spent channel end the pool owned; its spawn is done with it.
        unsafe {
            libc::close(fd);
        }
    }
    let mut opened = 0;
    let ready = || {
        kits.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ready
            .len()
    };
    while ready() < depth {
        let kit = SpawnKit::open(None)?;
        kits.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ready
            .push(kit);
        opened += 1;
    }
    Ok(opened)
}

/// A descriptor [`spawn_shell_with_pid_cell_px`] hands its child at a fixed number: the same
/// open file as `fd`, at `at` (above 2), open across the exec. The child holds it beside its
/// stdio and nothing else, a descriptor kept out of children ([`aterm_uds::kept_out`]) included.
#[derive(Debug, Clone, Copy)]
pub struct PassedFd<'fd> {
    /// The parent's descriptor, open until the spawn returns.
    pub fd: std::os::fd::BorrowedFd<'fd>,
    /// The number the child holds it at.
    pub at: libc::c_int,
}

/// Spawns a shell in a fresh `rows`×`cols` PTY, returning [`SpawnedShell`].
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    limits: aterm_sandbox::Limits,
) -> io::Result<SpawnedShell> {
    spawn_shell_with_pid_cell_px(
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        limits,
        None,
        None,
    )
}

/// [`spawn_shell_with_pid`], filling the winsize pixel fields from the host's cell metrics, and
/// handing the child `pass` ([`PassedFd`]) when there is one. A pass the child cannot put in
/// place fails the spawn closed: the child exits before its exec and the call is an `Err`. No
/// shell starts on a pty found dead up to the child's last check before its exec
/// (ATERM-KI-1028-4): one the parent finds dead before the fork (`dead_pty`), a pooled one
/// included, one the child cannot make its controlling terminal, or one dead in the child right
/// after it did, is replaced by a fresh pty once, and a dead replacement is an `Err` naming the
/// cause. A pty that dies after that check (during the exec, or under the running shell) still
/// ends the shell: nothing in user space sees that coming.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "Fork",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "Setrlimit",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "Chdir",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "CloseMaster",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "PassFd",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "fork_exec",
        action = "Exec",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "fork_exec",
        action = "UnsafeEnvOp",
        reason = "Modeled DEFECT only: UnsafeEnvOp fires solely in ForkExec's Buggy branch \
                  (the pre-fix child running setenv/var_os/current_dir/CString/format!/Vec — \
                  async-signal-UNSAFE work in the fork..exec window). The fixed child runs NONE \
                  of these (all env/argv/envp is pre-built in the parent before the fork), so \
                  there is no shipping code to bind; the action exists to let ty PROVE the \
                  defect is excluded (OnlySafeBeforeExec) at Buggy=TRUE."
    )
)]
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid_cell_px(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    limits: aterm_sandbox::Limits,
    cell_px: Option<(u16, u16)>,
    pass: Option<PassedFd<'_>>,
) -> io::Result<SpawnedShell> {
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
    let handed = pass.map(|p| std::os::fd::AsRawFd::as_raw_fd(&p.fd));
    let pass = pass
        .map(|p| aterm_uds::spawnfd::PassAt::new(p.fd, p.at))
        .transpose()?;

    let (shell, origin) = crate::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new));
    let shell_from = origin.suffix();
    let shell_cstring = |name: &std::ffi::OsStr| {
        CString::new(name.as_bytes())
            .map_err(|_| crate::program_nul_refusal(&format!("the shell{shell_from}")))
    };

    let with_shell: Vec<(String, String)>;
    let env_add = match crate::shell_env_for_child() {
        Some(login) => {
            with_shell =
                std::iter::once(("SHELL".to_string(), login.to_string_lossy().into_owned()))
                    .chain(env_add.iter().cloned())
                    .collect();
            &with_shell[..]
        }
        None => env_add,
    };
    let env_pairs = build_child_env(std::env::vars_os(), env_add);
    let env_store: Vec<CString> = env_pairs
        .iter()
        .filter_map(|(k, v)| {
            let mut kv = k.clone();
            kv.push("=");
            kv.push(v);
            CString::new(kv.as_bytes()).ok()
        })
        .collect();
    let mut envp: Vec<*const libc::c_char> = env_store.iter().map(|c| c.as_ptr()).collect();
    envp.push(ptr::null());

    let runs_program = exec_command.is_some_and(|c| !c.is_empty());
    let (exec_target, argv_store): (CString, Vec<CString>) =
        if let Some(cmd) = exec_command.filter(|c| !c.is_empty()) {
            let args: Vec<&std::ffi::OsStr> = cmd.iter().map(|a| a.as_os_str()).collect();
            let argv = argv_cstrings("the -e command", &args)?;
            (resolve_program(&cmd[0]), argv)
        } else if let Some(ov) = argv_override {
            let args: Vec<&std::ffi::OsStr> = ov.iter().map(std::ffi::OsStr::new).collect();
            (
                shell_cstring(shell.as_os_str())?,
                argv_cstrings("the shell's argv", &args)?,
            )
        } else if let Some(args) = shell_args.filter(|a| !a.is_empty()) {
            let cshell = shell_cstring(shell.as_os_str())?;
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            let mut argv = vec![shell_cstring(base)?];
            let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
            argv.extend(argv_cstrings("config `shell_args`", &args)?);
            (cshell, argv)
        } else {
            let cshell = shell_cstring(shell.as_os_str())?;
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            (cshell, vec![shell_cstring(&default_shell_argv0(base))?])
        };

    let (exec_target, argv_store): (CString, Vec<CString>) = if let Some(sbpl) = sandbox_wrap {
        build_sandbox_wrap(SANDBOX_EXEC_PATH, sbpl, &exec_target, &argv_store)?
    } else {
        (exec_target, argv_store)
    };

    let mut argv: Vec<*const libc::c_char> = argv_store.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());

    let chdir_c: Option<CString> = match cwd {
        Some(dir) => Some(CString::new(dir.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the working directory contains a NUL",
            )
        })?),
        None => default_start_folder().and_then(|h| CString::new(h.as_bytes()).ok()),
    };

    let (ws_xpixel, ws_ypixel) = winsize_pixels(rows, cols, cell_px);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    };
    let kits = kit_pool();
    let pooled = kits
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let mut kit = match pooled {
        Some(kit) => {
            kit.size(&ws);
            kit
        }
        None => fresh_kit(&ws)?,
    };
    // What each dead pty this spawn met died of: it forks onto no pty its parent finds dead, and
    // replaces a dead one, found here or by its child, with a fresh one, once
    // ([`replace_dead_pty`]; ATERM-KI-1028-4).
    let mut deaths: Vec<String> = Vec::new();
    let (master, pid, status) = loop {
        if let Some(died_of) = dead_pty(kit.master, kit.slave) {
            // The dead kit closes only once its replacement is open.
            let dead = std::mem::replace(&mut kit, replace_dead_pty(&mut deaths, died_of, &ws)?);
            drop(dead);
            continue;
        }
        let (master, slave, status_rd, status_wr) = kit.into_raw();
        #[cfg(test)]
        spawn_trace::forking_onto(master);
        #[cfg(all(test, target_os = "macos"))]
        let strikes = pty_race::child_strikes(master);
        let inherit_strip = aterm_uds::spawnfd::InheritStrip::prepare();
        // A passed descriptor kept out of children (`aterm_uds::kept_out`) is unmarked for this
        // fork alone, so the child has the copy it puts at its number.
        let spawning = aterm_uds::spawn_lock::handing(handed.as_slice());
        // SAFETY: `fork` takes no arguments and returns child pid, 0, or -1.
        let pid = unsafe { libc::fork() };
        if pid != 0 {
            // The parent's hold ends with the copy. The child's is never released (no lock call is
            // safe there): it execs or `_exit`s.
            drop(spawning);
        }
        if pid < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: fork failed, so no child exists; closing the four owned descriptors.
            unsafe {
                libc::close(slave);
                libc::close(master);
                libc::close(status_rd);
                libc::close(status_wr);
            }
            return Err(err);
        }
        if pid == 0 {
            // SAFETY: `status_rd` is the inherited read-end fd; `close` is a-s-safe.
            unsafe {
                libc::close(status_rd);
            }
            #[cfg(all(test, target_os = "macos"))]
            pty_race::in_child(strikes.0.as_ref());
            // SAFETY: `slave` is the child's inherited pty fd and `status_wr` its exec-status write
            // end; `login_tty`, `write` and `_exit` are a-s-safe, and `b` is a 1-byte stack local.
            // Nothing here allocates, takes a lock, or reads the environment. A slave `login_tty`
            // cannot make the controlling terminal is dead (ATERM-KI-1028-4): the child says `T`
            // and ends before its exec, so no shell runs without a terminal.
            unsafe {
                if libc::login_tty(slave) == -1 {
                    let b: u8 = b'T';
                    libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                    libc::_exit(126);
                }
            }
            #[cfg(all(test, target_os = "macos"))]
            pty_race::in_child(strikes.1.as_ref());
            // SAFETY: `st` and `t` are zeroed plain-integer stack out-params that `fstat` and
            // `tcgetattr` fill from fd 0, the slave `login_tty` just put there; `fstat`,
            // `tcgetattr`, `write` and `_exit` are a-s-safe, and `b` is a 1-byte stack local. No
            // allocation, no lock. A slave that died after `login_tty` took it (ATERM-KI-1028-4
            // a moment later) is dead all the same: the child says `D` and ends before its exec.
            unsafe {
                let mut st: libc::stat = std::mem::zeroed();
                let mut t: libc::termios = std::mem::zeroed();
                if libc::fstat(libc::STDIN_FILENO, &mut st) != 0
                    || libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0
                {
                    let b: u8 = b'D';
                    libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                    libc::_exit(126);
                }
            }
            // SAFETY: `sa` and `empty` are zeroed/initialized locals; signal calls are a-s-safe; no
            // allocation, no locks.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(std::ptr::addr_of_mut!(sa.sa_mask));
                sa.sa_flags = 0;
                let mut sig: libc::c_int = 1;
                while sig < 32 {
                    if sig != libc::SIGKILL && sig != libc::SIGSTOP {
                        libc::sigaction(sig, std::ptr::addr_of!(sa), std::ptr::null_mut());
                    }
                    sig += 1;
                }
                let mut empty: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(std::ptr::addr_of_mut!(empty));
                libc::sigprocmask(
                    libc::SIG_SETMASK,
                    std::ptr::addr_of!(empty),
                    std::ptr::null_mut(),
                );
            }
            inherit_strip.apply();
            if limits.apply(sandbox_cap).is_err() {
                // SAFETY: `write` and `_exit` are async-signal-safe; `b` is a 1-byte stack local.
                unsafe {
                    let b: u8 = b'S';
                    libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                    libc::_exit(126);
                }
            }
            #[cfg(target_os = "macos")]
            if !runs_program {
                // SAFETY: sets this process's own darwin role; takes no pointers.
                unsafe {
                    libc::setpriority(PRIO_DARWIN_ROLE, 0, SHELL_ROLE);
                }
            }
            if let Some(dir) = &chdir_c {
                // SAFETY: the path is a live C string; chdir, write and _exit are
                // async-signal-safe.
                unsafe {
                    if libc::chdir(dir.as_ptr()) != 0 {
                        let b: u8 = b'C';
                        libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                        libc::_exit(126);
                    }
                }
            }
            // SAFETY: `master` is this child's inherited master fd; `close` is async-signal-safe.
            unsafe {
                if master > libc::STDERR_FILENO {
                    libc::close(master);
                }
            }
            let status_wr = match pass {
                None => status_wr,
                // SAFETY: the forked child before its exec, after the strip and the master's close;
                // `status_wr` is its exec-status write end.
                Some(pass) => match unsafe { place_passed(pass, status_wr) } {
                    Ok(status_wr) => status_wr,
                    // SAFETY: `write` and `_exit` are async-signal-safe; `b` is a 1-byte stack
                    // local.
                    Err(status_wr) => unsafe {
                        let b: u8 = b'P';
                        libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                        libc::_exit(126);
                    },
                },
            };
            #[cfg(test)]
            pre_exec_stall::in_child();
            // SAFETY: `exec_target` is a live C string, and `argv` and `envp` null-terminated
            // arrays of live C strings, all outliving the call; `write`/`_exit` are
            // async-signal-safe.
            unsafe {
                libc::execve(exec_target.as_ptr(), argv.as_ptr(), envp.as_ptr());
                let b: u8 = b'E';
                libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                libc::_exit(127);
            }
        }
        // SAFETY: `slave` is the parent's copy of the pty slave fd; `close` releases this copy.
        unsafe {
            libc::close(slave);
        }
        // SAFETY: `status_wr` is the parent's copy of the write end.
        unsafe {
            libc::close(status_wr);
        }
        let status = wait_for_exec_status(status_rd, EXEC_START_HANG);
        retire_spent(status_rd);
        let how = match status {
            ExecStatus::FailedBeforeExec(b'T') => {
                "the shell could not make it its controlling terminal (login_tty failed)"
            }
            ExecStatus::FailedBeforeExec(b'D') => {
                "it died once the shell had made it its controlling terminal (fstat or \
                 tcgetattr on it failed in the child)"
            }
            _ => break (master, pid, status),
        };
        // The child could not take its pty, or lost it: the pty is dead, not the program. Its
        // fresh replacement opens while this master still holds the dead one's number.
        let died_of = format!("{}: {how}", pty_label(master));
        let fresh = replace_dead_pty(&mut deaths, died_of, &ws);
        // SAFETY: `pid` is this attempt's child, ended before its exec, and `master` the parent's
        // owned master fd of the dead pty.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::close(master);
        }
        reap(pid);
        kit = fresh?;
    };
    let failure: Option<(io::ErrorKind, String)> = match status {
        ExecStatus::Execed => None,
        ExecStatus::FailedBeforeExec(b'C') => Some((
            io::ErrorKind::Other,
            "the child's working directory could not be entered; no program was executed"
                .to_owned(),
        )),
        ExecStatus::FailedBeforeExec(b'S') => Some((
            io::ErrorKind::PermissionDenied,
            "sandbox confinement failed in child (fail-closed: shell not exec'd, _exit(126))"
                .to_owned(),
        )),
        ExecStatus::FailedBeforeExec(b'P') => Some((
            io::ErrorKind::Other,
            format!(
                "the child could not hold its passed descriptor at {} (fail-closed: `{}` not \
                 exec'd, _exit(126))",
                pass.map_or(-1, aterm_uds::spawnfd::PassAt::at),
                exec_target.to_string_lossy()
            ),
        )),
        ExecStatus::FailedBeforeExec(_) => Some((
            io::ErrorKind::Other,
            exec_failure(
                &exec_target,
                if runs_program || sandbox_wrap.is_some() {
                    ""
                } else {
                    shell_from
                },
                runs_program,
                chdir_c.as_deref(),
            ),
        )),
        ExecStatus::NoVerdict => Some((
            io::ErrorKind::TimedOut,
            format!(
                "`{}` did not start within {} s",
                exec_target.to_string_lossy(),
                EXEC_START_HANG.as_secs()
            ),
        )),
        ExecStatus::ChannelBroke(err) => Some((
            io::ErrorKind::Other,
            format!(
                "could not confirm `{}` started ({err})",
                exec_target.to_string_lossy()
            ),
        )),
    };
    if let Some((kind, what)) = failure {
        // SAFETY: `pid` is this call's child and `master` is the parent's owned pty master fd.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::close(master);
        }
        reap(pid);
        return Err(io::Error::new(kind, what));
    }
    Ok(SpawnedShell { master, pid })
}

/// In the forked child, after the master's close: put `pass` at its number. The exec-status
/// write end moves off that number first when it sits there (to the lowest free number from 3,
/// still close-on-exec), because the pass's `dup2` would close it: the parent would read that
/// end-of-file as the exec, and a failed exec would be told as a start. Answers the write end's
/// number, or `Err` with it when the pass could not be put in place.
///
/// # Safety
/// Only in the forked child before its exec, after the descriptor strip, with `status_wr` its
/// exec-status write end and nothing else it still needs at `pass.at()`.
unsafe fn place_passed(
    pass: aterm_uds::spawnfd::PassAt,
    status_wr: libc::c_int,
) -> Result<libc::c_int, libc::c_int> {
    let mut status_wr = status_wr;
    if status_wr == pass.at() {
        // SAFETY: `F_DUPFD_CLOEXEC` takes integers and touches no memory.
        let moved = unsafe { libc::fcntl(status_wr, libc::F_DUPFD_CLOEXEC, 3) };
        if moved < 0 {
            return Err(status_wr);
        }
        status_wr = moved;
    }
    // SAFETY: the caller's contract: the forked child after the strip, and `at` holds nothing it needs (the status end has left it).
    if unsafe { pass.apply() } {
        Ok(status_wr)
    } else {
        Err(status_wr)
    }
}

/// Hang up a spawned shell: `SIGHUP` to its process group (`pid` is the session leader and pgid).
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    // SAFETY: `killpg` posts `SIGHUP` to the child's process group and takes no pointers.
    unsafe {
        libc::killpg(pid, libc::SIGHUP);
    }
}

/// Returns the kernel start time for `pid` recorded and verified by [`ShellIdentity`].
#[must_use]
pub fn process_birth(pid: i32) -> Option<u64> {
    if pid <= 1 {
        return None;
    }
    process_birth_platform(pid)
}

#[cfg(target_os = "macos")]
fn process_birth_platform(pid: i32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points to `size` writable bytes matching `PROC_PIDTBSDINFO`.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    if u32::try_from(pid).ok()? != info.pbi_pid || info.pbi_status == libc::SZOMB {
        return None;
    }
    info.pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)
}

#[cfg(target_os = "linux")]
fn process_birth_platform(pid: i32) -> Option<u64> {
    let stat =
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    if fields.next()? == "Z" {
        return None;
    }
    fields
        .nth(18)
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_birth_platform(_pid: i32) -> Option<u64> {
    None
}

fn is_unreaped_child(pid: i32) -> bool {
    child_exit_unreaped(pid).is_some()
}

/// What `pid`, a child of this process, is, read without reaping it (`waitid` with `WNOWAIT`):
/// `Some(false)` running (or stopped), `Some(true)` exited and not yet reaped (a zombie, which
/// keeps its pid, and the id of a group it led, from naming any other process), `None` when no
/// unreaped child of this process has that pid (it was reaped, by std's `Child` or any wait, and
/// the kernel keeps no record of a status std cached) or the read failed. A reaped child's pid
/// names one of this process's children again only once the kernel's pid allocator has wrapped
/// around to it.
#[must_use]
pub fn child_exit_unreaped(pid: i32) -> Option<bool> {
    let id = libc::id_t::try_from(pid).ok()?;
    loop {
        // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is valid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, writable `siginfo_t`, and `WNOWAIT` leaves the child's state
        // intact.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // Only an exit's code is an exit: Darwin's `WEXITED` alone also reports a stopped
            // child (`CLD_STOPPED`, measured on 27.0: `a_stopped_child_is_read_as_running`).
            // When nothing is reported, Linux zeroes the siginfo and XNU leaves it as it was,
            // zeroed here, so `si_signo` is 0.
            return Some(
                info.si_signo == libc::SIGCHLD
                    && matches!(
                        info.si_code,
                        libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
                    ),
            );
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return None;
        }
    }
}

/// Records the [`ShellIdentity`] of a shell this process just spawned.
#[must_use]
pub fn record_spawned_shell(pid: i32) -> ShellIdentity {
    ShellIdentity::from_parts(pid, process_birth(pid), true)
}

/// Records the [`ShellIdentity`] of an adopted shell after verifying `tcgetsid(master) == pid`.
#[must_use]
pub fn record_adopted_shell(pid: i32, master: i32) -> ShellIdentity {
    let leads_the_pty = || {
        // SAFETY: `tcgetsid` is a read-only `TIOCGSID` ioctl on a caller-held descriptor.
        master >= 0 && pid > 1 && unsafe { libc::tcgetsid(master) } == pid
    };
    let birth = if leads_the_pty() {
        process_birth(pid).filter(|_| leads_the_pty())
    } else {
        None
    };
    ShellIdentity::from_parts(pid, birth, false)
}

impl ShellIdentity {
    /// Returns whether [`Self::pid`] still names the shell this identity was recorded for.
    #[must_use]
    pub fn verify(&self) -> bool {
        if self.pid <= 1 {
            return false;
        }
        match (process_birth(self.pid), self.birth) {
            (Some(now), Some(recorded)) => now == recorded,
            _ => self.own_child && is_unreaped_child(self.pid),
        }
    }
}

/// Teardown's `SIGHUP`, sent only where it provably belongs; non-blocking like [`hangup`].
pub fn hangup_shell(identity: &ShellIdentity, master: i32) -> ShellHangup {
    if identity.pid <= 1 {
        return ShellHangup::Nothing;
    }
    if identity.verify() {
        hangup(identity.pid);
        return ShellHangup::Shell(identity.pid);
    }
    if master < 0 {
        return ShellHangup::Nothing;
    }
    // SAFETY: `tcgetpgrp` is a read-only ioctl on a caller-held descriptor.
    let foreground = unsafe { libc::tcgetpgrp(master) };
    if foreground <= 1 {
        return ShellHangup::Nothing;
    }
    // SAFETY: `killpg` posts `SIGHUP` to the foreground process group and takes no pointers.
    unsafe {
        libc::killpg(foreground, libc::SIGHUP);
    }
    ShellHangup::Foreground(foreground)
}

/// Non-blockingly collects an exited child's status before [`reap`] discards it.
#[must_use]
pub fn collect_exit_status(pid: i32) -> Option<crate::ChildExit> {
    if pid <= 1 {
        return None;
    }
    let mut status: libc::c_int = 0;
    let waited = loop {
        // SAFETY: `WNOHANG` `waitpid` with a valid out-param.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break rc;
    };
    if waited != pid {
        return None;
    }
    aterm_uds::deaths::note(pid, status);
    if libc::WIFEXITED(status) {
        return Some(crate::ChildExit::Code(libc::WEXITSTATUS(status)));
    }
    if libc::WIFSIGNALED(status) {
        return Some(crate::ChildExit::Signal(libc::WTERMSIG(status) as u8));
    }
    None
}

/// Runs [`reap_shell`] for an unreaped child `pid` owned by the caller.
pub fn reap(pid: i32) {
    reap_shell(ShellIdentity::from_parts(pid, None, true));
}

/// Reaps a session's shell on the teardown thread, asking [`ignores_hangup`] as it starts.
pub fn reap_shell(identity: ShellIdentity) {
    reap_shell_scheduled(
        identity,
        Some(REAP_KILL_AT),
        REAP_DEADLINE,
        InGrace::enter(),
        ignores_hangup(&identity),
    );
}

/// [`reap_shell`] on a thread of its own, so the UI thread never waits. `ignored_hangup` is
/// [`ignores_hangup`]'s answer from before the hang-up.
pub fn reap_shell_in_background(identity: ShellIdentity, ignored_hangup: bool) {
    if identity.pid <= 1 {
        return;
    }
    let grace = InGrace::enter();
    std::thread::spawn(move || {
        reap_shell_scheduled(
            identity,
            Some(REAP_KILL_AT),
            REAP_DEADLINE,
            grace,
            ignored_hangup,
        );
    });
}

fn reap_shell_scheduled(
    identity: ShellIdentity,
    kill_at: Option<u32>,
    deadline: u32,
    grace: InGrace,
    ignored_hangup: bool,
) {
    reap_shell_draining(identity, kill_at, deadline, grace, ignored_hangup, None);
}

/// Hangs the shell up and reaps it on a thread of its own, so that no close of the master is a
/// second hang-up while the shell handles the first.
///
/// On macOS teardown first sets `CLOCAL` on the terminal and keeps it set ([`Held::Carrier`]):
/// the master's close is then end of file and no hang-up, so no master is retained, and the
/// caller's close both spares a `zshexit` or EXIT trap and ends a shell that waits on input, as
/// zsh at its prompt does before running a `trap ... HUP`'s `exit` (ATERM-KI-1104-7). Elsewhere,
/// or if the flag cannot be set, the terminal is retained as a close-on-exec duplicate drained
/// until the shell settles or the kill tick ([`Held::Master`]). The existing reap deadlines bound
/// both.
///
/// # Errors
/// Returns an error before hanging up if the nonblocking master cannot be retained.
pub fn hangup_and_reap_shell(identity: ShellIdentity, master: i32) -> io::Result<ShellHangup> {
    // SAFETY: fcntl borrows the caller's master; neither operation changes its flags.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_NONBLOCK == 0 {
        return Err(io::Error::other(
            "PTY teardown requires a nonblocking master",
        ));
    }
    let held = match carrier_watch(master) {
        Some(slave) => Held::Carrier(slave),
        None => Held::Master(dup_fd(master)?),
    };
    let ignored = ignores_hangup(&identity);
    let hung = hangup_shell(&identity, master);
    let grace = InGrace::enter();
    std::thread::spawn(move || {
        reap_shell_draining(
            identity,
            Some(REAP_KILL_AT),
            REAP_DEADLINE,
            grace,
            ignored,
            Some(held),
        )
    });
    Ok(hung)
}

/// What teardown keeps of a terminal while its shell handles the hang-up, until the shell settles
/// or the kill tick.
enum Held {
    /// A duplicate of the master, drained, so the caller's close is not the last close.
    Master(std::os::fd::OwnedFd),
    /// A descriptor on the slave through which `CLOCAL` is put back each reap tick
    /// ([`keep_clocal`]). It holds no master, so the master's last close still reaches the shell
    /// as end of file.
    Carrier(std::os::fd::OwnedFd),
}

impl Held {
    /// One reap tick's upkeep: drain the retained master, or put `CLOCAL` back.
    fn tend(&self) {
        use std::os::fd::AsRawFd;
        match self {
            // The existing bounded drain cannot let a noisy job postpone the kill tick.
            Self::Master(master) => {
                let _ = drain_master(master.as_raw_fd(), &mut |_| {});
            }
            Self::Carrier(slave) => {
                let _ = keep_clocal(slave.as_raw_fd());
            }
        }
    }
}

/// Opens `master`'s slave (never as a controlling terminal, never waiting) and sets `CLOCAL` on
/// it: `Some` once the flag reads back. With it, XNU's `ttymodem` sends the session leader no
/// `SIGHUP` when the master's last close drops the carrier, and `ptcclose` still marks the
/// terminal gone, so the slave reads end of file and its writes fail. Set at teardown, not at
/// spawn: a master closed any other way (aterm's crash) still hangs the session up.
///
/// The flag is not set once: a foreground program that exits on the hang-up (vim, less, a
/// readline or curses program) writes back the termios it saved at start, which lacks `CLOCAL`,
/// and a shell may write its own back once that job is reaped. The reaper therefore puts the flag
/// back every tick through this descriptor, so a write-back within one tick (10 ms) of the
/// master's last close is what still makes that close a hang-up. Linux hangs the slave up on the
/// master's close whatever the flag says, so there this answers `None` and the master is
/// retained.
#[cfg(target_os = "macos")]
fn carrier_watch(master: i32) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let name = pts_name(master).ok()?;
    // SAFETY: `name` is a NUL-terminated pts path; `open` reads it and returns a fresh fd or -1.
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is the descriptor `open` just returned, owned by nothing else.
    let slave = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    keep_clocal(slave.as_raw_fd()).then_some(slave)
}

#[cfg(not(target_os = "macos"))]
fn carrier_watch(_master: i32) -> Option<std::os::fd::OwnedFd> {
    None
}

/// Sets `CLOCAL` on `tty`'s termios if it is clear and says whether it holds, read back. A program
/// that writes its termios between this read and this write loses that write to the copy read
/// here, `CLOCAL` added; teardown accepts that for a terminal about to close.
fn keep_clocal(tty: i32) -> bool {
    // SAFETY: every field of `termios` is an integer, so all-zero is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `tcgetattr` reads the live descriptor `tty` into the valid out-param `t`, and
    // `tcsetattr` writes that termios, `CLOCAL` added, back to the same descriptor.
    unsafe {
        if libc::tcgetattr(tty, &raw mut t) != 0 {
            return false;
        }
        if t.c_cflag & libc::CLOCAL != 0 {
            return true;
        }
        t.c_cflag |= libc::CLOCAL;
        if libc::tcsetattr(tty, libc::TCSANOW, &raw const t) != 0 {
            return false;
        }
        libc::tcgetattr(tty, &raw mut t) == 0 && t.c_cflag & libc::CLOCAL != 0
    }
}

fn reap_shell_draining(
    identity: ShellIdentity,
    mut kill_at: Option<u32>,
    deadline: u32,
    mut grace: InGrace,
    ignored_hangup: bool,
    mut held: Option<Held>,
) {
    let pid = identity.pid;
    if pid <= 1 {
        return;
    }
    const POLL: std::time::Duration = REAP_POLL;
    let mut status: libc::c_int = 0;
    for tick in 0..deadline {
        if let Some(held) = held.as_ref() {
            held.tend();
        }
        // Distinct branch.
        if !identity.verify() {
            return;
        }
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            aterm_uds::deaths::note(pid, status);
            return;
        }
        // Alive at the holdout tick is not "ignored SIGHUP": a shell handling its hang-up is
        // alive too, so only one that ignored the signal before it came keeps this tick.
        if Some(tick) == kill_at && tick < REAP_HANDLER_KILL_AT && !ignored_hangup {
            kill_at = Some(REAP_HANDLER_KILL_AT);
        }
        #[cfg(any(test, debug_assertions))]
        if Some(tick) == kill_at {
            while reap_kills_held() {
                if let Some(held) = held.as_ref() {
                    held.tend();
                }
                std::thread::sleep(POLL);
            }
        }
        if Some(tick) == kill_at && identity.verify() {
            if identity.own_child {
                // Verified as this process's unreaped child, which pins the group id: the
                // group is signalled until no member is left (a member's fork can dodge one
                // signal on XNU).
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            } else {
                // SAFETY: `killpg` posts `SIGKILL` to the verified shell's process group. Not
                // this process's child, its pid is not pinned, so it is signalled once.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
            #[cfg(test)]
            REAP_KILL_TICK.with(|t| t.set(Some(tick)));
        }
        if Some(tick) == kill_at {
            drop(held.take());
            grace.leave();
        }
        #[cfg(test)]
        REAP_POLLS.with(|n| n.set(n.get() + 1));
        std::thread::sleep(POLL);
    }
    if identity.own_child && identity.verify() {
        aterm_uds::reaper::hand_off(move || straggler_settled(&identity));
    }
}

/// Whether the kernel says the shell ignores `SIGHUP` now. Asked before the hang-up, this is the
/// one state in which a shell alive after it is a holdout: it chose to outlive the signal. Asked
/// after, a handler that ignores `SIGHUP` for its own cleanup would read the same. A mask nothing
/// can read says no, so no shell is cut early without the kernel's word.
#[must_use]
pub fn ignores_hangup(identity: &ShellIdentity) -> bool {
    aterm_uds::process::ignored_signals(identity.pid)
        .is_some_and(|mask| mask & (1 << (libc::SIGHUP - 1)) != 0)
}

fn straggler_settled(identity: &ShellIdentity) -> bool {
    if !identity.verify() {
        return true;
    }
    let mut status: libc::c_int = 0;
    // SAFETY: `WNOHANG` `waitpid` on the verified own child; a valid out-param.
    let r = unsafe { libc::waitpid(identity.pid, &mut status, libc::WNOHANG) };
    if r == identity.pid {
        aterm_uds::deaths::note(r, status);
    }
    r == identity.pid
        || (r == -1 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
}

static IN_GRACE: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static GRACE_LEFT: std::sync::Condvar = std::sync::Condvar::new();

struct InGrace(bool);

impl InGrace {
    fn enter() -> Self {
        *IN_GRACE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        Self(true)
    }

    fn leave(&mut self) {
        if std::mem::take(&mut self.0) {
            let mut in_grace = IN_GRACE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *in_grace = in_grace.saturating_sub(1);
            GRACE_LEFT.notify_all();
        }
    }
}

impl Drop for InGrace {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Waits up to `bound` until every in-flight [`reap_shell`] settles or sends `SIGKILL`.
pub fn wait_for_reaps(bound: std::time::Duration) -> bool {
    let in_grace = IN_GRACE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (in_grace, _) = GRACE_LEFT
        .wait_timeout_while(in_grace, bound, |n| *n > 0)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *in_grace == 0
}

/// How many [`ReapKillHold`]s are alive. A count, not a flag: tests holding at once in one binary
/// each drop their own guard, and one drop must not release a reap another test still holds.
#[cfg(any(test, debug_assertions))]
static HOLD_REAP_KILLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test seam holding every [`reap_shell`] in its grace window until every guard drops.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn hold_reap_kills_for_test() -> ReapKillHold {
    HOLD_REAP_KILLS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    ReapKillHold(())
}

/// Whether any [`ReapKillHold`] is alive, which keeps a reap at its kill tick.
#[cfg(any(test, debug_assertions))]
fn reap_kills_held() -> bool {
    HOLD_REAP_KILLS.load(std::sync::atomic::Ordering::Acquire) > 0
}

/// [`hold_reap_kills_for_test`]'s guard: once it and every other drops, the held reaps go on.
#[cfg(any(test, debug_assertions))]
pub struct ReapKillHold(());

#[cfg(any(test, debug_assertions))]
impl Drop for ReapKillHold {
    fn drop(&mut self) {
        HOLD_REAP_KILLS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(10);
const REAP_KILL_AT: u32 = 25;
/// The tick that kills a shell alive at [`REAP_KILL_AT`] that did not ignore `SIGHUP` (~1.5 s).
const REAP_HANDLER_KILL_AT: u32 = 150;
const REAP_DEADLINE: u32 = 200;
/// The [`wait_for_reaps`] bound of a process about to exit or `exec`, which ends any reap still
/// running: past the give-up tick, with 40 ms per tick of the handler's grace for ticks a busy
/// machine stretches. A slot held longer than this is one whose group kill still has a member
/// left ([`aterm_uds::process::kill_group_until_empty`]), its first `SIGKILL` already sent.
pub const REAP_GRACE_WAIT: std::time::Duration = std::time::Duration::from_secs(6);
const _: () = assert!(
    REAP_KILL_AT < REAP_HANDLER_KILL_AT
        && REAP_HANDLER_KILL_AT < REAP_DEADLINE
        && REAP_POLL.as_millis() * (REAP_DEADLINE as u128) < REAP_GRACE_WAIT.as_millis()
);

#[cfg(test)]
thread_local! {
    static REAP_POLLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static REAP_KILL_TICK: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn reap_polls() -> u32 {
    REAP_POLLS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn take_reap_kill_tick() -> Option<u32> {
    REAP_KILL_TICK.with(std::cell::Cell::take)
}

/// What an exec failure says: the target, what is wrong with it, and `from` (where a shell came from; empty for a `-e` program or the sandbox wrapper).
pub(crate) fn exec_failure(
    target: &std::ffi::CStr,
    from: &str,
    runs_program: bool,
    cwd: Option<&std::ffi::CStr>,
) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = target.to_string_lossy();
    if path.is_empty() {
        let what = if runs_program {
            "the command"
        } else {
            "the shell path"
        };
        return format!("{what} is empty{from}");
    }
    let why = if !path.contains('/') {
        "is not on PATH"
    } else {
        let at = std::path::Path::new(std::ffi::OsStr::from_bytes(target.to_bytes()));
        let at = match cwd {
            Some(dir) if at.is_relative() => {
                std::path::Path::new(std::ffi::OsStr::from_bytes(dir.to_bytes())).join(at)
            }
            _ => at.to_path_buf(),
        };
        match std::fs::metadata(&at) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => "does not exist",
            Err(_) => "could not be run",
            Ok(m) if m.is_dir() => "is a folder",
            Ok(m) if m.permissions().mode() & 0o111 == 0 => "is not executable",
            Ok(_) => "could not be run",
        }
    };
    format!("`{path}` {why}{from}")
}

fn argv_cstrings(list: &str, args: &[&std::ffi::OsStr]) -> io::Result<Vec<CString>> {
    let mut out = Vec::with_capacity(args.len());
    for (index, arg) in args.iter().enumerate() {
        match CString::new(arg.as_bytes()) {
            Ok(c) => out.push(c),
            Err(_) => return Err(crate::argv_nul_refusal(list, index)),
        }
    }
    Ok(out)
}

#[cfg_attr(trust_verify, trust::skip)]
fn resolve_program(name: &std::ffi::OsStr) -> CString {
    let verbatim = || CString::new(name.as_bytes()).unwrap_or_else(|_| c"/nonexistent".to_owned());
    crate::unix_shell::find_on_path(name, std::env::var_os("PATH").as_deref())
        .and_then(|found| CString::new(found.as_os_str().as_bytes()).ok())
        .unwrap_or_else(verbatim)
}

fn default_shell_argv0(base: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut argv0 = std::ffi::OsString::new();
    if cfg!(target_os = "macos") {
        argv0.push("-");
    }
    argv0.push(base);
    argv0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStep {
    Retry,
    Stop,
    Advance(usize),
}

const fn classify_write_result(r: isize, is_eintr: bool) -> WriteStep {
    if r < 0 {
        if is_eintr {
            WriteStep::Retry
        } else {
            WriteStep::Stop
        }
    } else if r == 0 {
        WriteStep::Stop
    } else {
        WriteStep::Advance(r as usize)
    }
}

/// Writes all of `bytes` to the PTY master, retrying short writes and `EINTR`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "write_all",
        action = "Progress",
        project = "aterm_pty::write_all_project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "write_all",
        action = "Interrupted",
        project = "aterm_pty::write_all_project"
    )
)]
pub fn write_all(master: i32, bytes: &[u8]) {
    let mut data = bytes;
    while !data.is_empty() {
        // SAFETY: `master` is a valid PTY master fd and `data` is a valid slice.
        let r = unsafe { libc::write(master, data.as_ptr() as *const libc::c_void, data.len()) };
        let is_eintr = r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
        match classify_write_result(r, is_eintr) {
            WriteStep::Retry => continue,
            WriteStep::Stop => break,
            WriteStep::Advance(n) => data = data.get(n..).unwrap_or(&[]),
        }
    }
}

/// Writes `bytes` to the PTY master with a single `write(2)` (retrying `EINTR`).
#[inline]
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    loop {
        // SAFETY: `master` is a valid PTY master fd and `bytes` is a valid slice.
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r as usize);
    }
}

/// Unix twin of Windows `as_reply`; invokes `write()` directly.
#[inline]
pub fn as_reply<T>(write: impl FnOnce() -> T) -> T {
    write()
}

/// Unix twin of Windows `settle_input`; a no-op on POSIX PTYs.
#[inline]
pub const fn settle_input(_master: i32) {}

/// Outcome of a single non-parking `write(2)` in [`write_some_nonparking`].
#[cfg(unix)]
#[derive(Debug)]
pub enum NonParkWrite {
    /// `n > 0` bytes accepted; advance the cursor by `n`.
    Wrote(usize),
    /// Peer closed mid-write (`r == 0`) or nothing to write: stop.
    Closed,
    /// The fd would block (`EAGAIN`/`EWOULDBLOCK`): spill the tail.
    WouldBlock,
    /// A real error (the `Os(errno)` variant): the caller returns it, never drops it.
    Fatal(io::Error),
}

/// One non-parking `write(2)`, classified as [`NonParkWrite`]; `EINTR` retries.
#[cfg(unix)]
#[inline]
pub fn write_some_nonparking(master: i32, bytes: &[u8]) -> NonParkWrite {
    if bytes.is_empty() {
        return NonParkWrite::Closed;
    }
    loop {
        // SAFETY: `master` is a valid PTY master fd and `bytes` is a valid slice.
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return NonParkWrite::WouldBlock,
                _ => return NonParkWrite::Fatal(e),
            }
        }
        if r == 0 {
            return NonParkWrite::Closed;
        }
        return NonParkWrite::Wrote(r as usize);
    }
}

/// Poll the PTY master for writability, blocking up to `timeout_ms` (`0`: check and return).
#[inline]
pub fn poll_writable(fd: i32, timeout_ms: i32) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: `p` is a valid pollfd for the duration of the call; nfds == 1.
        let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
}

/// Whether the PTY master `master` holds output the slave wrote and nobody has read yet, asked
/// now (a `poll` for `POLLIN` that does not wait); an error or a hangup answers yes, so a caller
/// that acts only on a quiet master never acts on one it could not read. A `false` answer means
/// every byte the slave wrote before the call was read already: the macOS master's queue is
/// written within the slave's own `write`, and Linux's `n_tty` poll first flushes the pty's
/// pending buffer work (the work queue that moves a slave's write to the master) before it
/// answers no.
#[must_use]
pub fn has_unread(master: i32) -> bool {
    let mut p = libc::pollfd {
        fd: master,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `p` is a valid pollfd for the duration of the call; nfds == 1.
        let r = unsafe { libc::poll(&mut p, 1, 0) };
        if r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return r != 0;
    }
}

/// Duplicates `fd` with `F_DUPFD_CLOEXEC` into an owned [`std::os::fd::OwnedFd`].
#[inline]
pub fn dup_fd(fd: i32) -> io::Result<std::os::fd::OwnedFd> {
    // SAFETY: `fcntl(F_DUPFD_CLOEXEC, 0)` returns a fresh fd or -1, checked before `from_raw_fd`.
    let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `d` is a fresh fd we exclusively own, just created by the `fcntl` above.
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(d) })
}

/// Toggles `O_NONBLOCK` on the PTY master descriptor.
pub fn set_nonblocking(master: i32, nonblocking: bool) -> io::Result<()> {
    // SAFETY: `master` is a valid fd; `F_GETFL`/`F_SETFL` only read/modify flags.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if next == flags {
        return Ok(());
    }
    // SAFETY: `master` is a valid fd; `F_SETFL` sets the flags word we derived.
    let rc = unsafe { libc::fcntl(master, libc::F_SETFL, next) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Toggle `FD_CLOEXEC`, the descriptor flag (`F_GETFD`/`F_SETFD`), on `fd`.
pub fn set_cloexec(fd: i32, on: bool) -> io::Result<()> {
    // SAFETY: `fd` is a valid fd; `F_GETFD` only reads the descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if next == flags {
        return Ok(());
    }
    // SAFETY: `fd` is a valid fd; `F_SETFD` sets the descriptor-flags word we derived.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, next) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Returns the kernel `(rows, cols)` size (`TIOCGWINSZ`) of `master`, or `None` if unset.
#[must_use]
pub fn size(master: i32) -> Option<(u16, u16)> {
    if master < 0 {
        return None;
    }
    // SAFETY: `libc::winsize` is a plain struct of integer fields; all zeros is a valid value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `master` may be any integer: the ioctl fails cleanly on a bad or non-tty fd, and
    // `ws` is a valid `winsize` out-param on success.
    let rc = unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) };
    (rc == 0 && (ws.ws_row, ws.ws_col) != (0, 0)).then_some((ws.ws_row, ws.ws_col))
}

/// Returns whether `fd` responds to `TIOCGWINSZ` as a terminal.
#[must_use]
pub fn fd_is_tty(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    // SAFETY: `libc::winsize` is a plain integer struct; all zeros is a valid value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer: the ioctl fails cleanly on a bad or non-tty fd, and `ws`
    // is a valid `winsize` out-param on success.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    rc == 0
}

/// The process group that leads `fd`'s terminal's foreground (`tcgetpgrp`), or `None` when the
/// read fails or names no group.
#[must_use]
pub fn foreground_group(fd: i32) -> Option<i32> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` may be any integer: `tcgetpgrp` fails cleanly (-1) on a bad or non-tty fd.
    let group = unsafe { libc::tcgetpgrp(fd) };
    (group > 0).then_some(group)
}

/// Reads the slave's `ECHO`, `ICANON`, and `ISIG` signal characters via `fd` ([`crate::TtyEcho`]).
#[must_use]
pub fn tty_echo(fd: i32) -> Option<crate::TtyEcho> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `libc::termios` is a plain integer/byte-array struct; all zeros is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer: `tcgetattr` fails cleanly on a bad or non-tty fd, and `t`
    // is a valid `termios` out-param on success.
    let rc = unsafe { libc::tcgetattr(fd, &mut t) };
    if rc != 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    const VDISABLE: libc::cc_t = 0;
    #[cfg(not(target_os = "linux"))]
    const VDISABLE: libc::cc_t = 0xff;
    let special = |slot: usize| Some(t.c_cc[slot]).filter(|&c| c != VDISABLE);
    Some(crate::TtyEcho {
        echo: t.c_lflag & libc::ECHO != 0,
        canonical: t.c_lflag & libc::ICANON != 0,
        signals: (t.c_lflag & libc::ISIG != 0).then(|| crate::TtySignals {
            intr: special(libc::VINTR),
            quit: special(libc::VQUIT),
            susp: special(libc::VSUSP),
        }),
    })
}

/// Returns whether the slave line discipline passes every written byte to the input queue.
#[must_use]
pub fn tty_passes_every_byte(fd: i32) -> Option<bool> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `libc::termios` is a plain integer/byte-array struct; all zeros is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer: `tcgetattr` fails cleanly on a bad or non-tty fd, and `t`
    // is a valid `termios` out-param on success.
    let rc = unsafe { libc::tcgetattr(fd, &mut t) };
    if rc != 0 {
        return None;
    }
    Some(
        t.c_lflag & (libc::ICANON | libc::ISIG | libc::IEXTEN) == 0
            && t.c_iflag & (libc::IXON | libc::IGNCR) == 0,
    )
}

#[cfg(target_os = "macos")]
const TIOCOUTQ: libc::c_ulong = 0x4004_7473;

/// Returns unread slave input queue bytes (`FIONREAD`) on `fd`, macOS alone: the count
/// [`flush_input_queue`]'s discard is paired with (Linux's `tcflush` on a master drops the
/// program's output, not its input). A reader of the program's unread input reads
/// [`unread_input_len`], which Linux answers too.
#[must_use]
pub fn input_queue_len(fd: i32) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        queue_len(fd, libc::FIONREAD)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = fd;
        None
    }
}

/// Returns the bytes typed into the terminal behind the pty master `master` that the program at
/// its slave has not read (canonical mode: whole lines only), or `None` where that is unknown,
/// never a guess of zero. macOS reads it as [`input_queue_len`] does, by `FIONREAD` on the master.
/// On Linux the master's own `FIONREAD` counts the program's output waiting for aterm, so this
/// reads `FIONREAD` on a descriptor of the slave that `TIOCGPTPEER` opens for the call (read-only,
/// never as a controlling terminal, never waiting) and closes: `None` where that open fails (a
/// kernel before 4.13, a terminal its program made exclusive) or `master` is no pty master. A
/// write reaches Linux's slave from a work queue, so a key written a moment before may not count
/// yet. `None` on every other platform.
#[must_use]
pub fn unread_input_len(master: i32) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        queue_len(master, libc::FIONREAD)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        if master < 0 {
            return None;
        }
        // SAFETY: `TIOCGPTPEER` takes its open flags by value and returns a fresh descriptor or
        // -1; on any fd that is not a pty master it fails cleanly (`ENOTTY`, `EBADF`).
        let peer = unsafe {
            libc::ioctl(
                master,
                libc::TIOCGPTPEER,
                libc::O_RDONLY | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if peer < 0 {
            return None;
        }
        // SAFETY: `peer` is the descriptor `TIOCGPTPEER` just returned, owned by nothing else.
        let peer = unsafe { OwnedFd::from_raw_fd(peer) };
        let mut n: libc::c_int = 0;
        // SAFETY: `peer` is open for the call and `FIONREAD` writes one `c_int` into `n`.
        let rc = unsafe { libc::ioctl(peer.as_raw_fd(), libc::FIONREAD, &raw mut n) };
        if rc != 0 {
            return None;
        }
        usize::try_from(n).ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = master;
        None
    }
}

/// Returns unread master output queue bytes (`TIOCOUTQ`) on `fd`.
#[must_use]
pub fn output_queue_len(fd: i32) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        queue_len(fd, TIOCOUTQ)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = fd;
        None
    }
}

/// Flushes unread slave input bytes (`TCIFLUSH`) on `fd` and returns the flushed count.
#[must_use]
pub fn flush_input_queue(fd: i32) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let unread = queue_len(fd, libc::FIONREAD)?;
        // SAFETY: `fd` is a verified tty fd and `tcflush` takes no pointers.
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
            return None;
        }
        Some(unread)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if !fd_is_tty(fd) {
            return None;
        }
        // SAFETY: `fd` is a verified tty fd and `tcflush` takes no pointers.
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
            return None;
        }
        Some(0)
    }
}

#[cfg(target_os = "macos")]
fn queue_len(fd: i32, request: libc::c_ulong) -> Option<usize> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `isatty` takes an integer fd and queries the kernel without UB.
    if unsafe { libc::isatty(fd) } != 1 {
        return None;
    }
    let mut n: libc::c_int = 0;
    // SAFETY: `request` is `FIONREAD` or `TIOCOUTQ`, writing one `c_int` into `n`.
    let rc = unsafe { libc::ioctl(fd, request, &mut n) };
    if rc != 0 {
        return None;
    }
    usize::try_from(n).ok()
}

/// Read up to `buf.len()` bytes from the PTY master into `buf`.
pub fn read(master: i32, buf: &mut [u8]) -> isize {
    // SAFETY: `master` is a valid fd; `buf` is a valid mutable slice of `buf.len()` bytes.
    unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) }
}

/// Creates a non-blocking, close-on-exec `(read_end, write_end)` wake pipe.
///
/// Off Linux, where no `pipe2` sets the flags at creation, the pipe is made and flagged inside
/// [`aterm_uds::kept_out`]. On Apple that holds every spawn off between the pipe's creation and
/// its last flag, so no child copies an end before it is close-on-exec, and keeps each end out of
/// children (`FD_CLOFORK`, where the kernel keeps that mark).
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    #[cfg(target_os = "linux")]
    {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-int out-array for `pipe2`.
        (unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0)
            .then_some((fds[0], fds[1]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd};
        let (rd, wr) = aterm_uds::kept_out(|| {
            let mut fds = [0i32; 2];
            // SAFETY: `fds` is a valid 2-int out-array for pipe(2).
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: pipe(2) just opened both ends and nothing else owns them, so each is
            // closed when its owner drops, a failed flag included.
            let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            for end in [&ends.0, &ends.1] {
                let fd = end.as_raw_fd();
                // SAFETY: `fd` is an end this call owns; these fcntls only read and set flags.
                let configured = unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != -1
                        && flags >= 0
                        && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
                };
                if !configured {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(ends)
        })
        .ok()?;
        Some((rd.into_raw_fd(), wr.into_raw_fd()))
    }
}

/// Outcome of [`read_or_wake`].
pub enum ReadOutcome {
    /// `n` bytes (`n > 0`) were read into the caller's buffer.
    Data(usize),
    /// The master reached EOF / error — the child's slave closed (the shell exited).
    Eof,
    /// The wake pipe fired because the owning session closed.
    Wake,
    /// The poll timed out with no activity: the wakeless poll's own timeout, or the caller's
    /// ([`read_or_poke`]'s `within_ms`).
    Idle,
    /// The shell's exit watch (`exit_rd`) fired.
    Exited,
    /// The session's process watch (`proc_rd`, `aterm_uds::procwatch`) has events queued.
    Proc,
    /// The poke pipe (`poke_rd`, [`read_or_poke`]) fired, or its write end closed: something asks
    /// the reader to answer before it reads the master again. Nothing was read; [`drain_pokes`]
    /// empties the pipe and says whether it can fire again.
    Poke,
}

const fn proc_fired(entry: &libc::pollfd) -> bool {
    entry.revents & libc::POLLIN != 0
}

const fn exit_fired(entry: &libc::pollfd) -> bool {
    entry.revents & (libc::POLLIN | libc::POLLHUP) != 0
}

const WAKELESS_POLL_TIMEOUT_MS: libc::c_int = 250;

struct ReadFds {
    master: i32,
    wake: i32,
    exit: i32,
    process: i32,
    poke: i32,
}

trait ReadWait {
    fn now(&mut self) -> std::time::Instant;
    fn poll(&mut self, fds: &mut [libc::pollfd], timeout: i32) -> io::Result<usize>;
    fn read(&mut self, master: i32, buf: &mut [u8]) -> io::Result<usize>;
}

struct SystemReadWait;

impl ReadWait for SystemReadWait {
    fn now(&mut self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn poll(&mut self, fds: &mut [libc::pollfd], timeout: i32) -> io::Result<usize> {
        let count = libc::nfds_t::try_from(fds.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "poll descriptor count"))?;
        // SAFETY: the initialized descriptor slice remains writable for its exact count.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }

    fn read(&mut self, master: i32, buf: &mut [u8]) -> io::Result<usize> {
        let result = read(master, buf);
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }
}

/// One original wait bound. Even a zero bound permits its initial nonblocking ready read;
/// every later retry consumes elapsed monotonic time instead of restarting that bound.
struct ReadDeadline {
    until: Option<std::time::Instant>,
    first: bool,
}

impl ReadDeadline {
    fn new(timeout: i32, wait: &mut impl ReadWait) -> Option<Self> {
        let until = if timeout < 0 {
            None
        } else {
            Some(
                wait.now()
                    .checked_add(std::time::Duration::from_millis(timeout as u64))?,
            )
        };
        Some(Self { until, first: true })
    }

    fn next(&mut self, wait: &mut impl ReadWait) -> Option<i32> {
        let Some(until) = self.until else {
            return Some(-1);
        };
        let now = wait.now();
        if !self.first && now >= until {
            return None;
        }
        self.first = false;
        let millis = until
            .saturating_duration_since(now)
            .as_nanos()
            .div_ceil(1_000_000);
        Some(i32::try_from(millis).unwrap_or(i32::MAX))
    }
}

/// Block until the master is readable (or EOF), `wake_rd` fires, or `exit_rd` fires.
pub fn read_or_wake(
    master: i32,
    buf: &mut [u8],
    wake_rd: i32,
    exit_rd: i32,
    proc_rd: i32,
) -> ReadOutcome {
    read_or_poke(master, buf, wake_rd, exit_rd, proc_rd, -1, -1)
}

/// [`read_or_wake`] that also answers [`ReadOutcome::Poke`] when `poke_rd` (`-1` for none) fires,
/// before it reads the master, and [`ReadOutcome::Idle`] when its original `within_ms` wait
/// expires, including time spent in interrupted polls or reads and ignored watch readiness.
/// A zero bound still reads immediately ready data once; any negative bound is unbounded.
/// A wakeless poll keeps its own 250 ms cap, the shorter finite bound counting.
pub fn read_or_poke(
    master: i32,
    buf: &mut [u8],
    wake_rd: i32,
    exit_rd: i32,
    proc_rd: i32,
    poke_rd: i32,
    within_ms: libc::c_int,
) -> ReadOutcome {
    read_or_poke_with(
        ReadFds {
            master,
            wake: wake_rd,
            exit: exit_rd,
            process: proc_rd,
            poke: poke_rd,
        },
        buf,
        within_ms,
        &mut SystemReadWait,
    )
}

fn read_or_poke_with(
    fds: ReadFds,
    buf: &mut [u8],
    within_ms: i32,
    wait: &mut impl ReadWait,
) -> ReadOutcome {
    let ReadFds {
        master,
        wake: wake_rd,
        exit: exit_rd,
        process: proc_rd,
        poke: poke_rd,
    } = fds;
    let timeout = if wake_rd < 0 {
        if within_ms < 0 {
            WAKELESS_POLL_TIMEOUT_MS
        } else {
            within_ms.min(WAKELESS_POLL_TIMEOUT_MS)
        }
    } else {
        within_ms
    };
    let Some(mut deadline) = ReadDeadline::new(timeout, wait) else {
        return ReadOutcome::Idle;
    };
    let exit_watch = libc::pollfd {
        fd: exit_rd,
        events: libc::POLLIN,
        revents: 0,
    };
    let proc_watch = libc::pollfd {
        fd: proc_rd,
        events: libc::POLLIN,
        revents: 0,
    };
    let poke_watch = libc::pollfd {
        fd: poke_rd,
        events: libc::POLLIN,
        revents: 0,
    };
    if wake_rd < 0 {
        let mut fds = [
            libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            },
            exit_watch,
            proc_watch,
            poke_watch,
        ];
        let Some(timeout) = deadline.next(wait) else {
            return ReadOutcome::Idle;
        };
        let r = match wait.poll(&mut fds, timeout) {
            Ok(ready) => ready,
            Err(error) => {
                return if error.kind() == io::ErrorKind::Interrupted {
                    ReadOutcome::Idle
                } else {
                    ReadOutcome::Eof
                };
            }
        };
        if r == 0 {
            return ReadOutcome::Idle;
        }
        if fds[3].revents != 0 {
            return ReadOutcome::Poke;
        }
        if exit_fired(&fds[1]) {
            return ReadOutcome::Exited;
        }
        if proc_fired(&fds[2]) {
            return ReadOutcome::Proc;
        }
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            match wait.read(master, buf) {
                Ok(n) if n > 0 => return ReadOutcome::Data(n),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return ReadOutcome::Idle;
                }
                _ => return ReadOutcome::Eof,
            }
        }
        return ReadOutcome::Idle;
    }
    let mut fds = [
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_rd,
            events: libc::POLLIN,
            revents: 0,
        },
        exit_watch,
        proc_watch,
        poke_watch,
    ];
    loop {
        let Some(timeout) = deadline.next(wait) else {
            return ReadOutcome::Idle;
        };
        let r = match wait.poll(&mut fds, timeout) {
            Ok(ready) => ready,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return ReadOutcome::Eof,
        };
        if r == 0 {
            return ReadOutcome::Idle;
        }
        if fds[1].revents != 0 {
            return ReadOutcome::Wake;
        }
        if fds[4].revents != 0 {
            return ReadOutcome::Poke;
        }
        if exit_fired(&fds[2]) {
            return ReadOutcome::Exited;
        }
        if fds[2].revents != 0 {
            fds[2].fd = -1;
        }
        if proc_fired(&fds[3]) {
            return ReadOutcome::Proc;
        }
        if fds[3].revents != 0 {
            fds[3].fd = -1;
        }
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            match wait.read(master, buf) {
                Ok(n) if n > 0 => return ReadOutcome::Data(n),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                _ => return ReadOutcome::Eof,
            }
        }
    }
}

/// Tops up `buf` with immediately available bytes after an initial [`read_or_wake`] read.
pub fn drain_more(master: i32, buf: &mut [u8], filled: usize) -> usize {
    drain_more_watched(master, buf, filled, -1, &mut |_| false)
}

/// [`drain_more`] polling `proc_rd` before `master` and invoking `heard(filled)` when readable.
pub fn drain_more_watched(
    master: i32,
    buf: &mut [u8],
    mut filled: usize,
    proc_rd: i32,
    heard: &mut dyn FnMut(usize) -> bool,
) -> usize {
    const SATURATED: usize = 1024;
    const DRAIN_SPIN_MAX: u32 = 16;
    let mut spins = 0u32;
    let mut proven = false;
    while filled < buf.len() {
        let mut fds = [
            libc::pollfd {
                fd: proc_rd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid 2-element `pollfd` array live for the `poll` call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, 0) };
        if r > 0 && proc_fired(&fds[0]) {
            proven = true;
            if heard(filled) {
                break;
            }
            continue;
        }
        proven |= r >= 0 && fds[0].revents == 0;
        if r > 0 && fds[1].revents & libc::POLLIN != 0 {
            let n = read(master, &mut buf[filled..]);
            if n <= 0 {
                break;
            }
            filled += n as usize;
            proven = false;
            spins = 0;
        } else {
            // Quiet (or POLLHUP/EINTR — the outer read_or_wake handles those).
            if filled < SATURATED || spins >= DRAIN_SPIN_MAX {
                break;
            }
            spins += 1;
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
    }
    if proc_rd >= 0 && !proven && watch_ready(proc_rd) {
        heard(filled);
    }
    filled
}

#[cfg(test)]
thread_local! {
    static WATCH_CHECKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn watch_ready(proc_rd: i32) -> bool {
    #[cfg(test)]
    WATCH_CHECKS.with(|n| n.set(n.get() + 1));
    let mut fds = [libc::pollfd {
        fd: proc_rd,
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one pollfd, live for the call, no wait.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) > 0 && proc_fired(&fds[0]) }
}

enum EndPoll {
    Proc,
    Master,
    Quiet,
    Unknown,
}

fn end_poll(master: i32, proc_rd: i32) -> EndPoll {
    let mut fds = [
        libc::pollfd {
            fd: proc_rd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: a valid 2-element pollfd array of descriptors this gather owns, no wait.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, 0) };
    if r < 0 {
        return EndPoll::Unknown;
    }
    if proc_fired(&fds[0]) {
        return EndPoll::Proc;
    }
    if fds[0].revents != 0 {
        return EndPoll::Unknown;
    }
    let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if fds[1].revents & ready != 0 {
        EndPoll::Master
    } else {
        EndPoll::Quiet
    }
}

const IDLE_POLL_DEFAULT_US: u32 = 50;

const HOT_BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(1);

fn idle_poll_us() -> u32 {
    const IDLE_POLL_CLAMP_US: u32 = 5_000;
    static IDLE_POLL_US: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *IDLE_POLL_US.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_PTY_IDLE_POLL_US")
            .and_then(|v| {
                let s = v.to_string_lossy();
                let s = s.trim();
                s.bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| s.parse().ok())
                    .flatten()
            })
            .unwrap_or(IDLE_POLL_DEFAULT_US)
            .min(IDLE_POLL_CLAMP_US)
    })
}

enum BridgeWait {
    Refill,
    Quiet,
    Deliver,
    Proc,
}

#[cfg(test)]
thread_local! {
    static BRIDGE_POLLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    static IDLE_WAIT_INTERRUPTS: std::cell::RefCell<
        Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static BUDGET_FOR_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    static BUDGET_ENDED: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

fn batch_budget(shipped: std::time::Duration) -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        return budget;
    }
    shipped
}

fn bridge_poll(master: i32, wake_rd: i32, proc_rd: i32, timeout_ms: i32) -> BridgeWait {
    #[cfg(test)]
    BRIDGE_POLLS.with(|n| n.set(n.get() + 1));
    let mut fds = [
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_rd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: proc_rd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: polling fds this gather owns, with a bounded timeout.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), 3, timeout_ms) };
    if r == 0 {
        return BridgeWait::Quiet;
    }
    if r < 0 {
        return BridgeWait::Deliver;
    }
    if fds[1].revents != 0 {
        return BridgeWait::Deliver;
    }
    if proc_fired(&fds[2]) {
        return BridgeWait::Proc;
    }
    if fds[0].revents & libc::POLLIN != 0 {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver
}

fn idle_refill_wait(master: i32, wake_rd: i32, proc_rd: i32, wait_us: u32) -> BridgeWait {
    // select cannot represent a negative fd or one >= FD_SETSIZE (FD_SET would index outside the
    // set), and a wait with no master has nothing to refill from — deliver, same as knob-off.
    if !(0..libc::FD_SETSIZE as i32).contains(&master)
        || wake_rd >= libc::FD_SETSIZE as i32
        || proc_rd >= libc::FD_SETSIZE as i32
    {
        return BridgeWait::Deliver;
    }
    let wait = std::time::Duration::from_micros(u64::from(wait_us));
    let start = std::time::Instant::now();
    let mut left = wait;
    let (r, set) = loop {
        // SAFETY: an all-zero fd_set is the empty set FD_ZERO produces.
        let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
        // SAFETY: `master` is in 0..FD_SETSIZE (checked above), and the others are below
        // FD_SETSIZE and set only when not negative.
        unsafe {
            libc::FD_SET(master, &mut set);
            if wake_rd >= 0 {
                libc::FD_SET(wake_rd, &mut set);
            }
            if proc_rd >= 0 {
                libc::FD_SET(proc_rd, &mut set);
            }
        }
        let mut tv = libc::timeval {
            tv_sec: left.as_secs() as _,
            tv_usec: left.subsec_micros() as _,
        };
        // SAFETY: selecting on fds this gather owns, with a bounded timeout.
        let r = unsafe {
            libc::select(
                master.max(wake_rd).max(proc_rd) + 1,
                &mut set,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            #[cfg(test)]
            IDLE_WAIT_INTERRUPTS.with(|count| {
                if let Some(count) = count.borrow().as_ref() {
                    count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
            left = wait.saturating_sub(start.elapsed());
            if left.is_zero() {
                return BridgeWait::Deliver;
            }
            continue;
        }
        break (r, set);
    };
    if r == 0 {
        return BridgeWait::Quiet;
    }
    if r < 0 {
        return BridgeWait::Deliver;
    }
    // SAFETY: reading a set select just filled; fds are < FD_SETSIZE.
    if wake_rd >= 0 && unsafe { libc::FD_ISSET(wake_rd, &set) } {
        return BridgeWait::Deliver;
    }
    // SAFETY: as above.
    if proc_rd >= 0 && unsafe { libc::FD_ISSET(proc_rd, &set) } {
        return BridgeWait::Proc;
    }
    // SAFETY: as above.
    if unsafe { libc::FD_ISSET(master, &set) } {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver
}

/// Non-blocking [`drain_more`] that spins across brief writer refill gaps before polling.
#[allow(
    clippy::too_many_arguments,
    reason = "the gather threads its owned descriptors and independent scheduling inputs explicitly"
)]
pub fn drain_more_nonblocking(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    proc_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    interactive_pending: fn() -> bool,
    at_gap: &mut dyn FnMut(usize, bool) -> bool,
) -> usize {
    drain_more_nonblocking_with_idle_wait_after_gap(
        master,
        buf,
        filled,
        wake_rd,
        proc_rd,
        parse_in_flight,
        interactive_pending,
        idle_poll_us(),
        at_gap,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the gather's knobs are threaded explicitly so tests can drive each one \
              (idle wait, interactive hint, park seam) without a builder on the hot path"
)]
fn drain_more_nonblocking_with_idle_wait_after_gap(
    master: i32,
    buf: &mut [u8],
    mut filled: usize,
    wake_rd: i32,
    proc_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    interactive_pending: fn() -> bool,
    idle_wait_us: u32,
    mut at_gap: impl FnMut(usize, bool) -> bool,
) -> usize {
    const SATURATED: usize = 1024;
    const NB_SPIN_MAX: u32 = 16;
    const BRIDGE_POLL_MS: i32 = 1;
    const BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);
    let start = std::time::Instant::now();
    let mut spins = 0u32;
    let mut proven = false;
    while filled < buf.len() {
        // A batch under one tty queue ends at its first quiet poll of the watch and the
        // master, which stands in for the dry `read` that would end it: no added system
        // call, and the batch never ends past an `exec` the watch could hear.
        if proc_rd >= 0 && filled < SATURATED {
            match end_poll(master, proc_rd) {
                EndPoll::Proc => {
                    proven = true;
                    if at_gap(filled, true) {
                        break;
                    }
                    continue;
                }
                EndPoll::Quiet => {
                    proven = true;
                    break;
                }
                EndPoll::Master => proven = true,
                EndPoll::Unknown => {}
            }
        }
        let n = read(master, &mut buf[filled..]);
        if n > 0 {
            filled += n as usize;
            proven = false;
            spins = 0;
            continue;
        }
        if n == 0 {
            break;
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                let hot = interactive_pending();
                let budget = batch_budget(if hot { HOT_BATCH_BUDGET } else { BATCH_BUDGET });
                if filled < SATURATED {
                    break;
                }
                if start.elapsed() >= budget {
                    #[cfg(test)]
                    BUDGET_ENDED.with(|b| b.set(Some(budget)));
                    break;
                }
                if spins < NB_SPIN_MAX {
                    spins += 1;
                    continue;
                }
                if hot
                    || parse_in_flight
                        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
                {
                    // Distinct branch.
                    if idle_wait_us == 0 {
                        break;
                    }
                    if at_gap(filled, false) {
                        proven = true;
                        break;
                    }
                    match idle_refill_wait(master, wake_rd, proc_rd, idle_wait_us) {
                        BridgeWait::Refill => {
                            proven = true;
                            spins = 0;
                            continue;
                        }
                        BridgeWait::Proc => {
                            proven = true;
                            if at_gap(filled, true) {
                                break;
                            }
                            spins = 0;
                            continue;
                        }
                        BridgeWait::Quiet => {
                            proven = true;
                            break;
                        }
                        BridgeWait::Deliver => break,
                    }
                }
                if at_gap(filled, false) {
                    proven = true;
                    break;
                }
                match bridge_poll(master, wake_rd, proc_rd, BRIDGE_POLL_MS) {
                    BridgeWait::Refill => {
                        proven = true;
                        spins = 0;
                    }
                    BridgeWait::Proc => {
                        proven = true;
                        if at_gap(filled, true) {
                            break;
                        }
                        spins = 0;
                    }
                    BridgeWait::Quiet => {
                        proven = true;
                        break;
                    }
                    BridgeWait::Deliver => break,
                }
            }
            _ => break,
        }
    }
    if proc_rd >= 0 && !proven && watch_ready(proc_rd) {
        at_gap(filled, true);
    }
    filled
}

/// Wakes a reader parked in [`read_or_wake`]: one byte to the wake pipe.
pub fn wake(wake_wr: i32) {
    let b = [1u8];
    // SAFETY: `wake_wr` is the pipe write end; a 1-byte write is atomic (never partial).
    unsafe {
        libc::write(wake_wr, b.as_ptr() as *const libc::c_void, 1);
    }
}

/// Reads a non-blocking [`make_wake_pipe`] read end until it is empty (a [`ReadOutcome::Poke`]).
///
/// Returns `false` once the pipe's write end is closed (or `poke_rd` is no pipe), when it can never
/// fire again and the caller stops polling it.
#[must_use]
pub fn drain_pokes(poke_rd: i32) -> bool {
    let mut sink = [0u8; 64];
    loop {
        // SAFETY: `poke_rd` is the caller's non-blocking pipe read end; `sink` is a live buffer
        // of `sink.len()` bytes.
        let n = unsafe { libc::read(poke_rd, sink.as_mut_ptr().cast(), sink.len()) };
        if n > 0 {
            continue;
        }
        return n < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            );
    }
}

/// `PRIO_DARWIN_ROLE` (`<sys/resource.h>`, private): `setpriority`'s darwin-role selector.
#[cfg(target_os = "macos")]
pub(crate) const PRIO_DARWIN_ROLE: libc::c_int = 6;

/// The darwin role a spawned SHELL takes before its `execve`: `PRIO_DARWIN_ROLE_UI`, "may draw UI, focus unknown" (`TASK_DEFAULT_APPLICATION`).
#[cfg(target_os = "macos")]
pub(crate) const SHELL_ROLE: libc::c_int = 2;

/// Close a raw fd (a wake-pipe end).
pub fn close_fd(fd: i32) {
    if fd >= 0 {
        // SAFETY: closing an fd the caller owns.
        unsafe {
            libc::close(fd);
        }
    }
}

/// Focus-linked shell QoS boost: a no-op on Unix.
pub const fn set_focus_boost(_master: i32, _on: bool) {}

/// Resizes the PTY to `rows`×`cols` (`TIOCSWINSZ`) with zero pixel dimensions.
pub fn resize(master: i32, rows: u16, cols: u16) {
    resize_with_cell_px(master, rows, cols, None);
}

/// Resizes the PTY to `rows`×`cols` with pixel dimensions computed from `cell_px`.
pub fn resize_with_cell_px(master: i32, rows: u16, cols: u16, cell_px: Option<(u16, u16)>) {
    let (ws_xpixel, ws_ypixel) = winsize_pixels(rows, cols, cell_px);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    };
    // SAFETY: `master` is a valid PTY master fd and `&ws` is a valid `winsize`.
    unsafe {
        libc::ioctl(master, libc::TIOCSWINSZ, &ws);
    }
}

fn winsize_pixels(rows: u16, cols: u16, cell_px: Option<(u16, u16)>) -> (u16, u16) {
    let Some((cell_w, cell_h)) = cell_px else {
        return (0, 0);
    };
    if cell_w == 0 || cell_h == 0 {
        return (0, 0);
    }
    let x = u32::from(cols) * u32::from(cell_w);
    let y = u32::from(rows) * u32::from(cell_h);
    (
        u16::try_from(x).unwrap_or(u16::MAX),
        u16::try_from(y).unwrap_or(u16::MAX),
    )
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "unix_read_deadline_tests.rs"]
mod read_deadline_tests;

#[cfg(test)]
#[path = "unix_shell_identity_tests.rs"]
mod shell_identity_tests;
