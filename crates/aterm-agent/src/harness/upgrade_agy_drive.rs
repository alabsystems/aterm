// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LANE, ITS I/O HALF ([`super::super::upgrade_agy`] is the pure
//! half): an agy that crashed out of its tab is relaunched there on its own
//! conversation, and one aterm's own end took is brought back in its reopened
//! tab. And its LIVE UPGRADE'S QUIET FLOW (`upgrade_agy_live_drive.rs`,
//! [`live`]; [`super::super::upgrade_agy_live`] is its pure half): a
//! supervised agy behind the build atpkg landed is ended at a genuinely idle
//! point and relaunched through the managed twin on the same conversation by
//! the relaunch below, its exit claimed as the upgrade's own, nothing typed
//! after. Only the window's host's step visits it; a hand-run sweep moves no
//! agy, and nothing is ever typed into an agy that runs but a crash
//! relaunch's continuation.
//!
//! 1. **The snapshot** ([`snapshot`]): the tab's foreground agy TUI (no
//!    one-shot, no subcommand), its config directory (its `HOME`'s
//!    `.gemini/antigravity-cli`, moved by its hidden `--gemini_dir` and
//!    `--app_data_dir`), the conversation it holds — the one presence lock
//!    `<config>/presence/<uuid>.lock` its process holds open (`lsof`,
//!    [`witness`]) — the crash file it holds open, and the shell-integration
//!    block it runs in. The lock names no pid, so between idle points nothing
//!    follows it: the loop's idle points read the snapshot again (as a
//!    Codex's was), and `/clear` or an in-TUI `/resume` is caught there.
//! 2. **The exit** ([`look_at_exit`], [`after_exit`]): the shell's exit
//!    status of the block it ran in, AND whether its dead pid's crash file is
//!    left, read as the exit is seen (the next agy start deletes it), by
//!    agy's own table (`upgrade_agy::exit_kind`: `0` clean, `146` stopped, any
//!    other a crash only with the crash file left, else `2` a refused start
//!    and the rest unwitnessed); with no word, nothing is relaunched.
//! 3. **The relaunch** ([`relaunch`]): once no live process holds the
//!    conversation's presence lock, no live agy names it in its argv, AND no
//!    live agy holds its database with no presence lock (a second agy opened
//!    on a held conversation — `--conversation`, `-c`, an in-TUI `/resume` —
//!    holds no lock, but holds that database: measured),
//!    `cd <the launch's directory>` and the managed twin `--conversation=<uuid>`
//!    after the launch's carried flags, behind the `HOME` the process ran
//!    under, typed at the tab's shell by the relaunch primitive's own typing
//!    (`type_relaunch_line_with`). The new process is found as the shell's
//!    child resuming that conversation ([`relaunched_by_us`]); it must take
//!    the conversation's lock (at once with `--conversation`, measured) before
//!    it is handed to its loop ([`adopt`]): one that never does started a
//!    fresh conversation (another `HOME`, a missing hidden flag: agy says
//!    `conversation not found`), and the person is told, nothing typed
//!    (`refused:started-fresh`). A line whose own command ends with no agy
//!    found leading the tab is decided by its block's word at once
//!    ([`line_word`], [`line_ended`]), and so is every later attempt at the
//!    same exit ([`after_exit_onto`]): a relaunch agy refused (`2`), or a line
//!    the shell could not run, is never typed again; only that line's agy
//!    crashing (its crash file left) types it again.
//! 4. **The continuation** ([`carry_on`]): typed at the loop's first idle
//!    point, never as `-i` on the line — `-i` sends its text at once, before
//!    anything can tell whether the conversation came back, so a fresh start
//!    would get it; once the new process holds the conversation's lock and
//!    its composer reads empty and idle, the relaunch's continuation is typed
//!    and submitted by a guarded `turn`.
//!
//! Records are filed per tab (`<state>/upgrade/agy-<tab>.json`,
//! [`upgrade::Agent::record_key`]), pruned as codex-alab's path prunes its own
//! ([`prune`]), and their holders read for the owner's view ([`holders`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::relaunch::{
    ExitCause, NO_SHELL, NO_SHELL_DETAIL, WAIT_HAND_LIFTS, exit_clock, first_idle, hand_waits_out,
};
use super::super::upgrade_agy::{self as ag, ExitKind};
use super::upgrade::{Agent, Dialect, Phase};
use super::{
    Client, Hand, Kernel, Live, LiveTab, NoPlan, Opts, Plan, PromptLook, RelaunchLineError, Report,
    STALE_S, St, TURN_WAIT, TabLife, Typed, alive, connect, cwd_of, descendants_in, exe_of,
    exit_status, held, ids, is_generation, ledger, line_status, load, lsof_holders,
    lsof_open_files, managed_agent_twin, now_s, registry_graph, registry_life, roster,
    running_block, said, save, screen, shell_dialect_of, squash, started_ms, state_dir,
    store_version, table, turn_takes_gen, turn_verdict, twin_version, type_relaunch_line_with,
    under_hand, upgrade, wait_until,
};

// ---------------------------------------------------------------- the kernel

/// The process facts the agy lane rests on, each read when asked: the Claude
/// lane's [`Kernel`] plus agy's needs. A trait so a driver test can script an
/// agy that exits and a relaunch that appears on its line.
pub(super) trait AgyKernel: Kernel {
    /// Whether `pid` lives.
    fn alive(&self, pid: u32) -> bool;
    /// `pid`'s argv and initial environment (`KERN_PROCARGS2`).
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs>;
    /// The executable the kernel mapped for `pid`.
    fn exe(&self, pid: u32) -> Option<PathBuf>;
    /// Every file `pid` holds open (`lsof`), `None` when unreadable.
    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>>;
    /// Every process holding `file` open, `None` when unreadable.
    fn holders_of(&self, file: &Path) -> Option<Vec<u32>>;
    /// The live agy processes whose argv names `conversation`
    /// ([`ag::resumed_conversation`], any spelling), `None` when one's argv
    /// cannot be read: a second agy opened on a held conversation holds no
    /// lock, even once the holder is gone (measured).
    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>>;
    /// THE LIVE AGY PROCESSES THAT HAVE `conversation` OPEN WITHOUT ITS LOCK:
    /// each holds its database under `config` ([`ag::conversation_db`]) open
    /// and no presence lock there ([`ag::holds_no_lock`]) — one opened on it
    /// while another held it, by `--conversation`, by `-c`/`--continue`
    /// (whose argv names no id) or by an in-TUI `/resume`, which stays so once
    /// the holder is gone (measured on 1.4.2). `None` when the database's
    /// holders, or a live holder's open files, cannot be read.
    fn lockless_openers(&self, config: &Path, conversation: &str) -> Option<Vec<u32>>;
    /// Whether `shell`'s group holds its terminal again (`pgid == tpgid`).
    fn shell_has_terminal(&self, shell: u32) -> bool;
    /// The live `agy` processes whose parent is `shell`, with their argv.
    fn agy_children(&self, shell: u32) -> Vec<(u32, Vec<String>)>;
    /// `pid`'s working directory.
    fn cwd(&self, pid: u32) -> Option<String>;
    /// Whether `pid` is still the foreground group of `tab` by the host's own
    /// roster: asked right before every typed act.
    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool;
    /// Every descendant of `pid` (`(pid, basename)`), `None` where the
    /// process table cannot be read: the live upgrade's process rule
    /// (G-A2(d), an idle agy has no child at all). The default reads the live
    /// table, as [`Live`] does.
    fn descendants(&self, pid: u32) -> Option<Vec<(u32, String)>> {
        descendants_in(&table(), pid)
    }
    /// When `pid` started, unix milliseconds (the kernel's): where its
    /// conversation's transcript begins to be this process's. The default
    /// reads the kernel, as [`Live`] does.
    fn started_ms(&self, pid: u32) -> Option<u64> {
        started_ms(pid)
    }
    /// The answer of the read-only query of `conversation`'s summary rows in
    /// the database under `config` ([`live::read_summaries`]): G-A1 and
    /// G-A2(c). The default reads the live database, as [`Live`] does.
    fn summaries(&self, config: &Path, conversation: &str) -> Result<String, ()> {
        live::read_summaries(config, conversation)
    }
}

