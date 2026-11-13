// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The programs a tab started that its job does not hold, ended when the tab closes, as the job ends the rest.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

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

#[cfg(test)]
thread_local! {
    /// How many times [`images`] has walked the machine's processes on this thread.
    pub(super) static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where a session's shell runs the programs its commands start, as far as its listings and
/// its own image have shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProgramsRun {
    /// Nothing has shown it yet: no listing has held a program.
    Unknown,
    /// In the tab's job: a listing of the job held one.
    InJob,
    /// Outside it: the shell is a packaged app ([`packaged`]), or a walk below the shell found a
    /// program the job does not hold.
    OutsideJob,
}

/// What a session has learned of where its shell's programs run ([`ProgramsRun`]): it moves from
/// `Unknown` once and then stays, so a tab is asked the costly question (a walk of the
/// machine's processes) only until it has answered.
pub(super) struct Learned(AtomicU8);

impl Learned {
    const UNKNOWN: u8 = 0;
    const IN_JOB: u8 = 1;
    const OUTSIDE_JOB: u8 = 2;

    /// What is known as the session starts, from its shell's image path (`None`: unread).
    pub(super) fn of_shell(image: Option<&Path>) -> Self {
        Self(AtomicU8::new(if image.is_some_and(packaged) {
            Self::OUTSIDE_JOB
        } else {
            Self::UNKNOWN
        }))
    }

    pub(super) fn get(&self) -> ProgramsRun {
        match self.0.load(Ordering::Relaxed) {
            Self::IN_JOB => ProgramsRun::InJob,
            Self::OUTSIDE_JOB => ProgramsRun::OutsideJob,
            _ => ProgramsRun::Unknown,
        }
    }

    /// A listing found a program `at`; kept only where nothing was known.
    pub(super) fn found(&self, at: ProgramsRun) {
        let at = match at {
            ProgramsRun::Unknown => return,
            ProgramsRun::InJob => Self::IN_JOB,
            ProgramsRun::OutsideJob => Self::OUTSIDE_JOB,
        };
        let _ = self
            .0
            .compare_exchange(Self::UNKNOWN, at, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Whether a listing whose job holds no program walks below the shell for those outside it: a
/// shell known to run its programs outside its job always, one known to run them in it never
/// (its empty job is a shell at rest), and one not yet known only when the listing is one that
/// may `discover` (a block's start or end, not the pace between).
pub(super) const fn walks(known: ProgramsRun, discover: bool) -> bool {
    match known {
        ProgramsRun::OutsideJob => true,
        ProgramsRun::InJob => false,
        ProgramsRun::Unknown => discover,
    }
}

/// Whether `image` is a packaged app's: under a `WindowsApps` folder, where Windows keeps a
/// Store install (`C:\Program Files\WindowsApps\Microsoft.PowerShell_<version>\pwsh.exe`) and
/// its execution alias (`%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe`). Windows starts such
/// a shell's programs outside the tab's job.
pub(super) fn packaged(image: &Path) -> bool {
    image
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

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
    let to_end = |pid| {
        open(
            pid,
            ffi::PROCESS_QUERY_LIMITED_INFORMATION | ffi::PROCESS_TERMINATE,
        )
    };
    let taken = select(s.pid, root_born, &tree, to_end, |h: &Handle| {
        held_by(h.raw(), s.job)
    });
    for (_, process) in &taken {
        // SAFETY: a process handle the walk opened with PROCESS_TERMINATE and holds; it names the
        // process the walk vouched for.
        unsafe { ffi::TerminateProcess(process.raw(), ENDED) };
    }
}

/// The programs below session `s`'s first process that its job does not hold, each with its
/// image's path: what [`end`] would end, read and left running. A packaged shell's programs (a
/// Store pwsh's) are these, so a listing of the job alone names none of what such a tab runs.
/// Each process is opened to read its start, its parent and its image path, and nothing else of
/// it is read.
pub(super) fn images(s: &WinSession) -> Vec<(u32, PathBuf)> {
    if s.job == 0 {
        return Vec::new();
    }
    let Some(root_born) = console_guard::started(s.process) else {
        return Vec::new();
    };
    #[cfg(test)]
    WALKS.with(|walks| walks.set(walks.get() + 1));
    let tree = console_guard::process_tree();
    let read = |pid| open(pid, ffi::PROCESS_QUERY_LIMITED_INFORMATION);
    select(s.pid, root_born, &tree, read, |h: &Handle| {
        held_by(h.raw(), s.job)
    })
    .into_iter()
    .filter_map(|(pid, process)| {
        console_guard::image_of_handle(process.raw()).map(|image| (pid, image))
    })
    .collect()
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

/// Open process `pid` with `access` (which holds `PROCESS_QUERY_LIMITED_INFORMATION`), with its
/// start and its parent; `None` when it cannot be opened so (gone, elevated, another user's) or
/// either cannot be read.
fn open(pid: u32, access: u32) -> Option<Opened<Handle>> {
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
