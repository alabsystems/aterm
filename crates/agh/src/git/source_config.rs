// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Bounded source metadata, read without entering Git's repository setup.
//!
//! The existing owned-file reader refuses links, foreign or writable metadata,
//! nonregular files and over-limit reads. Parent namespace ownership and the
//! caller's exclusive use of the repository remain assumptions; this is not an
//! atomic snapshot through the later Git operation.

use std::path::{Path, PathBuf};

use aterm_uds::private_file::{PrivateFileError, Writable, read_owned};

#[derive(Debug)]
pub(super) enum ReadError {
    Missing,
    Refused(Refusal),
}

/// Why the source's metadata is refused, naming the path judged, and what lifts the refusal.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) why: String,
    pub(super) next: String,
}

/// What lifts a refusal that no chmod of a write bit does.
const REPAIR: &str = "repair what why= names: agh reads a source's git metadata only from this \
                      user's own regular files and directories, with no symlink, access list or \
                      group or other write bit, and each pointer as git writes it";

impl From<String> for Refusal {
    fn from(why: String) -> Self {
        Self {
            why,
            next: REPAIR.to_owned(),
        }
    }
}

impl From<Refusal> for ReadError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<String> for ReadError {
    fn from(why: String) -> Self {
        Self::Refused(why.into())
    }
}

pub(super) struct SourceConfig {
    pub(super) common: Vec<u8>,
    admin: PathBuf,
    max: usize,
}

impl SourceConfig {
    pub(super) fn read(root: &Path, max: usize) -> Result<Self, ReadError> {
        let marker = root.join(".git");
        let admin = match std::fs::symlink_metadata(&marker) {
            Ok(meta) if meta.is_dir() => marker,
            Ok(meta) if meta.is_file() => {
                let bytes = owned(root, ".git", max, "gitfile")?.ok_or_else(|| {
                    format!(
                        "source gitfile {}: {}",
                        marker.display(),
                        PrivateFileError::Missing
                    )
                })?;
                pointer(root, &bytes, Some("gitdir: "))
                    .map_err(|why| format!("source gitfile {}: {why}", marker.display()))?
            }
            Ok(_) => {
                return Err(format!(
                    "source .git {} is not a non-symlink file or directory",
                    marker.display()
                )
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.to_path_buf(),
            Err(error) => {
                return Err(format!("source .git {} metadata: {error}", marker.display()).into());
            }
        };
        directory(&admin, "admin")?;
        let common = match owned(&admin, "commondir", max, "commondir")? {
            Some(bytes) => pointer(&admin, &bytes, None).map_err(|why| {
                format!(
                    "source commondir {}: {why}",
                    admin.join("commondir").display()
                )
            })?,
            None => admin.clone(),
        };
        directory(&common, "common")?;
        let common = owned(&common, "config", max, "common config")?.ok_or(ReadError::Missing)?;
        Ok(Self { common, admin, max })
    }

    pub(super) fn worktree(&self) -> Result<Option<Vec<u8>>, Refusal> {
        owned(&self.admin, "config.worktree", self.max, "worktree config")
    }
}

/// `name` in `dir` under [`read_owned`]'s rules, or `None` when there is none. A refusal names
/// the file as `what` and its path, and a write bit the path that carries it, its mode and the
/// chmod that clears it.
fn owned(dir: &Path, name: &str, max: usize, what: &str) -> Result<Option<Vec<u8>>, Refusal> {
    let file = dir.join(name);
    match read_owned(dir, name, max) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(PrivateFileError::Writable(writable)) => Err(write_bit(what, dir, name, writable)),
        Err(error) => Err(format!("source {what} {}: {error}", file.display()).into()),
    }
}

/// The refusal of `name` in `dir` for `writable`, naming `dir` by its real path (a commondir's
/// `../..` resolved), which a chmod reaches through the same links. A directory's chmod also
/// names the file in it when that file carries a write bit too, which the next read would
/// refuse.
fn write_bit(what: &str, dir: &Path, name: &str, writable: Writable) -> Refusal {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let file = dir.join(name);
    let (cause, mode) = (writable.cause(), writable.mode());
    let (subject, paths): (_, Vec<(&Path, u32)>) = if writable.dir() {
        let held = written_file(&file).map(|mode| (file.as_path(), mode));
        (
            format!("{}: its directory {}", file.display(), dir.display()),
            std::iter::once((dir.as_path(), mode)).chain(held).collect(),
        )
    } else {
        (file.display().to_string(), vec![(file.as_path(), mode)])
    };
    Refusal {
        why: format!(
            "source {what} {subject} is {cause} (mode {mode:04o}); agh reads git metadata, which \
             can make git run programs, only where no group or other write bit is set"
        ),
        next: crate::identity::write_bit_remedy(&paths),
    }
}

/// The permission bits of `path` when it is this user's regular file with a group or other
/// write bit, read by path for the remedy alone: no read is admitted on them.
fn written_file(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mode = meta.mode() & 0o777;
    (meta.is_file() && meta.uid() == aterm_uds::peer::our_uid() && mode & 0o022 != 0)
        .then_some(mode)
}

fn directory(path: &Path, what: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "source {what} directory {} is not a non-symlink directory",
            path.display()
        )),
        Err(error) => Err(format!(
            "source {what} directory {}: {error}",
            path.display()
        )),
    }
}

fn pointer(base: &Path, bytes: &[u8], prefix: Option<&str>) -> Result<PathBuf, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the pointer is not UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = match prefix {
        Some(prefix) => text
            .strip_prefix(prefix)
            .ok_or("the gitfile has no gitdir prefix")?,
        None => text,
    };
    // Refuse ambiguous framing rather than trimming into a different path.
    // Interior spaces (including Application Support) remain literal.
    if text.is_empty() || text.trim() != text || text.chars().any(char::is_control) {
        return Err("the pointer has unsupported empty, whitespace or control framing");
    }
    let path = Path::new(text);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

#[cfg(test)]
#[path = "source_config_tests.rs"]
mod tests;