impl AgyKernel for Live {
    fn alive(&self, pid: u32) -> bool {
        alive(pid)
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        atpkg::caller_shell::process_args(pid)
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }

    // The lanes' one read of these kernel facts (`lsof`).
    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        lsof_open_files(pid)
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        lsof_holders(file)
    }

    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        for (pid, _, _) in table()
            .into_iter()
            .filter(|(_, _, name)| name == ag::PROGRAM)
        {
            match atpkg::caller_shell::process_args(pid) {
                Some(args) => {
                    if ag::resumed_conversation(&args.argv).as_deref() == Some(conversation) {
                        out.push(pid);
                    }
                }
                // Gone between the table and its read: none of it holds the
                // conversation. Alive and unreadable: nobody can say.
                None if alive(pid) => return None,
                None => {}
            }
        }
        Some(out)
    }

    fn lockless_openers(&self, config: &Path, conversation: &str) -> Option<Vec<u32>> {
        let agys: BTreeSet<u32> = table()
            .into_iter()
            .filter(|(_, _, name)| name == ag::PROGRAM)
            .map(|(pid, _, _)| pid)
            .collect();
        let mut out = Vec::new();
        for pid in lsof_holders(&ag::conversation_db(config, conversation))?
            .into_iter()
            .filter(|pid| agys.contains(pid))
        {
            match lsof_open_files(pid) {
                Some(files) => {
                    if ag::holds_no_lock(config, &files) {
                        out.push(pid);
                    }
                }
                // Gone between the two reads: it holds nothing now. Alive and
                // unreadable: nobody can say.
                None if alive(pid) => return None,
                None => {}
            }
        }
        Some(out)
    }

    fn shell_has_terminal(&self, shell: u32) -> bool {
        ids(shell).is_some_and(|(_, pgid, tpgid)| pgid == tpgid)
    }

    fn agy_children(&self, shell: u32) -> Vec<(u32, Vec<String>)> {
        table()
            .into_iter()
            .filter(|(_, ppid, name)| *ppid == shell && name == ag::PROGRAM)
            .filter_map(|(pid, _, _)| Some((pid, atpkg::caller_shell::process_args(pid)?.argv)))
            .collect()
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        cwd_of(pid)
    }

    fn in_tab(&self, c: &mut Client, pid: u32, tab: &str) -> bool {
        super::process_in_tab(c, pid, tab, None, None)
    }
}

// ---------------------------------------------------------------- the records

/// The agy lane's per-tab record prefix ([`Agent::record_prefix`]), `agy-`:
/// placed at compile time, which fails where agy keys none.
const PREFIX: &str = match Agent::Agy.record_prefix() {
    Some(prefix) => prefix,
    None => panic!("the agy lane keys its records by tab"),
};

/// The state key of the agy relaunch in `tab` (`agy-<tab>`,
/// [`Agent::record_key`]).
pub(super) fn key(tab: &str) -> String {
    format!("{PREFIX}{tab}")
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

// ---------------------------------------------------------------- the twin

/// THE MANAGED AGY A RELAUNCH TYPES: atpkg's twin (`<prefix>/agents/agy`,
/// which exports `AGY_CLI_DISABLE_AUTO_UPDATE='true'` and then `exec`s an
/// `agy` build), never the store build the crashed process ran (critic X3:
/// typed, it pins that build and skips the twin's export, so agy's own
/// updater would run). A process run from no store (another install) is
/// relaunched on its own program; one run from a store build with no twin to
/// type is relaunched by nothing (`refused:no-managed-agy`).
fn relaunch_program(program: &Path, twin: Option<PathBuf>) -> Result<PathBuf, &'static str> {
    if !ag::store_build(program) {
        return Ok(program.to_path_buf());
    }
    twin.ok_or("no-managed-agy")
}

/// WHETHER AN EXIT OF `snap`'S AGY NOW WOULD BE RELAUNCHED ON ITS
/// CONVERSATION onto the twin `twin` ([`managed_agent_twin`]), as far as a
/// live read tells: the explicit restart's gate
/// ([`super::super::relaunch::carries_restart`]), ONE PREDICATE WITH THE
/// RELAUNCH — every refusal [`after_exit_onto`] and [`plan`] make from what
/// the snapshot holds: a witnessed conversation (a UUIDv4), a shell a line is
/// written for, a launch whose flags its line carries ([`ag::rewrite_argv`]),
/// a program the line can type ([`relaunch_program`]: a store build only
/// through a managed twin, else `refused:no-managed-agy`), a line that
/// holds the `HOME` it carries ([`ag::line_env`],
/// [`upgrade::relaunch_line_env`]) within the tty's bound, behind the heal
/// [`plan`] prefixes for a user whose home is `home`. Asked without the
/// program, the act `SIGTERM`ed a store build with no twin, which its
/// relaunch then refused (the review of the agy lane, 2026-10-17). What only
/// the exit can tell — the shell alive, its prompt back, the conversation
/// free — the relaunch waits on.
///
/// THE `cd` BY DIALECT: [`plan`] writes one only where the shell's directory
/// differs from the launch's, which no snapshot tells, so the line asked is
/// the one the relaunch can meet at worst. At zsh and bash that is the line
/// WITH the `cd` (a `cd` only adds length and a directory that must hold no
/// control character): asked without it, a launch directory long enough
/// carried the act onto an agy whose relaunch from another directory then
/// refused the line for its length, leaving it at its shell (the review of
/// 40971b5782). A launch directory no `cd` can name, or one too long for it,
/// is so refused the act even where the shell stands in it and the relaunch
/// would type no `cd`: the safe side of what no snapshot tells. At fish it is
/// the line WITHOUT one: fish takes no `cd` on the line, whatever the
/// directory ([`upgrade::relaunch_line_env`]), so its relaunch resumes an
/// agy only from the launch's own directory, typing none — asked with one,
/// every agy at a fish shell was refused the act (the re-review of the agy
/// lane, 2026-10-17). A fish shell moved to another directory is the
/// predicate's one boundary: its relaunch refuses the `cd` it then needs.
pub(super) fn carries_restart_onto(
    snap: &super::super::relaunch::Snapshot,
    home: &Path,
    twin: Option<PathBuf>,
) -> bool {
    let Some(run) = snap.agy() else {
        return false;
    };
    let (Some(dialect), Some(conversation)) = (
        snap.dialect,
        snap.session
            .as_deref()
            .filter(|c| ag::is_conversation_uuid(c)),
    ) else {
        return false;
    };
    let (Ok(flags), Ok(program)) = (
        ag::rewrite_argv(&snap.argv, Some(conversation)),
        relaunch_program(&snap.program, twin),
    ) else {
        return false;
    };
    let heal = atpkg::hooks::hook_file(home, dialect.hook_ext()).map(|(p, _)| p);
    let cd = match dialect {
        Dialect::Zsh | Dialect::Bash => (!snap.cwd.is_empty()).then_some(snap.cwd.as_str()),
        Dialect::Fish => None,
    };
    upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd,
        &ag::line_env(&run.env),
        &program,
        &flags,
    )
    .is_ok()
}

// ---------------------------------------------------------------- the witness

/// What the files a live agy holds open say ([`witness`]): the conversation
/// whose presence lock it holds, with that lock, and the crash file it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Held {
    conversation: Option<(String, PathBuf)>,
    crash: Option<PathBuf>,
}

