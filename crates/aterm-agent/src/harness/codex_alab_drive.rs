// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CODEX-ALAB, the I/O half ([`super::super::codex_alab`] is the pure half): what aterm
//! does for a codex-alab beside its watch-only worker (the native harness contract
//! `~/codex-alab/alab/CONTRACT.md`, §4). A codex-alab supervises itself: it continues,
//! approves, answers and upgrades on its own (§5), so nothing here types into it, reads
//! its screen or decides a move. What is left is aterm's from outside:
//!
//! 1. **The crash rule's relaunch** ([`self_claimed_after_exit`]): a proven self-claimed
//!    codex-alab that crashed comes back on the line aterm's own copy of its session record
//!    validated while it ran ([`super::super::self_claimed`]): the shell back at its marked
//!    prompt leading the foreground group, no live writer lock on the thread's rollout, the
//!    line typed once per witness, the new TUI found leading the tab, and the harness's hand
//!    held to its first idle (its launch probe reads it there). No exit hint is read
//!    off the screen (the copy named the thread) and no continuation is typed after it:
//!    codex-alab's own carry-on continues a thread its crash relaunch resumed (§5).
//! 2. **The relaunch left in flight** ([`carry_in_flight`]): an exit whose line waited, or
//!    a relaunched TUI not yet found, carried on by the next step.
//! 3. **The run, read while it runs** ([`snapshot`]): its home, its thread (the writer lock
//!    it holds, else its launch's `resume <id>`), its block, for the relaunch's snapshot and
//!    a restore layout's leaf.
//! 4. **The prune** ([`prune`]) of the per-tab Codex records (`<state>/upgrade/codex-<tab>.json`)
//!    nothing will visit again, Tier-1 bound to `HarnessUpgradeStateCollection`.
//!
//! [`sweep`] is the sweep's Codex pass: the prune, then each restart left in flight. A record
//! the retired vendor Codex lane left in flight (no validated line, [`St::resume_argv`] empty)
//! is closed, never carried: nothing moves a vendor Codex any more.
//!
//! Never a signal to a Codex process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::codex_alab as ca;
use super::super::relaunch::{ExitCause, NO_SHELL, WAIT_HAND_LIFTS, exit_clock, hand_waits_out};
#[cfg(all(test, unix))]
use super::registry_vouches_for_state;
use super::upgrade::{Agent, Dialect, Phase};
use super::{
    Client, Hand, Kernel, Live, NoPlan, Opts, Plan, PromptLook, RelaunchLineError, Report,
    SAME_PROGRAM, STALE_S, St, TabLife, alive, base_name, connect, cwd_of, exe_of, first_word, ids,
    ledger, load, now_s, open_files, pause, process_in_tab, registry_graph, registry_life, roster,
    said, save, squash, state_dir, table, type_relaunch_line_with, upgrade, wait_until,
};

// ---------------------------------------------------------------- the kernel

/// The process facts the codex-alab path rests on, each read when asked: the
/// Claude lane's [`Kernel`] plus a Codex process's argv, image, open files,
/// working directory and lock holders, the shell's children and terminal, and
/// the host's roster. A trait so a driver test can script a crashed codex-alab
/// and the relaunch that appears on its line.
pub(super) trait AlabKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The executable the kernel mapped for `pid`.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// Every file `pid` holds open through a descriptor, `None` when unreadable.
    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>>;
    /// The conversation writer's kernel advisory lock, independently of other processes'
    /// descriptor tables. Only `Dead` establishes that it is free.
    fn writer_owner(&self, file: &Path) -> aterm_uds::ownerlock::Owner;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live `codex` processes whose parent is `shell`, with their argv.
    fn codex_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// `pid`'s working directory.
    fn cwd(&self, pid: u32) -> Option<String>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own
    /// roster ([`process_in_tab`]): asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
    /// `pid`'s kernel start as the record reader spells it
    /// ([`super::super::restart_stamp::kernel_start`]), `None` where it
    /// cannot be read (off Apple a `ps` that failed: unknown, never an end):
    /// the identity [`Self::first_frame`] reads under.
    fn birth(&self, pid: u32) -> Option<String>;
    /// THE RELAUNCHED CODEX-ALAB UP, as aterm reads it ([`hold_to_first_idle`]):
    /// `tab`'s `status` names `pid` as its proven self-claim
    /// ([`super::super::self_claimed::proven_pid`]), made only after its launch
    /// probe read the tab, and that process, born `birth`, names its TUI's frame
    /// in its own record read live ([`super::super::self_claimed::read_first_frame`]):
    /// at rest (its first idle) or a turn under way. `None` until both stand.
    fn first_frame(
        &self,
        c: &mut Client,
        pid: u32,
        birth: &str,
        tab: &str,
    ) -> Option<super::super::self_claimed::FirstFrame>;
}

