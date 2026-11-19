// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's image (`docs/DESIGN-lan-broker-2026-11-05.md` §3): a root-owned clone of an app
//! bundle. A running team image only moves up: it adopts a bundle only through a root-owned clone
//! that passes a strict static check against the team requirement and whose own sealed
//! `Info.plist` states a strictly newer build than the image's ([`newer_than`],
//! `aterm_uds::lan::install::build_number`), swaps it in for the image, checks that what it swapped
//! out was the image it runs ([`adopt`]), and exits 0 for launchd's `KeepAlive` to start the new
//! one. An ad-hoc image adopts nothing (the next build's elevation renews it). The broker also
//! exits when its own image is replaced. It looks every [`LOOK_EVERY`] at two lanes ([`Looks`]):
//!
//! - **Its source**, the `--source` bundle the elevation ran from ([`Adoption`]): when its
//!   executable changes, a candidate. A missing source is no candidate and no failure, and one
//!   whose `Info.plist` states no newer build than the image is passed over until it changes: the
//!   number read from a bundle an account can change only ever skips a clone.
//! - **Its askers**, the bundles of admitted team processes that asked it to consider them
//!   (`crate::askers`), looked at only where the source lane cloned nothing. Before anything of
//!   an asker is read, the mount table (read without touching a volume) must show its executable
//!   on the image's volume and nothing mounted in its bundle. Then it is authenticated: its
//!   executable on disk carries the CodeDirectory the kernel loaded for the asking process, whose
//!   Info slot seals the bundle's `Info.plist`, and that plist states a build newer than the
//!   image's, read with no link on the way and never waiting on a FIFO; and the bundle holds no
//!   more than [`PER_IMAGE`] times the image's entries and bytes, a filter that skips a clone and
//!   decides nothing. So the number that orders askers is the team's own, which the asking
//!   account cannot raise. At most one authenticated asker is cloned a look: the one whose bundle
//!   and code failed the fewest times, of those the newest build. A clone that fails backs that
//!   asker's uid off ([`retry_after`], and at once [`LONGEST_RETRY`] where its own check got no
//!   answer), whatever its bundle or stamp, until an adoption of that uid succeeds; one that fails
//!   for good for its bundle and code (refused, past the bound, no newer build, another volume) is
//!   never cloned again, and one that fails otherwise (a damaged copy, whose check gives no
//!   verdict) goes behind its uid's askers that never failed, and stays. What fails
//!   authentication costs nothing and is forgotten.
//!
//! Root's work on a candidate is bounded where no account can change it: the clone, in a folder
//! only root reaches, is counted before anything else touches it ([`tree_within`], by
//! descriptor, no link followed, on one volume), and one past [`PER_IMAGE`] times the image's
//! entries or bytes is refused before its seal and its strict check, which reads every byte
//! ([`adopt`]), and settles that candidate as a refusal of its code does. The strict check runs
//! through a gate of its own ([`ADOPTION_CHECKS`]), never the process's one `trustd` gate every
//! admission's peer check runs through, so a check a bundle makes long never turns an admission
//! away, and while one whose bound ended still runs no look clones anything ([`Looks::look`]).
//!
//! Where a round does not adopt its bundle (the check refused it or gave no verdict, the clone
//! states no newer build, the clone failed, the bundle could not be read), the broker writes why
//! ([`Refusals`], `aterm_uds::lan::adoption::Refusal`), naming that bundle, which an account reads
//! with no password: its source's to [`aterm_uds::lan::ADOPTION_REFUSED`], an asker's to that
//! bundle's own file of [`aterm_uds::lan::ADOPTION_REFUSED_ASKERS`] ([`AskerRefusals`]), so
//! neither lane's record takes another bundle's place. A lane removes a bundle's record once it
//! adopts that bundle or finds its image is that bundle; a record a broker left before it moved
//! up names that broker's build, which no account counts. A failure is written where it was not
//! the last said of that bundle. The clone is `clonefile` alone: a bundle on another volume than
//! the image is not copied here, as running a copy tool as root from this resident daemon would
//! be. An asker there is forgotten at the mount check, before any of its files is read, with no
//! record of its own (`docs/KNOWN_ISSUES.md`, ATERM-KI-1116-3471); a source there fails its clone
//! (`EXDEV`) and its record says so. Setup's root steps, whose install copies the bundle, renew
//! the image.

use aterm_keeper::identity::{CheckError, Identity};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::lan::install::{self, CodeDirectories};
use aterm_uds::process::LoadedImage;

use crate::askers::{Asker, Askers, stated_build};
use crate::server::log;

/// How often the source and the image are looked at: the period an account counts this
/// broker's looks in ([`aterm_uds::lan::ADOPTION_LOOK`], `aterm_uds::lan::adoption::LOOKS`).
const LOOK_EVERY: Duration = aterm_uds::lan::ADOPTION_LOOK;
/// Operational failures remain recoverable, without cloning an unchanged
/// failing source at every ordinary look.
const LONGEST_RETRY: Duration = Duration::from_secs(5 * 60);

/// How long after its `failures`th operational failure in a row a source is tried again: one
/// look, doubling with each further failure, at most [`LONGEST_RETRY`].
fn retry_after(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(31);
    LOOK_EVERY.saturating_mul(factor).min(LONGEST_RETRY)
}

/// How many times the running image's entries, and its bytes, a candidate's clone may hold: a
/// bound on what a bundle's account can make root walk, seal and check, byte by byte, which moves
/// with each adopted image.
const PER_IMAGE: u64 = 4;

/// The most entries of the running image counted.
const IMAGE_ENTRIES_MOST: u64 = 1 << 20;

