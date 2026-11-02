// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WAIT FOR ADMISSION OR A LOCK, MARKED WHERE A CEILING CAN SEE IT
//! (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT).
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

/// A wait of this process, marked while it lasts; the mark goes when it drops.
#[derive(Debug)]
pub struct Mark {
    lock: RecordLock,
    path: PathBuf,
    since: u64,
}

impl Mark {
    /// Mark this process's wait, `text` saying what it waits for, in the directory [`VAR`]
    /// names. `None` where it names none, or the mark cannot be laid there (a directory its
    /// ceiling has let go): a wait never fails for its mark.
    #[must_use]
    pub fn begin(text: &str) -> Option<Mark> {
        Mark::at(&dir()?, text).ok()
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
            options.mode(0o600);
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
            lock,
            path: taking,
            since: now_secs(),
        };
        mark.write(text, None)?;
        let named = dir.join(format!("{stem}{MARK}"));
        std::fs::rename(&mark.path, &named)?;
        mark.path = named;
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

    fn write(&mut self, text: &str, stuck: Option<&Stuck>) -> io::Result<()> {
        let line = record(std::process::id(), self.since, text, stuck);
        self.lock.seek(io::SeekFrom::Start(0))?;
        self.lock.write_all(line.as_bytes())?;
        self.lock
            .set_len(u64::try_from(line.len()).unwrap_or(RECORD))
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut head = Vec::new();
    (&mut file).take(RECORD).read_to_end(&mut head).ok()?;
    match RecordLock::try_lock(file, RECORD) {
        Err(std::fs::TryLockError::WouldBlock) => parse(&String::from_utf8_lossy(&head)),
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
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(&format!("atc-waits-{label}-"))
            .tempdir()
            .expect("a scratch dir")
    }

    /// A mark reads back while its waiter holds it, says what it last said, and is gone once
    /// it drops; a mark whose lock nobody holds (its waiter died) is no wait.
    #[test]
    fn a_mark_is_a_wait_while_its_waiter_holds_it() {
        let dir = scratch("live");
        assert!(waiting(dir.path()).is_empty());
        let mut mark = Mark::at(dir.path(), "lab linux proof waits for the lane").expect("laid");
        let seen = waiting(dir.path());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(
            (seen[0].pid, seen[0].text.as_str()),
            (std::process::id(), "lab linux proof waits for the lane")
        );
        mark.say("still waiting\tfor pid 7 (lab linux exec)");
        assert_eq!(
            waiting(dir.path())[0].text,
            "still waiting for pid 7 (lab linux exec)"
        );
        assert_eq!(waiting(dir.path())[0].stuck, None);
        let stuck = Stuck {
            holders: vec![7],
            span: Duration::from_secs(900),
        };
        mark.say_stuck(
            "waiting for pid 7, which has made no progress",
            Some(&stuck),
        );
        let seen = waiting(dir.path());
        assert_eq!(
            (seen[0].text.as_str(), seen[0].stuck.as_ref()),
            (
                "waiting for pid 7, which has made no progress",
                Some(&stuck)
            )
        );
        mark.say("waiting for pid 7, which works again");
        assert_eq!(waiting(dir.path())[0].stuck, None, "a record said anew");
        let path = mark.path.clone();
        drop(mark);
        assert!(waiting(dir.path()).is_empty());
        assert!(!path.exists(), "the mark goes with its wait");

        std::fs::write(
            dir.path().join("1-0.wait"),
            record(1, 0, "a waiter that died", None),
        )
        .expect("a dead waiter's mark");
        assert!(
            waiting(dir.path()).is_empty(),
            "a mark nobody holds is no wait"
        );
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt as _;
            let fifo = dir.path().join("fifo-0.wait");
            let c_fifo = CString::new(fifo.as_os_str().as_bytes()).expect("cstr");
            // SAFETY: `c_fifo` is a live NUL-terminated path in our scratch directory; `mkfifo` retains no pointer.
            assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
            assert_eq!(live(&fifo), None);
            let held = Mark::at(dir.path(), "real waiter").expect("held");
            let link = dir.path().join("link-0.wait");
            std::os::unix::fs::symlink(&held.path, &link).expect("symlink");
            assert_eq!(live(&link), None);
            assert_eq!(waiting(dir.path()).len(), 1);
            drop(held);
        }
    }

    #[test]
    fn a_long_text_is_cut_on_a_character_boundary() {
        let stuck = Stuck {
            holders: vec![7, 8],
            span: Duration::from_secs(960),
        };
        for s in [None, Some(&stuck)] {
            let line = record(9, 1, &"é".repeat(4096), s);
            assert!(line.len() <= RECORD as usize, "{}", line.len());
            let back = parse(&line).expect("a record");
            assert!(back.pid == 9 && back.text.starts_with('é'), "{back:?}");
            assert_eq!(back.stuck.as_ref(), s);
        }
        assert!(parse("0\t1\tz").is_none(), "pid 0 is no waiter");
        assert!(parse("+9\t1\tz").is_none());
        assert!(parse("9\t+1\tz").is_none());
        assert!(parse("9\t1\tz\nstuck\t960\t7,8").is_some_and(|w| w.stuck.is_some()));
        assert!(parse("9\t1\tz\nstuck\t+960\t7").is_some_and(|w| w.stuck.is_none()));
        assert!(parse("9\t1\tz\nstuck\t960\t7,+8").is_some_and(|w| w.stuck.is_none()));
        assert!(crate::named_lock::Holder::parse("+9\t1\tv\tc").is_none());
        assert!(crate::named_lock::Holder::parse("9\t+1\tv\tc").is_none());
    }

    #[test]
    fn no_directory_marks_nothing() {
        let dir = scratch("gone");
        let gone = dir.path().join("let-go");
        assert!(Mark::at(&gone, "x").is_err());
        assert!(waiting(&gone).is_empty());
    }
}
