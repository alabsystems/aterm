// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm spare's hold FIFOs in `<control dir>/spare` ([`crate::SPARE_DIR`]): the name the host
//! gives one, `<pid>-<birth>-<sid>` (its owner's identity and the session's id, beside the relative
//! symlink `<sid>` the shell finds), the two ends of the hold ([`hold_ends`]), and the sweep that
//! releases the ones whose owner has ended.
//!
//! The FIFO is the mark that says a session is a spare: a hold `stat`s it through the link and
//! never opens it. What the hold reads is the descriptor the host hands the shell at
//! [`HOLD_FD`], so a host gone at any point reads as end-of-file. The sweep releases the FIFOs of
//! older builds' spares, whose holds open the path at each read.

use std::path::Path;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::process::{Birth, ProcessPresence, process_presence, read_birth};

/// `<pid>-<birth>-<sid>`: the name of session `sid`'s hold FIFO, owned by this process. Its birth
/// ([`read_birth`], 0 where none reads) tells this process from a later one given its pid.
#[must_use]
pub fn name(sid: &str) -> String {
    static OWNER: OnceLock<String> = OnceLock::new();
    let owner = OWNER.get_or_init(|| {
        let pid = std::process::id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(read_birth)
            .map_or(0, |b| b.0);
        format!("{pid}-{birth}")
    });
    format!("{owner}-{sid}")
}

/// The descriptor number a spare shell reads its hold on: the host spawns it holding the shell's
/// end of [`hold_ends`] there, and every hold reads it (`read -u 10`; fish `read </dev/fd/10`).
/// 3 to 9 are the user's (`exec 3>&1`, `exec 9>lock`); zsh, bash and fish take their own
/// descriptors at the lowest free number from 10 up, so they step over it.
pub const HOLD_FD: i32 = 10;

/// The two ends of a spare's hold, each close-on-exec in this process.
#[derive(Debug)]
pub struct HoldEnds {
    /// What the host writes the protocol lines to. Its writes never wait: a full buffer is an
    /// error, not a stall.
    pub host: std::fs::File,
    /// What the shell reads, handed to it at [`HOLD_FD`]. Blocking, as every copy of it must stay:
    /// a copy shares this open file description, and a non-blocking read fails at once, which
    /// would spend the hold's retries. Its read is end-of-file once every writer is closed.
    pub shell: std::fs::File,
}

/// The ends of the hold for the spare whose FIFO is at `path`: on Darwin the FIFO itself, opened
/// read-write for the host (`O_NONBLOCK`) and read-only for the shell (blocking; the open does not
/// wait, since the host's end is a writer already), checked to be the one FIFO. Elsewhere an anonymous pipe made
/// close-on-exec atomically (`pipe2`), its write end alone non-blocking, and `path` is left as the
/// mark it is: fish reads its end through `/dev/fd/10`, which Linux opens
/// again, and opening a named FIFO again waits for a writer, so a fish whose host is gone would
/// wait there for ever, while opening a pipe again never waits. Darwin has no `pipe2`, and its
/// `/dev/fd` hands back the same open file, so the FIFO serves there.
///
/// # Errors
/// Either open's or `pipe2`'s; `fcntl`'s when the write end cannot be made non-blocking; and on
/// Darwin `InvalidData` when the two opens are not the one FIFO.
pub fn hold_ends(path: &Path) -> std::io::Result<HoldEnds> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _};
        let host = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let shell = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let (a, b) = (host.metadata()?, shell.metadata()?);
        let one_fifo = a.file_type().is_fifo()
            && b.file_type().is_fifo()
            && (a.dev(), a.ino()) == (b.dev(), b.ino());
        if !one_fifo || !close_on_exec(host.as_raw_fd()) || !close_on_exec(shell.as_raw_fd()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{}: the hold's two ends are not one close-on-exec FIFO",
                    path.display()
                ),
            ));
        }
        Ok(HoldEnds { host, shell })
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        let _ = path;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a live two-int buffer `pipe2` fills.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `pipe2` just made both descriptors, and nothing else owns either.
        let (shell, host) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let fd = host.as_raw_fd();
        // SAFETY: `F_GETFL` and `F_SETFL` read and set the status flags of `fd`, open and owned
        // here; they touch no memory. The flag is on the write end's own open file description.
        let made = unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
        };
        if !made {
            return Err(std::io::Error::last_os_error());
        }
        Ok(HoldEnds {
            host: host.into(),
            shell: shell.into(),
        })
    }
}

