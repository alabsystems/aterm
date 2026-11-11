// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stable executable an explicit installation selected for this account.
//!
//! This is a location, not authority to use credentials or start a service. Installers publish
//! it after placement; an ordinary startup never adopts its own executable or searches PATH.
//! A same-path update or rollback needs no edit. Readers must admit the account-owned record
//! and the executable afresh, and watch both for repair. Only an absent record permits legacy
//! locations; a refused or unavailable selection is not permission to choose another copy.

use std::path::{Path, PathBuf};

/// The account-relative declaration, independent of the installer's PATH and XDG overrides.
pub const FILE: &str = ".config/aterm/installed-command.json";
/// Maximum declaration size. The record holds one executable path, never credential bytes.
pub const MAX_BYTES: usize = 16 * 1024;

/// The declaration under the account's actual `home`.
#[must_use]
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

/// Install locations used before an installer wrote a declaration, in preference order.
#[must_use]
pub fn legacy(home: &Path) -> [PathBuf; 2] {
    [
        home.join(".local/bin/aterm"),
        home.join(".local/lib/aterm/bin/aterm"),
    ]
}

/// One explicitly placed stable executable, preserved across atomic replacement of its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    executable: PathBuf,
}

impl Selection {
    /// A location suitable for the declaration's UTF-8 wire representation.
    ///
    /// # Errors
    /// The path is relative, not UTF-8, contains NUL, or exceeds the declaration size bound.
    pub fn new(executable: PathBuf) -> Result<Self, String> {
        let text = executable
            .to_str()
            .ok_or("the installed command path is not UTF-8")?;
        if !executable.is_absolute() || text.contains('\0') {
            return Err("the installed command must be an absolute path without NUL".into());
        }
        if text.len() > MAX_BYTES {
            return Err("the installed command path is too large".into());
        }
        Ok(Self { executable })
    }

    /// The declared stable path. Resolving it is each consumer's fresh filesystem read.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Decode the bounded declaration, without quoting any of its contents in an error.
    ///
    /// # Errors
    /// The record is oversized, malformed, of another schema or has no absolute path.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("the installed command declaration is too large".into());
        }
        let value: aterm_json::Value = aterm_json::from_slice(bytes)
            .map_err(|_| "the installed command declaration is malformed")?;
        let object = value
            .as_object()
            .filter(|object| object.len() == 2)
            .ok_or("the installed command declaration has unexpected fields")?;
        if object.get("schema").and_then(aterm_json::Value::as_u64) != Some(1) {
            return Err("the installed command declaration has an unsupported schema".into());
        }
        let executable = object
            .get("executable")
            .and_then(aterm_json::Value::as_str)
            .ok_or("the installed command declaration has no executable path")?;
        Self::new(PathBuf::from(executable))
    }

    /// Encode the declaration. It carries no file identity that an update would invalidate.
    ///
    /// # Errors
    /// Serialization fails or the escaped record exceeds the byte bound.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let executable = self
            .executable
            .to_str()
            .ok_or("the installed command path is not UTF-8")?;
        let bytes = aterm_json::to_vec(&aterm_json::json!({
            "schema": 1,
            "executable": executable,
        }))
        .map_err(|_| "the installed command declaration could not be encoded")?;
        if bytes.len() > MAX_BYTES {
            return Err("the installed command declaration is too large".into());
        }
        Ok(bytes)
    }
}

#[cfg(unix)]
mod native {
    use super::{MAX_BYTES, Selection, path};
    use aterm_uds::file_lock::LockFile;
    use std::io::{self, Read as _};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const LOCK: &str = ".installed-command.lock";
    const BUSY: &str = "another installation is recording its command location";

