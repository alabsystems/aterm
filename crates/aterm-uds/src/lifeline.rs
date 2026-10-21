// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lifeline: the descriptor a throwaway `aterm --headless` watches so that it
//! ends with the process that started it, even when that process is SIGKILLed
//! and runs no cleanup.
//!
//! ## Contract
//!
//! The launcher keeps a write end and hands the child a read end, naming its
//! number with [`FLAG`] (`--lifeline-fd <n>`). The launcher never writes. When
//! every write end is gone (the kernel closes a dead process's descriptors),
//! the child's read returns end-of-file and a headless aterm shuts down. No pid
//! is watched and nothing is signalled. An instance started without the flag is
//! unaffected.
//!
//! ## A FIFO, not `pipe(2)`
//!
//! Darwin has no `pipe2`, so `pipe(2)` ends are briefly inheritable and a
//! concurrent `Command::spawn` can carry the write end into a stranger that then
//! holds the lifeline open. Only `open(2)` creates a descriptor close-on-exec
//! from birth, so [`Lifeline::open`] makes a FIFO in the launcher's scratch
//! directory, opens the held end `O_RDWR` (never blocks: it is its own reader)
//! and the far end `O_RDONLY` (does not block: a writer exists), then unlinks
//! the name. The far end sees end-of-file once every copy of the held end is
//! closed.
//!
//! Use one FIFO per instance: macOS delivers a FIFO's end-of-file to only one
//! blocked reader (see [`arm_for_process`]).
//!
//! ## The child's stdin
//!
//! Without a `pre_exec` hook ([`crate::spawnfd`]) `Command` can place an
//! inherited descriptor only at 0, 1 or 2, so the lifeline arrives as stdin.
//! The instance moves it off fd 0 at once ([`adopt`]) and puts `/dev/null` back
//! for the programs it runs.

#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The flag that names the lifeline's descriptor: `--lifeline-fd <n>`. Shared by
/// the launcher and the instance, since a flag the instance ignored would be a
/// lifeline nobody watches.
pub const FLAG: &str = "--lifeline-fd";

/// The descriptor number [`Lifeline::arm`] hands the child: its stdin.
pub const STDIN: RawFd = 0;

/// A launcher that can place the lifeline's reader at stdin and name that slot in the child's
/// argv: `spawnfd`'s `Command` and std's.
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

/// The launcher's end of a lifeline: the held write side of the FIFO whose read
/// side the instance watches. Dropping it, or the launcher dying, cuts the
/// lifeline.
#[derive(Debug)]
pub struct Lifeline {
    /// `O_RDWR` on the FIFO: the one writer. Never written to.
    held: File,
}

impl Lifeline {
    /// Arm `cmd` (a `--headless` launch) with a lifeline: its stdin becomes the
    /// far end and `--lifeline-fd 0` is appended to its arguments.
    ///
    /// Call this after any `.stdin(..)` the caller sets (the later one wins) and
    /// before any `-e`/`--command` payload is appended (everything after `-e` is
    /// the child command's argv).
    ///
    /// `dir` must be a directory only the launcher writes (its scratch root); the
    /// FIFO's name is unlinked before this returns.
    ///
    /// # Errors
    /// The FIFO could not be made or opened (see [`Lifeline::open`]).
    pub fn arm(cmd: &mut impl LifelineCommand, dir: &Path) -> io::Result<Self> {
        let (lifeline, far) = Self::open(dir)?;
        cmd.set_lifeline(far);
        Ok(lifeline)
    }

