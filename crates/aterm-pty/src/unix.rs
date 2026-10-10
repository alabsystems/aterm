// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX backend of the PTY seam: every raw `libc` PTY syscall —
//! `posix_openpt`/`fork`/`login_tty` (originally `forkpty`, replaced to close a
//! close-on-exec window — see [`open_pty_pair_cloexec`]), the slave open
//! (`ioctl(TIOCGPTPEER)` on Linux, `open` of the `ptsname` elsewhere — see
//! [`open_pts_slave`]),
//! `execve`, `read`, `write`, `ioctl(TIOCSWINSZ)` — moved VERBATIM from the
//! pre-split `lib.rs` (zero semantic change). The shared, portable items
//! ([`SpawnedShell`], `build_child_env`) live in
//! `lib.rs`; everything in this module is Unix-only by placement (no inline
//! cfg), and `lib.rs` re-exports it so Unix callers compile untouched.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{ShellHangup, ShellIdentity, SpawnedShell, build_child_env};

/// Fixed absolute path to the macOS Seatbelt wrapper used by the OS-sandbox wrap
/// (see [`spawn_shell_with_pid`]'s `sandbox_wrap`). Inlined here (rather than depending on
/// the policy crate) to keep this minimal syscall seam dependency-light; it MUST
/// equal `aterm_containment::SANDBOX_EXEC_PATH` — a test in this crate locks that.
const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// [`spawn_shell_with_pid`] minus the pid, with the hardened default limits.
/// Test-only: every frontend spawns through the `_with_pid` forms.
#[cfg(test)]
// The arg list is intentionally wide: this is the SINGLE spawn seam, and each
// argument is an independent, security-relevant input (caps, env, argv, cwd, the
// OS-sandbox wrap). Bundling them into a struct would hide that surface, not
// shrink it.
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
    // Thin compatibility wrapper: drop the child pid. Callers that need the pid
    // for a graceful, NON-BLOCKING teardown (SIGHUP the controlling-tty session
    // before closing the master — see `spawn_shell_with_pid`) use that instead.
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
        // No cell metrics on this seam (its callers are tests and the protected-spawn
        // gate, which have no font): the winsize pixel fields stay zero, which is the
        // honest answer rather than an invented one.
        None,
    )
    .map(|s| s.master)
}

/// The UTF-8 locale [`resolve_spawn_locale`] INJECTS into spawned children when the
/// inherited locale is non-UTF-8. On macOS `en_US.UTF-8` is guaranteed; OFF macOS use
/// `C.UTF-8` — the locale-INDEPENDENT UTF-8 codeset present on effectively every Linux
/// (glibc 2.35+ and always on musl) and modern BSD. Naming `en_US.UTF-8` off macOS is
/// unsafe: minimal Debian/Ubuntu, virtually all Docker base images, and musl/Alpine do
/// NOT generate it, so glibc silently falls back to C/POSIX (ASCII — the exact mojibake
/// this override exists to prevent) AND every locale-aware child (perl) then prints
/// `Setting locale failed`.
const SPAWN_UTF8_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// Whether a locale string selects a UTF-8 character encoding.
///
/// A POSIX locale is `language[_TERRITORY][.codeset][@modifier]`; the *codeset*
/// (the part after the last `.`, with any trailing `@modifier` stripped) decides
/// the encoding. The match is case-insensitive and ignores `-`, so `.UTF-8`,
/// `.UTF8`, `.utf-8`, `.utf8`, and `.UTF-8@euro` all qualify, while `C`, `POSIX`,
/// a bare `en_US` (no codeset), and `.ISO8859-1` do not.
fn is_utf8_locale(loc: &str) -> bool {
    let Some(dot) = loc.rfind('.') else {
        return false;
    };
    // `dot` is a byte index returned by `rfind('.')`, so `dot < loc.len()` and,
    // `.` being one byte, `dot + 1` is `<= loc.len()` and on a char boundary:
    // the add cannot wrap and the range is always valid, so neither the
    // `saturating_add` clamp nor the `unwrap_or("")` fallback ever fires —
    // behavior-identical while discharging the Trust slice/overflow obligations.
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

/// Resolve the locale overrides aterm must inject so the spawned child always runs
/// under a UTF-8 `LC_CTYPE`.
///
/// `LC_CTYPE` is the POSIX category that decides character encoding; locale-aware
/// programs (emacs, vim, python, tmux, perl, …) consult it to choose whether
/// terminal I/O is UTF-8. aterm's parser is UTF-8-only, so if the child runs under
/// a non-UTF-8 `LC_CTYPE` those programs re-encode multibyte text (e.g. pasted
/// box-drawing `┌─┐`) into the ASCII codeset and emit a literal `?` per character.
/// The terminal must therefore GUARANTEE a UTF-8 `LC_CTYPE` regardless of what
/// locale fragments it inherited.
///
/// `lc_all`/`lc_ctype`/`lang` are the inherited values: `None` = unset; `Some("")`
/// = set-but-empty, which POSIX treats as unset for category resolution (it falls
/// through to the next level). The *effective* encoding category follows POSIX
/// precedence **`LC_ALL` > `LC_CTYPE` > `LANG`**.
///
/// Returns the `(key, value)` pairs to APPEND to `env_add` (applied by
/// [`build_child_env`], which overrides an inherited key or appends a new one):
/// - **EMPTY** when the effective encoding is already UTF-8 — the user's locale is
///   left completely untouched (the common case; keeps every existing spawn test green).
/// - Otherwise `LC_CTYPE=`[`SPAWN_UTF8_LOCALE`] — the minimal override: it fixes only
///   the encoding category and dominates `LANG`. That value is `en_US.UTF-8` on macOS
///   (guaranteed present) and `C.UTF-8` off macOS (the locale-INDEPENDENT UTF-8 codeset
///   present on effectively every Linux/musl/BSD, where `en_US.UTF-8` is NOT). We
///   deliberately do NOT guess a territory locale (e.g. `fr_FR.UTF-8`) that may be
///   absent and would silently fall back to `C`, reintroducing the bug.
/// - …PLUS `LC_ALL=""` when a non-empty `LC_ALL` is the dominating inherited value:
///   `LC_ALL` would otherwise override the injected `LC_CTYPE` (it sits above it in
///   precedence), so we NEUTRALIZE it via POSIX empty-string fall-through. This is
///   surgical — the user's `LANG`/other `LC_*` still drive collation/messages/etc.;
///   only the encoding category is forced to UTF-8.
///
/// Pure in its inputs (like [`build_child_env`]) so it is unit-tested without
/// mutating the process-global environment, and called in the PARENT before
/// the fork, where allocation / env reads are safe. The property "the child's
/// effective `LC_CTYPE` is UTF-8 for every inherited locale shape" is proven by the
/// `SpawnLocale` Tier-0 `ty` model (`aterm_spec::derive::spawn_locale_model`) and
/// bound to this real function by the `spawn_locale_*` conformance tests below.
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
    // A set, non-empty LC_ALL dominates LC_CTYPE; here it is necessarily non-UTF-8
    // (else `effective` above would have been UTF-8 and we'd have returned). Empty
    // it so POSIX falls through to the LC_CTYPE we just injected.
    if set(lc_all).is_some() {
        overrides.push(("LC_ALL".to_string(), String::new()));
    }
    overrides
}

/// Size of the buffer a PTY slave's device path is fetched into. macOS's own
/// `openpty` calls `ptsname_r(master, buf, 128)` and `TIOCPTYGNAME` is an
/// `_IOC_OUT` ioctl with a FIXED 128-byte payload, so 128 is the size the
/// platform itself assumes; Linux's `/dev/pts/N` names are far shorter. Both
/// primitives report a name that would not fit as an error rather than
/// truncating, so this can never yield a half path.
const PTS_NAME_LEN: usize = 128;

/// The slave device path for `master` — deliberately NOT via `ptsname`.
///
/// `ptsname` returns a pointer into storage it owns, and `man 3 posix_openpt`
/// states plainly: "The `ptsname()` function is not guaranteed to be reentrant
/// or thread safe." aterm spawns sessions from more than one thread (the GUI
/// thread and the update-handoff worker), so this seam uses the reentrant
/// primitive each platform actually provides:
///
///  * Darwin — `ioctl(TIOCPTYGNAME)`, the exact ioctl `ptsname_r` wraps there.
///    macOS *has* `ptsname_r` (since 10.13.4), but the `libc` crate does not
///    declare it for apple targets, and an ad-hoc `extern "C"` block would be an
///    unchecked availability claim; the ioctl is already in `libc`.
///  * Everywhere else — `ptsname_r`, which `libc` does declare.
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

/// The error a failed `ptsname_r` (nonzero `rc`) reports. It RETURNS its error
/// number (POSIX.1-2024; glibc, musl, FreeBSD), and musl does not also store it
/// in `errno`, so reading `errno` there would report whatever an earlier call
/// left behind. A -1 is the older set-`errno` convention, read as such.
/// Compiled into every host's tests, Apple's too (which names the slave by
/// ioctl instead), so its test runs on every host.
#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
fn ptsname_r_error(rc: libc::c_int) -> io::Error {
    if rc > 0 {
        io::Error::from_raw_os_error(rc)
    } else {
        io::Error::last_os_error()
    }
}

/// The SLAVE end of `master` — close-on-exec and NOT a controlling terminal from
/// the moment it exists.
///
/// ## Why Linux does not reach it by path
///
/// `ptsname_r` + `open("/dev/pts/N")` is a NAME lookup in the caller's mount
/// namespace, and it is correct only while the devpts instance the master
/// belongs to is the instance currently mounted at `/dev/pts`. glibc's `openpty`
/// — which the `forkpty` this seam replaced called — does NOT depend on that:
/// since glibc 2.27 (login/openpty.c, 2017-10-08) it asks the KERNEL for the
/// peer first, `ioctl(ptmx, TIOCGPTPEER, O_RDWR | O_NOCTTY)` (Linux 4.13+), and
/// consults the path only when that ioctl is unsupported. A path-only slave open
/// is therefore a NARROWING of what already worked on Linux, and it was MEASURED
/// to be one — on Linux 6.8 aarch64 against glibc 2.36 and 2.39:
///
///  * In a STEADY-STATE topology where `/dev/ptmx` resolves into a devpts that is
///    not the one mounted at `/dev/pts` (a second devpts instance, the shape a
///    container/chroot with its own `/dev` produces; nothing perturbed after the
///    master was opened), `openpty` SUCCEEDS while the by-path open fails
///    `ENOENT` — the session simply would not spawn where it used to.
///  * When the instance at `/dev/pts` happens to hold a pty at the SAME index,
///    the by-path open SUCCEEDS onto a DIFFERENT terminal: measured, a byte
///    written into that slave never arrives on this master. A silent
///    cross-terminal misconnection, in the very seam that exists to stop
///    terminals from crossing.
///  * The same divergence appears whenever the mount topology moves under a live
///    master (`/dev/pts` unmounted, over-mounted, replaced by a fresh instance,
///    or left behind by a `chroot`): by-path fails or mispoints, `TIOCGPTPEER`
///    returns the right peer every time.
///
/// `TIOCGPTPEER` has none of those failure modes and is strictly better than the
/// path on every axis this seam cares about: one syscall, no name lookup, no
/// namespace dependency, and it returns the peer OF THIS MASTER by construction
/// rather than by name. Its argument IS the `open(2)` flag word, so the
/// descriptor is born `O_CLOEXEC` — the property [`open_pty_pair_cloexec`] exists
/// to guarantee — with no window at all. MEASURED on Linux 6.8: the returned fd
/// has `FD_CLOEXEC` set, and like `open(pts, O_NOCTTY)` it does NOT become this
/// process's controlling terminal (with the `O_NOCTTY`-dropped control adopting
/// one, so the observation is not vacuous).
///
/// The path stays as the FALLBACK, taken on any refusal — pre-4.13 kernels
/// answer `EINVAL`/`ENOTTY` — which is the same two-step glibc itself performs.
/// Non-Linux platforms take the path unconditionally and unchanged: Darwin has no
/// `TIOCGPTPEER` and its own `openpty` is path-based, so nothing there moves.
///
/// # Errors
/// The OS error from the name lookup or from the `open`, naming which
/// ([`pty_step_failed`]). No descriptor escapes on any error path — the only fd
/// this can produce is the one it returns.
fn open_pts_slave(master: libc::c_int) -> io::Result<libc::c_int> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `master` is a live `/dev/ptmx` descriptor. `TIOCGPTPEER` takes
        // its argument BY VALUE — an `open(2)` flag word, not a pointer — and
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
        // Any refusal falls through to the path, exactly as glibc's `openpty`
        // does with the same failure: a pre-4.13 kernel that never heard of the
        // ioctl is the case this fallback is FOR, and a newer kernel that
        // declines is no worse off than the path-only code this replaces.
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

/// Confirm `fd` is close-on-exec, setting the flag if the OPEN did not.
///
/// This is a VERIFICATION with a fallback, never the primary mechanism. The only
/// window-free way to get a close-on-exec descriptor is `O_CLOEXEC` at the
/// `open(2)` that creates it — a `fcntl` afterwards leaves a gap in which a fork
/// on another thread yields an exec-surviving copy, which is the entire defect
/// [`open_pty_pair_cloexec`] exists to remove. But POSIX does not *require*
/// `posix_openpt` to honor `O_CLOEXEC` (it is undocumented-but-real on Darwin —
/// `posix_openpt` there is a bare `open("/dev/ptmx", oflag)`), and aterm ships
/// back to macOS 11, so the flag is read back and only repaired if absent.
/// Taking the fallback re-opens the window, narrowed to the handful of
/// instructions between the `open` and this `fcntl` — the narrowest correct
/// behavior available on a platform that ignores the flag, and still strictly
/// better than the `forkpty` this replaces, which never set the flag on the
/// slave at all and set it on the master only AFTER the fork.
/// `pty_open_honors_the_cloexec_open_flag` asserts the fallback is NOT what runs here.
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

/// A step of opening a PTY pair failed: WHICH step, and the OS error it raised,
/// verbatim. It rides as the payload of the `io::Error` the opener returns, with
/// the OS error's own `kind`, so the message names the step and the true errno
/// stays reachable through `source()`.
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

/// XNU's kernel-private "redrive open" pseudo-error, `EREDRIVEOPEN`: `(-6)` in
/// xnu's `bsd/sys/errno.h`, under `KERNEL_PRIVATE`, so in no userspace header.
///
/// `posix_openpt` is `open("/dev/ptmx")`, a devfs CLONE open. `ptmx_clone`
/// (`bsd/kern/tty_ptmx.c`) hands out the first free minor under the devfs lock
/// but does not claim it; the claim happens later, in `ptcopen` →
/// `ptmx_get_ioctl`, after an unlocked allocation. Concurrent openers can
/// therefore be handed the SAME minor; every one but the first finds the slot
/// taken and `ptcopen` (`bsd/kern/tty_dev.c`) answers `EREDRIVEOPEN`.
/// `vn_open_auth` (`bsd/vfs/vfs_vnops.c`) redrives the whole open on it, but
/// only `max_retries = 10` times (sleeping 60–100 ms on the last five), and then
/// hands the pseudo-error to userspace verbatim, where it lands in `errno`.
///
/// It is neither a limit nor a fault of this opener: MEASURED (Darwin 25.6,
/// 2026-10-01) with ≤128 ptys held against `kern.tty.ptmx_max` 511 and a
/// 1 048 576-descriptor limit, 128 threads each opening and closing 200 pairs
/// through the former opener saw 196 of 25 600 opens fail with exactly
/// `Os { code: -6 }` (bare `posix_openpt`: 23 at 128 threads, 4 at 64, 0 at
/// 16) — the error two concurrent aterm-gui test suites hit on 2026-10-01.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const XNU_EREDRIVEOPEN: libc::c_int = -6;

/// How many times [`open_ptmx`] redrives an open the kernel gave up on with
/// [`XNU_EREDRIVEOPEN`]. Each redrive is a fresh kernel open with its own ten
/// internal redrives, so this is 99 attempts in all, and a contender loses only
/// to another opener that WON. MEASURED (same day): redriving bare
/// `posix_openpt` until it opened, 256 threads × 200 opens needed one redrive
/// 185 times and never two; 400 threads × 100 needed two once. This opener,
/// redriving, then failed 0 of 76 800 opens at 128 threads and 0 of 25 600 at
/// 256 (417 failed before it, at 256).
#[cfg(any(target_os = "macos", target_os = "ios"))]
const PTMX_REDRIVES: u32 = 8;

/// The pty MASTER, close-on-exec at the `open(2)` that creates it — step (1) of
/// [`open_pty_pair_cloexec`]. `open` is `posix_openpt` (a parameter so a test
/// can play the kernel's answers, which no test can provoke on demand); it
/// returns a fresh descriptor, or -1 with `errno` set.
///
/// # Errors
/// The OS error `posix_openpt` raised, naming the step: at once for a real
/// failure, which a retry would only repeat — out of ptys (`ENXIO` on macOS at
/// `kern.tty.ptmx_max`, `ENOSPC`/`EAGAIN` on Linux) or out of descriptors
/// (`EMFILE`/`ENFILE`); after [`PTMX_REDRIVES`] redrives for a lost allocation
/// race ([`XNU_EREDRIVEOPEN`]).
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
            // Only EINVAL can mean "this platform rejects the O_CLOEXEC bit"
            // (POSIX spells an unsupported `oflag` bit exactly that way): once
            // more without it, and `fd_is_cloexec_or_make_it` sets the flag.
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

