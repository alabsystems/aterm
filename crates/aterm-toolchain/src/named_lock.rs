// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lock file that names its holder, and a wait on it that names whom it waits for.
//!
//! The holder writes a [`Holder`] record at the file's head (the prefix a [`RecordLock`] leaves
//! readable), stamped with the moment it had the lock, and clears it before it lets go. The record
//! is advisory: one that cannot be written leaves the lock held and its holder unnamed. A taker that finds the lock held answers at once
//! with the holder named ([`Wait::No`]), or blocks on the lock, naming the holder when the wait
//! starts and once a minute while it lasts ([`Wait::Block`]). The holder is a live process whose
//! lock the kernel lets go when it exits, however it ends, so the wait is admission behind it:
//! no deadline, no retry and no signal to anyone (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! LOCK-WAIT). An interrupt ends the wait ([`crate::interrupt`]). Each time it names the holder,
//! the wait reads the holder's process tree, and once that has made no progress for its bound
//! ([`crate::stall`]) it says so, in its line and in its mark, which a ceiling over it may end it
//! on (CW-WAIT-UNBOUNDED). A holder whose work runs in processes outside its tree (a lane run's
//! VM, which launchd parents) names them in its record ([`NamedLock::credit`]), and the wait
//! reads them with its tree. The wait is marked in this user's waits too
//! ([`crate::waits::user_dir`]), so a waiter behind this process reads that it waits in its turn.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::record_lock::RecordLock;
use crate::stall::{Stall, Work};

/// The head of the file the record may fill; the lock's range starts after it.
pub const RECORD: u64 = 4096;

/// How often a wait names the holder again.
pub const SAY_EVERY: Duration = Duration::from_secs(60);

/// How often a wait looks for an interrupt and asks its caller whether to stop. A wait names its
/// holder only once a whole tick has passed without the lock, so a holder on its way out goes
/// unnamed.
pub const TICK: Duration = Duration::from_millis(100);

/// Who holds a lock, as its record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// When it took the lock, unix seconds; 0 when the record does not say.
    pub since: u64,
    /// What it runs (`lab linux proof`); empty when the record does not say.
    pub verb: String,
    /// The checkout it runs from; empty when the record does not say.
    pub checkout: String,
    /// The processes outside its tree its work runs in ([`NamedLock::credit`]: its lane's VM's),
    /// which its waiter reads with its tree; empty when the record names none.
    pub credit: Vec<u32>,
    /// How long its process tree has made no progress, once that reached its waiter's bound
    /// ([`Pace::stalled_after`]): what its waiter read, never part of the record.
    pub stalled: Option<Duration>,
}

impl Holder {
    /// This process, from now, running `verb` from `checkout`; [`take`] stamps [`Holder::since`]
    /// again with the moment it has the lock, however long it waited for it.
    #[must_use]
    pub fn me(verb: &str, checkout: &Path) -> Holder {
        Holder {
            pid: std::process::id(),
            since: now_secs(),
            verb: verb.to_string(),
            checkout: checkout.to_string_lossy().into_owned(),
            credit: Vec::new(),
            stalled: None,
        }
    }

    /// The record: [`Holder::line`], then, where it is credited with processes, a second line,
    /// `credit` and their pids, tab-separated, as many as fit in [`RECORD`] (an older reader
    /// reads the first line alone).
    #[must_use]
    pub fn record(&self) -> String {
        let mut record = self.line();
        if self.credit.is_empty() {
            return record;
        }
        let limit = usize::try_from(RECORD).map_or(0, |r| r.saturating_sub(1));
        let mut credit = String::from("credit");
        for pid in &self.credit {
            let next = format!("\t{pid}");
            if record.len() + credit.len() + next.len() + 1 > limit {
                break;
            }
            credit.push_str(&next);
        }
        if credit.len() > "credit".len() {
            record.push_str(&credit);
            record.push('\n');
        }
        record
    }