    /// The held end and the far end, for a launcher that wires the far end itself
    /// (a descriptor other than stdin, or a launch not built from a `Command`).
    ///
    /// # Errors
    /// `mkfifo(3)` or either `open(2)` failed; nothing is left behind (the name is
    /// removed and any end already opened is closed).
    pub fn open(dir: &Path) -> io::Result<(Self, File)> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = dir.join(format!(
            ".aterm-lifeline-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        make_fifo(&path)?;
        // std passes O_CLOEXEC on both opens, so no concurrent spawn carries
        // either end past its exec (a fork holds copies only until its exec).
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .and_then(|held| File::open(&path).map(|far| (held, far)));
        // The name has done its job either way; nothing may join the FIFO later.
        let unlinked = std::fs::remove_file(&path);
        let (held, far) = opened?;
        unlinked?;
        Ok((Self { held }, far))
    }

    /// Cut the lifeline now: the instance reads end-of-file and shuts down. Same
    /// as dropping it.
    pub fn cut(self) {
        drop(self.held);
    }
}

/// Arm `cmd` with a lifeline this process holds until it ends, however it ends:
/// for a harness that already kills its instances from a `Drop` and needs the
/// case it cannot cover, being SIGKILLed. Same ordering rule as
/// [`Lifeline::arm`]: after any `.stdin(..)`, before any `-e` payload.
///
/// Each call makes its own FIFO: XNU clears a FIFO's end-of-file after
/// returning it once, so of two readers blocked on one FIFO only the first
/// would wake.
///
/// # Errors
/// The FIFO could not be made or opened (see [`Lifeline::open`]).
pub fn arm_for_process(cmd: &mut crate::spawnfd::StdCommand, dir: &Path) -> io::Result<()> {
    /// The held ends, kept for the life of the process: never dropped, so the
    /// kernel closes each when the process ends and at no other moment.
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
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call, which
    // only reads it.
    if unsafe { mkfifo(c_path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether descriptor `number` could be taken over as a lifeline: every check [`adopt`] makes
/// and nothing it does (the private copy the checks read through is closed before this
/// returns).
///
/// For a launch that must refuse a bad lifeline before work that cannot wait for the adoption:
/// an aterm window's boot apply may replace the image, which must then find the lifeline at
/// the same number, so the window checks before the apply and adopts after it.
///
/// # Errors
/// Exactly [`adopt`]'s.
pub fn check(number: RawFd) -> io::Result<()> {
    validated_copy(number).map(drop)
}

/// Take over the lifeline an instance was started with: validate descriptor
/// `number`, move it to a fresh close-on-exec descriptor numbered 3 or above,
/// and retire `number` (`/dev/null` goes back on a stdio number 0-2, any other
/// number is closed). Nothing the instance spawns afterwards inherits it.
///
/// # Errors
/// `number` is negative, not open, or not a pipe, FIFO or socket (a regular file
/// reads end-of-file at once, a terminal never does). A pipe or FIFO end open
/// for writing is refused too: holding it would make this process a writer of
/// its own lifeline, so end-of-file could never come. On every error `number`
/// is left as it was.
pub fn adopt(number: RawFd) -> io::Result<OwnedFd> {
    let copy = validated_copy(number)?;
    if number <= 2 {
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?;
        // SAFETY: both descriptors are open; `dup2` atomically replaces `number`
        // with a copy of `/dev/null` (clearing close-on-exec on it, as a stdio
        // number should be) and touches nothing else.
        if unsafe { dup2(null.as_raw_fd(), number) } == -1 {
            return Err(io::Error::last_os_error());
        }
    } else {
        // SAFETY: `number` is open and, from here on, referenced by nothing: the
        // copy is a separate descriptor. A failed `close` is not actionable (the
        // number is released either way on every supported Unix).
        unsafe { close(number) };
    }
    Ok(OwnedFd::from(copy))
}

/// [`adopt`]'s and [`check`]'s shared half: every check a lifeline must pass,
/// made through a private close-on-exec copy of `number` (3 or above), which is
/// returned. `number` itself is only read.
fn validated_copy(number: RawFd) -> io::Result<File> {
    if number < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{FLAG} {number} is not a descriptor number"),
        ));
    }
    // SAFETY: `F_GETFD` only reads one descriptor's flags and takes no varargs; a
    // number that is not open answers -1/EBADF rather than anything undefined.
    if unsafe { fcntl(number, F_GETFD) } == -1 {
        let err = io::Error::last_os_error();
        return Err(io::Error::new(
            err.kind(),
            format!("{FLAG} {number} is not an open descriptor ({err})"),
        ));
    }
    // SAFETY: `number` was just shown open, and nothing closes it before the
    // borrow ends two lines down (only `adopt` retires it, and only after the
    // copy exists).
    let borrowed = unsafe { BorrowedFd::borrow_raw(number) };
    // `try_clone_to_owned` is `fcntl(F_DUPFD_CLOEXEC, 3)`: the copy is flagged
    // from birth and never lands on a stdio number.
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
    // A socket is read-write by nature and ends at its peer's close; a pipe or
    // FIFO end must be read-only, or this process is one of its own writers.
    if kind.is_fifo() {
        // SAFETY: `F_GETFL` only reads the status flags of the open copy.
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

/// Close every other descriptor of this process numbered 3 or above that refers
/// to the same FIFO node as `lifeline`, and return the numbers closed.
///
/// A copy of the instance's own lifeline is a writer it would wait on forever,
/// and a copy passed to its shells outlives it the same way (bash 3.2's
/// `( exec prog <fifo 9>&- )` hands `prog` a copy of the launcher's fd 9). Call
/// it once, right after [`adopt`], while the launch is single-threaded: it
/// closes numbers no other code in the process owns.
///
/// Identity finds copies of a named FIFO (both ends are one node). The two ends
/// of a Darwin `pipe(2)` are different nodes, so a leaked anonymous write end is
/// not found. Sockets are never swept: every TCP and UDP socket stats as one
/// shared (device, inode 0).
///
/// # Errors
/// The lifeline itself cannot be examined, or `/dev/fd` cannot be listed;
/// nothing is closed then.
pub fn close_inherited_copies(lifeline: &OwnedFd) -> io::Result<Vec<RawFd>> {
    use std::os::unix::fs::MetadataExt as _;
    // Only a FIFO node, and never inode 0 (no node at all), read from the
    // descriptor itself: a `/dev/fd` path lookup can answer `EBADF` for an open
    // descriptor ([`crate::spawnfd::descriptor_metadata`]), and a copy it missed
    // would stay open, a writer this instance waits on forever.
    let identity = |fd: RawFd| {
        crate::spawnfd::descriptor_metadata(fd)
            .map(|m| (m.file_type().is_fifo() && m.ino() != 0).then(|| (m.dev(), m.ino())))
    };
    let own_fd = lifeline.as_raw_fd();
    let Some(own) = identity(own_fd)? else {
        return Ok(Vec::new());
    };
    // Listed first and examined after, so the listing's own descriptor is
    // closed by the time any number is looked at (it then fails to stat).
    let numbers: Vec<RawFd> = std::fs::read_dir("/dev/fd")?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    let mut closed = Vec::new();
    for fd in numbers {
        if fd <= 2 || fd == own_fd || identity(fd).ok().flatten() != Some(own) {
            continue;
        }
        // SAFETY: `fd` is open, refers to the lifeline's FIFO, and is held by no
        // code in this process (see the caller contract above).
        unsafe { close(fd) };
        closed.push(fd);
    }
    Ok(closed)
}

/// How a lifeline ended.
#[derive(Debug)]
pub enum Cut {
    /// End-of-file: every write end is closed, so the launcher is gone.
    Closed,
    /// The read failed. A lifeline that cannot be read cannot say whether its
    /// launcher lives, so the watcher treats it as cut.
    Failed(io::Error),
}

/// How long [`wait_for_cut`] sleeps between reads of a non-blocking lifeline.
/// `poll(2)` cannot stand in: on macOS it reports neither `POLLIN` nor
/// `POLLHUP` for a FIFO whose last writer closed, while `read` returns
/// end-of-file. [`Lifeline`] hands over a blocking descriptor, which never takes
/// this path.
pub const NONBLOCKING_READ_GAP: Duration = Duration::from_millis(250);

/// Block until the lifeline is cut. Bytes a launcher writes are read and
/// discarded; only end-of-file or a failed read ends the wait.
#[must_use]
pub fn wait_for_cut(lifeline: OwnedFd) -> Cut {
    read_until_cut(File::from(lifeline), NONBLOCKING_READ_GAP)
}

/// [`wait_for_cut`]'s loop over any reader, so each answer a read can give is
/// driven deterministically in a test.
fn read_until_cut(mut lifeline: impl io::Read, gap: Duration) -> Cut {
    let mut buf = [0_u8; 64];
    loop {
        match lifeline.read(&mut buf) {
            Ok(0) => return Cut::Closed,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(gap),
            Err(e) => return Cut::Failed(e),
        }
    }
}

// The whole FFI surface of this module. `fcntl` is variadic by declaration,
// which matters on arm64 Darwin; see `crate::spawnfd`.
unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
    fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> i32;
}

/// `mode_t`: 16 bits on Darwin, 32 on Linux. The width matters: Apple's arm64
/// convention has the caller extend a sub-word argument.
#[cfg(target_vendor = "apple")]
type Mode = u16;
#[cfg(not(target_vendor = "apple"))]
type Mode = u32;

/// `fcntl(2)`'s descriptor-flag getter; 1 on every supported Unix.
const F_GETFD: i32 = 1;
/// `fcntl(2)`'s status-flag getter; 3 on every supported Unix.
const F_GETFL: i32 = 3;
/// The access-mode bits of the status flags, and the read-only mode: 3 and 0 on
/// every supported Unix.
const O_ACCMODE: i32 = 3;
const O_RDONLY: i32 = 0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::fd::AsFd as _;
    use std::process::Command;

    /// `fcntl(2)`'s status-flag setter (4 on every supported Unix) and
    /// `O_NONBLOCK`, which differs: 0x0004 on Darwin, 0x0800 on Linux.
    const F_SETFL: i32 = 4;
    #[cfg(target_vendor = "apple")]
    const O_NONBLOCK: i32 = 0x0004;
    #[cfg(not(target_vendor = "apple"))]
    const O_NONBLOCK: i32 = 0x0800;
    const FD_CLOEXEC: i32 = 1;

    /// A private scratch directory for one test's FIFO.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aterm-uds-lifeline-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// `F_GETFD` on `fd`: its descriptor flags, or -1 when it is not open.
    fn fd_flags(fd: RawFd) -> i32 {
        // SAFETY: as in `adopt` — a read of one descriptor's flags.
        unsafe { fcntl(fd, F_GETFD) }
    }

    /// The node that descriptor `fd` refers to, as (device, inode), or `None`
    /// when the number is not open.
    ///
    /// In this multi-threaded test process a closed number may be reused at once
    /// by another test, so "closed" is asserted as "no longer names the
    /// lifeline", never by the number alone. Read from the descriptor, never its
    /// `/dev/fd` path ([`crate::spawnfd::descriptor_metadata`]).
    fn node(fd: RawFd) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = crate::spawnfd::descriptor_metadata(fd).ok()?;
        Some((meta.dev(), meta.ino()))
    }

