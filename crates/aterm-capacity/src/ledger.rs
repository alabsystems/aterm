// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LEDGER: one directory of claims per user. A claim is a file its holder keeps locked
//! ([`RecordLock`]) for as long as it lives, and its record says who holds it and for what:
//! `<start>-<pid>-<seq>.wait` while it waits, `.claim` once admitted. The kernel lets the lock
//! go when the holder dies, however it dies, so a file whose lock can be taken is a dead
//! holder's and counts for nothing. `ledger.lock` orders the admissions: a waiter holds it only
//! while it reads the ledger and decides.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_json::{Map, Value};
use aterm_toolchain::record_lock::RecordLock;

use crate::{Need, Patience};

/// The most of a claim file any reader takes, and where its lock's range starts on Windows: the record lies before it.
pub const RECORD_MAX: u64 = 16 * 1024;

/// The most bytes of `what`, `checkout` or the volume a record keeps.
const TEXT_MAX: usize = 512;

// A record's three texts at their longest, each character escaped to JSON's six bytes, and its
// figures, admission line and measured writes, still lie before RECORD_MAX: no live claim reads
// back cut short.
const _: () = assert!(3 * 6 * TEXT_MAX + 1024 + WROTE_LINE <= RECORD_MAX as usize);

/// A waiting claim's suffix.
const WAIT: &str = ".wait";
/// An admitted claim's suffix.
const CLAIM: &str = ".claim";
/// The suffix of a claim file being laid: no reader looks at it.
const TAKING: &str = ".taking";
/// The admissions' lock.
const LOCK: &str = "ledger.lock";

/// The length of the line a holder's measured writes take ([`Record::wrote`]): the figure padded to `u64`'s widest.
const WROTE_LINE: usize = "{\"wrote\":}\n".len() + 20;

/// The ledger's directory under the per-user cache: `~/Library/Caches/aterm/capacity` on macOS, `~/.cache/aterm/capacity` on Linux, `%LOCALAPPDATA%\aterm\capacity` on Windows; `None` with no home. On Unix `~` is the account database's home, whatever `$HOME` or `$XDG_CACHE_HOME` says, so a tool run under a scratch home (the lab, the release buildplan, verify's smoke stages) claims in the one ledger every session of this user reads.
#[must_use]
pub fn user_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if cfg!(windows) {
        return var("LOCALAPPDATA").map(|d| PathBuf::from(d).join("aterm").join("capacity"));
    }
    let home = crate::account_home().or_else(|| var("HOME").map(PathBuf::from))?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/aterm/capacity")
    } else {
        home.join(".cache/aterm/capacity")
    })
}

/// An admitted claim's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    /// When it was admitted, unix ms.
    pub admitted_ms: u64,
    /// The cores it was granted.
    pub cores: u32,
    /// Whether its memory was admitted ALONE ([`crate::Short::Alone`]): while it is held, no
    /// other memory claim is admitted.
    pub alone: bool,
}

/// One claim's record: who holds it, for what, and whether it is admitted.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The holder's pid.
    pub pid: u32,
    /// When the claim was made, unix ms: waiters are served in this order.
    pub start_ms: u64,
    /// The holder's own count of its claims, which orders two made in one millisecond.
    pub seq: u64,
    /// The checkout the holder runs for: its need's ([`Need::checkout`]), else its working directory's enclosing git checkout.
    pub checkout: String,
    /// What it needs; its `path` is the mount of the volume its disk need is on.
    pub need: Need,
    /// Its grant, once admitted.
    pub held: Option<Held>,
    /// Processes outside its holder's tree whose use is its own ([`crate::Claim::credit`]): a
    /// VM's, which the system parents to launchd, not to the run that booted it. Their trees'
    /// use is credited against this claim alone, before its holder's.
    pub credit: Vec<u32>,
    /// What its holder has measured itself writing toward its disk need since it was admitted
    /// ([`crate::Claim::wrote`]): the free space a waiter reads has already lost it, so its
    /// disk reservation is the need less this. `None` where its holder measures nothing.
    pub wrote: Option<u64>,
    /// When a quiet claim was first next for the quiet machine, unix ms: short of the load with
    /// no other quiet claim held or waiting ahead of it. Its [`Patience`] counts from here.
    pub next_ms: Option<u64>,
}

