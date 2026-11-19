// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::Value;
use aterm_sysprobe::slots;
use std::collections::BTreeMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

const CAP: u64 = slots::RECORD_CAP as u64;

fn same(a: &Metadata, b: &Metadata) -> bool {
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

fn private(meta: &Metadata, directory: bool) -> bool {
    // SAFETY: geteuid has no preconditions.
    meta.uid() == unsafe { libc::geteuid() }
        && meta.mode() & 0o022 == 0
        && if directory {
            meta.is_dir()
        } else {
            meta.is_file() && meta.nlink() == 1
        }
}

fn slot(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|n| n.strip_prefix("slot-"))
        .is_some_and(|n| {
            !n.is_empty()
                && n.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                && (n.parse::<u32>().is_ok()
                    || (n.len() == 8 && u32::from_str_radix(n, 16).is_ok()))
        })
}

fn exclusive(file: &File) -> io::Result<bool> {
    use aterm_uds::ownerlock::{Owner, probe_file};
    match probe_file(file) {
        Owner::Dead => Ok(false),
        Owner::Live => Ok(true),
        Owner::Unknown => Err(io::Error::other(
            "could not determine exclusive slot ownership",
        )),
    }
}

fn bytes(file: &mut File, path: &Path, before: &Metadata) -> Option<Vec<u8>> {
    if before.len() > CAP {
        return None;
    }
    let mut bytes = Vec::new();
    (&mut *file).take(CAP + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > CAP
        || !same(before, &file.metadata().ok()?)
        || !same(before, &std::fs::symlink_metadata(path).ok()?)
    {
        return None;
    }
    Some(bytes)
}

// Deserialize authority fields once before interpreting the dynamic document.
// Value itself intentionally has last-entry-wins keys; these fields must not.
#[derive(serde::Deserialize)]
struct Declaration {
    #[serde(rename = "v")]
    _version: Option<Value>,
    #[serde(rename = "pid")]
    _pid: Option<Value>,
    #[serde(rename = "supervisor_pid")]
    _supervisor: Option<Value>,
    #[serde(rename = "started")]
    _started: Option<Value>,
    #[serde(rename = "cwd")]
    _cwd: Option<Value>,
    #[serde(rename = "cpu_mode")]
    _mode: Option<Value>,
    #[serde(rename = "cpu_budget")]
    _budget: Option<Value>,
    #[serde(rename = "pool_slots")]
    _slots: Option<Value>,
    #[serde(rename = "lease_id")]
    _lease: Option<Value>,
}

fn declaration(data: &[u8]) -> Option<Value> {
    let _: Declaration = aterm_json::from_slice(data).ok()?;
    aterm_json::from_slice(data).ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Lease {
    owner: Holder,
    id: String,
    cores: u32,
    slots: u32,
    mode: String,
}

fn reservation(value: &Value, owner: Holder, cores: u32) -> Option<(Held, Option<Lease>)> {
    let record = slots::Record::parse(&aterm_json::to_vec(value).ok()?);
    if !matches!(record, slots::RecordRead::Rec(r) if r.pid == owner.pid) {
        return None;
    }
    let count = match value.get("pool_slots") {
        Some(v) => Some(
            u32::try_from(v.as_u64()?)
                .ok()
                .filter(|n| (1..=65536).contains(n))?,
        ),
        None => None,
    };
    let explicit = match value.get("cpu_budget") {
        Some(v) => Some(u32::try_from(v.as_u64()?).ok().filter(|n| *n > 0)?),
        None => None,
    };
    let budget = explicit.or_else(|| count.map(|slots| (cores / slots).max(1)));
    let lease = match value.get("lease_id") {
        None => None,
        Some(v) => {
            let id = v.as_str()?;
            if id.len() != 32
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            let mode = value.get("cpu_mode")?.as_str()?;
            if !matches!(mode, "fixed-v1" | "workers-v1") {
                return None;
            }
            match (explicit, count) {
                (Some(budget), Some(count)) => Some(Lease {
                    owner,
                    id: id.into(),
                    cores: budget,
                    slots: count,
                    mode: mode.into(),
                }),
                // A whole-lease budget is repeated on each descriptor. Without
                // its pool identity those rows cannot safely be added together.
                (Some(_), None) => return None,
                (None, _) => None,
            }
        }
    };
    Some((
        match budget {
            Some(width) => Held::verified(owner, width.min(cores.max(1))),
            None => Held::unmeasured(Some(owner), cores),
        },
        lease,
    ))
}

/// Native runtime leases are one executing CPU unit each. Their exact bounded
/// producer format and hex filename differ from build-slot's JSON reservations.
fn native_reservation(data: &[u8], named: &Path, meta: &Metadata) -> Option<Held> {
    let record = crate::build_cpu::Record::parse(std::str::from_utf8(data).ok()?).ok()?;
    if record.slot >= crate::build_cpu::MAX_SLOTS
        || named.file_name()? != std::ffi::OsStr::new(&format!("slot-{:08x}", record.slot))
        || record.owner.birth.is_none()
        || crate::build_cpu::process_cpu::read(record.owner.pid)?.owner != record.owner
    {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        let p = aterm_sysprobe::kproc::record_read(record.owner.pid).ok()??;
        // SAFETY: geteuid has no preconditions.
        if p.zombie || p.uid != unsafe { libc::geteuid() } {
            return None;
        }
        let holder = Holder {
            pid: p.pid,
            birth_us: p
                .start_s
                .saturating_mul(1_000_000)
                .saturating_add(u64::from(p.start_us)),
        };
        if !holder.holds_file(meta.dev() as u32, meta.ino())
            || crate::build_cpu::process_cpu::read(record.owner.pid)?.owner != record.owner
        {
            return None;
        }
        Some(Held::verified(holder, 1))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        None
    }
}

fn insert(
    held: &mut HeldSlots,
    leases: &mut BTreeMap<String, Lease>,
    row: Held,
    lease: Option<Lease>,
) -> Option<()> {
    if let Some(lease) = lease {
        match leases.get(&lease.id) {
            Some(old) if old == &lease => return Some(()),
            Some(_) => return None,
            None => {
                leases.insert(lease.id.clone(), lease);
            }
        }
    }
    held.holders.push(row);
    Some(())
}

fn adoption_owner(
    adoption: Option<&slots::AdoptedSlot>,
    file: (u32, u64),
    native: Option<Holder>,
) -> Option<Option<Holder>> {
    let Some(adoption) = adoption else {
        return Some(None);
    };
    if adoption.file != file {
        return None;
    }
    match adoption.claim {
        slots::AdoptionClaim::Held(owner) if native.is_none_or(|writer| writer == owner) => {
            Some(Some(owner))
        }
        _ => None,
    }
}

fn pool(
    path: &Path,
    adoptions: &Path,
    cores: u32,
    leases: &mut BTreeMap<String, Lease>,
) -> Option<HeldSlots> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(HeldSlots::default()),
        Err(_) => return None,
    };
    if !private(&before, true) {
        return None;
    }
    #[cfg(target_os = "macos")]
    let adopted = slots::adoptions_for_pool(adoptions, path)?;
    #[cfg(not(target_os = "macos"))]
    let adopted: BTreeMap<std::path::PathBuf, slots::AdoptedSlot> = {
        match std::fs::read_dir(adoptions) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(entries) => {
                if entries.enumerate().any(|(index, entry)| {
                    index >= slots::MAX_ENTRIES
                        || entry.is_err()
                        || entry.is_ok_and(|entry| {
                            entry.path().extension().is_some_and(|ext| ext == "json")
                        })
                }) {
                    return None;
                }
            }
            Err(_) => return None,
        }
        BTreeMap::new()
    };
    let mut seen_adoptions = std::collections::BTreeSet::new();
    let mut held = HeldSlots::default();
    let mut files = std::collections::BTreeSet::new();
    for (index, entry) in std::fs::read_dir(path).ok()?.enumerate() {
        if index >= slots::MAX_ENTRIES {
            return None;
        }
        let entry = entry.ok()?;
        if !slot(&entry.file_name()) {
            continue;
        }
        let named = entry.path();
        let meta = std::fs::symlink_metadata(&named).ok()?;
        if !private(&meta, false) {
            return None;
        }
        if !files.insert((meta.dev(), meta.ino())) {
            return None;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&named)
            .ok()?;
        if !same(&meta, &file.metadata().ok()?) {
            return None;
        }
        let locked = exclusive(&file).ok()?;
        let canonical = std::fs::canonicalize(&named).ok()?;
        let adoption = adopted.get(&canonical);
        if adoption.is_some() {
            seen_adoptions.insert(canonical);
        }
        if !locked && adoption.is_none() {
            continue;
        }
        let data = bytes(&mut file, &named, &meta)?;
        let value = declaration(&data);
        let writer = match slots::Record::parse(&data) {
            slots::RecordRead::Rec(record) => {
                slots::record_holder(record, meta.dev() as u32, meta.ino())
            }
            _ => None,
        };
        let native = if data.starts_with(b"trust-build-cpu-") {
            native_reservation(&data, &named, &meta).map(|row| (row, None))
        } else {
            writer.and_then(|owner| reservation(value.as_ref()?, owner, cores))
        };
        let observed_owner = writer.or_else(|| native.as_ref().and_then(|(row, _)| row.process));
        let adopted_owner =
            adoption_owner(adoption, (meta.dev() as u32, meta.ino()), observed_owner)?;
        let (row, lease) = if locked {
            // An adoption does not explain an unknown EX holder or override a
            // conflicting live writer. EX accounting needs its actual writer.
            native?
        } else {
            let owner = adopted_owner?;
            // The real `build-slot adopt` producer leaves slot bytes empty/stale
            // and publishes neither its budget nor its configurable pool size.
            // Neither old file bytes nor today's default describes that grant.
            (Held::unmeasured(Some(owner), cores), None)
        };
        insert(&mut held, leases, row, lease)?;
    }
    if adopted.keys().any(|path| !seen_adoptions.contains(path)) {
        return None;
    }
    // Directory mutation during this bounded census is unknown, not a free pool.
    same(&before, &std::fs::symlink_metadata(path).ok()?).then_some(held)
}

pub(super) fn read(path: &Path, cores: u32) -> HeldSlots {
    let mut leases = BTreeMap::new();
    let adoptions = path.join("adopted");
    let Some(mut held) = pool(path, &adoptions, cores, &mut leases) else {
        return HeldSlots::unknown(cores);
    };
    let Some(runtime) = pool(&path.join("runtime-v2"), &adoptions, cores, &mut leases) else {
        return HeldSlots::unknown(cores);
    };
    held.holders.extend(runtime.holders);
    held
}

#[cfg(test)]
#[path = "build_slot_held_native_tests.rs"]
mod tests;
