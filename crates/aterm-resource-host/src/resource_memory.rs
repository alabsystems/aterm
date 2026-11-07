// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded memory observations for the actor's explicit owned dedicated groups.
//! A census is not containment, a hard quota, control authority or closure evidence.

use aterm_uds::process::{self, GroupMembers, Lineage, ProcessPresence};
use std::{collections::BTreeSet, io};

const MAX_MEMBERS: usize = aterm_sysprobe::procs::MAX_PROCESSES;

fn unavailable(why: &str) -> io::Error {
    io::Error::other(why)
}

trait Probe {
    fn members(&self, group: u32) -> Option<GroupMembers>;
    fn group_presence(&self, group: u32) -> ProcessPresence;
    /// The maintained native reader only returns live same-UID lineage.
    fn lineage(&self, pid: i32) -> Option<Lineage>;
    fn memory(&self, pid: u32) -> io::Result<u64>;
    fn presence(&self, pid: i32) -> ProcessPresence;
}

struct Native;

impl Probe for Native {
    fn members(&self, group: u32) -> Option<GroupMembers> {
        process::group_members(group)
    }
    fn group_presence(&self, group: u32) -> ProcessPresence {
        process::group_presence(group)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        process::read_lineage(pid)
    }
    fn memory(&self, pid: u32) -> io::Result<u64> {
        aterm_sysprobe::procs::pid_memory_bytes(pid)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        process::process_presence(pid)
    }
}

/// Read every listed same-UID process once, under stable native identity and group reads.
/// Groups are supplied from actual actor custody; accepting a number grants no authority.
/// Missing or incomplete coverage refuses the whole sample, never a partial total.
///
/// # Errors
/// A group census, same-UID identity or memory read is unavailable or changes; the bounded
/// input is invalid/incomplete, or a per-PID counter or aggregate arithmetic overflows.
pub(crate) fn sample(groups: &[i32]) -> io::Result<u64> {
    sample_with(groups, &Native)
}

/// A complete current census with no other process capable of executing.
/// This supplies no control or closure authority. A controller may use it
/// only while retaining its actual unreaped dedicated leader.
pub(crate) fn other_members_exited(leader: u32) -> bool {
    other_members_exited_with(leader, &Native)
}

fn other_members_exited_with(leader: u32, probe: &impl Probe) -> bool {
    if leader <= 1 {
        return false;
    }
    if probe.group_presence(leader) == ProcessPresence::Absent {
        return true;
    }
    #[cfg(target_vendor = "apple")]
    {
        // XNU proc_listpids holds proc_list_lock across allproc AND zombproc.
        // A complete singleton census therefore leaves no other member that
        // could fork. Do not replace this with later per-PID exit readings:
        // a listed parent could fork an unlisted child and exit between them.
        // Empty/error and zombie siblings retain the actual unreaped pin.
        // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
        probe.members(leader).is_some_and(|members| {
            members.complete
                && i32::try_from(leader).is_ok_and(|pid| members.pids.as_slice() == [pid])
        })
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        // The /proc walk is not an atomic membership census. This host's
        // retained launch path refuses before spawn until its control is wired.
        false
    }
}

fn sample_with(groups: &[i32], probe: &impl Probe) -> io::Result<u64> {
    if groups.len() > MAX_MEMBERS {
        return Err(unavailable(
            "owned-group memory input exceeds the census budget",
        ));
    }
    let groups = groups
        .iter()
        .map(|group| {
            u32::try_from(*group)
                .ok()
                .filter(|group| *group > 1)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "memory sample requires dedicated positive groups",
                    )
                })
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    let mut pids = BTreeSet::new();
    for group in &groups {
        let members = probe
            .members(*group)
            .ok_or_else(|| unavailable("owned-group census is unavailable"))?;
        if !members.complete || members.pids.len() > process::MAX_GROUP_PIDS {
            return Err(unavailable("owned-group census is incomplete"));
        }
        if members.pids.is_empty() && probe.group_presence(*group) != ProcessPresence::Absent {
            return Err(unavailable(
                "empty census does not establish memory coverage",
            ));
        }
        for pid in members.pids {
            if pid <= 1 {
                return Err(unavailable("owned-group census contains an invalid pid"));
            }
            pids.insert(pid);
            if pids.len() > MAX_MEMBERS {
                return Err(unavailable(
                    "owned-group census exceeds the observation budget",
                ));
            }
        }
    }
    let mut total = 0u64;
    for pid in pids {
        let Some(before) = probe.lineage(pid) else {
            exited_or_error(
                probe,
                pid,
                unavailable("process identity or same-UID coverage is unavailable"),
            )?;
            continue;
        };
        if before.identity.pid != pid || !groups.contains(&before.pgid) {
            return Err(unavailable("process left the observed owned groups"));
        }
        let bytes = match probe.memory(pid as u32) {
            Ok(bytes) => bytes,
            // Malformed/truncated counters or arithmetic overflow are not an exit race.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(error),
            Err(error) => {
                exited_or_error(probe, pid, error)?;
                continue;
            }
        };
        let Some(after) = probe.lineage(pid) else {
            exited_or_error(
                probe,
                pid,
                unavailable("process identity became unavailable during memory sampling"),
            )?;
            continue;
        };
        if after.identity != before.identity
            || after.uniqueid != before.uniqueid
            || after.pgid != before.pgid
        {
            return Err(unavailable(
                "process identity or group changed during memory sampling",
            ));
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| unavailable("owned-group memory total overflowed"))?;
    }
    Ok(total)
}

fn exited_or_error(probe: &impl Probe, pid: i32, error: io::Error) -> io::Result<()> {
    if matches!(
        probe.presence(pid),
        ProcessPresence::Exited | ProcessPresence::Absent
    ) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
#[path = "resource_memory/tests.rs"]
mod tests;