impl Record {
    /// Its place in the queue: earlier is served first.
    #[must_use]
    pub const fn order(&self) -> (u64, u32, u64) {
        (self.start_ms, self.pid, self.seq)
    }

    /// The file stem every file of this claim carries.
    fn stem(&self) -> String {
        format!("{:013}-{}-{}", self.start_ms, self.pid, self.seq)
    }

    /// The record's first line: who and what.
    fn head_line(&self) -> String {
        let mut o = Map::new();
        o.insert("v".to_owned(), Value::from(1u64));
        o.insert("pid".to_owned(), Value::from(self.pid));
        o.insert("start_ms".to_owned(), Value::from(self.start_ms));
        o.insert("seq".to_owned(), Value::from(self.seq));
        o.insert("checkout".to_owned(), Value::from(cut(&self.checkout)));
        o.insert("what".to_owned(), Value::from(cut(&self.need.what)));
        o.insert("mem".to_owned(), Value::from(self.need.mem));
        o.insert("margin".to_owned(), Value::from(self.need.margin));
        o.insert("cores".to_owned(), Value::from(self.need.cores));
        o.insert("disk".to_owned(), Value::from(self.need.disk));
        let quiet = self.need.quiet.map(|q| {
            if q.is_nan() || q < 0.0 {
                0.0
            } else if q.is_infinite() {
                f64::MAX
            } else {
                q
            }
        });
        o.insert("quiet".to_owned(), Value::from(quiet));
        if self.need.alone {
            o.insert("alone".to_owned(), Value::from(true));
        }
        if let Some(p) = self.need.patience {
            let mut q = Map::new();
            q.insert("reserve_ms".to_owned(), Value::from(millis(p.reserve)));
            q.insert("start_ms".to_owned(), Value::from(millis(p.start)));
            q.insert("hold_ms".to_owned(), Value::from(millis(p.hold)));
            o.insert("patience".to_owned(), Value::Object(q));
        }
        o.insert(
            "volume".to_owned(),
            Value::from(self.need.path.as_deref().and_then(Path::to_str).map(cut)),
        );
        line_of(o)
    }

    /// The line an admission adds.
    fn held_line(held: Held) -> String {
        let mut o = Map::new();
        o.insert("admitted_ms".to_owned(), Value::from(held.admitted_ms));
        o.insert("cores".to_owned(), Value::from(held.cores));
        if held.alone {
            o.insert("alone".to_owned(), Value::from(true));
        }
        line_of(o)
    }

    /// The line a quiet waiter adds when it is first next for the quiet machine ([`Self::next_ms`]).
    pub(crate) fn next_line(ms: u64) -> String {
        let mut o = Map::new();
        o.insert("next_ms".to_owned(), Value::from(ms));
        line_of(o)
    }

    /// How much longer, at most, this quiet claim holds back claimed work that asked after it, at
    /// `now` (unix ms): once next for its [`Patience::reserve`], while it waits, and for its
    /// [`Patience::hold`] after it starts, which it does by its [`Patience::start`]. `None` when
    /// it reserves nothing now: no patience, never next, not next long enough, admitted before
    /// it reserved, or its reservation spent.
    #[must_use]
    pub fn reserves(&self, now: u64) -> Option<Duration> {
        let p = self.need.patience?;
        self.need.quiet?;
        let next = self.next_ms?;
        let from = next.saturating_add(millis(p.reserve));
        let bound = next.saturating_add(millis(p.start));
        let started = match self.held {
            Some(h) if h.admitted_ms < from => return None,
            Some(h) => h.admitted_ms.min(bound),
            None if now < from => return None,
            None => bound,
        };
        let end = started.saturating_add(millis(p.hold));
        (now < end).then(|| Duration::from_millis(end - now))
    }

