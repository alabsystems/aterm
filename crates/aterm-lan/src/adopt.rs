// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's image (`docs/DESIGN-lan-broker-2026-11-05.md` §3): a root-owned clone of the app
//! bundle the elevation ran from. The broker watches that `--source` bundle's executable; when it
//! changes and the running image has a team, a clone of the new bundle that passes a strict
//! static check against the team requirement is swapped in for the image and the broker exits 0,
//! for launchd's `KeepAlive` to start it. An ad-hoc image adopts nothing (the next build's
//! elevation renews it). The broker also exits when its own image is replaced.

use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_uds::process::LoadedImage;

use crate::server::log;

/// How often the source and the image are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(5);

/// The executable inside a bundle.
fn executable_of(bundle: &Path) -> PathBuf {
    bundle.join("Contents").join("MacOS").join("aterm")
}

/// A file as it is on disk: device, inode, size, modification time.
type Stamp = (u64, u64, u64, i64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)?;
    Some((meta.dev(), meta.ino(), meta.size(), meta.mtime()))
}

/// Whether two files hold the same bytes (`None`: one could not be read).
fn same_bytes(a: &Path, b: &Path) -> Option<bool> {
    let (mut a, mut b) = (std::fs::File::open(a).ok()?, std::fs::File::open(b).ok()?);
    if a.metadata().ok()?.len() != b.metadata().ok()?.len() {
        return Some(false);
    }
    let (mut x, mut y) = ([0u8; 32 * 1024], [0u8; 32 * 1024]);
    loop {
        let n = a.read(&mut x).ok()?;
        if n == 0 {
            return Some(true);
        }
        b.read_exact(&mut y[..n]).ok()?;
        if x[..n] != y[..n] {
            return Some(false);
        }
    }
}

/// Start watching `source` (the app bundle) and this process's own image, on a thread of its own.
///
/// # Errors
/// The thread could not start.
pub(crate) fn watch(source: PathBuf) -> io::Result<()> {
    let own = std::env::current_exe()?;
    let image = LoadedImage::read(i32::try_from(std::process::id()).unwrap_or(0));
    let checker = adoption_check();
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
            let source_exe = executable_of(&source);
            let mut seen = None;
            loop {
                if image.as_ref().and_then(|image| image.matches_file(&own)) == Some(false) {
                    log(format_args!(
                        "{} was replaced: exiting for launchd to start it",
                        own.display()
                    ));
                    std::process::exit(0);
                }
                let now = stamp(&source_exe);
                if adopting && now.is_some() && now != seen {
                    seen = now;
                    if same_bytes(&source_exe, &own) == Some(false)
                        && let Some(check) = &checker
                    {
                        let image_bundle = Path::new(aterm_uds::lan::IMAGE_BUNDLE);
                        match adopt(&source, image_bundle, |staged| {
                            check.admits_code_at_strictly(staged)
                        }) {
                            Ok(()) => {
                                log(format_args!(
                                    "adopted {} as {}: exiting for launchd to start it",
                                    source.display(),
                                    image_bundle.display()
                                ));
                                std::process::exit(0);
                            }
                            Err(error) => {
                                log(format_args!("did not adopt {}: {error}", source.display()))
                            }
                        }
                    }
                }
                std::thread::sleep(LOOK_EVERY);
            }
        })?;
    Ok(())
}

/// The check a bundle must pass to be adopted: the running image's team requirement, or `None`
/// for an ad-hoc image (or one whose team cannot be read).
fn adoption_check() -> Option<aterm_keeper::identity::Identity> {
    let team = match aterm_keeper::identity::own_team() {
        Ok(Some(team)) => team,
        Ok(None) => return None,
        Err(why) => {
            log(format_args!(
                "the running image's team could not be read: {why}"
            ));
            return None;
        }
    };
    let requirement = crate::server::team_requirement(&team).ok()?;
    match aterm_keeper::identity::Identity::for_requirement(&requirement) {
        Ok(identity) => Some(identity),
        Err(why) => {
            log(format_args!("the team requirement did not compile: {why}"));
            None
        }
    }
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
fn clone(from: &Path, to: &Path) -> io::Result<()> {
    let folder = open_folder(from).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{} is not a bundle folder (a link is never adopted): {error}",
                from.display()
            ),
        )
    })?;
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
        Err(io::Error::last_os_error())
    }
}

/// `Ok` when `path` is a real folder (no link) owned by this process's user.
///
/// # Errors
/// `PermissionDenied` naming what it is instead, or the `lstat`'s error.
fn owned_folder(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    let ours = aterm_uds::peer::our_uid();
    if !meta.file_type().is_dir() || meta.uid() != ours {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a folder of uid {ours} (a {:?} of uid {})",
                path.display(),
                meta.file_type(),
                meta.uid()
            ),
        ));
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
fn strip_write(root: &Path, depth: usize) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(root)?;
        // A link's own depth is its folder's.
        if !stays_inside(&target, depth.saturating_sub(1)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} leads out of the bundle ({})",
                    root.display(),
                    target.display()
                ),
            ));
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
/// confirm `image` is that real folder, and remove what it replaced. A clone refused at any step
/// is removed and `image` is left as it was.
///
/// # Errors
/// `source` that is no real folder, the clone's, a clone that is not this user's real folder or
/// that holds a link out of it, the check's (`PermissionDenied`, its words), the swap's, or an
/// `image` that is not this user's real folder after it.
pub(crate) fn adopt(
    source: &Path,
    image: &Path,
    check: impl FnOnce(&Path) -> Result<(), String>,
) -> io::Result<()> {
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
        .map_err(|error| error.to_string())
        .and_then(|()| check(&staging));
    if let Err(why) = judged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
    }
    if std::fs::symlink_metadata(image).is_ok() {
        if let Err(error) = swap(&staging, image) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        // `staging` now names the image that was replaced.
        std::fs::remove_dir_all(&staging)?;
    } else {
        std::fs::rename(&staging, image)?;
    }
    owned_folder(image)
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
        let refused = adopt(&source, &image, |_| Err("not the team".to_owned())).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
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
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{out}");
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
        assert_eq!(
            same_bytes(&executable_of(&source), &executable_of(&image)),
            Some(true)
        );
        bundle(&source, b"other");
        assert_eq!(
            same_bytes(&executable_of(&source), &executable_of(&image)),
            Some(false)
        );
    }
}
