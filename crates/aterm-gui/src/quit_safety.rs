// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 quit-safety guard (audit): a close/quit request while a foreground job is
//! running is REFUSED the first time — a mis-click or a stray Cmd-W/Cmd-Q must not
//! kill an in-flight build or AI run — and confirmed by a second request inside a
//! brief window. The decision logic is pure and unit-tested here; `App` wires the
//! PTY (`tcgetpgrp` on the session master on unix; a child-process walk of the
//! shell pid on windows) and the winit title/timer shims.

use std::time::{Duration, Instant};

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

/// How long ONE system process-table capture may keep serving the tab-status
/// observer while no session's evidence has moved.
///
/// This is the CEILING, not the cadence: any session whose observable evidence
/// changed (a grid write, a byte of PTY output, a keystroke) forces a fresh
/// capture in that same sweep, so every state change a user can cause is still
/// answered against a snapshot taken after it. The ceiling exists only so a
/// completely silent, input-less job start — a shell that spawns a child while
/// emitting nothing at all — still converges, and 2 s bounds that worst case.
///
/// Sized against [`crate::session_status`]'s default 250 ms observation
/// interval: eight idle prompts used to buy eight system snapshots every
/// 250 ms (~32/s, ~100 ms of CPU per second on the machine this was measured
/// on). With this ceiling an idle window of ANY tab count buys one every 2 s.
pub(crate) const JOB_PROBE_MAX_AGE: Duration = Duration::from_secs(2);

/// The observable evidence a foreground-job verdict is derived from — the
/// inputs whose movement means a fresh process-table capture is owed.
///
/// A shell cannot acquire a foreground child without consuming input or
/// emitting output, and both of those move one of these fields, so an
/// unchanged key means the previous verdict is still the right answer.
/// [`JobProbe::JOB_PROBE_MAX_AGE`](JOB_PROBE_MAX_AGE) bounds the case where
/// that reasoning does not hold.
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

/// Every parent pid that owned a live child at one instant, as a SORTED,
/// deduplicated list.
///
/// A sorted `Vec` rather than a `HashSet` on purpose. The capture books one
/// entry per process on the machine (several hundred), and it is read only a
/// handful of times per sweep — once per due session. Hashing several hundred
/// keys to answer eight membership questions is the wrong trade, and measurably
/// so: the `HashSet` form of this capture cost ~15 ms against ~3.9 ms for the
/// single-pid walk it replaces, which would have handed back most of the saving
/// this type exists to make. Sort once, binary-search per session.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildParents(Vec<u32>);

impl ChildParents {
    /// Sort + dedup a raw parent-pid list into a searchable capture.
    #[cfg(any(windows, test))]
    pub(crate) fn from_parent_pids(mut pids: Vec<u32>) -> Self {
        pids.sort_unstable();
        pids.dedup();
        Self(pids)
    }

    /// Whether `pid` owned a live child when this capture was taken — the
    /// membership test [`windows_jobs::has_child_process`] answers one pid at a
    /// time.
    pub(crate) fn contains(&self, pid: u32) -> bool {
        self.0.binary_search(&pid).is_ok()
    }
}

/// The rate-limited, BATCHED foreground-job oracle behind the tab-status
/// observer's `foreground_job` evidence.
///
/// THE DEFECT IT REMOVES. [`foreground_is_job`] answers one pid per call and
/// pays a whole system process-table snapshot to do it (~3.1 ms measured on
/// the machine this was found on). The status observer called it once per due
/// session per sweep, so the idle cost was `sessions / observe_interval`
/// system snapshots per second — 32/s for an idle eight-tab window, and the
/// reason such a window cost several times an idle one-tab window while
/// presenting the same two frames a second. None of it appeared in the
/// scheduler's deadline counters, because it rides the per-turn sweep rather
/// than any armed wake.
///
/// WHAT THIS CHANGES. Nothing about the verdict: the predicate is the same
/// parent-pid test, and any evidence movement still forces a capture taken
/// AFTER that movement. What changes is HOW MANY captures answer it — at most
/// one per sweep no matter how many sessions are due (they all read the same
/// capture), and at most one per [`JOB_PROBE_MAX_AGE`] while nothing moves.
///
/// The close-confirm path keeps calling [`foreground_is_job`] directly: it
/// asks once, at the instant of a close gesture, and must never answer from a
/// cache.
#[derive(Default)]
pub(crate) struct JobProbe {
    /// The last capture and when it was taken. Every session in a sweep reads
    /// this one set.
    snapshot: Option<(Instant, ChildParents)>,
    /// Per-session evidence at the moment its verdict was last derived.
    keys: std::collections::HashMap<u64, JobEvidenceKey>,
    /// How many process-table captures this probe has taken. THE regression
    /// number for the idle-wake fix: it must not scale with session count, and
    /// an idle window must add at most one per [`JOB_PROBE_MAX_AGE`].
    /// Instance-local on purpose — a process-global counter could not be
    /// asserted on from a parallel test suite.
    captures: u64,
}

