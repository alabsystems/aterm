// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        // SAFETY: plain OpenProcess; the handle is owned by the guard.
        let parent = parent
            .map(|pid| Handle::new(unsafe { ffi::OpenProcess(PROCESS_CREATE_PROCESS, 0, pid) }));
        let mut parent_raw = parent.as_ref().map_or(0, Handle::raw);
        if parent.is_some() {
            assert_ne!(parent_raw, 0, "the parent opens to start a child under it");
            // SAFETY: an initialized list; the value points at a live
            // handle that outlives the CreateProcessW below.
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
        // SAFETY: zeroed STARTUPINFOEXW is valid; cb and the list are set.
        let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
        si.lpAttributeList = attr.ptr();
        // SAFETY: PROCESS_INFORMATION is POD out-memory.
        let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated, writable command line; the list and
        // the parent handle live across the call.
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
                let access =
                    SYNCHRONIZE | ffi::PROCESS_QUERY_LIMITED_INFORMATION | ffi::PROCESS_TERMINATE;
                // SAFETY: plain OpenProcess; the handle is owned by the guard.
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
    tab_whose_job(true)
}

/// [`tab`], its job letting its programs go (`lets_go`, as a packaged shell's leave it) or
/// keeping them, as an ordinary shell's job does.
fn tab_whose_job(lets_go: bool) -> (crate::SpawnedShell, mpsc::Receiver<()>) {
    // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    JOB_LETS_GO.with(|flag| flag.set(lets_go));
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
    JOB_LETS_GO.with(|flag| flag.set(false));
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
fn the_programs_the_shell_runs_outside_its_job_are_listed_with_its_jobs() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (shell, eof) = tab();
    let programs = Programs::start(&shell);
    let listed = super::super::job_images(shell.pid);
    let names = |pid: u32| -> Vec<String> {
        listed
            .iter()
            .filter(|(p, _)| *p == pid)
            .filter_map(|(_, image)| image.file_name())
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .collect()
    };
    assert_eq!(names(programs.outside.pid), ["cmd.exe"], "{listed:?}");
    assert_eq!(names(programs.ping.pid), ["ping.exe"], "{listed:?}");
    assert!(
        names(programs.stranger.pid).is_empty() && names(programs.stranger_ping.pid).is_empty(),
        "a program outside the tab's tree is no command's: {listed:?}"
    );
    assert!(
        names(shell.pid as u32).is_empty(),
        "the shell is no command's program: {listed:?}"
    );
    assert!(
        programs.outside.running() && programs.ping.running(),
        "a listing ends nothing"
    );
    super::super::hangup(shell.pid);
    programs.assert_only_the_tabs_ended();
    eof.recv_timeout(Duration::from_secs(60))
        .expect("the tab's output ends");
    close_master(shell.master);
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

/// How many walks of the machine's processes this thread's listings have taken.
fn walks_taken() -> usize {
    WALKS.with(std::cell::Cell::get)
}

/// End the tab and hear its output end.
fn close(shell: &crate::SpawnedShell, eof: &mpsc::Receiver<()>) {
    super::super::hangup(shell.pid);
    eof.recv_timeout(Duration::from_secs(60))
        .expect("the tab's output ends");
    close_master(shell.master);
}

#[test]
fn only_a_shell_not_known_to_keep_its_programs_is_walked() {
    use ProgramsRun::{InJob, OutsideJob, Unknown};
    assert!(walks(OutsideJob, true) && walks(OutsideJob, false));
    assert!(!walks(InJob, true) && !walks(InJob, false));
    assert!(walks(Unknown, true), "a discovering listing finds out");
    assert!(!walks(Unknown, false), "the pace between never asks");
}

#[test]
fn what_a_session_learns_first_of_its_shell_stays() {
    let learned = Learned::of_shell(Some(Path::new(r"C:\Windows\System32\cmd.exe")));
    assert_eq!(learned.get(), ProgramsRun::Unknown);
    learned.found(ProgramsRun::Unknown);
    assert_eq!(learned.get(), ProgramsRun::Unknown);
    learned.found(ProgramsRun::InJob);
    learned.found(ProgramsRun::OutsideJob);
    assert_eq!(learned.get(), ProgramsRun::InJob);
    let unread = Learned::of_shell(None);
    unread.found(ProgramsRun::OutsideJob);
    unread.found(ProgramsRun::InJob);
    assert_eq!(unread.get(), ProgramsRun::OutsideJob);
}

#[test]
fn a_packaged_shell_is_known_to_run_its_programs_outside_its_job_from_its_start() {
    for image in [
        r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.5.4.0_x64__8wekyb3d8bbwe\pwsh.exe",
        r"C:\Users\someone\AppData\Local\Microsoft\windowsapps\pwsh.exe",
    ] {
        assert!(packaged(Path::new(image)), "{image}");
        assert_eq!(
            Learned::of_shell(Some(Path::new(image))).get(),
            ProgramsRun::OutsideJob,
            "{image}"
        );
    }
    for image in [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        r"C:\tools\WindowsApps.exe",
        r"C:\WindowsAppsOld\pwsh.exe",
    ] {
        assert!(!packaged(Path::new(image)), "{image}");
    }
}

/// A shell at rest whose job keeps its programs: the listing at a pace takes no walk, and the
/// one that may discover takes one and learns nothing from an empty answer.
#[test]
fn a_shell_at_rest_is_walked_only_by_a_listing_that_may_find_out() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (shell, eof) = tab_whose_job(false);
    let s = session(shell.master).expect("the tab's session");
    let before = walks_taken();
    assert!(super::super::job_images_at_pace(shell.pid).is_empty());
    assert_eq!(walks_taken(), before, "the pace takes no walk");
    assert!(super::super::job_images(shell.pid).is_empty());
    assert_eq!(
        walks_taken(),
        before + 1,
        "a block's start or end takes one"
    );
    assert_eq!(s.programs.get(), ProgramsRun::Unknown);
    drop(s);
    close(&shell, &eof);
}

