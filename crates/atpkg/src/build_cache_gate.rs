// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHO HOLDS A REGISTERED ROOT'S GATE (HOLDER-RECORD). A gate (`locks.noindex/<key>.lock`) is an
//! `flock` rendezvous, and a producer that finds one held waits only for a running holder on
//! record and is refused otherwise ([`super::producer_gate`]): this module takes each gate and
//! writes, in the gate file itself, who took it, so a producer knows whom it would wait for and
//! its refusal names the holder.
//!
//! The record is one line a holder: its pid, its birth ([`aterm_uds::process::read_birth`]),
//! whether it holds the gate whole or reads beside others, and its command. It is written only
//! under the lock it describes and taken back before that lock is let go: a holder of the whole
//! gate empties the file as it takes it (no other holder can be there, so every line left is a
//! dead one's) and again as it lets go; one that reads beside others adds its line at the file's
//! end in one append, and writes spaces over exactly those bytes as it lets go. Those spaces stay
//! until a holder of the whole gate empties the file, which one about to read beside others does
//! first where it finds [`FULL`] bytes there and nobody holding the gate ([`try_shared`]). The
//! file is the gate, so the record adds no entry to `locks.noindex`, nothing a walk of it could
//! take for a gate or a receipt, and nothing to unlink; it is opened never through a link and
//! closed on exec, as the gate is, and read through the bounded reader.
//!
//! A reader never trusts a line: a holder that was killed leaves its line, and a pid is given
//! again. A line counts as a running holder only while its pid reads the birth it recorded
//! ([`Holders::running`]); every other well-formed line is a holder that has ended
//! ([`Holders::ended`]). A holder may leave no line at all: an atpkg from before this record
//! takes the gate and writes nothing, and so does a holder whose line could not be written (its
//! own birth unread, a gate it may only read, the volume full, or [`FULL`] bytes there already
//! while another holds it). And a holder of this module is off the record for a moment, between
//! its take of the lock and its line and between taking its line back and letting the lock go.
//! So "held, with no running holder on record" is its own answer ([`Holders::words`]), never
//! read as free, and a producer takes it as final only where it reads so again after a pause
//! ([`super::producer_gate`]).

use std::fs::File;
use std::path::Path;

/// The longest gate file read for its holders: a longer one reads as no record.
const MAX: usize = super::MAX_RECORD;

/// The length from which a holder reading beside others adds no line while another holds the
/// gate: the lines of such holders stay, as spaces, until a holder of the whole gate empties the
/// file ([`try_exclusive`], or [`try_shared`] where nobody holds it), so a gate only ever read is
/// bounded here, well below what is read ([`MAX`]).
const FULL: usize = MAX / 2;

/// The longest command a line keeps, in bytes.
const COMMAND: usize = 160;

/// A gate held, its holder on record while it is ([`try_exclusive`], [`try_shared`]). Dropping
/// it takes the record back, then lets the lock go.
pub(super) struct Held {
    mine: Mine,
    lock: crate::lock::Flock,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("mine", &self.mine)
            .finish_non_exhaustive()
    }
}

/// What a [`Held`] wrote in its gate file.
#[derive(Clone, Copy, Debug)]
enum Mine {
    /// Nothing: its line could not be written.
    Unrecorded,
    /// The file's only line: it holds the gate whole.
    Whole,
    /// `len` bytes at `at`, among the lines of other readers.
    Line { at: u64, len: usize },
}

impl Drop for Held {
    fn drop(&mut self) {
        match self.mine {
            Mine::Unrecorded => {}
            Mine::Whole => {
                let _ = self.lock.set_len(0);
            }
            Mine::Line { at, len } => blank(&self.lock, at, len),
        }
    }
}

/// The gate at `path`, which stands: opened to read and to append, or to read alone where this
/// process may not write it (its mode, or a read-only volume), since the lock needs no more and
/// such a holder is left off the record; never through a link, never created, and only where it
/// is a regular file.
pub(super) fn open(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let open = |append: bool| {
        std::fs::OpenOptions::new()
            .read(true)
            .append(append)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    };
    let file = match open(true) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            open(false)?
        }
        opened => opened?,
    };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a registered cache's gate is not a regular file",
        ));
    }
    Ok(file)
}

