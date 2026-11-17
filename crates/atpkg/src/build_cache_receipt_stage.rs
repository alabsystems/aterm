// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unpublished receipt stages left by a writer that no longer exists. They carry no
//! custody authority: publication is the atomic rename of `receipt` into the index.
//! Keep their bytes, but do not let a recognized abandoned stage poison clearance.

use std::path::Path;

pub(super) fn abandoned(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let Some(pid) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(writer)
    else {
        return false;
    };
    let Ok(directory) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !directory.is_dir() || directory.uid() != super::euid() || directory.mode() & 0o777 != 0o700
    {
        return false;
    }
    let Ok(mut entries) = std::fs::read_dir(path) else {
        return false;
    };
    if let Some(entry) = entries.next() {
        let Ok(entry) = entry else { return false };
        if entry.file_name() != "receipt" || entries.next().is_some() {
            return false;
        }
        let Ok(file) = std::fs::symlink_metadata(entry.path()) else {
            return false;
        };
        if !file.is_file()
            || file.uid() != directory.uid()
            || file.mode() & 0o077 != 0
            || file.nlink() != 1
            || file.len() > super::MAX_RECORD as u64
        {
            return false;
        }
    }
    // Only ESRCH establishes absence. Permission failures and all uncertain probes retain
    // the entry. A reused PID conservatively retains it until that process also ends.
    // SAFETY: signal zero only probes the positive, representable PID parsed below.
    (unsafe { libc::kill(pid, 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn writer(name: &str) -> Option<i32> {
    let mut parts = name.strip_prefix("cache-receipt-")?.split('_');
    let text = parts.next()?;
    let pid = text.parse::<i32>().ok()?;
    if pid <= 0 || pid.to_string() != text {
        return None;
    }
    let random = parts.next()?;
    if random.len() != 16
        || !random
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let count = parts.next()?;
    if count.parse::<u64>().ok()?.to_string() != count || parts.next().is_some() {
        return None;
    }
    Some(pid)
}
