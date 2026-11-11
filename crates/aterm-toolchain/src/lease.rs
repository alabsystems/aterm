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
/// read as a separator or a path.
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
        (is_word(program) && crate::layout::is_valid_build_id(build)).then(|| Self::Build {
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
            if build.is_empty()
                || (build.len() > 1 && build.starts_with('0'))
                || !build.bytes().all(|b| b.is_ascii_digit())
            {
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

    /// The subject a toolchain DIRECTORY belongs to — a store build's (`store/<p>/<n>/…`), its exec root's (`compat/<p>/<n>/…`, which is that build's), or a seam view's (`rustup/<name>/…`, or `rustup/<name>.noindex/…` once laid aside) — or `None` for a directory atpkg does not manage (a checkout, a hand-made rustup toolchain): nothing of atpkg's ever reclaims one of those.
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
                if rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    continue;
                }
                let mut parts = rel.components().filter_map(|c| match c {
                    std::path::Component::Normal(s) => s.to_str(),
                    _ => None,
                });
                let found = match (parts.next(), parts.next(), parts.next()) {
                    (Some(STORE_DIR | COMPAT_DIR), Some(program), Some(raw_build)) => {
                        let build = raw_build
                            .strip_suffix(crate::build_cache::NOINDEX)
                            .unwrap_or(raw_build);
                        (build.bytes().all(|b| b.is_ascii_digit())
                            && !build.is_empty()
                            && !(build.len() > 1 && build.starts_with('0')))
                        .then(|| build.parse().ok().and_then(|b| Self::build(program, b)))
                        .flatten()
                    }
                    (Some(VIEWS_DIR), Some(name), _) => Self::view(
                        name.strip_suffix(crate::build_cache::NOINDEX)
                            .unwrap_or(name),
                    ),
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
/// On Unix, every actual duplicate keeps the named slot in use until its final
/// close. Reclaim or sweep removes the slot only after probing its kernel lock.
#[derive(Debug)]
pub struct Lease {
    #[cfg(unix)]
    file: crate::record_lock::CloseOnlyLock,
    #[cfg(not(unix))]
    file: crate::record_lock::RecordLock,
    path: PathBuf,
    subject: Subject,
}

impl Lease {
    /// What the lease is on.
    #[must_use]
    pub const fn subject(&self) -> &Subject {
        &self.subject
    }

    /// The holder's file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Duplicate the original held lease for explicit child inheritance.
    /// The returned close-on-exec descriptor shares the same close-only lock;
    /// dropping this Lease cannot permit GC until every duplicate has closed.
    /// Recipients must preserve close-only handling: never unlock or relock it.
    /// This checks custody only, never SDK content authenticity or immutability.
    ///
    /// # Errors
    /// The original named/held lease is unsafe or changed, or duplication fails.
    #[cfg(unix)]
    pub fn try_clone_descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        self.check_current()?;
        let descriptor = self.file.try_clone_fd()?;
        self.check_current()?;
        Ok(descriptor)
    }

    /// Adopt an explicitly inherited lease descriptor for `toolchain_dir` under the
    /// independently selected store `prefix`.
    ///
    /// `leaf` is only a bounded locator. The subject is derived from `prefix` and
    /// `toolchain_dir`, and the accepted leaf must be one this lease implementation can have
    /// generated for that exact subject. No environment-supplied path or subject grants lease
    /// custody.
    ///
    /// The owned descriptor is consumed on every answer, including a refusal before or after the
    /// lock boundary. Callers never recover or close its former raw number.
    ///
    /// # Safety
    /// Call before threads or callbacks can close, replace or otherwise use `fd`'s raw number,
    /// with the exclusive startup custody required by
    /// [`crate::record_lock::CloseOnlyLock::adopt_inherited`]. `prefix` and `toolchain_dir`
    /// must have been selected independently of the inherited descriptor and its environment
    /// locator.
    ///
    /// # Errors
    /// The store paths or leaf do not identify one exact managed subject, the inherited
    /// description is not its original held lease, or the current named lease is unsafe.
    #[cfg(unix)]
    pub unsafe fn adopt_inherited(
        fd: std::os::fd::OwnedFd,
        prefix: &Path,
        toolchain_dir: &Path,
        leaf: &str,
    ) -> io::Result<Self> {
        // SAFETY: this public boundary carries the caller's startup and path-selection contract.
        unsafe { Self::adopt_inherited_with(fd, prefix, toolchain_dir, leaf, |_| {}) }
    }

    #[cfg(unix)]
    unsafe fn adopt_inherited_with(
        fd: std::os::fd::OwnedFd,
        prefix: &Path,
        toolchain_dir: &Path,
        leaf: &str,
        after_lock: impl FnOnce(&Path),
    ) -> io::Result<Self> {
        use std::os::fd::{FromRawFd as _, IntoRawFd as _};

        if !prefix.is_absolute() || !toolchain_dir.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lease adoption needs absolute independently selected store paths",
            ));
        }
        let subject = Subject::of_dir(prefix, toolchain_dir).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "toolchain directory does not name a managed lease subject",
            )
        })?;
        if !inherited_leaf_matches(&subject, leaf) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inherited lease leaf does not name the selected toolchain subject",
            ));
        }
        let path = dir(prefix).join(leaf);
        let raw = fd.into_raw_fd();
        // SAFETY: the caller supplies exclusive startup custody. The expected path is derived
        // from the independently selected prefix and the strictly validated subject leaf. The
        // lower boundary consumes `raw` exactly on success and leaves it open on every error.
        let file = match unsafe { crate::record_lock::CloseOnlyLock::adopt_inherited(raw, &path) } {
            Ok(file) => file,
            Err(error) => {
                // SAFETY: CloseOnlyLock's refusal contract leaves this exclusively offered raw
                // number open. Reconstructing its sole owner makes this API consume it on error.
                drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) });
                return Err(error);
            }
        };
        after_lock(&path);
        let lease = Self {
            file,
            path,
            subject,
        };
        lease.check_current()?;
        Ok(lease)
    }

    /// Check that the named slot still denotes this actual held description.
    /// This checks lease custody, not SDK authenticity or immutable SDK bytes.
    /// Failure leaves the original lock held and never adopts a replacement.
    ///
    /// # Errors
    /// The held or named file cannot be read, their identities differ, required
    /// private regular-file properties changed, or this platform is unsupported.
    pub fn check_current(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let held = self.file.metadata()?;
            let named = std::fs::symlink_metadata(&self.path)?;
            // SAFETY: geteuid observes this process's effective uid without memory arguments.
            let uid = unsafe { libc::geteuid() };
            if held.dev() != named.dev() || held.ino() != named.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "original lease slot changed",
                ));
            }
            for metadata in [&held, &named] {
                if !metadata.is_file()
                    || metadata.nlink() != 1
                    || metadata.uid() != uid
                    || metadata.mode() & 0o022 != 0
                    || metadata.len() > WHO_MAX
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "original lease slot is unsafe",
                    ));
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "checked SDK lease identity requires Unix",
            ))
        }
    }
}

