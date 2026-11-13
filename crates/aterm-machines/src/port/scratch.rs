// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An allocation token, not a claim inferred from a path or its permissions.

use super::System;
use std::{
    io,
    path::{Path, PathBuf},
};

/// An exclusive ephemeral directory owned until cleanup or explicit transfer.
///
/// Keep the guard alive while an external tool or publisher uses its inner paths.
/// Cleanup is path based: this does not protect against a hostile concurrent ancestor
/// or same-UID replacement, and Drop is best effort rather than proof of cleanup.
pub struct Scratch<'s> {
    owner: Box<dyn OwnedDirectory + 's>,
}

impl<'s> Scratch<'s> {
    /// Allocate under the backend's temporary parent (the Fake stays in RAM).
    ///
    /// # Errors
    /// Invalid label, exhausted exclusive allocation attempts or backend refusal.
    pub fn new(sys: &'s dyn System, label: &str) -> io::Result<Self> {
        prefix(label)?;
        sys.scratch_dir(None, label)
    }

    /// Allocate in `parent`, keeping staged renames on its filesystem.
    /// The caller admits or creates that parent; this never adopts an occupied child.
    ///
    /// # Errors
    /// Invalid label, exhausted exclusive allocation attempts or backend refusal.
    pub fn new_in(sys: &'s dyn System, parent: &Path, label: &str) -> io::Result<Self> {
        prefix(label)?;
        sys.scratch_dir(Some(parent), label)
    }

    /// The exact allocation this guard owns.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.owner.path()
    }

    /// Remove this allocation. An error retains the token for retry or explicit keep.
    ///
    /// # Errors
    /// The cleanup error, with unresolved ownership still held by this guard.
    pub fn close(&mut self) -> io::Result<()> {
        self.owner.close()
    }

    /// Transfer cleanup responsibility, for example with a retained rollback bundle.
    #[must_use]
    pub fn keep(self) -> PathBuf {
        self.owner.keep()
    }

    pub(super) fn from_owner(owner: Box<dyn OwnedDirectory + 's>) -> Self {
        Self { owner }
    }
}

impl AsRef<Path> for Scratch<'_> {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::fmt::Debug for Scratch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scratch")
            .field("path", &self.path())
            .finish()
    }
}

pub(super) trait OwnedDirectory: Send + Sync {
    fn path(&self) -> &Path;
    fn close(&mut self) -> io::Result<()>;
    fn keep(self: Box<Self>) -> PathBuf;
}

pub(super) fn prefix(label: &str) -> io::Result<String> {
    if label.is_empty()
        || label.len() > 48
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch label is not a short filename component",
        ));
    }
    Ok(format!(".aterm-machines-{label}-"))
}

impl Scratch<'static> {
    pub(super) fn live(parent: Option<&Path>, label: &str) -> io::Result<Self> {
        let builder = aterm_tempfile::Builder::new().prefix(&prefix(label)?);
        let dir = match parent {
            Some(parent) => builder.tempdir_in(parent)?,
            None => builder.tempdir()?,
        };
        Ok(Self::from_owner(Box::new(LiveDirectory { dir: Some(dir) })))
    }
}

struct LiveDirectory {
    dir: Option<aterm_tempfile::TempDir>,
}

impl OwnedDirectory for LiveDirectory {
    fn path(&self) -> &Path {
        self.dir
            .as_ref()
            .map_or_else(|| Path::new(""), aterm_tempfile::TempDir::path)
    }

    fn close(&mut self) -> io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        match std::fs::remove_dir_all(dir.path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Disarm only after successful removal. A failed close retains the owner.
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
        Ok(())
    }

    fn keep(mut self: Box<Self>) -> PathBuf {
        self.dir
            .take()
            .map_or_else(PathBuf::new, aterm_tempfile::TempDir::keep)
    }
}

#[cfg(test)]
#[path = "scratch_tests.rs"]
mod tests;
