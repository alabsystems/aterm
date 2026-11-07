// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Physical namespace metadata only. It does not authenticate a Book, prove
//! independent credentials, or extend the lifetime of the original file claim.

use super::*;

#[derive(Clone)]
pub(in crate::harness) struct Identity {
    directory: PathBuf,
    directory_id: (u64, u64),
    claim_id: (u64, u64),
}
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        self.directory_id == other.directory_id && self.claim_id == other.claim_id
    }
}
impl Eq for Identity {}
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeStoreIdentity(redacted)")
    }
}
impl Identity {
    pub(in crate::harness) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(super) fn capture(
        directory: &Path,
        claim: &super::super::browser_window::ProfileLock,
    ) -> Result<Self, &'static str> {
        #[cfg(unix)]
        {
            let directory = directory.canonicalize().map_err(|_| UNAVAILABLE)?;
            let observed = inspect(&directory)?;
            if claim.file_identity().map_err(|_| UNAVAILABLE)? != observed.claim_id
                || !claim.current(&directory)
            {
                return Err(UNAVAILABLE);
            }
            Ok(observed)
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, claim);
            Err(UNAVAILABLE)
        }
    }

    /// No create, lock acquisition or filesystem I/O under a registry mutex.
    pub(in crate::harness) fn current(&self) -> bool {
        #[cfg(unix)]
        {
            inspect(&self.directory).is_ok_and(|now| now == *self)
        }
        #[cfg(not(unix))]
        {
            let _ = &self.directory;
            false
        }
    }
}

#[cfg(unix)]
fn inspect(directory: &Path) -> Result<Identity, &'static str> {
    use aterm_uds::private_file;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    private_file::check_private_dir(directory).map_err(|_| UNAVAILABLE)?;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)
        .map_err(|_| UNAVAILABLE)?;
    let descriptor = opened.metadata().map_err(|_| UNAVAILABLE)?;
    let name = super::super::browser_window::PROFILE_LOCK_FILE;
    let (claim, claim_metadata) =
        private_file::open_private(directory, name).map_err(|_| UNAVAILABLE)?;
    // Join descriptor identities directly, then verify the trusted paths still
    // name them. A replace-and-restore between probes cannot supply another fd.
    let now = std::fs::symlink_metadata(directory).map_err(|_| UNAVAILABLE)?;
    private_file::check_private_dir(directory).map_err(|_| UNAVAILABLE)?;
    if !now.is_dir()
        || descriptor.dev() != now.dev()
        || descriptor.ino() != now.ino()
        || !astream_sys::file_matches_path(&claim, &directory.join(name)).unwrap_or(false)
    {
        return Err(UNAVAILABLE);
    }
    Ok(Identity {
        directory: directory.into(),
        directory_id: (descriptor.dev(), descriptor.ino()),
        claim_id: (claim_metadata.dev(), claim_metadata.ino()),
    })
}
