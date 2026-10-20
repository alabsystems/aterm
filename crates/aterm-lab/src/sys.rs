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

/// Local calendar fields of a unix second: (year, month 1-12, day, hour, minute, second).
#[must_use]
pub fn local_time(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
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

/// Signals delivered as bytes on a pipe: the handler writes the signal's
/// number, and the reader takes it in ordinary code (a thread, a `poll`).
/// One per process.
pub struct SignalPipe {
    /// The read end; one byte per signal.
    pub read_fd: RawFd,
}

impl SignalPipe {
    /// Install the handler for `signals` (`SA_RESTART`, an empty mask).
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

    #[test]
    fn the_user_temp_dir_is_named_and_this_process_is_in_no_jail() {
        assert!(user_temp_dir().is_some_and(|t| t.starts_with('/')));
        // a mark no profile names is denied to no process: none is found, and the query itself is answered
        let none = jail_pids("lab.jail.test.0000000000000000").unwrap();
        assert!(none.is_empty(), "{none:?}");
    }
}
