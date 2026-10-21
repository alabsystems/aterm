// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AGY LIVE UPGRADE, ITS I/O HALF: the quiet flow's one step
//! ([`upgrade_visit`]) at the idle point the window's host parked its loop at
//! ([`super::super::super::upgrade_agy_live`] is the pure half, and says what
//! was measured). For the tab's foreground agy TUI:
//!
//! 1. **Behind** ([`behind`]): the build it runs (its mapped executable, a
//!    store build) against the build the managed twin execs, each by its store
//!    record's version, in atpkg's order; never `--version`.
//! 2. **The look** ([`look_at`]), read in this order so that work ending
//!    while it is read shows in a later read — the screen first, the files
//!    last: no hold, no hand, no unread input and no person within the grace;
//!    no background work on the screen (G-A2(a)) and its reader's idle reading
//!    (Idle, authoritative, no wall) over an empty composer (G-A1); no
//!    descendant process at all (G-A2(d)); no other live agy with the
//!    conversation open ([`sharers`], G-A4: the relaunch could never type
//!    while one has); the conversation's summary rows read only, live
//!    ([`read_summaries`]: G-A1, G-A2(c)); its transcript's
//!    rows of this process with no task or subagent open ([`transcript_look`]:
//!    G-A2(b)); no wake pending in its messages ([`messages_look`]: G-A2(e)).
//!    One wait word each ([`upgrade_agy_live::gate`]); every unreadable
//!    witness fails closed.
//! 3. **The build proved to start** (`build_start::proved_named`: `<twin>
//!    --help` with null stdio in its own process group, measured in that very
//!    form by the lab's `agy-upgrade`: exit 0, nothing written, no request).
//! 4. **The line planned** before anything ends it: a launch the relaunch
//!    cannot carry is never restarted (`skip:<why>`) — its flags, or an
//!    environment it set on its own typed line (`skip:env-not-carried`: the
//!    shell's record of the command the tab runs, [`live::inline_env`]; the
//!    relaunch line carries only `HOME`).
//! 5. **The claim** ([`claim_record`], G-A3): the tab's record names the
//!    process, its conversation, its config and the `HOME` its line carries,
//!    the twin and the upgrade's cause, in flight, ON DISK BEFORE THE SIGNAL —
//!    so its exit `0` is the upgrade's own restart
//!    (`relaunch::ended_by_restart`), carried by the host's exit watch, never
//!    read as someone's graceful end. A write that does not complete waits
//!    (`wait:claim-unwritten`) and sends no signal. Then the last look again
//!    (the same process, still holding its conversation's presence lock, its
//!    shell's foreground job in this tab, the whole gate still open), else the
//!    record put back as it was (`wait:changed-before-signal`).
//! 6. **SIGTERM** (`terminate`: one server-side decision that refuses under a
//!    hold or a person's input within the grace), then **the relaunch**: the
//!    lane's own (`relaunch_held`: the exit, the shell's prompt, the
//!    conversation free, `<twin> … --conversation=<uuid>` typed), adopted by
//!    the new process that leads the tab, and carried on QUIETLY: nothing is
//!    typed after, the record done once the new process holds
//!    `presence/<uuid>.lock` (`done:resumed`).
//!
//! The ask flow never runs here: `[harness] upgrade_ask` is read by Claude
//! Code's lane alone, and at a background break (`opts.background`) nothing
//! of this step acts (`wait:break`). A hand-run sweep (no roster of its own)
//! moves no agy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::super::upgrade_agy_live::{self as live, Look, Message, Open};
use super::super::build_start;
use super::super::upgrade::{Agent, Phase};
use super::super::{
    Client, Due, Hand, LiveTab, Opts, Report, St, Terminated, bounded_output, connect, held, load,
    managed_agent_twin, now_s, restore, said, save, save_checked, screen, store_version, terminate,
};
use super::{AgyKernel, ag, carry_in_flight_with, key, ledger, plan, relaunch_held};

/// The install word an agy build's start note is filed under
/// (`build_start::proved_named`): its own, so a Claude Code build of the same
/// spelling is never another's.
pub(super) const SOURCE: &str = "agy-managed";