/// The gate a clone's strict check runs through ([`strictly`]): its own, never the process's one
/// `trustd` gate (`aterm_uds::deadline::TRUST`) every admission's peer check runs through. How
/// long a clone's check takes is what its bundle's account put in it, up to [`PER_IMAGE`] times
/// the image; one past its bound leaves this gate overdue, which turns away only the next
/// adoption check until it ends, never an admission.
static ADOPTION_CHECKS: aterm_uds::deadline::Gate =
    aterm_uds::deadline::Gate::new("aterm-lan-adoption-check");

/// The strict check of the clone at `staged` against `checker`'s requirement, through
/// [`ADOPTION_CHECKS`].
///
/// # Errors
/// The check's typed result; the gate overdue is [`CheckError::Unanswered`].
fn strictly(checker: &Identity, staged: &Path) -> Result<(), CheckError> {
    checker.admits_code_at_strictly_through(&ADOPTION_CHECKS, staged)
}

/// How many askers the asker lane remembers of each kind ([`Looks`]: those whose clone failed for
/// good, how often each other's failed, and the last failure said of each uid's bundle): four
/// times the askers the broker keeps ([`crate::askers::MOST`]).
const REFUSED_MOST: usize = 4 * crate::askers::MOST;

/// The executable inside a bundle.
fn executable_of(bundle: &Path) -> PathBuf {
    bundle.join(install::BUNDLE_EXECUTABLE)
}

/// What a running team image adopts with: its strict check of a clone, and the askers its server
/// keeps.
pub(crate) struct Updates {
    /// The team requirement a clone must meet.
    pub(crate) checker: Arc<Identity>,
    /// The admitted processes that asked it to consider their bundles.
    pub(crate) askers: Arc<Askers>,
}

/// The running image's build: what `image`'s `Info.plist` states ([`stated_build`]), 0 where it
/// states none (an image installed before the version rule, or one only root could damage), so
/// any candidate that states a build passes the rule, as before it.
fn running_build(image: &Path) -> u64 {
    stated_build(image).unwrap_or(0)
}

/// What a tree holds: its entries (every file, folder and link, none followed) and the bytes of
/// its regular files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tree {
    entries: u64,
    bytes: u64,
}

impl Tree {
    /// `self` `factor` times over, each saturating.
    const fn times(self, factor: u64) -> Self {
        Self {
            entries: self.entries.saturating_mul(factor),
            bytes: self.bytes.saturating_mul(factor),
        }
    }
}

/// The deepest folder a tree may hold below its root ([`tree_within`]): far past any app
/// bundle's, and the most folders the walk holds open at once.
const DEPTH_MOST: usize = 64;

/// What the tree at `root` holds ([`Tree`]), read where no link and no other volume leads: `root`
/// is opened with no link anywhere on its way, and each folder below it by its name in its
/// parent's descriptor with no link followed, so a folder swapped for a link is never entered; a
/// link is counted, never followed. Before anything is read, `mounts` (the mount table, read
/// without touching any volume) must show nothing mounted at or under `root`, and every folder
/// opened must be on `root`'s device, so no lookup reaches another volume: a share that stalls
/// stalls nothing here. `Ok(None)` once the tree passes `most` in entries or in bytes (each
/// regular file's length, however few blocks it holds), where the walk stops.
///
/// # Errors
/// A folder or an entry that could not be read, a link on the way to `root`, a mount at or under
/// it or a folder on another device, or a folder deeper than [`DEPTH_MOST`].
fn tree_within(
    root: &Path,
    most: Tree,
    mounts: &aterm_sysprobe::volumes::Table,
) -> io::Result<Option<Tree>> {
    use aterm_dirfd::{AtFlags, Dir, FileType, Mode, OFlags};
    use std::os::fd::AsFd as _;
    let errno = |error: aterm_dirfd::Errno| io::Error::from_raw_os_error(error.raw_os_error());
    if let Some(mount) = aterm_sysprobe::volumes::mounts_under(root, mounts).next() {
        return Err(io::Error::other(format!(
            "{} is mounted at or under {}",
            mount.on.display(),
            root.display()
        )));
    }
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    // No link anywhere on the way to the root (macOS refuses `O_NOFOLLOW` beside it).
    #[cfg(target_vendor = "apple")]
    let first = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW_ANY | OFlags::CLOEXEC;
    #[cfg(not(target_vendor = "apple"))]
    let first = folder;
    let top = aterm_dirfd::openat(aterm_dirfd::CWD, root, first, Mode::empty()).map_err(errno)?;
    let device = aterm_dirfd::fstat(&top).map_err(errno)?.st_dev;
    let mut held = Tree::default();
    let mut open = vec![Dir::new(top).map_err(errno)?];
    loop {
        let depth = open.len();
        let Some(dir) = open.last_mut() else {
            break;
        };
        let Some(entry) = dir.next() else {
            open.pop();
            continue;
        };
        let entry = entry.map_err(errno)?;
        let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
        if matches!(name.as_bytes(), b"." | b"..") {
            continue;
        }
        // The entry itself: a link is counted, never followed.
        let stat =
            aterm_dirfd::fstatat(dir.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
        let kind = FileType::from_raw_mode(stat.st_mode);
        held.entries += 1;
        if kind.is_file() {
            held.bytes = held.bytes.saturating_add(stat.st_size);
        }
        if held.entries > most.entries || held.bytes > most.bytes {
            return Ok(None);
        }
        if !kind.is_dir() {
            continue;
        }
        if depth >= DEPTH_MOST {
            return Err(io::Error::other(format!(
                "{} holds a folder deeper than {DEPTH_MOST}",
                root.display()
            )));
        }
        let below = aterm_dirfd::openat(dir.as_fd(), name, folder, Mode::empty()).map_err(errno)?;
        if stat.st_dev != device || aterm_dirfd::fstat(&below).map_err(errno)?.st_dev != device {
            return Err(io::Error::other(format!(
                "{} holds a folder on another volume",
                root.display()
            )));
        }
        open.push(Dir::new(below).map_err(errno)?);
    }
    Ok(Some(held))
}

/// What the running image holds a candidate to: a build above its own `build` (0 where it states
/// none, which any candidate stating a build passes), and a tree no larger than `most`
/// ([`PER_IMAGE`] times its own).
#[derive(Clone, Copy, Debug)]
struct Bounds {
    build: u64,
    most: Tree,
}

/// Executable scheduling identity, never a bundle-integrity or permission receipt.
type Stamp = (u64, u64, u64, i64, i64, i64, i64);
fn stamp(bundle: &Path) -> io::Result<Stamp> {
    let meta = std::fs::metadata(executable_of(bundle))?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the source executable is not a regular file",
        ));
    }
    Ok((
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    ))
}

