// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-learn`, Aos M6's learner verbs (design §13.7, stage D): `init`, `chain`, `train`,
//! `rollback`, `verify` and `session`, over m6's state machines.
//!
//! This module does byte I/O alone: the files behind m6's `DurableLog` ([`LabLog`]) and
//! `DurableStore` ([`LabStore`]), the named refusal exits, the cost report, the footprint
//! self-report and the `held_out_judged` check every exit path makes. Only `session` labels a
//! held-out, report or practice set, and only `session --pair` names the real sets' capability.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_durable::{durable, durable_dir};
use aterm_effects::aos_momentum::m4::{Evaluator, RealSets, held_out_judged};
use aterm_effects::aos_momentum::m6::{
    Arm, ChainPin, DurableLog, DurableStore, Labeller, Ledger, Line, LogFault, M6_CHAIN_V1, Pair,
    PairLines, SessionEnv, SessionRun, Sets, Step, Store, StoreFault, chain, ledger_head,
    session_a, session_b,
};
use aterm_effects::aos_momentum::m6_policy::{RANKER_POLICY_NAME, RANKER_POLICY_VERSION};

use crate::instance::take_value;
use crate::lab_build::{LAB_BUILD, check_build};
use crate::paths::{from_caller, paths};
use crate::spread::Threads;
use crate::sys::max_footprint;
use crate::trail_gallery::{checkouts, resolve_out};
use crate::{Fail, Res};

/// The verb's own help (`aos-learn --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-learn VERB ...

Aos M6's learner (design §13). Every verb reads training data alone but `session`, which alone labels
a held-out, report or practice set, under its pair's write-ahead ledger.

  build
        this lab's build and profile, which aos-learn-bench checks before either child (G0)
  init --store DIR (--pair I | --practice J)
        a new store for the pair: v0 active and at the head, revision 0
  chain [--threads N]
        the chain v0..v4 on training data alone, printed as M6_CHAIN_V1's source (stage E)
  train --store DIR [--threads N]
        derives the chain member after the head and moves the head to it
  rollback --store DIR --to ID
        a validated rollback to v0 or a member a recorded look admitted
  verify --store DIR [--threads N]
        re-derives every retained member and checks the revisions and, read only, the pair's ledger
  session --store DIR (--pair I | --practice J) --tests-log-digest HEX [--threads N | --replay A]
        child A of a session or, with --replay naming child A's store, child B (single-threaded);
        aos-learn-bench runs it, with a new store

The ledgers live under the account database's home, in Library/Application Support/aterm-lab/aos-m6/
<policy>-v<version>/, never under HOME or LAB_STATE; `session` refuses NoHome without that home and
ScratchHome under a scratch HOME. A store is refused inside this repository's checkouts.
exit: 0 done; 1 refused (named) or a defect (held_out_judged is not 0); 2 usage
";

/// The first line of a session child's summary, which the runner reads.
pub const SUMMARY: &str = "aos-learn-session-v1";

/// The most lines a pair's ledger may hold before it is read as broken.
const MAX_LINES: u64 = 1_024;

/// The pair a ledger directory's name names, written canonically (`pair-1`, `practice-0`).
fn pair_named(name: &str) -> Option<Pair> {
    let number = |digits: &str| {
        let canonical = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'));
        canonical.then(|| digits.parse::<u32>().ok()).flatten()
    };
    if let Some(i) = name.strip_prefix("pair-") {
        number(i).map(Pair::Real)
    } else {
        name.strip_prefix("practice-")
            .and_then(number)
            .map(Pair::Practice)
    }
}

/// A 64-digit lowercase hex digest.
#[must_use]
pub fn parse_digest(hex: &str) -> Option<Digest> {
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(Digest(out))
}

/// The temp names this process writes, numbered so no two collide.
static TEMPS: AtomicU64 = AtomicU64::new(0);

/// A fresh temp path under `dir/.tmp/`, which no listing m6 reads holds. The staging directory is
/// not flushed: nothing is read through it after a crash, and what is linked or renamed out of it
/// is flushed under its final name.
fn temp_in(dir: &Path) -> io::Result<PathBuf> {
    let tmp = dir.join(".tmp");
    fs::create_dir_all(&tmp)?;
    let n = TEMPS.fetch_add(1, Ordering::SeqCst);
    Ok(tmp.join(format!("{}-{n}", std::process::id())))
}

/// Writes `bytes` to a new file `path`, whole, and flushes it durably: a file the store holds is
/// made new and never appended to, so its bytes written are its size (design §13.5).
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = File::options().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?;
    durable(&f)
}

/// Writes `bytes` to a fresh temp file under `dir`, durably.
fn write_temp(dir: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = temp_in(dir)?;
    write_new(&path, bytes)?;
    Ok(path)
}

/// Makes `dir` and each missing directory above it, every new name flushed durably in its
/// parent, so a file later made in `dir` is reachable after a crash. Every directory a ledger
/// line, a store file or the runner's record lives in is made here; only the `.tmp/` staging
/// directories are not ([`temp_in`]).
/// # Errors
/// A directory cannot be made or flushed.
pub fn make_dirs(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let up = dir.parent().filter(|up| !up.as_os_str().is_empty());
    if let Some(up) = up {
        make_dirs(up)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    up.map_or(Ok(()), durable_dir)
}

/// Creates `path` holding `bytes` by `link(2)` from a fsynced temp file under `temps`, which fails
/// when the name exists: a second writer of the name gets `AlreadyExists`.
fn create_linked(temps: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("a file with no directory"))?;
    make_dirs(parent)?;
    let tmp = write_temp(temps, bytes)?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    linked?;
    durable_dir(parent)
}