/// The wait word of a step at the agent's background break: nothing ends an
/// agy there (the restart is the next idle point's).
const AT_BREAK: &str = "wait:break";

/// The foreground leader of tab `tab` by the host's roster (`tabs`), where it
/// is an agy: its pid. A leader whose program cannot be read, or that is no
/// `agy`, is not this lane's (`None`).
pub(super) fn leader(tabs: &[LiveTab], tab: &str, k: &dyn AgyKernel) -> Option<u32> {
    let pid = u32::try_from(tabs.iter().find(|t| t.sid == tab)?.fgpgid?).ok()?;
    let named = |p: &Path| p.file_name().and_then(|n| n.to_str()) == Some(ag::PROGRAM);
    let agy = k.exe(pid).is_some_and(|exe| named(&exe))
        || k.args(pid)
            .and_then(|a| a.argv.first().cloned())
            .is_some_and(|argv0| named(Path::new(&argv0)));
    agy.then_some(pid)
}

/// A move owed: the version the running build's record names, the twin's
/// build's, and the twin a relaunch types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Move {
    /// The running build's version (its store record).
    pub(super) running: String,
    /// The twin's build's version (its store record): newer by
    /// [`live::moves_onto`].
    pub(super) target: String,
    /// The managed twin (`<prefix>/agents/agy`) the relaunch types.
    pub(super) twin: PathBuf,
}

/// A store build's version as its record names it, only where the record
/// and the build's directory agree ([`live::build_version`]).
fn build_record(exe: &Path) -> Option<String> {
    let version = store_version(exe)?;
    let dir = exe.parent()?.parent()?.file_name()?.to_str()?;
    live::build_version(&version, Some(dir)).map(|_| version)
}

/// THE MOVE `exe` (the running agy's mapped executable) IS OWED onto the
/// build the managed twin `twin` execs: `Ok(Some)` where the twin's build is
/// strictly newer by atpkg's order of their store records; `Ok(None)` for a
/// process run from no store build (another install: nothing managed moves
/// it), no twin, or a build not newer; `Err(word)` where a record cannot be
/// read (`no-version`).
pub(super) fn behind(exe: &Path, twin: Option<PathBuf>) -> Result<Option<Move>, &'static str> {
    if !ag::store_build(exe) {
        return Ok(None);
    }
    let Some(twin) = twin else {
        return Ok(None);
    };
    let Some(build) = std::fs::read_to_string(&twin)
        .ok()
        .and_then(|text| super::super::catalog::twin_binary(&text))
    else {
        return Ok(None);
    };
    let running = build_record(exe).ok_or(super::super::UNREAD_VERSION)?;
    let target = build_record(&build).ok_or(super::super::UNREAD_VERSION)?;
    Ok(live::moves_onto(&running, &target).then_some(Move {
        running,
        target,
        twin,
    }))
}

/// [`super::super::due`]'s answer for a tab an agy leads (`None`: not this
/// lane's tab): `Yes` while its build is behind the twin's, `No` when current
/// (or no TUI, or no managed build), `Unread` where its process or a build
/// record cannot be read. A relaunch of this tab in flight is
/// [`super::super::in_flight`]'s, asked before.
pub(in super::super) fn upgrade_due(opts: &Opts, tabs: &[LiveTab]) -> Option<Due> {
    upgrade_due_with(
        opts,
        tabs,
        managed_agent_twin(ag::PROGRAM),
        &super::super::Live,
    )
}

/// [`upgrade_due`] over the twin `twin` and the kernel `k`.
pub(super) fn upgrade_due_with(
    opts: &Opts,
    tabs: &[LiveTab],
    twin: Option<PathBuf>,
    k: &dyn AgyKernel,
) -> Option<Due> {
    let tab = opts.only_sid.as_deref()?;
    let pid = leader(tabs, tab, k)?;
    let Some(args) = k.args(pid) else {
        return Some(Due::Unread(super::super::UNREAD_PROCESS));
    };
    if !ag::is_tui_argv(&args.argv) {
        return Some(Due::No);
    }
    let Some(exe) = k.exe(pid) else {
        return Some(Due::Unread(super::super::UNREAD_PROCESS));
    };
    Some(match behind(&exe, twin) {
        Ok(Some(_)) => Due::Yes,
        Ok(None) => Due::No,
        Err(word) => Due::Unread(word),
    })
}