/// Whether two files hold the same bytes, retaining an unsuccessful read's error.
fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = ([0u8; 32 * 1024], [0u8; 32 * 1024]);
    loop {
        let n = a.read(&mut x)?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

#[derive(Debug)]
enum Failure {
    Io(io::Error),
    /// The clone of the source failed, its errno kept (`EXDEV` for a source on another volume).
    Clone(io::Error),
    Check(CheckError),
    Refused(String),
    /// The clone holds more than its bound ([`within`]), counted before its seal and check:
    /// definite, as a refusal is, for its bundle as it is.
    TooLarge(String),
    /// The clone passed its strict check but its own files state no build newer than the running
    /// image's ([`newer_than`]): definite, as a refusal is.
    NotNewer(String),
    /// The checked image was published; a subsequent verification or cleanup failed.
    Published(Box<Failure>),
    /// What the swap retired was not this broker's own image: another root transaction (an
    /// elevation's install) published it first. The swap was undone and the clone removed.
    Superseded,
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CheckError> for Failure {
    fn from(error: CheckError) -> Self {
        Self::Check(error)
    }
}

impl From<aterm_uds::app_image::Error> for Failure {
    fn from(error: aterm_uds::app_image::Error) -> Self {
        match error {
            aterm_uds::app_image::Error::Refused(why) => Self::Refused(why),
            aterm_uds::app_image::Error::Io(error) => Self::Io(error),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) | Self::Clone(error) => error.fmt(out),
            Self::Check(error) => error.fmt(out),
            Self::Refused(why) | Self::TooLarge(why) | Self::NotNewer(why) => out.write_str(why),
            Self::Published(error) => write!(out, "image replaced; finalization failed: {error}"),
            Self::Superseded => out.write_str(
                "another root transaction published the image this adoption would have \
                 retired: the swap was undone",
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    Io,
    Refused,
    Unchecked,
    Unanswered,
    Published,
}

impl Failure {
    fn class(&self) -> FailureClass {
        match self {
            Self::Io(_) | Self::Clone(_) => FailureClass::Io,
            Self::Refused(_)
            | Self::TooLarge(_)
            | Self::NotNewer(_)
            | Self::Check(CheckError::Refused(_)) => FailureClass::Refused,
            Self::Check(CheckError::Unchecked(_)) => FailureClass::Unchecked,
            Self::Check(CheckError::Unanswered(_)) => FailureClass::Unanswered,
            Self::Published(_) | Self::Superseded => FailureClass::Published,
        }
    }

    /// Whether the failure says this broker will not adopt the source as it is, as its record
    /// tells an account ([`Refusals`]): its check refused the bundle, the bundle was refused
    /// before its check, or the clone failed, any errno; or the check gave no verdict (`trustd`
    /// did not answer, the gate turned it down, it could not run) or the source could not be
    /// read, which a later round may get past.
    fn kind(&self) -> aterm_uds::lan::adoption::Kind {
        use aterm_uds::lan::adoption::Kind;
        match self {
            Self::Check(CheckError::Refused(_))
            | Self::Refused(_)
            | Self::TooLarge(_)
            | Self::NotNewer(_)
            | Self::Clone(_) => Kind::Definite,
            Self::Check(CheckError::Unchecked(_) | CheckError::Unanswered(_))
            | Self::Io(_)
            | Self::Published(_)
            | Self::Superseded => Kind::NoVerdict,
        }
    }

    /// Why the round did not adopt the source, in the words its record gives an account
    /// ([`Refusals`]).
    fn reason(&self) -> String {
        match self {
            Self::Check(CheckError::Refused(why)) => {
                format!("its strict check refused the bundle: {why}")
            }
            Self::Check(error) => format!("its strict check gave no verdict: {error}"),
            Self::Clone(error) if error.raw_os_error() == Some(libc::EXDEV) => format!(
                "the bundle is on another volume than the broker's image, which clonefile cannot \
                 clone and the broker does not copy ({error}); setup's root steps copy it"
            ),
            Self::Clone(error) => format!("cloning the bundle failed: {error}"),
            Self::Refused(why) | Self::TooLarge(why) => {
                format!("the bundle was refused before its check: {why}")
            }
            Self::NotNewer(why) => format!("its strict check passed, but {why}"),
            Self::Io(error) => format!("the adoption failed: {error}"),
            Self::Published(error) => format!("{error}"),
            Self::Superseded => self.to_string(),
        }
    }
}

/// One ordinary source observation and the real adoption operation share this
/// retry state; it is scheduling memory, never an admission receipt.
#[derive(Default)]
struct Adoption {
    source: Option<Stamp>,
    settled: bool,
    failures: u32,
    retry_at: Duration,
    said: Option<(FailureClass, String)>,
}

enum Round {
    Quiet,
    /// The image already is the source: nothing to adopt.
    Current,
    Applied,
    Failed {
        error: Failure,
        report: bool,
    },
}

impl Adoption {
    fn failed(&mut self, error: Failure, now: Duration, stable: bool) -> Round {
        let key = (error.class(), error.to_string());
        let report = self.said.as_ref() != Some(&key);
        self.said = Some(key);
        // Filesystem shape can be repaired without touching the executable
        // (a resource link restored to its signed target). A definitive
        // code-identity refusal settles this executable generation, and so does a
        // clone past its bound: cloning, counting and removing such a tree again at
        // every retry is root's work an account would choose the size of.
        if matches!(
            error,
            Failure::Check(CheckError::Refused(_)) | Failure::NotNewer(_) | Failure::TooLarge(_)
        ) && stable
        {
            self.settled = true;
        } else {
            self.failures = self.failures.saturating_add(1);
            self.retry_at = now.saturating_add(retry_after(self.failures));
        }
        Round::Failed { error, report }
    }

    /// One look at `source` against the image of build `running`: [`Round::Current`] where the
    /// image is its executable, else `adopt`'s round. A source with no executable is no
    /// candidate ([`Round::Quiet`], no record). One whose `Info.plist` states a build no newer than
    /// `running` is passed over, settled until its executable changes: that number, read from a
    /// bundle an account can change, only skips a clone, and the clone's own decides
    /// ([`newer_than`]).
    fn round(
        &mut self,
        clock: impl Fn() -> Duration,
        source: &Path,
        image: &Path,
        running: u64,
        adopt: impl FnOnce() -> Result<(), Failure>,
    ) -> Round {
        let observed = stamp(source);
        if let Ok(current) = observed.as_ref()
            && self.source != Some(*current)
        {
            *self = Self {
                source: Some(*current),
                ..Self::default()
            };
        }
        if self.settled || clock() < self.retry_at {
            return Round::Quiet;
        }
        let before = match observed {
            Ok(stamp) => stamp,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Round::Quiet,
            Err(error) => return self.failed(error.into(), clock(), false),
        };
        let result = match same_bytes(&executable_of(source), &executable_of(image)) {
            Ok(true) => {
                if stamp(source).ok() == Some(before) {
                    self.settled = true;
                }
                return Round::Current;
            }
            Ok(false) if stated_build(source).is_some_and(|build| build <= running) => {
                if stamp(source).ok() == Some(before) {
                    self.settled = true;
                }
                return Round::Quiet;
            }
            Ok(false) => adopt(),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(()) => {
                self.settled = true;
                Round::Applied
            }
            Err(error @ (Failure::Published(_) | Failure::Superseded)) => Round::Failed {
                error,
                report: true,
            },
            Err(error) => self.failed(error, clock(), stamp(source).ok() == Some(before)),
        }
    }
}

/// One uid's backoff in the asker lane ([`Looks`]): scheduling memory, never an admission
/// receipt.
#[derive(Default)]
struct Backoff {
    failures: u32,
    retry_at: Duration,
}

/// One bundle with the code a process of it loaded: what the asker lane remembers a failure of.
type Candidate = (PathBuf, [u8; 20]);

/// The broker's looks at what it may adopt (module docs): its source's lane and its askers', what
/// the running image holds a candidate to ([`Bounds`]), each uid's backoff, the askers whose clone
/// failed definitely for their bundle and code, how often each other asker's failed, and the last
/// failure said of each uid's bundle, each at most [`REFUSED_MOST`], the oldest forgotten first.
/// `mounts` reads the mount table ([`aterm_sysprobe::volumes::table`] for the broker's).
struct Looks {
    source: Adoption,
    bounds: Bounds,
    backoff: BTreeMap<u32, Backoff>,
    refused: VecDeque<Candidate>,
    failed: VecDeque<(Candidate, u32)>,
    said: VecDeque<((u32, PathBuf), (FailureClass, String))>,
    mounts: fn() -> aterm_sysprobe::volumes::Table,
}

/// Whether `error` says the asker lane will never adopt that bundle with that code: its check
/// refused it, it was refused before its check (its clone past the bound included), its clone
/// states no newer build, or it is on another volume (`EXDEV`). Any other failure (a check with no
/// verdict, which is how the framework reports a sealed resource modified or missing, or any
/// other failure of the clone) the next try may get past: that asker stays, behind its uid's
/// askers that never failed ([`Looks::asked`]).
fn refuses_for_good(error: &Failure) -> bool {
    match error {
        Failure::Check(CheckError::Refused(_))
        | Failure::Refused(_)
        | Failure::TooLarge(_)
        | Failure::NotNewer(_) => true,
        Failure::Clone(error) => error.raw_os_error() == Some(libc::EXDEV),
        _ => false,
    }
}

/// Keep `value` for `key` in `kept`, latest last, at most [`REFUSED_MOST`]: the oldest goes first.
fn remember<K: PartialEq, V>(kept: &mut VecDeque<(K, V)>, key: K, value: V) {
    kept.retain(|(at, _)| *at != key);
    if kept.len() >= REFUSED_MOST {
        kept.pop_front();
    }
    kept.push_back((key, value));
}

impl Looks {
    fn new(bounds: Bounds) -> Self {
        Self {
            source: Adoption::default(),
            bounds,
            backoff: BTreeMap::new(),
            refused: VecDeque::new(),
            failed: VecDeque::new(),
            said: VecDeque::new(),
            mounts: aterm_sysprobe::volumes::table,
        }
    }

    /// One look: the source lane's round, and where that cloned nothing, the asker lane's, with
    /// the bundle it tried. `adopt` clones, checks and swaps in the bundle it is given. Where a
    /// strict check is still running (`checking`: one whose bound ended, [`ADOPTION_CHECKS`]),
    /// the look clones nothing: its check would be turned away, and the asker or source that
    /// cloned for it would wait out the bound of a check that was never its own.
    fn look(
        &mut self,
        clock: impl Fn() -> Duration,
        checking: bool,
        source: &Path,
        image: &Path,
        askers: &Askers,
        mut adopt: impl FnMut(&Path) -> Result<(), Failure>,
    ) -> (Round, Option<(PathBuf, Round)>) {
        if checking {
            return (Round::Quiet, None);
        }
        let mut cloned = false;
        let round = self
            .source
            .round(&clock, source, image, self.bounds.build, || {
                cloned = true;
                adopt(source)
            });
        if cloned {
            return (round, None);
        }
        (round, self.asked(&clock, image, askers, adopt))
    }

    /// How often the asker lane's clone of `asker`'s bundle with its code failed, as far as it
    /// remembers.
    fn failures_of(&self, asker: &Asker) -> u32 {
        self.failed
            .iter()
            .find(|((bundle, cdhash), _)| *bundle == asker.bundle && *cdhash == asker.cdhash)
            .map_or(0, |(_, failures)| *failures)
    }

    /// The asker lane: of the askers whose uid is not backed off, the authenticated one adopted
    /// ([`Self::authenticated`], against the image at `image`) whose bundle and code failed the
    /// fewest times, of those the newest build, `None` where there is none: so an asker whose
    /// clone keeps failing (a damaged copy of a newer build) goes behind its uid's next, which is
    /// adopted, and stays to be tried again. Each asker that fails authentication is forgotten, and
    /// so is each whose clone failed for good ([`refuses_for_good`]), which is remembered by bundle
    /// and code and never cloned again. A failure is reported (recorded for its bundle) where the
    /// last one said of that uid's bundle was another.
    fn asked(
        &mut self,
        clock: impl Fn() -> Duration,
        image: &Path,
        askers: &Askers,
        adopt: impl FnOnce(&Path) -> Result<(), Failure>,
    ) -> Option<(PathBuf, Round)> {
        let now = clock();
        let mounts = (self.mounts)();
        let mut best: Option<((u32, std::cmp::Reverse<u64>), Asker)> = None;
        for asker in askers.now() {
            if self
                .refused
                .iter()
                .any(|(bundle, cdhash)| *bundle == asker.bundle && *cdhash == asker.cdhash)
            {
                askers.forget(&asker);
                continue;
            }
            if self
                .backoff
                .get(&asker.uid)
                .is_some_and(|backoff| now < backoff.retry_at)
            {
                continue;
            }
            match self.authenticated(&asker, image, &mounts) {
                Some(build) => {
                    askers.stated(&asker, build);
                    let rank = (self.failures_of(&asker), std::cmp::Reverse(build));
                    if best.as_ref().is_none_or(|(kept, _)| rank < *kept) {
                        best = Some((rank, asker));
                    }
                }
                None => askers.forget(&asker),
            }
        }
        let (_, asker) = best?;
        let round = match adopt(&asker.bundle) {
            Ok(()) => {
                self.backoff.remove(&asker.uid);
                Round::Applied
            }
            Err(error @ (Failure::Published(_) | Failure::Superseded)) => Round::Failed {
                error,
                report: true,
            },
            Err(error) => {
                let candidate = (asker.bundle.clone(), asker.cdhash);
                if refuses_for_good(&error) {
                    askers.forget(&asker);
                    self.failed.retain(|(at, _)| *at != candidate);
                    if self.refused.len() >= REFUSED_MOST {
                        self.refused.pop_front();
                    }
                    self.refused.push_back(candidate);
                } else {
                    let failures = self.failures_of(&asker).saturating_add(1);
                    remember(&mut self.failed, candidate, failures);
                }
                let key = (error.class(), error.to_string());
                let named = (asker.uid, asker.bundle.clone());
                let report = self
                    .said
                    .iter()
                    .find(|(at, _)| *at == named)
                    .is_none_or(|(_, said)| *said != key);
                remember(&mut self.said, named, key);
                let backoff = self.backoff.entry(asker.uid).or_default();
                backoff.failures = backoff.failures.saturating_add(1);
                // A check that got no answer leaves its gate overdue until it ends: each try
                // before then would clone and seal the bundle for nothing.
                let wait = if error.class() == FailureClass::Unanswered {
                    LONGEST_RETRY
                } else {
                    retry_after(backoff.failures)
                };
                backoff.retry_at = clock().saturating_add(wait);
                Round::Failed { error, report }
            }
        };
        Some((asker.bundle, round))
    }

    /// The build `asker`'s bundle states, where the kernel's account of the asking process holds
    /// it. First, read from the mount table alone (`mounts`, which touches no volume), its
    /// executable must be on the volume the image at `image` is on, and nothing may be mounted at
    /// or under its bundle: nothing is read from another volume, which could stall the adoption
    /// thread, nor cloned across one. Then its executable, read with no link on the way, carries
    /// the CodeDirectory whose cdhash the kernel loaded for the process; that directory's Info
    /// slot seals the bundle's `Info.plist`, read the same way and never waiting on a FIFO; the
    /// plist states a build newer than the image's; and the bundle holds no more than the bound
    /// ([`Bounds::most`], [`tree_within`]), a filter that skips a clone: the bound that holds is
    /// the clone's ([`adopt`]). `None` otherwise.
    fn authenticated(
        &self,
        asker: &Asker,
        image: &Path,
        mounts: &aterm_sysprobe::volumes::Table,
    ) -> Option<u64> {
        use aterm_sysprobe::volumes::mount_of;
        let exe = executable_of(&asker.bundle);
        let volume = mount_of(&exe, mounts)?;
        if !mounts.complete || mount_of(image, mounts) != Some(volume) {
            return None;
        }
        let code = CodeDirectories::of_unlinked(&exe).ok()?;
        let slot = code.info_slot(asker.cdhash)?;
        let plist = install::read_unlinked(&asker.bundle.join(install::BUNDLE_INFO)).ok()?;
        let build = install::build_number(&plist)
            .filter(|build| slot.seals(&plist) && *build > self.bounds.build)?;
        tree_within(&asker.bundle, self.bounds.most, mounts).ok()??;
        Some(build)
    }
}

/// The broker's record at `path` of why it did not adopt the bundle `source`, in the form of
/// [`aterm_uds::lan::adoption::Refusal`], with the failure's kind, which sets how long an account
/// waits: its source lane's is [`aterm_uds::lan::ADOPTION_REFUSED`] (which builds from before the
/// asker lane read too), and each asker's its bundle's own ([`AskerRefusals`]). It is written as
/// root, `0644`, where a round failed and said so (a failure the last round of that lane or uid
/// did not already record), and removed where a round adopted `source`, or found the image is
/// `source` and the record names it. A failure after publication is not recorded: the broker
/// exits, and the one launchd starts finds its image in place. Every record names the build of
/// the broker that wrote it, and an account counts only the answering broker's
/// (`aterm_uds::lan::adoption::expect`), so one a broker left before it moved up counts for
/// nothing. Best effort: a record that cannot be written or removed leaves an account to say only
/// that the broker gave no reason, or an old one.
struct Refusals<'a> {
    path: &'a Path,
    source: &'a Path,
}

impl Refusals<'_> {
    fn after(&self, round: &Round) {
        match round {
            Round::Applied => {
                let _ = std::fs::remove_file(self.path);
            }
            Round::Current => {
                let names = std::fs::read(self.path)
                    .ok()
                    .and_then(|bytes| aterm_uds::lan::adoption::Refusal::parse(&bytes))
                    .is_some_and(|refusal| refusal.source == self.source);
                if names {
                    let _ = std::fs::remove_file(self.path);
                }
            }
            Round::Failed {
                error,
                report: true,
            } if !matches!(error, Failure::Published(_) | Failure::Superseded) => {
                let refusal = aterm_uds::lan::adoption::Refusal::new(
                    self.source,
                    aterm_uds::lan::hello_build(),
                    error.kind(),
                    &error.reason(),
                );
                if let Some(text) = refusal.text() {
                    let _ = aterm_uds::atomic_file::write(self.path, text.as_bytes(), 0o644);
                }
            }
            Round::Quiet | Round::Failed { .. } => {}
        }
    }
}

/// The asker lane's records ([`Refusals`]): each asker's in its bundle's own file of `folder`
/// ([`aterm_uds::lan::ADOPTION_REFUSED_ASKERS`], `aterm_uds::lan::adoption::asker_refusal_in`),
/// never the source lane's, at most [`crate::askers::MOST`] of them, the oldest removed first: so
/// no asker's record ever takes another bundle's place, and an account that reads its own
/// bundle's finds its own reason. The folder is `0755`, made where a record is written.
struct AskerRefusals<'a> {
    folder: &'a Path,
}

impl AskerRefusals<'_> {
    /// What the asker lane's `round` of `bundle` leaves recorded.
    fn after(&self, bundle: &Path, round: &Round) {
        let writes = matches!(round, Round::Failed { report: true, .. });
        if writes && !self.made() {
            return;
        }
        Refusals {
            path: &aterm_uds::lan::adoption::asker_refusal_in(self.folder, bundle),
            source: bundle,
        }
        .after(round);
        if writes {
            self.bound();
        }
    }

    /// Whether the folder stands, a real folder of this process's user, `0755` whatever the umask
    /// (made where there was none), so every account reads its records.
    fn made(&self) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        let made = std::fs::create_dir(self.folder);
        (made.is_ok() || made.is_err_and(|error| error.kind() == io::ErrorKind::AlreadyExists))
            && owned_folder(self.folder).is_ok()
            && std::fs::set_permissions(self.folder, std::fs::Permissions::from_mode(0o755)).is_ok()
    }

    /// Remove the oldest records past [`crate::askers::MOST`].
    fn bound(&self) {
        let Ok(entries) = std::fs::read_dir(self.folder) else {
            return;
        };
        let mut records: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((meta.modified().ok()?, entry.path()))
            })
            .collect();
        records.sort();
        let past = records.len().saturating_sub(crate::askers::MOST);
        for (_, path) in records.into_iter().take(past) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Start watching `source` (the app bundle), the askers `checker` keeps and this process's own
