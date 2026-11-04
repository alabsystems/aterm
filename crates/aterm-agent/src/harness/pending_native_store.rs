// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claimed, bounded, machine-encrypted custody of an unpublished native completion.

use aterm_secrets::SecretBytes;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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
        if !directory.is_absolute() || !home.is_absolute() {
            return Err(UNAVAILABLE);
        }
        aterm_secrets::fsx::ensure_private_dir(directory).map_err(|_| UNAVAILABLE)?;
        let claim = super::browser_window::ProfileLock::claim(directory)
            .map_err(|_| "Native completion is held or unavailable")?;
        let path = directory.join("candidate.age");
        let ciphertext = aterm_secrets::fsx::read_private(&path, max).map_err(|_| UNAVAILABLE)?;
        let paths = aterm_secrets::client::Paths::for_home(home);
        let machine = if ciphertext.is_some() {
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
        Ok((
            Self {
                expected: ciphertext.as_ref().map(|bytes| digest(bytes.as_slice())),
                path,
                recipient: machine.recipient(),
                max,
                retired: AtomicBool::new(false),
                _claim: claim,
            },
            plaintext,
        ))
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
