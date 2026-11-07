// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel exclusion for supported native Codex source work. An exclusive file
//! lock alone is not proof of consumer registration or publication authority.

use aterm_uds::private_file;
use std::fs::File;
use std::path::{Path, PathBuf};

const UNAVAILABLE: &str = "Codex source custody is unavailable";
const BUSY: &str = "Codex source custody is busy";

struct Held {
    file: File,
    path: PathBuf,
}

fn private_directory(path: &Path) -> Result<(), &'static str> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match private_file::create_private_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(UNAVAILABLE),
            }
        }
        Err(_) => return Err(UNAVAILABLE),
        Ok(_) => {}
    }
    private_file::check_private_dir(path).map_err(|_| UNAVAILABLE)
}

fn opened(home: &Path) -> Result<Held, &'static str> {
    if !home.is_absolute() {
        return Err(UNAVAILABLE);
    }
    let home = home.canonicalize().map_err(|_| UNAVAILABLE)?;
    let profile = home.join(".aterm");
    private_directory(&profile)?;
    let directory = profile.join("codex-source-custody");
    private_directory(&directory)?;
    open_gate(&directory)
}

fn open_gate(directory: &Path) -> Result<Held, &'static str> {
    let file = match private_file::open_private_rw(directory, "gate") {
        Ok(file) => file,
        Err(private_file::PrivateFileError::Missing) => {
            private_file::write_private_new(
                directory,
                "gate",
                b"aterm codex source gate v1\n",
                |_| Ok(()),
            )
            .map_err(|_| UNAVAILABLE)?;
            private_file::open_private_rw(directory, "gate").map_err(|_| UNAVAILABLE)?
        }
        Err(_) => return Err(UNAVAILABLE),
    };
    let path = directory.join("gate");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata().map_err(|_| UNAVAILABLE)?.nlink() != 1 {
            return Err(UNAVAILABLE);
        }
    }
    let held = Held { file, path };
    held.current()?;
    Ok(held)
}

impl Held {
    fn current(&self) -> Result<(), &'static str> {
        if !astream_sys::file_matches_path(&self.file, &self.path).map_err(|_| UNAVAILABLE)? {
            return Err(UNAVAILABLE);
        }
        let parent = self.path.parent().ok_or(UNAVAILABLE)?;
        private_file::check_private_dir(parent).map_err(|_| UNAVAILABLE)?;
        let checked = private_file::open_private_rw(parent, "gate").map_err(|_| UNAVAILABLE)?;
        if !astream_sys::file_matches_path(&checked, &self.path).map_err(|_| UNAVAILABLE)? {
            return Err(UNAVAILABLE);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if checked.metadata().map_err(|_| UNAVAILABLE)?.nlink() != 1 {
                return Err(UNAVAILABLE);
            }
        }
        Ok(())
    }
}

/// Metadata of the actual preload gate, not a lock or publication authority.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceGateIdentity {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl std::fmt::Debug for SourceGateIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceGateIdentity(redacted)")
    }
}
impl Held {
    fn identity(&self) -> Result<SourceGateIdentity, &'static str> {
        self.current()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = self.file.metadata().map_err(|_| UNAVAILABLE)?;
            Ok(SourceGateIdentity {
                path: self.path.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Err(UNAVAILABLE)
        }
    }
}
impl SourceGateIdentity {
    fn shared(&self) -> Result<SourceWorkLease, &'static str> {
        let directory = self.path.parent().ok_or(UNAVAILABLE)?;
        private_file::check_private_dir(directory).map_err(|_| UNAVAILABLE)?;
        // An identity is a reference to an existing gate, never permission to
        // recreate a missing/replaced namespace during a read acquisition.
        let held = Held {
            file: private_file::open_private_rw(directory, "gate").map_err(|_| UNAVAILABLE)?,
            path: self.path.clone(),
        };
        if held.identity()? != *self {
            return Err(UNAVAILABLE);
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "retain the authenticated preload's global shared gate"
        )]
        held.file.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        if held.identity()? != *self {
            return Err(UNAVAILABLE);
        }
        Ok(SourceWorkLease(held))
    }
}

/// Shared native source work. Keep it through child retirement, or transfer its
/// lifetime to a real source-ready registration; typing a command is not ready.
pub struct SourceWorkLease(Held);

impl std::fmt::Debug for SourceWorkLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceWorkLease(redacted)")
    }
}

impl SourceWorkLease {
    /// Acquire without waiting. This is source exclusion, not permission to
    /// execute a program, read credentials, or change a native assignment.
    pub fn for_home(home: &Path) -> Result<Self, &'static str> {
        let held = opened(home)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this source gate owns the kernel shared claim"
        )]
        held.file.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        held.current()?;
        Ok(Self(held))
    }

    pub fn identity(&self) -> Result<SourceGateIdentity, &'static str> {
        self.0.identity()
    }

    /// Recheck the original opened gate before source work starts or advances.
    pub fn current(&self) -> Result<(), &'static str> {
        self.0.current()
    }
}

/// Exclusive exclusion only. This deliberately cannot be used as a fresh
/// activation lease: live registered source and manager proof is still required.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only tests reach this native custody step until its production caller lands; drop this then"
    )
)]
pub(crate) struct PublicationLock(Held);

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only tests reach this native custody step until its production caller lands; drop this then"
    )
)]
impl PublicationLock {
    pub(crate) fn for_home(home: &Path) -> Result<Self, &'static str> {
        let held = opened(home)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this source gate owns the kernel exclusive claim"
        )]
        held.file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => BUSY,
            std::fs::TryLockError::Error(_) => UNAVAILABLE,
        })?;
        held.current()?;
        Ok(Self(held))
    }

    pub(crate) fn current(&self) -> Result<(), &'static str> {
        self.0.current()
    }
}

#[path = "codex_source_gate_family.rs"]
mod family;
pub use family::{FamilyExclusive, FamilySource, FamilyWork, SourceBorrow};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_excludes_real_live_source_work_and_replaced_gate_is_refused() {
        let home = aterm_tempfile::tempdir().unwrap();
        let first = SourceWorkLease::for_home(home.path()).unwrap();
        let second = SourceWorkLease::for_home(home.path()).unwrap();
        assert!(PublicationLock::for_home(home.path()).is_err());
        drop(first);
        assert!(PublicationLock::for_home(home.path()).is_err());
        drop(second);
        let publication = PublicationLock::for_home(home.path()).unwrap();
        assert!(SourceWorkLease::for_home(home.path()).is_err());
        publication.current().unwrap();
        let path = publication.0.path.clone();
        std::fs::rename(&path, path.with_extension("held")).unwrap();
        private_file::write_private_new(path.parent().unwrap(), "gate", b"replacement", |_| Ok(()))
            .unwrap();
        assert!(publication.current().is_err());
    }
}
