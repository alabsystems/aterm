// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The records a clone has still to append to the journal (`docs/DESIGN-landing-economy.md`
//! slice 4, `docs/DESIGN-judged-main-release-pump-2026-11-11.md` §3.1):
//! `<git common dir>/aterm-verify/.ubd/land-pending/`, one per clone and shared by its worktrees,
//! and the one spool of both kinds of land record. A land puts each record here before it appends
//! it and marks it done once an append holds it, by the bytes the append held, so a kill, a
//! failed append or a concurrent session loses no record the spool keeps: a newer record a
//! concurrent landing put at the same path stays, and the next land's append carries what is
//! pending.
//!
//! Each file is a record byte for byte ([`Signed::bytes`]), named by its journal path with the
//! `land/` dropped and each `/` a `.`: a verdict record's `<day>.<T>.<check>.<key>.<word>.toml`
//! ([`crate::land_record::Filed`]), a run record's `runs.<day>.<run>.<seq>.<kind>.toml`
//! ([`crate::land_run::Filed`]).

use std::path::{Path, PathBuf};

use crate::land_record::{Filed, Signed, Word};
use crate::land_run::Filed as RunFiled;

/// The prefix of a run record's spool file.
const RUNS: &str = "runs.";

/// The prefix of a file [`Dir`]'s `done` takes a record aside to: no record's name spells it.
const CLAIM: &str = ".claim.";

/// Where a clone keeps its records until an append holds them.
pub trait Spool {
    /// Keep `record`, whole or not at all. A verdict FAIL already kept at its path is kept as it
    /// was, as the journal keeps a FAIL's first record; any other record replaces the one at its
    /// path.
    ///
    /// # Errors
    /// It could not be written.
    fn put(&self, record: &Signed) -> Result<(), String>;

    /// Every record kept, in name order, and each file that could not be read, with why.
    ///
    /// # Errors
    /// The spool could not be listed.
    fn pending(&self) -> Result<Pending, String>;

    /// Forget `record`, which an append holds: the record kept at its path where that is still
    /// `record`'s bytes, and nothing where none is kept or a newer record took the path since
    /// the append read the spool (a concurrent landing of the clone put it), which stays for the
    /// next append.
    ///
    /// # Errors
    /// It could not be removed, or a newer record it took aside could not be put back.
    fn done(&self, record: &Signed) -> Result<(), String>;
}

/// What a spool holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub records: Vec<Signed>,
    /// Each file named as a record that could not be read, and why.
    pub unreadable: Vec<(PathBuf, String)>,
}

/// A spool in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    dir: PathBuf,
}

/// The spool's file name for journal path `path`.
///
/// # Errors
/// `path` is neither a verdict record's ([`Filed::parse`]) nor a run record's
/// ([`RunFiled::parse`]).
pub fn file_of(path: &str) -> Result<String, String> {
    if path.starts_with(crate::land_run::DIR) {
        let filed = RunFiled::parse(path)?;
        return Ok(format!("{RUNS}{}.{}", filed.day, filed.file()));
    }
    let filed = Filed::parse(path)?;
    Ok(format!(
        "{}.{}.{}",
        filed.day,
        filed.tree,
        filed.name.file()
    ))
}

/// The journal path the spool's file `file` holds a record for, when it names one.
#[must_use]
pub fn path_of(file: &str) -> Option<String> {
    if let Some(run) = file.strip_prefix(RUNS) {
        let (day, name) = run.split_once('.')?;
        let path = format!("{}/{day}/{name}", crate::land_run::DIR);
        return RunFiled::parse(&path).is_ok().then_some(path);
    }
    let (day, rest) = file.split_once('.')?;
    let (tree, name) = rest.split_once('.')?;
    let path = format!("{}/{day}/{tree}/{name}", crate::land_record::DIR);
    Filed::parse(&path).is_ok().then_some(path)
}

/// What journal path `path` files: its UTC day, whether the journal's prune by UTC day `today`
/// drops it ([`crate::judged::expired`], by its kind's retention), and whether it is a verdict
/// FAIL, whose first record a path keeps.
///
/// # Errors
/// It is no record's path.
pub fn kept_as(path: &str, today: u64) -> Result<(u64, bool, bool), String> {
    if path.starts_with(crate::land_run::DIR) {
        let filed = RunFiled::parse(path)?;
        let expired = crate::judged::expired(crate::judged::Kept::Run, filed.day, today);
        return Ok((filed.day, expired, false));
    }
    let filed = Filed::parse(path)?;
    let expired = crate::judged::expired(crate::judged::Kept::Verdict, filed.day, today);
    Ok((filed.day, expired, filed.name.word == Ok(Word::Fail)))
}