/// Open a PTY pair in which BOTH descriptors are close-on-exec FROM BIRTH, and
/// apply `termp`/`winp` to the slave — `openpty(3)` minus its defect.
///
/// ## Why not `openpty`/`forkpty`
///
/// `openpty` (and therefore `forkpty`, which is `openpty` + `fork` +
/// `login_tty`) opens BOTH ends without `O_CLOEXEC`, VERIFIED at the instruction
/// level on macOS 26.5.1: `posix_openpt(0x20002 = O_RDWR|O_NOCTTY)` → `grantpt`
/// → `unlockpt` → `ptsname_r` → `open(path, 0x20002)`, with the `O_CLOEXEC` bit
/// (`0x1000000`) absent from both flag words. So from the moment `openpty`
/// returns until the parent's `close(slave)` on the far side of the fork, BOTH
/// fds sit in the parent's table with no close-on-exec flag. aterm is
/// multi-threaded and the update-handoff worker calls `Command::spawn` on its
/// own thread while the GUI thread may be spawning a session; a `posix_spawn`
/// landing in that window (MEASURED: 414 inheritance events across 400 real
/// spawns, ~1.5e-3 slave / ~1.1e-2 master per spawn pair) hands an unrelated
/// process a live, WRITABLE descriptor onto another session's terminal — a
/// confirmed cross-session injection and exfiltration channel that bypasses the
/// WriteInput/EdgeToken gate entirely. Setting `FD_CLOEXEC` after the fact does
/// NOT close it: a thread that forked microseconds ago already holds a copy that
/// its own `exec` will preserve. The flag has to be there at birth.
///
/// ## The sequence, and what each step preserves
///
/// 1. `posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC)` — master, flagged by the
///    `open(2)` itself. There is NO window on the master. An open that lost
///    XNU's allocation race is redriven ([`open_ptmx`]).
/// 2. `grantpt` + `unlockpt`. On Darwin these are the single ioctls
///    `TIOCPTYGRANT` / `TIOCPTYUNLK`; unlike historical glibc there is no
///    `pt_chown` helper fork, so they open no window of their own.
/// 3. The SLAVE with `O_RDWR | O_NOCTTY | O_CLOEXEC`, likewise flagged at birth —
///    `ioctl(TIOCGPTPEER)` on Linux, `open(pts, …)` everywhere else and as the
///    fallback ([`open_pts_slave`] carries the measurements for why the path
///    alone is a regression on Linux; both forms take the same flag word, and
///    both honor it). `O_NOCTTY` is LOAD-BEARING and newly so: `openpty` opened
///    the slave inside libutil, but this open happens in ATERM'S OWN PROCESS,
///    and a parent that is a session leader with no controlling terminal (the
///    launchd/Finder `.app` shape aterm ships in) would SILENTLY ADOPT it as its
///    controlling terminal without it (MEASURED: `/dev/tty` goes from unopenable
///    to openable). The child acquires the ctty later and explicitly, via
///    `login_tty`'s `TIOCSCTTY`, which is unaffected by `O_NOCTTY`.
/// 4. `tcsetattr(slave, TCSAFLUSH, termp)` then `ioctl(slave, TIOCSWINSZ, winp)`
///    — the same two calls, in the same order, with the same `TCSAFLUSH`, that
///    `openpty` makes internally. Applying them HERE, in the parent, before any
///    fork, is what preserves the atomicity the seam has always relied on: the
///    child's very first `tcgetattr`/`TIOCGWINSZ` already answers with these
///    values, so there is no post-fork `tcsetattr` race with the shell's own
///    termios reads. MEASURED byte-for-byte identical (`memcmp` of the whole
///    `termios`, plus the `winsize`) to `openpty(&termp, &winp)`, and
///    non-vacuously so — a kernel-default slave differs. Like `openpty`, both
///    are BEST-EFFORT: `openpty` discards their return values, and failing the
///    spawn on a `tcsetattr` that cannot fail on a freshly-opened pts would be a
///    new error path, not a preserved one.
///
/// Both fds are close-on-exec before this returns, VERIFIED by reading the flag
/// back rather than assumed (see [`fd_is_cloexec_or_make_it`]). The master stays
/// an ordinary, flag-mutable descriptor whose number is stable — `set_cloexec`
/// must be able to CLEAR the flag so the master survives the seamless-update
/// re-exec, and that fd number rides `ATERM_SEAMLESS_FDS` across the handoff.
///
/// # Errors
/// The OS error from whichever step failed, naming that step
/// ([`pty_step_failed`]). FAIL-CLOSED on every path: no descriptor and no
/// half-built pair escapes — the master is closed before returning any error
/// raised after it was opened.
fn open_pty_pair_cloexec(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
) -> io::Result<(libc::c_int, libc::c_int)> {
    // (1) MASTER — close-on-exec at the `open(2)` that creates it.
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

    // (3) SLAVE — close-on-exec at birth, and NOT this process's ctty.
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

    // (4) termios + winsize on the SLAVE, in the parent, before any fork —
    //     exactly the two calls `openpty` makes, and best-effort exactly as it
    //     makes them (it discards both return values).
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

/// A bare PTY pair `(master, slave)`, BOTH ends close-on-exec from birth
/// ([`open_pty_pair_cloexec`], with no termios or winsize applied) — for a
/// caller that needs a pair and no child, such as the seamless handoff's test
/// fixture. `openpty(3)` followed by `fcntl(FD_CLOEXEC)` leaves a window in
/// which a fork on any other thread takes an exec-surviving copy of the
/// master; that copy keeps the master open after its owner closes it, which is
/// exactly what a test reading the slave's EOF as "the master is closed"
/// cannot tell from a leak.
///
/// # Errors
/// The OS error from whichever step failed; no descriptor escapes on error.
pub fn open_pty_pair() -> io::Result<(i32, i32)> {
    open_pty_pair_cloexec(None, None)
}

/// The termios a NULL-termios `openpty`/`forkpty` gives the slave — the kernel's
/// compiled-in defaults — probed ONCE per process via a throwaway PTY pair and
/// cached. Probing (instead of hardcoding `TTYDEF_*`) guarantees "identical to
/// the NULL path except our documented deltas" by construction. `None` when the
/// probe fails; the spawn then passes NULL exactly as before.
///
/// The throwaway pair comes from [`open_pty_pair_cloexec`], not `openpty`, for
/// the same reason the real spawn does: `openpty` would put two NON-close-on-exec
/// descriptors in this multi-threaded process for the duration of the probe. The
/// window is narrower here (both fds are closed inside this function, once per
/// process) but it is the SAME defect, and leaving one live counter-example in
/// the file that fixes it would be an oversight, not a simplification.
fn kernel_default_termios() -> Option<libc::termios> {
    static DEFAULTS: std::sync::OnceLock<Option<libc::termios>> = std::sync::OnceLock::new();
    *DEFAULTS.get_or_init(|| {
        // `None`/`None` is the "kernel defaults" form: open the pair and change
        // nothing about the slave, so `tcgetattr` reports what the kernel chose.
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

/// Build the slave termios applied at slave-open time: the probed kernel defaults with
/// exactly two deltas (everything else — cc chars, lflags, iflags, oflags — is
/// bit-identical):
///
///  * `IUTF8` ON. The kernel default leaves it off (probed macOS iflag
///    `0x2b02`), so canonical-mode ERASE deletes one BYTE of a multibyte UTF-8
///    char. We already force a UTF-8 locale into the child (`SPAWN_UTF8_LOCALE`);
///    the tty line discipline should agree. Terminal.app/iTerm2/kitty all set it.
///  * ispeed/ospeed `B230400` instead of the default `B9600`. PTY speed is
///    nominal (no UART), but xnu's `ttsetwater` sizes the slave output-queue
///    high-water from ospeed. MEASURED on this M5 Max (2026-07-19 probe): the
///    master-read chunk stays hard-clamped at exactly 1024 B/read at ospeeds
///    9600/38400/230400/1e6 (32 MiB → 32768 reads each), so this is expected
///    ~neutral here — kept because it is free, matches other emulators, and the
///    clamp may differ on other xnu/Linux versions.
///
/// No env knob reverts either delta: an env read in the shipped crate would let
/// an exported value put a release on a termios no test covers. Measure by
/// editing this function in a scratch build. (Clearing `OPOST` gains ~6% raw
/// throughput but breaks cooked-mode output: bare `\n` stairsteps.)
fn build_spawn_termios(mut t: libc::termios) -> libc::termios {
    t.c_iflag |= libc::IUTF8;
    // SAFETY: `cfsetspeed` only writes the speed fields (and, on Linux, CBAUD
    // bits) of the valid, stack-owned `t`.
    unsafe {
        libc::cfsetspeed(&mut t, libc::B230400);
    }
    t
}

/// The termios applied to the spawn's slave — `None` (the kernel defaults)
/// only when the probe itself fails. Runs in the
/// PARENT before the fork (the one-time probe allocates; the post-fork child
/// stays async-signal-safe and untouched).
fn spawn_termios() -> Option<libc::termios> {
    kernel_default_termios().map(build_spawn_termios)
}

// ===========================================================================
// THE EXEC-STATUS CHANNEL
//
// `spawn_shell_with_pid` needs one bit back from its child across the fork:
// "did you reach `execve`, or did you fail before it?". The protocol is one
// byte plus EOF, and it is unchanged by everything below:
//
//   child writes 1 byte then `_exit`  ->  FAILED before exec (b'S' / b'E')
//   `execve` succeeds, O_CLOEXEC closes the write end  ->  parent reads EOF
//
// ## Why not `pipe(2)` + `fcntl(FD_CLOEXEC)`
//
// `pipe(2)` returns two descriptors with NO
// close-on-exec flag (MEASURED on macOS 26.5.1: `F_GETFD` = 0x0 on both ends the
// instant `pipe` returns), so between the `pipe` and the second `fcntl` BOTH ends
// sit unflagged in a MULTI-THREADED process — the defect
// [`open_pty_pair_cloexec`] removes on the pty pair.
//
// aterm reaches it from its OWN code: the update-handoff worker calls
// `Command::spawn` on its own thread (aterm-gui `app_update_handoff.rs`) while
// the GUI thread opens sessions, and `Command::spawn` bottoms out in
// `posix_spawn(2)` — ONE syscall, which libSystem does NOT serialise against
// anything (MEASURED: a registered `pthread_atfork` prepare handler runs 1 time
// for `fork()` and 0 times for `posix_spawn()`). So no application-level lock can
// ever cover the window, and an unrelated process ends up holding a copy of
// `status_wr`.
//
// The harm is worse than it first looks, and it is TWO distinct bugs:
//
//  1. THE HANG (success path only). A stranger holding a copy of the write end
//     keeps it open, so EOF NEVER ARRIVES until the stranger exits, and the
//     parent's blocking `read` waits exactly that long. MEASURED against this
//     very function: 6 write-end inherits in 1500 spawns run against a 3-thread
//     `Command::spawn` storm, and EXACTLY 6 calls blocked, each for 20.005-20.238 s
//     against a stranger told to live 20 s — a 1:1 correlation, with the block
//     ending at the instant the stranger died. In production the stranger is the
//     SUCCESSOR ATERM PROCESS the update handoff spawns, which lives for the rest
//     of the user's login session; the thread opening the session is simply gone.
//  2. SILENT CORRUPTION OF A GOOD SPAWN (and NO timeout can fix this one). A
//     stranger that WRITES into the inherited write end makes the parent's read
//     return n=1 from a child that exec'd PERFECTLY — MEASURED 3/3, ~1-2 ms. The
//     parent then takes the failure branch, closes the master, kills a healthy
//     shell and reports "child failed to exec". Only closing the window prevents
//     this, which is why a bounded read is defence in depth here and NOT the fix.
//
// A pre-exec FAILURE is unaffected by a stranger either way: the byte is in the
// channel's BUFFER and `read` returns it regardless of who else holds a write end
// (MEASURED with the stranger verified still alive at unblock — `waitpid(WNOHANG)
// == 0` — read n=1 in 1-2 ms). That asymmetry is what makes a bounded wait sound:
// a bound can never convert a real pre-exec failure into a false success.
//
// ## What is actually available on Darwin
//
// The entire atomic-close-on-exec surface on this platform is THREE constants:
// `O_CLOEXEC`, `F_DUPFD_CLOEXEC` and `FD_CLOEXEC`. Verified absent, each three
// independent ways (header grep, `dlsym`, and a link attempt): `pipe2`,
// `accept4`, `dup3`, `SOCK_CLOEXEC` (`socketpair` with the Linux bit fails
// `EPROTONOSUPPORT`), and `MSG_CMSG_CLOEXEC` (an SCM_RIGHTS broker just relocates
// the window into the receiver — the received fds arrive `F_GETFD` = 0x0).
// `F_DUPFD_CLOEXEC` produces a correctly-flagged COPY but cannot help: `pipe(2)`
// already ran, so the ORIGINALS were unflagged for the whole duration.
//
// So on Darwin only `open(2)` can create an already-flagged descriptor, which
// means a window-free carrier must have a PATHNAME. Hence the FIFO.
//
// ## Rejected, with the measurement that rejected it
//
//  * A PTY PAIR as the carrier (atomic, no filesystem, and it would reuse
//    [`open_pty_pair_cloexec`] which is already in this file and already
//    audited). It is an ATTRACTIVE TRAP and a naive test PASSES: Darwin's
//    `read(master)` really does return 0/EOF after the last slave closes. But the
//    tty OUTPUT QUEUE IS FLUSHED at last-slave-close, so if the parent does not
//    reach its `read` before the child fully exits — the ORDINARY case under load —
//    the failure byte is GONE and `read` returns 0. MEASURED 15/15 deterministic,
//    with pipe and fifo both retaining the byte in the same harness. Shipping it
//    would turn a sandbox-confinement failure into the EOF SUCCESS verdict: a
//    fail-OPEN on the one path this seam fails closed on. If anyone re-proposes a
//    pty carrier, that is the test to run first.
//  * A POOL of pre-created pipes. A pipe is single-use (after EOF it re-reads
//    n=0 forever, and an anonymous pipe has no name to reopen a write end
//    through), and a RETAINED template write end suppresses EOF exactly like a
//    stranger does — MEASURED: `select` on the read end times out at 300 ms while
//    the template is held, and EOF arrives the instant it is closed. So a pool is
//    a finite stock whose refill reopens the window; it would bound the damage
//    invisibly and ship as a false fix.
//  * A SPAWN LOCK. Provably cannot cover `posix_spawn` (the `pthread_atfork`
//    measurement above), so it cannot cover `Command::spawn` or any third-party
//    spawn.
// ===========================================================================

/// How long the parent waits for the child's exec-status verdict before giving
/// up and FAILING CLOSED.
///
/// This is DEFENCE IN DEPTH, not the fix — [`open_exec_status_channel`] closes
/// the window that made an unbounded wait reachable. The bound exists so that a
/// FUTURE leak (a new unflagged descriptor somewhere, a platform that drops to
/// [`ExecStatusCarrier::RacyPipe`], a child stopped by a debugger between fork
/// and exec) degrades to a slow, visible, recoverable failure instead of the
/// unbounded freeze measured above.
///
/// TEN SECONDS, against measurements of the legitimate path:
///  * fork -> EOF on a successful spawn: p50 2.99 ms, p99 6.0 ms, max 12.7 ms
///    over n=5000 at ordinary load.
///  * the whole `spawn_shell_with_pid` call: p50 3.2 ms, p99 5.3 ms.
///  * under a DELIBERATELY pathological load (loadavg 118 on 18 cores, spawner
///    at `nice -n 20`), n=20000: p50 4.5 ms, p99 206 ms, MAX 523 ms. That 523 ms
///    is the slowest legitimate success reproducible on this machine.
///  * the FAILURE paths are consistently FASTER (they skip `execve`): b'E' max
///    182 ms, b'S' max 405 ms under the same pathological load.
///  * the exec TARGET does not move the number — EOF fires at the kernel image
///    switch, before dyld runs the new image: /bin/zsh, 40 freshly-copied
///    binaries with cold code-signature validation, and a quarantined binary all
///    land at p50 ~2.9-3.0 ms.
///
/// So the margin is 19x over the slowest legitimate outcome ever observed and
/// ~3000x over the median, while the measured HANGS were 5 s, 20 s and 65 s
/// (whatever the stranger's lifetime happened to be). Margin is deliberately
/// generous rather than tight because two legitimate slow paths could NOT be
/// measured here: an exec target on a stalled network filesystem (the wait
/// includes the kernel image load) and severe swap thrash.
const EXEC_STATUS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// The longest single sleep [`wait_for_exec_status`] will take inside its budget.
///
/// LOAD-BEARING, and not a tuning knob. All the CORRECTNESS of the wait lives in
/// the non-blocking `read` at the top of each iteration; the readiness primitive
/// is only there to avoid busy-waiting. Capping each wait slice means that even a
/// readiness primitive that never fires at all degrades the wait to a 100 ms-granular
/// poll — a LATENCY bug, never a hang and never a wrong verdict.
///
/// That is not hypothetical on this platform. MEASURED on macOS 26.5.1 against a
/// FIFO read end whose last writer closes: `poll()` returns n=0 with revents 0x0
/// and `kqueue`/`EVFILT_READ` returns n=0 — NEITHER EVER REPORTS EOF, even at a
/// 2 s timeout — while `read()` on the same descriptor correctly returns 0. Only
/// `select()` is right (it woke at 100.2 ms on a delayed EOF). The same three
/// mechanisms are all correct on a PIPE, which is exactly why this is a trap: a
/// maintainer who "modernises" the `select` below into a `poll` would see every
/// pipe-based test still pass.
///
/// The blast radius of that mistake is EXACTLY one slice, and that is this
/// constant's whole job. VERIFIED by mutation: replacing the `select` with a
/// `poll` leaves every correctness test green and costs up to 100 ms of latency
/// per successful spawn — a real regression on every session open, but a
/// LATENCY one, not the 10 s hang the same mistake would cause if the wait
/// slept for its whole remaining budget in one call.
/// The Darwin-only `darwin_fifo_eof_is_invisible_to_poll_and_kqueue_but_not_select`
/// test pins both the platform fact and [`wait_readable_briefly`]'s own behaviour
/// on it, so that mutation does not pass silently. (Named in prose, not linked:
/// it is `cfg(target_os = "macos")`, and an intra-doc link to it would dangle in
/// a Linux docs build.)
const EXEC_STATUS_SLICE: std::time::Duration = std::time::Duration::from_millis(100);

/// Which mechanism produced the exec-status channel — recorded so a fallback to
/// the racy carrier is OBSERVABLE rather than a silent security regression.
///
/// The two atomic variants are `cfg`-gated to the platform that can produce
/// them, so a build in which the atomic route was silently compiled out fails
/// to NAME it rather than carrying a variant nothing constructs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecStatusCarrier {
    /// Linux `pipe2(fds, O_CLOEXEC)`: ONE syscall, both ends flagged by the call
    /// that creates them. No window, no filesystem, no cost.
    #[cfg(target_os = "linux")]
    Pipe2,
    /// Darwin: a FIFO created with `mkfifo(2)` and opened twice with `O_CLOEXEC`,
    /// then immediately unlinked. Both descriptors are flagged BY `open(2)`
    /// ITSELF, so there is no instant at which either exists unflagged.
    #[cfg(target_os = "macos")]
    Fifo,
    /// `pipe(2)` + two `fcntl(F_SETFD)` — the ORIGINAL, RACY carrier, kept ONLY
    /// as a last resort for a platform with neither of the above or a machine
    /// where the atomic route failed (a read-only or full temp filesystem, an
    /// exhausted inode table, a sandbox profile that denies `mkfifo`). It
    /// reintroduces the window, so taking it is counted and announced, and
    /// [`EXEC_STATUS_BUDGET`] is what stops it becoming an unbounded hang.
    RacyPipe,
}

/// How many times this process fell back to [`ExecStatusCarrier::RacyPipe`].
///
/// The honest answer to "what should the fallback do?": refusing to spawn would
/// turn a temp-directory problem into an inability to open a terminal at all,
/// while falling back SILENTLY would reintroduce the defect on exactly the
/// machines where it cannot be seen. So it falls back, bounded, and says so.
static RACY_EXEC_STATUS_CARRIERS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Record — and, the first time only, announce — a fallback to the racy carrier.
fn note_racy_exec_status_carrier(why: &io::Error) {
    use std::sync::atomic::Ordering;
    let prior = RACY_EXEC_STATUS_CARRIERS.fetch_add(1, Ordering::Relaxed);
    if prior == 0 {
        // Best-effort: `eprintln!` PANICS when stderr is closed or its reader is gone
        // (EPIPE/EIO), and this runs inside the GUI process where that panic would
        // abort a spawn.
        {
            use std::io::Write as _;
            // The folder is the one thing a person can fix (a missing or read-only
            // $TMPDIR): on macOS it is where the private FIFO was refused — unless the
            // refusal was the descriptor limit, which no folder explains.
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

/// Confirm `fd` really is close-on-exec, WITHOUT the repairing fallback
/// [`fd_is_cloexec_or_make_it`] offers.
///
/// The pty pair tolerates a repair because `posix_openpt` is not REQUIRED by
/// POSIX to honor `O_CLOEXEC`. Here the flag came from `pipe2`/`open(2)`, both of
/// which are specified to honor it, so a missing flag means the atomic property
/// this carrier exists for does not hold — and the answer is to fall back to the
/// carrier that at least announces itself, not to paper over it with an `fcntl`.
fn fd_is_cloexec(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is a descriptor the caller just opened and still owns;
    // `F_GETFD` only reads the descriptor-flag word.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags != -1 && flags & libc::FD_CLOEXEC != 0
}

/// Put `fd` in non-blocking mode.
///
/// Applied to the READ end of every carrier, because [`wait_for_exec_status`]
/// derives its whole verdict from a non-blocking `read` (the one primitive that
/// is correct on every carrier and platform combination measured). `O_NONBLOCK`
/// is a status flag on THIS file description — the parent's own, created above
/// and not yet shared with anyone — so setting it races with nothing, and it does
/// not touch the WRITE end's description, which the child uses blocking exactly
/// as before.
fn fd_set_nonblocking(fd: libc::c_int) -> bool {
    // SAFETY: `fd` is a descriptor the caller just opened and still owns;
    // `F_GETFL`/`F_SETFL` only read and write the file-status flag word.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags != -1 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    }
}

/// The directory the per-spawn status FIFO is created in, resolved ONCE.
///
/// ## Why NOT `confstr(_CS_DARWIN_USER_TEMP_DIR)`
///
/// That is the obvious choice — it asks the OS instead of trusting `$TMPDIR`,
/// and it answers `/var/folders/…/T/`, already mode 0700 owned by the invoking
/// uid. This code called it, and it was WRONG in a way worth recording, because
/// nothing about the API suggests it:
///
/// `confstr(_CS_DARWIN_USER_TEMP_DIR)` goes through libsystem_notify, whose
/// shared region is lazily built under an `os_alloc_once` gate. `fork(2)` runs
/// libSystem's own `pthread_atfork` CHILD handlers before returning, one of which
/// is `notify_fork_child`, and it takes that same gate. If any OTHER thread was
/// inside that once-gate when a thread forked, the child finds it held by a
/// thread that does not exist in the child, and libplatform ABORTS:
///
/// ```text
///   BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt
///   libsystem_c.dylib: crashed on child side of fork pre-exec
///   _os_once_gate_corruption_abort <- _os_once <- _os_alloc_once
///     <- notify_fork_child <- libSystem_atfork_child <- fork <- forkpty
/// ```
///
/// The child is killed with **SIGKILL** — inside `fork()` itself, before a single
/// instruction of our post-fork branch runs, so nothing in the child branch can
/// prevent or even observe it. MEASURED, and not as a subtlety: with unmodified
/// production code and a test that does nothing but call
/// `confstr(_CS_DARWIN_USER_TEMP_DIR)` on six threads, a concurrent `forkpty`
/// child died 16 times in 25 runs. In aterm that is a session's shell being
/// SIGKILLed at random because an unrelated thread asked where the temp
/// directory is. It is the same class of hazard as everything else in this file:
/// a multi-threaded process forking is allowed to touch almost nothing.
///
/// The spawn below now passes that gate itself before it forks
/// (`aterm_uds::spawnfd::InheritStrip::prepare`, as every fork that strips
/// does), so a first libnotify call on another thread can no longer kill the
/// shell. This lookup still must not be that first call: a fork elsewhere in
/// the process need not have passed it, and the other child handlers keep
/// once-gates of their own.
///
/// ## What is used instead
///
/// `$TMPDIR`, read with `getenv`, else `/tmp` ([`fifo_dir_in`]). No notify, no
/// XPC, no lazily-initialised gate, so it cannot arm the abort above. NOT
/// `std::env::temp_dir()`: with `$TMPDIR` unset, std on Apple falls back to the
/// very `confstr` above — MEASURED, a session started without `$TMPDIR` made
/// its first libnotify call from there
/// (`temp_dir` -> `confstr` -> `_dirhelper` -> `mbr_uid_to_uuid` ->
/// `notify_register_check`). Resolved ONCE into a `OnceLock` so even the
/// environment read happens a single time, off the per-spawn path.
///
/// ## Trusting `$TMPDIR`
///
/// This does trust an environment variable, which `confstr` did not. It is
/// nonetheless the right trade, because a redirected `TMPDIR` cannot get an
/// attacker anything: [`open_exec_status_fifo`] creates the node with `mkfifo`
/// (which FAILS `EEXIST` rather than opening something already there), names it
/// unpredictably, opens both ends `O_NOFOLLOW`, and then VERIFIES the result is
/// one fifo, owned by our own euid, mode 0600, with both descriptors on the same
/// `st_dev`/`st_ino` ([`fifo_ends_are_one_private_object`]). A hostile directory
/// therefore yields a failed `mkfifo` or a rejected verification — a DoS that
/// degrades to the announced racy carrier — never a channel someone else can
/// read or write. And anyone who can set aterm's `TMPDIR` already controls the
/// environment aterm launches with, which is a strictly larger capability than
/// this. Against a SIGKILLed shell, that is not a close call.
#[cfg(target_os = "macos")]
fn exec_status_fifo_dir() -> &'static [u8] {
    static DIR: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| fifo_dir_in(std::env::var_os("TMPDIR")))
}

/// The status FIFO's directory for a `$TMPDIR` of `tmpdir`, ending in one `/`:
/// that directory, or `/tmp` when it is unset — never a lookup that asks the OS
/// ([`exec_status_fifo_dir`] says why).
#[cfg(target_os = "macos")]
fn fifo_dir_in(tmpdir: Option<std::ffi::OsString>) -> Vec<u8> {
    let mut dir = tmpdir.map_or_else(|| b"/tmp".to_vec(), |d| d.as_bytes().to_vec());
    if dir.last() != Some(&b'/') {
        dir.push(b'/');
    }
    dir
}

/// A fresh, unique, unguessable path for one spawn's FIFO.
///
/// UNIQUE PER SPAWN IS MANDATORY, not hygiene: two reader/writer pairs opened on
/// the SAME fifo path share ONE kernel object — MEASURED, pair #2 wrote a byte
/// and pair #1's READER received it. A single long-lived fifo path would make
/// concurrent session opens read each other's exec verdicts.
///
/// Uniqueness and unpredictability come from different sources on purpose: the
/// process-wide counter guarantees no two spawns IN THIS PROCESS can collide even
/// if the entropy source repeated itself, and `getentropy` makes the name
/// unguessable to anything outside it. If `getentropy` somehow fails, the clock
/// stands in — a predictable name is a much smaller problem than no channel at
/// all, and the counter still guarantees correctness.
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

/// Both fifo descriptors name ONE object, and it is the private fifo we made.
///
/// This is the TOCTOU check for the only interval that matters: the two `open`
/// calls are separate syscalls, so in principle the name could be replaced
/// between them. Comparing `st_dev`/`st_ino` proves the read end and the write
/// end are the SAME kernel object — which is the property the protocol needs and
/// the one substitution would break — and `S_ISFIFO` plus the owning uid proves
/// it is ours. `O_NOFOLLOW` on both opens already refuses a symlink at the final
/// component (MEASURED: accepted on a fifo open, and `O_CLOEXEC` still sticks
/// alongside it), and the containing directory is 0700, so this is the third
/// layer rather than the first.
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

/// Build the Darwin carrier: a FIFO whose two ends are close-on-exec FROM BIRTH.
///
/// ORDER IS LOAD-BEARING at every step, and each of these is a silent correctness
/// bug rather than a compile error if reversed:
///
///  1. `mkfifo(path, 0600)` — the name now exists, so EVERY exit path below must
///     `unlink` it.
///  2. `chmod(path, 0600)` — `mkfifo`'s mode is masked by the process `umask`, so
///     a hostile umask could produce a fifo WE cannot open (permission bits are
///     enforced against the owner too). `chmod` is not masked. Without this, a
///     weird umask would silently demote every spawn to the racy carrier. Best
///     effort: if it fails the opens below may still succeed.
///  3. READ END FIRST, and with `O_NONBLOCK`. `open(O_RDONLY)` on a fifo blocks
///     until a writer appears and `open(O_WRONLY)` blocks until a READER appears,
///     so the writer-first order deadlocks and a blocking read-open deadlocks.
///     `O_NONBLOCK` on `O_RDONLY` returns immediately with no writer present.
///  4. WRITE END SECOND, and it does NOT need `O_NONBLOCK`: the reader is already
///     open, so it returns immediately (MEASURED 13 us) and the child then writes
///     its one status byte with ordinary blocking semantics, exactly as before.
///  5. `unlink` AFTER BOTH OPENS. The kernel object outlives the name (MEASURED:
///     the full fork/exec protocol works after the unlink, and the failure byte
///     survives `waitpid` plus a 50 ms delay), so from here the channel is
///     anonymous — no other process can reach it by name at all.
///
/// COST, measured on this machine: 274 us per channel against 1.2 us for
/// `pipe`+2x`fcntl`. Absolutely that is 0.27 ms on a path whose median is 3.2 ms
/// and which already does `fork(2)`, `execve(2)` of a shell and a full pty setup;
/// it would only matter to something spawning in a tight loop. That figure is a
/// QUIET volume's: `mkfifo` and `unlink` are filesystem metadata operations and
/// cost whatever the volume charges for one. MEASURED 2026-09-24 with dozens of
/// compiler processes writing to the same volume: a channel took p50 ~33 ms
/// (`mkfifo` ~24 ms, `unlink` ~8 ms), and `creat`/`mkdir` in the same directory
/// cost the same, so it is the volume, not the fifo — a slower session open on
/// a busy machine, never a wrong verdict, and still far inside
/// [`EXEC_STATUS_BUDGET`].
///
/// THE RESIDUAL, stated plainly: for the ~100 us (max 318 us measured) between
/// `mkfifo` and `unlink` the fifo has a NAME. A same-uid process that opened that
/// exact path for writing inside that window would hold a write end and delay
/// EOF — the same hang by a different route. That is categorically weaker than
/// what it replaces: the defect being fixed is an ACCIDENT that fires on ordinary
/// concurrent spawning (measured 126 times in 25 seconds), while this needs a
/// same-uid process to GUESS a per-spawn random path inside a 100 us window. It
/// is a trade of an accidental hazard for a much smaller adversarial one, not a
/// reduction to zero — and [`EXEC_STATUS_BUDGET`] bounds even that.
///
/// # Errors
/// The OS error from whichever step failed. FAIL-CLOSED on every path: no
/// descriptor and no filesystem name escapes on any error.
#[cfg(target_os = "macos")]
fn open_exec_status_fifo() -> io::Result<(libc::c_int, libc::c_int)> {
    // A collision means our random name already existed; a couple of retries
    // costs nothing and makes the (astronomically unlikely) case non-fatal.
    let mut last = io::Error::new(
        io::ErrorKind::AlreadyExists,
        "exec-status fifo path collision",
    );
    for _attempt in 0..4 {
        let path = exec_status_fifo_path()?;
        // (1) create the name.
        // SAFETY: `path` is a NUL-terminated path in a 0700 per-user directory;
        // `mkfifo` reads it and creates a fifo, or returns -1.
        if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
            last = io::Error::last_os_error();
            if last.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(last);
        }
        // (2) undo any umask masking. Best-effort by design (see the doc above).
        // SAFETY: `path` is the fifo we just created; `chmod` only sets its mode.
        unsafe {
            libc::chmod(path.as_ptr(), 0o600);
        }
        // (3) READ END — flagged close-on-exec by this `open(2)` itself, and
        //     non-blocking so it does not wait for a writer that does not exist
        //     yet. `O_NOFOLLOW` refuses a symlink swapped in at the final
        //     component.
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
        // (4) WRITE END — likewise flagged by `open(2)`; blocking, and it returns
        //     immediately because the read end is already open.
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
        // (5) the channel is now ANONYMOUS: the object lives on in these two
        //     descriptors and is unreachable by name.
        // SAFETY: removing the name we created above; both fds stay valid.
        unsafe {
            libc::unlink(path.as_ptr());
        }
        // Verify what we were handed rather than assuming it: one private fifo,
        // and both ends really carrying the flag the whole exercise is about.
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

/// Create the exec-status channel, close-on-exec FROM BIRTH wherever the
/// platform can express that, and put its read end in non-blocking mode.
///
/// Returns `(read_end, write_end, carrier)`. Both ends are close-on-exec before
/// this returns; on the atomic carriers (`ExecStatusCarrier::Pipe2` on Linux,
/// `ExecStatusCarrier::Fifo` on Darwin — named in prose rather than linked,
/// since each variant is `cfg`-gated to its own platform and a link would dangle
/// on the other) there is additionally NO INSTANT at which either descriptor
/// existed without the flag, which is the property that actually closes the defect —
/// `fcntl(F_SETFD)` afterwards cannot, because a thread that forked microseconds
/// ago already holds an exec-surviving copy.
///
/// # Errors
/// Only if even the racy fallback cannot be created. FAIL-CLOSED on every path:
/// no descriptor escapes on any error.
fn open_exec_status_channel() -> io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> {
    // The ATOMIC route for this platform. Both arms verify the flag by reading
    // it back rather than trusting the call, because the entire point of the
    // carrier is that flag.
    #[cfg(target_os = "linux")]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a valid 2-element buffer; `pipe2` fills exactly it and
        // applies the flag word to BOTH descriptors as it creates them.
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
    // Any other Unix: no atomic route is known to be available here, so say so
    // once through the same channel a real failure would use, rather than
    // pretending the window is closed.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let atomic: io::Result<(libc::c_int, libc::c_int, ExecStatusCarrier)> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no atomic close-on-exec exec-status carrier is implemented for this platform",
    ));

    let (rd, wr, carrier) = match atomic {
        Ok(chan) => chan,
        Err(why) => {
            // THE RACY FALLBACK — the original carrier, verbatim. `pipe(2)`
            // returns both ends UNFLAGGED, so the window is open from here until
            // the second `fcntl` lands. Kept only because refusing to spawn at
            // all would be a worse failure than a bounded, counted, announced
            // one; `EXEC_STATUS_BUDGET` is what stops it hanging.
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` is a valid 2-element buffer that `pipe` fills.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (rd, wr) = (fds[0], fds[1]);
            // A failure to set FD_CLOEXEC would break the success/failure
            // distinction outright (the write end would survive into the shell
            // and EOF would never come), so it is a hard error, not a warning.
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
            // Announced only once it exists: "shells still start" must not print ahead
            // of a spawn that fails because the fallback failed too (EMFILE/ENFILE).
            note_racy_exec_status_carrier(&why);
            (rd, wr, ExecStatusCarrier::RacyPipe)
        }
    };

    // The parent's whole verdict comes from a non-blocking `read` (see
    // `wait_for_exec_status`), so the read end must be non-blocking on EVERY
    // carrier. The fifo's already is, from its `open`; this makes the others
    // match. Failing here would leave the wait unable to distinguish "not yet"
    // from a verdict, so it fails the spawn rather than guessing.
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
    /// EOF: every write end is gone, so the child reached `execve` and the
    /// kernel closed the close-on-exec write end during the image switch.
    Execed,
    /// The child wrote a reason byte and `_exit`ed BEFORE exec.
    FailedBeforeExec(u8),
    /// [`EXEC_STATUS_BUDGET`] elapsed with neither a byte nor EOF.
    NoVerdict,
    /// The channel itself broke (not `EINTR`, not `EAGAIN`).
    ChannelBroke(io::Error),
}

/// Read a verdict from the status channel if one is available RIGHT NOW.
///
/// `None` means "no verdict yet" and nothing else. `EINTR` is retried in place —
/// a signal is not an answer — which is the EINTR semantics the original
/// blocking read had, preserved.
///
/// An unexpected error is reported as [`ExecStatus::ChannelBroke`] rather than
/// swallowed. The code this replaces broke `read`'s EINTR loop on ANY error and
/// then tested `n > 0`, so an `EBADF`/`EIO` fell into the success branch and
/// handed back a master for a child whose fate was never established — a latent
/// fail-OPEN in a seam whose entire job is to fail closed.
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
            // EAGAIN on a non-blocking read: writers still exist and none has
            // spoken. This is the ONLY "not yet" answer.
            io::ErrorKind::WouldBlock => None,
            _ => Some(ExecStatus::ChannelBroke(err)),
        };
    }
}

/// Sleep until `rd` looks readable, or until `slice` elapses — whichever first.
///
/// PURELY AN OPTIMISATION. The verdict is never taken from here; the caller
/// always re-reads. That framing is deliberate, because the readiness primitives
/// on this platform are not all trustworthy (see [`EXEC_STATUS_SLICE`]):
/// `select(2)` is the only one MEASURED correct on a Darwin FIFO whose last
/// writer closed — `poll(2)` and `kqueue`/`EVFILT_READ` never report that EOF at
/// all, while all three are correct on a pipe.
///
/// `select` is used with an explicit `FD_SETSIZE` guard because `FD_SET` on a
/// descriptor at or above it writes past the `fd_set` bitmap — undefined
/// behaviour, and reachable in a process that raised `RLIMIT_NOFILE` and holds
/// more than `FD_SETSIZE` descriptors. Above the limit this degrades to a plain
/// sleep, which the caller's re-read makes correct, just coarser.
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
/// The verdict comes ENTIRELY from the non-blocking `read` at the top of each
/// iteration — the one primitive measured correct on every carrier and platform
/// here — with [`wait_readable_briefly`] only there to keep the loop from
/// spinning. The deadline is monotonic and recomputed every iteration, so a
/// storm of signals cannot extend the bound the way an `EINTR`-retrying blocking
/// read could.
///
/// On expiry it takes ONE FINAL non-blocking read before answering
/// [`ExecStatus::NoVerdict`], so a byte that landed during the last sleep is
/// never dropped on the floor. That matters because of the asymmetry measured on
/// this protocol: a pre-exec failure byte sits in the channel BUFFER and is
/// readable no matter who else holds a write end (MEASURED n=1 in 1-2 ms with the
/// stranger verified still alive), so a byte is ALWAYS the true answer when one
/// exists, and only EOF can be suppressed by a third party.
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

/// Spawn `$SHELL` in a fresh PTY of `rows`×`cols`, returning the master fd and the
/// child pid (see [`SpawnedShell`]).
/// Defaults to `/bin/sh` when `$SHELL` is unset. (A command to run instead of
/// the shell is `exec_command`, the `-e` flag; no environment variable injects
/// one.)
///
/// `env_add` is a set of `(key, value)` environment entries injected into the
/// child before exec (e.g. the OSC 133/633 shell-integration loader vars +
/// nonce); `argv_override`, when `Some`, replaces the shell's argv (e.g. bash's
/// `--rcfile`). Both are GENERIC — this seam knows nothing about shell
/// integration; the frontend computes them. Pass `&[]` / `None` for a bare
/// interactive shell.
///
/// `exec_command`, when `Some(&[prog, args…])`, runs that command DIRECTLY in the
/// PTY instead of a shell (the `-e` convention: when it exits, the PTY closes and
/// the window follows). `prog` is PATH-resolved HERE in the parent (the child must
/// stay async-signal-safe, so no `execvp` PATH search there); `argv[0]` is `prog`
/// as given. It takes precedence over `argv_override` — there is no interactive
/// shell to integrate with. An unresolved/again-failing `prog` ends
/// the child with `_exit(127)`, closing the window, just like a failed shell exec.
/// The command is `OsString`s because it comes from the OS's argv: each element
/// reaches `execve` as the bytes it was given, so an argument that is not UTF-8
/// (`aterm --window -e prog $'\xff'`) arrives as itself, never as U+FFFD.
///
/// No argv element is ever dropped: one that holds a NUL byte (which `execve`
/// cannot pass) refuses the spawn with `InvalidInput`, naming its list and
/// index, rather than leave it out and shift every argument after it into its
/// place ([`argv_cstrings`]). That holds for `exec_command`, `argv_override` and
/// `shell_args` alike. Nor is the shell ever swapped for another: a
/// `shell_override` holding a NUL refuses the spawn the same way whenever the
/// shell is what would run (an `-e` command does not run it, so is not refused).
///
/// `cwd`, when `Some`, is the working directory the child `chdir`s into before
/// exec (the `--working-directory` flag); it overrides the default
/// `/`→`$HOME` Finder-launch fallback. A failed `chdir` is non-fatal (the child
/// starts in the inherited directory), matching the existing best-effort `chdir`.
///
/// ## OS sandbox wrap (`sandbox_wrap`, macOS Seatbelt — ATERM_DESIGN §5.6)
///
/// `sandbox_wrap`, when `Some(sbpl)`, wraps the WHOLE resolved program+argv in
/// `/usr/bin/sandbox-exec -p <sbpl>` so the macOS kernel Seatbelt applies the SBPL
/// profile (e.g. `(deny network*)` for `Containment` mode) before the target
/// `exec`s. The wrap is BUILT IN THE PARENT: `sandbox-exec` becomes the exec
/// target (a fixed absolute path — no PATH search, async-signal-safe in the child)
/// and the original program+argv become its trailing arguments, so the login-shell
/// argv[0], `--rcfile`, and `-e` paths are all preserved verbatim
/// as what sandbox-exec runs. This is **fail-closed**: if `sandbox-exec` is not
/// present at its fixed path, `spawn_shell` returns an error and does NOT spawn —
/// it never silently runs an UNSANDBOXED shell when the caller demanded the
/// sandbox. `None` means no wrap: the spawn is byte-identical to before (used for
/// every non-`Containment` mode, so the default User-mode spawn is unchanged).
///
/// Spawning a child process is a privileged effect (ATERM_DESIGN WS-G), so it
/// requires a `Cap<Spawn>` of at least `Trusted` tier (`aterm-cap`): there is no
/// way to spawn without one.
///
/// ## Fail-closed confinement (ATERM_DESIGN §5.6, exit-before-exec)
///
/// The child applies the resource sandbox BEFORE `execve`. If the sandbox
/// `apply()` returns an error the child does NOT exec — it writes a one-byte
/// failure indicator on the close-on-exec status pipe and `_exit(126)`s, so a
/// confinement failure can never silently hand back a master fd for an
/// UNCONFINED shell. The parent reads the status pipe: a clean EOF (the write
/// end closed by `execve`'s O_CLOEXEC) means the child exec'd confined; any byte
/// means the child failed before exec, and the parent returns an error instead
/// of the master fd.
///
/// With NO cell metrics, so the child's winsize pixel fields start zero: the
/// metric-free shim over [`spawn_shell_with_pid_cell_px`], which is the one
/// fork/exec body (this adds no behavior of its own). A caller that knows the
/// host's cell size should call the `_cell_px` form so a tool reading
/// `ioctl(TIOCGWINSZ)` gets real pixel geometry instead of zeros.
///
/// # Errors
/// Returns `PermissionDenied` if the capability's tier is too low, `InvalidInput`
/// if an argv element holds a NUL byte, the OS error
/// if the PTY pair / `pipe` / `fork` fails, or `PermissionDenied`/`Other` if the child failed
/// to confine itself (sandbox `apply` error) or to `execve` before exec. On any
/// pre-exec child failure the master fd is closed and NO unconfined shell is
/// returned.
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

/// [`spawn_shell_with_pid`], filling the winsize PIXEL fields from the host's cell
/// metrics. Identical spawn/sandbox/exec behavior.
///
/// SPEC: the parent-prebuild + child branch of this fork/exec seam is the real
/// implementation of the external `ForkExec.tla` model (TRUST_NATIVE_TLA Phase 2,
/// PTY-spawn SAFETY family, WS-G). The spec's ordered child program-counter walk
/// `Fork → Setrlimit → Chdir → CloseMaster → Exec` is exactly the child branch
/// below (the `pid == 0` branch: `Limits::apply` = `Setrlimit`, `chdir`
/// = `Chdir`, `close(master)` = `CloseMaster`, `execve` = `Exec`), and the parent
/// pre-builds `envp`/argv BEFORE the fork (the spec's `envPrebuilt = ~Buggy`) so
/// `OnlySafeBeforeExec` / `MasterClosedBeforeExec` / `SafeImpliesEnvPrebuilt` hold.
/// The `login_tty` that replacing `forkpty` moved into this branch (step 0a) is
/// part of the spec's `Fork` action — "the child branch has been entered", which
/// is what `login_tty` completes — so the walk and the model's 6-action inventory
/// are UNCHANGED by that rewrite, and no binding was added, renamed, or dropped
/// to keep the gate green.
///
/// NO Tier-1 conformance is attached (honest): the modeled trajectory lives in the
/// real CHILD after `fork`, which `execve`s or `_exit`s — it can never be driven
/// in-process to observe the `pc` walk as projectable state. The binding is
/// structural (anchors close obligations 1/3/4); the BEHAVIORAL guarantees are
/// proven in the abstract (Tier-0 `ty check` of `ForkExec.tla`) and defended by the
/// crate's real fork/exec unit tests (fail-closed-on-sandbox-failure, master-fd not
/// leaked). `UnsafeEnvOp` is `#[spec_unmodeled]` — it exists ONLY in the spec's
/// `Buggy` branch (the pre-fix child's setenv/alloc in the window); the fixed code
/// has NO such step, so there is nothing to bind.
///
/// NOT MODELED, stated so nobody reads a green `ty` as more than it is:
/// `ForkExec.tla` has no notion of a slave fd, of `O_CLOEXEC`, or of a CONCURRENT
/// actor. `MasterClosedBeforeExec` says THIS child closes THIS master before its
/// exec; it says nothing about an unrelated process inheriting either descriptor.
/// The close-on-exec-from-birth property that [`open_pty_pair_cloexec`] provides is
/// therefore guarded by runtime verification (the flag is read back before the
/// fork) and by the `pty_open_honors_the_cloexec_open_flag`,
/// `pty_pair_is_close_on_exec_from_birth` and
/// `an_unrelated_exec_inherits_neither_end_of_a_live_pty` regression tests below
/// — NOT by the model checker.
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): each fork_exec action projects the
// real child program-counter walk onto the spec's `<<pc, masterClosed, unsafeOpRan,
// envPrebuilt>>`. The witness is `aterm_pty::child_spawn::project_pc` — the structural
// projection of the child's ordered step list (BeforeFork→…→Execed) that the fork/exec
// unit tests drive. fork_exec is NOT in-process Tier-1 (the post-fork child cannot be
// driven from the test harness — the gate's ISOLATION note), so the projection is named
// for L2 (Trust requires a non-empty projection NAME, not its execution); the behavioral
// binding is the crate's real fork/exec unit tests.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Fork",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Setrlimit",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Chdir",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "CloseMaster",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Exec",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "fork_exec",
        action = "UnsafeEnvOp",
        reason = "Modeled DEFECT only: UnsafeEnvOp fires solely in ForkExec.tla's Buggy branch \
                  (the pre-fix child running setenv/var_os/current_dir/CString/format!/Vec — \
                  async-signal-UNSAFE work in the fork..exec window). The fixed child runs NONE \
                  of these (all env/argv/envp is pre-built in the parent before the fork), so \
                  there is no shipping code to bind; the action exists to let ty PROVE the \
                  defect is excluded (OnlySafeBeforeExec) at Buggy=TRUE."
    )
)]
// Skip: extraction classifies this body `TreatedAsAssumption(AddressOfField)`
// (a `&raw`-of-field shape the extractor cannot yet model; $HOME/trust's
// trust-mir-extract `supportability.rs` returns `UnsupportedReason::AddressOfField`
// for any `Rvalue::RawPtr` over a field projection). Measured 2026-09-27 on
// seal 321aaeda7 with this skip removed: the body is not lowered into the
// bundle (its caller sees an absent callee) and the function reads
// INCONCLUSIVE, which strict verification refuses. The explicit skip is the
// same epistemic state (unverified-by-capability-gap, machine-visible), spelled
// through the honored opt-out channel. The fork..exec window's safety is
// separately machine-checked by the ForkExec.tla refinement anchors above
// (OnlySafeBeforeExec proven). Needed from $HOME/trust, in a promoted seal:
// field-address `&raw` extraction; then drop this.
//
// WHICH shape, kept current: the field-address shapes in this body are in the
// child branch —
// `addr_of_mut!(sa.sa_mask)` / `addr_of!(sa)` in the signal normalisation, and
// `addr_of!(b)` for the status byte. If the extractor ever stops flagging those,
// this skip becomes unjustified rather than merely redundant, and nothing
// notices on its own: re-check with `targo trust check -p aterm-pty --lib
// --allow-l0-gaps` after each `aterm pkg update trust`.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid_cell_px(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    // Config `shell` / `--shell`: the program to run instead of `$SHELL`. An
    // absolute path or a bare name (used verbatim as the exec target on Unix —
    // `execve` needs a path, so a bare name relies on it being absolute or the
    // caller's discovery having resolved it). `None` → `$SHELL` (the default).
    shell_override: Option<&str>,
    // Config `shell_args`: extra argv after argv[0]. When set, the shell runs as
    // a NON-login interactive shell with exactly these args (a login `-basename`
    // argv[0] and explicit `-l` would conflict). `None` → the default login argv.
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    // The `rlimit` set applied in the child before exec. The caller chooses it by
    // containment mode: hardened ([`aterm_sandbox::Limits::shell_default`]) for
    // Safety/Containment, permissive ([`aterm_sandbox::Limits::inherit`]) for the
    // daily-driver User/Master modes so normal programs aren't constrained more than
    // the launching login shell.
    limits: aterm_sandbox::Limits,
    // The host's REAL cell size in device pixels, `(width, height)`, used to fill
    // the winsize PIXEL fields the child reads with its very first
    // `ioctl(TIOCGWINSZ)` — see [`resize_with_cell_px`]. Passed at SPAWN rather
    // than left to the first resize because an image tool run as the session's
    // first command (`aterm -e chafa …`) reads the geometry before any resize
    // exists; `None` (headless, tests, a frontend with no metrics yet) keeps the
    // fields zero.
    cell_px: Option<(u16, u16)>,
) -> io::Result<SpawnedShell> {
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

    // EVERYTHING that allocates or reads the environment happens HERE, in the
    // PARENT, BEFORE the fork. The frontend is multi-threaded (GPU/Metal + socket
    // threads are live), and POSIX permits ONLY async-signal-safe calls between
    // fork and exec — so the child below must not allocate, take the std env
    // lock, or call `setenv`. We pre-build the C arrays and hand them to
    // `execve`; a lock a vanished thread held would otherwise deadlock (or, with
    // the macOS Obj-C runtime, hard-abort) the child.
    // The shell, and where it came from: an exec failure names the source, so the
    // person knows which setting to fix.
    let (shell, shell_from): (std::ffi::OsString, &str) =
        match shell_override.filter(|s| !s.is_empty()) {
            Some(s) => (s.into(), " (set by --shell or `shell` in aterm.toml)"),
            None => match std::env::var_os("SHELL") {
                Some(s) => (s, " (from $SHELL)"),
                None => ("/bin/sh".into(), ""),
            },
        };
    // The shell's path, and a name built from it, as C strings. Only the branches
    // that run the shell build them, so a bad configured shell does not refuse an
    // unrelated `-e`. A NUL in the shell refuses the spawn: `execve` would read the
    // path only up to it, and a substitute (this seam once swapped in `/bin/sh`)
    // runs a program the person did not name. `$SHELL` cannot hold a NUL, so
    // only `--shell` / config `shell` reaches this.
    let shell_cstring = |name: &std::ffi::OsStr| {
        CString::new(name.as_bytes())
            .map_err(|_| crate::program_nul_refusal(&format!("the shell{shell_from}")))
    };

    // envp = the inherited environment with every deny-listed key removed, then
    // `env_add` applied on top (overriding or appending). Built by `build_child_env`
    // (pure in its inputs) so the deny-list wiring is unit-tested deterministically,
    // without mutating the process-global environment. `env_store` owns the C
    // strings `envp` points into.
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

    // exec target + argv. `-e prog args…` (`exec_command`) runs the command
    // DIRECTLY and takes precedence over every shell path. Otherwise the program is
    // `$SHELL` and argv is: an explicit override (bash `--rcfile …`) wins; else
    // the config `shell_args`; else a LOGIN interactive shell whose argv[0] is
    // "-"+basename (the macOS convention → sources
    // .zprofile / .bash_profile / path_helper). `argv_store` + `exec_target` own
    // the C strings the child's `execve` reads.
    // `-e`'s program is named as typed, with no shell source beside it.
    let runs_program = exec_command.is_some_and(|c| !c.is_empty());
    // Every list is converted whole or not at all (`argv_cstrings`): an element
    // holding a NUL refuses the spawn, before the fork, instead of being dropped.
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
            // Config `shell_args`: a non-login interactive shell, argv[0] = the
            // shell basename (not the login "-" form — explicit args own the
            // login/interactive choice), then the user's args verbatim.
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
            let mut argv0 = std::ffi::OsString::from("-");
            argv0.push(base);
            (cshell, vec![shell_cstring(argv0.as_os_str())?])
        };

    // OS-sandbox wrap (macOS Seatbelt, ATERM_DESIGN §5.6). When the caller demands
    // a sandbox (`Some(sbpl)` — Containment mode denies network), wrap the resolved
    // program+argv in `/usr/bin/sandbox-exec -p <sbpl>` so the kernel applies the
    // profile before the target execs. We FAIL CLOSED in the PARENT (before any
    // fork) if the wrapper binary is absent: a caller that demanded the sandbox
    // must NEVER get an unsandboxed shell. The wrapped argv is:
    //   sandbox-exec, "-p", <sbpl>, <program-path>, <original argv[1..]>
    // i.e. the original argv with argv[0] replaced by the resolved program PATH
    // (sandbox-exec execs its first positional and sets that path as the child's
    // argv[0]). This preserves every real argument (`--rcfile FILE`, `-c SCRIPT`,
    // a `-e` command's args); only the cosmetic leading-dash login marker on a
    // BARE interactive shell is dropped (a Containment shell is a non-login
    // interactive shell — an accepted, documented tradeoff for the hostile mode).
    // `exec_target`/`argv_store` from above are shadowed by the wrapped versions so
    // the rest of the seam (the C-array build, the child's execve) is unchanged.
    let (exec_target, argv_store): (CString, Vec<CString>) = if let Some(sbpl) = sandbox_wrap {
        // FAIL CLOSED in the PARENT, before any fork, if the wrapper is missing or
        // the argv can't be built — never spawn an unsandboxed shell when a sandbox
        // was demanded. The presence check + argv build is the pure, testable
        // `build_sandbox_wrap`.
        build_sandbox_wrap(SANDBOX_EXEC_PATH, sbpl, &exec_target, &argv_store)?
    } else {
        // No wrap requested → byte-identical to the pre-sandbox spawn.
        (exec_target, argv_store)
    };

    let mut argv: Vec<*const libc::c_char> = argv_store.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());

    // chdir target: an explicit `--working-directory` (`cwd`) wins; else, when
    // launched from `/` (a Finder/launchd .app start), begin in $HOME instead of
    // the filesystem root. Resolved up front — the child only calls `chdir`.
    let chdir_c: Option<CString> = if let Some(dir) = cwd {
        CString::new(dir.as_bytes()).ok()
    } else if std::env::current_dir().ok().as_deref() == Some(std::path::Path::new("/")) {
        std::env::var_os("HOME").and_then(|h| CString::new(h.as_bytes()).ok())
    } else {
        None
    };

    // Exec-status channel: a close-on-exec channel whose write end the child
    // holds. A successful `execve` closes that end (O_CLOEXEC) and the parent
    // reads EOF (0 bytes) = "child exec'd confined". A pre-exec failure (sandbox
    // apply error, or execve itself failing) makes the child WRITE a one-byte
    // reason then `_exit`, and the parent reads that byte = "child failed before
    // exec" and returns an error rather than a master fd for an unconfined shell.
    //
    // BOTH ends are close-on-exec FROM BIRTH — `pipe2(O_CLOEXEC)` on Linux, an
    // unlinked `O_CLOEXEC` FIFO on Darwin — so, unlike the `pipe(2)` + two
    // `fcntl(F_SETFD)` this replaces, there is no instant at which a concurrent
    // `Command::spawn` on another thread can inherit either one. See the long
    // comment on `open_exec_status_channel` for the defect, the measurements, and
    // the carriers that were tried and rejected.
    let (status_rd, status_wr, _status_carrier) = open_exec_status_channel()?;
    // The child's inheritance strip (step 0c), prepared HERE: the bound its
    // last-resort loop reaches is the kernel's per-process descriptor maximum,
    // which no thread of this process can raise between this read and the fork
    // (the soft limit, the old ceiling, could be — and was capped at 65 536
    // besides).
    let inherit_strip = aterm_uds::spawnfd::InheritStrip::prepare();

    // The child's FIRST winsize, pixel fields included: a tool that reads
    // TIOCGWINSZ before anything resizes the window must still learn the pixel
    // geometry (`winsize_pixels`).
    let (ws_xpixel, ws_ypixel) = winsize_pixels(rows, cols, cell_px);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel,
        ws_ypixel,
    };
    // Explicit slave termios (kernel defaults + IUTF8 + B230400 — see
    // `build_spawn_termios`); `None` = kernel defaults. Built here in
    // the parent so it is applied atomically at slave-open time (no post-fork
    // tcsetattr race with the shell's own termios reads).
    let termp = spawn_termios();
    // The PTY pair, BOTH ends close-on-exec FROM BIRTH — this is `openpty`'s
    // job, done without `openpty`'s window (see `open_pty_pair_cloexec` for the
    // full rationale: `forkpty` = `openpty` + `fork` + `login_tty`, and it leaves
    // two unflagged pty descriptors in this multi-threaded parent across the
    // fork, which a concurrent `Command::spawn` inherits straight through its
    // `exec`). termios and winsize are applied to the SLAVE here, before the
    // fork, exactly where `openpty` applied them.
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
    // Everything the child needs was pre-built above; the child branch below
    // is async-signal-safe only.
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
        // CHILD — async-signal-safe ONLY. Everything was pre-built in the parent
        // above; nothing here allocates, locks, or reads std env.
        // (0) the read end is the parent's; drop it in the child so only the
        //     write end (closed by exec on success) carries the status.
        // SAFETY: `status_rd` is the inherited read-end fd; `close` is a-s-safe.
        unsafe {
            libc::close(status_rd);
        }
        // (0a) ACQUIRE THE CONTROLLING TERMINAL. `login_tty` is, VERIFIED by
        //     disassembly of libsystem_c on macOS 26.5.1: `setsid()`; `ioctl(fd, TIOCSCTTY, NULL)` (→ -1 on
        //     failure); `dup2(fd,0)`; `dup2(fd,1)`; `dup2(fd,2)`; `close(fd)`
        //     when fd >= 3. Every one of those is async-signal-safe — no
        //     allocation, no locks, no libc state — so it is legal in this
        //     window. It is what makes the child a SESSION LEADER with
        //     pid == sid == pgid, the identity `hangup`'s `killpg`,
        //     `record_adopted_shell`'s `tcgetsid` proof, and the GUI's
        //     `tcgetpgrp(master)` all depend on.
        //
        //     The `dup2`s CLEAR FD_CLOEXEC on the copies (dup2 never carries the
        //     flag over), so a close-on-exec slave still yields stdio that
        //     SURVIVES `execve` — the property that would silently leave the
        //     shell with no terminal if this design were wrong, which is why
        //     `spawned_child_stdio_is_an_inherited_tty_with_a_ctty_and_our_winsize`
        //     asserts it end to end.
        //     They land on 0/1/2 and nowhere else: the status pipe's write end
        //     and the master keep their numbers, exactly as under `forkpty`.
        //
        //     The child must NOT close the slave afterwards — `login_tty` has
        //     already closed it (any fd >= 3), and a defensive close here would
        //     hit whatever number was recycled into its place, which in this
        //     child could be `status_wr` — silently turning the status
        //     protocol's "EOF means the child exec'd" into a lie.
        //
        //     On failure, reproduce `forkpty`'s OWN fallback (it syslogs and
        //     still wires up stdio rather than exec'ing a shell with none):
        //     dup2 the slave over 0/1/2 by hand and close the original. The
        //     shell then runs without a controlling terminal — degraded, but
        //     byte-identical to what this seam has always done, and so NOT a new
        //     status-byte case the parent would have to learn to interpret.
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
        // (0b) NORMALIZE SIGNALS — hand the shell a POSIX-clean slate. `execve`
        //     auto-resets CAUGHT signals to SIG_DFL, but IGNORED dispositions AND the
        //     signal MASK both SURVIVE exec. Rust's std installs SIGPIPE=SIG_IGN
        //     process-wide, and aterm-gui blocks SIGUSR1 before spawning; without this
        //     reset every shell and ALL its descendants would inherit SIGPIPE ignored
        //     (spurious "Broken pipe" on `cmd | head`, pagers/`git log` not dying on
        //     SIGPIPE) and SIGUSR1 blocked. Resetting the standard signals to SIG_DFL
        //     also repairs an inherited SIGCHLD=SIG_IGN, which would otherwise stop the
        //     shell from reaping its own children (`wait` → ECHILD). SIGKILL/SIGSTOP
        //     cannot be reset (sigaction → EINVAL); skip them.
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
        // (0c) NOTHING ELSE IS INHERITED. The shell must start with its stdio
        //     and nothing more, but `fork` copied every descriptor this process
        //     held, and the ones opened WITHOUT close-on-exec would survive
        //     `execve` into the shell and every program it ever runs. This
        //     crate opens all of its own close-on-exec; the frameworks the GUI
        //     loads do not — MEASURED 2026-09-27: a login shell aterm spawned
        //     held Metal's shader-cache files (`com.apple.metal/…/
        //     libraries.data`, `functions.list`, …) open READ-WRITE on fds 8,
        //     10, 11 and 17, inherited from the GUI's GPU driver, for its whole
        //     life, and so did everything it started. So every descriptor above
        //     stdio is marked close-on-exec (`InheritStrip::apply`), found by
        //     LISTING this child's own table — `PROC_PIDLISTFDS` on macOS,
        //     `close_range(CLOSE_RANGE_CLOEXEC)` or `/proc/self/fd` on Linux —
        //     and probed number by number up to the kernel's per-process maximum
        //     only past a listing that fails or fills (the old ceiling, the soft
        //     RLIMIT_NOFILE capped at 65 536 and read where another thread could
        //     raise it before the fork, let a descriptor above it through; the
        //     loop to `kern.maxfilesperproc` it was first replaced with cost
        //     ~8 ms per spawn, the listing nothing measurable). MARKING rather
        //     than closing leaves `status_wr` (already close-on-exec) open for
        //     the failure bytes below while `execve` closes it as before. Stdio
        //     (0/1/2) is below the range: `login_tty`'s `dup2`s are what the
        //     shell must keep, and nothing after this step opens a descriptor.
        //     It runs BEFORE the sandbox's `setrlimit` (1): a lowered
        //     RLIMIT_NOFILE could leave no number free for Linux's
        //     `/proc/self/fd`, and the strip needs none of the limits. This
        //     child cannot be a `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`,
        //     which needs no strip at all: the sandbox's `setrlimit` and
        //     `login_tty`'s `TIOCSCTTY` are not file actions.
        //     `apply` is async-signal-safe: raw syscalls on integers only.
        inherit_strip.apply();
        // (1) confine resource use (WS-G auto-sandbox). FAIL-CLOSED (§5.6): if
        //     the sandbox cannot be installed, do NOT exec an unconfined shell —
        //     signal the parent and exit before exec. With a valid cap `apply`
        //     does not allocate, and `setrlimit` is async-signal-safe.
        if limits.apply(sandbox_cap).is_err() {
            // SAFETY: write a single async-signal-safe failure byte then exit.
            // `write`/`_exit` are async-signal-safe; the byte distinguishes a
            // sandbox failure (b'S') for the parent's diagnostic.
            unsafe {
                let b: u8 = b'S';
                libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                libc::_exit(126);
            }
        }
        // (2) chdir to $HOME when started from `/`.
        if let Some(dir) = &chdir_c {
            // SAFETY: `dir` is a valid NUL-terminated path; `chdir` is async-signal-safe.
            unsafe {
                libc::chdir(dir.as_ptr());
            }
        }
        // (3) close the inherited master fd: the slave is already this child's
        //     controlling tty (step 0a), so the master must not leak into the
        //     shell or any process it spawns. This is the spec's `CloseMaster`.
        //
        //     Guarded on `master > 2` because step (0a)'s `dup2` closes its
        //     destination: had the master landed on 0/1/2, it is ALREADY closed
        //     and that number now holds the slave — closing it here would hand
        //     the shell no stdio. Unreachable in any real launch (aterm always
        //     has stdio open, so a pty master is >= 3), and cheap to be exact
        //     about in a branch that cannot be tested from inside the child.
        // SAFETY: `master` is this child's inherited master fd; `close` is
        // async-signal-safe.
        unsafe {
            if master > libc::STDERR_FILENO {
                libc::close(master);
            }
        }
        // (4) exec. `execve` (not `execvp`) takes the pre-built `envp` and does no
        //     PATH-search allocation; the target is an absolute path ($SHELL, or a
        //     `-e` program already PATH-resolved in the parent).
        //     On success `execve` does not return and the O_CLOEXEC `status_wr`
        //     is closed by the kernel → parent reads EOF (confined-and-exec'd).
        //     On failure, signal the parent (b'E') and exit before any shell runs.
        // SAFETY: exec_target/argv/envp are null-terminated arrays of live C
        // strings that outlive the call; `write`/`_exit` are async-signal-safe.
        unsafe {
            libc::execve(exec_target.as_ptr(), argv.as_ptr(), envp.as_ptr());
            let b: u8 = b'E';
            libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
            libc::_exit(127);
        }
    }
    // PARENT. Drop our copy of the SLAVE immediately — not optional: the child
    // holds the slave as its controlling terminal and its stdio, and a copy left open HERE would hold the pty open for as long as
    // this process lives. The master could then never report hangup after the
    // shell died (the "no ctty-owning leader exits" shape — MEASURED: `poll`
    // revents stay 0x0000 indefinitely), and `handoff_masters_closed` is
    // fail-closed on exactly that condition, so the seamless update would wait
    // forever on a session that is already gone. Every error path above closes
    // it too; from here on the parent owns only the master and the status pipe.
    // SAFETY: `slave` is the parent's own copy of the pty slave fd (pid > 0
    // here, so the child has its own); `close` releases just this copy.
    unsafe {
        libc::close(slave);
    }
    // There is deliberately NO `fcntl(master, F_SETFD, FD_CLOEXEC)` here, and
    // that absence is the point of the rewrite rather than an omission. The
    // property that call established — the master must not leak into shells
    // spawned by LATER sessions, since each session holds its master open for
    // its whole lifetime (the SinkWriter Arc owns it) and an inherited one is an
    // ungated cross-session input-injection / output-exfiltration channel that
    // bypasses the WriteInput/EdgeToken gate — now holds from the `open(2)` that
    // created the fd, VERIFIED before the fork by `open_pty_pair_cloexec`. Doing
    // it again post-fork would be dead code setting a flag that is already set,
    // and would re-imply that the window between fork and here was ever the
    // interesting one; it was not, which is precisely the defect. Its old
    // fail-closed teardown moved with it: a master that cannot be made
    // close-on-exec now fails the spawn BEFORE the fork, where there is no child
    // to SIGKILL and reap. `spawned_master_is_close_on_exec` still asserts the
    // returned fd carries the flag, and the flag stays ordinary and mutable —
    // `set_cloexec` CLEARS it so the master survives the seamless-update re-exec.
    //
    // Close our copy of the write end so the read sees EOF once the only
    // remaining write end (the child's) is gone (exec-closed or after the child
    // exits). Then read the status: 0 bytes (EOF) = success; any byte = the child
    // failed BEFORE exec, so there is no confined shell to hand back.
    // SAFETY: `status_wr` is the parent's copy of the write end.
    unsafe {
        libc::close(status_wr);
    }
    // BOUNDED wait for the verdict. The bound is defence in depth, not the fix
    // — the carrier above is what makes an unbounded wait unreachable in the first place — but it means
    // a future leak of a write end cannot resurrect the freeze this seam was
    // measured taking (5 s, 20 s and 65 s blocks, each exactly the lifetime of the
    // unrelated process that had inherited the write end).
    let status = wait_for_exec_status(status_rd, EXEC_STATUS_BUDGET);
    // SAFETY: done with the read end, on every branch below.
    unsafe {
        libc::close(status_rd);
    }
    let failure: Option<(io::ErrorKind, String)> = match status {
        // EOF. The child reached `execve` and the kernel closed its close-on-exec
        // write end during the image switch. This is the ONLY success verdict.
        ExecStatus::Execed => None,
        ExecStatus::FailedBeforeExec(b'S') => Some((
            io::ErrorKind::PermissionDenied,
            "sandbox confinement failed in child (fail-closed: shell not exec'd, _exit(126))"
                .to_owned(),
        )),
        ExecStatus::FailedBeforeExec(_) => Some((
            io::ErrorKind::Other,
            // NAME THE TARGET, and what is wrong with it. This is overwhelmingly
            // a config typo (`shell` in aterm.toml, `--shell` — the exec target
            // is used VERBATIM, no PATH search). The child cannot say more
            // (async-signal-safe: one status byte), but the parent holds the
            // exact path the child's `execve` read — `exec_target` covers every
            // mode, including `-e`, where the program is not the shell at all —
            // and can look at it. Under the sandbox the target is the wrapper,
            // so no shell source is named.
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
        // The budget elapsed with neither a byte nor EOF. FAIL CLOSED.
        //
        // With an ATOMIC carrier this is not ambiguous, and the reasoning is
        // worth spelling out because it is what makes the choice principled
        // rather than a coin flip. EOF fires at the kernel image switch, and no
        // process other than this parent and this child can hold a write end, so
        // a child that exec'd successfully has ALREADY produced EOF. A byte, if
        // one was ever written, sits in the buffer and would have been returned
        // by the final read regardless of who else holds a write end. So neither
        // outcome is merely late: expiry means the child has NOT exec'd — it is
        // stopped (a debugger, a stray job-control signal in the window before
        // its `sigprocmask`) or the machine is wedged far past anything
        // measurable here. Handing back a master would be exactly the §5.6
        // unconfined-shell hole the b'S' byte exists to plug.
        //
        // On the RACY fallback carrier the verdict is genuinely ambiguous — a
        // stranger holding a write end suppresses EOF from a healthy child — and
        // failing is still the right answer: a session that refuses to open with
        // a clear error is recoverable and visible, while returning a master for
        // a child whose confinement was never confirmed is not.
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
        // No confirmed, confined shell — so NOTHING usable escapes to the caller.
        // Kill first, then close the master: `kill` is unignorable and needs no
        // grace (we have already waited out the whole budget on the timeout
        // path), while closing the master revokes the pty and would only HUP a
        // child that had got far enough to have a controlling terminal.
        // `reap` is the crate's own BOUNDED collector (WNOHANG polling with a
        // ~2 s deadline, then it gives up rather than parking) — deliberately not
        // a blocking `waitpid`, because a `waitpid(…, 0)` here would reintroduce
        // an unbounded wait on the very path that exists to end one. A child it
        // cannot collect is left to the kernel at process exit.
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

/// HANG UP a spawned shell's controlling-tty session by sending `SIGHUP` to the
/// child's process group (`pid` from [`SpawnedShell`] is its session-leader pid
/// == pgid). The child — and its jobs — receive SIGHUP and exit; the PTY slave
/// then closes, so a reader thread blocked in `read(master)` gets EOF and ends on
/// its own. This is the NON-BLOCKING half of teardown: it never touches the tty
/// lock the way `close(master)` does, so the caller can run it from the UI thread
/// and only close the master afterwards (off-thread / at process exit). A no-op
/// for a non-positive pid (a pgid of <= 1 would target init / every process — we
/// refuse it). Best-effort: a child that already exited makes `killpg` fail
/// harmlessly (ESRCH), which is fine — the reader still sees EOF.
///
/// UNVERIFIED BY DESIGN: this signals whatever `pid` names right now. Only a
/// caller that KNOWS `pid` is its own unreaped child (a test, a spawn it has not
/// handed anywhere yet) may use it. A session's teardown goes through
/// [`hangup_shell`], which verifies a recorded [`ShellIdentity`] first (an
/// adopted shell's pid is freed without us ever reaping it).
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    // SAFETY: `killpg` with a signal merely posts SIGHUP to the process group;
    // `pid` is the session-leader pid the spawn returned (`login_tty` = setsid),
    // so the group is the child's own job tree. Any error (already-reaped child) is ignored.
    unsafe {
        libc::killpg(pid, libc::SIGHUP);
    }
}

/// The kernel's START TIME for `pid`, as an opaque number that differs between
/// two processes that held the same pid — the fact a [`ShellIdentity`] records
/// and re-reads.
///
/// macOS: `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec/usec`, folded to
/// microseconds. Measured on Darwin 25.6: libproc answers a live
/// process (136 bytes) and REFUSES a zombie (0 bytes) — so a zombie reads
/// `None` here, which is why [`ShellIdentity::verify`] keeps the unreaped-child
/// proof for that case. It also stops answering the moment a SIGKILL lands
/// (about 10 µs), well before the corpse is waitable (a few hundred µs idle,
/// tens of ms under load): a `None` here is "dying or dead", not "waitable".
/// Linux: `/proc/<pid>/stat` field 22 (`starttime`, clock
/// ticks since boot), with a zombie (`Z`) folded to `None` to match. Any other
/// Unix: `None` — no reader is wired, and nothing is claimed.
///
/// `None` also for a non-positive pid, no such process, or a kernel record
/// about a different pid than the one asked (libproc answering for a pid it
/// was not asked about would make any comparison meaningless).
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
    // `comm` (field 2) is parenthesised and may itself contain spaces or `)`,
    // so the fields proper start after the LAST `)`: field 3 (`state`) is the
    // first token there and field 22 (`starttime`) the twentieth.
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

/// Whether `pid` is THIS process's child and not yet reaped (alive or a
/// zombie) — asked without reaping it (`waitid(WNOHANG | WNOWAIT)`). Such a pid
/// cannot have been reissued by the kernel: the number stays ours until we
/// wait for it. Measured on Darwin 25.6 (2026-09-26): `0` for a live child and
/// for a zombie (twice — `WNOWAIT` leaves it collectable), `ECHILD` for pid 1.
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

/// Record the identity of a shell THIS process just spawned
/// ([`spawn_shell_with_pid`]'s `pid`), before anything could have reaped it.
///
/// The child is ours and unreaped, so the pid cannot have been reissued yet and
/// the start time read now is the shell's. A child that already died (an exec
/// that failed at once) reads no birth; `own_child` still proves it until it is
/// reaped, which is all teardown needs.
#[must_use]
pub fn record_spawned_shell(pid: i32) -> ShellIdentity {
    ShellIdentity::from_parts(pid, process_birth(pid), true)
}

/// Record the identity of a shell ADOPTED with its PTY master (the seamless
/// update handoff), proving first that `pid` is still the shell on that PTY.
///
/// The proof is the terminal's own answer: `tcgetsid(master) == pid` — the
/// session that owns the carried PTY is led by `pid`. Measured on Darwin 25.6
/// (2026-09-26): the master answers `TIOCGSID` with the shell's pid while it
/// lives and `-1` once the leader has exited (XNU clears the tty's session at
/// the leader's exit), so a shell that died before adoption — whose pid may
/// already name someone else — records NO birth and is never signalled by pid.
/// The check runs on both sides of the birth read, so a pid that changed hands
/// in between cannot slip a stranger's start time into the record.
///
/// This is also the ONLY identity an adopted shell can have: launchd, not us,
/// is its parent (`own_child` is `false`), so a pid freed by its exit is never
/// held for us by a zombie.
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
    /// Whether [`Self::pid`] STILL names the shell this identity was recorded
    /// for — the question every signal sent by pid must ask immediately before
    /// it is sent.
    ///
    /// * The pid's current start time is readable: it must equal the recorded
    ///   one. A reissued pid has a different start time. With nothing
    ///   recorded, only our own unreaped child passes (another Unix, where no
    ///   birth reader exists).
    /// * It is not readable (dead, dying — libproc stops answering the moment a
    ///   SIGKILL lands — a zombie, or no reader): only our own unreaped child
    ///   passes, because the kernel cannot reissue a pid we have not waited
    ///   for. An ADOPTED shell never passes here: launchd reaps it, so its pid
    ///   is free the moment it exits.
    ///
    /// Racy only within the kernel's own pid-reuse window between this check
    /// and the `kill` that follows it — microseconds, against the minutes-long
    /// `--hold` window the check exists for.
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

/// Teardown's `SIGHUP`, sent only where it provably belongs. Non-blocking and
/// UI-thread-safe like [`hangup`].
///
/// * `identity` verifies ([`ShellIdentity::verify`]): `SIGHUP` the shell's
///   process group, what `docs/DESIGN-pty-keeper-2026-09-26.md` F3 relies on.
/// * It does not: signal ONLY `tcgetpgrp(master)`, the group that is on this
///   terminal right now (only a process in the terminal's own session can be
///   its foreground group, so this can never reach a stranger). A shell that
///   already exited leaves no session on the PTY, so this answers `0` and
///   nothing is sent; the reader is ended by its wake pipe instead.
///
/// * No shell pid at all (`pid <= 1`, a stub): nothing, as before.
///
/// Returns what was signalled.
pub fn hangup_shell(identity: &ShellIdentity, master: i32) -> ShellHangup {
    // No shell pid at all (a stub or sentinel session): nothing of this
    // session's is on any terminal, exactly as [`hangup`]'s `pid <= 1` no-op.
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

/// Collect an exited child's status WITHOUT blocking and WITHOUT waiting for
/// teardown — the one narrow window in which a shell's exit code is still
/// answerable.
///
/// Call this at the PTY-exit edge, before [`reap`] runs on the teardown thread.
/// aterm never installs a `SIGCHLD` handler and the child is a direct `forkpty`
/// child, so between the reader seeing EOF and the session being dropped the
/// child is an unreaped zombie whose status is still collectable. [`reap`] does
/// the same `waitpid` and throws the status away, which is why this exists
/// separately rather than as a return value there: `reap` is a teardown helper
/// that may run long after anyone cares.
///
/// Three honest `None`s, all of which the caller must treat as "unknown", never
/// as success:
///
/// * the child has NOT exited — EOF on the master means the master went
///   unreadable, which a read error or a closed slave can also cause;
/// * `ECHILD` — an ADOPTED session (parented to launchd after an overlap
///   handoff), or one already reaped;
/// * a non-positive pid.
///
/// Never escalate to a blocking wait to "get a real answer": this runs on the
/// UI thread and an unbounded wait there is the defect, not the unknown.
///
/// INTERACTION WITH [`reap_shell`]. Once this has reaped, the caller must latch
/// that and never hand the pid to teardown again (the GUI's `child_reaped`):
/// the number is free. `reap_shell` would refuse it anyway — a reaped own child
/// is neither readable-with-the-recorded-birth nor an unreaped child — but the
/// latch is the first line. This must stay on the EXIT edge only — calling it
/// on the close path would disarm `reap_shell`'s SIGKILL escalation against a
/// HUP-ignoring shell, which is the whole reason that escalation exists.
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
        // Signal numbers are small positives; the cast cannot lose information
        // for any signal a process can actually be killed by.
        return Some(crate::ChildExit::Signal(libc::WTERMSIG(status) as u8));
    }
    // Stopped/continued without WUNTRACED/WCONTINUED is not reachable here, and
    // is not an exit in any case.
    None
}

/// Reap an exited child of OURS without ever blocking unboundedly: [`reap_shell`]
/// for a pid the caller knows is its own unreaped child (a spawn that failed
/// half-way, a test). A pid that is not our child (`ECHILD`) is left alone —
/// no signal is ever sent to a pid this process cannot prove it owns
/// (`getpgid(pid) == pid` proves nothing: every job-control group leader
/// passes it).
pub fn reap(pid: i32) {
    reap_shell(ShellIdentity::from_parts(pid, None, true));
}

/// Reap a session's shell WITHOUT ever blocking unboundedly, escalating to
/// `SIGKILL` only against a shell whose identity still verifies. Runs on the
/// detached teardown thread AFTER [`hangup_shell`] (the UI thread has already
/// moved on). A well-behaved child exits on SIGHUP within milliseconds and is
/// reaped on the first poll. The hazard this guards against: a child that TRAPS
/// or ignores SIGHUP (e.g. `trap '' HUP`, or one wedged in uninterruptible
/// D-state) would leave a plain blocking `waitpid(…, 0)` parked here FOREVER —
/// one leaked thread (and the fd/process slot it pins) per such mid-run close.
/// So poll `WNOHANG`: escalate to an unignorable SIGKILL after a short grace,
/// and after a hard deadline return — handing a shell that is still OUR
/// unreaped child to the process-wide straggler reaper ([`reap_in_background`]'s
/// thread), which waits for it without blocking anyone. A no-op for a
/// non-positive pid.
///
/// NEVER GIVEN UP ON. A session leader that
/// exits with output still queued on its terminal does not become a zombie:
/// its exit sleeps in the tty drain until the master is read or closed, and
/// teardown has already stopped the reader. No signal ends that sleep — the
/// leader has left its process group, so `killpg(pid, SIGKILL)` answers
/// `EPERM`, and a `kill(pid, SIGKILL)` is accepted and changes nothing
/// (measured on Darwin 25.6: `ps` state `?Es`, repeated `SIGKILL`s included,
/// well past this loop's deadline, and a zombie at once when the master
/// closed). The master closes only when its last `SinkWriter` clone drops,
/// which can be long after this loop's deadline, so a shell still OUR unreaped
/// child at the deadline is handed to the straggler reaper, which waits for it
/// whenever it does exit (else it stays `<defunct>` for the window's life).
///
/// NOT-OUR-CHILD SHELLS (the overlap handoff): a seamlessly ADOPTED session's
/// shell is a child of the PREVIOUS aterm process — after the overlap swap it
/// reparents to launchd, so `waitpid` here answers `ECHILD`. It still gets the
/// SAME grace/escalation schedule (a HUP-trapping shell closed via Cmd-W after
/// an update must still be force-killed), polled by identity instead of by
/// `waitpid` (launchd reaps the corpse, so no zombie risk on this path).
///
/// IDENTITY FIRST, EVERY TICK: before the
/// `waitpid` and again immediately before the `SIGKILL`, `identity` must still
/// [`verify`](ShellIdentity::verify). The moment it does not, this returns
/// having sent nothing: the pid is somebody else's now. Checking before the
/// `waitpid` matters too — an adopted shell's freed pid can be reissued to one
/// of OUR later children (another tab's shell), whose exit status a blind
/// `waitpid` here would steal.
pub fn reap_shell(identity: ShellIdentity) {
    // ~2 s budget: poll every 10 ms (200 ticks). A SIGHUP-ignoring holdout is
    // SIGKILLed at ~250 ms, so the common case returns on the first poll and the
    // pathological case is still bounded.
    reap_shell_scheduled(identity, Some(REAP_KILL_AT), REAP_DEADLINE);
}

/// [`reap_shell`] with its schedule given: `kill_at` is the tick of the
/// `SIGKILL` escalation (`None`: never — a test's stand-in for a child no
/// signal can finish), `deadline` the tick it hands a still-unreaped own child
/// to the straggler reaper.
fn reap_shell_scheduled(identity: ShellIdentity, kill_at: Option<u32>, deadline: u32) {
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
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param.
        // Returns the pid when reaped, 0 if still running as our child, -1
        // (`ECHILD`) when it is not our child — an ADOPTED shell parented to
        // launchd, whose liveness the identity check above already answered.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return; // reaped
        }
        // Still alive past the grace ⇒ it ignored SIGHUP. SIGKILL the group,
        // re-verifying IMMEDIATELY before the kill (the check above may be up to
        // one `waitpid` stale).
        if Some(tick) == kill_at && identity.verify() {
            // SAFETY: best-effort signal post to the verified shell's own
            // process group; it takes no pointers.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            #[cfg(test)]
            REAP_KILL_TICK.with(|t| t.set(Some(tick)));
        }
        #[cfg(test)]
        REAP_POLLS.with(|n| n.set(n.get() + 1));
        std::thread::sleep(POLL);
    }
    // Past the deadline. A shell that is still OUR unreaped child is never
    // dropped: the straggler reaper waits for it. (An adopted shell is
    // launchd's to reap; there is nothing of ours to wait for.)
    if identity.own_child && identity.verify() {
        hand_to_reaper(Straggler::Shell(identity));
    }
}