impl AlabKernel for Live {
    fn alive(&self, pid: u32) -> bool {
        alive(pid)
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        atpkg::caller_shell::process_args(pid)
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        open_files(pid)
    }

    fn writer_owner(&self, file: &Path) -> aterm_uds::ownerlock::Owner {
        aterm_uds::ownerlock::probe(file)
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn codex_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == "codex")
            .filter_map(|(pid, _, _)| Some((pid, atpkg::caller_shell::process_args(pid)?.argv)))
            .collect()
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        cwd_of(pid)
    }

    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool {
        process_in_tab(c, pid, tab, None, None)
    }

    fn birth(&self, pid: u32) -> Option<String> {
        super::super::restart_stamp::kernel_start(pid)
    }

    fn first_frame(
        &self,
        c: &mut Client,
        pid: u32,
        birth: &str,
        tab: &str,
    ) -> Option<super::super::self_claimed::FirstFrame> {
        use super::super::self_claimed::{proven_pid, read_first_frame};
        c.request_line(&format!("@{tab} status"))
            .is_ok_and(|status| proven_pid(&status) == Some(pid))
            .then(|| read_first_frame(pid, birth))
            .flatten()
    }
}

// ---------------------------------------------------------------- the home and the thread

/// The `$CODEX_HOME` a process started with `env` in the working directory
/// `cwd` uses: `CODEX_HOME`, a relative one joined to `cwd` (Codex
/// canonicalizes the variable against its own working directory as it
/// starts, and the relaunch line carries it verbatim from that same
/// directory), else `$HOME/.codex`, else `<home>/.codex`; canonical when it
/// exists, matching `lsof`'s canonical paths. An empty `CODEX_HOME` is
/// unset. `None`: a relative `CODEX_HOME` whose process's directory could
/// not be read, which names no home aterm can place — never `$HOME/.codex`,
/// which may hold another Codex's threads, locks and daemon.
pub(super) fn home_of(
    env: &atpkg::caller_shell::ProcArgs,
    cwd: Option<&str>,
    fallback: &Path,
) -> Option<PathBuf> {
    let home = match env.env_var("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => PathBuf::from(set),
        Some(relative) => Path::new(cwd.filter(|c| Path::new(c).is_absolute())?).join(relative),
        None => env
            .env_var("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map_or_else(|| fallback.join(".codex"), |h| h.join(".codex")),
    };
    Some(canonical(&home))
}

pub(super) fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The Codex records' per-tab prefix ([`Agent::record_prefix`]), `codex-`:
/// placed at compile time, which fails where Codex keys none.
const PREFIX: &str = match Agent::Codex.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("a Codex record is keyed by its tab"),
};

/// The state key of the Codex record in `tab` (`codex-<tab>`,
/// [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

/// The rollout of `thread` under `home`'s `sessions/<y>/<m>/<d>/`, when one
/// exists; a thread without one has no user message, and `codex resume`
/// refuses it.
pub(super) fn find_rollout(home: &Path, thread: &str) -> Option<PathBuf> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for year in dirs(&home.join("sessions")) {
        for month in dirs(&year) {
            for day in dirs(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for e in entries.flatten() {
                    if super::super::codex_rollout::is_rollout_of(
                        &e.file_name().to_string_lossy(),
                        thread,
                    ) {
                        return Some(e.path());
                    }
                }
            }
        }
    }
    None
}

/// The dialect of the shell `shell`, as its executable names it: the kernel's exec path, else
/// its name in the process table `read_table` gives (read only then). `None` (unread, or a
/// shell no relaunch line is written for: `sh`, tcsh, nushell…) is what a relaunch plan waits on
/// for ever (`wait:shell-dialect`). The run's snapshots read it from the exec path alone,
/// so the explicit restart is carried only where a plan would write its line
/// ([`super::super::relaunch::carries_restart`]).
pub(super) fn shell_dialect(
    shell: u32,
    k: &dyn AlabKernel,
    read_table: impl FnOnce() -> Vec<super::ProcRow>,
) -> Option<Dialect> {
    k.args(shell)
        .map(|a| a.exec_path)
        .or_else(|| {
            read_table()
                .into_iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.to_string())
        })
        .as_deref()
        .map(base_name)
        .as_deref()
        .and_then(Dialect::from_exe_name)
}

// ---------------------------------------------------------------- the crash rule's relaunch