/// Take `file`'s gate whole without waiting, and put this process on record as its only holder.
/// The lock is what is taken: a record that cannot be written leaves it held and unrecorded.
pub(super) fn try_exclusive(file: File) -> Result<Held, std::fs::TryLockError> {
    use std::os::unix::fs::FileExt as _;
    let lock = crate::lock::Flock::try_lock(file)?;
    // Nobody else holds it, so every line here is of a holder that is gone.
    let recorded = lock.set_len(0).is_ok()
        && line("exclusive").is_some_and(|line| lock.write_all_at(line.as_bytes(), 0).is_ok());
    Ok(Held {
        mine: if recorded {
            Mine::Whole
        } else {
            Mine::Unrecorded
        },
        lock,
    })
}

/// Take `file`'s gate beside other readers, and add this process to its record. Where the record
/// has reached [`FULL`] bytes it is emptied first, if nobody holds the gate ([`empty_if_full`]).
/// A reader never refuses another reader: a hold of the whole gate that writes no record (another
/// reader's emptying of it, or a waiting producer's look, [`free`], each a call or two long) is
/// ridden out, the take tried again every [`SETTLE_TICK`], at least once and then for up to
/// [`SETTLE`]; a running holder of the whole gate on record refuses it at once, and so does any
/// hold once it has tried again and [`SETTLE`] has passed. The lock is what is taken: a line that cannot be added leaves it held and unrecorded.
pub(super) fn try_shared(file: File) -> Result<Held, std::fs::TryLockError> {
    empty_if_full(&file);
    let began = std::time::Instant::now();
    let mut paused = false;
    let lock = loop {
        // Each take locks its own descriptor, which a refusal consumes; all share one open file.
        let attempt = file.try_clone().map_err(std::fs::TryLockError::Error)?;
        match crate::lock::Flock::try_lock_shared(attempt) {
            Ok(lock) => break lock,
            // The first refusal always pauses, so a reader held up before it looked still waits.
            Err(std::fs::TryLockError::WouldBlock)
                if (!paused || began.elapsed() < SETTLE)
                    && !record_of(&file).running.iter().any(|holder| !holder.shared) =>
            {
                paused = true;
                seam::settling();
                std::thread::sleep(SETTLE_TICK);
            }
            Err(refused) => return Err(refused),
        }
    };
    let mine = append(&lock).unwrap_or(Mine::Unrecorded);
    Ok(Held { mine, lock })
}

/// The longest a reader rides out a hold of the whole gate that writes no record ([`try_shared`]).
const SETTLE: std::time::Duration = std::time::Duration::from_millis(100);

/// How often a reader tries the gate again meanwhile.
const SETTLE_TICK: std::time::Duration = std::time::Duration::from_millis(1);

/// Empty the gate `file` where its record has reached [`FULL`] bytes and nobody holds the gate:
/// taken whole without waiting, for that alone, as any holder of the whole gate empties it
/// ([`try_exclusive`]), since every byte there is then a holder's that let go or ended. Where
/// another holds it, or the file may only be read, it stays as it is.
fn empty_if_full(file: &File) {
    if file
        .metadata()
        .ok()
        .and_then(|metadata| usize::try_from(metadata.len()).ok())
        .is_some_and(|len| len >= FULL)
        && let Ok(clone) = file.try_clone()
        && let Ok(whole) = crate::lock::Flock::try_lock(clone)
    {
        let _ = whole.set_len(0);
    }
}

/// Whether the gate at `path` is free now: taken whole without waiting and let go at once, with
/// nothing written ([`super::producer_gate`]'s look at a gate between its pauses, which holds no
/// other lock).
pub(super) fn free(path: &Path) -> bool {
    open(path).is_ok_and(|file| crate::lock::Flock::try_lock(file).is_ok())
}

