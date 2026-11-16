// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Service coalition observations and their deterministic fold.
//! Image reads follow a caller-supplied footprint cutoff; memory and restart policy stay above.

use std::collections::HashMap;

/// macOS keeps the uids below this for its own accounts (root, `_windowserver`, `_spotlight`).
pub const SYSTEM_UID_BELOW: u32 = 500;

/// `nobody`'s uid (-2), a system account above [`SYSTEM_UID_BELOW`].
pub const NOBODY_UID: u32 = u32::MAX - 1;

/// Whether `uid` is one of the system's own accounts.
#[must_use]
pub const fn system_uid(uid: u32) -> bool {
    uid < SYSTEM_UID_BELOW || uid == NOBODY_UID
}

/// Whether `pid` names `kernel_task` (pid 0) or launchd (pid 1), the members that
/// identify the kernel's own coalition and are excluded from the service fold.
#[must_use]
pub const fn kernel_member(pid: u32) -> bool {
    pid <= 1
}

/// The borrowed identity a caller asks a native reader to observe. This carries no restart
/// policy, signature requirement or authorization to act on the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    /// The kernel's short process name.
    pub name: &'a str,
    /// Its image's exact path.
    pub path: &'a str,
    /// The account it runs as.
    pub uid: u32,
}

/// One service as read: a resource coalition of the system's own accounts and its lead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Service {
    /// The coalition's id.
    pub coalition: u64,
    /// Its lead's pid: its earliest-born live member.
    pub pid: u32,
    /// When the lead started: unix seconds.
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
    /// The lead's uid.
    pub uid: u32,
    /// The kernel's short name for the lead (`p_comm`): data, never an instruction.
    pub name: String,
    /// The lead's image, where read: at or above the cutoff passed to [`fold`].
    pub path: Option<String>,
    /// Its live members.
    pub live: u32,
    /// The coalition's live footprint, bytes.
    pub footprint: u64,
}

impl Service {
    /// This observation's episode label, `<pid>-<start second>`.
    #[must_use]
    pub fn episode(&self) -> String {
        episode(self.pid, self.start_s)
    }
}

/// `<pid>-<start second>`: the process an episode is of ([`Service::episode`]).
#[must_use]
pub fn episode(pid: u32, start_s: u64) -> String {
    format!("{pid}-{start_s}")
}

/// One live process as a reader saw it: the kernel's record of it and its resource coalition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    /// Its pid.
    pub pid: u32,
    /// Its uid.
    pub uid: u32,
    /// The kernel's short name for it (`p_comm`).
    pub name: String,
    /// When it started: unix seconds.
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
    /// Its resource coalition's id.
    pub coalition: u64,
}

impl Member {
    /// The process, `<pid>-<start second>` ([`episode`]).
    #[must_use]
    pub fn episode(&self) -> String {
        episode(self.pid, self.start_s)
    }
}

/// The reading of a machine with `total` bytes of memory whose live processes are `members`:
/// each coalition whose every member runs as a system account ([`system_uid`]) and none is the
/// kernel's ([`kernel_member`]) is a service, led by its earliest-born member (the lower pid on a
/// tie), with its footprint as `footprint` reads it by coalition id. A coalition whose footprint
/// does not read is one of [`Reading::unread`], by its lead. `path` reads the image of each lead
/// whose footprint reaches the caller's `read_paths_from` cutoff. The memory strain is left
/// unread ([`Reading::pressure_warn`], [`Reading::swapped_kib`]) for the reader to fill.
pub fn fold(
    members: &[Member],
    total: u64,
    read_paths_from: u64,
    mut footprint: impl FnMut(u64) -> Option<u64>,
    mut path: impl FnMut(u32) -> Option<String>,
) -> Reading {
    let mut groups: HashMap<u64, Vec<&Member>> = HashMap::new();
    for m in members {
        groups.entry(m.coalition).or_default().push(m);
    }
    let mut out = Vec::new();
    let mut unread = Vec::new();
    for (coalition, group) in groups {
        if !group
            .iter()
            .all(|m| !kernel_member(m.pid) && system_uid(m.uid))
        {
            continue;
        }
        let Some(lead) = group.iter().min_by_key(|m| (m.start_s, m.start_us, m.pid)) else {
            continue;
        };
        let Some(footprint) = footprint(coalition) else {
            unread.push((*lead).clone());
            continue;
        };
        out.push(Service {
            coalition,
            pid: lead.pid,
            start_s: lead.start_s,
            start_us: lead.start_us,
            uid: lead.uid,
            name: lead.name.clone(),
            path: None,
            live: u32::try_from(group.len()).unwrap_or(u32::MAX),
            footprint,
        });
    }
    out.sort_by(|a, b| {
        b.footprint
            .cmp(&a.footprint)
            .then(a.coalition.cmp(&b.coalition))
    });
    unread.sort_by_key(|m| (m.coalition, m.pid));
    for service in out.iter_mut().filter(|s| s.footprint >= read_paths_from) {
        service.path = path(service.pid);
    }
    Reading {
        total,
        services: out,
        unread,
        pressure_warn: None,
        swapped_kib: None,
    }
}

/// One read of every service, and of the memory strain they are read against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The machine's physical memory, bytes.
    pub total: u64,
    /// Every service whose footprint was read, largest first.
    pub services: Vec<Service>,
    /// The lead of each service whose footprint did not read: not under its bar, unknown.
    pub unread: Vec<Member>,
    /// Whether the kernel's memory pressure level is past normal; `None` where unread.
    pub pressure_warn: Option<bool>,
    /// KiB swapped in plus out since boot; `None` where unread.
    pub swapped_kib: Option<u64>,
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