/// THE CONVERSATION `pid` HOLDS, by the kernel and by agy's own lock: among
/// the files `pid` holds open (`files`, `lsof`'s canonical paths), the
/// presence locks under `config` ([`ag::lock_conversation`]); exactly one is
/// its conversation, with that lock's path (`/clear` and `/resume` release
/// the old one at once, measured). None, or two, prove none. And the crash
/// file of `pid` it holds open ([`ag::crash_file`]), where there is exactly
/// one.
fn witness(config: &Path, files: &[PathBuf], pid: u32) -> Held {
    fn one<T: Clone>(found: &[T]) -> Option<T> {
        match found {
            [one] => Some(one.clone()),
            _ => None,
        }
    }
    let held: BTreeSet<&PathBuf> = files.iter().collect();
    let conversations: Vec<(String, PathBuf)> = held
        .iter()
        .filter_map(|lock| {
            ag::lock_conversation(config, lock).map(|conversation| (conversation, (*lock).clone()))
        })
        .collect();
    let crashes: Vec<PathBuf> = held
        .iter()
        .filter(|file| ag::crash_file(config, file, pid))
        .map(|file| (*file).clone())
        .collect();
    Held {
        conversation: one(&conversations),
        crash: one(&crashes),
    }
}

/// WHETHER THE DEAD PROCESS'S CRASH FILE IS LEFT: the very file its snapshot
/// saw it hold open (`crash`), else — none seen — any crash file of `pid`
/// under `config` written no earlier than its start (`started_ms`, a second's
/// slack; `0`: unread, and then none is believed: a dead pid's file a crash
/// of an earlier process with the same number left is no witness of this
/// one). Read as the exit is seen: SIGKILL leaves it, every graceful way out
/// removes it, and the next agy start deletes a dead pid's file.
fn crash_left(config: &Path, pid: u32, crash: Option<&Path>, started_ms: u64) -> bool {
    if let Some(file) = crash {
        return file.is_file();
    }
    if started_ms == 0 {
        return false;
    }
    let written_ms = |file: &Path| -> Option<u64> {
        let at = std::fs::metadata(file).ok()?.modified().ok()?;
        u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
    };
    std::fs::read_dir(config.join(ag::CRASHES))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .any(|file| {
            ag::crash_file(config, &file, pid)
                && written_ms(&file).is_some_and(|at| at.saturating_add(1000) >= started_ms)
        })
}

/// The config `args` and `argv` name ([`ag::config_dir`], canonical), and the
/// home its relaunch carries ([`ag::relaunch_env`]: its `HOME` where that is
/// another than `user_home`, the user's own).
fn homes_of(
    args: &atpkg::caller_shell::ProcArgs,
    user_home: &Path,
) -> Option<(PathBuf, Vec<(String, String)>)> {
    let config = ag::config_dir(&args.argv, args.env_var(ag::HOME_ENV), user_home)?;
    Some((
        canonical(&config),
        ag::relaunch_env(|name| args.env_var(name), user_home),
    ))
}

/// THE WORD OF THE RELAUNCH LINE'S OWN COMMAND (its block `block`,
/// [`St::line_block`], read right before the line was typed), once it ended
/// ([`ag::line_exit_kind`], the crash file of the agy it started where one
/// was found, [`St::relaunched_pid`]): agy refused the relaunch (`2`), or the
/// shell could not run the line (`cd` failed, no program: refused too), or
/// the relaunched agy ended by its own or someone's hand, or crashed, or
/// ended with no witness. `None` while that command runs, where no block was
/// read before it was typed, or where the tab no longer shows it.
///
/// A LINE WHOSE AGY STILL LIVES is [`ExitKind::Stopped`], whatever its status
/// and its crash file: the line runs `exec` agy, so its block ends while the
/// process lives only where a signal stopped it (Ctrl-Z, SIGSTOP, SIGTTIN,
/// SIGTTOU) — alive, holding its lock and the crash file it made, its end
/// still to come and never a crash's.
fn line_word(c: &mut Client, st: &St, k: &dyn AgyKernel) -> Option<ExitKind> {
    let status = line_status(c, &st.tab, st.line_block)?;
    if relaunched_lives(st, k) {
        return Some(ExitKind::Stopped);
    }
    let left = st.relaunched_pid != 0
        && crash_left(
            Path::new(&st.agy_config),
            st.relaunched_pid,
            None,
            st.relaunched_at.saturating_mul(1000),
        );
    Some(ag::line_exit_kind(status, left))
}

/// Whether the agy the relaunch line started ([`St::relaunched_pid`]) still
/// lives: alive, and the very process found (its kernel start, where read,
/// [`St::relaunched_start`]), never another that took its number.
fn relaunched_lives(st: &St, k: &dyn AgyKernel) -> bool {
    st.relaunched_pid != 0
        && k.alive(st.relaunched_pid)
        && (st.relaunched_start.is_empty()
            || k.process_start(st.relaunched_pid)
                .is_some_and(|s| squash(&s) == st.relaunched_start))
}

/// WHETHER `st` IS THE RECORD OF AN EARLIER ATTEMPT AT THIS VERY EXIT THAT
/// TYPED ITS LINE: the same tab's record (its key), naming `snap`'s process,
/// its line typed ([`St::relaunched_at`]) after that process started (a
/// record of an earlier process that had the same pid number was typed
/// before it).
fn typed_for_exit(st: &St, snap: &super::super::relaunch::Snapshot) -> bool {
    let started_ms = snap.agy().map_or(0, |run| run.started_ms);
    st.agent == Agent::Agy
        && st.pid == snap.pid
        && st.relaunched_at != 0
        && (started_ms == 0
            || st.relaunched_at.saturating_mul(1000).saturating_add(1000) >= started_ms)
}

// ------------------------------------------------------- relaunch on exit

/// An agy TUI's run, read while it runs, for relaunch on exit
/// ([`super::super::relaunch::snapshot`] vetted job, tab and terminal): its
/// program and argv, its config and the home its line carries, the
/// conversation it holds ([`witness`]: `None` where none is proven — a fresh
/// agy that took no turn has none — which no relaunch then resumes), the
/// crash file it holds, the block it runs in, and its working directory (its
/// workspace: agy's is its launch directory).
pub(super) fn snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    k: &dyn AgyKernel,
) -> Result<super::super::relaunch::Snapshot, &'static str> {
    if !ag::is_tui_argv(&args.argv) {
        return Err("not-a-tui");
    }
    let exe = k.exe(pid).ok_or("exe-unreadable")?;
    let (config, env) = homes_of(args, &opts.home).ok_or("config-unreadable")?;
    let files = k.open_files(pid).ok_or("files-unreadable")?;
    let held = witness(&config, &files, pid);
    let cwd = k.cwd(pid).unwrap_or_default();
    let started_ms = k.started_ms(pid).unwrap_or(0);
    Ok(super::super::relaunch::Snapshot {
        tab: tab.to_string(),
        pid,
        start,
        shell,
        version: store_version(&exe),
        program: exe,
        argv: args.argv.clone(),
        session: held.conversation.as_ref().map(|(c, _)| c.clone()),
        cwd,
        run: super::super::relaunch::AgentRun::Agy(super::super::relaunch::AgyRun {
            config,
            env,
            lock: held.conversation.map(|(_, lock)| lock),
            crash: held.crash,
            started_ms,
            // A tab's own agy ([`NO_SHELL`]) runs in no shell's block, and no
            // shell writes it a line: read for its readers, relaunched by
            // nothing ([`after_exit_onto`], [`plan`]).
            block: (shell != NO_SHELL)
                .then(|| connect(opts, tab).ok())
                .flatten()
                .and_then(|mut c| running_block(&mut c, tab)),
        }),
        dialect: (shell != NO_SHELL)
            .then(|| shell_dialect(shell, k))
            .flatten(),
        // Claude Code's variables: an agy has no such login or home, and no
        // usage tap.
        login: None,
        home: None,
        claude_dir: None,
        stored_login: false,
        profile: None,
    })
}

