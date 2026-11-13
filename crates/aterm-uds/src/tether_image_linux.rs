// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain the kernel-matched ELF description, never a parent PID's reusable proc path.

use super::{cache_root, copy_image, prepare};
use crate::private_file::{PrivateDirectory, create_private_dir};
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) struct Source {
    identity: (u64, u64),
    size: u64,
    created: (i64, i64),
}

impl Source {
    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > 2 << 30 {
            return Err(io::Error::other("loaded image is outside retention bounds"));
        }
        Ok(Self {
            identity: (meta.dev(), meta.ino()),
            size: meta.len(),
            created: (meta.ctime(), meta.ctime_nsec()),
        })
    }

    pub(crate) fn bundled(&self) -> bool {
        false
    }

    pub(crate) fn retain(&self, file: &File, name: &std::ffi::OsStr) -> io::Result<Retained> {
        if Path::new(name).file_name() != Some(name) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let meta = file.metadata()?;
        if (meta.dev(), meta.ino()) != self.identity || meta.len() != self.size {
            return Err(io::Error::other("loaded image description changed"));
        }
        let root = cache_root()?;
        let boot = crate::process::boot_id()
            .ok_or_else(|| io::Error::other("boot identity unavailable"))?;
        let current = prepare(&root, &boot)?;
        // This key bounds reuse; equality against the held loaded image proves it.
        // Even inode reuse within this boot cannot admit another image's bytes.
        let key = format!(
            "elf-{:x}-{:x}-{:x}-{:x}-{:x}",
            self.identity.0, self.identity.1, self.size, self.created.0, self.created.1
        );
        let directory = current.join(format!("{key}.noindex"));
        create_private_dir(&directory)?;
        let owned = PrivateDirectory::open(&directory)?;
        let parent = PrivateDirectory::open(&current)?;
        let _lock = parent.lock(
            &format!("{key}.lock"),
            Duration::from_secs(60),
            "ELF image retention in progress",
        )?;
        let path = directory.join(name);
        if path.try_exists()? {
            return Retained::read(path, file, self.size);
        }
        let stage = aterm_tempfile::Builder::new()
            .prefix("image-")
            .suffix(".noindex")
            .tempdir_in(&directory)?;
        let staged = stage.path().join(name);
        copy_image(file, &staged)?;
        equal_image(file, &staged, self.size)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o500))?;
        if !owned.is_at(&directory)? {
            return Err(io::Error::other("ELF image directory changed"));
        }
        std::fs::rename(staged, &path)?;
        Retained::read(path, file, self.size)
    }
}

fn equal_image(source: &File, path: &Path, size: u64) -> io::Result<()> {
    let retained = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = retained.metadata()?;
    if !meta.is_file() || meta.len() != size {
        return Err(io::Error::other(
            "retained ELF size differs from loaded code",
        ));
    }
    let mut original = [0_u8; 64 * 1024];
    let mut copy = [0_u8; 64 * 1024];
    let mut at = 0;
    while at < size {
        let wanted = usize::try_from((size - at).min(original.len() as u64)).unwrap();
        source.read_exact_at(&mut original[..wanted], at)?;
        retained.read_exact_at(&mut copy[..wanted], at)?;
        if original[..wanted] != copy[..wanted] {
            return Err(io::Error::other("retained ELF differs from loaded code"));
        }
        at += wanted as u64;
    }
    Ok(())
}

pub(crate) struct Retained {
    path: PathBuf,
    identity: (u64, u64),
    source: File,
    size: u64,
}

impl Retained {
    fn read(path: PathBuf, source: &File, size: u64) -> io::Result<Self> {
        equal_image(source, &path, size)?;
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.mode() & 0o222 != 0 {
            return Err(io::Error::other(
                "retained ELF is not an immutable regular file",
            ));
        }
        Ok(Self {
            path,
            identity: (meta.dev(), meta.ino()),
            source: source.try_clone()?,
            size,
        })
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        let meta = std::fs::symlink_metadata(&self.path).ok()?;
        equal_image(&self.source, &self.path, self.size).ok()?;
        (meta.is_file() && (meta.dev(), meta.ino()) == self.identity && meta.mode() & 0o222 == 0)
            .then_some(&self.path)
    }
}

#[cfg(test)]
#[path = "tether_image_linux_tests.rs"]
mod tests;
