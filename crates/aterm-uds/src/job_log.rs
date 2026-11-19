// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The log of every job aterm installs, kept bounded by the job's own process.
//!
//! A job's definition (its launchd plist, or the systemd unit that appends to a file) names the
//! files its standard output and error go to: the service manager opens them for the process at
//! each start, and nothing it does trims them. Each job's verb, as it starts, hands [`keep`] the
//! files its definition names, taken from the function that writes that definition. Of those,
//! `keep` keeps each that is the very file this process's standard output or error is (the same
//! device and inode) and leaves every other path alone: a person's run of the verb in a terminal,
//! or with its output sent to any file but one its definition names, keeps nothing and changes
//! nothing; a run whose output is such a file, by whoever started it, is kept as the job's. What it
//! kept comes back as a [`Kept`], which only `keep` makes and each job's serving function takes,
//! so no job serves without its start having called `keep`.
//!
//! A kept file never holds more than [`CAP`] for long. `keep` cuts it back at once where an
//! earlier run left it past (a job restarted faster than the thread's first cut). A [`line`] that
//! would take it past cuts it to nothing first, in place ([`Bounded`]: its inode kept, so every
//! descriptor of it, the service manager's included, writes on), and one thread, which takes no
//! signal sent to the process, cuts it back every [`BOUND_EVERY`] where other writes took it
//! past: an `eprintln!`, a vendored library's warning, a child that inherited the file. A cut
//! file opens with a dated line that says so.
//!
//! [`line`] writes one diagnostic as one line, dated ([`stamp`]: UTC, RFC 3339, to the second).
//! A line other code writes straight to standard error is not dated. Nothing here moves a
//! descriptor: standard output and error stay the files through an exec, whatever makes it, and
//! the new image's own start keeps them again; a child writes the file itself.

