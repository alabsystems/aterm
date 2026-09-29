// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M2 quit-safety guard (audit): a close/quit request while a foreground job is
//! running is REFUSED the first time — a mis-click or a stray Cmd-W/Cmd-Q must not
//! kill an in-flight build or AI run — and confirmed by a second request inside a
//! brief window. The decision logic is pure and unit-tested here; `App` wires the
//! PTY (`tcgetpgrp` on the session master on unix; a child-process walk of the
//! shell pid on windows, vetted by creation time — [`ChildLinks::has_job`]), the
//! shell-integration state ([`shell_word`]: the shell's own word that a command
//! is running, or that it went idle), and the winit title/timer shims.

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
/// process-tree walk instead — a shell with a live CHILD process it started is
/// running a job (the honest ConPTY analogue; the pty host is a child of aterm,
/// not the shell, so it never counts). What "a child it started" means, and why
/// a matching parent pid alone is not it, is [`ChildLinks::has_job`]. A
/// dead/unknown shell has no children and stays idle.
///
/// `since_ms` (unix epoch ms) is windows-only: count only a child created at or
/// after it — the instant the shell's own integration said it went idle
/// ([`ShellWord::idle_since_ms`]). unix ignores it: `tcgetpgrp` already names
/// the one foreground job, so a background child never reads as busy there.
pub(crate) fn foreground_is_job(fg_pgrp: i32, shell_pid: i32) -> bool {
    foreground_is_job_since(fg_pgrp, shell_pid, None)
}

/// [`foreground_is_job`] with the Windows child-age floor (`since_ms`, see its
/// doc) — the close guard's form. A separate name rather than a third argument
/// on `foreground_is_job`, whose two-argument shape other callers keep using.
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

/// Whether `term`'s shell is EXECUTING a command: the shell said a command
/// started (OSC 133;C) and has not yet said it finished (133;D).
///
/// THE DEFECT THIS CLOSES (audit 2026-09-22). On windows the job oracle is the
/// child-process walk ([`foreground_is_job`]), and a cmdlet, loop or script
/// running INSIDE `pwsh.exe` spawns no child: `close` on a last tab running
/// `Start-Sleep 60` answered `OK closed` and the instance ended, while the same
/// close on `ping -t` was refused. Yet the session model knew the shell was
/// busy in both cases — `status`/`blocks` showed the block executing. This is
/// that knowledge, read from the engine the close gesture is about to hang up.
/// No shell integration (no 133 marks at all) reads as idle, so the process
/// walk stays the only oracle for a bare shell — and a shell whose integration
/// marks only its prompts (cmd.exe's `%PROMPT%` tier, pwsh without PSReadLine)
/// never sends a 133;C either, so it is a bare shell here too.
///
/// It reads the engine's SHELL STATE, not its current block. The two are set
/// together at 133;C and cleared together at 133;D, a new prompt (133;A) and
/// a full reset — but the block is also dropped mid-command by anything that
/// makes absolute rows unknowable: ED 3 (`CSI 3 J`, which `clear` / `cls` /
/// `Clear-Host` emit) and a scrollback invalidation. A running in-shell loop
/// that clears the screen (`while ($true) { Clear-Host; …; Start-Sleep 5 }`)
/// therefore read idle through the block, and on windows the child walk misses
/// it too — defect (a) again. The shell state carries no row, so neither
/// touches it, and a 133;D still ends it (the phase it gates on survives too).
pub(crate) fn shell_executing(term: &aterm_core::terminal::Terminal) -> bool {
    term.shell_state() == aterm_core::terminal::ShellState::Executing
}

/// What the shell's OWN integration says, as the close guard reads it: the
/// two facts [`foreground_busy`] needs from the engine, taken under one lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellWord {
    /// The shell said a command started and has not said it finished
    /// ([`shell_executing`]).
    pub(crate) executing: bool,
    /// When the shell last said a command FINISHED (its 133;D, unix epoch ms)
    /// — `Some` only when that word is both the shell's own and complete:
    /// the host required a capability nonce and has one in use (posture `On`,
    /// so every mark the engine took carried it, and the pwsh, bash, zsh and
    /// fish scripts scrub it from the environment, so no program the shell
    /// runs can sign one), and the shell has reported at least one whole
    /// command (133;C then 133;D — the engine drops a D that no C opened). A
    /// shell that has done both SAYS when it runs something, so from this
    /// instant on a child it had already started is not a foreground job
    /// ([`ChildLinks::has_job`]).
    pub(crate) idle_since_ms: Option<u64>,
}