/// `path`'s bytes, at most `cap` of them; `None` when it does not exist.
fn read_capped(path: &Path, cap: u64) -> io::Result<Option<Vec<u8>>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    f.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::other(format!(
            "{} holds more than {cap} bytes",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// The most bytes a store file may hold as read (a version directory's cap, design §13.6).
const MAX_STORE_FILE: u64 = 64 * 1024;

/// The most bytes a ledger line may hold as read.
const MAX_LINE_FILE: u64 = 4 * 1024;

/// One pair's ledger under a version's root, over files (design §13.4, rule 4): line k is
/// `<root>/<pair>/<k>.line`, created by `link(2)` from a fsynced temp file, and a writer holds an
/// exclusive `flock` on `<root>/ledger.lock` for the log's life.
#[derive(Debug)]
pub struct LabLog {
    root: PathBuf,
    dir: PathBuf,
    /// The writer's lock; `None` for a read-only log, whose appends are refused.
    lock: Option<Held>,
}

impl LabLog {
    /// The version's root under `home`, the account database's home (design §13.4, rule 4).
    #[must_use]
    pub fn version_root(home: &str) -> PathBuf {
        Path::new(home)
            .join("Library/Application Support/aterm-lab/aos-m6")
            .join(format!("{RANKER_POLICY_NAME}-v{RANKER_POLICY_VERSION}"))
    }

    /// The runner's lock on `root`, `runner.lock`, held for a whole session so no two runners
    /// overlap; the children take `ledger.lock` while it is held. The file then holds the
    /// runner's pid alone, which [`LabLog::need_runner`] checks against a session's parent. It is
    /// written under the lock and read only while the lock is held, so it is not flushed.
    /// # Errors
    /// It cannot be opened or written, or another runner holds it.
    pub fn hold_runner(root: &Path) -> Result<Held, String> {
        let path = root.join("runner.lock");
        let held = lock_file(&path)?;
        held.0
            .set_len(0)
            .and_then(|()| (&held.0).write_all(format!("{}\n", std::process::id()).as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(held)
    }

    /// `pair`'s ledger under `root`, as its one writer.
    /// # Errors
    /// The root cannot be made, or another writer holds `ledger.lock`.
    pub fn open(root: &Path, pair: Pair) -> Result<LabLog, String> {
        let dir = root.join(pair.name());
        make_dirs(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        let lock = lock_file(&root.join("ledger.lock"))?;
        Ok(LabLog {
            root: root.to_path_buf(),
            dir,
            lock: Some(lock),
        })
    }

    /// [`LabLog::open`], waiting up to `limit` for another writer to let `ledger.lock` go (the
    /// runner's, after its children). Each wait between tries is `pause`'s, which ends the wait
    /// with its error (the runner's: an interrupt).
    /// # Errors
    /// The root cannot be made, `ledger.lock` is still held at the limit, or `pause` ended the
    /// wait.
    pub fn open_within(
        root: &Path,
        pair: Pair,
        limit: Duration,
        pause: &dyn Fn(Duration) -> Result<(), String>,
    ) -> Result<LabLog, String> {
        let dir = root.join(pair.name());
        make_dirs(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        let lock = lock_within(&root.join("ledger.lock"), limit, pause)?;
        Ok(LabLog {
            root: root.to_path_buf(),
            dir,
            lock: Some(lock),
        })
    }

    /// Refuses unless this session's own runner holds `root`'s `runner.lock`: a session child runs
    /// only under a live runner, which alone writes its pair's `void` or `record` (design §13.4,
    /// rule 4), and that runner is its parent, which spawned it directly.
    /// # Errors
    /// No runner holds it, the runner holding it is not this session's parent, or it cannot be
    /// read.
    pub fn need_runner(root: &Path) -> Result<(), String> {
        LabLog::need_runner_of(root, std::os::unix::process::parent_id())
    }

    /// [`LabLog::need_runner`] for a session whose parent's pid is `parent`: the lock must be held
    /// and the file must name `parent`.
    fn need_runner_of(root: &Path, parent: u32) -> Result<(), String> {
        let path = root.join("runner.lock");
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(format!("no runner has run under {}", root.display()));
            }
            Err(e) => return Err(format!("cannot open {}: {e}", path.display())),
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = f.try_lock();
        match taken {
            Err(std::fs::TryLockError::WouldBlock) => {
                let mut text = String::new();
                (&f).take(32)
                    .read_to_string(&mut text)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let pid = text.strip_suffix('\n').and_then(|t| t.parse::<u32>().ok());
                if pid == Some(parent) {
                    Ok(())
                } else {
                    Err(format!(
                        "{} is held by a runner that is not this session's: it names {}, and this \
                         session's parent is {parent}",
                        path.display(),
                        pid.map_or_else(|| "no pid".to_owned(), |p| format!("pid {p}"))
                    ))
                }
            }
            Ok(()) => {
                drop(Held(f));
                Err(format!(
                    "no runner holds {}: a session runs under aos-learn-bench",
                    path.display()
                ))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                Err(format!("cannot lock {}: {e}", path.display()))
            }
        }
    }

    /// `pair`'s ledger under `root`, read only: it takes no lock and refuses every write.
    #[must_use]
    pub fn read_only(root: &Path, pair: Pair) -> LabLog {
        LabLog {
            root: root.to_path_buf(),
            dir: root.join(pair.name()),
            lock: None,
        }
    }

    /// Every pair directory under `root` (each name starting `pair-` or `practice-`) by its name,
    /// each with its own result: its pair and line files, or why not, so one pair's fault is
    /// never another's. Ordered by pair, after every name that names none. A root that does not
    /// exist holds none.
    /// # Errors
    /// The root cannot be read.
    pub fn pairs(root: &Path) -> Result<Vec<PairEntry>, LogFault> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(LogFault::Io(e.to_string())),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| LogFault::Io(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("pair-") || name.starts_with("practice-")) {
                continue;
            }
            let read = pair_named(&name)
                .ok_or_else(|| LogFault::Io(format!("{name} names no pair canonically")))
                .and_then(|pair| Ok((pair, line_files(&entry.path())?)));
            out.push((name, read));
        }
        out.sort_by_key(|(name, _)| (pair_named(name), name.clone()));
        Ok(out)
    }
}

/// One pair directory under a version's root: its name, and its pair and line files or why not.
pub type PairEntry = (String, Result<PairLines, LogFault>);

/// An exclusive `flock` on an open file, released by `LOCK_UN` before the file closes
/// (clippy.toml's flock rule), so a process this one spawns never holds it.
#[derive(Debug)]
pub struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// `path` opened (made when absent, its directory by [`make_dirs`]) and locked with an exclusive
/// `flock`, without waiting.
fn lock_file(path: &Path) -> Result<Held, String> {
    lock_within(path, Duration::ZERO, &|_| Ok(()))
}

/// [`lock_file`], re-trying each second for up to `limit` while another holder has it, each
/// second waited by `pause`, whose error ends the wait.
fn lock_within(
    path: &Path,
    limit: Duration,
    pause: &dyn Fn(Duration) -> Result<(), String>,
) -> Result<Held, String> {
    if let Some(parent) = path.parent() {
        make_dirs(parent).map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
    }
    let f = File::options()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let start = Instant::now();
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = f.try_lock();
        match taken {
            Ok(()) => return Ok(Held(f)),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < limit => {
                pause(Duration::from_secs(1))?;
            }
            Err(e) => {
                return Err(format!(
                    "{} is held by another writer ({e})",
                    path.display()
                ));
            }
        }
    }
}

/// The line files of the ledger in `dir`, line 0 first. The directory must hold `0.line` to
/// `<n-1>.line` and nothing else, so a gap or a stray file is a broken ledger, never a shorter one.
fn line_files(dir: &Path) -> Result<Vec<Vec<u8>>, LogFault> {
    let io = |e: io::Error| LogFault::Io(e.to_string());
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    let mut ks = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(io)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let k = name
            .strip_suffix(".line")
            .filter(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
            .filter(|k| *k == "0" || !k.starts_with('0'))
            .and_then(|k| k.parse::<u64>().ok())
            .ok_or_else(|| LogFault::Io(format!("{} holds a stray {name}", dir.display())))?;
        ks.push(k);
    }
    ks.sort_unstable();
    if ks.len() as u64 > MAX_LINES {
        return Err(LogFault::Io(format!(
            "{} holds more than {MAX_LINES} lines",
            dir.display()
        )));
    }
    if ks.iter().enumerate().any(|(i, k)| *k != i as u64) {
        return Err(LogFault::Io(format!(
            "{} has a gap in its lines",
            dir.display()
        )));
    }
    let mut out = Vec::with_capacity(ks.len());
    for k in ks {
        let path = dir.join(format!("{k}.line"));
        let bytes = read_capped(&path, MAX_LINE_FILE)
            .map_err(io)?
            .ok_or_else(|| LogFault::Io(format!("{} went away", path.display())))?;
        out.push(bytes);
    }
    Ok(out)
}

impl DurableLog for LabLog {
    fn lines(&self) -> Result<Vec<Vec<u8>>, LogFault> {
        line_files(&self.dir)
    }

    fn append(&mut self, k: u64, bytes: &[u8]) -> Result<(), LogFault> {
        if self.lock.is_none() {
            return Err(LogFault::Io(
                "a read-only ledger is never written".to_owned(),
            ));
        }
        match create_linked(&self.root, &self.dir.join(format!("{k}.line")), bytes) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(LogFault::Exists),
            Err(e) => Err(LogFault::Io(e.to_string())),
        }
    }

    fn root(&self) -> Result<Vec<PairLines>, LogFault> {
        LabLog::pairs(&self.root)?
            .into_iter()
            .map(|(_, pair)| pair)
            .collect()
    }

    fn chain(&mut self, digest: &Digest) -> Result<Digest, LogFault> {
        let path = self.root.join("chain");
        if !path.exists() {
            if self.lock.is_none() {
                return Err(LogFault::Io(
                    "a read-only ledger makes no chain file".to_owned(),
                ));
            }
            match create_linked(&self.root, &path, format!("{}\n", digest.hex()).as_bytes()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(LogFault::Io(e.to_string())),
            }
        }
        let bytes = read_capped(&path, 65)
            .map_err(|e| LogFault::Io(e.to_string()))?
            .unwrap_or_default();
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| t.strip_suffix('\n'))
            .and_then(parse_digest)
            .ok_or_else(|| LogFault::Io(format!("{} is not a chain digest", path.display())))
    }
}