#[cfg(unix)]
fn inherited_leaf_matches(subject: &Subject, leaf: &str) -> bool {
    if leaf.len() > 192 {
        return false;
    }
    let head = format!("{}{HOLDER_SEP}", subject.stem());
    let Some(tag) = leaf
        .strip_prefix(&head)
        .and_then(|name| name.strip_suffix(LEASE_SUFFIX))
    else {
        return false;
    };
    let Some((pid, nonce)) = tag.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid != "0"
        && !(pid.len() > 1 && pid.starts_with('0'))
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !nonce.is_empty()
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// How long [`Lease::release`] waits for the lease to be let go: a hang detector, not a budget.
#[cfg(any(test, debug_assertions))]
const LET_GO_HANG: Duration = Duration::from_secs(60);

#[cfg(any(test, debug_assertions))]
impl Lease {
    /// Test seam: let the lease go, and return once it is let go ([`let_go_within`], under a
    /// one-minute hang detector). A lease never offered to a child is let go by `LOCK_UN` at its
    /// drop; one whose descriptor was offered out (on Unix, close-only custody) is let go by the
    /// last close of a copy, so a test that asks what letting such a lease go changed asks after
    /// this, not after a drop.
    ///
    /// # Panics
    /// It was still held when the hang detector ended, or its file could not be tested.
    pub fn release(self) {
        let path = self.path.clone();
        drop(self);
        if let Err(why) = let_go_within(&path, LET_GO_HANG) {
            panic!("the lease {} was not let go: {why}", path.display());
        }
    }
}

/// Test seam: wait up to `within` until nothing holds the lease whose holder's file is `path`
/// (no copy of its descriptor is open, as reclaim tests it), looking again after a 1 ms slice.
///
/// # Errors
/// Who still held it once `within` had passed, or why its file could not be tested.
#[cfg(any(test, debug_assertions))]
pub fn let_go_within(path: &Path, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within;
    loop {
        match probe_file(path, false) {
            Ok(None) => return Ok(()),
            Ok(Some(who)) if Instant::now() >= deadline => {
                return Err(format!("still held by {who}"));
            }
            Ok(Some(_)) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(format!("its file could not be tested: {e}")),
        }
    }
}

#[cfg(not(unix))]
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

/// `<prefix>/leases`, made if absent — private (`0700`) and owned by this user on Unix, since
/// a lease names the holder's working directory.
fn ensure_dir(prefix: &Path) -> io::Result<PathBuf> {
    let d = dir(prefix);
    match std::fs::symlink_metadata(&d) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a directory", d.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new().mode(0o700).create(&d)
            };
            #[cfg(not(unix))]
            let created = std::fs::DirBuilder::new().create(&d);
            match created {
                Ok(()) => {}
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists
                        && std::fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&d)?;
        let metadata = opened.metadata()?;
        // SAFETY: `geteuid` has no preconditions and reads this process's effective UID.
        let euid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != euid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not this user's plain directory", d.display()),
            ));
        }
        if metadata.mode() & 0o777 != 0o700 {
            opened.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            let narrowed = opened.metadata()?;
            if narrowed.mode() & 0o777 != 0o700 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} did not become 0700", d.display()),
                ));
            }
        }
    }
    Ok(d)
}