/// A child of this process that nobody else will wait for: a shell past
/// [`reap_shell`]'s deadline, or a helper a caller spawned and let go
/// ([`reap_in_background`]).
enum Straggler {
    /// A spawned shell, re-verified before every wait exactly as
    /// [`reap_shell`] does, so its pid is never waited for once it names
    /// anyone else.
    Shell(ShellIdentity),
    /// A spawned helper: the handle owns the pid until it is waited for.
    Child(aterm_uds::spawnfd::Child),
}

impl Straggler {
    /// Whether it is settled — reaped now, or no longer ours to reap. Never
    /// blocks, never signals.
    fn settled(&mut self) -> bool {
        match self {
            Self::Shell(identity) => {
                if !identity.verify() {
                    return true;
                }
                let mut status: libc::c_int = 0;
                // SAFETY: `WNOHANG` `waitpid` on the verified own child; a
                // valid out-param.
                let r = unsafe { libc::waitpid(identity.pid, &mut status, libc::WNOHANG) };
                r == identity.pid
                    || (r == -1 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
            }
            Self::Child(child) => !matches!(child.try_wait(), Ok(None)),
        }
    }
}

/// The straggler reaper's ledger: the children it still waits for, and
/// whether its thread is running. A `const` initializer — nothing lazy.
struct Stragglers {
    waiting: Vec<Straggler>,
    running: bool,
}

static STRAGGLERS: std::sync::Mutex<Stragglers> = std::sync::Mutex::new(Stragglers {
    waiting: Vec::new(),
    running: false,
});

/// How soon the straggler reaper first looks. It runs only while it holds a
/// child, and backs off to [`STRAGGLER_POLL_MAX`] while nothing it holds
/// settles, so a child held for hours (its terminal's master never closing)
/// costs a wake every two seconds, not ten a second.
const STRAGGLER_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// The straggler reaper's slowest look.
const STRAGGLER_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(2);

/// Hand `straggler` to the process-wide reaper thread, starting it if it is
/// not running. A thread that cannot start leaves the child queued; the next
/// hand-off tries again.
fn hand_to_reaper(straggler: Straggler) {
    let mut ledger = STRAGGLERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ledger.waiting.push(straggler);
    if !ledger.running {
        ledger.running = std::thread::Builder::new()
            .name("aterm-reaper".into())
            .spawn(reap_stragglers)
            .is_ok();
    }
}

/// The reaper thread: wait (without blocking) for each queued child, first
/// after [`STRAGGLER_POLL`] and then twice as long each time nothing settles,
/// up to [`STRAGGLER_POLL_MAX`]; end once none is left.
fn reap_stragglers() {
    let mut pause = STRAGGLER_POLL;
    loop {
        std::thread::sleep(pause);
        let mut ledger = STRAGGLERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = ledger.waiting.len();
        ledger.waiting.retain_mut(|straggler| !straggler.settled());
        if ledger.waiting.is_empty() {
            ledger.running = false;
            return;
        }
        pause = if ledger.waiting.len() < before {
            STRAGGLER_POLL
        } else {
            (pause * 2).min(STRAGGLER_POLL_MAX)
        };
    }
}

/// Reap `child` once it exits, off the caller's thread and without blocking
/// anyone: for a helper this process starts and does not wait for (a URL
/// handed to `open`, a detached tool). Dropping a `Child` that is still
/// running neither waits nor kills, so it stays a zombie once it exits, for the
/// life of the process — in a window that runs for days, one per click.
/// Nothing is signalled.
pub fn reap_in_background(mut child: aterm_uds::spawnfd::Child) {
    if matches!(child.try_wait(), Ok(None)) {
        hand_to_reaper(Straggler::Child(child));
    }
}

/// [`reap_shell`]'s poll period.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(10);
/// The [`reap_shell`] tick that SIGKILLs a SIGHUP-ignoring holdout (~250 ms).
const REAP_KILL_AT: u32 = 25;
/// The [`reap_shell`] tick at which it gives up (~2 s).
const REAP_DEADLINE: u32 = 200;

#[cfg(test)]
thread_local! {
    /// How many [`REAP_POLL`] waits [`reap_shell`] has taken on this thread: a
    /// test's census of the loop, counted in the product's own ticks, so a
    /// descheduled test thread cannot move it the way it moves a stopwatch.
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

/// Build the `sandbox-exec`-wrapped `(exec_target, argv)` for an OS-sandboxed
/// spawn, FAILING CLOSED if the wrapper at `wrapper_path` is missing/not
/// executable. Pure (its only side effect is the `access(X_OK)` probe of
/// `wrapper_path`), so the fail-closed and argv-shape behavior is unit-testable
/// without forking.
///
/// On success the returned exec target is `wrapper_path` and the argv is:
///   ["sandbox-exec", "-p", <sbpl>, <program-path>, <orig argv[1..]>]
/// i.e. the original argv with argv[0] replaced by the resolved program PATH
/// (`prog`), because `sandbox-exec` execs its first positional and sets that path
/// as the child's argv[0]. Every real argument after argv[0] is preserved; only a
/// cosmetic login-dash argv[0] on a bare shell is dropped (documented on
/// [`spawn_shell_with_pid`]).
///
/// # Errors
/// `NotFound` if `wrapper_path` is missing/not executable (fail-closed — the
/// caller must NOT spawn unsandboxed); `Other`/`InvalidInput` if `wrapper_path`
/// or `sbpl` cannot be turned into a C string (interior NUL).
fn build_sandbox_wrap(
    wrapper_path: &str,
    sbpl: &str,
    prog: &CString,
    orig_argv: &[CString],
) -> io::Result<(CString, Vec<CString>)> {
    let wrapper = CString::new(wrapper_path.as_bytes())
        .map_err(|_| io::Error::other("sandbox-exec path not representable"))?;
    // `access(X_OK)` in the PARENT (the child does no PATH search). A missing
    // wrapper means the policy-demanded sandbox cannot be applied → refuse.
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
    // saturating_add: a real argv cannot approach usize::MAX; the capacity
    // is a hint either way. Identical on every real input.
    // Clamp the pre-size HINT (advisory; the Vec grows on demand) so the bulk
    // allocation carries a provable bound for the L0 gate. A real argv is far
    // below the cap — identical contents for every input.
    let mut wrapped: Vec<CString> = Vec::with_capacity(orig_argv.len().saturating_add(3).min(4096));
    wrapped.push(CString::new("sandbox-exec").unwrap_or_else(|_| wrapper.clone()));
    // `CStr` literal (not `CString::new(..).unwrap()`): the unwrap's panic
    // lives in an absent std body and the literal is NUL-terminated at compile
    // time — same bytes, no panic path.
    wrapped.push(CString::from(c"-p"));
    wrapped.push(sbpl_c);
    wrapped.push(prog.clone());
    wrapped.extend(orig_argv.iter().skip(1).cloned());
    Ok((wrapper, wrapped))
}

/// What an exec failure says: the target, what is wrong with it, and `from` — where
/// a shell came from (empty for a `-e` program or the sandbox wrapper).
///
/// Read in the PARENT after the child's `execve` failed, so it may stat. A name with
/// no `/` is not looked at: `execve` does no PATH search, so a bare shell name is the
/// fault itself, and a bare `-e` program is one [`resolve_program`] found nowhere on
/// PATH. A relative path is judged from `cwd`, the folder the child ran it in.
fn exec_failure(
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
        if runs_program {
            "is not on PATH"
        } else {
            "is not a full path"
        }
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

/// `args` as the C strings `execve` hands a program, every element in its place,
/// or the refusal of the whole spawn when one holds a NUL byte. A C string ends
/// at its first NUL, so such an element cannot be passed at all, and leaving it
/// out would move every argument after it into its place: `rm -- "$a" "$b"` with
/// a NUL in `$a` would run `rm -- "$b"`. `list` names where the arguments came
/// from, for the error. Run in the PARENT, before the fork.
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

/// PATH-resolve a `-e` program name to an absolute path, IN THE PARENT (the child
/// must stay async-signal-safe, so it cannot do its own `execvp` PATH search). A
/// name containing `/` is used verbatim (an explicit path). Otherwise each `$PATH`
/// entry is probed for an executable regular file. Falls back to the name verbatim
/// when nothing matches, so `execve` fails cleanly (`_exit(127)`) instead of this
/// resolver masking a not-found command.
// Skip: `CString::new` panics only on allocation (interior NUL returns
// Err) — the audited-alloc class; the argv strings are bounded by the
// config. Re-measured 2026-09-27 on seal 321aaeda7 with this skip removed:
// VIOLATIONS (1 of 5 obligations refuted, 1 unknown). Needed from $HOME/trust, in
// a promoted seal: a totality entry for `CString::new` (total modulo
// allocation); then drop this.
#[cfg_attr(trust_verify, trust::skip)]
fn resolve_program(name: &std::ffi::OsStr) -> CString {
    // The interior-NUL fallback is a compile-time `c"..."` literal (NUL-free by
    // construction), not a runtime `CString::new(..).unwrap()`: same bytes, no
    // panic path — behavior-identical while discharging the Trust unwrap
    // panic-boundary obligations.
    let verbatim = || CString::new(name.as_bytes()).unwrap_or_else(|_| c"/nonexistent".to_owned());
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return verbatim();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join(name);
            if let Ok(c) = CString::new(candidate.as_os_str().as_bytes()) {
                // Executable (X_OK) AND a regular file — something we can exec.
                // SAFETY: `c` is a valid NUL-terminated path string.
                let executable = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
                if executable && candidate.is_file() {
                    return c;
                }
            }
        }
    }
    verbatim()
}

/// The decision a single `write(2)` return drives in the `write_all` drain loop.
/// Extracted as a pure value so the EINTR-retry / short-write / peer-closed branch
/// logic is unit-testable WITHOUT provoking a real (timing-dependent) `EINTR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStep {
    /// A signal interrupted the write before any byte moved (`EINTR`): retry.
    Retry,
    /// A real error, or the peer closed (`r == 0`): stop draining.
    Stop,
    /// `n` bytes were written: advance the slice cursor by `n` and continue.
    Advance(usize),
}

