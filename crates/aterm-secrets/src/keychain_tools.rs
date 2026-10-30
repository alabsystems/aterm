// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded keychain tools and path preservation shared by release and development identities.

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

const PROTECTED_BASENAMES: [&str; 3] = ["login.keychain-db", "login.keychain", "System.keychain"];
/// The one hang detector every tool call runs under: an honest call answers in well under a second.
pub const WAIT: Duration = Duration::from_secs(60);

/// How a tool child runs ([`Bounded`]).
pub trait Run {
    /// `cmd`'s output, if it finishes within `wait`.
    /// # Errors
    /// [`RunError`]: it could not start, or did not finish in time.
    fn run(&self, cmd: &mut Command, wait: Duration) -> Result<Output, RunError>;
}

/// [`aterm_uds::deadline::output_until`].
pub struct Bounded;

impl Run for Bounded {
    fn run(&self, cmd: &mut Command, wait: Duration) -> Result<Output, RunError> {
        aterm_uds::deadline::output_until(cmd, Instant::now() + wait)
    }
}

/// The paths `security` prints one per line, each in double quotes.
pub fn unquote_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(PathBuf::from)
        })
        .collect()
}

/// `current` with `keychain` appended, every entry kept in its order: `None` when it is
/// already there, refused when `current` read back empty (a write would drop login).
pub fn search_list_with(
    current: &[PathBuf],
    keychain: &Path,
) -> Result<Option<Vec<PathBuf>>, String> {
    if current.iter().any(|entry| entry == keychain) {
        return Ok(None);
    }
    if current.is_empty() {
        return Err(format!(
            "the keychain search list read back empty; not rewriting it with {} alone (that \
             would drop login from it)",
            keychain.display()
        ));
    }
    let mut list = current.to_vec();
    list.push(keychain.to_path_buf());
    Ok(Some(list))
}

fn follow(path: &Path) -> Option<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..=32 {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&path).ok()?;
                path = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(Path::new("")).join(target)
                };
            }
            _ => return Some(path),
        }
    }
    None
}

fn canon(path: &Path) -> PathBuf {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::canonicalize(dir).map_or_else(|_| path.to_path_buf(), |dir| dir.join(name))
}

fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn folded(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// Whether `path` is a login or default keychain, every compare case-folded.
#[must_use]
pub fn protected(path: &Path, named: &[PathBuf]) -> bool {
    let Some(real) = follow(path) else {
        return true;
    };
    let protected_name = |p: &Path| {
        p.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            PROTECTED_BASENAMES
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&name))
        })
    };
    if protected_name(path) || protected_name(&real) {
        return true;
    }
    let want = folded(&canon(&real));
    let id = file_id(path);
    named.iter().any(|entry| {
        let resolved = follow(entry).unwrap_or_else(|| entry.clone());
        folded(&canon(&resolved)) == want || (id.is_some() && file_id(entry) == id)
    })
}