    /// The record's first line: pid, since, verb and checkout, tab-separated, cut short of
    /// [`RECORD`] on a character boundary.
    #[must_use]
    pub fn line(&self) -> String {
        let field = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let mut line = format!(
            "{}\t{}\t{}\t{}",
            self.pid,
            self.since,
            field(&self.verb),
            field(&self.checkout)
        );
        let limit = usize::try_from(RECORD).map_or(0, |r| r.saturating_sub(1));
        if line.len() > limit {
            let mut end = limit;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        line.push('\n');
        line
    }

    /// The holder `text` names: a [`Holder::record`], or a bare pid (the trust lane's run lock
    /// wrote only that before it wrote this record). A credit line is read only whole, its
    /// newline written: a record being rewritten in place credits nothing it has not finished.
    #[must_use]
    pub fn parse(text: &str) -> Option<Holder> {
        let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let credit = text
            .split_inclusive('\n')
            .nth(1)
            .and_then(|l| l.strip_suffix('\n')?.strip_prefix("credit\t"))
            .map(|pids| {
                pids.split('\t')
                    .filter(|p| uint(p))
                    .filter_map(|p| p.parse().ok())
                    .filter(|p| *p != 0)
                    .collect()
            })
            .unwrap_or_default();
        let line = text.lines().next()?;
        let mut fields = line.splitn(4, '\t');
        let pid = fields
            .next()
            .map(str::trim)
            .filter(|s| uint(s))?
            .parse()
            .ok()
            .filter(|p| *p != 0)?;
        let since = match fields.next() {
            None => 0,
            Some(s) => uint(s).then(|| s.parse().ok()).flatten()?,
        };
        Some(Holder {
            pid,
            since,
            verb: fields.next().unwrap_or_default().to_string(),
            checkout: fields.next().unwrap_or_default().to_string(),
            credit,
            stalled: None,
        })
    }

    /// `pid N (verb, checkout, since HH:MM)`, the clock read `offset` seconds east of UTC; what
    /// the record does not say is left out. A holder its waiter found stuck adds how long it
    /// has made no progress ([`crate::stall::no_progress`]).
    #[must_use]
    pub fn describe(&self, offset: i64) -> String {
        let mut what: Vec<String> = [&self.verb, &self.checkout]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        if self.since != 0 {
            let day = i64::try_from(self.since)
                .unwrap_or(0)
                .saturating_add(offset)
                .rem_euclid(86_400);
            what.push(format!("since {:02}:{:02}", day / 3600, day % 3600 / 60));
        }
        let named = if what.is_empty() {
            format!("pid {}", self.pid)
        } else {
            format!("pid {} ({})", self.pid, what.join(", "))
        };
        match self.stalled {
            Some(span) => format!("{named}, {}", crate::stall::no_progress(span)),
            None => named,
        }
    }
}

/// The wall clock in unix seconds; 0 before the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The holder `path`'s record names, if it names one.
#[must_use]
pub fn read(path: &Path) -> Option<Holder> {
    let (file, meta) = open_regular_existing(path).ok()?;
    let cap = usize::try_from(meta.len().min(RECORD).saturating_add(1)).unwrap_or(0);
    let mut head = Vec::with_capacity(cap);
    file.take(RECORD).read_to_end(&mut head).ok()?;
    Holder::parse(&String::from_utf8_lossy(&head))
}

/// Whom a taker names: the holder `holder`'s record names while that pid lives (`alive`), its
/// clock `offset_at` the time it took the lock; else a holder that wrote no record (a run of a
/// build older than this record), with the last record's exited pid when there is one.
#[must_use]
pub fn name(
    holder: Option<&Holder>,
    offset_at: impl Fn(u64) -> i64,
    alive: impl Fn(u32) -> bool,
) -> String {
    match holder {
        Some(h) if alive(h.pid) => h.describe(offset_at(h.since)),
        Some(h) => format!(
            "a run that wrote no record (the last record names {}, which has exited)",
            h.describe(offset_at(h.since))
        ),
        None => "a run that wrote no record".to_string(),
    }
}

/// How a blocked wait paces its lines and when it calls its holder stuck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    /// Between two lines naming the holder, after the first.
    pub say_every: Duration,
    /// How long the holder's process tree may make no progress before the wait calls it stuck;
    /// its reads judge the tree busy over [`crate::stall::BUSY_OVER`], or over this where it is
    /// shorter (a test's compressed pace): a tree is never judged over more than it may stall.
    pub stalled_after: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Pace {
            say_every: SAY_EVERY,
            stalled_after: crate::stall::STALLED_AFTER,
        }
    }
}