/// THE WORD THE EXIT OF `snap`'S AGY LEFT: its shell's exit status of the
/// block it ran in ([`super::super::relaunch::AgyRun::block`]) and whether
/// its crash file is left ([`crash_left`]), read by agy's table
/// ([`ag::exit_kind`]); `None` while no status is drawn.
fn shell_word(opts: &Opts, snap: &super::super::relaunch::Snapshot) -> Option<ExitKind> {
    let run = snap.agy()?;
    let status = connect(opts, &snap.tab)
        .ok()
        .and_then(|mut c| exit_status(&mut c, &snap.tab, run.block))?;
    let left = crash_left(&run.config, snap.pid, run.crash.as_deref(), run.started_ms);
    Some(ag::exit_kind(status, left))
}

/// One look at an agy that left its tab
/// ([`super::super::relaunch::exit_record`] repeats it): whether the process
/// still runs (a Ctrl-Z'd agy does: stopped, it holds its lock, and its exit
/// is still to come) and, once gone, its shell's word with its crash file
/// ([`shell_word`]): a crash, its own or someone's end, or a start it
/// refused. A stopped or unwitnessed end decides nothing here (read again at
/// the attempt); the lock is no witness of the end (the file stays after
/// every way out).
pub(super) fn look_at_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    k: &dyn AgyKernel,
) -> super::super::relaunch::ExitLook {
    let running =
        k.alive(snap.pid) && k.process_start(snap.pid).as_deref() == Some(snap.start.as_str());
    let word = if running {
        None
    } else {
        shell_word(opts, snap)
    };
    super::super::relaunch::ExitLook {
        running,
        record: None,
        agent: Agent::Agy,
        crashed: match word {
            Some(ExitKind::Crashed) => Some(true),
            Some(ExitKind::Clean | ExitKind::Refused) => Some(false),
            Some(ExitKind::Stopped | ExitKind::Unwitnessed) | None => None,
        },
        refused: word == Some(ExitKind::Refused),
    }
}

/// The dialect of `shell` from its executable, as the relaunch line is
/// written for it ([`shell_dialect_of`]); `None` for a shell no line is
/// written for, which [`plan`] waits on for ever.
fn shell_dialect(shell: u32, k: &dyn AgyKernel) -> Option<Dialect> {
    shell_dialect_of(shell, k.args(shell))
}

/// Relaunch on exit, agy lane ([`super::super::relaunch::after_exit`] hands
/// an agy snapshot here): its TUI `snap` left its tab and nobody owns the
/// exit. Its shell's word with its crash file (`left`, else read now)
/// decides: its own end or someone's signal is theirs
/// (`ended:graceful-exit`), a start agy refused is never retried
/// (`ended:refused-start`), a stopped agy is no exit yet (`wait:not-exited`
/// while it lives, `ended:stopped` once it is gone with that word), and no
/// witness relaunches nothing (`ended:exit-unwitnessed`) — unless the
/// explicit restart ended it ([`ExitCause::Stall`], [`ExitCause::Asked`]: its
/// SIGTERM is one of agy's graceful ways out, mid-turn too, so a conversation
/// it ended comes back) or aterm itself ended ([`ExitCause::HostEnded`]). A
/// crash is relaunched via [`relaunch`] on the conversation the witness
/// proved, in the launch's directory, on the managed twin. `_upgrade`
/// (`[harness] upgrade`) moves nothing here: a relaunch on exit is no
/// upgrade (agy's live upgrade is its quiet flow's own step, [`live`], whose
/// exit is carried as the harness's restart and never comes here), and its
/// relaunch types the twin a person's own `agy` runs.
pub(super) fn after_exit(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    _upgrade: bool,
    cause: ExitCause,
    k: &dyn AgyKernel,
) -> Report {
    after_exit_onto(opts, snap, left, cause, managed_agent_twin(ag::PROGRAM), k)
}