/// Classify a `write(2)` result for the `write_all` loop. `r` is the raw return;
/// `is_eintr` is whether `errno` was `EINTR` (only consulted when `r < 0`, exactly
/// as the loop does — the caller reads `errno` only on the error branch). Pure: no
/// syscalls, no `errno` read of its own, so it can be tested with synthetic inputs.
///
/// This is a behavior-preserving extraction of the original inline branch ladder;
/// the runtime decisions are byte-identical:
///   r < 0 && EINTR      -> Retry
///   r < 0 && other      -> Stop
///   r == 0 (peer closed) -> Stop
///   r > 0               -> Advance(r)
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

/// Write all of `bytes` to the PTY master, retrying short writes AND `EINTR`
/// (a signal interrupting the write must not silently drop the rest of the
/// buffer — that would lose terminal input). Stops only on a real error or a
/// zero/negative non-`EINTR` return (peer closed).
///
/// SPEC: this drain loop is the real implementation of the external `WriteAll.tla`
/// model (TRUST_NATIVE_TLA Phase 2, I/O DURABILITY family). Its `off` cursor
/// (`data = &data[n..]`) is monotone non-decreasing and only the FULL-buffer exit
/// (`data.is_empty()`) reports completion — exactly the spec's `NoSilentDrop`
/// (`done ⇒ off = Size`): a short write or `EINTR` ([`WriteStep::Advance`] with a
/// partial `n`, or [`WriteStep::Retry`]) keeps looping rather than claiming success
/// with a dropped tail. A progress write that reaches the end is the spec's
/// `Progress`; a short/EINTR step that does NOT finish is `Interrupted`. Tier-1
/// conformance drives this real loop over a slow reader and validates the `<<off,
/// done>>` trajectory against `WriteAll.tla` (`tests/conformance_writeall.rs`).
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): both drain-loop actions project the
// real `write_all` cursor onto the spec's `<<off, done>>` — `off = bytes.len() -
// data.len()` (the monotone consumed prefix), `done = data.is_empty()`. The witness is
// `aterm_pty::write_all_project`; the L2 obligation requires the projection NAME be
// present (Trust does not execute it — that is the aterm-side conformance binding).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "write_all",
        action = "Progress",
        project = "aterm_pty::write_all_project"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
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
        // `errno` is only meaningful when `r < 0`; mirror the original loop, which
        // read `last_os_error()` solely on the error branch.
        let is_eintr = r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
        match classify_write_result(r, is_eintr) {
            WriteStep::Retry => continue,
            WriteStep::Stop => break,
            // `Advance(n)` carries `n = r as usize` with `r > 0`, and `write(2)`
            // never reports more bytes written than the `data.len()` it was asked
            // for, so `n <= data.len()` and the range is always valid: the
            // `unwrap_or(&[])` fallback never fires — behavior-identical while
            // discharging the Trust slice-bounds obligation.
            WriteStep::Advance(n) => data = data.get(n..).unwrap_or(&[]),
        }
    }
}