/// Open (creating) a lease-directory file for locking: `0600`, never truncated — a lock
/// file is a rendezvous, not data.
fn open_rendezvous(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
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

fn open_regular_existing_with_meta(path: &Path) -> io::Result<(File, std::fs::Metadata)> {
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

fn open_regular_existing(path: &Path) -> io::Result<File> {
    open_regular_existing_with_meta(path).map(|(file, _)| file)
}

/// The "who" line a holder writes: the first line of `who`, cut on a character boundary
/// short of [`WHO_MAX`] so a reader's bounded read takes it whole.
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

// Publication must never replace a still-live slot whose process tag collided.
// The shared rendezvous gate remains held until the taking link is removed.
fn publish_lease_file(taking: &Path, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::hard_link(taking, path)?;
        std::fs::remove_file(taking)
    }
    #[cfg(not(unix))]
    {
        std::fs::rename(taking, path)
    }
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
    let deadline = Instant::now().checked_add(wait);
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the gate, SHARED: let go by unlock() once the lease is laid"
        )]
        let taken = gate.try_lock_shared();
        match taken {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock)
                if deadline.is_none_or(|d| Instant::now() < d) =>
            {
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
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
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
            .and_then(|()| publish_lease_file(&taking, &path));
        if let Err(e) = laid {
            file.unlock();
            return Err(e);
        }
        Ok(file)
    })();
    let _ = gate.unlock();
    match laid {
        Ok(file) => {
            // Keep the rendezvous named while any genuine duplicate remains: a lease
            // offered to a child is let go only by its last close, which reclaim's kernel
            // probes reap after; one never offered out lets go by LOCK_UN at its drop.
            #[cfg(unix)]
            let file = file.into_sole_close_only();
            Ok(Lease {
                file,
                path,
                subject: subject.clone(),
            })
        }
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
    /// The lease could not be taken. Best-effort callers may proceed; callers requiring a lease refuse.
    NotTaken(Subject, LeaseError),
}

/// The path a holder was asked to hold does not resolve to a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub dir: PathBuf,
    pub managed: bool,
}

