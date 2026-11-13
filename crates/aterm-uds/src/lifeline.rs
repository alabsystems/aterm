// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FIFO lifeline descriptor watched by `aterm --headless` and `aterm link broker` so each exits with its launcher.

#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// CLI flag naming the lifeline descriptor (`--lifeline-fd <n>`).
pub const FLAG: &str = "--lifeline-fd";

/// Descriptor number [`Lifeline::arm`] wires in the child (`stdin`).
pub const STDIN: RawFd = 0;

/// Command builder capable of wiring the lifeline reader to stdin and appending [`FLAG`].
pub trait LifelineCommand {
    /// Place `far` at stdin and append the matching `--lifeline-fd 0` arguments.
    fn set_lifeline(&mut self, far: File);
}

impl LifelineCommand for crate::spawnfd::Command {
    fn set_lifeline(&mut self, far: File) {
        self.arg(FLAG).arg(STDIN.to_string()).stdin(far);
    }
}

impl LifelineCommand for crate::spawnfd::StdCommand {
    fn set_lifeline(&mut self, far: File) {
        self.arg(FLAG).arg(STDIN.to_string()).stdin(far);
    }
}

/// Launcher-held write end of the lifeline FIFO.
#[derive(Debug)]
pub struct Lifeline {
    /// Held `O_RDWR` FIFO handle.
    held: File,
}

impl Lifeline {
    /// Arms `cmd` with a lifeline on stdin and appends `--lifeline-fd 0`.
    /// # Errors
    /// Returns an error if creating or opening the FIFO in `dir` fails.
    pub fn arm(cmd: &mut impl LifelineCommand, dir: &Path) -> io::Result<Self> {
        let (lifeline, far) = Self::open(dir)?;
        cmd.set_lifeline(far);
        Ok(lifeline)
    }

    /// Opens the held and child ends of a lifeline FIFO in `dir`.
    ///
    /// Both ends are open, and the FIFO unlinked, before this returns, so before the child given
    /// the far end exists: that open returns at once because `held` already writes, and the child
    /// is handed the descriptor, never the name. That is why the cut is race-free. On macOS a FIFO
    /// reader whose open returns just as the only writer closes can miss the end of file and sleep
    /// in its read for ever (measured 2026-10-29 under load); here no close can come before the far
    /// end's open has returned.
    /// # Errors
    /// Returns an error if creating, opening, or unlinking the FIFO fails.
    pub fn open(dir: &Path) -> io::Result<(Self, File)> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = dir.join(format!(
            ".aterm-lifeline-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        make_fifo(&path)?;
        use std::os::unix::fs::OpenOptionsExt as _;
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .and_then(|held| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)
                    .map(|far| (held, far))
            });
        let unlinked = std::fs::remove_file(&path);
        let (held, far) = opened?;
        unlinked?;
        Ok((Self { held }, far))
    }

    /// Closes the lifeline immediately so the watched child observes EOF.
    pub fn cut(self) {
        drop(self.held);
    }
}

/// Arms `cmd` with a lifeline leaked for the lifetime of this process.
/// # Errors
/// Returns an error if [`Lifeline::arm`] fails.
pub fn arm_for_process(cmd: &mut crate::spawnfd::StdCommand, dir: &Path) -> io::Result<()> {
    /// Held lifeline ends retained for the lifetime of the process.
    static HELD: std::sync::Mutex<Vec<Lifeline>> = std::sync::Mutex::new(Vec::new());
    let lifeline = Lifeline::arm(cmd, dir)?;
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(lifeline);
    Ok(())
}