impl Dir {
    /// The spool in `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The spool of the clone `root` is a checkout of: `<git common dir>/aterm-verify/.ubd/
    /// land-pending/`.
    ///
    /// # Errors
    /// Git did not name the clone's common directory.
    pub fn of_clone(root: &Path) -> Result<Self, String> {
        let common = crate::judged::git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        if common.is_empty() {
            return Err("git named no common directory".to_owned());
        }
        Ok(Self::at(
            Path::new(&common).join("aterm-verify/.ubd/land-pending"),
        ))
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Remove every kept record the journal's prune drops by `day`, each by its kind's
    /// retention ([`kept_as`]): an append would not file it.
    fn expire(&self, day: u64) {
        let Ok(listing) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for item in listing.flatten() {
            let name = item.file_name();
            let expired = name
                .to_str()
                .and_then(path_of)
                .and_then(|path| kept_as(&path, day).ok())
                .is_some_and(|(_, expired, _)| expired);
            if expired {
                let _ = std::fs::remove_file(item.path());
            }
        }
    }
}

impl Spool for Dir {
    fn put(&self, record: &Signed) -> Result<(), String> {
        let (day, _, fail) = kept_as(&record.path, 0)?;
        let file = self.dir.join(file_of(&record.path)?);
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("the spool {} was not made: {e}", self.dir.display()))?;
        self.expire(day);
        if fail && file.exists() {
            return Ok(());
        }
        // The inner result is the directory's flush after the rename: either way the record is
        // in place for every later reader while the machine runs, which is what a spool keeps.
        let _flushed = aterm_uds::atomic_file::write_landed(&file, &record.bytes, 0o644)
            .map_err(|e| format!("{} was not written: {e}", file.display()))?;
        Ok(())
    }

    fn pending(&self) -> Result<Pending, String> {
        let listing = match std::fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pending::default()),
            Err(e) => {
                return Err(format!(
                    "the spool {} was not read: {e}",
                    self.dir.display()
                ));
            }
        };
        let mut found = Vec::new();
        for item in listing {
            let item =
                item.map_err(|e| format!("the spool {} was not read: {e}", self.dir.display()))?;
            if let Some(path) = item.file_name().to_str().and_then(path_of) {
                found.push((item.path(), path));
            }
        }
        found.sort();
        let mut pending = Pending::default();
        let max = crate::land_record::MAX_BYTES;
        for (file, path) in found {
            match aterm_uds::atomic_file::read_regular(&file, max) {
                Ok((bytes, _)) => pending.records.push(Signed { path, bytes }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => pending.unreadable.push((file, e.to_string())),
            }
        }
        Ok(pending)
    }

    /// The record at `record`'s path is taken aside first, by one rename to a name no reader
    /// takes for a record ([`CLAIM`]), so no put lands between the comparison and the removal:
    /// its bytes are `record`'s, and it is removed; or they are not, and it goes back, kept first
    /// where it is a verdict FAIL (as [`Spool::put`] keeps one), else only where no later put
    /// took the path meanwhile, whose record is newer.
    fn done(&self, record: &Signed) -> Result<(), String> {
        static CLAIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = file_of(&record.path)?;
        let file = self.dir.join(&name);
        let claim = self.dir.join(format!(
            "{CLAIM}{}.{}.{name}",
            std::process::id(),
            CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match std::fs::rename(&file, &claim) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{} was not removed: {e}", file.display())),
        }
        let held = aterm_uds::atomic_file::read_regular(&claim, crate::land_record::MAX_BYTES);
        if held.is_ok_and(|(bytes, _)| bytes == record.bytes) {
            return std::fs::remove_file(&claim)
                .map_err(|e| format!("{} was not removed: {e}", claim.display()));
        }
        let (_, _, fail) = kept_as(&record.path, 0)?;
        let restored = if fail {
            std::fs::rename(&claim, &file)
        } else {
            match std::fs::hard_link(&claim, &file) {
                Ok(()) => std::fs::remove_file(&claim),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::remove_file(&claim)
                }
                Err(e) => Err(e),
            }
        };
        restored.map_err(|e| {
            format!(
                "a newer record at {} was not put back from {}: {e}",
                file.display(),
                claim.display()
            )
        })
    }
}

#[cfg(test)]
#[path = "land_spool_tests.rs"]
mod tests;
