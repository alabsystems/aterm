// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claimed, bounded, machine-encrypted custody of an unpublished native completion.

use aterm_secrets::SecretBytes;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "pending_native_store_identity.rs"]
mod physical;
pub(super) use physical::Identity as PhysicalIdentity;

const UNAVAILABLE: &str = "Protected native completion is unavailable";

pub(super) struct Store {
    path: PathBuf,
    recipient: aterm_secrets::age::Recipient,
    expected: Option<[u8; 32]>,
    max: usize,
    retired: AtomicBool,
    _claim: super::browser_window::ProfileLock,
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

impl Store {
    pub(super) fn open(
        directory: &Path,
        home: &Path,
        max: usize,
    ) -> Result<(Self, Option<SecretBytes>), &'static str> {
        Self::open_using(directory, home, max, true, true)?.ok_or(UNAVAILABLE)
    }

    /// Claim an existing response without creating a directory or machine key.
    pub(super) fn open_existing(
        directory: &Path,
        home: &Path,
        max: usize,
    ) -> Result<Option<(Self, Option<SecretBytes>)>, &'static str> {
        Self::open_using(directory, home, max, false, true)
    }

    /// Prepare custody only with an established machine identity. This cannot
    /// enroll, replace, set aside, or mint a machine key.
    pub(super) fn open_owned(
        directory: &Path,
        home: &Path,
        max: usize,
    ) -> Result<(Self, Option<SecretBytes>), &'static str> {
        Self::open_using(directory, home, max, true, false)?.ok_or(UNAVAILABLE)
    }

    pub(super) fn open_owned_existing(
        directory: &Path,
        home: &Path,
        max: usize,
    ) -> Result<Option<(Self, Option<SecretBytes>)>, &'static str> {
        Self::open_using(directory, home, max, false, false)
    }

    /// An immutable issuance checkpoint can bind this exact encrypted revision.
    /// Encryption alone is not producer-origin authentication.
    /// Descriptive physical namespace captured from this real claimed store.
    /// The caller must separately authenticate its envelope and current role.
    pub(super) fn physical_identity(&self) -> Result<PhysicalIdentity, &'static str> {
        self.unchanged()?;
        let directory = self.path.parent().ok_or(UNAVAILABLE)?;
        let observed = PhysicalIdentity::capture(directory, &self._claim)?;
        self.unchanged()?;
        Ok(observed)
    }

    pub(super) fn sealed_digest(&self) -> Option<[u8; 32]> {
        self.expected
    }

    pub(super) fn recipient(&self) -> aterm_secrets::age::Recipient {
        self.recipient
    }

    fn open_using(
        directory: &Path,
        home: &Path,
        max: usize,
        prepare: bool,
        may_create_identity: bool,
    ) -> Result<Option<(Self, Option<SecretBytes>)>, &'static str> {
        if !directory.is_absolute() || !home.is_absolute() {
            return Err(UNAVAILABLE);
        }
        if prepare {
            aterm_secrets::fsx::ensure_private_dir(directory).map_err(|_| UNAVAILABLE)?;
        } else {
            match std::fs::symlink_metadata(directory) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => return Err(UNAVAILABLE),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    return Ok(None);
                }
                Err(_) => return Err(UNAVAILABLE),
            }
        }
        let path = directory.join("candidate.age");
        // Absence authorizes no exchange. Do not require writable new custody for a
        // read-only observation; a refresh will claim and re-read through open().
        if !prepare
            && aterm_secrets::fsx::read_private(&path, max)
                .map_err(|_| UNAVAILABLE)?
                .is_none()
        {
            return Ok(None);
        }
        let claim = super::browser_window::ProfileLock::claim(directory)
            .map_err(|_| "Native completion is held or unavailable")?;
        let ciphertext = aterm_secrets::fsx::read_private(&path, max).map_err(|_| UNAVAILABLE)?;
        if !prepare && ciphertext.is_none() {
            return Ok(None);
        }
        let paths = if may_create_identity {
            aterm_secrets::client::Paths::for_home(home)
        } else {
            aterm_secrets::client::Paths::locate_for_home(home)
        };
        let machine = if ciphertext.is_some() || !may_create_identity {
            aterm_secrets::client::read_machine_identity(&paths)
                .map_err(|_| UNAVAILABLE)?
                .ok_or("Native completion's machine key is unavailable")?
        } else {
            aterm_secrets::client::machine_identity(&paths).map_err(|_| UNAVAILABLE)?
        };
        let plaintext = ciphertext
            .as_ref()
            .map(|bytes| {
                aterm_secrets::age::decrypt(std::slice::from_ref(&machine), bytes.as_slice())
                    .map_err(|_| "Protected native completion failed decryption")
            })
            .transpose()?;
        Ok(Some((
            Self {
                expected: ciphertext.as_ref().map(|bytes| digest(bytes.as_slice())),
                path,
                recipient: machine.recipient(),
                max,
                retired: AtomicBool::new(false),
                _claim: claim,
            },
            plaintext,
        )))
    }

    pub(super) fn unchanged(&self) -> Result<(), &'static str> {
        if self.retired.load(Ordering::Acquire) {
            return Ok(());
        }
        let bytes =
            aterm_secrets::fsx::read_private(&self.path, self.max).map_err(|_| UNAVAILABLE)?;
        if bytes.as_ref().map(|bytes| digest(bytes.as_slice())) != self.expected {
            return Err("Native completion changed during recovery");
        }
        Ok(())
    }

    pub(super) fn save(&mut self, plaintext: &[u8]) -> Result<(), &'static str> {
        if plaintext.is_empty()
            || plaintext.len() > self.max
            || self.retired.load(Ordering::Acquire)
        {
            return Err(UNAVAILABLE);
        }
        let ciphertext =
            aterm_secrets::age::encrypt(&[self.recipient], plaintext).map_err(|_| UNAVAILABLE)?;
        if ciphertext.len() > self.max {
            return Err(UNAVAILABLE);
        }
        self.unchanged()?;
        aterm_secrets::fsx::write_atomic(&self.path, &ciphertext, 0o600)
            .map_err(|_| UNAVAILABLE)?;
        self.expected = Some(digest(&ciphertext));
        Ok(())
    }

    /// Reconcile a rename that may have succeeded before its writer observed a
    /// flush failure. Only the exact caller-owned payload can update this claim;
    /// an unrelated replacement is never overwritten or adopted.
    pub(super) fn recover_exact_write(
        &mut self,
        identity: &aterm_secrets::age::Identity,
        plaintext: &[u8],
    ) -> Result<bool, &'static str> {
        if self.retired.load(Ordering::Acquire)
            || identity.recipient() != self.recipient
            || plaintext.is_empty()
            || plaintext.len() > self.max
        {
            return Err(UNAVAILABLE);
        }
        let parent = self.path.parent().ok_or(UNAVAILABLE)?;
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(UNAVAILABLE)?;
        let (file, metadata) = match aterm_uds::private_file::open_private(parent, name) {
            Ok(opened) => opened,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(false),
            Err(_) => return Err(UNAVAILABLE),
        };
        if metadata.len() == 0 || metadata.len() > self.max as u64 {
            return Err(UNAVAILABLE);
        }
        let mut ciphertext = zeroize::Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
        (&file)
            .take(self.max as u64 + 1)
            .read_to_end(&mut ciphertext)
            .map_err(|_| UNAVAILABLE)?;
        if ciphertext.len() > self.max {
            return Err(UNAVAILABLE);
        }
        let decoded = aterm_secrets::age::decrypt(std::slice::from_ref(identity), &ciphertext)
            .map_err(|_| UNAVAILABLE)?;
        if !aterm_digest::ct_eq(decoded.as_slice(), plaintext) {
            return Ok(false);
        }
        aterm_durable::Flush::Durable
            .file(&file)
            .map_err(|_| UNAVAILABLE)?;
        aterm_durable::Flush::Durable
            .dir(parent)
            .map_err(|_| UNAVAILABLE)?;
        let current = aterm_secrets::fsx::read_private(&self.path, self.max)
            .map_err(|_| UNAVAILABLE)?
            .ok_or(UNAVAILABLE)?;
        let expected = digest(&ciphertext);
        if digest(current.as_slice()) != expected
            || !astream_sys::file_matches_path(&file, &self.path).unwrap_or(false)
        {
            return Err(UNAVAILABLE);
        }
        self.expected = Some(expected);
        Ok(true)
    }

    pub(super) fn retire(&self) -> Result<(), &'static str> {
        if self.retired.load(Ordering::Acquire) {
            return Ok(());
        }
        self.unchanged()?;
        if self.expected.is_some() {
            std::fs::remove_file(&self.path).map_err(|_| UNAVAILABLE)?;
        }
        self.retired.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (aterm_tempfile::TempDir, aterm_secrets::age::Identity, Store) {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        let identity = aterm_secrets::client::machine_identity_as(
            &aterm_secrets::client::Paths::locate_for_home(&home),
            aterm_secrets::Making {
                test_binary: true,
                ..Default::default()
            },
        )
        .unwrap();
        let (store, _) = Store::open_owned(&root.path().join("custody"), &home, 64 * 1024).unwrap();
        (root, identity, store)
    }

    #[test]
    fn exact_owned_payload_recovers_an_uncertain_completed_rename() {
        let (_root, identity, mut store) = fixture();
        store.save(b"prior").unwrap();
        let ciphertext =
            aterm_secrets::age::encrypt(&[identity.recipient()], b"completed-response").unwrap();
        aterm_secrets::fsx::write_atomic(&store.path, &ciphertext, 0o600).unwrap();
        assert!(store.unchanged().is_err());
        assert!(
            store
                .recover_exact_write(&identity, b"completed-response")
                .unwrap()
        );
        assert!(store.unchanged().is_ok());
        store.save(b"acknowledged").unwrap();
        assert!(store.unchanged().is_ok());
    }

    #[test]
    fn recovery_never_adopts_or_overwrites_a_different_payload() {
        let (_root, identity, mut store) = fixture();
        store.save(b"prior").unwrap();
        let before = store.expected;
        let ciphertext =
            aterm_secrets::age::encrypt(&[identity.recipient()], b"different-response").unwrap();
        aterm_secrets::fsx::write_atomic(&store.path, &ciphertext, 0o600).unwrap();
        assert!(
            !store
                .recover_exact_write(&identity, b"completed-response")
                .unwrap()
        );
        assert_eq!(store.expected, before);
        assert_eq!(std::fs::read(&store.path).unwrap(), ciphertext);
        assert!(store.unchanged().is_err());
    }

    #[test]
    fn retired_claim_cannot_recover_or_create_another_record() {
        let (_root, identity, mut store) = fixture();
        store.save(b"completed-response").unwrap();
        store.retire().unwrap();
        assert!(
            store
                .recover_exact_write(&identity, b"completed-response")
                .is_err()
        );
        assert!(!store.path.exists());
    }
    #[cfg(unix)]
    #[test]
    fn physical_store_identity_joins_aliases_without_reacquiring_or_granting_a_lock() {
        let (root, _identity, mut store) = fixture();
        store.save(b"namespace-fixture").unwrap();
        let observed = store.physical_identity().unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(store.path.parent().unwrap(), &alias).unwrap();
        let through_alias = PhysicalIdentity::capture(&alias, &store._claim).unwrap();
        assert_eq!(observed, through_alias);
        assert!(observed.current());
        let directory = store.path.parent().unwrap().to_path_buf();
        drop(store);
        let _exclusive = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
        assert!(
            observed.current(),
            "identity observation acquires no competing lock and grants no exclusion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn physical_store_identity_refuses_replaced_directory_or_claim_descriptor() {
        let (root, _identity, mut store) = fixture();
        store.save(b"namespace-fixture").unwrap();
        let observed = store.physical_identity().unwrap();
        let directory = store.path.parent().unwrap().to_path_buf();
        std::fs::rename(&directory, root.path().join("retired-custody")).unwrap();
        aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
        let _replacement = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
        assert!(!observed.current());
        assert!(store.physical_identity().is_err());
        assert!(
            PhysicalIdentity::capture(&directory, &store._claim).is_err(),
            "different actual held fd cannot adopt a replacement path"
        );
    }
}
