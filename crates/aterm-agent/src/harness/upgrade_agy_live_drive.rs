// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! I/O half of the live `agy` upgrade step ([`upgrade_visit`]), paired with [`super::super::super::upgrade_agy_live`].

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

/// Install source label recorded by `build_start::proved_named` for managed `agy` builds.
pub(super) const SOURCE: &str = "agy-managed";

/// The wait word of a step at the agent's background break: nothing ends an
/// agy there (the restart is the next idle point's).
const AT_BREAK: &str = "wait:break";

/// Returns the foreground `agy` leader PID for `tab` from `tabs`.
pub(super) fn leader(tabs: &[LiveTab], tab: &str, k: &dyn AgyKernel) -> Option<u32> {
    let pid = u32::try_from(tabs.iter().find(|t| t.sid == tab)?.fgpgid?).ok()?;
    let named = |p: &Path| p.file_name().and_then(|n| n.to_str()) == Some(ag::PROGRAM);
    let agy = k.exe(pid).is_some_and(|exe| named(&exe))
        || k.args(pid)
            .and_then(|a| a.argv.first().cloned())
            .is_some_and(|argv0| named(Path::new(&argv0)));
    agy.then_some(pid)
}

/// Pending version move from the running `agy` build to the managed twin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Move {
    /// Running build version from its store record.
    pub(super) running: String,
    /// Target twin build version from its store record ([`live::moves_onto`]).
    pub(super) target: String,
    /// Managed twin executable path (`<prefix>/agents/agy`).
    pub(super) twin: PathBuf,
}

/// A store build's version as its record names it, only where the record
/// and the build's directory agree ([`live::build_version`]).
fn build_record(exe: &Path) -> Option<String> {
    let version = store_version(exe)?;
    let dir = exe.parent()?.parent()?.file_name()?.to_str()?;
    live::build_version(&version, Some(dir)).map(|_| version)
}

/// Returns the [`Move`] owed by `exe` onto `twin`, or `Err` when unprovable ([`Open::Unproved`]).
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

/// Answers [`super::super::due`] for a tab led by `agy`.
pub(in super::super) fn upgrade_due(opts: &Opts, tabs: &[LiveTab]) -> Option<Due> {
    upgrade_due_with(
        opts,
        tabs,
        managed_agent_twin(ag::PROGRAM),
        &super::super::Live,
    )
}

/// Evaluates [`upgrade_due`] against `twin` and kernel `k`.
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

/// Returns `true` when `agy` leads `opts.only_sid` ([`super::super::note_behind`]).
pub(in super::super) fn leads(opts: &Opts, tabs: &[LiveTab]) -> bool {
    opts.only_sid
        .as_deref()
        .is_some_and(|tab| leader(tabs, tab, &super::super::Live).is_some())
}

/// Runs the `agy` upgrade sweep step ([`super::super::lanes`]) for `opts.only_sid`.
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

/// Reads the conversation transcript tail for process `started_ms` (`G-A2(b)`).
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

/// Inspects `.system_generated/messages/` for unread or recent messages (`G-A2(e)`).
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
const SQLITE3: &str = aterm_uds::os_path!("sqlite3");

/// File name of `agy`'s conversation summary database.
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

/// Reads summary rows for `conversation` and its subagents from [`SUMMARIES`] via [`aterm_uds::foreign_db::read`].
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

/// Returns `true` when `rows` show `agy` authoritatively idle with no active surface.
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

/// Assembles a [`Look`] from the held state, screen rows, summaries, transcript, and messages.
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

/// Finds other live `agy` processes sharing `conversation` (`G-A4`) via [`AgyKernel::agy_resuming`].
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

/// Gathers the [`Look`] at `agy` process `pid` in `tab` for [`live::gate`].
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

/// Builds the restart claim record (`G-A3`) written before `SIGTERM` (`--resume <uuid>`).
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
        cwd: snap.cwd.clone(),
        twin: mv.twin.to_string_lossy().into_owned(),
        pid: snap.pid,
        shell: snap.shell,
        tab: snap.tab.clone(),
        from: mv.running.clone(),
        to: mv.target.clone(),
        source: "managed".to_string(),
        cause: String::new(),
        quiet: true,
        salt: now,
        phase: Phase::Exiting { at_s: now },
        ..St::default()
    })
}

/// Executes one quiet-upgrade step for `opts.only_sid` across `tabs`, `twin`, and kernel `k`.
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
    match plan(opts, &st, k) {
        Ok(_) => {}
        Err(super::NoPlan::Wait(why)) => return Some(said(r, format!("wait:{why}"))),
        Err(super::NoPlan::Refused { why, .. }) => return Some(said(r, format!("skip:{why}"))),
    }
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
    let Some(mut hand) = Hand::take(&mut c, &tab) else {
        return Some(said(r, "wait:held"));
    };
    let prior = load(opts, &session_key);
    if save_checked(opts, &session_key, &st).is_err() {
        hand.give_back(&mut c);
        return Some(said(r, "wait:claim-unwritten"));
    }
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
