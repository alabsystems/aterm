// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this process runs inside a release cut's own process tree.
//!
//! The last core requests of an identity are kept for the release cutter
//! ([`super::RateBook::refuse_background`]). A cut reads GitHub through more than its own
//! client: its publication engine and every tool that engine starts read through the `agh`
//! command. Those are the cutter too, and they prove it by what only a cut has: a release-line
//! record of this host ([`aterm_ctl::release_holder::DIRECTORY`]), which a cutter writes when it
//! takes its fleet line, names a holder that is this process or one of its live ancestors, by
//! pid, boot and kernel start. No variable is read, so nothing a session set and forgot, and
//! nothing a child inherited past its cut, makes a process the cutter: once the holder exits,
//! its children are no longer below it and its record names no running process.

use std::path::Path;

/// The most ancestors read: a cut's engine runs its client a dozen processes below the cutter.
#[cfg(unix)]
const MAX_ANCESTORS: usize = 64;

/// The most registry entries listed. A host keeps one small record per line it ever took.
#[cfg(unix)]
const MAX_ENTRIES: usize = 4096;

/// The longest record read: the bound the fleet survey reads the same files under.
#[cfg(unix)]
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// The kernel reads the proof rests on.
#[cfg(unix)]
pub(crate) struct Kernel<'a> {
    /// This boot's id.
    pub boot_id: &'a dyn Fn() -> Option<String>,
    /// A pid's kernel start in this boot.
    pub start_of: &'a dyn Fn(u32) -> Option<u64>,
    /// A pid's parent.
    pub parent_of: &'a dyn Fn(u32) -> Option<u32>,
    /// The account this process runs as.
    pub uid: u32,
}

/// One running process of this process's line of descent: its pid and kernel start.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    pid: u32,
    start: u64,
}

/// This process and its ancestors, nearest first, each with the start it kept across the read
/// of its parent, so a pid that changed hands mid-walk ends the line there. A process whose
/// start does not read ends it too: nothing above an unread process is proven an ancestor.
#[cfg(unix)]
fn lineage(own: u32, kernel: &Kernel<'_>) -> Vec<Member> {
    let mut line: Vec<Member> = Vec::new();
    let mut pid = own;
    while line.len() < MAX_ANCESTORS && pid > 1 && line.iter().all(|member| member.pid != pid) {
        let Some(start) = (kernel.start_of)(pid) else {
            break;
        };
        let parent = (kernel.parent_of)(pid);
        if (kernel.start_of)(pid) != Some(start) {
            break;
        }
        line.push(Member { pid, start });
        let Some(parent) = parent else {
            break;
        };
        pid = parent;
    }
    line
}

/// Whether the record at `path` is this account's own regular file and names exactly `member`
/// as its holder, in boot `boot`.
#[cfg(unix)]
fn names(path: &Path, member: Member, boot: &str, uid: u32) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let Ok((file, metadata)) = aterm_uds::regular_file::open(path) else {
        return false;
    };
    if metadata.uid() != uid || metadata.len() > MAX_RECORD_BYTES {
        return false;
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_RECORD_BYTES
    {
        return false;
    }
    let Ok(record) = aterm_json::from_slice::<aterm_json::Value>(&bytes) else {
        return false;
    };
    let holder = &record["holder"];
    holder["pid"].as_u64() == Some(u64::from(member.pid))
        && holder["start"].as_u64() == Some(member.start)
        && holder["boot"].as_str() == Some(boot)
}

/// [`in_cut_tree`] over `kernel`'s reads, for the process `own`.
#[cfg(unix)]
pub(crate) fn in_cut_tree_with(registry: &Path, own: u32, kernel: &Kernel<'_>) -> bool {
    let Ok(entries) = std::fs::read_dir(registry) else {
        return false;
    };
    let Some(boot) = (kernel.boot_id)() else {
        return false;
    };
    let line: Vec<(Member, String)> = lineage(own, kernel)
        .into_iter()
        .map(|member| {
            let identity = aterm_ctl::release_holder::Identity {
                pid: member.pid,
                boot: boot.clone(),
                start: member.start,
            };
            (member, identity.record_suffix())
        })
        .collect();
    if line.is_empty() {
        return false;
    }
    entries.take(MAX_ENTRIES).flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        line.iter().any(|(member, suffix)| {
            // The name only finds the record; what it holds, and that this account wrote it,
            // is the proof.
            name.ends_with(suffix.as_str())
                && names(&entry.path(), *member, &boot, kernel.uid)
                // The holder still runs, the very process: its start read again after its
                // record.
                && (kernel.start_of)(member.pid) == Some(member.start)
        })
    })
}

/// Whether a release-line record in `registry` names this process, or one of its live
/// ancestors, as its holder: the same pid, this boot, the kernel start that process still has.
/// A record of a process that is no ancestor, of an earlier holder of a pid, of another boot,
/// one another account wrote, and no record at all are each `false`. Nothing but this host's
/// kernel and that one folder is read.
#[must_use]
pub fn in_cut_tree(registry: &Path) -> bool {
    #[cfg(unix)]
    {
        use aterm_uds::process::{boot_id, read_process_parent, read_process_start};
        let native = |pid: u32| i32::try_from(pid).ok();
        in_cut_tree_with(
            registry,
            std::process::id(),
            &Kernel {
                boot_id: &boot_id,
                start_of: &|pid| read_process_start(native(pid)?).map(|start| start.boot.0),
                parent_of: &|pid| read_process_parent(native(pid)?),
                uid: aterm_uds::peer::our_uid(),
            },
        )
    }
    #[cfg(not(unix))]
    {
        let _ = registry;
        false
    }
}

#[cfg(all(test, unix))]
#[path = "cutter_tests.rs"]
mod tests;
