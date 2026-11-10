// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One-time macOS handoffs of owned state. Atomic exchanges preserve inodes and leave
//! type fences at retired names, never second writable stores. Directory handoffs retain
//! the legacy writer lease; non-authoritative file caches fence older atomic publishers.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const RETIRED: &[u8] = b"aterm state moved atomically into .ubd; do not replace this fence\n";

/// The macOS quiet destination, retaining Spotlight's `.noindex` suffix. Other platforms
/// keep their original directory and do not migrate.
#[must_use]
pub fn destination(legacy: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let mut name = legacy.file_name().unwrap_or_default().to_os_string();
        name.push(".noindex");
        legacy
            .parent()
            .unwrap_or(Path::new("."))
            .join(".ubd")
            .join(name)
    }
    #[cfg(not(target_os = "macos"))]
    legacy.to_path_buf()
}

/// Read-only selection during the one-time handoff. Before retirement all callers use
/// the legacy directory; afterward all callers use the quiet directory, even if damaged.
/// There is no fallback to a retired store and no second writable copy.
#[must_use]
pub fn selected(legacy: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    if mac::read_marker(legacy).is_ok() {
        return destination(legacy);
    }
    legacy.to_path_buf()
}

/// Move an owned, non-authoritative file cache into `.ubd`, preserving the file's inode.
/// The directory fence and file exchange atomically: an old writer either publishes before
/// the exchange (its latest file moves), or its rename over the directory is refused.
/// No legacy writer lease or second writable store is needed. Returns its new path;
/// other platforms return its original path. A retired cache never falls back.
///
/// # Errors
/// On macOS, refuses unsafe/private state, files exceeding `maximum`, ambiguous stores,
/// a busy migration, unsupported atomic exchanges and filesystem/durability failures.
pub fn prepare_file_cache(legacy: &Path, maximum: usize) -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return mac::prepare_file_cache(legacy, maximum).map(|()| destination(legacy));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (legacy, maximum);
        Ok(legacy.to_path_buf())
    }
}

