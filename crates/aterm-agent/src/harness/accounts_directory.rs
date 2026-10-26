// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Directory ownership at account admission. These observations assume stable filesystem
//! paths while cooperating writers hold their locks; they grant no credential proof.

use super::{Logins, Roster, Row, Store};
use std::io;
use std::path::{Path, PathBuf};

enum Location {
    Default,
    Directory {
        path: PathBuf,
        node: Option<(u64, u64)>,
    },
}

impl Location {
    fn of(dir: Option<&str>, default: Option<&Path>) -> Result<Self, String> {
        let Some(path) = dir.map(Path::new).or(default) else {
            return Ok(Self::Default);
        };
        resolve(path).map_err(|why| {
            format!(
                "cannot establish login directory ownership for {}: {why}; nothing was written",
                path.display()
            )
        })
    }

    fn conflicts(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Default, Self::Default) => true,
            (
                Self::Directory {
                    path: left,
                    node: ln,
                },
                Self::Directory {
                    path: right,
                    node: rn,
                },
            ) => {
                left == right
                    || ln.is_some() && ln == rn
                    || left
                        .to_str()
                        .zip(right.to_str())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }
            _ => false,
        }
    }
}

// Resolve every existing ancestor. A missing leaf does not make a linked parent invisible.
// Refuse uncertainty: dangling links, unreadable paths, and unresolved parent or non-ASCII
// components. ASCII case-only differences are conservatively treated as collisions on every
// platform, so admission does not depend on a volume's case-folding mode.
fn resolve(path: &Path) -> io::Result<Location> {
    if !path.is_absolute() {
        return Err(io::Error::other("the directory is not absolute"));
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                let metadata = std::fs::metadata(&resolved)?;
                if !metadata.is_dir() {
                    return Err(io::Error::other("the existing ancestor is not a directory"));
                }
                #[cfg(unix)]
                let node = {
                    use std::os::unix::fs::MetadataExt as _;
                    missing
                        .is_empty()
                        .then_some((metadata.dev(), metadata.ino()))
                };
                #[cfg(not(unix))]
                let node = None;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(Location::Directory {
                    path: resolved,
                    node,
                });
            }
            Err(why) if why.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(ancestor) {
                    Ok(_) => return Err(io::Error::other("an existing link does not resolve")),
                    Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                    Err(why) => return Err(why),
                }
                let name = ancestor
                    .file_name()
                    .filter(|name| name.to_str().is_some_and(str::is_ascii))
                    .ok_or_else(|| {
                        io::Error::other("an unresolved path component is not a normal ASCII name")
                    })?;
                missing.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| io::Error::other("the directory has no existing ancestor"))?;
            }
            Err(why) => return Err(why),
        }
    }
}

/// Another roster row or binding that retains this explicit directory.
/// Unknown filesystem ownership refuses instead of treating a failed lookup as separation.
pub(in crate::harness) fn retained_directory_owner<'a>(
    logins: &'a Logins,
    rows: &'a [Row],
    label: &str,
    dir: &str,
) -> Result<Option<&'a str>, String> {
    let wanted = Location::of(Some(dir), None)?;
    for row in rows.iter().filter(|row| row.label != label) {
        if let Some(kept) = row.dir.as_deref()
            && wanted.conflicts(&Location::of(Some(kept), None)?)
        {
            return Ok(Some(row.label.as_str()));
        }
    }
    for (owner, binding) in logins
        .labels
        .iter()
        .filter(|(owner, _)| owner.as_str() != label)
    {
        for kept in binding.dir.iter().chain(binding.was.iter().flatten()) {
            if wanted.conflicts(&Location::of(Some(kept), None)?) {
                return Ok(Some(owner));
            }
        }
    }
    Ok(None)
}

impl Store {
    /// Admits new rows without assigning another account's retained directory to them.
    /// The caller holds the writer and sign-in locks through publication. No directory
    /// is created and no retained row or binding is changed here.
    /// # Errors
    /// As [`Self::check_new_labels`], or a directory cannot be resolved or aliases another
    /// current/new roster row or another binding's current/historical directory. A default
    /// home is an opaque shared location for Claude, whose custom home this store cannot know.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountDirectoryAdmission",
            action = "Check",
            project = "aterm_agent::harness::accounts::directory::tests::directory_transition"
        )
    )]
    pub(in crate::harness) fn check_new_rows(
        &self,
        rows: &[Row],
        existing: &Roster,
        now: i64,
    ) -> Result<(), String> {
        self.check_new_labels(
            &rows
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            now,
        )?;
        let (logins, _) = self.load_logins(now)?;
        let default = self.default_home();
        let mut retained = Vec::new();
        for row in existing.rows.iter().chain(rows) {
            retained.push((
                row.label.as_str(),
                Location::of(row.dir.as_deref(), default.as_deref())?,
            ));
        }
        for (owner, binding) in &logins.labels {
            for dir in std::iter::once(&binding.dir).chain(&binding.was) {
                retained.push((
                    owner.as_str(),
                    Location::of(dir.as_deref(), default.as_deref())?,
                ));
            }
        }
        for row in rows {
            let wanted = Location::of(row.dir.as_deref(), default.as_deref())?;
            if let Some((owner, _)) = retained
                .iter()
                .find(|(owner, location)| *owner != row.label && wanted.conflicts(location))
            {
                return Err(format!(
                    "cannot use `{}`: its login directory is retained by `{owner}`; choose an unused label or directory; nothing was written",
                    row.label
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "accounts_directory_tests.rs"]
mod tests;