/// `mkfifo(3)` at `path`, mode 0600.
fn make_fifo(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the FIFO path"))?;
    // SAFETY: `c_path` is a valid NUL-terminated string for the duration of `mkfifo`.
    if unsafe { mkfifo(c_path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Validates that `number` is a readable FIFO or pipe without consuming it.
/// # Errors
/// Returns an error if `number` is invalid, closed, not a FIFO/pipe, or writable.
pub fn check(number: RawFd) -> io::Result<()> {
    validated_copy(number).map(drop)
}

/// Adopts descriptor `number` as a close-on-exec lifeline and retires the original slot.
///
/// The end of file is the cut, and on macOS a FIFO reader whose open returns just as the only
/// writer closes can miss it and never end (measured 2026-10-29 under load), so no close of the
/// launcher's writer may meet the open of the read end this adopts. The launcher opens that end
/// before the child starts and hands over the descriptor (a pipe's, or a FIFO's opened as
/// [`Lifeline::open`] opens one); or, where the child opens the FIFO by its name, either the
/// launcher's writer closes only once the child has shown its open returned, or the opener starts
/// the program that adopts it only on a byte the launcher writes after seeing that (aterm-lab's
/// app band, a launchd job that can inherit nothing, which ends without its program when its
/// launcher is gone first).
/// # Errors
/// Returns an error if `number` fails validation or descriptor duplication fails.
pub fn adopt(number: RawFd) -> io::Result<OwnedFd> {
    let copy = validated_copy(number)?;
    if number <= 2 {
        use std::os::unix::fs::OpenOptionsExt as _;
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/null")?;
        // SAFETY: both descriptors are open; `dup2` atomically replaces `number` with `/dev/null`.
        if unsafe { dup2(null.as_raw_fd(), number) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        // SAFETY: `number` is open and unreferenced after `copy` was created.
        unsafe { close(number) };
    }
    Ok(OwnedFd::from(copy))
}

/// Validate `number` as a read-only pipe or FIFO and return a close-on-exec duplicate.
fn validated_copy(number: RawFd) -> io::Result<File> {
    if number < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{FLAG} {number} is not a descriptor number"),
        ));
    }
    // SAFETY: `F_GETFD` reads one descriptor's flags with no varargs.
    if unsafe { fcntl(number, F_GETFD) } == -1 {
        let err = io::Error::last_os_error();
        return Err(io::Error::new(
            err.kind(),
            format!("{FLAG} {number} is not an open descriptor ({err})"),
        ));
    }
    // SAFETY: `number` is open and outlives `borrowed`.
    let borrowed = unsafe { BorrowedFd::borrow_raw(number) };
    let copy = File::from(borrowed.try_clone_to_owned()?);
    let kind = copy.metadata()?.file_type();
    if !(kind.is_fifo() || kind.is_socket()) {
        let what = if kind.is_file() {
            "a regular file"
        } else if kind.is_char_device() {
            "a character device (a terminal or /dev/null)"
        } else if kind.is_dir() {
            "a directory"
        } else {
            "not a pipe"
        };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{FLAG} {number} is {what}; a lifeline is the read end of a pipe or FIFO"),
        ));
    }
    if kind.is_fifo() {
        // SAFETY: `F_GETFL` reads the status flags of the open `copy` descriptor.
        let flags = unsafe { fcntl(copy.as_raw_fd(), F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if flags & O_ACCMODE != O_RDONLY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{FLAG} {number} is open for writing; a lifeline is the READ end of a pipe or \
                     FIFO — a write end held here would keep it from ever reading end-of-file"
                ),
            ));
        }
    }
    Ok(copy)
}

/// Closes descriptors `>= 3` referring to the same FIFO node as `lifeline`.
/// # Errors
/// Returns an error if `fstat` on `lifeline` fails.
pub fn close_inherited_copies(lifeline: &OwnedFd) -> io::Result<Vec<RawFd>> {
    use std::os::unix::fs::MetadataExt as _;
    let identity = |fd: RawFd| {
        crate::spawnfd::descriptor_metadata(fd)
            .map(|m| (m.file_type().is_fifo() && m.ino() != 0).then(|| (m.dev(), m.ino())))
    };
    let own_fd = lifeline.as_raw_fd();
    let Some(own) = identity(own_fd)? else {
        return Ok(Vec::new());
    };
    let numbers: Vec<RawFd> = std::fs::read_dir("/dev/fd")?
        .filter_map(|entry| {
            entry
                .ok()?
                .file_name()
                .to_str()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()
        })
        .collect();
    let mut closed = Vec::new();
    for fd in numbers {
        if fd <= 2 || fd == own_fd || identity(fd).ok().flatten() != Some(own) {
            continue;
        }
        // SAFETY: `fd` is open, matches the lifeline FIFO, and is unowned in this process.
        unsafe { close(fd) };
        closed.push(fd);
    }
    Ok(closed)
}

