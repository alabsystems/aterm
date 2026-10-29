// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX PTY backend: [`open_pty_pair_cloexec`], `fork`, `login_tty`, `execve`, and tty ioctls.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{ShellHangup, ShellIdentity, SpawnedShell, build_child_env};

const SANDBOX_EXEC_PATH: &str = aterm_uds::os_path!("sandbox-exec");

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
    // SAFETY: `buf` is a live owned buffer of `buf.len()` bytes that `ptsname_r` NUL-terminates.
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
    // SAFETY: `name` is a NUL-terminated pts path; `open` reads it and returns a fresh fd or -1.
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

fn open_pty_pair_cloexec(
    termp: Option<&libc::termios>,
    winp: Option<&libc::winsize>,
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

    // SAFETY: `master` is a live ptmx fd; `grantpt` and `unlockpt` take no pointer arguments.
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
        // SAFETY: `path` is the fifo we just created; `chmod` only sets its mode.
        unsafe {
            libc::chmod(path.as_ptr(), 0o600);
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
    )
}

/// [`spawn_shell_with_pid`], filling the winsize pixel fields from the host's cell metrics.
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
) -> io::Result<SpawnedShell> {
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

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
        Some(dir) => CString::new(dir.as_bytes()).ok(),
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
    let kit = match pooled {
        Some(kit) => {
            kit.size(&ws);
            kit
        }
        None => SpawnKit::open(Some(&ws))?,
    };
    let (master, slave, status_rd, status_wr) = kit.into_raw();
    let inherit_strip = aterm_uds::spawnfd::InheritStrip::prepare();
    // SAFETY: `fork` takes no arguments and returns child pid, 0, or -1.
    let pid = unsafe { libc::fork() };
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
        // SAFETY: `slave` is the child's inherited pty fd; `login_tty`/`dup2`/`close` are a-s-safe.
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
        // SAFETY: `sa` and `empty` are zeroed/initialized locals; signal calls are a-s-safe.
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
            // SAFETY: `dir` is a valid NUL-terminated path; `chdir` is async-signal-safe.
            unsafe {
                libc::chdir(dir.as_ptr());
            }
        }
        // SAFETY: `master` is this child's inherited master fd; `close` is async-signal-safe.
        unsafe {
            if master > libc::STDERR_FILENO {
                libc::close(master);
            }
        }
        #[cfg(test)]
        pre_exec_stall::in_child();
        // SAFETY: `exec_target` is a live C string, and `argv` and `envp` null-terminated arrays
        // of live C strings, all outliving the call; `write`/`_exit` are async-signal-safe.
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
    let failure: Option<(io::ErrorKind, String)> = match status {
        ExecStatus::Execed => None,
        ExecStatus::FailedBeforeExec(b'S') => Some((
            io::ErrorKind::PermissionDenied,
            "sandbox confinement failed in child (fail-closed: shell not exec'd, _exit(126))"
                .to_owned(),
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
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
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

fn is_unreaped_child(pid: i32) -> bool {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return false;
    };
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` is a valid `siginfo_t` out-param and `WNOWAIT` leaves child state intact.
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
    // SAFETY: `WNOHANG` `waitpid` with a valid out-param.
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

/// Runs [`reap_shell`] for an unreaped child `pid` owned by the caller.
pub fn reap(pid: i32) {
    reap_shell(ShellIdentity::from_parts(pid, None, true));
}

/// Reaps a session's shell on the teardown thread after [`hangup_shell`].
pub fn reap_shell(identity: ShellIdentity) {
    reap_shell_scheduled(
        identity,
        Some(REAP_KILL_AT),
        REAP_DEADLINE,
        InGrace::enter(),
    );
}

/// [`reap_shell`] on a thread of its own, so the UI thread never waits.
pub fn reap_shell_in_background(identity: ShellIdentity) {
    if identity.pid <= 1 {
        return;
    }
    let grace = InGrace::enter();
    std::thread::spawn(move || {
        reap_shell_scheduled(identity, Some(REAP_KILL_AT), REAP_DEADLINE, grace);
    });
}

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
        // Distinct branch.
        if !identity.verify() {
            return;
        }
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        if Some(tick) == kill_at {
            while HOLD_REAP_KILLS.load(std::sync::atomic::Ordering::Acquire) {
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

#[cfg(any(test, debug_assertions))]
static HOLD_REAP_KILLS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test seam holding every [`reap_shell`] in its grace window until the guard drops.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn hold_reap_kills_for_test() -> ReapKillHold {
    HOLD_REAP_KILLS.store(true, std::sync::atomic::Ordering::Release);
    ReapKillHold(())
}

/// [`hold_reap_kills_for_test`]'s guard: dropping it lets the held reaps go on.
#[cfg(any(test, debug_assertions))]
pub struct ReapKillHold(());

#[cfg(any(test, debug_assertions))]
impl Drop for ReapKillHold {
    fn drop(&mut self) {
        HOLD_REAP_KILLS.store(false, std::sync::atomic::Ordering::Release);
    }
}

const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(10);
const REAP_KILL_AT: u32 = 25;
const REAP_DEADLINE: u32 = 200;

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

fn build_sandbox_wrap(
    wrapper_path: &str,
    sbpl: &str,
    prog: &CString,
    orig_argv: &[CString],
) -> io::Result<(CString, Vec<CString>)> {
    let wrapper = CString::new(wrapper_path.as_bytes())
        .map_err(|_| io::Error::other("sandbox-exec path not representable"))?;
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
    let mut wrapped: Vec<CString> = Vec::with_capacity(orig_argv.len().saturating_add(3).min(4096));
    wrapped.push(CString::new("sandbox-exec").unwrap_or_else(|_| wrapper.clone()));
    wrapped.push(CString::from(c"-p"));
    wrapped.push(sbpl_c);
    wrapped.push(prog.clone());
    wrapped.extend(orig_argv.iter().skip(1).cloned());
    Ok((wrapper, wrapped))
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
    /// The wake pipe fired because the owning session closed.
    Wake,
    /// The wakeless poll timed out with no master activity.
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

/// Block until the master is readable (or EOF), `wake_rd` fires, or `exit_rd` fires.
pub fn read_or_wake(
    master: i32,
    buf: &mut [u8],
    wake_rd: i32,
    exit_rd: i32,
    proc_rd: i32,
) -> ReadOutcome {
    read_or_poke(master, buf, wake_rd, exit_rd, proc_rd, -1)
}

/// [`read_or_wake`] that also answers [`ReadOutcome::Poke`] when `poke_rd` (`-1` for none) fires,
/// before it reads the master.
pub fn read_or_poke(
    master: i32,
    buf: &mut [u8],
    wake_rd: i32,
    exit_rd: i32,
    proc_rd: i32,
    poke_rd: i32,
) -> ReadOutcome {
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
        // SAFETY: `fds` is a valid 4-element `pollfd` array live for the `poll` call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 4, WAKELESS_POLL_TIMEOUT_MS) };
        if r < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                ReadOutcome::Idle
            } else {
                ReadOutcome::Eof
            };
        }
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
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
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
        // SAFETY: `fds` is a valid 5-element `pollfd` array live for the `poll` call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 5, -1) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return ReadOutcome::Eof;
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
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
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
            .and_then(|v| v.to_string_lossy().trim().parse().ok())
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
    // select cannot represent fds >= FD_SETSIZE — deliver, same as knob-off.
    if master >= libc::FD_SETSIZE as i32
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
        // SAFETY: both fds were bounds-checked against FD_SETSIZE above.
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
#[path = "unix_shell_identity_tests.rs"]
mod shell_identity_tests;
