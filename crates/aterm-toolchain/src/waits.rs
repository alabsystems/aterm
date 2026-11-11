// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WAIT FOR ADMISSION OR A LOCK, MARKED WHERE A CEILING CAN SEE IT
//! (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT), AND WHERE A WAITER BEHIND IT CAN.
//!
//! A tool that ends a child at a ceiling (verify's per-check one) times the child's work, not
//! its waits: it names a directory in [`VAR`] for the child's tree, and a process of that tree
//! that waits for a named lock ([`crate::named_lock`]) or for room in the capacity ledger keeps a
//! [`Mark`] there while it waits. A mark is a file its waiter keeps locked ([`RecordLock`]), its
//! record saying what it waits for and who holds that; the kernel lets the lock go when the
//! waiter dies, however it dies, so a mark whose lock can be taken is no wait ([`waiting`]). A
//! wait needs no ceiling: its liveness is its once-a-minute line naming the holder, and a dead
//! holder lets its lock go. A live holder whose processes make no progress ([`crate::stall`]) is
//! what can keep it for ever, so the record says when every holder the wait is behind has made
//! none for the waiter's bound ([`Stuck`]), and the tool over it may end the wait on that.
//!
//! Every wait is marked in this user's own directory too ([`user_dir`]), whatever ceiling it
//! runs under or none: a holder that waits in its turn does no work of its own while it waits,
//! and the waiter behind it reads there that it waits, and whether what it waits behind is stuck
//! ([`user_waiting`]), rather than calling it stuck for a wait that is not its own doing.

use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::record_lock::RecordLock;

/// The variable naming the directory a wait of this process's tree is marked in.
pub const VAR: &str = "ATERM_WAITS";

/// The head of a mark the record may fill; the lock's range starts after it.
const RECORD: u64 = 4096;

/// A live mark's suffix.
const MARK: &str = ".wait";

/// The suffix of a mark being laid: no reader looks at it.
const TAKING: &str = ".taking";

static SEQ: AtomicU64 = AtomicU64::new(0);

/// The directory [`VAR`] names: where a wait of this process's tree is marked.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    std::env::var_os(VAR)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// How old a mark still being laid must be before [`Mark::begin`] takes it for one its waiter
/// died laying: laying takes a moment.
const LAYING: Duration = Duration::from_secs(60);

/// In a Unix test binary, the variable naming the directory it takes for this user's waits
/// ([`user_dir`]): a test sets it, to its own, on a stand-in whose marks it reads. Read nowhere
/// else.
pub const TEST_USER_VAR: &str = "ATERM_TEST_USER_WAITS";

/// This user's own waits directory, where every wait is marked whatever ceiling it runs under
/// ([`Mark::begin`]): `~/Library/Caches/aterm/waits` on macOS, `~/.cache/aterm/waits` on Linux,
/// `%LOCALAPPDATA%\aterm\waits` on Windows; `None` with no home. On Unix `~` is the account
/// database's home, whatever `$HOME` says, so a tool run under a scratch home (the lab, verify's
/// smoke stages) marks its waits where every session of this user reads them, as the capacity
/// ledger is kept. A Unix test binary's waits stay out of them: its own scratch root's `waits`
/// ([`aterm_uds::test_binary::scratch_root`]), or the one [`TEST_USER_VAR`] names.
#[must_use]
pub fn user_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if cfg!(windows) {
        return var("LOCALAPPDATA").map(|d| PathBuf::from(d).join("aterm").join("waits"));
    }
    #[cfg(unix)]
    if let Some(root) = aterm_uds::test_binary::scratch_root() {
        return Some(var(TEST_USER_VAR).map_or_else(|| root.join("waits"), PathBuf::from));
    }
    #[cfg(unix)]
    let home = aterm_uds::account::home().or_else(|| var("HOME").map(PathBuf::from))?;
    #[cfg(not(unix))]
    let home = PathBuf::from(var("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/aterm/waits")
    } else {
        home.join(".cache/aterm/waits")
    })
}

/// The waits marked in this user's own directory now ([`user_dir`], [`waiting`]), this
/// process's own left out: what a waiter reads of the holder it waits behind
/// ([`crate::stall::Waits`]), and its own waits are not that holder's. None with no home.
#[must_use]
pub fn user_waiting() -> Vec<Waiting> {
    let me = std::process::id();
    user_dir().map_or_else(Vec::new, |d| {
        let mut found = waiting(&d);
        found.retain(|w| w.pid != me);
        found
    })
}

/// A wait of this process, marked while it lasts in each directory it was laid in; the marks go
/// when it drops.
#[derive(Debug)]
pub struct Mark {
    laid: Vec<Laid>,
    since: u64,
}

/// One file of a [`Mark`], which its waiter keeps locked.
#[derive(Debug)]
struct Laid {
    lock: RecordLock,
    path: PathBuf,
}

impl Mark {
    /// Mark this process's wait, `text` saying what it waits for, in the directory [`VAR`]
    /// names and in this user's own ([`user_dir`], made where it is missing, the marks of
    /// waiters that died there removed first). `None` where it is laid in neither (no ceiling
    /// and no home, or a directory its ceiling has let go): a wait never fails for its mark.
    #[must_use]
    pub fn begin(text: &str) -> Option<Mark> {
        Mark::in_each(&Mark::dirs(), text)
    }