/// Once the job has held a program the shell is known to run its programs there: the same
/// listing that walked a shell nothing was known of (the test above) takes no walk now.
#[test]
fn a_shell_whose_job_held_a_program_is_not_walked_again() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (shell, eof) = tab_whose_job(false);
    let s = session(shell.master).expect("the tab's session");
    let program = Watched::start(Some(shell.pid as u32));
    let ping = program.child();
    assert!(
        program.in_job(s.job) && ping.in_job(s.job),
        "the job keeps them"
    );
    let before = walks_taken();
    let listed = super::super::job_images(shell.pid);
    assert!(listed.iter().any(|&(pid, _)| pid == ping.pid), "{listed:?}");
    assert_eq!(
        walks_taken(),
        before,
        "a job that holds a program is enough"
    );
    assert_eq!(s.programs.get(), ProgramsRun::InJob);
    let (program_pid, ping_pid) = (program.pid, ping.pid);
    drop((program, ping));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while console_guard::pids_in(s.job)
        .iter()
        .any(|&pid| pid == program_pid || pid == ping_pid)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the job never let its ended programs go"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(super::super::job_images(shell.pid).is_empty());
    assert_eq!(walks_taken(), before, "its empty job is a shell at rest");
    drop(s);
    close(&shell, &eof);
}

/// A shell found running its programs outside its job is walked at the pace too; before any
/// listing found that out, the pace listed none of them and took no walk.
#[test]
fn a_shell_found_running_programs_outside_its_job_is_walked_at_the_pace_too() {
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (shell, eof) = tab();
    let s = session(shell.master).expect("the tab's session");
    let programs = Programs::start(&shell);
    let holds = |listed: &[(u32, PathBuf)]| listed.iter().any(|&(pid, _)| pid == programs.ping.pid);
    let before = walks_taken();
    assert!(!holds(&super::super::job_images_at_pace(shell.pid)));
    assert_eq!(
        walks_taken(),
        before,
        "nothing known yet: the pace takes no walk"
    );
    assert!(holds(&super::super::job_images(shell.pid)));
    assert_eq!(s.programs.get(), ProgramsRun::OutsideJob);
    assert!(holds(&super::super::job_images_at_pace(shell.pid)));
    assert_eq!(walks_taken(), before + 2);
    drop(s);
    super::super::hangup(shell.pid);
    programs.assert_only_the_tabs_ended();
    eof.recv_timeout(Duration::from_secs(60))
        .expect("the tab's output ends");
    close_master(shell.master);
}