/// Add this process's line at the end of `file` in one append, which the kernel places whole
/// after every other reader's: only through a descriptor that appends ([`open`]), and only below
/// [`FULL`]. A write cut short is written over with spaces and counts as none.
fn append(file: &File) -> Option<Mine> {
    use std::io::{Seek as _, Write as _};
    if status(file)? & libc::O_APPEND == 0
        || usize::try_from(file.metadata().ok()?.len()).ok()? >= FULL
    {
        return None;
    }
    let line = line("shared")?;
    let mut file = file;
    let len = file.write(line.as_bytes()).ok()?;
    let at = file
        .stream_position()
        .ok()?
        .checked_sub(u64::try_from(len).ok()?)?;
    if len != line.len() {
        blank(file, at, len.saturating_add(1));
        return None;
    }
    Some(Mine::Line { at, len })
}

/// Write spaces over the line of `len` bytes at `at`, keeping the line break that ends it, so
/// every other reader's line stays where it is. The descriptor stops appending first: Linux
/// appends a positioned write through one that does.
fn blank(file: &File, at: u64, len: usize) {
    use std::os::unix::fs::FileExt as _;
    let Some(flags) = status(file) else {
        return;
    };
    if flags & libc::O_APPEND != 0 && !set_status(file, flags & !libc::O_APPEND) {
        return;
    }
    let _ = file.write_all_at(&vec![b' '; len.saturating_sub(1)], at);
}

/// `file`'s status flags (`F_GETFL`).
fn status(file: &File) -> Option<libc::c_int> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `file` is open for the call, and `F_GETFL` takes no argument and touches no memory
    // of this process.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    (flags >= 0).then_some(flags)
}

/// Set `file`'s status flags (`F_SETFL`); whether they were set.
fn set_status(file: &File, flags: libc::c_int) -> bool {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `file` is open for the call, and `F_SETFL` takes one integer and touches no memory
    // of this process.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags) == 0 }
}

/// This process's line, or `None` where its own birth does not read: a line no reader could
/// check is not written. What is read once is kept for this pid alone: a child forked without
/// an exec reads its own.
fn line(mode: &str) -> Option<String> {
    fn me() -> Option<(u32, u64, String)> {
        let pid = std::process::id();
        let birth = aterm_uds::process::read_birth(i32::try_from(pid).ok()?)?;
        Some((pid, birth.0, command(std::env::args_os())))
    }
    static ME: std::sync::OnceLock<Option<(u32, u64, String)>> = std::sync::OnceLock::new();
    let kept = ME.get_or_init(me);
    let fresh;
    let (pid, birth, command) = match kept {
        Some(kept) if kept.0 == std::process::id() => kept,
        _ => {
            fresh = me()?;
            &fresh
        }
    };
    Some(format!(
        "holder pid={pid} birth={birth} mode={mode} command={command}\n"
    ))
}

/// A command line as one line of at most [`COMMAND`] bytes: its program's file name and its
/// arguments, a control character (a line break is one) as `?`.
fn command(mut argv: impl Iterator<Item = std::ffi::OsString>) -> String {
    let program = argv.next().unwrap_or_default();
    let program = Path::new(&program)
        .file_name()
        .map_or_else(|| program.clone(), std::ffi::OsStr::to_os_string);
    let mut text = String::new();
    for word in std::iter::once(program).chain(argv) {
        if !text.is_empty() {
            text.push(' ');
        }
        text.extend(
            word.to_string_lossy()
                .chars()
                .map(|c| if c.is_control() { '?' } else { c }),
        );
        if text.len() > COMMAND {
            break;
        }
    }
    let mut end = text.len().min(COMMAND);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// One holder a gate's record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) pid: i32,
    /// Its birth as it recorded it ([`aterm_uds::process::Birth`]).
    pub(super) birth: u64,
    /// Whether it reads beside other holders, rather than holding the gate whole.
    pub(super) shared: bool,
    pub(super) command: String,
}

impl Holder {
    /// Whether it still runs: its pid reads the birth it recorded.
    fn runs(&self) -> bool {
        aterm_uds::process::read_birth(self.pid) == Some(aterm_uds::process::Birth(self.birth))
    }

