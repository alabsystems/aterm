// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe immutable local custody without taking the control journal's writer
//! lock. Mutable journal contents are checked by every actual repair operation.

use crate::{Error, Result};
use aterm_digest::Sha256;
use aterm_secrets::{client::Paths, fsx};
use std::path::{Path, PathBuf};

pub(super) struct Custody {
    immutable: Vec<(PathBuf, [u8; 32])>,
    journal: PathBuf,
}
fn digest(path: &Path) -> Result<[u8; 32]> {
    let bytes = fsx::read_private(path, 4096)
        .map_err(|_| Error::new("Commons custody file is not private"))?
        .ok_or_else(|| Error::new("Commons custody file is missing"))?;
    Ok(Sha256::digest(bytes.as_slice()))
}
impl Custody {
    pub(super) fn capture(paths: &Paths, journal: &Path) -> Result<Self> {
        let commons = paths.dir.join("commons");
        let immutable = [
            paths.dir.join("identity"),
            commons.join("device.age"),
            commons.join("device.claim"),
            commons.join("default.pin"),
            journal.join("control.anchor"),
        ]
        .into_iter()
        .map(|path| digest(&path).map(|hash| (path, hash)))
        .collect::<Result<_>>()?;
        let custody = Self {
            immutable,
            journal: journal.into(),
        };
        custody.recheck()?;
        Ok(custody)
    }
    pub(super) fn recheck(&self) -> Result<()> {
        for (path, expected) in &self.immutable {
            if &digest(path)? != expected {
                return Err(Error::new("Commons immutable custody changed"));
            }
        }
        for name in ["control.astream", "control.checkpoint"] {
            aterm_uds::private_file::open_private(&self.journal, name)
                .map_err(|_| Error::new("Commons established history is missing or not private"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_change_or_lost_history_withdraws_without_repairing_storage() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        let journal = paths.dir.join("commons/control");
        fsx::ensure_private_dir(&paths.dir).unwrap();
        fsx::ensure_private_dir(&paths.dir.join("commons")).unwrap();
        fsx::ensure_private_dir(&journal).unwrap();
        for path in [
            paths.dir.join("identity"),
            paths.dir.join("commons/device.age"),
            paths.dir.join("commons/device.claim"),
            paths.dir.join("commons/default.pin"),
            journal.join("control.anchor"),
            journal.join("control.astream"),
            journal.join("control.checkpoint"),
        ] {
            fsx::write_atomic(&path, b"fixture", 0o600).unwrap();
        }
        let custody = Custody::capture(&paths, &journal).unwrap();
        fsx::write_atomic(
            &journal.join("control.checkpoint"),
            b"mutable checkpoint",
            0o600,
        )
        .unwrap();
        custody.recheck().unwrap();
        fsx::write_atomic(&paths.dir.join("identity"), b"replacement", 0o600).unwrap();
        assert!(custody.recheck().is_err());
        fsx::write_atomic(&paths.dir.join("identity"), b"fixture", 0o600).unwrap();
        std::fs::remove_file(journal.join("control.astream")).unwrap();
        assert!(custody.recheck().is_err());
        assert!(!journal.join("control.astream").exists());
    }
}