/// What [`take`] does when another handle holds the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Block until the holder lets go.
    Block,
    /// Answer at once ([`Refused::Held`]).
    No,
}

/// Why [`take`] came back without the lock.
#[derive(Debug)]
pub enum Refused {
    /// Another handle holds it and the taker would not wait: the holder its record names.
    Held(Option<Holder>),
    /// An interrupt, or the taker's `stop`, ended the wait: the holder its record named then.
    Stopped {
        /// The holder its record named.
        holder: Option<Holder>,
        /// The interrupt that ended it ([`crate::interrupt`]); `None` when `stop` did.
        signal: Option<i32>,
    },
    /// The file could not be opened, locked or written.
    Io(io::Error),
}

impl From<io::Error> for Refused {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A held lock whose file names this process; the record is cleared before the lock goes.
#[derive(Debug)]
pub struct NamedLock {
    /// Taken only by the consuming Unix close-only transfer. An ordinary guard
    /// retains its record-clearing and explicit-unlock destruction semantics.
    lock: Option<RecordLock>,
    holder: Holder,
    waited: Duration,
}

impl NamedLock {
    /// Transfer the actual held Unix description without clearing its advisory
    /// holder record or issuing LOCK_UN when this original guard is destroyed.
    /// Each returned guard or explicitly exported duplicate closes only its own
    /// description; the kernel releases the lock after the final copy closes.
    /// The retained holder record may outlive its process and is advisory, never
    /// proof of admission, timing qualification or child-group drain.
    #[cfg(unix)]
    #[must_use]
    pub fn into_close_only(mut self) -> crate::record_lock::CloseOnlyLock {
        self.lock
            .take()
            .expect("a named lock owns its guard until consuming transfer")
            .into_close_only()
    }

    /// [`Self::into_close_only`] for a lock this process took, no copy of whose description it
    /// made: its last guard lets it go by `LOCK_UN` unless a copy was offered out since
    /// ([`crate::record_lock::RecordLock::into_sole_close_only`]).
    #[cfg(unix)]
    #[must_use]
    pub fn into_sole_close_only(mut self) -> crate::record_lock::CloseOnlyLock {
        self.lock
            .take()
            .expect("a named lock owns its guard until consuming transfer")
            .into_sole_close_only()
    }

    /// The record this lock's file holds: its taker, since the moment the taker had the lock.
    #[must_use]
    pub fn holder(&self) -> &Holder {
        &self.holder
    }

    /// How long the taker waited for the lock: zero when it was free.
    #[must_use]
    pub fn waited(&self) -> Duration {
        self.waited
    }

    /// Name `pids` in the record as the processes outside this process's tree its work runs in
    /// (a lane run's VM, which launchd parents), in place of any named before, so a waiter
    /// behind it reads them with its tree ([`Holder::credit`]): this process left out, each pid
    /// once. The record is rewritten in place, its first line unchanged, so a waiter's read
    /// never finds it unnamed.
    ///
    /// # Errors
    /// The record could not be written; the lock is held all the same.
    pub fn credit(&mut self, pids: &[u32]) -> io::Result<()> {
        let me = self.holder.pid;
        let mut credit: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|p| *p != me && *p != 0)
            .collect();
        credit.sort_unstable();
        credit.dedup();
        self.holder.credit = credit;
        let record = self.holder.record();
        let lock = self
            .lock
            .as_mut()
            .expect("a named lock owns its guard until consuming transfer");
        lock.seek(io::SeekFrom::Start(0))?;
        lock.write_all(record.as_bytes())?;
        lock.set_len(u64::try_from(record.len()).unwrap_or(RECORD))
    }
}