/// Write `bytes` to the PTY master with a single `write(2)` (retrying only `EINTR`),
/// returning the number of bytes the kernel ACCEPTED (which may be fewer than
/// `bytes.len()`). A true count is what lets a sink apply real end-to-end
/// backpressure instead of silently dropping the tail — the routing-fabric
/// `SinkWriter` (`aterm-session`) is built on this.
///
/// Errors: [`io::ErrorKind::WouldBlock`] when `master` is non-blocking (see
/// [`set_nonblocking`]) and the input buffer is full; other errors when the slave is
/// gone. A `0` return (peer closed mid-write) is reported as `Ok(0)`.
// #[inline] so the MIR crosses the crate boundary: callers' Trust gates
// (aterm-session's drain loop) bundle and VERIFY this body instead of
// assuming an absent callee. Semantics unchanged.
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

/// [`write_some`] with BLOCKING semantics restored on an `O_NONBLOCK` master: on
/// `WouldBlock` it parks in `poll(POLLOUT)` and retries. The direct-read gather
/// flips the master's file description non-blocking (`O_NONBLOCK` is
/// per-DESCRIPTION, so a `dup` cannot isolate it) — every writer that relies
/// on the kernel parking a write into a full tty input queue (reply
/// writer, paste, control verbs, the spill drainer) keeps that exact behavior by
/// going through this instead of a bare [`write_some`]. `POLLERR`/`POLLHUP`
/// report as writable inside [`poll_writable`] ON PURPOSE: the retried write then
/// surfaces the real error. `EINTR` retries (in both the write and the poll);
/// other errors propagate.
// #[inline] so the MIR crosses the crate boundary (the `write_some` precedent):
// aterm-session's frame writers bundle and VERIFY this body. Its only error
// values are `Os(errno)` (born in `write_some`/`poll_writable`, returned — never
// dropped here); no panic path.
#[inline]
pub fn write_some_blocking(master: i32, bytes: &[u8]) -> io::Result<usize> {
    loop {
        match write_some(master, bytes) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Park until the tty input queue has room (or the peer errors —
                // reported writable, so the retry surfaces it).
                poll_writable(master, -1)?;
            }
            other => return other,
        }
    }
}

