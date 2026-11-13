// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable custody of one eligible snapshot while its tree and Git entry are removed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::super::registered::volume;

pub(super) const SUFFIX: &str = "-verify-retiring.noindex";
const MAX_BYTES: usize = 32 << 10;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Pending {
    schema: u32,
    snapshot: PathBuf,
    pub(super) snapshot_id: [u64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) snapshot_volume: Option<volume::Uuid>,
    pub(super) entry: PathBuf,
    pub(super) entry_id: [u64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) entry_volume: Option<volume::Uuid>,
    gitdir: Vec<u8>,
    pub(super) gitfile: Vec<u8>,
    pub(super) head: String,
    pub(super) tree_gone: bool,
    pub(super) started: [u64; 2],
}

impl Pending {
    const fn snapshot_identity(&self) -> Identity {
        Identity {
            id: self.snapshot_id,
            volume: self.snapshot_volume,
        }
    }

    const fn entry_identity(&self) -> Identity {
        Identity {
            id: self.entry_id,
            volume: self.entry_volume,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    id: [u64; 2],
    volume: Option<volume::Uuid>,
}

impl Identity {
    fn names(&self, now: &Self) -> bool {
        self.id[1] == now.id[1]
            && match &self.volume {
                Some(volume) => now.volume.as_ref() == Some(volume),
                None => self.id[0] == now.id[0],
            }
    }
}

fn location(path: &Path) -> Option<PathBuf> {
    Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?))
}

fn path(checkout: &Path) -> Result<PathBuf, String> {
    super::beside(checkout, SUFFIX).ok_or_else(|| "no snapshot receipt path".to_owned())
}

fn identity(path: &Path) -> Result<Option<Identity>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => meta,
        Ok(_) => return Err(format!("{} is no longer a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} could not be read: {error}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let volume = volume::at(aterm_dirfd::CWD, path)
            .map_err(|error| format!("{} volume could not be read: {error}", path.display()))?;
        let again = std::fs::symlink_metadata(path)
            .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        if !again.is_dir() || (again.dev(), again.ino()) != (meta.dev(), meta.ino()) {
            return Err(format!(
                "{} changed while its identity was read",
                path.display()
            ));
        }
        Ok(Some(Identity {
            id: [meta.dev(), meta.ino()],
            volume,
        }))
    }
    #[cfg(windows)]
    {
        let _ = meta;
        crate::platform::directory_identity(path)
            .map(|(vol, index)| {
                Some(Identity {
                    id: [u64::from(vol), index],
                    volume: None,
                })
            })
            .ok_or_else(|| "snapshot directory identity unavailable".to_owned())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        Err("snapshot directory identity unavailable".to_owned())
    }
}

/// Now as the file system dates a write made from here on: never after the time such a write
/// takes, so work arriving after a removal began is never older than its cutoff
/// ([`unchanged_since`]). Linux dates a write by its coarse clock (`CLOCK_REALTIME_COARSE`, the
/// wall clock at its last tick), up to a tick behind the wall clock, so a file written just
/// after a read of the wall clock can be dated before it; a write in the tick that reading
/// falls in is then dated at the cutoff, and stays. Windows dates a write by the system time at
/// its last tick too (about 15.6 ms), which `GetSystemTimeAsFileTime` reads, while
/// [`SystemTime::now`] there is the precise clock: measured on Windows 11, 210 of 300 files
/// written just after a precise read were dated before it, none of 300 after a coarse read.
/// Elsewhere it is the wall clock.
pub(super) fn file_clock_now() -> SystemTime {
    #[cfg(windows)]
    {
        /// 100 ns ticks between the `FILETIME` epoch (1601-01-01 UTC) and the Unix epoch.
        const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        unsafe extern "system" {
            fn GetSystemTimeAsFileTime(now: *mut FileTime);
        }
        let mut now = FileTime { low: 0, high: 0 };
        // SAFETY: `GetSystemTimeAsFileTime` writes the one `FILETIME` it is handed, live on this
        // frame, and cannot fail.
        unsafe { GetSystemTimeAsFileTime(&raw mut now) };
        let ticks = (u64::from(now.high) << 32) | u64::from(now.low);
        if let Some(nanos) = ticks
            .checked_sub(UNIX_EPOCH_TICKS)
            .and_then(|since| since.checked_mul(100))
        {
            return SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos);
        }
    }
    #[cfg(target_os = "linux")]
    {
        const CLOCK_REALTIME_COARSE: libc::clockid_t = 5;
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `clock_gettime` writes the one `timespec` it is handed, live on this frame.
        if unsafe { libc::clock_gettime(CLOCK_REALTIME_COARSE, &raw mut now) } == 0
            && let (Ok(seconds), Ok(nanos)) =
                (u64::try_from(now.tv_sec), u32::try_from(now.tv_nsec))
            && nanos < 1_000_000_000
        {
            return SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos);
        }
    }
    SystemTime::now()
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
    let bytes = match crate::metadata_io::read_bounded_regular(&path, MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!("snapshot removal receipt cannot be read: {error}"));
        }
    };
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
    if let Some(now) = identity(wt)? {
        if pending.tree_gone || !pending.snapshot_identity().names(&now) {
            return Err("snapshot was replaced after removal began".to_owned());
        }
        let gitfile = wt.join(".git");
        match crate::metadata_io::read_bounded_regular(&gitfile, 4096) {
            Ok(bytes) if bytes == pending.gitfile => {}
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
    if let Some(now) = identity(&pending.entry)? {
        if !pending.entry_identity().names(&now) {
            return Err("snapshot Git entry changed after removal began".to_owned());
        }
        let gitdir = pending.entry.join("gitdir");
        match crate::metadata_io::read_bounded_regular(&gitdir, 4096) {
            Ok(bytes) if bytes == pending.gitdir => {}
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
    let snapshot_identity = identity(wt)?.ok_or("snapshot disappeared before removal")?;
    let entry_identity = identity(entry)?.ok_or("snapshot Git entry disappeared before removal")?;
    let pending = Pending {
        schema: 1,
        snapshot: location(wt).ok_or("snapshot location unavailable")?,
        snapshot_id: snapshot_identity.id,
        snapshot_volume: snapshot_identity.volume,
        entry: entry.to_path_buf(),
        entry_id: entry_identity.id,
        entry_volume: entry_identity.volume,
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
    let pending = prepare(wt, entry, head, gitfile, file_clock_now())?;
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
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
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
