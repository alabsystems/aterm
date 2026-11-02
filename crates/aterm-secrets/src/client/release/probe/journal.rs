// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private, create-only ownership and serialized durable progress for one probe.

use super::{encoding, model::Journal};
use crate::{Error, Result, fsx};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};

const FILE: &str = "probe.json";
const LOCK: &str = ".probe.lock";
const MARKER: &str = "reservation.json";
const CAP: usize = 128 * 1024;

pub(super) struct Store {
    dir: PathBuf,
    identity: (u64, u64),
    _lock: File,
    bytes: Option<Vec<u8>>,
    reservation: Vec<u8>,
    initial: Journal,
}

impl Store {
    pub(super) fn open(dir: &Path, nonce: &str, seed: Option<&Journal>) -> Result<Self> {
        path(dir, nonce)?;
        if let Some(seed) = seed {
            seed.validate()?;
            if seed.nonce != nonce
                || seed.subject.uid != fsx::euid()
                || seed.artifact.is_some()
                || seed.submission.is_some()
                || seed.pending.is_some()
                || seed.cleanup_only
                || seed
                    .repositories
                    .iter()
                    .any(|r| r != &super::model::Repository::new(r.owner.clone()))
            {
                return Err(Error::new("invalid initial release probe reservation"));
            }
        }
        let parent = dir
            .parent()
            .ok_or_else(|| Error::new("probe directory has no parent"))?;
        match std::fs::symlink_metadata(dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && seed.is_some() => {
                // The explicit parent must already be private. Never create or narrow an
                // unrelated directory tree just because a caller supplied its path.
                private(parent)?;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(dir)
                    .map_err(|_| Error::new("cannot create owned release probe directory"))?;
                aterm_durable::Flush::Durable
                    .dir(parent)
                    .map_err(|_| Error::new("cannot persist release probe directory"))?;
            }
            Err(_) => return Err(Error::new("release probe directory is unavailable")),
            Ok(_) => {}
        }
        let identity = identity(dir)?;
        let reservation = match read(dir, MARKER)? {
            Some(bytes) => bytes,
            None => {
                // A crash immediately after mkdir leaves exactly this empty, owned 0700
                // nonce-named directory. It can be reserved, but no unknown file is adopted.
                let seed = seed.ok_or_else(|| {
                    Error::new("probe has no reservation; repeat the original run request")
                })?;
                if std::fs::read_dir(dir)
                    .map_err(|_| Error::new("cannot inspect empty probe reservation"))?
                    .next()
                    .is_some()
                {
                    return Err(Error::new("unreserved probe directory is not empty"));
                }
                let bytes = reservation_bytes(seed, identity)?;
                if !fsx::write_new(&dir.join(MARKER), &bytes, 0o600)? {
                    return Err(Error::new(
                        "release probe reservation appeared concurrently",
                    ));
                }
                aterm_durable::Flush::Durable
                    .dir(dir)
                    .map_err(|_| Error::new("cannot persist release probe reservation"))?;
                bytes
            }
        };
        let initial = reservation_plan(&reservation, identity, nonce)?;
        if initial.subject.uid != fsx::euid()
            || seed.is_some_and(|seed| {
                seed.subject != initial.subject
                    || seed
                        .repositories
                        .iter()
                        .map(|r| &r.owner)
                        .ne(initial.repositories.iter().map(|r| &r.owner))
            })
        {
            return Err(Error::new(
                "probe reservation names another subject or repository",
            ));
        }
        if read(dir, FILE)?.is_none() {
            reserved_entries(dir)?;
        }
        owned_lock(dir)?;
        let lock: File = fsx::open_lock(&dir.join(LOCK))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "nonblocking owned probe transaction lock, explicitly unlocked by Store::drop"
        )]
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::new("release probe is busy"));
            }
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(Error::new("cannot lock release probe"));
            }
        }
        let mut store = Self {
            dir: dir.to_path_buf(),
            identity,
            _lock: lock,
            bytes: None,
            reservation,
            initial,
        };
        store.recheck()?;
        store.bytes = read(dir, FILE)?;
        if let Some(journal) = store.load()? {
            if !journal.same_plan(&store.initial) {
                return Err(Error::new("probe journal differs from its reserved plan"));
            }
        } else {
            store.save(&store.initial.clone())?;
        }
        Ok(store)
    }

    pub(super) fn load(&self) -> Result<Option<Journal>> {
        self.recheck()?;
        let bytes = read(&self.dir, FILE)?;
        if bytes != self.bytes {
            return Err(Error::new(
                "release probe journal changed outside its transaction",
            ));
        }
        bytes.as_deref().map(encoding::decode).transpose()
    }

    pub(super) fn save(&mut self, journal: &Journal) -> Result<()> {
        let before = self.load()?;
        if !journal.same_plan(&self.initial)
            || before.as_ref().is_some_and(|old| !journal.follows(old))
        {
            return Err(Error::new(
                "probe save changes its reserved identity or prior receipts",
            ));
        }
        let bytes = encoding::encode(journal)?;
        if bytes.len() > CAP {
            return Err(Error::new("release probe journal exceeds its bound"));
        }
        let file = self.dir.join(FILE);
        if self.bytes.is_some() {
            fsx::write_atomic(&file, &bytes, 0o600)?;
        } else if !fsx::write_new(&file, &bytes, 0o600)? {
            return Err(Error::new(
                "release probe journal appeared before publication",
            ));
        }
        // write_new's directory flush is deliberately best-effort for its other callers.
        // An external effect here requires the intent's directory entry to be durable.
        aterm_durable::Flush::Durable
            .dir(&self.dir)
            .map_err(|_| Error::new("cannot persist release probe intent"))?;
        self.recheck()?;
        self.bytes = Some(bytes);
        Ok(())
    }

    fn recheck(&self) -> Result<()> {
        if identity(&self.dir)? != self.identity {
            return Err(Error::new("release probe directory changed"));
        }
        if read(&self.dir, MARKER)?.as_deref() != Some(self.reservation.as_slice()) {
            return Err(Error::new("release probe reservation changed"));
        }
        Ok(())
    }
}