/// One store's files under its directory (design §13.5), held under an exclusive `flock` on its
/// `.lock`. A new file is written to a temp name under `.tmp/`, fsynced and linked into place; a
/// version's directory is made whole under `.tmp/` and renamed into place; the pointer is replaced
/// by one rename; each directory a name is made in is fsynced.
#[derive(Debug)]
pub struct LabStore {
    dir: PathBuf,
    /// The store's lock, held for the value's life.
    _lock: Held,
    /// The fault seam: the `n`th write of a step, after which that write reports a stop (tests).
    #[cfg(test)]
    stop: Option<(Step, usize)>,
    /// The steps written so far (tests).
    #[cfg(test)]
    seen: Vec<Step>,
}

impl LabStore {
    /// The store whose files are under the existing directory `dir`, locked.
    /// # Errors
    /// `dir` is not a directory, or another process holds the store.
    pub fn open(dir: PathBuf) -> Result<LabStore, String> {
        if !dir.is_dir() {
            return Err(format!("{} is not a store's directory", dir.display()));
        }
        let lock = lock_file(&dir.join(".lock"))?;
        Ok(LabStore {
            dir,
            _lock: lock,
            #[cfg(test)]
            stop: None,
            #[cfg(test)]
            seen: Vec::new(),
        })
    }

    /// This store with its `nth` write of `step` reporting a stop once it is durable (tests).
    #[cfg(test)]
    pub(crate) fn stopping(self, step: Step, nth: usize) -> LabStore {
        LabStore {
            stop: Some((step, nth)),
            ..self
        }
    }

    /// The path of the store file `name`: relative, `/`-separated, no part empty, `.`, `..` or
    /// starting with `.`.
    fn path(&self, name: &str) -> Result<PathBuf, StoreFault> {
        let ours = !name.is_empty()
            && name
                .split('/')
                .all(|p| !p.is_empty() && !p.starts_with('.'));
        if ours {
            Ok(self.dir.join(name))
        } else {
            Err(StoreFault::Io(format!("{name:?} names no store file")))
        }
    }

