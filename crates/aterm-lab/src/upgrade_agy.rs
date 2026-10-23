// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agy-upgrade [tag] [--keep]`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::door::Door;
use crate::gemini::Agent;
use crate::pyjson::J;
use crate::relaunch_agy::{lock_conversation, resumed, shell_exports, twin_script};
use crate::relaunch_check::{host_said, newest_dump, open_files, request_with, said, wait_for};
use crate::run::{Run, is_run};
use crate::stop;
use crate::upgrade_muse::{
    RAN_MS, held_while, ledger, quiet, record_naming, row_at, show_held, show_rows,
};
use crate::util::now_ms;
use crate::{Fail, LAB, Res};

/// The word the first turn asks agy to remember.
pub const WORD: &str = "HERON-7070";

/// How long phase 6's stub holds its answer.
pub const LONG_TURN_MS: i64 = 130_000;

/// How long the stub holds phase 3's subagent's model call.
pub const SUBAGENT_HOLD_MS: i64 = 40_000;

/// The host's line on agy's own activation notice.
pub const NOTICED: &str = "an activation notice for Antigravity CLI";

/// The words agy's command box asks with (`Run this command?`).
const COMMAND_BOX: &str = "Run this command";

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("agy-upgrade: COULD NOT RUN: {m}"))
}

/// The store build id of a plain version `x.y.z`.
#[must_use]
pub fn plain_build_id(version: &str) -> Option<u64> {
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && (s == "0" || !s.starts_with('0')))
        .then(|| s.parse().ok())
        .flatten()
        .filter(|n| *n < 1_000_000)
    };
    let mut parts = version.split('.');
    let (x, y, z) = (
        num(parts.next()?)?,
        num(parts.next()?)?,
        num(parts.next()?)?,
    );
    if parts.next().is_some() {
        return None;
    }
    Some(1_000_000_000_000_000_000 + x * 1_000_000_000_000 + y * 1_000_000 + z)
}

/// `version` (`x.y.z`) with its patch raised by `k`: newer in atpkg's order.
#[must_use]
pub fn bumped(version: &str, k: u64) -> Option<String> {
    plain_build_id(version)?;
    let (head, patch) = version.rsplit_once('.')?;
    Some(format!("{head}.{}", patch.parse::<u64>().ok()? + k))
}

/// The build a twin script execs (`exec '<build>' "$@"`), or `path` itself
/// where it is no script.
fn exec_target(path: &Path) -> PathBuf {
    let text = crate::util::scratch_text(path);
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("exec '"))
        .find_map(|rest| rest.split_once('\'').map(|(p, _)| PathBuf::from(p)))
        .unwrap_or_else(|| path.to_path_buf())
}

/// The scratch store: its prefix, the source build, its record.
struct Store {
    /// `<scratch home>/Library/Application Support/aterm/pkg`.
    prefix: PathBuf,
    /// The lab's build the store's clones are made of.
    source: PathBuf,
    /// The record text the source build's `.vendor` holds (or one written).
    record: String,
}

impl Store {
    fn store(&self) -> PathBuf {
        self.prefix.join("store/agy")
    }

    fn twin(&self) -> PathBuf {
        self.prefix.join("agents/agy")
    }

