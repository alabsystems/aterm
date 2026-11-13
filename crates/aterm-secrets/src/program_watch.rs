// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A service's immutable executable identity, retained from startup through its last round: the
//! vault's server and subscriber, and the setup login job, each of which retires once an update
//! replaced the program its supervisor starts.

use aterm_uds::process::LoadedImage;
use std::path::{Path, PathBuf};

/// A running service and the path its supervisor will start next. Neither is re-sampled as
/// a baseline: the path may already hold a replacement before this watch is constructed.
pub struct ProgramWatch {
    path: Option<PathBuf>,
    image: Option<LoadedImage>,
    started: Option<Stamp>,
}

/// An executable as it is on disk. The loaded identity also catches a replacement that
/// happened before the first disk read; these fields retain detection of in-place rewrites.
type Stamp = (u64, u64, u64, i64);

fn stamp(meta: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino(), meta.size(), meta.mtime())
}

fn metadata(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
}

fn initial_stamp(now: Option<Stamp>, matches: Option<bool>) -> Option<Stamp> {
    // A replacement observed before startup is not the running image's baseline. If the
    // old image later returns to the path, it must not be mistaken for another replacement.
    (matches != Some(false)).then_some(now).flatten()
}

impl ProgramWatch {
    /// The running process's image and the path its supervisor starts.
    #[must_use]
    pub fn current() -> Self {
        let image = LoadedImage::read(std::process::id().try_into().unwrap_or(0));
        let path = std::env::current_exe().ok();
        // On macOS keep the original dyld launch path: a kernel image path may now name
        // the moved-aside bundle. Linux's verified kernel path removes its deleted marker
        // and names a translated program rather than the interpreter, when proven.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let path = image
            .as_ref()
            .map(|image| image.launch_path().to_path_buf())
            .or(path);
        let current = path.as_deref().and_then(metadata);
        let matches = image
            .as_ref()
            .zip(current.as_ref())
            .map(|(image, meta)| image.matches_metadata(meta));
        let started = initial_stamp(current.as_ref().map(stamp), matches);
        Self {
            path,
            image,
            started,
        }
    }

    /// The path the supervisor starts next.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// A positive mismatch asks a supervised process to retire cleanly. A missing path means
    /// no successor is there to start; an unreadable image never means a replacement.
    #[must_use]
    pub fn replaced(&self) -> bool {
        let current = self.path().and_then(metadata);
        let matches = self
            .image
            .as_ref()
            .zip(current.as_ref())
            .map(|(image, meta)| image.matches_metadata(meta));
        replaced(self.started, current.as_ref().map(stamp), matches)
    }
}

fn replaced(started: Option<Stamp>, now: Option<Stamp>, matches: Option<bool>) -> bool {
    now.is_some() && (matches == Some(false) || started.zip(now).is_some_and(|(a, b)| a != b))
}

#[cfg(test)]
#[path = "program_watch_tests.rs"]
mod tests;
