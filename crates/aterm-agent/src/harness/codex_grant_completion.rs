// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fresh native code-response provenance. File evidence alone grants no identity
//! or ownership; only the successful exchange can mint a FreshCodeGrant.

use super::{AuthLock, Secret};
use crate::harness::codex_renewal::Identity;
use std::path::{Path, PathBuf};

const SLOT_AUTH_MAX: usize = 16 * 1024;

/// An opened destination parent, without any provider or credential authority.
pub struct StagedWitness {
    auth_file: PathBuf,
    parent: std::fs::File,
}

impl StagedWitness {
    pub(in crate::harness) fn open(auth_file: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let directory = auth_file.parent().ok_or("auth destination has no parent")?;
            let name = auth_file
                .file_name()
                .ok_or("auth destination has no name")?;
            let parent = astream_sys::open_nonblocking(directory, astream_sys::Links::Refuse)
                .map_err(|_| "auth destination parent unavailable")?;
            astream_sys::owner_private_dir(&parent)
                .map_err(|_| "auth parent unavailable")?
                .map_err(|_| "auth parent is not owner-private")?;
            let canonical = directory
                .canonicalize()
                .map_err(|_| "auth destination parent unavailable")?;
            let witness = Self {
                auth_file: canonical.join(name),
                parent,
            };
            if !witness.matches_auth_file(auth_file) {
                return Err("auth destination parent changed".into());
            }
            Ok(witness)
        }
        #[cfg(not(unix))]
        {
            let _ = auth_file;
            Err("native grant custody is unsupported on this platform".into())
        }
    }

    pub(in crate::harness) fn same_open_parent(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            astream_sys::owner_private_dir(&self.parent)
                .ok()
                .and_then(Result::ok)
                .zip(
                    astream_sys::owner_private_dir(&other.parent)
                        .ok()
                        .and_then(Result::ok),
                )
                .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
        }
        #[cfg(not(unix))]
        {
            let _ = other;
            false
        }
    }

    pub(in crate::harness) fn matches_auth_file(&self, auth_file: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let Some(parent) = auth_file.parent() else {
                return false;
            };
            let Some(name) = auth_file.file_name() else {
                return false;
            };
            let Ok(current) = astream_sys::open_nonblocking(parent, astream_sys::Links::Refuse)
            else {
                return false;
            };
            let Ok(Ok(current)) = astream_sys::owner_private_dir(&current) else {
                return false;
            };
            let Ok(Ok(opened)) = astream_sys::owner_private_dir(&self.parent) else {
                return false;
            };
            if opened.dev() != current.dev() || opened.ino() != current.ino() {
                return false;
            }
            let Ok(canonical) = parent.canonicalize() else {
                return false;
            };
            if canonical.join(name) != self.auth_file {
                return false;
            }
            let Some(name) = name.to_str() else {
                return false;
            };
            let file_private = match aterm_uds::private_file::open_private(parent, name) {
                Ok(_) | Err(aterm_uds::private_file::PrivateFileError::Missing) => true,
                Err(_) => false,
            };
            file_private && astream_sys::file_matches_path(&self.parent, parent).unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            let _ = auth_file;
            false
        }
    }

    /// Stable opened-parent coordinates for an authenticated encrypted receipt.
    /// These are file evidence, never independent grant authority.
    pub(in crate::harness) fn coordinates(&self) -> Result<(&Path, u64, u64), String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = self
                .parent
                .metadata()
                .map_err(|_| "auth parent unavailable")?;
            Ok((&self.auth_file, metadata.dev(), metadata.ino()))
        }
        #[cfg(not(unix))]
        {
            Err("native grant custody is unsupported on this platform".into())
        }
    }
}

impl std::fmt::Debug for StagedWitness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedWitness").finish_non_exhaustive()
    }
}

/// One actual successful native authorization-code response, consumed under its
/// original AuthLock. It has no reader, deserializer or copied-file constructor.
pub struct FreshCodeGrant<'auth> {
    identity: Identity,
    owner: String,
    doc: Secret,
    witness: StagedWitness,
    predecessor: Option<String>,
    _lock: &'auth AuthLock,
}

impl<'auth> FreshCodeGrant<'auth> {
    pub(super) fn mint(
        identity: Identity,
        owner: &str,
        doc: Vec<u8>,
        witness: StagedWitness,
        predecessor: Option<&str>,
        lock: &'auth AuthLock,
    ) -> Result<Self, Vec<u8>> {
        if owner.is_empty()
            || doc.len() > SLOT_AUTH_MAX
            || predecessor.is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(doc);
        }
        Ok(Self {
            identity,
            owner: owner.to_owned(),
            doc: Secret::from_vec(doc),
            witness,
            predecessor: predecessor.map(str::to_owned),
            _lock: lock,
        })
    }

    /// Observed by the producer under this same AuthLock before its POST.
    pub(in crate::harness) fn predecessor(&self) -> Option<&str> {
        self.predecessor.as_deref()
    }

    pub(in crate::harness) fn consume<R>(
        self,
        f: impl FnOnce(&Identity, &str, &[u8], &StagedWitness) -> R,
    ) -> R {
        f(
            &self.identity,
            &self.owner,
            self.doc.expose(),
            &self.witness,
        )
    }
}

impl std::fmt::Debug for FreshCodeGrant<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FreshCodeGrant").finish_non_exhaustive()
    }
}

/// The existing native owner retains the completed response on every outcome,
/// including publication failure. Its owned Output carries the receipt or the
/// exact response/failure, never an error string that discards completed secrets.
pub(in crate::harness) trait CodeGrantCapture {
    type Output;
    fn begin_exchange(&mut self) -> Result<(), String>;
    /// Called only by the live producer before invoking the HTTP send.
    fn not_dispatched(self) -> Self::Output;
    fn retain_verified(self, grant: FreshCodeGrant<'_>) -> Self::Output;
    fn retain_unverified(self, doc: &[u8]) -> Self::Output;
    fn retain_rejected(self, rejection: super::CodeRejection<'_>) -> Self::Output;
    fn verified_durable(output: &Self::Output) -> bool;
}

// Uninhabited: a legacy wrapper can pass None but can never accidentally
// construct a capture that drops a completed response.
pub(in crate::harness) enum NoGrantCapture {}
impl CodeGrantCapture for NoGrantCapture {
    type Output = std::convert::Infallible;
    fn begin_exchange(&mut self) -> Result<(), String> {
        match *self {}
    }
    fn not_dispatched(self) -> Self::Output {
        match self {}
    }
    fn retain_verified(self, _grant: FreshCodeGrant<'_>) -> Self::Output {
        match self {}
    }
    fn retain_unverified(self, _doc: &[u8]) -> Self::Output {
        match self {}
    }
    fn retain_rejected(self, _rejection: super::CodeRejection<'_>) -> Self::Output {
        match self {}
    }
    fn verified_durable(output: &Self::Output) -> bool {
        match *output {}
    }
}

#[must_use = "native response custody must be retained independently of its sign-in outcome"]
pub(in crate::harness) struct CapturedLogin<R> {
    pub(super) drove: super::Drove,
    pub(super) retained: Option<R>,
}
impl<R> CapturedLogin<R> {
    pub(in crate::harness) fn new(drove: super::Drove, retained: Option<R>) -> Self {
        Self { drove, retained }
    }
    pub(in crate::harness) fn into_parts(self) -> (super::Drove, Option<R>) {
        (self.drove, self.retained)
    }
}
