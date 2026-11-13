// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe immutable local custody without taking the control journal's writer
//! lock. Mutable journal contents are checked by every actual repair operation.

use crate::{Error, Result};
use aterm_digest::Sha256;
use aterm_secrets::client::Paths;
use std::path::{Path, PathBuf};

pub(super) struct Custody {
    root: PathBuf,
    identity: [u8; 32],
    commons: PathBuf,
    commons_files: [(&'static str, [u8; 32]); 3],
    journal: PathBuf,
    anchor: [u8; 32],
}
fn digest_in(dir: &aterm_uds::private_file::JudgedDir, name: &str) -> Result<[u8; 32]> {
    let bytes = dir.read(name, 4096).map_err(|error| match error {
        aterm_uds::private_file::PrivateFileError::Missing => {
            Error::new("Commons custody file is missing")
        }
        _ => Error::new("Commons custody file is not private"),
    })?;
    let bytes = zeroize::Zeroizing::new(bytes);
    Ok(Sha256::digest(bytes.as_slice()))
}
fn judge_custody_dir(dir: &Path) -> Result<aterm_uds::private_file::JudgedDir<'_>> {
    aterm_uds::private_file::JudgedDir::judge(dir)
        .map_err(|_| Error::new("Commons custody file is not private"))
}
impl Custody {
    pub(super) fn capture(paths: &Paths, journal: &Path) -> Result<Self> {
        let root_dir = judge_custody_dir(&paths.dir)?;
        let identity = digest_in(&root_dir, "identity")?;
        let commons = paths.dir.join("commons");
        let commons_dir = judge_custody_dir(&commons)?;
        let commons_files = [
            ("device.age", digest_in(&commons_dir, "device.age")?),
            ("device.claim", digest_in(&commons_dir, "device.claim")?),
            ("default.pin", digest_in(&commons_dir, "default.pin")?),
        ];
        let journal_dir = aterm_uds::private_file::JudgedDir::judge(journal)
            .map_err(|_| Error::new("Commons custody file is not private"))?;
        let anchor = digest_in(&journal_dir, "control.anchor")?;
        for name in ["control.astream", "control.checkpoint"] {
            journal_dir
                .open(name)
                .map_err(|_| Error::new("Commons established history is missing or not private"))?;
        }
        Ok(Self {
            root: paths.dir.clone(),
            identity,
            commons,
            commons_files,
            journal: journal.into(),
            anchor,
        })
    }
    pub(super) fn recheck(&self) -> Result<()> {
        let root_dir = judge_custody_dir(&self.root)?;
        if digest_in(&root_dir, "identity")? != self.identity {
            return Err(Error::new("Commons immutable custody changed"));
        }
        let commons_dir = judge_custody_dir(&self.commons)?;
        for (name, expected) in &self.commons_files {
            if &digest_in(&commons_dir, name)? != expected {
                return Err(Error::new("Commons immutable custody changed"));
            }
        }
        let journal_dir = aterm_uds::private_file::JudgedDir::judge(&self.journal)
            .map_err(|_| Error::new("Commons established history is missing or not private"))?;
        if digest_in(&journal_dir, "control.anchor")? != self.anchor {
            return Err(Error::new("Commons immutable custody changed"));
        }
        for name in ["control.astream", "control.checkpoint"] {
            journal_dir
                .open(name)
                .map_err(|_| Error::new("Commons established history is missing or not private"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;