    /// Whether it is this process: its pid is this process's, and reads the birth it recorded.
    fn me(&self) -> bool {
        i32::try_from(std::process::id()) == Ok(self.pid) && self.runs()
    }

    fn words(&self) -> String {
        let who = if self.me() {
            format!("this process (pid {})", self.pid)
        } else {
            format!("pid {}", self.pid)
        };
        let reading = if self.shared { ", reading it" } else { "" };
        format!("{who} ({}){reading}", self.command)
    }
}

/// What a gate's record says of its holders, each line checked against the process table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Holders {
    /// The holders on record that run: each pid reads the birth its line recorded.
    pub(super) running: Vec<Holder>,
    /// The holders on record that have ended, or whose pid is another process now.
    pub(super) ended: Vec<Holder>,
}

impl Holders {
    /// Whether a producer waits for this gate ([`super::producer_gate`]): a holder on record
    /// runs, and none of them is this process, which would wait for itself.
    pub(super) fn awaited(&self) -> bool {
        !self.running.is_empty() && !self.running.iter().any(Holder::me)
    }

    /// What a refusal says of a gate found held: its running holders by pid and command; else
    /// that the holder on record has ended, so whoever holds it left no record; else that no
    /// holder is on record, and who leaves none. Each begins `held`.
    pub(super) fn words(&self) -> String {
        let named = |holders: &[Holder]| {
            holders
                .iter()
                .map(Holder::words)
                .collect::<Vec<_>>()
                .join(" and ")
        };
        if !self.running.is_empty() {
            format!("held by {}", named(&self.running))
        } else if !self.ended.is_empty() {
            format!(
                "held, and the holder on record, {}, has ended: no running holder is on record",
                named(&self.ended)
            )
        } else {
            "held, and no holder is on record there (an atpkg from before holders were recorded \
             leaves none, and so does a holder whose line could not be written)"
                .into()
        }
    }
}