/// THE LINE OF A PROVEN SELF-CLAIMED CODEX-ALAB ([`super::super::self_claimed`]): its own
/// record's resume line as validated against the live process ([`St::resume_argv`]), typed on
/// the program it names and under its captured `CODEX_HOME` ([`St::codex_home_env`]), with the
/// `cd` and the integration heal the shell it finds needs — never a line this lane plans from
/// its own flag table.
pub(super) fn self_claimed_plan(
    opts: &Opts,
    st: &St,
    shell: u32,
    k: &dyn AlabKernel,
) -> Result<Plan, NoPlan> {
    let Some(dialect) = shell_dialect(shell, k, table) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let Some((exe, flags)) = st.resume_argv.split_first() else {
        return Err(NoPlan::Refused {
            why: "no-resume-line".to_string(),
            what: "no-resume-line".to_string(),
            detail: "the session record named no resume line".to_string(),
        });
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    let want = st.cwd.clone();
    let cd = (!want.is_empty() && k.cwd(shell).as_deref() != Some(want.as_str())).then_some(want);
    let home = st.codex_home_env.as_deref().or_else(|| {
        // Older self-claimed records stored only a nonempty home string.
        (!st.resume_home.is_empty()).then_some(st.resume_home.as_str())
    });
    let env: Vec<(&str, &str)> = home
        .into_iter()
        .map(|value| ("CODEX_HOME", value))
        .collect();
    let line = upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &env,
        Path::new(exe),
        flags,
    )
    .map_err(|e| NoPlan::Refused {
        why: format!("line:{e}"),
        what: "line".to_string(),
        detail: e,
    })?;
    Ok(Plan {
        shell,
        dialect,
        line,
    })
}

/// THE CRASH RULE'S RELAUNCH (the self-claim contract, §4; [`super::super::self_claimed`]): a
/// proven self-claimed codex-alab that `snap` names crashed (the caller decided so from aterm's
/// own copy of its record, after `relaunch::on_exit`), and `resume` is the line that copy
/// validated while it ran. Relaunched once per witness — its `(pid, identity)` in aterm's own
/// ledger ([`super::super::self_claimed::Witnesses`]) — through [`relaunch`]: the shell back at
/// its marked prompt leading the foreground group, no live writer lock on the thread's rollout
/// ([`type_relaunch_line_with`]'s `owner_clear`), the line typed, the new TUI found leading the
/// tab, the hand held to its first idle ([`hold_to_first_idle`]), and nothing typed after it. A
/// tab whose agent was its spawn command (no shell) is refused, said once by the caller.
pub(super) fn self_claimed_after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    resume: &super::super::self_claimed::Resume,
    cause: ExitCause,
    k: &dyn AlabKernel,
) -> Report {
    use super::super::relaunch::{CAUSE_ASKED, CAUSE_EXIT, CAUSE_HOST, CAUSE_STALL};
    let tab = snap.tab.clone();
    let session = key(&tab);
    let from = snap.version.clone().unwrap_or_else(|| "-".to_string());
    let mut r = Report {
        pid: snap.pid,
        tab: tab.clone(),
        session: session.clone(),
        from: from.clone(),
        to: "-".to_string(),
        step: String::new(),
    };
    let Some(run) = snap.codex() else {
        return said(r, "refused:not-codex");
    };
    let _held = match super::sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if k.alive(snap.pid) && super::kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        return said(r, "wait:not-exited");
    }
    let witnesses = super::super::self_claimed::Witnesses::under(&opts.state);
    // A relaunch of this very exit already in flight is carried on, never typed twice.
    if let Some(mut st) = load(opts, &session).filter(St::in_flight) {
        if st.pid == snap.pid
            && st.resume_argv == resume.argv
            && (st.resume_start.is_empty() || st.resume_start == snap.start)
        {
            st.resume_start.clone_from(&snap.start);
            return carry_in_flight_with(opts, r, st, &session, k);
        }
        if !opts.dry_run {
            st.close_overtaken("resumed-elsewhere", now_s());
            save(opts, &session, &st);
        }
    }
    if witnesses.handled(snap.pid, &snap.start) {
        return said(r, "ended:handled");
    }
    if snap.shell == 0 || !k.alive(snap.shell) {
        return said(r, "refused:no-shell");
    }
    // The rollout and writer lock belong to the captured canonical home;
    // the raw optional environment is carried separately on the launch line.
    let home = run.home.clone();
    if find_rollout(&home, &resume.session).is_none() {
        return said(r, "refused:no-rollout");
    }
    r.to = format!("{from}({SAME_PROGRAM})");
    if opts.dry_run {
        return said(r, "would-relaunch");
    }
    let Ok(mut c) = connect(opts, &tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&tab) {
        return said(r, "wait:tab-not-live");
    }
    let now = now_s();
    let mut st = St {
        agent: Agent::Codex,
        // codex-alab holds its thread's writer lock itself: the relaunch waits for it to be free.
        thread: resume.session.clone(),
        codex_home: home.to_string_lossy().into_owned(),
        codex_home_env: resume.codex_home.clone(),
        argv: snap.argv.clone(),
        cwd: resume.cwd.clone().unwrap_or_else(|| snap.cwd.clone()),
        twin: resume.argv[0].clone(),
        resume_argv: resume.argv.clone(),
        resume_home: resume.codex_home.clone().unwrap_or_default(),
        resume_start: snap.start.clone(),
        pid: snap.pid,
        shell: snap.shell,
        tab: tab.clone(),
        from: from.clone(),
        to: from,
        source: SAME_PROGRAM.to_string(),
        cause: match cause {
            ExitCause::Stall => CAUSE_STALL,
            ExitCause::Asked => CAUSE_ASKED,
            ExitCause::HostEnded => CAUSE_HOST,
            ExitCause::Exit => CAUSE_EXIT,
        }
        .to_string(),
        salt: now,
        exited_at: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    };
    save(opts, &session, &st);
    super::super::relaunch::consume_stamp(opts, snap, cause);
    ledger(
        opts,
        &said(r.clone(), "exited"),
        "the proven self-claimed codex-alab crashed; relaunched on its own record's resume line",
    );
    let r = relaunch(opts, r, &mut st, &mut c, k);
    save(opts, &session, &st);
    r
}