/// Whether an agy leads tab `opts.only_sid` ([`super::super::note_behind`]'s
/// lane answer): its lane mints no ladder state (the quiet flow has none), so
/// nothing is noted for it.
pub(in super::super) fn leads(opts: &Opts, tabs: &[LiveTab]) -> bool {
    opts.only_sid
        .as_deref()
        .is_some_and(|tab| leader(tabs, tab, &super::super::Live).is_some())
}

/// The agy lane's share of one sweep ([`super::super::lanes`]): the quiet
/// flow's step for tab `opts.only_sid`, where an agy leads it by the host's
/// own roster. A pass with no roster of its own (the CLI's sweep) moves no
/// agy.
pub(in super::super) fn upgrade_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let Some(tabs) = live_tabs else {
        return Vec::new();
    };
    upgrade_visit(
        opts,
        tabs,
        managed_agent_twin(ag::PROGRAM),
        &super::super::Live,
    )
    .into_iter()
    .collect()
}

// ---------------------------------------------------------------- the files

/// The directory agy keeps a conversation's own files under
/// (`<config>/brain/<uuid>/.system_generated`).
fn generated(config: &Path, conversation: &str) -> PathBuf {
    config
        .join("brain")
        .join(conversation)
        .join(".system_generated")
}

/// How much of a transcript the look reads from its end: this process's
/// start must be in the window (else [`Open::Unproved`]).
const TRANSCRIPT_BYTES: u64 = 32 << 20;

/// The transcript's text as read — whole, or its last [`TRANSCRIPT_BYTES`]
/// with the partial first line dropped (`true`) — or `None` where it cannot
/// be read.
fn transcript_tail(path: &Path) -> Option<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let truncated = len > TRANSCRIPT_BYTES;
    if truncated {
        f.seek(SeekFrom::Start(len - TRANSCRIPT_BYTES)).ok()?;
    }
    let mut buf = Vec::new();
    f.take(TRANSCRIPT_BYTES).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if truncated {
        let rest = text.split_once('\n').map_or("", |(_, rest)| rest);
        (rest.to_owned(), true)
    } else {
        (text, false)
    })
}

/// G-A2(b) OVER THE CONVERSATION'S TRANSCRIPT (`<config>/brain/<uuid>/
/// .system_generated/logs/transcript.jsonl`) for the process started at
/// `started_ms` ([`live::quiet_transcript`]): one that cannot be read FAILS
/// CLOSED ([`Open::Unreadable`]).
pub(super) fn transcript_look(
    config: &Path,
    conversation: &str,
    started_ms: u64,
) -> Result<(), Open> {
    let path = generated(config, conversation).join("logs/transcript.jsonl");
    let (text, truncated) = transcript_tail(&path).ok_or(Open::Unreadable)?;
    live::quiet_transcript(&text, truncated, started_ms)
}

