// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every OS call the lab makes outside `std`, each behind a safe wrapper: the
//! crate's one `unsafe` module, so an audit reads one file.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

/// Whether `pid` names a live process this user may signal. Never 0, 1 or a
/// negative number: `kill(0, 0)` would ask about our own process group.
#[must_use]
pub fn alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 sends nothing; it only checks the pid.
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

/// Send `sig` to `pid` (a pid above 1 only: see [`alive`]).
///
/// # Errors
/// The kernel's refusal, or `InvalidInput` for a pid of 1 or less.
pub fn kill(pid: i32, sig: i32) -> io::Result<()> {
    if pid <= 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "pid <= 1"));
    }
    // SAFETY: a plain `kill(2)` on a positive pid other than init.
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Send `sig` to every process in the group `pgid` leads (a `setsid` child's
/// pid; above 1 only, since `killpg(0, …)` would signal our own group).
///
/// # Errors
/// The kernel's refusal, or `InvalidInput` for a group of 1 or less.
pub fn kill_group(pgid: i32, sig: i32) -> io::Result<()> {
    if pgid <= 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "pgid <= 1"));
    }
    // SAFETY: a plain `killpg(2)` on a positive group other than init's.
    if unsafe { libc::killpg(pgid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `kill(pid, 0)` as the probe judges it: `Ok` or the errno.
///
/// # Errors
/// The kernel's refusal.
pub fn signal_probe(pid: i32, sig: i32) -> io::Result<()> {
    // SAFETY: as [`kill`]; the probe's pids are its own parent, itself and its
    // own children, never 0 or 1.
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// This process's parent.
#[must_use]
pub fn getppid() -> i32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::getppid() }
}

/// The kernel session containing a live process, when it can be read.
#[must_use]
pub fn session_id(pid: i32) -> Option<i32> {
    if pid <= 1 {
        return None;
    }
    // SAFETY: getsid only reads the kernel record for a positive process id.
    let sid = unsafe { libc::getsid(pid) };
    (sid >= 0).then_some(sid)
}

/// This process's user id.
#[must_use]
pub fn getuid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::getuid() }
}

/// This user's login name from the password database (`id -un`), never
/// from the environment, which `env -i` may have emptied.
#[must_use]
pub fn login_name() -> Option<String> {
    // SAFETY: `passwd` is plain data (pointers and integers); all-zero is a valid value.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `pw`, `buf` and `out` live for the call; `buf.len()` is its size.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &raw mut pw,
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut out,
        )
    };
    if rc != 0 || out.is_null() || pw.pw_name.is_null() {
        return None;
    }
    // SAFETY: on success `pw_name` points at a NUL-terminated string inside `buf`.
    let name = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) };
    Some(name.to_string_lossy().into_owned()).filter(|n| !n.is_empty())
}

/// Remove every variable `forget` names from this process's environment.
/// Called first in `main`, before any thread exists.
pub fn forget_env(forget: fn(&str) -> bool) {
    let keys: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.to_str().is_some_and(forget))
        .collect();
    for k in keys {
        // SAFETY: called before any other thread starts, so nothing reads the environment concurrently.
        unsafe { std::env::remove_var(k) };
    }
}

/// Nothing the lab runs OUTSIDE the jail may be, or be chosen by, something a
/// jailed process could have written: a bare-name program comes from the
/// system's own directories only (`PATH=/usr/bin:/bin:/usr/sbin:/sbin`;
/// SIP: no process of this user writes there, jailed or not), and the xcrun
/// shims among them (`git`, `cc`, `python3`, …) resolve from the developer
/// dir, never from the per-user cache any process of the user may write
/// (`xcrun_nocache=1`, handed to everything the lab starts). The lab then
/// works from `/`, never from the caller's directory, which may be the lab's
/// project dir; a relative path the caller gave (`LAB_CLAUDE`, `LAB_CODEX`)
/// is made absolute against that directory first, once, so a nested call
/// reads the same file. Called first in `main`, before any thread exists.
pub fn confine_env() {
    let caller = crate::paths::caller_dir();
    for k in ["LAB_CLAUDE", "LAB_CODEX"] {
        if let Ok(v) = std::env::var(k)
            && !v.is_empty()
            && !v.starts_with('/')
        {
            let abs = format!("{}/{v}", caller.trim_end_matches('/'));
            // SAFETY: called before any other thread starts (as [`forget_env`]).
            unsafe { std::env::set_var(k, abs) };
        }
    }
    // SAFETY: as above.
    unsafe {
        std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        std::env::set_var("xcrun_nocache", "1");
    }
    let _ = std::env::set_current_dir("/");
}

/// Whether `path` may be executed by this process (`access(X_OK)`).
#[must_use]
pub fn executable(path: &str) -> bool {
    let Ok(c) = CString::new(path) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

unsafe extern "C" {
    fn mkdtemp(template: *mut libc::c_char) -> *mut libc::c_char;
    fn localtime_r(clock: *const libc::time_t, out: *mut Tm) -> *mut Tm;
}

/// A new private directory from a `mkdtemp(3)` template (`/tmp/at.XXXX`).
///
/// # Errors
/// The template holds a NUL, or `mkdtemp` failed.
pub fn make_temp_dir(template: &str) -> io::Result<String> {
    let c = CString::new(template).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut buf = c.into_bytes_with_nul();
    // SAFETY: `buf` is a writable, NUL-terminated template that outlives the call.
    let p = unsafe { mkdtemp(buf.as_mut_ptr().cast()) };
    if p.is_null() {
        return Err(io::Error::last_os_error());
    }
    buf.pop();
    String::from_utf8(buf).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

/// `struct tm` as macOS and glibc lay it out.
#[repr(C)]
struct Tm {
    tm_sec: libc::c_int,
    tm_min: libc::c_int,
    tm_hour: libc::c_int,
    tm_mday: libc::c_int,
    tm_mon: libc::c_int,
    tm_year: libc::c_int,
    tm_wday: libc::c_int,
    tm_yday: libc::c_int,
    tm_isdst: libc::c_int,
    tm_gmtoff: libc::c_long,
    tm_zone: *const libc::c_char,
}

/// The machine's zone's offset from UTC at a unix second, in seconds east
/// (`localtime_r`'s `tm_gmtoff`).
#[must_use]
pub fn local_offset_s(secs: i64) -> i64 {
    local_tm(secs).tm_gmtoff
}

/// Local calendar fields of a unix second: (year, month 1-12, day, hour, minute, second).
#[must_use]
pub fn local_time(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let tm = local_tm(secs);
    let u = |v: libc::c_int| u32::try_from(v).unwrap_or(0);
    (
        tm.tm_year + 1900,
        u(tm.tm_mon + 1),
        u(tm.tm_mday),
        u(tm.tm_hour),
        u(tm.tm_min),
        u(tm.tm_sec),
    )
}

/// `localtime_r` of a unix second.
fn local_tm(secs: i64) -> Tm {
    let t: libc::time_t = secs;
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 1,
        tm_mon: 0,
        tm_year: 70,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // SAFETY: both pointers are valid for the call; `localtime_r` writes `tm` only.
    unsafe { localtime_r(&t, &mut tm) };
    tm
}

// ------------------------------------------------------------------ processes

/// One process as `ps` reads it: `proc_pidinfo`'s BSD record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcRow {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// The controlling terminal's device (`-1`: none).
    pub tdev: i32,
    pub tpgid: i32,
    /// The command name (at most 16 bytes).
    pub comm: String,
    /// When it started, seconds since the epoch.
    pub start: i64,
}

/// The BSD record of `pid`, or `None` when it is gone or not ours to read.
#[must_use]
pub fn proc_row(pid: i32) -> Option<ProcRow> {
    // SAFETY: a zeroed `proc_bsdinfo` is a valid buffer for the kernel to fill.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is valid for `size` bytes of writes.
    let got =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if got != size {
        return None;
    }
    let i = |v: u32| i32::from_ne_bytes(v.to_ne_bytes());
    let comm: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| u8::from_ne_bytes(c.to_ne_bytes()))
        .collect();
    Some(ProcRow {
        pid: i(info.pbi_pid),
        ppid: i(info.pbi_ppid),
        pgid: i(info.pbi_pgid),
        tdev: i(info.e_tdev),
        tpgid: i(info.e_tpgid),
        comm: String::from_utf8_lossy(&comm).into_owned(),
        start: i64::try_from(info.pbi_start_tvsec).unwrap_or(0),
    })
}

/// Every pid on the system.
#[must_use]
pub fn all_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count only.
    let need = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let n = usize::try_from(need).unwrap_or(0) + 256;
    let mut buf = vec![0 as libc::c_int; n];
    let bytes = libc::c_int::try_from(buf.len() * std::mem::size_of::<libc::c_int>())
        .unwrap_or(libc::c_int::MAX);
    // SAFETY: `buf` is valid for `bytes` bytes of writes.
    let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
    buf.truncate(usize::try_from(got).unwrap_or(0).min(n));
    buf.retain(|&p| p > 0);
    buf
}

unsafe extern "C" {
    fn devname(dev: libc::dev_t, kind: libc::mode_t) -> *const libc::c_char;
}

/// The name of the terminal device `dev` (`ttys003`), `??` for none.
#[must_use]
pub fn tty_name(dev: i32) -> String {
    if dev == -1 || dev == 0 {
        return "??".into();
    }
    // SAFETY: `devname` reads its two integer arguments and returns a static buffer or null.
    let p = unsafe { devname(dev, libc::S_IFCHR) };
    if p.is_null() {
        return "??".into();
    }
    // SAFETY: a non-null answer is a NUL-terminated string in `devname`'s own buffer.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

// ------------------------------------------------------------------ signals

/// The write end the handler of [`SignalPipe`] writes each signal's number to.
static SIGNAL_WR: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_WR.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = u8::try_from(sig).unwrap_or(0);
        // SAFETY: `write(2)` is async-signal-safe; errno is saved and restored
        // around it so the interrupted code never sees it change.
        unsafe {
            let saved = *libc::__error();
            let _ = libc::write(fd, (&raw const b).cast(), 1);
            *libc::__error() = saved;
        }
    }
}

/// `sig`'s disposition now: `SIG_DFL`, `SIG_IGN` or a handler's address.
///
/// # Errors
/// `sigaction` refused `sig`.
pub fn disposition(sig: libc::c_int) -> io::Result<libc::sighandler_t> {
    // SAFETY: a null new action only reads the current one into `old`, a zeroed
    // `sigaction` (a valid value) that outlives the call.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(sig, std::ptr::null(), &raw mut old) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(old.sa_sigaction)
    }
}

/// Set `sig` ignored (`ignore`) or at its default, as `nohup` (or its absence)
/// starts a process: a test's probe of [`SignalPipe::install`].
#[cfg(test)]
pub fn start_signal(sig: libc::c_int, ignore: bool) {
    let h = if ignore { libc::SIG_IGN } else { libc::SIG_DFL };
    // SAFETY: SIG_IGN and SIG_DFL are dispositions, no handler of ours.
    unsafe {
        libc::signal(sig, h);
    }
}

/// Signals delivered as bytes on a pipe: the handler writes the signal's
/// number, and the reader takes it in ordinary code (a thread, a `poll`).
/// One per process.
pub struct SignalPipe {
    /// The read end; one byte per signal.
    pub read_fd: RawFd,
}

