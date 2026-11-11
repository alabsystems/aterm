// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Mount`: the `aterm` virtiofs share at [`AT`], and its directories bound read-only at
//! their host paths. The plan is read from `/proc/self/mountinfo` and is pure; the Linux
//! arm carries it out (`crate::linux`).

use std::path::{Component, Path, PathBuf};

use crate::wire::Share;

/// The share's virtiofs tag.
pub const TAG: &str = "aterm";

/// Where the share is mounted.
pub const AT: &str = "/mnt/aterm";

/// The file system the share is.
pub const FSTYPE: &str = "virtiofs";

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mounted {
    /// The directory within its file system it shows (`/` for a whole mount, the bound
    /// directory for a bind).
    pub root: PathBuf,
    /// Where it is mounted.
    pub point: PathBuf,
    /// Its file system type.
    pub fstype: String,
    /// Its source (a virtiofs mount's tag).
    pub source: String,
}

/// One step of a [`plan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Make the directory and its parents.
    Mkdir(PathBuf),
    /// Mount the `aterm` tag read-only at [`AT`].
    MountShare,
    /// Detach the mount at the path (`umount2(MNT_DETACH)`).
    Unmount(PathBuf),
    /// Bind the first path at the second.
    Bind(PathBuf, PathBuf),
    /// Remount the bind at the path read-only.
    ReadOnly(PathBuf),
}

fn unescape_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = |at: usize| b.get(at).filter(|c| (b'0'..=b'7').contains(c));
        if b[i] == b'\\'
            && let (Some(x), Some(y), Some(z)) = (octal(i + 1), octal(i + 2), octal(i + 3))
            && let Ok(byte) = u8::try_from(
                u32::from(x - b'0') * 64 + u32::from(y - b'0') * 8 + u32::from(z - b'0'),
            )
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// A mountinfo field with its octal escapes (`\040` for a space) decoded.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    if !field.contains('\\') {
        return PathBuf::from(field);
    }
    PathBuf::from(std::ffi::OsString::from_vec(unescape_bytes(
        field.as_bytes(),
    )))
}

fn unescape_str(field: &str) -> Option<String> {
    if !field.contains('\\') {
        return Some(field.to_owned());
    }
    String::from_utf8(unescape_bytes(field.as_bytes())).ok()
}

/// Every well-formed line of `/proc/self/mountinfo`'s text.
#[must_use]
pub fn parse(mountinfo: &str) -> Vec<Mounted> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (head, tail) = line.split_once(" - ")?;
            let mut head = head.split(' ');
            let root = head.nth(3)?;
            let point = head.next()?;
            head.next()?;
            let mut tail = tail.split(' ');
            Some(Mounted {
                root: unescape(root),
                point: unescape(point),
                fstype: unescape_str(tail.next()?)?,
                source: unescape_str(tail.next()?)?,
            })
        })
        .collect()
}

/// Whether `path` is absolute, names no `.` or `..`, and is not `/` or under [`AT`].
fn bindable(path: &Path) -> bool {
    let mut comps = path.components();
    if comps.next() != Some(Component::RootDir) {
        return false;
    }
    let Some(Component::Normal(_)) = comps.next() else {
        return false;
    };
    comps.all(|c| matches!(c, Component::Normal(_))) && !path.starts_with(AT)
}

