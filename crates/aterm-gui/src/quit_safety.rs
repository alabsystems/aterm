// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 quit-safety guard (audit): a close/quit request while a foreground job is
//! running is REFUSED the first time — a mis-click or a stray Cmd-W/Cmd-Q must not
//! kill an in-flight build or AI run — and confirmed by a second request inside a
//! brief window. The decision logic is pure and unit-tested here; `App` wires the
//! PTY (`tcgetpgrp` on the session master on unix; a child-process walk of the
//! shell pid on windows) and the winit title/timer shims.

use std::time::Duration;

/// How long a refused close keeps the confirm window armed: a second request inside
/// this window proceeds; after it lapses the program title is restored and the next
/// request starts over.
pub(crate) const CLOSE_CONFIRM_WINDOW: Duration = Duration::from_secs(2);

/// The titlebar hint shown while the confirm window is armed.
pub(crate) const CLOSE_WARNING_TITLE: &str = "job running — press again to close";

/// What a close/quit request should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseDecision {
    /// Proceed now: nothing is busy, or this is the confirming second request.
    Close,
    /// Refuse and arm the warning: a foreground job is running and no confirm
    /// window is currently armed.
    Warn,
}

/// Decide a close/quit request: a busy foreground job refuses the FIRST request
/// (arming the confirm window); a second request while armed — or any request with
/// nothing busy — proceeds.
pub(crate) fn close_decision(foreground_busy: bool, warning_armed: bool) -> CloseDecision {
    if foreground_busy && !warning_armed {
        CloseDecision::Warn
    } else {
        CloseDecision::Close
    }
}

/// The copy for a destructive close/quit confirmation dialog. Static strings, so the
/// platform layer (`AppRt::confirm` → the native `NSAlert`) just hands them straight
/// to the alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfirmPrompt {
    /// The alert's primary message (`NSAlert.messageText`).
    pub title: &'static str,
    /// The secondary explanatory line (`NSAlert.informativeText`).
    pub body: &'static str,
    /// The affirmative (destructive) button label; a "Cancel" button is always added
    /// alongside it.
    pub proceed: &'static str,
}

