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
mod tests {
    use super::*;

    fn scratch(label: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(&format!("atc-named-{label}-"))
            .tempdir()
            .expect("a scratch dir")
    }

    fn holder(verb: &str) -> Holder {
        Holder::me(verb, Path::new("/checkouts/a"))
    }

    fn quiet(_: Option<&Holder>, _: Duration) {}

    #[test]
    fn a_record_reads_back_and_a_bare_pid_is_a_holder() {
        let h = Holder {
            pid: 42,
            since: 1_792_000_000,
            verb: "lab linux\tproof".into(),
            checkout: "/a b/c".into(),
            credit: Vec::new(),
            stalled: None,
        };
        let back = Holder::parse(&h.line()).expect("a record");
        assert_eq!(back.verb, "lab linux proof", "a tab in a field is a space");
        assert_eq!(
            (back.pid, back.since, back.checkout.as_str()),
            (42, h.since, "/a b/c")
        );
        assert_eq!(
            Holder::parse("4242\n").map(|h| (h.pid, h.since)),
            Some((4242, 0))
        );
        assert_eq!(Holder::parse(""), None);
        assert_eq!(Holder::parse("0\t1\tx\ty"), None, "pid 0 names nobody");
        assert!(
            h.describe(-7 * 3600).ends_with("since 10:46)"),
            "{}",
            h.describe(-7 * 3600)
        );
        assert_eq!(
            Holder::parse("4242").map(|h| h.describe(0)),
            Some("pid 4242".to_string())
        );
    }

    /// A record names the processes its holder is credited with on a second line, which reads
    /// back only whole: a record cut before its newline, as one rewritten in place may be read,
    /// credits nothing, and a record of one line, an older run's, credits nothing either.
    #[test]
    fn a_records_credit_reads_back_only_whole() {
        let h = Holder {
            credit: vec![501, 77_000],
            ..holder("lab linux exec")
        };
        let record = h.record();
        assert_eq!(record.lines().nth(1), Some("credit\t501\t77000"));
        assert_eq!(Holder::parse(&record), Some(h.clone()));
        let torn = record.trim_end_matches('\n');
        assert_eq!(
            Holder::parse(torn).map(|h| h.credit),
            Some(Vec::new()),
            "{torn:?}"
        );
        assert_eq!(Holder::parse(&h.line()).map(|h| h.credit), Some(Vec::new()));
        let many = Holder {
            credit: (1..2000).collect(),
            ..h
        };
        let record = many.record();
        assert!(
            record.len() < usize::try_from(RECORD).expect("fits"),
            "{}",
            record.len()
        );
        let back = Holder::parse(&record).expect("a record").credit;
        assert!(
            !back.is_empty() && many.credit.starts_with(&back),
            "{back:?}"
        );
    }

    #[test]
    fn a_taken_lock_names_its_holder_until_it_goes() {
        let dir = scratch("names");
        let path = dir.path().join("lane.lock");
        let me = holder("lab linux proof");
        let held = take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free");
        assert_eq!(held.waited(), Duration::ZERO, "a free lock is had at once");
        assert_eq!(
            held.holder(),
            &Holder {
                since: held.holder().since,
                ..me.clone()
            }
        );
        assert_eq!(read(&path).as_ref(), Some(held.holder()));
        match take(
            &path,
            &holder("lab linux up"),
            Wait::No,
            &mut quiet,
            &|| false,
        ) {
            Err(Refused::Held(Some(named))) => assert_eq!(&named, held.holder()),
            other => panic!("a held lock must name its holder: {other:?}"),
        }
        drop(held);
        assert_eq!(read(&path), None, "the record goes with the lock");
        drop(take(&path, &me, Wait::No, &mut quiet, &|| false).expect("free again"));
    }

