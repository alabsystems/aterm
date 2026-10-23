// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TOOLCHAIN LEASES: a run holds the store build it resolved for exactly as long as it runs.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::layout::{COMPAT_DIR, STORE_DIR, VIEWS_DIR};

/// `<prefix>/leases` — every lease and gate file is a direct child of it.
pub const DIR: &str = "leases";

/// The suffix of a holder's lease file.
const LEASE_SUFFIX: &str = ".lease";
/// The suffix a holder's file carries while it is being laid.
const TAKING_SUFFIX: &str = ".taking";
/// The suffix of a subject's gate.
const GATE_SUFFIX: &str = ".gate";
/// The separator between a subject and its holder's `<pid>-<nonce>`.
const HOLDER_SEP: char = '@';
/// The most of a lease file's "who" line any reader takes.
const WHO_MAX: u64 = 512;
/// How often a holder re-tries a gate the reclaimer holds.
const GATE_POLL: Duration = Duration::from_millis(100);

/// How long [`take_for_dir`] waits for a reclaim in progress to finish before it gives up: a reclaim holds the gate across one build's delete, which for a multi-gigabyte toolchain is seconds, never minutes.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(120);

/// `<prefix>/leases`.
#[must_use]
pub fn dir(prefix: &Path) -> PathBuf {
    prefix.join(DIR)
}

/// What a lease is on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// `store/<program>/<build>` — and its exec root, which goes with it (`<prefix>/compat/<program>/<build>`, [`crate::layout`]).
    Build { program: String, build: u64 },
    /// `<prefix>/rustup/<name>` — the seam's view (`atpkg::seam::view_dir`), which the rustup `trust` toolchain resolves into.
    View { name: String },
}

/// A word a subject may carry: what every program and seam name is, and nothing that could
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Subject {
    /// A store build of `program`, or `None` for a name no store program carries.
    #[must_use]
    pub fn build(program: &str, build: u64) -> Option<Self> {
        is_word(program).then(|| Self::Build {
            program: program.to_string(),
            build,
        })
    }

    /// The seam view `name`, or `None` for a name no seam carries.
    #[must_use]
    pub fn view(name: &str) -> Option<Self> {
        is_word(name).then(|| Self::View {
            name: name.to_string(),
        })
    }

    /// The file-name stem every file of this subject starts with.
    #[must_use]
    pub fn stem(&self) -> String {
        match self {
            Self::Build { program, build } => format!("store-{program}-{build}"),
            Self::View { name } => format!("rustup-{name}"),
        }
    }

    /// [`Self::stem`] read back.
    #[must_use]
    pub fn parse_stem(stem: &str) -> Option<Self> {
        if let Some(rest) = stem.strip_prefix("store-") {
            let (program, build) = rest.rsplit_once('-')?;
            if build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            return Self::build(program, build.parse().ok()?);
        }
        Self::view(stem.strip_prefix("rustup-")?)
    }

    /// How a person reads it: `trust build 9192`, `the rustup trust toolchain`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Build { program, build } => format!("{program} build {build}"),
            Self::View { name } => format!("the rustup {name} toolchain"),
        }
    }

    /// Where the subject lives under the store prefix `prefix` (atpkg hands its `Layout`, which is its prefix): `store/<program>/<build>` or `rustup/<name>` ([`crate::layout`]).
    #[must_use]
    pub fn path(&self, prefix: impl AsRef<Path>) -> PathBuf {
        let prefix = prefix.as_ref();
        match self {
            Self::Build { program, build } => crate::layout::build_dir(prefix, program, *build),
            Self::View { name } => crate::layout::view_dir(prefix, name),
        }
    }

    /// The subject a toolchain DIRECTORY belongs to — a store build's (`store/<p>/<n>/…`), its exec root's (`compat/<p>/<n>/…`, which is that build's), or a seam view's (`rustup/<name>/…`) — or `None` for a directory atpkg does not manage (a checkout, a hand-made rustup toolchain): nothing of atpkg's ever reclaims one of those.
    #[must_use]
    pub fn of_dir(prefix: &Path, dir: &Path) -> Option<Self> {
        let prefixes = [
            Some(prefix.to_path_buf()),
            std::fs::canonicalize(prefix).ok(),
        ];
        let dirs = [Some(dir.to_path_buf()), std::fs::canonicalize(dir).ok()];
        for p in prefixes.iter().flatten() {
            for d in dirs.iter().flatten() {
                let Ok(rel) = d.strip_prefix(p) else {
                    continue;
                };
                let mut parts = rel.components().filter_map(|c| match c {
                    std::path::Component::Normal(s) => s.to_str(),
                    _ => None,
                });
                let found = match (parts.next(), parts.next(), parts.next()) {
                    (Some(STORE_DIR | COMPAT_DIR), Some(program), Some(build))
                        if build.bytes().all(|b| b.is_ascii_digit()) =>
                    {
                        build.parse().ok().and_then(|b| Self::build(program, b))
                    }
                    (Some(VIEWS_DIR), Some(name), _) => Self::view(name),
                    _ => None,
                };
                if found.is_some() {
                    return found;
                }
            }
        }
        None
    }
}

