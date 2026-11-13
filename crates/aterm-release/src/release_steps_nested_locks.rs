// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admitted Git paths and pinned root/nested lock replacement for the release bump.

use std::collections::BTreeSet;
use std::path::{Component, Path};

const PATH_BYTES: usize = 1024 * 1024;
const PATHS: usize = 256;

/// Decode complete `git -z` records without trimming or Git's quoted-path spelling.
/// # Errors
/// Oversized, incomplete, non-UTF-8, duplicate or non-relative path records.
pub(super) fn paths(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.len() > PATH_BYTES {
        return Err("Git paths exceed the 1 MiB admission limit".into());
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let Some(records) = bytes.strip_suffix(&[0]) else {
        return Err("Git paths did not end in a NUL record terminator".into());
    };
    let mut paths = Vec::new();
    for record in records.split(|byte| *byte == 0) {
        if paths.len() == PATHS {
            return Err("Git paths exceed the 256-record admission limit".into());
        }
        paths.push(
            std::str::from_utf8(record)
                .map(str::to_owned)
                .map_err(|_| "Git paths are not UTF-8".to_owned())?,
        );
    }
    admit(&paths)?;
    Ok(paths)
}

fn relative(path: &str) -> Result<(), String> {
    if path.as_bytes().contains(&0)
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("Git named an empty, escaping or ambiguous path component".into());
    }
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.is_relative() {
        return Err("Git named an empty or absolute path".into());
    }
    for component in path.components() {
        let Component::Normal(_) = component else {
            return Err("Git named a path with a non-normal component".into());
        };
    }
    Ok(())
}