impl SignalPipe {
    /// Install the handler for `signals` (`SA_RESTART`, an empty mask), except a
    /// signal this process was started ignoring, which stays ignored: under
    /// `nohup` a hang-up never reaches a lab that a closed terminal started, as
    /// every program that respects `SIG_IGN` does (`if (signal(sig, SIG_IGN) !=
    /// SIG_IGN) signal(sig, handler)`; `aterm-release`'s `interrupt::install`).
    ///
    /// # Errors
    /// `pipe` or `sigaction` failed.
    pub fn install(signals: &[libc::c_int]) -> io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a two-int array, as `pipe(2)` requires.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            // SAFETY: both are descriptors `pipe` just returned.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            }
        }
        SIGNAL_WR.store(fds[1], Ordering::Relaxed);
        for &sig in signals {
            if disposition(sig)? == libc::SIG_IGN {
                continue;
            }
            // SAFETY: a zeroed `sigaction` is a valid starting value; the handler
            // is an `extern "C" fn(c_int)` that only calls async-signal-safe code.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&raw mut sa.sa_mask);
                if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(Self { read_fd: fds[0] })
    }

    /// The signals delivered since the last call (none when the pipe is empty).
    #[must_use]
    pub fn drain(&self) -> Vec<i32> {
        let mut buf = [0u8; 64];
        let mut out = Vec::new();
        loop {
            match read(self.read_fd, &mut buf) {
                Ok(0) | Err(_) => return out,
                Ok(n) => out.extend(buf[..n].iter().map(|&b| i32::from(b))),
            }
        }
    }

    /// Block until a signal arrives; every signal delivered by then, in order
    /// (never empty: two that land together are both reported).
    #[must_use]
    pub fn wait(&self) -> Vec<i32> {
        loop {
            let mut fds = [pollfd(self.read_fd)];
            let _ = poll(&mut fds, -1);
            let sigs = self.drain();
            if !sigs.is_empty() {
                return sigs;
            }
        }
    }
}

// ------------------------------------------------------------------ descriptors

/// A `pollfd` asking for input on `fd`.
#[must_use]
pub fn pollfd(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

/// `poll(2)`, retried on `EINTR`; the number of ready descriptors.
///
/// # Errors
/// Any other `poll` failure.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<usize> {
    loop {
        let n = libc::nfds_t::try_from(fds.len()).unwrap_or(0);
        // SAFETY: `fds` is a valid slice of `pollfd` of length `n`.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), n, timeout_ms) };
        if r >= 0 {
            return Ok(usize::try_from(r).unwrap_or(0));
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// One `read(2)`: the bytes read (0 at end of file).
///
/// # Errors
/// The read failed (`EINTR` and `EAGAIN` included: the caller decides).
pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for `buf.len()` bytes of writes.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(n).map_err(|_| io::Error::last_os_error())
}

/// Write all of `buf` to `fd`, waiting (`poll`) when it would block.
///
/// # Errors
/// The write failed for another reason than `EINTR`/`EAGAIN`.
pub fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: `buf` is valid for `buf.len()` bytes of reads.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if let Ok(n) = usize::try_from(n) {
            buf = &buf[n..];
            continue;
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => {
                let mut fds = [libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                }];
                poll(&mut fds, -1)?;
            }
            _ => return Err(e),
        }
    }
    Ok(())
}

/// Take ownership of a descriptor this process was just handed and owns alone
/// (a PTY end `open_pty_pair` returned).
#[must_use]
pub fn own_fd(fd: RawFd) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    // SAFETY: the caller hands over a valid, open descriptor nothing else owns.
    unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }
}

/// Start `cmd`'s child in a session of its own (`setsid` between its fork and
/// its exec), so it has no controlling terminal: the std-`Command` spelling of
/// the spawn seam's `setsid(true)`.
pub fn in_own_session(cmd: &mut aterm_uds::spawnfd::StdCommand) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: the hook runs in the forked child before its exec and makes one
    // async-signal-safe call; `last_os_error` reads `errno` and allocates
    // nothing.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// The monotonic clock (`CLOCK_MONOTONIC`) in nanoseconds: a recorder's
/// stamps. Darwin counts it in microseconds; a take's reader works in
/// milliseconds from its first line, so neither the resolution nor the origin
/// is read.
#[must_use]
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes one `timespec` into the pointer given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
    u64::try_from(ts.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(ts.tv_nsec).unwrap_or(0))
}

/// Close a descriptor this process owns.
pub fn close(fd: RawFd) {
    // SAFETY: the caller owns `fd` and does not use it again.
    unsafe { libc::close(fd) };
}

// ------------------------------------------------------------------ terminals

/// A terminal's size, every field (rows, columns and both pixel counts).
pub type WinSize = libc::winsize;

/// The size of the terminal on `fd`, or `None` when it is no terminal.
#[must_use]
pub fn winsize(fd: RawFd) -> Option<WinSize> {
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes one `winsize` into the pointer given.
    (unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut ws) } == 0).then_some(ws)
}

/// Set the size of the terminal on `fd` (the kernel signals its foreground group).
pub fn set_winsize(fd: RawFd, ws: &WinSize) {
    // SAFETY: `TIOCSWINSZ` reads one `winsize` from the pointer given.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, std::ptr::from_ref(ws)) };
}

/// Raw mode on a terminal, put back as it was when dropped.
pub struct RawMode {
    fd: RawFd,
    saved: libc::termios,
}