/// image, on a thread of its own. Only a team image running from [`aterm_uds::lan::IMAGE_EXECUTABLE`]
/// whose loaded executable the kernel names adopts: it reads its own build once, from the image's
/// `Info.plist` (0 where that states none), and counts the image's tree once, for the bound a
/// candidate's clone is held to ([`Bounds`]); an image whose tree cannot be counted adopts
/// nothing, as no bound would hold.
///
/// # Errors
/// The thread could not start.
pub(crate) fn watch(source: PathBuf, checker: Option<Updates>) -> io::Result<()> {
    let own = std::env::current_exe()?;
    let image = LoadedImage::read(i32::try_from(std::process::id()).unwrap_or(0));
    let loaded =
        checker.is_some() && image.is_some() && own == Path::new(aterm_uds::lan::IMAGE_EXECUTABLE);
    let image_bundle = Path::new(aterm_uds::lan::IMAGE_BUNDLE);
    let counted = loaded.then(|| {
        tree_within(
            image_bundle,
            Tree {
                entries: IMAGE_ENTRIES_MOST,
                bytes: u64::MAX,
            },
            &aterm_sysprobe::volumes::table(),
        )
    });
    let bounds = match &counted {
        Some(Ok(Some(tree))) => Some(Bounds {
            build: running_build(image_bundle),
            most: tree.times(PER_IMAGE),
        }),
        _ => None,
    };
    let adopting = bounds.is_some();
    if checker.is_none() {
        log(format_args!(
            "an ad-hoc image adopts no update: the next elevation renews it"
        ));
    } else if own != Path::new(aterm_uds::lan::IMAGE_EXECUTABLE) {
        log(format_args!(
            "running from {}, not {}: adopting no update",
            own.display(),
            aterm_uds::lan::IMAGE_EXECUTABLE
        ));
    } else if !loaded {
        log(format_args!(
            "the kernel named no loaded image of this process, so no swap could be checked: \
             adopting no update"
        ));
    } else if let Some(bounds) = &bounds {
        log(format_args!(
            "build {} (a tree of {} entries, {} bytes, at most {PER_IMAGE} times which a \
             candidate's clone may hold): adopting only a newer build",
            bounds.build,
            bounds.most.entries / PER_IMAGE,
            bounds.most.bytes / PER_IMAGE,
        ));
    } else {
        let why = match counted {
            Some(Err(error)) => error.to_string(),
            _ => format!("more than {IMAGE_ENTRIES_MOST} entries"),
        };
        log(format_args!(
            "the image's tree could not be counted ({why}), so no candidate could be bounded: \
             adopting no update"
        ));
    }
    let bounds = bounds.unwrap_or(Bounds {
        build: 0,
        most: Tree::default(),
    });
    std::thread::Builder::new()
        .name("lan-adopt".into())
        .spawn(move || {
            let mut looks = Looks::new(bounds);
            let clock = std::time::Instant::now();
            loop {
                if image.as_ref().and_then(|image| image.matches_file(&own)) == Some(false) {
                    log(format_args!(
                        "{} was replaced: exiting for launchd to start it",
                        own.display()
                    ));
                    crate::scope::exit_for_launchd();
                }
                if adopting && let Some(updates) = &checker {
                    let image_bundle = Path::new(aterm_uds::lan::IMAGE_BUNDLE);
                    // No link is followed to what the swap retired: a link is not the image.
                    let ours = |exe: &Path| {
                        let image = image.as_ref()?;
                        let meta = std::fs::symlink_metadata(exe).ok()?;
                        Some(image.matches_metadata(&meta))
                    };
                    let (round, asked) = looks.look(
                        || clock.elapsed(),
                        ADOPTION_CHECKS.running() > 0,
                        &source,
                        image_bundle,
                        &updates.askers,
                        |bundle| {
                            adopt(bundle, image_bundle, bounds, &ours, |staged| {
                                strictly(&updates.checker, staged)
                            })
                        },
                    );
                    Refusals {
                        path: Path::new(aterm_uds::lan::ADOPTION_REFUSED),
                        source: &source,
                    }
                    .after(&round);
                    match round {
                        Round::Applied => {
                            log(format_args!(
                                "adopted {} as {}: exiting for launchd to start it",
                                source.display(),
                                image_bundle.display()
                            ));
                            crate::scope::exit_for_launchd();
                        }
                        Round::Failed {
                            error: error @ (Failure::Published(_) | Failure::Superseded),
                            ..
                        } => {
                            log(format_args!(
                                "{}: {error}; exiting for launchd",
                                source.display()
                            ));
                            crate::scope::exit_for_launchd();
                        }
                        Round::Failed {
                            error,
                            report: true,
                        } => log(format_args!("did not adopt {}: {error}", source.display())),
                        Round::Quiet | Round::Current | Round::Failed { report: false, .. } => {}
                    }
                    if let Some((bundle, round)) = asked {
                        AskerRefusals {
                            folder: Path::new(aterm_uds::lan::ADOPTION_REFUSED_ASKERS),
                        }
                        .after(&bundle, &round);
                        after_asked(&bundle, image_bundle, &round);
                    }
                }
                std::thread::sleep(LOOK_EVERY);
            }
        })?;
    Ok(())
}

