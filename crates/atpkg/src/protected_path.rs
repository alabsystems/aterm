// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Resolve paths without querying a target macOS could ask for consent to inspect.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};

/// Why a path was not admitted; callers keep a consent fence distinct from unreadable input.
#[derive(Debug)]
pub(crate) enum PathRefusal {
    /// The target must not be queried automatically, whether or not it exists.
    ConsentFenced,
    /// The path cannot be established because its spelling or inspection failed.
    Unreadable(String),
}

impl From<String> for PathRefusal {
    fn from(why: String) -> Self {
        Self::Unreadable(why)
    }
}

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConsentFenced => {
                f.write_str("it resolves under a folder macOS guards with a consent dialog")
            }
            Self::Unreadable(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for PathRefusal {}

/// Walk an absolute path with an admitted, canonical `home` as the consent root, checking
/// each spelling before metadata I/O. Link targets are read as text and walked in turn;
/// missing components are allowed for a later directory creation.
/// The returned metadata describes the final component when it was inspected.
///
/// # Errors
/// Refuses protected targets, relative paths, link cycles, non-directory ancestors, and
/// inspection errors other than a missing component.
pub(crate) fn resolve_path(
    home: &Path,
    path: &Path,
    mut metadata: impl FnMut(&Path) -> std::io::Result<Metadata>,
    mut read_link: impl FnMut(&Path) -> std::io::Result<PathBuf>,
) -> Result<(PathBuf, Option<Metadata>), PathRefusal> {
    if !path.is_absolute() {
        return Err("its path is not absolute".to_owned().into());
    }
    let parts = |path: &Path| {
        path.components()
            .map(|part| part.as_os_str().to_os_string())
            .collect::<VecDeque<OsString>>()
    };
    let mut pending = parts(path);
    let mut resolved = PathBuf::new();
    let mut final_metadata = None;
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        final_metadata = None;
        match Path::new(&part).components().next() {
            Some(Component::Prefix(_) | Component::RootDir) => {
                resolved.push(part);
                continue;
            }
            Some(Component::CurDir) => continue,
            Some(Component::ParentDir) => {
                resolved.pop();
                continue;
            }
            Some(Component::Normal(_)) => {}
            None => return Err("its path has an empty component".to_owned().into()),
        }
        let next = resolved.join(part);
        if super::under_protected_root(home, &next) {
            return Err(PathRefusal::ConsentFenced);
        }
        let meta = match metadata(&next) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                resolved = next;
                continue;
            }
            Err(error) => return Err(format!("its path cannot be checked ({error})").into()),
        };
        if meta.file_type().is_symlink() {
            links += 1;
            if links > 32 {
                return Err("its path has too many symbolic links".to_owned().into());
            }
            let target = read_link(&next)
                .map_err(|error| format!("its symbolic link cannot be read ({error})"))?;
            let target = if target.is_absolute() {
                target
            } else {
                resolved.join(target)
            };
            let mut target_parts = parts(&target);
            target_parts.append(&mut pending);
            pending = target_parts;
            resolved = PathBuf::new();
        } else if !pending.is_empty() && !meta.is_dir() {
            return Err("its path has a non-directory ancestor".to_owned().into());
        } else {
            resolved = next;
            final_metadata = Some(meta);
        }
    }
    Ok((resolved, final_metadata))
}

#[cfg(all(test, unix))]
#[path = "protected_path_tests.rs"]
mod tests;
