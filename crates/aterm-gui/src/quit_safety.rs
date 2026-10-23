// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 quit-safety guard (audit): a close/quit request while a foreground job is running asks first.

use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use aterm_uds::process::child_pids;

/// The copy for a destructive close/quit confirmation dialog, handed as is to the platform alert (`AppRt::confirm` → `NSAlert`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfirmPrompt {
    /// The alert's primary message (`NSAlert.messageText`).
    pub title: &'static str,
    /// The secondary explanatory line (`NSAlert.informativeText`).
    pub body: &'static str,
    /// The affirmative (destructive) button label; a "Cancel" button is always added alongside it.
    pub proceed: &'static str,
}

/// Whether a close/quit gesture needs a confirmation dialog, and with what copy.
pub(crate) fn confirm_prompt(exits_app: bool, busy: bool, reopens: bool) -> Option<ConfirmPrompt> {
    if exits_app {
        Some(ConfirmPrompt {
            title: "Are you sure you want to quit aterm?",
            body: match (busy, reopens) {
                (true, true) => {
                    "A running process ends and every shell's scrollback is lost; windows and \
                     tabs come back when aterm starts."
                }
                (false, true) => {
                    "Every shell ends and its scrollback is lost; windows and tabs come back \
                     when aterm starts."
                }
                (true, false) => {
                    "A process is still running. Quitting will end it and close every window."
                }
                (false, false) => "This will close every window and end your terminal sessions.",
            },
            proceed: "Quit",
        })
    } else if busy {
        Some(ConfirmPrompt {
            title: "A process is still running in this window.",
            body: "Closing it now will end that process.",
            proceed: "Close",
        })
    } else {
        None
    }
}

/// The affirmative verb of a busy TAB close's confirm ([`busy_view_close_prompt`]), which names the wire `kind=close-tab` (`close_confirm::kind`).
pub(crate) const CLOSE_TAB: &str = "Close Tab";
/// The affirmative verb of a busy PANE close's confirm (`kind=close-pane`).
pub(crate) const CLOSE_PANE: &str = "Close Pane";

/// The copy for closing a tab (`pane` false) or a pane that leaves its window standing while a job runs in it.
pub(crate) fn busy_view_close_prompt(pane: bool) -> ConfirmPrompt {
    ConfirmPrompt {
        title: if pane {
            "A process is still running in this pane."
        } else {
            "A process is still running in this tab."
        },
        body: "Closing it now will end that process.",
        proceed: if pane { CLOSE_PANE } else { CLOSE_TAB },
    }
}

/// The Windows close-confirm policy, Windows Terminal's (`confirmCloseAllTabs`).
#[cfg(windows)]
pub(crate) fn confirm_prompt_windows(
    exits_app: bool,
    tabs_closing: usize,
    busy: bool,
    reopens: bool,
) -> Option<ConfirmPrompt> {
    if !busy && tabs_closing <= 1 {
        // a single idle tab: instant, per the WT convention
        return None;
    }
    confirm_prompt(exits_app, busy, reopens).or(Some(ConfirmPrompt {
        title: "Close all tabs?",
        body: "This window has several tabs open. Closing it will end all of their sessions.",
        proceed: "Close",
    }))
}

/// Whether a PTY's foreground process group is a job rather than the shell itself.
pub(crate) fn foreground_is_job(fg_pgrp: i32, shell_pid: i32) -> bool {
    foreground_is_job_since(fg_pgrp, shell_pid, None)
}

/// [`foreground_is_job`] with the windows child-age floor, the close guard's form.
pub(crate) fn foreground_is_job_since(fg_pgrp: i32, shell_pid: i32, since_ms: Option<u64>) -> bool {
    if fg_pgrp > 0 {
        return fg_pgrp != shell_pid;
    }
    #[cfg(windows)]
    {
        shell_pid > 0 && capture_child_links().has_job(shell_pid as u32, since_ms, process_facts)
    }
    #[cfg(not(windows))]
    {
        let _ = since_ms;
        false
    }
}

/// Whether `term`'s shell is executing a command: it said a command started (OSC 133;C) and has not yet said it finished (133;D).
pub(crate) fn shell_executing(term: &aterm_core::terminal::Terminal) -> bool {
    term.shell_state() == aterm_core::terminal::ShellState::Executing
}

/// Whether `term`'s shell integration has ever reported a whole COMMAND (a 133;C that a 133;D closed: the engine drops a D no C opened) rather than only its prompts.
pub(crate) fn shell_reports_commands(term: &aterm_core::terminal::Terminal) -> bool {
    term.completed_command_seq() > 0
}

/// What the shell's own integration says, as the close guard reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellWord {
    /// The shell said a command started and has not said it finished ([`shell_executing`]).
    pub(crate) executing: bool,
    /// When the shell last said a command finished (its 133;D, unix epoch ms), `Some` only when that word is the shell's own and complete.
    pub(crate) idle_since_ms: Option<u64>,
}

/// Read [`ShellWord`] from the engine a close is about to hang up.
pub(crate) fn shell_word(term: &aterm_core::terminal::Terminal) -> ShellWord {
    let own_marks =
        term.shell_integration_posture() == aterm_core::terminal::ShellIntegrationPosture::On;
    let reports_commands = shell_reports_commands(term);
    ShellWord {
        executing: shell_executing(term),
        idle_since_ms: (own_marks && reports_commands)
            .then(|| {
                term.last_completed_command()
                    .and_then(|mark| mark.command_end_time_ms)
            })
            .flatten(),
    }
}

/// The close guard's verdict for one session.
pub(crate) fn foreground_busy(fg_pgrp: i32, shell_pid: i32, word: ShellWord) -> bool {
    word.executing || foreground_is_job_since(fg_pgrp, shell_pid, word.idle_since_ms)
}

/// How long one system process-table capture may keep serving the tab-status observer while no session's evidence has moved.
pub(crate) const JOB_PROBE_MAX_AGE: Duration = Duration::from_secs(2);

/// The observable evidence a foreground-job verdict is derived from: inputs whose movement owes a fresh process-table capture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct JobEvidenceKey {
    /// The engine's monotonic content sequence — any grid write moves it.
    pub(crate) content_seq: u64,
    /// Alternate-screen state: a full-screen program entering/leaving.
    pub(crate) alt_screen: bool,
    /// The session's latest PTY-output stamp (`lat_epoch` nanos, 0 = never).
    pub(crate) output_ns: u64,
    /// The last keystroke into the window this session is focused in.
    pub(crate) input: Option<Instant>,
}

/// Every live `(parent pid, child pid)` link at one instant, sorted by parent and deduplicated.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildLinks(Vec<(u32, u32)>);