/// Outcome of waiting on a lifeline.
#[derive(Debug)]
pub enum Cut {
    /// Every write end closed (EOF).
    Closed,
    /// Reading the lifeline failed.
    Failed(io::Error),
}

/// Sleep interval between non-blocking reads in [`wait_for_cut`].
pub const NONBLOCKING_READ_GAP: Duration = Duration::from_millis(250);

/// Blocks until `lifeline` reaches EOF or fails a read.
#[must_use]
pub fn wait_for_cut(lifeline: OwnedFd) -> Cut {
    read_until_cut(File::from(lifeline), NONBLOCKING_READ_GAP)
}

/// [`wait_for_cut`]'s loop over any reader, so each answer a read can give is
/// driven deterministically in a test.
fn read_until_cut(mut lifeline: impl io::Read, gap: Duration) -> Cut {
    loop {
        if let Some(cut) = look(&mut lifeline) {
            return cut;
        }
        std::thread::sleep(gap);
    }
}

/// One look at a lifeline: read what is there, discarding bytes, until a read
/// would block (`None`: the launcher still holds it), answers end-of-file, or
/// fails.
fn look(mut lifeline: impl io::Read) -> Option<Cut> {
    let mut buf = [0_u8; 64];
    loop {
        match lifeline.read(&mut buf) {
            Ok(0) => return Some(Cut::Closed),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
            Err(e) => return Some(Cut::Failed(e)),
        }
    }
}

/// Non-blocking lifeline reader polled on each wake of an event loop.
#[derive(Debug)]
pub struct OnWake(File);

impl OnWake {
    /// Watch `lifeline` (from [`adopt`]) at each wake.
    ///
    /// # Errors
    /// Its status flags could not be read or set.
    pub fn new(lifeline: OwnedFd) -> io::Result<Self> {
        let file = File::from(lifeline);
        let fd = file.as_raw_fd();
        // SAFETY: `F_GETFL` only reads the status-flag word of the descriptor
        // `file` keeps open.
        let flags = unsafe { fcntl(fd, F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above; the third argument is the new status-flag word.
        if unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }

    /// Non-blocking check returning `Some(Cut)` once the lifeline has ended.
    pub fn cut(&mut self) -> Option<Cut> {
        look(&mut self.0)
    }
}

unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
    fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> i32;
}

/// Platform `mode_t` width (16-bit on Darwin, 32-bit on Linux).
#[cfg(target_vendor = "apple")]
type Mode = u16;
#[cfg(not(target_vendor = "apple"))]
type Mode = u32;

/// `fcntl(2)`'s descriptor-flag getter; 1 on every supported Unix.
const F_GETFD: i32 = 1;
/// `fcntl(2)`'s status-flag getter and setter; 3 and 4 on every supported Unix.
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
/// `O_NONBLOCK`, which differs: 0x0004 on Darwin, 0x0800 on Linux.
#[cfg(target_vendor = "apple")]
const O_NONBLOCK: i32 = 0x0004;
#[cfg(not(target_vendor = "apple"))]
const O_NONBLOCK: i32 = 0x0800;
/// The access-mode bits of the status flags, and the read-only mode: 3 and 0 on
/// every supported Unix.
const O_ACCMODE: i32 = 3;
const O_RDONLY: i32 = 0;

#[cfg(test)]
#[path = "lifeline_tests.rs"]
mod tests;