/// Classification of a single non-parking `write(2)`, for
/// [`SinkWriter::write_frame_nonparking`]. Distinguishes `WouldBlock` (an O_NONBLOCK
/// race) from a fatal error WITHOUT the CALLER ever holding — hence dropping — an
/// `io::Error`. The only `io::Error` constructed is the `Os(errno)` from
/// `last_os_error`, born inside [`write_some_nonparking`] and dropped THERE on the
/// EINTR / would-block branches (trivially total — no boxed `Custom` payload), or moved
/// into `Fatal` for the caller to RETURN. This keeps the caller's non-parking write loop
/// free of any opaque `io::Error` drop while it still surfaces `WouldBlock` and the real error.
#[cfg(unix)]
#[derive(Debug)]
pub enum NonParkWrite {
    /// `n > 0` bytes accepted; advance the cursor by `n`.
    Wrote(usize),
    /// Peer closed mid-write (`r == 0`) or nothing to write: stop.
    Closed,
    /// The fd would block (`EAGAIN`/`EWOULDBLOCK`): spill the tail.
    WouldBlock,
    /// A real error (the `Os(errno)` variant): the caller RETURNS it, never drops it.
    Fatal(io::Error),
}

/// One non-parking `write(2)`, classified as [`NonParkWrite`] — behavior-identical to
/// the prior inline `match write_some(..) { Ok(0)=>.., Ok(n)=>.., Err(WouldBlock)=>..,
/// Err(e)=>Err(e) }` ladder, but the `WouldBlock` decision no longer forces the caller
/// to drop an `io::Error`. `EINTR` retries internally.
// #[inline] so the MIR crosses the crate boundary (the `write_some` precedent):
// aterm-session's `write_frame_nonparking` bundles and VERIFIES this body.
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
            // `last_os_error()` is the `Os(errno)` variant, born here and dropped here on
            // the EINTR / would-block branches (total drop, no `Custom` payload) or moved
            // into `Fatal` below.
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