/// What the job walk reads about one live process, through a handle to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessFacts {
    /// When the process was created, unix epoch milliseconds.
    pub(crate) created_ms: u64,
    /// Its image's file name, ASCII-lowercased (`bash.exe`); empty when the name could not be read.
    pub(crate) image: String,
}

/// How soon after a shell a same-image child must be created to be the shell that
/// shell launched rather than a job.
const LAUNCHER_WINDOW_MS: u64 = 2_000;

impl ProcessFacts {
    /// Whether this process is the real shell `launcher` started for itself:
    /// the same image name, created inside [`LAUNCHER_WINDOW_MS`] of it.
    fn is_the_shell_launched_by(&self, launcher: &Self) -> bool {
        !self.image.is_empty()
            && self.image == launcher.image
            && self.created_ms.saturating_sub(launcher.created_ms) <= LAUNCHER_WINDOW_MS
    }
}

impl ChildLinks {
    /// Sort + dedup a raw `(parent, child)` list into a searchable capture.
    #[cfg(any(windows, test))]
    pub(crate) fn from_links(mut links: Vec<(u32, u32)>) -> Self {
        links.sort_unstable();
        links.dedup();
        Self(links)
    }

    /// The child pids this capture lists under `parent`.
    fn children_of(&self, parent: u32) -> impl Iterator<Item = u32> + '_ {
        let start = self.0.partition_point(|&(p, _)| p < parent);
        self.0[start..]
            .iter()
            .take_while(move |&&(p, _)| p == parent)
            .map(|&(_, child)| child)
    }

    /// The job predicate: whether `shell` has a live child that is a job it started.
    pub(crate) fn has_job(
        &self,
        shell: u32,
        since_ms: Option<u64>,
        facts: impl Fn(u32) -> Option<ProcessFacts>,
    ) -> bool {
        if self.children_of(shell).next().is_none() {
            return false;
        }
        let Some(shell_facts) = facts(shell) else {
            return false;
        };
        let floor = since_ms.map_or(shell_facts.created_ms, |since| {
            since.max(shell_facts.created_ms)
        });
        self.children_of(shell).any(|child| {
            let Some(child_facts) = facts(child) else {
                return false;
            };
            if child_facts.created_ms < shell_facts.created_ms {
                return false;
            }
            if child_facts.is_the_shell_launched_by(&shell_facts) {
                let inner_floor = floor.max(child_facts.created_ms);
                return self
                    .children_of(child)
                    .any(|job| facts(job).is_some_and(|job| job.created_ms >= inner_floor));
            }
            child_facts.created_ms >= floor
        })
    }
}

/// The rate-limited, batched foreground-job oracle behind the tab-status observer's `foreground_job` evidence.
#[derive(Default)]
pub(crate) struct JobProbe {
    /// The last capture and when it was taken. Every session in a sweep reads
    /// this one table.
    snapshot: Option<(Instant, ChildLinks)>,
    /// Per-session evidence at the moment its verdict was last derived.
    keys: std::collections::HashMap<u64, JobEvidenceKey>,
    /// How many process-table captures this probe has taken: it must not scale with
    /// session count, and an idle window adds at most one per [`JOB_PROBE_MAX_AGE`].
    captures: u64,
}

impl JobProbe {
    /// See [`Self::captures`].
    #[cfg(test)]
    pub(crate) fn capture_count(&self) -> u64 {
        self.captures
    }

    /// The observer's entry point.
    pub(crate) fn is_job(
        &mut self,
        session: u64,
        fg: i32,
        shell_pid: i32,
        key: JobEvidenceKey,
        now: Instant,
    ) -> bool {
        self.is_job_with(session, fg, shell_pid, key, now, capture_child_links)
    }

    /// [`Self::is_job`] with the system capture injected, so the batching and rate limit are provable without a process table.
    pub(crate) fn is_job_with(
        &mut self,
        session: u64,
        fg: i32,
        shell_pid: i32,
        key: JobEvidenceKey,
        now: Instant,
        capture: impl FnOnce() -> ChildLinks,
    ) -> bool {
        // The unix answer is the per-fd `tcgetpgrp` the sweep read, never a
        if fg > 0 {
            self.keys.remove(&session);
            return fg != shell_pid;
        }
        if !cfg!(windows) || shell_pid <= 0 {
            // Matches `foreground_is_job`'s non-windows / unknown-pid arm.
            return false;
        }
        self.child_verdict(session, key, now, capture, |links| {
            links_have_job(links, shell_pid as u32)
        })
    }

    /// The rate-limited child-walk oracle, free of any platform call so the
    /// scheduling laws are unit-testable everywhere: `capture` takes the
    /// system table, `verdict` reads one shell's answer from it.
    fn child_verdict(
        &mut self,
        session: u64,
        key: JobEvidenceKey,
        now: Instant,
        capture: impl FnOnce() -> ChildLinks,
        verdict: impl FnOnce(&ChildLinks) -> bool,
    ) -> bool {
        let this_sweep = self.snapshot.as_ref().is_some_and(|(at, _)| *at == now);
        if !this_sweep {
            let expired = self
                .snapshot
                .as_ref()
                .is_none_or(|(at, _)| now.duration_since(*at) >= JOB_PROBE_MAX_AGE);
            let moved = self.keys.get(&session) != Some(&key);
            if expired || moved {
                self.captures = self.captures.saturating_add(1);
                self.snapshot = Some((now, capture()));
            }
        }
        self.keys.insert(session, key);
        self.snapshot
            .as_ref()
            .is_some_and(|(_, links)| verdict(links))
    }

    /// Drop a retired session's evidence so the map cannot outlive the pool.
    pub(crate) fn forget(&mut self, session: u64) {
        self.keys.remove(&session);
    }
}

/// One system process-table capture. Non-windows never reaches this (the child
/// walk is the windows-only arm) and answers with an empty table.
fn capture_child_links() -> ChildLinks {
    #[cfg(windows)]
    {
        windows_jobs::capture_links()
    }
    #[cfg(not(windows))]
    {
        ChildLinks::default()
    }
}

/// The probe's verdict for one shell, read from a capture: the same vetted
/// walk the close path runs ([`ChildLinks::has_job`]), with the system's own
/// process facts.
fn links_have_job(links: &ChildLinks, shell_pid: u32) -> bool {
    links.has_job(shell_pid, None, process_facts)
}