    /// The line a credit adds ([`crate::Claim::credit`]): the latest one read whole stands.
    pub(crate) fn credit_line(pids: &[u32]) -> String {
        let mut o = Map::new();
        o.insert(
            "credit".to_owned(),
            Value::Array(pids.iter().map(|p| Value::from(*p)).collect()),
        );
        line_of(o)
    }

    /// The line its holder's measured writes take ([`Self::wrote`]), always [`WROTE_LINE`] bytes, so
    /// a later figure is written over it in place and a reader never finds the record cut short.
    fn wrote_line(bytes: u64) -> String {
        let line = format!("{{\"wrote\":{bytes:<20}}}\n");
        debug_assert_eq!(line.len(), WROTE_LINE);
        line
    }

    /// A record read back from a claim file's text; `held` says the file is admitted (`.claim`).
    #[must_use]
    pub fn decode(text: &str, held: bool) -> Option<Record> {
        let mut lines = text.lines();
        let head: Value = aterm_json::from_str(lines.next()?).ok()?;
        if head.get("v").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
        let start_ms = num(&head, "start_ms")?;
        let patience = match head.get("patience") {
            None | Some(Value::Null) => None,
            Some(p) => Some(Patience {
                reserve: Duration::from_millis(num(p, "reserve_ms")?),
                start: Duration::from_millis(num(p, "start_ms")?),
                hold: Duration::from_millis(num(p, "hold_ms")?),
            }),
        };
        let quiet = match head.get("quiet") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_f64().filter(|q| q.is_finite() && *q >= 0.0)?),
        };
        let need = Need {
            what: head.get("what").and_then(Value::as_str)?.to_owned(),
            mem: num(&head, "mem")?,
            margin: num(&head, "margin").unwrap_or(0),
            cores: u32::try_from(num(&head, "cores")?).ok()?,
            disk: num(&head, "disk")?,
            quiet,
            patience,
            path: head
                .get("volume")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            checkout: None,
            alone: head.get("alone").and_then(Value::as_bool) == Some(true),
        };
        // The lines after the head, in the order they were added: a quiet waiter's `next_ms`,
        // the admission, then the measured writes' (written over in place) and each credit,
        // whichever came first.
        let after: Vec<Value> = lines
            .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
            .collect();
        let held = held.then(|| {
            after
                .iter()
                .find_map(|v| {
                    Some(Held {
                        admitted_ms: num(v, "admitted_ms")?,
                        cores: u32::try_from(num(v, "cores")?).ok()?,
                        alone: v.get("alone").and_then(Value::as_bool) == Some(true),
                    })
                })
                .unwrap_or(Held {
                    admitted_ms: start_ms,
                    cores: need.cores,
                    alone: false,
                })
        });
        let next_ms = after.iter().find_map(|v| num(v, "next_ms"));
        let credit = after
            .iter()
            .filter_map(|v| {
                v.get("credit")?
                    .as_array()?
                    .iter()
                    .map(|p| p.as_u64().and_then(|p| u32::try_from(p).ok()))
                    .collect::<Option<Vec<u32>>>()
            })
            .next_back()
            .unwrap_or_default();
        let wrote = held.and_then(|_| after.iter().find_map(|v| num(v, "wrote")));
        Some(Record {
            pid: u32::try_from(num(&head, "pid")?).ok()?,
            start_ms,
            seq: num(&head, "seq")?,
            checkout: head
                .get("checkout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            need,
            held,
            credit,
            wrote,
            next_ms,
        })
    }
}

