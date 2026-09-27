// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LIFELINE — the descriptor a throwaway `aterm --headless` watches so that it
//! ends with the process that started it.
//!
//! ## The defect
//!
//! A headless instance outlived the harness that started it whenever the harness
//! died without running its cleanup: a SIGKILLed test runner, a script killed
//! mid-run. The in-tree harnesses ended their instances only from a `Drop`
//! (`child.kill()`), and a killed process runs no `Drop`. Measured 2026-09-26:
//! one `./aterm-before --headless` had been running for eleven days, parented to
//! launchd, holding a shell, and invisible to `aterm ctl ls` because its socket
//! lived in a scratch directory nothing reads (gap #36).
//!
//! ## The contract: one descriptor, one flag
//!
//! The launcher keeps a WRITE end and hands the child a READ end, naming its
//! number with [`FLAG`] (`--lifeline-fd <n>`). The launcher never writes. When
//! every write end is gone — the launcher exited, however it exited, since the
//! kernel closes a dead process's descriptors — the child's read returns
//! end-of-file, and a headless aterm reads that as "the process that started me
//! is gone" and shuts down. No pid is watched (a pid is recycled; a descriptor
//! cannot outlive its holders) and nothing is signalled. An instance started
//! WITHOUT the flag — a person's, a service's — behaves exactly as before.
//!
//! ## Why the launcher's ends come from a FIFO, not `pipe(2)`
//!
//! Darwin has no `pipe2`. `pipe(2)` returns both ends without close-on-exec, and
//! a `Command::spawn` on another thread of a multi-threaded launcher (every test
//! binary is one) inherits whatever is unflagged at that instant. A stranger that
//! inherited the WRITE end holds the lifeline open for as long as it lives — and
//! if the stranger is a second lifelined instance holding ours while we hold its,
//! neither ever sees end-of-file. `aterm-pty`'s `open_exec_status_channel`
//! measured that race (6 inherits in 1500 spawns) and records why no spawn lock
//! can close it; its conclusion is the one used here: on Darwin only `open(2)`
//! creates a descriptor that is close-on-exec from birth, so a window-free carrier
//! needs a PATHNAME.
//!
//! So [`Lifeline::open`] makes a FIFO in the launcher's own scratch directory,
//! opens the held end `O_RDWR` — which never blocks on a FIFO, since that
//! descriptor is its own reader — and the far end `O_RDONLY`, which does not block
//! because a writer now exists, then unlinks the name. std opens every `File`
//! close-on-exec. The held end is the writer; the far end sees end-of-file once
//! the held end (and every copy of it) is closed. Measured on macOS 26.5
//! (2026-09-26): a child blocked in `read(0)` on the far end stayed blocked while
//! the `O_RDWR` holder lived and returned end-of-file the instant it closed.
//!
//! ONE FIFO PER INSTANCE, never one shared by several: macOS hands a FIFO's
//! end-of-file to ONE reader ([`arm_for_process`] has the measurement), so a
//! lifeline two instances read would free only one of them.
//!
//! ## Why the child's STDIN
//!
//! `Command` places an inherited descriptor at 0, 1 or 2 and nowhere else without
//! a `pre_exec` hook ([`crate::spawnfd`]); a shell script can redirect any number,
//! and stdin is the one number every launcher can hand over. The instance moves it
//! off fd 0 at once ([`adopt`]) and puts `/dev/null` back, so the programs it runs
//! see the stdin a harness always gave them.

#![cfg(unix)]

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The flag that names the lifeline's descriptor: `--lifeline-fd <n>`. Named here,
/// once, because the launcher that passes it and the instance that parses it must
/// spell it the same — a misspelled flag is an `unknown option` exit, but a flag
/// the instance ignored would be a lifeline nobody watches.
pub const FLAG: &str = "--lifeline-fd";

/// The descriptor number [`Lifeline::arm`] hands the child: its stdin.
pub const STDIN: RawFd = 0;

/// The launcher's end of a lifeline: the held WRITE side of the FIFO whose read
/// side the instance watches. Dropping it — or the launcher dying, which closes it
/// without any code running — cuts the lifeline.
#[derive(Debug)]
pub struct Lifeline {
    /// `O_RDWR` on the FIFO: the one writer. Never written to.
    held: File,
}