/// Whether `fd` is close-on-exec.
#[cfg(target_vendor = "apple")]
fn close_on_exec(fd: libc::c_int) -> bool {
    // SAFETY: `F_GETFD` reads the descriptor flags of `fd`, open here; it touches no memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0 && flags & libc::FD_CLOEXEC != 0
}

/// Release every FIFO in `dir` whose owner has ended or whose pid now has a different birth,
/// with its `<sid>` link while that still names it. An unread identity alone releases nothing.
/// What a launch does in its control dir, so a spare whose owner died waiting stops waiting.
pub fn sweep_dead(dir: &Path) {
    sweep_dead_with(dir, read_birth, process_presence);
}

fn sweep_dead_with(
    dir: &Path,
    birth_of: impl Fn(i32) -> Option<Birth>,
    presence: impl Fn(i32) -> ProcessPresence,
) {
    use std::os::unix::fs::FileTypeExt as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, birth, sid)) = name.to_str().and_then(name_parts) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_fifo())
            || !owner_ended(pid, birth, &birth_of, &presence)
        {
            continue;
        }
        if let Some(wake) = release(&entry.path(), &dir.join(sid), write_end) {
            keep(wake);
        }
    }
}

/// The line a release leaves in a FIFO once both its names are gone. No hold knows it: an older
/// build's hold, which opens the FIFO's path at each read, loops, fails to open the name it holds
/// on, and exits; this build's, which reads its descriptor (on Darwin the FIFO itself), loops and
/// reads end-of-file once the wake closes.
const END: &[u8] = b"end\n";

fn write_end(wake: &std::fs::File) -> std::io::Result<()> {
    use std::io::Write as _;
    (&*wake).write_all(END)
}

/// How long a released FIFO's wake stays open while the FIFO still holds a line. It serves only a
/// shell inside the one `open` call that resolved the FIFO's name before the unlinks, which takes
/// far less.
const KEEP_FOR: Duration = Duration::from_secs(60);

/// How often [`KEPT`]'s thread looks for a wake to close while it keeps one.
const REAP_EVERY: Duration = Duration::from_secs(1);

/// The wakes a release left `end` in, each with the instant it closes by.
#[derive(Default)]
struct Keeper {
    kept: Vec<(std::fs::File, Instant)>,
    /// Whether the thread that reaps [`KEPT`] runs.
    reaping: bool,
}

impl Keeper {
    /// Keep `wake`, released at `now`, until [`KEEP_FOR`] has passed, closing first what
    /// [`Self::reap`] closes.
    fn keep(&mut self, wake: std::fs::File, now: Instant) {
        self.reap(now);
        self.kept.push((wake, now + KEEP_FOR));
    }

    /// Close each wake whose FIFO a shell has emptied, or whose time is up at `now`. A shell that
    /// read the line opens no name of the FIFO again, so nothing waits on the wake any more.
    fn reap(&mut self, now: Instant) {
        self.kept
            .retain(|(wake, until)| now < *until && holds_a_line(wake));
    }
}