/// A file's modification time, unix ms.
fn modified_ms(meta: &std::fs::Metadata) -> Option<u64> {
    let at = meta.modified().ok()?;
    u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// G-A2(e) OVER THE CONVERSATION'S MESSAGES (`<config>/brain/<uuid>/
/// .system_generated/messages/`: one `<id>.json` each, `read.json` marking
/// the read ones) at `now_ms` ([`live::wakes`]): the wakes pending, `None`
/// where the directory, a message or `read.json` cannot be read (fail closed).
/// No directory yet is no message.
pub(super) fn messages_look(config: &Path, conversation: &str, now_ms: u64) -> Option<usize> {
    let dir = generated(config, conversation).join("messages");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut messages = Vec::new();
    for entry in entries {
        let path = entry.ok()?.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .filter(|id| *id != "read")
        else {
            continue;
        };
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() {
            continue;
        }
        messages.push(Message {
            id: id.to_owned(),
            body: std::fs::read_to_string(&path).ok()?,
            written_ms: modified_ms(&meta)?,
        });
    }
    let read = match std::fs::read_to_string(dir.join("read.json")) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    live::wakes(read.as_deref(), &messages, now_ms).ok()
}

/// Apple's `sqlite3`, named absolutely (a platform binary): the one reader of
/// agy's summary database, READ ONLY.
const SQLITE3: &str = "/usr/bin/sqlite3";

/// agy's summary database under its config.
pub(super) const SUMMARIES: &str = "conversation_summaries.db";

/// How long one read may take: it takes milliseconds (4 ms typical, 61 ms the
/// most over 2 179 live reads); the bound is for a wedged process.
const SUMMARY_WAIT: Duration = Duration::from_secs(5);

/// The query: the conversation's own row and its subagents' (`conversation`
/// a UUIDv4, checked by the caller, so no quote can be in it).
fn summary_query(conversation: &str) -> String {
    format!(
        "SELECT conversation_id, status, not_fully_idle, parent_conversation_id FROM \
         conversation_summaries WHERE conversation_id = '{conversation}' OR \
         parent_conversation_id = '{conversation}';"
    )
}

/// One run of the reader over `target` (a private copy's path, or an
/// immutable URI: [`aterm_uds::foreign_db::read`]):
/// `-readonly`, no `~/.sqliterc` (`-init /dev/null`), `-safe` (no command that
/// touches another file), `-nofollow`, one `|`-separated line per row, no
/// header.
fn run_reader(target: &str, sql: &str) -> Option<String> {
    let words = [
        "-init",
        "/dev/null",
        "-readonly",
        "-safe",
        "-nofollow",
        "-batch",
        "-bail",
        "-noheader",
        "-list",
        "-separator",
        "|",
        target,
        sql,
    ];
    let args: Vec<&std::ffi::OsStr> = words.iter().map(std::ffi::OsStr::new).collect();
    bounded_output(Path::new(SQLITE3), &args, SUMMARY_WAIT, 1 << 16)
}

/// THE CONVERSATION'S SUMMARY ROWS, READ FROM THE LIVE DATABASE WITH NO LOCK
/// ON IT (`<config>/conversation_summaries.db`, [`aterm_uds::foreign_db::read`]):
/// while agy holds it open (its `-wal` beside it), from a private copy of the
/// file and its `-wal`; while agy has it closed — some 30 s after its last
/// write it closes it, deleting both — as an immutable file. Never through
/// SQLite's own open of the live file, which joins agy's locking: a reader
/// that finds the WAL index unbuilt (its `-shm` just made by agy's open) takes
/// the index's write lock to build it, and agy, whose busy timeout is 5 s,
/// logs `summary store: sync upsert failed: ... database is locked` past it
/// and drops that write for good (measured, 2026-10-20). An answer is kept
/// only where the files are the same after the read as before; twice at most
/// (a write, a close or an open between the look at the files and the read);
/// `Err` where neither read holds — fail closed.
pub(super) fn read_summaries(config: &Path, conversation: &str) -> Result<String, ()> {
    if !ag::is_conversation_uuid(conversation) {
        return Err(());
    }
    let db = config.join(SUMMARIES);
    let sql = summary_query(conversation);
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Ok(out) = aterm_uds::foreign_db::read(&db, |target| run_reader(target, &sql)) {
            return Ok(out);
        }
    }
    Err(())
}

// ---------------------------------------------------------------- the look

/// G-A1'S SCREEN HALF: whether agy's own reader reads `rows` as the quiet
/// restart's idle point — Idle AUTHORITATIVELY and no wall. A turn's spinner,
/// its own background work over an ended turn, an exit pending, a box, a
/// question the turn ended on and a screen agy left all read otherwise; a
/// sign-in or an API error stands as a wall, which a restart would bury
/// under the resumed screen.
#[must_use]
pub(super) fn screen_idle(rows: &[String]) -> bool {
    aterm_phase::agy::phase(rows) == (aterm_phase::Phase::Idle, true)
        && aterm_phase::agy::wall(rows).is_none()
}

/// Whether agy's composer on `rows` is empty: its caret row holds nothing (a
/// permission mode's hint is nothing) and no row under it a word.
fn composer_empty(rows: &[String]) -> bool {
    aterm_phase::agy::composer_draft(rows)
        .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
}