/// [`after_exit`] onto the twin `twin` ([`managed_agent_twin`]): the seam a
/// test names its own through.
#[allow(clippy::too_many_lines)]
pub(super) fn after_exit_onto(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
    left: &super::super::relaunch::ExitRecord,
    cause: ExitCause,
    twin: Option<PathBuf>,
    k: &dyn AgyKernel,
) -> Report {
    use super::super::relaunch::{CAUSE_ASKED, CAUSE_EXIT, CAUSE_HOST, CAUSE_STALL, ExitRecord};
    let tab = snap.tab.clone();
    let session_key = key(&tab);
    let from = snap.version.clone().unwrap_or_else(|| "-".to_string());
    let mut r = Report {
        pid: snap.pid,
        tab: tab.clone(),
        session: session_key.clone(),
        from: from.clone(),
        to: "-".to_string(),
        step: String::new(),
    };
    let Some(run) = snap.agy() else {
        return said(r, "refused:not-agy");
    };
    let _held = match super::sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return said(r, format!("busy:{why}")),
    };
    if k.alive(snap.pid) && k.process_start(snap.pid).as_deref() == Some(snap.start.as_str()) {
        // Ctrl-Z: agy stopped, alive, holding its lock — no exit yet, never a
        // crash (the host leaves a program still there as `not-exited`).
        return said(r, "wait:not-exited");
    }
    // Records of tabs that are gone go first, under the same lock: this tab
    // is live, and the rendezvous registry speaks for the rest where it may.
    prune_known(opts, vec![snap.tab.clone()]);
    // A relaunch already in flight for this tab is carried on, never typed
    // twice; one of another process is closed, and so is one whose relaunch
    // brought up an agy gone already ([`St::relaunched_gone`]). One of this
    // exit, closed, says what an earlier attempt of it read.
    let mut relaunched_before = false;
    let prior = load(opts, &session_key);
    let mut closed = None;
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        let overtaken = (st.pid != snap.pid || st.relaunched_gone(|pid| k.alive(pid))).then(|| {
            match st.phase {
                Phase::Exiting { .. } => (
                    "resumed-elsewhere",
                    "the conversation was resumed in this tab outside aterm's relaunch, and that \
                     process has exited"
                        .to_owned(),
                ),
                _ => (
                    "exited-before-continuing",
                    "the relaunched agy exited before its continuation was typed".to_owned(),
                ),
            }
        });
        let overtook = overtaken.is_some();
        let Some((why, detail)) = overtaken.or_else(|| expired(&st, now_s(), k)) else {
            return carry_in_flight_with(opts, r, st, &session_key, k);
        };
        relaunched_before = st.relaunched_on_exit_of(snap.pid);
        if !opts.dry_run {
            if overtook {
                st.close_overtaken(why, now_s());
            } else {
                st.fail(why, now_s());
            }
            ledger(opts, &said(r.clone(), format!("failed:{why}")), &detail);
            save(opts, &session_key, &st);
        }
        closed = Some(st);
    }
    // AN EXIT AN EARLIER ATTEMPT ALREADY RELAUNCHED is typed again only on
    // the word of THAT relaunch's own line: its agy crashing (its crash file
    // left) types it again; a relaunch agy refused (`2`) or a line the shell
    // could not run is never typed again, whatever the first exit was
    // (`refused:resume-refused`); one that ended by its own or someone's hand
    // is theirs (`ended:relaunch-ended`); and one whose end cannot be read,
    // or that no crash file witnesses, relaunches nothing more
    // (`refused:relaunch-unwitnessed`: an agy it started may run there
    // unseen). Both stop for good: the owner is told the conversation was not
    // brought back.
    if let Some(earlier) = closed.or(prior).filter(|st| typed_for_exit(st, snap)) {
        let line = connect(opts, &tab)
            .ok()
            .and_then(|mut c| line_word(&mut c, &earlier, k));
        let stop = match line {
            Some(ExitKind::Crashed) => None,
            Some(ExitKind::Refused) => Some((
                "refused:resume-refused",
                "agy refused the relaunch, or the shell could not run its line: typed again it \
                 would refuse again",
            )),
            Some(ExitKind::Clean | ExitKind::Stopped) => Some((
                "ended:relaunch-ended",
                "the relaunched agy ended, or was stopped, by its own or someone's hand before it \
                 was adopted",
            )),
            Some(ExitKind::Unwitnessed) | None => Some((
                "refused:relaunch-unwitnessed",
                "the end of the relaunch's own line cannot be read, or no crash file tells it: \
                 nothing is typed again",
            )),
        };
        if let Some((step, detail)) = stop {
            let r = said(r, step);
            if !opts.dry_run {
                ledger(opts, &r, &format!("{detail} ({})", earlier.line.trim()));
            }
            return r;
        }
        relaunched_before = true;
    }
    // What the exit left, as it was seen; an exit nothing read then is read
    // now: the block the TUI ran in, with its crash file. Once an earlier
    // attempt read it and relaunched — and that relaunch's own line crashed,
    // above — it stands (the relaunch's agy deleted the first crash file as
    // it started).
    let word = match left {
        ExitRecord::Crashed => Some(ExitKind::Crashed),
        ExitRecord::Removed => Some(ExitKind::Clean),
        ExitRecord::Refused => Some(ExitKind::Refused),
        ExitRecord::Unread | ExitRecord::Survived(_) if cause == ExitCause::Exit => {
            if relaunched_before {
                Some(ExitKind::Crashed)
            } else {
                shell_word(opts, snap)
            }
        }
        ExitRecord::Unread | ExitRecord::Survived(_) => None,
    };
    if cause == ExitCause::Exit {
        match word {
            Some(ExitKind::Crashed) => {}
            Some(ExitKind::Clean) => return said(r, "ended:graceful-exit"),
            Some(ExitKind::Refused) => return said(r, "ended:refused-start"),
            Some(ExitKind::Stopped) => return said(r, "ended:stopped"),
            Some(ExitKind::Unwitnessed) | None => return said(r, "ended:exit-unwitnessed"),
        }
    }
    // A tab's own agy ([`NO_SHELL`]): no shell is left to type its relaunch
    // into, and none would come back in that tab.
    if snap.shell == NO_SHELL {
        return said(r, "refused:not-a-shell-job");
    }
    if !k.alive(snap.shell) {
        return said(r, "refused:shell-gone");
    }
    // The conversation its process held, as the witness proved it (a fresh
    // agy that took no turn holds none: a fresh `agy` would start another
    // conversation, never this one), and its presence lock under its config:
    // a lock file that is not there names a conversation agy never held here
    // (the file is never removed), which a resume would answer `not found`.
    let Some(conversation) = snap.session.clone().filter(|c| ag::is_conversation_uuid(c)) else {
        return said(r, "ended:no-conversation");
    };
    let lock = run
        .lock
        .clone()
        .unwrap_or_else(|| ag::presence_lock(&run.config, &conversation));
    if !lock.is_file() {
        return said(r, "ended:conversation-gone");
    }
    let program = match relaunch_program(&snap.program, twin) {
        Ok(p) => p,
        Err(why) => return said(r, format!("refused:{why}")),
    };
    let to = if program == snap.program {
        from.clone()
    } else {
        twin_version(&program).unwrap_or_else(|| "-".to_string())
    };
    let source = if program == snap.program {
        super::SAME_PROGRAM
    } else {
        "managed"
    };
    r.to = format!("{to}({source})");
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
        agent: Agent::Agy,
        thread: conversation,
        agy_config: run.config.to_string_lossy().into_owned(),
        agy_env: ag::env_words(&run.env),
        argv: snap.argv.clone(),
        // The workspace: agy's is the directory it was launched in.
        cwd: snap.cwd.clone(),
        twin: program.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: tab.clone(),
        from,
        to,
        source: source.to_string(),
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
    save(opts, &session_key, &st);
    super::super::relaunch::consume_stamp(opts, snap, cause);
    ledger(
        opts,
        &said(r.clone(), "exited"),
        &format!(
            "the agy left its tab ({}); relaunched on its conversation {}",
            if cause == ExitCause::HostEnded {
                "aterm ended while it ran"
            } else {
                word.map_or(left.word(), ExitKind::word)
            },
            st.thread
        ),
    );
    let r = relaunch(opts, r, &mut st, &mut c, k);
    save(opts, &session_key, &st);
    r
}

/// [`super::expired`] for an agy relaunch: its exit over [`STALE_S`] ago — or
/// the last hand that held its line, whichever is later — and not relaunched,
/// its shell gone, or relaunched and never come up.
fn expired(st: &St, now: u64, k: &dyn AgyKernel) -> Option<(&'static str, String)> {
    match st.phase {
        Phase::Exiting { at_s }
            if now.saturating_sub(exit_clock(at_s, 0, st.hand_seen_s)) > STALE_S =>
        {
            Some((
                "stale-exit",
                super::super::relaunch::stale_exit_line(st, "agy", now),
            ))
        }
        Phase::Exiting { .. } if st.shell == 0 || !k.alive(st.shell) => Some((
            "shell-gone",
            "the shell that ran agy is gone: there is no prompt to relaunch at".to_owned(),
        )),
        Phase::Relaunched { at_s } if now.saturating_sub(at_s) > STALE_S => Some((
            "no-resume",
            "the relaunched agy never came up in the tab".to_owned(),
        )),
        _ => None,
    }
}

/// The presence lock of the conversation `st` relaunches, under the config it
/// records.
fn st_lock(st: &St) -> PathBuf {
    ag::presence_lock(Path::new(&st.agy_config), &st.thread)
}

/// WHETHER THE CONVERSATION `st` RELAUNCHES IS FREE: no live process holds
/// its presence lock (a holder's `agy --conversation` would open it a second
/// time, `Conversation already open`, holding no lock), no live agy names it
/// in its argv, and no live agy holds its database with no presence lock
/// ([`AgyKernel::lockless_openers`]): one opened on it while another held it
/// — by `--conversation`, `-c`/`--continue` or an in-TUI `/resume` — holds no
/// lock, even once that holder is gone, but holds that database (measured).
/// Holders, argv or files that cannot be read are read as held, never as
/// free.
fn conversation_free(st: &St, k: &dyn AgyKernel) -> bool {
    free_at(Path::new(&st.agy_config), &st_lock(st), &st.thread, k)
}

/// [`conversation_free`] of the conversation `conversation` under the config
/// `config`, whose presence lock is `lock`.
fn free_at(config: &Path, lock: &Path, conversation: &str, k: &dyn AgyKernel) -> bool {
    k.holders_of(lock).is_some_and(|h| h.is_empty())
        && k.agy_resuming(conversation).is_some_and(|p| p.is_empty())
        && k.lockless_openers(config, conversation)
            .is_some_and(|p| p.is_empty())
}