    /// The fault seam, after a durable write of `step`.
    fn seam(&mut self, step: Step) -> Result<(), StoreFault> {
        #[cfg(test)]
        {
            self.seen.push(step);
            let n = self.seen.iter().filter(|s| **s == step).count();
            if self.stop == Some((step, n)) {
                return Err(StoreFault::Stopped(step));
            }
        }
        #[cfg(not(test))]
        let _ = step;
        Ok(())
    }

    /// The bytes the store retains, from file metadata: every file under `versions/`,
    /// `revisions/` and `eval/` but dot-named ones, and the count of version directories (the cost
    /// report's retained figures, design §13.5).
    /// # Errors
    /// A directory cannot be read.
    pub fn retained(&self) -> io::Result<(u64, usize)> {
        fn walk(dir: &Path, total: &mut u64) -> io::Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    walk(&entry.path(), total)?;
                } else if kind.is_file() {
                    *total += entry.metadata()?.len();
                }
            }
            Ok(())
        }
        let mut total = 0;
        for sub in ["versions", "revisions", "eval"] {
            match walk(&self.dir.join(sub), &mut total) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                other => other?,
            }
        }
        let versions = match fs::read_dir(self.dir.join("versions")) {
            Ok(entries) => entries.count(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        Ok((total, versions))
    }
}

/// An I/O error as a store fault.
fn io_fault(e: &io::Error) -> StoreFault {
    StoreFault::Io(e.to_string())
}

impl DurableStore for LabStore {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreFault> {
        read_capped(&self.path(name)?, MAX_STORE_FILE).map_err(|e| io_fault(&e))
    }

    fn list(&self, dir: &str) -> Result<Vec<String>, StoreFault> {
        let entries = match fs::read_dir(self.path(dir)?) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_fault(&e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_fault(&e))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn create(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault> {
        let path = self.path(name)?;
        match create_linked(&self.dir, &path, bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StoreFault::Exists(name.to_owned()));
            }
            Err(e) => return Err(io_fault(&e)),
        }
        self.seam(step)
    }

    fn create_dir(
        &mut self,
        step: Step,
        dir: &str,
        files: &[(&str, &[u8])],
    ) -> Result<(), StoreFault> {
        let path = self.path(dir)?;
        if path.exists() {
            return Err(StoreFault::Exists(dir.to_owned()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| StoreFault::Io(format!("{dir} has no directory")))?
            .to_path_buf();
        let tmp = temp_in(&self.dir).map_err(|e| io_fault(&e))?;
        let made = (|| {
            make_dirs(&parent)?;
            fs::create_dir(&tmp)?;
            for (name, bytes) in files {
                if name.is_empty() || name.contains('/') || name.starts_with('.') {
                    return Err(io::Error::other(format!("{name:?} names no version file")));
                }
                write_new(&tmp.join(name), bytes)?;
            }
            durable_dir(&tmp)?;
            fs::rename(&tmp, &path)?;
            durable_dir(&parent)
        })();
        if let Err(e) = made {
            let _ = fs::remove_dir_all(&tmp);
            return Err(io_fault(&e));
        }
        self.seam(step)
    }

    fn replace(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault> {
        let path = self.path(name)?;
        let parent = path
            .parent()
            .ok_or_else(|| StoreFault::Io(format!("{name} has no directory")))?
            .to_path_buf();
        let replaced = (|| {
            make_dirs(&parent)?;
            let tmp = write_temp(&self.dir, bytes)?;
            fs::rename(&tmp, &path)?;
            durable_dir(&parent)
        })();
        replaced.map_err(|e| io_fault(&e))?;
        self.seam(step)
    }
}

/// A refusal of `verb`, named: exit 1.
fn refused(verb: &str, name: &str, why: impl std::fmt::Display) -> Fail {
    Fail::die(format!("aos-learn {verb}: refused {name}: {why}"))
}

/// What a verb was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verb {
    Build,
    Init { store: String, pair: Pair },
    Chain { threads: usize },
    Train { store: String, threads: usize },
    Rollback { store: String, to: Digest },
    Verify { store: String, threads: usize },
    Session(SessionArgs),
}

/// What `session` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionArgs {
    store: String,
    pair: Pair,
    tests_log: Digest,
    threads: usize,
    replay: Option<String>,
}

/// The pair `--pair I` (a real pair, I in 1..=3) or `--practice J` names, at most one of them.
fn take_pair(args: &mut Vec<String>) -> Result<Option<Pair>, Fail> {
    let real = take_value(args, "--pair")?;
    let practice = take_value(args, "--practice")?;
    let number = |flag: &str, v: &str| {
        v.parse::<u32>()
            .ok()
            .filter(|n| v == n.to_string())
            .ok_or_else(|| Fail::usage(format!("aos-learn: {flag} {v}: not a number")))
    };
    match (real, practice) {
        (Some(_), Some(_)) => Err(Fail::usage(
            "aos-learn: name one of --pair and --practice, not both",
        )),
        (Some(i), None) => {
            let i = number("--pair", &i)?;
            if (1..=3).contains(&i) {
                Ok(Some(Pair::Real(i)))
            } else {
                Err(Fail::usage(format!(
                    "aos-learn: --pair {i}: the real pairs are 1, 2 and 3"
                )))
            }
        }
        (None, Some(j)) => Ok(Some(Pair::Practice(number("--practice", &j)?))),
        (None, None) => Ok(None),
    }
}

/// `args` as a verb, each refused flag or value a usage error.
fn parse(args: &[String]) -> Result<Verb, Fail> {
    let mut rest: Vec<String> = args.to_vec();
    if rest.is_empty() {
        return Err(Fail::usage("aos-learn: name a verb (aos-learn --help)"));
    }
    let verb = rest.remove(0);
    let store = take_value(&mut rest, "--store")?;
    let pair = take_pair(&mut rest)?;
    let threads = take_value(&mut rest, "--threads")?
        .map(|v| {
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=64).contains(n) && v == n.to_string())
                .ok_or_else(|| Fail::usage(format!("aos-learn: --threads {v}: 1 to 64")))
        })
        .transpose()?;
    let to = take_value(&mut rest, "--to")?;
    let tests_log = take_value(&mut rest, "--tests-log-digest")?;
    let replay = take_value(&mut rest, "--replay")?;
    if let Some(extra) = rest.first() {
        return Err(Fail::usage(format!(
            "aos-learn {verb}: unexpected '{extra}'"
        )));
    }
    let given = [
        ("--store", store.is_some()),
        ("--pair or --practice", pair.is_some()),
        ("--threads", threads.is_some()),
        ("--to", to.is_some()),
        ("--tests-log-digest", tests_log.is_some()),
        ("--replay", replay.is_some()),
    ];
    let takes: &[&str] = match verb.as_str() {
        "build" => &[],
        "init" => &["--store", "--pair or --practice"],
        "chain" => &["--threads"],
        "train" | "verify" => &["--store", "--threads"],
        "rollback" => &["--store", "--to"],
        "session" => &[
            "--store",
            "--pair or --practice",
            "--threads",
            "--tests-log-digest",
            "--replay",
        ],
        _ => {
            return Err(Fail::usage(format!(
                "aos-learn: no verb {verb:?} (aos-learn --help)"
            )));
        }
    };
    if let Some((flag, _)) = given.iter().find(|(f, g)| *g && !takes.contains(f)) {
        return Err(Fail::usage(format!("aos-learn {verb} takes no {flag}")));
    }
    let need = |what: Option<String>, flag: &str| {
        what.ok_or_else(|| Fail::usage(format!("aos-learn {verb}: {flag} is required")))
    };
    if verb == "session" && replay.is_some() && threads.is_some() {
        return Err(Fail::usage(
            "aos-learn session --replay takes no --threads: child B runs single-threaded",
        ));
    }
    let threads = threads.unwrap_or(1);
    Ok(match verb.as_str() {
        "build" => Verb::Build,
        "init" => Verb::Init {
            store: need(store, "--store DIR")?,
            pair: pair.ok_or_else(|| {
                Fail::usage("aos-learn init: --pair I or --practice J is required")
            })?,
        },
        "chain" => Verb::Chain { threads },
        "train" => Verb::Train {
            store: need(store, "--store DIR")?,
            threads,
        },
        "verify" => Verb::Verify {
            store: need(store, "--store DIR")?,
            threads,
        },
        "rollback" => {
            let to = need(to, "--to ID")?;
            Verb::Rollback {
                store: need(store, "--store DIR")?,
                to: parse_digest(&to).ok_or_else(|| {
                    Fail::usage(format!(
                        "aos-learn rollback: --to {to}: not a 64-digit hex id"
                    ))
                })?,
            }
        }
        _ => {
            let hex = need(tests_log, "--tests-log-digest HEX")?;
            Verb::Session(SessionArgs {
                store: need(store, "--store DIR")?,
                pair: pair.ok_or_else(|| {
                    Fail::usage("aos-learn session: --pair I or --practice J is required")
                })?,
                tests_log: parse_digest(&hex).ok_or_else(|| {
                    Fail::usage(format!(
                        "aos-learn session: --tests-log-digest {hex}: not a 64-digit hex digest"
                    ))
                })?,
                threads,
                replay,
            })
        }
    })
}

