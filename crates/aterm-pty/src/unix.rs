// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX backend of the PTY seam: the pty pair ([`open_pty_pair_cloexec`]), `fork` and
//! `login_tty`, `execve`, `read`/`write` and the tty ioctls. `lib.rs` holds the portable
//! items ([`SpawnedShell`], `build_child_env`) and re-exports this module.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{ShellHangup, ShellIdentity, SpawnedShell, build_child_env};

/// The macOS Seatbelt wrapper for [`spawn_shell_with_pid`]'s `sandbox_wrap`. Inlined to
/// keep this seam off the policy crate; it must equal
/// `aterm_containment::SANDBOX_EXEC_PATH`.
const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// [`spawn_shell_with_pid`] minus the pid, with the hardened default limits.
/// Test-only: every frontend spawns through the `_with_pid` forms.
#[cfg(test)]
// Wide on purpose: each argument is an independent, security-relevant spawn input.
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
        // The GUI's real spawn path picks the limits by containment mode.
        aterm_sandbox::Limits::shell_default(),
        // No cell metrics in tests: the winsize pixel fields stay zero.
        None,
    )
    .map(|s| s.master)
}

/// The UTF-8 locale [`resolve_spawn_locale`] injects when the inherited one is not
/// UTF-8: `en_US.UTF-8` on macOS, `C.UTF-8` elsewhere. Minimal Debian/Ubuntu, most
/// Docker images and musl do not generate `en_US.UTF-8`, and glibc would fall back to
/// ASCII `C`.
const SPAWN_UTF8_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// Whether a locale string selects UTF-8: its codeset (after the last `.`, minus any
/// `@modifier`), case-insensitive and ignoring `-`. `.UTF-8`, `.utf8` and `.UTF-8@euro`
/// qualify; `C`, `POSIX`, bare `en_US` and `.ISO8859-1` do not.
fn is_utf8_locale(loc: &str) -> bool {
    let Some(dot) = loc.rfind('.') else {
        return false;
    };
    // `dot + 1 <= loc.len()` on a char boundary, so neither fallback fires; they
    // discharge the Trust slice/overflow obligations.
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

/// The locale overrides that make the spawned child run under a UTF-8 `LC_CTYPE`.
/// aterm's parser is UTF-8-only, and a program under a non-UTF-8 `LC_CTYPE` turns
/// multibyte text into a `?` per character.
///
/// `lc_all`/`lc_ctype`/`lang` are the inherited values (`None` unset; `Some("")` set but
/// empty, which POSIX treats as unset); precedence is `LC_ALL` > `LC_CTYPE` > `LANG`.
/// Returns the pairs to append to `env_add`:
/// - nothing when the effective encoding is already UTF-8;
/// - otherwise `LC_CTYPE=`[`SPAWN_UTF8_LOCALE`], the minimal override (no territory is
///   guessed, since a missing locale falls back to `C`);
/// - plus `LC_ALL=""` when a non-empty `LC_ALL` dominates, so the other `LC_*` and
///   `LANG` still apply.
///
/// Pure, and run in the parent before the fork. The `SpawnLocale` model
/// (`aterm_models_session::spawn_locale_model`) proves the property, bound to this
/// function by `spawn_locale_conformance_child_always_utf8_ctype`.
#[must_use]
pub fn resolve_spawn_locale(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> Vec<(String, String)> {
    // POSIX: an empty value is treated as unset for category resolution.
    fn set(o: Option<&str>) -> Option<&str> {
        o.filter(|s| !s.is_empty())
    }
    // Effective encoding category under precedence LC_ALL > LC_CTYPE > LANG.
    let effective = set(lc_all).or(set(lc_ctype)).or(set(lang));
    // Already UTF-8 (incl. a UTF-8 dominating LC_ALL): change nothing.
    if effective.is_some_and(is_utf8_locale) {
        return Vec::new();
    }
    let mut overrides = vec![("LC_CTYPE".to_string(), SPAWN_UTF8_LOCALE.to_string())];
    // A set LC_ALL (necessarily non-UTF-8 here) would dominate the injected LC_CTYPE:
    // empty it so POSIX falls through.
    if set(lc_all).is_some() {
        overrides.push(("LC_ALL".to_string(), String::new()));
    }
    overrides
}

/// Size of the buffer a PTY slave's path is fetched into: `TIOCPTYGNAME`'s fixed
/// payload, and what macOS's own `openpty` passes `ptsname_r`. Both primitives report a
/// name that would not fit as an error, never a truncated path.
const PTS_NAME_LEN: usize = 128;

/// The slave device path for `master`, not via `ptsname`, which is not thread-safe
/// (aterm spawns from the GUI thread and the update-handoff worker). Darwin uses
/// `ioctl(TIOCPTYGNAME)`, which its `ptsname_r` wraps (the `libc` crate does not declare
/// `ptsname_r` for Apple targets); elsewhere `ptsname_r`.
fn pts_name(master: libc::c_int) -> io::Result<CString> {
    let mut buf = [0 as libc::c_char; PTS_NAME_LEN];
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    // SAFETY: `TIOCPTYGNAME` is an `_IOC_OUT` ioctl whose payload size is
    // exactly `PTS_NAME_LEN`; `buf` is a live, owned buffer of precisely that
    // size, and the kernel NUL-terminates the name it writes into it.
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
    // SAFETY: `buf` is a live, owned buffer and we pass its exact length;
    // `ptsname_r` either NUL-terminates within that length or fails (ERANGE).
    let rc = unsafe { libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    if rc != 0 {
        return Err(ptsname_r_error(rc));
    }
    // SAFETY: `rc == 0` means the primitive wrote a NUL-terminated path into
    // `buf`, which is live for this borrow; `to_owned` copies it out at once.
    Ok(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

/// The error a failed `ptsname_r` (nonzero `rc`) reports. It returns its error number
/// (POSIX.1-2024; glibc, musl, FreeBSD) and musl does not set `errno`; a -1 is the older
/// set-`errno` convention. Compiled into every host's tests, so its test runs everywhere.
#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
fn ptsname_r_error(rc: libc::c_int) -> io::Error {
    if rc > 0 {
        io::Error::from_raw_os_error(rc)
    } else {
        io::Error::last_os_error()
    }
}

/// The slave end of `master`, close-on-exec and not a controlling terminal from the
/// moment it exists.
///
/// On Linux, `ioctl(master, TIOCGPTPEER, flags)` (4.13+), as glibc's `openpty` does
/// first: a path open of `ptsname_r` is a name lookup in the caller's mount namespace
/// and is right only while the master's devpts instance is the one at `/dev/pts`. In a
/// container or chroot with its own `/dev`, or after the mount moves, it fails `ENOENT`
/// or opens a different terminal at the same index. The ioctl's argument is the open
/// flag word, so the descriptor is born `O_CLOEXEC` and does not become this process's
/// ctty. Any refusal falls back to the path; other platforms (Darwin has no
/// `TIOCGPTPEER`) always take the path.
///
/// # Errors
/// The OS error from the name lookup or the `open`, naming which ([`pty_step_failed`]);
/// no descriptor escapes.
fn open_pts_slave(master: libc::c_int) -> io::Result<libc::c_int> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `master` is a live `/dev/ptmx` descriptor. `TIOCGPTPEER` takes
        // its argument by value (an `open(2)` flag word, not a pointer) and
        // returns a fresh descriptor or -1, writing through no memory we own.
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
        // Any refusal (pre-4.13 kernels) falls through to the path, as glibc does.
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const NAME_STEP: &str = "ioctl(TIOCPTYGNAME)";
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    const NAME_STEP: &str = "ptsname_r";
    let name = pts_name(master).map_err(|err| pty_step_failed(NAME_STEP, err))?;
    // SAFETY: `name` is the NUL-terminated pts path the kernel just reported for
    // `master`; `open` reads it and returns a fresh fd or -1.
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(pty_step_failed(
            "open of the slave",
            io::Error::last_os_error(),
        ));
    }
    Ok(slave)
}

/// Confirm `fd` is close-on-exec, setting the flag if the open did not. A check with a
/// fallback, not the mechanism: POSIX does not require `posix_openpt` to honour
/// `O_CLOEXEC`, so the flag is read back and repaired only if absent, which reopens a
/// window of a few instructions.
fn fd_is_cloexec_or_make_it(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is a descriptor the caller just opened and still owns;
    // `F_GETFD`/`F_SETFD` only read and write the descriptor-flag word.
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

/// A step of opening a PTY pair failed: which step, and its OS error, carried with the
/// OS error's `kind` so the message names the step and the errno stays reachable
/// through `source()`.
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

/// `os`, as raised by `step` of opening a PTY pair ([`PtyOpenError`]).
fn pty_step_failed(step: &'static str, os: io::Error) -> io::Error {
    io::Error::new(os.kind(), PtyOpenError { step, os })
}

/// XNU's kernel-private "redrive open" pseudo-error `EREDRIVEOPEN` (`-6`), in no
/// userspace header.
///
/// `ptmx_clone` hands concurrent openers of `/dev/ptmx` the same free minor without
/// claiming it, so all but the first find the slot taken in `ptcopen`. `vn_open_auth`
/// redrives the open only ten times, then hands `-6` to userspace in `errno`. It is
/// contention, not a limit: 128 threads opening pairs saw it on about 1 in 130 opens.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const XNU_EREDRIVEOPEN: libc::c_int = -6;

/// How many times [`open_ptmx`] redrives an open the kernel gave up on with
/// [`XNU_EREDRIVEOPEN`]; each is a fresh open with ten internal redrives, and a contender
/// loses only to an opener that won. Measured at 256 threads: a redriven open needed at
/// most one redrive, and with redrives no open failed.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const PTMX_REDRIVES: u32 = 8;

/// The pty master, close-on-exec at the `open(2)` that creates it: step (1) of
/// [`open_pty_pair_cloexec`]. `open` is `posix_openpt`, a parameter so a test can play
/// the kernel's answers.
///
/// # Errors
/// The OS error, naming the step: at once for a real failure (out of ptys or
/// descriptors), or after [`PTMX_REDRIVES`] redrives of a lost allocation race.
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
            // EINVAL may mean the platform rejects the O_CLOEXEC bit: retry without
            // it, and `fd_is_cloexec_or_make_it` sets the flag.
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

/// Open a PTY pair in which both descriptors are close-on-exec from birth, and apply
/// `termp`/`winp` to the slave: `openpty(3)` minus its defect.
///
/// `openpty` (and so `forkpty`) opens both ends without `O_CLOEXEC`, so until the
/// parent's `close(slave)` after the fork both sit unflagged in a multi-threaded
/// process. The update-handoff worker's `Command::spawn` landing in that window hands
/// an unrelated process a writable descriptor onto another session's terminal, around
/// the WriteInput gate. A later `FD_CLOEXEC` cannot fix it: a thread that just forked
/// already holds a copy its `exec` keeps.
///
/// 1. `posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC)`: the master, flagged by the open
///    itself; a lost XNU allocation race is redriven ([`open_ptmx`]).
/// 2. `grantpt` + `unlockpt`: single ioctls on Darwin, no helper fork.
/// 3. The slave with `O_RDWR | O_NOCTTY | O_CLOEXEC` ([`open_pts_slave`]). `O_NOCTTY`
///    matters: this open runs in aterm's own process, and a session leader with no
///    controlling terminal (the launchd/Finder `.app` shape) would adopt the slave as
///    one. The child acquires its ctty later via `login_tty`'s `TIOCSCTTY`.
/// 4. `tcsetattr(slave, TCSAFLUSH, termp)` then `TIOCSWINSZ`, the calls `openpty` makes,
///    in the parent before the fork, so the child's first `tcgetattr`/`TIOCGWINSZ`
///    already sees them. Best-effort, as in `openpty`.
///
/// Both flags are read back, not assumed ([`fd_is_cloexec_or_make_it`]). The master's
/// flag stays mutable: `set_cloexec` clears it for the seamless re-exec.
///
/// # Errors
/// The OS error from whichever step failed, naming it ([`pty_step_failed`]); no
/// descriptor escapes.
fn open_pty_pair_cloexec(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
) -> io::Result<(libc::c_int, libc::c_int)> {
    // (1) master: close-on-exec at the `open(2)` that creates it.
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

    // (2) grant + unlock the slave side.
    // SAFETY: `master` is a live /dev/ptmx descriptor; both calls are ioctls on
    // it and write nothing through any pointer we own.
    let refused = unsafe {
        if libc::grantpt(master) != 0 {
            Some("grantpt")
        } else if libc::unlockpt(master) != 0 {
            Some("unlockpt")
        } else {
            None
        }
    };
    if let Some(step) = refused {
        let err = pty_step_failed(step, io::Error::last_os_error());
        // SAFETY: closing the master we opened above.
        unsafe {
            libc::close(master);
        }
        return Err(err);
    }

    // (3) slave: close-on-exec at birth, and not this process's ctty.
    let slave = match open_pts_slave(master) {
        Ok(s) => s,
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

    // (4) termios and winsize on the slave, before any fork; best-effort, as `openpty`.
    if let Some(t) = termp {
        // SAFETY: `t` is a live, fully-initialized termios and `slave` is a
        // freshly-opened pts; `tcsetattr` only reads through the pointer.
        unsafe {
            libc::tcsetattr(slave, libc::TCSAFLUSH, ptr::from_ref(t));
        }
    }
    if let Some(w) = winp {
        // SAFETY: `w` is a live, fully-initialized winsize; `TIOCSWINSZ` reads
        // exactly that struct through the pointer.
        unsafe {
            libc::ioctl(slave, libc::TIOCSWINSZ, ptr::from_ref(w));
        }
    }
    Ok((master, slave))
}

/// A bare PTY pair `(master, slave)`, both ends close-on-exec from birth
/// ([`open_pty_pair_cloexec`] with no termios or winsize), for a caller that needs a pair
/// and no child.
///
/// # Errors
/// The OS error from whichever step failed; no descriptor escapes on error.
pub fn open_pty_pair() -> io::Result<(i32, i32)> {
    open_pty_pair_cloexec(None, None)
}

/// The termios a NULL-termios `openpty` gives the slave (the kernel's defaults), probed
/// once per process on a throwaway close-on-exec pair and cached, so the spawn's
/// termios differs from the NULL path only by its stated deltas. `None` when the probe
/// fails.
fn kernel_default_termios() -> Option<libc::termios> {
    static DEFAULTS: std::sync::OnceLock<Option<libc::termios>> = std::sync::OnceLock::new();
    *DEFAULTS.get_or_init(|| {
        let (m, s) = open_pty_pair_cloexec(None, None).ok()?;
        // SAFETY: `s`/`m` are the live pair just opened and owned here;
        // `tcgetattr` only fills the zeroed out-param, and both are closed
        // before this returns on every path.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            let ok = libc::tcgetattr(s, &mut t) == 0;
            libc::close(s);
            libc::close(m);
            ok.then_some(t)
        }
    })
}

/// The slave termios: the probed kernel defaults with exactly two deltas.
///
///  * `IUTF8` on, so canonical-mode ERASE deletes a whole UTF-8 character, agreeing with
///    the UTF-8 locale forced on the child (`SPAWN_UTF8_LOCALE`).
///  * Speed `B230400` instead of `B9600`: xnu's `ttsetwater` sizes the output-queue
///    high-water mark from ospeed. Measured neutral on macOS (reads stay 1024 B), kept
///    because it is free and the clamp may differ elsewhere.
///
/// No env knob reverts either: a release would then run a termios no test covers.
/// (Clearing `OPOST` gains ~6% raw throughput but stairsteps bare `\n`.)
fn build_spawn_termios(mut t: libc::termios) -> libc::termios {
    t.c_iflag |= libc::IUTF8;
    // SAFETY: `cfsetspeed` only writes the speed fields (and, on Linux, CBAUD
    // bits) of the valid, stack-owned `t`.
    unsafe {
        libc::cfsetspeed(&mut t, libc::B230400);
    }
    t
}

/// The termios applied to the spawn's slave; `None` (kernel defaults) only when the
/// probe fails. Runs in the parent before the fork.
fn spawn_termios() -> Option<libc::termios> {
    kernel_default_termios().map(build_spawn_termios)
}

// The exec-status channel: `spawn_shell_with_pid` learns from its child whether it
// reached `execve`. The child writes one byte and `_exit`s on failure (b'S' sandbox,
// b'E' exec); a successful `execve` closes the close-on-exec write end and the parent
// reads EOF.
//
// Both ends must be close-on-exec from birth. `pipe(2)` + `fcntl` leaves a window in
// which another thread's `Command::spawn` (`posix_spawn`, which no lock or
// `pthread_atfork` handler covers) inherits the write end. A stranger holding it delays
// EOF for its whole life (the update handoff's successor aterm lives for the login
// session), and one that writes to it makes a healthy spawn read as failed. A failure
// byte is unaffected: it is in the buffer whoever holds a write end.
//
// Darwin has no `pipe2`, `dup3`, `accept4` or `SOCK_CLOEXEC`; only `open(2)` creates a
// flagged descriptor, so the carrier needs a pathname: a FIFO. Rejected carriers: a
// PTY pair (the tty output queue is flushed at last-slave close, so a failure byte is
// lost when the child exits before the parent reads: a fail-open); a pool of pipes
// (single-use, and a retained template write end suppresses EOF); a spawn lock (cannot
// cover `posix_spawn`).

/// How long the parent waits for the child's exec-status verdict before failing closed:
/// a leaked write end, the racy carrier, or a child stopped between fork and exec is a
/// slow, visible failure rather than a freeze. Fork to EOF measures p99 6 ms and at most
/// 523 ms under extreme load; ten seconds leaves room for an exec target on a stalled
/// network filesystem.
const EXEC_STATUS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// The longest single sleep [`wait_for_exec_status`] takes inside its budget. The
/// verdict comes from the non-blocking `read`; this caps each readiness wait, so a
/// primitive that never fires costs latency, never a hang. On macOS `poll` and `kqueue`
/// never report EOF on a FIFO whose last writer closed (`select` does), though all
/// three work on a pipe; the Darwin-only
/// `wait_readable_briefly_wakes_when_a_fifos_last_writer_closes` pins
/// [`wait_readable_briefly`]'s wake on a fifo.
const EXEC_STATUS_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

/// Which mechanism produced the exec-status channel, so a fallback to the racy carrier
/// is observable. The atomic variants are `cfg`-gated to their platform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecStatusCarrier {
    /// Linux `pipe2(fds, O_CLOEXEC)`: both ends flagged by the call that creates them.
    #[cfg(target_os = "linux")]
    Pipe2,
    /// Darwin: a FIFO opened twice with `O_CLOEXEC`, then unlinked.
    #[cfg(target_os = "macos")]
    Fifo,
    /// `pipe(2)` + two `fcntl(F_SETFD)`: racy, the last resort for a platform with
    /// neither of the above or a failed atomic route (a read-only or full temp
    /// filesystem, a sandbox denying `mkfifo`). Counted and announced.
    RacyPipe,
}

/// How many times this process fell back to [`ExecStatusCarrier::RacyPipe`]: refusing
/// to spawn would be worse, and falling back silently would hide the defect, so it
/// falls back, bounded, and says so.
static RACY_EXEC_STATUS_CARRIERS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Record — and, the first time only, announce — a fallback to the racy carrier.
fn note_racy_exec_status_carrier(why: &io::Error) {
    use std::sync::atomic::Ordering;
    let prior = RACY_EXEC_STATUS_CARRIERS.fetch_add(1, Ordering::Relaxed);
    if prior == 0 {
        // `writeln!`, not `eprintln!`, which panics when stderr is gone.
        {
            use std::io::Write as _;
            // Name the folder (a missing or read-only $TMPDIR is fixable), unless the
            // refusal was the descriptor limit.
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

/// Confirm `fd` is close-on-exec, without [`fd_is_cloexec_or_make_it`]'s repair:
/// `pipe2`/`open(2)` must honour `O_CLOEXEC`, so a missing flag means the atomic
/// property does not hold and the announced fallback is the honest answer.
fn fd_is_cloexec(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is a descriptor the caller just opened and still owns;
    // `F_GETFD` only reads the descriptor-flag word.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags != -1 && flags & libc::FD_CLOEXEC != 0
}

/// Put `fd` in non-blocking mode: the read end of every carrier, since
/// [`wait_for_exec_status`] reads non-blocking. The description is the parent's own and
/// not yet shared; the child's write end stays blocking.
fn fd_set_nonblocking(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is a descriptor the caller just opened and still owns;
    // `F_GETFL`/`F_SETFL` only read and write the file-status flag word.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags != -1 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    }
}

/// The directory the per-spawn status FIFO is created in, resolved once: `$TMPDIR` via
/// `getenv`, else `/tmp` ([`fifo_dir_in`]).
///
/// Not `confstr(_CS_DARWIN_USER_TEMP_DIR)`, nor `std::env::temp_dir()` (which falls
/// back to it with `$TMPDIR` unset): it goes through libsystem_notify's lazily built
/// `os_alloc_once` gate, which `fork`'s `notify_fork_child` handler also takes. A fork
/// while another thread is inside that gate kills the child inside `fork()` itself
/// ("os_once_t is corrupt"). The spawn passes that gate before forking
/// (`aterm_uds::spawnfd::InheritStrip::prepare`), but a fork elsewhere need not have,
/// so this lookup must not be anyone's first libnotify call.
///
/// Trusting `$TMPDIR` gains an attacker nothing: [`open_exec_status_fifo`] creates the
/// node with `mkfifo` (which fails `EEXIST` on an existing one), names it
/// unpredictably, opens both ends `O_NOFOLLOW` and verifies them
/// ([`fifo_ends_are_one_private_object`]). A hostile directory degrades to the
/// announced racy carrier, and whoever sets aterm's `TMPDIR` controls its environment
/// anyway.
#[cfg(target_os = "macos")]
fn exec_status_fifo_dir() -> &'static [u8] {
    static DIR: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| fifo_dir_in(std::env::var_os("TMPDIR")))
}

/// The status FIFO's directory for a `$TMPDIR` of `tmpdir`, ending in one `/`: that
/// directory, or `/tmp` when unset ([`exec_status_fifo_dir`] says why not the OS).
#[cfg(target_os = "macos")]
fn fifo_dir_in(tmpdir: Option<std::ffi::OsString>) -> Vec<u8> {
    let mut dir = tmpdir.map_or_else(|| b"/tmp".to_vec(), |d| d.as_bytes().to_vec());
    if dir.last() != Some(&b'/') {
        dir.push(b'/');
    }
    dir
}

/// A fresh, unique, unguessable path for one spawn's FIFO. Unique per spawn is
/// mandatory: two pairs opened on one fifo path share one kernel object, so concurrent
/// opens would read each other's verdicts. The counter guarantees uniqueness in this
/// process; `getentropy` (else the clock) makes the name unguessable.
#[cfg(target_os = "macos")]
fn exec_status_fifo_path() -> io::Result<CString> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let mut path = exec_status_fifo_dir().to_vec();
    let mut rnd = [0u8; 8];
    // SAFETY: `getentropy` fills exactly the 8-byte buffer it is given (its
    // documented limit is 256 bytes) and returns 0 on success.
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

/// Both fifo descriptors name one object, and it is our private fifo: the TOCTOU check
/// for the gap between the two `open`s (same `st_dev`/`st_ino`, a fifo, our euid, mode
/// 0600). `O_NOFOLLOW` on both opens already refuses a final-component symlink.
#[cfg(target_os = "macos")]
fn fifo_ends_are_one_private_object(rd: libc::c_int, wr: libc::c_int) -> bool {
    // SAFETY: both are live descriptors this function just opened; `fstat`
    // writes exactly one `stat` through each pointer, and `geteuid` takes none.
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

/// Build the Darwin carrier: a FIFO whose two ends are close-on-exec from birth. The
/// order matters:
///
///  1. `mkfifo(path, 0600)`: from here every exit path must `unlink` it.
///  2. `chmod(path, 0600)`: `mkfifo`'s mode is masked by the umask, which could leave a
///     fifo we cannot open. Best-effort.
///  3. Read end first, `O_NONBLOCK`: a blocking read-open waits for a writer, and a
///     write-open waits for a reader.
///  4. Write end second, blocking: the reader is open, so it returns at once.
///  5. `unlink` after both opens: the object outlives the name, so the channel is
///     anonymous.
///
/// It costs ~0.3 ms on a quiet volume and tens of ms on a busy one (`mkfifo`/`unlink`
/// are metadata operations), still far inside [`EXEC_STATUS_BUDGET`]. For the ~100 µs
/// the fifo has a name, a same-uid process that guessed the random path could open a
/// write end and delay EOF; the budget bounds that too.
///
/// # Errors
/// The OS error from whichever step failed; no descriptor or name escapes.
#[cfg(target_os = "macos")]
fn open_exec_status_fifo() -> io::Result<(libc::c_int, libc::c_int)> {
    // A few retries make a name collision non-fatal.
    let mut last = io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exec-status fifo path collision",
    );
    for _attempt in 0..4 {
        let path = exec_status_fifo_path()?;
        // (1) create the name.
        // SAFETY: `path` is a NUL-terminated path; `mkfifo` reads it and creates a
        // fifo, or returns -1.
        if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
            last = io::Error::last_os_error();
            if last.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(last);
        }
        // (2) undo any umask masking (best-effort).
        // SAFETY: `path` is the fifo we just created; `chmod` only sets its mode.
        unsafe {
            libc::chmod(path.as_ptr(), 0o600);
        }
        // (3) read end, non-blocking so it does not wait for a writer; `O_NOFOLLOW`
        //     refuses a symlink at the final component.
        // SAFETY: `path` is the fifo we just created; `open` reads the path and
        // returns a fresh descriptor or -1.
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
        // (4) write end, blocking; it returns at once because the read end is open.
        // SAFETY: as (3).
        let wr = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if wr < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: closing the read end we opened in (3) and removing the
            // name we created in (1). Nothing else was created.
            unsafe {
                libc::close(rd);
                libc::unlink(path.as_ptr());
            }
            return Err(err);
        }
        // (5) unlink: the channel is now anonymous.
        // SAFETY: removing the name we created above; both fds stay valid.
        unsafe {
            libc::unlink(path.as_ptr());
        }
        // Verify: one private fifo, both ends close-on-exec.
        if !fifo_ends_are_one_private_object(rd, wr) || !fd_is_cloexec(rd) || !fd_is_cloexec(wr) {
            // SAFETY: closing the two descriptors opened in (3) and (4); the
            // name is already gone.
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

/// Create the exec-status channel, `(read_end, write_end, carrier)`, with both ends
/// close-on-exec and the read end non-blocking. On the atomic carriers (`Pipe2` on
/// Linux, `Fifo` on Darwin) no instant exists at which either end lacked the flag;
/// a later `fcntl` cannot give that, since a thread that just forked already holds an
/// exec-surviving copy.
///
/// # Errors
/// Only if even the racy fallback cannot be created; no descriptor escapes.
fn open_exec_status_channel() -> io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> {
    // The atomic route for this platform; both arms read the flag back.
    #[cfg(target_os = "linux")]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid 2-element buffer; `pipe2` fills exactly it and
        // applies the flag word to both descriptors as it creates them.
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
    // Any other Unix: no atomic route, so take the announced fallback.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no atomic close-on-exec exec-status carrier is implemented for this platform",
    ));

    let (rd, wr, carrier) = match atomic {
        Ok(chan) => chan,
        Err(why) => {
            // The racy fallback: `pipe(2)` returns both ends unflagged until the
            // `fcntl`s land. Better than refusing to spawn; counted, announced, and
            // bounded by `EXEC_STATUS_BUDGET`.
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` is a valid 2-element buffer that `pipe` fills.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (rd, wr) = (fds[0], fds[1]);
            // Without FD_CLOEXEC the write end would survive into the shell and EOF
            // would never come: a hard error.
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
            // Announced only once it exists, so "shells still start" never precedes a
            // spawn that fails anyway.
            note_racy_exec_status_carrier(&why);
            (rd, wr, ExecStatusCarrier::RacyPipe)
        }
    };

    // The verdict comes from a non-blocking `read` (`wait_for_exec_status`), so the read
    // end must be non-blocking on every carrier (the fifo's already is).
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

/// The child's verdict, as the parent was able to establish it.
#[derive(Debug)]
enum ExecStatus {
    /// EOF: the kernel closed the child's close-on-exec write end at `execve`.
    Execed,
    /// The child wrote a reason byte and `_exit`ed before exec.
    FailedBeforeExec(u8),
    /// [`EXEC_STATUS_BUDGET`] elapsed with neither a byte nor EOF.
    NoVerdict,
    /// The channel itself broke (not `EINTR`, not `EAGAIN`).
    ChannelBroke(io::Error),
}

/// Read a verdict from the status channel if one is available now. `None` means "no
/// verdict yet" and nothing else; `EINTR` retries. Any other error is
/// [`ExecStatus::ChannelBroke`], never a success.
fn read_exec_status_now(rd: libc::c_int) -> Option<ExecStatus> {
    let mut byte = [0u8; 1];
    loop {
        // SAFETY: `rd` is the parent's live read end; `byte` is a 1-byte buffer
        // and `read` writes at most the 1 byte it is asked for.
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
            // EAGAIN: writers exist and none has spoken; the only "not yet".
            io::ErrorKind::WouldBlock => None,
            _ => Some(ExecStatus::ChannelBroke(err)),
        };
    }
}

/// Sleep until `fd` looks readable or `slice` elapses. Only an optimisation: the caller
/// always re-reads. `select(2)` because it is the one primitive that reports EOF on a
/// Darwin FIFO whose last writer closed (see [`EXEC_STATUS_SLICE`]). At or above
/// `FD_SETSIZE`, where `FD_SET` would write past the bitmap, it is a plain sleep.
fn wait_readable_briefly(fd: libc::c_int, slice: std::time::Duration) {
    if fd >= 0 && (fd as usize) < libc::FD_SETSIZE {
        // SAFETY: `set` is a zeroed `fd_set` and `fd` is verified in range for
        // its bitmap, so `FD_SET` writes within it; `select` reads and writes
        // only `set` and `tv`. A -1 return (EINTR, or anything else) needs no
        // handling — the caller re-reads the descriptor either way.
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
    // SAFETY: `ts` is a fully-initialized timespec; `nanosleep` reads it and
    // writes nothing through the null remainder pointer.
    unsafe {
        let ts = libc::timespec {
            tv_sec: slice.as_secs() as libc::time_t,
            tv_nsec: libc::c_long::from(slice.subsec_nanos() as i32),
        };
        libc::nanosleep(&ts, ptr::null_mut());
    }
}

/// Wait for the child's exec-status verdict, for at most `budget`.
///
/// The verdict comes only from the non-blocking `read` at the top of each iteration;
/// [`wait_readable_briefly`] just keeps the loop from spinning. The monotonic deadline
/// is recomputed every iteration, so signals cannot extend it. On expiry one final read
/// runs before [`ExecStatus::NoVerdict`]: a failure byte sits in the buffer whoever else
/// holds a write end, so a byte is always the true answer; only EOF can be suppressed.
fn wait_for_exec_status(rd: libc::c_int, budget: std::time::Duration) -> ExecStatus {
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

/// Spawn the shell in a fresh PTY of `rows`×`cols`, returning the master fd and the
/// child pid (see [`SpawnedShell`]). Requires a `Cap<Spawn>` of at least `Trusted` tier.
/// The shell is `$SHELL`; with it unset or empty, the account's login shell from the
/// password database, else `/bin/sh`, and the child is told that as `SHELL`
/// ([`crate::select_shell_with_origin`]).
///
/// `env_add` entries are injected into the child's environment and `argv_override`
/// replaces the shell's argv (bash's `--rcfile`); this seam knows nothing of shell
/// integration. `exec_command` (`-e prog args…`) runs a command directly instead of a
/// shell, taking precedence over `argv_override`: `prog` is PATH-resolved in the parent,
/// and its arguments reach `execve` as their exact bytes (non-UTF-8 included).
///
/// No argv element is dropped: one holding a NUL refuses the spawn with `InvalidInput`
/// ([`argv_cstrings`]), as does a `shell_override` holding one when the shell would run.
///
/// `cwd` is the directory the child `chdir`s into (`--working-directory`), overriding
/// the `/`→`$HOME` fallback; a failed `chdir` is non-fatal.
///
/// `sandbox_wrap = Some(sbpl)` runs the program under `/usr/bin/sandbox-exec -p <sbpl>`
/// (macOS Seatbelt), built in the parent, and fails closed if the wrapper is missing.
///
/// The child applies `limits` before `execve`; if that fails it writes a byte on the
/// close-on-exec status channel and exits, so no unconfined shell is ever returned. EOF
/// on the channel means it exec'd.
///
/// The child's winsize pixel fields start zero; a caller that knows the cell size uses
/// [`spawn_shell_with_pid_cell_px`].
///
/// # Errors
/// `PermissionDenied` for a too-low tier, `InvalidInput` for a NUL in an argv element,
/// the OS error if the PTY pair, channel or `fork` fails, or `PermissionDenied`/`Other`
/// if the child failed to confine itself or to `execve`. On any pre-exec failure the
/// master is closed.
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
    )
}

/// [`spawn_shell_with_pid`], filling the winsize pixel fields from the host's cell
/// metrics.
///
/// The child branch implements `ForkExec`'s walk (`aterm_models_session::fork_exec_model`): `Fork → Setrlimit → Chdir →
/// CloseMaster → Exec` (`login_tty` is part of `Fork`), and the parent pre-builds
/// `envp`/argv before the fork (`envPrebuilt`). There is no trace conformance: the
/// child `execve`s or `_exit`s and cannot be observed in-process, so the behaviour is
/// covered by `ty check` of the model and the fork/exec unit tests. `UnsafeEnvOp`
/// exists only in the model's `Buggy` branch.
///
/// The model has no slave fd, `O_CLOEXEC` or concurrent actor, so the close-on-exec-from-birth
/// property that [`open_pty_pair_cloexec`] provides (no unrelated process inherits either
/// descriptor) is guarded by reading the flag back before the fork and by the
/// `an_unrelated_exec_inherits_no_end_of_a_live_pty_or_status_channel` regression test.
// Projection onto `<<pc, masterClosed, unsafeOpRan, envPrebuilt>>`:
// `child_spawn::project_pc` is a name only (Trust requires it, never runs it), since
// the post-fork child cannot be driven in-process.
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
// Skip: the extractor cannot model field-address `&raw` (`UnsupportedReason::
// AddressOfField`): `addr_of_mut!(sa.sa_mask)`, `addr_of!(sa)` and `addr_of!(b)` in the
// child branch. Without the skip the body reads INCONCLUSIVE, which strict verification
// refuses; the fork..exec window is covered by the `ForkExec` anchors above. Drop
// it once Trust extracts those shapes; re-check with `targo trust check -p aterm-pty
// --lib --allow-l0-gaps` after each `aterm pkg update trust`.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid_cell_px(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    // Config `shell` / `--shell`: the program to run instead of `$SHELL`. An
    // absolute path, or a bare name looked up on `PATH` here in the parent
    // ([`crate::select_shell_with_origin`], the one resolution). `None` → `$SHELL`,
    // else the account's login shell (the default).
    shell_override: Option<&str>,
    // Config `shell_args`: extra argv after argv[0]. When set, the shell runs as
    // a NON-login interactive shell with exactly these args (a login `-basename`
    // argv[0] and explicit `-l` would conflict). `None` → the default argv
    // ([`default_shell_argv0`]).
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    // The rlimits applied in the child before exec, chosen by containment mode:
    // [`aterm_sandbox::Limits::shell_default`] for Safety,
    // [`aterm_sandbox::Limits::contained`] for Containment, inherited for User.
    limits: aterm_sandbox::Limits,
    // The host's cell size in device pixels, `(width, height)`, for the winsize pixel
    // fields of the child's first `TIOCGWINSZ` ([`resize_with_cell_px`]): an image tool
    // run as the first command (`aterm -e chafa …`) reads them before any resize.
    // `None` keeps them zero.
    cell_px: Option<(u16, u16)>,
) -> io::Result<SpawnedShell> {
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

    // Everything that allocates or reads the environment happens here, before the fork.
    // The frontend is multi-threaded, and only async-signal-safe calls are legal between
    // fork and exec: a lock held by a vanished thread would deadlock or kill the child.
    // An exec failure names where the shell came from, so the person knows which
    // setting to fix. One resolution serves every caller that names or detects the shell
    // ([`crate::select_shell_with_origin`]): the override, else `$SHELL`, else the
    // account's login shell, a bare name looked up on `PATH`.
    let (shell, origin) = crate::select_shell_with_origin(shell_override.map(std::ffi::OsStr::new));
    let shell_from = origin.suffix();
    // The shell's path and names built from it, as C strings, built only on the branches
    // that run the shell, so a bad configured shell does not refuse an unrelated `-e`.
    // `$SHELL` cannot hold a NUL, so only `--shell` / config `shell` can be refused.
    let shell_cstring = |name: &std::ffi::OsStr| {
        CString::new(name.as_bytes())
            .map_err(|_| crate::program_nul_refusal(&format!("the shell{shell_from}")))
    };

    // `env_store` owns the C strings `envp` points into. With no `$SHELL` of its own to
    // inherit, the child is told the account's login shell ([`crate::shell_env_for_child`])
    // first, so a caller's own `SHELL` in `env_add` (a substitute it started) wins.
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

    // Exec target and argv: `-e prog args…` (`exec_command`) runs directly and wins.
    // Otherwise the shell runs with an explicit `argv_override` (bash `--rcfile …`), else
    // config `shell_args`, else the platform's interactive argv ([`default_shell_argv0`]).
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
            // A non-login interactive shell: argv[0] is the basename (explicit args own
            // the login choice), then the args verbatim.
            let cshell = shell_cstring(shell.as_os_str())?;
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            // The basename is a part of the shell, which holds no NUL by now.
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

    // OS-sandbox wrap (macOS Seatbelt): `Some(sbpl)` wraps the resolved program and argv
    // in `sandbox-exec -p <sbpl>`, failing closed here, before the fork, if the wrapper
    // is absent (`build_sandbox_wrap`). A bare shell loses its login-dash argv[0], so a
    // Containment shell is a non-login interactive shell.
    let (exec_target, argv_store): (CString, Vec<CString>) = if let Some(sbpl) = sandbox_wrap {
        build_sandbox_wrap(SANDBOX_EXEC_PATH, sbpl, &exec_target, &argv_store)?
    } else {
        (exec_target, argv_store)
    };

    let mut argv: Vec<*const libc::c_char> = argv_store.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());

    // chdir target: `--working-directory` (`cwd`) wins; else a launch from `/` (Finder,
    // launchd) starts in $HOME.
    let chdir_c: Option<CString> = if let Some(dir) = cwd {
        CString::new(dir.as_bytes()).ok()
    } else if std::env::current_dir().ok().as_deref() == Some(std::path::Path::new("/")) {
        std::env::var_os("HOME").and_then(|h| CString::new(h.as_bytes()).ok())
    } else {
        None
    };

    // Exec-status channel, both ends close-on-exec from birth (see
    // `open_exec_status_channel`): a successful `execve` closes the child's write end
    // and the parent reads EOF; a pre-exec failure (sandbox or `execve`) writes one
    // reason byte, and the parent returns an error instead of the master.
    let (status_rd, status_wr, _status_carrier) = open_exec_status_channel()?;
    // The child's inheritance strip (step 0c), prepared in the parent: its last-resort
    // bound is the kernel's per-process maximum, which no thread can raise before the
    // fork.
    let inherit_strip = aterm_uds::spawnfd::InheritStrip::prepare();

    // The child's first winsize, pixel fields included, for a tool that reads
    // TIOCGWINSZ before any resize.
    let (ws_xpixel, ws_ypixel) = winsize_pixels(rows, cols, cell_px);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    };
    // Kernel defaults + IUTF8 + B230400 (`build_spawn_termios`), applied at slave-open
    // time so no post-fork `tcsetattr` races the shell's own termios reads.
    let termp = spawn_termios();
    // The PTY pair, both ends close-on-exec from birth, with termios and winsize applied
    // to the slave before the fork (see `open_pty_pair_cloexec`).
    let (master, slave) = match open_pty_pair_cloexec(termp.as_ref(), Some(&ws)) {
        Ok(pair) => pair,
        Err(err) => {
            // SAFETY: closing the two pipe fds we just opened; no pty exists —
            // `open_pty_pair_cloexec` fails closed and leaks no descriptor.
            unsafe {
                libc::close(status_rd);
                libc::close(status_wr);
            }
            return Err(err);
        }
    };
    // SAFETY: `fork` takes no arguments and returns the child pid in the parent,
    // 0 in the child, or -1 on failure.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = io::Error::last_os_error();
        // SAFETY: closing the pty pair and the two pipe fds (fork failed, so
        // there is no child holding a copy of any of them).
        unsafe {
            libc::close(slave);
            libc::close(master);
            libc::close(status_rd);
            libc::close(status_wr);
        }
        return Err(err);
    }
    if pid == 0 {
        // Child: async-signal-safe only. Nothing here allocates, locks, or reads env.
        // (0) the read end is the parent's; drop it in the child so only the
        //     write end (closed by exec on success) carries the status.
        // SAFETY: `status_rd` is the inherited read-end fd; `close` is a-s-safe.
        unsafe {
            libc::close(status_rd);
        }
        // (0a) acquire the controlling terminal. On macOS `login_tty` is `setsid()`,
        //     `ioctl(fd, TIOCSCTTY, NULL)`, `dup2` onto 0/1/2 and `close(fd)` for
        //     fd >= 3, all async-signal-safe. It makes the child a session leader with
        //     pid == sid == pgid, which `hangup`, `record_adopted_shell` and the GUI's
        //     `tcgetpgrp(master)` depend on. The `dup2`s clear FD_CLOEXEC on the copies,
        //     so stdio survives `execve` (pinned by
        //     `a_spawned_child_gets_its_own_ctty_our_winsize_and_no_other_descriptor`).
        //
        //     Never close the slave afterwards: `login_tty` already did, and a second
        //     close would hit whatever descriptor reuses the number. On failure, do what
        //     `forkpty` does: `dup2` the slave over 0/1/2 by hand and close it; the shell
        //     runs with no controlling terminal.
        // SAFETY: `slave` is this child's inherited pty slave fd; `login_tty`,
        // `dup2` and `close` are all async-signal-safe. Nothing allocates, takes
        // a lock, or reads the environment.
        unsafe {
            if libc::login_tty(slave) == -1 {
                libc::dup2(slave, libc::STDIN_FILENO);
                libc::dup2(slave, libc::STDOUT_FILENO);
                libc::dup2(slave, libc::STDERR_FILENO);
                if slave > libc::STDERR_FILENO {
                    libc::close(slave);
                }
            }
        }
        // (0b) normalize signals. `execve` resets caught signals, but ignored
        //     dispositions and the mask survive: std sets SIGPIPE=SIG_IGN and aterm-gui
        //     blocks SIGUSR1, which every shell and descendant would inherit (`cmd |
        //     head` printing "Broken pipe"). SIG_DFL also repairs an inherited
        //     SIGCHLD=SIG_IGN. SIGKILL/SIGSTOP cannot be reset.
        // SAFETY: sigaction / sigemptyset / sigprocmask are async-signal-safe; the
        // sigaction/sigset values are stack-local and fully initialized by zeroed() +
        // sigemptyset before use; no allocation, no locks.
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
        // (0c) nothing else is inherited. `fork` copied every descriptor, and those
        //     opened without close-on-exec (the GUI's frameworks: Metal's shader-cache
        //     files, read-write) would survive into the shell and everything it runs.
        //     `InheritStrip::apply` marks every descriptor above stdio close-on-exec,
        //     found by listing this child's table (`PROC_PIDLISTFDS` on macOS,
        //     `close_range(CLOSE_RANGE_CLOEXEC)` or `/proc/self/fd` on Linux) and
        //     probing up to the per-process maximum only when a listing fails or fills.
        //     Marking, not closing, keeps `status_wr` usable until `execve`. It runs
        //     before the `setrlimit` (1), which could leave Linux's `/proc/self/fd` no
        //     free number. `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT` cannot
        //     replace this child: `setrlimit` and `TIOCSCTTY` are not file actions,
        //     and a file action opening the slave does not stand in for `TIOCSCTTY`.
        //     On Darwin the kernel runs file actions before `POSIX_SPAWN_SETSID`, so
        //     the child leads a session with no controlling terminal (`tcgetsid`
        //     fails `ENOTTY`, Ctrl-C never reaches the foreground job). A `/bin/sh`
        //     (bash) child hides this by reopening the slave itself; zsh does not.
        //     `apply` is async-signal-safe.
        inherit_strip.apply();
        // (1) confine resource use, failing closed: if the limits cannot be installed,
        //     tell the parent and exit before exec. With a valid cap `apply` does not
        //     allocate.
        if limits.apply(sandbox_cap).is_err() {
            // SAFETY: `write`/`_exit` are async-signal-safe; b'S' tells the parent the
            // sandbox failed.
            unsafe {
                let b: u8 = b'S';
                libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                libc::_exit(126);
            }
        }
        // (2) chdir (`--working-directory`, or $HOME when started from `/`).
        if let Some(dir) = &chdir_c {
            // SAFETY: `dir` is a valid NUL-terminated path; `chdir` is async-signal-safe.
            unsafe {
                libc::chdir(dir.as_ptr());
            }
        }
        // (3) close the inherited master (the spec's `CloseMaster`) so it leaks into no
        //     process the shell spawns. Guarded on `master > 2`: had it landed on
        //     0/1/2, step (0a)'s `dup2` already replaced it with the slave.
        // SAFETY: `master` is this child's inherited master fd; `close` is
        // async-signal-safe.
        unsafe {
            if master > libc::STDERR_FILENO {
                libc::close(master);
            }
        }
        // (4) exec. `execve` takes the pre-built `envp` and an absolute target (no PATH
        //     search). On success the kernel closes `status_wr` and the parent reads
        //     EOF; on failure the child writes b'E' and exits before any shell runs.
        // SAFETY: exec_target/argv/envp are null-terminated arrays of live C
        // strings that outlive the call; `write`/`_exit` are async-signal-safe.
        unsafe {
            libc::execve(exec_target.as_ptr(), argv.as_ptr(), envp.as_ptr());
            let b: u8 = b'E';
            libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
            libc::_exit(127);
        }
    }
    // Parent. Drop our copy of the slave now: left open, it keeps the pty open, so the
    // master never reports hangup after the shell dies and the seamless update's
    // `handoff_masters_closed` waits forever. From here the parent owns only the master
    // and the status pipe.
    // SAFETY: `slave` is the parent's own copy of the pty slave fd (pid > 0
    // here, so the child has its own); `close` releases just this copy.
    unsafe {
        libc::close(slave);
    }
    // No post-fork `fcntl(master, F_SETFD, FD_CLOEXEC)`: the master is close-on-exec
    // from the `open(2)` that created it, verified before the fork, so it cannot leak
    // into a later session's shell (an ungated channel around the WriteInput gate).
    // The real-spawn test pins it; `set_cloexec` clears it for the seamless re-exec.
    //
    // Close our write end so the read sees EOF once the child's is gone: EOF is
    // success, any byte a failure before exec.
    // SAFETY: `status_wr` is the parent's copy of the write end.
    unsafe {
        libc::close(status_wr);
    }
    let status = wait_for_exec_status(status_rd, EXEC_STATUS_BUDGET);
    // SAFETY: done with the read end, on every branch below.
    unsafe {
        libc::close(status_rd);
    }
    let failure: Option<(io::ErrorKind, String)> = match status {
        // EOF: the kernel closed the child's close-on-exec write end at the image
        // switch. The only success verdict.
        ExecStatus::Execed => None,
        ExecStatus::FailedBeforeExec(b'S') => Some((
            io::ErrorKind::PermissionDenied,
            "sandbox confinement failed in child (fail-closed: shell not exec'd, _exit(126))"
                .to_owned(),
        )),
        ExecStatus::FailedBeforeExec(_) => Some((
            io::ErrorKind::Other,
            // Name the target and what is wrong with it (usually a config typo: the
            // target is used verbatim). The child can send only one byte; the parent
            // holds the exact path `execve` read. Under the sandbox the target is the
            // wrapper, so no shell source is named.
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
        // The budget elapsed with neither a byte nor EOF: fail closed. With an atomic
        // carrier no stranger holds a write end, so a child that exec'd has already
        // produced EOF and a written byte would have been read: expiry means the child
        // has not exec'd (stopped, or the machine wedged), and handing back a master
        // would be an unconfined shell. On the racy carrier the verdict is ambiguous,
        // and a session that refuses to open with a clear error is still the safe side.
        ExecStatus::NoVerdict => Some((
            io::ErrorKind::TimedOut,
            format!(
                "`{}` did not start within {} s",
                exec_target.to_string_lossy(),
                EXEC_STATUS_BUDGET.as_secs()
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
        // No confirmed, confined shell, so nothing usable escapes. SIGKILL first (no
        // grace needed), then close the master; `reap` is the bounded collector, since
        // a blocking `waitpid` would reintroduce the unbounded wait this path ends.
        // SAFETY: `pid` is this call's own child and `master` the parent's pty
        // master fd; `kill` posts a signal and `close` releases one descriptor.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::close(master);
        }
        reap(pid);
        return Err(io::Error::new(kind, what));
    }
    Ok(SpawnedShell { master, pid })
}

/// Hang up a spawned shell: `SIGHUP` to its process group (`pid` is the session leader
/// and pgid). The slave then closes, so a reader blocked in `read(master)` gets EOF.
/// The non-blocking half of teardown: it never takes the tty lock as `close(master)`
/// does, so the UI thread runs it and closes the master later. A no-op for `pid <= 1`;
/// an exited child makes `killpg` fail harmlessly.
///
/// Unverified: it signals whatever `pid` names now, so only a caller that knows `pid`
/// is its own unreaped child may use it. Session teardown uses [`hangup_shell`].
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    // SAFETY: `killpg` only posts SIGHUP to the group, the child's own job tree; an
    // error (an already-reaped child) is ignored.
    unsafe {
        libc::killpg(pid, libc::SIGHUP);
    }
}

/// The kernel's start time for `pid`, an opaque number that differs between two
/// processes that held the same pid: what a [`ShellIdentity`] records and re-reads.
///
/// macOS: `proc_pidinfo(PROC_PIDTBSDINFO)`'s start time in microseconds. libproc refuses
/// a zombie, and stops answering about 10 µs after a SIGKILL lands, well before the
/// corpse is waitable: `None` is "dying or dead", not "waitable" (hence
/// [`ShellIdentity::verify`]'s unreaped-child proof). Linux: `/proc/<pid>/stat` field 22
/// (clock ticks since boot), a zombie folded to `None`. Other Unixes: `None`.
///
/// `None` also for a pid of 1 or less, no such process, or a record for a different pid.
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
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote and
    // reads nothing through the pointer.
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
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is parenthesised and may contain spaces or `)`, so fields start
    // after the last `)`: `state` (field 3) first, `starttime` (field 22) twentieth.
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    if fields.next()? == "Z" {
        return None;
    }
    fields.nth(18)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_birth_platform(_pid: i32) -> Option<u64> {
    None
}

/// Whether `pid` is this process's child and not yet reaped (alive or a zombie), asked
/// with `waitid(WNOHANG | WNOWAIT)`, which reaps nothing. Such a pid cannot have been
/// reissued.
fn is_unreaped_child(pid: i32) -> bool {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return false;
    };
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `waitid` writes at most one `siginfo_t` through the valid pointer;
    // `WNOWAIT` leaves any waitable status in place, so this reaps nothing.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    rc == 0
}

/// Record the identity of a shell this process just spawned ([`spawn_shell_with_pid`]'s
/// `pid`), before anything could reap it. A child that already died reads no birth;
/// `own_child` still proves it until it is reaped.
#[must_use]
pub fn record_spawned_shell(pid: i32) -> ShellIdentity {
    ShellIdentity::from_parts(pid, process_birth(pid), true)
}

/// Record the identity of a shell adopted with its PTY master (the seamless-update
/// handoff), proving first that `pid` is still the shell on that PTY:
/// `tcgetsid(master) == pid`. XNU clears the tty's session at the leader's exit (the
/// master then answers -1), so a shell that died before adoption records no birth and
/// is never signalled by pid. The check runs on both sides of the birth read, so a pid
/// that changed hands in between cannot slip a stranger's start time in. `own_child` is
/// `false`: launchd is the parent.
#[must_use]
pub fn record_adopted_shell(pid: i32, master: i32) -> ShellIdentity {
    let leads_the_pty = || {
        // SAFETY: `tcgetsid` is a read-only `TIOCGSID` ioctl on a descriptor the
        // caller holds; a bad descriptor answers -1.
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
    /// Whether [`Self::pid`] still names the shell this identity was recorded for; every
    /// signal sent by pid asks immediately before it is sent.
    ///
    /// * The pid's start time is readable: it must equal the recorded one. With none
    ///   recorded, only our own unreaped child passes.
    /// * It is not readable (dead, dying, a zombie, or no reader): only our own unreaped
    ///   child passes, since the kernel cannot reissue a pid we have not waited for. An
    ///   adopted shell never passes: launchd reaps it.
    ///
    /// Racy only within the kernel's pid-reuse window between this check and the `kill`
    /// that follows: microseconds, against the minutes-long `--hold` window.
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

/// Teardown's `SIGHUP`, sent only where it provably belongs; non-blocking like
/// [`hangup`]. Returns what was signalled.
///
/// * `identity` verifies: `SIGHUP` the shell's process group
///   (`docs/DESIGN-pty-keeper-2026-09-26.md` F3).
/// * It does not: signal only `tcgetpgrp(master)`, the terminal's foreground group,
///   which can only be a process in the terminal's own session. A shell that already
///   exited leaves no session, so nothing is sent; the reader's wake pipe ends it.
/// * No shell pid (`pid <= 1`, a stub): nothing.
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
    // SAFETY: `tcgetpgrp` is a read-only ioctl on a descriptor the caller
    // holds; a bad descriptor answers -1.
    let foreground = unsafe { libc::tcgetpgrp(master) };
    if foreground <= 1 {
        return ShellHangup::Nothing;
    }
    // SAFETY: `killpg` posts SIGHUP to the terminal's own foreground group,
    // read from the master just above; it takes no pointers.
    unsafe {
        libc::killpg(foreground, libc::SIGHUP);
    }
    ShellHangup::Foreground(foreground)
}

/// Collect an exited child's status without blocking, at the PTY-exit edge, before
/// [`reap`] runs on the teardown thread and discards it. aterm installs no `SIGCHLD`
/// handler, so between the reader's EOF and the session's drop the child is a
/// collectable zombie.
///
/// `None` means unknown, never success: the child has not exited (EOF can also mean a
/// read error or a closed slave), `ECHILD` (an adopted session, or already reaped), or a
/// pid of 1 or less. Never escalate to a blocking wait: this runs on the UI thread.
///
/// Once this has reaped, the caller must latch it (the GUI's `child_reaped`) and never
/// hand the pid to teardown: the number is free. Call it on the exit edge only; on the
/// close path it would disarm [`reap_shell`]'s SIGKILL of a HUP-ignoring shell.
#[must_use]
pub fn collect_exit_status(pid: i32) -> Option<crate::ChildExit> {
    if pid <= 1 {
        return None;
    }
    let mut status: libc::c_int = 0;
    // SAFETY: `WNOHANG` `waitpid` with a valid out-param. Returns the pid when
    // reaped, 0 when the child is alive, -1 when it is not ours.
    if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } != pid {
        return None;
    }
    if libc::WIFEXITED(status) {
        return Some(crate::ChildExit::Code(libc::WEXITSTATUS(status)));
    }
    if libc::WIFSIGNALED(status) {
        return Some(crate::ChildExit::Signal(libc::WTERMSIG(status) as u8));
    }
    None
}

/// [`reap_shell`] for a pid the caller knows is its own unreaped child (a spawn that
/// failed half-way, a test). A pid that is not our child (`ECHILD`) is left alone.
pub fn reap(pid: i32) {
    reap_shell(ShellIdentity::from_parts(pid, None, true));
}

/// Reap a session's shell without ever blocking unboundedly, on the detached teardown
/// thread after [`hangup_shell`]. It polls `WNOHANG` every [`REAP_POLL`]: a well-behaved
/// shell is reaped at once; one that traps or ignores SIGHUP (`trap '' HUP`) is
/// SIGKILLed at [`REAP_KILL_AT`]; at [`REAP_DEADLINE`] a shell still our unreaped child
/// is handed to `aterm_uds::reaper`. A plain blocking `waitpid` would leak a thread per
/// such close.
///
/// The hand-off matters: a session leader that exits with output still queued on its
/// terminal sleeps in the tty drain until the master is read or closed, and no signal
/// ends that sleep (it has left its group, so `killpg` answers `EPERM`). The master
/// closes only when its last `SinkWriter` clone drops, possibly long after the deadline.
///
/// An adopted shell is not our child (`waitpid` answers `ECHILD`), so it is polled by
/// identity on the same schedule; launchd reaps it.
///
/// `identity` is re-verified before every `waitpid` and immediately before the SIGKILL;
/// once it fails this returns having sent nothing. Before the `waitpid` matters too: an
/// adopted shell's freed pid can be reissued to one of our later children, whose status
/// a blind `waitpid` would steal.
pub fn reap_shell(identity: ShellIdentity) {
    reap_shell_scheduled(
        identity,
        Some(REAP_KILL_AT),
        REAP_DEADLINE,
        InGrace::enter(),
    );
}

/// [`reap_shell`] on a thread of its own, so the UI thread never waits. Counted by
/// [`wait_for_reaps`] from this call on, before the thread runs.
pub fn reap_shell_in_background(identity: ShellIdentity) {
    if identity.pid <= 1 {
        return;
    }
    let grace = InGrace::enter();
    std::thread::spawn(move || {
        reap_shell_scheduled(identity, Some(REAP_KILL_AT), REAP_DEADLINE, grace);
    });
}

/// [`reap_shell`] with its schedule given: `kill_at` is the SIGKILL tick (`None`: never,
/// a test's stand-in for a child no signal can finish), `deadline` the tick it hands a
/// still-unreaped own child to `aterm_uds::reaper`. `grace` is its place in
/// [`wait_for_reaps`]'s count.
fn reap_shell_scheduled(
    identity: ShellIdentity,
    kill_at: Option<u32>,
    deadline: u32,
    mut grace: InGrace,
) {
    let pid = identity.pid;
    if pid <= 1 {
        return;
    }
    const POLL: std::time::Duration = REAP_POLL;
    let mut status: libc::c_int = 0;
    for tick in 0..deadline {
        if !identity.verify() {
            return; // gone, or the pid now names a different process
        }
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param. `-1`
        // (`ECHILD`) is an adopted shell, whose liveness the identity check answered.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return; // reaped
        }
        // Alive past the grace: it ignored SIGHUP. Re-verify immediately before the
        // SIGKILL, since the check above may be one `waitpid` stale.
        if Some(tick) == kill_at && identity.verify() {
            // SAFETY: best-effort signal post to the verified shell's own
            // process group; it takes no pointers.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            #[cfg(test)]
            REAP_KILL_TICK.with(|t| t.set(Some(tick)));
        }
        if Some(tick) == kill_at {
            grace.leave();
        }
        #[cfg(test)]
        REAP_POLLS.with(|n| n.set(n.get() + 1));
        std::thread::sleep(POLL);
    }
    // Past the deadline: a shell that is still our unreaped child goes to
    // `aterm_uds::reaper`. An adopted shell is launchd's to reap.
    if identity.own_child && identity.verify() {
        aterm_uds::reaper::hand_off(move || straggler_settled(&identity));
    }
}

/// Whether a straggling shell is settled: reaped now, or no longer ours to reap.
/// Re-verified before every wait, as in [`reap_shell`]. Never blocks, never signals.
fn straggler_settled(identity: &ShellIdentity) -> bool {
    if !identity.verify() {
        return true;
    }
    let mut status: libc::c_int = 0;
    // SAFETY: `WNOHANG` `waitpid` on the verified own child; a valid out-param.
    let r = unsafe { libc::waitpid(identity.pid, &mut status, libc::WNOHANG) };
    r == identity.pid
        || (r == -1 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
}

/// How many [`reap_shell`] calls are still inside their grace: neither settled nor past
/// their SIGKILL.
static IN_GRACE: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
/// Signalled whenever [`IN_GRACE`] falls.
static GRACE_LEFT: std::sync::Condvar = std::sync::Condvar::new();

/// One [`reap_shell`]'s place in [`IN_GRACE`], given back once (at the SIGKILL
/// tick, or when the reap ends first).
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

/// Wait, at most `bound`, until every [`reap_shell`] in flight has settled its shell or
/// sent its SIGKILL; `true` when they all have.
///
/// For a process about to exit: [`reap_shell_in_background`]'s thread dies with the
/// process, so a SIGHUP-ignoring shell still in its grace would never be SIGKILLed. A
/// holdout costs the quit the grace (~250 ms); a well-behaved shell, milliseconds.
pub fn wait_for_reaps(bound: std::time::Duration) -> bool {
    let in_grace = IN_GRACE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (in_grace, _) = GRACE_LEFT
        .wait_timeout_while(in_grace, bound, |n| *n > 0)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *in_grace == 0
}

/// [`reap_shell`]'s poll period.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(10);
/// The [`reap_shell`] tick that SIGKILLs a SIGHUP-ignoring holdout (~250 ms).
const REAP_KILL_AT: u32 = 25;
/// The [`reap_shell`] tick at which it gives up (~2 s).
const REAP_DEADLINE: u32 = 200;

#[cfg(test)]
thread_local! {
    /// How many [`REAP_POLL`] waits [`reap_shell`] has taken on this thread, counted in
    /// the loop's own ticks so a descheduled test thread cannot skew it.
    static REAP_POLLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// The tick at which [`reap_shell`] last sent its SIGKILL on this thread.
    static REAP_KILL_TICK: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// This thread's [`REAP_POLLS`] reading.
#[cfg(test)]
fn reap_polls() -> u32 {
    REAP_POLLS.with(std::cell::Cell::get)
}

/// This thread's [`REAP_KILL_TICK`], cleared as it is read.
#[cfg(test)]
fn take_reap_kill_tick() -> Option<u32> {
    REAP_KILL_TICK.with(std::cell::Cell::take)
}

/// Build the `sandbox-exec`-wrapped `(exec_target, argv)` for an OS-sandboxed spawn,
/// failing closed if `wrapper_path` is not executable. Pure apart from that `access`
/// probe, so it is tested without forking.
///
/// The target is `wrapper_path` and the argv
/// `["sandbox-exec", "-p", <sbpl>, <prog>, <orig argv[1..]>]`: `sandbox-exec` execs its
/// first positional with that path as argv[0], so a login-dash argv[0] is dropped.
///
/// # Errors
/// `NotFound` if the wrapper is missing (the caller must not spawn unsandboxed);
/// `Other`/`InvalidInput` for an interior NUL in `wrapper_path` or `sbpl`.
fn build_sandbox_wrap(
    wrapper_path: &str,
    sbpl: &str,
    prog: &CString,
    orig_argv: &[CString],
) -> io::Result<(CString, Vec<CString>)> {
    let wrapper = CString::new(wrapper_path.as_bytes())
        .map_err(|_| io::Error::other("sandbox-exec path not representable"))?;
    // A missing wrapper means the demanded sandbox cannot be applied: refuse.
    // SAFETY: `wrapper` is a valid NUL-terminated absolute path.
    let present = unsafe { libc::access(wrapper.as_ptr(), libc::X_OK) } == 0;
    if !present {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "OS sandbox demanded but {wrapper_path} is missing/not executable — refusing \
                 to spawn an unsandboxed shell (fail-closed, ATERM_DESIGN §5.6)"
            ),
        ));
    }
    let sbpl_c = CString::new(sbpl.as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "SBPL profile has interior NUL")
    })?;
    // The capacity is a hint, clamped so the allocation has a provable bound.
    let mut wrapped: Vec<CString> = Vec::with_capacity(orig_argv.len().saturating_add(3).min(4096));
    wrapped.push(CString::new("sandbox-exec").unwrap_or_else(|_| wrapper.clone()));
    // A `CStr` literal, not `CString::new(..).unwrap()`: no panic path.
    wrapped.push(CString::from(c"-p"));
    wrapped.push(sbpl_c);
    wrapped.push(prog.clone());
    wrapped.extend(orig_argv.iter().skip(1).cloned());
    Ok((wrapper, wrapped))
}