/// What the broker does after its asker lane's round of `bundle`: exits 0 for launchd to start the
/// image it adopted, or the one another root transaction published, and logs a failure it says.
fn after_asked(bundle: &Path, image: &Path, round: &Round) {
    match round {
        Round::Applied => {
            log(format_args!(
                "adopted {}, which asked, as {}: exiting for launchd to start it",
                bundle.display(),
                image.display()
            ));
            std::process::exit(0);
        }
        Round::Failed {
            error: error @ (Failure::Published(_) | Failure::Superseded),
            ..
        } => {
            log(format_args!(
                "{}: {error}; exiting for launchd",
                bundle.display()
            ));
            std::process::exit(0);
        }
        Round::Failed {
            error,
            report: true,
        } => log(format_args!("did not adopt {}: {error}", bundle.display())),
        Round::Quiet | Round::Current | Round::Failed { report: false, .. } => {}
    }
}

fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holding a NUL"))
}

/// `Ok` when `path` is a real folder (no link) owned by this process's user.
///
/// # Errors
/// A refusal naming what it is instead, or the `lstat`'s operational error.
fn owned_folder(path: &Path) -> Result<(), Failure> {
    let meta = std::fs::symlink_metadata(path)?;
    let ours = aterm_uds::peer::our_uid();
    if !meta.file_type().is_dir() || meta.uid() != ours {
        return Err(Failure::Refused(format!(
            "{} is not a folder of uid {ours} (a {:?} of uid {})",
            path.display(),
            meta.file_type(),
            meta.uid()
        )));
    }
    Ok(())
}