/// THE HOLDER VERB's lease (`aterm pkg lease <dir> -- <command…>`): the subject `dir` belongs to, leased as `who` under `prefix` for as long as the returned [`Hold`] lives, and `dir` checked to resolve to a directory AFTER the lease is laid — the check [`take`] leaves to its caller, since only the caller knows what it resolved.
/// # Errors
/// [`Missing`] when `dir` does not resolve to a directory once the lease is laid (or could not be): the holder runs nothing. Valid directory symlinks are followed; files and dangling symlinks are refused.
pub fn hold_for(prefix: &Path, dir: &Path, who: &str, wait: Duration) -> Result<Hold, Missing> {
    let there = || dir.is_dir();
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
    pub const fn in_use(&self) -> bool {
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
/// `reap`), `Err` when it could not be tested.
fn probe_file(path: &Path, reap: bool) -> io::Result<Option<String>> {
    let (mut file, meta): (std::fs::File, std::fs::Metadata) =
        match open_regular_existing_with_meta(path) {
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
            let cap = usize::try_from(meta.len().min(WHO_MAX).saturating_add(1)).unwrap_or(0);
            let mut raw = Vec::with_capacity(cap);
            let _ = (&mut file).take(WHO_MAX).read_to_end(&mut raw);
            let who = String::from_utf8_lossy(&raw);
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
        match entry.file_type() {
            Ok(ft) if ft.is_file() => {}
            Ok(_) => return Holders::Unknown(format!("{name}: not a regular file")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Holders::Unknown(format!("{name}: {e}")),
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
    let mut by_subject: std::collections::BTreeMap<Subject, Result<Vec<String>, String>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(subject) = name
            .strip_suffix(LEASE_SUFFIX)
            .and_then(|n| n.split_once(HOLDER_SEP))
            .and_then(|(stem, _)| Subject::parse_stem(stem))
        else {
            continue;
        };
        let slot = by_subject.entry(subject).or_insert_with(|| Ok(Vec::new()));
        if slot.is_err() {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_file() => {}
            Ok(_) => {
                *slot = Err(format!("{name}: not a regular file"));
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                *slot = Err(format!("{name}: {e}"));
                continue;
            }
        }
        match probe_file(&entry.path(), false) {
            Ok(Some(who)) => {
                if let Ok(live) = slot {
                    live.push(who);
                }
            }
            Ok(None) => {}
            Err(e) => *slot = Err(format!("{name}: {e}")),
        }
    }
    Ok(by_subject
        .into_iter()
        .filter_map(|(subject, slot)| match slot {
            Ok(mut live) if !live.is_empty() => {
                live.sort();
                Some((subject, Holders::Held(live)))
            }
            Ok(_) => None,
            Err(why) => Some((subject, Holders::Unknown(why))),
        })
        .collect())
}

/// A subject's gate held EXCLUSIVE: while it stands no lease on the subject can be laid, so what the reclaimer read of the leases stays true until it drops this.
#[derive(Debug)]
pub struct ReclaimGuard {
    gate: File,
}

impl ReclaimGuard {
    /// Check that this actual exclusive guard is the expected native subject gate.
    /// This neither opens nor probes a lock and never acquires new authority.
    /// # Errors
    /// The original gate is replaced, unsafe, or belongs to another subject/prefix.
    #[cfg(unix)]
    pub fn check_subject(&self, prefix: &Path, subject: &Subject) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let directory = std::fs::symlink_metadata(dir(prefix))?;
        let held = self.gate.metadata()?;
        let named = std::fs::symlink_metadata(
            dir(prefix).join(format!("{}{GATE_SUFFIX}", subject.stem())),
        )?;
        // SAFETY: geteuid only observes the effective process identity.
        let uid = unsafe { libc::geteuid() };
        if !directory.is_dir()
            || directory.uid() != uid
            || directory.mode() & 0o077 != 0
            || held.dev() != named.dev()
            || held.ino() != named.ino()
        {
            return Err(io::Error::other("native reclaim guard subject changed"));
        }
        for meta in [&held, &named] {
            if !meta.is_file() || meta.nlink() != 1 || meta.uid() != uid || meta.mode() & 0o077 != 0
            {
                return Err(io::Error::other("native reclaim guard is unsafe"));
            }
        }
        Ok(())
    }
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
        let Ok(gate) = open_regular_existing(&path) else {
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
/// only while its subject's gate is held EXCLUSIVE.
fn reap_taking(prefix: &Path, laying: &str, path: &Path) {
    let stem = laying
        .split_once(HOLDER_SEP)
        .map_or(laying, |(stem, _)| stem);
    let gate = match open_regular_existing(&dir(prefix).join(format!("{stem}{GATE_SUFFIX}"))) {
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
#[path = "lease_tests.rs"]
mod tests;
