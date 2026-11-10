// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel exclusion for supported native Codex source work. An exclusive file
//! lock alone is not proof of consumer registration or publication authority.

use super::native_custody_platform as platform;
#[cfg(unix)]
use aterm_uds::private_file;
use std::fs::File;
use std::path::{Path, PathBuf};

const UNAVAILABLE: &str = "Codex source custody is unavailable";
const BUSY: &str = "Codex source custody is busy";

struct Held {
    file: File,
    path: PathBuf,
}

// A captured identity may inspect or lock only its existing private inode.
// open_private_rw creates a missing file, so it belongs only to initial creation.
fn existing_gate_file(directory: &Path) -> Result<File, &'static str> {
    platform::open_private(directory, "gate")
        .map(|(file, _)| file)
        .map_err(|_| UNAVAILABLE)
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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
            existing_gate_file(directory)?
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
        platform::check_private_dir(parent).map_err(|_| UNAVAILABLE)?;
        let checked = existing_gate_file(parent)?;
        if !astream_sys::file_matches_path(&checked, &self.path).map_err(|_| UNAVAILABLE)? {
            return Err(UNAVAILABLE);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let original = self.file.metadata().map_err(|_| UNAVAILABLE)?;
            let reopened = checked.metadata().map_err(|_| UNAVAILABLE)?;
            if (original.dev(), original.ino()) != (reopened.dev(), reopened.ino())
                || original.nlink() != 1
                || reopened.nlink() != 1
            {
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
    /// Selected real home namespace of this captured gate; metadata only.
    pub(in crate::harness) fn existing_home(&self) -> Option<PathBuf> {
        if !self.current() {
            return None;
        }
        self.path
            .parent()?
            .parent()?
            .parent()
            .map(Path::to_path_buf)
    }

    /// Compare the existing native namespace without creating a gate or taking
    /// another lock. This metadata join grants no source or lock authority.
    pub(super) fn matches_home(&self, home: &Path) -> bool {
        existing_identity_for_home(home).is_ok_and(|identity| identity == *self)
    }

    /// Recheck only the actual captured private gate identity. This neither
    /// creates a namespace nor acquires a source lock or credential authority.
    pub(super) fn current(&self) -> bool {
        let existing = || -> Result<SourceGateIdentity, &'static str> {
            let directory = self.path.parent().ok_or(UNAVAILABLE)?;
            platform::check_private_dir(directory).map_err(|_| UNAVAILABLE)?;
            let held = Held {
                file: existing_gate_file(directory)?,
                path: self.path.clone(),
            };
            held.identity()
        };
        existing().is_ok_and(|identity| identity == *self)
    }

    fn shared(&self) -> Result<SourceWorkLease, &'static str> {
        let directory = self.path.parent().ok_or(UNAVAILABLE)?;
        platform::check_private_dir(directory).map_err(|_| UNAVAILABLE)?;
        // An identity is a reference to an existing gate, never permission to
        // recreate a missing/replaced namespace during a read acquisition.
        let held = Held {
            file: existing_gate_file(directory)?,
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

/// Metadata of an existing native machine-home gate. This owns no lock and
/// confers no source, credential, constructor, or publication authority.
pub(super) struct SourceNamespace(SourceGateIdentity);
impl std::fmt::Debug for SourceNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceNamespace(redacted)")
    }
}
impl SourceNamespace {
    pub(super) fn directory(&self) -> Result<&Path, &'static str> {
        if !self.0.current() {
            return Err(UNAVAILABLE);
        }
        self.0.path.parent().ok_or(UNAVAILABLE)
    }
}
/// Inspect only an already existing native-home gate. Unlike for_home this
/// neither creates the namespace nor acquires/reacquires source exclusion.
pub(super) fn existing_namespace_for_home(home: &Path) -> Result<SourceNamespace, &'static str> {
    Ok(SourceNamespace(existing_identity_for_home(home)?))
}