/// Read [`ShellWord`] from the engine a close is about to hang up.
///
/// `None` for [`ShellWord::idle_since_ms`] is every shell whose idle prompt is
/// silence rather than a statement: no integration, marks a program could
/// forge (no nonce required), a nonce not in use (an adopted shell whose
/// handoff lost it), and an integration that marks prompts but never commands
/// — cmd.exe's `%PROMPT%` tier and pwsh without PSReadLine send no 133;C, so
/// their completed-command count never moves, and their prompt says nothing
/// about what runs behind it.
pub(crate) fn shell_word(term: &aterm_core::terminal::Terminal) -> ShellWord {
    let own_marks =
        term.shell_integration_posture() == aterm_core::terminal::ShellIntegrationPosture::On;
    let reports_commands = term.completed_command_seq() > 0;
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

/// THE close guard's verdict for one session: busy when EITHER the shell said
/// a command is executing ([`ShellWord::executing`]) OR the PTY says a job
/// holds the foreground ([`foreground_is_job`]), read against the instant the
/// shell last said it went idle ([`ShellWord::idle_since_ms`]).
///
/// The shell is consulted FIRST on purpose: on windows the process walk is a
/// whole system snapshot (~3 ms measured), so a session the shell already
/// reports busy never pays for it. Neither signal subsumes the other — a bare
/// shell without integration never says it is executing, and an in-shell loop
/// has no child — so the guard is their union. Every close path (the ctl
/// `close` / `tab close` refusal, the caption ✕, Close Tab, Quit) reads this
/// one predicate, so the four cannot disagree about what "busy" means.
///
/// WHICH CHILDREN COUNT (windows, measured 2026-09-27 on 0.95.0). The walk
/// used to count every live process whose parent pid was the shell's, and
/// that is wrong twice over now that the wire REFUSES a busy close outright:
/// an idle pwsh tab was refused three times because a process created 39 s
/// before that pwsh listed its (recycled) pid as parent, and every idle Git
/// Bash tab was refused because `Git\bin\bash.exe` — the program the ConPTY
/// starts — keeps the real `usr\bin\bash.exe` as its child for life. The rule
/// now: a child counts only if it was created after the shell, the shell a
/// launcher starts is not a job (its own children are), and — once the
/// shell's integration has reported a whole command — only a child created
/// after the shell last said it went idle counts.
///
/// That last rule is a decision, not a measurement, and it is why a program
/// the shell launched IN THE BACKGROUND does not refuse a close: `Start-Process
/// notepad`, a pwsh `Start-Job`, bash's `sleep 60 &` are all created while
/// their command runs, before its 133;D, and the prompt that follows is the
/// shell's word that nothing holds the foreground. That matches unix, where
/// `tcgetpgrp` never names a background job either, and a GUI program is not
/// attached to the ConPTY's console, so the close would not end it anyway. A
/// foreground job has its own reading: its command is an executing block. The
/// child walk still runs behind the shell's word rather than being dropped,
/// because a child created AFTER that word is one the integration failed to
/// announce (a prompt function a profile replaced, PSReadLine removed
/// mid-session) — a job the shell is running without saying so.
pub(crate) fn foreground_busy(fg_pgrp: i32, shell_pid: i32, word: ShellWord) -> bool {
    word.executing || foreground_is_job_since(fg_pgrp, shell_pid, word.idle_since_ms)
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

/// Every live `(parent pid, child pid)` link at one instant, SORTED by parent,
/// deduplicated — the process table as the job walk reads it.
///
/// A sorted `Vec` rather than a hash map on purpose. The capture books one
/// entry per process on the machine (several hundred), and it is read only a
/// handful of times per sweep — once per due session. Hashing several hundred
/// keys to answer eight questions is the wrong trade, and measurably so: the
/// `HashSet` form of this capture cost ~15 ms against ~3.9 ms for the
/// single-pid walk it replaced, which would have handed back most of the
/// saving the batched probe exists to make. Sort once, binary-search per
/// session.
///
/// A link is only a CANDIDATE: a parent pid is a number Windows reuses, so
/// [`Self::has_job`] reads each candidate's creation time before it counts.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChildLinks(Vec<(u32, u32)>);

/// What the job walk reads about one live process, through a handle to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessFacts {
    /// When the process was created, unix epoch milliseconds.
    pub(crate) created_ms: u64,
    /// Its image's file name, ASCII-lowercased (`bash.exe`); empty when the
    /// name could not be read.
    pub(crate) image: String,
}

/// How soon after a shell a same-image child must be created to be the shell
/// that shell LAUNCHED rather than a job. Git for Windows' `bin\bash.exe` —
/// the program a `--shell bash` ConPTY starts — starts `usr\bin\bash.exe`
/// 22–38 ms after itself (seven live pairs measured 2026-09-27), and keeps it
/// as its child for life; a shell a person types at a prompt comes seconds
/// later at the earliest. 2 s is fifty times the measured gap.
const LAUNCHER_WINDOW_MS: u64 = 2_000;

impl ProcessFacts {
    /// Whether this process is the real shell `launcher` started for itself:
    /// the same image name, created inside [`LAUNCHER_WINDOW_MS`] of it. The
    /// caller has already checked it is `launcher`'s child and no older.
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

    /// THE job predicate: whether `shell` has a live child that is a job it
    /// started. `facts` reads a process through a handle
    /// ([`process_facts`]); it is injected so every rule below is
    /// provable without a process table.
    ///
    /// * A child created BEFORE the shell is not its child. Windows reuses
    ///   pids, and a process keeps the parent pid it was born with after that
    ///   parent dies, so a stranger can list a new shell's pid as its parent —
    ///   measured 2026-09-27: an idle pwsh tab refused `tab close` three times
    ///   over an `aterm-gui.exe` created 39 s before that pwsh existed.
    /// * A process whose facts cannot be read is NOT counted. The handle asks
    ///   for `PROCESS_QUERY_LIMITED_INFORMATION`, which Windows grants on any
    ///   process the same user started, elevated or not — every child a shell
    ///   in this session can start. What refuses it is another user's or a
    ///   protected process, which the shell did not start, or a process that
    ///   already exited, which no close can end. An unreadable SHELL means the
    ///   same of every candidate — none can be told from a recycled-pid
    ///   stranger — so nothing counts.
    /// * The shell a launcher started is not a job, and ITS children are
    ///   ([`ProcessFacts::is_the_shell_launched_by`]): Git Bash's launcher
    ///   holds the real bash as its child for life, and read as a job it made
    ///   every idle Git Bash tab refuse its close.
    /// * With `since_ms` (the instant the shell's own integration said it went
    ///   idle, [`ShellWord::idle_since_ms`]), only a child created at or after
    ///   it counts: a background program a finished command started is not a
    ///   foreground job ([`foreground_busy`] has why).
    ///
    /// A shell with no candidate costs no handle at all — the idle case, and
    /// the common one.
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
                // One level only: the launched shell's children are the jobs,
                // vetted against the launched shell's own birth.
                let inner_floor = floor.max(child_facts.created_ms);
                return self
                    .children_of(child)
                    .any(|job| facts(job).is_some_and(|job| job.created_ms >= inner_floor));
            }
            child_facts.created_ms >= floor
        })
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
/// vetted child walk ([`ChildLinks::has_job`], with no `since`: this is the
/// raw process fact, and the status classifier weighs the shell's own marks
/// above it by itself), and any evidence movement still forces a capture taken
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
    /// this one table.
    snapshot: Option<(Instant, ChildLinks)>,
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
        self.is_job_with(session, master, shell_pid, key, now, capture_child_links)
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
        capture: impl FnOnce() -> ChildLinks,
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