impl std::ops::Deref for NamedLock {
    type Target = File;
    fn deref(&self) -> &File {
        self.lock
            .as_ref()
            .expect("a named lock owns its guard until consuming transfer")
    }
}

impl Drop for NamedLock {
    fn drop(&mut self) {
        if let Some(lock) = &self.lock {
            let _ = lock.set_len(0);
        }
    }
}

/// Take `path`'s lock as `me`, making the file (owner-only on Unix) when it is missing.
///
/// Held by another handle: with [`Wait::No`], [`Refused::Held`] at once. With [`Wait::Block`],
/// `say` hears the holder (its record as it reads then) once a [`TICK`] has passed without the
/// lock, and every [`SAY_EVERY`] after with the time waited. SIGINT, SIGTERM or SIGHUP while it
/// waits ([`crate::interrupt::Watch`]), or `stop`, asked every tick, saying `true`, ends the wait
/// ([`Refused::Stopped`]). A stopped wait leaves a thread blocked on the lock with a handle of its
/// own until the holder lets go; the lock it then has goes back at once.
/// While it blocks, the wait is marked where a ceiling over this process's tree reads it and in
/// this user's waits ([`crate::waits::Mark::dirs`]), naming the holder afresh each time `say`
/// does. Each time, it reads the holder's process tree with the processes its record credits it
/// with ([`crate::stall::tree_work`], [`Holder::credit`]): once those have made no progress for
/// [`crate::stall::STALLED_AFTER`], the holder `say` hears says so ([`Holder::stalled`]), and so
/// does the mark ([`crate::waits::Stuck`]). Nobody signals it.
///
/// # Errors
/// [`Refused`]: [`Refused::Io`] when the file cannot be opened or locked, never for its record.
pub fn take(
    path: &Path,
    me: &Holder,
    wait: Wait,
    say: &mut dyn FnMut(Option<&Holder>, Duration),
    stop: &dyn Fn() -> bool,
) -> Result<NamedLock, Refused> {
    take_marked(
        path,
        me,
        wait,
        say,
        stop,
        &crate::waits::Mark::dirs,
        Pace::default(),
        &crate::stall::tree_work,
    )
}

/// [`take`] at `pace`: a test's seam, to say and call its holder stuck sooner.
///
/// # Errors
/// As [`take`].
#[cfg(any(test, debug_assertions))]
pub fn take_paced(
    path: &Path,
    me: &Holder,
    wait: Wait,
    say: &mut dyn FnMut(Option<&Holder>, Duration),
    stop: &dyn Fn() -> bool,
    pace: Pace,
) -> Result<NamedLock, Refused> {
    take_marked(
        path,
        me,
        wait,
        say,
        stop,
        &crate::waits::Mark::dirs,
        pace,
        &crate::stall::tree_work,
    )
}

