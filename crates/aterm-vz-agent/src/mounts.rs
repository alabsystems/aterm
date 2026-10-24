// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Mount`: the `aterm` virtiofs share at [`AT`], and its directories bound read-only at
//! their host paths. The plan is read from `/proc/self/mountinfo` and is pure; the Linux
//! arm carries it out ([`crate::linux`]).

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

/// A mountinfo field with its octal escapes (`\040` for a space) decoded.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    let b = field.as_bytes();
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
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Every well-formed line of `/proc/self/mountinfo`'s text.
#[must_use]
pub fn parse(mountinfo: &str) -> Vec<Mounted> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (head, tail) = line.split_once(" - ")?;
            let head: Vec<&str> = head.split(' ').collect();
            let mut tail = tail.split(' ');
            if head.len() < 6 {
                return None;
            }
            Some(Mounted {
                root: unescape(head[3]),
                point: unescape(head[4]),
                fstype: tail.next()?.to_string(),
                source: tail.next()?.to_string(),
            })
        })
        .collect()
}

/// Whether `path` is absolute, names no `.` or `..`, and is not `/` or under [`AT`].
fn bindable(path: &Path) -> bool {
    path.is_absolute()
        && path.components().count() > 1
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && !path.starts_with(AT)
}

/// The steps that take `mounted` to the share at [`AT`] with exactly `shares` bound:
/// the share mounted if it is not, every bind of it an earlier `Mount` made detached
/// (deepest first), then each share bound and made read-only (shallowest first).
///
/// # Errors
/// A share's name is not one directory, or its path is not bindable.
pub fn plan(mounted: &[Mounted], shares: &[Share]) -> Result<Vec<Op>, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut binds = Vec::new();
    for s in shares {
        let name = Path::new(&s.name);
        let one_dir = name.components().count() == 1
            && matches!(name.components().next(), Some(Component::Normal(_)));
        if !one_dir {
            return Err(format!(
                "share `{}` is not one directory of the share",
                s.name
            ));
        }
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&s.path));
        if !bindable(&path) {
            return Err(format!(
                "share `{}` cannot be bound at {}: not an absolute path of plain directories outside {AT}",
                s.name,
                path.display()
            ));
        }
        binds.push((Path::new(AT).join(name), path));
    }
    let ours = |m: &&Mounted| m.fstype == FSTYPE && m.source == TAG;
    let at = Path::new(AT);
    let mut ops = Vec::new();
    if !mounted.iter().filter(ours).any(|m| m.point == at) {
        ops.push(Op::Mkdir(at.to_path_buf()));
        ops.push(Op::MountShare);
    }
    let mut stale: Vec<&PathBuf> = mounted
        .iter()
        .filter(ours)
        .map(|m| &m.point)
        .filter(|p| *p != at)
        .collect();
    stale.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    stale.dedup();
    ops.extend(stale.into_iter().map(|p| Op::Unmount(p.clone())));
    binds.sort_by_key(|(_, to)| to.components().count());
    for (from, to) in binds {
        ops.push(Op::Mkdir(to.clone()));
        ops.push(Op::Bind(from, to.clone()));
        ops.push(Op::ReadOnly(to));
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(name: &str, path: &str) -> Share {
        Share {
            name: name.into(),
            path: path.as_bytes().to_vec(),
        }
    }

    const BOOTED: &str = "\
25 1 253:0 / / rw,relatime shared:1 - ext4 /dev/vda rw
26 25 0:5 / /proc rw,nosuid shared:12 - proc proc rw
40 25 0:40 / /mnt/aterm-boot ro,relatime shared:20 - virtiofs aterm-boot ro
41 25 0:41 / /mnt/aterm-rosetta ro,relatime shared:21 - virtiofs rosetta ro
";

    #[test]
    fn mountinfo_lines_are_read_with_their_escapes() {
        let text = format!(
            "{BOOTED}50 25 0:42 /repo /Users//me/My\\040Work/aterm ro,relatime shared:30 - virtiofs aterm ro\nnot a line\n"
        );
        let m = parse(&text);
        assert_eq!(m.len(), 5);
        assert_eq!(
            m[4],
            Mounted {
                root: "/repo".into(),
                point: "/Users//me/My Work/aterm".into(),
                fstype: "virtiofs".into(),
                source: "aterm".into(),
            }
        );
        assert_eq!(m[2].source, "aterm-boot");
        assert_eq!(unescape("a\\134b\\011c"), PathBuf::from("a\\b\tc"));
        assert_eq!(unescape("bad\\9x\\04"), PathBuf::from("bad\\9x\\04"));
    }

    #[test]
    fn a_first_mount_mounts_the_share_then_binds_each_read_only_shallowest_first() {
        let ops = plan(
            &parse(BOOTED),
            &[
                share("git", "/Users//me/aterm/.git"),
                share("repo", "/Users//me/aterm"),
            ],
        )
        .unwrap();
        let p = PathBuf::from;
        assert_eq!(
            ops,
            [
                Op::Mkdir(p("/mnt/aterm")),
                Op::MountShare,
                Op::Mkdir(p("/Users//me/aterm")),
                Op::Bind(p("/mnt/aterm/repo"), p("/Users//me/aterm")),
                Op::ReadOnly(p("/Users//me/aterm")),
                Op::Mkdir(p("/Users//me/aterm/.git")),
                Op::Bind(p("/mnt/aterm/git"), p("/Users//me/aterm/.git")),
                Op::ReadOnly(p("/Users//me/aterm/.git")),
            ]
        );
    }

    #[test]
    fn a_later_mount_detaches_every_earlier_bind_deepest_first_and_keeps_the_share() {
        let mounted = format!(
            "{BOOTED}\
             60 25 0:42 / /mnt/aterm ro,relatime shared:30 - virtiofs aterm ro\n\
             61 25 0:42 /repo /Users//me/old ro,relatime shared:30 - virtiofs aterm ro\n\
             62 61 0:42 /git /Users//me/old/.git ro,relatime shared:30 - virtiofs aterm ro\n"
        );
        let ops = plan(&parse(&mounted), &[share("repo", "/Users//me/new")]).unwrap();
        let p = PathBuf::from;
        assert_eq!(
            ops,
            [
                Op::Unmount(p("/Users//me/old/.git")),
                Op::Unmount(p("/Users//me/old")),
                Op::Mkdir(p("/Users//me/new")),
                Op::Bind(p("/mnt/aterm/repo"), p("/Users//me/new")),
                Op::ReadOnly(p("/Users//me/new")),
            ],
            "the boot and rosetta shares are another tag's and stay"
        );
    }

    #[test]
    fn a_share_that_is_not_one_directory_or_not_bindable_is_refused() {
        let m = parse(BOOTED);
        for bad in [
            share("", "/x"),
            share("..", "/x"),
            share("a/b", "/x"),
            share("/abs", "/x"),
            share("repo", "relative/path"),
            share("repo", "/"),
            share("repo", "/Users/../etc"),
            share("repo", "/mnt/aterm/repo"),
            share("repo", "/mnt/aterm"),
        ] {
            assert!(plan(&m, std::slice::from_ref(&bad)).is_err(), "{bad:?}");
        }
        assert!(plan(&m, &[]).is_ok());
    }
}