/// `d` in whole milliseconds, as a record keeps it.
pub(crate) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `text` cut on a character boundary short of [`TEXT_MAX`] bytes.
fn cut(text: &str) -> &str {
    let mut end = text.len().min(TEXT_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One JSON object and its newline.
fn line_of(o: Map) -> String {
    let mut line = aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned());
    line.push('\n');
    line
}

/// The next claim of this process: two claims of one process never share a stem.
fn next_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A claim file this process laid and keeps locked; it goes, and its lock with it, when this is dropped.
#[derive(Debug)]
pub(crate) struct Laid {
    lock: RecordLock,
    path: PathBuf,
    /// The grant its file has written ([`Ledger::admit`]): a rename that failed after it is tried
    /// again under that grant, without writing another.
    granted: Option<Held>,
}

impl Laid {
    /// The claim file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Add `line` to the record, refused where it would reach into the room kept for the
    /// measured writes' line ([`Self::wrote`]) before [`RECORD_MAX`], which no reader passes.
    pub(crate) fn append(&mut self, line: &str) -> io::Result<()> {
        let end = self.lock.seek(SeekFrom::End(0))?;
        let room = RECORD_MAX - WROTE_LINE as u64;
        if end.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX)) > room {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("the record would pass {room} bytes"),
            ));
        }
        self.lock.write_all(line.as_bytes())
    }

    /// Write `bytes` as the admitted claim's measured writes ([`Record::wrote`]): the line appended
    /// the first time, at the offset kept in `at`, and written over in place after, at one length.
    pub(crate) fn wrote(&mut self, at: &mut Option<u64>, bytes: u64) -> io::Result<()> {
        let offset = match *at {
            Some(offset) => offset,
            None => self.lock.seek(SeekFrom::End(0))?,
        };
        if offset.saturating_add(WROTE_LINE as u64) > RECORD_MAX {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("the record would pass {RECORD_MAX} bytes"),
            ));
        }
        self.lock.seek(SeekFrom::Start(offset))?;
        self.lock.write_all(Record::wrote_line(bytes).as_bytes())?;
        *at = Some(offset);
        Ok(())
    }
}

impl Drop for Laid {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.lock.unlock();
    }
}

/// The admissions' lock, held while one waiter reads the ledger and decides; its holder's pid lies before the locked range, for a waiter kept out to name.
#[derive(Debug)]
pub(crate) struct Turn {
    _lock: RecordLock,
}

/// The claims of one user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ledger {
    dir: PathBuf,
}

