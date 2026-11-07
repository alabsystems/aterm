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
use std::os::unix::fs::MetadataExt as _;
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
        // Filesystem shape can be repaired without touching the executable
        // (a resource link restored to its signed target). Only a definitive
        // code-identity refusal can settle this executable generation.
        if matches!(error, Failure::Check(CheckError::Refused(_))) && stable {
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
    adopt_allocating(source, image, check, |parent| {
        aterm_tempfile::Builder::new()
            .prefix(".adopting-")
            .tempdir_in(parent)
    })
}

fn adopt_allocating(
    source: &Path,
    image: &Path,
    check: impl FnOnce(&Path) -> Result<(), CheckError>,
    allocate: impl FnOnce(&Path) -> io::Result<aterm_tempfile::TempDir>,
) -> Result<(), Failure> {
    let dir = image
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an image with no folder"))?;
    // The private parent exists before cloning. An inherited source ACL can
    // grant no account access to candidate files before the seal removes it.
    let owner = allocate(dir)?;
    let staging = owner.path().join("aterm.app");
    aterm_uds::app_image::clone(source, &staging)?;
    // SAFETY: getegid reads the caller's group and cannot fail or mutate state.
    let gid = unsafe { libc::getegid() };
    let judged = owned_folder(&staging)
        .and_then(|()| {
            aterm_uds::app_image::seal(&staging, aterm_uds::peer::our_uid(), gid, true)
                .map_err(Failure::from)
        })
        .and_then(|()| check(&staging).map_err(Failure::Check));
    judged?;
    if std::fs::symlink_metadata(image).is_ok() {
        if let Err(error) = swap(&staging, image) {
            return Err(error.into());
        }
    } else {
        std::fs::rename(&staging, image)?;
    }
    // `staging` is now the retired image (or absent after a first rename).
    // Preserve the post-publication result even if Drop later manages cleanup.
    std::fs::remove_dir_all(owner.path())
        .map_err(|error| Failure::Published(Box::new(error.into())))?;
    let _ = owner.keep();
    owned_folder(image).map_err(|error| Failure::Published(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

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
        let staging_parent = root.path().join("unavailable-staging-parent");
        std::fs::write(&staging_parent, b"temporary obstruction").unwrap();
        let mut observation = Adoption::default();
        let now = std::cell::Cell::new(Duration::ZERO);
        let first = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt_allocating(
                    &source,
                    &image,
                    |_| panic!("failed private allocation precedes code verification"),
                    |_| aterm_tempfile::TempDir::new_in(&staging_parent),
                )
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
        std::fs::remove_file(&staging_parent).unwrap();
        std::fs::create_dir(&staging_parent).unwrap();
        now.set(LOOK_EVERY);
        let retried = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt_allocating(
                    &source,
                    &image,
                    |_| Ok(()),
                    |_| aterm_tempfile::TempDir::new_in(&staging_parent),
                )
            },
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

        // Resource-only repair leaves the executable stamp unchanged. The
        // actual signed bundle becomes admissible again at the existing retry.
        aterm_uds::deadline::require_trust_service();
        let resources = source.join("Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        let resource = resources.join("restored-link");
        std::os::unix::fs::symlink("../MacOS/aterm", &resource).unwrap();
        std::fs::copy("/usr/bin/true", executable_of(&source)).unwrap();
        std::fs::write(source.join("Contents/Info.plist"), b"<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>aterm</string><key>CFBundleIdentifier</key><string>com.aterm.adoption-fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>").unwrap();
        let signed = aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&source)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        let identity = Identity::for_code_at(&source).unwrap();
        let signed_stamp = stamp(&source).unwrap();
        std::fs::remove_file(&resource).unwrap();
        std::os::unix::fs::symlink("../../../outside", &resource).unwrap();
        let damaged = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |candidate| {
                    identity.admits_code_at_strictly(candidate)
                })
            },
        );
        assert!(matches!(
            damaged,
            Round::Failed {
                error: Failure::Refused(_),
                ..
            }
        ));
        assert!(!observation.settled);
        assert_eq!(std::fs::read(&image_exe).unwrap(), b"new");
        std::fs::remove_file(&resource).unwrap();
        std::os::unix::fs::symlink("../MacOS/aterm", &resource).unwrap();
        assert_eq!(stamp(&source).unwrap(), signed_stamp);
        now.set(now.get() + LOOK_EVERY);
        let repaired = observation.round(
            || now.get(),
            &source,
            &image,
            || {
                adopt(&source, &image, |candidate| {
                    identity.admits_code_at_strictly(candidate)
                })
            },
        );
        assert!(matches!(repaired, Round::Applied));
        assert_eq!(
            std::fs::read(&image_exe).unwrap(),
            std::fs::read(executable_of(&source)).unwrap()
        );

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

        for out in ["../../../outside", "/Users//someone/evil", "../x/../../.."] {
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
