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

use aterm_uds::private_file::{PrivateFileError, read_owned};

#[derive(Debug)]
pub(super) enum ReadError {
    Missing,
    Refused(String),
}

impl From<String> for ReadError {
    fn from(error: String) -> Self {
        Self::Refused(error)
    }
}

impl From<&str> for ReadError {
    fn from(error: &str) -> Self {
        Self::Refused(error.to_owned())
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
                let bytes = read_owned(root, ".git", max)
                    .map_err(|error| format!("source gitfile: {error}"))?;
                pointer(root, &bytes, Some("gitdir: "))?
            }
            Ok(_) => return Err("source .git is not a non-symlink file or directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.to_path_buf(),
            Err(error) => return Err(format!("source .git metadata: {error}").into()),
        };
        directory(&admin)?;
        let common = match read_owned(&admin, "commondir", max) {
            Ok(bytes) => pointer(&admin, &bytes, None)?,
            Err(PrivateFileError::Missing) => admin.clone(),
            Err(error) => return Err(format!("source commondir: {error}").into()),
        };
        directory(&common)?;
        let common = read_owned(&common, "config", max).map_err(|error| match error {
            PrivateFileError::Missing => ReadError::Missing,
            error => ReadError::Refused(format!(
                "source common config is invalid or too long: {error}"
            )),
        })?;
        Ok(Self { common, admin, max })
    }

    pub(super) fn worktree(&self) -> Result<Option<Vec<u8>>, String> {
        match read_owned(&self.admin, "config.worktree", self.max) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(PrivateFileError::Missing) => Ok(None),
            Err(error) => Err(format!("source worktree config: {error}")),
        }
    }
}

fn directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err("source admin path is not a non-symlink directory".into()),
        Err(error) => Err(format!("source admin metadata: {error}")),
    }
}

fn pointer(base: &Path, bytes: &[u8], prefix: Option<&str>) -> Result<PathBuf, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "source pointer is not UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = match prefix {
        Some(prefix) => text
            .strip_prefix(prefix)
            .ok_or("source gitfile has no gitdir prefix")?,
        None => text,
    };
    // Refuse ambiguous framing rather than trimming into a different path.
    // Interior spaces (including Application Support) remain literal.
    if text.is_empty() || text.trim() != text || text.chars().any(char::is_control) {
        return Err("source pointer has unsupported empty, whitespace or control framing".into());
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
