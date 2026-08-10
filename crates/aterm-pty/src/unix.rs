// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX backend of the PTY seam: every raw `libc` PTY syscall —
//! `posix_openpt`/`fork`/`login_tty` (originally `forkpty`, replaced to close a
//! close-on-exec window — see [`open_pty_pair_cloexec`]), the slave open
//! (`ioctl(TIOCGPTPEER)` on Linux, `open` of the `ptsname` elsewhere — see
//! [`open_pts_slave`]),
//! `execve`, `read`, `write`, `ioctl(TIOCSWINSZ)` — moved VERBATIM from the
//! pre-split `lib.rs` (zero semantic change). The shared, portable items
//! ([`SpawnedShell`], [`crate::UTF8_LOCALE`], `build_child_env`) live in
//! `lib.rs`; everything in this module is Unix-only by placement (no inline
//! cfg), and `lib.rs` re-exports it so Unix callers compile untouched.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{SpawnedShell, build_child_env};

/// Fixed absolute path to the macOS Seatbelt wrapper used by the OS-sandbox wrap
/// (see [`spawn_shell`]'s `sandbox_wrap`). Inlined here (rather than depending on
/// the policy crate) to keep this minimal syscall seam dependency-light; it MUST
/// equal `aterm_containment::SANDBOX_EXEC_PATH` — a test in this crate locks that.
const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Spawn `$SHELL` in a fresh PTY of `rows`×`cols`, returning the master fd.
///
/// Honors `$ATERM_EXEC`: if set, the shell runs that command first (to paint a
/// known screen) and then `exec`s an interactive shell so the result persists.
/// Defaults to `/bin/sh` when `$SHELL` is unset.
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
/// as given. It takes precedence over `argv_override` and `$ATERM_EXEC` — there is
/// no interactive shell to integrate with. An unresolved/again-failing `prog` ends
/// the child with `_exit(127)`, closing the window, just like a failed shell exec.
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
/// argv[0], `--rcfile`, `$ATERM_EXEC`, and `-e` paths are all preserved verbatim
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
/// # Errors
/// Returns `PermissionDenied` if the capability's tier is too low, the OS error
/// if the PTY pair / `pipe` / `fork` fails, or `PermissionDenied`/`Other` if the child failed
/// to confine itself (sandbox `apply` error) or to `execve` before exec. On any
/// pre-exec child failure the master fd is closed and NO unconfined shell is
/// returned.
// The arg list is intentionally wide: this is the SINGLE spawn seam, and each
// argument is an independent, security-relevant input (caps, env, argv, cwd, the
// OS-sandbox wrap). Bundling them into a struct would hide that surface, not
// shrink it.
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[String]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
) -> io::Result<i32> {
    // Thin compatibility wrapper: drop the child pid. Callers that need the pid
    // for a graceful, NON-BLOCKING teardown (SIGHUP the controlling-tty session
    // before closing the master — see `spawn_shell_with_pid`) use that instead.
    spawn_shell_with_pid(
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
        // The thin wrapper preserves the historical hardened default; the GUI's
        // real spawn path picks the limits by containment mode.
        aterm_sandbox::Limits::shell_default(),
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
/// `Setting locale failed`. Kept separate from [`UTF8_LOCALE`], which is the macOS-only
/// pbcopy/pbpaste pin; on macOS both resolve to the same value so they cannot drift.
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
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    // SAFETY: `buf` is a live, owned buffer and we pass its exact length;
    // `ptsname_r` either NUL-terminates within that length or fails (ERANGE).
    let rc = unsafe { libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `rc == 0` means the primitive wrote a NUL-terminated path into
    // `buf`, which is live for this borrow; `to_owned` copies it out at once.
    Ok(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_owned())
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
/// The OS error from `ptsname_r` or from the `open`. No descriptor escapes on any
/// error path — the only fd this can produce is the one it returns.
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

    let name = pts_name(master)?;
    // SAFETY: `name` is the NUL-terminated pts path the kernel just reported for
    // `master`; `open` reads it and returns a fresh fd or -1.
    let slave = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
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
/// `pty_open_honors_cloexec_flag` asserts the fallback is NOT what runs here.
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
///    `open(2)` itself. There is NO window on the master.
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
/// The OS error from whichever step failed. FAIL-CLOSED on every path: no
/// descriptor and no half-built pair escapes — the master is closed before
/// returning any error raised after it was opened.
fn open_pty_pair_cloexec(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
) -> io::Result<(libc::c_int, libc::c_int)> {
    // (1) MASTER — close-on-exec at the `open(2)` that creates it.
    // SAFETY: `posix_openpt` takes only a flag word and returns a fresh fd or -1.
    let opened = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    let master = if opened >= 0 {
        opened
    } else {
        let first = io::Error::last_os_error();
        // Only EINVAL can mean "this platform rejects the O_CLOEXEC bit" (POSIX
        // spells an unsupported `oflag` bit exactly that way). Anything else —
        // ENXIO/EAGAIN/EMFILE, i.e. out of ptys or out of descriptors — is a
        // real failure that a retry would only repeat, so surface it as-is.
        if first.raw_os_error() != Some(libc::EINVAL) {
            return Err(first);
        }
        // SAFETY: as above; the retry drops only the flag bit.
        let retry = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if retry < 0 {
            return Err(io::Error::last_os_error());
        }
        retry
    };
    if !fd_is_cloexec_or_make_it(master) {
        let err = io::Error::last_os_error();
        // SAFETY: closing the master we just opened; nothing else exists yet.
        unsafe {
            libc::close(master);
        }
        return Err(err);
    }

    // (2) grant + unlock the slave side.
    // SAFETY: `master` is a live /dev/ptmx descriptor; both calls are ioctls on
    // it and write nothing through any pointer we own.
    let granted = unsafe { libc::grantpt(master) == 0 && libc::unlockpt(master) == 0 };
    if !granted {
        let err = io::Error::last_os_error();
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
        let err = io::Error::last_os_error();
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
///    clamp may differ on other xnu/Linux versions. A/B-revert:
///    `ATERM_PTY_NULL_TERMIOS=1`.
///
/// `bench_no_opost` (BENCH-ONLY, env `ATERM_PTY_BENCH_NO_OPOST=1`) additionally
/// clears `OPOST`: the probe showed kernel output post-processing (ONLCR `\n`
/// scan) costs ~6% of raw PTY throughput (209.9 vs ~197 MB/s). It CHANGES
/// cooked-mode display semantics (bare `\n` stairsteps, no `\r` insertion), so
/// it must never be on for interactive use — default OFF, measurement only.
fn build_spawn_termios(mut t: libc::termios, bench_no_opost: bool) -> libc::termios {
    t.c_iflag |= libc::IUTF8;
    // SAFETY: `cfsetspeed` only writes the speed fields (and, on Linux, CBAUD
    // bits) of the valid, stack-owned `t`.
    unsafe {
        libc::cfsetspeed(&mut t, libc::B230400);
    }
    if bench_no_opost {
        t.c_oflag &= !libc::OPOST;
    }
    t
}

/// The termios applied to the spawn's slave — `None` means "apply none", i.e.
/// the kernel defaults, which is the historical spawn path (`forkpty`'s NULL
/// `termp`), byte-identical behavior. `ATERM_PTY_NULL_TERMIOS=1` forces that
/// historical path (the A/B revert switch for the [`build_spawn_termios`]
/// deltas), and with the explicit slave open it means exactly what it always
/// meant: open the slave and do NOT `tcsetattr` it. Runs in the PARENT before
/// the fork (env reads + the one-time probe allocate; the post-fork child stays
/// async-signal-safe and untouched).
fn spawn_termios() -> Option<libc::termios> {
    if std::env::var_os("ATERM_PTY_NULL_TERMIOS").is_some_and(|v| v == "1") {
        return None;
    }
    let bench_no_opost = std::env::var_os("ATERM_PTY_BENCH_NO_OPOST").is_some_and(|v| v == "1");
    kernel_default_termios().map(|t| build_spawn_termios(t, bench_no_opost))
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
// What changed is HOW the channel is built and HOW the parent waits on it.
//
// ## The defect this replaces
//
// The channel used to be `pipe(2)` followed by two separate
// `fcntl(F_SETFD, FD_CLOEXEC)` calls. `pipe(2)` returns two descriptors with NO
// close-on-exec flag (MEASURED on macOS 26.5.1: `F_GETFD` = 0x0 on both ends the
// instant `pipe` returns), so between the `pipe` and the second `fcntl` BOTH ends
// sat unflagged in a MULTI-THREADED process. This is the identical defect
// [`open_pty_pair_cloexec`] exists to remove on the pty pair, simply missed on
// the status pipe.
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
        eprintln!(
            "aterm-pty: the atomic exec-status channel could not be created ({why}); falling \
             back to pipe(2)+fcntl, which leaves both ends briefly inheritable by a concurrent \
             spawn. The spawn's status wait stays bounded at {EXEC_STATUS_BUDGET:?}, so this \
             degrades to a slow visible failure rather than a hang."
        );
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
/// ## What is used instead
///
/// `std::env::temp_dir()` — a plain `getenv("TMPDIR")` with a `/tmp` fallback. No
/// notify, no XPC, no lazily-initialised gate, so it cannot arm the abort above.
/// Resolved ONCE into a `OnceLock` so even the environment read happens a single
/// time, off the per-spawn path.
///
/// ## The `$TMPDIR` question, answered rather than waved away
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
    DIR.get_or_init(|| {
        let mut dir = std::env::temp_dir().as_os_str().as_bytes().to_vec();
        if dir.is_empty() {
            dir.push(b'/');
        }
        if dir.last() != Some(&b'/') {
            dir.push(b'/');
        }
        dir
    })
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
    let entropy_ok = unsafe { libc::getentropy(rnd.as_mut_ptr().cast::<libc::c_void>(), rnd.len()) }
        == 0;
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
    path.extend_from_slice(format!("aterm-xstatus-{pid:x}-{seq:x}-{unpredictable:016x}").as_bytes());
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
/// it would only matter to something spawning in a tight loop.
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
    let mut last = io::Error::new(io::ErrorKind::AlreadyExists, "exec-status fifo path collision");
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
                Err(io::Error::new(
                    io::ErrorKind::Other,
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
            note_racy_exec_status_carrier(&why);
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
            libc::select(
                fd + 1,
                &mut set,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut tv,
            );
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

/// Like [`spawn_shell`] but also returns the child pid (see [`SpawnedShell`]).
/// Identical spawn/sandbox/exec behavior — `spawn_shell` is this minus the pid.
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
// (a `&raw`-of-field shape the extractor cannot yet model) — the DEFAULT lane
// records exactly that assumption row, but the explicit-full gate lane ABORTS
// the whole crate on it. The explicit skip is the same epistemic state
// (unverified-by-capability-gap, machine-visible), spelled through the honored
// opt-out channel. The fork..exec window's safety is separately machine-checked
// by the ForkExec.tla refinement anchors above (OnlySafeBeforeExec proven).
// Droppable when AddressOfField extraction lands.
//
// WHICH shape, kept current: the `forkpty` rewrite REMOVED the
// `ptr::addr_of_mut!(*t)` that used to hand `forkpty` its optional termios (the
// pair is now opened by `open_pty_pair_cloexec`, which takes an `Option<&_>`).
// The field-address shapes that remain in this body are in the child branch —
// `addr_of_mut!(sa.sa_mask)` / `addr_of!(sa)` in the signal normalisation, and
// `addr_of!(b)` for the status byte. If the extractor ever stops flagging those,
// this skip becomes unjustified rather than merely redundant; `xtask gate
// dormant` and the explicit-full lane are what notice.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid(
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
    exec_command: Option<&[String]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    // The `rlimit` set applied in the child before exec. The caller chooses it by
    // containment mode: hardened ([`aterm_sandbox::Limits::shell_default`]) for
    // Safety/Containment, permissive ([`aterm_sandbox::Limits::inherit`]) for the
    // daily-driver User/Master modes so normal programs aren't constrained more than
    // the launching login shell.
    limits: aterm_sandbox::Limits,
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
    let shell = shell_override
        .filter(|s| !s.is_empty())
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("SHELL"))
        .unwrap_or_else(|| "/bin/sh".into());
    // The fallback is a compile-time `c"..."` literal (interior-NUL-free by
    // construction), not a runtime `CString::new(..).unwrap()`: same bytes, no
    // panic path — behavior-identical while discharging the Trust unwrap
    // panic-boundary obligations.
    let cshell =
        CString::new(shell.as_os_str().as_bytes()).unwrap_or_else(|_| c"/bin/sh".to_owned());

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
    // `$ATERM_EXEC` runs a command then execs the shell; else a LOGIN interactive
    // shell whose argv[0] is "-"+basename (the macOS convention → sources
    // .zprofile / .bash_profile / path_helper). `argv_store` + `exec_target` own
    // the C strings the child's `execve` reads.
    let (exec_target, argv_store): (CString, Vec<CString>) =
        if let Some(cmd) = exec_command.filter(|c| !c.is_empty()) {
            let argv: Vec<CString> = cmd
                .iter()
                .filter_map(|a| CString::new(a.as_bytes()).ok())
                .collect();
            (resolve_program(&cmd[0]), argv)
        } else if let Some(ov) = argv_override {
            let argv = ov
                .iter()
                .filter_map(|a| CString::new(a.as_bytes()).ok())
                .collect();
            (cshell.clone(), argv)
        } else if let Some(args) = shell_args.filter(|a| !a.is_empty()) {
            // Config `shell_args`: a non-login interactive shell, argv[0] = the
            // shell basename (not the login "-" form — explicit args own the
            // login/interactive choice), then the user's args verbatim.
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            let mut argv = vec![CString::new(base.as_bytes()).unwrap_or_else(|_| cshell.clone())];
            argv.extend(args.iter().filter_map(|a| CString::new(a.as_bytes()).ok()));
            (cshell.clone(), argv)
        } else if let Some(cmd) = std::env::var_os("ATERM_EXEC") {
            let script = format!(
                "{}; exec {}",
                cmd.to_string_lossy(),
                shell.to_string_lossy()
            );
            let argv = vec![
                cshell.clone(),
                CString::new("-c").unwrap(),
                CString::new(script).unwrap_or_else(|_| CString::new("true").unwrap()),
            ];
            (cshell.clone(), argv)
        } else {
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            let mut argv0 = std::ffi::OsString::from("-");
            argv0.push(base);
            let argv = vec![CString::new(argv0.as_bytes()).unwrap_or_else(|_| cshell.clone())];
            (cshell.clone(), argv)
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
    let (status_rd, status_wr, status_carrier) = open_exec_status_channel()?;

    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Explicit slave termios (kernel defaults + IUTF8 + B230400 — see
    // `build_spawn_termios`); `None` = the historical NULL path. Built here in
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
    // The fork `forkpty` used to do for us. Everything the child needs was
    // pre-built above; the child branch below is async-signal-safe only.
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
        // (0a) ACQUIRE THE CONTROLLING TERMINAL — the step `forkpty` used to
        //     perform for us, explicit now that the pair above is opened here.
        //     `login_tty` is, VERIFIED by disassembly of libsystem_c on macOS
        //     26.5.1: `setsid()`; `ioctl(fd, TIOCSCTTY, NULL)` (→ -1 on
        //     failure); `dup2(fd,0)`; `dup2(fd,1)`; `dup2(fd,2)`; `close(fd)`
        //     when fd >= 3. Every one of those is async-signal-safe — no
        //     allocation, no locks, no libc state — so it is legal in this
        //     window. It is what makes the child a SESSION LEADER with
        //     pid == sid == pgid, the identity `hangup`'s `killpg`, `reap`'s
        //     `getpgid` check, and the GUI's `tcgetpgrp(master)` all depend on.
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
        //     This step only became REAL work with the `forkpty` replacement.
        //     `forkpty` assigns the master out-param in its PARENT branch only —
        //     MEASURED: the child read back `master == -1` — and closes the
        //     child's copy itself, inside libc, before `login_tty`. So the call
        //     that used to stand here was `close(-1)`: an EBADF no-op standing in
        //     for work libc had already done. It now closes the actual fd.
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
    // PARENT. Drop our copy of the SLAVE immediately — this is the `close(slave)`
    // that used to live inside `forkpty`'s parent branch, and it is not
    // optional: the child holds the slave as its controlling terminal and its
    // stdio, and a copy left open HERE would hold the pty open for as long as
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
    // BOUNDED wait for the verdict, where this used to be an unbounded blocking
    // read. The bound is defence in depth, not the fix — the carrier above is
    // what makes an unbounded wait unreachable in the first place — but it means
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
            "child failed to exec the shell before exec (_exit(127))".to_owned(),
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
                "child never reported exec status within {EXEC_STATUS_BUDGET:?} \
                 (fail-closed: no confirmed exec, so no master is handed back; \
                 status carrier: {status_carrier:?})"
            ),
        )),
        ExecStatus::ChannelBroke(err) => Some((
            io::ErrorKind::Other,
            format!(
                "exec-status channel failed before the child reported ({err}); fail-closed \
                 (status carrier: {status_carrier:?})"
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
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    // SAFETY: `killpg` with a signal merely posts SIGHUP to the process group;
    // `pid` is the session-leader pid the spawn returned (`login_tty` = setsid),
    // so the group is the
    // child's own job tree. Any error (already-reaped child) is ignored.
    unsafe {
        libc::killpg(pid, libc::SIGHUP);
    }
}

/// Reap an exited child WITHOUT ever blocking unboundedly. Runs on the detached
/// teardown thread AFTER [`hangup`] (the UI thread has already moved on). A
/// well-behaved child exits on SIGHUP within milliseconds and is reaped on the first
/// poll. The hazard this guards against: a child that TRAPS or ignores SIGHUP (e.g.
/// `trap '' HUP`, or one wedged in uninterruptible D-state) would leave a plain
/// blocking `waitpid(…, 0)` parked here FOREVER — one leaked thread (and the
/// fd/process slot it pins) per such mid-run close. So poll `WNOHANG`: escalate to an
/// unignorable SIGKILL after a short grace, and after a hard deadline give up and
/// return, leaving the kernel to reap the orphan at process exit. Keeps the child
/// from lingering as a zombie in the common case. Best-effort; a no-op for a
/// non-positive pid.
///
/// NOT-OUR-CHILD SHELLS (the overlap handoff): a seamlessly ADOPTED session's
/// shell is a child of the PREVIOUS aterm process — after the overlap swap it
/// reparents to launchd, so `waitpid` here answers `ECHILD` on the FIRST tick.
/// Treating that as "already gone" (the old behaviour) skipped the SIGKILL
/// escalation entirely: a HUP-trapping shell closed via Cmd-W after an update
/// was never force-killed. `ECHILD` now switches to a signal-0 liveness poll
/// with the SAME grace/escalation schedule (launchd reaps the corpse, so no
/// zombie risk on this path — only the escalation matters).
pub fn reap(pid: i32) {
    if pid <= 1 {
        return;
    }
    // ~2 s budget: poll every 10 ms (200 ticks). A SIGHUP-ignoring holdout is
    // SIGKILLed at ~250 ms, so the common case returns on the first poll and the
    // pathological case is still bounded.
    const POLL: std::time::Duration = std::time::Duration::from_millis(10);
    const KILL_AT: u32 = 25;
    const DEADLINE: u32 = 200;
    let mut status: libc::c_int = 0;
    for tick in 0..DEADLINE {
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param.
        // Returns the pid when reaped, 0 if still running as our child, -1
        // (`ECHILD`) when it is not our child — either already reaped, or an
        // ADOPTED shell parented to launchd.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return; // reaped
        }
        let mut not_ours = false;
        if r == -1 {
            not_ours = true;
            // Not our child. IDENTITY CHECK before anything else: an adopted
            // shell is a SESSION LEADER (login_tty = setsid), so its pgid == its
            // pid. A recycled pid (the shell died and the kernel reissued the
            // number) is overwhelmingly NOT a fresh group leader — treating
            // `getpgid` mismatch (or `ESRCH`) as "gone" makes the escalation
            // below unable to SIGKILL an innocent bystander's process group.
            // SAFETY: getpgid is a read-only probe.
            if unsafe { libc::getpgid(pid) } != pid {
                return; // dead (ESRCH) or a recycled non-leader pid
            }
        }
        if tick == KILL_AT {
            // Still alive past the grace ⇒ it ignored SIGHUP. SIGKILL the group.
            // Re-verify the not-our-child identity IMMEDIATELY before the kill
            // (the per-tick probe above may be up to one tick stale).
            // SAFETY: read-only probe + best-effort signal post to the child's
            // own process group.
            if !not_ours || unsafe { libc::getpgid(pid) } == pid {
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
        std::thread::sleep(POLL);
    }
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
/// [`spawn_shell`]).
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

/// PATH-resolve a `-e` program name to an absolute path, IN THE PARENT (the child
/// must stay async-signal-safe, so it cannot do its own `execvp` PATH search). A
/// name containing `/` is used verbatim (an explicit path). Otherwise each `$PATH`
/// entry is probed for an executable regular file. Falls back to the name verbatim
/// when nothing matches, so `execve` fails cleanly (`_exit(127)`) instead of this
/// resolver masking a not-found command.
// Skip: `CString::new` panics only on allocation (interior NUL returns
// Err) — the audited-alloc class; the argv strings are bounded by the
// config. Droppable when the CString totality entry lands.
#[cfg_attr(trust_verify, trust::skip)]
fn resolve_program(name: &str) -> CString {
    // The interior-NUL fallback is a compile-time `c"..."` literal (NUL-free by
    // construction), not a runtime `CString::new(..).unwrap()`: same bytes, no
    // panic path — behavior-identical while discharging the Trust unwrap
    // panic-boundary obligations.
    let verbatim = || CString::new(name.as_bytes()).unwrap_or_else(|_| c"/nonexistent".to_owned());
    if name.is_empty() || name.contains('/') {
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

/// Like [`write_some`], but reports ONLY the byte count the kernel accepted and
/// NEVER yields an [`io::Error`]: every error collapses to `0`, `EINTR` is
/// retried internally. Built for the sink drain loop (`aterm-session`): returning
/// `usize` keeps any `io::Error` from crossing the call boundary into the loop,
/// so the loop has no opaque error value to drop — eliminating the `io::Error`
/// drop-glue gap that forced the loop's old skip'd `discard_write_error` helper.
/// Mapping vs [`write_some`]: `Ok(n) -> n`, `Ok(0) -> 0`, `Err(_) -> 0` —
/// behavior-identical for the drainer, which treats every non-positive result as
/// session-dead.
///
/// INVARIANT (load-bearing for `drain_loop`'s panic-freedom proof): the ONLY
/// `io::Error` this body may ever construct is `io::Error::last_os_error()` (the
/// `Os(errno)` variant), and it is born AND dropped inside this body. That drop
/// is trivially total — no boxed `Custom` payload, so no user `Drop` runs. Do NOT
/// introduce `io::Error::new`/`io::Error::other`/a `?`-propagated error here: a
/// `Custom` variant runs arbitrary user `Drop` (may panic) and would silently
/// regress the drain loop from PROVED back to a coverage gap.
// #[inline] so the MIR crosses the crate boundary (the `write_some`/`dup_fd`
// precedent): aterm-session's drain loop bundles and VERIFIES this body rather
// than assuming an absent callee.
#[inline]
pub fn write_some_count(master: i32, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes (identical to `write_some`).
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            // `last_os_error()` is the `Os(errno)` variant, constructed AND dropped
            // right here — no boxed `Custom` payload, so the drop is total. Only
            // `EINTR` retries; every other error collapses to `0` (session-dead).
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return 0;
        }
        return r as usize;
    }
}

/// [`write_some`] with BLOCKING semantics restored on an `O_NONBLOCK` master: on
/// `WouldBlock` it parks in `poll(POLLOUT)` and retries. The direct-read gather
/// flips the master's file description non-blocking (`O_NONBLOCK` is
/// per-DESCRIPTION, so a `dup` cannot isolate it) — every writer that previously
/// relied on the kernel parking a write into a full tty input queue (reply
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

/// [`write_some_count`] with BLOCKING semantics restored on an `O_NONBLOCK`
/// master (the [`write_some_blocking`] twin for the sink's spill drain loop):
/// `EAGAIN` parks in `poll(POLLOUT)` and retries instead of collapsing to `0` —
/// without this the drainer would misread a full-but-alive tty input queue (the
/// wedged foreground it EXISTS to absorb) as session-dead and drop the spill.
/// All other errors still collapse to `0`; `EINTR` retries internally.
///
/// INVARIANT (the `write_some_count` drop discipline): every `io::Error` touched
/// here is the `Os(errno)` variant, born in this body or in [`poll_writable`],
/// and dropped HERE — a trivially total drop with no boxed `Custom` payload.
// #[inline] so the MIR crosses the crate boundary (the `write_some_count`
// precedent): aterm-session's drain loop bundles and VERIFIES this body.
#[inline]
pub fn write_some_count_blocking(master: i32, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes (identical to `write_some`).
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r >= 0 {
            return r as usize;
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                // Full-but-alive input queue: park for room, then retry. A broken
                // poll collapses to 0 (session-dead), like any other hard error.
                if poll_writable(master, -1).is_err() {
                    return 0;
                }
            }
            _ => return 0,
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
/// free of any opaque `io::Error` drop (the `write_some_count` idiom, generalized to a
/// path that must still surface `WouldBlock` and the real error).
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
pub fn make_wake_pipe() -> Option<(i32, i32)> {
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

/// Revert knob (`ATERM_PTY_BRIDGE_SPIN=1`): the legacy paced-probe bridge
/// (256 `spin_loop` pauses between re-reads, deliver once the probe budget is
/// spent). Kept because the bridge shape is a measured-perf hot spot; measured
/// on M5 Max, the pause block costs ~2 µs (`spin_loop` lowers to `ISB` on
/// Apple Silicon), 4× its design assumption — the reason the legacy bridge
/// lost ~100 MB/s of drain rate.
fn bridge_legacy_spin() -> bool {
    static LEGACY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEGACY.get_or_init(|| std::env::var_os("ATERM_PTY_BRIDGE_SPIN").is_some_and(|v| v == "1"))
}

/// Default idle-refill patience. Measured sweep (quiet M5 Max, 500 MB flood +
/// CPR round-trip probe): 0 ⇒ 231 MB/s / +0 µs; 50 ⇒ 266 / +79 µs p50;
/// 250 ⇒ 265 / +327 µs; 1000 ⇒ 254 / +1.27 ms (the bb0ac4c fps-regression
/// class). 50 µs recovers the full cutoff-churn loss at an invisible query
/// tax — and IMPROVES p99 (679 vs 1385 µs: fewer, fuller deliveries).
const IDLE_POLL_DEFAULT_US: u32 = 50;

/// Idle-cutoff hysteresis (`ATERM_PTY_IDLE_POLL_US=<µs>`): how long a dry,
/// parser-idle gap may wait for a refill before delivering. An immediate
/// cutoff (0) protects the fps/request-response class (the ghostty bb0ac4c
/// regression) but its small-batch churn costs ~33-43 MB/s of cat-flood
/// throughput; [`IDLE_POLL_DEFAULT_US`] is the measured sweet spot. `0`
/// restores the immediate cutoff. Read once; unparsable values mean the
/// default. Clamped to [`IDLE_POLL_CLAMP_US`]: a wait beyond the batch
/// budget is meaningless, and on the wake-pipe-less fallback path an
/// unbounded select would stall teardown for the full value.
fn idle_poll_us() -> u32 {
    /// Upper clamp — just past [`BATCH_BUDGET`]'s 3 ms.
    const IDLE_POLL_CLAMP_US: u32 = 5_000;
    static IDLE_POLL_US: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *IDLE_POLL_US.get_or_init(|| {
        std::env::var("ATERM_PTY_IDLE_POLL_US")
            .ok()
            .and_then(|v| v.trim().parse().ok())
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

/// Park for the writer's next refill: one bounded poll on `[master, wake_rd]`.
/// A pending wake byte is deliberately NOT consumed — [`read_or_wake`] owns the
/// wake protocol and must observe it after the batch is delivered.
fn bridge_poll(master: i32, wake_rd: i32, timeout_ms: i32) -> BridgeWait {
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
/// `ATERM_PTY_IDLE_POLL_US`, default [`IDLE_POLL_DEFAULT_US`]): a dry idle
/// gap waits up to that many µs for a refill before delivering — the
/// immediate cutoff's small-batch churn costs ~33-43 MB/s of flood
/// throughput; `0` restores immediate delivery.
pub fn drain_more_nonblocking(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
) -> usize {
    drain_more_nonblocking_with_idle_wait(
        master,
        buf,
        filled,
        wake_rd,
        parse_in_flight,
        idle_poll_us(),
    )
}

/// [`drain_more_nonblocking`] body with the idle-cutoff hysteresis as an
/// explicit parameter: the [`idle_poll_us`] OnceLock caches the env once per
/// process, so tests needing different knob values drive this directly.
fn drain_more_nonblocking_with_idle_wait(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    idle_wait_us: u32,
) -> usize {
    drain_more_nonblocking_with_idle_wait_after_gap(
        master,
        buf,
        filled,
        wake_rd,
        parse_in_flight,
        idle_wait_us,
        || {},
    )
}

/// Shared gather body. `before_gap_park` is a deterministic test seam invoked
/// after the immediate probes are exhausted and immediately before the drain
/// PARKS on a dry gap — either the armed parser-idle wait (parser idle) or the
/// `BRIDGE_POLL_MS` bridge poll (parser busy). Exactly one of those two parks
/// runs per dry gap, so the seam fires exactly once per gap on whichever path is
/// taken. That is what lets a test inject a refill that PROVABLY lands inside
/// the park, instead of racing a sleep against it. The shipping wrapper supplies
/// an inlined no-op.
fn drain_more_nonblocking_with_idle_wait_after_gap(
    master: i32,
    buf: &mut [u8],
    mut filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    idle_wait_us: u32,
    mut before_gap_park: impl FnMut(),
) -> usize {
    /// One kernel tty output queue's worth — at/above this the writer saturated it.
    const SATURATED: usize = 1024;
    /// Immediate re-read budget per refill gap (ghostty's bridge length; 8-16
    /// catches >90% of gaps).
    const NB_SPIN_MAX: u32 = 16;
    /// Legacy paced-probe pause length (revert knob only).
    const NB_SPIN_ITERS: u32 = 256;
    /// One parked poll per dry gap; quiet for the full timeout ⇒ burst over.
    const BRIDGE_POLL_MS: i32 = 1;
    /// Per-batch latency budget (ghostty's); checked per dry gap, so the
    /// effective bound is this plus one [`BRIDGE_POLL_MS`] park.
    const BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);
    let legacy = bridge_legacy_spin();
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
                // Quiet. Interactive (< one outq) delivers NOW; over-budget
                // batches deliver regardless of bridge luck.
                if filled < SATURATED || start.elapsed() >= BATCH_BUDGET {
                    break;
                }
                if spins < NB_SPIN_MAX {
                    spins += 1;
                    if legacy {
                        for _ in 0..NB_SPIN_ITERS {
                            std::hint::spin_loop();
                        }
                    }
                    continue;
                }
                if legacy {
                    break; // legacy bridge delivered once the probe budget ran dry
                }
                if parse_in_flight
                    .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
                {
                    if idle_wait_us == 0 {
                        break; // parser idle: bridging would add pure output latency
                    }
                    // Hysteresis: a µs-bounded wait for the refill instead of
                    // delivering a churn-sized batch (BATCH_BUDGET still caps
                    // the whole gather — it is re-checked per dry gap).
                    before_gap_park();
                    match idle_refill_wait(master, wake_rd, idle_wait_us) {
                        BridgeWait::Refill => {
                            spins = 0;
                            continue;
                        }
                        BridgeWait::Deliver => break,
                    }
                }
                before_gap_park();
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
/// PTY is a zero-process byte queue with no starvable middlemen, and the
/// scheduler's own interactivity wake-up boosts already protect the shell's
/// echo; the real implementation (ConPTY conhost + shell root,
/// `SetPriorityClass` + power-throttling) lives in `src/windows/mod.rs`.
pub fn set_focus_boost(_master: i32, _on: bool) {}

/// Resize the PTY to `rows`×`cols` (`TIOCSWINSZ`).
pub fn resize(master: i32, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `master` is a valid PTY master fd; `&ws` is a valid `winsize` for
    // the `TIOCSWINSZ` ioctl.
    unsafe {
        libc::ioctl(master, libc::TIOCSWINSZ, &ws);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pipe_is_cloexec_and_nonblocking_on_both_ends() {
        let (rd, wr) = make_wake_pipe().expect("wake pipe");
        // SAFETY: read-only fcntl queries on two live fds owned by this test.
        unsafe {
            for fd in [rd, wr] {
                assert_ne!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
                assert_ne!(libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);
            }
        }
        close_fd(rd);
        close_fd(wr);
    }

    #[test]
    fn saturated_wake_pipe_never_blocks_the_caller() {
        let (rd, wr) = make_wake_pipe().expect("wake pipe");
        let fill = [0x55u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            let wrote = unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) };
            if wrote < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock,
                    "pipe saturation must report EAGAIN"
                );
                break;
            }
        }
        let started = std::time::Instant::now();
        wake(wr);
        assert!(
            // A genuine failure blocks FOREVER on a full pipe with no reader, so any
            // finite bound catches it. 20ms sits inside one scheduler quantum and was
            // measuring preemption, not the never-blocks property.
            started.elapsed() < std::time::Duration::from_secs(5),
            "a redundant wake against a full pipe must return promptly"
        );
        close_fd(rd);
        close_fd(wr);
    }

    /// MEM-L2: a reader parked in `read_or_wake` on a master that STAYS OPEN with no
    /// data and never EOFs (the orphaned-child-holds-the-slave scenario) must still
    /// unblock the instant the owner fires the wake pipe — otherwise `Session::drop`
    /// could never reclaim the reader (and the Terminal + master fd it pins).
    #[test]
    fn read_or_wake_unblocks_on_wake_even_when_master_never_eofs() {
        use std::sync::mpsc;
        use std::time::Duration;
        // A pipe stands in for a master held open with no data: its write end lives for
        // the whole test, so the read end never sees data OR EOF.
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (master_rd, master_wr) = (m[0], m[1]);
        let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");

        let (tx, rx) = mpsc::channel();
        // Publish entry so the negative below cannot pass merely because the thread
        // was never scheduled: a 200ms `recv_timeout` is well inside a scheduling
        // delay on a loaded box, and without this a `read_or_wake` that busy-returned
        // immediately would still look like "stayed parked".
        let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_entered = entered.clone();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            reader_entered.store(true, std::sync::atomic::Ordering::SeqCst);
            let out = read_or_wake(master_rd, &mut buf, wake_rd);
            tx.send(matches!(out, ReadOutcome::Wake)).unwrap();
            close_fd(wake_rd);
        });
        while !entered.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }

        // Neither fd is ready ⇒ the reader must stay parked (not busy-return).
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "reader must block while master has no data and the wake pipe is empty"
        );

        // Fire the wake: the reader must return `Wake` promptly even though `master_rd`
        // never saw data or EOF (`master_wr` is still open).
        wake(wake_wr);
        let got_wake = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader must unblock promptly once the wake pipe fires");
        assert!(got_wake, "the wake pipe must yield ReadOutcome::Wake");

        h.join().unwrap();
        close_fd(wake_wr);
        // SAFETY: closing the two pipe fds this test owns.
        unsafe {
            libc::close(master_rd);
            libc::close(master_wr);
        }
    }

    /// Direct-read drain (O_NONBLOCK gather): an interactive-sized burst (< one
    /// outq) must be delivered on the FIRST quiet read — no spin bridge, no
    /// parking — and gather exactly the bytes written.
    #[test]
    fn drain_more_nonblocking_delivers_interactive_burst_on_first_quiet_read() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(unsafe { libc::write(wr, b"echo".as_ptr().cast(), 4) }, 4);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1]); // stand-in for the read_or_wake first chunk
        assert_eq!(n, 1);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking(rd, &mut buf, 1, -1, None);
        assert_eq!(filled, 4, "must gather the whole burst");
        assert_eq!(&buf[..4], b"echo");
        assert!(
            // Failure bound only: the regressed path parks (unbounded), so any finite
            // deadline discriminates. 50ms sits inside a single scheduler quantum on a
            // loaded box and was measuring preemption, not the latency property. The
            // structural assertions beside this one (byte counts, wake-byte
            // preservation) are what actually prove the behaviour.
            t0.elapsed() < std::time::Duration::from_secs(2),
            "interactive drain must return on the first quiet read, not spin/park"
        );
        close_fd(rd);
        close_fd(wr);
    }

    /// Direct-read drain: a saturated burst (>= one outq) is gathered fully into
    /// one batch, and EOF mid-drain just stops the top-up (kept bytes intact).
    #[test]
    fn drain_more_nonblocking_gathers_saturated_burst_and_survives_eof() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0xA5u8; 8192];
        // SAFETY: bounded write to this test's live pipe end (8 KiB fits the
        // default pipe buffer).
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        close_fd(wr); // EOF after the burst: the drain must keep what it read
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, None);
        assert_eq!(
            filled,
            payload.len(),
            "must gather the full saturated burst"
        );
        assert!(buf[..filled].iter().all(|&b| b == 0xA5));
        close_fd(rd);
    }

    /// Bridge teardown contract: a wake byte pending during the bridge poll
    /// makes the drain deliver what it has WITHOUT consuming the byte —
    /// `read_or_wake` owns the wake protocol and must still observe it.
    #[test]
    fn drain_more_nonblocking_bridge_yields_to_wake_and_leaves_the_byte() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0x5Au8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let mut wk = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
        let (wake_rd, wake_wr) = (wk[0], wk[1]);
        wake(wake_wr); // teardown byte queued BEFORE the drain bridges
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, wake_rd, None);
        assert_eq!(filled, payload.len(), "keeps the drained burst");
        let mut b = [0u8; 4];
        // SAFETY: bounded read from this test's live wake pipe end.
        let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
        assert_eq!(wn, 1, "bridge must NOT consume the wake byte");
        close_fd(rd);
        close_fd(wr);
        close_fd(wake_rd);
        close_fd(wake_wr);
    }

    /// Refill continuation — THE flood-throughput mechanism (181→226 MB/s):
    /// with the parse stage BUSY (`in_flight > 0`), a refill landing during the
    /// bridge poll must be gathered into the SAME batch, not delivered as a
    /// partial. Also pins the idle-cutoff's sign: inverting `== 0` would
    /// deliver at the first dry gap and this test would see only chunk 1.
    /// DETERMINISTIC: the refill is injected FROM the drain's own dry-gap seam,
    /// so it provably lands inside the bridge park. No sleep, no retry, no
    /// scheduling assumption — a failure here is a real regression.
    ///
    /// This test used to race a 300 µs sleep against the 1 ms `BRIDGE_POLL_MS`
    /// window and retry until one attempt happened to land, which failed a
    /// full-workspace run. The seam existed already but fired only on the
    /// parser-IDLE path; the busy-parser bridge park had none, which is why the
    /// test had to guess. Firing it before both parks is the actual fix.
    #[test]
    fn drain_more_nonblocking_busy_parser_bridges_refill_into_same_batch() {
        use std::sync::atomic::AtomicUsize;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0xC3u8; 2048];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );

        let busy = AtomicUsize::new(1); // parser busy ⇒ the bridge parks, not the idle wait
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut injected = false;
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&busy),
            // 0 = the idle path's immediate-deliver cutoff, which returns BEFORE
            // the seam. That is what keeps the idle-cutoff SIGN pinned: invert
            // `== 0` and this busy parser takes the idle branch, breaks at the
            // cutoff, and never fires the seam — caught by `injected` below.
            0,
            || {
                // The drain has exhausted its probes and is about to park on a
                // dry gap. Refill exactly here: a bridge that continues gathers
                // it into THIS batch; a broken one has already delivered.
                if injected {
                    return;
                }
                injected = true;
                // SAFETY: bounded write to this test's live pipe end.
                assert_eq!(
                    unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize
                );
            },
        );
        close_fd(wr);
        close_fd(rd);
        assert!(
            injected,
            "the drain must reach the BUSY-parser bridge park and fire the seam \
             (a parser-busy drain that took the idle branch means the cutoff's \
             sign is inverted)"
        );
        assert_eq!(
            filled,
            2 * chunk.len(),
            "a busy-parser bridge must continue the batch across a refill gap"
        );
    }

    /// Parser-idle cutoff (the ghostty fps-fire class): with `in_flight == 0`
    /// and the stream dry, the drain must deliver after the probe burst —
    /// never park in the 1 ms bridge poll. A regressed cutoff parks every
    /// attempt (≥1 ms); the µs-path passing once proves the cutoff. Runs both
    /// the public fn (env unset ⇒ the measured `IDLE_POLL_DEFAULT_US` = 50 µs
    /// wait, still far under this test's 1 ms park bound) and the inner body
    /// with an explicit 0 (the immediate-deliver revert path).
    ///
    /// ATTEMPT BUDGET: the assertion is "at least one attempt delivers inside the
    /// idle wait", so a single clean run proves the cutoff and only a REGRESSED
    /// cutoff — which parks ≥1 ms on EVERY attempt — can exhaust the budget. The
    /// budget is therefore about scheduler noise, not about the property, and 3
    /// was too few: an unrelated CPU-hungry test running in parallel can push all
    /// three attempts past a 1 ms bound (OBSERVED, ~1 run in 20-40, once this
    /// file gained a test that forks 300 times against 6 busy threads). Raising
    /// it makes the test robust to that without weakening it by one bit — a real
    /// regression still fails all 12 (VERIFIED by removing the cutoff: caught
    /// 6/6).
    #[test]
    fn drain_more_nonblocking_idle_parser_delivers_without_parking() {
        use std::sync::atomic::AtomicUsize;
        for explicit_zero in [false, true] {
            let mut ok = false;
            for _ in 0..12 {
                let mut m = [0i32; 2];
                // SAFETY: valid 2-int out-array for pipe(2).
                assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
                let (rd, wr) = (m[0], m[1]);
                set_nonblocking(rd, true).expect("nonblock read end");
                let chunk = [0x7Eu8; 4096];
                // SAFETY: bounded write to this test's live pipe end.
                assert_eq!(
                    unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize
                );
                let idle = AtomicUsize::new(0);
                let mut buf = [0u8; 65_536];
                let n = read(rd, &mut buf[..1024]);
                assert!(n > 0);
                let t0 = std::time::Instant::now();
                let filled = if explicit_zero {
                    drain_more_nonblocking_with_idle_wait(
                        rd,
                        &mut buf,
                        n as usize,
                        -1,
                        Some(&idle),
                        0,
                    )
                } else {
                    drain_more_nonblocking(rd, &mut buf, n as usize, -1, Some(&idle))
                };
                let el = t0.elapsed();
                close_fd(rd);
                close_fd(wr);
                assert_eq!(filled, chunk.len(), "keeps the drained burst");
                // The public path folds in the CONFIGURED idle wait (env
                // knob or the default, clamped) so an exported
                // ATERM_PTY_IDLE_POLL_US cannot fail this test spuriously.
                let bound = if explicit_zero {
                    std::time::Duration::from_millis(1)
                } else {
                    std::time::Duration::from_millis(1)
                        + std::time::Duration::from_micros(u64::from(idle_poll_us()))
                };
                if el < bound {
                    ok = true;
                    break;
                }
            }
            assert!(
                ok,
                "an idle-parser dry gap must deliver within its idle wait, not \
                 park the 1ms bridge poll (explicit_zero={explicit_zero}, \
                 configured idle wait {}µs)",
                idle_poll_us()
            );
        }
    }

    /// Pins the SHIPPED default from below: [`IDLE_POLL_DEFAULT_US`] must be
    /// armed (> 0) and must actually bridge a refill that lands AFTER the
    /// immediate probe window, inside the default wait. The refill is injected
    /// synchronously at that exact seam: a sub-50µs cross-thread deadline would
    /// test scheduler luck under load rather than the gather protocol.
    #[test]
    fn drain_more_nonblocking_default_idle_wait_is_armed_and_bridges() {
        use std::sync::atomic::AtomicUsize;
        const {
            assert!(
                IDLE_POLL_DEFAULT_US > 0,
                "the measured idle-refill default must stay armed"
            );
        }
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        const CHUNK_LEN: usize = 2048;
        let chunk = [0xD6u8; CHUNK_LEN];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let idle = AtomicUsize::new(0);
        // Bound the gather to exactly the initial chunk plus the injected
        // refill. Sibling fork/PTY tests can briefly inherit raw descriptors,
        // so an EOF-driven test races their lifetime; reaching capacity after
        // exactly two chunks terminates deterministically.
        let mut buf = [0u8; 2 * CHUNK_LEN];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut injected = false;
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&idle),
            IDLE_POLL_DEFAULT_US,
            || {
                assert!(!injected, "the refill must be injected exactly once");
                // SAFETY: bounded write to this test's live pipe end.
                assert_eq!(
                    unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize
                );
                injected = true;
            },
        );
        close_fd(wr);
        close_fd(rd);
        assert!(
            injected,
            "the armed default must reach the parser-idle wait after its probes"
        );
        assert_eq!(
            filled,
            2 * chunk.len(),
            "the default idle wait must bridge the post-probe refill into the same batch"
        );
    }

    /// Hysteresis knob (`ATERM_PTY_IDLE_POLL_US` > 0): an idle parser at a dry
    /// gap WAITS for the writer's refill instead of delivering, so the refill
    /// lands in the SAME batch. 100 ms is deliberately huge so scheduling can
    /// never expire the wait before the +2 ms refill; the writer's close (EOF)
    /// ends the batch. Retry damps scheduling flakes: a knob-off cutoff NEVER
    /// continues the batch, so any passing attempt proves the hysteresis.
    #[test]
    fn drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch() {
        use std::sync::atomic::AtomicUsize;
        let mut ok = false;
        for _ in 0..3 {
            let mut m = [0i32; 2];
            // SAFETY: valid 2-int out-array for pipe(2).
            assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
            let (rd, wr) = (m[0], m[1]);
            set_nonblocking(rd, true).expect("nonblock read end");
            let chunk = [0xB4u8; 2048];
            // SAFETY: bounded write to this test's live pipe end.
            assert_eq!(
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                chunk.len() as isize
            );
            let writer = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(2));
                // SAFETY: bounded write to the pipe end this thread owns.
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) };
                close_fd(wr);
            });
            let idle = AtomicUsize::new(0);
            let mut buf = [0u8; 65_536];
            let n = read(rd, &mut buf[..1024]);
            assert!(n > 0);
            let filled = drain_more_nonblocking_with_idle_wait(
                rd,
                &mut buf,
                n as usize,
                -1,
                Some(&idle),
                100_000,
            );
            writer.join().unwrap();
            close_fd(rd);
            if filled == 2 * chunk.len() {
                ok = true;
                break;
            }
        }
        assert!(
            ok,
            "an armed idle wait must gather the refill into the same batch"
        );
    }

    /// Hysteresis + teardown: a wake byte pending when the idle wait parks
    /// must deliver promptly WITHOUT consuming the byte — [`read_or_wake`]
    /// owns the wake protocol (same contract as the busy-parser bridge poll).
    #[test]
    fn drain_more_nonblocking_idle_wait_yields_to_wake_and_leaves_the_byte() {
        use std::sync::atomic::AtomicUsize;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0x9Cu8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let mut wk = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
        let (wake_rd, wake_wr) = (wk[0], wk[1]);
        wake(wake_wr); // teardown byte queued BEFORE the idle wait parks
        let idle = AtomicUsize::new(0);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking_with_idle_wait(
            rd,
            &mut buf,
            n as usize,
            wake_rd,
            Some(&idle),
            100_000,
        );
        assert_eq!(filled, payload.len(), "keeps the drained burst");
        assert!(
            // Failure bound only: the regressed path parks (unbounded), so any finite
            // deadline discriminates. 50ms sits inside a single scheduler quantum on a
            // loaded box and was measuring preemption, not the latency property. The
            // structural assertions beside this one (byte counts, wake-byte
            // preservation) are what actually prove the behaviour.
            t0.elapsed() < std::time::Duration::from_secs(2),
            "a queued wake must deliver promptly, not park the full idle wait"
        );
        let mut b = [0u8; 4];
        // Non-blocking: a CONSUMED wake byte must fail the assert below, not
        // hang this read forever.
        set_nonblocking(wake_rd, true).expect("nonblock wake read end");
        // SAFETY: bounded read from this test's live wake pipe end.
        let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
        assert_eq!(wn, 1, "idle wait must NOT consume the wake byte");
        close_fd(rd);
        close_fd(wr);
        close_fd(wake_rd);
        close_fd(wake_wr);
    }

    /// Producer death mid-bridge: the write end closing while the drain is
    /// parked in the bridge poll must deliver promptly with what it has
    /// (via the POLLHUP Deliver arm, or the read()==0 EOF break one syscall
    /// later — both protocol-safe), never stall against a dead producer.
    #[test]
    fn drain_more_nonblocking_delivers_promptly_on_writer_hup() {
        use std::sync::atomic::AtomicUsize;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0x11u8; 2048];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_micros(300));
            close_fd(wr); // HUP lands while the drain bridges the dry gap
        });
        let busy = AtomicUsize::new(1); // busy parser: the bridge parks
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, Some(&busy));
        // Sample BEFORE the join: the join waits on a thread that sleeps 300us, so
        // reading `elapsed` after it folded another thread's scheduling into the
        // number this assertion is about.
        let drain_elapsed = t0.elapsed();
        closer.join().unwrap();
        assert_eq!(filled, chunk.len(), "keeps what was drained before HUP");
        assert!(
            // Failure bound only: the regressed path parks (unbounded), so any finite
            // deadline discriminates. 50ms sits inside a single scheduler quantum on a
            // loaded box and was measuring preemption, not the latency property. The
            // structural assertions beside this one (byte counts, wake-byte
            // preservation) are what actually prove the behaviour.
            drain_elapsed < std::time::Duration::from_secs(2),
            "a dead producer must deliver promptly, not stall the batch"
        );
        close_fd(rd);
    }

    /// Blocking-emulated write on an O_NONBLOCK description: a full queue must
    /// PARK (in poll) until the reader drains, then complete — never surface
    /// `WouldBlock` (the legacy blocking-master contract every frame writer keeps).
    #[test]
    fn write_some_blocking_parks_through_a_full_queue_instead_of_erroring() {
        use std::time::Duration;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(wr, true).expect("nonblock write end");
        // Fill the pipe to EAGAIN.
        let fill = [0u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            if unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) } < 0 {
                assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                break;
            }
        }
        // Drain the whole pipe from a helper thread after a beat, so the parked
        // writer has room for the retry.
        // The drainer's sleep alone used to be what made the writer park FIRST. If the
        // main thread is descheduled before its write, the drain lands first, the
        // write never parks, and the park-through-EAGAIN property under test simply
        // does not run — a vacuous pass. Gate the drain on the writer announcing
        // itself, and keep a short sleep as slack for the gap between the flag and
        // the actual park.
        let about_to_write = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drainer_gate = about_to_write.clone();
        let h = std::thread::spawn(move || {
            while !drainer_gate.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(50));
            let mut sink_buf = [0u8; 65_536];
            set_nonblocking(rd, true).expect("nonblock read end");
            loop {
                let n = read(rd, &mut sink_buf);
                if n <= 0 {
                    break;
                }
            }
            rd // keep rd open until after the writer completes
        });
        about_to_write.store(true, std::sync::atomic::Ordering::SeqCst);
        let wrote = write_some_blocking(wr, b"parked-frame").expect("must not surface WouldBlock");
        assert!(
            wrote > 0,
            "the retried write must land bytes after the drain"
        );
        let rd = h.join().unwrap();
        close_fd(rd);
        close_fd(wr);
    }

    /// The drain-loop twin: same park-through-EAGAIN contract, count-only shape —
    /// a full-but-alive queue must NOT collapse to 0 (0 = session-dead drops the spill).
    #[test]
    fn write_some_count_blocking_parks_instead_of_reporting_dead() {
        use std::time::Duration;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(wr, true).expect("nonblock write end");
        let fill = [0u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            if unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) } < 0 {
                break;
            }
        }
        // The drainer's sleep alone used to be what made the writer park FIRST. If the
        // main thread is descheduled before its write, the drain lands first, the
        // write never parks, and the park-through-EAGAIN property under test simply
        // does not run — a vacuous pass. Gate the drain on the writer announcing
        // itself, and keep a short sleep as slack for the gap between the flag and
        // the actual park.
        let about_to_write = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drainer_gate = about_to_write.clone();
        let h = std::thread::spawn(move || {
            while !drainer_gate.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(50));
            let mut sink_buf = [0u8; 65_536];
            set_nonblocking(rd, true).expect("nonblock read end");
            loop {
                let n = read(rd, &mut sink_buf);
                if n <= 0 {
                    break;
                }
            }
            rd
        });
        about_to_write.store(true, std::sync::atomic::Ordering::SeqCst);
        let wrote = write_some_count_blocking(wr, b"spill-chunk");
        assert!(
            wrote > 0,
            "a full-but-alive queue must park+retry, not report dead"
        );
        let rd = h.join().unwrap();
        close_fd(rd);
        close_fd(wr);
    }

    /// MEM-L2 fallback: when NO wake pipe exists (`wake_rd < 0`, the `make_wake_pipe`
    /// failed-under-fd-exhaustion path), a reader on a master that stays open with no data
    /// and never EOFs must NOT park forever — it must time out and return `Idle` so the
    /// owning session's fd-free stop flag can still tear it down. A bare blocking `read`
    /// here is exactly the silent MEM-L2 re-open this guards against.
    #[test]
    fn read_or_wake_wakeless_fallback_times_out_instead_of_parking() {
        use std::time::{Duration, Instant};
        // A pipe read end whose write end lives for the whole test stands in for an
        // orphan-held master: never readable, never EOF.
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (master_rd, master_wr) = (m[0], m[1]);

        let mut buf = [0u8; 16];
        let t0 = Instant::now();
        let out = read_or_wake(master_rd, &mut buf, -1);
        let elapsed = t0.elapsed();

        assert!(
            matches!(out, ReadOutcome::Idle),
            "wakeless fallback must return Idle on a quiet master, never block or read"
        );
        // It returned via the bounded poll timeout, not by parking indefinitely.
        assert!(
            elapsed < Duration::from_secs(2),
            "fallback must return within the poll timeout, not hang"
        );

        // SAFETY: closing the two pipe fds this test owns.
        unsafe {
            libc::close(master_rd);
            libc::close(master_wr);
        }
    }

    /// PROOF-CARRYING DSU (RFC Rung 1b) — the FD-handoff mechanism, proven with REAL
    /// syscalls: a PTY master survives `execve` IFF `FD_CLOEXEC` is cleared. Opens a
    /// real pty pair, and forks+execs a child that reports whether the inherited fd is
    /// open — the exact "the shell's master survives the seamless re-exec" contract.
    #[test]
    fn cloexec_controls_master_survival_across_exec() {
        // A real pty pair (no shell), from the seam's own opener rather than
        // `openpty`. The MASTER's flag is what this test toggles and asserts, so
        // the starting state is immaterial to it — but `openpty`'s SLAVE is
        // never close-on-exec, and this pair stays open for the whole test, so
        // using `openpty` here would park an inheritable pty slave in a
        // multi-threaded harness. Any concurrent `Command::spawn` (the sibling
        // leak-detection test does exactly that) would inherit it and keep that
        // pts alive after this test closed it — and when the device number was
        // recycled by the next test's session, the stranger's exit would hang up
        // THAT session. Measured: it did, killing an unrelated test's child with
        // SIGHUP ~1 run in 8. That is precisely the defect this file now fixes,
        // so the test scaffolding must not re-create it.
        let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");

        // fd_is_tty is a SANITY backstop, not a master discriminator: it accepts any
        // tty (master AND slave) and rejects the obviously-wrong (pipe / bad fd).
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let tty_master = fd_is_tty(master);
        let tty_slave = fd_is_tty(slave); // a slave IS a tty — documents the non-guarantee
        let tty_bad = fd_is_tty(-1);
        let tty_pipe = fd_is_tty(pipe[0]);

        // A tiny helper: fork+exec `/bin/test -e /dev/fd/<master>` and return whether
        // the fd was OPEN in the exec'd child (i.e. survived the exec). NOTE: libtest
        // runs tests on a THREAD POOL, so this process is multi-threaded at fork time.
        // That is safe here because the child touches ONLY async-signal-safe calls
        // (`execv`, `_exit`) before the image is replaced — it never allocates, locks,
        // or calls back into std. The parent reaps the child.
        let survived = |m: i32| -> bool {
            let path = std::ffi::CString::new("/bin/test").unwrap();
            let arg0 = std::ffi::CString::new("test").unwrap();
            let arg1 = std::ffi::CString::new("-e").unwrap();
            let arg2 = std::ffi::CString::new(format!("/dev/fd/{m}")).unwrap();
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                let argv = [
                    arg0.as_ptr(),
                    arg1.as_ptr(),
                    arg2.as_ptr(),
                    std::ptr::null(),
                ];
                unsafe {
                    libc::execv(path.as_ptr(), argv.as_ptr());
                    libc::_exit(127); // execv only returns on failure
                }
            }
            let mut status = 0i32;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            // WIFEXITED && WEXITSTATUS == 0  ⇒ `test -e` found the fd open.
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
        };

        // CLOEXEC set (the isolation default the spawn seam opens the master with) ⇒ the
        // master is CLOSED at exec (a naive re-exec would orphan the shell). Cleared ⇒
        // it SURVIVES (the seamless handoff's contract).
        set_cloexec(master, true).unwrap();
        let survives_when_cloexec = survived(master);
        set_cloexec(master, false).unwrap();
        let survives_when_cleared = survived(master);

        // The surviving master is still a live, usable pty (write master → read slave).
        set_nonblocking(slave, true).unwrap();
        write_all(master, b"ping\n");
        let mut buf = [0u8; 8];
        let mut got = 0isize;
        for _ in 0..1000 {
            got = read(slave, &mut buf);
            if got > 0 {
                break;
            }
        }

        // Close every fd BEFORE asserting, so a failed assertion can't leak them.
        for fd in [master, slave, pipe[0], pipe[1]] {
            unsafe { libc::close(fd) };
        }

        assert!(tty_master, "the master is a tty");
        assert!(
            tty_slave,
            "a pty SLAVE is also a tty (fd_is_tty is not master-only)"
        );
        assert!(!tty_bad, "a bad fd is not a tty");
        assert!(!tty_pipe, "a pipe end is not a tty");
        assert!(
            !survives_when_cloexec,
            "a CLOEXEC master does NOT survive exec"
        );
        assert!(
            survives_when_cleared,
            "a cleared-CLOEXEC master SURVIVES exec (seamless handoff)"
        );
        assert!(got > 0, "the re-adoptable master still drives its slave");
    }

    // Validates the `write_all` + `read` syscall wrappers on a real fd (a plain
    // pipe), so the seam's IO is exercised without spawning a shell (no flake, no
    // leftover process). `spawn_shell`/`resize` are exercised end-to-end by the
    // GUI that depends on this crate.
    #[test]
    fn write_all_then_read_roundtrips_on_a_pipe() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);

        write_all(wr, b"hello-pty-seam");
        let mut buf = [0u8; 64];
        let n = read(rd, &mut buf);
        assert!(n > 0, "read returned {n}");
        assert_eq!(&buf[..n as usize], b"hello-pty-seam");

        // SAFETY: closing the two fds we just opened.
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    // SEC-2: a confinement failure in the child must FAIL CLOSED. We force the
    // sandbox `apply` to fail by handing it an UNTRUSTED `Cap<Sandbox>` (its gate
    // requires Trusted+), so the child takes the `_exit(126)` path BEFORE exec and
    // the parent returns an error instead of a master fd for an unconfined shell.
    #[test]
    fn sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        // A valid spawn cap (passes the PARENT gate) but a too-weak sandbox cap
        // (fails the CHILD's `apply` gate) — exactly the silent-unconfined hole.
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);

        let result = spawn_shell(
            24,
            80,
            &spawn_cap,
            &weak_sandbox,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        );
        let err = result
            .expect_err("a sandbox confinement failure must surface as an error, NOT a master fd");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "child sandbox failure must be reported as PermissionDenied, got: {err}",
        );
        assert!(
            err.to_string().contains("fail-closed"),
            "error should describe the fail-closed confinement: {err}",
        );
    }

    // The success path still works: with a properly-tiered sandbox cap a real
    // `$SHELL` spawns and the parent gets a live master fd. Reading from it (the
    // shell's first prompt/banner, or at least the PTY echo) proves a process is
    // attached; then we close the master to tear the child down.
    #[test]
    fn normal_shell_spawns_with_a_trusted_sandbox_cap() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // Run a deterministic command then exit, so the test does not hang on an
        // interactive prompt: ATERM_EXEC makes the child run it, then exec $SHELL.
        // Using a bare `echo` + immediate close is enough to prove a live master.
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect("a normal shell must spawn with a Trusted sandbox cap");
        assert!(master >= 0, "master fd must be valid, got {master}");
        // Best-effort: write a harmless newline and read whatever echoes back, to
        // confirm the fd is a live PTY master, not a dangling descriptor.
        write_all(master, b"\n");
        let mut buf = [0u8; 64];
        let _ = read(master, &mut buf); // may be 0 if the child raced exit; fd is still valid
        // SAFETY: closing the master tears down the child's controlling tty.
        unsafe {
            libc::close(master);
        }
    }

    #[test]
    fn spawned_master_is_close_on_exec() {
        // REGRESSION: the forkpty master was returned WITHOUT FD_CLOEXEC, so a
        // later session's child shell would inherit (through execve) every prior
        // session's still-open master — an ungated cross-session input-injection
        // / output-exfiltration channel. The parent must mark the master
        // close-on-exec before handing it back.
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect("a normal shell must spawn with a Trusted sandbox cap");
        assert!(master >= 0, "master fd must be valid, got {master}");
        // SAFETY: querying fd flags on a valid fd.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD must succeed on the master fd");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "PTY master must be FD_CLOEXEC so it never leaks into a later session's shell"
        );
        // SAFETY: closing the master tears down the child's controlling tty.
        unsafe {
            libc::close(master);
        }
    }

    // ---- PTY pair creation: close-on-exec FROM BIRTH (the forkpty window) ----

    /// Read `fd`'s descriptor flags, failing the test if the fd is not live.
    fn fd_flags(fd: libc::c_int, what: &str) -> libc::c_int {
        // SAFETY: `F_GETFD` only reads the descriptor-flag word of `fd`.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(
            flags >= 0,
            "F_GETFD must succeed on {what} (fd {fd}): {}",
            io::Error::last_os_error()
        );
        flags
    }

    /// The winsize `fd` reports, as `(rows, cols)`.
    fn win_size(fd: libc::c_int, what: &str) -> (u16, u16) {
        // SAFETY: `TIOCGWINSZ` fills the zeroed, stack-owned out-param.
        unsafe {
            let mut w: libc::winsize = std::mem::zeroed();
            assert_eq!(
                libc::ioctl(fd, libc::TIOCGWINSZ, &mut w),
                0,
                "TIOCGWINSZ on {what} (fd {fd}): {}",
                io::Error::last_os_error()
            );
            (w.ws_row, w.ws_col)
        }
    }

    /// Does a byte written into `slave` come out of `master`?
    ///
    /// The ONLY observation that separates a real pty pair from two
    /// correctly-flagged descriptors onto DIFFERENT terminals. Bounded by a
    /// poll, so a broken pairing fails the test instead of hanging it.
    fn byte_crosses(slave: libc::c_int, master: libc::c_int, byte: u8) -> bool {
        // SAFETY: `slave` is a live pts fd the caller owns; `write` reads
        // exactly the one byte living at `byte`'s address.
        let wrote = unsafe { libc::write(slave, ptr::from_ref(&byte).cast(), 1) };
        if wrote != 1 {
            return false;
        }
        if poll_revents(master, std::time::Duration::from_millis(500)) & libc::POLLIN == 0 {
            return false;
        }
        let mut buf = [0u8; 16];
        // SAFETY: `read` fills at most `buf.len()` bytes of this live, owned
        // buffer, and we pass exactly that length.
        let got = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        let Ok(n) = usize::try_from(got) else {
            return false;
        };
        buf[..n].contains(&byte)
    }

    /// `poll(fd, POLLIN)` for at most `budget`, returning the revents once any
    /// arrive (or 0 on timeout). Mirrors `handoff_masters_closed`'s poll shape
    /// exactly, including `events: POLLIN` — with `events: 0` macOS reports
    /// nothing at all.
    fn poll_revents(fd: libc::c_int, budget: std::time::Duration) -> libc::c_short {
        let deadline = std::time::Instant::now() + budget;
        loop {
            let mut p = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: a single valid pollfd with a 20 ms timeout.
            let rc = unsafe { libc::poll(&mut p, 1, 20) };
            if rc > 0 {
                return p.revents;
            }
            if std::time::Instant::now() >= deadline {
                return 0;
            }
        }
    }

    /// THE PLATFORM FACT THE WHOLE SEAM RESTS ON: `O_CLOEXEC` passed to the
    /// `open(2)` that CREATES a pty descriptor is honored, so the descriptor is
    /// close-on-exec before it exists anywhere else. That is the only window-free
    /// way to get there — an `fcntl` afterwards leaves a gap in which a fork on
    /// another thread yields an exec-surviving copy.
    ///
    /// This is not redundant with the tests below: `open_pty_pair_cloexec` will
    /// SILENTLY fall back to `fcntl` (correct, but with the narrow window back)
    /// if a platform ever ignores the flag, and every other assertion here would
    /// still pass. This test is what says so out loud. Both halves are checked,
    /// so "the flag was set" cannot be trivially true of every descriptor.
    #[test]
    fn pty_open_honors_the_cloexec_open_flag() {
        // NEGATIVE CONTROL: the same call WITHOUT the flag must NOT be
        // close-on-exec. This is the state `openpty`/`forkpty` leave both ends
        // in, i.e. the defect itself, asserted rather than assumed.
        // SAFETY: `posix_openpt` takes only a flag word; both fds are closed below.
        let plain = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(
            plain >= 0,
            "posix_openpt failed: {}",
            io::Error::last_os_error()
        );
        // SAFETY: as above, with the flag added.
        let flagged =
            unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        assert!(
            flagged >= 0,
            "posix_openpt(O_CLOEXEC) failed — the seam would fall back to a racy \
             fcntl: {}",
            io::Error::last_os_error()
        );
        let plain_flags = fd_flags(plain, "master opened without O_CLOEXEC");
        let flagged_flags = fd_flags(flagged, "master opened with O_CLOEXEC");

        // Same question for the SLAVE open, which is the end `forkpty` never
        // flagged at all. Open the SAME pts twice off the flagged master.
        // SAFETY: `flagged` is a live /dev/ptmx fd; both are ioctls on it.
        assert_eq!(unsafe { libc::grantpt(flagged) }, 0, "grantpt");
        // SAFETY: as above.
        assert_eq!(unsafe { libc::unlockpt(flagged) }, 0, "unlockpt");
        let name = pts_name(flagged).expect("pts_name must resolve the slave path");
        // SAFETY: `name` is the NUL-terminated pts path just reported for `flagged`.
        let slave_plain = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
        // SAFETY: as above, with the flag added.
        let slave_flagged = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        assert!(
            slave_plain >= 0 && slave_flagged >= 0,
            "opening the pts failed"
        );
        let slave_plain_flags = fd_flags(slave_plain, "slave opened without O_CLOEXEC");
        let slave_flagged_flags = fd_flags(slave_flagged, "slave opened with O_CLOEXEC");

        // Close everything BEFORE asserting so a failure cannot leak descriptors.
        for fd in [plain, flagged, slave_plain, slave_flagged] {
            // SAFETY: each is a live fd this test opened and owns.
            unsafe { libc::close(fd) };
        }

        assert_eq!(
            plain_flags & libc::FD_CLOEXEC,
            0,
            "control: posix_openpt WITHOUT O_CLOEXEC must leave the master \
             inheritable — this is the forkpty state the seam replaces"
        );
        assert_ne!(
            flagged_flags & libc::FD_CLOEXEC,
            0,
            "posix_openpt must HONOR O_CLOEXEC — otherwise the master is only \
             close-on-exec after a follow-up fcntl, and a concurrent \
             fork+exec in that gap still inherits it"
        );
        assert_eq!(
            slave_plain_flags & libc::FD_CLOEXEC,
            0,
            "control: opening the pts WITHOUT O_CLOEXEC must leave it inheritable"
        );
        assert_ne!(
            slave_flagged_flags & libc::FD_CLOEXEC,
            0,
            "opening the pts must HONOR O_CLOEXEC — the slave is the end \
             forkpty never flagged at all"
        );
    }

    /// BOTH ends of the pair this seam hands the spawn are close-on-exec from the
    /// moment they exist, and the pair is a REAL, correctly-configured pty — not
    /// a pair of flags on nothing. The precondition is asserted alongside the
    /// property: two distinct live descriptors, both ttys, carrying the requested
    /// winsize and the requested termios deltas, readable from both ends.
    #[test]
    fn pty_pair_is_close_on_exec_from_birth() {
        let want = build_spawn_termios(
            kernel_default_termios().expect("kernel termios probe"),
            false,
        );
        let ws = libc::winsize {
            ws_row: 41,
            ws_col: 137,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (master, slave) =
            open_pty_pair_cloexec(Some(&want), Some(&ws)).expect("pty pair must open");

        // PRECONDITION — a real pair actually came into existence. Without this
        // the FD_CLOEXEC assertions could be true of two closed/bogus numbers.
        assert!(master >= 0 && slave >= 0, "both ends must be live fds");
        assert_ne!(master, slave, "the two ends must be distinct descriptors");
        let master_is_tty = fd_is_tty(master);
        let slave_is_tty = fd_is_tty(slave);
        let master_flags = fd_flags(master, "pty master");
        let slave_flags = fd_flags(slave, "pty slave");
        let slave_ws = win_size(slave, "pty slave");
        let master_ws = win_size(master, "pty master");
        // SAFETY: `tcgetattr` fills the zeroed out-param from the live slave.
        let slave_termios = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut t), 0, "tcgetattr(slave)");
            t
        };

        // NEGATIVE CONTROL, on this same machine and in this same process: the
        // `openpty` this replaced leaves BOTH ends inheritable. It is what makes
        // the assertions above non-trivial.
        let (mut om, mut os) = (-1i32, -1i32);
        // SAFETY: valid out-params; the null name/termios/winsize form. Both fds
        // are closed below.
        let orc = unsafe {
            libc::openpty(
                &mut om,
                &mut os,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        assert_eq!(orc, 0, "control openpty failed");
        let open_pty_master_flags = fd_flags(om, "openpty master");
        let open_pty_slave_flags = fd_flags(os, "openpty slave");

        for fd in [master, slave, om, os] {
            // SAFETY: each is a live fd this test owns.
            unsafe { libc::close(fd) };
        }

        assert!(master_is_tty && slave_is_tty, "both ends must be ttys");
        assert_ne!(
            master_flags & libc::FD_CLOEXEC,
            0,
            "the MASTER must be close-on-exec from birth: an unrelated \
             fork+exec must never inherit a readable/writable handle on \
             another session's terminal"
        );
        assert_ne!(
            slave_flags & libc::FD_CLOEXEC,
            0,
            "the SLAVE must be close-on-exec from birth: this is the end \
             forkpty never flagged at all, and an inherited copy both holds \
             the pty open (suppressing the master's hangup) and is a live \
             bidirectional channel onto the session"
        );
        assert_eq!(
            (
                open_pty_master_flags & libc::FD_CLOEXEC,
                open_pty_slave_flags & libc::FD_CLOEXEC
            ),
            (0, 0),
            "control: openpty (what forkpty uses) leaves BOTH ends inheritable — \
             if this ever changes, the assertions above stopped being meaningful"
        );
        // The winsize/termios were applied to the slave in the parent, before any
        // fork could observe them — the atomicity property the seam relies on.
        assert_eq!(slave_ws, (41, 137), "winsize must be live on the slave");
        assert_eq!(master_ws, (41, 137), "and visible from the master");
        assert_ne!(
            slave_termios.c_iflag & libc::IUTF8,
            0,
            "the spawn termios must be applied at slave-open time"
        );
        // SAFETY: `cfgetospeed` only reads the termios.
        assert_eq!(unsafe { libc::cfgetospeed(&slave_termios) }, libc::B230400);
    }

    /// The two descriptors this seam returns are ACTUALLY each other's ends.
    ///
    /// Everything else asserted about the pair is a FLAG — close-on-exec,
    /// `O_NOCTTY`, termios, winsize — and every one of those assertions would
    /// still pass if the slave belonged to a DIFFERENT terminal. That is not
    /// hypothetical: reaching the slave by path (`ptsname_r` →
    /// `open("/dev/pts/N")`) was MEASURED on Linux 6.8 returning a live,
    /// correctly-flagged pts from another devpts instance — one of the reasons
    /// [`open_pts_slave`] asks the kernel for the peer instead of asking the
    /// filesystem for a name. So prove the pairing the one way flags cannot
    /// fake, and prove the proof is non-vacuous with a pair that must NOT cross.
    #[test]
    fn pty_pair_ends_are_peers_of_each_other() {
        let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");
        let (other_master, other_slave) =
            open_pty_pair_cloexec(None, None).expect("second pty pair");

        let paired = byte_crosses(slave, master, b'A');
        // NEGATIVE CONTROL: an unrelated pair's slave must not reach this master.
        let crossed = byte_crosses(other_slave, master, b'B');

        // Close BEFORE asserting so a failure cannot leak descriptors.
        for fd in [master, slave, other_master, other_slave] {
            // SAFETY: each is a live fd this test owns.
            unsafe { libc::close(fd) };
        }

        assert!(
            paired,
            "the slave must be the peer OF THIS MASTER: a byte written into it \
             has to come out of the master handed back alongside it"
        );
        assert!(
            !crossed,
            "control: a foreign pty's slave must NOT reach this master — if it \
             could, the positive assertion above would prove nothing"
        );
    }

    /// Linux takes the slave FROM THE MASTER, not from a path.
    ///
    /// `ioctl(TIOCGPTPEER)` returns the peer of this exact ptmx with the
    /// `open(2)` flag word applied, so the slave is close-on-exec from birth AND
    /// cannot be a descriptor onto another devpts instance's terminal — the two
    /// failure modes measured for the path route ([`open_pts_slave`]). On a
    /// kernel too old to know the ioctl (pre-4.13) the seam falls back to the
    /// path, so this asserts THAT route is a true peer too rather than
    /// pretending the ioctl exists everywhere.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_slave_comes_from_the_master_via_tiocgptpeer() {
        // SAFETY: `posix_openpt` takes only a flag word; the fd is closed below.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        assert!(master >= 0, "posix_openpt: {}", io::Error::last_os_error());
        // SAFETY: `master` is a live /dev/ptmx descriptor; both are ioctls on it.
        assert_eq!(unsafe { libc::grantpt(master) }, 0, "grantpt");
        // SAFETY: as above.
        assert_eq!(unsafe { libc::unlockpt(master) }, 0, "unlockpt");

        // SAFETY: `TIOCGPTPEER` takes its argument BY VALUE (an `open(2)` flag
        // word, not a pointer) and returns a fresh descriptor or -1.
        let peer = unsafe {
            libc::ioctl(
                master,
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        let peer_errno = io::Error::last_os_error();

        // NEGATIVE CONTROL: the ioctl is specific to a ptmx. If it handed out a
        // descriptor for an ordinary pipe, its success above would mean nothing.
        let mut pipe_fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills exactly the two-element array it is given.
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: as the peer call above, on a descriptor that is not a ptmx.
        let non_ptmx = unsafe {
            libc::ioctl(
                pipe_fds[0],
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };

        // Whichever route this kernel supports, the seam's own opener must end
        // up with a close-on-exec descriptor that is a TRUE peer of `master`.
        let slave = open_pts_slave(master).expect("the slave must open by SOME route");
        let slave_flags = fd_flags(slave, "pts slave");
        let slave_is_tty = fd_is_tty(slave);
        let slave_is_peer = byte_crosses(slave, master, b'P');

        // Close BEFORE asserting so a failure cannot leak descriptors.
        for fd in [master, slave, pipe_fds[0], pipe_fds[1]] {
            // SAFETY: each is a live fd this test owns.
            unsafe { libc::close(fd) };
        }
        if peer >= 0 {
            // SAFETY: the peer descriptor this test opened and still owns.
            unsafe { libc::close(peer) };
        }
        if non_ptmx >= 0 {
            // SAFETY: as above, on the (unexpected) pipe-derived descriptor.
            unsafe { libc::close(non_ptmx) };
        }

        assert!(
            non_ptmx < 0,
            "control: TIOCGPTPEER must refuse a non-ptmx descriptor"
        );
        if peer < 0 {
            // Only "this kernel has no such ioctl" may send the seam down the
            // path fallback. Any other errno means the primary route broke.
            assert!(
                matches!(peer_errno.raw_os_error(), Some(libc::EINVAL | libc::ENOTTY)),
                "TIOCGPTPEER must either work or be unsupported (pre-4.13 \
                 kernel), not fail for some other reason: {peer_errno}"
            );
        }
        assert!(slave_is_tty, "the slave must be a tty");
        assert_ne!(
            slave_flags & libc::FD_CLOEXEC,
            0,
            "TIOCGPTPEER applies the open(2) flag word it is given, so the peer \
             is close-on-exec from birth exactly as the path open is"
        );
        assert!(
            slave_is_peer,
            "the descriptor must be the peer OF THIS MASTER — the property a \
             path lookup cannot guarantee once /dev/pts is not the devpts this \
             master came from"
        );
    }

    /// THE DEFECT, as a deterministic regression: while a session's pty pair is
    /// open in this (multi-threaded) parent, an UNRELATED `Command::spawn` — the
    /// exact shape of the update-handoff worker's `job.command.spawn()` — must
    /// inherit NEITHER end.
    ///
    /// Deterministic by construction, with no race and no timing: the pair is
    /// open across the whole `spawn`, so anything inheritable WOULD be inherited.
    /// (The historical window was narrower only because `forkpty` closed the
    /// slave promptly; the inheritance rule being tested is the same one.)
    ///
    /// NON-VACUITY is carried by a control fd in the same run: a plain `dup` of
    /// the slave, which POSIX defines to clear `FD_CLOEXEC`, is handed to the
    /// same helper. It must be inherited AND must be able to write onto this
    /// session's terminal — which simultaneously proves the detector can see a
    /// leak at all, and reproduces the actual harm (forged output injected into
    /// aterm's render path from a process that was never given a terminal).
    #[test]
    fn an_unrelated_exec_inherits_neither_end_of_a_live_pty() {
        use std::time::Duration;
        let ws = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (master, slave) = open_pty_pair_cloexec(None, Some(&ws)).expect("pty pair");
        // The control leak: `dup` never copies FD_CLOEXEC, so this is a slave
        // handle in exactly the state forkpty left the real one in.
        // SAFETY: `slave` is live; `dup` returns a new fd onto the same pty.
        let leaked = unsafe { libc::dup(slave) };
        assert!(leaked >= 0, "dup(slave) failed");
        assert_eq!(
            fd_flags(leaked, "control dup of the slave") & libc::FD_CLOEXEC,
            0,
            "PRECONDITION: the control fd must be inheritable, or it proves nothing"
        );

        // The unrelated process: reports which of the three fd numbers it was
        // actually handed, then writes a marker through each. Its stdout is a
        // pipe (fd 1), so nothing it prints can be confused with pty traffic.
        // The `/dev/fd` probes all run BEFORE any redirection, so a shell reusing
        // a freed number internally cannot fake an inheritance.
        let script = format!(
            "for f in {master} {slave} {leaked}; do [ -e /dev/fd/$f ] && printf 'got:%s ' \"$f\"; done; \
             printf 'ran '; \
             {{ printf 'LEAKMARK' >&{leaked}; }} 2>/dev/null; \
             {{ printf 'SLAVEMARK' >&{slave}; }} 2>/dev/null; \
             {{ printf 'MASTERMARK' >&{master}; }} 2>/dev/null; \
             printf 'end'"
        );
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("the unrelated helper must run");
        let report = String::from_utf8_lossy(&out.stdout).into_owned();
        // Parse the reported fd NUMBERS rather than substring-matching them:
        // with concurrent tests the descriptors reach two digits, and "got:3"
        // is a substring of "got:33" — a false leak report waiting to happen.
        let inherited: Vec<libc::c_int> = report
            .split_whitespace()
            .filter_map(|tok| tok.strip_prefix("got:"))
            .filter_map(|n| n.parse::<libc::c_int>().ok())
            .collect();

        // Drain whatever reached our terminal.
        set_nonblocking(master, true).expect("nonblocking master");
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            let mut buf = [0u8; 256];
            let n = read(master, &mut buf);
            if n > 0 {
                seen.extend_from_slice(&buf[..n as usize]);
                if seen.len() >= b"LEAKMARK".len() {
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let seen = String::from_utf8_lossy(&seen).into_owned();

        for fd in [leaked, slave, master] {
            // SAFETY: each is a live fd this test owns.
            unsafe { libc::close(fd) };
        }

        // PRECONDITION: the helper really ran and really reported.
        assert!(
            report.contains("ran") && report.contains("end"),
            "the unrelated helper did not run to completion; report: {report:?}"
        );
        // NON-VACUITY: an inheritable slave IS inherited, and IS a live channel
        // onto this session's terminal.
        assert!(
            inherited.contains(&leaked),
            "control: an fd WITHOUT FD_CLOEXEC must be inherited across exec, \
             else this test cannot detect a leak at all; report: {report:?}"
        );
        assert!(
            seen.contains("LEAKMARK"),
            "control: a leaked slave lets an unrelated process forge output into \
             this session's terminal; master saw: {seen:?}"
        );
        // THE PROPERTY.
        assert!(
            !inherited.contains(&master),
            "an unrelated exec'd process inherited the pty MASTER (fd {master}); \
             report: {report:?}"
        );
        assert!(
            !inherited.contains(&slave),
            "an unrelated exec'd process inherited the pty SLAVE (fd {slave}); \
             report: {report:?}"
        );
        assert!(
            !seen.contains("SLAVEMARK") && !seen.contains("MASTERMARK"),
            "an unrelated exec'd process wrote onto this session's terminal \
             through an inherited pty end; master saw: {seen:?}"
        );
    }

    // ---- The EXEC-STATUS CHANNEL: the window, and the bound behind it ----

    /// Build the ORIGINAL, RACY carrier — `pipe(2)` + two `fcntl(F_SETFD)` —
    /// so the tests below can reproduce the defect deliberately.
    ///
    /// This is the shape `open_exec_status_channel` now only reaches as a
    /// last-resort fallback. Reproducing it by hand is the point: the fix makes
    /// the leak unreachable through the real seam, so the only honest way to test
    /// that the BOUND still ends the wait is to re-create the leak on purpose.
    fn racy_status_pipe() -> (libc::c_int, libc::c_int) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills exactly the two-element array it is given.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: both fds are live; `F_SETFD` only sets a flag word.
        unsafe {
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        assert!(
            fd_set_nonblocking(fds[0]),
            "the read end must be non-blocking, as the real carrier makes it"
        );
        (fds[0], fds[1])
    }

    /// Is the channel still WITHOUT a verdict after `grace`?
    ///
    /// `true` means someone other than us holds a write end: every copy we own is
    /// closed, so if nobody else had one the read would answer EOF (0) at once.
    /// This is the precondition that makes the bound tests non-vacuous — it is
    /// exactly the state in which the ORIGINAL blocking `read` was measured
    /// parking for the stranger's entire lifetime (5 s, 20 s, 65 s).
    fn eof_is_suppressed(rd: libc::c_int, grace: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if read_exec_status_now(rd).is_some() {
                return false; // a verdict arrived — nothing is suppressing EOF
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }

    /// Spawn an unrelated process that INHERITS `fd` and holds it open.
    ///
    /// Deliberately `std::process::Command::spawn` and nothing more exotic: that
    /// is the literal call the update-handoff worker makes on its own thread
    /// (aterm-gui `app_update_handoff.rs`), it bottoms out in `posix_spawn(2)`,
    /// and it closes nothing but 0/1/2 — so any descriptor without `FD_CLOEXEC`
    /// rides straight through its `exec`. That is the whole defect, reproduced
    /// with the production mechanism rather than a stand-in.
    fn stranger_holding(fd: libc::c_int) -> std::process::Child {
        assert_eq!(
            fd_flags(fd, "the fd handed to the stranger") & libc::FD_CLOEXEC,
            0,
            "PRECONDITION: the fd must be inheritable, or the stranger cannot \
             receive it and the test proves nothing"
        );
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the unrelated helper must spawn")
    }

    /// THE FIX: the exec-status channel's two ends are close-on-exec FROM BIRTH.
    ///
    /// The carrier is asserted to be an ATOMIC one, not the racy fallback,
    /// because the flag alone is not the property that matters — `pipe(2)` plus
    /// two `fcntl`s ends up with the same two flags set and is precisely the
    /// defect. What matters is that no unflagged instant ever existed, and the
    /// only machine-checkable proxy for that from here is WHICH carrier ran.
    ///
    /// NON-VACUITY is carried by a plain `pipe(2)` in the same run: it must show
    /// both ends unflagged, or this test could not tell the two apart at all.
    #[test]
    fn exec_status_channel_is_close_on_exec_from_birth() {
        let (rd, wr, carrier) =
            open_exec_status_channel().expect("the exec-status channel must open");

        // The control: the carrier this replaces, built the way it used to be.
        let mut racy = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills exactly the two-element array it is given.
        assert_eq!(unsafe { libc::pipe(racy.as_mut_ptr()) }, 0, "pipe");
        let racy_flags = (
            fd_flags(racy[0], "control pipe read end"),
            fd_flags(racy[1], "control pipe write end"),
        );

        let flags = (
            fd_flags(rd, "status read end"),
            fd_flags(wr, "status write end"),
        );
        // Close BEFORE asserting so a failure cannot leak descriptors.
        for fd in [rd, wr, racy[0], racy[1]] {
            // SAFETY: each is a live fd this test owns.
            unsafe { libc::close(fd) };
        }

        assert_eq!(
            (
                racy_flags.0 & libc::FD_CLOEXEC,
                racy_flags.1 & libc::FD_CLOEXEC
            ),
            (0, 0),
            "control: `pipe(2)` must return BOTH ends unflagged — that unflagged \
             instant IS the defect, and if it were not observable here this test \
             would be comparing nothing"
        );
        assert_ne!(
            flags.0 & libc::FD_CLOEXEC,
            0,
            "the status READ end must be close-on-exec"
        );
        assert_ne!(
            flags.1 & libc::FD_CLOEXEC,
            0,
            "the status WRITE end must be close-on-exec — this is the end whose \
             exec-close IS the success signal"
        );
        assert_ne!(
            carrier,
            ExecStatusCarrier::RacyPipe,
            "the channel must come from a carrier that flags both ends AT BIRTH \
             ({:?} on this platform), not from `pipe(2)` + `fcntl`, which sets \
             the same flags a moment too late",
            if cfg!(target_os = "linux") {
                "pipe2(O_CLOEXEC)"
            } else {
                "an unlinked O_CLOEXEC fifo"
            }
        );
    }

    /// THE DEFECT, as a deterministic regression: an unrelated `Command::spawn`
    /// running while a status channel is open must inherit NEITHER end.
    ///
    /// Deterministic by construction — the channel is open across the whole
    /// `spawn`, so anything inheritable WOULD be inherited. NON-VACUITY comes
    /// from a control fd in the same run: a plain `dup` of the write end, which
    /// POSIX defines to clear `FD_CLOEXEC`, must be inherited, and must still be
    /// able to suppress EOF — which is not just "the detector works" but the
    /// actual harm, since that suppression is what parked the session-opening
    /// thread for the stranger's entire lifetime.
    #[test]
    fn an_unrelated_exec_inherits_neither_end_of_the_exec_status_channel() {
        let (rd, wr, _carrier) =
            open_exec_status_channel().expect("the exec-status channel must open");
        // The control leak: `dup` never copies FD_CLOEXEC.
        // SAFETY: `wr` is live; `dup` returns a new fd onto the same channel.
        let leaked = unsafe { libc::dup(wr) };
        assert!(leaked >= 0, "dup(status_wr) failed");
        assert_eq!(
            fd_flags(leaked, "control dup of the status write end") & libc::FD_CLOEXEC,
            0,
            "PRECONDITION: the control fd must be inheritable, or it proves nothing"
        );

        // Report which of the three numbers actually arrived, then hold them.
        let script = format!(
            "for f in {rd} {wr} {leaked}; do [ -e /dev/fd/$f ] && printf 'got:%s ' \"$f\"; done; \
             printf 'ran '; printf 'end'"
        );
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .expect("the unrelated helper must run");
        let report = String::from_utf8_lossy(&out.stdout).into_owned();
        // Parse fd NUMBERS, not substrings: "got:3" is a substring of "got:33".
        let inherited: Vec<libc::c_int> = report
            .split_whitespace()
            .filter_map(|tok| tok.strip_prefix("got:"))
            .filter_map(|n| n.parse::<libc::c_int>().ok())
            .collect();

        // The harm, demonstrated with the control fd: hand it to a LIVE stranger,
        // drop every copy the "parent" holds, and EOF still does not arrive.
        let mut stranger = stranger_holding(leaked);
        // SAFETY: these are the parent's own copies; the stranger has its own.
        unsafe {
            libc::close(leaked);
            libc::close(wr);
        }
        let control_suppressed = eof_is_suppressed(rd, std::time::Duration::from_millis(300));
        let _ = stranger.kill();
        let _ = stranger.wait();
        // SAFETY: the read end is the last descriptor this test owns.
        unsafe { libc::close(rd) };

        // PRECONDITION: the helper really ran and really reported.
        assert!(
            report.contains("ran") && report.contains("end"),
            "the unrelated helper did not run to completion; report: {report:?}"
        );
        // NON-VACUITY: an inheritable write end IS inherited, and IS enough to
        // suppress the success verdict indefinitely.
        assert!(
            inherited.contains(&leaked),
            "control: an fd WITHOUT FD_CLOEXEC must be inherited across exec, \
             else this test cannot detect a leak at all; report: {report:?}"
        );
        assert!(
            control_suppressed,
            "control: a stranger holding an inherited WRITE end must suppress EOF \
             — that suppression is the hang, and without it the property below \
             would be asserting nothing"
        );
        // THE PROPERTY.
        assert!(
            !inherited.contains(&wr),
            "an unrelated exec'd process inherited the status WRITE end (fd {wr}) \
             — it can now suppress this spawn's EOF for its whole lifetime; \
             report: {report:?}"
        );
        assert!(
            !inherited.contains(&rd),
            "an unrelated exec'd process inherited the status READ end (fd {rd}); \
             report: {report:?}"
        );
    }

    /// THE HANG, bounded: with a stranger holding a duplicate of the write end,
    /// the status wait must END — by the budget — instead of parking forever.
    ///
    /// The leak is re-created ON PURPOSE (the fix makes it unreachable through
    /// the real seam), using the production mechanism: a non-close-on-exec
    /// duplicate inherited by a real `Command::spawn`.
    ///
    /// NON-VACUITY is the whole design of this test:
    ///  * the stranger is asserted to be ALIVE at the end, so the wait ended
    ///    because of the bound and not because the holder happened to exit;
    ///  * `eof_is_suppressed` asserts the channel genuinely has no verdict — every
    ///    copy the "parent" owns is closed, so without the stranger EOF would be
    ///    immediate, which is exactly the state the ORIGINAL blocking read was
    ///    measured parking in for 5 s / 20 s / 65 s;
    ///  * a CONTROL run with no stranger must answer `Execed` promptly, proving
    ///    the harness can see a verdict when one exists.
    ///
    /// A short budget is passed explicitly rather than using
    /// `EXEC_STATUS_BUDGET`: what is under test is that the bound is what ends
    /// the wait, not how long the shipping bound is (that is a separate
    /// assertion below, and waiting 10 s here would buy nothing).
    #[test]
    fn a_stranger_holding_the_write_end_cannot_block_the_status_wait_forever() {
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(400);

        // CONTROL first: same protocol, no stranger. A verdict must arrive.
        let (crd, cwr) = racy_status_pipe();
        // SAFETY: the only write end; closing it is the "child exec'd" signal.
        unsafe { libc::close(cwr) };
        let control = wait_for_exec_status(crd, BUDGET);
        // SAFETY: the read end this test owns.
        unsafe { libc::close(crd) };

        // THE DEFECT: a stranger inherits a duplicate of the write end.
        let (rd, wr) = racy_status_pipe();
        // SAFETY: `wr` is live; `dup` copies it WITHOUT FD_CLOEXEC — exactly what
        // `pipe(2)`'s unflagged window handed to a concurrent `posix_spawn`.
        let leaked = unsafe { libc::dup(wr) };
        assert!(leaked >= 0, "dup(status_wr) failed");
        let mut stranger = stranger_holding(leaked);
        // Drop every copy the "parent" holds — including the one that stands in
        // for the child's, which a successful `execve` would have closed.
        // SAFETY: all three are this process's own copies.
        unsafe {
            libc::close(leaked);
            libc::close(wr);
        }

        let suppressed = eof_is_suppressed(rd, std::time::Duration::from_millis(250));
        let started = std::time::Instant::now();
        let status = wait_for_exec_status(rd, BUDGET);
        let waited = started.elapsed();
        let stranger_alive = stranger.try_wait().expect("try_wait").is_none();

        let _ = stranger.kill();
        let _ = stranger.wait();
        // SAFETY: the read end is the last descriptor this test owns.
        unsafe { libc::close(rd) };

        assert!(
            matches!(control, ExecStatus::Execed),
            "control: with no stranger, closing the last write end must answer \
             `Execed` — if it did not, the harness could not tell a suppressed \
             verdict from a missing one; got {control:?}"
        );
        assert!(
            suppressed,
            "PRECONDITION: with every parent-side copy closed, the channel must \
             still have NO verdict — that is the state the original unbounded \
             `read` parked in for the stranger's entire lifetime"
        );
        assert!(
            stranger_alive,
            "PRECONDITION: the stranger must STILL hold the write end when the \
             wait returns — otherwise the wait ended because the holder exited, \
             which is the old behaviour passing as the new one"
        );
        assert!(
            matches!(status, ExecStatus::NoVerdict),
            "a suppressed EOF must expire as `NoVerdict` (which the seam turns \
             into a fail-closed error), got {status:?}"
        );
        assert!(
            waited >= BUDGET && waited < BUDGET * 8,
            "the wait must be ended BY THE BUDGET: expected ~{BUDGET:?}, took \
             {waited:?}"
        );
    }

    /// The case a naive timeout would silently turn into a FALSE SUCCESS: a
    /// pre-exec failure, reported while a stranger holds the write end.
    ///
    /// This is the asymmetry the whole bounded design rests on. The failure byte
    /// sits in the channel's BUFFER, so `read` returns it regardless of who else
    /// holds a write end — only EOF depends on every write end being gone. If
    /// that were not true, a bound would convert a sandbox-confinement failure
    /// into the EOF success verdict and hand back an unconfined shell, which is
    /// precisely the §5.6 hole `b'S'` exists to plug.
    ///
    /// NON-VACUITY: the stranger is asserted to be alive AND still holding at the
    /// moment the byte comes back (so the byte really did cross a channel with a
    /// third holder), and the verdict is required to arrive in a small fraction
    /// of the budget — a byte that only appeared at expiry would mean the buffer
    /// claim is false and the test had merely timed out into the right answer.
    #[test]
    fn a_pre_exec_failure_is_still_detected_while_a_stranger_holds_the_write_end() {
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(2000);

        let (rd, wr) = racy_status_pipe();
        // SAFETY: `wr` is live; `dup` copies it WITHOUT FD_CLOEXEC.
        let leaked = unsafe { libc::dup(wr) };
        assert!(leaked >= 0, "dup(status_wr) failed");
        let mut stranger = stranger_holding(leaked);
        // SAFETY: the parent's own copy of the leaked duplicate.
        unsafe { libc::close(leaked) };

        // The child's report: the sandbox-confinement failure byte, then its copy
        // of the write end goes away exactly as `_exit` would take it.
        let b: u8 = b'S';
        // SAFETY: `wr` is live; `write` reads exactly the one byte at `b`.
        let wrote = unsafe { libc::write(wr, ptr::from_ref(&b).cast::<libc::c_void>(), 1) };
        // SAFETY: the parent's own copy of the write end.
        unsafe { libc::close(wr) };

        let started = std::time::Instant::now();
        let status = wait_for_exec_status(rd, BUDGET);
        let waited = started.elapsed();
        let stranger_alive = stranger.try_wait().expect("try_wait").is_none();

        let _ = stranger.kill();
        let _ = stranger.wait();
        // SAFETY: the read end is the last descriptor this test owns.
        unsafe { libc::close(rd) };

        assert_eq!(wrote, 1, "the failure byte must have been written");
        assert!(
            stranger_alive,
            "PRECONDITION: the stranger must still hold a write end when the byte \
             comes back, or this proves nothing about a suppressed channel"
        );
        assert!(
            matches!(status, ExecStatus::FailedBeforeExec(b'S')),
            "a pre-exec failure must still be read as itself while a third party \
             holds a write end — a bound must never launder it into success; \
             got {status:?}"
        );
        assert!(
            waited < BUDGET / 4,
            "the byte is in the channel BUFFER, so it must come back immediately \
             rather than at expiry; took {waited:?} of a {BUDGET:?} budget"
        );
    }

    /// The real seam, end to end: a spawn that succeeds does so promptly, on the
    /// ATOMIC carrier, and nowhere near the fail-closed budget.
    ///
    /// The counter assertion is what makes this more than a smoke test: if
    /// `open_exec_status_channel` had quietly fallen back to `pipe(2)` + `fcntl`
    /// on this machine, every other assertion here would still pass while the
    /// window sat wide open. The count is required not to move.
    #[test]
    fn a_real_spawn_settles_promptly_on_the_atomic_status_carrier() {
        use std::sync::atomic::Ordering;
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        let before = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);
        let started = std::time::Instant::now();
        let spawned = spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("a normal shell must still spawn");
        let elapsed = started.elapsed();
        let after = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);

        hangup(spawned.pid);
        // SAFETY: `master` is the fd this test was just handed.
        unsafe { libc::close(spawned.master) };
        reap(spawned.pid);

        assert_eq!(
            before, after,
            "the spawn fell back to the RACY `pipe(2)`+fcntl status carrier — the \
             close-on-exec window is open on this machine, and every other \
             assertion in this test would pass anyway"
        );
        assert!(
            elapsed < EXEC_STATUS_BUDGET / 10,
            "a successful spawn must settle far inside the fail-closed budget \
             (measured p50 ~3 ms against a {EXEC_STATUS_BUDGET:?} budget); took \
             {elapsed:?}"
        );
    }

    /// Building the status channel must not KILL a concurrent fork's child.
    ///
    /// This is a scar, not a hypothetical. The first version of this carrier
    /// located its FIFO with `confstr(_CS_DARWIN_USER_TEMP_DIR)` — the
    /// conservative choice, since it asks the OS instead of trusting `$TMPDIR`.
    /// That call reaches libsystem_notify, whose shared region sits behind an
    /// `os_alloc_once` gate, and `fork(2)` runs `notify_fork_child` in the child
    /// before it returns. A fork that lands while another thread is inside that
    /// gate leaves the child holding a once-gate owned by a thread that does not
    /// exist, and libplatform kills it:
    ///
    /// ```text
    ///   BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt
    ///   libsystem_c.dylib: crashed on child side of fork pre-exec
    ///   _os_once_gate_corruption_abort <- _os_alloc_once <- notify_fork_child
    ///     <- libSystem_atfork_child <- fork
    /// ```
    ///
    /// SIGKILL, raised INSIDE `fork()` before one instruction of the child branch
    /// runs — so no amount of care in that branch can prevent it. MEASURED on
    /// otherwise-unmodified production code, with a test doing nothing but
    /// `confstr` on six threads: a concurrent `forkpty` child died in 16 of 25
    /// runs. In aterm that is a user's shell SIGKILLed at random because another
    /// thread asked where the temp directory was.
    ///
    /// So the invariant under test is deliberately broader than "don't call
    /// `confstr`": building the exec-status channel must not make a CONCURRENT
    /// FORK'S CHILD DIE, whatever it is built from. Anything added to the carrier
    /// later — a different directory lookup, a randomness source, a logging call —
    /// is covered by construction.
    ///
    /// NON-VACUITY, and its LIMIT, stated honestly because this one matters: the
    /// channel builder and the fork loop are both asserted to have actually run,
    /// and the builder is asserted to have produced the ATOMIC carrier, so the
    /// fifo path (the one with a directory lookup in it) is the one being
    /// exercised rather than the pipe fallback. The test was then checked against
    /// the real defect by putting the `confstr` back: it FAILS, but only in about
    /// 1-3 runs out of 12 (measured; raising the fork count to 1500 and 4000 did
    /// not improve it, so the limit is how briefly the once-gate is actually
    /// held, not how many forks are thrown at it). On fork-safe code it has never
    /// failed — 0 in 12 runs, and the failure mode is a libSystem abort that
    /// correct code cannot produce, so there are no false positives.
    ///
    /// So: this is a SMOKE ALARM, not a proof. It will catch a re-introduction
    /// across a handful of runs and it cannot cry wolf, but a single green run is
    /// not evidence of fork-safety. The primary defence against re-introducing
    /// `confstr` here is the documentation on [`exec_status_fifo_dir`], which
    /// says why that obvious-looking call is forbidden; this test is the backstop
    /// for anyone who changes the carrier without reading it.
    ///
    /// DARWIN ONLY, and for a structural reason rather than convenience: the
    /// hazard belongs to the PATHNAME-based carrier, and the naming path this
    /// churns ([`exec_status_fifo_path`]) exists only there. Linux's
    /// `pipe2(O_CLOEXEC)` is one syscall with no directory lookup, no lazily
    /// initialised subsystem behind it, and therefore nothing of this shape to
    /// get wrong.
    #[cfg(target_os = "macos")]
    #[test]
    fn building_the_status_channel_never_kills_a_concurrent_forks_child() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let built = Arc::new(AtomicU64::new(0));
        let named = Arc::new(AtomicU64::new(0));
        let racy = Arc::new(AtomicU64::new(0));

        // TWO kinds of churn, and the split is what makes the test sensitive.
        //
        // Building a whole channel costs ~270 us (mkfifo + two opens + unlink), so
        // threads doing only that touch the NAMING path a few thousand times a
        // second — far too sparse to reliably catch a once-gate that is only held
        // for the duration of one lookup. VERIFIED: with channel-building churn
        // alone, restoring the `confstr` did NOT fail this test in 10 runs. So
        // most threads hammer the naming path DIRECTLY, in a tight loop, which is
        // what raises the probability of a fork landing inside the gate to
        // something a test can rely on.
        let churn: Vec<_> = (0..6)
            .map(|i| {
                let (stop, built, named, racy) =
                    (stop.clone(), built.clone(), named.clone(), racy.clone());
                std::thread::spawn(move || {
                    // Two threads exercise the FULL carrier (so mkfifo/open/unlink
                    // are in the race too); the rest hammer path resolution.
                    let full = i < 2;
                    while !stop.load(Ordering::Relaxed) {
                        if full {
                            match open_exec_status_channel() {
                                Ok((rd, wr, carrier)) => {
                                    if carrier == ExecStatusCarrier::RacyPipe {
                                        racy.fetch_add(1, Ordering::Relaxed);
                                    }
                                    built.fetch_add(1, Ordering::Relaxed);
                                    // SAFETY: the two ends this thread just opened.
                                    unsafe {
                                        libc::close(rd);
                                        libc::close(wr);
                                    }
                                }
                                Err(_) => break,
                            }
                        } else if exec_status_fifo_path().is_ok() {
                            named.fetch_add(1, Ordering::Relaxed);
                        } else {
                            break;
                        }
                    }
                })
            })
            .collect();

        // Meanwhile, fork. The child does nothing but `_exit(0)` — so ANY death
        // by signal is the fork machinery itself dying, never our own code.
        let mut forks = 0u32;
        let mut signalled: Vec<(i32, i32)> = Vec::new();
        for _ in 0..300 {
            // SAFETY: `fork` takes no arguments; the child below reaches only
            // `_exit`, which is async-signal-safe.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork: {}", io::Error::last_os_error());
            if pid == 0 {
                // SAFETY: async-signal-safe, and the only call the child makes.
                unsafe { libc::_exit(0) };
            }
            forks += 1;
            let mut wstatus: libc::c_int = 0;
            // SAFETY: reaping the child just forked; `wstatus` is a valid out-param.
            unsafe { libc::waitpid(pid, &mut wstatus, 0) };
            if !libc::WIFEXITED(wstatus) {
                signalled.push((pid, libc::WTERMSIG(wstatus)));
            }
        }
        stop.store(true, Ordering::Relaxed);
        for t in churn {
            let _ = t.join();
        }
        let built = built.load(Ordering::Relaxed);
        let named = named.load(Ordering::Relaxed);
        let racy = racy.load(Ordering::Relaxed);

        // PRECONDITIONS: both sides of the race really ran.
        assert_eq!(forks, 300, "the fork loop must have run");
        assert!(
            built > 50 && named > 10_000,
            "PRECONDITION: the carrier must have been exercised HARD alongside \
             the forks, else nothing was raced (and a sparse race is exactly what \
             was measured to miss this defect); built {built}, named {named}"
        );
        assert_eq!(
            racy, 0,
            "PRECONDITION: the builder must have produced the ATOMIC carrier — \
             the pipe fallback has no directory lookup, so racing it would not \
             exercise the hazard this test exists for"
        );
        // THE PROPERTY.
        assert!(
            signalled.is_empty(),
            "a child that does nothing but _exit(0) was killed by a signal while \
             the exec-status channel was being built on other threads — the \
             carrier is doing something that is not fork-safe (SIGKILL here means \
             `fork` itself aborted in libSystem's atfork child handler, e.g. the \
             `confstr`/libsystem_notify `os_once` corruption this test records). \
             (pid, signal) pairs: {signalled:?}"
        );
    }

    /// The platform trap that dictates `select(2)` in `wait_readable_briefly`.
    ///
    /// On macOS a FIFO read end whose LAST WRITER HAS CLOSED is at EOF —
    /// `read(2)` returns 0 — but `poll(2)` reports nothing at all and
    /// `kqueue`/`EVFILT_READ` reports nothing at all, indefinitely. Only
    /// `select(2)` sees it. All three are correct on a pipe, which is what makes
    /// this a trap rather than a curiosity: a maintainer who "modernises" the
    /// `select` into a `poll` would watch every pipe-based test stay green and
    /// silently add the full budget to every successful spawn on macOS.
    ///
    /// NON-VACUITY: the same `poll` and `kqueue` calls are run against a PIPE at
    /// EOF in the same test and must both report it — so a failure here means
    /// "fifos are special", not "the probe is broken". Pinned as an executable
    /// fact so that if a future macOS fixes it, this test says so out loud
    /// instead of leaving the comment to rot.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_fifo_eof_is_invisible_to_poll_and_kqueue_but_not_select() {
        /// Does `poll` report readability on `fd` within `ms`?
        fn polls_ready(fd: libc::c_int, ms: libc::c_int) -> bool {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a live, fully-initialized one-element array.
            unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
        }
        /// Does `kqueue`/`EVFILT_READ` report readability on `fd` within `ms`?
        fn kqueue_ready(fd: libc::c_int, ms: i64) -> bool {
            // SAFETY: `ch`/`ev` are zeroed, fully-initialized kevents; `kevent`
            // reads one change and writes at most one event through them.
            unsafe {
                let kq = libc::kqueue();
                let mut ch: libc::kevent = std::mem::zeroed();
                ch.ident = fd as usize;
                ch.filter = libc::EVFILT_READ;
                ch.flags = libc::EV_ADD | libc::EV_ENABLE;
                let mut ev: libc::kevent = std::mem::zeroed();
                let ts = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: ms * 1_000_000,
                };
                let n = libc::kevent(kq, &ch, 1, &mut ev, 1, &ts);
                libc::close(kq);
                n > 0
            }
        }
        /// Does `select` report readability on `fd` within `ms`?
        fn selects_ready(fd: libc::c_int, ms: i32) -> bool {
            // SAFETY: `set` is a zeroed fd_set and test fds are far below
            // FD_SETSIZE; `select` touches only `set` and `tv`.
            unsafe {
                let mut set: libc::fd_set = std::mem::zeroed();
                libc::FD_SET(fd, &mut set);
                let mut tv = libc::timeval {
                    tv_sec: 0,
                    tv_usec: ms * 1000,
                };
                libc::select(fd + 1, &mut set, ptr::null_mut(), ptr::null_mut(), &mut tv) > 0
            }
        }

        // A fifo at EOF: the real carrier, with its write end closed.
        let (frd, fwr) = open_exec_status_fifo().expect("the exec-status fifo must open");
        // SAFETY: the only write end; closing it puts the read end at EOF.
        unsafe { libc::close(fwr) };
        let fifo = (
            selects_ready(frd, 200),
            polls_ready(frd, 200),
            kqueue_ready(frd, 200),
        );
        // And what `read(2)` says about the very same descriptor.
        let fifo_verdict = read_exec_status_now(frd);
        // SAFETY: the read end this test owns.
        unsafe { libc::close(frd) };

        // THE PRODUCTION FUNCTION, bound to the fact above — on a FRESH carrier,
        // and in the state `wait_for_exec_status` actually waits in.
        //
        // That distinction is load-bearing and was got wrong once: the loop reads
        // FIRST and returns on EOF, so it never waits on a descriptor already at
        // EOF. It waits with a writer still open and nothing sent — i.e. the
        // child has not exec'd yet — and needs to WAKE when that last writer goes
        // away. (MEASURED aside, which is why this must use a fresh fifo: once an
        // EOF has been READ on a Darwin fifo, `select` stops reporting the
        // descriptor ready, so timing a wait after a read measures a state
        // production never sees and reports a 2 s "hang" that is not one.)
        //
        // So: hold the write end, close it 100 ms later, and time the wait.
        // `select` wakes on that transition; `poll` and `kqueue` never do.
        // Without this binding, swapping `select` for `poll` in
        // `wait_readable_briefly` leaves every other test green — the slice cap
        // keeps it CORRECT, just slow — and the latency regression ships.
        let (drd, dwr) = open_exec_status_fifo().expect("the exec-status fifo must open");
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            // SAFETY: the only write end; this thread owns it now.
            unsafe { libc::close(dwr) };
        });
        let waited = std::time::Instant::now();
        wait_readable_briefly(drd, std::time::Duration::from_secs(2));
        let wait_readable_took = waited.elapsed();
        let after_wake = read_exec_status_now(drd);
        closer.join().expect("closer thread");
        // SAFETY: the read end this test owns.
        unsafe { libc::close(drd) };

        // The CONTROL: a pipe at EOF, probed identically.
        let (prd, pwr) = racy_status_pipe();
        // SAFETY: the only write end; closing it puts the read end at EOF.
        unsafe { libc::close(pwr) };
        let pipe = (
            selects_ready(prd, 200),
            polls_ready(prd, 200),
            kqueue_ready(prd, 200),
        );
        // SAFETY: the read end this test owns.
        unsafe { libc::close(prd) };

        // CONTROL: on a pipe all three mechanisms see EOF. If this ever fails,
        // the probes above are broken and the fifo result below means nothing.
        assert_eq!(
            pipe,
            (true, true, true),
            "control: select/poll/kqueue must ALL report EOF on a PIPE \
             (select, poll, kqueue) = {pipe:?}"
        );
        // The ground truth `wait_for_exec_status` actually relies on.
        assert!(
            matches!(fifo_verdict, Some(ExecStatus::Execed)),
            "a fifo whose last writer closed must read as EOF; got {fifo_verdict:?}"
        );
        assert!(
            fifo.0,
            "select(2) must report EOF on a fifo — it is the ONLY readiness \
             primitive `wait_readable_briefly` can use on this platform"
        );
        // The binding that makes the fact above load-bearing on real code.
        assert!(
            matches!(after_wake, Some(ExecStatus::Execed)),
            "PRECONDITION: the last writer really did close, so the wait had a \
             real EOF to wake on; got {after_wake:?}"
        );
        assert!(
            wait_readable_took < std::time::Duration::from_millis(900),
            "`wait_readable_briefly` must WAKE when a fifo's last writer closes \
             (~100 ms here) rather than sleeping out its 2 s slice — it took \
             {wait_readable_took:?}, which is what a readiness primitive blind to \
             fifo EOF (poll, kqueue) does. Correctness is unaffected (the caller \
             re-reads), but every successful spawn on macOS pays this in latency"
        );
        assert_eq!(
            (fifo.1, fifo.2),
            (false, false),
            "poll/kqueue are expected to MISS fifo EOF on macOS; if this now \
             fails, the platform was fixed — update `EXEC_STATUS_SLICE`'s and \
             `wait_readable_briefly`'s comments rather than deleting this test \
             (poll, kqueue) = {:?}",
            (fifo.1, fifo.2)
        );
    }

    /// The property that would SILENTLY break the shell if the close-on-exec
    /// design were wrong: the child's stdio must NOT be close-on-exec.
    ///
    /// `login_tty` `dup2`s the (close-on-exec) slave onto 0/1/2, and `dup2` never
    /// carries `FD_CLOEXEC` to the new descriptor — so the copies survive
    /// `execve`. If that were wrong the exec'd program would start with fds
    /// 0/1/2 CLOSED and this test could not receive a single byte: the `printf`
    /// below is written to fd 1, through the real spawn seam, and read off the
    /// master. Same run also proves the child got a CONTROLLING TERMINAL
    /// (`/dev/tty` opens) and the parent-applied winsize (`stty size`).
    #[test]
    fn spawned_child_stdio_is_an_inherited_tty_with_a_ctty_and_our_winsize() {
        use std::time::Duration;
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // Deliberately NOT 24x80: a default-shaped answer would not distinguish
        // "our winsize arrived" from "the kernel's default happens to match".
        // The child reports, then PARKS, so the parent can interrogate the
        // process identities below while the session is still live.
        let script = "s=n; [ -t 0 ] && [ -t 1 ] && [ -t 2 ] && s=y; \
                      c=n; { : < /dev/tty; } 2>/dev/null && c=y; \
                      printf 'PROBE stdio=%s ctty=%s size=[%s] tty=[%s]\\n' \
                        \"$s\" \"$c\" \"$(stty size)\" \"$(tty)\"; \
                      exec sleep 30";
        let exec: Vec<String> = vec!["/bin/sh".into(), "-c".into(), script.into()];
        let sh = spawn_shell_with_pid(
            41,
            137,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None, // argv_override
            Some(&exec),
            None, // cwd
            None, // sandbox_wrap
            aterm_sandbox::Limits::inherit(),
        )
        .expect("the probe command must spawn");

        set_nonblocking(sh.master, true).expect("nonblocking master");
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let mut buf = [0u8; 512];
            let n = read(sh.master, &mut buf);
            if n > 0 {
                seen.extend_from_slice(&buf[..n as usize]);
                if seen.windows(1).len() > 0 && seen.ends_with(b"\n") && seen.starts_with(b"PROBE")
                {
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let seen = String::from_utf8_lossy(&seen).into_owned();

        // The child's controlling terminal must be THIS session's slave, named
        // exactly. `ctty=y` alone would be satisfied by an inherited terminal
        // (the harness has one); matching the device proves setsid + TIOCSCTTY
        // ran against OUR pts.
        let want_tty = pts_name(sh.master)
            .expect("the master must still resolve its pts name")
            .to_string_lossy()
            .into_owned();

        // Process identities, read from the PARENT while the session is parked:
        // pid == pgid == sid, and the pty's foreground process group is the
        // child. These are the exact probes `hangup` (killpg), `reap` (getpgid),
        // and the GUI's quit_safety/control_input (tcgetpgrp) depend on.
        // SAFETY: read-only identity probes on a live child pid / our master fd.
        let (pgid, sid, fg) = unsafe {
            (
                libc::getpgid(sh.pid),
                libc::getsid(sh.pid),
                libc::tcgetpgrp(sh.master),
            )
        };

        hangup(sh.pid);
        // SAFETY: closing the master this test owns.
        unsafe { libc::close(sh.master) };
        reap(sh.pid);

        // PRECONDITION: bytes arrived at all. This IS the stdio-survives-exec
        // proof — an exec'd child with close-on-exec 0/1/2 has nothing to write
        // to, so an empty read here is exactly the failure being guarded.
        assert!(
            seen.contains("PROBE"),
            "no output reached the master: the exec'd child had no usable stdio \
             (this is what a close-on-exec 0/1/2 would look like). Saw: {seen:?}"
        );
        assert!(
            seen.contains("stdio=y"),
            "the exec'd child's fds 0/1/2 must all be ttys — dup2 must have \
             cleared FD_CLOEXEC on the slave copies. Saw: {seen:?}"
        );
        assert!(
            seen.contains("ctty=y"),
            "the child must have a CONTROLLING TERMINAL (/dev/tty openable) — \
             login_tty's setsid + TIOCSCTTY. Saw: {seen:?}"
        );
        assert!(
            seen.contains("size=[41 137]"),
            "the parent-applied winsize must be live before the child's first \
             read of it. Saw: {seen:?}"
        );
        assert!(
            seen.contains(&format!("tty=[{want_tty}]")),
            "the child's controlling terminal must be THIS session's slave \
             ({want_tty}), not an inherited one. Saw: {seen:?}"
        );
        assert_eq!(
            (pgid, sid, fg),
            (sh.pid, sh.pid, sh.pid),
            "the child must be a SESSION LEADER with pid == pgid == sid and be \
             the pty's foreground process group — the identity hangup (killpg), \
             reap (getpgid) and the GUI's tcgetpgrp all depend on"
        );
    }

    /// The parent must not keep its copy of the slave. A retained slave holds the
    /// pty open for this process's whole life, so the master could never report
    /// hangup after the shell died — and `handoff_masters_closed` is fail-closed
    /// on exactly that condition, so the seamless update would wait forever on a
    /// session that is already gone.
    ///
    /// Non-vacuous by asserting the PRECONDITION in the same run: while the child
    /// is demonstrably alive and attached, the same poll must NOT report hangup.
    /// Without that half, a master that reported `POLLHUP` unconditionally (or a
    /// bad fd reporting `POLLNVAL`) would pass.
    #[test]
    fn master_reports_hangup_once_the_session_dies() {
        use std::time::Duration;
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // Announce liveness, then park: the session stays up until we hang it up.
        let exec: Vec<String> = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'UP\\n'; exec sleep 30".into(),
        ];
        let sh = spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("the parked session must spawn");

        set_nonblocking(sh.master, true).expect("nonblocking master");
        let mut alive = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let mut buf = [0u8; 64];
            let n = read(sh.master, &mut buf);
            if n > 0 && String::from_utf8_lossy(&buf[..n as usize]).contains("UP") {
                alive = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // PRECONDITION: a live, attached session does NOT report hangup.
        let while_alive = poll_revents(sh.master, Duration::from_millis(150));

        // Now end it, exactly as teardown does.
        hangup(sh.pid);
        reap(sh.pid);
        let after_death = poll_revents(sh.master, Duration::from_secs(5));

        // SAFETY: closing the master this test owns.
        unsafe { libc::close(sh.master) };

        assert!(
            alive,
            "the child never announced itself; the test proved nothing"
        );
        assert_eq!(
            while_alive & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL),
            0,
            "PRECONDITION: a LIVE session must not look dead to \
             handoff_masters_closed; revents were 0x{while_alive:04x}"
        );
        assert_ne!(
            after_death & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL),
            0,
            "the master never reported hangup after the session died — the \
             parent is still holding a copy of the pty SLAVE, and \
             handoff_masters_closed would block the seamless update forever"
        );
    }

    // ---- spawn termios (kernel defaults + IUTF8/B230400 deltas) ----

    #[test]
    fn kernel_default_termios_probe_yields_cooked_defaults() {
        let d = kernel_default_termios().expect("throwaway-pty termios probe must succeed");
        // Sanity that we captured the kernel's COOKED defaults, not garbage: a
        // fresh NULL-termios pty is canonical + echoing + output-processing.
        assert_ne!(d.c_lflag & libc::ICANON, 0, "default pty must be canonical");
        assert_ne!(d.c_lflag & libc::ECHO, 0, "default pty must echo");
        assert_ne!(
            d.c_oflag & libc::OPOST,
            0,
            "default pty must post-process output"
        );
    }

    #[test]
    fn spawn_termios_deltas_are_exactly_iutf8_and_speed() {
        let d = kernel_default_termios().expect("probe");
        let t = build_spawn_termios(d, false);
        // The two documented deltas...
        assert_ne!(
            t.c_iflag & libc::IUTF8,
            0,
            "IUTF8 must be set for the UTF-8 child locale"
        );
        // SAFETY: cfget*speed only read the valid termios.
        unsafe {
            assert_eq!(libc::cfgetospeed(&t), libc::B230400);
            assert_eq!(libc::cfgetispeed(&t), libc::B230400);
        }
        // ...and NOTHING else: flag-for-flag identical to the kernel defaults.
        assert_eq!(t.c_iflag & !libc::IUTF8, d.c_iflag & !libc::IUTF8);
        assert_eq!(t.c_oflag, d.c_oflag);
        assert_eq!(t.c_lflag, d.c_lflag);
        assert_eq!(t.c_cc, d.c_cc);
        // On macOS speed lives in dedicated c_ispeed/c_ospeed fields, so c_cflag
        // must be untouched too (Linux stores CBAUD bits inside c_cflag — skip).
        #[cfg(target_os = "macos")]
        assert_eq!(t.c_cflag, d.c_cflag);
    }

    #[test]
    fn bench_no_opost_gate_clears_only_opost() {
        let d = kernel_default_termios().expect("probe");
        let bench = build_spawn_termios(d, true);
        let normal = build_spawn_termios(d, false);
        assert_eq!(
            bench.c_oflag & libc::OPOST,
            0,
            "bench gate must clear OPOST"
        );
        assert_eq!(
            bench.c_oflag | libc::OPOST,
            normal.c_oflag | libc::OPOST,
            "bench gate must change no other oflag"
        );
        assert_eq!(bench.c_iflag, normal.c_iflag);
        assert_eq!(bench.c_lflag, normal.c_lflag);
        assert_eq!(bench.c_cc, normal.c_cc);
    }

    #[test]
    fn spawned_pty_carries_iutf8_and_b230400() {
        // END-TO-END: the explicit termios must actually reach the spawned PTY.
        // Run `sleep` (touches no termios, unlike a shell) and tcgetattr the
        // master — on both BSD and Linux ptys the master reflects the slave's
        // termios, so the two deltas must be visible.
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<String> = vec!["/bin/sleep".into(), "5".into()];
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None, // argv_override
            Some(&exec),
            None,
            None,
        )
        .expect("sleep must spawn");
        assert!(master >= 0);
        // SAFETY: tcgetattr on a valid master fd fills the zeroed out-param.
        let t = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(master, &mut t), 0, "tcgetattr(master)");
            t
        };
        assert_ne!(t.c_iflag & libc::IUTF8, 0, "spawned pty must carry IUTF8");
        // SAFETY: cfgetospeed only reads the termios.
        assert_eq!(unsafe { libc::cfgetospeed(&t) }, libc::B230400);
        // SAFETY: close the master (sleep gets SIGHUP via slave close at exit).
        unsafe {
            libc::close(master);
        }
    }

    /// A DUPLICATE OF THE MASTER IS CLOSE-ON-EXEC TOO.
    ///
    /// The spawn seam above opens both PTY ends close-on-exec FROM BIRTH, but
    /// that guarantee is only as strong as every copy anyone later makes of
    /// them. `dup(2)` is defined by POSIX to CLEAR `FD_CLOEXEC` on the new
    /// descriptor, so the spill drainer — which duplicates the master and holds
    /// its copy for as long as a wedged shell keeps the write queue spilling —
    /// used to hand any concurrently-`exec`ing process a writable descriptor
    /// onto that session's terminal, for that whole period. Found by adversarial
    /// review of the `forkpty` replacement, not by a user report.
    ///
    /// Non-vacuous in both directions: it asserts the ORIGINAL is close-on-exec
    /// (so a regression in the spawn seam cannot make this pass by accident) and
    /// that a plain `dup` of the same fd is NOT — which is the property that
    /// makes `F_DUPFD_CLOEXEC` load-bearing rather than decorative.
    #[test]
    fn a_duplicated_master_stays_close_on_exec() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<String> = vec!["/bin/sleep".into(), "5".into()];
        let master = spawn_shell(
            24, 80, &spawn_cap, &sandbox_cap, &[], None, None, None, Some(&exec), None, None,
        )
        .expect("sleep must spawn");

        // SAFETY: F_GETFD only reads the descriptor flags of a valid fd.
        let flags_of = |fd: i32| unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(
            flags_of(master) & libc::FD_CLOEXEC != 0,
            "PRECONDITION: the spawn seam opens the master close-on-exec"
        );

        let dup = dup_fd(master).expect("dup_fd must succeed on a live master");
        assert!(
            flags_of(std::os::fd::AsRawFd::as_raw_fd(&dup)) & libc::FD_CLOEXEC != 0,
            "a dup_fd copy of the master must be close-on-exec: the drainer holds \
             this for the whole spill, so a bare dup(2) leaks the tty into every \
             process that execs meanwhile"
        );

        // NON-VACUITY: prove the flag is not simply inherited by duplication —
        // a plain `dup` of the SAME fd clears it, which is the defect.
        // SAFETY: `dup` on a valid fd; the result is closed below.
        let bare = unsafe { libc::dup(master) };
        assert!(bare >= 0, "dup must succeed");
        assert_eq!(
            flags_of(bare) & libc::FD_CLOEXEC,
            0,
            "control: POSIX dup(2) clears FD_CLOEXEC, so the assertion above is \
             testing F_DUPFD_CLOEXEC rather than restating inheritance"
        );

        // SAFETY: closing fds this test exclusively owns.
        unsafe {
            libc::close(bare);
            libc::close(master);
        }
    }

    // ---- write_all branch logic (pure, via the extracted classifier) ----
    //
    // The EINTR-retry / short-write / peer-closed branch ladder of `write_all` is
    // a behavior-preserving extraction into `classify_write_result`. Testing the
    // pure classifier covers the EXACT decision the loop drives, WITHOUT having to
    // provoke a real (timing-dependent, flaky) `EINTR`.

    #[test]
    fn classify_write_eintr_negative_retries() {
        // r < 0 with EINTR => retry the write (do not drop the rest of the buffer).
        assert_eq!(classify_write_result(-1, true), WriteStep::Retry);
    }

    #[test]
    fn classify_write_noneintr_error_stops() {
        // r < 0 with any other errno (EIO, EBADF, EPIPE, …) => stop: master is gone.
        assert_eq!(classify_write_result(-1, false), WriteStep::Stop);
    }

    #[test]
    fn classify_write_zero_is_peer_closed_stop() {
        // r == 0 => peer closed; stop draining (errno is irrelevant here).
        assert_eq!(classify_write_result(0, false), WriteStep::Stop);
        assert_eq!(classify_write_result(0, true), WriteStep::Stop);
    }

    #[test]
    fn classify_write_partial_advances_by_exact_count() {
        // r > 0 => advance the cursor by EXACTLY r bytes (short-write handling).
        assert_eq!(classify_write_result(1, false), WriteStep::Advance(1));
        assert_eq!(classify_write_result(4096, false), WriteStep::Advance(4096));
    }

    // ---- locale resolution: the child always runs under a UTF-8 LC_CTYPE ----
    //
    // REGRESSION (the emacs `?` bug): the old GUI guard injected a UTF-8 locale ONLY
    // when LANG/LC_ALL/LC_CTYPE were ALL unset, so a present-but-non-UTF-8 locale
    // (LANG=C, bare en_US, LC_ALL=C, a stray non-UTF-8 LC_CTYPE) reached the child and
    // programs like emacs re-encoded pasted box-drawing UTF-8 to ASCII `?`. These
    // pin `resolve_spawn_locale` (which `build_child_env` then composes onto the env).

    #[test]
    fn is_utf8_locale_classifies_codeset() {
        // UTF-8 codesets in every spelling/case, with and without an @modifier.
        for ok in [
            "en_US.UTF-8",
            "en_US.UTF8",
            "en_US.utf-8",
            "en_US.utf8",
            "de_DE.UTF-8@euro",
        ] {
            assert!(is_utf8_locale(ok), "{ok} should be UTF-8");
        }
        // No codeset, or a non-UTF-8 one, is NOT UTF-8.
        for no in [
            "C",
            "POSIX",
            "en_US",
            "en_US.ISO8859-1",
            "",
            "fr_FR.ISO8859-15@euro",
        ] {
            assert!(!is_utf8_locale(no), "{no} should NOT be UTF-8");
        }
    }

    #[test]
    fn resolve_spawn_locale_edge_cases() {
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        // The injected value is platform-aware (C.UTF-8 off macOS; see SPAWN_UTF8_LOCALE).
        let ctype = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE)];
        let ctype_and_neutralize = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE), kv("LC_ALL", "")];

        // All unset (Finder/.app launch): inject the encoding category.
        assert_eq!(resolve_spawn_locale(None, None, None), ctype());
        // present-but-non-UTF-8 LANG — the emacs `?` repro.
        assert_eq!(resolve_spawn_locale(None, None, Some("C")), ctype());
        assert_eq!(resolve_spawn_locale(None, None, Some("POSIX")), ctype());
        // bare LANG (no codeset).
        assert_eq!(resolve_spawn_locale(None, None, Some("en_US")), ctype());
        // a non-UTF-8 LC_CTYPE shadowing a UTF-8 LANG: override LC_CTYPE; LANG untouched.
        assert_eq!(
            resolve_spawn_locale(None, Some("en_US.ISO8859-1"), Some("en_US.UTF-8")),
            ctype()
        );
        // LC_ALL=C dominating a UTF-8 LANG: override LC_CTYPE AND neutralize LC_ALL,
        // else the LC_CTYPE override would be dead (LC_ALL > LC_CTYPE).
        assert_eq!(
            resolve_spawn_locale(Some("C"), None, Some("en_US.UTF-8")),
            ctype_and_neutralize()
        );
        // both LC_ALL and LC_CTYPE non-UTF-8 at once: LC_ALL wins, same outcome.
        assert_eq!(
            resolve_spawn_locale(Some("C"), Some("en_US.ISO8859-1"), None),
            ctype_and_neutralize()
        );

        // Already UTF-8 anywhere in the effective slot: change NOTHING (no clobber).
        assert!(resolve_spawn_locale(None, None, Some("en_US.UTF-8")).is_empty());
        assert!(resolve_spawn_locale(Some("en_US.UTF-8"), None, None).is_empty());
        assert!(resolve_spawn_locale(None, Some("fr_FR.UTF-8"), Some("C")).is_empty());
        // A UTF-8 LC_ALL must NOT be touched even though a lower slot is non-UTF-8.
        assert!(resolve_spawn_locale(Some("en_US.UTF-8"), Some("C"), Some("C")).is_empty());
        // set-but-empty falls through (POSIX): empty LC_ALL/LC_CTYPE + UTF-8 LANG -> nothing.
        assert!(resolve_spawn_locale(Some(""), Some(""), Some("en_US.UTF-8")).is_empty());
        // UTF-8 spelling variants are all recognized (no needless override).
        for v in ["en_US.UTF8", "en_US.utf-8", "de_DE.UTF-8@euro"] {
            assert!(
                resolve_spawn_locale(None, None, Some(v)).is_empty(),
                "{v} is UTF-8"
            );
        }
    }

    /// CONFORMANCE: drive the REAL `resolve_spawn_locale` + `build_child_env` over
    /// every inherited-locale shape and assert the child's effective `LC_CTYPE` is
    /// UTF-8. The UTF-8/precedence oracle here is written INDEPENDENTLY of
    /// `is_utf8_locale` (a `.ends_with` check vs the production codeset parse) so a
    /// shared predicate bug cannot make the assertion vacuous.
    #[test]
    fn spawn_locale_conformance_child_always_utf8_ctype() {
        use std::ffi::OsString;

        #[derive(Clone, Copy)]
        enum Cls {
            Unset,
            Empty,
            NonUtf8,
            Utf8,
        }
        // Representative concrete value per class (None = the var is unset).
        let val = |c: Cls| match c {
            Cls::Unset => None,
            Cls::Empty => Some(""),
            Cls::NonUtf8 => Some("C"),
            Cls::Utf8 => Some("en_US.UTF-8"),
        };
        // Independent codeset check (different impl than `is_utf8_locale`).
        let looks_utf8 = |s: &str| {
            let lo = s.to_ascii_lowercase();
            lo.ends_with(".utf-8") || lo.ends_with(".utf8")
        };
        // Effective LC_CTYPE of a composed child env (POSIX precedence, empty==unset).
        let child_ctype_utf8 = |env: &[(OsString, OsString)]| -> bool {
            let get = |k: &str| {
                env.iter()
                    .find(|(ek, _)| ek.to_str() == Some(k))
                    .map(|(_, v)| v.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
            };
            match get("LC_ALL")
                .or_else(|| get("LC_CTYPE"))
                .or_else(|| get("LANG"))
            {
                None => false, // "C" default
                Some(loc) => looks_utf8(&loc),
            }
        };

        let classes = [Cls::Unset, Cls::Empty, Cls::NonUtf8, Cls::Utf8];
        let mut checked = 0u32;
        for &a in &classes {
            for &c in &classes {
                for &l in &classes {
                    // Build the inherited env the child would have started from.
                    let mut inherited: Vec<(OsString, OsString)> =
                        vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
                    for (k, cl) in [("LC_ALL", a), ("LC_CTYPE", c), ("LANG", l)] {
                        if let Some(v) = val(cl) {
                            inherited.push((OsString::from(k), OsString::from(v)));
                        }
                    }

                    let overrides = resolve_spawn_locale(val(a), val(c), val(l));

                    // INDEPENDENT "was the inherited effective locale already UTF-8?"
                    let ne = |cl: Cls| val(cl).filter(|s| !s.is_empty());
                    let orig_utf8 = ne(a).or(ne(c)).or(ne(l)).map(looks_utf8).unwrap_or(false);

                    // No-clobber & always-fix: overrides are empty IFF already UTF-8.
                    assert_eq!(
                        overrides.is_empty(),
                        orig_utf8,
                        "overrides emptiness must track already-UTF-8 for (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?})",
                        val(a),
                        val(c),
                        val(l)
                    );

                    // A dominating non-UTF-8 LC_ALL must be neutralized, else the
                    // LC_CTYPE override would be powerless.
                    if matches!(a, Cls::NonUtf8) {
                        assert!(
                            overrides.iter().any(|(k, v)| k == "LC_ALL" && v.is_empty()),
                            "non-UTF-8 LC_ALL must be neutralized; got {overrides:?}"
                        );
                    }

                    // THE INVARIANT: the child the terminal spawns is UTF-8.
                    let child = build_child_env(inherited.into_iter(), &overrides);
                    assert!(
                        child_ctype_utf8(&child),
                        "child LC_CTYPE NOT UTF-8 for inherited (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?}); env={:?}",
                        val(a),
                        val(c),
                        val(l),
                        child
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked, 64,
            "all 4x4x4 inherited-locale shapes must be exercised"
        );
    }

    // ---- read() syscall wrapper: EOF and bad-fd error contract ----

    // EOF: when the write end of a pipe is closed and the buffer is drained, a
    // `read` of the read end returns exactly 0 (not negative, not a partial-read
    // surprise). This is the `0 = EOF` half of the documented `read` contract.
    #[test]
    fn read_returns_zero_on_eof_after_write_end_closed() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);
        // Close the only write end with no data pending => the next read sees EOF.
        // SAFETY: `wr` is the pipe write end we just opened.
        unsafe {
            libc::close(wr);
        }
        let mut buf = [0u8; 16];
        let n = read(rd, &mut buf);
        assert_eq!(n, 0, "read at EOF must return 0, got {n}");
        // SAFETY: closing the read end we opened.
        unsafe {
            libc::close(rd);
        }
    }

    // Error: a `read` of an invalid descriptor must return a negative value (the
    // `< 0 = error` half of the contract), with `errno == EBADF`. We use fd -1,
    // which is never a valid descriptor, so this is hermetic and deterministic and
    // never touches a real, possibly-open fd. (We assert the raw errno, not
    // `ErrorKind`, because libstd categorizes EBADF as `Uncategorized` here — the
    // stable contract is the negative return + the POSIX errno, not the kind.)
    #[test]
    fn read_returns_negative_with_ebadf_on_invalid_fd() {
        let mut buf = [0u8; 16];
        let n = read(-1, &mut buf);
        assert!(n < 0, "read on a bad fd must be negative, got {n}");
        let err = io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EBADF),
            "read on a bad fd must set errno=EBADF, got {err}",
        );
        // And EBADF is NOT EINTR, so the read loop would STOP (not spin-retry) on it
        // — the very decision the classifier encodes.
        assert_ne!(err.kind(), io::ErrorKind::Interrupted);
    }

    // Zero-length buffer: `read(2)` with count 0 returns 0 WITHOUT consuming any
    // of the pending bytes. This is the third arm of the wrapper's return
    // contract, and the one that is easy to get wrong: `buf.as_mut_ptr()` on an
    // empty slice is a dangling-but-aligned pointer, so a wrapper that did any
    // pointer math or passed a bogus count would fault or, worse, silently eat
    // the queued data. We prove BOTH halves — the 0 return AND that the data is
    // still there afterwards — so a regression cannot hide behind the 0.
    #[test]
    fn read_into_an_empty_buffer_reads_nothing() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);

        // Queue data on the pipe so a "reads nothing" result is meaningful: the
        // read end is READABLE, so a 0 return can only come from the count, not
        // from an empty pipe. (Well under the pipe buffer, so no blocking.)
        let pending: &[u8] = b"data-is-waiting";
        write_all(wr, pending);

        let mut empty: [u8; 0] = [];
        let n = read(rd, &mut empty);
        assert_eq!(
            n, 0,
            "zero-length read must return 0 even with data pending"
        );

        // The pending bytes must be UNCONSUMED — a real read still sees them all.
        let mut buf = [0u8; 64];
        let n = read(rd, &mut buf);
        assert!(n > 0, "the queued bytes must survive the zero-length read");
        assert_eq!(
            &buf[..n as usize],
            pending,
            "the zero-length read must not have consumed any pending byte",
        );

        // SAFETY: closing both pipe ends we opened above.
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    // ---- write_all drains a buffer larger than one pipe write (partial writes) ----

    // A pipe's kernel buffer is finite (typically 16–64 KiB), so a single
    // `write(2)` of a buffer larger than the pipe capacity CANNOT move all the
    // bytes at once: the kernel returns a short count and `write_all` must loop to
    // drain the remainder. A dedicated reader thread keeps draining so the writer
    // never blocks forever; we assert the bytes arrive byte-for-byte, in order,
    // for the full payload. This exercises the real `Advance(n)` short-write path
    // of `write_all` on a live fd (not just the pure classifier).
    #[test]
    fn write_all_drains_payload_larger_than_one_pipe_write() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);

        // 1 MiB — far larger than any pipe buffer, so >=1 short write is forced.
        // A deterministic, position-dependent pattern catches reorder/drop bugs.
        let n_bytes = 1usize << 20;
        let payload: Vec<u8> = (0..n_bytes).map(|i| (i % 251) as u8).collect();

        // Drain thread: read the read end to completion (until EOF) and return what
        // it saw. It must run concurrently with the writer or the pipe deadlocks.
        let reader = std::thread::spawn(move || {
            // Clamp the pre-size HINT (advisory; the Vec grows on demand) so
            // the bulk allocation carries a provable bound for the L0 gate.
            let mut got = Vec::with_capacity(n_bytes.min(1 << 20));
            let mut chunk = [0u8; 8192];
            loop {
                let r = read(rd, &mut chunk);
                if r <= 0 {
                    break; // 0 = EOF (writer closed), <0 = error
                }
                got.extend_from_slice(&chunk[..r as usize]);
            }
            // SAFETY: closing the read end this thread owns.
            unsafe {
                libc::close(rd);
            }
            got
        });

        write_all(wr, &payload);
        // Close the write end so the reader observes EOF and the thread joins.
        // SAFETY: `wr` is the write end this thread owns after `write_all`.
        unsafe {
            libc::close(wr);
        }

        let got = reader.join().expect("reader thread panicked");
        assert_eq!(got.len(), payload.len(), "drained byte count mismatch");
        assert!(
            got == payload,
            "drained bytes differ from the payload byte-for-byte"
        );
    }

    // ---- fail-closed spawn: under-tier capability is denied WITHOUT forking ----

    // An under-tier `Cap<Spawn>` (Untrusted, below the required Trusted) must be
    // rejected by the PARENT gate BEFORE any fork — there must be no way to
    // spawn a child with an insufficient capability. We assert PermissionDenied;
    // the absence of a leaked child is implicit (no fork happened, so there is
    // nothing to reap), and the error originates from `aterm_cap::require`, not
    // from a child status byte.
    #[test]
    fn under_tier_spawn_cap_is_denied_before_forking() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        // Untrusted spawn cap: below the Trusted floor `spawn_shell` requires.
        let weak_spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Untrusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        let result = spawn_shell(
            24,
            80,
            &weak_spawn,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        );
        let err = result.expect_err("an under-tier spawn cap must be denied, not spawn a shell");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "under-tier spawn must be PermissionDenied, got: {err}",
        );
    }

    // ---- fail-closed spawn: a child that cannot exec takes the _exit(127) path ----

    // The exec-failure path through the REAL production code: a `-e` command naming
    // a nonexistent absolute program forces the child's `execve` to fail, so the
    // child writes the b'E' status byte and `_exit(127)`s. The parent reads that
    // byte off the status pipe, reaps the child internally, and surfaces an
    // `io::Error` (ErrorKind::Other) describing the pre-exec exec failure — never a
    // master fd. This drives a real fork/exec + the full status-pipe protocol.
    //
    // NOTE on "$SHELL in the child": `spawn_shell` resolves the exec target in the
    // PARENT (it must, to stay async-signal-safe in the child), so a bogus `$SHELL`
    // can only be injected by mutating the parent's env — which is a data race
    // against the multi-threaded test harness under edition 2024. We therefore
    // drive the SAME child exec-failure path hermetically via a bogus `exec_command`
    // (no env mutation). The raw 127 exit code is consumed by `spawn_shell`'s own
    // `waitpid` reap, so it is not observable here; the contract that exit code 127
    // is what a bogus `execve` yields is locked by the sibling test below.
    #[test]
    fn bogus_exec_command_takes_child_exec_failure_path() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        // An absolute path that cannot exist => `resolve_program` returns it
        // verbatim => the child's `execve` fails => b'E' + _exit(127).
        let bogus = vec![String::from("/nonexistent/aterm-pty-no-such-prog-xyz")];
        let result = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&bogus),
            None,
            None,
        );
        let err =
            result.expect_err("a child that cannot exec must surface an error, not a master fd");
        assert_eq!(
            err.kind(),
            io::ErrorKind::Other,
            "exec failure before exec must be reported as Other, got: {err}",
        );
        assert!(
            err.to_string().contains("127"),
            "error should describe the _exit(127) exec failure: {err}",
        );
    }

    // Contract lock for the exit code the design depends on: a child that writes a
    // status byte and `_exit(127)`s after a failed `execve` is reaped by the parent
    // with the WEXITSTATUS == 127 the spawn protocol claims. It keeps using the
    // libc `forkpty` DELIBERATELY, as an independent oracle: `spawn_shell` no
    // longer calls it (see `open_pty_pair_cloexec`), so this locks the OS
    // primitive's execve→127 contract without going through the code under test.
    // It is therefore no longer a mirror of `spawn_shell`'s child syscall shape —
    // it never asserted that anyway. It ASSERTS the raw exit code — which `spawn_shell` itself
    // consumes during its internal reap, so it cannot be observed through that API.
    // It is a contract test of the OS primitive, NOT a re-implementation of product
    // logic: it locks "bogus execve => _exit(127), reapable" so a future change to
    // the child's exit code would be caught here.
    #[test]
    // Linux-only: this contract test forks the PROCESS with `forkpty` and runs the
    // child to `execve`/`_exit` INSIDE the libtest harness. On Linux the harness's
    // threaded runtime does not survive a raw fork — strace shows the child
    // deterministically `exit_group(1)`ing before its `execve` ever runs (true for
    // both `libc::execve` and a raw `SYS_execve`), so the child's exit code is the
    // harness's, not the test's. This is a harness↔fork incompatibility, NOT a
    // product defect: the SAME execve-failure → 127 contract on the real spawn path
    // is verified by `bogus_exec_command_takes_child_exec_failure_path` (which drives
    // `spawn_shell` and passes), and the live GUI spawn works. macOS's harness
    // tolerates the fork, so the raw-primitive lock still runs there.
    #[cfg_attr(
        target_os = "linux",
        ignore = "forkpty inside the libtest harness can't run the child to exec on Linux; \
                  the execve→127 contract is covered by bogus_exec_command_takes_child_exec_failure_path"
    )]
    fn child_exec_failure_exit_code_is_127_and_reapable() {
        let mut master: libc::c_int = -1;
        let ws = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid out-param for the master fd, null for the unused name/termios
        // buffers, and a valid winsize; returns the child pid (parent) or 0 (child).
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::addr_of!(ws).cast_mut(),
            )
        };
        assert!(pid >= 0, "forkpty failed: {}", io::Error::last_os_error());
        if pid == 0 {
            // CHILD — async-signal-safe only: attempt to exec a nonexistent program
            // (mirroring the child's `execve`), then take the _exit(127) failure
            // path exactly as `spawn_shell`'s child does.
            // SAFETY: a NUL-terminated absolute path; on `execve` failure we _exit.
            unsafe {
                let prog = b"/nonexistent/aterm-pty-no-such-prog-xyz\0";
                let argv: [*const libc::c_char; 2] =
                    [prog.as_ptr().cast::<libc::c_char>(), ptr::null()];
                let envp: [*const libc::c_char; 1] = [ptr::null()];
                libc::execve(
                    prog.as_ptr().cast::<libc::c_char>(),
                    argv.as_ptr(),
                    envp.as_ptr(),
                );
                libc::_exit(127);
            }
        }
        // PARENT: reap the child and assert the exit code.
        //
        // REAP FIRST, CLOSE SECOND — and that order is load-bearing, not style.
        // `close(master)` REVOKES the pty, which sends SIGHUP to the foreground
        // process group; this child is that group's leader (`forkpty` =
        // `openpty` + `fork` + `login_tty`, and `login_tty` calls `setsid`), and
        // SIGHUP's default disposition is terminate. Closing first therefore
        // RACES the child's `execve`-fails-then-`_exit(127)`, and when the close
        // wins the child dies by signal — `WIFEXITED` is false and the exit code
        // this test exists to lock is never produced. MEASURED at 1-in-30 runs on
        // otherwise-unmodified code once the machine is CPU-saturated (`sig=1`),
        // i.e. a latent flake that only ever needed the scheduler to look the
        // other way. Nothing needs the close to happen first: the child holds the
        // slave as its own stdio and exits on its own, so waiting for it and only
        // then dropping the master removes the race entirely rather than making
        // it rarer.
        let mut wstatus: libc::c_int = 0;
        // SAFETY: reaping the child we just forked; `wstatus` is a valid out-param.
        let w = unsafe { libc::waitpid(pid, &mut wstatus, 0) };
        // SAFETY: `master` is the forkpty master, closed now that the child has
        // been reaped and its exit status is already in hand.
        unsafe {
            libc::close(master);
        }
        assert_eq!(w, pid, "waitpid did not reap our child");
        assert!(
            libc::WIFEXITED(wstatus),
            "child did not exit normally (signalled: {}, sig {}) — a pty revoke \
             must not be able to race this child's _exit(127): {wstatus}",
            libc::WIFSIGNALED(wstatus),
            libc::WTERMSIG(wstatus)
        );
        assert_eq!(
            libc::WEXITSTATUS(wstatus),
            127,
            "a failed execve child must _exit(127)",
        );
    }

    // ---- OS-sandbox wrap (sandbox_wrap) ----

    // The seam's inlined wrapper path MUST be the SAME bytes as the policy crate's
    // canonical SANDBOX_EXEC_PATH. They are kept in lockstep by hand (the seam
    // stays dependency-light), so this test fails loudly if either drifts.
    #[test]
    fn inlined_sandbox_exec_path_matches_policy_crate() {
        assert_eq!(SANDBOX_EXEC_PATH, aterm_containment::SANDBOX_EXEC_PATH);
        assert_eq!(SANDBOX_EXEC_PATH, "/usr/bin/sandbox-exec");
    }

    // FAIL-CLOSED: when the wrapper binary is absent at the given path,
    // build_sandbox_wrap returns NotFound — the caller (`spawn_shell`) propagates
    // it and NEVER forks, so a policy-demanded sandbox that can't be applied
    // refuses to spawn rather than silently running an unsandboxed shell. We point
    // it at a guaranteed-nonexistent path to drive this without disturbing the real
    // /usr/bin/sandbox-exec.
    #[test]
    fn build_sandbox_wrap_fails_closed_when_wrapper_missing() {
        let prog = CString::new("/bin/zsh").unwrap();
        let argv = vec![CString::new("-zsh").unwrap()];
        let err = build_sandbox_wrap(
            "/nonexistent/aterm-no-such-sandbox-exec",
            aterm_containment::NETWORK_DENY_PROFILE,
            &prog,
            &argv,
        )
        .expect_err("a missing wrapper must fail closed, not silently skip the sandbox");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "fail-closed kind: {err}"
        );
        assert!(
            err.to_string().contains("fail-closed"),
            "error must describe the fail-closed refusal: {err}",
        );
    }

    // The wrapped argv has the exact shape the kernel needs: sandbox-exec, -p,
    // <profile>, <program-path>, then the original args AFTER argv[0]. The login
    // argv[0] ("-zsh") is replaced by the program PATH; "--rcfile FILE" style real
    // args are carried through verbatim. Uses the REAL /usr/bin/sandbox-exec path
    // (present on macOS) so the access() probe passes.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_sandbox_wrap_produces_correct_argv_shape() {
        let prog = CString::new("/bin/zsh").unwrap();
        // Original argv: a login-shell argv[0] plus a real flag+value pair.
        let argv = vec![
            CString::new("-zsh").unwrap(),
            CString::new("--rcfile").unwrap(),
            CString::new("/tmp/rc").unwrap(),
        ];
        let (target, wrapped) = build_sandbox_wrap(
            SANDBOX_EXEC_PATH,
            aterm_containment::NETWORK_DENY_PROFILE,
            &prog,
            &argv,
        )
        .expect("wrapper present → build succeeds");
        assert_eq!(target.to_str().unwrap(), SANDBOX_EXEC_PATH);
        let got: Vec<&str> = wrapped.iter().map(|c| c.to_str().unwrap()).collect();
        assert_eq!(
            got,
            vec![
                "sandbox-exec",
                "-p",
                aterm_containment::NETWORK_DENY_PROFILE,
                "/bin/zsh", // argv[0] replaced by the program PATH
                "--rcfile", // real args carried through verbatim …
                "/tmp/rc",  // …
            ],
            "wrapped argv shape must be sandbox-exec -p <sbpl> <prog> <orig argv[1..]>",
        );
    }

    // Default (no-wrap) spawn is byte-identical: passing `sandbox_wrap = None` must
    // NOT change the exec target — it stays `$SHELL`, never `sandbox-exec`. We
    // assert this through the SAME `-e` echo path used elsewhere: with no wrap, a
    // `-e /bin/echo MARKER` runs `/bin/echo` directly (argv[0] == the program), so
    // the PTY shows exactly "MARKER" with no sandbox-exec banner/argv mutation.
    #[test]
    fn no_wrap_spawn_runs_program_directly_unchanged() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let cmd = vec![
            String::from("/bin/echo"),
            String::from("ATERM-NOWRAP-MARKER"),
        ];
        // sandbox_wrap = None → no wrap, byte-identical spawn.
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&cmd),
            None,
            None,
        )
        .expect("unwrapped -e command must spawn");
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        for _ in 0..50 {
            let n = read(master, &mut buf);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
            if out
                .windows(b"ATERM-NOWRAP-MARKER".len())
                .any(|w| w == b"ATERM-NOWRAP-MARKER")
            {
                break;
            }
        }
        // SAFETY: tear down the child.
        unsafe {
            libc::close(master);
        }
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("ATERM-NOWRAP-MARKER"),
            "echo output not seen: {s:?}"
        );
        assert!(
            !s.contains("sandbox-exec"),
            "no-wrap spawn must NOT involve sandbox-exec: {s:?}",
        );
    }

    // The wrap path is well-formed AND actually applies Seatbelt: wrap a `-e`
    // command in the real `(deny network*)` profile and run `/usr/bin/nc` against a
    // live loopback listener bound in this parent. WITHOUT the wrap nc connects;
    // WITH the wrap the kernel denies network so nc cannot connect — observed via
    // the child's exit code (the wrapped sandbox-exec→nc child fails). This drives
    // the REAL `spawn_shell` wrap-argv construction end to end, not just a direct
    // sandbox-exec call.
    #[cfg(target_os = "macos")]
    #[test]
    fn wrapped_spawn_enforces_network_deny_via_seatbelt() {
        use std::io::Write;
        use std::net::TcpListener;

        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        // Loopback listener in the parent + a draining accept thread.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let accepter = std::thread::spawn(move || {
            for _ in 0..2 {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.write_all(b"x");
                    }
                    Err(_) => break,
                }
            }
        });
        let port_s = port.to_string();

        // Control: unwrapped `-e nc` to the listener CONNECTS (so the probe works).
        // We can't read nc's exit code through spawn_shell's API, so prove the
        // control via a direct connect from the parent instead, then focus the
        // wrapped assertion on the seam producing a sandbox-exec'd child that the
        // kernel network-denies (nc fails → its PTY closes quickly with no data
        // that looks like a successful connect).
        let probe = std::net::TcpStream::connect(("127.0.0.1", port));
        assert!(
            probe.is_ok(),
            "loopback listener must be connectable (probe)"
        );
        drop(probe);

        // Wrapped `-e nc` under (deny network*). The wrap is built by spawn_shell:
        // sandbox-exec -p <profile> /usr/bin/nc <args>. The connect is denied.
        let nc = vec![
            String::from("/usr/bin/nc"),
            String::from("-G"),
            String::from("1"),
            String::from("-w"),
            String::from("1"),
            String::from("-z"),
            String::from("127.0.0.1"),
            port_s.clone(),
        ];
        let profile = aterm_containment::NETWORK_DENY_PROFILE;
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&nc),
            None,
            Some(profile),
        )
        .expect("wrapped -e nc must spawn (sandbox-exec applies the profile)");
        // Drain to EOF (the child exits fast: nc's connect is denied). The success
        // banner "succeeded!" must NOT appear — a denied connect never prints it.
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        for _ in 0..200 {
            let n = read(master, &mut buf);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: tear down the child.
        unsafe {
            libc::close(master);
        }
        let _ = std::net::TcpStream::connect(("127.0.0.1", port)); // unblock accepter
        let _ = accepter.join();
        let s = String::from_utf8_lossy(&out);
        assert!(
            !s.contains("succeeded"),
            "DENY FAILED: wrapped nc reported a successful connect under (deny network*): {s:?}",
        );
    }
}

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
/// required — an absent Trust `ty` FAILS the test with a build hint (`cargo build
/// --release -p tla-cli` in $HOME/trust/first-party/ty).
#[cfg(test)]
mod writeall_conformance {
    use super::{WriteStep, classify_write_result, write_all};
    use aterm_spec::verify::ty_escalation;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The bounded buffer length the conformance trajectory uses (matches the cfg
    /// `Size` we pin below). Small so the ty runs are cheap.
    const SIZE: i64 = 5;

    /// The committed `WriteAll.tla`, with `Init` PARAMETERIZED (`off = off_init /\
    /// done = done_init`) so any predecessor state can be the strict first step. The
    /// rewrite touches ONLY the `CONSTANTS` line (adds `off_init, done_init`) and the
    /// `Init ==` line; every `Progress`/`Interrupted`/`Next`/invariant line is the
    /// committed text verbatim, so the actions cannot drift from the checked spec.
    fn parameterized_spec() -> String {
        let committed =
            std::fs::read_to_string(spec_path("WriteAll.tla")).expect("read WriteAll.tla");
        let mut out = String::new();
        for line in committed.lines() {
            let t = line.trim_start();
            if t.starts_with("CONSTANTS Size, Buggy") {
                out.push_str("CONSTANTS Size, Buggy, off_init, done_init\n");
            } else if t.starts_with("Init ==") {
                out.push_str("Init == off = off_init /\\ done = done_init\n");
            } else if t.starts_with("/\\ off = 0") || t.starts_with("/\\ done = FALSE") {
                // The two old Init conjunct lines — dropped (subsumed by the rewrite).
                continue;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    fn spec_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("aterm-spec-models/specs")
            .join(name)
    }

    fn transition_trace(prev: (i64, bool), next: (i64, bool), action: &str) -> String {
        let st = |off: i64, done: bool| {
            format!(
                "{{\"off\":{{\"type\":\"int\",\"value\":{off}}},\"done\":{{\"type\":\"bool\",\"value\":{done}}}}}"
            )
        };
        format!(
            "{{\"version\":\"1\",\"module\":\"WriteAll\",\"variables\":[\"off\",\"done\"],\"steps\":[\
             {{\"index\":0,\"state\":{}}},\
             {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
             ]}}",
            st(prev.0, prev.1),
            st(next.0, next.1),
            action
        )
    }