/// THE LOOK FROM WHAT WAS READ: the person's hold, the screen's rows
/// (`None`: unread, which reads neither idle nor empty, and shows no work
/// though the screen witness holds by the idle half), the summary database's
/// answer ([`live::summary_rows`] of `conversation`), the transcript, the
/// process's descendants, the wakes pending and the other agys on the
/// conversation ([`sharers`]). The one composition [`look_at`] makes and the
/// Tier-1 bind replays.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(super) fn look_of(
    held: bool,
    rows: Option<&[String]>,
    conversation: &str,
    summaries: Result<String, ()>,
    transcript: Result<(), Open>,
    children: Option<usize>,
    wakes: Option<usize>,
    shared: Option<usize>,
) -> Look {
    Look {
        held,
        shown: rows.and_then(aterm_phase::agy::background_shown),
        idle: rows.is_some_and(screen_idle),
        composer_empty: rows.is_some_and(composer_empty),
        summary: summaries
            .map_err(|()| live::Unreadable)
            .and_then(|out| live::summary_rows(conversation, &out)),
        transcript,
        children,
        wakes,
        shared,
    }
}

/// THE OTHER LIVE AGYS WITH `conversation` OPEN (G-A4): those whose argv
/// names it ([`AgyKernel::agy_resuming`]) and those holding its database under
/// `config` with no presence lock ([`AgyKernel::lockless_openers`]: opened on
/// it while `pid` held it, by `--conversation`, `-c` or an in-TUI `/resume`,
/// and holding it still once `pid` is gone: measured) — never `pid` itself.
/// The relaunch types nothing while one has it open (`wait:conversation-held`),
/// so a restart begun then would end an idle agy that nothing brings back.
/// `None` where either cannot be read: held, never free.
pub(super) fn sharers(
    k: &dyn AgyKernel,
    config: &Path,
    conversation: &str,
    pid: u32,
) -> Option<usize> {
    let mut others: std::collections::BTreeSet<u32> =
        k.agy_resuming(conversation)?.into_iter().collect();
    others.extend(k.lockless_openers(config, conversation)?);
    others.remove(&pid);
    Some(others.len())
}

/// THE LOOK AT AGY `pid` IN `tab` the quiet flow's gate takes
/// ([`live::gate`]), its conversation `conversation` under `config`, the
/// process started at `started_ms`: read screen first and files last (the
/// module header), each by its own rule.
#[allow(clippy::too_many_arguments)]
pub(super) fn look_at(
    opts: &Opts,
    c: &mut Client,
    tab: &str,
    pid: u32,
    config: &Path,
    conversation: &str,
    started_ms: u64,
    k: &dyn AgyKernel,
) -> Look {
    let held = held(c, tab, opts.human_grace_s);
    let scr = screen(c, tab);
    let children = k.descendants(pid).map(|d| d.len());
    let shared = sharers(k, config, conversation, pid);
    let summaries = k.summaries(config, conversation);
    let transcript = transcript_look(config, conversation, started_ms);
    let now_ms = now_s().saturating_mul(1000);
    let wakes = messages_look(config, conversation, now_ms);
    look_of(
        held,
        scr.as_ref().map(|s| s.rows.as_slice()),
        conversation,
        summaries,
        transcript,
        children,
        wakes,
        shared,
    )
}

/// THE CLAIM (G-A3): the record the quiet restart writes BEFORE its SIGTERM,
/// from the snapshot `snap` of the agy it ends and the move `mv` — the
/// process, its shell and tab, its conversation, its config and the `HOME`
/// its line carries, its launch's argv and directory, the twin, the versions,
/// and the UPGRADE'S cause (empty: [`super::super::super::relaunch::ends_its_agent`]),
/// in flight ([`Phase::Exiting`]) and quiet: what makes its exit the
/// upgrade's own (`relaunch::ended_by_restart`) and what the relaunch plans
/// its line from. `None` for a snapshot of no agy or with no conversation.
pub(super) fn claim_record(
    snap: &super::super::super::relaunch::Snapshot,
    mv: &Move,
    now: u64,
) -> Option<St> {
    let run = snap.agy()?;
    let thread = snap
        .session
        .clone()
        .filter(|s| ag::is_conversation_uuid(s))?;
    Some(St {
        agent: Agent::Agy,
        thread,
        agy_config: run.config.to_string_lossy().into_owned(),
        agy_env: ag::env_words(&run.env),
        argv: snap.argv.clone(),
        // The workspace: agy's is the directory it was launched in.
        cwd: snap.cwd.clone(),
        twin: mv.twin.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: snap.tab.clone(),
        from: mv.running.clone(),
        to: mv.target.clone(),
        source: "managed".to_string(),
        // The upgrade's own: ends its agent ([`relaunch::ends_its_agent`]).
        cause: String::new(),
        quiet: true,
        salt: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    })
}