/// The most children [`session_root`] reports: one is enough to say a job is
/// there, and a handful lets a test clean up after itself.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const ROOT_CHILDREN_MAX: usize = 64;

/// What the process table says about a session's ROOT — the process the PTY
/// was spawned with, its session leader (crash journal, ruling 292). The
/// foreground-group test alone misses two cases the quit confirm never had to
/// see: a program that IS the root (`aterm -e vim`, `exec vim`, a `shell=`
/// that is not a shell) holds the foreground as the root's own group, and a
/// background or suspended job (`make &`, a Ctrl-Z'd vim) leaves the
/// foreground at the shell. Both die with the session when aterm does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRoot {
    /// The root's executable name: `p_comm` on macOS, `/proc/<pid>/comm` on
    /// Linux. A login shell's `-zsh` is argv only; this says `zsh`.
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

/// [`SessionRoot`] of `pid`, or `None` when the table cannot answer (gone,
/// a zombie, a platform without the calls). Two cheap kernel reads on macOS
/// (`proc_pidinfo`, `proc_listchildpids`), two small `/proc` reads on Linux.
#[cfg_attr(not(unix), allow(dead_code))] // windows keeps its child walk
pub(crate) fn session_root(pid: i32) -> Option<SessionRoot> {
    if pid <= 0 {
        return None;
    }
    platform_session_root(pid)
}