    /// Where [`Mark::begin`] lays a mark: the directory [`VAR`] names, and this user's own
    /// once it is made, its dead marks swept.
    #[must_use]
    pub fn dirs() -> Vec<PathBuf> {
        let user = user_dir().filter(|d| {
            let mut made = std::fs::DirBuilder::new();
            made.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                made.mode(0o700);
            }
            made.create(d).is_ok()
        });
        if let Some(d) = &user {
            sweep(d);
        }
        dir().into_iter().chain(user).collect()
    }

    /// Mark this process's wait in each of `dirs` that takes one ([`Mark::at`]); `None` where
    /// none does.
    #[must_use]
    pub fn in_each(dirs: &[PathBuf], text: &str) -> Option<Mark> {
        let mut laid = Vec::new();
        let since = now_secs();
        for dir in dirs {
            if let Ok(mut one) = Mark::at(dir, text) {
                one.since = since;
                laid.append(&mut one.laid);
            }
        }
        (!laid.is_empty()).then(|| Mark { laid, since })
    }

    /// Mark this process's wait in `dir`, which must exist: laid under a name no reader looks
    /// at, locked, its record written, then named a mark.
    ///
    /// # Errors
    /// The file could not be made, locked, written or named.
    pub fn at(dir: &Path, text: &str) -> io::Result<Mark> {
        let stem = format!(
            "{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let taking = dir.join(format!("{stem}{TAKING}"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(&taking)?;
        let lock = match RecordLock::try_lock(file, RECORD) {
            Ok(lock) => lock,
            Err(e) => {
                let _ = std::fs::remove_file(&taking);
                return Err(match e {
                    std::fs::TryLockError::Error(e) => e,
                    std::fs::TryLockError::WouldBlock => io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "a mark this process just made was already locked",
                    ),
                });
            }
        };
        let mut mark = Mark {
            laid: vec![Laid { lock, path: taking }],
            since: now_secs(),
        };
        mark.write(text, None)?;
        let named = dir.join(format!("{stem}{MARK}"));
        std::fs::rename(&mark.laid[0].path, &named)?;
        mark.laid[0].path = named;
        Ok(mark)
    }

    /// Say what it waits for now, its holder named afresh. A record that cannot be written keeps
    /// what it said before.
    pub fn say(&mut self, text: &str) {
        self.say_stuck(text, None);
    }

    /// [`Self::say`], and whether every holder it waits behind has made no progress for the
    /// waiter's bound ([`Stuck`]).
    pub fn say_stuck(&mut self, text: &str, stuck: Option<&Stuck>) {
        let _ = self.write(text, stuck);
    }

    /// Write the record to each of its files; the first error, once every file was tried.
    fn write(&mut self, text: &str, stuck: Option<&Stuck>) -> io::Result<()> {
        let line = record(std::process::id(), self.since, text, stuck);
        let mut first = Ok(());
        for laid in &mut self.laid {
            let wrote = laid
                .lock
                .seek(io::SeekFrom::Start(0))
                .and_then(|_| laid.lock.write_all(line.as_bytes()))
                .and_then(|()| {
                    laid.lock
                        .set_len(u64::try_from(line.len()).unwrap_or(RECORD))
                });
            if first.is_ok() {
                first = wrote;
            }
        }
        first
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        for laid in &self.laid {
            let _ = std::fs::remove_file(&laid.path);
        }
    }
}

/// Remove the marks in `dir` whose waiters died without removing them (killed while they
/// waited), so a directory every wait of this user is marked in does not grow: a mark whose lock
/// can be taken is no wait ([`live`]), and one still being laid is a dead waiter's once it is
/// [`LAYING`] old. A mark named over the dead one meanwhile (its pid reused) is left alone.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let laying = name.ends_with(TAKING);
        if !laying && !name.ends_with(MARK) {
            continue;
        }
        let path = e.path();
        let Ok(file) = open_mark(&path) else {
            continue;
        };
        let Ok(meta) = file.metadata() else {
            continue;
        };
        let young = || {
            meta.modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_none_or(|age| age < LAYING)
        };
        if !meta.is_file() || (laying && young()) {
            continue;
        }
        if let Ok(dead) = RecordLock::try_lock(file, RECORD) {
            if same_file(&meta, &path) {
                let _ = std::fs::remove_file(&path);
            }
            drop(dead);
        }
    }
}

/// Whether `path` still names the file whose metadata is `open`.
#[cfg(unix)]
fn same_file(open: &std::fs::Metadata, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.dev() == open.dev() && m.ino() == open.ino())
}

/// Whether `path` still names the file whose metadata is `open`: a name cannot be given to
/// another file while this process holds the file open, so it does.
#[cfg(not(unix))]
fn same_file(_open: &std::fs::Metadata, _path: &Path) -> bool {
    true
}