/// What an exec failure says: the target, what is wrong with it, and `from` (where a
/// shell came from; empty for a `-e` program or the sandbox wrapper).
///
/// Read in the PARENT after the child's `execve` failed, so it may stat. A name with
/// no `/` is not looked at: a bare shell or `-e` program reaches `execve` only when
/// the PATH lookup ([`crate::select_shell_with_origin`], [`resolve_program`]) found
/// it nowhere. A relative path is judged from `cwd`, the folder the child ran it in.
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

/// `args` as the C strings `execve` hands a program, every element in its place, or the
/// refusal of the whole spawn when one holds a NUL: leaving it out would shift the rest
/// (`rm -- "$a" "$b"` with a NUL in `$a` would run `rm -- "$b"`). `list` names the
/// source for the error. Run in the parent, before the fork.
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

/// PATH-resolve a `-e` program name to an absolute path in the parent (the child must
/// stay async-signal-safe, so it does no `execvp` search). A name containing `/` is used
/// verbatim; otherwise the first executable regular file on `$PATH` wins. With no match
/// the name is returned verbatim, so `execve` fails cleanly rather than this masking it.
// Skip: `CString::new` panics only on allocation (an interior NUL is an `Err`). Drop
// this once Trust has a totality entry for `CString::new`.
#[cfg_attr(trust_verify, trust::skip)]
fn resolve_program(name: &std::ffi::OsStr) -> CString {
    // A `c"..."` literal fallback, not `CString::new(..).unwrap()`: no panic path.
    let verbatim = || CString::new(name.as_bytes()).unwrap_or_else(|_| c"/nonexistent".to_owned());
    // The lookup the shell's resolution makes too: an executable regular file in
    // the first non-empty PATH entry that has one.
    crate::unix_shell::find_on_path(name, std::env::var_os("PATH").as_deref())
        .and_then(|found| CString::new(found.as_os_str().as_bytes()).ok())
        .unwrap_or_else(verbatim)
}