/// Plan the relaunch from what the record keeps: the shell's dialect, the
/// heal it needs, the `cd` to the launch's directory (where the shell's
/// differs), and the line — `<twin> <carried flags> --conversation=<uuid>`
/// ([`ag::rewrite_argv`]) behind the `HOME` the process was started with
/// ([`ag::line_env`], REQUIRED: a line that cannot hold it is refused).
pub(super) fn plan(opts: &Opts, st: &St, k: &dyn AgyKernel) -> Result<Plan, NoPlan> {
    // No shell ([`NO_SHELL`], a tab's own agy) is refused for good, as the
    // Claude and Muse Code lanes' plans refuse it: nothing would read the
    // line.
    if st.shell == NO_SHELL {
        return Err(NoPlan::Refused {
            why: "not-a-shell-job".to_string(),
            what: "not-a-shell-job".to_string(),
            detail: NO_SHELL_DETAIL.to_string(),
        });
    }
    let Some(dialect) = shell_dialect(st.shell, k) else {
        return Err(NoPlan::Wait("shell-dialect"));
    };
    let heal = atpkg::hooks::hook_file(&opts.home, dialect.hook_ext()).map(|(p, _)| p);
    let want = st.cwd.clone();
    let cd =
        (!want.is_empty() && k.cwd(st.shell).as_deref() != Some(want.as_str())).then_some(want);
    let flags = ag::rewrite_argv(&st.argv, Some(&st.thread)).map_err(|e| NoPlan::Refused {
        why: format!("argv:{e}"),
        what: e.to_string(),
        detail: "the launch flags cannot be carried into a relaunch".to_string(),
    })?;
    let env = ag::env_of_words(&st.agy_env);
    let line = upgrade::relaunch_line_env(
        dialect,
        heal.as_deref(),
        cd.as_deref(),
        &ag::line_env(&env),
        Path::new(&st.twin),
        &flags,
    )
    .map_err(|e| NoPlan::Refused {
        why: format!("line:{e}"),
        what: "line".to_string(),
        detail: e,
    })?;
    Ok(Plan {
        shell: st.shell,
        dialect,
        line,
    })
}

/// Relaunch the agy `st` records at its tab's shell, the harness's hand on
/// the tab through every wait and until the new TUI's first idle.
fn relaunch(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AgyKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = relaunch_held(opts, r, st, c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(c);
    }
    r
}

/// [`relaunch`] with the harness's `hand` held.
fn relaunch_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn AgyKernel,
) -> Report {
    let old = st.pid;
    if !wait_until(opts.timings.exited, || !k.alive(old)) {
        return said(r, "wait:exiting");
    }
    hand.keep(c);
    if st.exited_at == 0 {
        st.exited_at = now_s();
    }
    let shell = st.shell;
    if !wait_until(opts.timings.prompt_back, || k.shell_has_terminal(shell)) {
        return said(r, "wait:shell-prompt");
    }
    hand.keep(c);
    // The shell draws its prompt after it takes the terminal back.
    std::thread::sleep(opts.timings.prompt_drawn);
    let tab = st.tab.clone();
    if !st_lock(st).is_file() {
        return fail(
            opts,
            r,
            st,
            "conversation-gone",
            "the conversation's presence lock is gone from its config: agy would start afresh",
        );
    }
    // A conversation another live agy has open is opened a second time with
    // no lock (`Conversation already open`): nothing is typed while one does.
    if !conversation_free(st, k) {
        return said(r, "wait:conversation-held");
    }
    let (line, dialect) = match plan(opts, st, k) {
        Ok(p) => (p.line, p.dialect),
        Err(NoPlan::Wait(why)) => return said(r, format!("wait:{why}")),
        Err(NoPlan::Refused { why, detail, .. }) => return fail(opts, r, st, &why, &detail),
    };
    st.line.clone_from(&line);
    save(opts, &key(&tab), st);
    // The command block the line is typed into, read before it is: its end
    // is the shell's own word on what the line's agy did ([`line_word`]).
    let block = super::super::relaunch::line_block(c, &tab);
    let (config, lock, conversation) = (
        PathBuf::from(&st.agy_config),
        st_lock(st),
        st.thread.clone(),
    );
    let free = |k: &dyn AgyKernel| free_at(&config, &lock, &conversation, k);
    let typed = type_relaunch_line_with(
        opts,
        c,
        shell,
        &tab,
        &line,
        Some(dialect),
        PromptLook::of(st),
        || free(k),
        |c, pid, tab| k.in_tab(c, pid, tab),
    );
    match typed {
        Ok(()) => st.prompt = None,
        Err(RelaunchLineError::Wait(why)) => {
            st.note_wait(why, now_s());
            return said(r, format!("wait:{why}"));
        }
        Err(RelaunchLineError::Hand) => {
            let now = now_s();
            st.saw_hand(now);
            st.note_wait("held", now);
            return said(r, "wait:held");
        }
        Err(RelaunchLineError::Turn(e)) => {
            st.fail("relaunch-refused", now_s());
            let r = said(r, format!("failed:relaunch:{}", super::first_word(&e)));
            ledger(opts, &r, &line);
            return r;
        }
    }
    st.relaunched(now_s());
    st.line_block = block;
    // A new line: no process of it is found yet ([`St::relaunched_pid`]).
    st.relaunched_pid = 0;
    st.relaunched_start.clear();
    ledger(
        opts,
        &said(r.clone(), "relaunched"),
        &format!("{} (resume)", st.line),
    );
    save(opts, &key(&tab), st);
    await_held(opts, r, st, c, hand, k)
}

/// Record an agy relaunch that stops here, with one ledger line.
fn fail(opts: &Opts, r: Report, st: &mut St, why: &str, detail: &str) -> Report {
    st.fail(why, now_s());
    let r = said(r, format!("failed:{why}"));
    ledger(opts, &r, detail);
    r
}

/// THE RELAUNCH LINE RAN AND ENDED with no relaunched agy found leading the
/// tab ([`line_word`]'s `kind`): it never came up, or was gone before it was
/// adopted. Decided at once, never waited out to the stale bound: a relaunch
/// agy refused, or a line the shell could not run, stops for good
/// (`refused:resume-refused`, never typed again: critic X4); one that ended
/// by its own or someone's hand is theirs (`ended:relaunch-ended`); one that
/// crashed (its crash file left) is closed as gone before it was continued,
/// and the exit's next attempt types it again on that very word
/// ([`after_exit_onto`]); one no crash file witnesses stops
/// (`refused:relaunch-unwitnessed`).
fn line_ended(opts: &Opts, r: Report, st: &mut St, kind: ExitKind) -> Report {
    let now = now_s();
    let (step, detail) = match kind {
        ExitKind::Refused => {
            st.fail("resume-refused", now);
            (
                "refused:resume-refused",
                "agy refused the relaunch, or the shell could not run its line",
            )
        }
        ExitKind::Clean | ExitKind::Stopped => {
            st.fail("relaunch-ended", now);
            (
                "ended:relaunch-ended",
                "the relaunched agy ended, or was stopped, by its own or someone's hand before it \
                 was adopted",
            )
        }
        ExitKind::Crashed => {
            st.close_overtaken("exited-before-continuing", now);
            (
                "failed:exited-before-continuing",
                "the relaunched agy crashed before its continuation was typed",
            )
        }
        ExitKind::Unwitnessed => {
            st.fail("relaunch-unwitnessed", now);
            (
                "refused:relaunch-unwitnessed",
                "the relaunched agy ended and no crash file tells how: nothing is typed again",
            )
        }
    };
    let r = said(r, step);
    ledger(opts, &r, &format!("{detail} ({})", st.line.trim()));
    r
}

/// Whether `pid` (argv `argv`) is the relaunch this lane typed: a child of
/// the line's shell, not the exited TUI, an agy TUI resuming the recorded
/// conversation.
fn relaunched_by_us(pid: u32, argv: &[String], st: &St, k: &dyn AgyKernel) -> bool {
    pid != st.pid
        && matches!(st.phase, Phase::Relaunched { .. } | Phase::Exiting { .. })
        && k.parent(pid) == Some(st.shell)
        && ag::is_tui_argv(argv)
        && ag::resumed_conversation(argv).as_deref() == Some(st.thread.as_str())
}

/// After the relaunch line: find the new TUI and carry on with it, or hand it
/// back to its loop ([`adopt`]). A later step finding the relaunch in flight
/// comes here too.
fn await_new(opts: &Opts, r: Report, st: &mut St, c: &mut Client, k: &dyn AgyKernel) -> Report {
    let mut hand = Hand::take_or_none(c, &st.tab);
    let r = await_held(opts, r, st, c, &mut hand, k);
    hand.give_back(c);
    r
}

