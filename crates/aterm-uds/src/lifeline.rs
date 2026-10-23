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
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .and_then(|held| File::open(&path).map(|far| (held, far)));
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
/// # Errors
/// Returns an error if `number` fails validation or descriptor duplication fails.
pub fn adopt(number: RawFd) -> io::Result<OwnedFd> {
    let copy = validated_copy(number)?;
    if number <= 2 {
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
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

/// [`adopt`]'s and [`check`]'s shared half: every check a lifeline must pass,
/// made through a private close-on-exec copy of `number` (3 or above), which is
/// returned.
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
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
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

/// A lifeline read at the wakes of a loop that already wakes on its own (the
/// broker's poll), so watching it costs no thread: its descriptor is made
/// non-blocking once, and each [`OnWake::cut`] reads what is there and returns.
/// A cut is seen at the first wake after it, never sooner.
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

    /// How the lifeline ended, once it has: `None` while the launcher holds it.
    /// Never blocks.
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

/// `mode_t`: 16 bits on Darwin, 32 on Linux.
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
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::fd::AsFd as _;
    use std::process::Command;

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
        // SAFETY: `F_GETFD` reads one descriptor's flags.
        unsafe { fcntl(fd, F_GETFD) }
    }

    /// The node that descriptor `fd` refers to, as (device, inode), or `None`
    /// when the number is not open.
    fn node(fd: RawFd) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = crate::spawnfd::descriptor_metadata(fd).ok()?;
        Some((meta.dev(), meta.ino()))
    }

    /// Put `file` in non-blocking mode, so a read answers instead of waiting.
    fn nonblocking(file: &File) {
        let fd = file.as_raw_fd();
        // SAFETY: `F_GETFL` reads the status flags of an open `File` descriptor.
        let flags = unsafe { fcntl(fd, F_GETFL) };
        assert_ne!(flags, -1, "F_GETFL");
        // SAFETY: `F_SETFL` updates status flags on an open `File` descriptor.
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

    /// One look answers at once: bytes and an interrupt are read past, a read
    /// that would block is a lifeline still held (`None`, after exactly those
    /// calls), and end-of-file or a failed read is the cut. NEGATIVE CONTROL: the
    /// held answer — a look that took a would-block for a cut would end a broker
    /// whose launcher lives.
    #[test]
    fn a_look_answers_at_once_and_only_end_of_file_or_a_failure_is_a_cut() {
        let mut held = Script {
            answers: [
                Ok(2),
                Err(io::ErrorKind::Interrupted.into()),
                Err(io::ErrorKind::WouldBlock.into()),
            ]
            .into(),
            calls: 0,
        };
        assert!(look(&mut held).is_none(), "a held lifeline is no cut");
        assert_eq!(held.calls, 3, "the look ended before the would-block");

        let mut closed = Script {
            answers: [Ok(0)].into(),
            calls: 0,
        };
        assert!(matches!(look(&mut closed), Some(Cut::Closed)));

        let mut failing = Script {
            answers: [Err(io::Error::other("EIO"))].into(),
            calls: 0,
        };
        let cut = look(&mut failing);
        assert!(
            matches!(&cut, Some(Cut::Failed(e)) if e.to_string() == "EIO"),
            "{cut:?}"
        );
    }

    /// [`OnWake`] reads a real lifeline without blocking: not cut at any wake
    /// while the launcher's end lives, cut (end-of-file) once it is dropped. Its
    /// descriptor is checked non-blocking before any look, so a watch that would
    /// block fails here instead of hanging the binary in `read(2)`.
    #[test]
    fn a_lifeline_read_on_wake_is_cut_only_after_the_launcher_lets_go() {
        let dir = scratch("on-wake");
        let (lifeline, far) = Lifeline::open(&dir).expect("open a lifeline");
        let mut watch = OnWake::new(far.into()).expect("watch it on wake");
        // SAFETY: `F_GETFL` reads the status flags of the descriptor `watch` keeps open.
        let flags = unsafe { fcntl(watch.0.as_raw_fd(), F_GETFL) };
        assert_eq!(flags & O_NONBLOCK, O_NONBLOCK, "a watch never blocks");
        assert!(watch.cut().is_none(), "a held lifeline is not cut");
        assert!(watch.cut().is_none(), "nor at the next wake");
        lifeline.cut();
        // End-of-file may lag the cut: a spawn on another test thread holds
        // copies of every descriptor until its exec. The deadline is a hang
        // detector.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let cut = loop {
            if let Some(cut) = watch.cut() {
                break cut;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no cut seen after the launcher let go"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(matches!(cut, Cut::Closed), "{cut:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While the held end lives, the far end is not at end-of-file (a
    /// non-blocking read would block); once it is cut, the read is end-of-file.
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
    #[test]
    fn a_sigkilled_launcher_cuts_its_process_lifeline() {
        if let Some(dir) = std::env::var_os(STAGE_ENV) {
            let dir = std::path::PathBuf::from(dir);
            let mut pids = Vec::new();
            for n in 0..2 {
                let mut cmd = Command::new("/bin/sh");
                cmd.arg("-c")
                    .arg("cat >/dev/null; echo done >\"$1\"")
                    .arg("sh")
                    .arg(dir.join(format!("mark{n}")))
                    .stdout(std::process::Stdio::null());
                arm_for_process(&mut cmd, &dir).expect("arm with the process lifeline");
                pids.push(cmd.spawn().expect("spawn a child").id());
            }
            std::fs::write(dir.join("ready"), format!("{pids:?}")).expect("say ready");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        let dir = scratch("killed");
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
    #[test]
    fn an_inherited_copy_of_the_lifeline_is_closed_and_the_cut_then_arrives() {
        if let Some(dir) = std::env::var_os(COPIES_STAGE_ENV) {
            let (Lifeline { held }, far) =
                Lifeline::open(std::path::Path::new(&dir)).expect("open a lifeline");
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
            drop(held);
            let mut missed = Vec::new();
            let rounds = crate::spawnfd::test_support::racing_lookups(COPY, 20, |looker| {
                // SAFETY: `F_DUPFD` duplicates an open descriptor to `>= COPY` or returns -1.
                let copy = unsafe { fcntl(adopted.as_raw_fd(), F_DUPFD, COPY) };
                assert_eq!(copy, COPY, "fd {COPY} is held elsewhere in this process");
                looker.probe();
                let closed = close_inherited_copies(&adopted).expect("sweep");
                if closed != [COPY] {
                    missed.push(closed);
                    // SAFETY: `COPY` is open and uniquely owned on this missed-sweep path.
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
    /// closes the original when it is not a stdio number.
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
        // SAFETY: `held` is an open duplicated descriptor uniquely owned here.
        drop(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(held) });

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
        // SAFETY: `write` is an open descriptor uniquely owned here.
        drop(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(write) });
        drop(keep);

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
