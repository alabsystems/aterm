// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable custody of one eligible snapshot while its tree and Git entry are removed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub(super) const SUFFIX: &str = "-verify-retiring.noindex";
const MAX_BYTES: usize = 32 << 10;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Pending {
    schema: u32,
    snapshot: PathBuf,
    snapshot_id: [u64; 2],
    pub(super) entry: PathBuf,
    entry_id: [u64; 2],
    gitdir: Vec<u8>,
    pub(super) gitfile: Vec<u8>,
    pub(super) head: String,
    pub(super) tree_gone: bool,
    pub(super) started: [u64; 2],
}

fn location(path: &Path) -> Option<PathBuf> {
    Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?))
}

fn path(checkout: &Path) -> Result<PathBuf, String> {
    super::beside(checkout, SUFFIX).ok_or_else(|| "no snapshot receipt path".to_owned())
}

fn identity(path: &Path) -> Result<Option<[u64; 2]>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => meta,
        Ok(_) => return Err(format!("{} is no longer a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} could not be read: {error}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(Some([meta.dev(), meta.ino()]))
    }
    #[cfg(windows)]
    {
        let _ = meta;
        crate::platform::directory_identity(path)
            .map(|(volume, index)| Some([u64::from(volume), index]))
            .ok_or_else(|| "snapshot directory identity unavailable".to_owned())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        Err("snapshot directory identity unavailable".to_owned())
    }
}

/// Unlike ordinary cache age discovery, custody revalidation includes nested Git
/// bookkeeping and fails closed on unreadable entries. The remover only unlinks files;
/// folder times and the root .git it restores are the two writes it can cause itself.
pub(super) fn unchanged_since(
    root: &Path,
    since: SystemTime,
    skip_gitfile: bool,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(at) = stack.pop() {
        if skip_gitfile && at == root.join(".git") {
            continue;
        }
        let meta = std::fs::symlink_metadata(&at)
            .map_err(|error| format!("snapshot custody cannot read {}: {error}", at.display()))?;
        if meta.is_dir() {
            let entries = std::fs::read_dir(&at).map_err(|error| {
                format!("snapshot custody cannot list {}: {error}", at.display())
            })?;
            for entry in entries {
                stack.push(
                    entry
                        .map_err(|error| format!("snapshot custody cannot read an entry: {error}"))?
                        .path(),
                );
            }
        } else {
            let modified = meta.modified().map_err(|error| {
                format!("snapshot custody cannot date {}: {error}", at.display())
            })?;
            if modified >= since {
                return Err(format!(
                    "{} was written after snapshot removal began; its new work stays",
                    at.display()
                ));
            }
        }
    }
    Ok(())
}

/// A receipt is authority only for the exact directories and Git binding it admitted.
pub(super) fn read(checkout: &Path, wt: &Path) -> Result<Option<Pending>, String> {
    read_at(checkout, wt, SystemTime::now())
}

pub(super) fn read_at(
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<Option<Pending>, String> {
    read_bound(checkout, wt, now, false)
}

pub(super) fn revalidate(checkout: &Path, wt: &Path, pending: &Pending) -> Result<(), String> {
    if read(checkout, wt)?.as_ref() == Some(pending) {
        Ok(())
    } else {
        Err("snapshot custody changed during admission".to_owned())
    }
}

fn read_bound(
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    proving_gone: bool,
) -> Result<Option<Pending>, String> {
    let path = path(checkout)?;
    if std::fs::symlink_metadata(&path)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    let bytes = crate::metadata_io::read_bounded_regular(&path, MAX_BYTES)
        .map_err(|error| format!("snapshot removal receipt cannot be read: {error}"))?;
    let pending: Pending = aterm_json::from_slice(&bytes)
        .map_err(|error| format!("snapshot removal receipt is invalid: {error}"))?;
    let [seconds, nanos] = pending.started;
    if nanos >= 1_000_000_000 {
        return Err("snapshot removal receipt has an invalid start time".to_owned());
    }
    let started = SystemTime::UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanos as u32))
        .ok_or("snapshot removal receipt start time is out of range")?;
    if started > now {
        return Err(
            "snapshot removal receipt starts after the current clock; its data stays".to_owned(),
        );
    }
    if pending.schema != 1
        || location(wt).as_ref() != Some(&pending.snapshot)
        || super::entry_named(checkout, wt, &pending.gitfile).as_ref() != Some(&pending.entry)
    {
        return Err("snapshot removal receipt no longer names this snapshot".to_owned());
    }
    if let Some(id) = identity(wt)? {
        if pending.tree_gone || id != pending.snapshot_id {
            return Err("snapshot was replaced after removal began".to_owned());
        }
        let gitfile = wt.join(".git");
        match std::fs::symlink_metadata(&gitfile) {
            Ok(_) if super::gitfile_text(&gitfile).as_ref() == Some(&pending.gitfile) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(
                    "snapshot Git binding changed or is unreadable after removal began".to_owned(),
                );
            }
        }
        unchanged_since(wt, started, true)?;
    } else if !pending.tree_gone && !proving_gone {
        return Err(
            "snapshot disappeared before its removal was proved; it may have moved".to_owned(),
        );
    }
    if let Some(id) = identity(&pending.entry)? {
        if id != pending.entry_id {
            return Err("snapshot Git entry changed after removal began".to_owned());
        }
        let gitdir = pending.entry.join("gitdir");
        match std::fs::symlink_metadata(&gitdir) {
            Ok(_) if super::gitfile_text(&gitdir).as_ref() == Some(&pending.gitdir) => {}
            Err(error) if pending.tree_gone && error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(
                    "snapshot gitdir changed or is unreadable after removal began".to_owned(),
                );
            }
        }
        unchanged_since(&pending.entry, started, false)?;
    }
    Ok(Some(pending))
}