// One metadata-only resolver serves both namespace equality and exchange
// admission, so private-path and native gate identity rules cannot diverge.
fn existing_identity_for_home(home: &Path) -> Result<SourceGateIdentity, &'static str> {
    if !home.is_absolute() {
        return Err(UNAVAILABLE);
    }
    let home = home.canonicalize().map_err(|_| UNAVAILABLE)?;
    let profile = home.join(".aterm");
    platform::check_private_dir(&profile).map_err(|_| UNAVAILABLE)?;
    let directory = profile.join("codex-source-custody");
    platform::check_private_dir(&directory).map_err(|_| UNAVAILABLE)?;
    let held = Held {
        file: existing_gate_file(&directory)?,
        path: directory.join("gate"),
    };
    held.identity()
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
    /// Cold maintenance may join an existing gate, never recreate a lost one.
    pub(super) fn for_existing_home(home: &Path) -> Result<Self, &'static str> {
        existing_identity_for_home(home)?.shared()
    }

    /// Join this actual held gate to the selected machine profile without
    /// creating or reacquiring source custody.
    pub(super) fn matches_home(&self, home: &Path) -> bool {
        self.0
            .identity()
            .is_ok_and(|identity| identity.matches_home(home))
    }
    pub(super) fn namespace(&self) -> Result<SourceNamespace, &'static str> {
        Ok(SourceNamespace(self.0.identity()?))
    }
    /// Acquire without waiting. This is source exclusion, not permission to
    /// execute a program, read credentials, or change a native assignment.
    pub fn for_home(home: &Path) -> Result<Self, &'static str> {
        Self::try_for_home(home)?.ok_or(BUSY)
    }

    /// Only kernel contention is retryable. Invalid namespaces and I/O errors
    /// must not become an indefinite admission wait.
    pub(crate) fn try_for_home(home: &Path) -> Result<Option<Self>, &'static str> {
        let held = opened(home)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this source gate owns the kernel shared claim"
        )]
        match held.file.try_lock_shared() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(_)) => return Err(UNAVAILABLE),
        }
        held.current()?;
        Ok(Some(Self(held)))
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
pub(crate) struct PublicationLock(Held);

impl PublicationLock {
    pub(super) fn namespace(&self) -> Result<SourceNamespace, &'static str> {
        Ok(SourceNamespace(self.0.identity()?))
    }
    #[cfg(test)]
    pub(crate) fn for_home(home: &Path) -> Result<Self, &'static str> {
        Self::try_for_home(home)?.ok_or(BUSY)
    }

    /// The lock, once its last holder's release has come through ([`released`]).
    #[cfg(test)]
    pub(crate) fn released_for_home(home: &Path) -> Self {
        released("the publication lock", || Self::try_for_home(home).unwrap())
    }

    /// Only actual kernel contention is retryable; namespace and I/O failures
    /// remain errors and never become an indefinite admission wait.
    pub(crate) fn try_for_home(home: &Path) -> Result<Option<Self>, &'static str> {
        let held = opened(home)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "this source gate owns the kernel exclusive claim"
        )]
        match held.file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(_)) => return Err(UNAVAILABLE),
        }
        held.current()?;
        Ok(Some(Self(held)))
    }

    pub(crate) fn current(&self) -> Result<(), &'static str> {
        self.0.current()
    }
}