    /// LAND build `version` as atpkg lands one (the module's order): its
    /// tree, its record and `.ready`, `current` swapped, the twin rewritten,
    /// and agy's activation notice published.
    fn land(&self, version: &str) -> Result<PathBuf, String> {
        let id = plain_build_id(version).ok_or("no build id")?;
        let bin = self.store().join(id.to_string()).join("bin/agy");
        crate::sys::put(&bin, 0o755, &crate::sys::PutSrc::File(&self.source))
            .map_err(|e| format!("the build's tree: {e}"))?;
        let store = self.store();
        crate::sys::put(
            &store.join(format!("{id}.vendor")),
            0o644,
            &crate::sys::PutSrc::Bytes(record_naming(&self.record, version).as_bytes()),
        )
        .map_err(|e| format!("its record: {e}"))?;
        crate::sys::put(
            &store.join(format!("{id}.ready")),
            0o644,
            &crate::sys::PutSrc::Bytes(b""),
        )
        .map_err(|e| format!("its .ready: {e}"))?;
        let tmp = store.join(format!(".current-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(id.to_string(), &tmp).map_err(|e| format!("current: {e}"))?;
        let current = store.join("current");
        std::fs::rename(&tmp, &current).map_err(|e| format!("current: {e}"))?;
        let resolved = std::fs::canonicalize(&current).map_err(|e| format!("current: {e}"))?;
        let build = bin
            .parent()
            .and_then(Path::parent)
            .and_then(|b| std::fs::canonicalize(b).ok());
        if build.as_deref() != Some(resolved.as_path()) {
            return Err(format!("current resolves to {}", resolved.display()));
        }
        crate::sys::put(
            &self.twin(),
            0o755,
            &crate::sys::PutSrc::Bytes(twin_script(&bin.to_string_lossy()).as_bytes()),
        )
        .map_err(|e| format!("the twin: {e}"))?;
        crate::sys::put(
            &self.prefix.join("activation-notices/agy"),
            0o600,
            &crate::sys::PutSrc::Bytes(b"1\n"),
        )
        .map_err(|e| format!("the activation notice: {e}"))?;
        Ok(bin)
    }
}

/// The agy processes under the lab's instance run from the scratch store
/// (`<store>/<build>/bin/agy …`), `(pid, build, words)`.
fn store_agys(run: &Run, store: &Path) -> Vec<(i32, String, Vec<String>)> {
    let prefix = store.to_string_lossy().into_owned();
    crate::procs::descendants(run.instance_pid)
        .into_iter()
        .filter_map(|pid| {
            let cmd = crate::procs::command_line(pid);
            let rest = cmd.strip_prefix(&prefix)?.strip_prefix('/')?;
            let (build, after) = rest.split_once("/bin/agy")?;
            (after.is_empty() || after.starts_with(' ')).then(|| {
                (
                    pid,
                    build.to_string(),
                    after.split_whitespace().map(str::to_string).collect(),
                )
            })
        })
        .collect()
}

/// How many MAIN turns the stub has answered agy (`kind=gemini`, never a
/// side request), by its log: logged as each answer ends.
fn main_turns(run: &Run) -> usize {
    crate::util::scratch_text(&run.w.join("stub.log"))
        .lines()
        .filter(|l| l.contains(" kind=gemini ") && l.contains("GenerateContent"))
        .count()
}

/// The descendants of `pid` (its tasks' processes).
fn children(pid: i32) -> usize {
    crate::procs::descendants(pid).len()
}

/// agy's config under its scratch home.
fn config(run: &Run) -> PathBuf {
    let c = run
        .w
        .join(Agent::Agy.home())
        .join(".gemini/antigravity-cli");
    std::fs::canonicalize(&c).unwrap_or(c)
}

/// The conversation whose presence lock `pid` holds open (`lsof`).
fn conversation_of(run: &Run, pid: i32) -> Option<String> {
    let config = config(run);
    let held: Vec<String> = open_files(pid)
        .iter()
        .filter_map(|f| lock_conversation(&config, f))
        .collect();
    match held.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// What one restart onto a build looked like, read off the real thing.
struct Moved {
    pid: i32,
    words: Vec<String>,
    holds_lock: bool,
    rows: Vec<(i64, String, String)>,
}

/// Wait up to `secs` for an agy resuming `conversation` from build `build`,
/// other than `old`; then its lock and the ledger since `since`.
fn moved_onto(
    run: &Run,
    store: &Path,
    build: u64,
    conversation: &str,
    old: i32,
    secs: u64,
    since: i64,
) -> Result<Option<Moved>, Fail> {
    let found = wait_for(secs, || {
        store_agys(run, store).into_iter().find(|(pid, b, w)| {
            *pid != old && *b == build.to_string() && resumed(w) == Some(conversation)
        })
    })?;
    let Some((pid, _, words)) = found else {
        return Ok(None);
    };
    let _ = wait_for(60, || {
        ledger(run, since)
            .iter()
            .any(|(_, s, _)| s.starts_with("done:"))
            .then_some(())
    })?;
    let holds_lock = conversation_of(run, pid).as_deref() == Some(conversation);
    Ok(Some(Moved {
        pid,
        words,
        holds_lock,
        rows: ledger(run, since),
    }))
}

/// Press `key` on agy's command box while it is drawn (fenced on its words,
/// so a key never lands in the composer): whether it was drawn.
fn approve_box(door: &Door<'_>) -> bool {
    door.out(&["text"]).contains(COMMAND_BOX)
        && door
            .ctl(&["key", &format!("if={COMMAND_BOX}"), "1"])
            .is_ok_and(|r| r.code == 0)
}

/// Whether the work first seen at `from` and seen ended at `end` (ms) ran its
/// course ([`RAN_MS`]).
fn ran(from: Option<i64>, end: Option<i64>) -> bool {
    matches!((from, end), (Some(from), Some(end)) if end - from >= RAN_MS)
}

/// Whether the `terminated` row of `m` came at or after `end` (ms), to the
/// second the ledger keeps — [`held_while`] already saw the agy stand past
/// the work's end.
fn after(m: Option<&Moved>, end: Option<i64>) -> bool {
    match (m, end) {
        (Some(m), Some(end)) => {
            row_at(&m.rows, "terminated").is_some_and(|t| t * 1000 >= end - 999)
        }
        _ => false,
    }
}

/// One restart said: its rows, its process.
fn show_moved(m: &Moved, build: u64, what: &str) {
    show_rows(&m.rows);
    println!(
        "  restarted agy pid {} from {build}{what}: {}; it holds its conversation's lock={}",
        m.pid,
        m.words.join(" "),
        m.holds_lock
    );
}

/// Whether `m` is a quiet restart onto its build that kept the conversation.
fn kept(m: Option<&Moved>) -> bool {
    m.is_some_and(|m| m.holds_lock && quiet(&m.rows))
}

/// One `agy-upgrade` run on the lab `tag`, which `cmd_agy_upgrade` brought up.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str) -> Result<bool, Fail> {
    let mut run = Run::load(tag)?;
    crate::gemini::need(&run, Agent::Agy).map_err(|f| could_not(&f.msg.unwrap_or_default()))?;
    let source = exec_target(Path::new(&run.agy_bin));
    println!(
        "agy-upgrade: lab '{tag}', agy {} ({}, built {}), {}",
        run.agy_ver,
        run.agy_bin,
        source.display(),
        run.aterm
    );
    let mut failed = Vec::new();
    let source_record = source
        .parent()
        .and_then(Path::parent)
        .map(|b| PathBuf::from(format!("{}.vendor", b.display())))
        .map(|p| crate::util::scratch_text(&p))
        .filter(|t| t.contains("version"))
        .unwrap_or_else(|| {
            format!(
                "schema = 1\nprogram = \"agy\"\nversion = \"{}\"\nvendor = \"Google\"\n",
                run.agy_ver
            )
        });
    let store = Store {
        prefix: run.w.join("home/Library/Application Support/aterm/pkg"),
        source: source.clone(),
        record: source_record,
    };
    let v1 = run.agy_ver.clone();
    let versions: Vec<String> = (0..7).filter_map(|k| bumped(&v1, k)).collect();
    let ids: Vec<u64> = versions.iter().filter_map(|v| plain_build_id(v)).collect();
    if versions.len() != 7 || ids.len() != 7 {
        return Err(could_not(&format!("{v1} is no plain x.y.z version")));
    }
    store
        .land(&versions[0])
        .map_err(|e| could_not(&format!("laying {v1}: {e}")))?;
    let rc = run.w.join("home/.zshrc");
    crate::sys::put(
        &rc,
        0o644,
        &crate::sys::PutSrc::Bytes(shell_exports(&run).as_bytes()),
    )
    .map_err(|e| could_not(&format!("the shell startup: {e}")))?;
    let door = Door::new(&run)?;
    let rc_s = rc.to_string_lossy().replace('\'', "'\\''");
    door.ctl(&["send", &format!(". '{rc_s}'")])?;
    door.ctl(&["key", "enter"])?;
    stop::sleep(Duration::from_millis(500))?;
    println!(
        "  store     {} — {v1} at {}, the twin {}",
        store.store().display(),
        ids[0],
        store.twin().display()
    );

    let mut through = run.clone();
    through.agy_bin = store.twin().to_string_lossy().into_owned();
    crate::gemini::launch(
        &through,
        Agent::Agy,
        &crate::gemini::Launch {
            no_wait: true,
            ..crate::gemini::Launch::default()
        },
    )?;
    run = Run::load(tag)?;
    let door = Door::new(&run)?;
    if !door.seen("Antigravity CLI", 60)? || !door.seen("^>", 60)? {
        return Err(could_not("agy never drew its composer"));
    }
    door.await_idle(1500, 30)?;
    if !crate::verbs::turn(&door, &format!("remember the word {WORD}"))? {
        return Err(could_not("the first turn was not sent"));
    }
    if !door.seen(r"\(reply 1\)", 60)? {
        return Err(could_not("the stub's first answer never showed"));
    }
    let Some((pid1, conversation)) = wait_for(30, || {
        store_agys(&run, &store.store())
            .into_iter()
            .find_map(|(pid, b, _)| {
                (b == ids[0].to_string())
                    .then(|| conversation_of(&run, pid).map(|c| (pid, c)))
                    .flatten()
            })
    })?
    else {
        return Err(could_not(
            "no agy from the scratch store holds a presence lock",
        ));
    };
    println!(
        "  launched  agy pid {pid1} from {}, conversation {conversation}",
        ids[0]
    );
    door.await_idle(2000, 60)?;
    let home_gemini = run.w.join("home/.gemini");
    let probed_home_before = home_gemini.exists();

    let turns_before = main_turns(&run);
    let t1 = now_ms();
    store
        .land(&versions[1])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[1])))?;
    println!(
        "  landed    {} at {} (the twin re-pointed, agy's activation notice published)",
        versions[1], ids[1]
    );
    let noticed = wait_for(30, || host_said(&run, t1, &[NOTICED]))?;
    println!(
        "  noticed   {}",
        noticed
            .as_deref()
            .map_or("NO host line on the notice within 30 s", |l| l
                .split_once("harness:")
                .map_or(l, |(_, s)| s.trim()))
    );
    let Some(m1) = moved_onto(&run, &store.store(), ids[1], &conversation, pid1, 120, t1)? else {
        show_rows(&ledger(&run, t1));
        show_held(&run, t1);
        said(
            false,
            "1. an idle agy is restarted onto the new build within 120 s",
            &mut failed,
        );
        return Ok(false);
    };
    show_moved(&m1, ids[1], "");
    stop::sleep(Duration::from_secs(5))?;
    let silent = main_turns(&run) == turns_before;
    let probed = row_at(&m1.rows, "build-probe:starts").is_some();
    let home_clean = probed_home_before || !home_gemini.exists();
    println!(
        "  probe     the ledger's build-probe:starts={probed}; the instance's HOME has no .gemini after it={home_clean}; no turn reached the stub since the landing={silent}"
    );
    door.await_idle(2000, 60)?;
    let before_turn = newest_dump(&run);
    crate::verbs::turn(&door, "what was the word?")?;
    let carried = wait_for(60, || request_with(&run, before_turn, &[WORD]))?;
    println!(
        "  next turn {}",
        carried.map_or_else(
            || "NO request carried the earlier word within 60 s".to_string(),
            |n| format!("req-{n:05} carried `{WORD}`: the prior turns came back")
        )
    );
    said(
        noticed.is_some()
            && kept(Some(&m1))
            && silent
            && probed
            && home_clean
            && carried.is_some()
            && !crate::procs::alive(pid1),
        "1. an idle agy is restarted onto the landed build on its conversation (argv, lock), nothing typed after; the stub sees the prior turns",
        &mut failed,
    );
    door.await_idle(2000, 60)?;
    let mut pid = m1.pid;

    crate::verbs::set_tool(
        &run,
        r#"{"name":"run_command","input":{"CommandLine":"sleep 40; echo LAB-BG-DONE","Cwd":"@PROJ@","WaitMsBeforeAsync":500,"toolAction":"Running background sleep","toolSummary":"Background sleep"}}"#,
    )?;
    crate::verbs::turn(&door, "toolstart")?;
    let child = wait_for(30, || {
        approve_box(&door);
        (children(pid) > 0).then_some(now_ms())
    })?;
    let drawn = wait_for(30, || door.out(&["text"]).contains("task(s)").then_some(()))?;
    let t2 = now_ms();
    store
        .land(&versions[2])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[2])))?;
    println!(
        "  bg task   a child under pid {pid}={}, its count drawn={}; landed {} at {}",
        child.is_some(),
        drawn.is_some(),
        versions[2],
        ids[2]
    );
    let (held2, ended2) = held_while(&run, pid, t2, 120, || children(pid) > 0)?;
    let ran2 = ran(child, ended2);
    println!(
        "  bg ended  {}; the agy stood while it ran={held2}, and after its end; it ran its course={ran2}",
        ended2.map_or_else(
            || "NOT within 60 s".to_string(),
            |t| format!("after {} ms", t - t2)
        )
    );
    let m2 = moved_onto(&run, &store.store(), ids[2], &conversation, pid, 180, t2)?;
    show_held(&run, t2);
    if let Some(m) = &m2 {
        show_moved(m, ids[2], "");
    }
    said(
        child.is_some() && held2 && ran2 && after(m2.as_ref(), ended2) && kept(m2.as_ref()),
        "2. an agy whose background command task runs is not restarted while it runs, and is once it ended",
        &mut failed,
    );
    let Some(m2) = m2 else {
        return Ok(false);
    };
    pid = m2.pid;
    door.await_idle(2000, 90)?;

    crate::verbs::set_tool(
        &run,
        &format!(
            r#"[{{"name":"invoke_subagent","input":{{"Subagents":[{{"Model":"inherit","Prompt":"LABSUB-HOLD: reply with the single word DONE.","Role":"Lab Waiter","TypeName":"research"}}],"toolAction":"Invoking lab subagent","toolSummary":"Lab subagent"}}}},{{"when":"LABSUB-HOLD","text":"DONE","delay_ms":{SUBAGENT_HOLD_MS}}}]"#
        ),
    )?;
    crate::verbs::turn(&door, "toolstart")?;
    let working = wait_for(30, || {
        door.out(&["text"])
            .contains("subagent(s)")
            .then_some(now_ms())
    })?;
    let t3 = now_ms();
    store
        .land(&versions[3])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[3])))?;
    println!(
        "  subagent  working, its count drawn={}; landed {} at {}",
        working.is_some(),
        versions[3],
        ids[3]
    );
    let (held3, ended3) = held_while(&run, pid, t3, 180, || {
        door.out(&["text"]).contains("subagent(s)")
    })?;
    let ran3 = ran(working, ended3);
    println!(
        "  sub idle  {}; the agy stood while it worked={held3}, and after it went idle; it ran its course={ran3}",
        ended3.map_or_else(
            || "NOT within 90 s".to_string(),
            |t| format!("after {} ms", t - t3)
        )
    );
    let m3 = moved_onto(&run, &store.store(), ids[3], &conversation, pid, 180, t3)?;
    show_held(&run, t3);
    if let Some(m) = &m3 {
        show_moved(m, ids[3], "");
    }
    said(
        working.is_some() && held3 && ran3 && after(m3.as_ref(), ended3) && kept(m3.as_ref()),
        "3. an agy whose subagent works is not restarted while it works, and is once it went idle",
        &mut failed,
    );
    let Some(m3) = m3 else {
        return Ok(false);
    };
    pid = m3.pid;
    door.await_idle(2000, 90)?;

    crate::verbs::set_tool(
        &run,
        r#"{"name":"run_command","input":{"CommandLine":"sleep 40","Cwd":"@PROJ@","IsDaemon":true,"WaitMsBeforeAsync":500,"toolAction":"Running daemon sleep","toolSummary":"Daemon sleep"}}"#,
    )?;
    crate::verbs::turn(&door, "toolstart")?;
    let daemon = wait_for(30, || {
        approve_box(&door);
        (children(pid) > 0).then_some(now_ms())
    })?;
    let t4 = now_ms();
    store
        .land(&versions[4])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[4])))?;
    println!(
        "  daemon    a child under pid {pid}={}; landed {} at {}",
        daemon.is_some(),
        versions[4],
        ids[4]
    );
    let (held4, ended4) = held_while(&run, pid, t4, 120, || children(pid) > 0)?;
    let ran4 = ran(daemon, ended4);
    println!(
        "  dmn ended {}; the agy stood while it ran={held4}, and after its end; it ran its course={ran4}",
        ended4.map_or_else(
            || "NOT within 60 s".to_string(),
            |t| format!("after {} ms", t - t4)
        )
    );
    let m4 = moved_onto(&run, &store.store(), ids[4], &conversation, pid, 180, t4)?;
    show_held(&run, t4);
    if let Some(m) = &m4 {
        show_moved(m, ids[4], "");
    }
    said(
        daemon.is_some() && held4 && ran4 && after(m4.as_ref(), ended4) && kept(m4.as_ref()),
        "4. an agy whose daemon task runs (no summary row sees it) is not restarted while it runs, and is once it ended",
        &mut failed,
    );
    let Some(m4) = m4 else {
        return Ok(false);
    };
    pid = m4.pid;
    door.await_idle(2000, 90)?;

    crate::verbs::set_mode(&run, "slow")?;
    crate::verbs::turn(&door, "a slow turn")?;
    let busy = door.seen("esc to cancel", 30)?;
    let t5 = now_ms();
    store
        .land(&versions[5])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[5])))?;
    println!(
        "  busy      the turn running={busy}; landed {} at {}",
        versions[5], ids[5]
    );
    let (held5, ended5) = held_while(&run, pid, t5, 240, || {
        door.out(&["text"]).contains("esc to cancel")
    })?;
    println!(
        "  turn end  {}; the agy stood while it ran={held5}, and after its end",
        ended5.map_or_else(
            || "NOT within 120 s".to_string(),
            |t| format!("after {} ms", t - t5)
        )
    );
    let m5 = moved_onto(&run, &store.store(), ids[5], &conversation, pid, 180, t5)?;
    show_held(&run, t5);
    if let Some(m) = &m5 {
        show_moved(m, ids[5], " after the turn");
    }
    said(
        busy && held5 && ended5.is_some() && after(m5.as_ref(), ended5) && kept(m5.as_ref()),
        "5. a busy agy is not restarted mid-turn, and is once its turn ended",
        &mut failed,
    );
    let Some(m5) = m5 else {
        let _ = crate::verbs::set_mode(&run, "ok");
        return Ok(false);
    };
    pid = m5.pid;

    crate::verbs::set_harness(&run, &[("continue".to_string(), "true".to_string())])?;
    stop::sleep(Duration::from_secs(5))?;
    door.await_idle(2000, 90)?;
    crate::verbs::set_custom_mode(
        &run,
        "lab-long-turn",
        &J::obj([
            ("delay_ms", J::Int(LONG_TURN_MS)),
            (
                "text",
                J::s("Part of the task is done; the next part is still ahead of me."),
            ),
        ]),
    )?;
    crate::verbs::turn(&door, "a long turn with the loop on")?;
    let busy6 = door.seen("esc to cancel", 30)?;
    let t6 = now_ms();
    let turns_at_landing = main_turns(&run);
    store
        .land(&versions[6])
        .map_err(|e| could_not(&format!("landing {}: {e}", versions[6])))?;
    println!(
        "  driven    continue=true, the turn running={busy6}; landed {} at {}",
        versions[6], ids[6]
    );
    let mut turns_at_restart = None;
    let mut turn_end6 = None;
    for _ in 0..720 {
        stop::sleep(Duration::from_millis(500))?;
        if row_at(&ledger(&run, t6), "terminated").is_some() || !crate::procs::alive(pid) {
            turns_at_restart = Some(main_turns(&run));
            break;
        }
        if turn_end6.is_none() && !door.out(&["text"]).contains("esc to cancel") {
            turn_end6 = Some(now_ms());
        }
    }
    let m6 = moved_onto(&run, &store.store(), ids[6], &conversation, pid, 180, t6)?;
    show_held(&run, t6);
    let between = turns_at_restart.map(|n| n.saturating_sub(turns_at_landing));
    println!(
        "  driven    the turn ended {}; turns the stub answered from the landing to the restart: {}",
        turn_end6.map_or_else(|| "NOT seen".to_string(), |t| format!("+{} ms", t - t6)),
        between.map_or_else(|| "NO restart".to_string(), |n| n.to_string())
    );
    if let Some(m) = &m6 {
        show_moved(m, ids[6], " at that turn's end");
    }
    let _ = crate::verbs::set_mode(&run, "ok");
    said(
        busy6 && turn_end6.is_some() && between == Some(1) && kept(m6.as_ref()),
        "6. with the loop's continuation on, the restart takes the turn end the loop would continue at: no turn typed between",
        &mut failed,
    );
    if failed.is_empty() {
        println!("agy-upgrade: PASS");
        Ok(true)
    } else {
        println!("agy-upgrade: FAIL: {}", failed.join("; "));
        Ok(false)
    }
}