/// [`await_new`] with the harness's `hand` held; its caller gives it back.
/// The bounds are the lane's own ([`super::super::Timings::agy_found`] and
/// its siblings).
fn await_held(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    hand: &mut Hand,
    k: &dyn AgyKernel,
) -> Report {
    let mut new = None;
    // The line's own command, looked at every `shell_look` while no agy is
    // found: once it ended, nothing is coming, and its word decides at once
    // ([`line_ended`]) — never the full bound with the hand on the tab.
    let mut ended = None;
    let mut looked: Option<std::time::Instant> = None;
    wait_until(opts.timings.agy_found, || {
        hand.keep(c);
        new = k
            .agy_children(st.shell)
            .into_iter()
            .find(|(pid, argv)| relaunched_by_us(*pid, argv, st, k))
            .map(|(pid, _)| pid);
        if let Some(pid) = new {
            // On the record at once: a line that ends as soon as it is found
            // is read with ITS crash file ([`line_word`]).
            found(st, pid, k);
        }
        if new.is_none() && looked.is_none_or(|at| at.elapsed() >= opts.timings.shell_look) {
            looked = Some(std::time::Instant::now());
            ended = line_word(c, st, k);
        }
        new.is_some() || ended.is_some()
    });
    if new.is_some_and(|pid| k.alive(pid)) {
        first_idle(c, hand, &st.tab, opts.timings.first_idle, || true);
    }
    match new {
        Some(pid) if opts.hand_back && st.resumed_pid == pid => {
            let mut r = said(r, "adopted");
            r.pid = pid;
            r
        }
        Some(pid) if opts.hand_back => adopt(opts, r, st, c, pid, k),
        Some(pid) => carry_on(opts, r, st, c, pid, k, Some(hand)),
        None => {
            // The line's own command ended: its agy never came up, or was
            // gone before it was found.
            if let Some(kind) = ended.or_else(|| line_word(c, st, k)) {
                return line_ended(opts, r, st, kind);
            }
            if let Phase::Relaunched { at_s } = st.phase
                && now_s().saturating_sub(at_s) > STALE_S
            {
                return fail(
                    opts,
                    r,
                    st,
                    "no-resume",
                    "the relaunched agy never came up in the tab",
                );
            }
            said(r, "wait:resume")
        }
    }
}

/// The relaunched TUI `new`, on the record whatever becomes of it: once it is
/// gone, an agy leaving the tab is that TUI, its exit its own.
fn found<K: Kernel + ?Sized>(st: &mut St, new: u32, k: &K) {
    if st.relaunched_pid == 0 {
        st.relaunched_start = k.process_start(new).map(|s| squash(&s)).unwrap_or_default();
    } else if st.relaunched_pid != new {
        st.relaunched_start.clear();
    }
    st.relaunched_pid = new;
}

/// THE RELAUNCHED AGY STARTED A FRESH CONVERSATION: it runs, and it does not
/// hold the conversation's presence lock, which `--conversation` takes at
/// once (measured 0.49 s after the line's Enter). agy says `conversation not
/// found` and starts afresh where it reads another config (another `HOME`, a
/// hidden `--gemini_dir`/`--app_data_dir` not carried), and opens a
/// conversation another agy has open with no lock. Nothing is typed into it,
/// and the person is told (`refused:started-fresh`: the host's `Cannot`, its
/// mark naming agy's own resume line).
fn started_fresh(opts: &Opts, r: Report, st: &mut St, new: u32) -> Report {
    st.fail("started-fresh", now_s());
    let mut r = said(r, "refused:started-fresh");
    r.pid = new;
    ledger(
        opts,
        &r,
        &format!(
            "the relaunched agy holds no lock on conversation {}: it started a fresh one \
             (`conversation not found` under its config), or another agy has that conversation \
             open; nothing is typed into it",
            st.thread
        ),
    );
    r
}

/// Whether the relaunched agy `new` takes the conversation's presence lock
/// within [`super::super::Timings::agy_lock`]; the hand kept meanwhile.
fn takes_lock(
    opts: &Opts,
    st: &St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
    mut hand: Option<&mut Hand>,
) -> bool {
    let lock = st_lock(st);
    wait_until(opts.timings.agy_lock, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        k.holders_of(&lock).is_some_and(|h| h.contains(&new))
    })
}

/// Hand the relaunched agy back to its supervisor (`adopted`): the new TUI
/// leads the tab by the host's own roster, HOLDS ITS CONVERSATION'S LOCK
/// (else it started fresh: [`started_fresh`]), and the record stays in
/// flight; at its loop's next idle point the relaunch primitive's
/// continuation ([`super::super::relaunch::resume`]) types the carry-on
/// ([`carry_on`]).
fn adopt(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
) -> Report {
    found(st, new, k);
    let tab = st.tab.clone();
    // A process gone is never coming to lead the tab: no wait for it.
    let mut gone = false;
    let led = wait_until(opts.timings.agy_adopt, || {
        let led = k.in_tab(c, new, &tab);
        gone = !led && !k.alive(new);
        led || gone
    }) && !gone;
    if !led {
        // Gone before it led the tab: its line's own word decides now — the
        // shell marks the line's end a moment after its program's, so a
        // process seen gone gets that moment.
        let mut word = None;
        let bound = if gone {
            opts.timings.ended_at_once
        } else {
            Duration::ZERO
        };
        wait_until(bound, || {
            word = line_word(c, st, k);
            word.is_some()
        });
        if let Some(kind) = word {
            return line_ended(opts, r, st, kind);
        }
        return said(r, "wait:resume");
    }
    if !takes_lock(opts, st, c, new, k, None) {
        if !k.alive(new) {
            // Gone while its lock was waited on: its line's word, once drawn.
            let mut word = None;
            wait_until(opts.timings.ended_at_once, || {
                word = line_word(c, st, k);
                word.is_some()
            });
            if let Some(kind) = word {
                return line_ended(opts, r, st, kind);
            }
            return said(r, "wait:resume");
        }
        return started_fresh(opts, r, st, new);
    }
    st.resumed_pid = new;
    r.pid = new;
    let r = said(r, "adopted");
    ledger(
        opts,
        &r,
        if st.quiet {
            "the relaunched agy leads the tab and holds its conversation; at its first idle \
             point its supervisor confirms it still does, and types nothing"
        } else {
            "the relaunched agy leads the tab and holds its conversation; its supervisor types \
             the continuation at its first idle point"
        },
    );
    r
}

/// An agy relaunch in flight carried on from where it stopped: the relaunch
/// primitive's continuation step for an agy record
/// ([`super::super::relaunch::resume`]), and every other caller's.
pub(in crate::harness) fn carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    carry_in_flight_with(opts, r, st, session, &Live)
}

/// [`carry_in_flight`] over the kernel `k`.
fn carry_in_flight_with(
    opts: &Opts,
    r: Report,
    mut st: St,
    session: &str,
    k: &dyn AgyKernel,
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

/// Whether agy's composer on `scr` is empty: its caret row holds nothing (a
/// permission mode's hint, agy's only placeholder, is read as nothing by
/// [`aterm_phase::agy::composer_draft`]) and no row under it holds a word.
fn composer_empty(scr: &super::Screen) -> bool {
    aterm_phase::agy::composer_draft(&scr.rows)
        .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
}

/// The last look before typing into agy's composer: a fresh read reading idle
/// by its reader (no box, no turn running, its composer drawn, no exit
/// pending) with the composer empty, and that read's generation, which the
/// typed turn is fenced on.
fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    let _ = turn_takes_gen(c);
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    match aterm_phase::agy::phase(&scr.rows) {
        (aterm_phase::Phase::Idle | aterm_phase::Phase::Question, true) => {}
        (aterm_phase::Phase::Prompt, _) => return Err("box"),
        _ => return Err("busy"),
    }
    if !composer_empty(&scr) {
        return Err("draft");
    }
    Ok(scr.generation)
}