/// argv[0] of a session's interactive shell when nothing else chose its argv,
/// from the program's basename.
///
/// **macOS:** the LOGIN form, `-zsh` — the platform's convention (Terminal.app,
/// iTerm2): no login ran before the window, so the shell sources `.zprofile` /
/// `.bash_profile` and `path_helper` builds PATH.
///
/// **Every other Unix:** the name itself, an interactive NON-login shell — the
/// convention of GNOME Terminal, Konsole, xterm, foot, kitty and Ghostty there. The
/// desktop session already ran the login profile, and a login shell per tab ran
/// it again: Ubuntu's stock `~/.profile` prepends `~/.local/bin` to PATH without
/// looking, so every tab doubled it, and a login bash reads `~/.bashrc` only when
/// `~/.profile` sources it. A person who wants a login shell anyway sets
/// `shell_args = ["-l"]`.
fn default_shell_argv0(base: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut argv0 = std::ffi::OsString::new();
    if cfg!(target_os = "macos") {
        argv0.push("-");
    }
    argv0.push(base);
    argv0
}

/// The decision a single `write(2)` return drives in the `write_all` loop, a pure value
/// so the branches are tested without provoking a real `EINTR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStep {
    /// A signal interrupted the write before any byte moved (`EINTR`): retry.
    Retry,
    /// A real error, or the peer closed (`r == 0`): stop draining.
    Stop,
    /// `n` bytes were written: advance the slice cursor by `n` and continue.
    Advance(usize),
}