/// A held lease.
#[derive(Debug)]
pub struct Lease {
    file: crate::record_lock::RecordLock,
    path: PathBuf,
    subject: Subject,
}

impl Lease {
    /// What the lease is on.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// The holder's file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.file.unlock();
    }
}

/// Why a lease could not be taken.
#[derive(Debug)]
pub enum LeaseError {
    /// A reclaim held the subject's gate for longer than the caller would wait.
    Busy(Subject),
    /// The lease directory or a file in it could not be made or locked.
    Io(io::Error),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(subject) => write!(
                f,
                "gc was reclaiming {} and did not finish in time",
                subject.describe()
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LeaseError {}

impl From<io::Error> for LeaseError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// `<prefix>/leases`, made if absent — private (`0700`) on Unix, since a lease names the
fn ensure_dir(prefix: &Path) -> io::Result<PathBuf> {
    let d = dir(prefix);
    match std::fs::symlink_metadata(&d) {
        Ok(m) if m.is_dir() => return Ok(d),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a directory", d.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(&d)
    };
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().create(&d);
    match created {
        Ok(()) => Ok(d),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(d),
        Err(e) => Err(e),
    }
}

/// Open (creating) a lease-directory file for locking: `0600`, never truncated — a lock
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

/// The "who" line a holder writes: the first line of `who`, cut on a character boundary
fn who_line(who: &str) -> String {
    let first = who.lines().next().unwrap_or_default();
    let limit = usize::try_from(WHO_MAX).map_or(0, |max| max.saturating_sub(1));
    let mut end = first.len().min(limit);
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
}

/// A process-unique `<pid>-<nonce>` for one holder's file name.
fn holder_tag() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{}-{nanos:x}{n:x}", std::process::id())
}

/// Take a lease on `subject` under `prefix`, saying `who` holds it (one line — a program, a pid, what it is doing).
/// # Errors
/// [`LeaseError::Busy`] when the gate stayed held past `wait`; [`LeaseError::Io`] when the directory or a file in it could not be made, written or locked.
pub fn take(
    prefix: &Path,
    subject: &Subject,
    who: &str,
    wait: Duration,
) -> Result<Lease, LeaseError> {
    let d = ensure_dir(prefix)?;
    let stem = subject.stem();
    let gate: std::fs::File = open_rendezvous(&d.join(format!("{stem}{GATE_SUFFIX}")))?;
    let deadline = Instant::now() + wait;
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the gate, SHARED: let go by unlock() once the lease is laid"
        )]
        let taken = gate.try_lock_shared();
        match taken {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(GATE_POLL);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(LeaseError::Busy(subject.clone()));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    let tag = holder_tag();
    let taking = d.join(format!("{stem}{HOLDER_SEP}{tag}{TAKING_SUFFIX}"));
    let path = d.join(format!("{stem}{HOLDER_SEP}{tag}{LEASE_SUFFIX}"));
    let laid = (|| -> io::Result<crate::record_lock::RecordLock> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file: std::fs::File = options.open(&taking)?;
        let mut file =
            crate::record_lock::RecordLock::try_lock(file, WHO_MAX).map_err(|e| match e {
                std::fs::TryLockError::Error(e) => e,
                std::fs::TryLockError::WouldBlock => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "a lease file this process just created was already locked",
                ),
            })?;
        let mut line = who_line(who);
        line.push('\n');
        let laid = file
            .write_all(line.as_bytes())
            .and_then(|()| std::fs::rename(&taking, &path));
        if let Err(e) = laid {
            file.unlock();
            return Err(e);
        }
        Ok(file)
    })();
    let _ = gate.unlock();
    match laid {
        Ok(file) => Ok(Lease {
            file,
            path,
            subject: subject.clone(),
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&taking);
            Err(e.into())
        }
    }
}