/// [`take`], its wait marked in each directory `marks` names once it blocks (none: unmarked),
/// paced by `pace`, its holder's tree, with the processes its record credits, read by `work`.
#[expect(
    clippy::too_many_arguments,
    reason = "take's own five, and the mark, pace and tree reader its tests set"
)]
fn take_marked(
    path: &Path,
    me: &Holder,
    wait: Wait,
    say: &mut dyn FnMut(Option<&Holder>, Duration),
    stop: &dyn Fn() -> bool,
    marks: &dyn Fn() -> Vec<std::path::PathBuf>,
    pace: Pace,
    work: &dyn Fn(u32, &[u32]) -> Option<Work>,
) -> Result<NamedLock, Refused> {
    match RecordLock::try_lock(open(path)?, RECORD) {
        Ok(lock) => return Ok(named(lock, me, Duration::ZERO)),
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        Err(std::fs::TryLockError::WouldBlock) => {}
    }
    if wait == Wait::No {
        return Err(Refused::Held(read(path)));
    }
    let watch = crate::interrupt::Watch::enter();
    let file = open(path)?;
    let mut mark = crate::waits::Mark::in_each(&marks(), &waits_for(path, me, read(path).as_ref()));
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("named-lock wait".into())
        .spawn(move || {
            // A lock taken after the waiter stopped goes back by the guard's drop, when the send
            // finds nobody to take it.
            let _ = tx.send(RecordLock::lock(file, RECORD));
        })?;
    let started = Instant::now();
    let mut next = TICK;
    let mut stall = Stall::over(crate::stall::BUSY_OVER.min(pace.stalled_after));
    loop {
        match rx.recv_timeout(TICK) {
            Ok(taken) => return Ok(named(taken?, me, started.elapsed())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("the wait on the lock ended with no answer").into());
            }
        }
        let signal = watch.take();
        if signal.is_some() || stop() {
            return Err(Refused::Stopped {
                holder: read(path),
                signal,
            });
        }
        let waited = started.elapsed();
        if waited >= next {
            let mut holder = read(path);
            if let Some(h) = &mut holder {
                h.stalled = stall.stuck(
                    h.pid,
                    work(h.pid, &h.credit),
                    Instant::now(),
                    pace.stalled_after,
                );
            }
            say(holder.as_ref(), waited);
            if let Some(mark) = &mut mark {
                let stuck = holder.as_ref().and_then(|h| {
                    Some(crate::waits::Stuck {
                        holders: vec![h.pid],
                        span: h.stalled?,
                    })
                });
                mark.say_stuck(&waits_for(path, me, holder.as_ref()), stuck.as_ref());
            }
            next = if next == TICK {
                pace.say_every
            } else {
                next + pace.say_every
            };
        }
    }
}

/// What `me`'s wait on `path` marks ([`crate::waits`]): the holder its record names, with no
/// clock of its own, how long it has held the lock, and how long it has made no progress once
/// that is long enough to say.
fn waits_for(path: &Path, me: &Holder, holder: Option<&Holder>) -> String {
    let lead = if me.verb.is_empty() {
        String::new()
    } else {
        format!("{}: ", me.verb)
    };
    match holder {
        Some(h) => {
            let named = Holder {
                since: 0,
                credit: Vec::new(),
                stalled: None,
                ..h.clone()
            }
            .describe(0);
            let held = if h.since == 0 {
                String::new()
            } else {
                format!(" for {} min", now_secs().saturating_sub(h.since) / 60)
            };
            let stuck = h.stalled.map_or_else(String::new, |span| {
                format!(", and {}", crate::stall::no_progress(span))
            });
            format!(
                "{lead}waiting for {named}, which has held {}{held}{stuck}",
                path.display()
            )
        }
        None => format!(
            "{lead}waiting for a run that wrote no record, which holds {}",
            path.display()
        ),
    }
}

/// `lock`, had after `waited`, its record now `me`'s since this moment. A record that cannot be
/// written (a full disk) is cleared and the lock kept: a waiter then names a run that wrote no
/// record, which this one is.
fn named(mut lock: RecordLock, me: &Holder, waited: Duration) -> NamedLock {
    let holder = Holder {
        since: now_secs(),
        ..me.clone()
    };
    let line = holder.record();
    let written = lock
        .set_len(0)
        .and_then(|()| lock.seek(io::SeekFrom::Start(0)))
        .and_then(|_| lock.write_all(line.as_bytes()));
    if written.is_err() {
        let _ = lock.set_len(0);
    }
    NamedLock {
        lock: Some(lock),
        holder,
        waited,
    }
}

/// Open (creating) `path` for locking and its record, never truncated: a lock file is a
/// rendezvous, and its record is the holder's to clear.
fn open(path: &Path) -> io::Result<File> {
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

fn open_regular_existing(path: &Path) -> io::Result<(File, std::fs::Metadata)> {
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok((file, meta))
}

#[cfg(test)]
#[path = "named_lock_tests.rs"]
mod tests;