/// Classify a `write(2)` result for the `write_all` loop: `r` is the raw return and
/// `is_eintr` whether `errno` was `EINTR` (consulted only when `r < 0`). Pure, so it is
/// tested with synthetic inputs.
fn classify_write_result(r: isize, is_eintr: bool) -> WriteStep {
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

/// Write all of `bytes` to the PTY master, retrying short writes and `EINTR` (dropping
/// the rest would lose terminal input). Stops only on a real error or a zero return.
///
/// This loop implements `WriteAll` (`aterm_models_session::write_all_model`): the cursor
/// only advances, and only the full-buffer exit reports completion (`NoSilentDrop`:
/// `done ⇒ off = Size`). A step that reaches the end is `Progress`, one that does not
/// (a short write, or `EINTR`: [`WriteStep::Advance`] with a partial `n`, or
/// [`WriteStep::Retry`]) is `Interrupted` and keeps looping rather than returning with a
/// dropped tail.
// Projection onto `<<off, done>>`: `off = bytes.len() - data.len()`,
// `done = data.is_empty()`. `write_all_project` is a name only: Trust requires it be
// present and does not execute it.
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
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `data` is a
        // valid slice of `data.len()` bytes.
        let r = unsafe { libc::write(master, data.as_ptr() as *const libc::c_void, data.len()) };
        // `errno` is meaningful only when `r < 0`.
        let is_eintr = r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
        match classify_write_result(r, is_eintr) {
            WriteStep::Retry => continue,
            WriteStep::Stop => break,
            // `write(2)` never reports more than `data.len()`, so the fallback never
            // fires; it discharges the Trust slice-bounds obligation.
            WriteStep::Advance(n) => data = data.get(n..).unwrap_or(&[]),
        }
    }
}

