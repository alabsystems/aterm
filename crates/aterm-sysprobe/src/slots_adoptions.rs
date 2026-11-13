// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observes Trust's process-bound adoption leases; never acquires or releases them.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use super::{Holder, MAX_ENTRIES, RECORD_CAP, Sight, SlotObs, birth_us};
use aterm_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    Held(Holder),
    Unknown,
}

fn birth(text: &str) -> Option<u64> {
    let mut parts = text.split(':');
    if parts.next()? != "darwin" {
        return None;
    }
    let seconds: u64 = parts.next()?.parse().ok()?;
    let micros: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || micros >= 1_000_000 || format!("darwin:{seconds}:{micros}") != text
    {
        return None;
    }
    seconds.checked_mul(1_000_000)?.checked_add(micros)
}

/// A complete malformed record blocks all leases, as Trust's admission does.
fn claims(
    value: &Value,
    uid: u32,
    process: &mut dyn FnMut(u32, &str) -> Option<Claim>,
) -> Option<Vec<(PathBuf, Claim)>> {
    if value.get("schema")?.as_u64()? != 1 || !value.get("adopted")?.as_bool()? {
        return None;
    }
    let identity = value.get("identity")?;
    let pid = u32::try_from(identity.get("pid")?.as_u64()?).ok()?;
    if pid == 0
        || pid > i32::MAX as u32
        || value.get("pid")?.as_u64()? != u64::from(pid)
        || identity.get("uid")?.as_u64()? != u64::from(uid)
        || identity.get("executable")?.as_str()?.is_empty()
        || !Path::new(value.get("pool")?.as_str()?).is_absolute()
    {
        return None;
    }
    let started = identity.get("birth")?.as_str()?;
    if started.is_empty() {
        return None;
    }
    let leases = value.get("leases")?.as_array()?;
    if leases.is_empty() {
        return None;
    }
    let paths: Option<Vec<_>> = leases
        .iter()
        .map(|lease| {
            let path = PathBuf::from(lease.as_str()?);
            (path.is_absolute()
                && !path.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::CurDir | std::path::Component::ParentDir
                    )
                })
                && std::fs::canonicalize(&path)
                    .is_ok_and(|canonical| canonical.as_os_str() == path.as_os_str()))
            .then_some(path)
        })
        .collect();
    let paths = paths?;
    match value.get("worktree") {
        Some(Value::Null) => {}
        Some(value)
            if paths
                .iter()
                .any(|p| p.as_os_str() == std::ffi::OsStr::new(value.as_str().unwrap_or(""))) => {}
        _ => return None,
    }
    let Some(claim) = process(pid, started) else {
        return Some(Vec::new());
    };
    Some(paths.into_iter().map(|path| (path, claim)).collect())
}

fn process(pid: u32, started: &str) -> Option<Claim> {
    // A successful exec or UID change retains the adoption. Only PID and birth identify
    // the same process; failure to query it is not evidence that it has exited.
    match crate::kproc::record_read(pid) {
        Ok(None) => None,
        Ok(Some(p)) if p.zombie => None,
        Ok(Some(p)) => match birth(started) {
            Some(expected) if birth_us(&p) == expected => Some(Claim::Held(Holder {
                pid,
                birth_us: expected,
            })),
            Some(_) => None,
            None => Some(Claim::Unknown),
        },
        Err(_) => Some(Claim::Unknown),
    }
}

fn same(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.nlink() == b.nlink()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

fn record(path: &Path, uid: u32) -> Option<Value> {
    record_with(path, uid, &mut || {})
}

fn record_with(path: &Path, uid: u32, after_read: &mut dyn FnMut()) -> Option<Value> {
    let before = std::fs::symlink_metadata(path).ok()?;
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != uid
        || before.mode() & 0o777 != 0o600
        || before.len() > RECORD_CAP as u64
    {
        return None;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !same(&before, &file.metadata().ok()?) {
        return None;
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(RECORD_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    after_read();
    if bytes.len() > RECORD_CAP
        || !same(&before, &file.metadata().ok()?)
        || !same(&before, &std::fs::symlink_metadata(path).ok()?)
    {
        return None;
    }
    aterm_json::from_slice(&bytes).ok()
}

fn snapshot(root: &Path) -> Option<BTreeMap<PathBuf, Claim>> {
    if !root.is_absolute() {
        return None;
    }
    let before = match std::fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(BTreeMap::new());
        }
        Err(_) => return None,
    };
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !before.is_dir() || before.uid() != uid || before.mode() & 0o777 != 0o700 {
        return None;
    }
    let mut result = BTreeMap::new();
    for (index, entry) in std::fs::read_dir(root).ok()?.enumerate() {
        if index >= MAX_ENTRIES {
            return None;
        }
        let entry = entry.ok()?;
        if entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        for (path, claim) in claims(&record(&entry.path(), uid)?, uid, &mut process)? {
            result
                .entry(path)
                .and_modify(|held| {
                    if *held != claim {
                        *held = Claim::Unknown;
                    }
                })
                .or_insert(claim);
        }
    }
    same(&before, &std::fs::symlink_metadata(root).ok()?).then_some(result)
}

fn combine(sight: &Sight, claim: Claim) -> Sight {
    match (sight, claim) {
        (Sight::Empty | Sight::NotHeld { .. }, Claim::Held(holder)) => Sight::Sighted(holder),
        (Sight::Sighted(a), Claim::Held(b)) if *a == b => Sight::Sighted(b),
        _ => Sight::Unproven,
    }
}

pub(super) fn holds(root: &Path, pool: &Path, holder: Holder) -> Option<bool> {
    let pool = std::fs::canonicalize(pool).ok()?;
    let mut unknown = false;
    for (path, claim) in snapshot(root)? {
        let Ok(relative) = path.strip_prefix(&pool) else {
            continue;
        };
        if relative
            .parent()
            .is_some_and(|parent| parent.as_os_str().is_empty())
            && relative
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(super::slot_index)
                .is_some()
        {
            match claim {
                Claim::Held(held) if held == holder => return Some(true),
                Claim::Unknown => unknown = true,
                _ => {}
            }
        }
    }
    (!unknown).then_some(false)
}

pub(super) fn overlay(root: Option<&Path>, named: &[(u32, PathBuf)], slots: &mut [SlotObs]) {
    let claims = root.and_then(snapshot);
    for slot in slots {
        let Some(claims) = &claims else {
            slot.sight = Sight::Unproven;
            continue;
        };
        let Some((_, path)) = named.iter().find(|(index, path)| {
            *index == slot.index
                && std::fs::symlink_metadata(path)
                    .is_ok_and(|meta| (meta.dev() as u32, meta.ino()) == slot.file)
        }) else {
            slot.sight = Sight::Unproven;
            continue;
        };
        let Ok(path) = std::fs::canonicalize(path) else {
            slot.sight = Sight::Unproven;
            continue;
        };
        if let Some(claim) = claims.get(&path) {
            slot.sight = combine(&slot.sight, *claim);
        }
    }
}

#[cfg(test)]
#[path = "slots_adoptions_tests.rs"]
mod tests;