/// `aterm-lab aos-learn VERB ...`. Every exit path, a refusal's included, checks that no held-out
/// item was labelled in this process (design §13.3): a non-zero `held_out_judged` is a defect.
/// # Errors
/// A usage error (exit 2), a named refusal or a defect (exit 1).
pub fn cmd_aos_learn(args: &[String]) -> Res {
    let out = if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        Ok(0)
    } else {
        parse(args).and_then(|verb| run(&verb))
    };
    guarded("aos-learn", out, held_out_judged())
}

/// A command's exit `out`, unless `read`, `held_out_judged` at that exit, is not 0: then the exit
/// is a defect (exit 1), whatever `out` was (design §13.3).
/// # Errors
/// `out`'s own error, or the defect.
pub fn guarded(command: &str, out: Res, read: u64) -> Res {
    if read != 0 {
        return Err(Fail::die(format!(
            "{command}: defect: held_out_judged reads {read}, not 0"
        )));
    }
    out
}

fn run(verb: &Verb) -> Res {
    match verb {
        Verb::Build => {
            print!("{}", build_text());
            Ok(0)
        }
        Verb::Init { store, pair } => init(store, *pair),
        Verb::Chain { threads } => print_chain(*threads),
        Verb::Train { store, threads } => train(store, *threads),
        Verb::Rollback { store, to } => rollback(store, *to),
        Verb::Verify { store, threads } => verify(store, *threads),
        Verb::Session(args) => session(args),
    }
}

/// `build`'s output: this lab's build and profile.
#[must_use]
pub fn build_text() -> String {
    format!("build {LAB_BUILD}\nprofile {}\n", env!("ATERM_LAB_PROFILE"))
}

/// The physical store directory `given` names, refused inside this repository's checkouts.
fn store_dir(verb: &str, given: &str) -> Result<PathBuf, Fail> {
    resolve_out(&from_caller(given), &checkouts(&paths().repo))
        .map_err(|e| Fail::usage(format!("aos-learn {verb}: --store {e}")))
}

/// Whether `dir` is absent or an empty directory.
fn is_new(dir: &Path) -> Result<bool, Fail> {
    match fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(Fail::die(format!(
            "aos-learn: cannot read {}: {e}",
            dir.display()
        ))),
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread asked for the account's home (tests).
    static HOMES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The account database's home, refused `NoHome` when it names none.
fn home(verb: &str) -> Result<&'static str, Fail> {
    #[cfg(test)]
    HOMES.with(|n| n.set(n.get() + 1));
    paths().need_home().map_err(|e| refused(verb, "NoHome", e))
}

/// The version's root, refused `NoHome` when the account database names no home.
fn root_for(verb: &str) -> Result<PathBuf, Fail> {
    home(verb).map(LabLog::version_root)
}