fn admit(paths: &[String]) -> Result<(), String> {
    if paths.len() > PATHS {
        return Err("Git paths exceed the 256-record admission limit".into());
    }
    let mut bytes = 0_usize;
    let mut seen = BTreeSet::new();
    for path in paths {
        bytes = bytes
            .checked_add(path.len().saturating_add(1))
            .filter(|bytes| *bytes <= PATH_BYTES)
            .ok_or("Git paths exceed the 1 MiB admission limit")?;
        relative(path)?;
        if !seen.insert(path) {
            return Err("Git repeated a path".into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
/// Refuse platforms without this release writer's pinned Unix operations.
/// # Errors
/// Always returns an explicit unsupported-operation refusal.
pub(super) fn bump(
    _: &Path,
    _: Result<Vec<String>, String>,
    _: &str,
    _: &str,
) -> Result<Vec<String>, String> {
    Err("nested lock replacement is unsupported without Unix pinned-directory operations".into())
}

#[cfg(unix)]
/// Stage admitted nested locks, then replace their pinned directory entries.
/// # Errors
/// Refused enumeration, paths, aliases, reads, identities, bounds, staging or publication.
pub(super) fn bump(
    repo: &Path,
    listing: Result<Vec<String>, String>,
    from: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    unix::bump(repo, listing, from, to)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use aterm_dirfd::{AtFlags, FileType, Mode, OFlags};
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, Metadata};
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    const LOCK_BYTES: u64 = 32 * 1024 * 1024;
    const BATCH_BYTES: u64 = 64 * 1024 * 1024;
    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Identity(u64, u64);

    fn identity(file: &File) -> Result<Identity, String> {
        let meta = file.metadata().map_err(|e| e.to_string())?;
        Ok(Identity(meta.dev(), meta.ino()))
    }

    fn named(dir: &File, name: &OsStr) -> Result<aterm_dirfd::Stat, String> {
        aterm_dirfd::fstatat(dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| e.to_string())
    }

    fn named_identity(dir: &File, name: &OsStr) -> Result<Identity, String> {
        let stat = named(dir, name)?;
        Ok(Identity(stat.st_dev, stat.st_ino))
    }

    fn directory(parent: &File, name: &OsStr) -> Result<File, String> {
        aterm_dirfd::openat(parent, name, directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(|e| e.to_string())
    }

    fn root(path: &Path) -> Result<File, String> {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let mut directory = File::from(
            aterm_dirfd::openat(aterm_dirfd::CWD, "/", directory_flags(), Mode::empty())
                .map_err(|e| e.to_string())?,
        );
        for part in absolute.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => directory = self::directory(&directory, name)?,
                _ => return Err("repository root has a non-normal component".into()),
            }
        }
        Ok(directory)
    }

    fn parent(root: &File, path: &str) -> Result<(File, OsString), String> {
        let mut parts = Path::new(path).components().peekable();
        let mut directory: Option<File> = None;
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err("nested lock has a non-normal component".into());
            };
            if parts.peek().is_none() {
                let dir = match directory {
                    Some(dir) => dir,
                    None => root.try_clone().map_err(|e| e.to_string())?,
                };
                return Ok((dir, name.to_owned()));
            }
            let base = directory.as_ref().unwrap_or(root);
            directory = Some(self::directory(base, name)?);
        }
        Err("nested lock has no filename".into())
    }

    #[derive(PartialEq, Eq)]
    struct Snapshot {
        id: Identity,
        mode: u32,
        links: u64,
        len: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }

    impl Snapshot {
        fn of(meta: &Metadata) -> Result<Self, String> {
            if !meta.is_file() || meta.nlink() != 1 {
                return Err("nested lock is not a regular single-link file".into());
            }
            Ok(Self {
                id: Identity(meta.dev(), meta.ino()),
                mode: meta.mode(),
                links: meta.nlink(),
                len: meta.len(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            })
        }
    }

    struct Stage {
        path: String,
        parent: File,
        target: OsString,
        original: File,
        before: Snapshot,
        name: OsString,
        directory: File,
        directory_id: Identity,
        data: Option<File>,
        data_id: Option<Identity>,
    }

    impl Stage {
        fn new(
            path: String,
            parent: File,
            target: OsString,
            original: File,
            before: Snapshot,
            text: &str,
        ) -> Result<Self, String> {
            use ring::rand::SecureRandom as _;

            let mut created = None;
            for _ in 0..8 {
                let mut nonce = [0_u8; 16];
                ring::rand::SystemRandom::new()
                    .fill(&mut nonce)
                    .map_err(|_| "nested lock staging could not obtain randomness")?;
                let name = OsString::from(format!(
                    ".aterm-lock-bump-{:032x}.noindex",
                    u128::from_le_bytes(nonce)
                ));
                match aterm_dirfd::mkdirat(&parent, &name, Mode::RWXU) {
                    Ok(()) => {
                        let observed = named_identity(&parent, &name)?;
                        let directory = self::directory(&parent, &name)?;
                        if identity(&directory)? != observed {
                            return Err("nested lock staging directory changed identity".into());
                        }
                        created = Some((name, directory, observed));
                        break;
                    }
                    Err(error)
                        if std::io::Error::from_raw_os_error(error.raw_os_error()).kind()
                            == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            let (name, directory, directory_id) =
                created.ok_or("nested lock staging names are occupied")?;
            let mut stage = Self {
                path,
                parent,
                target,
                original,
                before,
                name,
                directory,
                directory_id,
                data: None,
                data_id: None,
            };
            stage.data = Some(File::from(
                aterm_dirfd::openat(
                    &stage.directory,
                    "data",
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC
                        | OFlags::NONBLOCK,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(|e| e.to_string())?,
            ));
            let data = stage.data.as_mut().ok_or("nested lock stage has no data")?;
            stage.data_id = Some(identity(data)?);
            data.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            data.set_permissions(std::fs::Permissions::from_mode(stage.before.mode & 0o777))
                .map_err(|e| e.to_string())?;
            Ok(stage)
        }

        fn check(&self, root: &File) -> Result<(), String> {
            let (current, _) = parent(root, &self.path)?;
            if identity(&current)? != identity(&self.parent)? {
                return Err("nested lock parent changed identity".into());
            }
            let now = Snapshot::of(&self.original.metadata().map_err(|e| e.to_string())?)?;
            let entry = named(&self.parent, &self.target)?;
            if now != self.before
                || !FileType::from_raw_mode(entry.st_mode).is_file()
                || entry.st_nlink != 1
                || Identity(entry.st_dev, entry.st_ino) != self.before.id
            {
                return Err("nested lock changed after it was read".into());
            }
            let data = self.data.as_ref().ok_or("nested lock stage has no data")?;
            if named_identity(&self.directory, OsStr::new("data"))? != identity(data)?
                || data.metadata().map_err(|e| e.to_string())?.nlink() != 1
            {
                return Err("nested lock staged data changed identity".into());
            }
            Ok(())
        }

        fn publish(&self) -> Result<(), String> {
            aterm_dirfd::renameat(&self.directory, "data", &self.parent, &self.target)
                .map_err(|e| e.to_string())
        }
    }

    impl Drop for Stage {
        fn drop(&mut self) {
            if self.data_id.is_some_and(|id| {
                named_identity(&self.directory, OsStr::new("data")).ok() == Some(id)
            }) {
                let _ = aterm_dirfd::unlinkat(&self.directory, "data", AtFlags::empty());
            }
            if named_identity(&self.parent, &self.name).ok() == Some(self.directory_id) {
                let _ = aterm_dirfd::unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR);
            }
        }
    }

    /// Prepare and publish with the ordinary no-op observation boundary.
    /// # Errors
    /// Any admission, read, staging, identity or replacement refusal.
    pub(super) fn bump(
        repo: &Path,
        listing: Result<Vec<String>, String>,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, String> {
        bump_on(repo, listing, from, to, || {})
    }

    /// Observe completed staging before every target's final identity checks.
    /// # Errors
    /// The same filesystem and input refusals as [`bump`].
    /// # Panics
    /// The supplied observer panics.
    pub(super) fn bump_on(
        repo: &Path,
        listing: Result<Vec<String>, String>,
        from: &str,
        to: &str,
        before_publish: impl FnOnce(),
    ) -> Result<Vec<String>, String> {
        let paths = listing?;
        admit(&paths)?;
        for path in &paths {
            let path = Path::new(path);
            if path.file_name() != Some(OsStr::new("Cargo.lock")) {
                return Err("Git did not name a Cargo.lock".into());
            }
        }
        let root = root(repo).map_err(|why| format!("pin repository {repo:?}: {why}"))?;
        let mut stages = Vec::new();
        let mut remaining = BATCH_BYTES;
        let mut writable = BATCH_BYTES;
        for path in paths {
            let (parent, target) =
                parent(&root, &path).map_err(|why| format!("admit {path:?}: {why}"))?;
            let file = File::from(
                aterm_dirfd::openat(
                    &parent,
                    &target,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| format!("read {path}: {e}"))?,
            );
            let before = Snapshot::of(&file.metadata().map_err(|e| e.to_string())?)
                .map_err(|why| format!("admit {path:?}: {why}"))?;
            if before.len > LOCK_BYTES || before.len > remaining {
                return Err(
                    "nested locks exceed the 32 MiB/file or 64 MiB/batch read limit".into(),
                );
            }
            let mut bytes =
                Vec::with_capacity(usize::try_from(before.len).unwrap_or(0).saturating_add(1));
            (&file)
                .take(LOCK_BYTES.min(remaining) + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("read {path}: {e}"))?;
            let now = Snapshot::of(&file.metadata().map_err(|e| e.to_string())?)?;
            if now != before || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len {
                return Err(format!("{path} changed while it was read"));
            }
            remaining -= before.len;
            let text = std::str::from_utf8(&bytes).map_err(|_| format!("{path} is not UTF-8"))?;
            if let Some(moved) = super::super::lock_follows_bump(text, from, to) {
                let size =
                    u64::try_from(moved.len()).map_err(|_| "nested lock output is too large")?;
                if size > LOCK_BYTES || size > writable {
                    return Err(
                        "nested locks exceed the 32 MiB/file or 64 MiB/batch stage limit".into(),
                    );
                }
                writable -= size;
                stages.push(
                    Stage::new(path.clone(), parent, target, file, before, &moved)
                        .map_err(|why| format!("stage {path:?}: {why}"))?,
                );
            }
        }
        before_publish();
        // All reads and staged writes precede every publication. These checks observe identity;
        // they do not make the subsequent renames an atomic compare-and-swap or batch transaction.
        for stage in &stages {
            stage
                .check(&root)
                .map_err(|why| format!("recheck {:?}: {why}", stage.path))?;
        }
        let mut followed = Vec::with_capacity(stages.len());
        for mut stage in stages {
            stage
                .check(&root)
                .map_err(|why| format!("recheck {:?}: {why}", stage.path))?;
            stage
                .publish()
                .map_err(|why| format!("replace {:?}: {why}", stage.path))?;
            followed.push(std::mem::take(&mut stage.path));
        }
        Ok(followed)
    }
}

#[cfg(test)]
#[path = "release_steps_nested_locks_tests.rs"]
mod tests;
