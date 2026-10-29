// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One live transport owner per local secrets root. Never unlink the lease.

use crate::{Error, Result};
use aterm_uds::private_file;
use std::{
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) struct Lease {
    file: File,
    dir: PathBuf,
    identity: (u64, u64),
}
impl Lease {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTransportOwner",
            action = "Acquire",
            project = "aterm_db::runtime::tests::lease_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTransportOwner",
            action = "Contend",
            project = "aterm_db::runtime::tests::lease_transition"
        )
    )]
    pub(super) fn acquire(dir: &Path) -> Result<Option<Self>> {
        private_file::create_private_dir(dir)?;
        aterm_secrets::fsx::write_new(&dir.join("runtime.lock"), b"", 0o600)
            .map_err(|_| Error::new("Commons owner lease cannot be created"))?;
        let (file, meta) = private_file::open_private(dir, "runtime.lock")
            .map_err(|_| Error::new("Commons owner lease is not private"))?;
        aterm_uds::keep_out_of_children(&file)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "lease is CLOEXEC/CLOFORK and explicitly unlocks only after all transport workers join"
        )]
        match file.try_lock() {
            Ok(()) => {
                let lease = Self {
                    file,
                    dir: dir.into(),
                    identity: (meta.dev(), meta.ino()),
                };
                lease.recheck()?;
                Ok(Some(lease))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTransportOwner",
            action = "Replace",
            project = "aterm_db::runtime::tests::lease_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTransportOwner",
            action = "Remove",
            project = "aterm_db::runtime::tests::lease_transition"
        )
    )]
    pub(super) fn recheck(&self) -> Result<()> {
        let (_, meta) = private_file::open_private(&self.dir, "runtime.lock")
            .map_err(|_| Error::new("Commons owner lease disappeared"))?;
        if (meta.dev(), meta.ino()) != self.identity {
            return Err(Error::new("Commons owner lease was replaced"));
        }
        Ok(())
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTransportOwner",
            action = "Transfer",
            project = "aterm_db::runtime::tests::lease_transition"
        )
    )]
    fn release(&self) {
        let _ = self.file.unlock();
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.release();
    }
}

/// Shared advisory lock (`window.lock`) held by every running interactive `aterm` window so
/// `aterm node serve`'s background Commons runtime yields observer and transport ownership while
/// any window runs, and takes them back once the last window exits.
pub(super) struct WindowClaim {
    file: File,
}
impl WindowClaim {
    pub(super) fn acquire(dir: &Path) -> Result<Self> {
        private_file::create_private_dir(dir)?;
        aterm_secrets::fsx::write_new(&dir.join("window.lock"), b"", 0o600)
            .map_err(|_| Error::new("Commons window claim cannot be created"))?;
        let (file, _) = private_file::open_private(dir, "window.lock")
            .map_err(|_| Error::new("Commons window claim is not private"))?;
        aterm_uds::keep_out_of_children(&file)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "window claim is CLOEXEC/CLOFORK and WindowClaim::drop releases by LOCK_UN"
        )]
        file.lock_shared()?;
        Ok(Self { file })
    }
}
impl Drop for WindowClaim {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct WindowProbe(File);
impl Drop for WindowProbe {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether at least one local interactive `aterm` window holds [`WindowClaim`] in `dir`.
pub(super) fn window_claimed(dir: &Path) -> bool {
    let Ok((file, _)) = private_file::open_private(dir, "window.lock") else {
        return false;
    };
    if aterm_uds::keep_out_of_children(&file).is_err() {
        return false;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "probe lock is CLOEXEC/CLOFORK and WindowProbe::drop releases immediately by LOCK_UN"
    )]
    match file.try_lock() {
        Ok(()) => {
            drop(WindowProbe(file));
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}