/// The head of `pair`'s ledger under `root`, read only and checked link by link.
fn head_of(verb: &str, root: &Path, pair: Pair) -> Result<Digest, Fail> {
    let raw = LabLog::read_only(root, pair)
        .lines()
        .map_err(|e| refused(verb, "Log", format!("{e:?}")))?;
    ledger_head(pair, &raw).map_err(|e| refused(verb, "Ledger", e))
}

/// A nonce from the system's random source, for a store's id.
fn nonce() -> Result<u64, Fail> {
    let mut bytes = [0_u8; 8];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| Fail::die(format!("aos-learn: cannot read /dev/urandom: {e}")))?;
    Ok(u64::from_le_bytes(bytes))
}

/// The pinned evaluator, refused when its baseline drifts.
fn evaluator(verb: &str) -> Result<Evaluator, Fail> {
    Evaluator::v1(LAB_BUILD, Instant::now()).map_err(|e| refused(verb, "Evaluator", e))
}

/// The store under `dir`, locked, loaded and checked for this build.
fn load(verb: &str, dir: PathBuf) -> Result<Store<LabStore>, Fail> {
    if !dir.join("store.txt").is_file() {
        return Err(refused(
            verb,
            "Store",
            format!("{} holds no store", dir.display()),
        ));
    }
    let io = LabStore::open(dir).map_err(|e| refused(verb, "StoreBusy", e))?;
    Store::load(io, LAB_BUILD).map_err(|e| refused(verb, "Store", e))
}

/// A new store's directory under `dir`, made and locked: `dir` must be absent or empty.
fn new_store(verb: &str, dir: &Path) -> Result<LabStore, Fail> {
    if !is_new(dir)? {
        return Err(Fail::usage(format!(
            "aos-learn {verb}: --store {} is not empty: a store is made new",
            dir.display()
        )));
    }
    make_dirs(dir).map_err(|e| {
        Fail::die(format!(
            "aos-learn {verb}: cannot make {}: {e}",
            dir.display()
        ))
    })?;
    LabStore::open(dir.to_path_buf()).map_err(|e| refused(verb, "StoreBusy", e))
}

fn init(given: &str, pair: Pair) -> Res {
    check_build(LAB_BUILD)?;
    let dir = store_dir("init", given)?;
    if !is_new(&dir)? {
        return Err(Fail::usage(format!(
            "aos-learn init: --store {} is not empty: a store is made new",
            dir.display()
        )));
    }
    let root = root_for("init")?;
    let head = head_of("init", &root, pair)?;
    let io = new_store("init", &dir)?;
    let store = Store::init(io, LAB_BUILD, pair, nonce()?, head)
        .map_err(|e| refused("init", "Store", e))?;
    println!(
        "aos-learn init: store {} for {}: v0 {} active",
        store.id().hex(),
        pair.name(),
        store.pointer().active.hex()
    );
    Ok(0)
}

fn print_chain(threads: usize) -> Res {
    let ev = evaluator("chain")?;
    let spread = Threads(threads);
    let members = chain(&Sets::v1(), &Labeller::new(&ev, &spread))
        .map_err(|e| refused("chain", "Learn", e))?;
    let pin: ChainPin = ChainPin::of(&members)
        .ok_or_else(|| refused("chain", "Learn", "the chain is not v0..v4"))?;
    print!("{}", pin_source(&pin));
    Ok(0)
}

/// `pin` as `M6_CHAIN_V1`'s source (stage E).
fn pin_source(pin: &ChainPin) -> String {
    let digests = |all: &[Digest]| {
        all.iter()
            .map(|d| format!("        Digest({}),\n", byte_array(d)))
            .collect::<String>()
    };
    format!(
        "// The chain v0..v4 on training data alone, from aos-learn chain at {LAB_BUILD}; its digest \
         is {}.\npub const M6_CHAIN_V1: Option<ChainPin> = Some(ChainPin {{\n    ids: [\n{}    ],\n    \
         labels: [\n{}    ],\n}});\n",
        pin.digest().hex(),
        digests(&pin.ids),
        digests(&pin.labels),
    )
}

/// `d`'s bytes as a Rust array literal.
fn byte_array(d: &Digest) -> String {
    let parts: Vec<String> = d.0.iter().map(|b| format!("0x{b:02x}")).collect();
    format!("[{}]", parts.join(", "))
}

fn train(given: &str, threads: usize) -> Res {
    let dir = store_dir("train", given)?;
    let mut store = load("train", dir)?;
    let root = root_for("train")?;
    let head = head_of("train", &root, store.pair())?;
    let ev = evaluator("train")?;
    let spread = Threads(threads);
    let start = Instant::now();
    let (v, moved) = store
        .train(&Sets::v1(), &Labeller::new(&ev, &spread), head)
        .map_err(|e| refused("train", "Store", e))?;
    let wall = start.elapsed().as_millis();
    let (retained, versions) = store
        .io()
        .retained()
        .map_err(|e| Fail::die(format!("aos-learn train: cannot size the store: {e}")))?;
    println!(
        "aos-learn train: round {} {} at the head",
        v.round(),
        v.id().hex()
    );
    print!(
        "{}",
        cost_report(
            "train",
            &moved.written,
            &moved.read,
            retained,
            versions,
            wall
        )
    );
    Ok(0)
}

fn rollback(given: &str, to: Digest) -> Res {
    let dir = store_dir("rollback", given)?;
    let mut store = load("rollback", dir)?;
    let root = root_for("rollback")?;
    let head = head_of("rollback", &root, store.pair())?;
    let start = Instant::now();
    let moved = store
        .rollback(to, head)
        .map_err(|e| refused("rollback", "Store", e))?;
    let wall = start.elapsed().as_millis();
    let (retained, versions) = store
        .io()
        .retained()
        .map_err(|e| Fail::die(format!("aos-learn rollback: cannot size the store: {e}")))?;
    print!(
        "{}",
        cost_report(
            "rollback",
            &moved.written,
            &moved.read,
            retained,
            versions,
            wall
        )
    );
    Ok(0)
}