impl Ledger {
    /// The ledger in `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Ledger {
        Ledger { dir: dir.into() }
    }

    /// This user's ledger ([`user_dir`]).
    #[must_use]
    pub fn user() -> Option<Ledger> {
        user_dir().map(Ledger::at)
    }

    /// Its directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every live claim, oldest first, read without the admissions' lock and without removing a dead holder's file: what `aterm pkg capacity` prints.
    ///
    /// # Errors
    /// The directory could not be listed (never `NotFound`, which is no claims), or a claim file could not be tested.
    pub fn list(&self) -> io::Result<Vec<Record>> {
        self.scan(false)
    }

    /// Every live claim, oldest first, a dead holder's file removed when `reap`.
    pub(crate) fn scan(&self, reap: bool) -> io::Result<Vec<Record>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut live = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let held = if name.ends_with(CLAIM) {
                true
            } else if name.ends_with(WAIT) {
                false
            } else {
                continue;
            };
            if let Some(record) = probe(&entry.path(), held, reap)? {
                live.push(record);
            }
        }
        live.sort_by_key(Record::order);
        Ok(live)
    }

    /// Take the admissions' lock if nobody holds it, writing this process's pid into it; `Err(holder)` while another waiter holds it (its pid, where it wrote one).
    pub(crate) fn try_turn(&self) -> io::Result<Result<Turn, Option<u32>>> {
        ensure_dir(&self.dir)?;
        let path = self.dir.join(LOCK);
        let file = open_rendezvous(&path)?;
        match RecordLock::try_lock(file, RECORD_MAX) {
            Ok(mut lock) => {
                let _ = lock
                    .set_len(0)
                    .and_then(|()| lock.seek(SeekFrom::Start(0)))
                    .and_then(|_| writeln!(lock, "{}", std::process::id()));
                Ok(Ok(Turn { _lock: lock }))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(Err(lock_holder(&path))),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// A claim for `need` in the queue: the record laid under a name no reader looks at, locked, then named a waiter.
    pub(crate) fn lay(&self, need: &Need, start_ms: u64) -> io::Result<(Record, Laid)> {
        ensure_dir(&self.dir)?;
        let mut need = need.clone();
        let record = Record {
            pid: std::process::id(),
            start_ms,
            seq: next_seq(),
            checkout: need.checkout.take().unwrap_or_else(checkout_of_cwd),
            need,
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        let stem = record.stem();
        let taking = self.dir.join(format!("{stem}{TAKING}"));
        let path = self.dir.join(format!("{stem}{WAIT}"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file: File = options.open(&taking)?;
        let lock = match RecordLock::try_lock(file, RECORD_MAX) {
            Ok(lock) => lock,
            Err(e) => {
                let _ = std::fs::remove_file(&taking);
                return Err(match e {
                    std::fs::TryLockError::Error(e) => e,
                    std::fs::TryLockError::WouldBlock => io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "a claim file this process just created was already locked",
                    ),
                });
            }
        };
        let mut laid = Laid {
            lock,
            path: taking,
            granted: None,
        };
        laid.lock.write_all(record.head_line().as_bytes())?;
        std::fs::rename(&laid.path, &path)?;
        laid.path = path;
        Ok((record, laid))
    }

    /// Mark `laid` admitted with `held`: its grant written, then the file named a claim. The grant
    /// is written once: where the rename failed (the file removed meanwhile), the next call tries
    /// it again under the grant the file has, whatever `held` it is given.
    pub(crate) fn admit(&self, laid: &mut Laid, record: &mut Record, held: Held) -> io::Result<()> {
        let held = match laid.granted {
            Some(first) => first,
            None => {
                laid.lock.write_all(Record::held_line(held).as_bytes())?;
                laid.granted = Some(held);
                held
            }
        };
        let claim = self.dir.join(format!("{}{CLAIM}", record.stem()));
        std::fs::rename(&laid.path, &claim)?;
        laid.path = claim;
        record.held = Some(held);
        Ok(())
    }
}

/// The pid the admissions' lock at `path` names: its holder's, unless it was let go meanwhile.
fn lock_holder(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path)
        .and_then(|f| f.take(64).read_to_string(&mut text))
        .ok()?;
    text.trim().parse().ok()
}

/// Test one claim file: `Some(record)` while its holder lives, `None` for a dead holder's (removed when `reap`) or one gone meanwhile.
fn probe(path: &Path, held: bool, reap: bool) -> io::Result<Option<Record>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    match RecordLock::try_lock(file, RECORD_MAX) {
        Ok(lock) => {
            if reap {
                let _ = std::fs::remove_file(path);
            }
            drop(lock);
            Ok(None)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut text = String::new();
            File::open(path)
                .and_then(|f| f.take(RECORD_MAX).read_to_string(&mut text))
                .or_else(|e| match e.kind() {
                    io::ErrorKind::NotFound => Ok(0),
                    _ => Err(e),
                })?;
            Ok(Record::decode(&text, held))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// The ledger's directory, made if absent: private (`0700`) on Unix, since a record names a checkout.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Open (creating) a lock file: `0600`, never truncated.
fn open_rendezvous(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// The working directory's enclosing git checkout (the nearest directory holding `.git`), else the working directory; empty where it cannot be read.
fn checkout_of_cwd() -> String {
    let Ok(cwd) = std::env::current_dir() else {
        return String::new();
    };
    let found = cwd
        .ancestors()
        .find(|d| d.join(".git").symlink_metadata().is_ok())
        .unwrap_or(&cwd);
    found.to_string_lossy().into_owned()
}