/// `path` opened to read, never following a link at it nor blocking on a FIFO.
fn open_mark(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// A wait a mark names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    /// The waiter's pid.
    pub pid: u32,
    /// When it began to wait, unix seconds.
    pub since: u64,
    /// What it waits for and who holds that, as it last said.
    pub text: String,
    /// Whether every holder it waits behind had made no progress for its waiter's bound, as it
    /// last said.
    pub stuck: Option<Stuck>,
    identity: Option<LiveMark>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveMark {
    path: PathBuf,
    file: aterm_sysprobe::volumes::FileId,
}

impl Waiting {
    /// A wait as a record says it, with no live file behind it, so it confirms no mark
    /// ([`Self::same_mark`]): a test's stand-in for one read in this user's waits.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn said(pid: u32, since: u64, text: &str, stuck: Option<Stuck>) -> Waiting {
        Waiting {
            pid,
            since,
            text: text.to_owned(),
            stuck,
            identity: None,
        }
    }

    /// Whether both snapshots came from the same observed locked mark and record identity.
    /// Text and elapsed duration are not identity. Unavailable file identity never confirms
    /// another observation, and this comparison does not establish that a past mark is still live.
    #[must_use]
    pub fn same_mark(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.since == other.since
            && self
                .identity
                .as_ref()
                .is_some_and(|identity| Some(identity) == other.identity.as_ref())
    }
}

/// The holders a wait is behind, every one of which has made no progress ([`crate::stall`]) for
/// at least its waiter's bound, as the waiter last read them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stuck {
    /// Their pids.
    pub holders: Vec<u32>,
    /// How long the one that made progress last has made none.
    pub span: Duration,
}

/// The waits marked in `dir` now: every mark whose waiter still holds it, oldest first. A
/// missing or unreadable directory marks none.
#[must_use]
pub fn waiting(dir: &Path) -> Vec<Waiting> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Waiting> = entries
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| n.ends_with(MARK))
                && e.file_type().is_ok_and(|t| t.is_file())
        })
        .filter_map(|e| live(&e.path()))
        .collect();
    found.sort_by_key(|w| (w.since, w.pid));
    found
}

/// The wait `path` marks, while its waiter holds it.
fn live(path: &Path) -> Option<Waiting> {
    let mut file = open_mark(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt as _;
        Some(LiveMark {
            path: path.to_owned(),
            file: aterm_sysprobe::volumes::FileId {
                dev: metadata.dev(),
                ino: u128::from(metadata.ino()),
            },
        })
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::io::AsHandle as _;
        aterm_sysprobe::volumes::file_id_of_handle(file.as_handle()).map(|file| LiveMark {
            path: path.to_owned(),
            file,
        })
    };
    #[cfg(not(any(unix, windows)))]
    let identity = None;
    let cap = usize::try_from(metadata.len().min(RECORD).saturating_add(1)).unwrap_or(0);
    let mut head = Vec::with_capacity(cap);
    (&mut file).take(RECORD).read_to_end(&mut head).ok()?;
    match RecordLock::try_lock(file, RECORD) {
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut waiting = parse(&String::from_utf8_lossy(&head))?;
            waiting.identity = identity;
            Some(waiting)
        }
        Ok(_) | Err(std::fs::TryLockError::Error(_)) => None,
    }
}

/// A record: pid, since and text on one tab-separated line, cut short of [`RECORD`] on a
/// character boundary; then, for a wait whose holders are [`Stuck`], a line `stuck`, the seconds
/// and the holders' pids, which a reader that knows only the first line leaves alone.
fn record(pid: u32, since: u64, text: &str, stuck: Option<&Stuck>) -> String {
    let tail = stuck.map_or_else(String::new, |s| {
        let pids: Vec<String> = s.holders.iter().map(u32::to_string).collect();
        format!("stuck\t{}\t{}\n", s.span.as_secs(), pids.join(","))
    });
    let mut line = format!("{pid}\t{since}\t{}", text.replace(['\t', '\n', '\r'], " "));
    let limit = usize::try_from(RECORD).map_or(0, |r| r.saturating_sub(1 + tail.len()));
    if line.len() > limit {
        let mut end = limit;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
    }
    line.push('\n');
    line.push_str(&tail);
    line
}

fn parse(text: &str) -> Option<Waiting> {
    let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut lines = text.lines();
    let mut fields = lines.next()?.splitn(3, '\t');
    Some(Waiting {
        pid: fields
            .next()
            .filter(|s| uint(s))?
            .parse()
            .ok()
            .filter(|&p| p != 0)?,
        since: fields.next().filter(|s| uint(s))?.parse().ok()?,
        text: fields.next().unwrap_or_default().to_string(),
        stuck: lines.find_map(|l| {
            let mut f = l.strip_prefix("stuck\t")?.splitn(2, '\t');
            let span = Duration::from_secs(f.next().filter(|s| uint(s))?.parse().ok()?);
            let holders = f
                .next()?
                .split(',')
                .map(|h| h.parse().ok().filter(|_| uint(h)))
                .collect::<Option<Vec<u32>>>()
                .filter(|h| !h.is_empty())?;
            Some(Stuck { holders, span })
        }),
        identity: None,
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
#[path = "waits_tests.rs"]
mod tests;