    fn invalid(why: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, why.into())
    }

    fn directory(dir: &Path, uid: u32) -> io::Result<()> {
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the command declaration directory must be owned and not writable by others",
            ));
        }
        Ok(())
    }

    fn read(declaration: &Path, uid: u32) -> io::Result<Option<Vec<u8>>> {
        let (file, meta) = match aterm_uds::regular_file::open(declaration) {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if meta.uid() != uid || meta.mode() & 0o6022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the command declaration must be owned and not writable by others",
            ));
        }
        if meta.len() > MAX_BYTES as u64 {
            return Err(invalid("the installed command declaration is too large"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("the installed command declaration is too large"));
        }
        Ok(Some(bytes))
    }

    fn same_executable(command: &Path, running: &Path) -> io::Result<()> {
        let placed = std::fs::metadata(command)?;
        let own = std::fs::metadata(running)?;
        if !placed.is_file()
            || placed.mode() & 0o111 == 0
            || placed.dev() != own.dev()
            || placed.ino() != own.ino()
        {
            return Err(invalid("the placed command is not this executable"));
        }
        Ok(())
    }

    fn unchanged(declaration: &Path, before: &Option<Vec<u8>>, uid: u32) -> io::Result<()> {
        if read(declaration, uid)? == *before {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the installed command selection changed during this operation",
            ))
        }
    }

    fn lock(dir: &Path, uid: u32) -> io::Result<LockFile> {
        let path = dir.join(LOCK);
        let admit = |missing| match aterm_uds::regular_file::open(&path) {
            Ok((_, meta)) if meta.uid() == uid && meta.mode() & 0o6077 == 0 => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the installed command lock must be private and owned",
            )),
            Err(error) if missing && error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        let own = uid == aterm_uds::peer::our_uid();
        admit(own)?;
        let until = Instant::now() + Duration::from_secs(60);
        let held = if own {
            LockFile::take(&path, until, BUSY)?
        } else {
            // Root clearing another account's declaration uses the lock its producer made.
            // It never creates an artifact that would then belong to root in that account.
            LockFile::take_existing(&path, until, BUSY)?
        };
        admit(false)?;
        Ok(held)
    }

    /// Record `command` only from an explicit successful placement of the same executable as
    /// `running`. This writes no PATH link, service, credential or automatic startup choice.
    /// Concurrent publishers share a lock and retain the record read before waiting for it.
    ///
    /// # Errors
    /// The command is not absolute or not this executable, the account directory or existing
    /// record is unsafe, another selection won meanwhile, or the bounded lock/write fails.
    pub fn record(home: &Path, command: &Path, running: &Path) -> io::Result<()> {
        let selected = Selection::new(command.to_path_buf()).map_err(invalid)?;
        same_executable(command, running)?;
        let uid = aterm_uds::peer::our_uid();
        let declaration = path(home);
        let dir = declaration
            .parent()
            .ok_or_else(|| invalid("no account directory"))?;
        crate::fs_restricted::create_dir_all_unshared(dir, 0o700)?;
        directory(dir, uid)?;
        let before = read(&declaration, uid)?;
        let _held = lock(dir, uid)?;
        directory(dir, uid)?;
        unchanged(&declaration, &before, uid)?;
        let bytes = selected.encode().map_err(invalid)?;
        if before.as_deref() == Some(bytes.as_slice()) {
            return same_executable(command, running);
        }
        aterm_uds::atomic_file::write_checked(&declaration, &bytes, 0o600, || {
            directory(dir, uid)?;
            unchanged(&declaration, &before, uid)?;
            same_executable(command, running)
        })
    }

    /// The selection recorded under `home`, admitted as owned by `uid` and written by no one
    /// else; `None` where no installer recorded one. It names a location only.
    ///
    /// # Errors
    /// The record is unsafe, oversized, malformed or unreadable.
    pub fn selected(home: &Path, uid: u32) -> io::Result<Option<Selection>> {
        read(&path(home), uid)?
            .map(|bytes| Selection::decode(&bytes).map_err(invalid))
            .transpose()
    }

    fn absent(command: &Path) -> io::Result<bool> {
        match std::fs::symlink_metadata(command) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Clear the still-matching declaration after an uninstall removed `command`. The caller
    /// must establish that its owned runtime was actually removed and supply the account's
    /// verified `uid`. Only that account or root may clear it; root uses its existing lock.
    /// A kept or dangling file, a newer selection and another installation remain untouched.
    ///
    /// # Errors
    /// The caller cannot act for `uid`, the existing record, directory or lock is unsafe, or
    /// its read, bounded lock or removal fails.
    pub fn remove_if_matches(home: &Path, command: &Path, uid: u32) -> io::Result<bool> {
        let caller = aterm_uds::peer::our_uid();
        if caller != uid && caller != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this account cannot clear another account's command declaration",
            ));
        }
        let declaration = path(home);
        let dir = declaration
            .parent()
            .ok_or_else(|| invalid("no account directory"))?;
        let before = read(&declaration, uid)?;
        let Some(bytes) = before.as_deref() else {
            return Ok(false);
        };
        let selected = Selection::decode(bytes).map_err(invalid)?;
        if selected.executable() != command || !absent(command)? {
            return Ok(false);
        }
        directory(dir, uid)?;
        let _held = lock(dir, uid)?;
        directory(dir, uid)?;
        if read(&declaration, uid)? != before || !absent(command)? {
            return Ok(false);
        }
        std::fs::remove_file(&declaration)?;
        let _ = aterm_durable::Flush::Durable.dir_best_effort(dir)?;
        Ok(true)
    }
}

#[cfg(unix)]
pub use native::{record, remove_if_matches, selected};

#[cfg(all(test, unix))]
#[path = "installed_command_tests.rs"]
mod tests;