/// [`take`] for the toolchain directory a run resolved: the subject it belongs to ([`Subject::of_dir`]) under the prefix atpkg resolves, `Ok(None)` when it belongs to none (nothing of atpkg's can reclaim it, so there is nothing to hold).
/// # Errors
/// [`take`]'s.
pub fn take_for_dir(
    prefix: &Path,
    toolchain_dir: &Path,
    who: &str,
) -> Result<Option<Lease>, LeaseError> {
    let Some(subject) = Subject::of_dir(prefix, toolchain_dir) else {
        return Ok(None);
    };
    take(prefix, &subject, who, DEFAULT_WAIT).map(Some)
}

/// What the holder verb ([`hold_for`]) holds while the command it guards runs.
#[derive(Debug)]
pub enum Hold {
    /// The lease, taken, on a subject that is still there.
    Held(Lease),
    /// The directory is none of atpkg's ([`Subject::of_dir`]): nothing of atpkg's reclaims or re-lays it, so there is nothing to hold.
    Unmanaged,
    /// The lease could not be taken; the command runs exactly as exposed as it would have without one — the rule the cutter keeps too.
    NotTaken(Subject, LeaseError),
}

/// The directory a holder was asked to hold is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub dir: PathBuf,
    pub managed: bool,
}

/// THE HOLDER VERB's lease (`aterm pkg lease <dir> -- <command…>`): the subject `dir` belongs to, leased as `who` under `prefix` for as long as the returned [`Hold`] lives, and `dir` checked to be there AFTER the lease is laid — the check [`take`] leaves to its caller, since only the caller knows what it resolved.
/// # Errors
/// [`Missing`] when `dir` is not there once the lease is laid (or could not be): the command would run on a toolchain that is gone, so the holder runs nothing.
pub fn hold_for(prefix: &Path, dir: &Path, who: &str, wait: Duration) -> Result<Hold, Missing> {
    let there = || std::fs::symlink_metadata(dir).is_ok();
    let Some(subject) = Subject::of_dir(prefix, dir) else {
        return if there() {
            Ok(Hold::Unmanaged)
        } else {
            Err(Missing {
                dir: dir.to_path_buf(),
                managed: false,
            })
        };
    };
    let held = match take(prefix, &subject, who, wait) {
        Ok(lease) => Hold::Held(lease),
        Err(e) => Hold::NotTaken(subject, e),
    };
    if there() {
        Ok(held)
    } else {
        Err(Missing {
            dir: dir.to_path_buf(),
            managed: true,
        })
    }
}

/// Who holds a subject, as far as the lease files can say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holders {
    /// Nobody: no live holder's file.
    Free,
    /// Each live holder's "who" line.
    Held(Vec<String>),
    /// The lease directory or a file in it could not be read or tested — the caller treats the subject as held (the fail-safe direction: disk is spent before a run is broken).
    Unknown(String),
}

impl Holders {
    /// Whether the subject must be treated as in use.
    #[must_use]
    pub fn in_use(&self) -> bool {
        !matches!(self, Self::Free)
    }