/// One process's facts, read through a handle. Non-windows never reaches this
/// (the child walk is the windows-only arm) and reads nothing.
fn process_facts(pid: u32) -> Option<ProcessFacts> {
    #[cfg(windows)]
    {
        windows_jobs::process_facts(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// The PTY master's foreground process group, or `-1` when unknown.
pub(crate) fn foreground_pgrp(master: i32) -> i32 {
    #[cfg(unix)]
    {
        // SAFETY: `tcgetpgrp` on a PTY master fd; a <= 0 error return is treated as idle by the callers.
        unsafe { libc::tcgetpgrp(master) }
    }
    #[cfg(windows)]
    {
        let _ = master;
        -1
    }
}

/// The most children [`session_root`] reports on Linux, as macOS's listing
/// ([`aterm_uds::process::CHILD_PIDS_MAX`]): one is enough to say a job is
/// there, and a handful lets a test clean up after itself.
#[cfg(target_os = "linux")]
const ROOT_CHILDREN_MAX: usize = 64;

/// What the process table says about a session's root, the process the PTY was spawned with (its session leader).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRoot {
    /// The root's executable name: `p_comm` on macOS, `/proc/<pid>/comm` on Linux.
    pub(crate) comm: String,
    /// The root's live children (at most 64).
    pub(crate) children: Vec<i32>,
}

/// The shells a session root may be and still be "at its prompt" when it has
/// no child and holds the foreground itself.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ash", "ksh", "mksh", "oksh", "loksh", "pdksh", "yash",
    "tcsh", "csh", "nu", "xonsh", "elvish", "ion", "pwsh", "rc", "es",
];

/// Whether `comm` names a shell (a leading login `-` ignored).
pub(crate) fn is_shell_name(comm: &str) -> bool {
    let name = comm.trim_start_matches('-');
    SHELLS.contains(&name)
}

/// [`SessionRoot`] of `pid`, or `None` when the table cannot answer (gone, a zombie, a platform without the calls).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn session_root(pid: i32) -> Option<SessionRoot> {
    if pid <= 0 {
        return None;
    }
    platform_session_root(pid)
}

/// `pid`'s `proc_pidinfo(PROC_PIDTBSDINFO)` record: an exact-size read that names `pid` (a record about another pid is no witness).
#[cfg(target_os = "macos")]
pub(crate) fn pid_bsdinfo(pid: libc::pid_t) -> Option<libc::proc_bsdinfo> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure PROC_PIDTBSDINFO fills.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    (u32::try_from(pid).ok()? == info.pbi_pid).then_some(info)
}

#[cfg(target_os = "macos")]
fn platform_session_root(pid: i32) -> Option<SessionRoot> {
    let comm = process_name(pid)?;
    let children = child_pids(pid)?;
    Some(SessionRoot { comm, children })
}

/// `pid`'s process name (its `comm`, at most 16 bytes), or `None` (gone, another user's, a platform without the read).
#[cfg(target_os = "macos")]
pub(crate) fn process_name(pid: i32) -> Option<String> {
    let info = pid_bsdinfo(pid)?;
    Some(
        info.pbi_comm
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| char::from(c.to_ne_bytes()[0]))
            .collect(),
    )
}

/// Whether `shell_pid` runs a program started since `since_ms` outside the foreground process group (`Terminal::set_program_probe`).
pub(crate) fn shell_runs_program_since(shell_pid: i32, since_ms: u64) -> bool {
    if shell_pid <= 0 {
        return false;
    }
    #[cfg(windows)]
    {
        capture_child_links().has_job(shell_pid.unsigned_abs(), Some(since_ms), process_facts)
    }
    #[cfg(target_os = "macos")]
    {
        let Some(shell) = pid_bsdinfo(shell_pid) else {
            return false;
        };
        child_pids(shell_pid).is_some_and(|children| {
            children.into_iter().any(|child| {
                pid_bsdinfo(child).is_some_and(|info| {
                    let started_ms = info
                        .pbi_start_tvsec
                        .saturating_mul(1000)
                        .saturating_add(info.pbi_start_tvusec / 1000);
                    info.pbi_status != libc::SZOMB
                        && info.pbi_pgid == shell.pbi_pgid
                        && started_ms >= since_ms
                })
            })
        })
    }
    #[cfg(target_os = "linux")]
    {
        let Some((shell_group, _)) = linux_group_and_start_ms(shell_pid) else {
            return false;
        };
        crate::foreground_handback::linux_children(shell_pid).is_some_and(|children| {
            children.into_iter().any(|child| {
                linux_group_and_start_ms(child).is_some_and(|(group, started_ms)| {
                    group == shell_group && started_ms >= since_ms
                })
            })
        })
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = since_ms;
        false
    }
}

/// Linux: `pid`'s process group and start time (unix epoch ms) from
/// `/proc/<pid>/stat` (`pgrp`, and `starttime` in clock ticks since boot, set
/// against the boot instant `CLOCK_BOOTTIME` gives,
/// [`aterm_uds::process::boot_clock_now`]).
#[cfg(target_os = "linux")]
fn linux_group_and_start_ms(pid: i32) -> Option<(i32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_ascii_whitespace();
    if fields.next()? == "Z" {
        return None;
    }
    let group = fields.nth(1)?.parse::<i32>().ok()?;
    let ticks = fields.nth(16)?.parse::<u64>().ok()?;
    // SAFETY: sysconf reads a constant.
    let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let per_second = u64::try_from(per_second).ok().filter(|t| *t > 0)?;
    let aterm_uds::process::BootClock(since_boot_ns) = aterm_uds::process::boot_clock_now()?;
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis(),
    )
    .ok()?;
    let since_boot_ms = since_boot_ns / 1_000_000;
    let booted_ms = now_ms.checked_sub(since_boot_ms)?;
    Some((group, booted_ms + ticks.saturating_mul(1000) / per_second))
}

/// See the macOS [`process_name`].
#[cfg(target_os = "linux")]
pub(crate) fn process_name(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_string())
}

