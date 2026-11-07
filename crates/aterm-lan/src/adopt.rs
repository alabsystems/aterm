// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's image (`docs/DESIGN-lan-broker-2026-11-05.md` §3): a root-owned clone of the app
//! bundle the elevation ran from. The broker watches that `--source` bundle's executable; when it
//! changes and the running image has a team, a clone of the new bundle that passes a strict
//! static check against the team requirement is swapped in for the image and the broker exits 0,
//! for launchd's `KeepAlive` to start it. An ad-hoc image adopts nothing (the next build's
//! elevation renews it). The broker also exits when its own image is replaced.

use aterm_keeper::identity::{CheckError, Identity};
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::process::LoadedImage;

use crate::server::log;

/// How often the source and the image are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// Operational failures remain recoverable, without cloning an unchanged
/// failing source at every ordinary look.
const LONGEST_RETRY: Duration = Duration::from_secs(5 * 60);

/// The executable inside a bundle.
fn executable_of(bundle: &Path) -> PathBuf {
    bundle.join("Contents").join("MacOS").join("aterm")
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
    Check(CheckError),
    Refused(String),
    /// The checked image was published; a subsequent verification or cleanup failed.
    Published(Box<Failure>),
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

impl std::fmt::Display for Failure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(out),
            Self::Check(error) => error.fmt(out),
            Self::Refused(why) => out.write_str(why),
            Self::Published(error) => write!(out, "image replaced; finalization failed: {error}"),
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
            Self::Io(_) => FailureClass::Io,
            Self::Refused(_) | Self::Check(CheckError::Refused(_)) => FailureClass::Refused,
            Self::Check(CheckError::Unchecked(_)) => FailureClass::Unchecked,
            Self::Check(CheckError::Unanswered(_)) => FailureClass::Unanswered,
            Self::Published(_) => FailureClass::Published,
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
    Applied,
    Failed { error: Failure, report: bool },
}

impl Adoption {
    fn failed(&mut self, error: Failure, now: Duration, stable: bool) -> Round {
        let key = (error.class(), error.to_string());
        let report = self.said.as_ref() != Some(&key);
        self.said = Some(key);
        if error.class() == FailureClass::Refused && stable {
            self.settled = true;
        } else {
            self.failures = self.failures.saturating_add(1);
            let factor = 1u32 << self.failures.saturating_sub(1).min(31);
            self.retry_at =
                now.saturating_add(LOOK_EVERY.saturating_mul(factor).min(LONGEST_RETRY));
        }
        Round::Failed { error, report }
    }

    fn round(
        &mut self,
        clock: impl Fn() -> Duration,
        source: &Path,
        image: &Path,
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
            Err(error) => return self.failed(error.into(), clock(), false),
        };
        let result = match same_bytes(&executable_of(source), &executable_of(image)) {
            Ok(true) => {
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
            Err(error @ Failure::Published(_)) => Round::Failed {
                error,
                report: true,
            },
            Err(error) => self.failed(error, clock(), stamp(source).ok() == Some(before)),
        }
    }
}

/// Start watching `source` (the app bundle) and this process's own image, on a thread of its own.
///
/// # Errors
/// The thread could not start.
pub(crate) fn watch(source: PathBuf, checker: Option<Arc<Identity>>) -> io::Result<()> {
    let own = std::env::current_exe()?;
    let image = LoadedImage::read(i32::try_from(std::process::id()).unwrap_or(0));
    let adopting = checker.is_some() && own == Path::new(aterm_uds::lan::IMAGE_EXECUTABLE);
    if checker.is_none() {
        log(format_args!(
            "an ad-hoc image adopts no update: the next elevation renews it"
        ));
    } else if !adopting {
        log(format_args!(
            "running from {}, not {}: adopting no update",
            own.display(),
            aterm_uds::lan::IMAGE_EXECUTABLE
        ));
    }
    std::thread::Builder::new()
        .name("lan-adopt".into())
        .spawn(move || {
            let mut adoption = Adoption::default();
            let clock = std::time::Instant::now();
            loop {
                if image.as_ref().and_then(|image| image.matches_file(&own)) == Some(false) {
                    log(format_args!(
                        "{} was replaced: exiting for launchd to start it",
                        own.display()
                    ));
                    std::process::exit(0);
                }
                if adopting && let Some(check) = &checker {
                    let image_bundle = Path::new(aterm_uds::lan::IMAGE_BUNDLE);
                    match adoption.round(
                        || clock.elapsed(),
                        &source,
                        image_bundle,
                        || {
                            adopt(&source, image_bundle, |staged| {
                                check.admits_code_at_strictly(staged)
                            })
                        },
                    ) {
                        Round::Applied => {
                            log(format_args!(
                                "adopted {} as {}: exiting for launchd to start it",
                                source.display(),
                                image_bundle.display()
                            ));
                            std::process::exit(0);
                        }
                        Round::Failed {
                            error: error @ Failure::Published(_),
                            ..
                        } => {
                            log(format_args!(
                                "{}: {error}; exiting for launchd",
                                source.display()
                            ));
                            std::process::exit(0);
                        }
                        Round::Failed {
                            error,
                            report: true,
                        } => log(format_args!("did not adopt {}: {error}", source.display())),
                        Round::Quiet | Round::Failed { report: false, .. } => {}
                    }
                }
                std::thread::sleep(LOOK_EVERY);
            }
        })?;
    Ok(())
}