    /// One clause for a person, after `<subject> is`: `in use by <who>[; <who>…]`, or that it is treated as in use because its lease could not be read.
    #[must_use]
    pub fn clause(&self) -> String {
        match self {
            Self::Free => "not in use".to_string(),
            Self::Held(who) => format!("in use by {}", who.join("; ")),
            Self::Unknown(why) => format!("treated as in use (its lease is unreadable: {why})"),
        }
    }
}

/// Test one holder's file: `Ok(Some(who))` live, `Ok(None)` a dead holder's (removed when
fn probe_file(path: &Path, reap: bool) -> io::Result<Option<String>> {
    let mut file: std::fs::File = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe: a dead holder's file it locks is let go by unlock() before its close"
    )]
    let taken = file.try_lock();
    match taken {
        Ok(()) => {
            if reap {
                let _ = std::fs::remove_file(path);
            }
            let _ = file.unlock();
            Ok(None)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut who = String::new();
            let _ = (&mut file).take(WHO_MAX).read_to_string(&mut who);
            let who = who.lines().next().unwrap_or_default().trim().to_string();
            Ok(Some(if who.is_empty() {
                "a run that named itself nothing".to_string()
            } else {
                who
            }))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// The holders of `subject`, reaping dead holders' files when `reap`.
fn scan(prefix: &Path, subject: &Subject, reap: bool) -> Holders {
    let entries = match std::fs::read_dir(dir(prefix)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Holders::Free,
        Err(e) => return Holders::Unknown(format!("{}: {e}", dir(prefix).display())),
    };
    let mut head = subject.stem();
    head.push(HOLDER_SEP);
    let mut live = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return Holders::Unknown(e.to_string()),
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&head) || !name.ends_with(LEASE_SUFFIX) {
            continue;
        }
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match probe_file(&entry.path(), reap) {
            Ok(Some(who)) => live.push(who),
            Ok(None) => {}
            Err(e) => return Holders::Unknown(format!("{name}: {e}")),
        }
    }
    if live.is_empty() {
        Holders::Free
    } else {
        live.sort();
        Holders::Held(live)
    }
}

/// Who holds `subject` now.
#[must_use]
pub fn holders(prefix: &Path, subject: &Subject) -> Holders {
    scan(prefix, subject, false)
}

/// Every subject with at least one live holder, and who — what `aterm pkg doctor` prints.
/// # Errors
/// The listing's error (never `NotFound`, which is no leases).
pub fn live(prefix: &Path) -> io::Result<Vec<(Subject, Holders)>> {
    let entries = match std::fs::read_dir(dir(prefix)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut subjects = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(stem) = name
            .strip_suffix(LEASE_SUFFIX)
            .and_then(|n| n.split_once(HOLDER_SEP))
            .map(|(stem, _)| stem)
            && let Some(subject) = Subject::parse_stem(stem)
        {
            subjects.insert(subject);
        }
    }
    Ok(subjects
        .into_iter()
        .map(|s| {
            let h = holders(prefix, &s);
            (s, h)
        })
        .filter(|(_, h)| h.in_use())
        .collect())
}

/// A subject's gate held EXCLUSIVE: while it stands no lease on the subject can be laid, so what the reclaimer read of the leases stays true until it drops this.
#[derive(Debug)]
pub struct ReclaimGuard {
    gate: File,
}

impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        let _ = self.gate.unlock();
    }
}

/// What [`reclaim`] found.
#[derive(Debug)]
pub enum Reclaim {
    /// No lease: the subject may go, and nothing can lease it while the guard stands.
    Clear(ReclaimGuard),
    /// Leased, or being leased this moment, or its leases could not be read: keep it.
    Keep(Holders),
}