/// The steps that take `mounted` to the share at [`AT`] with exactly `shares` bound: every
/// bind of it an earlier `Mount` made detached (deepest first), the share itself detached
/// and mounted afresh, then each share bound and made read-only (shallowest first).
///
/// The fresh mount is the helper's swap's other half (§7): after a swap the share's old
/// mount still holds each name's cached entry, and a bind made through it lands on the
/// directory the swap took away (measured in V4: the bind's root reads `/repo//deleted`
/// and its directory is gone). A new mount looks every name up again, unless a guest
/// process still references the old mount: virtiofs keeps one superblock per device while
/// anything does, and the new mount shares its cached entries. So the lab names each
/// checkout's entries apart (a swap binds names never looked up), and [`check`] holds every
/// bind to its own entry after the plan ran.
///
/// # Errors
/// A share's name is not one directory, or its path is not bindable. Two shares binding
/// the same path are also refused.
pub fn plan(mounted: &[Mounted], shares: &[Share]) -> Result<Vec<Op>, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut binds: Vec<(PathBuf, PathBuf)> = Vec::new();
    for s in shares {
        let valid_name =
            !s.name.is_empty() && s.name != "." && s.name != ".." && !s.name.contains(['/', '\0']);
        if !valid_name {
            return Err(format!(
                "share `{}` is not one directory of the share",
                s.name
            ));
        }
        let name = Path::new(&s.name);
        let valid_raw_path = !s.path.contains(&0) && s.path.starts_with(b"/") && {
            let mut any = false;
            let mut plain = true;
            for seg in s.path[1..]
                .split(|&b| b == b'/')
                .filter(|seg| !seg.is_empty())
            {
                any = true;
                if seg == b"." || seg == b".." {
                    plain = false;
                    break;
                }
            }
            any && plain
        };
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&s.path));
        if !valid_raw_path || !bindable(&path) {
            return Err(format!(
                "share `{}` cannot be bound at {}: not an absolute path of plain directories outside {AT}",
                s.name,
                path.display()
            ));
        }
        if binds.iter().any(|(_, prev)| *prev == path) {
            return Err(format!(
                "share `{}` duplicates bind path {}",
                s.name,
                path.display()
            ));
        }
        binds.push((Path::new(AT).join(name), path));
    }
    let ours = |m: &&Mounted| m.fstype == FSTYPE && m.source == TAG;
    let at = Path::new(AT);
    let mut ops = Vec::new();
    let mut stale: Vec<&PathBuf> = mounted
        .iter()
        .rev()
        .filter(ours)
        .map(|m| &m.point)
        .filter(|p| *p != at)
        .collect();
    stale.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    ops.extend(stale.into_iter().map(|p| Op::Unmount(p.clone())));
    let at_mounts = mounted
        .iter()
        .filter(ours)
        .filter(|m| m.point == at)
        .count();
    ops.extend(std::iter::repeat_n(
        Op::Unmount(at.to_path_buf()),
        at_mounts,
    ));
    ops.push(Op::Mkdir(at.to_path_buf()));
    ops.push(Op::MountShare);
    binds.sort_by_key(|(_, to)| to.components().count());
    for (from, to) in binds {
        ops.push(Op::Mkdir(to.clone()));
        ops.push(Op::Bind(from, to.clone()));
        ops.push(Op::ReadOnly(to));
    }
    Ok(ops)
}

/// Create `at` and any missing parent directories without traversing symbolic links.
///
/// # Errors
/// `at` is not an absolute path of plain components, a component along `at` is a symbolic
/// link or non-directory, or creating a missing directory fails.
pub fn ensure_plain_dir(at: &Path) -> Result<(), String> {
    let mut comps = at.components();
    if comps.next() != Some(Component::RootDir) {
        return Err(format!("mkdir {}: not an absolute path", at.display()));
    }
    let mut cur = PathBuf::from("/");
    for comp in comps {
        let Component::Normal(part) = comp else {
            return Err(format!(
                "mkdir {}: non-plain path component in {}",
                at.display(),
                cur.display()
            ));
        };
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "mkdir {}: {} is a symbolic link",
                    at.display(),
                    cur.display()
                ));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "mkdir {}: {} is not a directory",
                    at.display(),
                    cur.display()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match std::fs::create_dir(&cur) {
                Ok(()) => {}
                Err(ce) if ce.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = std::fs::symlink_metadata(&cur)
                        .map_err(|me| format!("mkdir {}: {me}", cur.display()))?;
                    if meta.file_type().is_symlink() || !meta.is_dir() {
                        return Err(format!(
                            "mkdir {}: {} is not a plain directory",
                            at.display(),
                            cur.display()
                        ));
                    }
                }
                Err(ce) => return Err(format!("mkdir {}: {ce}", cur.display())),
            },
            Err(e) => return Err(format!("mkdir {}: {e}", cur.display())),
        }
    }
    Ok(())
}

/// Whether every share is bound at its path from the share's own entry once a [`plan`] ran:
/// the topmost mount at the path is the `aterm` share's, rooted at `/<name>` exactly. A bind
/// made through a cached entry a swap took away reads `/<name>//deleted` (measured in V4),
/// and one made through another entry reads that entry's name.
///
/// # Errors
/// A share whose path shows no such mount, naming what it shows.
pub fn check(mounted: &[Mounted], shares: &[Share]) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt as _;
    for s in shares {
        let path = Path::new(std::ffi::OsStr::from_bytes(&s.path));
        match mounted.iter().rev().find(|m| m.point == path) {
            Some(m)
                if m.fstype == FSTYPE
                    && m.source == TAG
                    && m.root.strip_prefix("/") == Ok(Path::new(&s.name)) => {}
            Some(m) => {
                return Err(format!(
                    "share `{}` at {}: the mount there shows {} of {} `{}`, not /{}",
                    s.name,
                    path.display(),
                    m.root.display(),
                    m.fstype,
                    m.source,
                    s.name
                ));
            }
            None => {
                return Err(format!(
                    "share `{}`: nothing is mounted at {}",
                    s.name,
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "mounts_tests.rs"]
mod tests;