/// Write `bytes` to the PTY master with a single `write(2)` (retrying only `EINTR`),
/// returning how many bytes the kernel accepted, so a sink (aterm-session's
/// `SinkWriter`) applies real backpressure instead of dropping the tail.
///
/// Errors: [`io::ErrorKind::WouldBlock`] when `master` is non-blocking
/// ([`set_nonblocking`]) and the input buffer is full; others when the slave is gone. A
/// peer closed mid-write is `Ok(0)`.
// #[inline] so the MIR crosses the crate boundary: callers' Trust gates verify this body.
#[inline]
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes.
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

/// A single non-parking `write(2)`, classified for aterm-session's
/// `SinkWriter::write_frame_nonparking`, which must tell `WouldBlock` from a fatal error
/// without dropping an `io::Error`.
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
// #[inline] so the MIR crosses the crate boundary: aterm-session verifies this body.
#[cfg(unix)]
#[inline]
pub fn write_some_nonparking(master: i32, bytes: &[u8]) -> NonParkWrite {
    if bytes.is_empty() {
        return NonParkWrite::Closed;
    }
    loop {
        // SAFETY: identical to `write_some` — `master` is a PTY master fd; `bytes` is a
        // valid slice of `bytes.len()` bytes.
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            // The `Os(errno)` variant: dropped here (a total drop) or moved into `Fatal`.
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

/// Poll the PTY master for writability, blocking up to `timeout_ms` (`0`: check and
/// return). `POLLERR`/`POLLHUP`/`POLLNVAL` report writable on purpose, so the following
/// write surfaces the real error instead of the bytes spilling forever. `EINTR` retries.
/// The basis of aterm-session's non-parking keystroke egress: a UI-thread writer checks
/// it before a `write(2)` that a wedged program would park the event loop on.
// #[inline] so the MIR crosses the crate boundary: aterm-session's non-parking write
// path verifies this body. Its only error is the `Os(errno)` `io::Error`; no panic path.
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

/// Duplicate an fd into an [`std::os::fd::OwnedFd`] the caller owns: the same open file
/// description with an independent lifetime, so a detached thread (the sink's spill
/// drainer) keeps naming the PTY even if the original fd number is closed and recycled.
/// The duplicate is close-on-exec.
// #[inline] so the MIR crosses the crate boundary: aterm-session's spill path verifies
// this body.
#[inline]
pub fn dup_fd(fd: i32) -> io::Result<std::os::fd::OwnedFd> {
    // `F_DUPFD_CLOEXEC`, not `dup`: `dup(2)` clears `FD_CLOEXEC` on the copy, and the
    // spill drainer (`aterm_session::sink::Shared::arrange_drainer`) holds its copy as
    // long as a wedged shell keeps the queue spilling, so every `Command::spawn` in that
    // time would inherit a writable descriptor onto the terminal. The flag is set
    // atomically with the duplication.
    //
    // SAFETY: `fcntl(F_DUPFD_CLOEXEC, 0)` returns a fresh descriptor or -1; the negative
    // return is checked before `from_raw_fd`.
    let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `d` is a fresh fd we exclusively own, just created by the `fcntl` above.
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(d) })
}