/// Ask to reclaim `subject`: the gate taken exclusive WITHOUT waiting (a holder laying its lease this moment keeps the subject), then its leases read, dead holders' reaped.
#[must_use]
pub fn reclaim(prefix: &Path, subject: &Subject) -> Reclaim {
    let d = match ensure_dir(prefix) {
        Ok(d) => d,
        Err(e) => return Reclaim::Keep(Holders::Unknown(e.to_string())),
    };
    let gate_path = d.join(format!("{}{GATE_SUFFIX}", subject.stem()));
    let gate: std::fs::File = match open_rendezvous(&gate_path) {
        Ok(f) => f,
        Err(e) => return Reclaim::Keep(Holders::Unknown(e.to_string())),
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the gate goes to a ReclaimGuard, whose Drop releases by LOCK_UN, or is \
                  let go by unlock() below"
    )]
    let taken = gate.try_lock();
    match taken {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Reclaim::Keep(Holders::Held(vec![
                "a run taking a lease on it right now".to_string(),
            ]));
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Reclaim::Keep(Holders::Unknown(e.to_string()));
        }
    }
    match scan(prefix, subject, true) {
        Holders::Free => Reclaim::Clear(ReclaimGuard { gate }),
        other => {
            let _ = gate.unlock();
            Reclaim::Keep(other)
        }
    }
}

/// Sweep the lease directory: a dead holder's lease file, a `.taking` file a holder died laying, and the gate of a subject that no longer exists (`exists` answers for a subject; a gate whose stem names no subject is swept too).
pub fn sweep(prefix: &Path, exists: &dyn Fn(&Subject) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir(prefix)) else {
        return;
    };
    let mut gates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if name.ends_with(LEASE_SUFFIX) {
            let _ = probe_file(&entry.path(), true);
        } else if let Some(laying) = name.strip_suffix(TAKING_SUFFIX) {
            reap_taking(prefix, laying, &entry.path());
        } else if let Some(stem) = name.strip_suffix(GATE_SUFFIX) {
            gates.push((stem.to_string(), entry.path()));
        }
    }
    for (stem, path) in gates {
        let subject = Subject::parse_stem(&stem);
        if subject.as_ref().is_some_and(exists) {
            continue;
        }
        let Ok(gate) = File::open(&path) else {
            continue;
        };
        let gate: std::fs::File = gate;
        #[expect(
            clippy::disallowed_methods,
            reason = "a gate it locks is let go by unlock() below, before its close"
        )]
        let taken = gate.try_lock();
        if taken.is_err() {
            continue;
        }
        let free = subject
            .as_ref()
            .is_none_or(|s| matches!(scan(prefix, s, true), Holders::Free));
        if free {
            let _ = std::fs::remove_file(&path);
        }
        let _ = gate.unlock();
    }
}