impl JobProbe {
    /// See [`Self::captures`].
    #[cfg(test)]
    pub(crate) fn capture_count(&self) -> u64 {
        self.captures
    }

    /// The observer's entry point: is `shell_pid` running a foreground job?
    ///
    /// `now` must be the sweep's single clock reading — sharing it is what
    /// makes "one capture per sweep" exact.
    pub(crate) fn is_job(
        &mut self,
        session: u64,
        master: i32,
        shell_pid: i32,
        key: JobEvidenceKey,
        now: Instant,
    ) -> bool {
        self.is_job_with(session, master, shell_pid, key, now, capture_child_parents)
    }

    /// [`Self::is_job`] with the system capture injected, so the batching and
    /// rate limit are provable without a process table.
    pub(crate) fn is_job_with(
        &mut self,
        session: u64,
        master: i32,
        shell_pid: i32,
        key: JobEvidenceKey,
        now: Instant,
        capture: impl FnOnce() -> ChildParents,
    ) -> bool {
        // The unix answer is a cheap per-fd `tcgetpgrp`, never a process-table
        // walk: take it verbatim and keep no state for it.
        let fg = foreground_pgrp(master);
        if fg > 0 {
            self.keys.remove(&session);
            return fg != shell_pid;
        }
        if !cfg!(windows) || shell_pid <= 0 {
            // Matches `foreground_is_job`'s non-windows / unknown-pid arm.
            return false;
        }
        self.child_verdict(session, shell_pid as u32, key, now, capture)
    }

    /// The rate-limited child-walk oracle, free of any platform call so the
    /// scheduling laws are unit-testable everywhere.
    fn child_verdict(
        &mut self,
        session: u64,
        shell_pid: u32,
        key: JobEvidenceKey,
        now: Instant,
        capture: impl FnOnce() -> ChildParents,
    ) -> bool {
        // Taken THIS sweep already: every other due session reads it rather
        // than buying a second snapshot of the same instant.
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
            .is_some_and(|(_, parents)| parents.contains(shell_pid))
    }

    /// Drop a retired session's evidence so the map cannot outlive the pool.
    pub(crate) fn forget(&mut self, session: u64) {
        self.keys.remove(&session);
    }
}