/// What `take` claims once its last holder's release has come through: a test that dropped
/// every holder of a gate lock must find it free, but an `flock` lives with its open file
/// description, and a child another test forks at that moment holds a copy of the descriptor
/// until its `exec` closes it, so the claim can read busy for a moment after the drop (more so
/// on a loaded Mac). The wait is a one-minute hang detector: a lock still held then is a leaked
/// holder, and fails naming `what`.
#[cfg(test)]
pub(crate) fn released<T>(what: &str, mut take: impl FnMut() -> Option<T>) -> T {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    let started = std::time::Instant::now();
    loop {
        if let Some(taken) = take() {
            return taken;
        }
        assert!(
            started.elapsed() < HANG_DETECTOR,
            "{what} stayed held {HANG_DETECTOR:?} after its holders dropped"
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[path = "codex_source_gate_family.rs"]
mod family;
pub use family::{FamilyExclusive, FamilySource, FamilyWork, SourceBorrow};
pub(in crate::harness) use family::{ReservationFamilyExclusive, TargetFamilyLease};

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captured_gate_probes_never_create_a_missing_inode() {
        let home = aterm_tempfile::tempdir().unwrap();
        let work = SourceWorkLease::for_home(home.path()).unwrap();
        let identity = work.identity().unwrap();
        let namespace = work.namespace().unwrap();
        let gate = identity.path.clone();
        let displaced = gate.with_extension("held-original");
        std::fs::rename(&gate, &displaced).unwrap();

        assert!(!identity.current());
        assert!(!gate.exists(), "identity inspection must not create a gate");
        assert!(!identity.matches_home(home.path()));
        assert!(
            !gate.exists(),
            "namespace comparison must not create a gate"
        );
        assert!(existing_namespace_for_home(home.path()).is_err());
        assert!(!gate.exists(), "namespace lookup must not create a gate");
        assert!(SourceWorkLease::for_existing_home(home.path()).is_err());
        assert!(
            !gate.exists(),
            "maintenance admission must not create a gate"
        );
        assert!(identity.shared().is_err());
        assert!(
            !gate.exists(),
            "captured shared acquisition must not create a gate"
        );
        assert!(work.current().is_err());
        assert!(namespace.directory().is_err());
        assert!(!gate.exists(), "held read probes must not create a gate");

        std::fs::rename(&displaced, &gate).unwrap();
        assert!(identity.current());
        let existing = SourceWorkLease::for_existing_home(home.path()).unwrap();
        drop(work);
        assert!(
            PublicationLock::try_for_home(home.path())
                .unwrap()
                .is_none()
        );
        drop(existing);
        let publication = PublicationLock::released_for_home(home.path());
        assert!(publication.current().is_ok());
    }

    #[test]
    fn publication_excludes_real_live_source_work_and_replaced_gate_is_refused() {
        let home = aterm_tempfile::tempdir().unwrap();
        let first = SourceWorkLease::for_home(home.path()).unwrap();
        let second = SourceWorkLease::for_home(home.path()).unwrap();
        assert!(PublicationLock::for_home(home.path()).is_err());
        drop(first);
        assert!(PublicationLock::for_home(home.path()).is_err());
        drop(second);
        let publication = PublicationLock::released_for_home(home.path());
        assert!(SourceWorkLease::for_home(home.path()).is_err());
        publication.current().unwrap();
        let path = publication.0.path.clone();
        std::fs::rename(&path, path.with_extension("held")).unwrap();
        private_file::write_private_new(path.parent().unwrap(), "gate", b"replacement", |_| Ok(()))
            .unwrap();
        assert!(publication.current().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn namespace_join_checks_existing_gate_and_accepts_canonical_home_alias() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        let missing = root.path().join("missing-gate");
        for directory in [&home, &other, &missing] {
            private_file::create_private_dir(directory).unwrap();
        }
        let work = SourceWorkLease::for_home(&home).unwrap();
        let identity = work.identity().unwrap();
        assert!(identity.matches_home(&home));
        assert!(identity.current());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert!(identity.matches_home(&alias));
        drop(work);
        let _publication = PublicationLock::released_for_home(&home);
        // Namespace validation performs no hidden shared acquisition, even
        // while this actual gate is exclusively held.
        assert!(identity.matches_home(&home));
        assert!(
            identity.current(),
            "captured metadata remains valid after READY released its lease; no shared reacquisition under exclusive publication"
        );
        // A metadata comparison cannot manufacture a missing namespace.
        assert!(!identity.matches_home(&missing));
        assert!(!missing.join(".aterm").exists());
        let _other = SourceWorkLease::for_home(&other).unwrap();
        assert!(!identity.matches_home(&other));
        // Even the same path is insufficient after kernel identity changes.
        let gate = identity.path.clone();
        std::fs::rename(&gate, gate.with_extension("old")).unwrap();
        private_file::write_private_new(gate.parent().unwrap(), "gate", b"replacement", |_| Ok(()))
            .unwrap();
        assert!(!identity.matches_home(&home));
        assert!(!identity.matches_home(&alias));
        assert!(
            !identity.current(),
            "a READY snapshot cannot survive physical gate replacement"
        );
    }
}

#[cfg(not(unix))]
fn opened(_: &Path) -> Result<Held, &'static str> {
    Err("native credential custody is unsupported on this platform")
}