    /// Put `file` in non-blocking mode, so a read answers instead of waiting.
    fn nonblocking(file: &File) {
        let fd = file.as_raw_fd();
        // SAFETY: a read of the status-flag word of a descriptor `file` keeps open.
        let flags = unsafe { fcntl(fd, F_GETFL) };
        assert_ne!(flags, -1, "F_GETFL");
        // SAFETY: as above; the third argument is the new status-flag word.
        let set = unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
        assert_ne!(set, -1, "F_SETFL");
    }

    /// A reader that answers a script, one entry per call, and counts the calls.
    struct Script {
        answers: std::collections::VecDeque<io::Result<usize>>,
        calls: usize,
    }

    impl io::Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            let answer = self
                .answers
                .pop_front()
                .expect("read past the end of the script");
            if let Ok(n) = &answer {
                buf[..*n].fill(b'x');
            }
            answer
        }
    }

    /// Every answer a read can give: bytes, an interrupt and a would-block keep
    /// it reading; end-of-file ends it as `Closed` on exactly that call, and a
    /// failed read ends it as `Failed`.
    #[test]
    fn only_end_of_file_or_a_failed_read_ends_the_wait() {
        let mut script = Script {
            answers: [
                Ok(1),
                Err(io::ErrorKind::Interrupted.into()),
                Err(io::ErrorKind::WouldBlock.into()),
                Ok(3),
                Ok(0),
            ]
            .into(),
            calls: 0,
        };
        let cut = read_until_cut(&mut script, Duration::ZERO);
        assert!(matches!(cut, Cut::Closed), "{cut:?}");
        assert_eq!(script.calls, 5, "the wait ended before end-of-file");

        let mut failing = Script {
            answers: [Ok(1), Err(io::Error::other("EIO"))].into(),
            calls: 0,
        };
        let cut = read_until_cut(&mut failing, Duration::ZERO);
        assert!(
            matches!(&cut, Cut::Failed(e) if e.to_string() == "EIO"),
            "{cut:?}"
        );
        assert_eq!(failing.calls, 2);
    }

    /// While the held end lives, the far end is not at end-of-file (a
    /// non-blocking read would block); once it is cut, the read is end-of-file.
    /// Both ends are close-on-exec from birth and the FIFO's name is gone.
    #[test]
    fn the_far_end_reads_end_of_file_only_after_the_cut() {
        let dir = scratch("carrier");
        let (lifeline, mut far) = Lifeline::open(&dir).expect("open a lifeline");
        assert_eq!(fd_flags(lifeline.held.as_raw_fd()) & FD_CLOEXEC, FD_CLOEXEC);
        assert_eq!(fd_flags(far.as_raw_fd()) & FD_CLOEXEC, FD_CLOEXEC);
        let left: Vec<_> = std::fs::read_dir(&dir).expect("read scratch").collect();
        assert!(
            left.is_empty(),
            "the FIFO's name must be unlinked: {left:?}"
        );

        nonblocking(&far);
        let mut buf = [0_u8; 8];
        let held = far
            .read(&mut buf)
            .expect_err("a held lifeline has nothing to read");
        assert_eq!(held.kind(), io::ErrorKind::WouldBlock, "{held}");
        lifeline.cut();
        // End-of-file may lag the cut: a spawn on another test thread holds
        // copies of every descriptor until its exec. The deadline is a hang
        // detector.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match far.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => panic!("a lifeline carries no data, read {n} bytes"),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no end-of-file after the cut"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("read after the cut: {e}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `arm` gives the child the far end as stdin and names it `--lifeline-fd 0`:
    /// a `/bin/sh` that copies stdin to nowhere finishes only after the launcher
    /// lets go.
    #[test]
    fn an_armed_child_reads_end_of_file_when_the_launcher_lets_go() {
        let dir = scratch("child");
        let mut cmd = crate::spawnfd::command("/bin/sh");
        // The appended flag lands in sh's `$@`, where nothing reads it.
        cmd.arg("-c").arg("cat >/dev/null; echo done").arg("sh");
        cmd.stdout(crate::spawnfd::Stdio::piped());
        let lifeline = Lifeline::arm(&mut cmd, &dir).expect("arm");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[args.len() - 2..], [FLAG, "0"]);
        let child = cmd.spawn().expect("spawn sh");
        drop(lifeline);
        let out = child.wait_with_output().expect("wait for sh");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Selects the staged launcher: present in the environment means this is
    /// the launcher that gets SIGKILLed; its value is the directory for marks.
    const STAGE_ENV: &str = "ATERM_UDS_LIFELINE_STAGE";

    /// A launcher arms two children with [`arm_for_process`] and is then
    /// SIGKILLed; both children still read end-of-file because the kernel
    /// closed the held end.
    ///
    /// The test re-execs its own binary as the launcher, since the lifeline is
    /// held for the life of the process that made it.
    #[test]
    fn a_sigkilled_launcher_cuts_its_process_lifeline() {
        if let Some(dir) = std::env::var_os(STAGE_ENV) {
            let dir = std::path::PathBuf::from(dir);
            let mut pids = Vec::new();
            for n in 0..2 {
                let mut cmd = Command::new("/bin/sh");
                // `cat` copies stdin until end-of-file; only then is the mark made.
                cmd.arg("-c")
                    .arg("cat >/dev/null; echo done >\"$1\"")
                    .arg("sh")
                    .arg(dir.join(format!("mark{n}")))
                    .stdout(std::process::Stdio::null());
                arm_for_process(&mut cmd, &dir).expect("arm with the process lifeline");
                pids.push(cmd.spawn().expect("spawn a child").id());
            }
            // A file, not stdout: libtest owns this process's stdout.
            std::fs::write(dir.join("ready"), format!("{pids:?}")).expect("say ready");
            // Wait to be killed; return nothing, so no cleanup of ours can run.
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        let dir = scratch("killed");
        // `spawnfd`'s, so the launcher holds no other test thread's pipe.
        let mut launcher =
            crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .arg("a_sigkilled_launcher_cuts_its_process_lifeline")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(STAGE_ENV, &dir)
                .stdin(crate::spawnfd::Stdio::null())
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .spawn()
                .expect("re-exec this test binary as the launcher");
        // A hang detector; each wait below ends at its event.
        let ceiling = Duration::from_secs(60);
        let deadline = std::time::Instant::now() + ceiling;
        let ready = loop {
            if let Ok(pids) = std::fs::read_to_string(dir.join("ready")) {
                break pids;
            }
            if let Ok(Some(status)) = launcher.try_wait() {
                panic!("the launcher exited before it armed its children: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the launcher never armed its children"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let marks = [dir.join("mark0"), dir.join("mark1")];
        assert!(
            marks.iter().all(|m| !m.exists()),
            "a child finished while its launcher lived ({ready})"
        );
        launcher.kill().expect("SIGKILL the launcher");
        launcher.wait().expect("reap the launcher");
        // The marks appear as soon as each `cat` sees end-of-file.
        let deadline = std::time::Instant::now() + ceiling;
        while !marks.iter().all(|m| m.exists()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the children never saw the cut ({ready})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Selects the staged instance of the next test; its value is a scratch dir.
    const COPIES_STAGE_ENV: &str = "ATERM_UDS_LIFELINE_COPIES_STAGE";

    /// An instance holding a copy of its launcher's end (as bash 3.2 leaks at
    /// fd 11) would wait on itself forever; `close_inherited_copies` closes
    /// exactly that copy, and the wait then ends at once.
    ///
    /// Runs in a re-exec'd child, since the sweep closes descriptors by number
    /// and this multi-threaded test process could reuse one.
    #[test]
    fn an_inherited_copy_of_the_lifeline_is_closed_and_the_cut_then_arrives() {
        if let Some(dir) = std::env::var_os(COPIES_STAGE_ENV) {
            let (Lifeline { held }, far) =
                Lifeline::open(std::path::Path::new(&dir)).expect("open a lifeline");
            // The launcher's end, leaked into the instance the way bash leaks it.
            let leaked = std::os::fd::IntoRawFd::into_raw_fd(held);
            let number = std::os::fd::IntoRawFd::into_raw_fd(far);
            let adopted = adopt(number).expect("adopt the lifeline");
            let closed = close_inherited_copies(&adopted).expect("sweep");
            assert_eq!(closed, vec![leaked], "exactly the leaked copy is closed");
            assert_eq!(fd_flags(leaked), -1, "and it is gone");
            assert!(matches!(wait_for_cut(adopted), Cut::Closed));
            return;
        }
        let dir = scratch("copies");
        let out = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("an_inherited_copy_of_the_lifeline_is_closed_and_the_cut_then_arrives")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(COPIES_STAGE_ENV, &dir)
            .output()
            .expect("re-exec this test binary as the instance");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the staged instance: {:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Selects the staged instance of the next test; its value is unused.
    const SOCKETS_STAGE_ENV: &str = "ATERM_UDS_LIFELINE_SOCKETS_STAGE";

    /// Given a socket lifeline the sweep closes nothing: every TCP and UDP
    /// socket stats as the same (device, inode 0), and a socketpair's ends are
    /// different inodes, so node identity cannot find a socket's copies.
    ///
    /// Runs in a re-exec'd child, for the reason the test above gives.
    #[test]
    fn the_sweep_never_closes_a_socket_that_merely_stats_alike() {
        if std::env::var_os(SOCKETS_STAGE_ENV).is_some() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
            let near = std::net::TcpStream::connect(listener.local_addr().expect("addr"))
                .expect("connect");
            let (far, _) = listener.accept().expect("accept");
            let unrelated = std::net::UdpSocket::bind("127.0.0.1:0").expect("udp");
            let adopted = adopt(std::os::fd::IntoRawFd::into_raw_fd(far)).expect("adopt");
            let closed = close_inherited_copies(&adopted).expect("sweep");
            assert_eq!(closed, Vec::<RawFd>::new(), "nothing is a copy of a socket");
            for fd in [
                listener.as_raw_fd(),
                near.as_raw_fd(),
                unrelated.as_raw_fd(),
            ] {
                assert_ne!(fd_flags(fd), -1, "fd {fd} was closed by the sweep");
            }
            return;
        }
        let out = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("the_sweep_never_closes_a_socket_that_merely_stats_alike")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(SOCKETS_STAGE_ENV, "1")
            .output()
            .expect("re-exec this test binary as the instance");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the staged instance: {:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Selects the staged instance of the next test; its value is a scratch dir.
    #[cfg(target_vendor = "apple")]
    const CONTENDED_STAGE_ENV: &str = "ATERM_UDS_LIFELINE_CONTENDED_STAGE";

    /// The sweep finds a copy of the lifeline while another thread looks its number up.
    /// The sweep once read each number's node by `/dev/fd` path, which on Darwin answers
    /// `EBADF` for an open descriptor when another lookup of the same number lands
    /// mid-call (`spawnfd::descriptor_metadata`): the copy was passed over and stayed open,
    /// a writer the instance would wait on forever. Each round puts a copy at one number
    /// and sweeps while another thread looks that number up
    /// (`spawnfd::test_support::racing_lookups`, whose path `stat`s failing are the
    /// negative control); every sweep must close exactly that copy. On the path-reading
    /// sweep, 20 of 20 runs here missed copies.
    ///
    /// Runs in a re-exec'd child, for the reason the tests above give.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_copy_is_swept_while_its_number_is_looked_up_elsewhere() {
        /// The number each round's copy sits at: one no other test looks up.
        const COPY: RawFd = 1600;
        const F_DUPFD: i32 = 0;
        if let Some(dir) = std::env::var_os(CONTENDED_STAGE_ENV) {
            let (Lifeline { held }, far) =
                Lifeline::open(std::path::Path::new(&dir)).expect("open a lifeline");
            let adopted = adopt(std::os::fd::IntoRawFd::into_raw_fd(far)).expect("adopt");
            // The launcher's end is a copy of the same node too: not this test's.
            drop(held);
            let mut missed = Vec::new();
            let rounds = crate::spawnfd::test_support::racing_lookups(COPY, 20, |looker| {
                // SAFETY: `F_DUPFD` returns a fresh descriptor or -1.
                let copy = unsafe { fcntl(adopted.as_raw_fd(), F_DUPFD, COPY) };
                assert_eq!(copy, COPY, "fd {COPY} is held elsewhere in this process");
                looker.probe();
                let closed = close_inherited_copies(&adopted).expect("sweep");
                if closed != [COPY] {
                    missed.push(closed);
                    // SAFETY: the copy the sweep passed over, owned here alone.
                    drop(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(COPY) });
                }
            });
            assert!(
                missed.is_empty(),
                "{} of {rounds} sweeps missed the copy at {COPY}: {:?}",
                missed.len(),
                missed.first()
            );
            return;
        }
        let dir = scratch("contended");
        let out = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("a_copy_is_swept_while_its_number_is_looked_up_elsewhere")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CONTENDED_STAGE_ENV, &dir)
            .output()
            .expect("re-exec this test binary as the instance");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(why) = stderr
            .lines()
            .find_map(|l| l.split_once(crate::host::COULD_NOT_RUN).map(|(_, why)| why))
        {
            crate::host::could_not_run(format!("the staged sweep{why}"));
        }
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the staged instance: {:?}\nstdout: {stdout}\nstderr: {stderr}",
            out.status
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `adopt` moves the lifeline to a close-on-exec copy at 3 or above and
    /// closes the original when it is not a stdio number. "Closed" means the
    /// number no longer names the lifeline's node (see [`node`]).
    #[test]
    fn adopt_moves_the_descriptor_and_retires_the_original() {
        let dir = scratch("adopt");
        let (lifeline, far) = Lifeline::open(&dir).expect("open");
        let fifo = node(far.as_raw_fd()).expect("the lifeline's node");
        let raw =
            std::os::fd::IntoRawFd::into_raw_fd(far.as_fd().try_clone_to_owned().expect("dup"));
        assert_eq!(
            node(raw),
            Some(fifo),
            "the number names the lifeline before"
        );
        let adopted = adopt(raw).expect("adopt a FIFO");
        assert_ne!(adopted.as_raw_fd(), raw);
        assert!(adopted.as_raw_fd() >= 3);
        assert_eq!(fd_flags(adopted.as_raw_fd()) & FD_CLOEXEC, FD_CLOEXEC);
        assert_eq!(
            node(adopted.as_raw_fd()),
            Some(fifo),
            "the copy is the lifeline"
        );
        assert_ne!(node(raw), Some(fifo), "the original number is closed");
        drop(far);
        drop(lifeline);
        assert!(matches!(wait_for_cut(adopted), Cut::Closed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pipe or FIFO end open for writing (`exec 9<>fifo` then `--lifeline-fd 9`, or a bare
    /// write end) is refused and left open.
    #[test]
    fn adopt_refuses_an_end_open_for_writing() {
        let dir = scratch("writable");
        let (lifeline, far) = Lifeline::open(&dir).expect("open a lifeline");
        let held = std::os::fd::IntoRawFd::into_raw_fd(
            lifeline.held.as_fd().try_clone_to_owned().expect("dup"),
        );
        let err = adopt(held).expect_err("the launcher's read-write end is refused");
        assert!(err.to_string().contains("open for writing"), "{err}");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert_ne!(fd_flags(held), -1, "a refused number is left open");
        // SAFETY: `held` is the dup made above, open (just checked) and owned by
        // nothing else.
        drop(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(held) });

        // A write-only end, opened by path (close-on-exec from birth); it does
        // not block because the held end is a reader.
        let path = dir.join("write-only");
        make_fifo(&path).expect("mkfifo");
        let keep = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("the held end");
        let write_only = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("a write-only end");
        let write = std::os::fd::IntoRawFd::into_raw_fd(write_only);
        let err = adopt(write).expect_err("a write-only end is refused");
        assert!(err.to_string().contains("open for writing"), "{err}");
        assert_ne!(fd_flags(write), -1, "a refused number is left open");
        // SAFETY: `write` is open (checked above) and owned by nothing else.
        drop(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(write) });
        drop(keep);

        // The read end beside them is still a lifeline, and still cuts.
        let far = std::os::fd::IntoRawFd::into_raw_fd(far);
        let adopted = adopt(far).expect("the read-only end is adopted");
        lifeline.cut();
        assert!(matches!(wait_for_cut(adopted), Cut::Closed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`check`] gives [`adopt`]'s verdicts and moves nothing: a lifeline it passes is the same
    /// FIFO end at the same number with the same flags, still adoptable, and a refusal leaves
    /// the number open.
    #[test]
    fn check_judges_like_adopt_and_moves_nothing() {
        let dir = scratch("check");
        let (lifeline, far) = Lifeline::open(&dir).expect("open a lifeline");
        let far = std::os::fd::IntoRawFd::into_raw_fd(far);
        let (before, flags) = (node(far), fd_flags(far));
        check(far).expect("the read end is a lifeline");
        assert_eq!(node(far), before, "the same node at the same number");
        assert_eq!(fd_flags(far), flags, "with the same flags");
        let adopted = adopt(far).expect("still adoptable");
        lifeline.cut();
        assert!(matches!(wait_for_cut(adopted), Cut::Closed));

        let err = check(-1).expect_err("negative");
        assert!(err.to_string().contains("not a descriptor number"), "{err}");
        let err = check(RawFd::MAX).expect_err("a number no descriptor can have");
        assert!(err.to_string().contains("not an open descriptor"), "{err}");
        let file = File::create(dir.join("plain")).expect("a regular file");
        let raw = file.as_raw_fd();
        let err = check(raw).expect_err("a regular file is refused");
        assert!(err.to_string().contains("a regular file"), "{err}");
        assert_ne!(fd_flags(raw), -1, "a refused number is left open");
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