/// `Ok` where the clone at `staging` holds no more than `most` ([`tree_within`]).
///
/// # Errors
/// [`Failure::TooLarge`] naming the bound it passed, or the walk's [`Failure::Io`].
fn within(staging: &Path, most: Tree) -> Result<(), Failure> {
    match tree_within(staging, most, &aterm_sysprobe::volumes::table())? {
        Some(_) => Ok(()),
        None => Err(Failure::TooLarge(format!(
            "its clone holds more than {} entries or {} bytes, {PER_IMAGE} times the image's",
            most.entries, most.bytes
        ))),
    }
}

/// Swap `a` and `b` in one rename (`renamex_np`, `RENAME_SWAP`).
fn swap(a: &Path, b: &Path) -> io::Result<()> {
    let (a, b) = (c_path(a)?, c_path(b)?);
    // SAFETY: both are NUL-terminated paths live for the call.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether the sealed clone at `staging` states a strictly newer build than `running`, from its
/// own root-owned files after its strict check: every CodeDirectory of its executable binds an
/// `Info.plist` and seals the one beside it (`CodeDirectories::seals_info_plist`), and that plist
/// states a build above `running` ([`install::build_number`]). A clone that does not is refused,
/// a definite failure: the broker never moves to an older build, whatever a bundle's account
/// made its own files say before the clone.
///
/// # Errors
/// [`Failure::NotNewer`] refusing it, or the read's [`Failure::Io`].
fn newer_than(staging: &Path, running: u64) -> Result<(), Failure> {
    let refused = |why: String| Err(Failure::NotNewer(why));
    let code = CodeDirectories::of_unlinked(&executable_of(staging))?;
    let plist = install::read_unlinked(&staging.join(install::BUNDLE_INFO))?;
    if !code.seals_info_plist(&plist) {
        return refused(
            "its executable does not seal its Info.plist in every code directory, so the plist \
             states no build"
                .to_owned(),
        );
    }
    match install::build_number(&plist) {
        None => refused("its Info.plist states no build".to_owned()),
        Some(build) if build <= running => refused(format!(
            "its build {build} is not newer than the running image's {running}: the broker never \
             moves to an older build"
        )),
        Some(_) => Ok(()),
    }
}

/// Adopt `source` as `image`, held to `bounds` ([`Bounds`]): clone the folder `source` is (never
/// a link, which could lead somewhere the person controls) into a private folder beside `image`
/// that only this process's user reaches, confirm the clone is a real folder of that user
/// (root's), refuse it where its tree passes the bound ([`tree_within`]: counted there, where no
/// account can change it, before anything else walks, seals or checks it), take its group and
/// other write bits off, refuse it if a link inside leads out of it, let `check` judge the clone
/// (never the person's bundle, which could change after the check), refuse it unless its own
/// files state a newer build ([`newer_than`]), then swap the clone in for `image` in one rename. What the swap retired must hold this process's own loaded executable (`ours`, from
/// the kernel's reading of this process's image): where it does not, another root transaction
/// (an elevation's install) published that image, so the swap is undone and the clone removed
/// ([`Failure::Superseded`]), and where that cannot be told the swap is undone too, an
/// operational failure. Then `image` is confirmed a real folder and what it replaced removed. A
/// clone refused before publication is removed and `image` is left as it was; with no image
/// there is nothing to swap, and nothing is published.
///
/// # Errors
/// A source or clone that is no owned real folder, a clone past the bound, a link out of the
/// clone, the check's typed result, a build no newer; filesystem errors remain operational. A
/// failure after publication is marked separately so cleanup cannot prevent the running broker
/// from relinquishing its old image.
fn adopt(
    source: &Path,
    image: &Path,
    bounds: Bounds,
    ours: &dyn Fn(&Path) -> Option<bool>,
    check: impl FnOnce(&Path) -> Result<(), CheckError>,
) -> Result<(), Failure> {
    adopt_allocating(
        source,
        image,
        bounds,
        ours,
        check,
        |parent| {
            aterm_tempfile::Builder::new()
                .prefix(".adopting-")
                .tempdir_in(parent)
        },
        aterm_uds::app_image::clone,
    )
}

/// [`adopt`], with the private staging folder made by `allocate` in the image's folder and the
/// source cloned into it by `clone` ([`aterm_uds::app_image::clone`] for the broker's, a test's
/// to fail it as another volume fails it). A clone that fails is [`Failure::Clone`], its errno
/// kept; a source refused as no real folder stays [`Failure::Refused`].
///
/// # Errors
/// As [`adopt`].
fn adopt_allocating(
    source: &Path,
    image: &Path,
    bounds: Bounds,
    ours: &dyn Fn(&Path) -> Option<bool>,
    check: impl FnOnce(&Path) -> Result<(), CheckError>,
    allocate: impl FnOnce(&Path) -> io::Result<aterm_tempfile::TempDir>,
    clone: impl FnOnce(&Path, &Path) -> Result<(), aterm_uds::app_image::Error>,
) -> Result<(), Failure> {
    let dir = image
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an image with no folder"))?;
    // The private parent exists before cloning. An inherited source ACL can
    // grant no account access to candidate files before the seal removes it.
    let owner = allocate(dir)?;
    let staging = owner.path().join("aterm.app");
    clone(source, &staging).map_err(|error| match error {
        aterm_uds::app_image::Error::Refused(why) => Failure::Refused(why),
        aterm_uds::app_image::Error::Io(error) => Failure::Clone(error),
    })?;
    // SAFETY: getegid reads the caller's group and cannot fail or mutate state.
    let gid = unsafe { libc::getegid() };
    let judged = owned_folder(&staging)
        .and_then(|()| within(&staging, bounds.most))
        .and_then(|()| {
            aterm_uds::app_image::seal(&staging, aterm_uds::peer::our_uid(), gid, true)
                .map_err(Failure::from)
        })
        .and_then(|()| check(&staging).map_err(Failure::Check))
        .and_then(|()| newer_than(&staging, bounds.build));
    judged?;
    swap(&staging, image)?;
    // `staging` now holds what the swap retired: this broker's own image, unless another root
    // transaction published one there since this process started.
    match ours(&executable_of(&staging)) {
        Some(true) => {}
        retired => {
            swap(&staging, image).map_err(|error| Failure::Published(Box::new(error.into())))?;
            return Err(match retired {
                Some(false) => Failure::Superseded,
                _ => Failure::Io(io::Error::other(
                    "the image the swap retired could not be told to be this broker's own: the \
                     swap was undone",
                )),
            });
        }
    }
    // Preserve the post-publication result even if Drop later manages cleanup.
    std::fs::remove_dir_all(owner.path())
        .map_err(|error| Failure::Published(Box::new(error.into())))?;
    let _ = owner.keep();
    owned_folder(image).map_err(|error| Failure::Published(Box::new(error)))
}

#[cfg(test)]
#[path = "adopt_tests.rs"]
mod tests;