use std::fmt;
use std::fs::File;
use std::io::{self, Seek as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The most bytes a kept log holds: a [`line`] that would take it past cuts it to nothing first,
/// and [`BOUND_EVERY`] cuts back one that other writes took past.
pub const CAP: u64 = 512 * 1024;

/// The longest line [`line`] writes, its date and newline included: a longer diagnostic is cut.
pub const LINE_CAP: usize = 4096;

/// The length of a [`stamp`]: `YYYY-MM-DDTHH:MM:SSZ`.
pub const STAMP_LEN: usize = 20;

/// How often the kept logs are cut back where writes not through [`line`] took them past
/// [`CAP`].
pub const BOUND_EVERY: Duration = Duration::from_secs(30);

/// The longest line body: [`LINE_CAP`] less the date, its space and the newline.
const BODY_MAX: usize = LINE_CAP - STAMP_LEN - 2;

/// [`BOUND_EVERY`] in milliseconds, which a test's stand-in job sets shorter
/// (`seam::bound_every`).
static EVERY_MS: AtomicU64 = AtomicU64::new(BOUND_EVERY.as_millis() as u64);

static STATE: Mutex<State> = Mutex::new(State {
    logs: Vec::new(),
    stderr: Stderr::Stream,
    ticking: false,
});

/// What this process keeps.
struct State {
    /// Each kept file, once however many of the standard descriptors it is.
    logs: Vec<Log>,
    /// Where [`line`] writes.
    stderr: Stderr,
    /// Whether the thread that cuts the logs back runs.
    ticking: bool,
}

/// Where [`line`] writes.
enum Stderr {
    /// Standard error is no kept file: straight to it.
    Stream,
    /// Standard error is `logs[_]`: through its bound.
    Log(usize),
}

/// A kept file and its identity.
struct Log {
    /// `(st_dev, st_ino)`.
    id: (u64, u64),
    bounded: Bounded,
}

/// What [`keep`] kept: which of this process's standard descriptors are one of the files it was
/// handed. Only `keep` makes one (its fields are private), and each job's serving function takes
/// it, so a job's start that does not call `keep` does not compile. A run that keeps nothing (a
/// terminal's, a test's) has the one `keep` returns there, which keeps neither.
#[must_use = "a job's serving function takes what `keep` kept"]
#[derive(Debug, PartialEq, Eq)]
pub struct Kept {
    stdout: bool,
    stderr: bool,
}

impl Kept {
    /// Standard output is one of the files, kept.
    #[must_use]
    pub fn stdout(&self) -> bool {
        self.stdout
    }

    /// Standard error is one of the files, kept: [`line`] writes there through its bound.
    #[must_use]
    pub fn stderr(&self) -> bool {
        self.stderr
    }
}

fn hold() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now() -> u64 {
    aterm_time::unix_secs_at(std::time::SystemTime::now()).unwrap_or(0)
}

/// `secs` past the Unix epoch as UTC, RFC 3339, to the second (`2026-11-17T09:41:07Z`); an
/// instant past 9999-12-31T23:59:59Z is that.
#[must_use]
pub fn stamp(secs: u64) -> [u8; STAMP_LEN] {
    /// 9999-12-31T23:59:59Z: the last instant four digits of year can say.
    const LAST: u64 = 253_402_300_799;
    let secs = secs.min(LAST);
    let rem = secs % 86_400;
    let (year, month, day) = aterm_stamp::civil::civil_from_days((secs / 86_400) as i64);
    let mut out = *b"0000-00-00T00:00:00Z";
    for (at, width, value) in [
        (0, 4, year.unsigned_abs()),
        (5, 2, month.unsigned_abs()),
        (8, 2, day.unsigned_abs()),
        (11, 2, rem / 3600),
        (14, 2, rem % 3600 / 60),
        (17, 2, rem % 60),
    ] {
        let mut value = value;
        for digit in out[at..at + width].iter_mut().rev() {
            *digit = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    out
}

/// Whether `bytes` are a [`stamp`]'s shape: digits, and `-`, `T`, `:` and `Z` where it puts them.
fn is_stamp(bytes: &[u8]) -> bool {
    bytes.len() == STAMP_LEN
        && bytes.iter().enumerate().all(|(at, &b)| match at {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

/// `line` without the date [`line`] put before it (and the space after the date), or all of
/// `line` where it carries none: what a reader of a job's log quotes. A line that is only a date
/// (an empty diagnostic, or one a reader trimmed) is empty.
#[must_use]
pub fn undated(line: &str) -> &str {
    let bytes = line.as_bytes();
    if bytes.len() >= STAMP_LEN
        && is_stamp(&bytes[..STAMP_LEN])
        && bytes.get(STAMP_LEN).is_none_or(|&b| b == b' ')
    {
        &line[(STAMP_LEN + 1).min(line.len())..]
    } else {
        line
    }
}

/// The line a cut log opens with.
fn cut_line(now: u64, cap: u64) -> Vec<u8> {
    let mut said = stamp(now).to_vec();
    said.extend_from_slice(
        format!(" this log reached {cap} bytes and was cut here: the lines before are gone\n")
            .as_bytes(),
    );
    said
}

/// One file that never holds more than its cap for long: the inode the service manager opened
/// for a job's output.
pub(crate) struct Bounded {
    pub(crate) file: File,
    pub(crate) cap: u64,
}

impl Bounded {
    /// Append `line`. Where it would take the file past the cap, the file is first cut to nothing
    /// in place (the inode the service manager opened, which every descriptor of the log names,
    /// so renaming it would leave writers on the old one) and opens with [`cut_line`], dated
    /// `now`, where that and `line` fit. Past a cut the write goes to the file's end, so a
    /// descriptor opened without `O_APPEND` leaves no hole at its old offset.
    ///
    /// # Errors
    /// `InvalidInput` for a line longer than the cap; the file's metadata, cut, seek or write
    /// error. A failed cut writes nothing: never an append past the cap.
    pub(crate) fn write(&mut self, line: &[u8], now: u64) -> io::Result<()> {
        if line.len() as u64 > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log line exceeds cap",
            ));
        }
        let len = self.file.metadata()?.len();
        if len.saturating_add(line.len() as u64) > self.cap {
            self.cut(now, line.len())?;
        }
        self.file.seek(io::SeekFrom::End(0))?;
        self.file.write_all(line)
    }

    /// Cut the file to nothing in place, opening with [`cut_line`] (dated `now`) where it and
    /// `room` more bytes fit.
    fn cut(&mut self, now: u64, room: usize) -> io::Result<()> {
        self.file.set_len(0)?;
        let said = cut_line(now, self.cap);
        if (said.len() + room) as u64 <= self.cap {
            self.file.seek(io::SeekFrom::End(0))?;
            self.file.write_all(&said)?;
        }
        Ok(())
    }

    /// Cut the file where writes other than this one's (standard error written straight, a child
    /// that holds the file itself) took it past the cap. Such a write made between the cut and
    /// its line lands before that line.
    ///
    /// # Errors
    /// The file's metadata or cut error.
    pub(crate) fn bound(&mut self, now: u64) -> io::Result<()> {
        if self.file.metadata()?.len() > self.cap {
            self.cut(now, 0)?;
        }
        Ok(())
    }
}

/// The identity of the regular file at `path` (a link followed, as the service manager's open
/// follows it), `None` for anything else or nothing there.
fn id_of_path(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| (meta.dev(), meta.ino()))
}

/// Keep bounded each of `paths` that is the very file this process's standard output or error is
/// (module docs); every other path is left alone, unopened. A job's verb calls this first, with
/// the files its definition names, and hands what comes back to its serving function. A file it
/// keeps for the first time is cut back at once where it is past the cap. The thread that cuts
/// the logs back takes no signal sent to the process ([`spawn_deaf`]). Calls add to what earlier
/// calls kept. Returns which standard descriptors this call kept.
pub fn keep<P: AsRef<Path>>(paths: &[P]) -> Kept {
    #[cfg(any(test, debug_assertions))]
    seam::asked_now(paths);
    let named: Vec<(u64, u64)> = paths
        .iter()
        .filter_map(|path| id_of_path(path.as_ref()))
        .collect();
    // The descriptors first, before the kept state is held (the dup takes the spawn lock): under
    // the state nothing takes another of this crate's locks or standard error's, so `line`'s
    // order (standard error's lock, then the state) cannot meet its reverse.
    let mut found = Vec::new();
    for (fd, is_stderr) in [(io::stderr().as_fd(), true), (io::stdout().as_fd(), false)] {
        // A descriptor of what is there (a regular file, or anything else), kept out of every
        // child: its identity, and the kept log's own descriptor where it is one of `paths`.
        let Ok(file) = crate::kept_out(|| fd.try_clone_to_owned()).map(File::from) else {
            continue;
        };
        let Some(id) = file
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|meta| (meta.dev(), meta.ino()))
            .filter(|id| named.contains(id))
        else {
            continue;
        };
        found.push((is_stderr, id, file));
    }
    let mut kept = hold();
    let mut keeping = Kept {
        stdout: false,
        stderr: false,
    };
    for (is_stderr, id, file) in found {
        let at = match kept.logs.iter().position(|log| log.id == id) {
            Some(at) => at,
            None => {
                let mut bounded = Bounded { file, cap: CAP };
                // A log an earlier run took past the cap (one that ended, or crashed, before
                // the thread's first cut, as a job its supervisor restarts sooner can) is cut
                // back now, before this run writes to it.
                let _ = bounded.bound(now());
                kept.logs.push(Log { id, bounded });
                kept.logs.len() - 1
            }
        };
        if is_stderr {
            kept.stderr = Stderr::Log(at);
            keeping.stderr = true;
        } else {
            keeping.stdout = true;
        }
    }
    if !kept.logs.is_empty() && !kept.ticking {
        kept.ticking = spawn_deaf("aterm-job-log", bound_forever);
    }
    keeping
}

/// The faults a thread raises on itself: left unblocked on a [`spawn_deaf`] thread, since POSIX
/// leaves one raised while blocked undefined, and none is how a process is asked to stop.
const FAULTS: [libc::c_int; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGFPE,
    libc::SIGILL,
    libc::SIGTRAP,
];

/// The highest signal number of any platform here (Linux's real-time signals end at 64): a number
/// past a platform's range is refused by `sigaddset` and adds nothing (glibc and musl also refuse
/// their internal ones), and Apple's accepts 32, which sets a bit that names no signal.
const LAST_SIGNAL: libc::c_int = 64;

/// Start `run` on a thread named `name` that takes no signal sent to the process: every signal but
/// [`FAULTS`] is blocked on this thread while it starts the new one, which inherits that mask, and
/// this thread's own mask is put back after. A job's later
/// [`take_stop_signals`](crate::process::take_stop_signals) blocks its stop signals only on its
/// own thread and the ones it starts after, so a thread started earlier with them unblocked would
/// take a `SIGTERM` sent to the process and end it by the default action, before the job's
/// orderly stop. Returns whether the thread started.
fn spawn_deaf(name: &str, run: fn()) -> bool {
    let mut deaf = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    let mut before = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: both sets are writable locals, initialised before use and not retained; the mask
    // call changes only this thread's mask and writes the complete old one into `before`.
    let blocked = unsafe {
        libc::sigemptyset(deaf.as_mut_ptr());
        for sig in (1..=LAST_SIGNAL).filter(|sig| !FAULTS.contains(sig)) {
            libc::sigaddset(deaf.as_mut_ptr(), sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, deaf.as_ptr(), before.as_mut_ptr()) == 0
    };
    let started = std::thread::Builder::new()
        .name(name.into())
        .spawn(run)
        .is_ok();
    if blocked {
        // SAFETY: the complete mask `pthread_sigmask` saved on this same thread above.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, before.as_ptr(), std::ptr::null_mut()) };
    }
    started
}

/// The thread that cuts each kept log back past the cap, every [`BOUND_EVERY`], for the life of
/// the process.
fn bound_forever() {
    loop {
        std::thread::sleep(Duration::from_millis(EVERY_MS.load(Ordering::Relaxed)));
        let now = now();
        for log in &mut hold().logs {
            let _ = log.bounded.bound(now);
        }
    }
}

/// One diagnostic, as one dated line: a newline or carriage return in it is a space (a value a
/// client sent cannot start a line of its own), and it is cut to fit [`LINE_CAP`]. Where
/// standard error is a kept log it is written through that log's bound; elsewhere straight to
/// standard error. Either way it is written under standard error's lock, so it never lands inside
/// a line another writer makes through it (an `eprintln!`, a library's line of several writes).
/// A log that cannot take it loses it.
pub fn line(words: fmt::Arguments<'_>) {
    let mut text = fmt::format(words).replace(['\r', '\n'], " ");
    text.truncate(char_floor(&text, BODY_MAX));
    let now = now();
    let mut line = Vec::with_capacity(STAMP_LEN + 2 + text.len());
    line.extend_from_slice(&stamp(now));
    line.push(b' ');
    line.extend_from_slice(text.as_bytes());
    line.push(b'\n');
    // Standard error's lock, then the kept state: nothing that holds the state takes that lock
    // (under it `keep`, the thread and the seam only read and cut files and start the thread;
    // `keep` dups its descriptors before it takes the state), and std's lock is re-entrant, so a
    // thread that already holds it (an `eprintln!`'s caller, a panic hook that writes) takes it
    // again here and the order cannot turn round.
    let mut stderr = io::stderr().lock();
    let mut kept = hold();
    match kept.stderr {
        Stderr::Log(at) => {
            let _ = kept.logs[at].bounded.write(&line, now);
        }
        Stderr::Stream => {
            let _ = stderr.write_all(&line);
        }
    }
}

/// The largest char boundary of `text` at most `most`.
fn char_floor(text: &str, most: usize) -> usize {
    let mut end = most.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// For a test of a job: what its verb handed [`keep`], the outputs its definition names, and a
/// stand-in job (the test's binary run again with its standard output and error on the files, as
/// the service manager opens them).
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use std::cell::RefCell;
    use std::fs::OpenOptions;
    use std::io;
    use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    thread_local! {
        static ASKED: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    }

    pub(super) fn asked_now<P: AsRef<Path>>(paths: &[P]) {
        let paths = paths.iter().map(|p| p.as_ref().to_path_buf()).collect();
        let _ = ASKED.try_with(|asked| *asked.borrow_mut() = Some(paths));
    }

    /// The paths the last [`keep`](super::keep) on this thread was handed, taken; `None` where
    /// none was called since.
    #[must_use]
    pub fn asked() -> Option<Vec<PathBuf>> {
        ASKED
            .try_with(|asked| asked.borrow_mut().take())
            .ok()
            .flatten()
    }

    /// Cut the kept logs back every `every`, not every [`super::BOUND_EVERY`]: a stand-in job's
    /// setting, made before its first [`keep`](super::keep).
    pub fn bound_every(every: Duration) {
        super::EVERY_MS.store(every.as_millis().max(1) as u64, Ordering::Relaxed);
    }

    /// What a job's definition sends its two streams to: a file each, or `None` (launchd's
    /// `/dev/null` where no path is named, systemd's journal, which bounds itself).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Outputs {
        /// `StandardOutPath`, or a unit's `StandardOutput=append:`/`file:`.
        pub stdout: Option<PathBuf>,
        /// `StandardErrorPath`, or a unit's `StandardError=append:`/`file:`.
        pub stderr: Option<PathBuf>,
    }

    impl Outputs {
        /// The files named, each once.
        #[must_use]
        pub fn files(&self) -> Vec<PathBuf> {
            let mut files: Vec<PathBuf> = Vec::new();
            for path in [&self.stdout, &self.stderr].into_iter().flatten() {
                if !files.contains(path) {
                    files.push(path.clone());
                }
            }
            files
        }
    }

    /// The outputs a launchd plist names, its XML entities read back, in any whitespace between
    /// a key and its string.
    #[must_use]
    pub fn plist_outputs(plist: &str) -> Outputs {
        let path = |key: &str| {
            let after = plist.split(&format!("<key>{key}</key>")).nth(1)?;
            let value = after.trim_start().strip_prefix("<string>")?;
            let value = &value[..value.find("</string>")?];
            Some(PathBuf::from(
                value
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&amp;", "&"),
            ))
        };
        Outputs {
            stdout: path("StandardOutPath"),
            stderr: path("StandardErrorPath"),
        }
    }

    /// The outputs a systemd unit names a file for (`append:` or `file:`).
    #[must_use]
    pub fn unit_outputs(unit: &str) -> Outputs {
        let path = |key: &str| {
            unit.lines().find_map(|line| {
                let value = line.trim().strip_prefix(key)?.strip_prefix('=')?;
                let file = value
                    .strip_prefix("append:")
                    .or_else(|| value.strip_prefix("file:"))?;
                Some(PathBuf::from(file))
            })
        };
        Outputs {
            stdout: path("StandardOutput"),
            stderr: path("StandardError"),
        }
    }

    /// How many lines a stand-in job's burst writes: past [`super::CAP`] for any line of 27 bytes
    /// or more, dated or not.
    pub const BURST: usize = 20_000;

    /// The stage a stand-in job runs ([`rerun`]).
    pub const STAGE: &str = "ATERM_JOB_LOG_STAGE";

    /// The scratch folder a stand-in job is handed ([`rerun`]).
    pub const DIR: &str = "ATERM_JOB_LOG_DIR";

    /// Run test `test` (its full name in this test binary) again as a stand-in job, its stage
    /// `stage` and its folder `dir` ([`stage`] reads them there), and wait for it while it makes
    /// progress (a one-minute hang detector).
    ///
    /// # Errors
    /// It could not be started, it stalled, or it did not exit 0 (its first output and status
    /// said).
    pub fn rerun(test: &str, stage: &str, dir: &Path) -> io::Result<()> {
        let mut job = crate::spawnfd::command(std::env::current_exe()?);
        job.args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(STAGE, stage)
            .env(DIR, dir);
        let ran = crate::deadline::output_while_process_progressing(
            &mut job,
            Duration::from_secs(60),
            format!("the stand-in job's {stage} stage"),
        )
        .map_err(io::Error::other)?;
        if ran.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "the stand-in job's {stage} stage: {}: {}{}",
                ran.status,
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr)
            )))
        }
    }

    /// In a stand-in job ([`rerun`]): its stage and its folder.
    #[must_use]
    pub fn stage() -> Option<(String, PathBuf)> {
        let stage = std::env::var(STAGE).ok()?;
        Some((stage, PathBuf::from(std::env::var_os(DIR)?)))
    }

    /// Put `outputs` on this process's standard output and error as the service manager opens
    /// them for a job: each named file opened to append (made where missing), `/dev/null` for a
    /// stream that names none.
    ///
    /// # Errors
    /// A file could not be opened, or put in place.
    pub fn open_as_service_manager(outputs: &Outputs) -> io::Result<()> {
        use std::io::Write as _;
        let _ = io::stdout().flush();
        for (fd, path) in [(1, &outputs.stdout), (2, &outputs.stderr)] {
            let file = match path {
                Some(path) => OpenOptions::new().create(true).append(true).open(path)?,
                None => OpenOptions::new().write(true).open("/dev/null")?,
            };
            put(file.as_fd(), fd)?;
        }
        Ok(())
    }

    /// Put `from` at standard descriptor `at` (`dup2`).
    fn put(from: BorrowedFd<'_>, at: std::os::fd::RawFd) -> io::Result<()> {
        loop {
            // SAFETY: `from` is open for the call; `at` is a standard descriptor of this
            // process, which `dup2` replaces atomically.
            if unsafe { libc::dup2(from.as_raw_fd(), at) } >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// Wait until every one of `files` holds at most [`super::CAP`]; `false` after a minute that
    /// did not see it (a hang detector: the thread that cuts the logs back runs every
    /// [`bound_every`]). Each look holds the kept logs, so it never sees a cut half made.
    #[must_use]
    pub fn until_bounded(files: &[PathBuf]) -> bool {
        let hang = Instant::now() + Duration::from_secs(60);
        loop {
            let held = super::hold();
            let bounded = files
                .iter()
                .all(|file| std::fs::metadata(file).is_ok_and(|m| m.len() <= super::CAP));
            drop(held);
            if bounded {
                return true;
            }
            if Instant::now() >= hang {
                return false;
            }
            crate::deadline::poll_yield();
        }
    }
}

#[cfg(test)]
#[path = "job_log_tests.rs"]
mod tests;