#[cfg(target_os = "linux")]
fn platform_session_root(pid: i32) -> Option<SessionRoot> {
    let comm = process_name(pid)?;
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
    Some(SessionRoot {
        comm,
        children: children
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .take(ROOT_CHILDREN_MAX)
            .collect(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_session_root(_pid: i32) -> Option<SessionRoot> {
    None
}

/// See the macOS [`process_name`]: no read here.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process_name(_pid: i32) -> Option<String> {
    None
}

/// Windows running-job detection: one Toolhelp32 pass over the system process
/// list ([`capture_links`]), and a handle read of each candidate a shell's
/// verdict turns on ([`process_facts`]).
#[cfg(windows)]
mod windows_jobs {
    use super::{ChildLinks, ProcessFacts};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> isize;
        fn Process32FirstW(hSnapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
        fn Process32NextW(hSnapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
        fn CloseHandle(hObject: isize) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
        fn GetProcessTimes(
            hProcess: isize,
            lpCreationTime: *mut FILETIME,
            lpExitTime: *mut FILETIME,
            lpKernelTime: *mut FILETIME,
            lpUserTime: *mut FILETIME,
        ) -> i32;
        fn QueryFullProcessImageNameW(
            hProcess: isize,
            dwFlags: u32,
            lpExeName: *mut u16,
            lpdwSize: *mut u32,
        ) -> i32;
    }

    /// `TH32CS_SNAPPROCESS` — snapshot every process in the system.
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: isize = -1;
    /// The one right [`process_facts`] asks for: creation time and image name,
    /// and nothing that could read or change the process.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// FILETIME ticks (100 ns) from 1601-01-01 to the unix epoch.
    const FILETIME_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

    /// `PROCESSENTRY32W`, transcribed from the SDK.
    #[repr(C)]
    #[allow(non_snake_case)]
    struct PROCESSENTRY32W {
        dwSize: u32,
        cntUsage: u32,
        th32ProcessID: u32,
        th32DefaultHeapID: usize,
        th32ModuleID: u32,
        cntThreads: u32,
        th32ParentProcessID: u32,
        pcPriClassBase: i32,
        dwFlags: u32,
        szExeFile: [u16; 260],
    }

    /// `FILETIME`, transcribed from the SDK: 100 ns ticks since 1601, split.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    struct FILETIME {
        dwLowDateTime: u32,
        dwHighDateTime: u32,
    }

    /// The console host the OS attaches to a console process: plumbing, never a user
    /// job, so the child walk ignores it (a shell that allocated a classic console
    /// must not read as busy forever).
    const CONHOST: &str = "conhost.exe";

    /// Every live parent -> child link, in one Toolhelp32 pass (~2.2 ms in the kernel plus ~1.6 ms of walk), so one capture answers every shell.
    pub(super) fn capture_links() -> ChildLinks {
        let mut links = Vec::new();
        // SAFETY: standard Toolhelp walk.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap == 0 {
                return ChildLinks::default();
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                if entry_counts_as_child(
                    entry.th32ProcessID,
                    entry.th32ParentProcessID,
                    exe_name_is(&entry.szExeFile, CONHOST),
                ) {
                    links.push((entry.th32ParentProcessID, entry.th32ProcessID));
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
        }
        ChildLinks::from_links(links)
    }

    /// Which snapshot entries are candidate children at all, before any handle is opened.
    pub(super) const fn entry_counts_as_child(
        entry_pid: u32,
        entry_parent: u32,
        is_conhost: bool,
    ) -> bool {
        entry_pid != entry_parent && !is_conhost
    }

    /// Read `pid`'s creation time and image name through a `PROCESS_QUERY_LIMITED_INFORMATION` handle, or `None` when it cannot be opened or timed ([`ChildLinks::has_job`] says why that is "not a job").
    pub(super) fn process_facts(pid: u32) -> Option<ProcessFacts> {
        // SAFETY: `OpenProcess` returns 0 on failure and a handle otherwise, which is closed on every path past it.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process == 0 {
                return None;
            }
            let mut created = FILETIME::default();
            let (mut exited, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            let timed = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
            let mut path = [0u16; 1024];
            let mut len = path.len() as u32;
            let named = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if timed == 0 {
                return None;
            }
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            let image = if named != 0 {
                image_file_name(&path[..(len as usize).min(path.len())])
            } else {
                String::new()
            };
            Some(ProcessFacts {
                created_ms: filetime_to_unix_ms(ticks),
                image,
            })
        }
    }

    /// FILETIME ticks to unix epoch milliseconds.
    pub(super) const fn filetime_to_unix_ms(ticks: u64) -> u64 {
        ticks.saturating_sub(FILETIME_UNIX_EPOCH_TICKS) / 10_000
    }

    /// The file name of a full image path (`C:\...\usr\bin\bash.exe` -> `bash.exe`), ASCII-lowercased.
    pub(super) fn image_file_name(path: &[u16]) -> String {
        let start = path
            .iter()
            .rposition(|&unit| unit == u16::from(b'\\') || unit == u16::from(b'/'))
            .map_or(0, |separator| separator + 1);
        char::decode_utf16(path[start..].iter().copied())
            .map(|c| c.unwrap_or('\u{fffd}').to_ascii_lowercase())
            .collect()
    }

    /// Case-insensitive (ASCII) match of a NUL-terminated UTF-16 exe name.
    fn exe_name_is(exe: &[u16; 260], name: &str) -> bool {
        let len = exe.iter().position(|&u| u == 0).unwrap_or(exe.len());
        char::decode_utf16(exe[..len].iter().copied())
            .map(|c| c.unwrap_or('\u{fffd}').to_ascii_lowercase())
            .eq(name.chars())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_always_prompts_and_notes_a_running_job() {
        for reopens in [false, true] {
            let idle =
                confirm_prompt(true, false, reopens).expect("a whole-app quit always confirms");
            assert_eq!(idle.proceed, "Quit");
            let busy =
                confirm_prompt(true, true, reopens).expect("a whole-app quit always confirms");
            assert_eq!(busy.proceed, "Quit");
            assert_ne!(
                idle.body, busy.body,
                "the busy-quit copy must mention the running process"
            );
            assert!(busy.body.contains("process"), "{}", busy.body);
        }
    }

    #[test]
    fn window_close_confirms_only_while_busy() {
        assert!(
            confirm_prompt(false, false, true).is_none(),
            "an idle window/tab close needs no confirmation"
        );
        let busy = confirm_prompt(false, true, true).expect("a busy window/tab close confirms");
        assert_eq!(busy.proceed, "Close");
    }

    /// The Windows policy (WT convention): a single idle tab never prompts, the
    /// last-window quit included.
    #[cfg(windows)]
    #[test]
    fn windows_single_idle_tab_closes_without_a_prompt() {
        assert!(confirm_prompt_windows(false, 1, false, true).is_none());
        assert!(
            confirm_prompt_windows(true, 1, false, true).is_none(),
            "an idle one-tab last window must quit instantly on Windows"
        );
        assert!(confirm_prompt_windows(false, 0, false, true).is_none());
    }

    /// Busy always prompts, with the shared copy: "Quit" for a whole-app exit,
    /// "Close" otherwise.
    #[cfg(windows)]
    #[test]
    fn windows_busy_prompts_with_the_real_verbs() {
        let busy_close = confirm_prompt_windows(false, 1, true, true).expect("busy close confirms");
        assert_eq!(busy_close.proceed, "Close");
        let busy_quit = confirm_prompt_windows(true, 1, true, true).expect("busy quit confirms");
        assert_eq!(busy_quit.proceed, "Quit");
    }

    /// Closing multiple tabs prompts even when idle: quit copy when the gesture exits
    /// the app, the multi-tab close copy when another window survives.
    #[cfg(windows)]
    #[test]
    fn windows_multi_tab_close_prompts_when_idle() {
        let quit = confirm_prompt_windows(true, 4, false, true).expect("multi-tab quit confirms");
        assert_eq!(quit.proceed, "Quit");
        let close =
            confirm_prompt_windows(false, 4, false, true).expect("multi-tab close confirms");
        assert_eq!(close.proceed, "Close");
        assert!(
            close.title.contains("all tabs"),
            "the non-quit multi-tab prompt must say it closes every tab: {}",
            close.title
        );
    }

    #[test]
    fn foreground_job_detection() {
        assert!(
            !foreground_is_job_since(1234, 1234, None),
            "the shell's own pgrp at the prompt is idle"
        );
        assert!(
            foreground_is_job_since(5678, 1234, None),
            "a different foreground pgrp is a running job"
        );
        assert!(
            foreground_is_job_since(5678, 1234, Some(u64::MAX)),
            "tcgetpgrp names the foreground job: the shell's idle word never masks it"
        );
        #[cfg(not(windows))]
        {
            assert!(
                !foreground_is_job_since(0, 1234, None),
                "no foreground pgrp is idle"
            );
            assert!(
                !foreground_is_job_since(-1, 1234, None),
                "a tcgetpgrp error must not wedge the close"
            );
        }
    }

    /// The shell's word that a command is running, and nothing else.
    const EXECUTING: ShellWord = ShellWord {
        executing: true,
        idle_since_ms: None,
    };

    /// A running block is busy on its own: the `Start-Sleep 60` shape, executing with
    /// no child process and no foreground pgrp (every ConPTY on windows, a
    /// `tcgetpgrp` error on unix), arms the guard where `foreground_is_job` reads idle.
    #[test]
    fn a_running_block_is_busy_without_a_foreground_child() {
        assert!(
            foreground_busy(-1, -1, EXECUTING),
            "the shell said a command is executing: busy, whatever the PTY says"
        );
        assert!(
            !foreground_busy(-1, -1, ShellWord::default()),
            "no block and no foreground evidence is idle"
        );
        assert!(foreground_busy(-1, 0, EXECUTING));
    }

    /// The union: the PTY's verdict still counts when the shell has no blocks
    /// (a bare shell without integration), and an idle prompt stays idle.
    #[test]
    fn the_guard_is_the_union_of_the_pty_and_the_block() {
        assert!(
            foreground_busy(5678, 1234, ShellWord::default()),
            "a foreground job with no block state is still a job"
        );
        assert!(
            !foreground_busy(1234, 1234, ShellWord::default()),
            "the shell's own pgrp at an idle prompt is idle"
        );
        assert!(
            foreground_busy(1234, 1234, EXECUTING),
            "an executing block wins over an idle-looking pgrp (an in-shell loop)"
        );
    }

    /// A terminal whose host required and authorized a capability nonce —
    /// the shape every aterm-spawned integrated shell has (`spawn.rs`) — and
    /// the hex its marks must carry.
    fn nonced_terminal() -> (aterm_core::terminal::Terminal, String) {
        let nonce = [0x5Au8; 32];
        let hex = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.authorize_shell_integration(nonce);
        term.set_require_shell_integration_nonce(true);
        (term, hex)
    }

    fn now_ms() -> u64 {
        aterm_time::since_epoch()
            .expect("clock after 1970")
            .as_millis() as u64
    }

    /// The shell's idle word is earned, not assumed: only a nonce-signed
    /// integration that has reported a whole command (C then D) carries one,
    /// stamped with that command's 133;D.
    #[test]
    fn the_shells_idle_word_needs_its_own_completed_command() {
        let (mut term, id) = nonced_terminal();
        term.process(format!("\x1b]133;A;id={id}\x07PS> \x1b]133;B;id={id}\x07").as_bytes());
        assert_eq!(
            shell_word(&term),
            ShellWord::default(),
            "a first prompt has reported no command yet: silence, not a word"
        );
        term.process(format!("ping -t 127.0.0.1\n\x1b]133;C;id={id}\x07").as_bytes());
        assert_eq!(shell_word(&term), EXECUTING);
        let before = now_ms();
        term.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
        let after = now_ms();
        let word = shell_word(&term);
        assert!(!word.executing);
        let since = word.idle_since_ms.expect("a whole command earns the word");
        assert!(
            (before..=after).contains(&since),
            "stamped at the 133;D: {before} <= {since} <= {after}"
        );
        term.process(format!("\x1b]133;A;id={id}\x07PS> \x1b]133;B;id={id}\x07").as_bytes());
        assert_eq!(shell_word(&term).idle_since_ms, Some(since));
    }

    /// Unsigned marks (no nonce required: `--no-shell-integration`, a user's own
    /// prompt script) are output a program could print, so they never silence the
    /// child walk.
    #[test]
    fn unsigned_marks_never_earn_the_shells_idle_word() {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07true\n\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(
            term.completed_command_seq(),
            1,
            "precondition: it completed"
        );
        assert_eq!(shell_word(&term), ShellWord::default());
    }

    /// A prompt-only integration (cmd.exe's `%PROMPT%` tier, pwsh without
    /// PSReadLine) never earns the word: the engine drops a D that no C opened.
    #[test]
    fn a_prompt_only_integration_never_earns_the_shells_idle_word() {
        let (mut term, id) = nonced_terminal();
        for _ in 0..3 {
            term.process(
                format!("\x1b]133;A;id={id}\x07C:\\> \x1b]133;B;id={id}\x07ping -t 127.0.0.1\n")
                    .as_bytes(),
            );
        }
        term.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
        assert_eq!(
            term.completed_command_seq(),
            0,
            "no C, so no completed command"
        );
        assert_eq!(shell_word(&term), ShellWord::default());
    }

    /// A process as `facts` reports it.
    fn born(created_ms: u64, image: &str) -> ProcessFacts {
        ProcessFacts {
            created_ms,
            image: image.to_string(),
        }
    }

    /// A `facts` reader over a fixed table; a pid it does not list is
    /// unreadable (exited, or another user's).
    fn facts_of(table: &[(u32, ProcessFacts)]) -> impl Fn(u32) -> Option<ProcessFacts> + '_ {
        move |pid| {
            table
                .iter()
                .find(|(listed, _)| *listed == pid)
                .map(|(_, facts)| facts.clone())
        }
    }

    const SHELL_BORN: u64 = 1_790_549_217_000;

    /// A process born before the shell it names as parent is a recycled pid (Windows
    /// hands pids on), not a job: an idle pwsh tab must not refuse its close.
    #[test]
    fn a_child_older_than_its_shell_is_a_recycled_pid_not_a_job() {
        let links = ChildLinks::from_links(vec![(17636, 22432)]);
        let table = [
            (17636, born(SHELL_BORN, "pwsh.exe")),
            (22432, born(SHELL_BORN - 39_000, "aterm-gui.exe")),
        ];
        assert!(!links.has_job(17636, None, facts_of(&table)));
        let table = [
            (17636, born(SHELL_BORN, "pwsh.exe")),
            (22432, born(SHELL_BORN + 5_000, "ping.exe")),
        ];
        assert!(links.has_job(17636, None, facts_of(&table)));
    }

    /// A candidate that cannot be read is not counted — exited, or not one the
    /// shell's user started — and an unreadable shell vets nothing.
    #[test]
    fn an_unreadable_process_is_not_counted() {
        let links = ChildLinks::from_links(vec![(100, 200)]);
        let shell_only = [(100, born(SHELL_BORN, "pwsh.exe"))];
        assert!(!links.has_job(100, None, facts_of(&shell_only)));
        let child_only = [(200, born(SHELL_BORN + 1_000, "ping.exe"))];
        assert!(
            !links.has_job(100, None, facts_of(&child_only)),
            "with no shell birth to compare against, no candidate is ours"
        );
    }

    /// The idle shell — no candidate at all — opens no handle: the common
    /// case costs the snapshot and nothing more.
    #[test]
    fn a_shell_with_no_candidate_opens_no_handle() {
        let links = ChildLinks::from_links(vec![(7, 8), (9, 10)]);
        let reads = std::cell::Cell::new(0);
        let busy = links.has_job(100, None, |_| {
            reads.set(reads.get() + 1);
            Some(born(SHELL_BORN, "x.exe"))
        });
        assert!(!busy);
        assert_eq!(reads.get(), 0, "no candidate, no handle");
    }

    /// Git Bash: `Git\bin\bash.exe` (the program the ConPTY starts) keeps the real
    /// `usr\bin\bash.exe` as its child for life, started ~30 ms after itself.
    #[test]
    fn the_shell_a_launcher_started_is_not_a_job_but_its_children_are() {
        let launcher = 26612;
        let bash = 16752;
        let links = ChildLinks::from_links(vec![(launcher, bash)]);
        let mut table = vec![
            (launcher, born(SHELL_BORN, "bash.exe")),
            (bash, born(SHELL_BORN + 30, "bash.exe")),
        ];
        assert!(
            !links.has_job(launcher, None, facts_of(&table)),
            "an idle Git Bash prompt is idle"
        );
        let links = ChildLinks::from_links(vec![(launcher, bash), (bash, 4040)]);
        table.push((4040, born(SHELL_BORN + 60_000, "ping.exe")));
        assert!(links.has_job(launcher, None, facts_of(&table)));
        table.last_mut().expect("ping").1.created_ms = SHELL_BORN + 10;
        assert!(!links.has_job(launcher, None, facts_of(&table)));
    }

    /// The launcher rule is narrow: a same-image child started later than
    /// the launch window is a shell a person typed (`pwsh` at a pwsh prompt),
    /// which is a job like any other.
    #[test]
    fn a_nested_shell_typed_later_is_a_job() {
        let links = ChildLinks::from_links(vec![(300, 301)]);
        let table = [
            (300, born(SHELL_BORN, "pwsh.exe")),
            (301, born(SHELL_BORN + LAUNCHER_WINDOW_MS + 1, "pwsh.exe")),
        ];
        assert!(links.has_job(300, None, facts_of(&table)));
    }

    /// With the shell's idle word, a child the finished command left behind
    /// — `Start-Process notepad`, a `Start-Job`, `sleep 60 &` — is not a
    /// foreground job, and one created after the word is: a command the
    /// integration failed to announce.
    #[test]
    fn after_the_shells_idle_word_only_a_newer_child_is_a_job() {
        let since = SHELL_BORN + 20_000;
        let links = ChildLinks::from_links(vec![(500, 501)]);
        let background = [
            (500, born(SHELL_BORN, "pwsh.exe")),
            (501, born(SHELL_BORN + 10_000, "notepad.exe")),
        ];
        assert!(
            links.has_job(500, None, facts_of(&background)),
            "precondition: without the word, the child walk counts it"
        );
        assert!(!links.has_job(500, Some(since), facts_of(&background)));
        let unannounced = [
            (500, born(SHELL_BORN, "pwsh.exe")),
            (501, born(since + 5_000, "ping.exe")),
        ];
        assert!(links.has_job(500, Some(since), facts_of(&unannounced)));
        let links = ChildLinks::from_links(vec![(600, 601), (601, 602)]);
        let mut table = vec![
            (600, born(SHELL_BORN, "bash.exe")),
            (601, born(SHELL_BORN + 25, "bash.exe")),
            (602, born(SHELL_BORN + 10_000, "sleep.exe")),
        ];
        assert!(!links.has_job(600, Some(since), facts_of(&table)));
        table[2].1.created_ms = since + 1;
        assert!(links.has_job(600, Some(since), facts_of(&table)));
    }

    /// `shell_executing` follows the OSC 133 marks exactly: only the span
    /// between C and D is busy; a prompt, an entered-but-unrun command line, a
    /// completed command and a shell without integration are all idle.
    #[test]
    fn shell_executing_follows_the_133_marks() {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        assert!(!shell_executing(&term), "no shell integration: idle");
        term.process(b"\x1b]133;A\x07PS> ");
        assert!(!shell_executing(&term), "prompt only");
        term.process(b"\x1b]133;B\x07Start-Sleep 60");
        assert!(
            !shell_executing(&term),
            "entering the command line, not running it"
        );
        term.process(b"\n\x1b]133;C\x07");
        assert!(
            shell_executing(&term),
            "133;C opened, no 133;D yet: executing"
        );
        term.process(b"\x1b]133;D;0\x07");
        assert!(!shell_executing(&term), "133;D closed the command: idle");
        term.process(b"\x1b]133;A\x07PS> ");
        assert!(!shell_executing(&term));
    }

    /// A running command that clears the scrollback stays busy: ED 3 (`clear`, `cls`,
    /// `Clear-Host`) is output, and only the shell's own 133;D ends the command.
    #[test]
    fn a_command_that_clears_the_scrollback_is_still_executing() {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(b"\x1b]133;A\x07PS> \x1b]133;B\x07Clear-Host; Start-Sleep 60\n\x1b]133;C\x07");
        assert!(shell_executing(&term), "precondition: executing");
        term.process(b"\x1b[H\x1b[2J\x1b[3J");
        assert_eq!(
            term.current_block().map(|b| b.state),
            Some(aterm_types::BlockState::Executing),
            "ED 3 keeps the running block"
        );
        assert!(
            shell_executing(&term),
            "the command is still running: ED 3 is output, not the end of the command"
        );
        let mut carry = term.checkpoint();
        carry.shell.current_block = None;
        let mut adopted = aterm_core::terminal::Terminal::new(24, 80);
        adopted.restore_checkpoint(&carry);
        assert!(
            adopted.current_block().is_none(),
            "precondition: the carry held no block"
        );
        assert!(
            shell_executing(&adopted),
            "the open C..D phase is the reading, block or no block"
        );
        adopted.process(b"\x1b]133;D;0\x07");
        assert!(!shell_executing(&adopted), "and its 133;D ends it");
        term.process(b"\x1b]133;D;0\x07");
        assert!(
            !shell_executing(&term),
            "133;D after the clear still ends the command"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_invalid_shell_pid_is_idle() {
        assert!(
            !foreground_is_job_since(-1, 0, None),
            "an unknown shell pid must not wedge the close"
        );
        assert!(!foreground_is_job_since(-1, -5, None));
    }

    /// The real walk, end to end: the live table, real handles, real
    /// creation times.
    #[cfg(windows)]
    #[test]
    fn windows_child_walk_detects_a_live_child() {
        let mut child = aterm_uds::spawnfd::command("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn ping");
        let me = std::process::id();
        let mine = windows_jobs::process_facts(me).expect("this process reads");
        let ping = windows_jobs::process_facts(child.id()).expect("its child reads");
        assert_eq!(ping.image, "ping.exe");
        assert!(ping.created_ms >= mine.created_ms, "{ping:?} vs {mine:?}");
        assert!(
            foreground_is_job_since(-1, me as i32, None),
            "a live child process must read as a running job"
        );
        assert!(
            !foreground_is_job_since(-1, child.id() as i32, None),
            "a process with no children is idle"
        );
        assert!(
            !foreground_is_job_since(-1, me as i32, Some(ping.created_ms + 60_000)),
            "a child older than the shell's idle word is not a foreground job"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// FILETIME arithmetic and the image-name read, pinned to known values.
    #[cfg(windows)]
    #[test]
    fn windows_process_facts_units() {
        use super::windows_jobs::{filetime_to_unix_ms, image_file_name};
        let ticks = 116_444_736_000_000_000 + 1_790_467_200_000 * 10_000;
        assert_eq!(filetime_to_unix_ms(ticks), 1_790_467_200_000);
        assert_eq!(
            filetime_to_unix_ms(0),
            0,
            "before 1970 saturates, never wraps"
        );
        let path: Vec<u16> = r"C:\Program Files\Git\bin\..\usr\bin\BASH.exe"
            .encode_utf16()
            .collect();
        assert_eq!(image_file_name(&path), "bash.exe");
        let bare: Vec<u16> = "pwsh.exe".encode_utf16().collect();
        assert_eq!(image_file_name(&bare), "pwsh.exe");
    }

    /// Which snapshot entries are candidates at all — the one filter the
    /// capture applies before any handle is opened.
    #[cfg(windows)]
    #[test]
    fn the_capture_books_only_candidate_children() {
        use super::windows_jobs::entry_counts_as_child;
        assert!(
            entry_counts_as_child(4321, 1234, false),
            "an ordinary child counts"
        );
        assert!(
            !entry_counts_as_child(1234, 1234, false),
            "a process that parents itself is not its own job"
        );
        assert!(
            !entry_counts_as_child(4321, 1234, true),
            "the console host is plumbing, never a user job"
        );
    }

    fn key(seq: u64) -> JobEvidenceKey {
        JobEvidenceKey {
            content_seq: seq,
            alt_screen: false,
            output_ns: 0,
            input: None,
        }
    }

    /// Every process in a probe fixture was born at the same instant with no
    /// readable name: a listed child is then always a vetted one, so these
    /// tests pin the probe's scheduling, not the walk's rules (proven above).
    fn same_age(_: u32) -> Option<ProcessFacts> {
        Some(born(SHELL_BORN, ""))
    }

    /// The probe's verdict for `shell` against the capture it is handed.
    fn job_of(shell: u32) -> impl FnOnce(&ChildLinks) -> bool {
        move |links| links.has_job(shell, None, same_age)
    }

    /// `master` values are ConPTY registry keys on windows (>= 0x4000_0000) and
    /// never a real fd here; the probe tests drive `child_verdict` directly so
    /// no `tcgetpgrp` is involved on any platform.
    #[test]
    fn one_capture_answers_every_session_in_a_sweep() {
        let mut probe = JobProbe::default();
        let now = Instant::now();
        let mut captures = 0usize;
        for session in 0..8u64 {
            let capture = || {
                captures += 1;
                ChildLinks::default()
            };
            let busy =
                probe.child_verdict(session, key(0), now, capture, job_of(1000 + session as u32));
            assert!(!busy, "no pid has a child in this fixture");
        }
        assert_eq!(
            captures, 1,
            "a sweep must buy ONE system process-table capture, not one per session"
        );
        assert_eq!(probe.capture_count(), 1);
    }

    /// A sweep whose evidence has not moved reuses the last capture until the
    /// ceiling — the idle property.
    #[test]
    fn a_settled_window_buys_one_capture_per_ceiling() {
        let mut probe = JobProbe::default();
        let t0 = Instant::now();
        let mut captures = 0usize;
        let sweep = |probe: &mut JobProbe, at: Instant, captures: &mut usize| {
            for session in 0..8u64 {
                let capture = || {
                    *captures += 1;
                    ChildLinks::default()
                };
                probe.child_verdict(session, key(0), at, capture, job_of(1000 + session as u32));
            }
        };
        let mut at = t0;
        let mut sweeps = 0usize;
        while at.duration_since(t0) < JOB_PROBE_MAX_AGE {
            sweep(&mut probe, at, &mut captures);
            sweeps += 1;
            at += Duration::from_millis(250);
        }
        assert!(
            sweeps >= 8,
            "the fixture must actually run sweeps: {sweeps}"
        );
        assert_eq!(
            captures, 1,
            "{sweeps} settled sweeps of 8 sessions must share ONE capture"
        );
        sweep(&mut probe, t0 + JOB_PROBE_MAX_AGE, &mut captures);
        assert_eq!(captures, 2, "the ceiling refreshes once, for every session");
    }

    /// Moved evidence is never answered from a capture taken before it: a
    /// session whose grid/output/input changed forces a fresh capture in that
    /// same sweep, which is what keeps a real job start detected on time.
    #[test]
    fn moved_evidence_forces_a_fresh_capture() {
        let mut probe = JobProbe::default();
        let t0 = Instant::now();
        let mut captures = 0usize;
        let idle = |captures: &mut usize| {
            *captures += 1;
            ChildLinks::default()
        };
        assert!(!probe.child_verdict(7, key(0), t0, || idle(&mut captures), job_of(1234)));
        assert_eq!(captures, 1);
        let t1 = t0 + Duration::from_millis(250);
        assert!(!probe.child_verdict(7, key(0), t1, || idle(&mut captures), job_of(1234)));
        assert_eq!(captures, 1, "a settled session buys nothing");
        let t2 = t1 + Duration::from_millis(250);
        let capture = || {
            captures += 1;
            ChildLinks::from_links(vec![(1234, 4321)])
        };
        let busy = probe.child_verdict(7, key(1), t2, capture, job_of(1234));
        assert!(
            busy,
            "the fresh capture must be what the verdict is read from"
        );
        assert_eq!(captures, 2, "moved evidence buys exactly one fresh capture");
    }

    /// The verdict itself is the vetted child walk, read from the shared
    /// capture: every session asks its own shell's question of one table.
    #[test]
    fn the_verdict_is_the_child_walk_of_the_shared_capture() {
        let mut probe = JobProbe::default();
        let now = Instant::now();
        let links = ChildLinks::from_links(vec![(42, 500), (99, 501)]);
        assert!(probe.child_verdict(1, key(0), now, || links.clone(), job_of(42)));
        assert!(probe.child_verdict(2, key(0), now, || links.clone(), job_of(99)));
        assert!(!probe.child_verdict(3, key(0), now, || links.clone(), job_of(7)));
    }

    /// A retired session's evidence is dropped, so a recycled id re-probes
    /// instead of inheriting a dead tab's answer.
    #[test]
    fn forget_drops_the_retired_sessions_evidence() {
        let mut probe = JobProbe::default();
        let t0 = Instant::now();
        let mut captures = 0usize;
        let mut capture = || {
            captures += 1;
            ChildLinks::default()
        };
        probe.child_verdict(5, key(0), t0, &mut capture, job_of(1234));
        assert_eq!(probe.capture_count(), 1);
        probe.forget(5);
        let t1 = t0 + Duration::from_millis(250);
        probe.child_verdict(5, key(0), t1, &mut capture, job_of(1234));
        assert_eq!(
            captures, 2,
            "a forgotten session has no remembered evidence, so it re-probes"
        );
    }

    /// The unix arm is untouched: a live `tcgetpgrp` answer short-circuits
    /// before any capture, and keeps no state.
    #[test]
    fn a_live_foreground_pgrp_never_buys_a_capture() {
        let mut probe = JobProbe::default();
        let now = Instant::now();
        let mut captures = 0usize;
        let mut capture = || {
            captures += 1;
            ChildLinks::default()
        };
        assert!(
            probe.is_job_with(1, 4321, 1234, key(0), now, &mut capture),
            "a group in front that is not the shell's is a job"
        );
        assert!(
            !probe.is_job_with(1, 1234, 1234, key(0), now, &mut capture),
            "the shell's own group in front is idle"
        );
        let busy = probe.is_job_with(1, -1, 0, key(0), now, &mut capture);
        assert!(!busy, "an unknown shell pid is idle, and buys nothing");
        assert_eq!(captures, 0);
        assert_eq!(probe.capture_count(), 0);
    }

    /// The program probe on real processes: a stand-in shell (`sh -c`, which
    /// runs its programs in its own group, as PowerShell does) running a
    /// program it started since an instant is seen; the same program before
    /// the instant is not; and one the shell put in a group of its own (`set
    /// -m`, as zsh, bash and fish do) is the foreground word's, never the
    /// probe's.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_probe_sees_a_program_the_shell_runs_in_its_own_group() {
        let now_ms = || {
            u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("after the epoch")
                    .as_millis(),
            )
            .expect("fits")
        };
        let since = now_ms().saturating_sub(100);
        let home = aterm_tempfile::tempdir().expect("scratch home");
        let shell = |program: &std::path::Path, script: &str| {
            aterm_spec::host::shell_command(program, home.path())
                .args(["-c", script])
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
        };
        struct Started(Vec<aterm_uds::spawnfd::Child>);
        impl Started {
            fn hold(&mut self, child: aterm_uds::spawnfd::Child) -> i32 {
                let pid = i32::try_from(child.id()).expect("pid fits");
                self.0.push(child);
                pid
            }
        }
        impl Drop for Started {
            fn drop(&mut self) {
                for shell in &mut self.0 {
                    let programs = i32::try_from(shell.id())
                        .ok()
                        .and_then(session_root)
                        .map(|root| root.children)
                        .unwrap_or_default();
                    for program in programs {
                        // SAFETY: SIGKILL to a `sleep` this test's shell forked.
                        unsafe { libc::kill(program, libc::SIGKILL) };
                    }
                    let _ = shell.kill();
                    let _ = shell.wait();
                }
            }
        }
        let mut started = Started(Vec::new());
        let bash = aterm_spec::host::test_shell("bash").unwrap_or_else(|| {
            aterm_spec::host::could_not_run(aterm_spec::host::tool("bash").refusal())
        });
        let job_control = started.hold(
            shell(&bash, "set -m; sleep 120 & wait")
                .unwrap_or_else(|error| panic!("spawn {}: {error}", bash.display())),
        );
        let own_group =
            started.hold(shell(std::path::Path::new("/bin/sh"), "sleep 120; :").expect("spawn sh"));
        let forks = |pid: i32| {
            aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(5),
                || {
                    session_root(pid)
                        .filter(|root| !root.children.is_empty())
                        .map(drop)
                },
            )
            .expect("the shell forked its program");
        };
        forks(own_group);
        assert!(shell_runs_program_since(own_group, since), "started since");
        assert!(
            !shell_runs_program_since(own_group, now_ms() + 3_600_000),
            "started before the instant"
        );
        forks(job_control);
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || {
                session_root(job_control)
                    .filter(|root| {
                        !root.children.is_empty()
                            && root.children.iter().all(|&program| {
                                // SAFETY: getpgid reads a process's group.
                                let group = unsafe { libc::getpgid(program) };
                                group == program
                            })
                    })
                    .map(drop)
            },
        )
        .expect("bash moved its program to a group of its own");
        assert!(
            !shell_runs_program_since(job_control, since),
            "a program in a group of its own"
        );
        assert!(!shell_runs_program_since(-1, since), "no shell");
    }
}
