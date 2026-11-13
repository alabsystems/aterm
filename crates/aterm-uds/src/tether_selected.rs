// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain the selected foreign file's code without claiming it is our loaded image.

use super::image;
use crate::lan::install::CodeDirectories;
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

type Stamp = (u64, u64, u64, i64, i64, i64, i64);
fn stamp(meta: &Metadata) -> Stamp {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

/// The actual selected file and a byte-identical retained native image.
/// Its copy uses the existing boot-lifetime image owner: an unproved first
/// execution never causes its file or bundle to be removed. This is code
/// custody only, with no resource, reader-protocol or execution-success grant.
pub struct SelectedExecutable {
    selected: PathBuf,
    original: PathBuf,
    file: File,
    stamp: Stamp,
    directories: CodeDirectories,
    retained: image::Retained,
}

impl SelectedExecutable {
    /// Hold exactly the selected native file and retain its required bundle.
    /// # Errors
    /// Selection is relative, unsafe, changes while read, is not a supported
    /// native executable, or its retained code cannot be provided unchanged.
    pub fn retain(selected: &Path, deadline: Instant) -> io::Result<Self> {
        before(deadline)?;
        if !selected.is_absolute() {
            return Err(io::Error::other("selected executable must be absolute"));
        }
        let original = selected.canonicalize()?;
        let (file, meta) = crate::regular_file::open(&original)?;
        if meta.mode() & 0o6022 != 0
            || meta.mode() & 0o111 == 0
            || (meta.uid() != crate::peer::our_uid() && meta.uid() != 0)
            || meta.len() == 0
            || meta.len() > 2 << 30
        {
            return Err(io::Error::other("unsafe selected native executable"));
        }
        let directories = CodeDirectories::of_held(&file)?;
        let source = image::Source::selected(&file)?;
        let name = original
            .file_name()
            .ok_or_else(|| io::Error::other("selected image has no filename"))?;
        let retained = source.retain(&file, name)?;
        let this = Self {
            selected: selected.into(),
            original,
            file,
            stamp: stamp(&meta),
            directories,
            retained,
        };
        this.path(deadline)?;
        Ok(this)
    }

    /// The retained path, after original selection and complete-byte bookends.
    /// # Errors
    /// The original file, selection, CodeDirectory or retained bytes changed.
    pub fn path(&self, deadline: Instant) -> io::Result<&Path> {
        before(deadline)?;
        self.check_original()?;
        let path = self
            .retained
            .path()
            .ok_or_else(|| io::Error::other("retained selected image changed"))?;
        let (copy, meta) = crate::regular_file::open(path)?;
        if meta.len() != self.stamp.2 || CodeDirectories::of_held(&copy)? != self.directories {
            return Err(io::Error::other(
                "retained selected image differs from original",
            ));
        }
        let mut a = [0_u8; 64 * 1024];
        let mut b = [0_u8; 64 * 1024];
        let mut offset = 0;
        while offset < meta.len() {
            before(deadline)?;
            let count = usize::try_from((meta.len() - offset).min(a.len() as u64)).unwrap();
            self.file.read_exact_at(&mut a[..count], offset)?;
            copy.read_exact_at(&mut b[..count], offset)?;
            if a[..count] != b[..count] {
                return Err(io::Error::other(
                    "retained selected executable bytes changed",
                ));
            }
            offset += count as u64;
        }
        if stamp(&copy.metadata()?) != stamp(&meta)
            || stamp(&std::fs::symlink_metadata(path)?) != stamp(&meta)
        {
            return Err(io::Error::other(
                "retained selected executable changed during read",
            ));
        }
        self.check_original()?;
        before(deadline)?;
        Ok(path)
    }

    fn check_original(&self) -> io::Result<()> {
        if self.selected.canonicalize()? != self.original
            || stamp(&self.file.metadata()?) != self.stamp
            || stamp(&std::fs::symlink_metadata(&self.original)?) != self.stamp
            || CodeDirectories::of_held(&self.file)? != self.directories
        {
            return Err(io::Error::other(
                "selected executable differs from its original held file",
            ));
        }
        Ok(())
    }
}

fn before(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tether_selected_tests.rs"]
mod tests;
