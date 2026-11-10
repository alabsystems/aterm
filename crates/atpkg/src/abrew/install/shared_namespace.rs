// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transactional expansion of receipt-owned aliases into shared prefix directories.

use super::*;
use aterm_dirfd::{AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat, openat};
use std::ffi::{CStr, OsString};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

/// Admits a physical formula directory at the identical prefix-relative path.
/// A receipt does not authorize aliases into unrelated portions of a keg.
pub(super) fn owned_directory(prefix: &Prefix, link: &Path) -> Result<PathBuf, InstallError> {
    let refuse = || {
        format!(
            "abrew: refusing foreign link destination {}",
            link.display()
        )
    };
    check_directory(link.parent().ok_or_else(refuse)?, &prefix.root)?;
    let raw = std::fs::read_link(link).map_err(|_| refuse())?;
    let target = resolved_link_target(link, &raw).ok_or_else(refuse)?;
    let cellar = prefix.cellar();
    let mut parts = target
        .strip_prefix(&cellar)
        .map_err(|_| refuse())?
        .components();
    let (Some(std::path::Component::Normal(name)), Some(std::path::Component::Normal(version))) =
        (parts.next(), parts.next())
    else {
        return Err(refuse().into());
    };
    let (Some(name), Some(version)) = (name.to_str(), version.to_str()) else {
        return Err(refuse().into());
    };
    if !super::super::valid_component(name)
        || !super::super::valid_component(version)
        || target
            != prefix
                .keg(name, version)
                .join(link.strip_prefix(&prefix.root).map_err(|_| refuse())?)
        || !replacement_target_owned(prefix, link, &raw, &cellar.join(name))
    {
        return Err(refuse().into());
    }
    check_directory(&target, &prefix.root)?;
    Ok(target)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn open_directory(root: &Path, path: &Path) -> Result<OwnedFd, InstallError> {
    let mut fd = openat(aterm_dirfd::CWD, root, directory_flags(), Mode::empty())
        .map_err(|e| format!("abrew: cannot open namespace root: {e}"))?;
    for part in path
        .strip_prefix(root)
        .map_err(|_| "abrew: namespace escapes prefix".to_string())?
        .components()
    {
        let std::path::Component::Normal(name) = part else {
            return Err("abrew: invalid namespace component".to_string().into());
        };
        fd = openat(&fd, name, directory_flags(), Mode::empty())
            .map_err(|e| format!("abrew: cannot open namespace directory: {e}"))?;
    }
    Ok(fd)
}

fn stat(fd: &OwnedFd, name: &CStr) -> Result<Stat, InstallError> {
    aterm_dirfd::fstatat(
        fd,
        std::ffi::OsStr::from_bytes(name.to_bytes()),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| format!("abrew: {e}").into())
}

fn same(a: &Stat, b: &Stat) -> bool {
    (a.st_dev, a.st_ino, FileType::from_raw_mode(a.st_mode))
        == (b.st_dev, b.st_ino, FileType::from_raw_mode(b.st_mode))
}

fn read_link(fd: &OwnedFd, name: &CStr) -> Result<PathBuf, InstallError> {
    let mut bytes = vec![0u8; 256];
    loop {
        // SAFETY: the held directory, NUL-terminated name and writable buffer
        // remain valid throughout readlinkat. Its byte count is checked below.
        let count = unsafe {
            libc::readlinkat(
                fd.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            return Err(format!("abrew: {}", std::io::Error::last_os_error()).into());
        }
        let count = usize::try_from(count).map_err(|_| "abrew: invalid link length".to_string())?;
        if count < bytes.len() {
            bytes.truncate(count);
            return Ok(OsString::from_vec(bytes).into());
        }
        if bytes.len() >= 1024 * 1024 {
            return Err("abrew: namespace link is too long".to_string().into());
        }
        bytes.resize(bytes.len() * 2, 0);
    }
}

/// Copies no payload: every old leaf remains in its original keg. Directory
/// handles prevent a substituted symlink from becoming a traversal authority.
fn project(
    source: &Path,
    fd: &OwnedFd,
    parent: &OwnedFd,
    name: &CStr,
    destination: &Path,
) -> Result<OwnedFd, InstallError> {
    project_with(source, fd, parent, name, destination, &mut |_| Ok(()))
}

fn symlink_at(source: &Path, parent: &OwnedFd, name: &CStr) -> Result<(), InstallError> {
    let target =
        std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|e| format!("abrew: {e}"))?;
    // SAFETY: the held destination directory and both NUL-terminated names
    // outlive symlinkat. It creates an entry without following that name.
    let status = unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("abrew: {}", std::io::Error::last_os_error()).into())
    }
}

