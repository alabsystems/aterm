// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original runtime search directories and dynamic libraries from the closing Targo compile.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs::{File, Metadata, OpenOptions};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    ctime_sec: i64,
    ctime_nsec: i64,
}

impl Identity {
    fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            size: m.len(),
            mtime_sec: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime_sec: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        }
    }

    fn matches(&self, m: &Metadata) -> bool {
        *self == Self::of(m)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    kind: String,
    identity: Identity,
    link: Option<PathBuf>,
    canonical: Option<PathBuf>,
    target_identity: Option<Identity>,
    sha256: Option<String>,
    size: Option<u64>,
}

impl Entry {
    fn validate(&self, dir: &Path) -> Result<(), String> {
        let leaf = dir.join(&self.name);
        let changed = || format!("native search entry changed: {}", leaf.display());
        let m = leaf.symlink_metadata().map_err(|_| changed())?;
        let (kind, resolved, link) = if m.is_symlink() {
            let target = match leaf.canonicalize() {
                Ok(target) => Some(target),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(changed()),
            };
            let target_identity = target
                .as_ref()
                .map(|t| t.metadata().map(|m| Identity::of(&m)))
                .transpose()
                .map_err(|_| changed())?;
            if target_identity != self.target_identity {
                return Err(changed());
            }
            (
                "symlink",
                target,
                Some(std::fs::read_link(&leaf).map_err(|_| changed())?),
            )
        } else if m.is_file() {
            (
                "file",
                Some(leaf.canonicalize().map_err(|_| changed())?),
                None,
            )
        } else if m.is_dir() {
            ("directory", None, None)
        } else {
            return Err(changed());
        };
        if kind != self.kind
            || self.link != link
            || !self.identity.matches(&m)
            || resolved != self.canonical
            || (!m.is_symlink() && self.target_identity.is_some())
        {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Directory {
    pub(super) path: PathBuf,
    canonical: Option<PathBuf>,
    identity: Option<Identity>,
    entries: Vec<Entry>,
}

impl Directory {
    pub(super) fn library_bytes(&self) -> Result<u64, String> {
        self.entries
            .iter()
            .filter_map(|entry| entry.size)
            .try_fold(0_u64, |bytes, size| {
                bytes
                    .checked_add(size)
                    .ok_or_else(|| "runtime library size overflow".into())
            })
    }

    pub(super) fn entries(&self) -> usize {
        self.entries.len()
    }
}

/// Keep the selected directories and dynamic regular files open until all children finish.
#[derive(Debug)]
pub(super) struct HeldDirectory {
    directory: Directory,
    held: Option<File>,
    libraries: Vec<(PathBuf, File, Identity)>,
}

impl HeldDirectory {
    pub(super) fn path(&self) -> &Path {
        &self.directory.path
    }

    pub(super) fn capture(directory: Directory) -> Result<Self, String> {
        let path = &directory.path;
        if !path.is_absolute() {
            return Err("relative runtime search directory".into());
        }
        let held = match (&directory.identity, &directory.canonical) {
            (Some(expected), Some(canonical)) if canonical.is_absolute() => {
                let dir = OpenOptions::new()
                    .read(true)
                    .custom_flags(
                        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                    .open(canonical)
                    .map_err(|e| e.to_string())?;
                if !expected.matches(&dir.metadata().map_err(|e| e.to_string())?) {
                    return Err("native library directory changed".into());
                }
                Some(dir)
            }
            (None, None) if directory.entries.is_empty() => None,
            _ => return Err("incomplete native library directory capture".into()),
        };
        let mut libraries = Vec::new();
        for entry in &directory.entries {
            let filename = Path::new(&entry.name);
            if filename.file_name() != Some(filename.as_os_str())
                || entry.name.is_empty()
                || entry.name.ends_with(".framework")
            {
                return Err("invalid library directory entry".into());
            }
            match (&entry.sha256, entry.size) {
                (None, None)
                    if !(entry.name.ends_with(".dylib")
                        || entry.name.ends_with(".so")
                        || entry.name.contains(".so.")) => {}
                (Some(digest), Some(size)) if super::digest_hex(digest) && size <= 256 << 20 => {
                    let selected = path.join(filename);
                    let physical = entry.canonical.as_ref().ok_or("unbound dynamic library")?;
                    if selected.canonicalize().map_err(|e| e.to_string())? != *physical {
                        return Err("dynamic library path changed".into());
                    }
                    let (file, meta) =
                        aterm_uds::regular_file::open(physical).map_err(|e| e.to_string())?;
                    let expected = entry.target_identity.as_ref().unwrap_or(&entry.identity);
                    if meta.len() != size
                        || !expected.matches(&meta)
                        || aterm_codec::hex::encode_lower(
                            &crate::compiler_artifact::contents(&file, size)
                                .map_err(|e| e.to_string())?,
                        ) != *digest
                    {
                        return Err("dynamic library differs from compile".into());
                    }
                    libraries.push((selected, file, Identity::of(&meta)));
                }
                _ => return Err("malformed dynamic library digest or size".into()),
            }
        }
        let captured = Self {
            directory,
            held,
            libraries,
        };
        captured.validate()?;
        Ok(captured)
    }

    // Between the complete membership reads, each child checks the original search
    // directory and each retained dynamic library. Replacing a name changes the
    // namespace stamp. In-place edits to other entries are checked at the end of the run.
    pub(super) fn validate_selected(&self) -> Result<(), String> {
        let dir = &self.directory;
        let path = &dir.path;
        let changed = || format!("native library directory changed: {}", path.display());
        let Some(held) = &self.held else {
            return match path.symlink_metadata() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(changed()),
            };
        };
        let stamp = dir.identity.as_ref().ok_or_else(changed)?;
        if path.canonicalize().ok().as_ref() != dir.canonical.as_ref()
            || !stamp.matches(&held.metadata().map_err(|_| changed())?)
            || !stamp.matches(&path.metadata().map_err(|_| changed())?)
        {
            return Err(changed());
        }
        for (name, file, stamp) in &self.libraries {
            if !stamp.matches(&file.metadata().map_err(|_| changed())?)
                || !stamp.matches(&name.metadata().map_err(|_| changed())?)
            {
                return Err(changed());
            }
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        self.validate_selected()?;
        if self.held.is_none() {
            return Ok(());
        }
        let dir = &self.directory;
        let names: BTreeSet<_> = std::fs::read_dir(&dir.path)
            .map_err(|e| e.to_string())?
            .take(dir.entries.len() + 1)
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if names.len() != dir.entries.len()
            || !dir
                .entries
                .iter()
                .all(|entry| names.contains(std::ffi::OsStr::new(&entry.name)))
            || dir.entries.windows(2).any(|w| w[0].name >= w[1].name)
        {
            return Err(format!(
                "native library directory membership changed: {}",
                dir.path.display()
            ));
        }
        for entry in &dir.entries {
            entry.validate(&dir.path)?;
        }
        self.validate_selected()
    }
}

#[cfg(test)]
#[path = "test_launch_directory_tests.rs"]
mod tests;
