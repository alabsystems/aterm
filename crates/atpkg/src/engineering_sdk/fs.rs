// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original-directory traversal and held-file COW construction. No receipt is an input owner.

use std::ffi::{CString, OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use aterm_dirfd::{AtFlags, Mode, OFlags};
use serde::Serialize;

use super::{invalid, time};

pub(super) fn directory(parent: &File, name: &OsStr) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        aterm_dirfd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}
pub(super) fn regular(parent: &File, name: &OsStr) -> io::Result<File> {
    let file = aterm_uds::kept_out(|| {
        aterm_dirfd::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })?;
    crate::local_harness::fs::owned_file(&file)?;
    Ok(file)
}
pub(super) fn sync(file: &File) -> io::Result<()> {
    aterm_durable::refusal_accepted(aterm_durable::primitive::fsync(file))
}
pub(super) fn mkdir(parent: &File, name: &str) -> io::Result<File> {
    aterm_dirfd::mkdirat(parent, name, Mode::from_bits(0o700)).map_err(io::Error::from)?;
    let dir = directory(parent, OsStr::new(name))?;
    sync(parent)?;
    Ok(dir)
}

/// Outer ancestors are identity/permissions bookends, not clocks for unrelated siblings.
pub(super) struct Pin {
    chain: Vec<(OsString, File, Stamp)>,
    path: PathBuf,
}
impl Pin {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute()
            || path.components().count() > 64
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
            || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        {
            return Err(invalid(
                "engineering input path is not bounded absolute component syntax",
            ));
        }
        let root = aterm_uds::kept_out(|| {
            aterm_dirfd::openat(
                aterm_dirfd::CWD,
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(io::Error::from)
        })?;
        let root_stamp = Stamp::of(&root.metadata()?);
        let mut chain = vec![(OsString::from("/"), root, root_stamp)];
        for part in path.components() {
            if let Component::Normal(name) = part {
                let file = directory(&chain.last().unwrap().1, name)?;
                let stamp = Stamp::of(&file.metadata()?);
                chain.push((name.to_owned(), file, stamp));
            }
        }
        let pin = Self {
            chain,
            path: path.to_owned(),
        };
        pin.check()?;
        Ok(pin)
    }
    pub(super) fn held(&self) -> &File {
        &self.chain.last().unwrap().1
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn check(&self) -> io::Result<()> {
        for pair in self.chain.windows(2) {
            let named = directory(&pair[0].1, &pair[1].0)?;
            let now = Stamp::of(&named.metadata()?);
            if !pair[1].2.same_owner(&now)
                || !pair[1].2.same_owner(&Stamp::of(&pair[1].1.metadata()?))
            {
                return Err(invalid("engineering input ancestor changed"));
            }
        }
        Ok(())
    }
    pub(super) fn check_original_root(&self) -> io::Result<()> {
        self.check()?;
        if self.chain.last().unwrap().2 != Stamp::of(&self.held().metadata()?) {
            return Err(invalid("original selected input namespace changed"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    size: u64,
    links: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    flags: u32,
}
impl Stamp {
    pub(super) fn of(m: &Metadata) -> Self {
        use std::os::macos::fs::MetadataExt as _;
        Self {
            dev: m.dev(),
            ino: m.ino(),
            uid: m.uid(),
            gid: m.gid(),
            mode: m.mode(),
            size: m.len(),
            links: m.nlink(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
            flags: m.st_flags(),
        }
    }
    fn same_owner(&self, other: &Self) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
            && self.flags == other.flags
    }
    pub(super) fn identity(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
    pub(super) fn mode(&self) -> u32 {
        self.mode & 0o777
    }
    fn at(parent: &File, name: &str) -> io::Result<Self> {
        let name = CString::new(name).map_err(|_| invalid("NUL entry name"))?;
        let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the original directory and C string stay live; fstatat initializes
        // exactly one native stat on success and never follows this final name.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                raw.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful fstatat above initialized every field.
        let m = unsafe { raw.assume_init() };
        Ok(Self {
            dev: u64::try_from(m.st_dev).map_err(|_| invalid("negative device identifier"))?,
            ino: m.st_ino,
            uid: m.st_uid,
            gid: m.st_gid,
            mode: u32::from(m.st_mode),
            size: u64::try_from(m.st_size).map_err(|_| invalid("negative entry size"))?,
            links: u64::from(m.st_nlink),
            mtime: m.st_mtime,
            mtime_ns: m.st_mtime_nsec,
            ctime: m.st_ctime,
            ctime_ns: m.st_ctime_nsec,
            flags: m.st_flags,
        })
    }
}

pub(super) fn hash(file: &File, deadline: Instant) -> io::Result<(u64, String)> {
    let before = Stamp::of(&file.metadata()?);
    if before.size > 2 << 30 {
        return Err(invalid("engineering input file exceeds 2GiB"));
    }
    let mut hasher = aterm_digest::Sha256::new();
    let mut offset = 0;
    let mut buffer = vec![0u8; 128 << 10];
    loop {
        time(deadline)?;
        let n = file.read_at(&mut buffer, offset)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        if offset > before.size {
            return Err(invalid("engineering input grew"));
        }
        hasher.update(&buffer[..n]);
    }
    if offset != before.size || before != Stamp::of(&file.metadata()?) {
        return Err(invalid("engineering input changed while hashing"));
    }
    Ok((offset, aterm_codec::hex::encode_lower(&hasher.finalize())))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Entry {
    Directory {
        path: String,
        stamp: Stamp,
    },
    File {
        path: String,
        stamp: Stamp,
        bytes: u64,
        sha256: String,
    },
    Link {
        path: String,
        stamp: Stamp,
        target: String,
    },
}
impl Entry {
    pub(super) fn path(&self) -> &str {
        match self {
            Self::Directory { path, .. } | Self::File { path, .. } | Self::Link { path, .. } => {
                path
            }
        }
    }
}

/// Captured from actual original inputs only. Nothing deserializes this type.
#[derive(PartialEq, Eq)]
pub(super) struct Snapshot {
    pub(super) root: Stamp,
    pub(super) entries: Vec<Entry>,
    pub(super) omitted_source_namespaces: Vec<Entry>,
    total_bytes: u64,
    runtime_only: bool,
}
impl Snapshot {
    pub(super) fn capture(root: &File, deadline: Instant) -> io::Result<Self> {
        Self::capture_mode(root, deadline, false)
    }
    pub(super) fn capture_runtime(root: &File, deadline: Instant) -> io::Result<Self> {
        Self::capture_mode(root, deadline, true)
    }
    fn capture_mode(root: &File, deadline: Instant, runtime_only: bool) -> io::Result<Self> {
        let mut snapshot = Self {
            root: Stamp::of(&root.metadata()?),
            entries: Vec::new(),
            omitted_source_namespaces: Vec::new(),
            total_bytes: 0,
            runtime_only,
        };
        snapshot.walk(root, "", deadline)?;
        snapshot.check_links()?;
        Ok(snapshot)
    }
    fn walk(&mut self, dir: &File, path: &str, deadline: Instant) -> io::Result<()> {
        time(deadline)?;
        if path.split('/').count() > 32 {
            return Err(invalid("engineering tree depth exceeds 32"));
        }
        let before = Stamp::of(&dir.metadata()?);
        match astream_sys::owner_private_dir(dir)? {
            Ok(_) => {}
            Err(astream_sys::NotPrivate::Mode(mode)) if mode & 0o022 == 0 => {}
            Err(why) => return Err(invalid(format!("SDK directory custody: {why}"))),
        }
        let mut names = Vec::new();
        for entry in aterm_dirfd::Dir::read_from(dir).map_err(io::Error::from)? {
            time(deadline)?;
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            let name = std::str::from_utf8(name).map_err(|_| invalid("non-UTF8 SDK entry"))?;
            if name.is_empty() || name.len() > 255 || name.bytes().any(|b| b < 32 || b == 127) {
                return Err(invalid("SDK entry name is not bounded printable UTF8"));
            }
            names.push(name.to_owned());
            if self.entries.len() + names.len() > 32768 {
                return Err(invalid("SDK inventory exceeds 32768 entries"));
            }
        }
        names.sort();
        for name in names {
            if self.entries.len() >= 32768 {
                return Err(invalid("SDK inventory exceeds 32768 entries"));
            }
            let rel = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            let stat = aterm_dirfd::fstatat(dir, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            if self.runtime_only
                && matches!(
                    rel.as_str(),
                    "lib/rustlib/src" | "lib/rustlib/rustc-src" | "lib/rustlib/source-snapshot"
                )
            {
                // These are explicit *non-input* source namespaces, never runtime or source
                // custody. Keep their actual named-entry observations, without traversing,
                // cloning, hashing, or claiming anything about their contents.
                let stamp = Stamp::at(dir, &name)?;
                let omission = match stat.st_mode & 0o170000 {
                    0o040000 => Entry::Directory {
                        path: rel,
                        stamp: stamp.clone(),
                    },
                    0o120000 => Entry::Link {
                        path: rel,
                        stamp: stamp.clone(),
                        target: read_link(dir, &name)?,
                    },
                    _ => {
                        return Err(invalid(
                            "declared source namespace is neither directory nor alias",
                        ));
                    }
                };
                if stamp != Stamp::at(dir, &name)? {
                    return Err(invalid("source namespace entry changed during observation"));
                }
                self.omitted_source_namespaces.push(omission);
                continue;
            }
            match stat.st_mode & 0o170000 {
                0o040000 => {
                    let child = directory(dir, OsStr::new(&name))?;
                    let stamp = Stamp::of(&child.metadata()?);
                    self.entries.push(Entry::Directory {
                        path: rel.clone(),
                        stamp,
                    });
                    self.walk(&child, &rel, deadline)?;
                }
                0o100000 => {
                    let file = regular(dir, OsStr::new(&name))?;
                    let stamp = Stamp::of(&file.metadata()?);
                    if stamp.mode & 0o7022 != 0 || stamp.flags != 0 {
                        return Err(invalid(
                            "SDK input grants writes or carries special permissions/flags",
                        ));
                    }
                    let (bytes, sha256) = hash(&file, deadline)?;
                    self.total_bytes = self
                        .total_bytes
                        .checked_add(bytes)
                        .ok_or_else(|| invalid("SDK byte population overflow"))?;
                    if self.total_bytes > 64 << 30 {
                        return Err(invalid("SDK input byte population exceeds 64GiB"));
                    }
                    self.entries.push(Entry::File {
                        path: rel,
                        stamp,
                        bytes,
                        sha256,
                    });
                }
                0o120000 => {
                    let stamp = Stamp::at(dir, &name)?;
                    let target = read_link(dir, &name)?;
                    if stamp != Stamp::at(dir, &name)? {
                        return Err(invalid("SDK alias changed during capture"));
                    }
                    self.entries.push(Entry::Link {
                        path: rel,
                        stamp,
                        target,
                    });
                }
                _ => {
                    return Err(invalid(
                        "SDK contains a non-directory/file/internal-alias entry",
                    ));
                }
            }
        }
        if before != Stamp::of(&dir.metadata()?) {
            return Err(invalid("SDK namespace changed during capture"));
        }
        Ok(())
    }
    fn check_links(&self) -> io::Result<()> {
        let by_path = self
            .entries
            .iter()
            .map(|entry| (entry.path(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        for entry in &self.entries {
            if let Entry::Link { path, target, .. } = entry {
                let mut current = path.clone();
                let mut next = target.clone();
                let mut terminal = false;
                for _ in 0..32 {
                    current = internal_target(&current, &next)?;
                    match by_path.get(current.as_str()).copied() {
                        Some(Entry::Link { target, .. }) => next = target.clone(),
                        Some(_) => {
                            terminal = true;
                            break;
                        }
                        None => return Err(invalid("SDK alias has no inventoried target")),
                    }
                }
                if !terminal {
                    return Err(invalid("SDK alias cycle or depth exceeds 32"));
                }
            }
        }
        Ok(())
    }
    pub(super) fn recheck(&self, root: &File, deadline: Instant) -> io::Result<()> {
        if *self != Self::capture_mode(root, deadline, self.runtime_only)? {
            return Err(invalid("original SDK inventory/bytes/stamps changed"));
        }
        Ok(())
    }
}

pub(super) fn internal_target(path: &str, target: &str) -> io::Result<String> {
    if target.starts_with('/') || target.is_empty() {
        return Err(invalid("SDK alias is not internal relative syntax"));
    }
    let mut parts: Vec<_> = path.split('/').collect();
    parts.pop();
    for component in Path::new(target).components() {
        match component {
            Component::Normal(p) => {
                parts.push(p.to_str().ok_or_else(|| invalid("alias is not UTF8"))?)
            }
            Component::CurDir => {}
            Component::ParentDir if !parts.is_empty() => {
                parts.pop();
            }
            _ => return Err(invalid("SDK alias escapes its original tree")),
        }
    }
    Ok(parts.join("/"))
}

pub(super) fn parent(root: &File, path: &str) -> io::Result<(File, OsString)> {
    let path = Path::new(path);
    let name = path
        .file_name()
        .ok_or_else(|| invalid("empty relative entry"))?
        .to_owned();
    let mut dir = directory(root, OsStr::new("."))?;
    for part in path.parent().unwrap_or(Path::new("")).components() {
        let Component::Normal(name) = part else {
            return Err(invalid("noncanonical relative entry"));
        };
        dir = directory(&dir, name)?;
    }
    Ok((dir, name))
}
pub(super) fn clone_file(source: &File, destination: &File, name: &OsStr) -> io::Result<()> {
    let name = CString::new(name.as_bytes()).map_err(|_| invalid("NUL clone name"))?;
    unsafe extern "C" {
        fn fclonefileat(
            source: i32,
            directory: i32,
            path: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    // CLONE_NOFOLLOW | CLONE_NOOWNERCOPY: original source OFD, actual destination directory,
    // fresh independent inode; no path retry, byte-copy fallback, or source cursor mutation.
    // SAFETY: both original File owners and the C string outlive the native call.
    if unsafe {
        fclonefileat(
            source.as_raw_fd(),
            destination.as_raw_fd(),
            name.as_ptr(),
            3,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn link(dir: &File, name: &str, target: &str) -> io::Result<()> {
    let name = CString::new(name).map_err(|_| invalid("NUL link name"))?;
    let target = CString::new(target).map_err(|_| invalid("NUL link target"))?;
    // SAFETY: the original directory and both C strings stay live through symlinkat.
    if unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn read_link(dir: &File, name: &str) -> io::Result<String> {
    let name = CString::new(name).map_err(|_| invalid("NUL link name"))?;
    let mut bytes = vec![0u8; 4097];
    // SAFETY: original directory/name and the full writable buffer outlive readlinkat.
    let n = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    bytes.truncate(n as usize);
    if bytes.len() > 4096 {
        return Err(invalid("oversized alias"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("alias is not UTF8"))
}
pub(super) fn write_new(dir: &File, name: &str, bytes: &[u8]) -> io::Result<File> {
    let mut file = File::from(
        aterm_dirfd::openat(
            dir,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits(0o600),
        )
        .map_err(io::Error::from)?,
    );
    file.write_all(bytes)?;
    aterm_dirfd::fchmod(&file, Mode::from_bits(0o444)).map_err(io::Error::from)?;
    crate::store::sync_contents_or_accept_refusal(&file)?;
    sync(dir)?;
    Ok(file)
}