/// One system process-table capture. Non-windows never reaches this (the child
/// walk is the windows-only arm) and answers with an empty set.
fn capture_child_parents() -> ChildParents {
    #[cfg(windows)]
    {
        ChildParents::from_parent_pids(windows_jobs::child_parent_pids())
    }
    #[cfg(not(windows))]
    {
        ChildParents::default()
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
                    && entry_counts_as_child(
                        entry.th32ProcessID,
                        entry.th32ParentProcessID,
                        exe_name_is(&entry.szExeFile, CONHOST),
                    )
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

    /// EVERY parent pid that currently has at least one live, non-conhost
    /// child — one Toolhelp32 pass that answers [`has_child_process`]'s
    /// question for **every** pid at once.
    ///
    /// WHY THIS EXISTS. `has_child_process` takes its own system-wide snapshot
    /// per call, and the tab-status observer calls it once per due session on
    /// every sweep. On this machine one snapshot costs ~2.2 ms in the kernel
    /// plus ~1.6 ms of walk, so an eight-tab window at the default 250 ms
    /// observation interval was spending ~100 ms of CPU per second answering
    /// "is a job running?" for eight idle prompts. The set form makes the cost
    /// O(1) snapshots per sweep instead of O(sessions) — see
    /// [`super::JobProbe`], which also rate-limits how often the set is
    /// refreshed. The predicate is byte-for-byte the one above: a child whose
    /// parent is `pid`, that is not `pid` itself, and is not the console host.
    pub(super) fn child_parent_pids() -> Vec<u32> {
        let mut parents = Vec::new();
        // SAFETY: standard Toolhelp walk — the entry is a plain #[repr(C)]
        // out-param with `dwSize` set as the API requires, and the snapshot
        // handle is closed on every path.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap == 0 {
                // A failed snapshot reads as "nobody has children" — the same
                // fail-idle posture `has_child_process` keeps.
                return parents;
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
                    parents.push(entry.th32ParentProcessID);
                }
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
        }
        parents
    }

    /// THE ONE child-counting predicate. Both walks above apply it to every
    /// snapshot entry — [`has_child_process`] after narrowing to `parent ==
    /// pid`, [`child_parent_pids`] before booking the parent into the set — so
    /// the single-pid answer and the set answer cannot drift: `pid` is in the
    /// set exactly when `has_child_process(pid)` is true of the same snapshot.
    /// A process that lists ITSELF as its parent is not its own job, and the
    /// console host the OS attaches to a console process is plumbing, never a
    /// user job (a shell that allocated a classic console must not read as busy
    /// forever).
    pub(super) const fn entry_counts_as_child(
        entry_pid: u32,
        entry_parent: u32,
        is_conhost: bool,
    ) -> bool {
        entry_pid != entry_parent && !is_conhost
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

    /// The set walk and the single-pid walk must answer the same question of
    /// the same snapshot — they share one predicate, and this pins it.
    #[cfg(windows)]
    #[test]
    fn the_two_child_walks_share_one_predicate() {
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

    // ---------------------------------------------------------------- probe --
    //
    // IDLE-WAKE REGRESSION GUARDS. These pin the two properties that make an
    // idle window's foreground-job evidence cost O(1) system captures instead
    // of O(sessions x sweeps): one capture serves a whole sweep, and a sweep
    // whose evidence did not move buys no capture at all until the ceiling.
    // The capture is injected, so the laws are proven without a process table
    // and without spawning anything.

    fn key(seq: u64) -> JobEvidenceKey {
        JobEvidenceKey {
            content_seq: seq,
            alt_screen: false,
            output_ns: 0,
            input: None,
        }
    }

    /// `master` values are ConPTY registry keys on windows (>= 0x4000_0000) and
    /// never a real fd here; the probe tests drive `child_verdict` directly so
    /// no `tcgetpgrp` is involved on any platform.
    #[test]
    fn one_capture_answers_every_session_in_a_sweep() {
        let mut probe = JobProbe::default();
        let now = Instant::now();
        let mut captures = 0usize;
        // Eight tabs, all due in the same sweep, none of them running a job.
        for session in 0..8u64 {
            let busy = probe.child_verdict(session, 1000 + session as u32, key(0), now, || {
                captures += 1;
                ChildParents::default()
            });
            assert!(!busy, "no pid has a child in this fixture");
        }
        assert_eq!(
            captures, 1,
            "a sweep must buy ONE system process-table capture, not one per session"
        );
        assert_eq!(probe.capture_count(), 1);
    }

    /// A sweep whose evidence has not moved reuses the last capture until the
    /// ceiling — the idle property. At the ceiling exactly one is bought, and
    /// it again serves every session.
    #[test]
    fn a_settled_window_buys_one_capture_per_ceiling() {
        let mut probe = JobProbe::default();
        let t0 = Instant::now();
        let mut captures = 0usize;
        let sweep = |probe: &mut JobProbe, at: Instant, captures: &mut usize| {
            for session in 0..8u64 {
                probe.child_verdict(session, 1000 + session as u32, key(0), at, || {
                    *captures += 1;
                    ChildParents::default()
                });
            }
        };
        // Four sweeps a second for just under the ceiling: the shape an idle
        // eight-tab window used to pay 8 captures per sweep for.
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
        // At the ceiling, one fresh capture — still one for all eight.
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
            ChildParents::default()
        };
        assert!(!probe.child_verdict(7, 1234, key(0), t0, || idle(&mut captures)));
        assert_eq!(captures, 1);
        // Same evidence, a later sweep inside the ceiling: no capture.
        let t1 = t0 + Duration::from_millis(250);
        assert!(!probe.child_verdict(7, 1234, key(0), t1, || idle(&mut captures)));
        assert_eq!(captures, 1, "a settled session buys nothing");
        // The grid moved: capture again, and see the job this time.
        let t2 = t1 + Duration::from_millis(250);
        let busy = probe.child_verdict(7, 1234, key(1), t2, || {
            captures += 1;
            ChildParents::from_parent_pids(vec![1234])
        });
        assert!(
            busy,
            "the fresh capture must be what the verdict is read from"
        );
        assert_eq!(captures, 2, "moved evidence buys exactly one fresh capture");
    }

    /// The verdict itself is the parent-set membership test — unchanged from
    /// the per-pid walk it replaces.
    #[test]
    fn the_verdict_is_parent_set_membership() {
        let mut probe = JobProbe::default();
        let now = Instant::now();
        let parents = ChildParents::from_parent_pids(vec![42, 99]);
        assert!(probe.child_verdict(1, 42, key(0), now, || parents.clone()));
        assert!(probe.child_verdict(2, 99, key(0), now, || parents.clone()));
        assert!(!probe.child_verdict(3, 7, key(0), now, || parents.clone()));
    }

    /// A retired session's evidence is dropped, so a recycled id re-probes
    /// instead of inheriting a dead tab's answer.
    #[test]
    fn forget_drops_the_retired_sessions_evidence() {
        let mut probe = JobProbe::default();
        let t0 = Instant::now();
        let mut captures = 0usize;
        probe.child_verdict(5, 1234, key(0), t0, || {
            captures += 1;
            ChildParents::default()
        });
        assert_eq!(captures, 1);
        probe.forget(5);
        let t1 = t0 + Duration::from_millis(250);
        probe.child_verdict(5, 1234, key(0), t1, || {
            captures += 1;
            ChildParents::default()
        });
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
        // `is_job_with` reads `foreground_pgrp(master)`; on windows that is
        // always -1, so drive the short-circuit through `child_verdict`'s
        // caller only where it can be observed. The state-free property is
        // what matters here and holds on both arms.
        let busy = probe.is_job_with(1, -1, 0, key(0), now, || {
            captures += 1;
            ChildParents::default()
        });
        assert!(!busy, "an unknown shell pid is idle, and buys nothing");
        assert_eq!(captures, 0);
        assert_eq!(probe.capture_count(), 0);
    }
}