/// The cost report of one transition (design §13.5), from file metadata after fsync; never scored.
fn cost_report(
    what: &str,
    written: &[(String, usize)],
    read: &[(String, usize)],
    retained: u64,
    versions: usize,
    wall_ms: u128,
) -> String {
    let sum = |files: &[(String, usize)]| files.iter().map(|(_, n)| n).sum::<usize>();
    format!(
        "aos-learn {what}: wrote {} bytes ({}); read {} bytes to validate ({} files); retained {retained} \
         bytes in {versions} versions; {wall_ms} ms MEASURED\n",
        sum(written),
        written
            .iter()
            .map(|(name, n)| format!("{name} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
        sum(read),
        read.len(),
    )
}

fn verify(given: &str, threads: usize) -> Res {
    let dir = store_dir("verify", given)?;
    let store = load("verify", dir)?;
    let root = root_for("verify")?;
    let head = head_of("verify", &root, store.pair())?;
    let ev = evaluator("verify")?;
    let spread = Threads(threads);
    let members = chain(&Sets::v1(), &Labeller::new(&ev, &spread))
        .map_err(|e| refused("verify", "Learn", e))?;
    let mut wrong = Vec::new();
    for (id, kept) in store.versions() {
        match members.iter().find(|m| m.id() == *id) {
            Some(member) if member == kept => {}
            Some(_) => wrong.push(format!("{} differs from its re-derived bytes", id.hex())),
            None => wrong.push(format!("{} is no chain member", id.hex())),
        }
    }
    if !wrong.is_empty() {
        return Err(refused("verify", "NotReproduced", wrong.join("; ")));
    }
    println!(
        "aos-learn verify: store {} for {}: {} versions re-derived byte for byte, {} revisions checked, \
         the ledger's head {}",
        store.id().hex(),
        store.pair().name(),
        store.versions().len(),
        store.revisions().len(),
        head.hex()
    );
    Ok(0)
}

/// A session child's summary (design §13.9), the runner's whole view of the child's run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The build the child ran.
    pub build: String,
    /// The profile it was built in (`debug` for a dev build).
    pub profile: String,
    /// `A`, or `B` for the replaying child.
    pub child: String,
    /// The pair's name.
    pub pair: String,
    /// The store's id, once made.
    pub store: Option<Digest>,
    /// `ok`, or the refusal, on one line.
    pub result: String,
    /// The looks' bits, `1` or `0`, in order: recorded (A) or replayed (B).
    pub bits: String,
    /// The active version at report time.
    pub mf: Option<Digest>,
    /// The round R2 rolled back to, if it was exercised.
    pub r2: Option<u8>,
    /// The report's digest, as the ledger's `report-done` holds it (A).
    pub report: Option<Digest>,
    /// Each criterion's line: the arm, the comparison and whether it is shown.
    pub criteria: Vec<String>,
    /// Each store transition's line: its name, the bytes written and the bytes read.
    pub moves: Vec<String>,
    /// The bytes the store retains and its version count.
    pub retained: Option<(u64, usize)>,
    /// The session's wall time, MEASURED.
    pub wall_ms: u128,
    /// `held_out_judged` at the end.
    pub held_out_judged: u64,
    /// The lifetime-maximum physical footprint, in bytes.
    pub footprint: Option<u64>,
}

impl Summary {
    /// Its text: the header, then one `key value` line each.
    #[must_use]
    pub fn text(&self) -> String {
        let opt = |d: &Option<Digest>| d.map_or_else(|| "none".to_owned(), |d| d.hex());
        let mut out = format!(
            "{SUMMARY}\nbuild {}\nprofile {}\nchild {}\npair {}\nstore {}\nresult {}\nbits {}\nmf {}\nr2 {}\nreport {}\n",
            self.build,
            self.profile,
            self.child,
            self.pair,
            opt(&self.store),
            self.result,
            if self.bits.is_empty() {
                "-"
            } else {
                &self.bits
            },
            opt(&self.mf),
            self.r2.map_or_else(|| "none".to_owned(), |r| r.to_string()),
            opt(&self.report),
        );
        for c in &self.criteria {
            out.push_str(&format!("criterion {c}\n"));
        }
        for m in &self.moves {
            out.push_str(&format!("move {m}\n"));
        }
        if let Some((bytes, versions)) = self.retained {
            out.push_str(&format!("retained {bytes} versions={versions}\n"));
        }
        out.push_str(&format!(
            "wall_ms {} MEASURED\nheld_out_judged {}\nfootprint {}\n",
            self.wall_ms,
            self.held_out_judged,
            self.footprint
                .map_or_else(|| "none".to_owned(), |f| f.to_string())
        ));
        out
    }

    /// The summary `text` holds: the header first, each key once, in order, nothing else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Summary> {
        let mut lines = text.lines();
        if lines.next()? != SUMMARY {
            return None;
        }
        let mut s = Summary::default();
        let mut field = |key: &str| -> Option<String> {
            let line = lines.next()?;
            line.strip_prefix(key)?.strip_prefix(' ').map(str::to_owned)
        };
        let digest = |v: String| -> Option<Option<Digest>> {
            if v == "none" {
                Some(None)
            } else {
                parse_digest(&v).map(Some)
            }
        };
        s.build = field("build")?;
        s.profile = field("profile")?;
        s.child = field("child")?;
        s.pair = field("pair")?;
        s.store = digest(field("store")?)?;
        s.result = field("result")?;
        let bits = field("bits")?;
        s.bits = if bits == "-" { String::new() } else { bits };
        if !s.bits.bytes().all(|b| b == b'0' || b == b'1') {
            return None;
        }
        s.mf = digest(field("mf")?)?;
        let r2 = field("r2")?;
        s.r2 = if r2 == "none" {
            None
        } else {
            Some(r2.parse().ok()?)
        };
        s.report = digest(field("report")?)?;
        let rest: Vec<&str> = text.lines().skip(11).collect();
        let mut at = 0;
        while let Some(c) = rest.get(at).and_then(|l| l.strip_prefix("criterion ")) {
            s.criteria.push(c.to_owned());
            at += 1;
        }
        while let Some(m) = rest.get(at).and_then(|l| l.strip_prefix("move ")) {
            s.moves.push(m.to_owned());
            at += 1;
        }
        if let Some(r) = rest.get(at).and_then(|l| l.strip_prefix("retained ")) {
            let (bytes, versions) = r.split_once(" versions=")?;
            s.retained = Some((bytes.parse().ok()?, versions.parse().ok()?));
            at += 1;
        }
        s.wall_ms = rest
            .get(at)?
            .strip_prefix("wall_ms ")?
            .strip_suffix(" MEASURED")?
            .parse()
            .ok()?;
        s.held_out_judged = rest
            .get(at + 1)?
            .strip_prefix("held_out_judged ")?
            .parse()
            .ok()?;
        let footprint = rest.get(at + 2)?.strip_prefix("footprint ")?;
        s.footprint = if footprint == "none" {
            None
        } else {
            Some(footprint.parse().ok()?)
        };
        (rest.len() == at + 3).then_some(s)
    }
}