/// Capture the admitted identities and cutoff before the potentially blocking process probe.
pub(super) fn prepare(
    wt: &Path,
    entry: &Path,
    head: &str,
    gitfile: &[u8],
    started: SystemTime,
) -> Result<Pending, String> {
    let started = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| format!("snapshot removal clock is before the epoch: {error}"))?;
    let pending = Pending {
        schema: 1,
        snapshot: location(wt).ok_or("snapshot location unavailable")?,
        snapshot_id: identity(wt)?.ok_or("snapshot disappeared before removal")?,
        entry: entry.to_path_buf(),
        entry_id: identity(entry)?.ok_or("snapshot Git entry disappeared before removal")?,
        gitdir: super::gitfile_text(&entry.join("gitdir")).ok_or("snapshot gitdir unreadable")?,
        gitfile: gitfile.to_vec(),
        head: head.to_owned(),
        tree_gone: false,
        started: [started.as_secs(), u64::from(started.subsec_nanos())],
    };
    Ok(pending)
}

/// Atomically install custody without replacing an existing receipt. The caller strictly
/// revalidates this captured record after its process probes and before deleting anything.
pub(super) fn commit(checkout: &Path, pending: &Pending) -> Result<(), String> {
    write(checkout, pending, false)
}

#[cfg(test)]
pub(super) fn begin(
    checkout: &Path,
    wt: &Path,
    entry: &Path,
    head: &str,
    gitfile: &[u8],
) -> Result<Pending, String> {
    let pending = prepare(wt, entry, head, gitfile, SystemTime::now())?;
    commit(checkout, &pending)?;
    Ok(pending)
}

fn write(checkout: &Path, pending: &Pending, replace: bool) -> Result<(), String> {
    use std::io::Write as _;
    let path = path(checkout)?;
    let parent = path.parent().ok_or("snapshot receipt parent unavailable")?;
    let stage = aterm_tempfile::Builder::new()
        .prefix("retired-receipt-")
        .tempdir_in(parent)
        .map_err(|error| format!("snapshot receipt staging failed: {error}"))?;
    let draft = stage.path().join("receipt");
    let bytes = aterm_json::to_vec(&pending).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("snapshot removal receipt is too large".to_owned());
    }
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&draft)?;
        file.write_all(&bytes)?;
        aterm_durable::ordered(&file)?;
        if replace {
            std::fs::rename(&draft, &path)?;
        } else {
            super::super::rename_noreplace(&draft, &path)?;
        }
        aterm_durable::ordered_dir(parent)
    };
    write().map_err(|error| format!("snapshot removal receipt could not be committed: {error}"))?;
    Ok(())
}

/// Only the remover's whole-tree result may advance Prepared to TreeGone. A missing
/// Prepared root on a later pass could have been moved, so it cannot authorize metadata.
pub(super) fn tree_gone(checkout: &Path, wt: &Path, pending: &mut Pending) -> Result<(), String> {
    if read_bound(checkout, wt, SystemTime::now(), true)?.as_ref() != Some(pending)
        || identity(wt)?.is_some()
    {
        return Err("snapshot changed while proving its removal".to_owned());
    }
    let mut gone = pending.clone();
    gone.tree_gone = true;
    write(checkout, &gone, true)?;
    *pending = gone;
    Ok(())
}

/// Only the same receipt goes, once both of its admitted directories are absent.
pub(super) fn finish(checkout: &Path, wt: &Path, pending: &Pending) -> Result<(), String> {
    if read(checkout, wt)?.as_ref() != Some(pending)
        || identity(wt)?.is_some()
        || identity(&pending.entry)?.is_some()
    {
        return Err("snapshot removal receipt still has data or changed custody".to_owned());
    }
    let path = path(checkout)?;
    std::fs::remove_file(&path).map_err(|error| format!("snapshot receipt stayed: {error}"))?;
    aterm_durable::ordered_dir(path.parent().ok_or("snapshot receipt parent unavailable")?)
        .map_err(|error| format!("snapshot receipt removal could not be synced: {error}"))
}
