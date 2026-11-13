// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observational legacy build admission, never a grant or a queue mutation.

#[cfg(target_os = "macos")]
use aterm_sysprobe::slots;
use aterm_sysprobe::{jobs::ProcFacts, slots::Holder};

/// A confirmed ticket's waiter and native slot holders observed in the same pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The ticket holder, including its native process birth.
    pub process: Holder,
    /// Current slot holders, never an admission grant or an assertion of their health.
    pub blockers: Vec<Holder>,
}

/// One native sampling pass's confirmed waits. Uses the account database home (the same
/// reader as `atpkg::platform::account_home`), never HOME or a request's path. Detailed
/// process facts must have been sampled before this call opens any ticket descriptor.
#[must_use]
pub fn read(facts: &[ProcFacts], now_us: u64) -> Vec<Wait> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt as _;
        let Some(home) =
            aterm_uds::account::home().and_then(|home| std::fs::canonicalize(home).ok())
        else {
            return Vec::new();
        };
        let pool = home.join(slots::SLOT_DIR);
        let dir = pool.join("admission");
        let Ok(before) = std::fs::symlink_metadata(&dir) else {
            return Vec::new();
        };
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if !before.is_dir() || before.uid() != uid || before.mode() & 0o022 != 0 {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut holders = Vec::new();
        for entry in entries.take(slots::MAX_ENTRIES).flatten() {
            let name = entry.file_name();
            let Some(digits) = name.to_str().and_then(|n| n.strip_prefix("wait-")) else {
                continue;
            };
            if digits.is_empty()
                || digits.len() > 20
                || !digits.bytes().all(|b| b.is_ascii_digit())
                || !digits.parse::<u64>().is_ok_and(|n| n > 0)
            {
                continue;
            }
            let path = entry.path();
            let Ok(fd) = aterm_uds::nofollow::open_reader(&path) else {
                continue;
            };
            let mut reader = std::fs::File::from(fd);
            if let Some(holder) = slots::waiting_ticket(&path, &mut reader, facts, now_us) {
                holders.push(holder);
            }
        }
        let Ok(after) = std::fs::symlink_metadata(dir) else {
            return Vec::new();
        };
        if !after.is_dir()
            || after.uid() != uid
            || after.mode() & 0o022 != 0
            || (before.dev(), before.ino()) != (after.dev(), after.ino())
        {
            return Vec::new();
        }
        if holders.is_empty() {
            return Vec::new();
        }
        let blockers: Vec<Holder> = slots::observe(&pool, Some(&pool.join("adopted")), 0, now_us)
            .ok()
            .into_iter()
            .flat_map(|o| o.slots)
            .filter_map(|s| match s.sight {
                slots::Sight::Sighted(h)
                    if facts
                        .iter()
                        .find(|p| p.pid == h.pid)
                        .is_none_or(|p| p.start == Some(h.birth_us)) =>
                {
                    Some(h)
                }
                _ => None,
            })
            .collect();
        holders
            .into_iter()
            .map(|process| Wait {
                process,
                blockers: blockers.clone(),
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (facts, now_us);
        Vec::new()
    }
}