impl RawMode {
    /// `cfmakeraw` on `fd`; `None` when `fd` is no terminal.
    #[must_use]
    pub fn enter(fd: RawFd) -> Option<Self> {
        // SAFETY: a zeroed `termios` is a valid buffer for `tcgetattr` to fill.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `saved` is a valid `termios` for the call to write.
        if unsafe { libc::tcgetattr(fd, &raw mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        // SAFETY: `raw` is a valid `termios` copied from `saved`.
        unsafe {
            libc::cfmakeraw(&raw mut raw);
            libc::tcsetattr(fd, libc::TCSANOW, &raw const raw);
        }
        Some(Self { fd, saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: `saved` is the `termios` read from this same descriptor.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.saved) };
    }
}

/// Make the terminal on `fd` this session leader's controlling terminal.
///
/// # Errors
/// The `TIOCSCTTY` refusal.
pub fn take_controlling_tty(fd: RawFd) -> io::Result<()> {
    // SAFETY: `TIOCSCTTY` takes an int argument and touches no memory.
    if unsafe { libc::ioctl(fd, libc::TIOCSCTTY as _, 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `TIOCSTI`: put `byte` into the input queue of the terminal on `fd` as if typed.
const TIOCSTI: libc::c_ulong = 0x8001_7472;

/// Try `TIOCSTI` on `fd` (the isolation probe: the jail must refuse it).
///
/// # Errors
/// The kernel's (or the sandbox's) refusal.
pub fn tiocsti(fd: RawFd, byte: u8) -> io::Result<()> {
    // SAFETY: `TIOCSTI` reads one byte from the pointer given.
    if unsafe { libc::ioctl(fd, TIOCSTI, &raw const byte) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// ------------------------------------------------------------------ the probe's network and Mach calls

/// A UDP `connect()` to an IPv4 address (sends no packet; the sandbox judges
/// it as an outbound connection).
///
/// # Errors
/// The `socket` or `connect` refusal.
pub fn udp_connect(ip: [u8; 4], port: u16) -> io::Result<()> {
    // SAFETY: a plain datagram socket; closed below on every path.
    let s = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if s < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a zeroed `sockaddr_in` is valid; the fields set below fill it.
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::sa_family_t::try_from(libc::AF_INET).unwrap_or(2);
    sa.sin_port = port.to_be();
    sa.sin_addr = libc::in_addr {
        s_addr: u32::from_ne_bytes(ip),
    };
    let len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>()).unwrap_or(16);
    // SAFETY: `sa` is a valid `sockaddr_in` of `len` bytes.
    let r = unsafe { libc::connect(s, (&raw const sa).cast(), len) };
    let out = if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    close(s);
    out
}

unsafe extern "C" {
    static bootstrap_port: libc::c_uint;
    fn bootstrap_look_up(
        bp: libc::c_uint,
        name: *const libc::c_char,
        sp: *mut libc::c_uint,
    ) -> libc::c_int;
}

/// `bootstrap_look_up` of a Mach service: the kernel return (0 found, 1100
/// not privileged, 1102 unknown service).
#[must_use]
pub fn mach_look_up(name: &str) -> i32 {
    let Ok(c) = CString::new(name) else { return -1 };
    let mut port: libc::c_uint = 0;
    // SAFETY: `bootstrap_port` is the task's bootstrap port, set by the runtime
    // before `main`; `c` is a valid C string and `port` a valid out-pointer.
    unsafe { bootstrap_look_up(bootstrap_port, c.as_ptr(), &raw mut port) }
}

/// The symbolic name of an errno (`EPERM`), as the probe reports outcomes.
#[must_use]
pub fn errname(code: i32) -> String {
    // The outcomes the probe meets; anything else is printed by number.
    let name = match code {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ESRCH => "ESRCH",
        libc::EINTR => "EINTR",
        libc::EIO => "EIO",
        libc::ENXIO => "ENXIO",
        libc::EBADF => "EBADF",
        libc::EACCES => "EACCES",
        libc::EEXIST => "EEXIST",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENOTTY => "ENOTTY",
        libc::EROFS => "EROFS",
        libc::EPIPE => "EPIPE",
        libc::EAGAIN => "EAGAIN",
        libc::ENOTSOCK => "ENOTSOCK",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::EADDRNOTAVAIL => "EADDRNOTAVAIL",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ECONNRESET => "ECONNRESET",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ELOOP => "ELOOP",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        _ => return format!("errno{code}"),
    };
    name.to_string()
}

/// The errno name of an I/O error, or its kind when it carries no errno.
#[must_use]
pub fn io_errname(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) if code != 0 => errname(code),
        _ => format!("{:?}", e.kind()),
    }
}

// ------------------------------------------------------------------ the scheduling band (`crate::band`)

/// `PRIO_DARWIN_ROLE` (`<sys/resource.h>`, private): `getpriority`'s darwin-role
/// selector. Not in `libc` (aterm-pty names it the same way).
#[cfg(target_os = "macos")]
const PRIO_DARWIN_ROLE: libc::c_int = 6;

/// A `posix_spawnattr_t` (an opaque pointer on Darwin; the workspace's libc
/// has no spawn API, as aterm-uds's `pspawn` declares its own).
#[cfg(target_os = "macos")]
type SpawnAttr = *mut libc::c_void;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn posix_spawnattr_init(attr: *mut SpawnAttr) -> libc::c_int;
    fn posix_spawnattr_destroy(attr: *mut SpawnAttr) -> libc::c_int;
    fn posix_spawnattr_setflags(attr: *mut SpawnAttr, flags: libc::c_short) -> libc::c_int;
    // <spawn_private.h>: what launchd and RunningBoard spawn an app with.
    fn posix_spawnattr_setprocesstype_np(attr: *mut SpawnAttr, kind: libc::c_int) -> libc::c_int;
    fn posix_spawnattr_set_darwin_role_np(attr: *mut SpawnAttr, role: u64) -> libc::c_int;
    fn posix_spawn(
        pid: *mut libc::pid_t,
        path: *const libc::c_char,
        actions: *const libc::c_void,
        attr: *const SpawnAttr,
        argv: *const *const libc::c_char,
        envp: *const *const libc::c_char,
    ) -> libc::c_int;
}

/// `POSIX_SPAWN_SETEXEC` and `POSIX_SPAWN_PROC_TYPE_APP_DEFAULT`
/// (`<spawn.h>`, `<spawn_private.h>`): an exec in place, as an app's task type.
#[cfg(target_os = "macos")]
const SPAWN_SETEXEC: libc::c_short = 0x40;
#[cfg(target_os = "macos")]
const PROC_TYPE_APP_DEFAULT: libc::c_int = 0x100;

/// Become `program` (`argv`, its first word included; exactly `env`) as an
/// APP-type task at darwin role `role`, through `posix_spawn` with
/// `POSIX_SPAWN_SETEXEC` (this pid, these descriptors 0-2). A launchd job is
/// a DAEMON-type task, in which `setpriority(PRIO_DARWIN_ROLE)` fails with
/// `ENOTSUP`: so neither a role set before an exec nor aterm-pty's UI role for
/// each shell would take (read on macOS 27, 2026-10-20). An aterm.app is
/// APP-type, and its shells inherit that. Not `spawnfd`: it sets neither
/// attribute. Returns only on failure.
#[cfg(target_os = "macos")]
pub fn exec_as_app(
    program: &str,
    argv: &[String],
    env: &[(String, String)],
    role: u64,
) -> io::Error {
    let c = |s: &str| CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let made = (|| -> io::Result<(CString, Vec<CString>, Vec<CString>)> {
        let args = argv.iter().map(|a| c(a)).collect::<io::Result<Vec<_>>>()?;
        let vars = env
            .iter()
            .map(|(k, v)| c(&format!("{k}={v}")))
            .collect::<io::Result<Vec<_>>>()?;
        Ok((c(program)?, args, vars))
    })();
    let (prog, args, vars) = match made {
        Ok(t) => t,
        Err(e) => return e,
    };
    let ptrs = |v: &[CString]| -> Vec<*const libc::c_char> {
        v.iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    };
    let (argp, envp) = (ptrs(&args), ptrs(&vars));
    let mut attr: SpawnAttr = std::ptr::null_mut();
    // SAFETY: `attr` is initialized before use and destroyed after; every
    // pointer passed (path, argv, envp: NULL-terminated arrays of C strings
    // that outlive the call) is valid; on success the call does not return.
    unsafe {
        let rc = posix_spawnattr_init(&raw mut attr);
        if rc != 0 {
            return io::Error::from_raw_os_error(rc);
        }
        let set = [
            posix_spawnattr_setflags(&raw mut attr, SPAWN_SETEXEC),
            posix_spawnattr_setprocesstype_np(&raw mut attr, PROC_TYPE_APP_DEFAULT),
            posix_spawnattr_set_darwin_role_np(&raw mut attr, role),
        ];
        let mut pid: libc::pid_t = 0;
        let e = match set.iter().find(|&&r| r != 0) {
            Some(&r) => r,
            None => posix_spawn(
                &raw mut pid,
                prog.as_ptr(),
                std::ptr::null(),
                &raw const attr,
                argp.as_ptr(),
                envp.as_ptr(),
            ),
        };
        posix_spawnattr_destroy(&raw mut attr);
        io::Error::from_raw_os_error(e)
    }
}

/// `dup2(fd, target)`: put `fd` at `target` (0-2, before an exec).
///
/// # Errors
/// The kernel's refusal.
pub fn dup_onto(fd: RawFd, target: RawFd) -> io::Result<()> {
    // SAFETY: `dup2` on descriptors the caller owns; takes no pointers.
    if unsafe { libc::dup2(fd, target) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The darwin role of `pid` (`getpriority(PRIO_DARWIN_ROLE, pid)`).
///
/// # Errors
/// The kernel's refusal (no such process, another user's).
#[cfg(target_os = "macos")]
pub fn darwin_role(pid: i32) -> io::Result<i32> {
    let who =
        libc::id_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `errno` is this thread's; `getpriority` takes no pointers. Its
    // answer can be -1 legitimately, so errno (cleared first) tells a failure.
    unsafe {
        *libc::__error() = 0;
        let role = libc::getpriority(PRIO_DARWIN_ROLE, who);
        match *libc::__error() {
            0 => Ok(role),
            e => Err(io::Error::from_raw_os_error(e)),
        }
    }
}

/// Set the darwin role of `pid` (`setpriority(PRIO_DARWIN_ROLE, pid, role)`).
///
/// # Errors
/// The kernel's refusal: `ENOTSUP` in a DAEMON-type task (a submitted launchd
/// job that did not exec as an app), `EPERM` for another user's process.
#[cfg(target_os = "macos")]
pub fn set_darwin_role(pid: i32, role: i32) -> io::Result<()> {
    let who =
        libc::id_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `setpriority` takes no pointers.
    if unsafe { libc::setpriority(PRIO_DARWIN_ROLE, who, role) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `pid`'s resource and jetsam coalition ids (`proc_pidinfo` flavour
/// `PROC_PIDCOALITIONINFO`, 20: XNU-private, two `u64` ids then three
/// reserved), or `None` when the kernel will not say.
#[cfg(target_os = "macos")]
#[must_use]
pub fn coalition_ids(pid: i32) -> Option<[u64; 2]> {
    let mut buf = [0u64; 5];
    let size = libc::c_int::try_from(std::mem::size_of_val(&buf)).ok()?;
    // SAFETY: `buf` is a writable buffer of exactly `size` bytes for the call.
    let got = unsafe { libc::proc_pidinfo(pid, 20, 0, buf.as_mut_ptr().cast(), size) };
    (got == size).then_some([buf[0], buf[1]])
}

/// `proc_pidinfo` flavours (`<sys/proc_info.h>`) not in the workspace's libc:
/// a process's thread ids, and one thread's info by its id.
#[cfg(target_os = "macos")]
const PROC_PIDLISTTHREADIDS: libc::c_int = 28;
#[cfg(target_os = "macos")]
const PROC_PIDTHREADID64INFO: libc::c_int = 15;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_threadid_np(thread: libc::pthread_t, id: *mut u64) -> libc::c_int;
}

/// The base priority of `pid`'s thread `tid` (`PROC_PIDTHREADID64INFO`).
#[cfg(target_os = "macos")]
fn thread_priority(pid: i32, tid: u64) -> Option<i32> {
    // SAFETY: an all-zero `proc_threadinfo` is valid (integers and a byte array).
    let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of_val(&info)).ok()?;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes for the call.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDTHREADID64INFO,
            tid,
            (&raw mut info).cast(),
            size,
        )
    };
    (got == size).then_some(info.pth_priority)
}

/// The base priority of `pid`'s main thread (its lowest thread id): 46-47 in
/// aterm.app's band, 20 under launchd's utility clamp, 31 for an app without
/// a role.
#[cfg(target_os = "macos")]
#[must_use]
pub fn main_thread_priority(pid: i32) -> Option<i32> {
    let mut ids = [0u64; 512];
    let size = libc::c_int::try_from(std::mem::size_of_val(&ids)).ok()?;
    // SAFETY: `ids` is a writable buffer of exactly `size` bytes for the call.
    let got =
        unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTTHREADIDS, 0, ids.as_mut_ptr().cast(), size) };
    let n = usize::try_from(got).ok()? / std::mem::size_of::<u64>();
    thread_priority(pid, *ids.get(..n)?.iter().min()?)
}

/// Ask for `QOS_CLASS_USER_INTERACTIVE` on the calling thread and return the
/// base priority the kernel then gives it: 46-47 in an app's band, 20 in a
/// task launchd clamped to utility (a `ProcessType` `Standard` job).
///
/// # Errors
/// The request or the read-back was refused.
#[cfg(target_os = "macos")]
pub fn ui_request_lands() -> io::Result<i32> {
    // SAFETY: sets this thread's own QoS class; takes no pointers.
    let rc = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let mut tid = 0u64;
    // SAFETY: a null thread names the calling one; `tid` is a writable u64.
    let rc = unsafe { pthread_threadid_np(0, &raw mut tid) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let pid = i32::try_from(std::process::id())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    thread_priority(pid, tid).ok_or_else(io::Error::last_os_error)
}

/// `mkfifo(3)` at `path`, mode 0600.
///
/// # Errors
/// The path holds a NUL, or `mkfifo` failed.
pub fn make_fifo(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `c` is a valid NUL-terminated path.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Clear `O_NONBLOCK` on `fd` (a FIFO end opened non-blocking so the open
/// could not wait for a writer).
///
/// # Errors
/// The kernel's refusal.
pub fn set_blocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fcntl` on a descriptor the caller owns; takes no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ------------------------------------------------------------------ the scratch's files, through no link

/// `O_NOFOLLOW_ANY` (macOS): the open fails on a link ANYWHERE on the path, not only its last part.
pub const O_NOFOLLOW_ANY: libc::c_int = 0x2000_0000;

unsafe extern "C" {
    fn fclonefileat(
        src_fd: libc::c_int,
        dst_dirfd: libc::c_int,
        dst: *const libc::c_char,
        flags: u32,
    ) -> libc::c_int;
    fn confstr(name: libc::c_int, buf: *mut libc::c_char, len: libc::size_t) -> libc::size_t;
    fn proc_listpids(
        kind: u32,
        typeinfo: u32,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        kind: libc::c_int,
        ...
    ) -> libc::c_int;
    static SANDBOX_CHECK_NO_REPORT: libc::c_int;
}

fn cpath(p: &std::path::Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt as _;
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn cname(n: &std::ffi::OsStr) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt as _;
    CString::new(n.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `fstat(2)` of a descriptor: (file type bits, owner, link count).
fn fstat_of(fd: RawFd) -> io::Result<(libc::mode_t, u32, u64)> {
    // SAFETY: `stat` is plain data; all-zero is a valid value for `fstat` to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid `stat` for the call to write.
    if unsafe { libc::fstat(fd, &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((st.st_mode & libc::S_IFMT, st.st_uid, u64::from(st.st_nlink)))
}

/// The regular file at `path`, read through no link on its whole path
/// (`O_NOFOLLOW_ANY`: the lab's state dir is a physical path, so a link there
/// is one a jailed process planted), and only a file of this user with one
/// name (a hard link is a second name for a file that lives elsewhere): the
/// one reader of what jailed code may write. `Ok(None)` when nothing is there.
///
/// # Errors
/// A link on the path, or something that is not this user's regular file with one name.
pub fn scratch_file(path: &std::path::Path) -> io::Result<Option<std::fs::File>> {
    let c = cpath(path)?;
    // SAFETY: `c` is a valid NUL-terminated path; the descriptor is owned below.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | O_NOFOLLOW_ANY,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(io::Error::new(
                e.kind(),
                format!("not reading {}: {e} (a link in its path?)", path.display()),
            ))
        };
    }
    let owned = own_fd(fd);
    let (kind, uid, nlink) = fstat_of(fd)?;
    if kind != libc::S_IFREG || uid != getuid() || nlink != 1 {
        return Err(io::Error::other(format!(
            "not reading {}: not the lab user's regular file with one name",
            path.display()
        )));
    }
    Ok(Some(std::fs::File::from(owned)))
}

/// What a directory entry of [`scratch_dir`] is: a subdirectory or a regular
/// file (links and every other kind are never listed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
}

/// The entries of the directory at `path`, opened through no link on its
/// whole path (`O_NOFOLLOW_ANY`) and only when it is this user's directory:
/// its subdirectories and regular files by name, never a link (the
/// directory-listing twin of [`scratch_file`], for trees jailed code writes).
/// `Ok(None)` when nothing is there.
///
/// # Errors
/// A link on the path, or something that is not this user's directory.
pub fn scratch_dir(
    path: &std::path::Path,
) -> io::Result<Option<Vec<(std::ffi::OsString, EntryKind)>>> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = cpath(path)?;
    // SAFETY: `c` is a valid NUL-terminated path; the descriptor is owned by the DIR below.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | O_NOFOLLOW_ANY,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(io::Error::new(
                e.kind(),
                format!("not listing {}: {e} (a link in its path?)", path.display()),
            ))
        };
    }
    let (kind, uid, _) = match fstat_of(fd) {
        Ok(v) => v,
        Err(e) => {
            close(fd);
            return Err(e);
        }
    };
    if kind != libc::S_IFDIR || uid != getuid() {
        close(fd);
        return Err(io::Error::other(format!(
            "not listing {}: not the lab user's directory",
            path.display()
        )));
    }
    // SAFETY: `fd` is an open directory this call owns; `closedir` closes it.
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let e = io::Error::last_os_error();
        close(fd);
        return Err(e);
    }
    let mut out = Vec::new();
    loop {
        // SAFETY: `dir` is a live DIR stream; the entry is read before the next call.
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        // SAFETY: a non-null entry from `readdir` holds a NUL-terminated `d_name`.
        let (name, d_type) = unsafe {
            (
                std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()),
                (*ent).d_type,
            )
        };
        let name = std::ffi::OsStr::from_bytes(name.to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        let kind = match d_type {
            libc::DT_DIR => EntryKind::Dir,
            libc::DT_REG => EntryKind::File,
            _ => continue,
        };
        out.push((name.to_os_string(), kind));
    }
    // SAFETY: `dir` came from `fdopendir` and is closed once.
    unsafe { libc::closedir(dir) };
    Ok(Some(out))
}

/// The contents of [`scratch_file`], through the same checked descriptor.
///
/// # Errors
/// The file was refused, or its read failed.
pub fn scratch_read(path: &std::path::Path) -> io::Result<Option<Vec<u8>>> {
    let Some(mut f) = scratch_file(path)? else {
        return Ok(None);
    };
    let mut buf = Vec::new();
    io::Read::read_to_end(&mut f, &mut buf)?;
    Ok(Some(buf))
}

/// What [`put`] writes.
pub enum PutSrc<'a> {
    /// These bytes.
    Bytes(&'a [u8]),
    /// A clone (APFS copy-on-write, else a copy) of this regular file, read through no link.
    File(&'a std::path::Path),
}

/// A directory opened from `d` by one name, through no link; made (0755) when missing.
fn open_dir_at(d: RawFd, name: &CString) -> io::Result<RawFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `d` is an open directory, `name` a valid C string.
    let fd = unsafe { libc::openat(d, name.as_ptr(), flags) };
    if fd >= 0 {
        return Ok(fd);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() != Some(libc::ENOENT) {
        return Err(e);
    }
    // SAFETY: as above; a race that made it first is fine (EEXIST).
    if unsafe { libc::mkdirat(d, name.as_ptr(), 0o755) } != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EEXIST) {
            return Err(e);
        }
    }
    // SAFETY: as above.
    let fd = unsafe { libc::openat(d, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Write `src` into the absolute `path` by a rename, with `mode`: each
/// directory on the way is opened from `/` through no link (made when
/// missing), and a link standing at `path` is replaced, never followed. A link
/// planted in either path (`src`'s included) is refused.
///
/// # Errors
/// A relative path, a link on the way, or the write or rename failing.
pub fn put(path: &std::path::Path, mode: u32, src: &PutSrc<'_>) -> io::Result<()> {
    put_with_nonce(path, mode, src, || {
        let mut r = [0u8; 4];
        let _ = aterm_uds::rand::fill(&mut r);
        u32::from_ne_bytes(r)
    })
}

/// [`put`] with its stage-name nonce supplied at the same point: deterministic
/// private filesystem controls never depend on a random collision.
fn put_with_nonce(
    path: &std::path::Path,
    mode: u32,
    src: &PutSrc<'_>,
    nonce: impl FnOnce() -> u32,
) -> io::Result<()> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("put: {} is not an absolute path", path.display()),
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let root = c"/";
    // SAFETY: a valid C string; the descriptor is owned below.
    let mut d = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut dir = own_fd(d);
    for comp in path
        .parent()
        .into_iter()
        .flat_map(std::path::Path::components)
    {
        let Component::Normal(c) = comp else { continue };
        let n = open_dir_at(d, &cname(c)?).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "not writing {}: {}: {e} (a link in its path?)",
                    path.display(),
                    c.to_string_lossy()
                ),
            )
        })?;
        dir = own_fd(n);
        d = n;
    }
    let tmp = cname(std::ffi::OsStr::new(&format!(
        ".{}.lab-{}-{:08x}",
        name.to_string_lossy(),
        std::process::id(),
        nonce()
    )))?;
    // Only a successfully created stage can arm cleanup. Keep its descriptor
    // even if writing fails, so its inode cannot be reused before the check
    // through this same pinned parent before an unlink.
    let mut owned_stage = None;
    let create = |d: RawFd,
                  owned: &mut Option<std::os::fd::OwnedFd>|
     -> io::Result<std::os::fd::OwnedFd> {
        // SAFETY: `d` is an open directory, `tmp` a valid C string.
        let fd = unsafe {
            libc::openat(
                d,
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            let fd = own_fd(fd);
            *owned = Some(fd.try_clone()?);
            Ok(fd)
        }
    };
    let written: io::Result<std::os::fd::OwnedFd> = (|| match src {
        PutSrc::Bytes(b) => {
            let fd = create(d, &mut owned_stage)?;
            write_all(std::os::fd::AsRawFd::as_raw_fd(&fd), b)?;
            Ok(fd)
        }
        PutSrc::File(from) => {
            let c = cpath(from)?;
            // SAFETY: a valid C string; the descriptor is owned below.
            let s = unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | O_NOFOLLOW_ANY,
                )
            };
            if s < 0 {
                let e = io::Error::last_os_error();
                return Err(io::Error::new(
                    e.kind(),
                    format!("not reading {}: {e} (a link in its path?)", from.display()),
                ));
            }
            let sfd = own_fd(s);
            if fstat_of(s)?.0 != libc::S_IFREG {
                return Err(io::Error::other(format!(
                    "not reading {}: not a regular file",
                    from.display()
                )));
            }
            // SAFETY: `s` and `d` are open descriptors, `tmp` a valid C string.
            if unsafe { fclonefileat(s, d, tmp.as_ptr(), 0) } == 0 {
                // SAFETY: as above.
                let fd = unsafe {
                    libc::openat(
                        d,
                        tmp.as_ptr(),
                        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                let fd = own_fd(fd);
                if fstat_of(std::os::fd::AsRawFd::as_raw_fd(&fd))?.0 != libc::S_IFREG {
                    return Err(io::Error::other(format!(
                        "not writing {}: its clone was replaced",
                        path.display()
                    )));
                }
                // A clone that cannot be reopened/retained is left in place:
                // cleanup must not guess which entry now stands there.
                owned_stage = Some(fd.try_clone()?);
                return Ok(fd);
            }
            let fd = create(d, &mut owned_stage)?;
            let mut i = std::fs::File::from(sfd);
            let mut o = std::fs::File::from(fd.try_clone()?);
            io::copy(&mut i, &mut o)?;
            Ok(fd)
        }
    })();
    let unlink_tmp = || {
        if owned_stage.as_ref().is_some_and(|held| {
            put_inode(std::os::fd::AsRawFd::as_raw_fd(held))
                .ok()
                .is_some_and(|inode| put_inode_at(d, &tmp) == Some(inode))
        }) {
            // SAFETY: `d` is the pinned parent; `tmp` is one name, inspected
            // without following links. It names the retained stage at the check.
            unsafe { libc::unlinkat(d, tmp.as_ptr(), 0) };
        }
    };
    let fd = match written {
        Ok(fd) => fd,
        Err(e) => {
            unlink_tmp();
            return Err(e);
        }
    };
    let m = libc::mode_t::try_from(mode & 0o7777).unwrap_or(0o600);
    // SAFETY: `fd` is open; `fchmod` touches no memory.
    if unsafe { libc::fchmod(std::os::fd::AsRawFd::as_raw_fd(&fd), m) } != 0 {
        let e = io::Error::last_os_error();
        unlink_tmp();
        return Err(e);
    }
    let dst = cname(name)?;
    // SAFETY: `d` is an open directory; both names are valid C strings.
    if unsafe { libc::renameat(d, tmp.as_ptr(), d, dst.as_ptr()) } != 0 {
        let e = io::Error::last_os_error();
        unlink_tmp();
        return Err(e);
    }
    drop(fd);
    drop(dir);
    Ok(())
}

/// The identity of a retained stage descriptor. A failed identity read
/// preserves the entry rather than removing an uncertain name.
fn put_inode(fd: RawFd) -> io::Result<(libc::dev_t, libc::ino_t)> {
    // SAFETY: `stat` is plain data; `fstat` fills this live value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `st` is writable for the call.
    if unsafe { libc::fstat(fd, &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((st.st_dev, st.st_ino))
}

/// The stage name still stands for its captured inode, through no final
/// link. This refuses an observed replacement; it is not an atomic
/// check-and-unlink against a concurrent writer of this same directory.
fn put_inode_at(dir: RawFd, name: &CString) -> Option<(libc::dev_t, libc::ino_t)> {
    // SAFETY: `stat` is plain data; `fstatat` fills this live value.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the pinned directory and one NUL-terminated name are live;
    // AT_SYMLINK_NOFOLLOW inspects an entry rather than its target.
    (unsafe { libc::fstatat(dir, name.as_ptr(), &raw mut st, libc::AT_SYMLINK_NOFOLLOW) } == 0)
        .then_some((st.st_dev, st.st_ino))
}

/// Remove the entry at the absolute `path` from the scratch: its directory
/// is opened through no link on its whole path (`O_NOFOLLOW_ANY`, as
/// [`scratch_file`] reads), and the entry is unlinked by name, so a link
/// standing there is removed, never followed. `Ok(())` when nothing is there.
///
/// # Errors
/// A link on the directory's path, or the unlink failing.
pub fn scratch_remove(path: &std::path::Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    let c = cpath(parent)?;
    // SAFETY: `c` is a valid NUL-terminated path; the descriptor is owned below.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | O_NOFOLLOW_ANY,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(io::Error::new(
                e.kind(),
                format!("not removing {}: {e} (a link in its path?)", path.display()),
            ))
        };
    }
    let _dir = own_fd(fd);
    let n = cname(name)?;
    // SAFETY: `fd` is an open directory, `n` a valid C string; unlinkat never follows its last part.
    if unsafe { libc::unlinkat(fd, n.as_ptr(), 0) } != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ENOENT) {
            return Err(e);
        }
    }
    Ok(())
}

/// What [`entry_stat`] read of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryStat {
    /// Its device.
    pub dev: u64,
    /// Its inode.
    pub ino: u64,
    /// Its file type bits (`S_IFREG`, `S_IFSOCK`, `S_IFDIR`, `S_IFLNK`, …).
    pub kind: libc::mode_t,
    /// Its owner.
    pub uid: u32,
    /// Its birth time, in seconds since the epoch.
    pub born: i64,
}

/// The directory of the absolute `path`, opened through no link on its whole
/// path (`O_NOFOLLOW_ANY`), and the entry's name: `Ok(None)` when the
/// directory is not there.
fn open_parent(path: &std::path::Path) -> io::Result<Option<(std::os::fd::OwnedFd, CString)>> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    let c = cpath(parent)?;
    // SAFETY: `c` is a valid NUL-terminated path; the descriptor is owned below.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | O_NOFOLLOW_ANY,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(e)
        };
    }
    Ok(Some((own_fd(fd), cname(name)?)))
}

/// `fstatat` of the entry `name` in the open directory `dir`, never following it.
fn stat_at(dir: RawFd, name: &CString) -> io::Result<Option<EntryStat>> {
    // SAFETY: `stat` is plain data; all-zero is a valid value for `fstatat` to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `dir` is an open directory, `name` a valid C string, `st` a live `stat`;
    // AT_SYMLINK_NOFOLLOW reads the entry, never what a link there points at.
    if unsafe { libc::fstatat(dir, name.as_ptr(), &raw mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(e)
        };
    }
    Ok(Some(EntryStat {
        dev: u64::try_from(st.st_dev).unwrap_or(u64::MAX),
        ino: st.st_ino,
        kind: st.st_mode & libc::S_IFMT,
        uid: st.st_uid,
        born: st.st_birthtime,
    }))
}

/// Which of an entry's times [`touch_now`] sets to now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    /// The access time (a file's: what a read sets).
    Accessed,
    /// The modification time (a directory's: what an entry made in it sets).
    Modified,
}

/// Set one of the times of the entry at the absolute `path` to now, the other
/// left as it is, its directory reached through no link on its whole path and
/// the entry itself never followed (a link there gets the time itself). The
/// change time moves too, as it does for any change of an entry's times.
/// `Ok(false)` when nothing is there.
///
/// # Errors
/// A link on the directory's path, or the kernel's refusal.
pub fn touch_now(path: &std::path::Path, which: Touch) -> io::Result<bool> {
    let Some((dir, name)) = open_parent(path)? else {
        return Ok(false);
    };
    let at = |nsec| libc::timespec {
        tv_sec: 0,
        tv_nsec: nsec,
    };
    let times = match which {
        Touch::Accessed => [at(libc::UTIME_NOW), at(libc::UTIME_OMIT)],
        Touch::Modified => [at(libc::UTIME_OMIT), at(libc::UTIME_NOW)],
    };
    // SAFETY: `dir` is an open directory, `name` a valid C string and `times` two live timespecs;
    // AT_SYMLINK_NOFOLLOW sets the entry's own times, never a link's target's.
    let rc = unsafe {
        libc::utimensat(
            std::os::fd::AsRawFd::as_raw_fd(&dir),
            name.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(e)
    }
}

/// What stands at the absolute `path`, its directory reached through no link
/// on its whole path and the entry itself not followed (a link there is read
/// as a link). `Ok(None)` when nothing is there.
///
/// # Errors
/// A link on the directory's path, or the read failing.
pub fn entry_stat(path: &std::path::Path) -> io::Result<Option<EntryStat>> {
    let Some((dir, name)) = open_parent(path)? else {
        return Ok(None);
    };
    stat_at(std::os::fd::AsRawFd::as_raw_fd(&dir), &name)
}

/// Unlink the entry at the absolute `path` only while it is the file `dev`/`ino`
/// it was, its directory reached through no link on its whole path and the
/// entry unlinked by name (never followed): `Ok(true)` when it was removed,
/// `Ok(false)` when nothing or another file stands there.
///
/// # Errors
/// A link on the directory's path, or the unlink failing.
pub fn remove_if_same(path: &std::path::Path, dev: u64, ino: u64) -> io::Result<bool> {
    let Some((dir, name)) = open_parent(path)? else {
        return Ok(false);
    };
    let fd = std::os::fd::AsRawFd::as_raw_fd(&dir);
    match stat_at(fd, &name)? {
        Some(st) if st.dev == dev && st.ino == ino => {}
        _ => return Ok(false),
    }
    // SAFETY: `fd` is an open directory, `name` a valid C string; unlinkat never follows its last part.
    if unsafe { libc::unlinkat(fd, name.as_ptr(), 0) } != 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(e)
        };
    }
    Ok(true)
}

/// The user's temp dir (`getconf DARWIN_USER_TEMP_DIR`), as the system names it.
#[must_use]
pub fn user_temp_dir() -> Option<String> {
    const CS_DARWIN_USER_TEMP_DIR: libc::c_int = 65537;
    let mut buf = vec![0 as libc::c_char; 1024];
    // SAFETY: `buf` is valid for `buf.len()` bytes; `confstr` writes at most that many.
    let n = unsafe { confstr(CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    // SAFETY: on success `confstr` wrote a NUL-terminated string into `buf`.
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(s.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// `pid`'s physical footprint in bytes (`proc_pid_rusage`'s
/// `ri_phys_footprint`, the "Memory" Activity Monitor shows): its own dirty
/// and compressed pages, never the shared clean text of its binary that its
/// resident set counts. `None` when the process is gone or not this user's.
#[must_use]
pub fn phys_footprint(pid: i32) -> Option<u64> {
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a zeroed `rusage_info_v4`, the struct `RUSAGE_INFO_V4`
    // fills; the kernel writes at most its size, and it is read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    // SAFETY: zeroed is a valid `rusage_info_v4` (plain integers), and on
    // success the kernel filled it.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

/// A shared read-only mapping of a whole regular file, unmapped when it drops;
/// `None` for an empty file. Mapping takes no page fault, so it reads nothing.
struct WholeMap {
    addr: *mut libc::c_void,
    len: usize,
}

impl WholeMap {
    fn of(path: &std::path::Path) -> io::Result<Option<WholeMap>> {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file too large to map"))?;
        if len == 0 {
            return Ok(None);
        }
        // SAFETY: a fresh read-only shared mapping of an open file; the
        // descriptor may close once it exists. Unmapped in `drop`.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(WholeMap { addr, len }))
    }
}

impl Drop for WholeMap {
    fn drop(&mut self) {
        // SAFETY: the mapping `of` made, of `len` bytes, unmapped once.
        unsafe { libc::munmap(self.addr, self.len) };
    }
}

/// How many of `path`'s pages are in the page cache, of how many: `mincore`
/// over a fresh mapping. A directory counts every regular file under it
/// (symbolic links are not followed). What `instance --pages cold` checks
/// before and after its launch (r6 sweep #19).
///
/// # Errors
/// A file could not be read, mapped or asked about.
pub fn resident_pages(path: &std::path::Path) -> io::Result<(u64, u64)> {
    unsafe extern "C" {
        /// `<sys/mman.h>`: `int mincore(const void *, size_t, char *)`; the
        /// first-party `libc` declares none.
        fn mincore(addr: *const libc::c_void, len: usize, vec: *mut libc::c_char) -> libc::c_int;
    }
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut total = (0, 0);
        for entry in std::fs::read_dir(path)? {
            let (resident, pages) = resident_pages(&entry?.path())?;
            total = (total.0 + resident, total.1 + pages);
        }
        return Ok(total);
    }
    if !meta.is_file() {
        return Ok((0, 0));
    }
    let Some(map) = WholeMap::of(path)? else {
        return Ok((0, 0));
    };
    // SAFETY: `sysconf` reads a constant.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .map_err(|_| io::Error::other("no page size"))?;
    let mut vec = vec![0 as libc::c_char; map.len.div_ceil(page)];
    // SAFETY: `vec` holds one byte per page of the live `map.len`-byte mapping.
    if unsafe { mincore(map.addr, map.len, vec.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // `MINCORE_INCORE` is bit 0.
    let resident = vec.iter().filter(|&&b| b & 1 != 0).count();
    Ok((resident as u64, vec.len() as u64))
}

/// Drop `path`'s clean pages from the page cache (every regular file under it,
/// for a directory): `msync(MS_INVALIDATE)` over a fresh mapping. Nothing is
/// written or renamed, either of which would void the verdict macOS keeps
/// from an executable's first exec, so a launch after this pays only its page
/// faults (r6 sweep #19: `instance --pages cold`).
///
/// # Errors
/// A file could not be read, mapped or invalidated.
pub fn evict_pages(path: &std::path::Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            evict_pages(&entry?.path())?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Ok(());
    }
    let Some(map) = WholeMap::of(path)? else {
        return Ok(());
    };
    // SAFETY: the live mapping, its whole length.
    if unsafe { libc::msync(map.addr, map.len, libc::MS_INVALIDATE) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read the byte of a named shared-memory canary created by the probe's parent.
/// The parent keeps it sized and unchanged until the child has exited. This
/// opens it read-only by name, then maps it read-only: an inherited descriptor
/// cannot bypass the policy being tested.
///
/// # Errors
/// Open/map/read refusal, an empty object, or a byte other than the canary.
#[cfg(test)]
pub fn read_named_shm(name: &str) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let name = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mode: libc::c_uint = 0o600;
    // SAFETY: a valid name and the correctly promoted variadic mode argument.
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = std::fs::File::from(own_fd(fd));
    if file.metadata()?.len() == 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    // SAFETY: a fresh read-only mapping of one byte of the retained, sized
    // object. No writable mapping or resize is active in the parent.
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            1,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful one-byte mapping is still live; volatile loads
    // read this externally backed memory without creating a Rust reference.
    let byte = unsafe { addr.cast::<u8>().read_volatile() };
    // SAFETY: exactly the one mapping made above, unmapped once.
    if unsafe { libc::munmap(addr, 1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if byte != 0x5a {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(())
}

/// An exclusively created named canary. Only the test parent creates/writes
/// these; children use [`read_named_shm`]. A name stays until confirmed child
/// EOF; the parent retains this whole owner if the kernel has not confirmed it.
#[cfg(test)]
pub struct NamedShm {
    name: CString,
    fd: std::os::fd::OwnedFd,
}

#[cfg(test)]
impl NamedShm {
    /// Create one random, mode-0600 canary under `prefix` without replacing a
    /// collision. The writable mapping is gone before this function returns.
    ///
    /// # Errors
    /// No entropy, a name collision, or an OS create/size/map refusal.
    pub fn create(prefix: &str) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let mut entropy = [0u8; 8];
        aterm_uds::rand::fill(&mut entropy)?;
        let name = CString::new(format!(
            "{prefix}{:012x}",
            u64::from_ne_bytes(entropy) >> 16
        ))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: the valid name and unsigned-int mode match shm_open's
        // variadic ABI. O_EXCL establishes custody before any cleanup exists.
        let mode: libc::c_uint = 0o600;
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                mode,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let owner = Self {
            name,
            fd: own_fd(fd),
        };
        // SAFETY: our exclusively owned descriptor; no writable alias is
        // handed to the child, by either exec inheritance or spawnfd custody.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let meta = std::fs::File::from(owner.fd.try_clone()?).metadata()?;
        if meta.mode() & 0o777 != 0o600 || meta.uid() != getuid() {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        // SAFETY: size this exclusively owned writable object once.
        if unsafe { libc::ftruncate(fd, 1) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh one-byte writable mapping of the sized object.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                1,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                owner.fd.as_raw_fd(),
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: our fresh writable mapping, touched only before any reader.
        unsafe { addr.cast::<u8>().write_volatile(0x5a) };
        // SAFETY: exactly the one mapping above, never used again.
        if unsafe { libc::munmap(addr, 1) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(owner)
    }

    /// The exact spelling used to create it, including an optional slash.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
            .to_str()
            .expect("a prefix and hexadecimal entropy")
    }
}

#[cfg(test)]
impl Drop for NamedShm {
    fn drop(&mut self) {
        // SAFETY: only a successfully O_EXCL-created name reaches here.
        unsafe { libc::shm_unlink(self.name.as_ptr()) };
    }
}

/// Ask the kernel about this process's preferences policy, without contacting
/// a service or loading a preference. `scope` is global/local Mach or the
/// domain-independent user-preference-read operation.
///
/// # Errors
/// An invalid query or no policy answer.
pub fn preference_policy(scope: &str, name: &str) -> io::Result<bool> {
    let (op, kind) = match scope {
        "global" => (c"mach-lookup", 2),
        "local" => (c"mach-lookup", 3),
        "read" => (c"user-preference-read", 0),
        _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
    };
    let name = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: the constant and C strings are valid. This is only a policy
    // query for our own process, with no service lookup or preference access.
    let rc = unsafe {
        sandbox_check(
            std::process::id().cast_signed(),
            op.as_ptr(),
            kind | SANDBOX_CHECK_NO_REPORT,
            name.as_ptr(),
        )
    };
    match rc {
        0 => Ok(true),
        n if n > 0 => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

/// The pids of every live process of this user in the jail that carries
/// `mark`, sorted. The lab's profile denies `(mach-lookup (global-name
/// MARK))`, a name nobody serves, random per lab and named by no other
/// profile; the kernel answers `sandbox_check` about any process of the same
/// user, and no process can leave its sandbox, so a process is found by its
/// jail, not by its parentage (an orphan launchd adopted, one in a session of
/// its own, one that cleared its environment). A process is in THIS jail when
/// `mark` is denied to it and `mark.none`, a name no profile names, is not (a
/// sandbox that denies every lookup is not the lab's). Asked with
/// `SANDBOX_CHECK_NO_REPORT`: a query, never a logged refusal.
///
/// # Errors
/// The kernel gives no answer here (this process runs in a sandbox).
pub fn jail_pids(mark: &str) -> Result<Vec<i32>, String> {
    const FILTER_NONE: libc::c_int = 0;
    const FILTER_GLOBAL_NAME: libc::c_int = 2;
    const PROC_UID_ONLY: u32 = 4;
    let m = CString::new(mark).map_err(|_| "jailpids: a NUL in the mark".to_string())?;
    let none = CString::new(format!("{mark}.none")).map_err(|_| "jailpids: a NUL in the mark")?;
    let op = c"mach-lookup";
    // SAFETY: a data symbol of libSystem, read once.
    let no_report = unsafe { SANDBOX_CHECK_NO_REPORT };
    // SAFETY: `sandbox_check` reads only its C-string arguments, all valid here.
    let sandboxed = |pid: i32| unsafe { sandbox_check(pid, std::ptr::null(), FILTER_NONE) };
    // SAFETY: as above; the variadic argument is the name, a valid C string.
    let denied = |pid: i32, name: &CString| unsafe {
        sandbox_check(
            pid,
            op.as_ptr(),
            FILTER_GLOBAL_NAME | no_report,
            name.as_ptr(),
        )
    };
    // the control of the query itself: this process is in no sandbox, so the mark is not denied to it
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    if sandboxed(me) != 0 || denied(me, &m) != 0 {
        return Err(
            "jailpids: sandbox_check gives no answer here (run outside any sandbox)".into(),
        );
    }
    // SAFETY: a null buffer asks for the size only.
    let need = unsafe { proc_listpids(PROC_UID_ONLY, getuid(), std::ptr::null_mut(), 0) };
    let n = usize::try_from(need).unwrap_or(0) / std::mem::size_of::<libc::c_int>() + 256;
    let mut buf = vec![0 as libc::c_int; n];
    let bytes = libc::c_int::try_from(buf.len() * std::mem::size_of::<libc::c_int>())
        .unwrap_or(libc::c_int::MAX);
    // SAFETY: `buf` is valid for `bytes` bytes of writes.
    let got = unsafe { proc_listpids(PROC_UID_ONLY, getuid(), buf.as_mut_ptr().cast(), bytes) };
    let got = usize::try_from(got).unwrap_or(0) / std::mem::size_of::<libc::c_int>();
    let mut out: Vec<i32> = buf[..got.min(buf.len())]
        .iter()
        .copied()
        .filter(|&pid| {
            pid > 1
                && pid != me
                && sandboxed(pid) == 1
                && denied(pid, &m) == 1
                && denied(pid, &none) == 0
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

// ------------------------------------------------------------ input sources

/// HIToolbox's keyboard input sources (`TextInputSources.h`) and the Unicode key
/// layout one carries (`UCKeyTranslate`): what `meta-keys` selects for its run,
/// puts back after it, and derives each layout's characters from. HIToolbox wants
/// its main thread, which is where the lab's verbs run; nothing here is called
/// from another.
pub mod tis {
    use std::ffi::c_void;

    use crate::input_sources::Pref;

    type CfRef = *const c_void;

    #[repr(C)]
    struct CfRange {
        location: isize,
        length: isize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CfRef);
        fn CFRetain(cf: CfRef) -> CfRef;
        fn CFGetTypeID(cf: CfRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFArrayGetCount(array: CfRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
        fn CFStringGetLength(string: CfRef) -> isize;
        fn CFStringGetCharacters(string: CfRef, range: CfRange, buffer: *mut u16);
        fn CFDataGetBytePtr(data: CfRef) -> *const u8;
        fn CFDataGetLength(data: CfRef) -> isize;
        fn CFDataCreate(alloc: CfRef, bytes: *const u8, len: isize) -> CfRef;
        fn CFStringCreateWithBytes(
            alloc: CfRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CfRef;
        fn CFPropertyListCreateData(
            alloc: CfRef,
            plist: CfRef,
            format: isize,
            options: usize,
            error: *mut CfRef,
        ) -> CfRef;
        fn CFPropertyListCreateWithData(
            alloc: CfRef,
            data: CfRef,
            options: usize,
            format: *mut isize,
            error: *mut CfRef,
        ) -> CfRef;
        fn CFPreferencesCopyAppValue(key: CfRef, app: CfRef) -> CfRef;
        fn CFPreferencesSetAppValue(key: CfRef, value: CfRef, app: CfRef);
        fn CFPreferencesAppSynchronize(app: CfRef) -> u8;
    }

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;
    /// `kCFPropertyListBinaryFormat_v1_0`.
    const BINARY_PLIST: isize = 200;
    /// The preferences domain HIToolbox keeps the input sources in.
    const HITOOLBOX: &str = "com.apple.HIToolbox";
    /// What HIToolbox writes there of its own when a source is selected, beside the
    /// enabled list, and no TIS call puts back: the history of selected sources (the
    /// one that would still name a layout the run selected) and the selection's own
    /// record. Read as this process's HIToolbox has them, which serves the selection's
    /// values even where the domain holds no key yet; written back, they land in the
    /// domain. `AppleInputSourceUpdateTime` is left out: HIToolbox stamps it on its own
    /// clock, about two seconds after a change, so nothing written before then holds.
    pub const BOOKKEEPING: [&str; 3] = [
        "AppleInputSourceHistory",
        "AppleSelectedInputSources",
        "AppleCurrentKeyboardLayoutInputSourceID",
    ];

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: CfRef;
        static kTISPropertyUnicodeKeyLayoutData: CfRef;
        fn TISCreateInputSourceList(properties: CfRef, include_all_installed: u8) -> CfRef;
        fn TISCopyCurrentKeyboardInputSource() -> CfRef;
        fn TISGetInputSourceProperty(source: CfRef, key: CfRef) -> CfRef;
        fn TISEnableInputSource(source: CfRef) -> i32;
        fn TISDisableInputSource(source: CfRef) -> i32;
        fn TISSelectInputSource(source: CfRef) -> i32;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const u8,
            key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: usize,
            actual_string_length: *mut usize,
            unicode_string: *mut u16,
        ) -> i32;
    }

    /// `kUCKeyActionDown`: the key pressed, dead-key state carried.
    const KEY_ACTION_DOWN: u16 = 0;

    /// One reference to a CoreFoundation object, released when it drops.
    struct Owned(CfRef);

    impl Owned {
        /// `cf` from a Create or Copy call (or retained); `None` for null.
        fn new(cf: CfRef) -> Option<Owned> {
            (!cf.is_null()).then(|| Owned(cf))
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` is non-null and this value owns one reference to it.
            unsafe { CFRelease(self.0) }
        }
    }

    /// The text of `s`, `None` unless it is a `CFString`.
    fn string(s: CfRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        // SAFETY: `CFGetTypeID` reads any live CF object; a non-string is refused
        // before it is read as one, and the buffer holds the string's whole length.
        unsafe {
            if CFGetTypeID(s) != CFStringGetTypeID() {
                return None;
            }
            let n = CFStringGetLength(s);
            let mut buf = vec![0u16; usize::try_from(n).ok()?];
            CFStringGetCharacters(
                s,
                CfRange {
                    location: 0,
                    length: n,
                },
                buf.as_mut_ptr(),
            );
            Some(String::from_utf16_lossy(&buf))
        }
    }

    /// A new `CFString` of `s`.
    fn cf_string(s: &str) -> Option<Owned> {
        // SAFETY: the bytes and their length are `s`'s; the result follows the
        // Create rule.
        Owned::new(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                s.as_ptr(),
                isize::try_from(s.len()).ok()?,
                UTF8,
                0,
            )
        })
    }

    /// HIToolbox's own bookkeeping ([`BOOKKEEPING`]) as it stands: each key with
    /// its value as a binary property list, `None` where the key is absent.
    ///
    /// # Errors
    /// A value cannot be read or written as a property list.
    pub fn bookkeeping() -> Result<Vec<Pref>, String> {
        let app = cf_string(HITOOLBOX).ok_or("cannot name com.apple.HIToolbox")?;
        // SAFETY: `app` is a live CFString.
        unsafe { CFPreferencesAppSynchronize(app.0) };
        BOOKKEEPING
            .iter()
            .map(|key| {
                let k = cf_string(key).ok_or_else(|| format!("cannot name {key}"))?;
                // SAFETY: two live CFStrings; the value follows the Copy rule.
                let Some(value) = Owned::new(unsafe { CFPreferencesCopyAppValue(k.0, app.0) })
                else {
                    return Ok(((*key).to_string(), None));
                };
                // SAFETY: `value` is a live property list; the data follows the Create
                // rule, and its bytes are copied while it lives.
                let data = Owned::new(unsafe {
                    CFPropertyListCreateData(
                        std::ptr::null(),
                        value.0,
                        BINARY_PLIST,
                        0,
                        std::ptr::null_mut(),
                    )
                })
                .ok_or_else(|| format!("{key}: not a property list"))?;
                // SAFETY: as above.
                let bytes = unsafe {
                    let len = usize::try_from(CFDataGetLength(data.0)).unwrap_or(0);
                    std::slice::from_raw_parts(CFDataGetBytePtr(data.0), len).to_vec()
                };
                Ok(((*key).to_string(), Some(bytes)))
            })
            .collect()
    }

    /// Write `prefs` (what [`bookkeeping`] read) back into HIToolbox's domain, a
    /// `None` by removing its key, and synchronize it.
    ///
    /// # Errors
    /// A value is not a property list, or the domain did not synchronize.
    pub fn put_bookkeeping(prefs: &[Pref]) -> Result<(), String> {
        let app = cf_string(HITOOLBOX).ok_or("cannot name com.apple.HIToolbox")?;
        for (key, bytes) in prefs {
            let k = cf_string(key).ok_or_else(|| format!("cannot name {key}"))?;
            let value = match bytes {
                None => None,
                Some(b) => {
                    // SAFETY: the bytes and their length are `b`'s; both results follow
                    // the Create rule.
                    let len = isize::try_from(b.len()).map_err(|_| format!("{key}: too long"))?;
                    let data =
                        Owned::new(unsafe { CFDataCreate(std::ptr::null(), b.as_ptr(), len) })
                            .ok_or_else(|| format!("{key}: cannot hold its value"))?;
                    let value = Owned::new(unsafe {
                        CFPropertyListCreateWithData(
                            std::ptr::null(),
                            data.0,
                            0,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    })
                    .ok_or_else(|| format!("{key}: the recorded value is not a property list"))?;
                    Some(value)
                }
            };
            // SAFETY: live CFStrings and a live property list, or null to remove the key.
            unsafe {
                CFPreferencesSetAppValue(
                    k.0,
                    value.as_ref().map_or(std::ptr::null(), |v| v.0),
                    app.0,
                );
            }
        }
        // SAFETY: `app` is a live CFString.
        if unsafe { CFPreferencesAppSynchronize(app.0) } == 0 {
            return Err("com.apple.HIToolbox did not synchronize".into());
        }
        Ok(())
    }

    /// The input source's ID (`com.apple.keylayout.US`).
    fn id_of(source: CfRef) -> Option<String> {
        // SAFETY: `source` is a live input source; the property is a Get-rule
        // CFString that `string` checks before reading.
        string(unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) })
    }

    /// HIToolbox's list of input sources: the enabled ones, or with `all` every
    /// one installed.
    fn list(all: bool) -> Option<Owned> {
        // SAFETY: a null property dictionary asks for every source; the list
        // follows the Create rule, so the owner releases it.
        Owned::new(unsafe { TISCreateInputSourceList(std::ptr::null(), u8::from(all)) })
    }

    /// Each source of `list` with its ID, until `f` answers `Some`.
    fn find<T>(list: &Owned, mut f: impl FnMut(CfRef, String) -> Option<T>) -> Option<T> {
        // SAFETY: `list` is a live CFArray of input sources, read in bounds; each
        // element is a Get-rule source the list keeps alive while `f` runs.
        let n = unsafe { CFArrayGetCount(list.0) };
        (0..n).find_map(|i| {
            // SAFETY: as above.
            let source = unsafe { CFArrayGetValueAtIndex(list.0, i) };
            id_of(source).and_then(|id| f(source, id))
        })
    }

    /// The IDs of the enabled input sources, in HIToolbox's order.
    #[must_use]
    pub fn enabled() -> Vec<String> {
        let mut out = Vec::new();
        if let Some(l) = list(false) {
            find(&l, |_, id| {
                out.push(id);
                None::<()>
            });
        }
        out
    }

    /// The ID of the selected keyboard input source.
    #[must_use]
    pub fn selected() -> Option<String> {
        // SAFETY: a Copy-rule source, released when the owner drops.
        let source = Owned::new(unsafe { TISCopyCurrentKeyboardInputSource() })?;
        id_of(source.0)
    }

    /// `call` (one of the TIS setters) on the installed input source `id`.
    fn on(id: &str, what: &str, call: unsafe extern "C" fn(CfRef) -> i32) -> Result<(), String> {
        let l = list(true).ok_or_else(|| format!("{what} {id}: HIToolbox lists no sources"))?;
        // SAFETY: `source` is a live input source of `l`, the argument every
        // setter takes.
        let status = find(&l, |source, sid| {
            (sid == id).then(|| unsafe { call(source) })
        });
        match status {
            Some(0) => Ok(()),
            Some(s) => Err(format!("{what} {id}: OSStatus {s}")),
            None => Err(format!("{what} {id}: no such input source is installed")),
        }
    }

    /// Enable the installed input source `id` (`TISEnableInputSource`).
    ///
    /// # Errors
    /// It is not installed, or HIToolbox refused.
    pub fn enable(id: &str) -> Result<(), String> {
        on(id, "TISEnableInputSource", TISEnableInputSource)
    }

    /// Disable the input source `id` (`TISDisableInputSource`).
    ///
    /// # Errors
    /// It is not installed, or HIToolbox refused.
    pub fn disable(id: &str) -> Result<(), String> {
        on(id, "TISDisableInputSource", TISDisableInputSource)
    }

    /// Select the enabled input source `id` (`TISSelectInputSource`).
    ///
    /// # Errors
    /// It is not installed, or HIToolbox refused.
    pub fn select(id: &str) -> Result<(), String> {
        on(id, "TISSelectInputSource", TISSelectInputSource)
    }

    /// The Unicode key layout of an input source, read for this Mac's keyboard type.
    pub struct Layout {
        /// The `kTISPropertyUnicodeKeyLayoutData`, retained.
        data: Owned,
        kbd: u32,
    }

    impl Layout {
        /// The layout of the installed input source `id`, enabled or not.
        ///
        /// # Errors
        /// It is not installed, or carries no Unicode key layout (an input method).
        pub fn of(id: &str) -> Result<Layout, String> {
            let l = list(true).ok_or_else(|| format!("{id}: HIToolbox lists no sources"))?;
            let data = find(&l, |source, sid| {
                if sid != id {
                    return None;
                }
                // SAFETY: `source` is live; the property is a Get-rule CFData (checked
                // by type before it is kept), retained here so it outlives the list.
                unsafe {
                    let d = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
                    (!d.is_null() && CFGetTypeID(d) == CFDataGetTypeID())
                        .then(|| Owned::new(CFRetain(d)))
                        .flatten()
                }
            })
            .ok_or_else(|| format!("{id}: not installed, or it has no Unicode key layout"))?;
            // SAFETY: a plain read of HIToolbox's keyboard type, on the main thread.
            let kbd = u32::from(unsafe { LMGetKbdType() });
            Ok(Layout { data, kbd })
        }

        /// The keyboard type (`LMGetKbdType`) the layout is read for.
        #[must_use]
        pub fn keyboard_type(&self) -> u32 {
            self.kbd
        }

        /// What `key_code` types pressed with `modifiers` (UCKeyTranslate's modifier
        /// byte: Carbon's `EventModifiers` shifted right by 8, so 0x02 Shift and 0x08
        /// Option) after the dead-key state `dead`, which it updates: a dead key
        /// types nothing and leaves `dead` non-zero. `None` when UCKeyTranslate
        /// refuses.
        #[must_use]
        pub fn press(&self, key_code: u16, modifiers: u32, dead: &mut u32) -> Option<String> {
            let mut buf = [0u16; 8];
            let mut len = 0usize;
            // SAFETY: the layout bytes live while `self.data` holds the CFData; the
            // output buffer and its length, the state and the count are valid
            // for the call.
            let status = unsafe {
                UCKeyTranslate(
                    CFDataGetBytePtr(self.data.0),
                    key_code,
                    KEY_ACTION_DOWN,
                    modifiers,
                    self.kbd,
                    0,
                    dead,
                    buf.len(),
                    &raw mut len,
                    buf.as_mut_ptr(),
                )
            };
            (status == 0).then(|| String::from_utf16_lossy(&buf[..len.min(buf.len())]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A physical scratch dir, removed when it drops.
    struct Dir(std::path::PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir(name: &str) -> Dir {
        let d = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-sys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    /// Errors before a stage is created never remove another entry at its
    /// name. The nonce is supplied to the real put path; every path belongs to
    /// this exclusively created, physical scratch directory.
    #[test]
    fn put_preserves_occupied_stages_and_source_refusals() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
        let scratch = aterm_tempfile::Builder::new()
            .prefix("lab-put-owned-")
            .tempdir()
            .expect("exclusive scratch");
        let root = scratch.path().canonicalize().expect("physical scratch");
        let stage = |dst: &Path, nonce: u32| {
            dst.with_file_name(format!(
                ".{}.lab-{}-{nonce:08x}",
                dst.file_name().expect("leaf").to_string_lossy(),
                std::process::id()
            ))
        };
        let stamp = |path: &Path| {
            std::fs::symlink_metadata(path).ok().map(|m| {
                let bytes = if m.file_type().is_symlink() {
                    std::fs::read_link(path)
                        .expect("link")
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec()
                } else {
                    std::fs::read(path).expect("standing regular file")
                };
                (m.dev(), m.ino(), m.mode(), bytes)
            })
        };
        let destination = root.join("ordinary");
        put_with_nonce(&destination, 0o640, &PutSrc::Bytes(b"ordinary"), || 1)
            .expect("ordinary write");
        assert_eq!(std::fs::read(&destination).unwrap(), b"ordinary");
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(std::fs::symlink_metadata(stage(&destination, 1)).is_err());
        let source = root.join("source");
        std::fs::write(&source, b"source bytes").unwrap();
        let copy = root.join("copy");
        put_with_nonce(&copy, 0o600, &PutSrc::File(&source), || 2).expect("file clone or copy");
        assert_eq!(std::fs::read(&copy).unwrap(), b"source bytes");
        let replace = root.join("replace-link");
        symlink(&source, &replace).unwrap();
        put_with_nonce(&replace, 0o600, &PutSrc::Bytes(b"replacement"), || 3).unwrap();
        assert_eq!(std::fs::read(&replace).unwrap(), b"replacement");
        assert!(
            !std::fs::symlink_metadata(&replace)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
        // A stage this call did create is removed after a failed publish.
        let directory = root.join("directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(put_with_nonce(&directory, 0o600, &PutSrc::Bytes(b"no"), || 4).is_err());
        assert!(directory.is_dir());
        assert!(std::fs::symlink_metadata(stage(&directory, 4)).is_err());
        // Preserve the whole-path no-follow boundary too.
        let parent_link = root.join("via");
        symlink(&directory, &parent_link).unwrap();
        assert!(
            put_with_nonce(
                &parent_link.join("child"),
                0o600,
                &PutSrc::Bytes(b"no"),
                || 5
            )
            .is_err()
        );
        assert!(std::fs::read_dir(&directory).unwrap().next().is_none());

        let missing = root.join("missing-source");
        let source_link = root.join("source-link");
        symlink(&source, &source_link).unwrap();
        let cases = [
            ("bytes/regular", PutSrc::Bytes(&b"new"[..]), false),
            ("bytes/link", PutSrc::Bytes(&b"new"[..]), true),
            ("file/regular", PutSrc::File(source.as_path()), false),
            ("file/link", PutSrc::File(source.as_path()), true),
            ("missing source", PutSrc::File(missing.as_path()), false),
            ("linked source", PutSrc::File(source_link.as_path()), false),
            ("directory source", PutSrc::File(directory.as_path()), false),
        ];
        let mut observed = Vec::new();
        for (i, (what, src, link)) in cases.into_iter().enumerate() {
            let dst = root.join(format!("destination-{i}"));
            std::fs::write(&dst, b"standing destination").unwrap();
            let nonce = 100 + u32::try_from(i).unwrap();
            let tmp = stage(&dst, nonce);
            let other = root.join(format!("other-{i}"));
            std::fs::write(&other, b"standing other").unwrap();
            if link {
                symlink(&other, &tmp).unwrap();
            } else {
                std::fs::write(&tmp, b"standing stage").unwrap();
            }
            let before = stamp(&tmp).expect("occupied stage");
            let refused = put_with_nonce(&dst, 0o600, &src, || nonce).is_err();
            observed.push((
                what,
                refused,
                stamp(&tmp).as_ref() == Some(&before),
                std::fs::read(&dst).unwrap() == b"standing destination",
                std::fs::read(&other).unwrap() == b"standing other",
            ));
        }
        // Run every refusal before the aggregate: EEXIST and failures before
        // creation must all preserve the standing inode/link and bytes.
        assert!(
            observed
                .iter()
                .all(|(_, refused, stage, dst, other)| *refused && *stage && *dst && *other),
            "put never cleans up a stage it did not create: {observed:?}"
        );
    }

    #[test]
    fn put_writes_by_a_rename_and_never_through_a_link() {
        let d = dir("put");
        let p = d.0.join("a/b/file");
        put(&p, 0o640, &PutSrc::Bytes(b"one")).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // a link standing at the name is replaced, never followed
        let outside = d.0.join("outside");
        std::fs::write(&outside, "keep").unwrap();
        let at = d.0.join("a/link");
        std::os::unix::fs::symlink(&outside, &at).unwrap();
        put(&at, 0o600, &PutSrc::Bytes(b"two")).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(
            !std::fs::symlink_metadata(&at)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // a link on the way is refused, and nothing is written where it points
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, d.0.join("via")).unwrap();
        assert!(put(&d.0.join("via/x"), 0o600, &PutSrc::Bytes(b"no")).is_err());
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
        // a clone of a regular file; a link as the source is refused
        put(&d.0.join("copy"), 0o644, &PutSrc::File(&p)).unwrap();
        assert_eq!(std::fs::read(d.0.join("copy")).unwrap(), b"one");
        assert!(
            put(
                &d.0.join("copy2"),
                0o644,
                &PutSrc::File(&at.with_file_name("no-such"))
            )
            .is_err()
        );
        std::os::unix::fs::symlink(&p, d.0.join("srclink")).unwrap();
        assert!(
            put(
                &d.0.join("copy3"),
                0o644,
                &PutSrc::File(&d.0.join("srclink"))
            )
            .is_err()
        );
        assert!(put(Path::new("relative/x"), 0o600, &PutSrc::Bytes(b"")).is_err());
    }

    use std::path::Path;

    #[test]
    fn scratch_read_takes_only_the_users_regular_file_with_one_name() {
        let d = dir("read");
        let f = d.0.join("f");
        std::fs::write(&f, "text").unwrap();
        assert_eq!(scratch_read(&f).unwrap().as_deref(), Some(&b"text"[..]));
        assert_eq!(scratch_read(&d.0.join("missing")).unwrap(), None);
        std::os::unix::fs::symlink(&f, d.0.join("l")).unwrap();
        assert!(scratch_read(&d.0.join("l")).is_err(), "a link at the name");
        std::os::unix::fs::symlink(&d.0, d.0.join("dl")).unwrap();
        assert!(
            scratch_read(&d.0.join("dl/f")).is_err(),
            "a link on the way"
        );
        std::fs::hard_link(&f, d.0.join("h")).unwrap();
        assert!(scratch_read(&f).is_err(), "a second name");
        assert!(scratch_read(&d.0).is_err(), "a directory");
        let fifo = d.0.join("fifo");
        let name = cpath(&fifo).unwrap();
        // SAFETY: a NUL-terminated path in this test's own private scratch.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(
            scratch_read(&fifo).is_err(),
            "a FIFO is refused without waiting for a writer"
        );
    }

    #[test]
    fn scratch_dir_lists_files_and_dirs_through_no_link() {
        let d = dir("list");
        std::fs::write(d.0.join("f"), "text").unwrap();
        std::fs::create_dir(d.0.join("sub")).unwrap();
        std::os::unix::fs::symlink(d.0.join("f"), d.0.join("l")).unwrap();
        let mut got = scratch_dir(&d.0).unwrap().unwrap();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            [
                ("f".into(), EntryKind::File),
                ("sub".into(), EntryKind::Dir)
            ],
            "the link is never listed"
        );
        assert_eq!(scratch_dir(&d.0.join("missing")).unwrap(), None);
        std::os::unix::fs::symlink(d.0.join("sub"), d.0.join("dl")).unwrap();
        assert!(scratch_dir(&d.0.join("dl")).is_err(), "a link at the name");
        std::os::unix::fs::symlink(&d.0, d.0.join("up")).unwrap();
        assert!(
            scratch_dir(&d.0.join("up/sub")).is_err(),
            "a link on the way"
        );
        assert!(scratch_dir(&d.0.join("f")).is_err(), "a file");
    }

    /// NEGATIVE CONTROLS: a link standing at the name is removed and what it
    /// names kept; a link on the way is refused, and the file it leads to kept.
    #[test]
    fn scratch_remove_unlinks_by_name_through_no_link() {
        let d = dir("remove");
        let f = d.0.join("sub/f");
        std::fs::create_dir_all(d.0.join("sub")).unwrap();
        std::fs::write(&f, "text").unwrap();
        scratch_remove(&f).unwrap();
        assert!(std::fs::symlink_metadata(&f).is_err());
        scratch_remove(&f).unwrap();
        scratch_remove(&d.0.join("missing/f")).unwrap();
        let outside = d.0.join("outside");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, d.0.join("sub/l")).unwrap();
        scratch_remove(&d.0.join("sub/l")).unwrap();
        assert!(std::fs::symlink_metadata(d.0.join("sub/l")).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("f"), "keep").unwrap();
        std::os::unix::fs::symlink(&elsewhere, d.0.join("via")).unwrap();
        assert!(scratch_remove(&d.0.join("via/f")).is_err());
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("f")).unwrap(),
            "keep"
        );
    }

    /// `entry_stat` reads an entry without following it, and `remove_if_same`
    /// unlinks it only while it is the file its device and inode name.
    /// NEGATIVE CONTROLS: another inode or device leaves the file; a link
    /// standing at the entry is read and removed as the link, and what it
    /// names kept; a link on the directory's path is refused, and the file it
    /// leads to kept; an absent directory or entry is nothing.
    #[test]
    fn remove_if_same_unlinks_only_the_file_it_was_through_no_link() {
        use std::os::unix::fs::{MetadataExt as _, symlink};
        let d = dir("same");
        std::fs::create_dir_all(d.0.join("sub")).unwrap();
        let f = d.0.join("sub/f");
        std::fs::write(&f, "text").unwrap();
        let m = std::fs::symlink_metadata(&f).unwrap();
        let st = entry_stat(&f).unwrap().unwrap();
        assert_eq!(
            (st.dev, st.ino, st.kind, st.uid),
            (m.dev(), m.ino(), libc::S_IFREG, m.uid())
        );
        assert!(st.born > 0);
        assert_eq!(
            entry_stat(&d.0.join("sub")).unwrap().map(|s| s.kind),
            Some(libc::S_IFDIR)
        );
        assert!(
            !remove_if_same(&f, st.dev, st.ino + 1).unwrap(),
            "another inode"
        );
        assert!(
            !remove_if_same(&f, st.dev.wrapping_add(1), st.ino).unwrap(),
            "another device"
        );
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "text");
        assert!(remove_if_same(&f, st.dev, st.ino).unwrap());
        assert!(std::fs::symlink_metadata(&f).is_err());
        assert_eq!(entry_stat(&f).unwrap(), None);
        assert!(!remove_if_same(&f, st.dev, st.ino).unwrap(), "already gone");
        // a link at the entry: read as the link, removed as the link, what it names kept
        let target = d.0.join("target");
        std::fs::write(&target, "keep").unwrap();
        let l = d.0.join("sub/l");
        symlink(&target, &l).unwrap();
        let ls = entry_stat(&l).unwrap().unwrap();
        let ts = entry_stat(&target).unwrap().unwrap();
        assert_eq!(ls.kind, libc::S_IFLNK);
        assert!(
            !remove_if_same(&l, ts.dev, ts.ino).unwrap(),
            "the target's identity is not the link's"
        );
        assert!(remove_if_same(&l, ls.dev, ls.ino).unwrap());
        assert!(std::fs::symlink_metadata(&l).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        // a link on the directory's path is refused, the file it leads to kept
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("f"), "keep").unwrap();
        let es = entry_stat(&elsewhere.join("f")).unwrap().unwrap();
        symlink(&elsewhere, d.0.join("via")).unwrap();
        assert!(entry_stat(&d.0.join("via/f")).is_err());
        assert!(remove_if_same(&d.0.join("via/f"), es.dev, es.ino).is_err());
        assert_eq!(
            std::fs::read_to_string(elsewhere.join("f")).unwrap(),
            "keep"
        );
        // an absent directory or entry is nothing
        assert_eq!(entry_stat(&d.0.join("missing/f")).unwrap(), None);
        assert!(!remove_if_same(&d.0.join("missing/f"), es.dev, es.ino).unwrap());
        assert_eq!(entry_stat(&d.0.join("sub/none")).unwrap(), None);
    }

    /// A file just written and read is in the page cache; evicting it leaves none of its
    /// pages there, and its bytes are unchanged (the eviction writes nothing).
    #[test]
    fn evicted_pages_leave_the_page_cache_and_the_file_unchanged() {
        let d = dir("pages");
        let f = d.0.join("sub/f");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = (0..4 << 20).map(|i: u32| (i % 251) as u8).collect();
        std::fs::write(&f, &bytes).unwrap();
        // Written out, so its pages are clean; a test promises nothing past a crash.
        aterm_durable::ordered(&std::fs::File::open(&f).unwrap()).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), bytes);
        let (resident, pages) = resident_pages(&d.0).unwrap();
        assert!(
            pages > 0 && resident == pages,
            "{resident}/{pages} after a read"
        );
        evict_pages(&d.0).unwrap();
        assert_eq!(resident_pages(&d.0).unwrap(), (0, pages));
        assert_eq!(std::fs::read(&f).unwrap(), bytes);
    }

    #[test]
    fn the_user_temp_dir_is_named_and_this_process_is_in_no_jail() {
        assert!(user_temp_dir().is_some_and(|t| t.starts_with('/')));
        // a mark no profile names is denied to no process: none is found, and the query itself is answered
        let none = jail_pids("lab.jail.test.0000000000000000").unwrap();
        assert!(none.is_empty(), "{none:?}");
    }
}