/// `CLONE_NOFOLLOW`: a link inside the bundle is cloned as the link.
const CLONE_NOFOLLOW: u32 = 0x1;
/// `CLONE_NOOWNERCOPY`: the clone is the caller's (root's), not the source's owner's.
const CLONE_NOOWNERCOPY: u32 = 0x2;

unsafe extern "C" {
    /// `<sys/clonefile.h>`:
    /// `int fclonefileat(int srcfd, int dst_dirfd, const char *dst, uint32_t flags)`.
    fn fclonefileat(srcfd: i32, dst_dirfd: i32, dst: *const std::ffi::c_char, flags: u32) -> i32;
}

fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holding a NUL"))
}

/// `from` opened as the folder it is: `ENOTDIR` or `ELOOP` (`O_NOFOLLOW`) for anything else, a
/// link to a folder included.
fn open_folder(from: &Path) -> io::Result<std::os::fd::OwnedFd> {
    let path = c_path(from)?;
    // SAFETY: `path` is a NUL-terminated path live for the call; the flags take no mode.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened here and nothing else owns it.
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
}

/// Clone the bundle `from` (a real folder, recursively; never through a link) to the new name
/// `to`, owned by this process's user. The clone is made from the folder's descriptor, so what
/// is cloned is the folder that was opened, whatever is renamed over `from` meanwhile.
fn clone(from: &Path, to: &Path) -> Result<(), Failure> {
    if !std::fs::symlink_metadata(from)?.is_dir() {
        return Err(Failure::Refused(format!(
            "{} is not a real bundle folder (a link is never adopted)",
            from.display()
        )));
    }
    let folder = open_folder(from)?;
    let to = c_path(to)?;
    // SAFETY: `folder` is a live descriptor and `to` a NUL-terminated path, both live for the
    // call.
    if unsafe {
        fclonefileat(
            std::os::fd::AsRawFd::as_raw_fd(&folder),
            libc::AT_FDCWD,
            to.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error().into())
    }
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

/// Whether the link target `target`, of a link `depth` folders below the bundle's root, stays
/// inside the bundle: relative, and never climbing above the root.
fn stays_inside(target: &Path, depth: usize) -> bool {
    use std::path::Component;
    let mut at = depth;
    for part in target.components() {
        match part {
            Component::Normal(_) => at += 1,
            Component::CurDir => {}
            Component::ParentDir => match at.checked_sub(1) {
                Some(up) => at = up,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Take the group and other write bits off everything under `root` (`depth` folders below the
/// bundle's root), and refuse a link that leads out of the bundle.
fn strip_write(root: &Path, depth: usize) -> Result<(), Failure> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(root)?;
        // A link's own depth is its folder's.
        if !stays_inside(&target, depth.saturating_sub(1)) {
            return Err(Failure::Refused(format!(
                "{} leads out of the bundle ({})",
                root.display(),
                target.display()
            )));
        }
        return Ok(());
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o022 != 0 {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(mode & !0o022))?;
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(root)? {
            strip_write(&entry?.path(), depth + 1)?;
        }
    }
    Ok(())
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

/// Adopt `source` as `image`: clone the folder `source` is (never a link, which could lead
/// somewhere the person controls) to a staging name beside `image`, confirm the clone is a real
/// folder of this process's user (root's), take its group and other write bits off, refuse it
/// if a link inside leads out of it, let `check` judge the clone (never the person's bundle,
/// which could change after the check), then swap the clone in for `image` in one rename,
/// confirm `image` is that real folder, and remove what it replaced. A clone refused before
/// publication is removed and `image` is left as it was.
///
/// # Errors
/// A source or clone that is no owned real folder, a link out of the clone, or the check's
/// typed result; filesystem errors remain operational. A failure after publication is marked
/// separately so cleanup cannot prevent the running broker from relinquishing its old image.
fn adopt(
    source: &Path,
    image: &Path,
    check: impl FnOnce(&Path) -> Result<(), CheckError>,
) -> Result<(), Failure> {
    let dir = image
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an image with no folder"))?;
    let staging = dir.join(format!(".adopting-{}.app", std::process::id()));
    if std::fs::symlink_metadata(&staging).is_ok() {
        std::fs::remove_dir_all(&staging)?;
    }
    clone(source, &staging)?;
    let judged = owned_folder(&staging)
        .and_then(|()| strip_write(&staging, 0))
        .and_then(|()| check(&staging).map_err(Failure::Check));
    if let Err(error) = judged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if std::fs::symlink_metadata(image).is_ok() {
        if let Err(error) = swap(&staging, image) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        // `staging` now names the image that was replaced.
        if let Err(error) = std::fs::remove_dir_all(&staging) {
            return Err(Failure::Published(Box::new(error.into())));
        }
    } else {
        std::fs::rename(&staging, image)?;
    }
    owned_folder(image).map_err(|error| Failure::Published(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(at: &Path, bytes: &[u8]) {
        let exe = executable_of(at);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, bytes).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
    }

    #[test]
    fn a_passing_clone_is_swapped_in_and_a_refused_one_leaves_the_image() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (source, image) = (
            root.path().join("source.app"),
            root.path().join("lan").join("aterm.app"),
        );
        bundle(&source, b"new");
        bundle(&image, b"old");
        let refused = adopt(&source, &image, |_| {
            Err(CheckError::Refused("not the team".to_owned()))
        })
        .unwrap_err();
        assert!(matches!(refused, Failure::Check(CheckError::Refused(_))));
        assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"old");
        let mut judged = None;
        adopt(&source, &image, |staged| {
            judged = Some(staged.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"new");
        let judged = judged.unwrap();
        assert_ne!(
            judged, source,
            "the clone is judged, never the person's bundle"
        );
        assert!(!judged.exists(), "the replaced image is gone");
        let mode = std::fs::metadata(executable_of(&image)).unwrap().mode();
        assert_eq!(mode & 0o022, 0, "no group or other write bit: {mode:o}");
        let entries: Vec<_> = std::fs::read_dir(image.parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1, "no staging left beside the image");
        assert_eq!(std::fs::read(executable_of(&source)).unwrap(), b"new");

        // A temporary staging obstruction is ordinary filesystem failure, not
        // a code refusal or a new source generation. Its repair must be noticed
        // by the next ordinary watch round without restarting the broker.
        bundle(&image, b"old");
        let image_exe = executable_of(&image);
        let original = stamp(&source).unwrap();
        let staging = image
            .parent()
            .unwrap()
            .join(format!(".adopting-{}.app", std::process::id()));
        std::fs::write(&staging, b"temporary obstruction").unwrap();
        let mut observation = Adoption::default();
        let now = std::cell::Cell::new(Duration::ZERO);
        let first = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |_| {
                    panic!("the failed staging cleanup precedes code verification")
                })
            },
        );
        assert!(matches!(
            first,
            Round::Failed {
                error: Failure::Io(_),
                report: true
            }
        ));
        assert_eq!(std::fs::read(&image_exe).unwrap(), b"old");
        std::fs::remove_file(&staging).unwrap();
        now.set(LOOK_EVERY);
        let retried = observation.round(
            || now.get(),
            &source,
            &image,
            || adopt(&source, &image, |_| Ok(())),
        );
        assert_eq!(
            stamp(&source).unwrap(),
            original,
            "the source did not change"
        );
        assert_eq!(
            std::fs::read(&image_exe).unwrap(),
            b"new",
            "the ordinary round must retry the same source after transient failure"
        );
        assert!(matches!(retried, Round::Applied));

        bundle(&source, b"refused");
        let denied = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |_| {
                    Err(CheckError::Refused("different signer".into()))
                })
            },
        );
        assert!(matches!(
            denied,
            Round::Failed {
                error: Failure::Check(CheckError::Refused(_)),
                report: true
            }
        ));
        for _ in 0..20 {
            now.set(now.get() + LOOK_EVERY);
            assert!(matches!(
                observation.round(
                    || now.get(),
                    &source,
                    &image,
                    || panic!("unchanged refused source must not clone or reverify")
                ),
                Round::Quiet
            ));
        }
        bundle(&source, b"retry");
        let last_attempt = std::cell::Cell::new(None);
        let intervals = std::cell::RefCell::new(Vec::new());
        let mut reports = 0;
        for _ in 0..600 {
            let result = observation.round(
                || now.get(),
                &source,
                &image,
                || {
                    adopt(&source, &image, |_| {
                        if let Some(last) = last_attempt.replace(Some(now.get())) {
                            intervals.borrow_mut().push(now.get() - last);
                        }
                        Err(CheckError::Unanswered(
                            "the strict check is temporarily unavailable".into(),
                        ))
                    })
                },
            );
            reports += usize::from(matches!(result, Round::Failed { report: true, .. }));
            now.set(now.get() + LOOK_EVERY);
            if intervals
                .borrow()
                .ends_with(&[LONGEST_RETRY, LONGEST_RETRY])
            {
                break;
            }
        }
        assert_eq!(
            reports, 1,
            "one unchanged operational diagnostic is not logged every round"
        );
        assert_eq!(intervals.borrow().first(), Some(&LOOK_EVERY));
        assert_eq!(intervals.borrow().get(1), Some(&(LOOK_EVERY * 2)));
        assert!(
            intervals
                .borrow()
                .ends_with(&[LONGEST_RETRY, LONGEST_RETRY]),
            "operational retry remains live at its bounded rate"
        );
        assert!(
            intervals
                .borrow()
                .iter()
                .all(|interval| *interval <= LONGEST_RETRY)
        );
        let pending = observation.round(
            || now.get(),
            &source,
            &image,
            || panic!("the same retry is not due"),
        );
        assert!(matches!(pending, Round::Quiet));
        bundle(&source, b"a new source bypasses the old backoff");
        assert!(matches!(
            observation.round(
                || now.get(),
                &source,
                &image,
                || adopt(&source, &image, |_| Ok(()))
            ),
            Round::Applied
        ));
        assert_eq!(
            std::fs::read(&image_exe).unwrap(),
            b"a new source bypasses the old backoff"
        );

        bundle(&source, b"changing");
        let changed = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |_| {
                    bundle(&source, b"after the check started");
                    Err(CheckError::Refused("the old candidate was refused".into()))
                })
            },
        );
        assert!(matches!(changed, Round::Failed { .. }));
        assert!(
            !observation.settled,
            "a refusal from a changed source cannot consume its newer generation"
        );
        assert!(matches!(
            observation.round(
                || now.get(),
                &source,
                &image,
                || adopt(&source, &image, |_| Ok(()))
            ),
            Round::Applied
        ));
    }

    #[test]
    fn a_source_that_is_a_link_or_holds_one_out_of_the_bundle_is_never_adopted() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (real, link, image) = (
            root.path().join("real.app"),
            root.path().join("link.app"),
            root.path().join("lan").join("aterm.app"),
        );
        bundle(&real, b"new");
        bundle(&image, b"old");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut judged = false;
        adopt(&link, &image, |_| {
            judged = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!judged, "a link is refused before anything is judged");
        assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"old");
        assert!(
            !std::fs::symlink_metadata(&image)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the image is still a real folder"
        );

        let resources = real.join("Contents").join("Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::os::unix::fs::symlink("../MacOS/aterm", resources.join("inside")).unwrap();
        adopt(&real, &image, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"new");

        for out in ["../../../outside", "/Users//someone/evil"] {
            let escape = resources.join("escape");
            let _ = std::fs::remove_file(&escape);
            std::os::unix::fs::symlink(out, &escape).unwrap();
            bundle(&real, b"newer");
            let refused = adopt(&real, &image, |_| Ok(())).unwrap_err();
            assert!(matches!(refused, Failure::Refused(_)), "{out}");
            assert_eq!(
                std::fs::read(executable_of(&image)).unwrap(),
                b"new",
                "{out}"
            );
        }
        let entries: Vec<_> = std::fs::read_dir(image.parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1, "no staging left beside the image");
    }

    #[test]
    fn a_link_stays_inside_only_while_it_never_climbs_past_the_root() {
        assert!(stays_inside(Path::new("A"), 3));
        assert!(stays_inside(Path::new("../../.."), 3));
        assert!(!stays_inside(Path::new("../../../.."), 3));
        assert!(!stays_inside(Path::new("../x/../../.."), 1));
        assert!(!stays_inside(Path::new("/etc"), 3));
    }

    #[test]
    fn a_first_adoption_renames_the_clone_into_place() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (source, image) = (
            root.path().join("source.app"),
            root.path().join("aterm.app"),
        );
        bundle(&source, b"first");
        adopt(&source, &image, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(executable_of(&image)).unwrap(), b"first");
        assert!(same_bytes(&executable_of(&source), &executable_of(&image)).unwrap());
        bundle(&source, b"other");
        assert!(!same_bytes(&executable_of(&source), &executable_of(&image)).unwrap());
        std::fs::remove_file(executable_of(&source)).unwrap();
        std::fs::create_dir(executable_of(&source)).unwrap();
        assert!(matches!(
            Adoption::default().round(
                || Duration::ZERO,
                &source,
                &image,
                || panic!("an incomplete source must not be cloned or checked")
            ),
            Round::Failed {
                error: Failure::Io(_),
                ..
            }
        ));
    }
}