/// [`sweep`]'s arm for a `.taking` file (`laying` is its name without the suffix): reaped
fn reap_taking(prefix: &Path, laying: &str, path: &Path) {
    let stem = laying
        .split_once(HOLDER_SEP)
        .map_or(laying, |(stem, _)| stem);
    let gate = match File::open(dir(prefix).join(format!("{stem}{GATE_SUFFIX}"))) {
        Ok(gate) => Some(gate),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return,
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the gate it locks is let go by unlock() below, before its close"
    )]
    let refused = gate.as_ref().is_some_and(|gate| gate.try_lock().is_err());
    if refused {
        return;
    }
    let _ = probe_file(path, true);
    if let Some(gate) = gate {
        let _ = gate.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch prefix, removed when the test ends however it ends: the
    struct Prefix(
        PathBuf,
        #[allow(dead_code, reason = "held for its drop")] aterm_tempfile::TempDir,
    );

    impl std::ops::Deref for Prefix {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl AsRef<Path> for &Prefix {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    fn prefix(label: &str) -> Prefix {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atc-lease-{label}-"))
            .tempdir()
            .expect("a scratch prefix");
        Prefix(dir.path().to_path_buf(), dir)
    }

    fn trust(build: u64) -> Subject {
        Subject::build("trust", build).unwrap()
    }

    /// The file names every lease reader and writer agree on.
    #[test]
    fn subjects_spell_and_read_back() {
        assert_eq!(trust(9192).stem(), "store-trust-9192");
        assert_eq!(
            Subject::build("trust-mc", 7).unwrap().stem(),
            "store-trust-mc-7"
        );
        assert_eq!(Subject::view("trust").unwrap().stem(), "rustup-trust");
        for s in [
            trust(9192),
            Subject::build("trust-mc", 7).unwrap(),
            Subject::view("trust-dev").unwrap(),
        ] {
            assert_eq!(Subject::parse_stem(&s.stem()), Some(s));
        }
        for bad in [
            "store-trust",
            "store--9",
            "store-trust-x9",
            "rustup-",
            "other-x",
        ] {
            assert_eq!(Subject::parse_stem(bad), None, "{bad}");
        }
        assert!(Subject::build("a/b", 1).is_none());
        assert!(Subject::build("a.b", 1).is_none());
        assert!(Subject::view("").is_none());
    }

    #[test]
    fn a_toolchain_dir_names_its_subject() {
        let p = prefix("of-dir");
        let bin = p.join("store/trust/9192/bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(Subject::of_dir(&p, &bin), Some(trust(9192)));
        let root = p.join("compat/trust/9192/bin");
        assert_eq!(Subject::of_dir(&p, &root), Some(trust(9192)));
        let view = p.join("rustup/trust/bin");
        assert_eq!(Subject::of_dir(&p, &view), Subject::view("trust"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(p.join("store/trust/9192"), p.join("store/trust/current"))
                .unwrap();
            assert_eq!(
                Subject::of_dir(&p, &p.join("store/trust/current/bin")),
                Some(trust(9192))
            );
        }
        assert_eq!(
            Subject::of_dir(&p, Path::new("/opt/trust/stage2/bin")),
            None
        );
        assert_eq!(Subject::of_dir(&p, &p.join("store/trust")), None);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// A live lease is seen with its holder's words; dropping it frees the subject, and a
    #[test]
    fn a_lease_is_held_until_dropped() {
        let p = prefix("held");
        assert_eq!(holders(&p, &trust(1)), Holders::Free);
        let lease = take(&p, &trust(1), "aterm-release (pid 7) in /w", DEFAULT_WAIT).unwrap();
        assert_eq!(lease.subject(), &trust(1));
        assert_eq!(
            holders(&p, &trust(1)),
            Holders::Held(vec!["aterm-release (pid 7) in /w".to_string()])
        );
        assert_eq!(
            holders(&p, &trust(2)),
            Holders::Free,
            "another build is free"
        );
        let second = take(&p, &trust(1), "aterm-release cut", DEFAULT_WAIT).unwrap();
        let Holders::Held(who) = holders(&p, &trust(1)) else {
            panic!("two holders");
        };
        assert_eq!(who.len(), 2);
        assert_eq!(live(&p).unwrap().len(), 1);
        drop(lease);
        drop(second);
        assert_eq!(holders(&p, &trust(1)), Holders::Free);
        assert!(live(&p).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&p);
    }

    /// A holder that died without dropping (its lock released by the kernel, its file left
    #[test]
    fn a_dead_holders_file_holds_nothing_and_is_reaped() {
        let p = prefix("dead");
        let lease = take(&p, &trust(3), "gone", DEFAULT_WAIT).unwrap();
        let path = lease.path().to_path_buf();
        lease.file.unlock();
        std::mem::forget(lease);
        assert!(path.exists());
        assert_eq!(holders(&p, &trust(3)), Holders::Free);
        assert!(path.exists(), "a read never reaps");
        let Reclaim::Clear(guard) = reclaim(&p, &trust(3)) else {
            panic!("a dead holder keeps nothing");
        };
        assert!(!path.exists(), "the reclaim reaped it");
        drop(guard);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// The race the gate exists for: while a reclaim holds it, a holder waits (and gives up
    #[test]
    fn the_gate_orders_a_holder_against_a_reclaim() {
        let p = prefix("gate");
        let lease = take(&p, &trust(4), "gate run", DEFAULT_WAIT).unwrap();
        match reclaim(&p, &trust(4)) {
            Reclaim::Keep(Holders::Held(who)) => assert_eq!(who, ["gate run"]),
            other => panic!("a leased build is kept: {other:?}"),
        }
        drop(lease);
        let Reclaim::Clear(guard) = reclaim(&p, &trust(4)) else {
            panic!("free once released");
        };
        match take(&p, &trust(4), "late", Duration::from_millis(250)) {
            Err(LeaseError::Busy(s)) => assert_eq!(s, trust(4)),
            other => panic!("a holder waits out a reclaim: {other:?}"),
        }
        assert!(matches!(reclaim(&p, &trust(4)), Reclaim::Keep(_)));
        drop(guard);
        let after = take(&p, &trust(4), "after", aterm_spec::wait::MUST_HAPPEN).unwrap();
        drop(after);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// The sweep takes dead holders' files, `.taking` debris and the gates of subjects that
    #[test]
    fn the_sweep_takes_only_the_dead() {
        let p = prefix("sweep");
        let kept = take(&p, &trust(6), "live", DEFAULT_WAIT).unwrap();
        let dead = take(&p, &trust(7), "dead", DEFAULT_WAIT).unwrap();
        let dead_path = dead.path().to_path_buf();
        dead.file.unlock();
        std::mem::forget(dead);
        let debris = dir(&p).join("store-trust-8@1-1.taking");
        std::fs::write(&debris, b"x\n").unwrap();
        let exists = |s: &Subject| *s == trust(6) || *s == trust(8);
        sweep(&p, &exists);
        assert!(kept.path().exists(), "a live holder's file stays");
        assert!(!dead_path.exists(), "a dead holder's goes");
        assert!(!debris.exists(), "a dead `.taking` goes");
        let names: Vec<String> = std::fs::read_dir(dir(&p))
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(GATE_SUFFIX))
            .collect();
        assert_eq!(
            names,
            ["store-trust-6.gate"],
            "only a live subject's gate stays"
        );
        drop(kept);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// A holder MID-LAY — its subject's gate held shared, its `.taking` created and not yet
    #[test]
    fn the_sweep_never_takes_a_lease_being_laid() {
        let p = prefix("sweep-laying");
        let d = ensure_dir(&p).unwrap();
        let gate = open_rendezvous(&d.join("store-trust-6.gate")).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the test's stand-in holder: it lets go of the gate by unlock() below"
        )]
        gate.try_lock_shared().unwrap();
        let laying = d.join("store-trust-6@1-1.taking");
        std::fs::write(&laying, b"").unwrap();
        sweep(&p, &|_| true);
        assert!(
            laying.exists(),
            "a `.taking` under a held gate is a live holder's"
        );
        gate.unlock().unwrap();
        sweep(&p, &|_| true);
        assert!(!laying.exists(), "with the gate free it is a dead holder's");
        let orphan = d.join("store-trust-9@1-1.taking");
        std::fs::write(&orphan, b"").unwrap();
        sweep(&p, &|_| true);
        assert!(!orphan.exists());
        let _ = std::fs::remove_dir_all(&p);
    }

    /// The who line is one line, cut on a character boundary under the reader's bound.
    #[test]
    fn the_who_line_is_one_bounded_line() {
        assert_eq!(who_line("a\nb"), "a");
        let long = "é".repeat(600);
        let line = who_line(&long);
        assert!(line.len() < usize::try_from(WHO_MAX).unwrap());
        assert!(line.chars().all(|c| c == 'é'));
    }

    /// An unreadable lease directory is never read as free.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_lease_directory_is_not_free() {
        use std::os::unix::fs::PermissionsExt as _;
        let p = prefix("unreadable");
        let l = take(&p, &trust(9), "x", DEFAULT_WAIT).unwrap();
        drop(l);
        std::fs::set_permissions(dir(&p), std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(dir(&p)).is_ok();
        let seen = holders(&p, &trust(9));
        std::fs::set_permissions(dir(&p), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            !readable,
            "NOT RUN as written: this process reads a 0000 directory (root?)"
        );
        assert!(matches!(seen, Holders::Unknown(_)), "{seen:?}");
        assert!(seen.in_use());
        let _ = std::fs::remove_dir_all(&p);
    }
}