/// Toggle `O_NONBLOCK` on the PTY master, so [`write_some`] returns
/// [`io::ErrorKind::WouldBlock`] when the input buffer is full instead of blocking.
/// Idempotent; leaves other status flags alone.
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

/// Toggle `FD_CLOEXEC`, the descriptor flag (`F_GETFD`/`F_SETFD`), on `fd`. Every master
/// is born close-on-exec ([`open_pty_pair_cloexec`]); the seamless re-exec clears it
/// (`on = false`) on each master it hands to the new binary, so the shell survives
/// `execve`. Idempotent; leaves other descriptor flags alone.
///
/// Clear it only on a master about to be handed off and then re-adopted or closed: one
/// that is neither leaks a live PTY across the exec.
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

/// The size the kernel holds for this PTY (`TIOCGWINSZ` on the master), `(rows, cols)`,
/// or `None` off a tty or at 0 x 0: what the child reads, so a host that briefly sets
/// another size (the repaint nudge) can verify it put the size back.
#[must_use]
pub fn size(master: i32) -> Option<(u16, u16)> {
    if master < 0 {
        return None;
    }
    // SAFETY: `libc::winsize` is a plain struct of integer fields; all zeros is
    // a valid value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `master` may be any integer; the ioctl fails cleanly on a bad or
    // non-tty fd, and `ws` is a valid out-param on success.
    let rc = unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) };
    (rc == 0 && (ws.ws_row, ws.ws_col) != (0, 0)).then_some((ws.ws_row, ws.ws_col))
}

/// Whether `fd` is a tty (`TIOCGWINSZ` succeeds): a sanity check on the seamless
/// re-exec's incoming side, not its authenticity gate. A pty slave or an inherited
/// stdio tty also passes; the defence against a spoofed `ATERM_SEAMLESS_FDS` is the
/// single-use nonce stamp and adopting only the fds the authenticated handoff map lists.
#[must_use]
pub fn fd_is_tty(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    // SAFETY: `libc::winsize` is a plain struct of integer fields (rows/cols/pixels);
    // an all-zeros bit pattern is a valid, fully-initialized value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer; `ioctl` fails cleanly (rc<0) on a bad/non-tty fd
    // and `ws` is a valid winsize out-param on success.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    rc == 0
}

/// The slave's ECHO and ICANON bits, and its signal characters while ISIG is set, read
/// through the master ([`crate::TtyEcho`]). `None` when `fd` is not a tty.
///
/// A PTY pair has one termios, the slave's, and `tcgetattr` on either end reads it (BSD
/// and Linux drivers alike; the test module's real-spawn test and aterm-gui's
/// `a_press_the_tty_will_not_echo_banks_no_credit` show it). One `ioctl`, cheap enough
/// for every key; the answer is a snapshot, so it is evidence about the press, never a
/// lock on the tty.
#[must_use]
pub fn tty_echo(fd: i32) -> Option<crate::TtyEcho> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `libc::termios` is a plain struct of integer fields and a `c_cc`
    // byte array; an all-zeros bit pattern is a valid, fully-initialized value.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer; `tcgetattr` fails cleanly (rc != 0,
    // EBADF/ENOTTY) on a bad or non-tty fd and fills `t` on success.
    let rc = unsafe { libc::tcgetattr(fd, &mut t) };
    if rc != 0 {
        return None;
    }
    // `_POSIX_VDISABLE`, which `libc` does not declare: 0xff on the BSDs, NUL on Linux.
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

/// Whether the slave's line discipline hands every byte written into this PTY to the
/// program's input queue, so a queue holding fewer bytes than were written means the
/// program read some. `Some(true)` exactly when `ICANON`, `ISIG`, `IEXTEN`, `IXON` and
/// `IGNCR` are all clear (`cfmakeraw`); `None` off a tty.
///
/// Each of those flags lets the driver consume a byte itself (`ISIG` signals and
/// flushes, `IXON` takes `VSTART`/`VSTOP`, `IEXTEN` `VLNEXT`/`VDISCARD`, `IGNCR` every
/// `\r`): in cbreak mode `^C`, `^S`, `^O` and `^V` each leave FIONREAD at 0 with nobody
/// reading.
#[must_use]
pub fn tty_passes_every_byte(fd: i32) -> Option<bool> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `libc::termios` is a plain struct of integer fields and a `c_cc`
    // byte array; an all-zeros bit pattern is a valid, fully-initialized value.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer; `tcgetattr` fails cleanly (rc != 0,
    // EBADF/ENOTTY) on a bad or non-tty fd and fills `t` on success.
    let rc = unsafe { libc::tcgetattr(fd, &mut t) };
    if rc != 0 {
        return None;
    }
    Some(
        t.c_lflag & (libc::ICANON | libc::ISIG | libc::IEXTEN) == 0
            && t.c_iflag & (libc::IXON | libc::IGNCR) == 0,
    )
}

/// `TIOCOUTQ` (`_IOR('t', 115, int)`): a tty's output-queue depth. The `libc` crate does
/// not declare it; `flush_input_queue_discards_the_unread_input_and_nothing_else` reads
/// it back against bytes a slave wrote.
#[cfg(target_os = "macos")]
const TIOCOUTQ: libc::c_ulong = 0x4004_7473;