fn project_with(
    source: &Path,
    fd: &OwnedFd,
    parent: &OwnedFd,
    name: &CStr,
    destination: &Path,
    prepared: &mut impl FnMut(&Path) -> Result<(), InstallError>,
) -> Result<OwnedFd, InstallError> {
    let os_name = std::ffi::OsStr::from_bytes(name.to_bytes());
    aterm_dirfd::mkdirat(
        parent,
        os_name,
        Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH,
    )
    .map_err(|e| format!("abrew: {e}"))?;
    prepared(destination)?;
    let output = openat(parent, os_name, directory_flags(), Mode::empty())
        .map_err(|e| format!("abrew: cannot open projection directory: {e}"))?;
    for entry in Dir::read_from(fd).map_err(|e| format!("abrew: {e}"))? {
        let entry = entry.map_err(|e| format!("abrew: {e}"))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let os_name = std::ffi::OsStr::from_bytes(name.to_bytes());
        let before = stat(fd, name)?;
        let kind = FileType::from_raw_mode(before.st_mode);
        if kind.is_dir() {
            let child = openat(fd, os_name, directory_flags(), Mode::empty())
                .map_err(|e| format!("abrew: {e}"))?;
            let held = aterm_dirfd::fstat(&child).map_err(|e| format!("abrew: {e}"))?;
            if !same(&before, &held) {
                return Err("abrew: namespace source changed".to_string().into());
            }
            project_with(
                &source.join(os_name),
                &child,
                &output,
                name,
                &destination.join(os_name),
                prepared,
            )?;
        } else if kind.is_file() || kind.is_symlink() {
            symlink_at(&source.join(os_name), &output, name)?;
        } else {
            return Err("abrew: unsupported namespace entry".to_string().into());
        }
    }
    Ok(output)
}

enum Entry {
    Directory(Stat),
    Link(PathBuf),
}