/// Poll the PTY master for writability, blocking up to `timeout_ms` (`0` =
/// check-and-return). `POLLERR`/`POLLHUP`/`POLLNVAL` report as "writable" ON PURPOSE:
/// the subsequent write then surfaces the real error to its caller instead of the
/// condition being silently classified as "not ready" and the bytes spilled forever.
/// `EINTR` retries; other `poll` errors propagate. The basis of the sink's
/// non-parking keystroke egress (`aterm-session`): a UI-thread writer checks this
/// before committing to a `write(2)` that a wedged foreground program (full tty input
/// buffer) would otherwise park the whole event loop on.
// #[inline] so the MIR crosses the crate boundary (the `write_some`/`dup_fd`
// precedent): aterm-session's non-parking write path (`write_frame_nonparking`)
// bundles and VERIFIES this body rather than assuming an absent callee. Its only
// error value is the `Os(errno)` `io::Error` (born+dropped on the EINTR branch,
// discharged by the Os-variant drop audit) or returned; no panic path.
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

/// Duplicate an fd into an [`std::os::fd::OwnedFd`] the caller owns outright — same
/// open file description (shared offset/flags), independent lifetime. Lets a detached
/// helper thread (the sink's spill drainer) keep writing safely without pinning the
/// original owner: even if the original fd number is closed and recycled, the dup
/// still names the PTY, so no write can land on a stranger's fd.
///
/// The duplicate is CLOSE-ON-EXEC, atomically — see the body for why a bare
/// `dup(2)` would reopen the very leak the spawn seam above closes.
// #[inline] so the MIR crosses the crate boundary (write_some precedent):
// aterm-session's spill path bundles and VERIFIES this body.
#[inline]
pub fn dup_fd(fd: i32) -> io::Result<std::os::fd::OwnedFd> {
    // `F_DUPFD_CLOEXEC`, NOT `dup`. POSIX defines `dup(2)` to CLEAR `FD_CLOEXEC`
    // on the new descriptor, so a bare `dup` of a session's PTY master hands back
    // a copy that survives `exec` — reintroducing, on the duplicate, exactly the
    // leak the `forkpty` replacement above exists to close. The one consumer is
    // the spill drainer (`aterm_session::sink::Shared::arrange_drainer`), which
    // holds its copy for as long as a wedged shell keeps the write queue spilling,
    // so the exposure is a long-lived duplicate rather than a fork-window race:
    // any `Command::spawn` during that whole period inherits a writable descriptor
    // onto that session's terminal.
    //
    // `F_DUPFD_CLOEXEC` sets the flag ATOMICALLY with the duplication, so unlike
    // `dup` + `fcntl` it leaves no window at all. It is POSIX 2008 and present on
    // macOS and Linux. The drainer only ever reads/writes the fd from a thread —
    // nothing here is meant to be inherited by a child — so close-on-exec is the
    // correct default, not a behaviour change the caller must opt into.
    //
    // SAFETY: `fcntl(F_DUPFD_CLOEXEC, 0)` on a caller-supplied fd returns a fresh
    // descriptor >= 0 or -1; the negative return is checked before `from_raw_fd`,
    // so ownership is only assumed for a real, freshly-duplicated fd.
    let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `d` is a fresh fd we exclusively own (just created by dup above).
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(d) })
}

/// Toggle `O_NONBLOCK` on the PTY master so [`write_some`] returns
/// [`io::ErrorKind::WouldBlock`] (instead of blocking) when the input buffer is full
/// — the basis for per-edge backpressure in the routing fabric. Idempotent; reads
/// the current flags first so it never clobbers unrelated `fcntl` state.
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

/// Toggle `FD_CLOEXEC` on `fd` — the DESCRIPTOR flag (`F_GETFD`/`F_SETFD`), distinct
/// from the STATUS flags [`set_nonblocking`] touches. Every PTY master is born
/// `FD_CLOEXEC` ([`open_pty_pair_cloexec`], so a later session's child can't inherit a
/// prior master — the cross-session isolation gate). Proof-carrying DSU's seamless re-exec
/// (RFC Rung 1b) must CLEAR it (`on = false`) on each master it hands to the new binary,
/// so the master SURVIVES `execve` and the running shell is not killed. Idempotent;
/// reads the current flags first so it never clobbers unrelated `fcntl` state.
///
/// SAFETY-CRITICAL: only clear `FD_CLOEXEC` on a master you are ABOUT to hand off and
/// then re-adopt (or close) in the new process — a cleared master that is neither
/// re-adopted nor closed leaks a live, ungated PTY channel across the exec.
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

/// The size the kernel holds for this PTY (`TIOCGWINSZ` on the master):
/// `(rows, cols)`, or `None` when the fd is not a tty or reads as 0 x 0. What
/// the child's own `TIOCGWINSZ` answers, so a host that briefly sets a PTY to a
/// size other than its grid's (the repaint nudge) can VERIFY it put the size
/// back instead of assuming it.
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

/// Whether `fd` is a tty (`TIOCGWINSZ` succeeds). A SANITY BACKSTOP for the seamless
/// re-exec's INCOMING side — NOT the authenticity gate. It rejects the obviously-wrong
/// (a pipe, a socket, a regular file, a closed/bad fd), but it does NOT prove `fd` is a
/// PTY MASTER: a pty SLAVE and even inherited `0`/`1`/`2` when aterm is launched from a
/// terminal are ttys and pass. The REAL fail-closed defense against a spoofed
/// `ATERM_SEAMLESS_FDS` is the single-use nonce stamp in the `0700` dir (only WE can
/// mint it) plus adopting ONLY the exact fd numbers listed in the authenticated
/// handoff map WE wrote — never "any fd that looks like a tty". This check is the
/// last-line "and it's still a tty" assertion after that authentication.
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

/// The slave's ECHO / ICANON bits, and its signal characters while ISIG is
/// set, read THROUGH THE MASTER — what the line discipline will do with the
/// next byte a keypress writes into this PTY ([`crate::TtyEcho`],
/// [`crate::TtySignals`]). `None` when `fd` is not a tty (a pipe sink, the `-1`
/// sentinel, a closed fd): the caller then knows nothing and must keep its
/// default behaviour.
///
/// WHY THE MASTER ANSWERS FOR THE SLAVE: a PTY pair has ONE termios — the
/// slave's — and `tcgetattr` on either end reads it. On both the BSD and the
/// Linux pty drivers the master's `TIOCGETA`/`TCGETS` is routed to the slave
/// tty's struct, which is the fact `spawned_pty_carries_iutf8_and_b230400`
/// (below, in the test module) proves end-to-end: it spawns `sleep`, reads the
/// master, and sees the IUTF8 + B230400 the spawn seam set on the SLAVE. The
/// same route carries a program's own `tcsetattr` on the slave — `read -s`
/// clearing ECHO, a TUI clearing ICANON — back to this read, which the
/// `tty_echo_*` tests below measure with real children.
///
/// One `ioctl`, no allocation, no blocking: cheap enough to run at every
/// typed key. The read is a snapshot — a program can flip the mode between
/// this call and the write that follows — so a consumer must treat it as
/// evidence about the press, never as a lock on the tty.
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
    // `_POSIX_VDISABLE`: the `c_cc` value that disables a special character.
    // Not declared in `aterm-libc`; `<sys/termios.h>` spells it 0xff on the
    // BSDs and `<bits/posix_vdisable.h>` '\0' on Linux.
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

/// Whether the slave's line discipline hands EVERY byte written into this PTY
/// to the program's input queue — so a queue that holds fewer bytes than were
/// written can only mean the program READ some. `Some(true)` exactly when
/// `ICANON`, `ISIG`, `IEXTEN`, `IXON` and `IGNCR` are all clear, which is
/// `cfmakeraw` (Node's `setRawMode`, Python's `tty.setraw`, every agent TUI);
/// `None` off a tty or when `tcgetattr` fails.
///
/// Each of those flags lets the driver CONSUME a written byte itself: `ISIG`
/// turns `VINTR`/`VQUIT`/`VSUSP` into a signal and flushes the queue, `IXON`
/// takes `VSTART`/`VSTOP` as flow control, `IEXTEN` takes `VLNEXT` and
/// `VDISCARD`, and `IGNCR` drops every `\r`. Measured on Darwin 25.6: in cbreak mode
/// (`ICANON` off, the rest on) `^C`, `^S`, `^O` and `^V` written through the
/// master each left FIONREAD at 0 with nothing reading the slave — and the
/// held gate lets a lone `^C` through by design, so counting that as a read
/// released a program that was still frozen.
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

/// `TIOCOUTQ` — `_IOR('t', 115, int)`, the output-queue depth of a tty. Not
/// declared in `aterm-libc` (nothing else reads it), so it is spelled here
/// the way aterm-gui's `session_program.rs` spells `KERN_PROCARGS2`; the test
/// module pins the value against its `_IOR` derivation.
#[cfg(target_os = "macos")]
const TIOCOUTQ: libc::c_ulong = 0x4004_7473;

/// How many bytes written INTO this PTY the program at the slave has not read
/// yet — `ioctl(FIONREAD)` on the master, which the Darwin pty driver answers
/// from the SLAVE's input queues. `None` when `fd` is not a tty (a pipe sink,
/// the `-1` sentinel, a closed fd), when the ioctl fails, and on every
/// platform but macOS.
///
/// WHY: a frozen program stops drawing, so every screen check passes while
/// keys queue unread behind each other. A raw-mode reader drains its queue
/// within a turn, so a byte that stays unread says the program stopped
/// reading, whatever the screen says.
///
/// What the count MEANS, measured on Darwin 25.6 and pinned by the
/// `queue_len_*` tests below:
///
/// * CANONICAL mode counts COMPLETE LINES only: `abc` typed at a cooked
///   prompt reads 0 (the line discipline holds a partial line where
///   `read(2)` cannot see it), and the `\n` that completes it makes the
///   count 4. So a canonical 0 does not mean "nothing typed".
/// * RAW mode counts EVERY byte: `\r` reads 1, and `\r` then `\x1b[B` reads
///   4, and ONE slave `read()` returns both.
/// * RAW with `VMIN` > the queued count and `VTIME` = 0 reads 0: the driver
///   reports what a `read(2)` would return right now, and that read would
///   still be waiting for `VMIN` bytes.
/// * Every error UNDERCOUNTS — `None`, never a guess — so a caller that acts
///   on a count can only act on bytes the kernel reported.
///
/// macOS-only ON PURPOSE: on Linux a master's `FIONREAD` counts the OUTPUT
/// the master has not read, not the slave's unread input, so the same call
/// would answer a different question. The Linux reading needs the slave
/// itself (a transient `TIOCGPTPEER` fd) and is not measured; until it is,
/// Linux answers `None` rather than a number that means something else.
///
/// One `isatty`, one `ioctl`, no allocation, no blocking — and no lock: a
/// caller may run it while a writer is parked on a full queue. The read is
/// a snapshot; a racing write or read moves it at once.
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

/// How many bytes the program at the slave has WRITTEN that the master has not
/// read yet — `ioctl(TIOCOUTQ)` on the master, the slave's output queue.
/// `None` off a tty, on error, and on every platform but macOS, exactly as
/// [`input_queue_len`].
///
/// Measured (Darwin 25.6, 2026-09-24): a raw slave writing 10 bytes reads 10,
/// and 0 once the master has read them; at a canonical slave with ECHO on,
/// the kernel's echo of typed bytes lands here too (`abc` typed reads 3).
///
/// Its use is as the fairness half of the stall reading: a program BLOCKED
/// WRITING to an aterm that is not draining its output is not frozen, it is
/// waiting for aterm. A non-zero count here says the lag may be aterm's, so
/// a consumer must not blame the program for input it has not read while
/// its own output sits unread. macOS-only for the same reason as
/// [`input_queue_len`]: this is the reading the Darwin driver gives, and
/// Linux is not measured.
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

/// DISCARD every byte written into this PTY that the program at the slave has
/// not read — `tcflush(master, TCIFLUSH)` — and return how many there were
/// ([`input_queue_len`], read just before). `None` off a tty, on error, and on
/// every platform but macOS, where nothing is discarded.
///
/// WHY: keys typed at a frozen program stay in the tty's input queue after
/// `signal term` kills it, and the shell that takes the terminal back reads
/// them as its own input — complete lines RUN AS SHELL COMMANDS. They were
/// typed for the dying program, so aterm drops them first.
///
/// Measured on Darwin 25.6, raw and canonical
/// alike: `tcflush(master, TCIFLUSH)` empties the SLAVE's input queue (the
/// master's and the slave's FIONREAD both read 0 after it) and leaves the
/// slave's OUTPUT queue (TIOCOUTQ) untouched; `TCOFLUSH` on the master does
/// the reverse. In canonical mode the flush also drops the partial line
/// FIONREAD cannot count, so a caller that means to keep a shell's
/// type-ahead must not call this there. Pinned by
/// `flush_input_queue_discards_the_unread_input_and_nothing_else`.
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
        let _ = fd;
        None
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

/// A wake pipe for CANCELLING a parked PTY reader: `(read_end, write_end)`. Both are
/// close-on-exec (they must NOT survive a seamless re-exec) and both ends are
/// non-blocking. The write end must be non-blocking because [`wake`] runs on the
/// event loop during handoff parking; a redundant wake against a full pipe may
/// never turn into input latency. Returns `None` on failure — the caller then degrades to a plain
/// un-cancellable [`read`], no worse than before. The owner unblocks a reader parked
/// in [`read_or_wake`] by writing the write end via [`wake`] (MEM-L2).
///
/// On Linux both flags are set BY THE CALL (`pipe2`), so no instant exists at which
/// a `Command::spawn` on another thread could inherit an unflagged end. Darwin
/// has no `pipe2`, and the window-free
/// carrier it does have — the exec-status channel's unlinked `O_CLOEXEC` fifo —
/// costs filesystem work (tens of ms on a busy volume) on the event loop's reader
/// attach, which a handoff pays once per adopted session: a visible stall is worse
/// than what the window costs here, because the wake is a byte and a stray copy of
/// either end blocks nothing. So Darwin keeps `pipe(2)` + `fcntl`.
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
    /// The wake pipe fired: the owning `Session` dropped and asked the reader to stop
    /// (tab/pane close) even though the master itself has NOT EOF'd — an orphaned child
    /// in another pgroup may still hold the slave open. The reader must exit and drop
    /// its refs so the master fd + Terminal + writer threads are released (MEM-L2).
    Wake,
    /// The wake-pipe-LESS fallback (`wake_rd < 0`, because `make_wake_pipe` failed under
    /// fd exhaustion) polled the master with a bounded timeout and it elapsed with no
    /// master activity. The caller must re-check its own (fd-free) stop flag and, if the
    /// session is still live, call again. This is what lets a reader that never got a
    /// wake pipe still be cancelled — instead of parking forever on a master an orphaned
    /// different-pgroup child keeps open, which would silently re-open the MEM-L2 leak
    /// exactly under the heavy-fd load that broke pipe creation. Never produced on the
    /// wake-pipe path (which blocks indefinitely with zero polling).
    Idle,
}

/// Timeout (ms) for the wake-pipe-LESS fallback poll in [`read_or_wake`]. ONLY the
/// degraded path (no wake pipe — `make_wake_pipe` failed under fd exhaustion) wakes on
/// this cadence; the normal wake-pipe path blocks indefinitely with zero polling. Small
/// enough that a dropped-but-orphan-pinned session is reclaimed promptly, large enough
/// that an otherwise-idle degraded reader costs only ~4 wakeups/sec.
const WAKELESS_POLL_TIMEOUT_MS: libc::c_int = 250;