/// The work `pid` has done: the CPU time it spent, its own and that of each child it reaped (on
/// macOS `proc_pid_rusage`, in ns; on Linux its `stat`'s `utime`, `stime`, `cutime` and `cstime`,
/// in clock ticks), comparable only with another reading of the same process. `None` where it
/// does not read (the process is gone or refuses it) and on any other platform.
pub(super) fn work(pid: i32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::jobs::cpu_with_reaped_ns(u32::try_from(pid).ok()?)
    }
    #[cfg(target_os = "linux")]
    {
        let stat =
            aterm_uds::process::read_proc_text(Path::new(&format!("/proc/{pid}/stat"))).ok()?;
        // The fields after the command name, which ends at the last `)`: the state, the stat's
        // third field, is at index 0 here, so its fourteenth to seventeenth are at 11 to 14.
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        fields
            .get(11..15)?
            .iter()
            .try_fold(0_u64, |sum, field| sum.checked_add(field.parse().ok()?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// A WAITING PRODUCER'S HANG DETECTOR (GATE-WAIT): since when no running holder on record of the
/// gate it waits for has done any work. Each look ([`Self::look`]) reads the work of every running
/// holder on record ([`work`]) and counts as work done where a holder's grew since the look
/// before, or where the running holders are not the ones the look before found (one took the
/// gate, or one let go). A reading that does not compare (absent now or then) counts as no work:
/// a holder whose work cannot be read is waited for no longer than one that does none.
#[derive(Debug)]
pub(super) struct Watch {
    /// Each running holder on record at the last look, by pid and birth, with its work then.
    seen: Vec<(i32, u64, Option<u64>)>,
    /// When work was last seen done, or the wait began.
    worked: std::time::Instant,
}

impl Watch {
    /// A watch that begins now.
    pub(super) fn new() -> Self {
        Self {
            seen: Vec::new(),
            worked: std::time::Instant::now(),
        }
    }

    /// Look at the work of `holders`' running holders now, each read by `work`.
    pub(super) fn look(&mut self, holders: &Holders, work: &mut dyn FnMut(i32) -> Option<u64>) {
        let mut now: Vec<_> = holders
            .running
            .iter()
            .map(|holder| (holder.pid, holder.birth, work(holder.pid)))
            .collect();
        now.sort_unstable();
        let same =
            |a: &(i32, u64, Option<u64>), b: &(i32, u64, Option<u64>)| (a.0, a.1) == (b.0, b.1);
        let worked = now.len() != self.seen.len()
            || now.iter().zip(&self.seen).any(|(after, before)| {
                !same(after, before)
                    || matches!((before.2, after.2), (Some(before), Some(after)) if after > before)
            });
        if worked {
            self.worked = std::time::Instant::now();
        }
        self.seen = now;
    }

    /// How long no work has been seen done: since the last look that saw some, or the wait began.
    pub(super) fn quiet(&self) -> std::time::Duration {
        self.worked.elapsed()
    }
}

/// The holders on record at the gate `path`, each checked against the process table now. A gate
/// that does not read (it is gone, no regular file, or longer than [`MAX`]) has none on record;
/// one whose length moved while it was read (a holder took or let go of it meanwhile) is read
/// once more.
pub(super) fn holders(path: &Path) -> Holders {
    crate::metadata_io::read_bounded_regular(path, MAX)
        .or_else(|_| crate::metadata_io::read_bounded_regular(path, MAX))
        .map(|bytes| checked(&bytes, &Holder::runs))
        .unwrap_or_default()
}

/// The holders on record in the open gate `file`, read through it as [`holders`] reads a path:
/// none where it is longer than [`MAX`] or does not read.
fn record_of(file: &File) -> Holders {
    use std::os::unix::fs::FileExt as _;
    let mut bytes = vec![0; MAX + 1];
    let mut read = 0;
    while read < bytes.len() {
        let Ok(at) = u64::try_from(read) else {
            return Holders::default();
        };
        match file.read_at(&mut bytes[read..], at) {
            Ok(0) => break,
            Ok(more) => read += more,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Holders::default(),
        }
    }
    if read > MAX {
        return Holders::default();
    }
    checked(&bytes[..read], &Holder::runs)
}

/// The well-formed lines of `bytes`, each once, as running or ended by `runs`. Only a line that
/// ends in a line break is read: one still being written is not.
fn checked(bytes: &[u8], runs: &dyn Fn(&Holder) -> bool) -> Holders {
    let mut found = Holders::default();
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return found;
    };
    for holder in bytes[..end]
        .split(|byte| *byte == b'\n')
        .filter_map(|line| parse(std::str::from_utf8(line).ok()?))
    {
        let into = if runs(&holder) {
            &mut found.running
        } else {
            &mut found.ended
        };
        if !into.contains(&holder) {
            into.push(holder);
        }
    }
    found
}

/// One line as [`line`] writes it, the spaces a line cut short left before it dropped; `None`
/// for spaces alone, or anything else.
fn parse(line: &str) -> Option<Holder> {
    fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    }
    let (pid, rest) = line
        .trim_start_matches(' ')
        .strip_prefix("holder pid=")?
        .split_once(" birth=")?;
    let (birth, rest) = rest.split_once(" mode=")?;
    let (mode, command) = rest.split_once(" command=")?;
    Some(Holder {
        pid: number::<i32>(pid).filter(|pid| *pid > 1)?,
        birth: number(birth)?,
        shared: match mode {
            "exclusive" => false,
            "shared" => true,
            _ => return None,
        },
        command: command.to_owned(),
    })
}

/// THE TESTS' CALLS AS A PRODUCER WAITS FOR A GATE ([`super::producer_gate`]), with the gate's
/// path, and as a reader rides out a hold of the whole gate ([`try_shared`]), each before its
/// pause: where a test lets the holder go.
#[cfg(test)]
#[path = "build_cache_gate_seam_tests.rs"]
pub(super) mod seam;

#[cfg(not(test))]
pub(super) mod seam {
    pub(in super::super) const fn waiting(_: &std::path::Path) {}
    pub(in super::super) const fn settling() {}
}

#[cfg(test)]
#[path = "build_cache_gate_tests.rs"]
mod tests;
