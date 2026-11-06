// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe the fixed part of build-slot's kernel-held reservations.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::{MAX_SLOTS, invalid};

/// Count external fixed CPU reservations while the caller holds its native
/// pool's admission lock. This function never waits for an external holder.
/// Unlocked stale metadata counts zero; held unknown metadata vetoes the full
/// host. The lock, not the reported PID, establishes that a record is live.
pub fn reserved_cpus(directory: &Path, cpus: u32, slots: u32) -> io::Result<u32> {
    if !directory.is_absolute() || cpus == 0 || slots == 0 || slots > MAX_SLOTS {
        return Err(invalid("invalid external CPU reservation namespace"));
    }
    let root = match crate::unix::open_parent(directory) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    match open_record(&root, "admission.lock") {
        Ok(fence) if held(&fence)? => return Ok(cpus),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut seen = BTreeMap::new();
    let mut total = 0u32;
    let weight = cpus.div_ceil(slots);
    let mut count = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(index) = name.strip_prefix("slot-") else { continue };
        if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        count += 1;
        if count > MAX_SLOTS {
            return Err(invalid("too many external CPU slot files"));
        }
        let index: u32 = index.parse().map_err(|_| invalid("external slot index overflows"))?;
        let mut file = open_record(&root, name)?;
        if !held(&file)? {
            continue;
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(65_537).read_to_end(&mut bytes)?;
        if bytes.len() > 65_536 {
            return Ok(cpus);
        }
        let Ok(row) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Ok(cpus) };
        let Some(row) = row.as_object() else { return Ok(cpus) };
        let Some(mode) = row.get("cpu_mode") else {
            if !row.get("pid").and_then(|n| n.as_u64()).is_some_and(|pid| pid > 0)
                || row.get("cmd").and_then(|s| s.as_str()).is_none()
                || !row
                    .get("cwd")
                    .and_then(|s| s.as_str())
                    .is_some_and(|cwd| Path::new(cwd).is_absolute())
                || !row
                    .get("started")
                    .and_then(|n| n.as_f64())
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            {
                return Ok(cpus);
            }
            total = total.saturating_add(if index >= slots { cpus } else { weight });
            continue;
        };
        let mode = mode.as_str().unwrap_or("");
        let lease = row.get("lease_id").and_then(|id| id.as_str()).unwrap_or("");
        let budget = row.get("cpu_budget").and_then(|n| n.as_u64()).unwrap_or(0);
        let pool_slots = row.get("pool_slots").and_then(|n| n.as_u64()).unwrap_or(0);
        if !matches!(mode, "workers-v1" | "fixed-v1")
            || lease.len() != 32
            || !lease.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || budget == 0
            || budget > u64::from(cpus)
            || pool_slots == 0
            || pool_slots > u64::from(MAX_SLOTS)
            || u64::from(index) >= pool_slots
        {
            return Ok(cpus);
        }
        let identity = (mode.to_owned(), budget, pool_slots);
        if let Some(previous) = seen.get(lease) {
            if previous != &identity {
                return Ok(cpus);
            }
        } else {
            seen.insert(lease.to_owned(), identity);
            if mode == "fixed-v1" {
                total = total.saturating_add(budget as u32);
            }
        }
    }
    let named = std::fs::symlink_metadata(directory)?;
    let original = root.metadata()?;
    if !named.is_dir() || named.dev() != original.dev() || named.ino() != original.ino() {
        return Err(invalid("external CPU reservation directory changed"));
    }
    Ok(total.min(cpus))
}

fn open_record(root: &File, name: &str) -> io::Result<File> {
    let file = crate::unix::open_at(root, name, libc::O_RDONLY, false)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid reads this process's effective UID and has no arguments.
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o022 != 0
    {
        return Err(invalid("external CPU record has unsafe ownership or mode"));
    }
    let named = crate::unix::open_at(root, name, libc::O_RDONLY, false)?.metadata()?;
    if (named.dev(), named.ino()) != (meta.dev(), meta.ino()) {
        return Err(invalid("external CPU record changed during observation"));
    }
    Ok(file)
}

fn held(file: &File) -> io::Result<bool> {
    match file.try_lock_shared() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
