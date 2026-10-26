// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted file reads (`O_NOFOLLOW | O_NONBLOCK`) and atomic 0600 writes.

use crate::{Error, SecretBytes};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Ensures `dir` exists as a 0700 directory owned by this user, refusing symlinks.
pub fn ensure_private_dir(dir: &Path) -> crate::Result<()> {
    match kind(dir)? {
        Kind::Dir => {}
        Kind::Missing => {
            if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::new(format!("{}: {e}", parent.display())))?;
            }
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::new(format!("{}: {e}", dir.display()))),
            }
            if kind(dir)? != Kind::Dir {
                return Err(Error::new(format!("{}: not a directory", dir.display())));
            }
        }
        Kind::Symlink => {
            return Err(Error::new(format!(
                "{}: is a symlink; refusing to keep secrets through it",
                dir.display()
            )));
        }
        Kind::File | Kind::Other => {
            return Err(Error::new(format!("{}: not a directory", dir.display())));
        }
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    if meta.uid() != euid() {
        return Err(Error::new(format!(
            "{}: owned by uid {}, not this user",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o7777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Who may own and what mode a read admits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admit {
    /// Owned by this user, no group/other bits.
    Private,
    /// Owned by this user, no group/other write.
    Owned,
    /// Anyone's.
    Any,
}

/// Open and read a regular file under `admit`, at most `max` bytes.
fn read_admitted(path: &Path, max: usize, admit: Admit) -> crate::Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(Error::new(format!(
                "{}: is a symlink; refused",
                path.display()
            )));
        }
        Err(e) => return Err(Error::new(format!("{}: {e}", path.display()))),
    };
    let meta = file
        .metadata()
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(Error::new(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    match admit {
        Admit::Any => {}
        Admit::Private | Admit::Owned => {
            if meta.uid() != euid() {
                return Err(Error::new(format!(
                    "{}: owned by uid {}, not this user",
                    path.display(),
                    meta.uid()
                )));
            }
            let forbidden = if admit == Admit::Private {
                0o077
            } else {
                0o022
            };
            if meta.mode() & forbidden != 0 || meta.mode() & 0o6000 != 0 {
                return Err(Error::new(format!(
                    "{}: mode {:o} lets other users {}; run chmod 600 on it",
                    path.display(),
                    meta.mode() & 0o7777,
                    if admit == Admit::Private {
                        "read it"
                    } else {
                        "write it"
                    }
                )));
            }
        }
    }
    let mut bytes = read_buffer(meta.len(), max);
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    crate::secret::read_to_end_wiping_on_error(file.take(limit), &mut bytes)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    if bytes.len() > max {
        zeroize::Zeroize::zeroize(&mut bytes);
        return Err(Error::new(format!(
            "{}: larger than {max} bytes",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// A buffer sized once for reading a file of `len` bytes (its `fstat` size) under a cap of
/// `max`. `read_to_end` through a `Take` has no size hint, so into an empty buffer it grows
/// from 32 bytes, freeing each block it outgrew, with what it had read, unwiped. One byte past
/// the file lets it see the end without growing; only a file that grows between the `fstat`
/// and the read still grows it.
fn read_buffer(len: u64, max: usize) -> Vec<u8> {
    let len = usize::try_from(len).unwrap_or(usize::MAX).min(max);
    Vec::with_capacity(len.saturating_add(1))
}

/// Reads a regular file owned by this user with no group/other bits, up to `max` bytes.
pub fn read_private(path: &Path, max: usize) -> crate::Result<Option<SecretBytes>> {
    Ok(read_admitted(path, max, Admit::Private)?.map(SecretBytes::new))
}

/// Reads a regular file owned by this user with no group/other write bits, up to `max` bytes.
pub fn read_owned(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_admitted(path, max, Admit::Owned)
}

/// Reads any regular file up to `max` bytes, refusing symlinks and non-regular files.
pub fn read_public(path: &Path, max: usize) -> crate::Result<Option<Vec<u8>>> {
    read_admitted(path, max, Admit::Any)
}

/// Writes `bytes` atomically to `path` with permissions `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    aterm_uds::atomic_file::write(path, bytes, mode)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Writes `bytes` to `path` at `mode` only if `path` does not exist.
///
/// The directory's flush after the link is best-effort and never reported, unlike
/// [`write_atomic`]'s: once the link has made `path`, an error would tell the caller nothing
/// was placed while its next try answers "taken", a claim no caller owns.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<bool> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && kind(parent)? != Kind::Dir
    {
        return Err(Error::new(format!(
            "{}: no such directory",
            parent.display()
        )));
    }
    let (tmp, mut file) = aterm_uds::atomic_file::create_temp_beside(path, mode)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .and_then(|()| sync(&file));
    drop(file);
    let placed = match written {
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
        Ok(()) => match link_then_unlink(&tmp, path) {
            Ok(()) => Ok(true),
            Err(Moved::NotLinked(e)) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(Moved::NotLinked(e)) => Err(Error::new(format!("{}: {e}", path.display()))),
            Err(Moved::NotUnlinked(e)) => Err(Error::new(format!("{}: {e}", tmp.display()))),
        },
    };
    if !matches!(placed, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    } else {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let _never_reported = aterm_durable::Flush::Durable.dir_best_effort(dir);
    }
    placed
}

/// What is at a path, without following a final symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Nothing.
    Missing,
    /// A regular file.
    File,
    /// A symlink.
    Symlink,
    /// A directory.
    Dir,
    /// Something else (FIFO, socket, device).
    Other,
}

/// What is at `path` (`lstat`).
pub fn kind(path: &Path) -> crate::Result<Kind> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let kind = meta.file_type();
            Ok(if kind.is_symlink() {
                Kind::Symlink
            } else if kind.is_dir() {
                Kind::Dir
            } else if kind.is_file() {
                Kind::File
            } else {
                Kind::Other
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Kind::Missing),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Returns `(device, inode)` of `path` via `lstat`, or `None` if missing.
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Renames `from` to `to` without replacing an existing `to`.
pub fn rename_noreplace(from: &Path, to: &Path) -> crate::Result<()> {
    link_then_unlink(from, to).map_err(|moved| match moved {
        Moved::NotLinked(e) => Error::new(format!("{} -> {}: {e}", from.display(), to.display())),
        Moved::NotUnlinked(e) => Error::new(format!("{}: {e}", from.display())),
    })
}

/// Why [`link_then_unlink`] did not move a file.
#[derive(Debug)]
enum Moved {
    /// The link failed (`AlreadyExists` when `to` was taken): nothing changed.
    NotLinked(io::Error),
    /// The removal of `from` failed; the link was undone.
    NotUnlinked(io::Error),
}

/// [`rename_noreplace`]'s steps, with the step that failed and its OS error.
fn link_then_unlink(from: &Path, to: &Path) -> Result<(), Moved> {
    std::fs::hard_link(from, to).map_err(Moved::NotLinked)?;
    let Err(e) = std::fs::remove_file(from) else {
        return Ok(());
    };
    let linked = file_id(to);
    if linked.is_some() && file_id(from) == linked {
        let _ = std::fs::remove_file(to);
    } else if e.kind() == io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(Moved::NotUnlinked(e))
}

/// Zeroes and removes a regular file at `path` (one this user cannot read stays), or unlinks a
/// symlink without following it.
pub fn wipe_and_remove(path: &Path) -> crate::Result<()> {
    if kind(path)? == Kind::File {
        return wipe(path, |_| Ok(true)).map(drop);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Zeroes and removes the regular file at `path` if its contents (up to `max` bytes) pass `ours`.
pub fn wipe_if(path: &Path, max: usize, ours: impl FnOnce(&[u8]) -> bool) -> crate::Result<bool> {
    wipe(path, |file| {
        let mut bytes = read_buffer(file.metadata().map_or(0, |meta| meta.len()), max);
        let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
        if crate::secret::read_to_end_wiping_on_error(file.take(limit), &mut bytes).is_err() {
            return Ok(false);
        }
        let bytes = SecretBytes::new(bytes);
        Ok(bytes.len() <= max && ours(bytes.as_slice()))
    })
}

/// [`wipe_if`]'s steps, with `judge` reading the file it opened.
fn wipe(path: &Path, judge: impl FnOnce(&mut File) -> io::Result<bool>) -> crate::Result<bool> {
    let at = |e: io::Error| Error::new(format!("{}: {e}", path.display()));
    let open = |write: bool| {
        OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
    };
    let opened = match open(true) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => open(false),
        opened => opened,
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) || e.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(at(e)),
    };
    let meta = file.metadata().map_err(at)?;
    if !meta.file_type().is_file()
        || !judge(&mut file).map_err(at)?
        || file_id(path) != Some((meta.dev(), meta.ino()))
    {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(at(e)),
    }
    let zeros = [0u8; 4096];
    let mut done = 0;
    while done < meta.len() {
        let n = usize::try_from((meta.len() - done).min(4096)).unwrap_or(4096);
        if file.write_all_at(&zeros[..n], done).is_err() {
            break;
        }
        done += n as u64;
    }
    let _ = sync(&file);
    Ok(true)
}

fn sync(file: &File) -> io::Result<()> {
    aterm_durable::durable_data(file)
}

/// This process's effective uid.
pub fn euid() -> u32 {
    aterm_uds::peer::our_uid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_private_dir_is_made_and_narrowed_to_0700() {
        let root = scratch();
        let dir = root.path().join("a").join("secrets");
        ensure_private_dir(&dir).expect("made");
        assert_eq!(
            std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
            0o700
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen");
        ensure_private_dir(&dir).expect("narrowed");
        assert_eq!(
            std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn a_symlinked_dir_is_refused() {
        let root = scratch();
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let error = ensure_private_dir(&link).expect_err("refused");
        assert!(error.0.contains("symlink"), "{error}");
    }

    #[test]
    fn private_reads_refuse_group_or_other_bits_and_symlinks() {
        let root = scratch();
        let path = root.path().join("token");
        write_atomic(&path, b"value", 0o600).expect("write");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").mode() & 0o777,
            0o600
        );
        assert_eq!(
            read_private(&path, 64)
                .expect("read")
                .expect("present")
                .as_slice(),
            b"value"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let error = read_private(&path, 64).expect_err("refused");
        assert!(!error.0.contains("value"), "{error}");
        assert!(
            read_owned(&path, 64)
                .expect("owned admits group read")
                .is_some()
        );
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).expect("link");
        assert!(read_public(&link, 64).is_err());
        assert!(
            read_private(&root.path().join("missing"), 64)
                .expect("ok")
                .is_none()
        );
    }

    /// A read fills one buffer sized from the file's length, a byte over so it sees the end
    /// without growing: a buffer that grew freed each block it outgrew, unwiped.
    #[test]
    fn a_read_fills_one_buffer_sized_from_the_files_length() {
        let root = scratch();
        let path = root.path().join("value");
        write_atomic(&path, &[7u8; 1000], 0o600).expect("write");
        let bytes = read_owned(&path, 4096).expect("read").expect("there");
        assert_eq!((bytes.len(), bytes.capacity()), (1000, 1001));
    }

    #[test]
    fn reads_are_bounded_and_a_fifo_never_blocks() {
        let root = scratch();
        let path = root.path().join("big");
        write_atomic(&path, &[7u8; 65], 0o600).expect("write");
        assert!(read_private(&path, 64).is_err());
        assert!(read_private(&path, 65).expect("ok").is_some());
        let fifo = root.path().join("fifo");
        let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        assert!(
            read_public(&fifo, 64).is_err(),
            "a FIFO is not a regular file"
        );
    }

    #[test]
    fn rename_noreplace_refuses_an_existing_target() {
        let root = scratch();
        let a = root.path().join("a");
        let b = root.path().join("b");
        write_atomic(&a, b"a", 0o600).expect("a");
        write_atomic(&b, b"b", 0o600).expect("b");
        assert!(rename_noreplace(&a, &b).is_err());
        assert_eq!(std::fs::read(&b).expect("b"), b"b");
        std::fs::remove_file(&b).expect("rm");
        rename_noreplace(&a, &b).expect("moved");
        assert_eq!(kind(&a).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&b).expect("b"), b"a");
    }

    #[test]
    fn a_rename_whose_unlink_fails_undoes_its_link() {
        let root = scratch();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).expect("dir");
        let from = locked.join("f");
        write_atomic(&from, b"kept", 0o600).expect("write");
        let to = root.path().join("to");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock");
        let result = rename_noreplace(&from, &to);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");
        if euid() == 0 {
            result.expect("root renames");
            assert_eq!(kind(&from).expect("kind"), Kind::Missing);
            return;
        }
        let error = result.expect_err("the unlink is refused");
        assert!(error.0.contains("locked/f"), "{error}");
        assert_eq!(
            kind(&to).expect("kind"),
            Kind::Missing,
            "the link is undone"
        );
        assert_eq!(std::fs::read(&from).expect("from"), b"kept");
        assert_eq!(
            std::fs::symlink_metadata(&from).expect("meta").nlink(),
            1,
            "one name again"
        );
    }

    #[test]
    fn file_ids_tell_one_file_from_two() {
        let root = scratch();
        let a = root.path().join("a");
        write_atomic(&a, b"x", 0o600).expect("a");
        let link = root.path().join("hard");
        std::fs::hard_link(&a, &link).expect("link");
        let other = root.path().join("other");
        write_atomic(&other, b"x", 0o600).expect("other");
        assert!(file_id(&a).is_some());
        assert_eq!(file_id(&a), file_id(&link));
        assert_ne!(file_id(&a), file_id(&other));
        assert_eq!(file_id(&root.path().join("missing")), None);
        let soft = root.path().join("soft");
        std::os::unix::fs::symlink(&a, &soft).expect("symlink");
        assert_ne!(file_id(&soft), file_id(&a));
    }

    #[test]
    fn wipe_removes_files_and_only_the_link_of_a_link() {
        let root = scratch();
        let file = root.path().join("f");
        write_atomic(&file, b"secret", 0o600).expect("write");
        let link = root.path().join("l");
        std::os::unix::fs::symlink(&file, &link).expect("link");
        wipe_and_remove(&link).expect("link removed");
        assert_eq!(std::fs::read(&file).expect("intact"), b"secret");
        wipe_and_remove(&file).expect("wiped");
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        wipe_and_remove(&file).expect("missing is fine");
    }

    #[test]
    fn wipe_zeroes_the_bytes_under_every_name() {
        let root = scratch();
        let file = root.path().join("f");
        write_atomic(&file, b"secret", 0o600).expect("write");
        let kept = root.path().join("kept");
        std::fs::hard_link(&file, &kept).expect("link");
        let mut open = File::open(&file).expect("open");
        wipe_and_remove(&file).expect("wiped");
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 6]);
        let mut left = Vec::new();
        open.read_to_end(&mut left).expect("read");
        assert_eq!(
            left, [0u8; 6],
            "a descriptor open before the wipe reads zeros"
        );
    }

    #[test]
    fn a_judged_wipe_never_touches_a_file_renamed_over_the_one_it_judged() {
        let root = scratch();
        let file = root.path().join("config");
        write_atomic(&file, b"the vault's", 0o600).expect("write");
        assert!(!wipe_if(&file, 64, |bytes| bytes == b"other").expect("judged"));
        assert!(
            !wipe_if(&file, 4, |_| true).expect("judged"),
            "past the bound"
        );
        assert_eq!(std::fs::read(&file).expect("kept"), b"the vault's");

        let tools = root.path().join("config.lock");
        write_atomic(&tools, b"the tool's", 0o600).expect("write");
        let removed = wipe_if(&file, 64, |bytes| {
            std::fs::rename(&tools, &file).expect("renamed over");
            bytes == b"the vault's"
        })
        .expect("judged");
        assert!(!removed);
        assert_eq!(
            std::fs::read(&file).expect("the tool's file"),
            b"the tool's"
        );

        let kept = root.path().join("kept");
        std::fs::hard_link(&file, &kept).expect("link");
        assert!(wipe_if(&file, 64, |bytes| bytes == b"the tool's").expect("wiped"));
        assert_eq!(kind(&file).expect("kind"), Kind::Missing);
        assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 10]);
    }

    /// The names in `dir`, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_new_fills_only_an_empty_name() {
        let root = scratch();
        let path = root.path().join("gap");
        assert!(write_new(&path, b"filled", 0o600).expect("written"));
        assert_eq!(std::fs::read(&path).expect("read"), b"filled");
        assert_eq!(
            std::fs::symlink_metadata(&path).expect("meta").mode() & 0o7777,
            0o600
        );
        assert_eq!(
            std::fs::symlink_metadata(&path).expect("meta").nlink(),
            1,
            "the temp's name is gone"
        );

        assert!(!write_new(&path, b"again", 0o600).expect("taken"));
        assert_eq!(std::fs::read(&path).expect("read"), b"filled");
        let dangling = root.path().join("dangling");
        let nowhere = root.path().join("nowhere");
        std::os::unix::fs::symlink(&nowhere, &dangling).expect("link");
        assert!(!write_new(&dangling, b"v", 0o600).expect("taken"));
        assert_eq!(kind(&dangling).expect("kind"), Kind::Symlink);
        assert_eq!(kind(&nowhere).expect("kind"), Kind::Missing);
        assert_eq!(names_in(root.path()), ["dangling", "gap"], "no temp left");

        let missing = root.path().join("no-dir").join("x");
        let error = write_new(&missing, b"v", 0o600).expect_err("no parent");
        assert!(error.0.contains("no-dir"), "{error}");
    }
}