// libproc's child listing (`<libproc.h>`: `int proc_listchildpids(pid_t
// ppid, void *buffer, int buffersize)`, answering the number of pids written
// or -1), in libSystem. Declared here because `crates/aterm-libc` is
// generated and does not carry it.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_listchildpids(
        ppid: libc::pid_t,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn platform_session_root(pid: i32) -> Option<SessionRoot> {
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
    if u32::try_from(pid).ok()? != info.pbi_pid {
        return None;
    }
    let comm: String = info
        .pbi_comm
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| char::from(c.to_ne_bytes()[0]))
        .collect();
    let mut pids = [0 as libc::pid_t; ROOT_CHILDREN_MAX];
    let bytes = i32::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is `bytes` writable bytes of pid_t slots; libproc writes
    // at most that many and answers how many pids it wrote, or -1.
    let found = unsafe { proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
    let found = usize::try_from(found).ok()?.min(ROOT_CHILDREN_MAX);
    let children = pids[..found].iter().copied().filter(|p| *p > 0).collect();
    Some(SessionRoot { comm, children })
}

#[cfg(target_os = "linux")]
fn platform_session_root(pid: i32) -> Option<SessionRoot> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
    Some(SessionRoot {
        comm: comm.trim_end().to_string(),
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

/// Windows running-job detection: one Toolhelp32 pass over the system process
/// list ([`capture_links`]), and a handle read of each candidate a shell's
/// verdict turns on ([`process_facts`]). Direct kernel32 FFI in the house
/// tiny-FFI style (see `aterm-pty/src/windows/ffi.rs`); SDK names are kept
/// verbatim so the struct layouts can be checked line by line.
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

    /// The console host the OS attaches to a console process — plumbing, never a
    /// user job, so the child walk ignores it (a shell that allocated a classic
    /// console must not read as busy forever).
    const CONHOST: &str = "conhost.exe";

    /// Every live parent -> child link, in one Toolhelp32 pass.
    ///
    /// ONE capture answers every shell. The tab-status observer asks once per
    /// due session per sweep, and one snapshot costs ~2.2 ms in the kernel plus
    /// ~1.6 ms of walk on the machine this was measured on, so a per-session
    /// walk had an idle eight-tab window spending ~100 ms of CPU per second on
    /// eight idle prompts; [`super::JobProbe`] shares one capture across a
    /// sweep and rate-limits it. The close path takes its own capture at the
    /// instant of the gesture ([`super::foreground_is_job`]). A failed snapshot
    /// reads as "no children" — an unknown state must never wedge a close, the
    /// same fail-idle posture as a `tcgetpgrp` error on unix.
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

    /// Which snapshot entries are CANDIDATE children at all, before any handle
    /// is opened: a process that lists ITSELF as its parent is not its own job
    /// (the System Idle Process is pid 0 with parent 0), and the console host
    /// the OS attaches to a console process is plumbing, never a user job (a
    /// shell that allocated a classic console must not read as busy forever).
    pub(super) const fn entry_counts_as_child(
        entry_pid: u32,
        entry_parent: u32,
        is_conhost: bool,
    ) -> bool {
        entry_pid != entry_parent && !is_conhost
    }

    /// Read `pid`'s creation time and image name through a
    /// `PROCESS_QUERY_LIMITED_INFORMATION` handle, or `None` when it cannot be
    /// opened or timed ([`ChildLinks::has_job`] says why that is "not a job").
    /// A name that cannot be read leaves [`ProcessFacts::image`] empty: the
    /// creation time alone still vets the candidate.
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

    /// FILETIME ticks to unix epoch milliseconds — the unit the engine stamps
    /// its command marks in, so a creation time and a 133;D compare directly.
    pub(super) const fn filetime_to_unix_ms(ticks: u64) -> u64 {
        ticks.saturating_sub(FILETIME_UNIX_EPOCH_TICKS) / 10_000
    }

    /// The file name of a full image path (`C:\...\usr\bin\bash.exe` ->
    /// `bash.exe`), ASCII-lowercased: Windows file names compare without case.
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

    /// The close truth table: a busy tab is refused the first time and closes when
    /// confirmed a second time within the window; an idle one closes immediately.
    #[test]
    fn close_decision_warns_once_while_busy_and_otherwise_closes() {
        for (busy, confirmed, expected) in [
            (true, false, CloseDecision::Warn),
            (true, true, CloseDecision::Close),
            (false, false, CloseDecision::Close),
            (false, true, CloseDecision::Close),
        ] {
            assert_eq!(
                close_decision(busy, confirmed),
                expected,
                "busy={busy} confirmed={confirmed}"
            );
        }
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
        // windows routes fg_pgrp <= 0 through the live child-process walk
        // (covered by its own tests below); unix treats it as idle outright.
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

    /// A running block is busy on its own: the `Start-Sleep 60` shape — the
    /// shell executing something with NO child process and NO foreground pgrp
    /// (the windows answer for every ConPTY, and a `tcgetpgrp` error on unix)
    /// — must arm the guard, where `foreground_is_job` alone reads it as idle.
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
        // A stub session (`master`/`pid` = -1) is exactly this shape, so a
        // test harness session flips from idle to busy on the 133;C mark alone.
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
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after 1970")
            .as_millis() as u64
    }

    /// The shell's idle word is earned, not assumed: only a nonce-signed
    /// integration that has reported a WHOLE command (C then D) carries one,
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
        // The next prompt keeps it: the shell is still idle since that D.
        term.process(format!("\x1b]133;A;id={id}\x07PS> \x1b]133;B;id={id}\x07").as_bytes());
        assert_eq!(shell_word(&term).idle_since_ms, Some(since));
    }

    /// Marks nothing authenticates are output a program could print, so they
    /// never silence the child walk — the posture `--no-shell-integration`
    /// leaves (no nonce required) and the one a user's own prompt script
    /// would run under.
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

    /// An integration that marks prompts but never commands — cmd.exe's
    /// `%PROMPT%` tier, pwsh without PSReadLine — says nothing about what runs
    /// behind its prompt, so it never earns the word: `for /L` and `ping -t`
    /// alike run with the prompt's B as the last mark, and a D no C opened is
    /// dropped by the engine.
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

    // ------------------------------------------------------------ job walk --
    //
    // `ChildLinks::has_job` with the handle read injected: every rule is
    // proven on a fixture process table, on every platform.

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

    /// THE MEASURED CASE (2026-09-27): an idle pwsh tab (pid 17636, born
    /// 15:46:57) refused `tab close` three times because the only process
    /// listing 17636 as its parent was an `aterm-gui.exe` born 15:46:18 — 39 s
    /// before the shell, so another process's child whose parent had died and
    /// whose pid Windows had handed on. The creation time says so.
    #[test]
    fn a_child_older_than_its_shell_is_a_recycled_pid_not_a_job() {
        let links = ChildLinks::from_links(vec![(17636, 22432)]);
        let table = [
            (17636, born(SHELL_BORN, "pwsh.exe")),
            (22432, born(SHELL_BORN - 39_000, "aterm-gui.exe")),
        ];
        assert!(!links.has_job(17636, None, facts_of(&table)));
        // Control: the same link from a process born after the shell is one.
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

    /// THE GIT BASH CASE: `Git\bin\bash.exe` (the program the ConPTY starts)
    /// keeps the real `usr\bin\bash.exe` as its child for life, started ~30 ms
    /// after itself. That child is the shell, not a job; ITS children are.
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
        // `ping -t` at that prompt: a child of the REAL bash.
        let links = ChildLinks::from_links(vec![(launcher, bash), (bash, 4040)]);
        table.push((4040, born(SHELL_BORN + 60_000, "ping.exe")));
        assert!(links.has_job(launcher, None, facts_of(&table)));
        // …and a recycled pid under the real bash is vetted against ITS birth.
        table.last_mut().expect("ping").1.created_ms = SHELL_BORN + 10;
        assert!(!links.has_job(launcher, None, facts_of(&table)));
    }

    /// The launcher rule is narrow: a same-image child started LATER than
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
        // The same holds one level down, behind a launcher.
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
        // The next prompt after a completed command is idle too — the guard
        // must not read the LAST command's state once a new prompt is up.
        term.process(b"\x1b]133;A\x07PS> ");
        assert!(!shell_executing(&term));
    }

    /// A running command that CLEARS THE SCROLLBACK stays busy. ED 3 (`CSI 3
    /// J`: `clear`, `cls`, `Clear-Host`) drops the engine's current block —
    /// its rows dangle once scrollback is gone — so a guard reading the block
    /// saw `while ($true) { Clear-Host; …; Start-Sleep 5 }` go idle on its
    /// first iteration, and on windows the child walk never saw it at all.
    /// Only the shell's own 133;D ends the command.
    #[test]
    fn a_command_that_clears_the_scrollback_is_still_executing() {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(b"\x1b]133;A\x07PS> \x1b]133;B\x07Clear-Host; Start-Sleep 60\n\x1b]133;C\x07");
        assert!(shell_executing(&term), "precondition: executing");
        term.process(b"\x1b[H\x1b[2J\x1b[3J");
        assert!(
            term.current_block().is_none(),
            "precondition: ED 3 dropped the block, the reading this guard used to take"
        );
        assert!(
            shell_executing(&term),
            "the command is still running: ED 3 is output, not the end of the command"
        );
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
        // A long-lived, childless child of THIS process (killed below); ping is
        // present on every Windows install.
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
        // 2026-09-27T00:00:00Z: 1_790_467_200_000 ms after the unix epoch.
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
        // Eight tabs, all due in the same sweep, none of them running a job.
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
    /// ceiling — the idle property. At the ceiling exactly one is bought, and
    /// it again serves every session.
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
            ChildLinks::default()
        };
        assert!(!probe.child_verdict(7, key(0), t0, || idle(&mut captures), job_of(1234)));
        assert_eq!(captures, 1);
        // Same evidence, a later sweep inside the ceiling: no capture.
        let t1 = t0 + Duration::from_millis(250);
        assert!(!probe.child_verdict(7, key(0), t1, || idle(&mut captures), job_of(1234)));
        assert_eq!(captures, 1, "a settled session buys nothing");
        // The grid moved: capture again, and see the job this time.
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
        // `is_job_with` reads `foreground_pgrp(master)`; on windows that is
        // always -1, so drive the short-circuit through `child_verdict`'s
        // caller only where it can be observed. The state-free property is
        // what matters here and holds on both arms.
        let busy = probe.is_job_with(1, -1, 0, key(0), now, || {
            captures += 1;
            ChildLinks::default()
        });
        assert!(!busy, "an unknown shell pid is idle, and buys nothing");
        assert_eq!(captures, 0);
        assert_eq!(probe.capture_count(), 0);
    }
}