impl Lifeline {
    /// Arm `cmd` — a `--headless` launch — with a lifeline: its stdin becomes the
    /// far end and `--lifeline-fd 0` is appended to its arguments.
    ///
    /// ORDER MATTERS in two ways. Call this AFTER any `.stdin(..)` the caller sets
    /// (the later one wins), and BEFORE any `-e`/`--command` payload is appended:
    /// everything after `-e` is the child command's argv, so a flag placed there
    /// would be handed to the shell instead of read by aterm.
    ///
    /// `dir` must be a directory only the launcher writes (its scratch root): the
    /// FIFO's name exists there for the two `open`s and is unlinked before this
    /// returns.
    ///
    /// # Errors
    /// The FIFO could not be made or opened (see [`Lifeline::open`]).
    pub fn arm(cmd: &mut Command, dir: &Path) -> io::Result<Self> {
        let (lifeline, far) = Self::open(dir)?;
        cmd.arg(FLAG).arg(STDIN.to_string()).stdin(far);
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
        // Both opens go through std, which passes O_CLOEXEC: neither end exists for
        // an instant without the flag, so no concurrent spawn carries one past its
        // exec. (Between its fork and its exec it does hold copies — which delays
        // the end-of-file by that instant, and no more.)
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

    /// Cut the lifeline now: the instance reads end-of-file and shuts down. The
    /// same as dropping it, spelled for a caller that means it.
    pub fn cut(self) {
        drop(self.held);
    }
}

/// Arm `cmd` with a lifeline this process holds until it ENDS, however it ends —
/// for a harness that already ends its instances itself (a `Drop` that kills
/// them) and needs only the case it cannot cover: a runner that is SIGKILLed runs
/// no `Drop`, but the kernel still closes every held end, and each instance armed
/// here reads end-of-file. Same ordering rule as [`Lifeline::arm`]: after any
/// `.stdin(..)`, before any `-e` payload.
///
/// ONE FIFO PER INSTANCE, never one shared by several: measured on macOS 26.5
/// (2026-09-26), XNU's FIFO read clears the end-of-file indication after returning
/// it ONCE, so of two readers blocked on one FIFO when its last writer closed, the
/// first returned end-of-file and the second stayed blocked with no writer left
/// anywhere. A lifeline read by two instances would free only one of them.
///
/// # Errors
/// The FIFO could not be made or opened (see [`Lifeline::open`]).
pub fn arm_for_process(cmd: &mut Command, dir: &Path) -> io::Result<()> {
    /// The held ends, kept for the life of the process: never dropped, so each is
    /// closed by the kernel when the process ends and at no other moment.
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
    // only reads it; `mkfifo` creates one filesystem node and touches no memory
    // of ours beyond that string.
    if unsafe { mkfifo(c_path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Take over the lifeline an instance was started with: validate descriptor
/// `number`, move it to a fresh close-on-exec descriptor numbered 3 or above,
/// and retire `number` — `/dev/null` goes back on a stdio number (0-2), any
/// other number is closed. Nothing the instance spawns afterwards inherits it.
///
/// # Errors
/// `number` is negative, not open, or not something whose read ends when the
/// far side goes away (a pipe, a FIFO or a socket): a regular file would read
/// end-of-file at once and a terminal never would, so either is refused rather
/// than watched. A pipe or FIFO end OPEN FOR WRITING is refused too: held here,
/// a read-write end (the launcher's own, passed by mistake) makes this process a
/// writer of the very lifeline it waits on, so end-of-file could never come — a
/// lifeline announced as armed that no launcher's exit can cut. On every error
/// `number` is left exactly as it was.
pub fn adopt(number: RawFd) -> io::Result<OwnedFd> {
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
    // borrow ends two lines down (this function is the only code that retires it,
    // and does so only after the copy exists).
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
    // A socket is read-write by nature and ends at its PEER's close; a pipe or
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

/// Close every OTHER descriptor of this process, numbered 3 or above, that refers
/// to the same FIFO node as `lifeline`, and name the numbers closed.
///
/// A copy the instance holds of its OWN lifeline is a writer it would wait on
/// forever — end-of-file never comes while the reader itself keeps a write end
/// open — and a copy it passes to its shells outlives it the same way. That is
/// not hypothetical: measured on macOS 26.5 (2026-09-26), bash 3.2's
/// `( exec prog <fifo 9>&- )` hands `prog` a copy of the launcher's read-write
/// fd 9 at fd 11, so a SIGKILLed `capture.sh` left its instance running with the
/// lifeline held by the instance itself. Nothing an instance does needs such a
/// copy, so it goes. Call it once, right after [`adopt`], while the launch is
/// single-threaded: it closes numbers no other code in the process owns.
///
/// WHAT IDENTITY CAN FIND, measured on macOS 26.5 (2026-09-26): both ends of a
/// NAMED FIFO are one node, so a copy of the launcher's end is found. The two
/// ends of a `pipe(2)` are two different nodes on Darwin, so a leaked write end
/// of an anonymous pipe is not (only other copies of the read end are, which are
/// harmless to close). A socket is never swept: every TCP and UDP socket stats as
/// one shared (device, inode 0), so "the same node" would be every inet socket
/// the instance inherited, and a socketpair's peer is a different node anyway.
///
/// # Errors
/// The lifeline itself cannot be examined, or `/dev/fd` cannot be listed; nothing
/// is closed then.
pub fn close_inherited_copies(lifeline: &OwnedFd) -> io::Result<Vec<RawFd>> {
    use std::os::unix::fs::MetadataExt as _;
    // Only a FIFO node, and never inode 0 (no node at all): see "what identity
    // can find" above.
    let identity = |fd: RawFd| {
        std::fs::metadata(format!("/dev/fd/{fd}"))
            .map(|m| (m.file_type().is_fifo() && m.ino() != 0).then(|| (m.dev(), m.ino())))
    };
    let own_fd = lifeline.as_raw_fd();
    let Some(own) = identity(own_fd)? else {
        return Ok(Vec::new());
    };
    // Listed first and examined after, so the listing's own descriptor is closed
    // by the time any number is looked at (it then fails to stat, and is skipped).
    let numbers: Vec<RawFd> = std::fs::read_dir("/dev/fd")?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    let mut closed = Vec::new();
    for fd in numbers {
        if fd <= 2 || fd == own_fd || identity(fd).ok().flatten() != Some(own) {
            continue;
        }
        // SAFETY: `fd` is open, refers to the lifeline's pipe, and is none of the
        // process's own descriptors that code holds (see the caller contract
        // above); closing it touches nothing else.
        unsafe { close(fd) };
        closed.push(fd);
    }
    Ok(closed)
}

/// How a lifeline ended.
#[derive(Debug)]
pub enum Cut {
    /// End-of-file: every write end is closed — the launcher is gone.
    Closed,
    /// The read failed; the error says why. A lifeline that cannot be read can no
    /// longer say whether its launcher lives, and the watcher treats that as cut.
    Failed(io::Error),
}

/// How long [`wait_for_cut`] sleeps between reads of a NON-BLOCKING lifeline.
/// `poll(2)` cannot stand in: measured on macOS 26.5 (2026-09-26), `poll` on a FIFO
/// whose last writer closed reports neither `POLLIN` nor `POLLHUP`, while `read`
/// returns end-of-file — so a non-blocking descriptor is read on this cadence
/// instead. [`Lifeline`] hands over a blocking one, which never takes this path.
pub const NONBLOCKING_READ_GAP: Duration = Duration::from_millis(250);

/// Block until the lifeline is cut. Bytes a launcher writes are read and
/// discarded — a byte is not a verdict — so only end-of-file or a failed read
/// ends the wait.
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

// The whole FFI surface of this module. `fcntl` is variadic by declaration, which
// is not cosmetic on arm64 Darwin (a variadic argument is passed differently from
// a fixed one); see `crate::spawnfd`.
unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
    fn mkfifo(path: *const std::ffi::c_char, mode: Mode) -> i32;
}

/// `mode_t`: 16 bits on Darwin, 32 on Linux. The width is the ABI, not a detail:
/// Apple's arm64 convention has the CALLER extend a sub-word argument.
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

    /// `fcntl(2)`'s status-flag setter (4 on every supported Unix; the getter is
    /// the module's) and `O_NONBLOCK`, which is NOT portable: 0x0004 on Darwin,
    /// 0x0800 on Linux.
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

    /// Put `file` in non-blocking mode, so a read ANSWERS instead of waiting.
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

    /// THE LOOP, every answer a read can give: bytes, an interrupt and a
    /// would-block each keep it reading; only end-of-file ends it, as `Closed`, on
    /// exactly the call that returned it — and a failed read ends it as `Failed`.
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

    /// THE CARRIER: while the held end lives, the far end has nothing to read and
    /// is NOT at end-of-file (a non-blocking read says would-block); once the held
    /// end is cut, the same read is end-of-file. Both ends are close-on-exec from
    /// birth and the FIFO's name is already gone — nothing can join it later.
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
        // End-of-file follows the cut — not always on the very next read: a spawn
        // on another test thread copies every descriptor, close-on-exec ones too,
        // and holds the copies until its exec closes them, so for that instant the
        // held end still has a holder. The wait ends at the end-of-file; the
        // deadline is a ceiling for a broken build.
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

    /// `wait_for_cut` on a real lifeline returns `Closed` once the held end goes.
    #[test]
    fn the_wait_returns_when_the_held_end_goes() {
        let dir = scratch("wait");
        let (lifeline, far) = Lifeline::open(&dir).expect("open a lifeline");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(wait_for_cut(OwnedFd::from(far)));
        });
        drop(lifeline);
        // A ceiling for a broken build, not a clock the pass depends on: the
        // answer arrives as soon as the watcher's read returns.
        let cut = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the watcher never saw the cut");
        assert!(matches!(cut, Cut::Closed), "{cut:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `arm` gives the child the far end as stdin and names it `--lifeline-fd 0`:
    /// a `/bin/sh` that copies stdin to nowhere finishes only after the launcher
    /// lets go.
    #[test]
    fn an_armed_child_reads_end_of_file_when_the_launcher_lets_go() {
        let dir = scratch("child");
        let mut cmd = Command::new("/bin/sh");
        // The appended flag lands in sh's `$@`, where nothing reads it.
        cmd.arg("-c").arg("cat >/dev/null; echo done").arg("sh");
        cmd.stdout(std::process::Stdio::piped());
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

    /// Selects the staged launcher: present in the environment means "you ARE the
    /// launcher that gets SIGKILLed"; its value is the directory for the marks.
    const STAGE_ENV: &str = "ATERM_UDS_LIFELINE_STAGE";

    /// THE CASE THE LIFELINE EXISTS FOR, at the library level: a launcher arms two
    /// children with its process lifeline ([`arm_for_process`]) and is then
    /// SIGKILLed — no `Drop`, no cleanup, nothing of its own runs — and both
    /// children read end-of-file anyway, because the kernel closed the held end.
    ///
    /// IT RUNS IN A CHILD OF ITSELF: the lifeline is held for the life of the
    /// process that made it, so the launcher has to be a process this test can
    /// kill. The test re-execs its own binary as that launcher.
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
            // Wait to be killed: never return, so nothing of ours can run a cleanup.
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        let dir = scratch("killed");
        let mut launcher = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("a_sigkilled_launcher_cuts_its_process_lifeline")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(STAGE_ENV, &dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("re-exec this test binary as the launcher");
        // A ceiling for a broken build, not a clock the pass depends on: each wait
        // below ends at its event.
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

    /// THE SELF-HELD WRITER: an instance that inherited a copy of its launcher's
    /// end (bash 3.2 leaks one at fd 11) would wait on itself forever.
    /// `close_inherited_copies` closes exactly that copy — named, and gone — and
    /// with the only writer gone the wait ends at once.
    ///
    /// IT RUNS IN A CHILD OF ITSELF: the sweep closes descriptors BY NUMBER, which
    /// in this multi-threaded test process could close a number another test just
    /// opened. The staged child is single-threaded where it matters and owns every
    /// number it has.
    #[test]
    fn an_inherited_copy_of_the_lifeline_is_closed_and_the_cut_then_arrives() {
        if let Some(dir) = std::env::var_os(COPIES_STAGE_ENV) {
            let (Lifeline { held }, far) =
                Lifeline::open(std::path::Path::new(&dir)).expect("open a lifeline");
            // The launcher's end, leaked INTO the instance the way bash leaks it.
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

    /// THE SWEEP KNOWS ONLY A FIFO. A node's (device, inode) names a FIFO, but not
    /// a socket: measured on macOS 26.5 (2026-09-26), every TCP and UDP socket —
    /// listening, connected or bound — stats as the SAME (device, inode 0), and a
    /// socketpair's two ends are two different inodes. So identity cannot find a
    /// copy of a socket's peer, and read as "the same node" it would find every
    /// unrelated inet socket the instance inherited and close it. Given a socket
    /// lifeline the sweep closes nothing.
    ///
    /// IT RUNS IN A CHILD OF ITSELF, for the reason the test above gives.
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

    /// `adopt` moves the lifeline off its number to a close-on-exec copy at 3 or
    /// above and closes the original when it is not a stdio number.
    #[test]
    fn adopt_moves_the_descriptor_and_retires_the_original() {
        let dir = scratch("adopt");
        let (lifeline, far) = Lifeline::open(&dir).expect("open");
        let raw =
            std::os::fd::IntoRawFd::into_raw_fd(far.as_fd().try_clone_to_owned().expect("dup"));
        let adopted = adopt(raw).expect("adopt a FIFO");
        assert_ne!(adopted.as_raw_fd(), raw);
        assert!(adopted.as_raw_fd() >= 3);
        assert_eq!(fd_flags(adopted.as_raw_fd()) & FD_CLOEXEC, FD_CLOEXEC);
        assert_eq!(fd_flags(raw), -1, "the original number is closed");
        drop(far);
        drop(lifeline);
        assert!(matches!(wait_for_cut(adopted), Cut::Closed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pipe or FIFO end OPEN FOR WRITING is refused, and left open. Adopted, the
    /// launcher's own read-write end would make the instance a writer of its own
    /// lifeline — end-of-file never comes while the reader holds a write end — so
    /// the harness that asked for a lifeline would get one that can never be cut:
    /// the leak, armed and announced. It is the natural slip for a script that
    /// opened `exec 9<>fifo` and then passed `--lifeline-fd 9`. A pipe's bare
    /// write end is refused too: a read on it fails at once, which would end the
    /// instance at boot for a reason no one reading the flag would guess.
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

        // A write-only end: opened by path (so close-on-exec from birth — no
        // concurrent spawn in this test process carries it), which does not block
        // because the held end is a reader.
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

        // The READ end beside them is still a lifeline, and still cuts.
        let far = std::os::fd::IntoRawFd::into_raw_fd(far);
        let adopted = adopt(far).expect("the read-only end is adopted");
        lifeline.cut();
        assert!(matches!(wait_for_cut(adopted), Cut::Closed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refusals leave the number alone and say what it was.
    #[test]
    fn adopt_refuses_what_cannot_be_a_lifeline() {
        let err = adopt(-1).expect_err("negative");
        assert!(err.to_string().contains("not a descriptor number"), "{err}");

        let dir = scratch("refuse");
        let file = File::create(dir.join("plain")).expect("a regular file");
        let raw = file.as_raw_fd();
        let err = adopt(raw).expect_err("a regular file is refused");
        assert!(err.to_string().contains("a regular file"), "{err}");
        assert_ne!(fd_flags(raw), -1, "a refused number is left open");

        // A number nothing has open, found rather than assumed.
        let closed = (64..4096)
            .find(|&fd| fd_flags(fd) == -1)
            .expect("a free descriptor number");
        let err = adopt(closed).expect_err("a closed number is refused");
        assert!(err.to_string().contains("not an open descriptor"), "{err}");
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
