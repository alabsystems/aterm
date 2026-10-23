// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private per-process run directory under `<aterm state>/ai/run/<pid>-<16 hex>/`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// The directory of run directories, under the AI store.
pub const DIR: &str = "run";

/// One run directory; dropping it removes it.
#[derive(Debug)]
pub struct RunDir {
    path: PathBuf,
}

impl RunDir {
    /// Makes a fresh run directory under `store_dir` after sweeping dead-process directories.
    ///
    /// # Errors
    /// The directory cannot be made, or one by that name already exists.
    pub fn make(store_dir: &Path) -> io::Result<RunDir> {
        let root = store_dir.join(DIR);
        aterm_types::fs_restricted::ensure_private_dir(store_dir)?;
        aterm_types::fs_restricted::ensure_private_dir(&root)?;
        sweep(&root);
        let token = aterm_uds::rand::hex_token::<8>()?;
        let path = root.join(format!("{}-{token}", std::process::id()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
        std::fs::DirBuilder::new().create(&path)?;
        Ok(RunDir { path })
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a new private file `name` here with `text`.
    ///
    /// # Errors
    /// It exists already, or the write failed.
    pub fn write(&self, name: &str, text: &str) -> io::Result<PathBuf> {
        let path = self.path.join(name);
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = open.open(&path)?;
        f.write_all(text.as_bytes())?;
        Ok(path)
    }

    /// Makes an empty private subdirectory `name` (a reviewer's starting directory).
    ///
    /// # Errors
    /// It exists already, or it could not be made.
    pub fn subdir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.path.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
        std::fs::DirBuilder::new().create(&path)?;
        Ok(path)
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Removes every run directory whose process is gone.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name.split('-').next().and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|p| p != std::process::id() && !aterm_uds::process::pid_alive(p)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