/// `agy-upgrade [tag] [--keep]`.
pub fn cmd_agy_upgrade(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("agy-upgrade: {m}"));
    let mut tag = None;
    let mut keep = false;
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("agy-upgrade-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: agy-upgrade brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    let mut u = crate::lab::UpOpts::from_env();
    u.trust = true;
    u.lifeline = !keep;
    u.harness_keys = format!("{}\ncontinue = false", u.harness_keys);
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: agy-upgrade: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_check(&tag);
    if keep {
        println!("agy-upgrade: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(&tag, false);
    }
    match verdict? {
        true => Ok(0),
        false => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The build id atpkg lays a plain version under, as its store holds them
    /// (the scratch-agents copy's `1000001000004000000` is `1.4.0`).
    #[test]
    fn a_plain_versions_build_id_is_atpkgs() {
        assert_eq!(plain_build_id("1.4.0"), Some(1_000_001_000_004_000_000));
        assert_eq!(plain_build_id("1.4.1"), Some(1_000_001_000_004_000_001));
        for bad in ["1.4.0-R1", "1.04.0", "1.4.1000000", "1.4.0.1", "x"] {
            assert_eq!(plain_build_id(bad), None, "{bad}");
        }
        assert_eq!(bumped("1.4.0", 0).as_deref(), Some("1.4.0"));
        assert_eq!(bumped("1.4.0", 6).as_deref(), Some("1.4.6"));
        assert_eq!(bumped("1.4.10-R6427.1", 1), None);
    }
}
