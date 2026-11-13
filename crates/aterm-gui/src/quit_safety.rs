// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 quit-safety guard (audit): a close/quit request while a foreground job is running asks first.

use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use aterm_uds::process::child_pids;

/// The copy for a destructive close/quit confirmation: a native sheet on a Mac window, the
/// in-window band elsewhere ([`crate::close_confirm`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfirmPrompt {
    /// The question.
    pub title: &'static str,
    /// One short line saying what the answer ends.
    pub body: &'static str,
    /// The affirmative (destructive) button label; a "Cancel" button is always added alongside it.
    pub proceed: &'static str,
}

/// The question a quit asks.
pub(crate) const QUIT_TITLE: &str = "Quit aterm?";

/// Whether a close/quit gesture needs a confirmation dialog, and with what copy.
pub(crate) const fn confirm_prompt(
    exits_app: bool,
    busy: bool,
    reopens: bool,
) -> Option<ConfirmPrompt> {
    if exits_app {
        Some(ConfirmPrompt {
            title: QUIT_TITLE,
            body: match (busy, reopens) {
                (true, true) => {
                    "A process is still running and will end. Windows and tabs reopen next time."
                }
                (false, true) => "Terminal sessions will end. Windows and tabs reopen next time.",
                (true, false) => "A process is still running and will end with every session.",
                (false, false) => "All terminal sessions will end.",
            },
            proceed: "Quit",
        })
    } else if busy {
        Some(ConfirmPrompt {
            title: "Close this window?",
            body: "A process is still running in it and will end.",
            proceed: "Close",
        })
    } else {
        None
    }
}

/// The question a quit asks when it leaves the Mac Setup tab's install running for the next
/// launch (`App::developer_tools_custody_at_exit`): its program goes on in the keeper's custody
/// and its tab comes back, so the copy says so, and says a process ends only when one of another
/// session runs (`others_busy`).
pub(crate) const fn quit_prompt_carrying(others_busy: bool, reopens: bool) -> ConfirmPrompt {
    ConfirmPrompt {
        title: QUIT_TITLE,
        body: match (others_busy, reopens) {
            (true, true) => {
                "A process is still running and will end; the Mac Setup install keeps running. \
                 Windows and tabs reopen next time, with it."
            }
            (false, true) => {
                "Terminal sessions will end; the Mac Setup install keeps running. Windows and \
                 tabs reopen next time, with it."
            }
            (true, false) => {
                "A process is still running and will end with every session but the Mac Setup \
                 install, which keeps running and reopens next time."
            }
            (false, false) => {
                "Terminal sessions will end; the Mac Setup install keeps running and reopens \
                 next time."
            }
        },
        proceed: "Quit",
    }
}

/// The affirmative verb of a busy TAB close's confirm ([`busy_view_close_prompt`]), which names the wire `kind=close-tab` (`close_confirm::kind`).
pub(crate) const CLOSE_TAB: &str = "Close Tab";
/// The affirmative verb of a busy PANE close's confirm (`kind=close-pane`).
pub(crate) const CLOSE_PANE: &str = "Close Pane";

/// The copy for closing a tab (`pane` false) or a pane that leaves its window standing while a job runs in it.
pub(crate) const fn busy_view_close_prompt(pane: bool) -> ConfirmPrompt {
    ConfirmPrompt {
        title: if pane {
            "Close this pane?"
        } else {
            "Close this tab?"
        },
        body: "A process is still running in it and will end.",
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
        return None;
    }
    confirm_prompt(exits_app, busy, reopens).or(Some(ConfirmPrompt {
        title: "Close all tabs?",
        body: "Every tab's session in this window will end.",
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
pub(crate) const fn shell_reports_commands(term: &aterm_core::terminal::Terminal) -> bool {
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

const LAUNCHER_WINDOW_MS: u64 = 2_000;

impl ProcessFacts {
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
        // process-table walk: take it verbatim and keep no state for it.
        if fg > 0 {
            self.keys.remove(&session);
            return fg != shell_pid;
        }
        if !cfg!(windows) || shell_pid <= 0 {
            return false;
        }
        self.child_verdict(session, key, now, capture, |links| {
            links_have_job(links, shell_pid as u32)
        })
    }

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

fn links_have_job(links: &ChildLinks, shell_pid: u32) -> bool {
    links.has_job(shell_pid, None, process_facts)
}

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

/// The PTY master's foreground process group, or `-1` when unknown
/// ([`aterm_pty::foreground_group`]: the read failed or named no group, and always on Windows).
pub(crate) fn foreground_pgrp(master: i32) -> i32 {
    aterm_pty::foreground_group(master).unwrap_or(-1)
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

/// Whether `comm` names a shell (a leading login `-` ignored): the jobs engine's one list,
/// which also tells an agent call's wrapper from the program it ran
/// ([`aterm_vitals::jobs::is_shell_name`]).
pub(crate) fn is_shell_name(comm: &str) -> bool {
    aterm_vitals::jobs::is_shell_name(comm)
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
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
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
    let stat =
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .ok()?;
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
    let comm =
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/comm")))
            .ok()?;
    Some(comm.trim_end().to_string())
}

#[cfg(target_os = "linux")]
fn platform_session_root(pid: i32) -> Option<SessionRoot> {
    let comm = process_name(pid)?;
    let children = aterm_uds::process::read_proc_text(std::path::Path::new(&format!(
        "/proc/{pid}/task/{pid}/children"
    )))
    .ok()?;
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

    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: isize = -1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
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

    const CONHOST: &str = "conhost.exe";

    /// Every live parent -> child link, in one Toolhelp32 pass (~2.2 ms in the kernel plus ~1.6 ms of walk), so one capture answers every shell.
    pub(super) fn capture_links() -> ChildLinks {
        let mut links = Vec::new();
        // SAFETY: standard Toolhelp walk — the entry is a plain #[repr(C)]
        // out-param with `dwSize` set as the API requires, and the snapshot
        // handle is closed on every path.
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
        // SAFETY: `OpenProcess` returns 0 on failure and a handle otherwise,
        // which is closed on every path past it; the FILETIMEs and the name
        // buffer are plain out-params sized as the APIs require, and
        // `QueryFullProcessImageNameW` writes at most `len` units and reports
        // how many it wrote.
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

    fn exe_name_is(exe: &[u16; 260], name: &str) -> bool {
        let len = exe.iter().position(|&u| u == 0).unwrap_or(exe.len());
        char::decode_utf16(exe[..len].iter().copied())
            .map(|c| c.unwrap_or('\u{fffd}').to_ascii_lowercase())
            .eq(name.chars())
    }
}

#[cfg(test)]
#[path = "quit_safety_tests.rs"]
mod tests;