/// Whether the FIFO `wake` holds read-write has anything left to read, asked of `poll`, which
/// reads the FIFO's buffer. An unanswered ask counts as a line left. Darwin's `FIONREAD` is no
/// stand-in: on a read-write descriptor of a FIFO it reports 0 with a line waiting.
fn holds_a_line(wake: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut fd = libc::pollfd {
        fd: wake.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `fd` is one valid `pollfd` that outlives the call, and the zero timeout never waits.
    let ready = unsafe { libc::poll(&raw mut fd, 1, 0) };
    ready < 0 || fd.revents & libc::POLLIN != 0
}

/// The wakes this process released FIFOs with: closing one before a shell has read its line, or
/// before [`KEEP_FOR`] has passed, would leave a shell that resolved the FIFO before the release,
/// and opens it after, waiting for ever.
static KEPT: Mutex<Keeper> = Mutex::new(Keeper {
    kept: Vec::new(),
    reaping: false,
});

/// Wakes [`KEPT`]'s thread when a wake is kept.
static KEPT_GREW: Condvar = Condvar::new();

/// Keep `wake` in [`KEPT`], starting the thread that reaps it unless one runs. Should no thread
/// start, each later keep reaps what is due and tries again.
fn keep(wake: std::fs::File) {
    let mut keeper = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    keeper.keep(wake, Instant::now());
    if !keeper.reaping {
        keeper.reaping = std::thread::Builder::new()
            .name("aterm-spare-wakes".to_owned())
            .spawn(reap_kept)
            .is_ok();
    }
    KEPT_GREW.notify_one();
}

/// [`KEPT`]'s thread: it reaps every [`REAP_EVERY`] while a wake is kept, and waits for one
/// otherwise.
fn reap_kept() {
    let mut keeper = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        keeper = if keeper.kept.is_empty() {
            KEPT_GREW
                .wait(keeper)
                .unwrap_or_else(PoisonError::into_inner)
        } else {
            KEPT_GREW
                .wait_timeout(keeper, REAP_EVERY)
                .unwrap_or_else(PoisonError::into_inner)
                .0
        };
        keeper.reap(Instant::now());
    }
}

/// Open a wake on the FIFO at `path`, then unlink `link` while it names the FIFO and the FIFO
/// itself, and write the line through the wake with `end` ([`end_after_unlinks`]), returning the
/// wake for the caller to keep. The wake opens the FIFO read-write, which never waits or fails for
/// want of a reader (a write-only, non-blocking open fails with `ENXIO` while none holds the
/// FIFO), and is open across both unlinks: a shell waiting to open the FIFO, or one that resolved
/// `link` before its unlink and opens the FIFO while the wake is kept, gets through and reads the
/// line. If the wake cannot open, or what it opened is no FIFO, both names stay for a later sweep;
/// if the line cannot be written, the wake closes and a shell that got through reads end-of-file
/// instead.
fn release(
    path: &Path,
    link: &Path,
    end: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> Option<std::fs::File> {
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let wake = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !wake.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
        return None;
    }
    end_after_unlinks(wake, path, Some(link), end)
}

/// Unlink `link` while it names the FIFO at `path`, then `path`, while `wake` holds the FIFO
/// open; then write the line through `wake` with `end`, and return `wake` once it is written. The
/// line follows both unlinks: a shell that read it before they were done would open the FIFO
/// again by a name still there and wait on the kept wake for a line that never comes.
fn end_after_unlinks(
    wake: std::fs::File,
    path: &Path,
    link: Option<&Path>,
    end: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> Option<std::fs::File> {
    if let Some(link) = link
        && std::fs::read_link(link).is_ok_and(|to| Some(to.as_os_str()) == path.file_name())
    {
        let _ = std::fs::remove_file(link);
    }
    let _ = std::fs::remove_file(path);
    end(&wake).ok()?;
    Some(wake)
}

/// `(pid, birth, sid)` of a FIFO's name, its sid `s-<hex>` of at most 64 characters.
fn name_parts(name: &str) -> Option<(u32, u64, &str)> {
    let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = name.splitn(3, '-');
    let pid = parts.next().filter(|s| uint(s))?.parse().ok()?;
    let birth = parts.next().filter(|s| uint(s))?.parse().ok()?;
    let sid = parts.next()?;
    let hex = sid.strip_prefix("s-")?;
    (sid.len() <= 64 && !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some((pid, birth, sid))
}

/// Positive evidence that the owner ended or its pid was reused. Identity reads can fail for a
/// live owner (on Linux, even descriptor exhaustion): absence requires a separate kernel answer.
fn owner_ended(
    pid: u32,
    birth: u64,
    birth_of: &impl Fn(i32) -> Option<Birth>,
    presence: &impl Fn(i32) -> ProcessPresence,
) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if birth != 0
        && let Some(now) = birth_of(pid)
    {
        return now != Birth(birth);
    }
    matches!(
        presence(pid),
        ProcessPresence::Exited | ProcessPresence::Absent
    )
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "spare_fifo_tests.rs"]
mod tests;