fn snapshot(fd: &OwnedFd) -> Result<BTreeMap<PathBuf, Entry>, InstallError> {
    fn walk(
        fd: &OwnedFd,
        relative: &Path,
        out: &mut BTreeMap<PathBuf, Entry>,
    ) -> Result<(), InstallError> {
        let held = aterm_dirfd::fstat(fd).map_err(|e| format!("abrew: {e}"))?;
        out.insert(relative.to_path_buf(), Entry::Directory(held));
        for entry in Dir::read_from(fd).map_err(|e| format!("abrew: {e}"))? {
            let entry = entry.map_err(|e| format!("abrew: {e}"))?;
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let path = relative.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            let before = stat(fd, name)?;
            let kind = FileType::from_raw_mode(before.st_mode);
            if kind.is_dir() {
                let child = openat(
                    fd,
                    std::ffi::OsStr::from_bytes(name.to_bytes()),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(|e| format!("abrew: {e}"))?;
                if !same(
                    &before,
                    &aterm_dirfd::fstat(&child).map_err(|e| format!("abrew: {e}"))?,
                ) {
                    return Err("abrew: namespace changed during snapshot"
                        .to_string()
                        .into());
                }
                walk(&child, &path, out)?;
            } else if kind.is_symlink() {
                out.insert(path, Entry::Link(read_link(fd, name)?));
            } else {
                return Err("abrew: foreign namespace entry".to_string().into());
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(fd, Path::new(""), &mut out)?;
    Ok(out)
}

/// Deletes only the private projection's named aliases and empty containers,
/// relative to held directories; no prefix or Cellar path is recursively removed.
fn clear_projection(
    fd: &OwnedFd,
    relative: &Path,
    expected: &BTreeMap<PathBuf, Entry>,
) -> Result<(), String> {
    for entry in Dir::read_from(fd).map_err(|e| format!("abrew: {e}"))? {
        let entry = entry.map_err(|e| format!("abrew: {e}"))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let os_name = std::ffi::OsStr::from_bytes(name.to_bytes());
        let path = relative.join(os_name);
        let now = stat(fd, name).map_err(|e| e.message())?;
        match expected.get(&path) {
            Some(Entry::Link(target))
                if FileType::from_raw_mode(now.st_mode).is_symlink()
                    && read_link(fd, name).is_ok_and(|actual| &actual == target) =>
            {
                aterm_dirfd::unlinkat(fd, os_name, AtFlags::empty())
                    .map_err(|e| format!("abrew: {e}"))?;
            }
            Some(Entry::Directory(identity)) if same(&now, identity) => {
                let child = openat(fd, os_name, directory_flags(), Mode::empty())
                    .map_err(|e| format!("abrew: {e}"))?;
                if !aterm_dirfd::fstat(&child).is_ok_and(|held| same(&held, identity)) {
                    return Err("abrew: private projection changed".into());
                }
                clear_projection(&child, &path, expected)?;
                if !aterm_dirfd::fstatat(fd, os_name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|actual| same(&actual, identity))
                {
                    return Err("abrew: private projection changed".into());
                }
                aterm_dirfd::unlinkat(fd, os_name, AtFlags::REMOVEDIR)
                    .map_err(|e| format!("abrew: {e}"))?;
            }
            _ => return Err("abrew: foreign entry retained in private staging".into()),
        }
    }
    Ok(())
}

struct Change {
    destination: PathBuf,
    parent: OwnedFd,
    name: OsString,
    reserved: PathBuf,
    stash: OwnedFd,
    holder: OwnedFd,
    before: Stat,
    target: PathBuf,
    projected: BTreeMap<PathBuf, Entry>,
    projection: OwnedFd,
    swapped: bool,
    saved_identity: Option<Stat>,
    saved_target: Option<PathBuf>,
    published_identity: Option<Stat>,
}

impl Change {
    fn unchanged_projection(&self) -> bool {
        let Ok(now) = snapshot(&self.projection) else {
            return false;
        };
        now.len() == self.projected.len()
            && self
                .projected
                .iter()
                .all(|(path, entry)| match (entry, now.get(path)) {
                    (Entry::Link(a), Some(Entry::Link(b))) => a == b,
                    (Entry::Directory(a), Some(Entry::Directory(b))) => same(a, b),
                    _ => false,
                })
    }

    fn old_alias_unchanged(&self) -> bool {
        aterm_dirfd::fstatat(&self.stash, "payload", AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|now| same(&now, &self.before))
            && read_link(&self.stash, c"payload").is_ok_and(|t| t == self.target)
    }

    fn exchange(&self) -> Result<(), String> {
        aterm_dirfd::renameat_with(
            &self.stash,
            "payload",
            &self.parent,
            self.name.as_os_str(),
            RenameFlags::EXCHANGE,
        )
        .map_err(|e| {
            format!(
                "abrew: cannot exchange namespace {}: {e}",
                self.destination.display()
            )
        })
    }

    fn projection_at(&self, fd: &OwnedFd, name: &std::ffi::OsStr) -> bool {
        aterm_dirfd::fstat(&self.projection).is_ok_and(|held| {
            aterm_dirfd::fstatat(fd, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|now| same(&held, &now))
        })
    }

    fn capture_exchange(&mut self) -> Result<(), InstallError> {
        let saved = aterm_dirfd::fstatat(&self.stash, "payload", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| format!("abrew: {e}"))?;
        self.saved_target = if FileType::from_raw_mode(saved.st_mode).is_symlink() {
            Some(read_link(&self.stash, c"payload")?)
        } else {
            None
        };
        self.saved_identity = Some(saved);
        self.published_identity = Some(
            aterm_dirfd::fstatat(
                &self.parent,
                self.name.as_os_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|e| format!("abrew: {e}"))?,
        );
        Ok(())
    }

    fn restore(&mut self) -> Result<(), String> {
        if self.swapped {
            let saved_matches = self.saved_identity.as_ref().is_some_and(|expected| {
                aterm_dirfd::fstatat(&self.stash, "payload", AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|now| same(expected, &now))
            }) && self.saved_target.as_ref().is_none_or(|target| {
                read_link(&self.stash, c"payload").is_ok_and(|now| &now == target)
            });
            let published_matches = self.published_identity.as_ref().is_some_and(|expected| {
                aterm_dirfd::fstatat(
                    &self.parent,
                    self.name.as_os_str(),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .is_ok_and(|now| same(expected, &now))
            });
            // An unexpected object exchanged during admission is returned by
            // identity alone, without traversing or deleting its contents.
            // Once the actual projection was published, its aliases must also
            // match the baseline before it can be moved away from the prefix.
            let projection_safe = !self.projection_at(&self.parent, self.name.as_os_str())
                || self.unchanged_projection();
            if !saved_matches || !published_matches || !projection_safe {
                return Err(format!(
                    "abrew: namespace changed; original alias retained at {}",
                    self.reserved.join("payload").display()
                ));
            }
            self.exchange()?;
            self.swapped = false;
        }
        // The inverse exchange must leave this exact private projection here.
        // A substituted foreign staging entry remains intact for inspection.
        if self.projection_at(&self.stash, std::ffi::OsStr::new("payload"))
            && self.unchanged_projection()
        {
            clear_projection(&self.projection, Path::new(""), &self.projected)?;
            aterm_dirfd::unlinkat(&self.stash, "payload", AtFlags::REMOVEDIR)
                .map_err(|e| format!("abrew: {e}"))?;
            self.remove_empty_reservation();
        }
        Ok(())
    }

    fn remove_empty_reservation(&self) {
        if let Some(name) = self.reserved.file_name()
            && aterm_dirfd::fstat(&self.stash).is_ok_and(|held| {
                aterm_dirfd::fstatat(&self.holder, name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|now| same(&held, &now))
            })
        {
            let _ = aterm_dirfd::unlinkat(&self.holder, name, AtFlags::REMOVEDIR);
        }
    }

    fn commit(&self) {
        if self.old_alias_unchanged() {
            let _ = aterm_dirfd::unlinkat(&self.stash, "payload", AtFlags::empty());
            self.remove_empty_reservation();
        }
    }
}

struct Transaction {
    changes: Vec<Change>,
    finished: bool,
}

impl Transaction {
    fn rollback(&mut self) -> Result<(), String> {
        self.finished = true;
        let failures: Vec<_> = self
            .changes
            .iter_mut()
            .rev()
            .filter_map(|c| c.restore().err())
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.rollback()
        {
            eprintln!("{error}");
        }
    }
}

fn discover(
    prefix: &Prefix,
    source: &OwnedFd,
    destination: &OwnedFd,
    path: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), InstallError> {
    for entry in Dir::read_from(source).map_err(|e| format!("abrew: {e}"))? {
        let entry = entry.map_err(|e| format!("abrew: {e}"))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") || name.to_bytes().ends_with(b".app") {
            continue;
        }
        if !FileType::from_raw_mode(stat(source, name)?.st_mode).is_dir() {
            continue;
        }
        let dest = path.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
        match aterm_dirfd::fstatat(
            destination,
            std::ffi::OsStr::from_bytes(name.to_bytes()),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(meta) if FileType::from_raw_mode(meta.st_mode).is_symlink() => {
                owned_directory(prefix, &dest)?;
                paths.push(dest);
            }
            Ok(meta) if FileType::from_raw_mode(meta.st_mode).is_dir() => {
                let left = openat(
                    source,
                    std::ffi::OsStr::from_bytes(name.to_bytes()),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(|e| format!("abrew: {e}"))?;
                let right = openat(
                    destination,
                    std::ffi::OsStr::from_bytes(name.to_bytes()),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(|e| format!("abrew: {e}"))?;
                discover(prefix, &left, &right, &dest, paths)?;
            }
            Ok(_) => {
                return Err(format!("abrew: refusing foreign namespace {}", dest.display()).into());
            }
            Err(aterm_dirfd::Errno::NOENT) => {}
            Err(e) => return Err(format!("abrew: {e}").into()),
        }
    }
    Ok(())
}

/// Shares only colliding, admitted formula aliases. Failed activation restores
/// the original aliases; an intervening foreign change is retained and reported.
pub(super) fn with_shared_namespaces<T>(
    prefix: &Prefix,
    payload: Option<&Path>,
    action: impl FnOnce() -> Result<T, InstallError>,
) -> Result<T, InstallError> {
    with_shared_namespaces_with(prefix, payload, &mut |_| Ok(()), action)
}

fn with_shared_namespaces_with<T>(
    prefix: &Prefix,
    payload: Option<&Path>,
    before_exchange: &mut impl FnMut(&Path) -> Result<(), InstallError>,
    action: impl FnOnce() -> Result<T, InstallError>,
) -> Result<T, InstallError> {
    let mut paths = Vec::new();
    if let Some(payload) = payload {
        for sub in ["lib", "include", "share", "etc", "Frameworks"] {
            let source = payload.join(sub);
            let dest = prefix.root.join(sub);
            if std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir()) {
                check_directory(&dest, &prefix.root)?;
                if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
                    discover(
                        prefix,
                        &open_directory(&prefix.root, &source)?,
                        &open_directory(&prefix.root, &dest)?,
                        &dest,
                        &mut paths,
                    )?;
                }
            }
        }
    }
    let mut transaction = Transaction {
        changes: Vec::new(),
        finished: false,
    };
    let result = (|| {
        for destination in paths {
            let source = owned_directory(prefix, &destination)?;
            let parent = open_directory(
                &prefix.root,
                destination
                    .parent()
                    .ok_or_else(|| "abrew: namespace has no parent".to_string())?,
            )?;
            let name = destination
                .file_name()
                .ok_or_else(|| "abrew: namespace has no name".to_string())?
                .to_os_string();
            let before = aterm_dirfd::fstatat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| format!("abrew: {e}"))?;
            let c_name =
                std::ffi::CString::new(name.as_bytes()).map_err(|e| format!("abrew: {e}"))?;
            let target = read_link(&parent, &c_name)?;
            // Failed or substituted staging is retained, never recursively
            // destroyed by a temporary-directory destructor.
            let holder = open_directory(&prefix.root, &prefix.root)?;
            let reserved = aterm_tempfile::TempDir::new_in(&prefix.root)
                .map_err(|e| format!("abrew: {e}"))?
                .keep();
            let staged = reserved.join("payload");
            let prepared = (|| {
                let stash = open_directory(&reserved, &reserved)?;
                let projection = project(
                    &source,
                    &open_directory(&prefix.root, &source)?,
                    &stash,
                    c"payload",
                    &staged,
                )?;
                let projected = snapshot(&projection)?;
                Ok::<_, InstallError>((stash, projection, projected))
            })();
            let (stash, projection, projected) = prepared.map_err(|error| {
                InstallError::Failed(format!(
                    "{}; namespace staging retained at {}",
                    error.message(),
                    reserved.display()
                ))
            })?;
            if owned_directory(prefix, &destination)? != source
                || !aterm_dirfd::fstatat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|now| same(&now, &before))
                || read_link(&parent, &c_name)? != target
            {
                return Err("abrew: namespace alias changed".to_string().into());
            }
            transaction.changes.push(Change {
                destination,
                parent,
                name,
                reserved,
                stash,
                holder,
                before,
                target,
                projected,
                projection,
                swapped: false,
                saved_identity: None,
                saved_target: None,
                published_identity: None,
            });
            let change = transaction
                .changes
                .last_mut()
                .ok_or_else(|| "abrew: missing namespace change".to_string())?;
            if !change.projection_at(&change.stash, std::ffi::OsStr::new("payload"))
                || !change.unchanged_projection()
            {
                return Err(format!(
                    "abrew: projection changed; staging retained at {}",
                    change.reserved.display()
                )
                .into());
            }
            before_exchange(&change.reserved.join("payload"))?;
            change.exchange()?;
            change.swapped = true;
            change.capture_exchange()?;
            if !change.old_alias_unchanged()
                || !change.projection_at(&change.parent, change.name.as_os_str())
            {
                return Err(format!(
                    "abrew: namespace changed during exchange; staging retained at {}",
                    change.reserved.display()
                )
                .into());
            }
        }
        action()
    })();
    match result {
        Ok(value) => {
            transaction.finished = true;
            for change in &transaction.changes {
                change.commit();
            }
            Ok(value)
        }
        Err(error) => match transaction.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(InstallError::Failed(format!(
                "{}; {rollback}",
                error.message()
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rejects_replaced_projection_destination() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let outside = tmp.path().join("outside");
        let staging = tmp.path().join("staging");
        for path in [&source, &outside, &staging] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(source.join("leaf"), b"source bytes").unwrap();
        std::fs::write(outside.join("keep"), b"user bytes").unwrap();
        let fd = open_directory(tmp.path(), &source).unwrap();
        let mut injected = false;
        let parent = open_directory(&staging, &staging).unwrap();
        let result = project_with(
            &source,
            &fd,
            &parent,
            c"payload",
            &staging.join("payload"),
            &mut |destination| {
                if !injected {
                    std::fs::rename(destination, staging.join("held-projection")).unwrap();
                    symlink_dir(&outside, destination).unwrap();
                    injected = true;
                }
                Ok(())
            },
        );
        assert!(injected, "the actual post-mkdir boundary must execute");
        assert!(
            !outside.join("leaf").exists(),
            "projection wrote a real alias outside its staging directory"
        );
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
        assert!(result.is_err());
    }
    #[test]
    fn activation_restores_old_alias_when_projection_is_substituted_before_exchange() {
        use std::os::unix::fs::MetadataExt as _;
        let tmp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(&tmp.path().join("prefix"));
        prefix.ensure().unwrap();
        let make = |name: &str| {
            let keg = prefix.keg(name, "1.0");
            std::fs::create_dir_all(keg.join("share/vim")).unwrap();
            std::fs::write(keg.join("share/vim").join(name), name).unwrap();
            prefix
                .write_receipt(
                    &keg,
                    &[
                        ("name", name),
                        ("version", "1.0"),
                        ("installed_by", "abrew"),
                    ],
                )
                .unwrap();
            keg
        };
        let left = make("left");
        let right = make("right");
        link_keg(&prefix, &left, false).unwrap();
        let namespace = prefix.root.join("share/vim");
        let old_identity = std::fs::symlink_metadata(&namespace).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"user bytes").unwrap();
        let mut injected = false;
        let result = with_shared_namespaces_with(
            &prefix,
            Some(&right),
            &mut |stage| {
                std::fs::rename(stage, stage.parent().unwrap().join("held-projection")).unwrap();
                symlink_dir(&outside, stage).unwrap();
                injected = true;
                Ok(())
            },
            || -> Result<(), InstallError> {
                panic!("substituted projection must never reach activation")
            },
        );
        assert!(injected);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_link(&namespace).unwrap(),
            left.join("share/vim")
        );
        assert_eq!(
            std::fs::symlink_metadata(&namespace).unwrap().ino(),
            old_identity.ino()
        );
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }
}