fn path(dir: &Path, nonce: &str) -> Result<()> {
    if !super::model::hex(nonce, 32)
        || !dir.is_absolute()
        || dir
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || dir.file_name().and_then(|s| s.to_str())
            != Some(format!("aterm-release-probe-{nonce}.noindex").as_str())
    {
        return Err(Error::new(
            "probe needs its explicit nonce-named absolute evidence directory",
        ));
    }
    Ok(())
}

fn private(dir: &Path) -> Result<()> {
    aterm_uds::private_file::check_private_dir(dir)
        .map_err(|_| Error::new("release probe directory is not an owned private real directory"))
}

fn identity(dir: &Path) -> Result<(u64, u64)> {
    private(dir)?;
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|_| Error::new("release probe directory metadata unavailable"))?;
    Ok((metadata.dev(), metadata.ino()))
}

fn owned_lock(dir: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(dir.join(LOCK)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Error::new("existing probe lock is unavailable")),
    };
    if !metadata.is_file()
        || metadata.uid() != fsx::euid()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(Error::new(
            "existing probe lock is not an owned private regular file",
        ));
    }
    Ok(())
}

fn read(dir: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    use aterm_uds::private_file::{PrivateFileError, open_private};
    let (file, metadata) = match open_private(dir, name) {
        Ok(value) => value,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(_) => return Err(Error::new("release probe journal cannot be read privately")),
    };
    if metadata.nlink() != 1 || metadata.len() > CAP as u64 {
        return Err(Error::new("release probe journal has unsafe links or size"));
    }
    let mut bytes = Vec::new();
    file.take(CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("release probe journal read failed"))?;
    if bytes.len() > CAP {
        return Err(Error::new("release probe journal grew past its bound"));
    }
    Ok(Some(bytes))
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self._lock.unlock();
    }
}

fn reserved_entries(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|_| Error::new("cannot read probe reservation"))? {
        let entry = entry.map_err(|_| Error::new("cannot read probe reservation entry"))?;
        if !matches!(entry.file_name().to_str(), Some(MARKER | LOCK)) {
            return Err(Error::new(
                "unfinished probe reservation contains an unknown file",
            ));
        }
    }
    Ok(())
}

fn reservation_bytes(seed: &Journal, identity: (u64, u64)) -> Result<Vec<u8>> {
    let seed: aterm_json::Value = aterm_json::from_slice(&encoding::encode(seed)?)
        .map_err(|_| Error::new("cannot encode probe reservation"))?;
    aterm_json::to_vec(&aterm_json::json!({
        "schema":1,"directory_device":identity.0,"directory_inode":identity.1,"initial":seed
    }))
    .map_err(|_| Error::new("cannot encode probe reservation"))
}

fn reservation_plan(bytes: &[u8], identity: (u64, u64), nonce: &str) -> Result<Journal> {
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes).map_err(|_| Error::new("invalid probe reservation"))?;
    if value.as_object().map_or(0, |v| v.len()) != 4
        || value["schema"].as_u64() != Some(1)
        || value["directory_device"].as_u64() != Some(identity.0)
        || value["directory_inode"].as_u64() != Some(identity.1)
    {
        return Err(Error::new("probe reservation belongs to another directory"));
    }
    let bytes = aterm_json::to_vec(&value["initial"])
        .map_err(|_| Error::new("invalid initial probe reservation"))?;
    let seed = encoding::decode(&bytes)?;
    if seed.nonce != nonce
        || seed.artifact.is_some()
        || seed.submission.is_some()
        || seed.pending.is_some()
        || seed.cleanup_only
        || seed
            .repositories
            .iter()
            .any(|r| r != &super::model::Repository::new(r.owner.clone()))
    {
        return Err(Error::new("invalid initial probe reservation"));
    }
    Ok(seed)
}

#[cfg(test)]
mod tests;
