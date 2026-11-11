// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The programs a tab started that its job does not hold, ended when the tab closes, as the job ends the rest.

use std::cell::RefCell;
use std::collections::HashMap;

use aterm_types::MutexExt;

use super::WinSession;
use super::console_guard::{self, Unread};
use super::ffi::{self, HANDLE, Handle};

/// The exit code of a program ended here: the job escalation's
/// (`TerminateJobObject(job, 1)`).
const ENDED: u32 = 1;

#[cfg(test)]
thread_local! {
    /// While set, a tab spawned on this thread gets a job its programs leave (`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`), as a packaged shell's leave it.
    pub(super) static JOB_LETS_GO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`.
#[cfg(test)]
pub(super) const JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK: u32 = 0x1000;

/// [`end`] the first time it is asked of session `s`, nothing after.
pub(super) fn end_once(s: &WinSession) {
    let mut ended = s.outside_job_ended.lock_or_recover();
    if !*ended {
        end(s);
        *ended = true;
    }
}

/// End the programs below session `s`'s first process that its job does not
/// hold.
fn end(s: &WinSession) {
    if s.job == 0 {
        return;
    }
    let Some(root_born) = console_guard::started(s.process) else {
        return;
    };
    let tree = console_guard::process_tree();
    let taken = select(s.pid, root_born, &tree, open, |h: &Handle| {
        held_by(h.raw(), s.job)
    });
    for (_, process) in &taken {
        // SAFETY: a process handle the walk opened with PROCESS_TERMINATE and holds; it names the
        // process the walk vouched for.
        unsafe { ffi::TerminateProcess(process.raw(), ENDED) };
    }
}

/// A process the walk opened: the handle (which pins its pid while held), its
/// start, and its parent's pid as the kernel keeps it.
struct Opened<H> {
    handle: H,
    born: u64,
    parent: u32,
}

/// What [`end`] ends, over injected lookups: each process below `root`
/// (started at `root_born`) in `tree` ((pid, parent) pairs from one snapshot)
/// that `console_guard::below` reaches with `Unread::LeftOut` when every step
/// is vouched for, and that the tab's job does not hold (`held` false), each
/// with the handle `open` gave.
fn select<H>(
    root: u32,
    root_born: u64,
    tree: &[(u32, u32)],
    open: impl Fn(u32) -> Option<Opened<H>>,
    held: impl Fn(&H) -> bool,
) -> Vec<(u32, H)> {
    let recorded: HashMap<u32, u32> = tree.iter().copied().collect();
    let opened: RefCell<HashMap<u32, H>> = RefCell::new(HashMap::new());
    let born = |pid: u32| {
        if pid == root {
            return Some(root_born);
        }
        let process = open(pid)?;
        if recorded.get(&pid) != Some(&process.parent) {
            return None;
        }
        opened.borrow_mut().insert(pid, process.handle);
        Some(process.born)
    };
    let below = console_guard::below(root, tree, born, Unread::LeftOut);
    let mut opened = opened.into_inner();
    below
        .into_iter()
        .filter_map(|pid| opened.remove(&pid).map(|h| (pid, h)))
        .filter(|(_, h)| !held(h))
        .collect()
}

/// Open process `pid` to end it, with its start and its parent; `None` when it
/// cannot be opened so (gone, elevated, another user's) or either cannot be
/// read.
fn open(pid: u32) -> Option<Opened<Handle>> {
    let access = ffi::PROCESS_QUERY_LIMITED_INFORMATION | ffi::PROCESS_TERMINATE;
    // SAFETY: plain OpenProcess; the handle is owned by the guard.
    let handle = Handle::new(unsafe { ffi::OpenProcess(access, 0, pid) });
    if handle.raw() == 0 {
        return None;
    }
    let born = console_guard::started(handle.raw())?;
    let parent = parent_of(handle.raw())?;
    Some(Opened {
        handle,
        born,
        parent,
    })
}

/// The pid of the process that created the one behind `process` (a handle
/// with `PROCESS_QUERY_LIMITED_INFORMATION`), as the kernel keeps it; `None`
/// when it cannot be read.
fn parent_of(process: HANDLE) -> Option<u32> {
    let mut info = ffi::PROCESS_BASIC_INFORMATION::default();
    // SAFETY: a process handle the caller holds and an out-buffer of the
    // stated size; no return length is asked for.
    let status = unsafe {
        ffi::NtQueryInformationProcess(
            process,
            ffi::PROCESS_BASIC_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<ffi::PROCESS_BASIC_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        return None;
    }
    u32::try_from(info.InheritedFromUniqueProcessId).ok()
}

/// Whether `job` holds the process behind `process`, so the job ends it. A
/// query that fails answers yes: the narrower act, leaving it to the job.
fn held_by(process: HANDLE, job: HANDLE) -> bool {
    let mut held = 0i32;
    // SAFETY: a process handle the caller holds, the session's live job, an out-param.
    let ok = unsafe { ffi::IsProcessInJob(process, job, &mut held) };
    ok == 0 || held != 0
}

#[cfg(test)]
#[path = "outside_job_tests.rs"]
mod tests;
