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
/// hold (the [module docs](self)).
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
        // SAFETY: a process handle the walk opened with PROCESS_TERMINATE and holds.
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
    // SAFETY: plain OpenProcess.
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
    // SAFETY: a process handle the caller holds and an out-buffer of the stated size.
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
mod tests {
    use super::*;

    /// A stand-in for [`open`]: each process's start and kernel-kept parent
    /// (absent: it cannot be opened).
    fn opener(table: &[(u32, u64, u32)]) -> impl Fn(u32) -> Option<Opened<u32>> + '_ {
        |pid| {
            table
                .iter()
                .find(|&&(p, _, _)| p == pid)
                .map(|&(p, born, parent)| Opened {
                    handle: p,
                    born,
                    parent,
                })
        }
    }

    fn pids(mut taken: Vec<(u32, u32)>) -> Vec<u32> {
        taken.sort_unstable();
        taken.into_iter().map(|(pid, _)| pid).collect()
    }

    #[test]
    fn the_programs_below_the_shell_outside_its_job_are_taken() {
        let tree = [
            (4, 1),
            (10, 4),
            (11, 10),
            (12, 10),
            (13, 12),
            (14, 12),
            (15, 11),
            (20, 4),
            (21, 20),
        ];
        let table = [
            (4, 10, 1),
            (11, 110, 10),
            (12, 120, 10),
            (13, 130, 12),
            (14, 131, 12),
            (15, 150, 11),
            (20, 105, 4),
            (21, 125, 20),
        ];
        let in_job = |h: &u32| *h == 11;
        let taken = select(10, 100, &tree, opener(&table), in_job);
        assert_eq!(pids(taken), [12, 13, 14, 15]);
        let none_held = select(10, 100, &tree, opener(&table), |_: &u32| false);
        assert_eq!(pids(none_held), [11, 12, 13, 14, 15]);
        let all_held = select(10, 100, &tree, opener(&table), |_: &u32| true);
        assert_eq!(pids(all_held), Vec::<u32>::new());
    }

    #[test]
    fn a_pid_reused_after_the_snapshot_is_left_with_its_tree() {
        let tree = [(10, 4), (12, 10), (13, 12), (11, 10)];
        let table = [(12, 300, 30), (13, 130, 12), (11, 110, 10)];
        let taken = select(10, 100, &tree, opener(&table), |_: &u32| false);
        assert_eq!(pids(taken), [11]);
    }

    #[test]
    fn an_orphan_of_an_earlier_holder_of_the_shells_pid_is_left() {
        let tree = [(10, 4), (30, 10), (31, 30), (11, 10)];
        let table = [(30, 50, 10), (31, 60, 30), (11, 110, 10)];
        let taken = select(10, 100, &tree, opener(&table), |_: &u32| false);
        assert_eq!(pids(taken), [11]);
    }

    #[test]
    fn a_process_that_cannot_be_opened_is_left_with_its_tree() {
        let tree = [(10, 4), (12, 10), (13, 12), (11, 10)];
        let table = [(13, 130, 12), (11, 110, 10)];
        let taken = select(10, 100, &tree, opener(&table), |_: &u32| false);
        assert_eq!(pids(taken), [11]);
    }

    use super::super::{
        AttrList, close_master, end_programs_outside_jobs, read, session, spawn_shell_with_pid,
        write_all,
    };
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The end-to-end tests below one at a time: a quit's sweep ends the
    /// programs outside every open session's job, another test's too.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// A wait for something that must happen: a hang detector, never a budget.
    const HANG_DETECTOR_MS: u32 = 60_000;
    /// `CREATE_NO_WINDOW`: a stand-in gets a console of its own with no window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// `OpenProcess` right to name a process as a new one's parent.
    const PROCESS_CREATE_PROCESS: u32 = 0x0080;
    /// `OpenProcess` right to wait on a process.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    /// `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`.
    const PROC_THREAD_ATTRIBUTE_PARENT_PROCESS: usize = 0x0002_0000;

    /// A program the test watches: a window-less `cmd.exe` running a long
    /// `ping`, as a pwsh tab's `CreateNoWindow` child runs one (or the `ping`
    /// below it), ended on drop if still running.
    struct Watched {
        process: Handle,
        pid: u32,
    }

    impl Watched {
        /// Start the `cmd.exe`, naming process `parent` its parent (it then
        /// runs below it, and in its parent's jobs that keep their
        /// processes), or as this test's own child for `None`.
        fn start(parent: Option<u32>) -> Self {
            let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let line = format!(r#""{system}\System32\cmd.exe" /d /c ping -n 600 127.0.0.1 >nul"#);
            let mut line: Vec<u16> = line.encode_utf16().chain([0]).collect();
            let mut attr = AttrList::new(1).expect("an attribute list");
            // SAFETY: plain OpenProcess.
            let parent = parent.map(|pid| {
                Handle::new(unsafe { ffi::OpenProcess(PROCESS_CREATE_PROCESS, 0, pid) })
            });
            let mut parent_raw = parent.as_ref().map_or(0, Handle::raw);
            if parent.is_some() {
                assert_ne!(parent_raw, 0, "the parent opens to start a child under it");
                // SAFETY: an initialized list.
                let ok = unsafe {
                    ffi::UpdateProcThreadAttribute(
                        attr.ptr(),
                        0,
                        PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
                        std::ptr::addr_of_mut!(parent_raw).cast(),
                        std::mem::size_of::<isize>(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                assert_ne!(ok, 0, "the parent attribute is set");
            }
            // SAFETY: zeroed STARTUPINFOEXW is valid.
            let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
            si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
            si.lpAttributeList = attr.ptr();
            // SAFETY: PROCESS_INFORMATION is POD out-memory.
            let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: a NUL-terminated, writable command line.
            let created = unsafe {
                ffi::CreateProcessW(
                    std::ptr::null(),
                    line.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    ffi::EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &si.StartupInfo,
                    &mut pi,
                )
            };
            assert_ne!(created, 0, "{}", std::io::Error::last_os_error());
            drop(Handle::new(pi.hThread));
            Self {
                process: Handle::new(pi.hProcess),
                pid: pi.dwProcessId,
            }
        }

        /// The `ping` the `cmd.exe` starts, once it runs: a process whose
        /// kernel-kept parent is this one and that started after it.
        fn child(&self) -> Self {
            let born = console_guard::started(self.process.raw()).expect("a start");
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            loop {
                for (pid, ppid) in console_guard::process_tree() {
                    if ppid != self.pid {
                        continue;
                    }
                    let access = SYNCHRONIZE
                        | ffi::PROCESS_QUERY_LIMITED_INFORMATION
                        | ffi::PROCESS_TERMINATE;
                    // SAFETY: plain OpenProcess.
                    let process = Handle::new(unsafe { ffi::OpenProcess(access, 0, pid) });
                    let ours = process.raw() != 0
                        && parent_of(process.raw()) == Some(self.pid)
                        && console_guard::started(process.raw()).is_some_and(|t| t >= born)
                        && console_guard::image_of_handle(process.raw()).is_some_and(|image| {
                            image
                                .file_name()
                                .is_some_and(|f| f.eq_ignore_ascii_case("PING.EXE"))
                        });
                    if ours {
                        return Self { process, pid };
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the ping below {} never started",
                    self.pid
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn running(&self) -> bool {
            // SAFETY: a process handle this guard holds.
            unsafe { ffi::WaitForSingleObject(self.process.raw(), 0) == ffi::WAIT_TIMEOUT }
        }

        /// Whether it ended (the wait is a hang detector).
        fn ends(&self) -> bool {
            // SAFETY: a process handle this guard holds.
            unsafe { ffi::WaitForSingleObject(self.process.raw(), HANG_DETECTOR_MS) == 0 }
        }

        fn in_job(&self, job: HANDLE) -> bool {
            let mut held = 0i32;
            // SAFETY: a process handle this guard holds, a live job, an out-param.
            assert_ne!(
                unsafe { ffi::IsProcessInJob(self.process.raw(), job, &mut held) },
                0
            );
            held != 0
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            if self.running() {
                // SAFETY: the process this guard started (or vouched for) and holds.
                unsafe { ffi::TerminateProcess(self.process.raw(), 1) };
            }
        }
    }

    /// A tab running an interactive `cmd.exe`, its output read to the end on a
    /// thread of its own; the receiver hears when the end comes.
    fn tab() -> (crate::SpawnedShell, mpsc::Receiver<()>) {
        // SAFETY: test-process mint.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        JOB_LETS_GO.with(|lets_go| lets_go.set(true));
        let shell = spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&["cmd.exe".into(), "/d".into(), "/q".into()]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        );
        JOB_LETS_GO.with(|lets_go| lets_go.set(false));
        let shell = shell.expect("a ConPTY spawn of cmd.exe");
        let (tx, rx) = mpsc::channel();
        let master = shell.master;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while read(master, &mut buf) > 0 {}
            let _ = tx.send(());
        });
        (shell, rx)
    }

    /// What a test watches: a program the tab's shell runs outside its job and
    /// the `ping` below it, and a program outside the tab's tree with its own
    /// `ping`; each still running at the end is ended on drop.
    struct Programs {
        outside: Watched,
        ping: Watched,
        stranger: Watched,
        stranger_ping: Watched,
    }

    impl Programs {
        /// Start them for `shell`'s tab, checked to be where they should be.
        fn start(shell: &crate::SpawnedShell) -> Self {
            let s = session(shell.master).expect("the tab's session");
            let outside = Watched::start(Some(shell.pid as u32));
            let ping = outside.child();
            let stranger = Watched::start(None);
            let stranger_ping = stranger.child();
            assert!(
                !outside.in_job(s.job),
                "the stand-in runs outside the tab's job"
            );
            assert!(!ping.in_job(s.job), "and so does what it started");
            let all = [&outside, &ping, &stranger, &stranger_ping];
            assert!(all.iter().all(|w| w.running()));
            Self {
                outside,
                ping,
                stranger,
                stranger_ping,
            }
        }

        /// The tab's two end; the stranger and its `ping` run on.
        fn assert_only_the_tabs_ended(&self) {
            assert!(self.outside.ends(), "the program outside the job ends");
            assert!(self.ping.ends(), "the ping below it ends too");
            assert!(
                self.stranger.running() && self.stranger_ping.running(),
                "programs outside the tab's tree run on"
            );
        }
    }

    #[test]
    fn a_close_ends_the_programs_the_shell_runs_outside_its_job() {
        let _one = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (shell, eof) = tab();
        let programs = Programs::start(&shell);
        let s = session(shell.master).expect("the tab's session");
        super::super::hangup(shell.pid);
        assert!(
            *s.outside_job_ended.lock_or_recover(),
            "ended on hangup's own thread, which a quit cannot outrun"
        );
        drop(s);
        programs.assert_only_the_tabs_ended();
        eof.recv_timeout(Duration::from_secs(60))
            .expect("the tab's output ends");
        close_master(shell.master);
    }

    #[test]
    fn a_shell_that_exits_ends_the_programs_it_runs_outside_its_job() {
        let _one = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (shell, eof) = tab();
        let programs = Programs::start(&shell);
        write_all(shell.master, b"exit\r");
        programs.assert_only_the_tabs_ended();
        eof.recv_timeout(Duration::from_secs(60))
            .expect("the tab's output ends");
        close_master(shell.master);
    }

    #[test]
    fn a_quit_ends_the_programs_every_shell_runs_outside_its_job() {
        let _one = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (shell, eof) = tab();
        let programs = Programs::start(&shell);
        end_programs_outside_jobs();
        programs.assert_only_the_tabs_ended();
        let s = session(shell.master).expect("the tab's session");
        assert!(*s.outside_job_ended.lock_or_recover());
        // SAFETY: the session's own process handle, held by the Arc.
        let shell_runs = unsafe { ffi::WaitForSingleObject(s.process, 0) } == ffi::WAIT_TIMEOUT;
        assert!(shell_runs, "the shell is the quit's to end");
        drop(s);
        super::super::hangup(shell.pid);
        eof.recv_timeout(Duration::from_secs(60))
            .expect("the tab's output ends");
        close_master(shell.master);
    }
}
