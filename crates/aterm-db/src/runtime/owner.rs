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
        let (file, meta) = aterm_uds::kept_out(|| private_file::open_private(dir, "runtime.lock"))
            .map_err(|_| Error::new("Commons owner lease is not private"))?;
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

/// Shared advisory lock (`window.lock`) held by an interactive `aterm` runtime once its first
/// LAN gate opens. `aterm node serve` yields observer and transport ownership while a ready
/// window runs, and takes them back once the last such window exits. Claims and probes
/// retain the observer configuration identity and resolve its current state directory,
/// so a history migration preserves the same held claim.
pub(super) struct WindowClaim {
    file: File,
}
impl WindowClaim {
    pub(super) fn acquire(config: &Path) -> Result<Self> {
        let dir = crate::network_state::directory(config);
        // Only the observer prepares or migrates its directory. A pre-open claim
        // must not create a legacy directory beside a concurrently prepared fence.
        private_file::check_private_dir(&dir)?;
        aterm_secrets::fsx::write_new(&dir.join("window.lock"), b"", 0o600)
            .map_err(|_| Error::new("Commons window claim cannot be created"))?;
        let (file, _) = aterm_uds::kept_out(|| private_file::open_private(&dir, "window.lock"))
            .map_err(|_| Error::new("Commons window claim is not private"))?;
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

/// Whether a local interactive `aterm` window holds [`WindowClaim`] for `config`.
pub(super) fn window_claimed(config: &Path) -> bool {
    let dir = crate::network_state::directory(config);
    let Ok((file, _)) = aterm_uds::kept_out(|| private_file::open_private(&dir, "window.lock"))
    else {
        return false;
    };
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

#[cfg(test)]
mod tests {
    use super::{WindowClaim, private_file, window_claimed};
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::MetadataExt as _;

    #[cfg(target_os = "macos")]
    #[test]
    fn window_claim_follows_observer_migration_and_preserves_release_identity() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let legacy = config.with_file_name("network-state");
        private_file::create_private_dir(&legacy).unwrap();
        let claim = WindowClaim::acquire(&config).unwrap();
        assert!(window_claimed(&config));
        assert_eq!(crate::network_state::directory(&config), legacy);
        let before = std::fs::metadata(legacy.join("window.lock")).unwrap();

        let history = crate::network_state::Observer::open(
            &config,
            crate::network_state::ObservationHandle::default(),
        )
        .unwrap();
        let current = crate::network_state::directory(&config);
        assert_ne!(current, legacy);
        assert!(legacy.is_file(), "the retired directory remains fenced");
        let after = std::fs::metadata(current.join("window.lock")).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert!(
            window_claimed(&config),
            "migration keeps the held claim visible"
        );

        drop(claim);
        assert!(!window_claimed(&config));
        let claim = WindowClaim::acquire(&config).unwrap();
        assert!(
            window_claimed(&config),
            "a new claim uses the migrated directory"
        );
        drop(claim);
        assert!(!window_claimed(&config));
        drop(history);
    }

    #[test]
    fn a_window_claim_never_creates_the_observers_directory() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let dir = crate::network_state::directory(&config);
        assert!(WindowClaim::acquire(&config).is_err());
        assert!(!dir.exists(), "the observer alone prepares its directory");

        private_file::create_private_dir(&dir).unwrap();
        let claim = WindowClaim::acquire(&config).unwrap();
        assert!(window_claimed(&config));
        drop(claim);
        assert!(!window_claimed(&config));
    }
}
