// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compiler path and its original open file, retained together while a runtime stage uses it.

use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            length: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Artifact {
    pub(crate) selected: PathBuf,
    pub(crate) canonical: PathBuf,
    held: File,
    stamp: Stamp,
    digest: [u8; 32],
}

impl Artifact {
    /// Make a separate, sealed image from the already admitted open file. Neither copy nor
    /// validation resolves the compiler's original pathname again.
    #[cfg(target_os = "macos")]
    pub(crate) fn copy_held(&self, destination: &Path) -> Result<Self, String> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        const LIMIT: u64 = 1 << 30;
        if self.len() > LIMIT
            || Stamp::of(&self.held.metadata().map_err(|e| e.to_string())?) != self.stamp
        {
            return Err("admitted compiler image changed or exceeds 1 GiB".into());
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o500)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(destination)
            .map_err(|e| e.to_string())?;
        let mut buffer = [0; 65536];
        let mut hash = aterm_digest::Sha256::new();
        let mut offset = 0;
        while offset < self.len() {
            let wanted = usize::try_from((self.len() - offset).min(buffer.len() as u64)).unwrap();
            let read = match self.held.read_at(&mut buffer[..wanted], offset) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => other.map_err(|e| e.to_string())?,
            };
            if read == 0 {
                return Err("admitted compiler image became short".into());
            }
            output
                .write_all(&buffer[..read])
                .map_err(|e| e.to_string())?;
            hash.update(&buffer[..read]);
            offset += read as u64;
        }
        if hash.finalize() != self.digest
            || Stamp::of(&self.held.metadata().map_err(|e| e.to_string())?) != self.stamp
        {
            return Err("admitted compiler image changed during its private copy".into());
        }
        output
            .set_permissions(std::fs::Permissions::from_mode(0o500))
            .map_err(|e| e.to_string())?;
        aterm_durable::Flush::Durable
            .file(&output)
            .map_err(|e| e.to_string())?;
        drop(output);
        let copied = Self::open_bounded(destination, LIMIT)?;
        copied.sealed_private()?;
        if copied.digest != self.digest || copied.len() != self.len() {
            return Err("private compiler image differs from its admitted file".into());
        }
        Ok(copied)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn file(&self) -> &File {
        &self.held
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn sealed_private(&self) -> Result<(), String> {
        let meta = self.held.metadata().map_err(|e| e.to_string())?;
        if meta.mode() & 0o7777 != 0o500
            || meta.uid() != aterm_uds::peer::our_uid()
            || meta.nlink() != 1
        {
            return Err("private compiler image is not sealed, single-link and owner-only".into());
        }
        Ok(())
    }

    pub(crate) fn sha256(&self) -> String {
        aterm_codec::hex::encode_lower(&self.digest)
    }

    pub(crate) fn len(&self) -> u64 {
        self.stamp.length
    }

    pub(crate) fn same_file(&self, other: &Self) -> bool {
        (self.stamp.device, self.stamp.inode) == (other.stamp.device, other.stamp.inode)
    }

    pub(crate) fn open(selected: &Path) -> Result<Self, String> {
        let held = Self::retain(selected)?;
        held.validate()?;
        Ok(held)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn open_bounded(selected: &Path, limit: u64) -> Result<Self, String> {
        let held = Self::retain_up_to(selected, limit)?;
        held.validate()?;
        Ok(held)
    }

    /// Read the digest from the original handle and recheck both names and metadata after the read.
    pub(crate) fn retain(selected: &Path) -> Result<Self, String> {
        Self::retain_up_to(selected, u64::MAX)
    }

    fn retain_up_to(selected: &Path, limit: u64) -> Result<Self, String> {
        if !selected.is_absolute()
            || !std::fs::symlink_metadata(selected)
                .map_err(|e| e.to_string())?
                .is_file()
        {
            return Err("compiler artifact must name an absolute regular file, not a link".into());
        }
        let canonical = selected.canonicalize().map_err(|e| e.to_string())?;
        let (held, meta) = aterm_uds::regular_file::open(&canonical).map_err(|e| e.to_string())?;
        if meta.mode() & 0o6022 != 0
            || meta.mode() & 0o111 == 0
            || (meta.uid() != aterm_uds::peer::our_uid() && meta.uid() != 0)
            || meta.len() == 0
            || meta.len() > limit
        {
            return Err("compiler artifact has unsafe ownership, mode or empty content".into());
        }
        let stamp = Stamp::of(&meta);
        let digest = contents(&held, stamp.length).map_err(|e| e.to_string())?;
        let artifact = Self {
            selected: selected.to_owned(),
            canonical,
            held,
            stamp,
            digest,
        };
        artifact.validate_identity()?;
        Ok(artifact)
    }

    pub(crate) fn validate_identity(&self) -> Result<(), String> {
        let changed = || format!("prepared artifact changed: {}", self.selected.display());
        let named = std::fs::symlink_metadata(&self.selected).map_err(|_| changed())?;
        if !named.is_file()
            || Stamp::of(&named) != self.stamp
            || self.selected.canonicalize().map_err(|_| changed())? != self.canonical
            || Stamp::of(&self.held.metadata().map_err(|_| changed())?) != self.stamp
        {
            return Err(changed());
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let changed = || format!("prepared artifact changed: {}", self.selected.display());
        let check = || -> Result<(), String> {
            let named = std::fs::symlink_metadata(&self.selected).map_err(|e| e.to_string())?;
            if !named.is_file()
                || Stamp::of(&named) != self.stamp
                || self.selected.canonicalize().map_err(|e| e.to_string())? != self.canonical
            {
                return Err(changed());
            }
            let (now, meta) =
                aterm_uds::regular_file::open(&self.canonical).map_err(|e| e.to_string())?;
            if Stamp::of(&meta) != self.stamp
                || Stamp::of(&self.held.metadata().map_err(|e| e.to_string())?) != self.stamp
                || contents(&self.held, self.stamp.length).map_err(|e| e.to_string())?
                    != self.digest
                || Stamp::of(&now.metadata().map_err(|e| e.to_string())?) != self.stamp
                || Stamp::of(&self.held.metadata().map_err(|e| e.to_string())?) != self.stamp
                || Stamp::of(
                    &std::fs::symlink_metadata(&self.canonical).map_err(|e| e.to_string())?,
                ) != self.stamp
                || Stamp::of(&std::fs::symlink_metadata(&self.selected).map_err(|e| e.to_string())?)
                    != self.stamp
                || self.selected.canonicalize().map_err(|e| e.to_string())? != self.canonical
            {
                return Err(changed());
            }
            Ok(())
        };
        check().map_err(|why| format!("{} ({why})", changed()))
    }
}

pub(crate) fn contents(file: &File, length: u64) -> io::Result<[u8; 32]> {
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut offset = 0;
    while offset < length {
        let wanted = usize::try_from((length - offset).min(buffer.len() as u64)).unwrap();
        let read = match file.read_at(&mut buffer[..wanted], offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        hash.update(&buffer[..read]);
        offset += read as u64;
    }
    Ok(hash.finalize())
}