    fn validate(
        ty: &Path,
        dir: &Path,
        spec: &str,
        prev: (i64, bool),
        next: (i64, bool),
        action: &str,
    ) -> (bool, String) {
        let spec_f = dir.join("WriteAll.tla");
        let cfg_f = dir.join("WriteAll.cfg");
        let trace_f = dir.join("t.json");
        std::fs::write(&spec_f, spec).expect("write spec");
        std::fs::write(
            &cfg_f,
            format!(
                "CONSTANT Size = {SIZE}\nCONSTANT Buggy = FALSE\n\
                 CONSTANT off_init = {}\nCONSTANT done_init = {}\n\
                 SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n",
                prev.0,
                if prev.1 { "TRUE" } else { "FALSE" } // TLA+ booleans are UPPERCASE
            ),
        )
        .expect("write cfg");
        std::fs::write(&trace_f, transition_trace(prev, next, action)).expect("write trace");
        let out = Command::new(ty)
            .arg("trace")
            .arg("validate")
            .arg(&trace_f)
            .arg("--spec")
            .arg(&spec_f)
            .arg("--config")
            .arg(&cfg_f)
            .output()
            .expect("run ty trace validate");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), combined)
    }

    /// Replay the REAL `write_all` loop's `off`/`done` trajectory over a scripted
    /// sequence of `write(2)` returns, using the REAL [`classify_write_result`]
    /// decision and the REAL `data = &data[n..]` advance. Returns the sequence of
    /// `(off, done, action_label)` transitions the loop takes. `done` is set on the
    /// step that exhausts the buffer (`data.is_empty()`), exactly as `write_all`'s
    /// `while !data.is_empty()` exit reports completion.
    fn replay(returns: &[(isize, bool)]) -> Vec<(i64, bool, &'static str)> {
        let total = SIZE;
        let mut off: i64 = 0;
        let mut steps = Vec::new();
        for &(r, is_eintr) in returns {
            if off >= total {
                break; // loop already exited (done)
            }
            match classify_write_result(r, is_eintr) {
                WriteStep::Retry => {
                    // EINTR: off unchanged, not done — the spec's `Interrupted` with k=0.
                    steps.push((off, false, "Interrupted"));
                }
                WriteStep::Stop => break, // peer closed / real error — loop ends, not "done"
                WriteStep::Advance(n) => {
                    off += n as i64;
                    let done = off >= total;
                    // A step that finishes the buffer is `Progress`; a short write that
                    // does NOT finish is `Interrupted` (advance-but-not-complete).
                    let label = if done { "Progress" } else { "Interrupted" };
                    steps.push((off, done, label));
                }
            }
        }
        steps
    }

    #[test]
    fn real_write_all_drains_a_large_payload_over_a_pipe() {
        // END-TO-END: a payload far larger than a pipe buffer + a draining reader.
        // The real `write_all` MUST deliver every byte — `NoSilentDrop` end-to-end.
        let mut fds = [0i32; 2];
        // SAFETY: valid 2-element out-array for pipe(2).
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);
        let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect(); // 1 MiB > pipe buf
        let expect = payload.clone();
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: rd is a valid pipe read end; buf is a valid 4096-byte buffer.
                let n = unsafe { libc::read(rd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n as usize]);
            }
            // SAFETY: own pipe read end; close once drained.
            unsafe { libc::close(rd) };
            got
        });
        write_all(wr, &payload);
        // SAFETY: own pipe write end; closing signals EOF to the reader.
        unsafe { libc::close(wr) };
        let got = reader.join().expect("reader thread");
        assert_eq!(
            got.len(),
            expect.len(),
            "write_all dropped bytes — NoSilentDrop violated"
        );
        assert_eq!(got, expect, "write_all delivered corrupted/reordered bytes");
    }

    #[test]
    fn real_write_all_offset_trajectory_conforms_to_writeall_spec() {
        // TIERED (VERIFY-1): a committed hand-written `.tla` — external-tool
        // obligation; runs only where the Trust toolchain is installed.
        let Some(ty) = ty_escalation("WriteAll conformance") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("aterm-writeall-conf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk tempdir");
        let spec = parameterized_spec();

        // A scripted run of `write(2)` returns the real loop classifies+advances:
        //   short write (+2), EINTR retry (0 advance), short write (+1), full tail (+2).
        // Total = SIZE (5). This exercises BOTH spec actions: `Interrupted` (short
        // writes and the EINTR) and `Progress` (the final completing write).
        let returns = [(2isize, false), (-1, true), (1, false), (2, false)];
        let steps = replay(&returns);
        assert_eq!(
            steps.last().map(|s| (s.0, s.1)),
            Some((SIZE, true)),
            "loop must finish at off=Size, done"
        );

        // POSITIVE: each real transition strictly conforms to WriteAll's `Next`.
        let mut prev = (0i64, false);
        let mut validated = 0usize;
        for &(off, done, action) in &steps {
            let next = (off, done);
            let (ok, out) = validate(&ty, &dir, &spec, prev, next, action);
            assert!(
                ok,
                "real {action} transition {prev:?} -> {next:?} must conform to WriteAll\n--- ty ---\n{out}"
            );
            prev = next;
            validated += 1;
        }

        // NEGATIVE CONTROL — the dropped-tail bug: a `Progress` step that claims
        // `done` while `off < Size`. `NoSilentDrop` forbids it; ty MUST reject.
        let (bad_ok, o) = validate(&ty, &dir, &spec, (1, false), (3, true), "Progress");
        assert!(
            !bad_ok,
            "NEGATIVE CONTROL (done with off=3 < Size=5 — dropped tail) MUST be rejected\n--- ty ---\n{o}"
        );

        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "WriteAll Tier-1 conformance: {validated} real loop transitions (short writes + EINTR + \
             completing write) strictly validated against committed WriteAll.tla; dropped-tail \
             negative control rejected."
        );
    }
}