/// How long a TUI told to go (a typed `/exit`, or a crash still being reaped) may take to be
/// gone before the relaunch waits for a later step (an embedded session shuts its runtime down
/// first).
pub(super) const EXIT_WAIT: Duration = Duration::from_secs(30);

/// How long a relaunched TUI may take to lead its tab's foreground group before the relaunch
/// waits for a later step ([`await_held`]; the Codex lane's `adopt` waits as long).
pub(super) const ADOPT_WAIT: Duration = Duration::from_secs(30);

/// The relaunch of a crashed codex-alab: once the TUI is gone and the shell has its prompt
/// back, type its record's line ([`relaunch_held`]) under the harness's hand on the tab.
fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AlabKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` on the tab ([`Hand`]) through every wait, the typed
/// line, and until the new TUI leads the tab and reaches its first idle ([`await_held`],
/// [`hold_to_first_idle`]). The thread is the record's
/// ([`St::thread`], the copy's validated `sessionId`): nothing is read off the screen, so an
/// exit hint naming another thread is no word here. Its rollout must exist and its writer lock
/// be free (codex-alab holds it itself), checked again right before the line.
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn AlabKernel,
) -> Report {
    let old = st.pid;
    if !wait_until(EXIT_WAIT, || !k.alive(old)) {
        // Never a signal: a later step finds the TUI as it is.
        return said(r, "wait:exiting");
    }
    hand.keep(c);
    // Gone: whatever stops this relaunch from here has no process left to vet it by, and is
    // the tab's record (`upgrade_status::Row::exited_at`).
    if st.exited_at == 0 {
        st.exited_at = now_s();
    }
    let shell = st.shell;
    if !wait_until(Duration::from_secs(15), || k.shell_has_terminal(shell)) {
        return said(r, "wait:shell-prompt");
    }
    hand.keep(c);
    // The shell draws its prompt after it takes the terminal back.
    pause(Duration::from_millis(600));
    let tab = st.tab.clone();
    let home = PathBuf::from(&st.codex_home);
    if find_rollout(&home, &st.thread).is_none() {
        return fail(
            opts,
            r,
            st,
            "no-rollout",
            "the conversation's rollout is gone",
        );
    }
    // Codex's WriterLockCoordinator holds File::try_lock for the writer's lifetime.
    // Probe that same lock rather than inspecting every same-uid process: an unrelated
    // protected descriptor can make the census unknown while this lock is readable/free.
    // A held lock or an unreadable/missing/nonregular file still grants no admission.
    let lock = home
        .join("thread-writer-locks")
        .join(format!("{}.lock", st.thread));
    let lock_free =
        |k: &dyn AlabKernel| -> bool { k.writer_owner(&lock) == aterm_uds::ownerlock::Owner::Dead };
    if !lock_free(k) {
        return said(r, "wait:thread-locked");
    }
    let (line, dialect) = match self_claimed_plan(opts, st, shell, k) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    // Recorded before the line is typed: a step that dies after typing it leaves the next one
    // the line to recognise its relaunch by.
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    match type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        // When its end was asked — known only for a restart that asked it
        // (`relaunch::end_asked_at`): a person's keys before it went to Codex;
        // and whether a hand held the line (`relaunch::hand_held_since`).
        PromptLook::of(st),
        || lock_free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    ) {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            // Kept on the record: the stale bound that ends the relaunch names what held its
            // line ([`expired`]).
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Hand) => {
            // A hand the line waits on: its clock stops while it stands, as
            // Claude Code's does (`relaunch::exit_clock`).
            let now = now_s();
            st.saw_hand(now);
            st.note_wait("held", now);
            return said(r, "wait:held");
        }
        Err(RelaunchLineError::Turn(e)) => {
            st.fail("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", first_word(&e)));
            ledger(opts, &r, &line);
            return r;
        }
    }
    st.relaunched(now_s());
    // The line landed, whether this is the exit's first attempt or a deferred carry. Record
    // before waiting for the new process to appear.
    if !st.resume_start.is_empty()
        && let Err(e) = super::super::self_claimed::Witnesses::under(&opts.state)
            .record(st.pid, &st.resume_start)
    {
        ledger(opts, &said(r.clone(), "witness-unrecorded"), &e.to_string());
    }
    ledger(
        opts,
        &said(r.clone(), "relaunched"),
        &format!("{} (resume)", st.line),
    );
    save(opts, &key(&tab), st);
    await_held(opts, r, st, c, hand, k)
}

/// After the relaunch line: find the new TUI ([`relaunched_by_us`]) under the harness's hand.
/// A later step finding the relaunch in flight comes here too.
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AlabKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, k);
    hand.give_back(c);
    r
}

/// [`await_new`] with the harness's `hand` on the tab, kept through the wait; its caller gives
/// it back. Done once the new TUI leads the tab by the host's own roster, the hand held on to
/// its first idle ([`hold_to_first_idle`]): nothing is typed into it, no box is answered and no
/// composer waited on, whoever asked (a window's worker under [`Opts::hand_back`] included,
/// which adopts nothing). codex-alab's own carry-on continues the thread its crash relaunch
/// resumed (the contract §5), once its launch probe saw this hand.
fn await_held(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn AlabKernel,
) -> Report {
    let mut new = None;
    wait_until(Duration::from_secs(90), || {
        hand.keep(c);
        new = k
            .codex_children(st.shell)
            .into_iter()
            .find(|(pid, argv)| relaunched_by_us(*pid, argv, st, k))
            .map(|(pid, _)| pid);
        new.is_some()
    });
    let Some(new) = new else {
        if let Phase::Relaunched { at_s } = st.phase
            && now_s().saturating_sub(at_s) > STALE_S
        {
            return fail(
                opts,
                r,
                st,
                "no-resume",
                "the relaunched Codex never came up in the tab",
            );
        }
        return said(r, "wait:resume");
    };
    found(st, new, k);
    r.pid = new;
    let tab = st.tab.clone();
    if !wait_until(ADOPT_WAIT, || {
        hand.keep(c);
        k.in_tab(c, new, &tab)
    }) {
        return said(r, "wait:resume");
    }
    let held = hold_to_first_idle(opts, c, hand, k, new, &tab);
    st.phase = Phase::Done;
    st.resumed_pid = new;
    st.resumed_on.clone_from(&st.to);
    st.done_at = now_s();
    st.outcome = format!(
        "codex on {} · the same conversation resumed; codex-alab carries it on itself",
        st.to
    );
    let r = said(r, "done:resumed");
    ledger(opts, &r, &format!("{}; {held}", st.outcome));
    r
}

/// How [`hold_to_first_idle`] ended, as the ledger's `done:resumed` row says it.
const HELD_TO_FIRST_IDLE: &str = "the hand held to its first idle";
/// The hold ended at a turn already under way
/// ([`super::super::self_claimed::FirstFrame::Turn`]).
const HELD_TO_A_TURN: &str = "the hand held until it was up, a turn already under way";
/// The relaunched process ended, or its pid is another process's now.
const HELD_TO_ITS_END: &str = "the hand given back: it ended before its first idle";
/// The hold ran to [`super::super::Timings::alab_first_idle`].
const HELD_TO_THE_BOUND: &str = "the hand given back at its bound, before its first idle";
/// Another driver's lease stands on the tab now.
const HELD_TO_A_LAPSE: &str = "the hand lapsed to another driver before its first idle";

/// THE HAND IS HELD TO THE RELAUNCHED PROCESS'S FIRST IDLE (the contract §5,
/// carry-on): aterm's relaunch lease (`hand=lease:aterm-harness@<pid>`),
/// taken before the line is typed, stands until the new codex-alab is up as
/// aterm reads it ([`AlabKernel::first_frame`]: its proven self-claim, which
/// codex-alab makes only after its launch probe read the tab, and its own
/// record naming its TUI's frame — at rest, its first idle, or a turn already
/// under way, whose own acts wait for this lease), the process gone, or
/// [`super::super::Timings::alab_first_idle`] (the launch probe's window),
/// whichever comes first; its caller then gives it back. codex-alab's launch
/// probe (its client's FIRST `@self status`, read as the process starts) must
/// see that lease to carry the resumed thread on: given back as soon as the
/// new TUI led the tab (the carve, 3999d9adc6, in 0.106.0), the probe read
/// `hand=-` 5 ms later and the resumed conversation waited at its prompt.
/// Every relaunch of a codex-alab comes here (the crash rule's, a stall or
/// memory remedy's, an asked restart's, a restore's, and each carried in
/// flight: [`await_held`]); a later step that finds one already up ends at
/// its first look. Nothing is typed meanwhile and no screen is read. A kernel
/// start that cannot be read is unknown, never the process's end (off Apple
/// it is a `ps`, which can fail once): only a process gone or another start
/// of its pid ends the hold early. With no hand held (another driver's, or a
/// server without hard leases) there is nothing to hold. Says how it ended,
/// for the ledger.
fn hold_to_first_idle(
    opts: &Opts,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn AlabKernel,
    new: u32,
    tab: &str,
) -> &'static str {
    use super::super::self_claimed::FirstFrame;
    if !hand.held {
        return "no hand held";
    }
    let mut birth: Option<String> = None;
    let mut ended = HELD_TO_THE_BOUND;
    wait_until(opts.timings.alab_first_idle, || {
        hand.keep(c);
        if !hand.held {
            ended = HELD_TO_A_LAPSE;
            return true;
        }
        if !k.alive(new) {
            ended = HELD_TO_ITS_END;
            return true;
        }
        match (k.birth(new), &birth) {
            (Some(now), Some(first)) if now != *first => {
                ended = HELD_TO_ITS_END;
                return true;
            }
            (Some(now), None) => birth = Some(now),
            _ => {}
        }
        let Some(born) = birth.as_deref() else {
            return false;
        };
        match k.first_frame(c, new, born, tab) {
            Some(FirstFrame::Idle) => ended = HELD_TO_FIRST_IDLE,
            Some(FirstFrame::Turn) => ended = HELD_TO_A_TURN,
            None => return false,
        }
        true
    });
    ended
}

/// Record a Codex restart that stops here, with one ledger line.
pub(super) fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let r = said(r, format!("failed:{why}"));
    ledger(opts, &r, detail);
    r
}

/// Whether `pid` (argv `argv`) is the relaunch this upgrade typed: a child of
/// the line's shell, not the exited TUI, whose argv resumes the recorded thread
/// (or nothing, for a plain relaunch).
pub(super) fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn AlabKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && ca::is_tui_argv(argv)
        && if st.thread.is_empty() {
            !argv.iter().any(|a| a == "resume")
        } else {
            argv.iter().any(|a| a == "resume") && argv.last() == Some(&st.thread)
        }
}

/// The relaunched TUI `new` found ([`relaunched_by_us`]), by the wait after the relaunch line or
/// by a visit (here, [`await_held`]; in the Codex lane, its `adopt` or `carry_on`): on the
/// record ([`St::relaunched_pid`])
/// whatever becomes of it, so once it is gone a Codex leaving the tab is that TUI, its exit its
/// own (`relaunch::restarted`).
///
/// A relaunch that kept the TUI on its own program ([`SAME_PROGRAM`]: a crash's, while the
/// owner's hold or the switch kept it off the managed build) hands its record to `new` as well:
/// pid and kernel start, by which a later look keeps the record its own.
/// The record carries what the relaunch kept of the upgrade's ([`St::kept_by_restart`]: the
/// owner's hold, how long the TUI has been behind); left naming the crashed TUI, the first
/// visit would mint a round with no word and its move clock at zero. A relaunch onto the
/// managed build leaves the record as it was.
///
/// The relaunched TUI's kernel birth is read once, as it is first found
/// ([`St::relaunched_start`]); a different later TUI has no witnessed
/// connection to the typed line, and a re-read of the same pid never
/// refreshes it into a reused pid's birth.
pub(super) fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
    if st.source == SAME_PROGRAM {
        st.notice_pid = new;
        st.notice_start.clone_from(&st.relaunched_start);
    }
}

/// A codex-alab relaunch in flight (its line waited, or the relaunched TUI was not yet found)
/// carried on from where it stopped: the relaunch primitive's continuation step for a Codex
/// record ([`super::super::relaunch::resume`], by `upgrade_drive::codex_carry_in_flight`); one
/// with no validated line is closed ([`carry_or_close`]).
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_or_close(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
pub(super) fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn AlabKernel,
) -> Report {
    let r = Report {
        pid: st.pid,
        tab: st.tab.clone(),
        session: session.to_string(),
        from: st.from.clone(),
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    if opts.dry_run {
        return said(r, format!("would-resume:{}", st.phase.word()));
    }
    // A hand the line waits on is waited out before the clock is read
    // (`relaunch::hand_waits_out`): uncounted, its clock stopped.
    if hand_waits_out(opts, &mut st, session, |pid| k.alive(pid)) {
        return said(r, WAIT_HAND_LIFTS);
    }
    if let Some((why, detail)) = expired(&st, now_s(), k) {
        let r = fail(opts, r, &mut st, why, &detail);
        save(opts, session, &st);
        return r;
    }
    let Ok(mut c) = connect(opts, &st.tab) else {
        return said(r, "wait:no-socket");
    };
    if !roster(&mut c).contains(&st.tab) {
        return said(r, "wait:tab-not-live");
    }
    let r = match st.phase {
        Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, k),
        _ => await_new(opts, r, &mut st, &mut c, k),
    };
    save(opts, session, &st);
    r
}

/// The sweep's Codex pass ([`super::sweep`]'s lanes): the records of closed tabs pruned
/// ([`prune`], under the sweep lock, as the Claude lane's), then every Codex restart left in
/// flight in a live tab within the pass's scope ([`in_flight_records`]). Nothing at a background
/// break, where no restart is carried on.
pub(super) fn sweep(opts: &Opts, live_tabs: Option<&[super::LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[super::LiveTab]>::is_empty) {
        return Vec::new();
    }
    let known_live: Vec<String> = live_tabs
        .into_iter()
        .flatten()
        .map(|t| t.sid.clone())
        .collect();
    let graph = registry_graph();
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
    prune_retired(opts, now_s());
    if opts.background {
        return Vec::new();
    }
    in_flight_records(opts, live_tabs, &Live)
}

/// What the retired vendor Codex lane left on disk, swept once a week old (unwritten for
/// [`super::STATE_KEEP_S`]), as the records are: each daemon's pin stamp
/// (`<state>/upgrade/codex-daemons/<label>-pin.json`, the directory too once empty) and each
/// tab's goal-pause record beside its loop's ledger (`<aterm state>/drive/<tab>.goal-hold.json`).
/// Nothing writes either any more. Nothing under a `CODEX_HOME` is touched, and nothing in a
/// dry run or a background break. Called under the sweep lock.
pub(super) fn prune_retired(opts: &Opts, now: u64) {
    if opts.dry_run || opts.background {
        return;
    }
    let sweep = |dir: &Path, stale: &dyn Fn(&str) -> bool| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if is_file && name.to_str().is_some_and(stale) && super::sat_a_week(&entry, now) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    };
    let pins = state_dir(opts).join("codex-daemons");
    sweep(&pins, &|name| name.ends_with("-pin.json"));
    // Gone only once empty: a directory with anything else in it is not ours to judge.
    let _ = std::fs::remove_dir(&pins);
    if let Some(root) = opts.aterm_state.as_deref() {
        let drive = crate::supervise::approvals::ledger_under(root, Some("self"));
        if let Some(drive) = drive.parent() {
            sweep(drive, &|name| name.ends_with(".goal-hold.json"));
        }
    }
}

/// Every Codex restart left in flight ([`St::in_flight`]) whose tab is live (or not known gone,
/// for a hand-run pass) and within `opts.only_sid`: a codex-alab's crash relaunch carried on by
/// its own path ([`carry_in_flight_with`]); a record with no validated line, which only the
/// retired vendor Codex lane wrote, closed as `failed:codex-lane-retired` (a dry run says
/// `would-fail:`) and never typed into.
pub(super) fn in_flight_records(
    opts: &Opts,
    live_tabs: Option<&[super::LiveTab]>,
    k: &dyn AlabKernel,
) -> Vec<Report> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(st) = load(opts, &session)
            .filter(|st| st.agent == Agent::Codex)
            .filter(St::in_flight)
        else {
            continue;
        };
        if live_tabs.is_some_and(|tabs| !tabs.iter().any(|t| t.sid == st.tab))
            || opts.only_sid.as_ref().is_some_and(|s| *s != st.tab)
        {
            continue;
        }
        let r = Report {
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            step: String::new(),
        };
        out.push(carry_or_close(opts, r, st, &session, k));
    }
    out
}

/// One Codex record in flight: carried on when it holds a codex-alab's validated line, else
/// closed as the retired vendor lane's ([`in_flight_records`]).
pub(super) fn carry_or_close(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn AlabKernel,
) -> Report {
    if !st.resume_argv.is_empty() {
        return carry_in_flight_with(opts, r, st, session, k);
    }
    if opts.dry_run {
        return said(r, format!("would-fail:{LANE_RETIRED}"));
    }
    let r = fail(
        opts,
        r,
        &mut st,
        LANE_RETIRED,
        "the vendor Codex lane that began this restart is retired: nothing carries it on",
    );
    save(opts, session, &st);
    r
}

/// The stop of a Codex restart the retired vendor lane left in flight ([`carry_or_close`]).
pub(super) const LANE_RETIRED: &str = "codex-lane-retired";

/// [`super::expired`] for a Codex restart: the exit seen over [`STALE_S`] ago — or the last hand
/// that held its line, whichever is later (`relaunch::exit_clock`) — and not relaunched, its
/// shell gone, or relaunched and never come up.
/// Its `stale-exit` line is Claude Code's, wait words and all
/// (`relaunch::stale_exit_line`).
pub(super) fn expired(st: &St, now: u64, k: &dyn AlabKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, 0, st.hand_seen_s)) > STALE_S =>
        {
            Some((
                "stale-exit",
                super::super::relaunch::stale_exit_line(st, "Codex", now),
            ))
        }
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran Codex is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched Codex never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------- the run, read while it runs

/// A codex-alab's run, read while it runs ([`super::super::relaunch::snapshot`] vetted job, tab
/// and terminal): its build and version, `$CODEX_HOME`, the thread (its writer lock's, else its
/// launch's `resume <id>`, [`ca::resumed_thread`]) and its directory. The vendor daemon is never
/// asked which thread it
/// began: codex-alab's crash relaunch comes from its own record's line
/// ([`super::super::relaunch::CodexRun::self_claimed`]), never from this read.
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn AlabKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    if !ca::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    let read_cwd = k.cwd(pid);
    let home = home_of(args, read_cwd.as_deref(), &opts.home).ok_or("home-unreadable")?;
    let files = k.open_files(pid).ok_or("files-unreadable")?;
    let locks = home.join("thread-writer-locks");
    let held: Vec<String> = files
        .iter()
        .filter(|f| f.parent() == Some(locks.as_path()))
        .filter_map(|f| ca::lock_thread(&f.file_name()?.to_string_lossy()).map(str::to_owned))
        .collect();
    let thread = match held.as_slice() {
        [thread] => Some(thread.clone()),
        [] => ca::resumed_thread(&args.argv),
        _ => return Err("threads-ambiguous"),
    };
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version: ca::package_version(&exe).map(|v| v.to_string()),
        program: exe,
        argv: args.argv.clone(),
        session: thread,
        cwd: read_cwd.unwrap_or_default(),
        run: super::super::relaunch::AgentRun::Codex(super::super::relaunch::CodexRun {
            home,
            home_env: args.env_var(ca::HOME_ENV).map(str::to_owned),
            self_claimed: None,
        }),
        // From the kernel's exec path alone ([`shell_dialect`]): a shell no line is written for
        // waits for ever at exit, so the explicit restart is not carried there
        // ([`super::super::relaunch::carries_restart`]). None for a tab's own codex-alab
        // ([`NO_SHELL`]): no shell holds it, and nothing relaunches it in its tab.
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, k, Vec::new))
            .flatten(),
        // Claude Code's variables: a Codex has no such login or home, and no usage tap.
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    })
}

// ---------------------------------------------------------------- the prune

/// Remove Codex records nothing will visit again: a visit forgets a record only when a current
/// Codex leads its tab, so closed tabs' records would stay forever. A record goes once its tab
/// is proven [`TabLife::Gone`] (`life`), it is unwritten for [`super::STATE_KEEP_S`] and its
/// upgrade is [`super::settled`]; never one in flight or not matching its name, and nothing in
/// a dry run or a background break. Called under the sweep lock.
///
/// Tier-1 bound to `HarnessUpgradeStateCollection`
/// (`aterm_models_harness::harness_upgrade_state_collection_model`) in this module's tests.
pub(super) fn prune(opts: &Opts, now: u64, life: impl Fn(&str) -> TabLife) {
    if opts.dry_run || opts.background {
        return;
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(tab) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|stem| Agent::Codex.record_tab(stem))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Codex && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(all(test, unix))]
#[path = "codex_alab_drive_tests.rs"]
mod tests;
