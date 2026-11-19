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
    /// Where `dir` is, `default` where it names none. What a failure stops is its caller's to say:
    /// an admission has written nothing, while a sync may have published rows before it.
    fn of(dir: Option<&str>, default: Option<&Path>) -> Result<Self, String> {
        let Some(path) = dir.map(Path::new).or(default) else {
            return Ok(Self::Default);
        };
        resolve(path).map_err(|why| {
            format!(
                "cannot establish login directory ownership for {}: {why}",
                path.display()
            )
        })
    }

    /// Whether `other` is shown to be this directory: one resolved path, or one node. What owns a
    /// login is judged by this alone ([`rows_at`]).
    fn same(&self, other: &Self) -> bool {
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
            ) => left == right || ln.is_some() && ln == rn,
            _ => false,
        }
    }

    /// Whether `other` may be this directory: [`Self::same`], or one spelling but for ASCII case,
    /// which a case-insensitive volume makes one directory and a case-sensitive one two. A
    /// conservative collision rule, for refusing; it never attributes a directory to an owner.
    fn conflicts(&self, other: &Self) -> bool {
        self.same(other)
            || match (self, other) {
                (Self::Directory { path: left, .. }, Self::Directory { path: right, .. }) => left
                    .to_str()
                    .zip(right.to_str())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)),
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

/// Canonical spelling, resolving existing ancestors even when the leaf is absent.
/// # Errors
/// The path is relative, unreadable, dangling, non-directory, or its missing
/// suffix cannot be resolved without ambiguity.
pub(in crate::harness) fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    match resolve(path).map_err(|why| why.to_string())? {
        Location::Directory { path, .. } => Ok(path),
        Location::Default => Err("directory is unspecified".into()),
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
    retained_directory_owner_at(logins, rows, label, dir, None)
}

/// Resolve default-login custody too when the native default home is known.
pub(in crate::harness) fn retained_directory_owner_at<'a>(
    logins: &'a Logins,
    rows: &'a [Row],
    label: &str,
    dir: &str,
    default: Option<&Path>,
) -> Result<Option<&'a str>, String> {
    let wanted = Location::of(Some(dir), None)?;
    for row in rows.iter().filter(|row| row.label != label) {
        if row_at(&wanted, row, default)? {
            return Ok(Some(row.label.as_str()));
        }
    }
    for (owner, binding) in logins
        .labels
        .iter()
        .filter(|(owner, _)| owner.as_str() != label)
    {
        for kept in std::iter::once(&binding.dir).chain(&binding.was) {
            if wanted.conflicts(&Location::of(kept.as_deref(), default)?) {
                return Ok(Some(owner));
            }
        }
    }
    Ok(None)
}

/// How the rows of a roster stand to one login directory ([`rows_at`]).
#[derive(Debug, Default)]
pub(in crate::harness) struct RowsAt<'a> {
    /// The rows whose login directory is shown to be it: one resolved path or one node. Only
    /// these own the login.
    pub(in crate::harness) same: Vec<&'a Row>,
    /// The rows whose login directory only [`retained_directory_owner_at`]'s collision rule
    /// relates to it: one spelling but for ASCII case, never shown to be one directory (a leaf
    /// not made yet, or two directories on a case-sensitive volume). Such a row neither owns the
    /// login nor leaves it unowned, so its account is never charged for a credential it was not
    /// shown to hold, and no login is admitted beside it as no account's.
    pub(in crate::harness) case_only: Vec<&'a Row>,
}

/// How the rows of `rows` stand to the login directory `dir` (a row that names none: the native
/// default home, `default`): the rows it is shown to be ([`RowsAt::same`]), and those only one
/// spelling but for ASCII case relates to it ([`RowsAt::case_only`]), together every row
/// [`retained_directory_owner_at`] finds there. Execution admission finds a login's owner rows by
/// this, so a current row's directory reached by another path or spelling is that row's where it
/// is the same node, and never reads as a directory only a binding retains.
/// # Errors
/// A directory's ownership cannot be established (an unreadable, dangling or unresolvable path).
pub(in crate::harness) fn rows_at<'a>(
    rows: &'a [Row],
    dir: &str,
    default: &Path,
) -> Result<RowsAt<'a>, String> {
    let wanted = Location::of(Some(dir), None)?;
    let mut at = RowsAt::default();
    for row in rows {
        let there = Location::of(row.dir.as_deref(), Some(default))?;
        if wanted.same(&there) {
            at.same.push(row);
        } else if wanted.conflicts(&there) {
            at.case_only.push(row);
        }
    }
    Ok(at)
}