/// How many bytes written into this PTY the program at the slave has not read yet
/// (`ioctl(FIONREAD)` on the master, which the Darwin driver answers from the slave's
/// input queue). `None` off a tty, on error, and on every platform but macOS.
///
/// A frozen program stops drawing, so screen checks pass while keys queue unread; a
/// raw-mode reader drains within a turn, so a byte that stays says it stopped reading.
/// Measured on Darwin (the raw and canonical readings are driven through aterm-session's
/// `tests/input_backlog_pty.rs`):
///
/// * canonical mode counts complete lines only (`abc` reads 0, `abc\n` reads 4);
/// * raw mode counts every byte;
/// * raw with `VMIN` above the queued count and `VTIME` = 0 reads 0;
/// * every error answers `None`, never a guess.
///
/// macOS only: a Linux master's `FIONREAD` counts the output the master has not read,
/// a different question. No lock and no blocking: a caller may run it while a writer
/// is parked on a full queue. The answer is a snapshot.
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

/// How many bytes the program at the slave has written that the master has not read
/// (`ioctl(TIOCOUTQ)` on the master). `None` as for [`input_queue_len`]. With ECHO on in
/// canonical mode the kernel's echo of typed bytes counts too.
///
/// The fairness half of the stall reading: a program blocked writing to an aterm that
/// is not draining is waiting for aterm, so a non-zero count means the lag may be
/// aterm's.
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

/// Discard every byte written into this PTY that the program at the slave has not read
/// (`tcflush(master, TCIFLUSH)`) and return how many there were. `None` off a tty or on
/// error. Off macOS it still flushes but counts nothing ([`input_queue_len`]): `Some(0)`.
///
/// Keys typed at a frozen program stay queued after it is killed, and the shell that
/// takes the terminal back would run complete lines as commands. Measured on Darwin:
/// the flush empties the slave's input queue and leaves its output queue
/// (`TIOCOUTQ`); in canonical mode it also drops the partial line FIONREAD cannot
/// count, so a caller keeping a shell's type-ahead must not call it there.
#[must_use]
pub fn flush_input_queue(fd: i32) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let unread = queue_len(fd, libc::FIONREAD)?;
        // SAFETY: `fd` is a tty (checked by `queue_len` just above) and
        // `tcflush` only discards queued data; a stale fd fails with rc < 0.
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
        // SAFETY: `fd` is a tty (checked by `fd_is_tty` just above) and
        // `tcflush` only discards queued data; a stale fd fails with rc < 0.
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
            return None;
        }
        Some(0)
    }
}

/// The shared body of the two queue probes: an int-valued tty ioctl, `None`
/// unless `fd` is a tty and the call succeeds with a non-negative count.
#[cfg(target_os = "macos")]
fn queue_len(fd: i32, request: libc::c_ulong) -> Option<usize> {
    if fd < 0 {
        return None;
    }
    // SAFETY: `isatty` takes any integer and only asks the kernel about it; a
    // bad or closed fd answers 0, never undefined behaviour.
    if unsafe { libc::isatty(fd) } != 1 {
        return None;
    }
    let mut n: libc::c_int = 0;
    // SAFETY: `request` is FIONREAD or TIOCOUTQ, both `_IOR(.., int)`: the
    // kernel writes exactly one `c_int` through the pointer, and `n` is a live
    // `c_int` on this stack. A bad fd fails cleanly with rc < 0.
    let rc = unsafe { libc::ioctl(fd, request, &mut n) };
    if rc != 0 {
        return None;
    }
    usize::try_from(n).ok()
}

/// Read up to `buf.len()` bytes from the PTY master into `buf`. Returns the number
/// of bytes read (`0` = EOF, `< 0` = error, per `read(2)`).
pub fn read(master: i32, buf: &mut [u8]) -> isize {
    // SAFETY: `master` is a valid fd; `buf` is a valid mutable slice of
    // `buf.len()` bytes.
    unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) }
}

/// A wake pipe for cancelling a parked PTY reader: `(read_end, write_end)`, both
/// close-on-exec (they must not survive a seamless re-exec) and non-blocking ([`wake`]
/// runs on the event loop, which a full pipe must never block). `None` on failure; the
/// caller then degrades to [`read_or_wake`]'s wake-pipe-less path.
///
/// Linux sets both flags in `pipe2`. Darwin has no `pipe2`, and its window-free carrier
/// (the exec-status channel's unlinked fifo) costs filesystem work, tens of ms on a busy
/// volume, on every adopted session's reader attach; a stray copy of a wake end blocks
/// nothing, so Darwin keeps `pipe(2)` + `fcntl`.
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    #[cfg(target_os = "linux")]
    {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-int out-array; `pipe2` applies both flags to
        // both descriptors as it creates them.
        (unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0)
            .then_some((fds[0], fds[1]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-int out-array for pipe(2).
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        let (rd, wr) = (fds[0], fds[1]);
        // SAFETY: rd/wr are freshly-created pipe fds; these fcntls only read/set flags.
        let configured = unsafe {
            let rd_flags = libc::fcntl(rd, libc::F_GETFL);
            let wr_flags = libc::fcntl(wr, libc::F_GETFL);
            libc::fcntl(rd, libc::F_SETFD, libc::FD_CLOEXEC) != -1
                && libc::fcntl(wr, libc::F_SETFD, libc::FD_CLOEXEC) != -1
                && rd_flags >= 0
                && wr_flags >= 0
                && libc::fcntl(rd, libc::F_SETFL, rd_flags | libc::O_NONBLOCK) != -1
                && libc::fcntl(wr, libc::F_SETFL, wr_flags | libc::O_NONBLOCK) != -1
        };
        if !configured {
            close_fd(rd);
            close_fd(wr);
            return None;
        }
        Some((rd, wr))
    }
}

/// Outcome of [`read_or_wake`].
pub enum ReadOutcome {
    /// `n` bytes (`n > 0`) were read into the caller's buffer.
    Data(usize),
    /// The master reached EOF / error — the child's slave closed (the shell exited).
    Eof,
    /// The wake pipe fired: the owning `Session` dropped (tab/pane close) though the
    /// master has not EOF'd, since an orphaned child may still hold the slave. The
    /// reader exits so the master, Terminal and writer threads are released.
    Wake,
    /// The wake-pipe-less fallback (`wake_rd < 0`) timed out with no master activity:
    /// the caller re-checks its fd-free stop flag and calls again if still live. Never
    /// produced on the wake-pipe path.
    Idle,
    /// The shell's exit watch (`exit_rd`) fired. A job it left behind (`cmd &`, a
    /// `setsid` grandchild) can hold the slave open, and Linux does not hang a PTY up
    /// at its session leader's exit, so the master may never EOF. The caller passes on
    /// what the master holds and ends the session as at EOF.
    Exited,
}

/// Whether a `poll(2)` entry for an exit watch says the process exited: a
/// pidfd or a kqueue reads readable then (a reaped pidfd also hangs up).
fn exit_fired(entry: &libc::pollfd) -> bool {
    entry.revents & (libc::POLLIN | libc::POLLHUP) != 0
}

/// Timeout (ms) of the wake-pipe-less poll in [`read_or_wake`]: prompt enough to reclaim
/// an orphan-pinned session, ~4 wakeups/s when idle.
const WAKELESS_POLL_TIMEOUT_MS: libc::c_int = 250;

/// Block until the master is readable (or EOF), `wake_rd` fires, or `exit_rd` fires. The
/// wake ([`ReadOutcome::Wake`]) frees a reader parked on a master that an orphaned,
/// different-pgroup child keeps open.
///
/// `exit_rd` (`-1`: none) reads readable once the session's shell has exited
/// (`aterm_uds::exitwatch::exit_fd`) and answers [`ReadOutcome::Exited`], checked after
/// the wake and before the master: the session ends with its shell, not with the
/// slave's last close (docs/AUDIT-pty-render-2026-10-09.md P3). A watch that reports an
/// error is never read as an exit.
///
/// `wake_rd < 0` (fd exhaustion) does not fall back to a blocking `read`, which would
/// pin the reader on an orphan-held master: it polls with a bounded timeout and returns
/// [`ReadOutcome::Idle`] so the caller can check an fd-free stop flag.
pub fn read_or_wake(master: i32, buf: &mut [u8], wake_rd: i32, exit_rd: i32) -> ReadOutcome {
    let exit_watch = libc::pollfd {
        fd: exit_rd,
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
        ];
        // SAFETY: `fds` is a valid 2-element pollfd array for the duration of the
        // call; a negative `fd` (no exit watch) is skipped by `poll`.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, WAKELESS_POLL_TIMEOUT_MS) };
        if r < 0 {
            let e = io::Error::last_os_error();
            // EINTR → hand control back so the caller re-checks its stop flag and retries;
            // any other poll error tears the reader down rather than letting it spin.
            return if e.kind() == io::ErrorKind::Interrupted {
                ReadOutcome::Idle
            } else {
                ReadOutcome::Eof
            };
        }
        if r == 0 {
            return ReadOutcome::Idle; // timed out, master quiet — caller re-checks stop
        }
        if exit_fired(&fds[1]) {
            return ReadOutcome::Exited;
        }
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // An `O_NONBLOCK` master can EAGAIN spuriously after POLLIN: that is Idle,
            // not Eof. EINTR likewise.
            if n < 0
                && matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
            {
                return ReadOutcome::Idle;
            }
            return ReadOutcome::Eof;
        }
        return ReadOutcome::Idle; // spurious wakeup — caller re-checks stop and retries
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
    ];
    loop {
        // SAFETY: `fds` is a valid 3-element pollfd array for the duration of the
        // call; a negative `fd` (no exit watch) is skipped by `poll`.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // A broken poll tears the reader down rather than letting it spin.
            return ReadOutcome::Eof;
        }
        // Wake takes priority — the owner wants us gone regardless of pending output.
        if fds[1].revents != 0 {
            return ReadOutcome::Wake;
        }
        if exit_fired(&fds[2]) {
            return ReadOutcome::Exited;
        }
        if fds[2].revents != 0 {
            fds[2].fd = -1; // a broken watch: poll without it rather than spin
        }
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // An `O_NONBLOCK` master can EAGAIN spuriously after POLLIN: poll again
            // rather than report a false Eof. EINTR likewise.
            if n < 0
                && matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
            {
                continue;
            }
            return ReadOutcome::Eof;
        }
        // Spurious wakeup with no revents — poll again.
    }
}

/// Top up `buf` past an initial [`read_or_wake`] read. macOS caps every master `read` at
/// ~1 KiB (the tty output-queue high-water mark), so gathering many chunks into one batch
/// amortizes every downstream per-read cost. Two regimes:
///   * `filled` below one outq: interactive output, returned on the first quiet poll;
///   * `filled` at or above one outq: the writer is saturating the queue, so its µs
///     refill gaps are bridged with up to `DRAIN_SPIN_MAX` short spins, not sleeps.
///
/// EOF or an error just ends the top-up; the next `read_or_wake` reports it.
pub fn drain_more(master: i32, buf: &mut [u8], mut filled: usize) -> usize {
    /// One kernel tty output queue's worth — at/above this the writer saturated it.
    const SATURATED: usize = 1024;
    /// Spin-retry budget bridging the writer's refill gaps (~0.5–1 µs each).
    const DRAIN_SPIN_MAX: u32 = 16;
    let mut spins = 0u32;
    while filled < buf.len() {
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is a valid 1-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
        if r > 0 && fds[0].revents & libc::POLLIN != 0 {
            let n = read(master, &mut buf[filled..]);
            if n <= 0 {
                break; // EOF/error: keep what we have; the next outer poll reports it
            }
            filled += n as usize;
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
    filled
}

/// Default idle-refill patience. Measured (500 MB flood + CPR probe): 0 µs ⇒ 231 MB/s;
/// 50 ⇒ 266 MB/s at +79 µs p50 query latency, with a better p99; 250 and 1000 only add
/// latency.
const IDLE_POLL_DEFAULT_US: u32 = 50;

/// [`drain_more_nonblocking`]'s per-batch budget while a keystroke is pending: short
/// enough not to hold a human's echo for a streaming program's cadence, long enough to
/// coalesce a TUI's per-keystroke repaint into one batch.
const HOT_BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(1);

/// Idle-cutoff hysteresis: how long a dry, parser-idle gap may wait for a refill before
/// delivering. An immediate cutoff (0) costs ~33-43 MB/s of flood throughput in
/// small-batch churn. A development build may set `ATERM_PTY_IDLE_POLL_US=<µs>`
/// ([`aterm_types::dev_seam!`]); read once, unparsable means the default. Clamped:
/// a wait past the batch budget is meaningless, and on the wake-pipe-less path it would
/// stall teardown.
fn idle_poll_us() -> u32 {
    /// Upper clamp, just past `BATCH_BUDGET`'s 3 ms.
    const IDLE_POLL_CLAMP_US: u32 = 5_000;
    static IDLE_POLL_US: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *IDLE_POLL_US.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_PTY_IDLE_POLL_US")
            .and_then(|v| v.to_string_lossy().trim().parse().ok())
            .unwrap_or(IDLE_POLL_DEFAULT_US)
            .min(IDLE_POLL_CLAMP_US)
    })
}