/// THE QUIET FLOW'S STEP for tab `opts.only_sid` over the host's roster
/// `tabs` (the module's steps 1–6), the twin `twin` and the kernel `k`.
/// `None`: no agy leads the tab and none of its relaunches is in flight there
/// (not this lane's). The words: `current` (nothing newer, or nothing
/// managed); `wait:<why>` (the gate's, the build's proof, the socket, a claim
/// not written, the claim's last look, the signal's refusal, `break` at a
/// background break); `skip:<why>` (a launch no relaunch carries — its flags,
/// or an environment set on its own line — a tab's own program);
/// `terminated` once signalled and the relaunch's own words after it
/// (`adopted`, `done:resumed`, a wait the host's exit watch carries on); a
/// relaunch of this tab in flight is carried by the lane's own rules.
#[allow(clippy::too_many_lines)]
pub(super) fn upgrade_visit(
    opts: &Opts,
    tabs: &[LiveTab],
    twin: Option<PathBuf>,
    k: &dyn AgyKernel,
) -> Option<Report> {
    let tab = opts.only_sid.clone()?;
    let session_key = key(&tab);
    let report = |pid: u32| Report {
        pid,
        tab: tab.clone(),
        session: session_key.clone(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: String::new(),
    };
    // A relaunch of this tab in flight — the upgrade's own, or a crash's —
    // is carried by its lane's rules, never begun again, WHOEVER LEADS THE
    // TAB NOW: between the SIGTERM and the typed line its shell does.
    if let Some(st) = load(opts, &session_key).filter(|st| st.agent == Agent::Agy && st.in_flight())
    {
        let r = report(leader(tabs, &tab, k).unwrap_or(st.pid));
        if opts.background {
            return Some(said(r, AT_BREAK));
        }
        return Some(carry_in_flight_with(opts, r, st, &session_key, k));
    }
    let pid = leader(tabs, &tab, k)?;
    let r = report(pid);
    let Some(args) = k.args(pid) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    if !ag::is_tui_argv(&args.argv) {
        return Some(said(r, "current"));
    }
    let Some(exe) = k.exe(pid) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    let mv = match behind(&exe, twin) {
        Ok(Some(mv)) => mv,
        Ok(None) => return Some(said(r, "current")),
        Err(word) => return Some(said(r, format!("wait:{word}"))),
    };
    let r = Report {
        from: mv.running.clone(),
        to: format!("{}(managed)", mv.target),
        ..r
    };
    if opts.background {
        return Some(said(r, AT_BREAK));
    }
    // Its shell's foreground job, else nothing would take the terminal back.
    let shell = match super::super::foreground_shell(k.job(pid)) {
        Ok(shell) => shell,
        Err("not-a-shell-job") => return Some(said(r, "skip:not-a-shell-job")),
        Err(why) => return Some(said(r, format!("wait:{why}"))),
    };
    let Some(start) = k.process_start(pid).map(|s| super::super::squash(&s)) else {
        return Some(said(r, format!("wait:{}", super::super::UNREAD_PROCESS)));
    };
    let snap = match super::snapshot(opts, &tab, pid, start.clone(), shell, &args, k) {
        Ok(snap) => snap,
        Err(why) => return Some(said(r, format!("wait:{why}"))),
    };
    let Some(run) = snap.agy() else {
        return Some(said(r, "wait:not-agy"));
    };
    // A fresh agy that took no turn holds no conversation: nothing to resume.
    let (Some(lock), Some(conversation)) = (run.lock.clone(), snap.session.clone()) else {
        return Some(said(r, "wait:no-session"));
    };
    let (config, started_ms) = (run.config.clone(), run.started_ms);
    let Ok(mut c) = connect(opts, &tab) else {
        return Some(said(r, "wait:no-socket"));
    };
    let look = |c: &mut Client| look_at(opts, c, &tab, pid, &config, &conversation, started_ms, k);
    if let Err(why) = live::gate(&look(&mut c)) {
        return Some(said(r, format!("wait:{why}")));
    }
    let now = now_s();
    let Some(mut st) = claim_record(&snap, &mv, now) else {
        return Some(said(r, "wait:no-session"));
    };
    // The line the relaunch types, planned before anything ends the agent: a
    // launch it cannot carry is never restarted.
    match plan(opts, &st, k) {
        Ok(_) => {}
        Err(super::NoPlan::Wait(why)) => return Some(said(r, format!("wait:{why}"))),
        Err(super::NoPlan::Refused { why, .. }) => return Some(said(r, format!("skip:{why}"))),
    }
    // An environment the launch set on its own typed line, which the line
    // the relaunch types in the shell's environment would not carry.
    if super::super::super::relaunch::running_cmdline(&mut c, &tab)
        .as_deref()
        .and_then(live::inline_env)
        .is_some()
    {
        return Some(said(r, "skip:env-not-carried"));
    }
    if let Err(why) = build_start::proved_named(opts, &r, &mv.twin, &mv.target, SOURCE, now) {
        return Some(said(r, format!("wait:{why}")));
    }
    if opts.dry_run {
        return Some(said(r, "would-restart"));
    }
    // The harness's hand on the tab from the last look to the relaunched
    // agy's first idle point: no other writer lands in the dying agent or the
    // bare shell.
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return Some(said(r, "wait:held"));
    };
    let prior = load(opts, &session_key);
    if save_checked(opts, &session_key, &st).is_err() {
        hand.give_back(&mut c);
        return Some(said(r, "wait:claim-unwritten"));
    }
    // THE LAST LOOK, the claim on disk: the same process, still holding its
    // conversation's presence lock, still its shell's foreground job in this
    // tab, and the gate still open.
    let still = k.alive(pid)
        && k.process_start(pid)
            .map(|s| super::super::squash(&s))
            .as_deref()
            == Some(start.as_str())
        && k.holders_of(&lock).is_some_and(|h| h.contains(&pid))
        && super::super::foreground_shell(k.job(pid)) == Ok(shell)
        && k.in_tab(&mut c, pid, &tab)
        && live::gate(&look(&mut c)).is_ok();
    if !still {
        restore(opts, &session_key, prior.as_ref());
        hand.give_back(&mut c);
        return Some(said(r, "wait:changed-before-signal"));
    }
    let Ok(signalled) = i32::try_from(pid) else {
        restore(opts, &session_key, prior.as_ref());
        hand.give_back(&mut c);
        return Some(said(r, "wait:pid"));
    };
    match terminate(&mut c, &tab, signalled, opts.human_grace_s) {
        Terminated::Sent => {}
        Terminated::Refused(why) => {
            restore(opts, &session_key, prior.as_ref());
            hand.give_back(&mut c);
            return Some(said(r, format!("wait:signal-{why}")));
        }
        Terminated::Failed => {
            st.fail("signal-refused", now_s());
            save(opts, &session_key, &st);
            hand.give_back(&mut c);
            let r = said(r, "failed:signal-refused");
            ledger(
                opts,
                &r,
                "the kernel refused the SIGTERM: agy runs on, on the build it ran",
            );
            return Some(r);
        }
    }
    ledger(
        opts,
        &said(r.clone(), "terminated"),
        &format!(
            "SIGTERM (idle: nothing asked or typed; its conversation {} resumed on {} as it was)",
            st.thread, mv.target
        ),
    );
    let r = relaunch_held(opts, r, &mut st, &mut c, &mut hand, k);
    if !matches!(st.phase, Phase::Exiting { .. }) {
        hand.give_back(&mut c);
    }
    save(opts, &session_key, &st);
    Some(r)
}