/// Whether `row`'s login directory (`default` where it names none) may be `wanted`
/// ([`Location::conflicts`]), as [`retained_directory_owner_at`] holds a row's directory to it: a
/// row [`rows_at`] reports as the same directory or as only one spelling but for case.
fn row_at(wanted: &Location, row: &Row, default: Option<&Path>) -> Result<bool, String> {
    Ok(wanted.conflicts(&Location::of(row.dir.as_deref(), default)?))
}

/// Whether `was` lists a directory shown to be `dir` (one resolved path or one node, never one
/// spelling but for case alone, [`Location::same`]): whether a process on `dir` runs on a login
/// the row whose `was` it is left, which attributes that process to the row.
/// # Errors
/// A directory's ownership cannot be established (an unreadable, dangling or unresolvable path).
pub(in crate::harness) fn left_directory(
    was: &[Option<String>],
    dir: &str,
) -> Result<bool, String> {
    let wanted = Location::of(Some(dir), None)?;
    for kept in was.iter().flatten() {
        if wanted.same(&Location::of(Some(kept), None)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `was` lists the directory `dir`, as [`retained_directory_owner`] compares directories.
pub(in crate::harness) fn lists_directory(
    was: &[Option<String>],
    dir: &str,
) -> Result<bool, String> {
    let wanted = Location::of(Some(dir), None)?;
    for kept in was.iter().flatten() {
        if wanted.conflicts(&Location::of(Some(kept), None)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The label of a row of `rows`, whatever its label, whose explicit directory is this one, as
/// [`retained_directory_owner`] compares directories.
/// Unknown filesystem ownership refuses instead of treating a failed lookup as separation.
pub(in crate::harness) fn naming_row<'a>(
    rows: impl IntoIterator<Item = &'a Row>,
    dir: &str,
) -> Result<Option<&'a str>, String> {
    let wanted = Location::of(Some(dir), None)?;
    for row in rows {
        if let Some(kept) = row.dir.as_deref()
            && wanted.conflicts(&Location::of(Some(kept), None)?)
        {
            return Ok(Some(row.label.as_str()));
        }
    }
    Ok(None)
}

/// Whether this checkpoint uses a directory retained by shared preparation custody.
pub(in crate::harness) fn shared_staging_retained(
    logins: &Logins,
    dir: &str,
) -> Result<bool, String> {
    let wanted = Location::of(Some(dir), None)?;
    for kept in logins.shared.staging_paths() {
        if wanted.conflicts(&Location::of(Some(kept), None)?) {
            return Ok(true);
        }
    }
    Ok(false)
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
        self.check_new_dirs(rows, existing, now, &|row, owner| {
            format!(
                "cannot use `{}`: its login directory is retained by `{owner}`; choose an unused label or directory; nothing was written",
                row.label
            )
        })
    }

    /// The directory half of [`Self::check_new_rows`], for a caller that admits the rows' labels
    /// itself (`accounts add` and `adopt-default`, whose rows take the records their account's
    /// reserved slug holds, [`Self::retained_from`]): no new row's directory
    /// aliases another current or new row's, or another binding's current or historical
    /// directory. `refused` says why a row is refused, given the label of the row or binding
    /// that holds its directory.
    /// # Errors
    /// As [`Self::check_new_rows`]'s directory refusals, worded by `refused`.
    pub(in crate::harness) fn check_new_dirs(
        &self,
        rows: &[Row],
        existing: &Roster,
        now: i64,
        refused: &dyn Fn(&Row, &str) -> String,
    ) -> Result<(), String> {
        let (logins, _) = self.load_logins(now)?;
        let default = self.default_home();
        let location = |dir: Option<&str>| {
            Location::of(dir, default.as_deref())
                .map_err(|why| format!("{why}; nothing was written"))
        };
        let mut retained = Vec::new();
        for row in existing.rows.iter().chain(rows) {
            retained.push((row.label.as_str(), location(row.dir.as_deref())?));
        }
        for (owner, binding) in &logins.labels {
            for dir in std::iter::once(&binding.dir).chain(&binding.was) {
                retained.push((owner.as_str(), location(dir.as_deref())?));
            }
        }
        for row in rows {
            let wanted = location(row.dir.as_deref())?;
            if let Some((owner, _)) = retained
                .iter()
                .find(|(owner, location)| *owner != row.label && wanted.conflicts(location))
            {
                return Err(refused(row, owner));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "accounts_directory_tests.rs"]
mod tests;