/// Block until EITHER the master is readable (or EOF) OR `wake_rd` fires, then act.
/// Unlike a bare `read(master)`, this returns [`ReadOutcome::Wake`] when the owning
/// `Session` drops and writes its wake pipe — so a reader parked on a master that an
/// orphaned, different-pgroup child keeps open (setsid / double-fork / `disown`) is no
/// longer pinned forever.
///
/// `wake_rd < 0` (no wake pipe could be made — fd exhaustion) does NOT degrade to a bare
/// blocking `read`: that would re-pin the reader forever on an orphan-held master and
/// silently re-open MEM-L2 precisely under the heavy-fd load that broke pipe creation.
/// Instead it polls the master ALONE (allocating no new fd) with a bounded timeout and
/// returns [`ReadOutcome::Idle`] on expiry, so the caller can honor an fd-free stop flag.
pub fn read_or_wake(master: i32, buf: &mut [u8], wake_rd: i32) -> ReadOutcome {
    if wake_rd < 0 {
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is a valid 1-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 1, WAKELESS_POLL_TIMEOUT_MS) };
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
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // O_NONBLOCK master (the direct-read gather): a read after POLLIN can
            // still EAGAIN spuriously — that is Idle, NOT Eof (an Eof here would
            // tear the session down on a kernel wakeup race). EINTR likewise.
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
    ];
    loop {
        // SAFETY: `fds` is a valid 2-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
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
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // O_NONBLOCK master (the direct-read gather): a read after POLLIN can
            // still EAGAIN spuriously — poll again rather than reporting a false
            // Eof that would tear the session down. EINTR likewise.
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

/// Top up `buf` past an initial [`read_or_wake`] read. macOS caps every master
/// `read` at ~1 KiB (the xnu tty output-queue high-water mark), so a
/// poll→read→process loop degenerates to lockstep with the writer at ~1 KiB per
/// cycle — every downstream cost (term-lock, taps, wake) is paid per KiB and the
/// drain ceils far below what the kernel can move. Gathering many kernel chunks
/// into one batch amortizes all of it. Two regimes:
///   * `filled` below one outq (~1 KiB) ⇒ interactive output: return on the FIRST
///     quiet poll, so a lone keystroke echo pays one extra zero-timeout poll and
///     nothing else;
///   * `filled` at/above one outq ⇒ the writer is saturating the queue: bridge its
///     microsecond refill gaps with up to [`DRAIN_SPIN_MAX`] short spins instead of
///     sleeping — a sleeping drain hands the whole gap back to the scheduler.
///
/// EOF/errors mid-drain just stop the top-up: the gathered bytes get processed and
/// the NEXT `read_or_wake` surfaces the Eof.
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

// No env knob restores the legacy paced-probe bridge (the retired
// `ATERM_PTY_BRIDGE_SPIN`, still named in docs/measured): an env read in this
// shipped crate could put the pre-fix drain latency back into a release. An
// A/B belongs in a scratch build with a reviewable diff.
/// Default idle-refill patience. Measured sweep (quiet M5 Max, 500 MB flood +
/// CPR round-trip probe): 0 ⇒ 231 MB/s / +0 µs; 50 ⇒ 266 / +79 µs p50;
/// 250 ⇒ 265 / +327 µs; 1000 ⇒ 254 / +1.27 ms (the bb0ac4c fps-regression
/// class). 50 µs recovers the full cutoff-churn loss at an invisible query
/// tax — and IMPROVES p99 (679 vs 1385 µs: fewer, fuller deliveries).
const IDLE_POLL_DEFAULT_US: u32 = 50;

/// [`drain_more_nonblocking`]'s per-batch budget WHILE A KEYSTROKE IS PENDING
/// (`interactive_pending() == true`): a third of the 3 ms flood budget, so the
/// gather can no longer hold a human's echo byte for a streaming program's
/// batch cadence. Still long enough — with the 16 immediate probes and the
/// µs idle bridge — to coalesce a TUI's per-keystroke repaint burst into one
/// batch rather than fragmenting it into ≤1 KiB slices.
const HOT_BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(1);

/// Idle-cutoff hysteresis: how long a dry, parser-idle gap may wait for a refill
/// before delivering. An immediate
/// cutoff (0) protects the fps/request-response class (the ghostty bb0ac4c
/// regression) but its small-batch churn costs ~33-43 MB/s of cat-flood
/// throughput; [`IDLE_POLL_DEFAULT_US`] is the measured sweet spot and the
/// shipped value. A development build may sweep it through the
/// `ATERM_PTY_IDLE_POLL_US=<µs>` seam ([`aterm_types::dev_seam!`]; `0` restores
/// the immediate cutoff). Read once; unparsable values mean the default.
/// Clamped to [`IDLE_POLL_CLAMP_US`]: a wait beyond the batch
/// budget is meaningless, and on the wake-pipe-less fallback path an
/// unbounded select would stall teardown for the full value.
fn idle_poll_us() -> u32 {
    /// Upper clamp — just past [`BATCH_BUDGET`]'s 3 ms.
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
/// the SAME batch) or delivery is due (quiet timeout, wake signal, HUP, error).
enum BridgeWait {
    Refill,
    Deliver,
}

// The bridge polls THIS thread has taken — tests only, so a gather's park
// primitive is read without a clock (the hot gather must take the µs idle wait,
// never this 1 ms poll). Thread-local on purpose: other tests in this binary
// take bridge polls concurrently, and a process-wide count would race them.
#[cfg(test)]
thread_local! {
    static BRIDGE_POLLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

// The timeout, in µs, that THIS thread's last [`idle_refill_wait`] handed the
// kernel — tests only. The census above says WHICH park a dry gap took; this
// says how long the idle park was allowed to last, read off the `timeval` the
// select call is given rather than off a clock. An idle wait rounded up to the
// millisecond (the `poll(2)` granularity the select exists to avoid) parks
// every idle gap for a full bridge poll, and no park count can see that.
#[cfg(test)]
thread_local! {
    static IDLE_WAIT_TIMEOUT_US: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

// A test's own batch budget for the gathers THIS thread runs — tests only, and
// only for a test whose subject is a park primitive or a cutoff, not the budget:
// the 1 ms [`HOT_BATCH_BUDGET`] and the 3 ms streaming budget are wall clocks
// started before the first read, so a gather preempted for that long on a
// loaded machine delivers on the budget before it ever reaches the dry gap the
// test is about (the merge gate at load 100-170 read `filled` 2048 of 4096
// there, and `parks` 1 of 2). It replaces whichever budget the gather would
// use, hot or streaming. The budgets themselves keep their own tests, which run
// on the shipped values.
#[cfg(test)]
thread_local! {
    static BUDGET_FOR_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

// The budget that ended the last gather THIS thread ran, when one did — tests
// only. Which budget cut a gather is a fact the gather knows; a stopwatch
// reading of how long it held is the machine's (a hot gather over a continuous
// stream held 178 ms at best of fifteen beside 72 spinners with the test
// clamped to background QoS, against a 2.2 ms ceiling).
#[cfg(test)]
thread_local! {
    static BUDGET_ENDED: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// The batch budget a gather checks at each dry gap: `shipped`, or a test's own
/// (`BUDGET_FOR_TEST`, a test-build item, so named rather than linked) in a
/// test build.
fn batch_budget(shipped: std::time::Duration) -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        return budget;
    }
    shipped
}

/// Park for the writer's next refill: one bounded poll on `[master, wake_rd]`.
/// A pending wake byte is deliberately NOT consumed — [`read_or_wake`] owns the
/// wake protocol and must observe it after the batch is delivered.
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

/// Park the parser-idle cutoff for the writer's next refill: one bounded
/// `select(2)` on `[master, wake_rd]` — select because `poll(2)` is
/// ms-granular and the refill gaps this bridges are µs-scale. A pending wake
/// byte is deliberately NOT consumed — [`read_or_wake`] owns the wake
/// protocol (same rule as [`bridge_poll`]). Timeout, error, or EINTR all
/// mean deliver: the hysteresis wait must never outlive its budget.
fn idle_refill_wait(master: i32, wake_rd: i32, wait_us: u32) -> BridgeWait {
    // select cannot represent fds >= FD_SETSIZE — deliver, same as knob-off.
    if master >= libc::FD_SETSIZE as i32 || wake_rd >= libc::FD_SETSIZE as i32 {
        return BridgeWait::Deliver;
    }
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
        tv_sec: (wait_us / 1_000_000) as _,
        tv_usec: (wait_us % 1_000_000) as _,
    };
    #[cfg(test)]
    IDLE_WAIT_TIMEOUT_US.with(|t| t.set(Some(tv.tv_sec as u64 * 1_000_000 + tv.tv_usec as u64)));
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
    if r <= 0 {
        return BridgeWait::Deliver; // timeout (burst over), EINTR, or error
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

/// [`drain_more`] for an `O_NONBLOCK` master: drain `read(2)` DIRECTLY to
/// `EAGAIN`, then bridge the writer's µs refill gaps with up to [`NB_SPIN_MAX`]
/// IMMEDIATE re-reads — the ~0.4 µs failed `read` IS the pause (a paced
/// 256×`spin_loop` block measures ~2 µs on Apple Silicon, so pacing overshoots
/// the ~1-2 µs refill by whole gap-lengths; measured ladder: paced 195 MB/s vs
/// ghostty-shape ~295 target). When a gap outlives the probe burst, ONE parked
/// [`BRIDGE_POLL_MS`] poll on `[master, wake]` CONTINUES the same batch on
/// refill rather than delivering a partial. Delivery triggers:
///   * first quiet with `filled` under one outq (~1 KiB) ⇒ interactive echo, NOW;
///   * the parse stage idle at gap-check time (`parse_in_flight == 0`, sampled
///     once per dry gap before parking) ⇒ holding the batch adds pure output
///     latency, and a mid-burst query's reply may be sitting in it;
///   * bridge poll quiet for a full timeout (burst over), wake-pipe signal
///     (teardown; the byte is LEFT for [`read_or_wake`]), [`BATCH_BUDGET`]
///     spent (checked per dry gap, so the effective bound is the budget plus
///     one bridge poll, ~4 ms — same shape as ghostty's), batch full, EOF, or
///     hard error (surfaced by the next [`read_or_wake`]).
///
/// CALLER CONTRACT: the master MUST be non-blocking ([`set_nonblocking`]) — on a
/// blocking fd the direct `read` would park the gather mid-batch. Callers keep
/// [`drain_more`] as the fallback when the fcntl fails. `wake_rd < 0` (fd
/// exhaustion) drops the wake fd from the bridge poll; `parse_in_flight = None`
/// (no parse stage attached) bridges unconditionally.
///
/// The parser-idle cutoff carries a measured hysteresis ([`idle_poll_us`],
/// [`IDLE_POLL_DEFAULT_US`]): a dry idle
/// gap waits up to that many µs for a refill before delivering — the
/// immediate cutoff's small-batch churn costs ~33-43 MB/s of flood
/// throughput; `0` restores immediate delivery.
///
/// `interactive_pending` is the host's "a human is waiting on the UI thread"
/// hint (armed at hardware key arrival, decaying a few hundred ms after the
/// last key; the same signal the parse stage slices its lock holds on). It is
/// consulted at each DRY GAP only: while it is true the batch budget is capped
/// at [`HOT_BATCH_BUDGET`] and the gap is bridged with the µs-scale idle wait
/// instead of the 1 ms bridge poll — so a keystroke's echo typed into a
/// continuously streaming program is delivered on the human's schedule, not
/// the batch's, while a TUI repaint burst (µs refill gaps) still coalesces.
/// A plain `fn` pointer so this crate stays independent of the host's metrics.
///
/// `at_gap(filled)` is the host's FENCE, called at every dry gap immediately
/// before the drain would park (the same point, and the same once-per-gap
/// cadence the drain tests inject refills at), with the number of bytes
/// gathered so far. Returning `true` delivers the batch NOW, without parking;
/// any refill stays queued in the tty for the next read. aterm-gui samples the
/// foreground process group here: the
/// drain has just hit `EAGAIN`, so every byte read so far is in `buf`, and a
/// group change seen at this point is cut exactly at `filled`. A host with no
/// fence passes `&mut |_| false`.
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

/// Shared gather body. `at_gap` is invoked after the immediate probes are
/// exhausted and immediately before the drain PARKS on a dry gap — either the
/// armed parser-idle wait (parser idle) or the `BRIDGE_POLL_MS` bridge poll
/// (parser busy). Exactly one of those two parks runs per dry gap, so it fires
/// exactly once per gap on whichever path is taken, with the bytes gathered so
/// far. Returning `true` delivers the batch instead of parking (the shipping
/// host's foreground fence — see [`drain_more_nonblocking`]). It is also the
/// deterministic test seam that lets a test inject a refill that PROVABLY lands
/// inside the park, instead of racing a sleep against it. The idle-cutoff
/// hysteresis is an explicit parameter here: the [`idle_poll_us`] OnceLock
/// caches the env once per process, so tests needing other knob values drive
/// this body directly.
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
    /// effective bound is this plus one [`BRIDGE_POLL_MS`] park.
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
                // Quiet. Interactive (< one outq) delivers NOW; over-budget
                // batches deliver regardless of bridge luck — and "over budget"
                // is the SHORT budget while a keystroke is pending: a byte that
                // is this human's echo must not ride the streaming program's
                // 3 ms batch schedule.
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
                // The µs-scale bridge: the parser-idle hysteresis, and — while a
                // key is pending — the ONLY park, whatever the parser is doing.
                // The 16 immediate probes plus this wait still coalesce a TUI
                // repaint's µs refill gaps into one batch (the bb0ac4c regression
                // class stays closed); what changes is that a continuous stream
                // no longer holds the echo for a 1 ms poll per gap.
                if hot
                    || parse_in_flight
                        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
                {
                    if idle_wait_us == 0 {
                        break; // knob off: bridging would add pure output latency
                    }
                    // Hysteresis: a µs-bounded wait for the refill instead of
                    // delivering a churn-sized batch (the budget still caps the
                    // whole gather — it is re-checked per dry gap).
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

/// Signal a reader parked in [`read_or_wake`] to stop: write one byte to the wake
/// pipe's write end. Best-effort — a full or closed pipe is harmless (one queued byte
/// is enough to make `poll` return).
pub fn wake(wake_wr: i32) {
    let b = [1u8];
    // SAFETY: `wake_wr` is the pipe write end; a 1-byte write is atomic (never partial).
    unsafe {
        libc::write(wake_wr, b.as_ptr() as *const libc::c_void, 1);
    }
}

/// Declare the CALLING thread `QOS_CLASS_USER_INTERACTIVE` (macOS; a no-op on
/// other unixes). For a worker that holds a lock the UI thread contends — the
/// sink's spill drainer — so it never runs below the thread it can stall (the
/// host's QoS floor rule). Safe wrapper so a `forbid(unsafe_code)` crate can
/// declare it; setting one's own class cannot fail in a way worth reporting.
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

/// Focus-linked shell QoS boost — Unix no-op stub (identical signature, so
/// the GUI's focus-change call site compiles unchanged). On Unix the kernel
/// PTY is a zero-process byte queue with no starvable middlemen, and there is
/// no boost to hand the child anyway: raising ANOTHER process's priority needs
/// root (an unprivileged `nice` moves a process down, never back up) and macOS
/// publishes no cross-process QoS API, so the child keeps the band it
/// inherited — the same `pri 31` a login shell and the TUI inside it run at.
///
/// The scheduler's interactivity boosts do not protect the shell's echo.
/// Measured on a saturated machine: a child sharing that band with this repo's own
/// compilers answered keystrokes at `echo_p95_ms=75.69 echo_p99_ms=150.04
/// echo_max_ms=367.85` while aterm wrote each key at `key_write_p99_ms=6.29`.
/// What aterm can actually do is DECLINE TO COMPETE — its merge gate, release
/// cutter and package lane all spawn at utility, below the band being typed
/// into — and publish the child's `echo_*` round trip on the `metrics` summary
/// beside its own slices, so a lag report can tell the two apart.
///
/// The real implementation (ConPTY conhost + shell root, `SetPriorityClass` +
/// power-throttling) lives in `src/windows/mod.rs`.
pub fn set_focus_boost(_master: i32, _on: bool) {}

/// Resize the PTY to `rows`×`cols` (`TIOCSWINSZ`), leaving the PIXEL fields
/// unset. Prefer [`resize_with_cell_px`] anywhere the caller knows the cell
/// metrics — a zeroed `ws_xpixel`/`ws_ypixel` is what every image tool reads as
/// "this terminal cannot tell me its pixel geometry".
pub fn resize(master: i32, rows: u16, cols: u16) {
    resize_with_cell_px(master, rows, cols, None);
}

/// Resize the PTY to `rows`×`cols` and fill the winsize PIXEL fields from
/// `cell_px` (the host's REAL device-pixel cell size, `(width, height)`).
///
/// `ws_xpixel`/`ws_ypixel` are the terminal's pixel geometry, and they are the
/// route an image tool actually takes: `chafa`, `timg`, `viu`, `img2sixel`,
/// matplotlib's sixel backend and libsixel's own probe all call
/// `ioctl(TIOCGWINSZ)` and size their output from these two fields, falling back
/// to a guessed cell size (or refusing to draw) when they are zero. aterm
/// hard-coded them to 0 at every construction site, so every one of those tools
/// got nothing — the XTWINOPS answers added later cover the escape-sequence
/// route only, and are off by default.
///
/// `None` leaves the fields zero (the honest answer when the caller has no
/// metrics — the headless CLI before its first `SIGWINCH`, a test harness).
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

/// The winsize pixel pair for a `rows`×`cols` grid of `cell_px`-sized cells:
/// `(cols * cell_w, rows * cell_h)`, saturating at `u16::MAX` because that is
/// the width of the kernel's fields — a window bigger than 65535 px on an axis
/// reports the cap rather than wrapping to a small number, which is the failure
/// mode that would have a tool render a thumbnail on a wall display.
///
/// A zero axis in `cell_px` is treated as "unknown" and yields 0 for BOTH fields:
/// half a geometry is not a geometry, and a tool dividing by it would divide by
/// zero.
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

/// Tier-1 trace conformance: bind the REAL `write_all` drain loop to the external
/// `WriteAll.tla` design spec (TRUST_NATIVE_TLA Phase 2, I/O DURABILITY family).
///
/// `WriteAll.tla` is model-checked in the abstract by aterm-spec-models'
/// `model_check.rs` (Tier-0: proves the loop reports completion ONLY when `off =
/// Size`, and catches the dropped-tail bug at `Buggy=TRUE`), but nothing tied it to
/// the code that runs. This test closes that gap two ways:
///
///   1. END-TO-END over a REAL pipe: drive `write_all` with a payload far larger
///      than the pipe buffer and a reader that drains it, proving the real loop
///      delivers EVERY byte (the spec's `NoSilentDrop` / `off = Size` exit) even
///      across the genuine short writes a small pipe forces.
///   2. PER-TRANSITION against `ty`: replay the loop's offset trajectory using the
///      REAL [`classify_write_result`] decision over synthetic `write(2)` returns
///      (`Advance(n)` advances `off`; `Retry`/`Stop` do not), and `ty trace
///      validate --spec` each `(off,done) -> (off',done')` step against `WriteAll`'s
///      `Next`. Because `WriteAll` is multi-transition (off climbs over several
///      steps) and `ty` strictly checks only `Init` + the FIRST transition, we pin
///      `Init` to each step's predecessor via a PARAMETERIZED variant of the
///      COMMITTED spec (mechanical `Init`/`CONSTANT` rewrite — every action and
///      invariant body is the committed text verbatim, so it cannot drift). A
///      NEGATIVE control (claim `done` while `off < Size` — the dropped tail) MUST
///      be ty-REJECTED, so a pass is never vacuous.
///
/// `ty` is located by the same fixed canonical path search. VERIFICATION GATE
/// (honesty ratchet, batteries-on, see [`aterm_spec::verify`]): verification is always
/// required — an absent Trust `ty` FAILS the test with the install hint (`aterm pkg
/// install ty`).
#[cfg(test)]
#[path = "unix_writeall_conformance_tests.rs"]
mod writeall_conformance;

/// TEARDOWN NEVER SIGNALS A RECYCLED PID (see [`ShellIdentity`]).
///
/// These tests hand the teardown a real PTY whose shell has gone, a recorded
/// identity for it, and the pid now naming a `setsid`'d `sleep` (a session AND
/// group leader, so `getpgid(pid) == pid` passes it), and require the sleep to
/// live. The positive controls keep the teardown for a shell that IS still the
/// one recorded: its group is hung up, and a HUP-ignoring one is still force-killed at ~250 ms.
#[cfg(test)]
#[path = "unix_shell_identity_tests.rs"]
mod shell_identity_tests;