/// Prepare the quiet directory without losing retained data. `acquire` must exclude the
/// legacy directory's durable writers, and its returned guard must retain that exclusion
/// until dropped. The callback is never called for a fresh or already migrated directory.
/// A competing migration is refused immediately; callers can retry their normal startup.
///
/// # Errors
/// Refuses symlinks, foreign or non-private state, ambiguous stores, a busy writer lease,
/// unsupported atomic exchanges, and filesystem/durability failures. No fallback store is
/// created. A crash before exchange leaves the original directory and a marker at the new
/// name; one after exchange leaves the migrated directory and a fence at the old name.
pub fn prepare<G>(legacy: &Path, acquire: impl FnOnce() -> io::Result<G>) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return mac::prepare(legacy, acquire);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (legacy, acquire);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use crate::private_file;
    use std::io::{Read as _, Write as _};
    use std::os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
    };

    const CACHE_MARKER: &str = "retired-cache";
    const CACHE_RETIRED: &[u8] = b"aterm file cache moved atomically into .ubd\n";

    fn name(legacy: &Path) -> io::Result<&str> {
        legacy
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 80
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
            .ok_or_else(|| io::Error::other("invalid state name"))
    }

    fn read_directory_marker(path: &Path) -> io::Result<()> {
        private_file::check_private_dir(path)?;
        let bytes = private_file::read_private(path, CACHE_MARKER, CACHE_RETIRED.len())
            .map_err(io::Error::other)?;
        let entries = std::fs::read_dir(path)?
            .take(2)
            .collect::<io::Result<Vec<_>>>()?;
        if bytes != CACHE_RETIRED || entries.len() != 1 || entries[0].file_name() != CACHE_MARKER {
            return Err(io::Error::other("file cache retirement fence differs"));
        }
        Ok(())
    }

    fn directory_marker(path: &Path) -> io::Result<()> {
        private_file::create_private_dir(path)?;
        // An empty stage is the crash state before its marker was written. Never adopt
        // a populated directory without validating its existing marker.
        if std::fs::read_dir(path)?.next().transpose()?.is_none() {
            private_file::write_private_flushed(
                path,
                CACHE_MARKER,
                CACHE_RETIRED,
                aterm_durable::Flush::Durable,
            )?;
        }
        read_directory_marker(path)
    }

    fn bounded_file(path: &Path, maximum: usize) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("cache needs a parent"))?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| io::Error::other("cache needs a filename"))?;
        let (_, metadata) = private_file::open_private(parent, name).map_err(io::Error::other)?;
        if metadata.len() > maximum as u64 {
            return Err(io::Error::other("file cache exceeds its bound"));
        }
        Ok(())
    }

    pub(super) fn prepare_file_cache(legacy: &Path, maximum: usize) -> io::Result<()> {
        let name = name(legacy)?;
        let target = destination(legacy);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("cache needs a parent"))?;
        private_file::create_private_dir(parent)?;
        let _migration = private_file::PrivateLock::take(
            parent,
            &format!("{name}.migration.lock"),
            std::time::Duration::ZERO,
            "cache migration is running",
        )?;
        let old = kind(legacy)?;
        let new = kind(&target)?;
        if old.is_some_and(|k| k.is_symlink()) || new.is_some_and(|k| k.is_symlink()) {
            return Err(io::Error::other("file cache migration refuses symlinks"));
        }
        if old.is_some_and(|k| k.is_dir()) {
            read_directory_marker(legacy)?;
            if new.is_some() {
                bounded_file(&target, maximum)?;
            }
            return Ok(());
        }
        if old.is_none() {
            if new.is_some() {
                return Err(io::Error::other("file cache has an unfenced destination"));
            }
            directory_marker(legacy)?;
            aterm_durable::Flush::Durable.dir(
                legacy
                    .parent()
                    .ok_or_else(|| io::Error::other("cache needs a parent"))?,
            )?;
            return Ok(());
        }
        if !old.is_some_and(|k| k.is_file()) || new.is_some_and(|k| !k.is_dir()) {
            return Err(io::Error::other(
                "file cache migration found ambiguous stores",
            ));
        }
        bounded_file(legacy, maximum)?;
        directory_marker(&target)?;
        swap_entries(legacy, &target)?;
        // A cooperating old atomic writer may have published a newer inode just before
        // the exchange. Judge that actual winner; never follow a substituted link.
        bounded_file(&target, maximum)
    }

    fn kind(path: &Path) -> io::Result<Option<std::fs::FileType>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata.file_type())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn marker(path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("state needs a parent"))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(crate::atomic_file::LOCK_OPEN_FLAGS)
            .open(path)
        {
            Ok(mut file) => {
                file.write_all(RETIRED)?;
                aterm_durable::Flush::Durable.file(&file)?;
                aterm_durable::Flush::Durable.dir(parent)?;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read_marker(path),
            Err(error) => Err(error),
        }
    }

    pub(super) fn read_marker(path: &Path) -> io::Result<()> {
        let (file, metadata) = crate::regular_file::open(path)?;
        if metadata.uid() != crate::peer::our_uid()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::other("state retirement fence is not private"));
        }
        let mut bytes = Vec::new();
        file.take((RETIRED.len() + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes == RETIRED {
            Ok(())
        } else {
            Err(io::Error::other("state retirement fence differs"))
        }
    }

    pub(super) fn prepare<G>(
        legacy: &Path,
        acquire: impl FnOnce() -> io::Result<G>,
    ) -> io::Result<()> {
        let name = name(legacy)?;
        let target = destination(legacy);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("state needs a parent"))?;
        private_file::create_private_dir(parent)?;
        let _migration = private_file::PrivateLock::take(
            parent,
            &format!("{name}.migration.lock"),
            std::time::Duration::ZERO,
            "state migration is already running",
        )?;
        let old = kind(legacy)?;
        let new = kind(&target)?;
        if old.is_some_and(|k| k.is_symlink()) || new.is_some_and(|k| k.is_symlink()) {
            return Err(io::Error::other("state migration refuses symlinks"));
        }
        if old.is_none() || old.is_some_and(|k| k.is_file()) {
            if new.is_some_and(|k| !k.is_dir()) {
                return Err(io::Error::other("quiet state is not a directory"));
            }
            marker(legacy)?;
            private_file::create_private_dir(&target)?;
            return Ok(());
        }
        if !old.is_some_and(|k| k.is_dir()) || new.is_some_and(|k| k.is_dir()) {
            return Err(io::Error::other(
                "state migration found ambiguous directories",
            ));
        }
        private_file::check_private_dir(legacy)?;
        let before = std::fs::symlink_metadata(legacy)?;
        let _legacy_writer = acquire()?;
        let after = std::fs::symlink_metadata(legacy)?;
        if before.dev() != after.dev() || before.ino() != after.ino() || !after.is_dir() {
            return Err(io::Error::other(
                "state directory changed while taking its writer lease",
            ));
        }
        marker(&target)?;
        swap_entries(legacy, &target)
    }

    fn swap_entries(legacy: &Path, target: &Path) -> io::Result<()> {
        let from = std::ffi::CString::new(legacy.as_os_str().as_bytes())?;
        let to = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        // SAFETY: both C strings outlive this call. RENAME_SWAP exchanges both entries in
        // one filesystem transaction, preserving each inode and every held inode lock.
        if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        aterm_durable::Flush::Durable.dir(
            target
                .parent()
                .ok_or_else(|| io::Error::other("state needs a parent"))?,
        )?;
        aterm_durable::Flush::Durable.dir(
            legacy
                .parent()
                .ok_or_else(|| io::Error::other("state needs a parent"))?,
        )?;
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[test]
    fn file_cache_exchange_preserves_the_inode_and_refuses_old_atomic_writers() {
        let scratch = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
        let old = scratch.path().join("peers.state");
        crate::private_file::write_private(scratch.path(), "peers.state", b"verified hints")
            .unwrap();
        let inode = std::fs::metadata(&old).unwrap().ino();
        let pending = scratch.path().join("old-writer-pending");
        std::fs::write(&pending, b"old publication").unwrap();
        let quiet = prepare_file_cache(&old, 1024).unwrap();
        assert!(aterm_tempfile::is_quiet(&quiet));
        assert_eq!(std::fs::metadata(&quiet).unwrap().ino(), inode);
        assert_eq!(std::fs::read(&quiet).unwrap(), b"verified hints");
        assert!(old.is_dir());
        assert!(
            std::fs::rename(&pending, &old).is_err(),
            "an old rename cannot fork the cache"
        );
        assert!(crate::private_file::write_private(scratch.path(), "peers.state", b"old").is_err());
        assert_eq!(prepare_file_cache(&old, 1024).unwrap(), quiet);
        let fresh = scratch.path().join("fresh.state");
        let target = prepare_file_cache(&fresh, 1024).unwrap();
        assert!(fresh.is_dir());
        assert!(!target.exists());
        crate::private_file::write_private(
            target.parent().unwrap(),
            target.file_name().unwrap().to_str().unwrap(),
            b"new hints",
        )
        .unwrap();
        assert_eq!(prepare_file_cache(&fresh, 1024).unwrap(), target);
    }

    #[test]
    fn staged_file_handoff_resumes_latest_publication_and_preserves_unsafe_stores() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let old = scratch.path().join("peers.state");
        crate::private_file::write_private(scratch.path(), "peers.state", b"before").unwrap();
        // A public-protocol fence reproduces the stage before an exchange. An old
        // writer may still publish while that migration is crashed.
        let donor = scratch.path().join("donor.state");
        prepare_file_cache(&donor, 1024).unwrap();
        std::fs::rename(&donor, destination(&old)).unwrap();
        crate::private_file::write_private(scratch.path(), "peers.state", b"latest").unwrap();
        let inode = std::fs::metadata(&old).unwrap().ino();
        let quiet = prepare_file_cache(&old, 1024).unwrap();
        assert_eq!(std::fs::read(&quiet).unwrap(), b"latest");
        assert_eq!(std::fs::metadata(&quiet).unwrap().ino(), inode);
        let ambiguous = scratch.path().join("ambiguous.state");
        crate::private_file::write_private(scratch.path(), "ambiguous.state", b"original").unwrap();
        crate::private_file::write_private(
            destination(&ambiguous).parent().unwrap(),
            "ambiguous.state.noindex",
            b"other",
        )
        .unwrap();
        assert!(prepare_file_cache(&ambiguous, 1024).is_err());
        assert_eq!(std::fs::read(&ambiguous).unwrap(), b"original");
        assert_eq!(std::fs::read(destination(&ambiguous)).unwrap(), b"other");
        let oversized = scratch.path().join("oversized.state");
        crate::private_file::write_private(scratch.path(), "oversized.state", b"too large")
            .unwrap();
        assert!(prepare_file_cache(&oversized, 1).is_err());
        assert!(oversized.is_file());
        assert!(!destination(&oversized).exists());
        let linked = scratch.path().join("linked.state");
        std::os::unix::fs::symlink(&oversized, &linked).unwrap();
        assert!(prepare_file_cache(&linked, 1024).is_err());
        assert!(oversized.is_file());
    }

    #[test]
    fn migration_preserves_inodes_and_locks_and_fences_legacy_writers() {
        let scratch = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
        let old = scratch.path().join("history");
        crate::private_file::create_private_dir(&old).unwrap();
        std::fs::write(old.join("retained"), b"acknowledged history").unwrap();
        std::fs::set_permissions(old.join("retained"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let inode = std::fs::metadata(old.join("retained")).unwrap().ino();
        let lease = crate::private_file::PrivateLock::take(
            &old,
            "writer.lock",
            std::time::Duration::ZERO,
            "held",
        )
        .unwrap();
        assert!(
            prepare(&old, || crate::private_file::PrivateLock::take(
                &old,
                "writer.lock",
                std::time::Duration::ZERO,
                "held"
            ))
            .is_err()
        );
        assert_eq!(
            std::fs::read(old.join("retained")).unwrap(),
            b"acknowledged history"
        );
        drop(lease);
        prepare(&old, || {
            crate::private_file::PrivateLock::take(
                &old,
                "writer.lock",
                std::time::Duration::ZERO,
                "held",
            )
        })
        .unwrap();
        let new = destination(&old);
        assert!(aterm_tempfile::is_quiet(&new));
        assert_eq!(
            std::fs::metadata(new.join("retained")).unwrap().ino(),
            inode
        );
        assert_eq!(
            std::fs::read(new.join("retained")).unwrap(),
            b"acknowledged history"
        );
        assert!(old.is_file());
        assert!(
            std::fs::create_dir_all(&old).is_err(),
            "an older writer cannot recreate a parallel store"
        );
        prepare(&old, || -> io::Result<()> {
            panic!("completed migrations take no old lease")
        })
        .unwrap();
    }

    #[test]
    fn fresh_state_is_fenced_and_ambiguous_or_symlinked_stores_are_preserved() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let old = scratch.path().join("fresh");
        prepare(&old, || -> io::Result<()> { panic!("no old writer") }).unwrap();
        assert!(destination(&old).is_dir());
        let old = scratch.path().join("ambiguous");
        crate::private_file::create_private_dir(&old).unwrap();
        crate::private_file::create_private_dir(&destination(&old)).unwrap();
        assert!(prepare(&old, || Ok(())).is_err());
        assert!(old.is_dir());
        let link = scratch.path().join("linked");
        std::os::unix::fs::symlink(&old, &link).unwrap();
        assert!(prepare(&link, || Ok(())).is_err());
        assert!(old.is_dir());
    }

    #[test]
    fn interrupted_exchange_resumes_and_a_replaced_source_is_refused() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let old = scratch.path().join("history");
        crate::private_file::create_private_dir(&old).unwrap();
        crate::private_file::create_private_dir(destination(&old).parent().unwrap()).unwrap();
        // Crash state immediately before RENAME_SWAP: marker staged, history untouched.
        std::fs::write(destination(&old), RETIRED).unwrap();
        std::fs::set_permissions(destination(&old), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::write(old.join("retained"), b"retained").unwrap();
        assert_eq!(selected(&old), old);
        prepare(&old, || Ok(())).unwrap();
        assert_eq!(selected(&old), destination(&old));
        assert_eq!(
            std::fs::read(destination(&old).join("retained")).unwrap(),
            b"retained"
        );

        let old = scratch.path().join("replaced");
        let saved = scratch.path().join("saved");
        crate::private_file::create_private_dir(&old).unwrap();
        assert!(
            prepare(&old, || {
                std::fs::rename(&old, &saved)?;
                crate::private_file::create_private_dir(&old)
            })
            .is_err()
        );
        assert!(saved.is_dir());
        assert!(old.is_dir());
        assert!(!destination(&old).exists());
    }
}