/// Outcome of one parked bridge poll: the master refilled (keep gathering into
/// the same batch) or delivery is due (quiet timeout, wake signal, HUP, error).
enum BridgeWait {
    Refill,
    Deliver,
}

// The bridge polls this thread has taken (tests only), so a test reads which park a
// gather took without a clock. Thread-local: other tests poll concurrently.
#[cfg(test)]
thread_local! {
    static BRIDGE_POLLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

// Where this thread's idle waits count the signals that interrupted them (tests only),
// so a test that signals a parked gather from another thread reads there that one
// landed inside the park.
#[cfg(test)]
thread_local! {
    static IDLE_WAIT_INTERRUPTS: std::cell::RefCell<
        Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    > = const { std::cell::RefCell::new(None) };
}

// A test's own batch budget for this thread's gathers (tests only), replacing the hot
// or streaming one, for tests whose subject is a park or a cutoff: the shipped budgets
// are wall clocks, and a preempted gather on a loaded machine delivers on them before
// reaching the gap under test. The budgets keep their own test on shipped values.
#[cfg(test)]
thread_local! {
    static BUDGET_FOR_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

// The budget that ended this thread's last gather, if one did (tests only): read from
// the gather, not a stopwatch, which a loaded machine makes meaningless.
#[cfg(test)]
thread_local! {
    static BUDGET_ENDED: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// The batch budget a gather checks at each dry gap: `shipped`, or in a test build the
/// test's own (`BUDGET_FOR_TEST`).
fn batch_budget(shipped: std::time::Duration) -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        return budget;
    }
    shipped
}

/// Park for the writer's next refill: one bounded poll on `[master, wake_rd]`. A pending
/// wake byte is not consumed: [`read_or_wake`] must see it after the batch is delivered.
fn bridge_poll(master: i32, wake_rd: i32, timeout_ms: i32) -> BridgeWait {
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
    ];
    let nfds: libc::nfds_t = if wake_rd >= 0 { 2 } else { 1 };
    // SAFETY: polling fds this gather owns, with a bounded timeout.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), nfds, timeout_ms) };
    if r <= 0 {
        return BridgeWait::Deliver; // timeout (burst over), EINTR, or error
    }
    if nfds == 2 && fds[1].revents != 0 {
        return BridgeWait::Deliver; // teardown wake — deliver what we have
    }
    if fds[0].revents & libc::POLLIN != 0 {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver // HUP/ERR without data: the next read_or_wake decides
}

/// Park the parser-idle cutoff for the writer's next refill: a bounded `select(2)` on
/// `[master, wake_rd]` (`poll(2)` is ms-granular; these gaps are µs). A pending wake
/// byte is left for [`read_or_wake`], as in [`bridge_poll`]. Timeout and error deliver.
///
/// A signal handler that runs on this thread meanwhile does not: `select` is never
/// restarted after one (signal(7)), so the park resumes for the time it has left. Any
/// handler can land here, and a binary translator runs its own: Rosetta signals every
/// thread of a process it translates whenever it syncs them (an internal `SIGTRAP`), and
/// a gather that delivered on that `EINTR` split the batch this wait exists to keep
/// whole (x86_64 under Rosetta, measured 2026-10-17: 2048 of 4096 bytes).
fn idle_refill_wait(master: i32, wake_rd: i32, wait_us: u32) -> BridgeWait {
    // select cannot represent fds >= FD_SETSIZE — deliver, same as knob-off.
    if master >= libc::FD_SETSIZE as i32 || wake_rd >= libc::FD_SETSIZE as i32 {
        return BridgeWait::Deliver;
    }
    let wait = std::time::Duration::from_micros(u64::from(wait_us));
    let start = std::time::Instant::now();
    let mut left = wait;
    let (r, set) = loop {
        // SAFETY: an all-zero fd_set is the empty set FD_ZERO produces.
        let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
        // SAFETY: both fds were bounds-checked against FD_SETSIZE above.
        unsafe {
            libc::FD_SET(master, &mut set);
            if wake_rd >= 0 {
                libc::FD_SET(wake_rd, &mut set);
            }
        }
        let mut tv = libc::timeval {
            tv_sec: left.as_secs() as _,
            tv_usec: left.subsec_micros() as _,
        };
        // SAFETY: selecting on fds this gather owns, with a bounded timeout.
        let r = unsafe {
            libc::select(
                master.max(wake_rd) + 1,
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
    if r <= 0 {
        return BridgeWait::Deliver; // timeout (burst over) or error
    }
    // SAFETY: reading a set select just filled; fds are < FD_SETSIZE.
    if wake_rd >= 0 && unsafe { libc::FD_ISSET(wake_rd, &set) } {
        return BridgeWait::Deliver; // teardown wake — byte left for read_or_wake
    }
    // SAFETY: as above.
    if unsafe { libc::FD_ISSET(master, &set) } {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver
}

/// [`drain_more`] for an `O_NONBLOCK` master: `read(2)` directly to `EAGAIN`, then bridge
/// the writer's µs refill gaps with up to `NB_SPIN_MAX` immediate re-reads (the ~0.4 µs
/// failed `read` is the pause; a paced spin overshoots the 1-2 µs refill). When a gap
/// outlives those, one parked `BRIDGE_POLL_MS` poll on `[master, wake]` continues the
/// batch on refill. Delivery triggers:
///   * the first quiet with under one outq (~1 KiB) filled: interactive echo;
///   * the parse stage idle at a dry gap (`parse_in_flight == 0`): holding the batch
///     only adds latency, and a mid-burst query's reply may be in it;
///   * a bridge poll quiet for its full timeout, the wake pipe (the byte is left for
///     [`read_or_wake`]), `BATCH_BUDGET` spent (checked per dry gap), a full buffer,
///     EOF, or a hard error (surfaced by the next [`read_or_wake`]).
///
/// The master must be non-blocking ([`set_nonblocking`]); callers fall back to
/// [`drain_more`] when the fcntl fails. `wake_rd < 0` drops the wake fd from the poll;
/// `parse_in_flight = None` bridges unconditionally. A parser-idle dry gap waits up to
/// [`idle_poll_us`] for a refill before delivering.
///
/// `interactive_pending` is the host's "a human is waiting" hint, read at each dry gap:
/// while true the budget is [`HOT_BATCH_BUDGET`] and the gap is bridged with the µs idle
/// wait instead of the poll, so an echo typed into a streaming program arrives on the
/// human's schedule while a TUI repaint still coalesces.
///
/// `at_gap(filled)` is the host's fence, called at each dry gap just before parking;
/// `true` delivers now and leaves any refill queued. aterm-gui samples the foreground
/// process group there: every byte read so far is in `buf`, so a group change is cut
/// exactly at `filled`. A host with no fence passes `&mut |_| false`.
pub fn drain_more_nonblocking(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    interactive_pending: fn() -> bool,
    at_gap: &mut dyn FnMut(usize) -> bool,
) -> usize {
    drain_more_nonblocking_with_idle_wait_after_gap(
        master,
        buf,
        filled,
        wake_rd,
        parse_in_flight,
        interactive_pending,
        idle_poll_us(),
        at_gap,
    )
}

/// Shared gather body, with the idle hysteresis explicit so tests can drive values other
/// than the cached [`idle_poll_us`]. `at_gap(filled)` runs once per dry gap, just before
/// whichever park runs (idle wait or bridge poll); `true` delivers instead. It is also
/// the seam that lands a test's refill provably inside the park.
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
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    interactive_pending: fn() -> bool,
    idle_wait_us: u32,
    mut at_gap: impl FnMut(usize) -> bool,
) -> usize {
    /// One kernel tty output queue's worth — at/above this the writer saturated it.
    const SATURATED: usize = 1024;
    /// Immediate re-read budget per refill gap (ghostty's bridge length; 8-16
    /// catches >90% of gaps).
    const NB_SPIN_MAX: u32 = 16;
    /// One parked poll per dry gap; quiet for the full timeout ⇒ burst over.
    const BRIDGE_POLL_MS: i32 = 1;
    /// Per-batch latency budget (ghostty's); checked per dry gap, so the
    /// effective bound is this plus one `BRIDGE_POLL_MS` park.
    const BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);
    let start = std::time::Instant::now();
    let mut spins = 0u32;
    while filled < buf.len() {
        let n = read(master, &mut buf[filled..]);
        if n > 0 {
            filled += n as usize;
            spins = 0; // fresh probe budget per refill gap
            continue;
        }
        if n == 0 {
            break; // EOF: keep what we have; the next read_or_wake reports it
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                // A human waiting on the UI thread? Sampled per dry gap (one
                // atomic + one clock read), never per byte.
                let hot = interactive_pending();
                // Quiet. Under one outq delivers now; an over-budget batch delivers
                // regardless, and the budget is the short one while a key is pending.
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
                // The µs-scale bridge: the parser-idle hysteresis and, while a key is
                // pending, the only park. With the immediate probes it still coalesces
                // a TUI repaint's µs refill gaps without holding an echo for 1 ms.
                if hot
                    || parse_in_flight
                        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
                {
                    if idle_wait_us == 0 {
                        break; // knob off: bridging would add pure output latency
                    }
                    // A µs-bounded wait for the refill instead of delivering a
                    // churn-sized batch; the budget is re-checked per dry gap.
                    if at_gap(filled) {
                        break; // the host's fence: deliver this batch now
                    }
                    match idle_refill_wait(master, wake_rd, idle_wait_us) {
                        BridgeWait::Refill => {
                            spins = 0;
                            continue;
                        }
                        BridgeWait::Deliver => break,
                    }
                }
                if at_gap(filled) {
                    break; // the host's fence: deliver this batch now
                }
                match bridge_poll(master, wake_rd, BRIDGE_POLL_MS) {
                    BridgeWait::Refill => spins = 0,
                    BridgeWait::Deliver => break,
                }
            }
            _ => break, // hard error: the next read_or_wake surfaces it
        }
    }
    filled
}

/// Wakes a reader parked in [`read_or_wake`]: one byte to the wake pipe. Best-effort; a
/// full or closed pipe is harmless.
pub fn wake(wake_wr: i32) {
    let b = [1u8];
    // SAFETY: `wake_wr` is the pipe write end; a 1-byte write is atomic (never partial).
    unsafe {
        libc::write(wake_wr, b.as_ptr() as *const libc::c_void, 1);
    }
}

/// Declares the calling thread `QOS_CLASS_USER_INTERACTIVE` (macOS; a no-op elsewhere),
/// for a worker holding a lock the UI thread contends (the sink's spill drainer). A safe
/// wrapper for `forbid(unsafe_code)` callers.
pub fn declare_interactive_thread() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets this thread's own QoS class; takes no pointers and mutates no
    // shared state, so there is nothing for another thread to observe.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// Close a raw fd (a wake-pipe end). Best-effort; ignores a negative sentinel.
pub fn close_fd(fd: i32) {
    if fd >= 0 {
        // SAFETY: closing an fd the caller owns.
        unsafe {
            libc::close(fd);
        }
    }
}

/// Focus-linked shell QoS boost: a no-op on Unix, where raising another process's
/// priority needs root and macOS has no cross-process QoS API. The Windows
/// implementation is in `src/windows/mod.rs`.
pub fn set_focus_boost(_master: i32, _on: bool) {}

/// Resize the PTY to `rows`×`cols` (`TIOCSWINSZ`) with the pixel fields zero; prefer
/// [`resize_with_cell_px`] when the cell metrics are known.
pub fn resize(master: i32, rows: u16, cols: u16) {
    resize_with_cell_px(master, rows, cols, None);
}

/// [`resize`] with the winsize pixel fields filled from `cell_px` (the host's
/// device-pixel cell size, `(width, height)`). Image tools (`chafa`, `timg`, `viu`,
/// libsixel) size their output from these fields and guess or refuse when they are
/// zero. `None` leaves them zero.
pub fn resize_with_cell_px(master: i32, rows: u16, cols: u16, cell_px: Option<(u16, u16)>) {
    let (ws_xpixel, ws_ypixel) = winsize_pixels(rows, cols, cell_px);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    };
    // SAFETY: `master` is a valid PTY master fd; `&ws` is a valid `winsize` for
    // the `TIOCSWINSZ` ioctl.
    unsafe {
        libc::ioctl(master, libc::TIOCSWINSZ, &ws);
    }
}

/// The winsize pixel pair for a `rows`×`cols` grid of `cell_px` cells, saturating at
/// `u16::MAX` (the kernel fields' width) rather than wrapping. A zero axis in `cell_px`
/// is unknown and yields 0 for both.
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

/// Teardown never signals a recycled pid (see [`ShellIdentity`]): a gone shell's pid,
/// now naming a `setsid`'d `sleep`, leaves the sleep alive; the positive controls hang
/// up, and force-kill, a shell that is still the recorded one.
#[cfg(test)]
#[path = "unix_shell_identity_tests.rs"]
mod shell_identity_tests;