    #[cfg(unix)]
    #[test]
    fn close_only_transfer_keeps_the_original_holder_record_and_native_lock() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = scratch("close-only");
        let path = dir.path().join("lane.lock");
        let original = take(&path, &holder("original"), Wait::No, &mut quiet, &|| false)
            .expect("the original lock");
        let record = std::fs::read(&path).unwrap();
        let identity = original.metadata().unwrap();
        let transferred = original.into_close_only();
        let duplicate = transferred.try_clone().unwrap();
        let exported = duplicate.try_clone_fd().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), record);
        let actual = transferred.metadata().unwrap();
        assert_eq!(
            (actual.dev(), actual.ino()),
            (identity.dev(), identity.ino())
        );
        drop(transferred);
        assert!(matches!(
            take(&path, &holder("contender"), Wait::No, &mut quiet, &|| false),
            Err(Refused::Held(_))
        ));
        drop(duplicate);
        assert!(matches!(
            take(&path, &holder("contender"), Wait::No, &mut quiet, &|| false),
            Err(Refused::Held(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), record);
        drop(exported);
        let next = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the final transferred description to close",
            || match take(&path, &holder("next"), Wait::No, &mut quiet, &|| false) {
                Ok(next) => Some(next),
                Err(Refused::Held(_)) => None,
                Err(why) => panic!("reacquisition failed: {why:?}"),
            },
        );
        assert_eq!(read(&path).as_ref(), Some(next.holder()));
        assert_eq!(next.holder().verb, "next");
        drop(next);
        assert_eq!(read(&path), None, "ordinary record clearing remains intact");
    }

    /// The waiter names the holder once it has waited a tick, then has the lock as soon as the
    /// holder lets go, and its record says since when it has it, not since it asked: the waiter
    /// asks as of second 1, so a record that kept the asking time would say so. Negative control:
    /// a waiter told not to wait answers Held at once.
    #[test]
    fn a_waiter_names_the_holder_and_takes_the_lock_when_it_goes() {
        let dir = scratch("waits");
        let path = dir.path().join("lane.lock");
        let first = holder("lab linux proof");
        let held = take(&path, &first, Wait::No, &mut quiet, &|| false).expect("free");
        let first = held.holder().clone();
        assert!(matches!(
            take(&path, &holder("w"), Wait::No, &mut quiet, &|| false),
            Err(Refused::Held(_))
        ));
        let (go, gone) = mpsc::channel::<()>();
        let letting_go = std::thread::spawn(move || {
            // The one-minute hang detector: the waiter says whom it waits for well before.
            gone.recv_timeout(Duration::from_secs(60))
                .expect("the waiter named its holder");
            let let_go = now_secs();
            drop(held);
            let_go
        });
        let mut heard = Vec::new();
        let started = Instant::now();
        let second = Holder {
            since: 1,
            ..holder("lab linux exec")
        };
        let taken = take(
            &path,
            &second,
            Wait::Block,
            &mut |h, _| {
                heard.push(h.cloned());
                let _ = go.send(());
            },
            &|| started.elapsed() > Duration::from_secs(60),
        )
        .expect("the lock, once its holder let go");
        let let_go = letting_go.join().expect("the holder's thread");
        assert_eq!(heard.first(), Some(&Some(first)), "{heard:?}");
        assert!(taken.waited() >= TICK, "{:?}", taken.waited());
        let record = read(&path).expect("the waiter's record");
        assert_eq!(&record, taken.holder());
        assert_eq!(
            (record.pid, record.verb.as_str(), record.checkout.as_str()),
            (second.pid, "lab linux exec", "/checkouts/a")
        );
        assert!(
            record.since >= let_go,
            "since {} is before the lock was let go at {let_go}",
            record.since
        );
        drop(taken);
    }

    /// A waiter whose caller says stop comes back without the lock, naming the holder.
    #[test]
    fn a_stopped_wait_names_the_holder() {
        let dir = scratch("stops");
        let path = dir.path().join("lane.lock");
        let held = take(
            &path,
            &holder("ship trust run"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        match take(&path, &holder("w"), Wait::Block, &mut quiet, &|| true) {
            Err(Refused::Stopped {
                holder: Some(named),
                signal: None,
            }) => assert_eq!(&named, held.holder()),
            other => panic!("a stopped wait must name its holder: {other:?}"),
        }
        drop(held);
    }

    /// Set, the test binary is [`standin_waits_for_the_lock`], on the lock at this path.
    #[cfg(unix)]
    const STANDIN: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_STANDIN";

    /// Run by [`an_interrupt_ends_a_wait_for_a_held_lock`] in a process of its own: waits for the
    /// lock [`STANDIN`] names, with SIGINT and SIGTERM at their default actions as a foreground
    /// process has them (set here, not inherited from the runner), writing `waiting` beside it
    /// once it waits, and exits 3, a tool's NOT RUN, naming what
    /// ended the wait and the holder.
    #[cfg(unix)]
    #[test]
    fn standin_waits_for_the_lock() {
        let Some(path) = std::env::var_os(STANDIN).map(std::path::PathBuf::from) else {
            return;
        };
        // SAFETY: SIG_DFL is a disposition, no handler of ours.
        unsafe {
            // SIGTERM at its default action too, whatever this test binary was started with: a
            // watch leaves an ignored one ignored, and the SIGTERM leg sends it.
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        let waiting = path.with_file_name("waiting");
        let started = Instant::now();
        let got = take(
            &path,
            &holder("lab linux exec"),
            Wait::Block,
            &mut |_, _| {
                let _ = std::fs::write(&waiting, std::process::id().to_string());
            },
            &|| started.elapsed() > Duration::from_secs(60),
        );
        match got {
            Err(Refused::Stopped {
                holder: Some(h),
                signal: Some(sig),
            }) => {
                println!(
                    "stopped by {} waiting for {}",
                    crate::interrupt::name(sig),
                    h.describe(0)
                );
                std::process::exit(3);
            }
            other => panic!("the wait ended otherwise: {other:?}"),
        }
    }

    /// A stand-in, killed when it is dropped still running (a test that failed before it
    /// ended), so none outlives its test.
    #[cfg(unix)]
    struct Standin(aterm_uds::spawnfd::Child);

    #[cfg(unix)]
    impl Drop for Standin {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// A process waiting for a held lock ends on SIGTERM, and on SIGINT where it is not
    /// ignored, well inside the one-minute hang detector (the time is reported), naming the
    /// holder and the signal, with nobody signalling the holder, which keeps the lock. Without
    /// the wait's watch its caller's `stop` never says so, and the stand-in dies of the signal
    /// instead (no exit 3).
    #[cfg(unix)]
    #[test]
    fn an_interrupt_ends_a_wait_for_a_held_lock() {
        for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
            let dir = scratch("interrupt");
            let path = dir.path().join("lane.lock");
            let held = take(
                &path,
                &holder("lab linux proof"),
                Wait::No,
                &mut quiet,
                &|| false,
            )
            .expect("free");
            let mut child = Standin(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "named_lock::tests::standin_waits_for_the_lock",
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(STANDIN, &path)
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the stand-in"),
            );
            let waiting = dir.path().join("waiting");
            let pid: i32 = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the stand-in to wait for the lock",
                || std::fs::read_to_string(&waiting).ok()?.trim().parse().ok(),
            );
            let sent = Instant::now();
            // SAFETY: `kill` takes two integers; the pid is this test's own child.
            assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
            let status = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the stand-in's end",
                || child.0.try_wait().ok().flatten(),
            );
            let took = sent.elapsed();
            let mut out = String::new();
            if let Some(mut o) = child.0.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut o, &mut out);
            }
            assert_eq!(status.code(), Some(3), "{word}: {status} {out}");
            assert!(
                out.contains(&format!(
                    "stopped by {word} waiting for pid {}",
                    held.holder().pid
                )),
                "{out}"
            );
            eprintln!("{word} ended the lock wait in {took:?}");
            assert_eq!(
                read(&path).as_ref(),
                Some(held.holder()),
                "the holder keeps it"
            );
            drop(held);
        }
    }

    /// Set, the test binary is [`standin_waits_in_the_chain`], on the lock at this path.
    #[cfg(unix)]
    const LINK: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_LINK";

    /// Run by [`a_holder_waiting_in_a_chain_is_as_stuck_as_its_head`] as a link of a chain: waits
    /// for the lock [`LINK`] names at a test's pace (its holder named every quarter of a second,
    /// judged busy over a second, and stuck after one), writing
    /// its pid to `waiting` beside it once it waits, for at most the hang detector.
    #[cfg(unix)]
    #[test]
    fn standin_waits_in_the_chain() {
        let Some(path) = std::env::var_os(LINK).map(std::path::PathBuf::from) else {
            return;
        };
        let waiting = path.with_file_name("waiting");
        let started = Instant::now();
        let _ = take_paced(
            &path,
            &holder("lab linux exec"),
            Wait::Block,
            &mut |_, _| {
                let _ = std::fs::write(&waiting, std::process::id().to_string());
            },
            &|| started.elapsed() > aterm_spec::wait::MUST_HAPPEN,
            Pace {
                say_every: Duration::from_millis(250),
                stalled_after: Duration::from_secs(1),
            },
        );
    }

    /// Set, the test binary is [`standin_heads_the_chain`], its work going on while the file this
    /// names is there.
    #[cfg(unix)]
    const HEAD: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_HEAD";

    /// Set with [`HEAD`], the stand-in is the head's working child.
    #[cfg(unix)]
    const HEAD_WORKS: &str = "ATERM_TOOLCHAIN_NAMED_LOCK_HEAD_WORKS";

    /// The head [`standin_heads_the_chain`] is, or with [`HEAD_WORKS`] its working child.
    #[cfg(unix)]
    fn head(works: &Path, child: bool) -> Standin {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        if child {
            cmd.env(HEAD_WORKS, "1");
        }
        Standin(
            cmd.args([
                "--exact",
                "named_lock::tests::standin_heads_the_chain",
                "--test-threads=1",
            ])
            .env(HEAD, works)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the head"),
        )
    }

    /// Run by [`a_holder_waiting_in_a_chain_is_as_stuck_as_its_head`] as the head of a chain: its
    /// child keeps a core busy while the file [`HEAD`] names is there and the head lives; once
    /// that child ends, the head does nothing, for twice the hang detector at most.
    #[cfg(unix)]
    #[test]
    fn standin_heads_the_chain() {
        let Some(works) = std::env::var_os(HEAD).map(std::path::PathBuf::from) else {
            return;
        };
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN * 2;
        if std::env::var_os(HEAD_WORKS).is_some() {
            let parent = std::os::unix::process::parent_id();
            let mut n = 0u64;
            while works.exists()
                && std::os::unix::process::parent_id() == parent
                && Instant::now() < until
            {
                for i in 0..1_000_000u64 {
                    n = std::hint::black_box(n.wrapping_mul(31).wrapping_add(i));
                }
            }
            return;
        }
        let mut child = head(&works, true);
        let _ = child.0.wait();
        std::thread::sleep(until.saturating_duration_since(Instant::now()));
    }

    /// A HOLDER THAT WAITS IN ITS TURN IS AS STUCK AS WHAT IT WAITS BEHIND (CW-WAIT-UNBOUNDED): a
    /// link waits, through the real wait and its mark in this user's waits, for a lock whose
    /// record names a head whose child works while a file is there. While that child works, the
    /// link's tree reads as waiting behind it and is never stuck, though it does nothing of its
    /// own for thirty minutes of the stall's clock (its own CPU time, a lock wait's looks and its
    /// sweeps, is reported and is no busy process's; the link names its holder every quarter of
    /// a second where a wait names it every minute, so its reads are judged on the stall's
    /// clock, each sweep standing for a minute's, as each read does); once the file goes, the
    /// child ends, the head idles, and once the link's own waiter calls it stuck, the link's tree
    /// reads stuck behind it for as long as the link's mark says. NEGATIVE CONTROL: the same reads
    /// with the mark left out call the link stuck at the bound while its head still works.
    #[cfg(unix)]
    #[test]
    fn a_holder_waiting_in_a_chain_is_as_stuck_as_its_head() {
        use crate::stall::{STALLED_AFTER, Stall, Waits, busy};
        if aterm_sysprobe::processes().is_none() || crate::waits::user_dir().is_none() {
            eprintln!("no process sweep or no home here: nothing to read");
            return;
        }
        let dir = scratch("chain");
        let path = dir.path().join("lane.lock");
        let works = dir.path().join("head-works");
        std::fs::write(&works, "").expect("the head's work");
        let head = head(&works, false);
        let held = take(
            &path,
            &Holder {
                pid: head.0.id(),
                ..holder("lab linux proof")
            },
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        let link = Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "named_lock::tests::standin_waits_in_the_chain",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(LINK, &path)
                .env(
                    crate::waits::TEST_USER_VAR,
                    crate::waits::user_dir().expect("a waits directory"),
                )
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the link"),
        );
        let pid: u32 = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the link to wait for the lock",
            || {
                std::fs::read_to_string(dir.path().join("waiting"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            },
        );
        let t0 = Instant::now();
        let minute = |m: u32| t0 + Duration::from_secs(60) * m;
        let (mut stall, mut unmarked) = (Stall::default(), Stall::default());
        let read = || Work {
            at: None,
            ..crate::stall::tree_work(pid, &[]).expect("the link is swept")
        };
        let first = read();
        let real = Instant::now();
        let mut last = first;
        for m in 0..=30 {
            last = read();
            assert_eq!(last.waits, Waits::Behind, "minute {m}: {last:?}");
            assert_eq!(stall.read(pid, Some(last), minute(m)), Some(Duration::ZERO));
            let alone = Work {
                waits: Waits::No,
                ..last
            };
            let span = unmarked.stuck(pid, Some(alone), minute(m), STALLED_AFTER);
            assert_eq!(span.is_some(), m >= 15, "minute {m} unmarked: {span:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        let (ran, over) = (last.cpu_ns.saturating_sub(first.cpu_ns), real.elapsed());
        eprintln!("the link's own CPU time while it waited: {ran} ns in {over:?}");
        assert!(
            !busy(ran, over),
            "a lock wait's looks read busy: {ran} ns in {over:?}"
        );
        std::fs::remove_file(&works).expect("the head idles");
        let (behind, span) = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the link's waiter to call its head stuck",
            || {
                let now = read();
                match now.waits {
                    Waits::Stuck(span) => Some((now, span)),
                    Waits::No | Waits::Behind => None,
                }
            },
        );
        assert!(span >= Duration::from_secs(1), "{span:?}");
        assert_eq!(stall.read(pid, Some(behind), minute(30)), Some(span));
        // The lock let go, the link has it and ends, its marks gone with its wait.
        drop(held);
        let mut link = link;
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the link's end", || {
            link.0.try_wait().ok().flatten()
        });
        assert!(
            !crate::waits::user_waiting().iter().any(|w| w.pid == pid),
            "the link's mark outlived its wait"
        );
        drop(head);
    }

    /// A blocked wait is marked for a ceiling to read, naming the holder, and the mark goes once
    /// the waiter has the lock. Without the mark the look for it hangs past its detector.
    #[test]
    fn a_blocked_wait_is_marked_until_it_has_the_lock() {
        let dir = scratch("marked");
        let path = dir.path().join("lane.lock");
        let marks = dir.path().join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let held = take(
            &path,
            &holder("lab linux proof"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        let pid = held.holder().pid;
        let waiter = {
            let (path, marks) = (path.clone(), marks.clone());
            std::thread::spawn(move || {
                let started = Instant::now();
                take_marked(
                    &path,
                    &holder("lab linux exec"),
                    Wait::Block,
                    &mut quiet,
                    &|| started.elapsed() > Duration::from_secs(60),
                    &|| vec![marks.clone()],
                    Pace::default(),
                    &crate::stall::tree_work,
                )
                .map(drop)
            })
        };
        let detector = Instant::now() + Duration::from_secs(60);
        let seen = loop {
            if let Some(w) = crate::waits::waiting(&marks).pop() {
                break w;
            }
            assert!(Instant::now() < detector, "the wait was never marked");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            seen.text.starts_with(&format!(
                "lab linux exec: waiting for pid {pid} (lab linux proof, /checkouts/a), which has held"
            )),
            "{}",
            seen.text
        );
        drop(held);
        assert!(waiter.join().expect("the waiter").is_ok());
        assert!(
            crate::waits::waiting(&marks).is_empty(),
            "the mark outlived the wait"
        );
    }

    /// A holder whose process tree does no work is called stuck once it has done none for the
    /// wait's bound: the holder `say` hears names how long, and the mark says so for a ceiling,
    /// naming its pid; the wait goes on, and the holder keeps its lock. Each read is handed the
    /// processes the holder's record credits it with ([`NamedLock::credit`]), this process left
    /// out. Negative control: a tree that runs a core between reads is never called stuck.
    #[test]
    fn a_holder_that_makes_no_progress_is_named_stuck() {
        use std::cell::RefCell;
        let dir = scratch("stuck");
        let path = dir.path().join("lane.lock");
        let marks = dir.path().join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let mut held = take(
            &path,
            &holder("lab linux proof"),
            Wait::No,
            &mut quiet,
            &|| false,
        )
        .expect("free");
        let me = held.holder().pid;
        held.credit(&[77_000, me, 501, 77_000]).expect("credited");
        assert_eq!(read(&path).map(|h| h.credit), Some(vec![501, 77_000]));
        let pace = Pace {
            say_every: Duration::from_millis(100),
            stalled_after: Duration::from_millis(500),
        };
        let idle = Work {
            cpu_ns: 7,
            members: 1,
            ..Work::default()
        };
        for moves in [false, true] {
            let handed = RefCell::new(Vec::new());
            let ran = Instant::now();
            let work = |_: u32, credited: &[u32]| {
                handed.borrow_mut().push(credited.to_vec());
                Some(if moves {
                    // A core since the wait began.
                    let core = u64::try_from(ran.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    Work {
                        cpu_ns: core,
                        runnable_ns: core,
                        ..idle
                    }
                } else {
                    idle
                })
            };
            let lines = RefCell::new(Vec::new());
            let marked = RefCell::new(Vec::new());
            let started = Instant::now();
            let got = take_marked(
                &path,
                &holder("lab linux exec"),
                Wait::Block,
                &mut |h, _| {
                    lines
                        .borrow_mut()
                        .push(h.map(|h| h.describe(0)).unwrap_or_default());
                    marked
                        .borrow_mut()
                        .extend(crate::waits::waiting(&marks).pop());
                },
                // Fifteen lines, a tenth of a second apart: three times the bound.
                &|| lines.borrow().len() >= 15 || started.elapsed() > Duration::from_secs(60),
                &|| vec![marks.clone()],
                pace,
                &work,
            );
            assert!(matches!(got, Err(Refused::Stopped { .. })), "{got:?}");
            let handed = handed.into_inner();
            assert!(
                !handed.is_empty() && handed.iter().all(|c| c == &[501, 77_000]),
                "{handed:?}"
            );
            let (lines, marked) = (lines.into_inner(), marked.into_inner());
            let said = |l: &String| l.contains(", which has made no progress for ");
            let stuck: Vec<_> = marked.iter().filter_map(|w| w.stuck.clone()).collect();
            if moves {
                assert!(!lines.iter().any(said), "{lines:#?}");
                assert!(stuck.is_empty(), "{marked:#?}");
            } else {
                assert!(!said(&lines[0]), "stuck at its first read: {lines:#?}");
                assert!(lines.last().is_some_and(said), "{lines:#?}");
                let last = stuck.last().expect("the mark said it was stuck");
                assert_eq!(last.holders, [held.holder().pid]);
                assert!(last.span >= pace.stalled_after, "{last:?}");
                assert!(
                    marked
                        .last()
                        .is_some_and(|w| w.text.contains(", and which has made no progress")),
                    "{marked:#?}"
                );
            }
            assert_eq!(
                read(&path).as_ref(),
                Some(held.holder()),
                "the holder keeps it"
            );
        }
    }

    #[test]
    fn a_dead_pids_record_is_not_its_holder() {
        let h = holder("lab linux proof");
        let live = name(Some(&h), |_| 0, |_| true);
        assert!(live.starts_with(&format!("pid {}", h.pid)), "{live}");
        let stale = name(Some(&h), |_| 0, |_| false);
        assert!(stale.starts_with("a run that wrote no record"), "{stale}");
        assert!(stale.contains(&format!("pid {}", h.pid)), "{stale}");
        assert!(!name(None, |_| 0, |_| true).contains("pid"));
        #[cfg(unix)]
        {
            let dir = scratch("nofollow");
            let real = dir.path().join("real.lock");
            std::fs::write(&real, h.line()).unwrap();
            let link = dir.path().join("link.lock");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert_eq!(read(&link), None, "read must not follow a symlink");
            assert!(
                matches!(
                    take(&link, &h, Wait::No, &mut quiet, &|| false),
                    Err(Refused::Io(_))
                ),
                "take must refuse a symlinked lock path"
            );
            let fifo = dir.path().join("fifo.lock");
            let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
            assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
            assert_eq!(read(&fifo), None, "read must not block on a FIFO");
            assert!(
                matches!(
                    take(&fifo, &h, Wait::No, &mut quiet, &|| false),
                    Err(Refused::Io(_))
                ),
                "take must refuse a FIFO lock path"
            );
        }
    }
}