/// `session`: child A of a session or, with `--replay`, child B (design §13.9). It prints its
/// summary on every exit path past its arguments, a refusal's included; the runner reads it.
fn session(a: &SessionArgs) -> Res {
    let start = Instant::now();
    let mut summary = Summary {
        build: LAB_BUILD.to_owned(),
        profile: env!("ATERM_LAB_PROFILE").to_owned(),
        child: if a.replay.is_some() { "B" } else { "A" }.to_owned(),
        pair: a.pair.name(),
        ..Summary::default()
    };
    let ran = session_run(a, &mut summary);
    summary.result = match &ran {
        Ok(()) => "ok".to_owned(),
        Err(f) => f.msg.clone().unwrap_or_default().replace('\n', " "),
    };
    summary.wall_ms = start.elapsed().as_millis();
    summary.held_out_judged = held_out_judged();
    summary.footprint = max_footprint();
    print!("{}", summary.text());
    ran.map(|()| 0)
}

fn session_run(a: &SessionArgs, summary: &mut Summary) -> Result<(), Fail> {
    let dir = store_dir("session", &a.store)?;
    if !is_new(&dir)? {
        return Err(Fail::usage(format!(
            "aos-learn session: --store {} is not empty: a session's store is new",
            dir.display()
        )));
    }
    let replayed = a
        .replay
        .as_deref()
        .map(|given| store_dir("session", given))
        .transpose()?;
    if replayed.as_ref() == Some(&dir) {
        return Err(Fail::usage(
            "aos-learn session: --replay names this session's own store",
        ));
    }
    let home = home("session")?;
    if paths().scratch_home() {
        return Err(refused(
            "session",
            "ScratchHome",
            "HOME is a scratch home, and a session's ledgers live under the account's own",
        ));
    }
    check_build(LAB_BUILD)?;
    let pin = M6_CHAIN_V1.ok_or_else(|| {
        refused(
            "session",
            "ChainMoved",
            "M6_CHAIN_V1 is not pinned (stage E)",
        )
    })?;
    let root = LabLog::version_root(home);
    LabLog::need_runner(&root).map_err(|e| refused("session", "NoRunner", e))?;
    let log = LabLog::open(&root, a.pair).map_err(|e| refused("session", "LedgerBusy", e))?;
    let mut ledger = match a.pair {
        Pair::Real(i) => Ledger::real(log, i, RealSets::new()),
        Pair::Practice(j) => Ledger::practice(log, j, pin),
    }
    .map_err(|e| refused("session", "Ledger", e))?;
    if let Some(replayed) = replayed {
        // Child B replays the session its ledger last opened, on child A's store, which must have
        // exited (it holds that store's lock while it runs).
        let first = load("session", replayed)?;
        let opened = ledger.lines().iter().rev().find_map(|l| match l {
            Line::Open {
                store,
                replay: false,
                ..
            } => Some(*store),
            _ => None,
        });
        if first.pair() != a.pair || opened != Some(first.id()) {
            return Err(refused(
                "session",
                "NotReplayed",
                "--replay names no store the pair's last session opened",
            ));
        }
    }
    let ev = evaluator("session")?;
    let sets = Sets::v1();
    let spread = Threads(a.threads);
    let env = SessionEnv {
        build: LAB_BUILD,
        sets: &sets,
        labeller: Labeller::new(&ev, &spread),
        chain: pin,
        tests_log: a.tests_log,
    };
    let io = new_store("session", &dir)?;
    let mut store = Store::init(io, LAB_BUILD, a.pair, nonce()?, ledger.head())
        .map_err(|e| refused("session", "Store", e))?;
    summary.store = Some(store.id());
    let run = if a.replay.is_some() {
        session_b(&env, &mut ledger, &mut store)
    } else {
        session_a(&env, &mut ledger, &mut store)
    };
    summary.retained = store.io().retained().ok();
    let run: SessionRun = run.map_err(|e| refused("session", "Session", e))?;
    record(summary, &run, ledger.lines());
    Ok(())
}

/// A finished child's figures into its summary.
fn record(summary: &mut Summary, run: &SessionRun, lines: &[Line]) {
    summary.bits = run
        .bits
        .iter()
        .map(|&b| if b { '1' } else { '0' })
        .collect();
    summary.mf = Some(run.mf);
    summary.r2 = run.r2;
    summary.report = lines.iter().find_map(|l| match l {
        Line::ReportDone { report } => Some(*report),
        _ => None,
    });
    if let Some(report) = &run.report {
        for (name, arm) in [("C1", Arm::U), ("C2", Arm::W2), ("C3", Arm::W1)] {
            let (c, shown) = report.criterion(arm);
            summary.criteria.push(format!(
                "{name} Mf-{} wins={} losses={} gain={}/{} against={}/{} {}",
                arm.name(),
                c.wins,
                c.losses,
                c.gain.0,
                c.gain.1,
                c.against.0,
                c.against.1,
                if shown.is_ok() { "shown" } else { "not-shown" }
            ));
        }
    }
    for (name, moved) in &run.moves {
        let sum = |files: &[(String, usize)]| files.iter().map(|(_, n)| n).sum::<usize>();
        summary.moves.push(format!(
            "{name} written={} read={}",
            sum(&moved.written),
            sum(&moved.read)
        ));
    }
}

#[cfg(test)]
#[path = "aos_learn_tests.rs"]
mod tests;
