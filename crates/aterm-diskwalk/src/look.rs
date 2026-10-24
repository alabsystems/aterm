// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! No-follow directory marker and symlink target inspection for root discovery.

#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

use crate::refuse::{Refusal, canonical, is_mount_point, refuse_look, within};
use crate::walk::Policy;

/// What a name is inside a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link (not followed).
    Link,
    /// Anything else (a socket, a device, a fifo).
    Other,
}

/// A git worktree root ([`worktree_above`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// The folder holding `.git`.
    pub root: PathBuf,
    /// Whether `.git` is a file (a linked worktree).
    pub linked: bool,
}

/// What `name` is inside `dir`, by one no-follow `stat`; `None` when nothing has that name.
/// # Errors
/// The [`Refusal`] `dir` earned before anything was opened, or the open's.
pub fn marker_at(dir: &Path, name: &str, policy: &Policy) -> Result<Option<Marker>, Refusal> {
    let [one] = markers_at(dir, [name], policy)?;
    Ok(one)
}

/// What each of `names` is inside `dir` through one open of `dir`.
/// # Errors
/// The [`Refusal`] `dir` earned before anything was opened, or the open's or a `stat`'s
/// (other than `ENOENT`, which is `None` for that name).
pub fn markers_at<const N: usize>(
    dir: &Path,
    names: [&str; N],
    policy: &Policy,
) -> Result<[Option<Marker>; N], Refusal> {
    let canon = tested(dir, policy)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        let fd = crate::walk::imp::open_checked(&canon)?;
        let mut out = [None; N];
        for (slot, name) in out.iter_mut().zip(names) {
            let e = crate::list::portable::stat_at(fd.as_fd(), name.as_bytes());
            *slot = match e.error {
                Some(libc::ENOENT) => None,
                Some(errno) => return Err(crate::walk::errno_refusal(errno)),
                None => Some(match e.kind {
                    crate::list::Kind::Dir => Marker::Dir,
                    crate::list::Kind::File => Marker::File,
                    crate::list::Kind::Link => Marker::Link,
                    crate::list::Kind::Other => Marker::Other,
                }),
            };
        }
        Ok(out)
    }
    #[cfg(not(unix))]
    {
        let _ = (canon, names);
        Err(Refusal::Unsupported)
    }
}

/// Where the link `name` inside `dir` points, resolved lexically against `dir`.
/// # Errors
/// The [`Refusal`] `dir` earned, or the open's.
pub fn link_target(dir: &Path, name: &str, policy: &Policy) -> Result<Option<PathBuf>, Refusal> {
    let canon = tested(dir, policy)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        let fd = crate::walk::imp::open_checked(&canon)?;
        let Ok(cname) = std::ffi::CString::new(name) else {
            return Ok(None);
        };
        let mut buf = vec![0u8; 4_096];
        // SAFETY: `fd` is an open directory, `cname` is NUL-terminated, and the buffer's length bounds the write.
        let n = unsafe {
            libc::readlinkat(
                fd.as_raw_fd(),
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return match errno {
                libc::EINVAL | libc::ENOENT => Ok(None),
                e => Err(crate::walk::errno_refusal(e)),
            };
        }
        buf.truncate(usize::try_from(n).unwrap_or(0));
        let target = Path::new(std::ffi::OsStr::from_bytes(&buf));
        Ok(Some(lexical_join(&canon, target)))
    }
    #[cfg(not(unix))]
    {
        let _ = (canon, name);
        Err(Refusal::Unsupported)
    }
}

/// The nearest git worktree root at or above `start` below the home folder and any mount point.
#[must_use]
pub fn worktree_above(start: &Path, policy: &Policy) -> Option<Worktree> {
    let canon = canonical(start)?;
    let fold = policy.deny.fold_case();
    for dir in canon.ancestors() {
        let at_or_above_home = within(policy.deny.home(), dir, fold);
        let mount_point = is_mount_point(dir, &policy.mounts, fold);
        if at_or_above_home || mount_point || dir.parent().is_none() {
            return None;
        }
        match marker_at(dir, ".git", policy) {
            Ok(Some(Marker::Dir)) => {
                return Some(Worktree {
                    root: dir.to_path_buf(),
                    linked: false,
                });
            }
            Ok(Some(Marker::File)) => {
                return Some(Worktree {
                    root: dir.to_path_buf(),
                    linked: true,
                });
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

const BUILD_MARKERS: [&str; 3] = ["CACHEDIR.TAG", ".rustc_info.json", ".cargo-lock"];

/// The nearest cargo target or profile folder at or above `start`.
#[must_use]
pub fn build_dir_above(start: &Path, policy: &Policy) -> Option<PathBuf> {
    let canon = canonical(start)?;
    let fold = policy.deny.fold_case();
    let mut profile: Option<PathBuf> = None;
    for dir in canon.ancestors() {
        let at_or_above_home = within(policy.deny.home(), dir, fold);
        let mount_point = is_mount_point(dir, &policy.mounts, fold);
        if at_or_above_home || mount_point || dir.parent().is_none() {
            break;
        }
        match markers_at(dir, BUILD_MARKERS, policy) {
            Ok([tag, info, lock]) => {
                if tag == Some(Marker::File) || info == Some(Marker::File) {
                    return Some(dir.to_path_buf());
                }
                if lock == Some(Marker::File) && profile.is_none() {
                    profile = Some(dir.to_path_buf());
                }
            }
            Err(_) => break,
        }
    }
    profile
}

fn tested(dir: &Path, policy: &Policy) -> Result<PathBuf, Refusal> {
    let canon = crate::walk::walk_path(dir)?;
    match refuse_look(Some(dir), &canon, &policy.mounts, &policy.deny) {
        Some(why) => Err(why),
        None => Ok(canon),
    }
}

/// `base` joined with `target`, `.` and `..` resolved by text (an absolute
/// `target` replaces `base`).
#[cfg(unix)]
fn lexical_join(base: &Path, target: &Path) -> PathBuf {
    let mut out = if target.is_absolute() {
        PathBuf::from("/")
    } else {
        base.to_path_buf()
    };
    for c in target.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}