/// Decide whether a close/quit gesture needs a confirmation dialog, and with what
/// copy. `exits_app` = the gesture quits the WHOLE program (Cmd-Q / app-menu Quit, or
/// a close that would remove the last window); `busy` = closing now would SIGHUP a
/// foreground job.
///
/// * A whole-app quit ALWAYS confirms — the iTerm-style "are you sure you want to
///   quit?" prompt the user expects from ⌘Q — with copy that notes a running job.
/// * A window/tab close that LEAVES the app running confirms only while a job is
///   running, so an idle close stays instant.
/// * An idle close that leaves the app running needs no confirmation (`None`).
pub(crate) fn confirm_prompt(exits_app: bool, busy: bool) -> Option<ConfirmPrompt> {
    if exits_app {
        Some(ConfirmPrompt {
            title: "Are you sure you want to quit aterm?",
            body: if busy {
                "A process is still running. Quitting will end it and close every window."
            } else {
                "This will close every window and end your terminal sessions."
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

/// The WINDOWS close-confirm policy — Windows Terminal's convention
/// (`confirmCloseAllTabs`), NOT macOS's: confirm when the gesture would close
/// MULTIPLE tabs or end a running job, and NEVER for a single idle tab — an
/// idle Alt+F4 on a one-tab window closes instantly, even when it is the last
/// window and thus quits the app. macOS's always-confirm-on-quit rule
/// ([`confirm_prompt`]'s `exits_app` arm) is deliberately NOT ported: ⌘Q is a
/// one-key slip next to ⌘W, while a Windows quit is Alt+F4 / the caption ✕ —
/// gestures no neighbouring key mistypes — and no native Windows app confirms
/// an idle single-document close.
///
/// `tabs_closing` is the gesture's blast radius: every tab the close would take
/// with it (the closing window's tabs, or all windows' for a whole-app quit).
/// The busy/quit copy is shared verbatim with [`confirm_prompt`]; only the
/// idle-multi-tab close of a NON-last window needs copy of its own (macOS never
/// prompts there at all).
#[cfg(windows)]
pub(crate) fn confirm_prompt_windows(
    exits_app: bool,
    tabs_closing: usize,
    busy: bool,
) -> Option<ConfirmPrompt> {
    if !busy && tabs_closing <= 1 {
        return None; // a single idle tab: instant, per the WT convention
    }
    confirm_prompt(exits_app, busy).or(Some(ConfirmPrompt {
        title: "Close all tabs?",
        body: "This window has several tabs open. Closing it will end all of their sessions.",
        proceed: "Close",
    }))
}

/// Whether a PTY's foreground process group is a JOB rather than the shell itself.
/// `tcgetpgrp(master)` returns the shell's own pgid (== its pid: the forkpty child
/// is the session leader) at an idle prompt, the running job's pgid while one runs,
/// and <= 0 on error — treated as idle on unix so a broken/torn-down PTY can never
/// wedge a window open. windows: ConPTY has no `tcgetpgrp` ([`foreground_pgrp`] is
/// always `-1` there), so the `fg_pgrp <= 0` case falls through to a Toolhelp32
/// process-tree walk instead — a shell with a live CHILD process is running a job
/// (the honest ConPTY analogue; the pty host is a child of aterm, not the shell,
/// so it never counts). A dead/unknown shell has no children and stays idle.
pub(crate) fn foreground_is_job(fg_pgrp: i32, shell_pid: i32) -> bool {
    if fg_pgrp > 0 {
        return fg_pgrp != shell_pid;
    }
    #[cfg(windows)]
    {
        shell_pid > 0 && windows_jobs::has_child_process(shell_pid as u32)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The PTY master's foreground process group, or `-1` when unknown.
///
/// unix: `tcgetpgrp(master)`. windows: a ConPTY has no `tcgetpgrp` analogue, so
/// this is always `-1` — [`foreground_is_job`] then detects a running job by
/// enumerating the shell's child processes instead.
pub(crate) fn foreground_pgrp(master: i32) -> i32 {
    #[cfg(unix)]
    {
        // SAFETY: `tcgetpgrp` on a PTY master fd; a <= 0 error return is
        // treated as idle by the callers.
        unsafe { libc::tcgetpgrp(master) }
    }
    #[cfg(windows)]
    {
        let _ = master;
        -1
    }
}

/// Windows running-job detection: one Toolhelp32 pass over the system process
/// list, reporting whether `pid` has any live child. Direct kernel32 FFI in the
/// house tiny-FFI style (see `aterm-pty/src/windows/ffi.rs`); SDK names are kept
/// verbatim so the struct layout can be checked line by line.
#[cfg(windows)]
mod windows_jobs {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> isize;
        fn Process32FirstW(hSnapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
        fn Process32NextW(hSnapshot: isize, lppe: *mut PROCESSENTRY32W) -> i32;
        fn CloseHandle(hObject: isize) -> i32;
    }

    /// `TH32CS_SNAPPROCESS` — snapshot every process in the system.
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: isize = -1;

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

    /// The console host the OS attaches to a console process — plumbing, never a
    /// user job, so the child walk ignores it (a shell that allocated a classic
    /// console must not read as busy forever).
    const CONHOST: &str = "conhost.exe";

    /// Whether any live process lists `pid` as its parent. A failed snapshot
    /// reads as "no children" — an unknown state must never wedge a close, the
    /// same fail-idle posture as a `tcgetpgrp` error on unix. (A recycled pid
    /// could in principle match a stale parent id, but this only ever arms a
    /// confirmation, never blocks outright.)
    pub(super) fn has_child_process(pid: u32) -> bool {
        // SAFETY: standard Toolhelp walk — the entry is a plain #[repr(C)]
        // out-param with `dwSize` set as the API requires, and the snapshot
        // handle is closed on every path.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap == 0 {
                return false;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            let mut found = false;
            while ok != 0 {
                if entry.th32ParentProcessID == pid
                    && entry.th32ProcessID != pid
                    && !exe_name_is(&entry.szExeFile, CONHOST)
                {
                    found = true;
                    break;
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            found
        }
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
    fn close_refused_first_time_while_a_job_runs() {
        assert_eq!(close_decision(true, false), CloseDecision::Warn);
    }

    #[test]
    fn close_confirmed_second_time_within_the_window() {
        assert_eq!(close_decision(true, true), CloseDecision::Close);
    }

    #[test]
    fn close_immediate_when_nothing_is_busy() {
        assert_eq!(close_decision(false, false), CloseDecision::Close);
        assert_eq!(close_decision(false, true), CloseDecision::Close);
    }

    #[test]
    fn quit_always_prompts_and_notes_a_running_job() {
        // ⌘Q confirms whether or not a job is running (the user asked for an
        // always-on "are you sure?"), and the affirmative button reads "Quit".
        let idle = confirm_prompt(true, false).expect("a whole-app quit always confirms");
        assert_eq!(idle.proceed, "Quit");
        let busy = confirm_prompt(true, true).expect("a whole-app quit always confirms");
        assert_eq!(busy.proceed, "Quit");
        assert_ne!(
            idle.body, busy.body,
            "the busy-quit copy must mention the running process"
        );
    }

    #[test]
    fn window_close_confirms_only_while_busy() {
        // Closing one window/tab while the app keeps running: instant when idle,
        // confirmed (with a "Close" button) only while a job is running.
        assert!(
            confirm_prompt(false, false).is_none(),
            "an idle window/tab close needs no confirmation"
        );
        let busy = confirm_prompt(false, true).expect("a busy window/tab close confirms");
        assert_eq!(busy.proceed, "Close");
    }

    /// The Windows policy (WT convention): a single idle tab NEVER prompts —
    /// including the last-window case that quits the app, which is exactly the
    /// macOS always-confirm rule this policy refuses to port.
    #[cfg(windows)]
    #[test]
    fn windows_single_idle_tab_closes_without_a_prompt() {
        assert!(confirm_prompt_windows(false, 1, false).is_none());
        assert!(
            confirm_prompt_windows(true, 1, false).is_none(),
            "an idle one-tab last window must quit instantly on Windows"
        );
        assert!(confirm_prompt_windows(false, 0, false).is_none());
    }

    /// Busy always prompts, with the shared macOS copy: "Quit" for a whole-app
    /// exit, "Close" otherwise — the REAL verbs the TaskDialog buttons carry.
    #[cfg(windows)]
    #[test]
    fn windows_busy_prompts_with_the_real_verbs() {
        let busy_close = confirm_prompt_windows(false, 1, true).expect("busy close confirms");
        assert_eq!(busy_close.proceed, "Close");
        let busy_quit = confirm_prompt_windows(true, 1, true).expect("busy quit confirms");
        assert_eq!(busy_quit.proceed, "Quit");
    }

    /// Closing MULTIPLE tabs prompts even when idle: quit copy when the gesture
    /// exits the app, the multi-tab close copy when another window survives
    /// (a case macOS never prompts on, hence the dedicated copy).
    #[cfg(windows)]
    #[test]
    fn windows_multi_tab_close_prompts_when_idle() {
        let quit = confirm_prompt_windows(true, 4, false).expect("multi-tab quit confirms");
        assert_eq!(quit.proceed, "Quit");
        let close = confirm_prompt_windows(false, 4, false).expect("multi-tab close confirms");
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
            !foreground_is_job(1234, 1234),
            "the shell's own pgrp at the prompt is idle"
        );
        assert!(
            foreground_is_job(5678, 1234),
            "a different foreground pgrp is a running job"
        );
        // windows routes fg_pgrp <= 0 through the live child-process walk
        // (covered by its own tests below); unix treats it as idle outright.
        #[cfg(not(windows))]
        {
            assert!(!foreground_is_job(0, 1234), "no foreground pgrp is idle");
            assert!(
                !foreground_is_job(-1, 1234),
                "a tcgetpgrp error must not wedge the close"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_invalid_shell_pid_is_idle() {
        assert!(
            !foreground_is_job(-1, 0),
            "an unknown shell pid must not wedge the close"
        );
        assert!(!foreground_is_job(-1, -5));
    }

    #[cfg(windows)]
    #[test]
    fn windows_child_walk_detects_a_live_child() {
        // A long-lived, childless child of THIS process (killed below); ping is
        // present on every Windows install.
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        assert!(
            foreground_is_job(-1, std::process::id() as i32),
            "a live child process must read as a running job"
        );
        assert!(
            !foreground_is_job(-1, child.id() as i32),
            "a process with no children is idle"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