/// Type `text` into agy's composer as one turn, fenced on `generation` and
/// submitted under the composer's guard once the echo has held still
/// (`idle=600`; agy submits an Enter in the text's own burst too, measured on
/// 1.4.0, so the settle only proves the text landed whole).
fn type_text(c: &mut Client, tab: &str, text: &str, generation: Option<&str>) -> Result<(), Typed> {
    let fence = generation
        .filter(|g| is_generation(g))
        .filter(|_| turn_takes_gen(c))
        .map(|g| format!("if-gen={g} yield=0.2 "))
        .unwrap_or_default();
    let guard = super::composer_guard('>', text);
    let (head, _) = c
        .request_counted(&format!(
            "@{tab} turn {fence}submit=guarded:{guard} idle=600 {TURN_WAIT} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// The relaunched TUI `new` is up: once its composer is drawn, its
/// conversation's lock is held by it (else it started fresh:
/// [`started_fresh`], nothing typed) and it reads idle and empty with no
/// person at the tab, the continuation is typed (`continued`); a quiet
/// relaunch types nothing.
#[allow(clippy::too_many_arguments)]
fn carry_on(
    opts: &Opts,
    mut r: Report,
    st: &mut St,
    c: &mut Client,
    new: u32,
    k: &dyn AgyKernel,
    mut hand: Option<&mut Hand>,
) -> Report {
    found(st, new, k);
    r.pid = new;
    let tab = st.tab.clone();
    let up = wait_until(opts.timings.agy_up, || {
        if let Some(h) = hand.as_deref_mut() {
            h.keep(c);
        }
        screen(c, &tab).is_some_and(|s| aterm_phase::agy::composer(&s.rows).is_some())
    });
    let mut typed = false;
    // THE QUIET RESTART'S ADOPTION (the live upgrade's G-A3): the relaunch
    // is the conversation's once the new process holds its presence lock,
    // never on a typed line's verdict, and nothing is typed either way.
    let mut holds = false;
    if up {
        holds = takes_lock(opts, st, c, new, k, hand.as_deref_mut());
        if !holds && k.alive(new) {
            return started_fresh(opts, r, st, new);
        }
        let settled = !st.quiet
            && wait_until(opts.timings.carry_settle, || {
                if let Some(h) = hand.as_deref_mut() {
                    h.keep(c);
                }
                typing_fence(c, &tab).is_ok()
            });
        // A person at the keyboard wins: held, it stays owed.
        if settled && held(c, &tab, opts.human_grace_s) {
            return said(r, "wait:held");
        }
        if settled && k.in_tab(c, new, &tab) {
            let text =
                super::super::relaunch::resumed_prompt_as(Agent::Agy.product(), &st.to, &st.cause);
            let tries = under_hand(c, &tab, hand, |c| {
                for _ in 0..3 {
                    let Ok(generation) = typing_fence(c, &tab) else {
                        break;
                    };
                    match type_text(c, &tab, &text, generation.as_deref()) {
                        Ok(()) => {
                            typed = true;
                            break;
                        }
                        Err(Typed::Changed | Typed::Yielded) => {
                            std::thread::sleep(Duration::from_millis(400));
                        }
                        Err(Typed::Cut(_) | Typed::Refused(_)) => break,
                    }
                }
            });
            if tries.is_none() {
                return said(r, "wait:held");
            }
        }
    }
    st.phase = Phase::Done;
    st.resumed_pid = new;
    st.resumed_on.clone_from(&st.to);
    st.done_at = now_s();
    st.outcome = if st.quiet {
        format!(
            "agy upgraded to {} (from {}) · the same conversation resumed, nothing typed",
            st.to, st.from
        )
    } else {
        format!("agy on {} · the same conversation resumed", st.to)
    };
    let step = if !up {
        "done:unconfirmed"
    } else if typed {
        "continued"
    } else if st.quiet && holds {
        "done:resumed"
    } else if st.quiet {
        // Up, and gone before its lock was seen held by it: the relaunch is
        // not proved the conversation's (nothing was typed either way).
        "done:unconfirmed"
    } else {
        "done:no-continue"
    };
    let r = said(r, step);
    ledger(opts, &r, &st.outcome);
    r
}

// ---------------------------------------------------------------- the prune

/// Remove agy records nothing will visit again, as codex-alab's path removes
/// its own (`codex_alab_drive::prune`): a record goes once its tab is proven
/// gone (`life`, [`TabLife::Gone`]), it is unwritten for
/// [`super::STATE_KEEP_S`] and settled; never one in flight or not matching
/// its name, and nothing in a dry run or a background break. Called under the
/// sweep lock.
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
            .and_then(|stem| Agent::Agy.record_tab(stem))
        else {
            continue;
        };
        if !super::sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = load(opts, &key(tab)) else {
            continue;
        };
        let ours = st.agent == Agent::Agy && (st.tab.is_empty() || st.tab == tab);
        if ours && !st.in_flight() && super::settled(&st) && life(tab) == TabLife::Gone {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The prune of one sweep, as codex-alab's sweep prunes its records
/// (`codex_alab_drive::sweep`): every tab the host's roster names is live,
/// and the rendezvous registry speaks for the rest where it may.
pub(super) fn sweep_prune(opts: &Opts, live_tabs: Option<&[LiveTab]>) {
    prune_known(
        opts,
        live_tabs
            .into_iter()
            .flatten()
            .map(|t| t.sid.clone())
            .collect(),
    );
}

/// [`prune`] with the tabs `known_live` proven live, the rendezvous registry
/// read for the rest only where it can vouch for this state root
/// ([`registry_graph`], [`registry_life`]).
fn prune_known(opts: &Opts, known_live: Vec<String>) {
    let graph = registry_graph();
    prune(opts, now_s(), registry_life(graph.as_deref(), &known_live));
}

// ---------------------------------------------------------------- the owner's view

/// The live agy holders of the `agy-<tab>` records in `sessions`, for the
/// status view ([`super::upgrade_status::Holder`]): the TUI leading the tab
/// and its build. The host proves the tab by roster; a hand-run view by the
/// TUI's environment.
pub(super) fn holders(
    sessions: &BTreeSet<String>,
    live_tabs: Option<&[LiveTab]>,
) -> Vec<super::upgrade_status::Holder> {
    if !sessions.iter().any(|s| Agent::Agy.record_tab(s).is_some()) {
        return Vec::new();
    }
    let procs = table();
    let tab_of = |pid: u32| -> Option<String> {
        match live_tabs {
            Some(tabs) => tabs
                .iter()
                .find(|t| t.fgpgid == Some(i64::from(pid)))
                .map(|t| t.sid.clone()),
            None => atpkg::caller_shell::process_args(pid)
                .and_then(|a| a.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned)),
        }
    };
    procs
        .iter()
        .filter(|(_, _, name)| name == ag::PROGRAM)
        .filter_map(|(pid, _, _)| {
            let tab = tab_of(*pid)?;
            let session = key(&tab);
            sessions
                .contains(&session)
                .then(|| super::upgrade_status::Holder {
                    session,
                    version: exe_of(*pid)
                        .and_then(|exe| store_version(&exe))
                        .unwrap_or_default(),
                    tab: Some(tab),
                    ..Default::default()
                })
        })
        .collect()
}

// THE LIVE UPGRADE'S QUIET FLOW, its I/O half: the step the window's host
// takes at the idle point it parked its loop at.
#[path = "upgrade_agy_live_drive.rs"]
pub(super) mod live;

// The agy lane's driver tests drive scripted processes over a stand-in tab.
#[cfg(all(test, unix))]
#[path = "upgrade_agy_drive_tests.rs"]
mod tests;
