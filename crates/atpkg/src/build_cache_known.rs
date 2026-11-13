// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only recognition of one workspace's shared Cargo cache for disk measurement.

use std::path::{Path, PathBuf};

/// A caller-selected directory in the exact shared-cache namespace for this workspace.
/// Recognition grants no registration, deletion or compiler-freshness authority. The held
/// bindings are checked when its path is requested; a later path-based meter is not a
/// continuous descriptor-relative filesystem proof.
#[derive(Debug)]
pub struct KnownCargoCache {
    path: PathBuf,
    #[cfg(unix)]
    bindings: Vec<(PathBuf, aterm_dirfd::OwnedFd, super::Identity)>,
    #[cfg(unix)]
    missing: Option<PathBuf>,
}

impl KnownCargoCache {
    /// Recognize only `build_dir(cache_root(home), workspace)`, on the target's volume.
    /// An unrelated caller path is not inspected. A missing cache, or an empty private
    /// directory in that exact namespace, is measurable without pretending Cargo has built
    /// there. A nonempty existing directory needs Cargo's exact tag, no source-root
    /// declarations, and a regular compiler-info file without a symlink where that name
    /// exists; its contents prove no particular compiler or unit fresh. No directories or
    /// receipts are created.
    ///
    /// # Errors
    /// A shared-cache choice names another workspace, its binding or contents are unsafe,
    /// or its volume differs from the target's. Non-Unix hosts recognize no shared cache.
    pub fn observe(
        home: &Path,
        workspace: &Path,
        selected: &Path,
        target: &Path,
    ) -> std::io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            Self::observe_unix(home, workspace, selected, target, &target_volume)
        }
        #[cfg(not(unix))]
        {
            let _ = (home, workspace, selected, target);
            Ok(None)
        }
    }

    /// Return the recognized path only while all observed directory bindings still hold.
    ///
    /// # Errors
    /// A directory was replaced, linked, made non-private, or appeared at the first name
    /// observed missing. The next observation can classify a newly created Cargo cache.
    pub fn path(&self) -> std::io::Result<&Path> {
        #[cfg(unix)]
        {
            self.bindings_intact()?;
            if let Some(missing) = &self.missing
                && !std::fs::symlink_metadata(missing)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                return Err(refused());
            }
        }
        Ok(&self.path)
    }

    /// Require this observation to continue the same namespace and every directory the
    /// previous observation held. A cache absent before compilation may gain new descendants
    /// below its unchanged anchor; a directory that already stood may never be replaced.
    ///
    /// # Errors
    /// Either binding is no longer valid, or the observations name different cache paths.
    pub fn continue_from(&self, previous: &Self) -> std::io::Result<()> {
        if self.path != previous.path {
            return Err(std::io::Error::other(
                "the selected shared Cargo cache changed",
            ));
        }
        #[cfg(unix)]
        previous.bindings_intact()?;
        self.path()?;
        Ok(())
    }

    #[cfg(unix)]
    fn bindings_intact(&self) -> std::io::Result<()> {
        for (path, held, identity) in &self.bindings {
            let current = super::creation::pin(path).ok_or_else(refused)?;
            if !super::retention::private_directory(held)
                || !super::Identity::held(held).is_some_and(|now| identity.names(&now))
                || !super::Identity::held(&current).is_some_and(|now| identity.names(&now))
            {
                return Err(refused());
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn observe_unix(
        home: &Path,
        workspace: &Path,
        selected: &Path,
        target: &Path,
        volume_of: &dyn Fn(&Path) -> Option<super::Identity>,
    ) -> std::io::Result<Option<Self>> {
        use aterm_toolchain::build_cache::{build_dir, cache_root};
        let root = cache_root(home);
        if !selected.starts_with(&root) {
            return Ok(None);
        }
        if !home.is_absolute()
            || !workspace.is_absolute()
            || [home, workspace, selected, target].iter().any(|path| {
                path.components()
                    .any(|part| part == std::path::Component::ParentDir)
            })
            || selected != build_dir(&root, workspace)
        {
            return Err(refused());
        }
        let relative = selected.strip_prefix(home).map_err(|_| refused())?;
        if relative.components().count() > 64 {
            return Err(refused());
        }
        let mut path = home.to_path_buf();
        let home_fd = super::creation::pin(home).ok_or_else(refused)?;
        let home_identity = super::Identity::held(&home_fd).ok_or_else(refused)?;
        let mut bindings = vec![(path.clone(), home_fd, home_identity)];
        let mut missing = None;
        for part in relative.components() {
            let std::path::Component::Normal(part) = part else {
                return Err(refused());
            };
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing = Some(path.clone());
                    break;
                }
                Err(_) => return Err(refused()),
                Ok(_) => {}
            }
            let held = super::creation::pin(&path).ok_or_else(refused)?;
            let identity = super::Identity::held(&held).ok_or_else(refused)?;
            bindings.push((path.clone(), held, identity));
        }
        let (_, held, cache_identity) = bindings.last().ok_or_else(refused)?;
        if missing.is_none() && !empty(held)? {
            if !super::adoption::source_free(held) || !super::adoption::cargo_tag(held) {
                return Err(refused());
            }
            // This is only a regular-file observation, not compiler/fingerprint proof.
            if !super::adoption::compiler_info(held)
                && !aterm_dirfd::fstatat(
                    held,
                    ".rustc_info.json",
                    aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
                )
                .is_err_and(|error| error.is_noent())
            {
                return Err(refused());
            }
        }
        let target_identity = volume_of(target).ok_or_else(refused)?;
        if !cache_identity.same_volume(&target_identity) {
            return Err(std::io::Error::other(
                "the selected shared Cargo cache is on a different volume from the target",
            ));
        }
        let known = Self {
            path: selected.to_path_buf(),
            bindings,
            missing,
        };
        known.path()?;
        Ok(Some(known))
    }
}

#[cfg(unix)]
fn empty(root: &aterm_dirfd::OwnedFd) -> std::io::Result<bool> {
    let entries = aterm_dirfd::Dir::read_from(root).map_err(std::io::Error::from)?;
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn target_volume(target: &Path) -> Option<super::Identity> {
    if !target.is_absolute() {
        return None;
    }
    for parent in target.ancestors() {
        match std::fs::symlink_metadata(parent) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
            Ok(metadata) if !metadata.is_dir() => return None,
            Ok(_) => {}
        }
        let now = aterm_uds::directory_identity::read(parent).ok()?;
        return Some(super::Identity {
            device: now.dev,
            inode: now.ino,
            volume: now.volume,
        });
    }
    None
}

#[cfg(unix)]
fn refused() -> std::io::Error {
    std::io::Error::other("the selected shared Cargo cache could not be bound to this workspace")
}

#[cfg(all(test, unix))]
#[path = "build_cache_known_tests.rs"]
mod tests;
